//! Fixed authenticated Desktop start carrier. No cloud, setup or replay path.
use crate::{
    lifecycle::{Origin, Phase, Snapshot, Store},
    services::{
        start_diagnostics::{observe as observe_start, StartStage},
        Observation, ProcessState, Services, TrustedStage,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io,
    path::Path,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const LIMIT: usize = 4096;
const DEADLINE: Duration = Duration::from_secs(15);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    schema: u16,
    nonce: String,
    scope: String,
    process: Option<ProcessState>,
    failure: Option<StartFailure>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StartFailure {
    Cancelled,
    NativeTimeout,
    ScopeChanged,
    PrivateStorage,
    Busy,
    Unknown,
}
impl StartFailure {
    fn from_error(error: &io::Error) -> Self {
        if error.kind() == io::ErrorKind::WouldBlock {
            return Self::Busy;
        }
        match error.to_string().as_str() {
            "host.cloud_cancelled" => Self::Cancelled,
            "host.service_native_timeout" => Self::NativeTimeout,
            "host.runtime_consent_changed" => Self::ScopeChanged,
            "host.secure_store_unavailable"
            | "owner.vault_locked_or_unavailable_unlock_native_store_and_retry"
            | "owner.vault_anchor_missing_local_reconciliation_required"
            | "owner.native_vault_unsupported_no_plaintext_fallback"
            | "owner.native_vault_failure_no_fallback" => Self::PrivateStorage,
            _ => Self::Unknown,
        }
    }
    fn error(self) -> io::Error {
        if matches!(self, Self::Busy) {
            return io::Error::new(io::ErrorKind::WouldBlock, "host.service_operation_busy");
        }
        io::Error::other(match self {
            Self::Cancelled => "host.cloud_cancelled",
            Self::NativeTimeout => "host.service_native_timeout",
            Self::ScopeChanged => "host.runtime_consent_changed",
            Self::PrivateStorage => "host.secure_store_unavailable",
            Self::Unknown => "host.desktop_start_unconfirmed",
            Self::Busy => unreachable!(),
        })
    }
}

fn scope_digest(snapshot: &Snapshot) -> io::Result<String> {
    let bytes = serde_json::to_vec(snapshot).map_err(|_| refused())?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
fn refused() -> io::Error {
    io::Error::other("host.desktop_start_unconfirmed")
}
fn current(store: &Store, installation: &str, epoch: u64, scope: &str) -> io::Result<Snapshot> {
    let snapshot = store.snapshot()?;
    if snapshot.origin != Origin::Desktop
        || snapshot.phase != Phase::Provisioned
        || snapshot.installation != installation
        || snapshot.epoch != epoch
        || scope_digest(&snapshot)? != scope
    {
        return Err(io::Error::other("host.runtime_consent_changed"));
    }
    Ok(snapshot)
}
fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value)
        .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == value)
}

/// Dedicated authenticated executable only, after its process-wide no-UI fence.
/// EOF on the private parent pipe revokes unsent effects; submitted effects
/// retain the existing native-manager uncertainty and independent Disable path.
pub fn execute(
    state: &Path,
    installation: &str,
    epoch: u64,
    scope: &str,
    nonce: &str,
) -> io::Result<String> {
    if !canonical_uuid(installation)
        || !canonical_uuid(nonce)
        || scope.len() != 64
        || !scope
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(refused());
    }
    let store = Store::open(state, Origin::Desktop)?;
    let before = current(&store, installation, epoch, scope)?;
    let services = Services::current_user(&store, state)?;
    let cancel = CancellationToken::new();
    let lifetime = cancel.clone();
    std::thread::Builder::new()
        .name("host-parent-lifetime".into())
        .spawn(move || {
            use std::io::Read;
            let mut byte = [0];
            // No input payload exists. EOF, unexpected data, or an I/O failure all
            // revoke the parent's one operation; no command parser or secret pipe.
            let _ = std::io::stdin().read(&mut byte);
            lifetime.cancel();
        })?;
    let result: io::Result<ProcessState> = (|| {
        let observation = services.start_from_desktop_parent(epoch, || {
            if cancel.is_cancelled() {
                return Err(io::Error::other("host.cloud_cancelled"));
            }
            current(&store, installation, epoch, scope).map(|_| ())
        })?;
        if cancel.is_cancelled() {
            return Err(io::Error::other("host.cloud_cancelled"));
        }
        observe_start(
            state,
            StartStage::MacChildCurrent,
            current(&store, installation, epoch, scope),
        )?;
        if before != observe_start(state, StartStage::MacChildSnapshot, store.snapshot())? {
            return Err(refused());
        }
        Ok(observation.process)
    })();
    let (process, failure) = match result {
        Ok(process) => (Some(process), None),
        Err(error) => (None, Some(StartFailure::from_error(&error))),
    };
    let bytes = serde_json::to_string(&Reply {
        schema: 1,
        nonce: nonce.into(),
        scope: scope.into(),
        process,
        failure,
    })
    .map_err(|_| refused())?;
    if bytes.len() > LIMIT {
        return Err(refused());
    }
    Ok(format!("{bytes}\n"))
}

fn parent_current(
    state: &Path,
    until: Instant,
    cancel: &CancellationToken,
    mut read: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let mut last_busy = None;
    loop {
        if cancel.is_cancelled() {
            return Err(io::Error::other("host.cloud_cancelled"));
        }
        if Instant::now() >= until {
            break;
        }
        let read_result = read();
        let read_result = match &read_result {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock && last_busy.is_some() => {
                read_result
            }
            // Keep evidence of the first Busy, even when a later read succeeds.
            _ => observe_start(state, StartStage::MacParentCurrent, read_result),
        };
        if cancel.is_cancelled() {
            return Err(io::Error::other("host.cloud_cancelled"));
        }
        match read_result {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                last_busy = Some(error);
                std::thread::sleep(
                    until
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(25)),
                );
            }
            Ok(()) if Instant::now() >= until => break,
            result => return result,
        }
    }
    Err(last_busy.unwrap_or_else(|| io::Error::other("host.service_native_timeout")))
}

/// The GUI holds no service-operation lease while waiting. The authenticated
/// child acquires the existing lease and its unchanged native deadline itself.
pub fn start(
    store: &Store,
    state: &Path,
    stage: &TrustedStage,
    expected: &Snapshot,
    cancel: &CancellationToken,
) -> io::Result<Observation> {
    stage.require_mac_host_access()?;
    let scope = scope_digest(expected)?;
    current(store, &expected.installation, expected.epoch, &scope)?;
    if cancel.is_cancelled() {
        return Err(io::Error::other("host.cloud_cancelled"));
    }
    let nonce = uuid::Uuid::new_v4().to_string();
    let args = vec![
        "start-desktop".into(),
        "--state-dir".into(),
        state.as_os_str().to_owned(),
        "--installation".into(),
        expected.installation.clone().into(),
        "--epoch".into(),
        expected.epoch.to_string().into(),
        "--scope-digest".into(),
        scope.clone().into(),
        "--nonce".into(),
        nonce.clone().into(),
        "--consent".into(),
        "start".into(),
    ];
    let until = Instant::now() + DEADLINE;
    let output = child(stage.path(), args, cancel.clone(), until)?;
    stage.revalidate_for_desktop_start()?;
    let reply: Reply = serde_json::from_slice(&output).map_err(|_| refused())?;
    if reply.schema != 1 || reply.nonce != nonce || reply.scope != scope {
        return Err(refused());
    }
    if cancel.is_cancelled() {
        return Err(io::Error::other("host.cloud_cancelled"));
    }
    if let Some(failure) = reply.failure {
        if reply.process.is_some() {
            return Err(refused());
        }
        return Err(failure.error());
    }
    let process = reply.process.ok_or_else(refused)?;
    // After the one child has exited, only the exact protected read may wait.
    // The parent and child transport share the deadline captured above.
    parent_current(state, until, cancel, || {
        current(store, &expected.installation, expected.epoch, &scope).map(drop)
    })?;
    Ok(Observation {
        process,
        installation: expected.installation.clone(),
        origin: Origin::Desktop,
        epoch: expected.epoch,
    })
}

fn child(
    tool: &Path,
    args: Vec<std::ffi::OsString>,
    cancel: CancellationToken,
    until: Instant,
) -> io::Result<Vec<u8>> {
    let tool = tool.to_owned();
    let home = festerm_unix_security::current_user_home()?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("host-desktop-start".into())
        .spawn(move || {
            let result = (|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(async move {
                    use std::process::Stdio;
                    use tokio::io::AsyncReadExt;
                    let mut child = tokio::process::Command::new(tool)
                        .args(args)
                        .env_clear()
                        .env("HOME", home)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .kill_on_drop(false)
                        .spawn()
                        .map_err(|_| refused())?;
                    let mut lifetime = child.stdin.take();
                    let Some(mut stdout) = child.stdout.take() else {
                        drop(lifetime.take());
                        let _ = child.wait().await;
                        return Err(refused());
                    };
                    let mut bytes = Vec::new();
                    let mut bounded = (&mut stdout).take(LIMIT as u64 + 1);
                    let result = tokio::select! {
                        _ = cancel.cancelled() => Err(io::Error::other("host.cloud_cancelled")),
                        result = tokio::time::timeout_at(tokio::time::Instant::from_std(until),
                            bounded.read_to_end(&mut bytes)) => {
                            match result {
                                Ok(Ok(_)) if bytes.len() <= LIMIT => Ok(()),
                                Ok(Ok(_)) | Ok(Err(_)) => Err(refused()),
                            Err(_) => Err(io::Error::other("host.service_native_timeout")),
                            }
                        }
                    };
                    // Close the exact parent's lifetime pipe on every path. As with
                    // existing mutating NativeRunner work, timeout is NOT permission
                    // to kill a submitted manager command. This owned worker drains
                    // the actual child. The backend call retains its GUI mutation
                    // permit until wait() completes, preserving the initial failure.
                    drop(lifetime.take());
                    if result.is_err() {
                        let _ = tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await;
                    }
                    let status = child.wait().await;
                    result?; // retain the original cancellation/timeout/overflow
                    if !status.map_err(|_| refused())?.success() {
                        return Err(refused());
                    }
                    Ok(bytes)
                })
            })();
            let _ = send.send(result);
        })?;
    receive.recv().map_err(|_| refused())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::{Binding, Settings, Startup};
    use crate::protected_state::Lease;
    use festerm_sessiond::private_storage as storage;
    use std::path::PathBuf;

    fn provisioned_parent() -> (storage::PrivateTestDirectory, PathBuf, Store, Snapshot) {
        let root = storage::private_test_directory();
        let state = root.path().join("state");
        let settings = Settings {
            filesystem: None,
            ordinary_access: Default::default(),
            friendly_name: "fixture".into(),
            allow_creation: false,
            allow_session_termination: false,
            startup: Startup::Manual,
        };
        let store = Store::create(&state, Origin::Desktop, settings.clone()).unwrap();
        let ticket = store.request_enable(&settings).unwrap();
        store
            .record_provisioned(
                &ticket,
                Binding {
                    host_id: uuid::Uuid::new_v4().to_string(),
                    account: "fixture-account".into(),
                    qualified_tunnel: "fixture.region".into(),
                    cloud_incarnation: "fixture-incarnation".into(),
                    host_key_fingerprint: "SHA256:fixture".into(),
                },
            )
            .unwrap();
        let expected = store.snapshot().unwrap();
        (root, state, store, expected)
    }

    fn hold_lifecycle_lease(state: &Path) -> Lease {
        let directory = storage::private_directory(state, false).unwrap();
        Lease::exclusive(
            storage::open_private_file(&directory, Path::new("host-lifecycle.lock"), false)
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn desktop_start_parent_current_recovers_after_real_lease_releases() {
        let (_root, state, store, expected) = provisioned_parent();
        let scope = scope_digest(&expected).unwrap();
        let mut lease = Some(hold_lifecycle_lease(&state));
        let mut reads = 0;
        parent_current(
            &state,
            Instant::now() + DEADLINE,
            &CancellationToken::new(),
            || {
                reads += 1;
                let result = current(&store, &expected.installation, expected.epoch, &scope);
                if reads == 1 {
                    assert_eq!(
                        result.as_ref().unwrap_err().kind(),
                        io::ErrorKind::WouldBlock
                    );
                    drop(lease.take());
                }
                result.map(drop)
            },
        )
        .expect("the post-child parent read should recover from a released lifecycle lease");
        assert_eq!(reads, 2);
        assert_eq!(store.snapshot().unwrap(), expected);
        let log = std::fs::read_to_string(state.join("worker.log")).unwrap();
        assert!(log.contains("host.start_observation stage=mac_parent_current kind=busy os=none"));
    }

    #[test]
    fn desktop_start_parent_current_refuses_changed_consent_after_real_contention() {
        let (_root, state, store, expected) = provisioned_parent();
        let scope = scope_digest(&expected).unwrap();
        let mut lease = Some(hold_lifecycle_lease(&state));
        let mut changed = expected.clone();
        changed.settings.allow_creation = true;
        let mut reads = 0;
        let error = parent_current(
            &state,
            Instant::now() + DEADLINE,
            &CancellationToken::new(),
            || {
                reads += 1;
                let result = current(&store, &expected.installation, expected.epoch, &scope);
                if let Some(lease) = lease.take() {
                    assert_eq!(
                        result.as_ref().unwrap_err().kind(),
                        io::ErrorKind::WouldBlock
                    );
                    // Change only the approved settings while the competing lease is held.
                    let directory = storage::private_directory(&state, false).unwrap();
                    storage::replace_private(
                        &directory,
                        Path::new("host-lifecycle.json"),
                        &serde_json::to_vec(&changed).unwrap(),
                    )
                    .unwrap();
                    drop(lease);
                }
                result.map(drop)
            },
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "host.runtime_consent_changed");
        assert_eq!(reads, 2);
        assert_eq!(store.snapshot().unwrap(), changed);
    }

    #[test]
    fn desktop_start_parent_current_cancelled_during_real_contention_cannot_read_again() {
        let (_root, state, store, expected) = provisioned_parent();
        let scope = scope_digest(&expected).unwrap();
        let _lease = hold_lifecycle_lease(&state);
        let cancel = CancellationToken::new();
        let mut reads = 0;
        let error = parent_current(&state, Instant::now() + DEADLINE, &cancel, || {
            reads += 1;
            let result = current(&store, &expected.installation, expected.epoch, &scope);
            assert_eq!(
                result.as_ref().unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            cancel.cancel();
            result.map(drop)
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "host.cloud_cancelled");
        assert_eq!(reads, 1);

        let error = parent_current(&state, Instant::now() + DEADLINE, &cancel, || {
            panic!("cancelled parent must not read protected state")
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "host.cloud_cancelled");
    }

    #[test]
    fn desktop_start_parent_current_preserves_original_deadline_and_busy() {
        let (_root, state, store, expected) = provisioned_parent();
        let scope = scope_digest(&expected).unwrap();
        let cancel = CancellationToken::new();
        let error = parent_current(&state, Instant::now(), &cancel, || {
            panic!("the child already consumed the original observation budget")
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "host.service_native_timeout");

        for release_lease in [false, true] {
            let mut lease = Some(hold_lifecycle_lease(&state));
            let mut reads = 0;
            let until = Instant::now() + Duration::from_millis(500);
            let error = parent_current(&state, until, &cancel, || {
                reads += 1;
                let result = current(&store, &expected.installation, expected.epoch, &scope);
                if reads == 1 {
                    assert_eq!(
                        result.as_ref().unwrap_err().kind(),
                        io::ErrorKind::WouldBlock
                    );
                } else {
                    assert_eq!(result.as_ref().unwrap(), &expected);
                }
                if release_lease && reads == 1 {
                    drop(lease.take());
                } else {
                    // A Busy or matched result returned after the original deadline
                    // cannot authorize another read or successful completion.
                    std::thread::sleep(until.saturating_duration_since(Instant::now()));
                }
                result.map(drop)
            })
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
            assert_eq!(error.to_string(), "host.installation_busy");
            assert_eq!(reads, if release_lease { 2 } else { 1 });
        }
    }

    #[test]
    fn desktop_start_parent_current_does_not_retry_fatal_read() {
        let (_root, state, store, expected) = provisioned_parent();
        let scope = scope_digest(&expected).unwrap();
        let mut lease = Some(hold_lifecycle_lease(&state));
        let mut reads = 0;
        let error = parent_current(
            &state,
            Instant::now() + DEADLINE,
            &CancellationToken::new(),
            || {
                reads += 1;
                if let Some(lease) = lease.take() {
                    let result = current(&store, &expected.installation, expected.epoch, &scope);
                    assert_eq!(
                        result.as_ref().unwrap_err().kind(),
                        io::ErrorKind::WouldBlock
                    );
                    drop(lease);
                    return result.map(drop);
                }
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "fixture denied",
                ))
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "fixture denied");
        assert_eq!(reads, 2);
    }

    #[test]
    fn desktop_start_reply_is_closed_bounded_and_never_claims_readiness() {
        let reply = Reply {
            schema: 1,
            nonce: uuid::Uuid::new_v4().to_string(),
            scope: "a".repeat(64),
            process: Some(ProcessState::Running),
            failure: None,
        };
        let bytes = serde_json::to_vec(&reply).unwrap();
        assert!(bytes.len() <= LIMIT);
        assert!(serde_json::from_slice::<Reply>(&bytes).is_ok());
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["ready"] = true.into();
        assert!(serde_json::from_value::<Reply>(value).is_err());
        assert!(!canonical_uuid("not-a-correlation"));
    }
    #[test]
    fn desktop_start_full_scope_digest_changes_with_consent() {
        let root = festerm_sessiond::private_storage::private_test_directory();
        let store = Store::create(
            &root.path().join("state"),
            Origin::Desktop,
            crate::lifecycle::Settings {
                filesystem: None,
                ordinary_access: Default::default(),
                friendly_name: "fixture".into(),
                allow_creation: false,
                allow_session_termination: false,
                startup: crate::lifecycle::Startup::Manual,
            },
        )
        .unwrap();
        let original = store.snapshot().unwrap();
        let first = scope_digest(&original).unwrap();
        let mut changed = original.clone();
        changed.settings.allow_creation = true;
        assert_ne!(scope_digest(&changed).unwrap(), first);
        changed = original.clone();
        changed.epoch += 1;
        assert_ne!(scope_digest(&changed).unwrap(), first);
        assert!(
            current(&store, &original.installation, original.epoch, &first).is_err(),
            "Disabled never authorizes start"
        );
    }
    #[test]
    fn desktop_start_cancel_closes_parent_pipe_and_drains_the_owned_child() {
        let root = tempfile::tempdir().unwrap();
        let finished = root.path().join("finished");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = child(
            Path::new("/bin/sh"),
            vec![
                "-c".into(),
                "/bin/cat >/dev/null; printf done > \"$1\"".into(),
                "fixture".into(),
                finished.as_os_str().to_owned(),
            ],
            cancel,
            Instant::now() + DEADLINE,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "host.cloud_cancelled");
        // child() sends its result only after wait() has reaped this child.
        assert_eq!(std::fs::read(finished).unwrap(), b"done");
    }
}
