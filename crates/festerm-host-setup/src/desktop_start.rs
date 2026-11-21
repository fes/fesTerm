//! Fixed authenticated Desktop start carrier. No cloud, setup or replay path.
use crate::{
    lifecycle::{Origin, Phase, Snapshot, Store},
    services::{Observation, ProcessState, Services, TrustedStage},
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
        current(&store, installation, epoch, scope)?;
        if before != store.snapshot()? {
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
    let output = child(stage.path(), args, cancel.clone())?;
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
    current(store, &expected.installation, expected.epoch, &scope)?;
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
) -> io::Result<Vec<u8>> {
    let tool = tool.to_owned();
    let home = festerm_unix_security::current_user_home()?;
    let until = Instant::now() + DEADLINE;
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
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "host.cloud_cancelled");
        // child() sends its result only after wait() has reaped this child.
        assert_eq!(std::fs::read(finished).unwrap(), b"done");
    }
}
