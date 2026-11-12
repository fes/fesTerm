//! Shared explicit foreground setup and restartable host composition.

mod linux_login;

use crate::{
    cloud::{Cleanup, CloudHostAdapter, Foreground, HOST_IDENTITY_FILE, Health},
    enrollment::{
        CurrentOwnerPolicySource, EnrollmentSnapshot, OwnerEnrollment, OwnerStorage,
        owner_key_fingerprint,
    },
    lifecycle::{Phase, Snapshot, Startup, Store},
    protected_state::Lease,
    services::{Observation, ProcessState, Services, TrustedStage},
};
use festerm_host_protocol::{Capabilities, ContractError};
use festerm_sessiond::{host_control::SessionControl, private_storage as storage};
use festerm_tunnel_bridge::host_runtime::HostRuntime;
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const LOCK: &str = "host-runtime.lock";
const RECORD: &str = "host-runtime.json";
const RELAY_RETRY_DELAY: Duration = Duration::from_secs(15);

/// Protected revocation is the stop request; runtime-lease release and exact
/// native task quiescence together are completion, never a PID/name or history.
pub(crate) fn revoked_runtime_released(store: &Store, expected: &Snapshot) -> io::Result<bool> {
    store.with_locked(|directory, current| {
        if current != expected || current.phase != Phase::StopRequested {
            return Err(refused("host.runtime_consent_changed"));
        }
        read_run_record(directory, current)?;
        match storage::open_private_file(directory, Path::new(LOCK), false) {
            Ok(file) => match Lease::exclusive(file) {
                Ok(_lease) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
                Err(error) => Err(error),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match storage::open_private_file(directory, Path::new(RECORD), false) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
                    _ => Err(refused("host.runtime_lock_missing_reconciliation_required")),
                }
            }
            Err(error) => Err(error),
        }
    })
}

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

pub(crate) struct SetupRequest<'a> {
    pub(crate) store: &'a Store,
    pub(crate) state: &'a Path,
    pub(crate) stage: &'a TrustedStage,
    pub(crate) account: u64,
    pub(crate) cancel: &'a CancellationToken,
}

pub(crate) trait SetupBoundary {
    #[cfg(test)]
    fn fixture_cloud<'a>(&self, store: &'a Store) -> CloudHostAdapter<'a> {
        CloudHostAdapter::new(store)
    }

    async fn sign_in(
        &self,
        cloud: &CloudHostAdapter<'_>,
        foreground: &Foreground,
        cancel: &CancellationToken,
        prompt: impl FnOnce(festerm_tunnel_client::identity::DevicePrompt),
    ) -> io::Result<()>;
    async fn provision(
        &self,
        cloud: &CloudHostAdapter<'_>,
        foreground: &Foreground,
        cancel: &CancellationToken,
    ) -> io::Result<()>;
    async fn health(
        &self,
        cloud: &CloudHostAdapter<'_>,
        cancel: &CancellationToken,
    ) -> io::Result<Health>;
    fn owners<'a>(&self, store: &'a Store) -> io::Result<OwnerEnrollment<'a>>;
    fn install(
        &self,
        store: &Store,
        state: &Path,
        stage: &TrustedStage,
        epoch: u64,
    ) -> io::Result<Observation>;
}

struct NativeBoundary;

impl SetupBoundary for NativeBoundary {
    async fn sign_in(
        &self,
        cloud: &CloudHostAdapter<'_>,
        foreground: &Foreground,
        cancel: &CancellationToken,
        prompt: impl FnOnce(festerm_tunnel_client::identity::DevicePrompt),
    ) -> io::Result<()> {
        cloud.sign_in(foreground, cancel, prompt).await
    }

    async fn provision(
        &self,
        cloud: &CloudHostAdapter<'_>,
        foreground: &Foreground,
        cancel: &CancellationToken,
    ) -> io::Result<()> {
        cloud.provision(foreground, cancel).await.map(|_| ())
    }

    async fn health(
        &self,
        cloud: &CloudHostAdapter<'_>,
        cancel: &CancellationToken,
    ) -> io::Result<Health> {
        cloud.health(cancel).await
    }

    fn owners<'a>(&self, store: &'a Store) -> io::Result<OwnerEnrollment<'a>> {
        store.owner_enrollment()
    }

    fn install(
        &self,
        store: &Store,
        state: &Path,
        stage: &TrustedStage,
        epoch: u64,
    ) -> io::Result<Observation> {
        Services::current_user(store, state)?.install(stage, epoch)
    }
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
    setup_with(
        SetupRequest {
            store,
            state,
            stage,
            account,
            cancel,
        },
        prompt,
        &NativeBoundary,
    )
    .await
}

pub struct SetupConsent {
    pub expected: Snapshot,
    pub settings: crate::lifecycle::Settings,
    pub account: u64,
}

/// Desktop consent stays tied to the observed installation even while signed
/// package verification, foreground sign-in or native startup is delayed.
pub async fn setup_if_current(
    store: &Store,
    state: &Path,
    stage: &TrustedStage,
    consent: &SetupConsent,
    cancel: &CancellationToken,
    prompt: impl FnOnce(festerm_tunnel_client::identity::DevicePrompt),
    reserved: impl FnOnce(&crate::lifecycle::Ticket),
) -> io::Result<SetupResult> {
    setup_scoped_with(
        SetupRequest {
            store,
            state,
            stage,
            account: consent.account,
            cancel,
        },
        prompt,
        &NativeBoundary,
        Some(consent),
        reserved,
    )
    .await
}

pub(crate) async fn setup_with(
    request: SetupRequest<'_>,
    prompt: impl FnOnce(festerm_tunnel_client::identity::DevicePrompt),
    boundary: &impl SetupBoundary,
) -> io::Result<SetupResult> {
    setup_scoped_with(request, prompt, boundary, None, |_| {}).await
}

pub(crate) async fn setup_scoped_with(
    request: SetupRequest<'_>,
    prompt: impl FnOnce(festerm_tunnel_client::identity::DevicePrompt),
    boundary: &impl SetupBoundary,
    consent: Option<&SetupConsent>,
    reserved: impl FnOnce(&crate::lifecycle::Ticket),
) -> io::Result<SetupResult> {
    let SetupRequest {
        store,
        state,
        stage,
        account,
        cancel,
    } = request;
    let before = store.snapshot()?;
    if consent.is_some_and(|consent| consent.expected != before) {
        return Err(refused("host.foreground_scope_changed"));
    }
    let settings = consent.map_or(&before.settings, |consent| &consent.settings);
    startup_admission(settings.startup, cancel).await?;
    if store.snapshot()? != before {
        return Err(refused("host.foreground_scope_changed"));
    }
    stage.sessiond_path()?;
    if account == 0 || cancel.is_cancelled() {
        return Err(refused("host.foreground_account_consent_required"));
    }
    #[cfg(not(test))]
    let cloud = CloudHostAdapter::new(store);
    #[cfg(test)]
    let cloud = boundary.fixture_cloud(store);
    let expected_epoch = match before.phase {
        Phase::Disabled => {
            let ticket = match consent {
                Some(consent) => store.request_enable_if_current(&consent.expected, settings)?,
                None => store.request_enable(settings)?,
            };
            reserved(&ticket);
            if cancel.is_cancelled() {
                return Err(refused("host.cloud_cancelled"));
            }
            let foreground = cloud.foreground(&ticket, account)?;
            boundary
                .sign_in(&cloud, &foreground, cancel, prompt)
                .await?;
            boundary.provision(&cloud, &foreground, cancel).await?;
            ticket.epoch()
        }
        Phase::Provisioned => {
            if settings != &before.settings {
                return Err(refused("host.disable_before_configuration"));
            }
            if before
                .binding
                .as_ref()
                .map(|binding| binding.account.as_str())
                != Some(crate::cloud::subject(account).as_str())
                || !matches!(
                    boundary.health(&cloud, cancel).await?,
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
    let current = store.snapshot()?;
    if current.epoch != expected_epoch || current.phase != Phase::Provisioned {
        return Err(refused("host.runtime_consent_changed"));
    }
    let account = current
        .binding
        .ok_or_else(|| refused("host.cloud_binding_changed"))?
        .account;
    let owners = boundary.owners(store)?;
    let initial = match owners.snapshot() {
        Ok(existing) => existing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            owners.initialize_at_epoch(Path::new(HOST_IDENTITY_FILE), account, expected_epoch)?
        }
        Err(error) => return Err(error),
    };
    let native_registration = boundary.install(store, state, stage, expected_epoch)?;
    Ok(SetupResult {
        native_registration,
        owner_approval_prerequisite: initial,
    })
}

pub fn approve_owner(store: &Store, consent: OwnerConsent) -> io::Result<EnrollmentSnapshot> {
    approve_owner_with(store, consent, &NativeBoundary)
}

pub fn approve_owner_if_current(
    store: &Store,
    expected: &Snapshot,
    expected_policy_revision: u64,
    consent: OwnerConsent,
) -> io::Result<EnrollmentSnapshot> {
    validate_owner_consent(&consent)?;
    store.owner_enrollment()?.approve_key_if_current(
        expected,
        expected_policy_revision,
        consent.owner_public_key,
        &consent.confirmed_owner_fingerprint,
        consent.permissions,
    )
}

fn validate_owner_consent(consent: &OwnerConsent) -> io::Result<()> {
    consent
        .permissions
        .validate_permissions()
        .map_err(|_| refused("host.invalid_owner_permissions"))?;
    if consent.permissions.issuance_epoch.is_some()
        || owner_key_fingerprint(&consent.owner_public_key)? != consent.confirmed_owner_fingerprint
    {
        return Err(refused("host.explicit_owner_confirmation_required"));
    }
    Ok(())
}

pub(crate) fn approve_owner_with(
    store: &Store,
    consent: OwnerConsent,
    boundary: &impl SetupBoundary,
) -> io::Result<EnrollmentSnapshot> {
    validate_owner_consent(&consent)?;
    let before = store.snapshot()?;
    if before.phase != Phase::Provisioned {
        return Err(refused("host.runtime_current_provisioned_consent_required"));
    }
    if consent.permissions.creation_allowed && !before.settings.allow_creation {
        return Err(refused("owner.creation_settings_consent_required"));
    }
    let owners = boundary.owners(store)?;
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

#[derive(Serialize)]
pub struct DisableResult {
    pub lifecycle: Snapshot,
    pub native_process: Option<Observation>,
}

/// Revoke first, observe exact manager stop and released foreground-host lease.
/// Neither native daemon nor persistent sessions are stop targets.
pub fn disable(store: &Store, state: &Path) -> io::Result<DisableResult> {
    disable_with(store, Duration::from_secs(10), |epoch| {
        Services::current_user(store, state)?.stop(epoch)
    })
}

pub fn disable_if_current(
    store: &Store,
    state: &Path,
    expected: &Snapshot,
) -> io::Result<DisableResult> {
    disable_scoped_with(store, Some(expected), Duration::from_secs(10), |epoch| {
        Services::current_user(store, state)?.stop(epoch)
    })
}

fn disable_with(
    store: &Store,
    wait: Duration,
    stop: impl FnOnce(u64) -> io::Result<Observation>,
) -> io::Result<DisableResult> {
    disable_scoped_with(store, None, wait, stop)
}

fn disable_scoped_with(
    store: &Store,
    expected: Option<&Snapshot>,
    wait: Duration,
    stop: impl FnOnce(u64) -> io::Result<Observation>,
) -> io::Result<DisableResult> {
    let before = store.snapshot()?;
    if expected.is_some_and(|expected| expected != &before) {
        return Err(refused("host.foreground_scope_changed"));
    }
    let stopped = match before.phase {
        Phase::Removed => return Err(refused("host.installation_removed")),
        Phase::CleanupPending => return Err(refused("host.cleanup_already_pending")),
        Phase::StopRequested => before,
        _ => store.revoke_if_current(&before)?,
    };
    if stopped.phase == Phase::Reconciling {
        return Err(refused("host.cloud_reconciliation_required"));
    }
    let Some(binding) = stopped.binding.as_ref() else {
        return Ok(DisableResult {
            lifecycle: stopped,
            native_process: None,
        });
    };
    let observation = stop(stopped.epoch)?;
    if observation.installation != stopped.installation
        || observation.origin != stopped.origin
        || observation.epoch != stopped.epoch
        || !matches!(
            observation.process,
            ProcessState::Absent | ProcessState::Stopped | ProcessState::Failed
        )
    {
        return Err(refused("host.service_stop_not_observed"));
    }
    let deadline = Instant::now() + wait;
    loop {
        let absent = store.with_locked(|directory, current| {
            if current != &stopped {
                return Err(refused("host.runtime_consent_changed"));
            }
            match storage::open_private_file(directory, Path::new(LOCK), false) {
                Ok(file) => Lease::exclusive(file).map(Some),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match storage::open_private_file(directory, Path::new(RECORD), false) {
                        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                        _ => Err(refused("host.runtime_lock_missing_reconciliation_required")),
                    }
                }
                Err(error) => Err(error),
            }
        });
        match absent {
            Ok(_lease) => break,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "host.runtime_stop_pending",
                    ));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error),
        }
    }
    let lifecycle = store.confirm_stopped_snapshot(stopped.epoch, binding)?;
    Ok(DisableResult {
        lifecycle,
        native_process: Some(observation),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UninstallResult {
    Removed,
    AlreadyRemovedNotReobserved,
    NativeRemovedCloudCleanupPending {
        context: crate::cloud::CleanupContext,
    },
}

/// Remove exact stopped registration, then reconcile authenticated cloud state.
/// ID-only cloud DELETE is never used to manufacture conditional-delete proof.
pub async fn uninstall(
    store: &Store,
    state: &Path,
    cancel: &CancellationToken,
) -> io::Result<UninstallResult> {
    let cloud = CloudHostAdapter::new(store);
    uninstall_with(store, state, cancel, cloud.cleanup(cancel)).await
}

pub async fn uninstall_if_current(
    store: &Store,
    state: &Path,
    expected: &Snapshot,
    cancel: &CancellationToken,
) -> io::Result<UninstallResult> {
    let cloud = CloudHostAdapter::new(store);
    uninstall_scoped_with(store, state, Some(expected), cancel, cloud.cleanup(cancel)).await
}

pub(crate) async fn uninstall_with(
    store: &Store,
    state: &Path,
    cancel: &CancellationToken,
    cleanup: impl std::future::Future<Output = io::Result<Cleanup>>,
) -> io::Result<UninstallResult> {
    uninstall_scoped_with(store, state, None, cancel, cleanup).await
}

async fn uninstall_scoped_with(
    store: &Store,
    state: &Path,
    expected: Option<&Snapshot>,
    cancel: &CancellationToken,
    cleanup: impl std::future::Future<Output = io::Result<Cleanup>>,
) -> io::Result<UninstallResult> {
    uninstall_scoped_native(
        store,
        expected,
        cancel,
        |before| disable_if_current(store, state, before),
        |epoch| Services::current_user(store, state)?.cleanup(epoch),
        cleanup,
    )
    .await
}

#[cfg(test)]
async fn uninstall_with_native(
    store: &Store,
    cancel: &CancellationToken,
    stop: impl FnOnce() -> io::Result<DisableResult>,
    native_cleanup: impl FnOnce(u64) -> io::Result<Observation>,
    cleanup: impl std::future::Future<Output = io::Result<Cleanup>>,
) -> io::Result<UninstallResult> {
    uninstall_scoped_native(store, None, cancel, |_| stop(), native_cleanup, cleanup).await
}

async fn uninstall_scoped_native(
    store: &Store,
    expected: Option<&Snapshot>,
    cancel: &CancellationToken,
    stop: impl FnOnce(&Snapshot) -> io::Result<DisableResult>,
    native_cleanup: impl FnOnce(u64) -> io::Result<Observation>,
    cleanup: impl std::future::Future<Output = io::Result<Cleanup>>,
) -> io::Result<UninstallResult> {
    let before = store.snapshot()?;
    if expected.is_some_and(|expected| expected != &before) {
        return Err(refused("host.foreground_scope_changed"));
    }
    if cancel.is_cancelled() {
        return Err(refused("host.cloud_cancelled"));
    }
    if before.phase == Phase::Removed {
        return Ok(UninstallResult::AlreadyRemovedNotReobserved);
    }
    let expected_cleanup = if before.phase != Phase::CleanupPending {
        let disabled = stop(&before)?;
        store.request_cleanup_if_current(&disabled.lifecycle)?
    } else {
        before
    };
    native_cleanup(expected_cleanup.epoch)?;
    let outcome = cleanup.await?;
    let mut expected_after = expected_cleanup;
    if matches!(outcome, Cleanup::Removed) {
        expected_after.phase = Phase::Removed;
    }
    if store.snapshot()? != expected_after {
        return Err(refused("host.foreground_scope_changed"));
    }
    match outcome {
        Cleanup::Removed => Ok(UninstallResult::Removed),
        Cleanup::GenerationConditionalDeleteUnavailable { context } => {
            Ok(UninstallResult::NativeRemovedCloudCleanupPending { context })
        }
    }
}

#[cfg(test)]
pub(crate) fn fixture_disable(store: &Store, state: &Path) -> io::Result<DisableResult> {
    disable_with(store, Duration::ZERO, |epoch| {
        crate::services::fixture_services(store, state)?.stop(epoch)
    })
}

#[cfg(test)]
pub(crate) async fn fixture_uninstall(
    store: &Store,
    state: &Path,
    cancel: &CancellationToken,
    cleanup: impl std::future::Future<Output = io::Result<Cleanup>>,
) -> io::Result<UninstallResult> {
    uninstall_with_native(
        store,
        cancel,
        || fixture_disable(store, state),
        |epoch| crate::services::fixture_services(store, state)?.cleanup(epoch),
        cleanup,
    )
    .await
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
    LoginProofUnavailable,
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
    store.with_locked(read_run_record)
}

fn read_run_record(
    directory: &std::fs::File,
    snapshot: &Snapshot,
) -> io::Result<Option<RunRecord>> {
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
}

/// Use the independently delivered new release's updater after explicit disable.
/// No sign-in, owner approval, grant reconstruction or automatic reenable.
pub async fn update(
    store: &Store,
    state: &Path,
    stage: &TrustedStage,
    from_version: &str,
    cancel: &CancellationToken,
) -> io::Result<crate::services::ReplacementReport> {
    update_with_admission(store, state, stage, from_version, cancel, |startup| {
        startup_admission(startup, cancel)
    })
    .await
}

async fn update_with_admission<A: std::future::Future<Output = io::Result<()>>>(
    store: &Store,
    state: &Path,
    stage: &TrustedStage,
    from_version: &str,
    cancel: &CancellationToken,
    admission: impl FnOnce(Startup) -> A,
) -> io::Result<crate::services::ReplacementReport> {
    let snapshot = store.snapshot()?;
    if cancel.is_cancelled() {
        return Err(refused("host.cloud_cancelled"));
    }
    admission(snapshot.settings.startup).await?;
    if cancel.is_cancelled() {
        return Err(refused("host.cloud_cancelled"));
    }
    if snapshot.phase != Phase::Disabled || snapshot.binding.is_none() {
        return Err(refused("host.update_confirmed_stop_required"));
    }
    store.with_locked(|directory, current| {
        if current != &snapshot {
            return Err(refused("host.runtime_consent_changed"));
        }
        let identity =
            festerm_tunnel_bridge::host_identity::HostIdentity::load_from_private_directory(
                directory,
                Path::new(HOST_IDENTITY_FILE),
            )?;
        let binding = current.binding.as_ref().unwrap();
        if identity.host_id() != binding.host_id
            || identity.fingerprint()? != binding.host_key_fingerprint
        {
            return Err(refused("host.cloud_identity_changed"));
        }
        Ok(())
    })?;
    Services::current_user(store, state)?.replace_stopped(stage, from_version, snapshot.epoch)
}

fn refused(reason: &'static str) -> io::Error {
    io::Error::other(reason)
}

async fn startup_admission(startup: Startup, cancel: &CancellationToken) -> io::Result<()> {
    if linux_login::required(std::env::consts::OS, startup) {
        linux_login::admit(&linux_login::Logind, cancel).await?;
    }
    Ok(())
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
                match storage::open_private_file(directory, Path::new(RECORD), false) {
                    Ok(_) => {
                        return Err(refused("host.runtime_lock_missing_reconciliation_required"));
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
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
    let result = if cancel.is_cancelled() && result.as_ref().is_err_and(crate::cloud::is_cancelled)
    {
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
    let stage = authorize_run(&store, state, expected, record, &store.snapshot()?)?;
    record.failure = Some(RunFailure::LoginProofUnavailable);
    let hosting_cancel = cancel.child_token();
    let result = if let Err(error) = startup_admission(expected.settings.startup, &cancel).await {
        if cancel.is_cancelled() {
            Ok(())
        } else {
            Err(error)
        }
    } else if linux_login::required(std::env::consts::OS, expected.settings.startup) {
        let hosting = async {
            let result = run_admitted(
                store.clone(),
                state,
                expected,
                record,
                &stage,
                hosting_cancel.clone(),
            )
            .await;
            if hosting_cancel.is_cancelled()
                && result.as_ref().is_err_and(crate::cloud::is_cancelled)
            {
                Ok(())
            } else {
                result
            }
        };
        linux_login::guard(
            &linux_login::Logind,
            cancel,
            hosting_cancel.clone(),
            hosting,
        )
        .await
    } else {
        run_admitted(
            store.clone(),
            state,
            expected,
            record,
            &stage,
            hosting_cancel,
        )
        .await
    };
    if result.as_ref().is_err_and(|error| {
        crate::run_exit::error_exit_code(error)
            == crate::run_exit::LOGIN_PROOF_UNAVAILABLE_EXIT_CODE
    }) {
        // Login loss never authorizes retry after a concurrent revoke or
        // bundle/owner change while the owned hosting future was draining.
        record.failure = Some(RunFailure::RuntimeFailed);
        let current = login_retry_snapshot(&store).await?;
        authorize_run(&store, state, expected, record, &current)?;
        record.failure = Some(RunFailure::LoginProofUnavailable);
    }
    result
}

async fn login_retry_snapshot(store: &Store) -> io::Result<Snapshot> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
    loop {
        match store.snapshot() {
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep_until(
                    (tokio::time::Instant::now() + Duration::from_millis(25)).min(deadline),
                )
                .await;
            }
            result => return result,
        }
    }
}

fn authorize_run(
    store: &Arc<Store>,
    state: &Path,
    expected: &Snapshot,
    record: &mut RunRecord,
    current: &Snapshot,
) -> io::Result<crate::services::TrustedStage> {
    record.failure = Some(RunFailure::RuntimeFailed);
    if current != expected {
        return Err(refused("host.runtime_consent_changed"));
    }
    record.failure = Some(RunFailure::AuthenticatedBundleRequired);
    let stage = Services::current_user(store, state)?.reopen_stage(expected.epoch)?;
    stage.sessiond_path()?;
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
    Ok(stage)
}

async fn run_admitted(
    store: Arc<Store>,
    state: &Path,
    expected: &Snapshot,
    record: &mut RunRecord,
    stage: &crate::services::TrustedStage,
    cancel: CancellationToken,
) -> io::Result<()> {
    if cancel.is_cancelled() {
        return Ok(());
    }
    record.failure = Some(RunFailure::AuthenticatedBundleRequired);
    let helper = stage.sessiond_path()?;
    let policy = Arc::new(CurrentOwnerPolicySource::new(
        store.clone(),
        OwnerStorage::NativeVault,
    ));
    record.failure = Some(RunFailure::DaemonBoundaryUnavailable);
    let sessions = SessionControl::with_helper(&state.join("daemon"), helper)
        .map_err(|_| refused("host.runtime_daemon_boundary_unavailable"))?;
    let host = RunningHost {
        store: &store,
        state,
        expected,
        stage,
        runtime: Arc::new(HostRuntime::new(sessions, policy)),
    };
    let shutdown = cancel.child_token();
    let recovering = recover_relay(&store, expected, record, &shutdown, async |record| {
        host.attempt(record, &shutdown).await
    });
    supervise(&store, expected, cancel, shutdown.clone(), recovering).await
}

struct RunningHost<'a> {
    store: &'a Arc<Store>,
    state: &'a Path,
    expected: &'a Snapshot,
    stage: &'a TrustedStage,
    runtime: Arc<HostRuntime<CurrentOwnerPolicySource>>,
}

impl RunningHost<'_> {
    fn authorize(&self, record: &mut RunRecord) -> io::Result<()> {
        let stage = authorize_run(
            self.store,
            self.state,
            self.expected,
            record,
            &self.store.snapshot()?,
        )?;
        record.failure = Some(RunFailure::AuthenticatedBundleRequired);
        if stage.sessiond_path()? != self.stage.sessiond_path()? {
            return Err(refused("host.service_authenticated_bundle_changed"));
        }
        Ok(())
    }

    async fn attempt(&self, record: &mut RunRecord, cancel: &CancellationToken) -> io::Result<()> {
        self.authorize(record)?;
        record.failure = Some(RunFailure::RelayUnavailable);
        let mut relay = CloudHostAdapter::new(self.store).open_relay(cancel).await?;
        let served = async {
            startup_admission(self.expected.settings.startup, cancel).await?;
            self.authorize(record)?;
            if cancel.is_cancelled() {
                return Ok(Err(festerm_tunnel_client::host_control::Error::Cancelled));
            }
            record.state = RunState::Listening;
            record.failure = None;
            persist(self.store, record)?;
            Ok::<_, io::Error>(self.runtime.clone().serve(&mut relay, cancel.clone()).await)
        }
        .await;
        relay.close().await?;
        match served? {
            Err(festerm_tunnel_client::host_control::Error::Cancelled) if cancel.is_cancelled() => {
                Ok(())
            }
            Err(festerm_tunnel_client::host_control::Error::Closed)
                if relay.failure_disposition()
                    == Some(festerm_tunnel_bridge::relay::HostFailureDisposition::Transient) =>
            {
                Err(crate::cloud::transient_relay("host.runtime_stream_closed"))
            }
            _ => Err(refused("host.runtime_stream_closed")),
        }
    }
}

async fn recover_relay(
    store: &Store,
    expected: &Snapshot,
    record: &mut RunRecord,
    cancel: &CancellationToken,
    mut attempt: impl std::ops::AsyncFnMut(&mut RunRecord) -> io::Result<()>,
) -> io::Result<()> {
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        if store.snapshot()? != *expected {
            return Err(refused("host.runtime_consent_changed"));
        }
        record.state = RunState::Starting;
        record.failure = None;
        persist(store, record)?;
        match attempt(record).await {
            Err(error) if crate::cloud::is_transient_relay(&error) => {
                record.state = RunState::Starting;
                record.failure = None;
                persist(store, record)?;
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(RELAY_RETRY_DELAY) => {}
                }
            }
            result => return result,
        }
    }
}

async fn supervise(
    store: &Store,
    expected: &Snapshot,
    cancel: CancellationToken,
    shutdown: CancellationToken,
    served: impl std::future::Future<Output = io::Result<()>>,
) -> io::Result<()> {
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
                // A short current-policy guard owns this same installation
                // lock. Contention is unobservable state, not revocation.
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => break Err(error),
            },
            result = &mut served => {
                terminated = true;
                break result;
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
    use crate::lifecycle::{Binding, Origin, Settings};

    #[tokio::test(start_paused = true)]
    async fn host_runtime_relay_recovery_keeps_run_identity_lease_and_fixed_delay() {
        let (root, store) = fixture();
        provision(&store);
        let (_lease, expected, mut record) = reserve(&store).unwrap();
        let original = record.clone();
        let calls = std::cell::Cell::new(0);
        let cancel = CancellationToken::new();
        let started = tokio::time::Instant::now();
        {
            let recovering = recover_relay(
                &store,
                &expected,
                &mut record,
                &cancel,
                async |record| {
                    calls.set(calls.get() + 1);
                    assert_eq!(record.run_id, original.run_id);
                    assert_eq!(record.epoch, original.epoch);
                    assert_eq!(record.installation, original.installation);
                    assert_eq!(record.state, RunState::Starting);
                    assert!(record.failure.is_none());
                    assert!(
                        matches!(reserve(&store), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
                    );
                    if calls.get() == 1 {
                        record.state = RunState::Listening;
                        persist(&store, record).unwrap();
                        Err(crate::cloud::transient_relay("host.runtime_stream_closed"))
                    } else {
                        Err(refused("host.fixture_terminal"))
                    }
                },
            );
            tokio::pin!(recovering);
            assert!(
                tokio::time::timeout(Duration::from_secs(14), &mut recovering)
                    .await
                    .is_err()
            );
            assert_eq!(calls.get(), 1);
            assert_eq!(last_run(&store).unwrap().unwrap(), original);
            assert_eq!(
                recovering.await.unwrap_err().to_string(),
                "host.fixture_terminal"
            );
        }
        assert_eq!(started.elapsed(), Duration::from_secs(15));
        assert_eq!(calls.get(), 2);
        assert_eq!(record, original);
        assert_eq!(store.snapshot().unwrap(), expected);
        assert!(!root.path().join("host/daemon").exists());
    }

    #[tokio::test(start_paused = true)]
    async fn host_runtime_relay_recovery_unknown_authority_and_error_text_never_retry() {
        for error in [
            io::Error::other("host.relay_establishment_timeout"),
            io::Error::other("host.cloud_rate_limited"),
            io::Error::other("host.cloud_offline_or_unknown"),
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "host.current_owner_required",
            ),
            io::Error::new(io::ErrorKind::WouldBlock, "host.installation_busy"),
        ] {
            let (root, store) = fixture();
            provision(&store);
            let (_lease, expected, mut record) = reserve(&store).unwrap();
            let original = record.clone();
            let calls = std::cell::Cell::new(0);
            let mut error = Some(error);
            let started = tokio::time::Instant::now();
            let result = recover_relay(
                &store,
                &expected,
                &mut record,
                &CancellationToken::new(),
                async |_| {
                    calls.set(calls.get() + 1);
                    Err(error.take().unwrap())
                },
            )
            .await;
            assert!(result.is_err());
            assert_eq!(calls.get(), 1);
            assert_eq!(started.elapsed(), Duration::ZERO);
            assert_eq!(store.snapshot().unwrap(), expected);
            assert_eq!(record, original);
            assert!(!root.path().join("host/daemon").exists());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn host_runtime_relay_recovery_cancel_and_epoch_watch_span_all_phases() {
        for phase in ["acquisition", "serving", "backoff"] {
            for revoke in [false, true] {
                let (_root, store) = fixture();
                provision(&store);
                let (_lease, expected, mut record) = reserve(&store).unwrap();
                let original = record.clone();
                let cancel = CancellationToken::new();
                let shutdown = cancel.child_token();
                let calls = std::cell::Cell::new(0);
                let drained = std::cell::Cell::new(false);
                {
                    let recovering =
                        recover_relay(&store, &expected, &mut record, &shutdown, async |record| {
                            calls.set(calls.get() + 1);
                            if phase == "backoff" {
                                return Err(crate::cloud::transient_relay(
                                    "host.runtime_stream_closed",
                                ));
                            }
                            if phase == "serving" {
                                record.state = RunState::Listening;
                                persist(&store, record).unwrap();
                            }
                            shutdown.cancelled().await;
                            drained.set(true);
                            Ok(())
                        });
                    let watched = supervise(
                        &store,
                        &expected,
                        cancel.clone(),
                        shutdown.clone(),
                        recovering,
                    );
                    tokio::pin!(watched);
                    assert!(
                        tokio::time::timeout(Duration::from_secs(3), &mut watched)
                            .await
                            .is_err()
                    );
                    assert_eq!(calls.get(), 1);
                    if revoke {
                        store.revoke().unwrap();
                    } else {
                        cancel.cancel();
                    }
                    let result = watched.await;
                    if revoke {
                        assert_eq!(
                            result.unwrap_err().to_string(),
                            "host.runtime_consent_changed"
                        );
                    } else {
                        result.unwrap();
                    }
                }
                assert!(shutdown.is_cancelled());
                assert_eq!(drained.get(), phase != "backoff");
                assert_eq!(calls.get(), 1);
                assert_eq!(record.run_id, original.run_id);
                assert_eq!(record.epoch, original.epoch);
            }
        }
    }

    fn fixture() -> (storage::PrivateTestDirectory, Arc<Store>) {
        fixture_startup(Startup::Manual)
    }

    fn fixture_startup(startup: Startup) -> (storage::PrivateTestDirectory, Arc<Store>) {
        let root = storage::private_test_directory();
        let store = Arc::new(
            Store::create(
                &root.path().join("host"),
                Origin::Headless,
                Settings {
                    friendly_name: "Runtime fixture".into(),
                    startup,
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

    #[tokio::test]
    async fn host_update_admission_revalidates_full_scope_and_cancellation_before_effects() {
        for action in [
            "denied",
            "cancel-before",
            "cancel-after",
            "revoked",
            "same-epoch-settings",
        ] {
            let (root, store) = fixture();
            let state = root.path().join("host");
            provision(&store);
            fixture_disable(&store, &state).unwrap();
            let before = store.snapshot().unwrap();
            assert_eq!(before.phase, Phase::Disabled);
            let stage = crate::services::fixture_stage(&root.path().join("candidate"));
            let cancel = CancellationToken::new();
            if action == "cancel-before" {
                cancel.cancel();
            }
            let calls = std::cell::Cell::new(0);
            let admitted = std::cell::RefCell::new(before.clone());
            let error =
                update_with_admission(&store, &state, &stage, "0.15.0", &cancel, |startup| {
                    assert_eq!(startup, Startup::Manual);
                    async {
                        calls.set(calls.get() + 1);
                        match action {
                            "cancel-after" => cancel.cancel(),
                            "revoked" => {
                                store.revoke().unwrap();
                            }
                            "same-epoch-settings" => {
                                store
                                    .with_locked(|directory, current| {
                                        let mut changed = current.clone();
                                        changed.settings.friendly_name.push_str("-changed");
                                        storage::replace_private(
                                            directory,
                                            Path::new("host-lifecycle.json"),
                                            &serde_json::to_vec(&changed).unwrap(),
                                        )
                                    })
                                    .unwrap();
                            }
                            _ => {}
                        }
                        *admitted.borrow_mut() = store.snapshot().unwrap();
                        if action == "denied" {
                            return Err(io::Error::other(crate::run_exit::LoginProofUnavailable));
                        }
                        Ok(())
                    }
                })
                .await
                .err()
                .unwrap();
            assert_eq!(calls.get(), usize::from(action != "cancel-before"));
            assert_eq!(store.snapshot().unwrap(), *admitted.borrow());
            if action == "denied" {
                assert_eq!(
                    crate::run_exit::error_exit_code(&error),
                    crate::run_exit::LOGIN_PROOF_UNAVAILABLE_EXIT_CODE
                );
            } else {
                assert_eq!(crate::run_exit::error_exit_code(&error), 1);
                assert_eq!(
                    error.to_string(),
                    if action.starts_with("cancel") {
                        "host.cloud_cancelled"
                    } else {
                        "host.runtime_consent_changed"
                    }
                );
            }
            for name in [
                "native-service.json",
                "native-service-update.json",
                "host-runtime.json",
                "host-cloud.json",
                "daemon",
                "host-identity",
            ] {
                assert!(!state.join(name).exists(), "{action}: {name}");
            }
        }
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

    #[test]
    fn host_runtime_revocation_proof_requires_current_snapshot_released_lease_and_valid_history() {
        let (root, store) = fixture();
        provision(&store);
        let (lease, provisioned, record) = reserve(&store).unwrap();
        persist(&store, &record).unwrap();
        assert!(revoked_runtime_released(&store, &provisioned).is_err());
        store.revoke().unwrap();
        let current = store.snapshot().unwrap();
        assert!(!revoked_runtime_released(&store, &current).unwrap());
        assert!(reserve(&store).is_err());
        drop(lease);
        assert!(revoked_runtime_released(&store, &current).unwrap());
        let mut foreign = current.clone();
        foreign.installation = uuid::Uuid::new_v4().to_string();
        assert!(revoked_runtime_released(&store, &foreign).is_err());
        std::fs::remove_file(root.path().join("host").join(LOCK)).unwrap();
        assert_eq!(
            revoked_runtime_released(&store, &current)
                .unwrap_err()
                .to_string(),
            "host.runtime_lock_missing_reconciliation_required"
        );
        std::fs::write(root.path().join("host").join(RECORD), b"corrupt").unwrap();
        assert!(revoked_runtime_released(&store, &current).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn host_runtime_revocation_waits_for_serving_shutdown_and_run_lease_release() {
        let (_root, store) = fixture();
        provision(&store);
        let (lease, expected, record) = reserve(&store).unwrap();
        persist(&store, &record).unwrap();
        let shutdown = CancellationToken::new();
        let (finish, finished) = tokio::sync::oneshot::channel::<()>();
        let serving = {
            let shutdown = shutdown.clone();
            async move {
                shutdown.cancelled().await;
                let _ = finished.await;
                Ok(())
            }
        };
        let watched = supervise(
            &store,
            &expected,
            CancellationToken::new(),
            shutdown.clone(),
            serving,
        );
        tokio::pin!(watched);
        store.revoke().unwrap();
        let current = store.snapshot().unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), &mut watched)
                .await
                .is_err()
        );
        assert!(shutdown.is_cancelled());
        assert!(!revoked_runtime_released(&store, &current).unwrap());
        finish.send(()).unwrap();
        assert!(watched.await.is_err());
        drop(lease);
        assert!(revoked_runtime_released(&store, &current).unwrap());
    }

    #[test]
    fn host_runtime_linux_sign_in_requires_logon_fence_not_lingering_user_manager() {
        assert!(!linux_login::required("linux", Startup::Manual));
        assert!(linux_login::required("linux", Startup::UserSignIn));
        assert!(!linux_login::required("macos", Startup::UserSignIn));
    }

    #[tokio::test(start_paused = true)]
    async fn host_runtime_login_retry_snapshot_waits_for_short_held_lease() {
        let (root, store) = fixture();
        provision(&store);
        let (_run_lease, expected, mut record) = reserve(&store).unwrap();
        let policy_lease = store.with_owned_lease(|_, _, lease| Ok(lease)).unwrap();
        let current = login_retry_snapshot(&store);
        tokio::pin!(current);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut current)
                .await
                .is_err()
        );
        drop(policy_lease);
        let current = current.await.unwrap();
        assert_eq!(current, expected);
        assert!(
            authorize_run(
                &store,
                &root.path().join("host"),
                &expected,
                &mut record,
                &current
            )
            .is_err()
        );
        assert_eq!(
            record.failure,
            Some(RunFailure::AuthenticatedBundleRequired)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn host_runtime_login_retry_snapshot_persistent_busy_and_nonbusy_errors_refuse() {
        let (_root, store) = fixture();
        provision(&store);
        let lease = store.with_owned_lease(|_, _, lease| Ok(lease)).unwrap();
        let started = tokio::time::Instant::now();
        let error = login_retry_snapshot(&store).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(crate::run_exit::error_exit_code(&error), 1);
        assert!(started.elapsed() >= Duration::from_millis(250));
        assert!(started.elapsed() <= Duration::from_millis(275));
        drop(lease);
        assert!(last_run(&store).unwrap().is_none());
        store
            .with_locked(|directory, _| {
                storage::replace_private(directory, Path::new("host-lifecycle.json"), b"invalid")
            })
            .unwrap();
        let started = tokio::time::Instant::now();
        let error = login_retry_snapshot(&store).await.unwrap_err();
        assert_ne!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(crate::run_exit::error_exit_code(&error), 1);
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn host_runtime_login_retry_snapshot_rechecks_revoked_or_changed_consent() {
        for change_settings in [false, true] {
            let (root, store) = fixture();
            provision(&store);
            let (_run_lease, expected, mut record) = reserve(&store).unwrap();
            let policy_lease = store.with_owned_lease(|_, _, lease| Ok(lease)).unwrap();
            let current = login_retry_snapshot(&store);
            tokio::pin!(current);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut current)
                    .await
                    .is_err()
            );
            drop(policy_lease);
            store.revoke().unwrap();
            if change_settings {
                let stopped = store.snapshot().unwrap();
                let binding = stopped.binding.unwrap();
                store.confirm_stopped(stopped.epoch, &binding).unwrap();
                let mut settings = expected.settings.clone();
                settings.allow_creation = !settings.allow_creation;
                store.configure(settings.clone()).unwrap();
                let ticket = store.request_enable(&settings).unwrap();
                store.record_provisioned(&ticket, binding).unwrap();
            }
            let current = current.await.unwrap();
            assert_ne!(current, expected);
            assert_eq!(
                current.phase,
                if change_settings {
                    Phase::Provisioned
                } else {
                    Phase::StopRequested
                }
            );
            let error = match authorize_run(
                &store,
                &root.path().join("host"),
                &expected,
                &mut record,
                &current,
            ) {
                Ok(_) => panic!("changed protected consent cannot authorize login retry"),
                Err(error) => error,
            };
            assert_eq!(error.to_string(), "host.runtime_consent_changed");
            assert_eq!(crate::run_exit::error_exit_code(&error), 1);
            assert_eq!(store.snapshot().unwrap(), current);
            assert!(last_run(&store).unwrap().is_none());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn host_runtime_watch_retries_real_installation_contention_then_observes_revocation() {
        let (_root, store) = fixture();
        provision(&store);
        let (_run_lease, expected, mut record) = reserve(&store).unwrap();
        record.state = RunState::Listening;
        persist(&store, &record).unwrap();
        let policy_lease = store.with_owned_lease(|_, _, lease| Ok(lease)).unwrap();
        let shutdown = CancellationToken::new();
        let serving = {
            let shutdown = shutdown.clone();
            async move {
                shutdown.cancelled().await;
                Ok(())
            }
        };
        struct Eligible;
        impl linux_login::Query for Eligible {
            async fn eligible(&self, _: &CancellationToken) -> io::Result<bool> {
                Ok(true)
            }
        }
        let cancel = CancellationToken::new();
        let hosting_cancel = cancel.child_token();
        let hosted = supervise(
            &store,
            &expected,
            hosting_cancel.clone(),
            shutdown.clone(),
            serving,
        );
        let watched = linux_login::guard(&Eligible, cancel, hosting_cancel, hosted);
        tokio::pin!(watched);
        assert!(
            tokio::time::timeout(Duration::from_secs(4), &mut watched)
                .await
                .is_err()
        );
        assert!(!shutdown.is_cancelled());
        drop(policy_lease);
        assert_eq!(
            last_run(&store).unwrap().unwrap().state,
            RunState::Listening
        );
        store.revoke().unwrap();
        let error = tokio::time::timeout(Duration::from_secs(2), &mut watched)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.to_string(), "host.runtime_consent_changed");
        assert_eq!(crate::run_exit::error_exit_code(&error), 1);
        assert_ne!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(shutdown.is_cancelled());
    }

    #[tokio::test]
    async fn host_runtime_manager_restart_after_disabled_stop_or_cleanup_is_not_retryable() {
        for phase in [Phase::Disabled, Phase::StopRequested, Phase::CleanupPending] {
            let (root, store) = fixture_startup(Startup::UserSignIn);
            provision(&store);
            store.revoke().unwrap();
            if phase != Phase::StopRequested {
                let stopped = store.snapshot().unwrap();
                store
                    .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
                    .unwrap();
            }
            if phase == Phase::CleanupPending {
                store.request_cleanup().unwrap();
            }
            assert_eq!(store.snapshot().unwrap().phase, phase);
            let before = store.snapshot().unwrap();
            let error = run(
                store.clone(),
                &root.path().join("host"),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
            assert_eq!(crate::run_exit::error_exit_code(&error), 1, "{phase:?}");
            assert_eq!(store.snapshot().unwrap(), before);
            assert!(last_run(&store).unwrap().is_none());
            assert!(!root.path().join("host/daemon").exists());
            assert!(!root.path().join("host/host-cloud.json").exists());
        }
    }

    #[tokio::test]
    async fn host_runtime_sign_in_bad_bundle_is_not_a_login_retry() {
        let (root, store) = fixture_startup(Startup::UserSignIn);
        provision(&store);
        let error = run(
            store.clone(),
            &root.path().join("host"),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(crate::run_exit::error_exit_code(&error), 1);
        assert_eq!(
            last_run(&store).unwrap().unwrap().failure,
            Some(RunFailure::AuthenticatedBundleRequired)
        );
        assert!(!root.path().join("host/daemon").exists());
        assert!(!root.path().join("host/host-cloud.json").exists());
    }

    fn observed_stop(store: &Store, epoch: u64) -> Observation {
        let snapshot = store.snapshot().unwrap();
        assert_eq!(snapshot.phase, Phase::StopRequested);
        assert_eq!(snapshot.epoch, epoch);
        Observation {
            process: ProcessState::Stopped,
            installation: snapshot.installation,
            origin: snapshot.origin,
            epoch,
        }
    }

    #[test]
    fn host_runtime_disable_revokes_before_stop_and_waits_for_exact_run_lease() {
        let (root, store) = fixture();
        let state = root.path().join("host");
        provision(&store);
        let (lease, before, _) = reserve(&store).unwrap();
        let error = disable_with(&store, Duration::ZERO, |epoch| {
            crate::services::fixture_services(&store, &state)?.stop(epoch)
        })
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(error.to_string(), "host.runtime_stop_pending");
        assert_eq!(store.snapshot().unwrap().phase, Phase::StopRequested);
        assert!(store.snapshot().unwrap().epoch > before.epoch);
        drop(lease);
        let stopped = disable_with(&store, Duration::ZERO, |epoch| {
            crate::services::fixture_services(&store, &state)?.stop(epoch)
        })
        .unwrap();
        assert_eq!(stopped.lifecycle.phase, Phase::Disabled);
        assert_eq!(stopped.lifecycle.binding, before.binding);
        store.request_cleanup().unwrap();
        assert_eq!(store.snapshot().unwrap().phase, Phase::CleanupPending);
    }

    #[test]
    fn host_runtime_fixture_native_context_still_refuses_a_foreign_state_directory() {
        let (_root, store) = fixture();
        let (other_root, _other_store) = fixture();
        let error = crate::services::fixture_services(&store, &other_root.path().join("host"))
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "host.service_foreign_state_directory");
    }

    #[test]
    fn host_runtime_disable_does_not_confirm_foreign_stop_or_cloud_uncertainty() {
        let (_root, store) = fixture();
        provision(&store);
        assert!(
            disable_with(&store, Duration::ZERO, |epoch| {
                let mut observed = observed_stop(&store, epoch);
                observed.installation = uuid::Uuid::new_v4().to_string();
                Ok(observed)
            })
            .is_err()
        );
        assert_eq!(store.snapshot().unwrap().phase, Phase::StopRequested);
        assert!(store.request_cleanup().is_err());

        let (_other_root, other) = fixture();
        other
            .request_enable(&other.snapshot().unwrap().settings)
            .unwrap();
        assert!(disable_with(&other, Duration::ZERO, |_| panic!("no native operation")).is_err());
        assert_eq!(other.snapshot().unwrap().phase, Phase::Reconciling);
        assert!(other.snapshot().unwrap().pending_ticket().is_some());
        assert!(other.request_cleanup().is_err());
    }

    #[test]
    fn host_runtime_missing_lock_never_authorizes_restart_or_confirmed_stop() {
        let (root, store) = fixture();
        let state = root.path().join("host");
        provision(&store);
        let (lease, _, record) = reserve(&store).unwrap();
        persist(&store, &record).unwrap();
        drop(lease);
        std::fs::remove_file(state.join(LOCK)).unwrap();
        assert!(reserve(&store).is_err());
        let error = disable_with(&store, Duration::ZERO, |epoch| {
            crate::services::fixture_services(&store, &state)?.stop(epoch)
        })
        .err()
        .unwrap();
        assert_eq!(
            error.to_string(),
            "host.runtime_lock_missing_reconciliation_required"
        );
        assert_eq!(store.snapshot().unwrap().phase, Phase::StopRequested);
        assert!(store.request_cleanup().is_err());
        assert_eq!(last_run(&store).unwrap().unwrap(), record);
    }

    #[tokio::test]
    async fn host_runtime_native_cleanup_denial_never_polls_cloud_cleanup() {
        let (root, store) = fixture();
        provision(&store);
        let error = uninstall_with_native(
            &store,
            &CancellationToken::new(),
            || fixture_disable(&store, &root.path().join("host")),
            |_| Err(io::ErrorKind::PermissionDenied.into()),
            async { panic!("native cleanup denial must not poll cloud cleanup") },
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(store.snapshot().unwrap().phase, Phase::CleanupPending);
    }

    #[tokio::test]
    async fn host_runtime_uninstall_cancel_and_removed_history_never_contact_native_or_cloud() {
        let (root, store) = fixture();
        provision(&store);
        let before = store.snapshot().unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            uninstall(&store, &root.path().join("host"), &cancel)
                .await
                .is_err()
        );
        assert_eq!(store.snapshot().unwrap(), before);
        fixture_disable(&store, &root.path().join("host")).unwrap();
        store.request_cleanup().unwrap();
        let pending = store.snapshot().unwrap();
        store
            .confirm_removed(pending.epoch, pending.binding.as_ref().unwrap())
            .unwrap();
        assert_eq!(
            uninstall(&store, &root.path().join("host"), &CancellationToken::new())
                .await
                .unwrap(),
            UninstallResult::AlreadyRemovedNotReobserved,
        );
        assert!(
            !root
                .path()
                .join("host")
                .join("native-service.json")
                .exists()
        );
        assert!(!root.path().join("host").join("host-cloud.json").exists());
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

    #[tokio::test]
    async fn host_runtime_failed_start_is_durable_without_shell_or_credentials() {
        let (root, store) = fixture();
        provision(&store);
        let error = run(
            store.clone(),
            &root.path().join("host"),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(crate::run_exit::error_exit_code(&error), 1);
        let record = last_run(&store).unwrap().unwrap();
        assert_eq!(record.state, RunState::Failed);
        assert_eq!(
            record.failure,
            Some(RunFailure::AuthenticatedBundleRequired)
        );
        assert!(!root.path().join("host").join("daemon").exists());
        assert!(!root.path().join("host").join("host-cloud.json").exists());
        assert!(
            !root
                .path()
                .join("host")
                .join("owner-enrollment.json")
                .exists()
        );
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
