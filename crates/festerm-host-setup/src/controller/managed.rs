//! Native exit translation follows durable error history, never product success.
use super::{RunFailure, RunRecord, Services, Store};
use crate::{
    lifecycle::{Origin, Phase},
    run_exit,
};
use festerm_sessiond::private_storage as storage;
use serde::{Deserialize, Serialize};
use std::{io, path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

const RECORD: &str = "host-managed-result.json";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Disposition {
    Stopped,
    Refused,
    Failed,
    WaitingForLogin,
}

/// Last completed managed invocation, not liveness, launch authority or Ready.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    version: u8,
    pub installation: String,
    pub origin: Origin,
    pub epoch: u64,
    pub run_id: Option<String>,
    pub disposition: Disposition,
    pub failure: Option<RunFailure>,
    pub product_success: bool,
}

pub fn last_report(store: &Store) -> io::Result<Option<Report>> {
    store.with_locked(|directory, snapshot| {
        let bytes = match storage::read_private(directory, Path::new(RECORD), 8192) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let report: Report = serde_json::from_slice(&bytes)
            .map_err(|_| io::Error::other("host.invalid_managed_result"))?;
        if report.version != 1
            || report.installation != snapshot.installation
            || report.origin != snapshot.origin
            || report.epoch > snapshot.epoch
            || report
                .run_id
                .as_ref()
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_err())
            || report.product_success != (report.disposition == Disposition::Stopped)
            || report.failure.is_none() != report.product_success
        {
            return Err(io::Error::other("host.invalid_managed_result"));
        }
        Ok(Some(report))
    })
}

/// Bootstrap reads may contend with the submitting UI's lifecycle operations.
/// One deadline covers opening the store and validating its registration. No
/// runtime invocation, reservation or history write is inside this retry loop.
pub async fn run_from_path(
    state: &Path,
    origin: Origin,
    cancel: CancellationToken,
) -> io::Result<()> {
    let deadline = tokio::time::Instant::now() + super::INITIAL_ADMISSION_CONTENTION;
    let store = Arc::new(bootstrap_read(deadline, &cancel, || Store::open(state, origin)).await?);
    let expected = bootstrap_read(deadline, &cancel, || store.snapshot()).await?;
    bootstrap_registration(&store, state, &expected, deadline, &cancel).await?;
    run_registered(store, state, cancel).await
}

pub async fn run(store: Arc<Store>, state: &Path, cancel: CancellationToken) -> io::Result<()> {
    let deadline = tokio::time::Instant::now() + super::INITIAL_ADMISSION_CONTENTION;
    let expected = bootstrap_read(deadline, &cancel, || store.snapshot()).await?;
    bootstrap_registration(&store, state, &expected, deadline, &cancel).await?;
    run_registered(store, state, cancel).await
}

async fn bootstrap_registration(
    store: &Store,
    state: &Path,
    expected: &crate::lifecycle::Snapshot,
    deadline: tokio::time::Instant,
    cancel: &CancellationToken,
) -> io::Result<()> {
    bootstrap_read(deadline, cancel, || {
        if store.snapshot()? != *expected {
            return Err(super::refused("host.runtime_consent_changed"));
        }
        Services::current_user(store, state)?.require_recovery_registration()?;
        if store.snapshot()? != *expected {
            return Err(super::refused("host.runtime_consent_changed"));
        }
        Ok(())
    })
    .await
}

async fn bootstrap_read<T>(
    deadline: tokio::time::Instant,
    cancel: &CancellationToken,
    mut read: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    loop {
        if cancel.is_cancelled() {
            return Err(super::refused("host.cloud_cancelled"));
        }
        let result = read();
        if cancel.is_cancelled() {
            return Err(super::refused("host.cloud_cancelled"));
        }
        match result {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(error);
                }
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(super::refused("host.cloud_cancelled")),
                    _ = tokio::time::sleep_until(
                        (tokio::time::Instant::now() + std::time::Duration::from_millis(25)).min(deadline)
                    ) => {}
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(error);
                }
            }
            result => return result,
        }
    }
}

async fn run_registered(
    store: Arc<Store>,
    state: &Path,
    cancel: CancellationToken,
) -> io::Result<()> {
    let (_run_lease, snapshot, mut record) = match super::reserve(&store) {
        Ok(reserved) => reserved,
        Err(error) => return finish(&store, &store.snapshot()?, None, Err(error)),
    };
    let result =
        super::run_to_completion(store.clone(), state, &snapshot, &mut record, cancel).await;
    // Keep the original run lease through recording, never adopt another attempt's history.
    finish(&store, &snapshot, Some(&record), result)
}

fn finish(
    store: &Store,
    initial: &crate::lifecycle::Snapshot,
    record: Option<&RunRecord>,
    result: io::Result<()>,
) -> io::Result<()> {
    let current = store.snapshot()?;
    let login = result.as_ref().is_err_and(|error| {
        run_exit::error_exit_code(error) == run_exit::LOGIN_PROOF_UNAVAILABLE_EXIT_CODE
    });
    let failure = if result.is_ok() {
        None
    } else {
        Some(record.and_then(|record| record.failure).unwrap_or_else(|| {
            if result
                .as_ref()
                .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock)
            {
                RunFailure::InstallationBusy
            } else {
                RunFailure::RuntimeFailed
            }
        }))
    };
    let disposition = if result.is_ok() {
        Disposition::Stopped
    } else if login {
        Disposition::WaitingForLogin
    } else if current.phase != Phase::Provisioned
        || matches!(
            failure,
            Some(
                RunFailure::AuthenticatedBundleRequired
                    | RunFailure::CurrentOwnerConsentRequired
                    | RunFailure::ConsentChanged
            )
        )
    {
        Disposition::Refused
    } else {
        Disposition::Failed
    };
    let report = Report {
        version: 1,
        installation: initial.installation.clone(),
        origin: initial.origin,
        epoch: initial.epoch,
        run_id: record.map(|record| record.run_id.clone()),
        disposition,
        failure,
        product_success: result.is_ok(),
    };
    store.with_locked(|directory, snapshot| {
        if snapshot.installation != initial.installation || snapshot.origin != initial.origin {
            return Err(io::Error::other("host.managed_installation_changed"));
        }
        storage::replace_private(
            directory,
            Path::new(RECORD),
            &serde_json::to_vec(&report)
                .map_err(|_| io::Error::other("host.invalid_managed_result"))?,
        )
    })?;
    match result {
        Ok(()) => Ok(()),
        Err(error) if login => Err(error),
        Err(source) => Err(io::Error::other(run_exit::RecordedManagedStop { source })),
    }
}

#[cfg(test)]
mod tests {
    use super::super::last_run;
    use super::*;
    use crate::lifecycle::{Settings, Startup};

    fn store(root: &Path) -> Store {
        Store::create(
            &root.join("state"),
            Origin::Headless,
            Settings {
                friendly_name: "managed fixture".into(),
                allow_creation: false,
                startup: Startup::Manual,
            },
        )
        .unwrap()
    }

    fn provision(store: &Store) {
        let ticket = store
            .request_enable(&store.snapshot().unwrap().settings)
            .unwrap();
        store
            .record_provisioned(
                &ticket,
                crate::lifecycle::Binding {
                    host_id: uuid::Uuid::new_v4().to_string(),
                    account: "fixture-account".into(),
                    qualified_tunnel: "fixture-qualified".into(),
                    cloud_incarnation: "fixture-incarnation".into(),
                    host_key_fingerprint: "fixture-key".into(),
                },
            )
            .unwrap();
    }

    fn held_lifecycle_lease(store: &Store) -> crate::protected_state::Lease {
        store.with_owned_lease(|_, _, lease| Ok(lease)).unwrap()
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test(start_paused = true)]
    async fn managed_bootstrap_waits_for_real_lifecycle_lease_before_preflight() {
        let root = storage::private_test_directory();
        let store = Arc::new(store(root.path()));
        provision(&store);
        let state = root.path().join("state");
        let lease = held_lifecycle_lease(&store);
        let attempt = run(store.clone(), &state, CancellationToken::new());
        tokio::pin!(attempt);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut attempt)
                .await
                .is_err(),
            "bootstrap must wait for lifecycle contention before touching runtime history"
        );
        drop(lease);
        let error = attempt.await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "host.native_recovery_registration_required"
        );
        assert!(super::super::last_run(&store).unwrap().is_none());
        assert!(last_report(&store).unwrap().is_none());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn managed_bootstrap_service_lease_alone_does_not_block_registration_preflight() {
        let root = storage::private_test_directory();
        let store = Arc::new(store(root.path()));
        provision(&store);
        let state = root.path().join("state");
        let file = store
            .with_locked(|directory, _| {
                storage::open_private_file(directory, Path::new("native-service.lock"), true)
            })
            .unwrap();
        let _lease = crate::protected_state::Lease::exclusive(file).unwrap();
        let error = run(store.clone(), &state, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "host.native_recovery_registration_required"
        );
        assert!(super::super::last_run(&store).unwrap().is_none());
    }

    #[test]
    fn managed_bootstrap_cli_store_open_can_fail_before_controller_entry() {
        let root = storage::private_test_directory();
        let store = store(root.path());
        provision(&store);
        let _lease = held_lifecycle_lease(&store);
        let error = Store::open(&root.path().join("state"), Origin::Headless)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test(start_paused = true)]
    async fn managed_bootstrap_entry_waits_for_store_open_without_new_history() {
        let root = storage::private_test_directory();
        let store = store(root.path());
        provision(&store);
        let state = root.path().join("state");
        let lease = held_lifecycle_lease(&store);
        let attempt = run_from_path(&state, Origin::Headless, CancellationToken::new());
        tokio::pin!(attempt);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut attempt)
                .await
                .is_err()
        );
        drop(lease);
        assert_eq!(
            attempt.await.unwrap_err().to_string(),
            "host.native_recovery_registration_required"
        );
        assert!(super::super::last_run(&store).unwrap().is_none());
        assert!(last_report(&store).unwrap().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn managed_bootstrap_persistent_lifecycle_contention_uses_one_deadline() {
        let root = storage::private_test_directory();
        let store = store(root.path());
        provision(&store);
        let lease = held_lifecycle_lease(&store);
        let started = tokio::time::Instant::now();
        let error = run_from_path(
            &root.path().join("state"),
            Origin::Headless,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            started.elapsed(),
            super::super::INITIAL_ADMISSION_CONTENTION
        );
        drop(lease);
        assert!(super::super::last_run(&store).unwrap().is_none());
        assert!(last_report(&store).unwrap().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn managed_bootstrap_read_phases_share_remaining_budget() {
        let root = storage::private_test_directory();
        let store = store(root.path());
        provision(&store);
        let started = tokio::time::Instant::now();
        let deadline = started + super::super::INITIAL_ADMISSION_CONTENTION;
        let cancel = CancellationToken::new();
        let lease = held_lifecycle_lease(&store);
        let first = bootstrap_read(deadline, &cancel, || {
            Store::open(&root.path().join("state"), Origin::Headless)
        });
        tokio::pin!(first);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), &mut first)
                .await
                .is_err()
        );
        drop(lease);
        let opened = first.await.unwrap();
        let _lease = held_lifecycle_lease(&store);
        let error = bootstrap_read(deadline, &cancel, || opened.snapshot())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            started.elapsed(),
            super::super::INITIAL_ADMISSION_CONTENTION
        );
    }

    #[tokio::test(start_paused = true)]
    async fn managed_bootstrap_cancellation_stops_before_open_or_retry() {
        let root = storage::private_test_directory();
        let store = store(root.path());
        provision(&store);
        let lease = held_lifecycle_lease(&store);
        let cancel = CancellationToken::new();
        let state = root.path().join("state");
        let attempt = run_from_path(&state, Origin::Headless, cancel.clone());
        tokio::pin!(attempt);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut attempt)
                .await
                .is_err()
        );
        cancel.cancel();
        assert_eq!(
            attempt.await.unwrap_err().to_string(),
            "host.cloud_cancelled"
        );
        drop(lease);
        assert!(super::super::last_run(&store).unwrap().is_none());
        assert_eq!(
            run_from_path(&root.path().join("missing"), Origin::Headless, cancel)
                .await
                .unwrap_err()
                .to_string(),
            "host.cloud_cancelled"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test(start_paused = true)]
    async fn managed_bootstrap_changed_consent_after_busy_refuses_without_history() {
        for change in ["revoke", "settings", "binding"] {
            let root = storage::private_test_directory();
            let store = store(root.path());
            provision(&store);
            let expected = store.snapshot().unwrap();
            let state = root.path().join("state");
            let deadline = tokio::time::Instant::now() + super::super::INITIAL_ADMISSION_CONTENTION;
            let cancel = CancellationToken::new();
            let lease = held_lifecycle_lease(&store);
            let attempt = bootstrap_registration(&store, &state, &expected, deadline, &cancel);
            tokio::pin!(attempt);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), &mut attempt)
                    .await
                    .is_err()
            );
            drop(lease);
            if change == "revoke" {
                store.revoke().unwrap();
            } else {
                store
                    .with_locked(|directory, current| {
                        let mut changed = current.clone();
                        if change == "settings" {
                            changed.settings.allow_creation = !changed.settings.allow_creation;
                        } else {
                            changed
                                .binding
                                .as_mut()
                                .unwrap()
                                .qualified_tunnel
                                .push_str("-changed");
                        }
                        storage::replace_private(
                            directory,
                            Path::new("host-lifecycle.json"),
                            &serde_json::to_vec(&changed).unwrap(),
                        )
                    })
                    .unwrap();
            }
            assert_eq!(
                attempt.await.unwrap_err().to_string(),
                "host.runtime_consent_changed"
            );
            assert!(super::super::last_run(&store).unwrap().is_none());
            assert!(last_report(&store).unwrap().is_none());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn managed_bootstrap_nonbusy_refusal_is_never_retried() {
        let cancel = CancellationToken::new();
        let started = tokio::time::Instant::now();
        let mut calls = 0;
        let error = bootstrap_read(
            started + super::super::INITIAL_ADMISSION_CONTENTION,
            &cancel,
            || {
                calls += 1;
                Err::<(), _>(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "fixture-refused",
                ))
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(calls, 1);
        assert_eq!(started.elapsed(), std::time::Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn managed_bootstrap_expired_deadline_still_allows_first_read_success() {
        let cancel = CancellationToken::new();
        let deadline = tokio::time::Instant::now() - std::time::Duration::from_secs(1);
        let mut calls = 0;
        let value = bootstrap_read(deadline, &cancel, || {
            calls += 1;
            Ok(17)
        })
        .await
        .unwrap();
        assert_eq!(value, 17);
        assert_eq!(calls, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn managed_bootstrap_expired_deadline_preserves_nonbusy_refusal() {
        let cancel = CancellationToken::new();
        let deadline = tokio::time::Instant::now() - std::time::Duration::from_secs(1);
        let mut calls = 0;
        let error = bootstrap_read(deadline, &cancel, || {
            calls += 1;
            Err::<(), _>(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "fixture-original-refusal",
            ))
        })
        .await
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "fixture-original-refusal");
        assert_eq!(calls, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn managed_bootstrap_deadline_preserves_original_busy_error() {
        for expired in [false, true] {
            let cancel = CancellationToken::new();
            let started = tokio::time::Instant::now();
            let deadline = if expired {
                started - std::time::Duration::from_secs(1)
            } else {
                started + std::time::Duration::from_millis(10)
            };
            let mut calls = 0;
            let error = bootstrap_read(deadline, &cancel, || {
                calls += 1;
                Err::<(), _>(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "fixture-original-busy",
                ))
            })
            .await
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
            assert_eq!(error.to_string(), "fixture-original-busy");
            assert_eq!(calls, 1);
            assert_eq!(
                started.elapsed(),
                if expired {
                    std::time::Duration::ZERO
                } else {
                    std::time::Duration::from_millis(10)
                }
            );
        }
    }

    #[tokio::test]
    async fn host_process_recovery_each_attempt_reenters_real_admission_and_revocation_wins() {
        let root = storage::private_test_directory();
        let store = Arc::new(store(root.path()));
        provision(&store);
        let initial = store.snapshot().unwrap();
        let state = root.path().join("state");
        let mut previous = None;
        for _ in 0..2 {
            let error = run_registered(store.clone(), &state, CancellationToken::new())
                .await
                .unwrap_err();
            assert!(run_exit::is_recorded_managed_stop(&error));
            let record = last_run(&store).unwrap().unwrap();
            assert_ne!(previous.as_deref(), Some(record.run_id.as_str()));
            assert_eq!(
                record.failure,
                Some(RunFailure::AuthenticatedBundleRequired)
            );
            assert_eq!(
                last_report(&store).unwrap().unwrap().run_id,
                Some(record.run_id.clone())
            );
            previous = Some(record.run_id);
            assert_eq!(store.snapshot().unwrap(), initial);
            assert!(!state.join("daemon").exists());
        }
        store.revoke().unwrap();
        let revoked = store.snapshot().unwrap();
        run_registered(store.clone(), &state, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(
            last_report(&store).unwrap().unwrap().disposition,
            Disposition::Refused
        );
        assert_eq!(last_run(&store).unwrap().unwrap().run_id, previous.unwrap());
        assert_eq!(store.snapshot().unwrap(), revoked);
    }

    #[test]
    fn host_process_recovery_busy_attempt_never_adopts_another_runs_record_or_lease() {
        let root = storage::private_test_directory();
        let store = Arc::new(store(root.path()));
        provision(&store);
        let (lease, snapshot, active) = super::super::reserve(&store).unwrap();
        super::super::persist(&store, &active).unwrap();
        let result = finish(
            &store,
            &snapshot,
            None,
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "host.installation_busy",
            )),
        );
        assert!(run_exit::is_recorded_managed_stop(&result.unwrap_err()));
        let report = last_report(&store).unwrap().unwrap();
        assert_eq!(report.run_id, None);
        assert_eq!(report.failure, Some(RunFailure::InstallationBusy));
        assert!(!report.product_success);
        assert_eq!(last_run(&store).unwrap().unwrap(), active);
        assert!(super::super::reserve(&store).is_err());
        drop(lease);
        let (_lease, _, next) = super::super::reserve(&store).unwrap();
        assert_ne!(next.run_id, active.run_id);
    }

    #[test]
    fn host_process_recovery_records_operational_failure_as_failure_not_product_success() {
        let root = storage::private_test_directory();
        let store = store(root.path());
        provision(&store);
        let initial = store.snapshot().unwrap();
        let error = finish(&store, &initial, None, Err(io::Error::other("uncertain"))).unwrap_err();
        assert!(run_exit::is_recorded_managed_stop(&error));
        let report = last_report(&store).unwrap().unwrap();
        assert_eq!(report.disposition, Disposition::Failed);
        assert!(!report.product_success);
        assert_eq!(report.failure, Some(RunFailure::RuntimeFailed));
        assert_eq!(store.snapshot().unwrap(), initial);
    }

    #[test]
    fn host_process_recovery_records_refusal_before_native_success_code() {
        let root = storage::private_test_directory();
        let store = store(root.path());
        let initial = store.snapshot().unwrap();
        let error = finish(
            &store,
            &initial,
            None,
            Err(io::Error::other("host.fixture_refusal")),
        )
        .unwrap_err();
        assert!(run_exit::is_recorded_managed_stop(&error));
        assert_eq!(run_exit::error_exit_code(&error), 0);
        let report = last_report(&store).unwrap().unwrap();
        assert_eq!(report.disposition, Disposition::Refused);
        assert!(!report.product_success);
        assert_eq!(report.failure, Some(RunFailure::RuntimeFailed));
        assert!(last_run(&store).unwrap().is_none());
        assert_eq!(store.snapshot().unwrap(), initial);
        assert_eq!(
            run_exit::error_exit_code(&io::Error::other("host.fixture_refusal")),
            1
        );
    }

    #[test]
    fn host_process_recovery_login_retry_is_typed_and_history_never_authorizes() {
        let root = storage::private_test_directory();
        let store = store(root.path());
        let initial = store.snapshot().unwrap();
        let error = finish(
            &store,
            &initial,
            None,
            Err(io::Error::other(run_exit::LoginProofUnavailable)),
        )
        .unwrap_err();
        assert_eq!(
            run_exit::error_exit_code(&error),
            run_exit::LOGIN_PROOF_UNAVAILABLE_EXIT_CODE
        );
        assert_eq!(
            last_report(&store).unwrap().unwrap().disposition,
            Disposition::WaitingForLogin
        );
        assert!(!last_report(&store).unwrap().unwrap().product_success);
        assert_eq!(store.snapshot().unwrap(), initial);
    }

    #[test]
    fn host_process_recovery_failed_history_write_cannot_translate_to_managed_stop() {
        let root = storage::private_test_directory();
        let store = store(root.path());
        let initial = store.snapshot().unwrap();
        std::fs::create_dir(root.path().join("state").join(RECORD)).unwrap();
        let error = finish(&store, &initial, None, Err(io::Error::other("uncertain"))).unwrap_err();
        assert!(!run_exit::is_recorded_managed_stop(&error));
        assert_ne!(run_exit::error_exit_code(&error), 0);
        assert_eq!(store.snapshot().unwrap(), initial);
    }
}
