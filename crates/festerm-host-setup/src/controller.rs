//! Shared explicit foreground setup and restartable host composition.

use crate::{
    cloud::{CloudHostAdapter, Health, HOST_IDENTITY_FILE},
    enrollment::{
        owner_key_fingerprint, CurrentOwnerPolicySource, EnrollmentSnapshot, OwnerStorage,
    },
    lifecycle::{Phase, Snapshot, Store},
    protected_state::Lease,
    services::{Observation, Services, TrustedStage},
};
use festerm_host_protocol::{Capabilities, ContractError};
use festerm_sessiond::{host_control::SessionControl, private_storage as storage};
use festerm_tunnel_bridge::host_runtime::HostRuntime;
use serde::{Deserialize, Serialize};
use std::{io, path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

const LOCK: &str = "host-runtime.lock";
const RECORD: &str = "host-runtime.json";

/// Separate host-local approval after the client reviewed its exact binding.
pub struct OwnerConsent {
    pub owner_public_key: [u8; 32],
    pub confirmed_owner_fingerprint: String,
    pub permissions: Capabilities,
}

#[derive(Serialize)]
pub struct SetupResult {
    pub native_registration: Observation,
    /// Public context for foreground review, not imported client authority.
    pub owner_approval_prerequisite: EnrollmentSnapshot,
}

/// Register zero-shell hosting after explicit package/account consent.
/// A failed or uncertain cloud submission is retained by the existing journal.
/// Owner access remains off until the separate local approval step.
pub async fn setup(
    store: &Store,
    state: &Path,
    stage: &TrustedStage,
    account: u64,
    cancel: &CancellationToken,
    prompt: impl FnOnce(festerm_tunnel_client::identity::DevicePrompt),
) -> io::Result<SetupResult> {
    stage.sessiond_path()?;
    if account == 0 || cancel.is_cancelled() {
        return Err(refused("host.foreground_account_consent_required"));
    }
    let before = store.snapshot()?;
    let cloud = CloudHostAdapter::new(store);
    let expected_epoch = match before.phase {
        Phase::Disabled => {
            let ticket = store.request_enable(&before.settings)?;
            let foreground = cloud.foreground(&ticket, account)?;
            cloud.sign_in(&foreground, cancel, prompt).await?;
            cloud.provision(&foreground, cancel).await?;
            ticket.epoch()
        }
        Phase::Provisioned => {
            if before
                .binding
                .as_ref()
                .map(|binding| binding.account.as_str())
                != Some(crate::cloud::subject(account).as_str())
                || !matches!(
                    cloud.health(cancel).await?,
                    Health::PresentEndpointUnqualified { .. }
                )
            {
                return Err(refused("host.cloud_binding_changed"));
            }
            before.epoch
        }
        _ => return Err(refused("host.reconcile_before_enable")),
    };
    if cancel.is_cancelled() {
        return Err(refused("host.cloud_cancelled"));
    }
    let owners = store.owner_enrollment()?;
    let current = store.snapshot()?;
    if current.epoch != expected_epoch || current.phase != Phase::Provisioned {
        return Err(refused("host.runtime_consent_changed"));
    }
    let account = current
        .binding
        .ok_or_else(|| refused("host.cloud_binding_changed"))?
        .account;
    let initial = match owners.snapshot() {
        Ok(existing) => existing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            owners.initialize_at_epoch(Path::new(HOST_IDENTITY_FILE), account, expected_epoch)?
        }
        Err(error) => return Err(error),
    };
    let native_registration =
        Services::current_user(store, state)?.install(stage, expected_epoch)?;
    Ok(SetupResult {
        native_registration,
        owner_approval_prerequisite: initial,
    })
}

pub fn approve_owner(store: &Store, consent: OwnerConsent) -> io::Result<EnrollmentSnapshot> {
    consent
        .permissions
        .validate_permissions()
        .map_err(|_| refused("host.invalid_owner_permissions"))?;
    if consent.permissions.issuance_epoch.is_some()
        || owner_key_fingerprint(&consent.owner_public_key)? != consent.confirmed_owner_fingerprint
    {
        return Err(refused("host.explicit_owner_confirmation_required"));
    }
    let before = store.snapshot()?;
    if before.phase != Phase::Provisioned {
        return Err(refused("host.runtime_current_provisioned_consent_required"));
    }
    if consent.permissions.creation_allowed && !before.settings.allow_creation {
        return Err(refused("owner.creation_settings_consent_required"));
    }
    let owners = store.owner_enrollment()?;
    let current = owners.snapshot()?;
    let enrolled = if current
        .keys
        .iter()
        .any(|key| key.public_key == consent.owner_public_key && !key.revoked)
    {
        current
    } else {
        owners.enroll(
            current.revision,
            consent.owner_public_key,
            &consent.confirmed_owner_fingerprint,
        )?
    };
    owners.approve_permissions(enrolled.revision, true, consent.permissions)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunState {
    Starting,
    Listening,
    Stopped,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunFailure {
    AuthenticatedBundleRequired,
    CurrentOwnerConsentRequired,
    DaemonBoundaryUnavailable,
    RelayUnavailable,
    InstallationBusy,
    ConsentChanged,
    StreamClosed,
    RuntimeFailed,
}

/// Durable last-run history, never a process liveness or endpoint-ready proof.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    version: u16,
    pub installation: String,
    pub epoch: u64,
    pub run_id: String,
    pub state: RunState,
    pub failure: Option<RunFailure>,
}

pub fn last_run(store: &Store) -> io::Result<Option<RunRecord>> {
    store.with_locked(|directory, snapshot| {
        let bytes = match storage::read_private(directory, Path::new(RECORD), 8192) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let record: RunRecord =
            serde_json::from_slice(&bytes).map_err(|_| refused("host.invalid_runtime_journal"))?;
        if record.version != 1
            || record.installation != snapshot.installation
            || uuid::Uuid::parse_str(&record.run_id).is_err()
            || record.epoch > snapshot.epoch
            || (record.state == RunState::Failed) != record.failure.is_some()
        {
            return Err(refused("host.invalid_runtime_journal"));
        }
        Ok(Some(record))
    })
}

fn refused(reason: &'static str) -> io::Error {
    io::Error::other(reason)
}

fn persist(store: &Store, record: &RunRecord) -> io::Result<()> {
    store.with_locked(|directory, snapshot| {
        if record.installation != snapshot.installation {
            return Err(refused("host.runtime_installation_changed"));
        }
        storage::replace_private(
            directory,
            Path::new(RECORD),
            &serde_json::to_vec(record).map_err(|_| refused("host.invalid_runtime_journal"))?,
        )
    })
}

fn reserve(store: &Store) -> io::Result<(Lease, Snapshot, RunRecord)> {
    store.with_locked(|directory, snapshot| {
        if snapshot.phase != Phase::Provisioned {
            return Err(refused("host.runtime_current_provisioned_consent_required"));
        }
        let file = match storage::open_private_file(directory, Path::new(LOCK), false) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                storage::open_private_file(directory, Path::new(LOCK), true)?
            }
            Err(error) => return Err(error),
        };
        let lease = Lease::exclusive(file)?;
        Ok((
            lease,
            snapshot.clone(),
            RunRecord {
                version: 1,
                installation: snapshot.installation.clone(),
                epoch: snapshot.epoch,
                run_id: uuid::Uuid::new_v4().to_string(),
                state: RunState::Starting,
                failure: None,
            },
        ))
    })
}

/// Run explicitly; no sign-in, shell creation or enrollment occurs here.
/// The independent persistent daemon is never stopped by host cancellation.
pub async fn run(store: Arc<Store>, state: &Path, cancel: CancellationToken) -> io::Result<()> {
    let (_run_lease, snapshot, mut record) = reserve(&store)?;
    persist(&store, &record)?;
    let result = run_reserved(store.clone(), state, &snapshot, &mut record, cancel.clone()).await;
    let result = if cancel.is_cancelled() {
        Ok(())
    } else {
        result
    };
    match &result {
        Ok(()) => {
            record.state = RunState::Stopped;
            record.failure = None;
        }
        Err(error) => {
            record.state = RunState::Failed;
            record.failure = Some(match error.kind() {
                io::ErrorKind::WouldBlock => RunFailure::InstallationBusy,
                _ => match error.to_string().as_str() {
                    "host.runtime_consent_changed" => RunFailure::ConsentChanged,
                    "host.runtime_stream_closed" => RunFailure::StreamClosed,
                    _ => record.failure.take().unwrap_or(RunFailure::RuntimeFailed),
                },
            });
        }
    }
    persist(&store, &record)?;
    result
}

async fn run_reserved(
    store: Arc<Store>,
    state: &Path,
    expected: &Snapshot,
    record: &mut RunRecord,
    cancel: CancellationToken,
) -> io::Result<()> {
    if cancel.is_cancelled() {
        return Ok(());
    }
    record.failure = Some(RunFailure::AuthenticatedBundleRequired);
    let stage = Services::current_user(&store, state)?.reopen_stage(expected.epoch)?;
    let helper = stage.sessiond_path()?;
    record.failure = Some(RunFailure::CurrentOwnerConsentRequired);
    {
        let current = store.owner_enrollment()?.snapshot()?;
        if !current.remote_access || !current.keys.iter().any(|key| !key.revoked) {
            return Err(refused("host.runtime_current_owner_consent_required"));
        }
    }
    let policy = Arc::new(CurrentOwnerPolicySource::new(
        store.clone(),
        OwnerStorage::NativeVault,
    ));
    drop(policy.current_policy().map_err(|error| {
        if error == ContractError::Busy {
            io::Error::new(io::ErrorKind::WouldBlock, "host.installation_busy")
        } else {
            refused("host.runtime_current_owner_consent_required")
        }
    })?);
    record.failure = Some(RunFailure::DaemonBoundaryUnavailable);
    let sessions = SessionControl::with_helper(&state.join("daemon"), helper)
        .map_err(|_| refused("host.runtime_daemon_boundary_unavailable"))?;
    let runtime = Arc::new(HostRuntime::new(sessions, policy));
    record.failure = Some(RunFailure::RelayUnavailable);
    let mut relay = CloudHostAdapter::new(&store).open_relay(&cancel).await?;
    stage.sessiond_path()?;
    if store.snapshot()? != *expected {
        return Err(refused("host.runtime_consent_changed"));
    }
    record.state = RunState::Listening;
    record.failure = None;
    persist(&store, record)?;
    let shutdown = CancellationToken::new();
    let served = runtime.serve(&mut relay, shutdown.clone());
    tokio::pin!(served);
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    let mut terminated = false;
    let outcome = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break Ok(()),
            _ = interval.tick() => match store.snapshot() {
                Ok(snapshot) if snapshot == *expected => {}
                Ok(_) => break Err(refused("host.runtime_consent_changed")),
                Err(error) => break Err(error),
            },
            _ = &mut served => {
                terminated = true;
                break Err(refused("host.runtime_stream_closed"));
            },
        }
    };
    shutdown.cancel();
    if !terminated {
        let _ = served.await;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::{Binding, Origin, Settings, Startup};

    fn fixture() -> (storage::PrivateTestDirectory, Arc<Store>) {
        let root = storage::private_test_directory();
        let store = Arc::new(
            Store::create(
                &root.path().join("host"),
                Origin::Headless,
                Settings {
                    friendly_name: "Runtime fixture".into(),
                    startup: Startup::Manual,
                    allow_creation: false,
                },
            )
            .unwrap(),
        );
        (root, store)
    }

    fn provision(store: &Store) {
        let snapshot = store.snapshot().unwrap();
        let ticket = store.request_enable(&snapshot.settings).unwrap();
        store
            .record_provisioned(
                &ticket,
                Binding {
                    host_id: uuid::Uuid::new_v4().to_string(),
                    account: "42".into(),
                    qualified_tunnel: "fixture-cluster/fixture-tunnel".into(),
                    cloud_incarnation: "fixture-incarnation".into(),
                    host_key_fingerprint: "fixture-pin".into(),
                },
            )
            .unwrap();
    }

    #[test]
    fn host_runtime_reservation_is_exclusive_without_holding_installation_lease() {
        let (_root, store) = fixture();
        assert!(reserve(&store).is_err());
        provision(&store);
        let (lease, snapshot, record) = reserve(&store).unwrap();
        persist(&store, &record).unwrap();
        assert!(matches!(reserve(&store), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
        assert_eq!(store.snapshot().unwrap(), snapshot);
        store.revoke().unwrap();
        assert_eq!(store.snapshot().unwrap().phase, Phase::StopRequested);
        assert_eq!(last_run(&store).unwrap().unwrap(), record);
        drop(lease);
        assert!(reserve(&store).is_err());
    }

    #[tokio::test]
    async fn host_runtime_cancelled_start_never_opens_relay_and_persists_stopped_history() {
        let (root, store) = fixture();
        provision(&store);
        let cancel = CancellationToken::new();
        cancel.cancel();
        run(store.clone(), &root.path().join("host"), cancel)
            .await
            .unwrap();
        let record = last_run(&store).unwrap().unwrap();
        assert_eq!(record.state, RunState::Stopped);
        assert_eq!(record.failure, None);
        assert!(!root.path().join("host").join("daemon").exists());
        assert!(!root.path().join("host").join("host-cloud.json").exists());
        assert_eq!(store.snapshot().unwrap().phase, Phase::Provisioned);
    }

    #[test]
    fn host_runtime_history_rejects_foreign_or_inconsistent_records() {
        let (_root, store) = fixture();
        assert_eq!(last_run(&store).unwrap(), None);
        provision(&store);
        let (_lease, _snapshot, mut record) = reserve(&store).unwrap();
        record.state = RunState::Failed;
        persist(&store, &record).unwrap();
        assert!(last_run(&store).is_err());
        record.failure = Some(RunFailure::RelayUnavailable);
        persist(&store, &record).unwrap();
        assert_eq!(last_run(&store).unwrap(), Some(record.clone()));
        store
            .with_locked(|directory, _| {
                record.installation = uuid::Uuid::new_v4().to_string();
                storage::replace_private(
                    directory,
                    Path::new(RECORD),
                    &serde_json::to_vec(&record).unwrap(),
                )
            })
            .unwrap();
        assert!(last_run(&store).is_err());
    }
}
