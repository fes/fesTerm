//! Native exit translation follows durable error history, never product success.
use super::{last_run, RunFailure, Services, Store};
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

pub async fn run(store: Arc<Store>, state: &Path, cancel: CancellationToken) -> io::Result<()> {
    Services::current_user(&store, state)?.require_recovery_registration()?;
    let snapshot = store.snapshot()?;
    let previous = last_run(&store)?.map(|record| record.run_id);
    let result = super::run(store.clone(), state, cancel).await;
    finish(&store, &snapshot, previous.as_deref(), result)
}

fn finish(
    store: &Store,
    initial: &crate::lifecycle::Snapshot,
    previous: Option<&str>,
    result: io::Result<()>,
) -> io::Result<()> {
    let current = store.snapshot()?;
    let record = last_run(store)?
        .filter(|record| record.epoch == initial.epoch && Some(record.run_id.as_str()) != previous);
    let login = result.as_ref().is_err_and(|error| {
        run_exit::error_exit_code(error) == run_exit::LOGIN_PROOF_UNAVAILABLE_EXIT_CODE
    });
    let failure = if result.is_ok() {
        None
    } else {
        Some(
            record
                .as_ref()
                .and_then(|record| record.failure)
                .unwrap_or(RunFailure::RuntimeFailed),
        )
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
        run_id: record.map(|record| record.run_id),
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

    #[tokio::test]
    async fn host_process_recovery_each_attempt_reenters_real_admission_and_revocation_wins() {
        let root = storage::private_test_directory();
        let store = Arc::new(store(root.path()));
        provision(&store);
        let initial = store.snapshot().unwrap();
        let state = root.path().join("state");
        let mut previous = None;
        for _ in 0..2 {
            let result = super::super::run(store.clone(), &state, CancellationToken::new()).await;
            assert!(result.is_err());
            let error = finish(&store, &initial, previous.as_deref(), result).unwrap_err();
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
        let result = super::super::run(store.clone(), &state, CancellationToken::new()).await;
        assert!(result.is_err());
        finish(&store, &revoked, previous.as_deref(), result).unwrap_err();
        assert_eq!(
            last_report(&store).unwrap().unwrap().disposition,
            Disposition::Refused
        );
        assert_eq!(last_run(&store).unwrap().unwrap().run_id, previous.unwrap());
        assert_eq!(store.snapshot().unwrap(), revoked);
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
