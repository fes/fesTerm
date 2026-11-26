//! Shared explicit foreground setup and restartable host composition.

mod linux_login;
pub mod runtime_diagnostics;
use runtime_diagnostics::{
    Log as RuntimeLog, Outcome as DiagnosticOutcome, Phase as DiagnosticPhase,
};

use crate::{
    cloud::{Cleanup, CloudHostAdapter, Foreground, Health, HOST_IDENTITY_FILE},
    enrollment::{
        owner_key_fingerprint, CurrentOwnerPolicySource, EnrollmentSnapshot, OwnerEnrollment,
        OwnerStorage,
    },
    lifecycle::{OrdinaryAccess, Phase, Snapshot, Startup, Store},
    ordinary::CurrentHostPolicySource,
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

pub mod managed;

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

    fn host_item_admission(&self, _store: &Store, _stage: &TrustedStage) -> io::Result<()> {
        Ok(())
    }

    async fn sign_in(
        &self,
        cloud: &CloudHostAdapter<'_>,
        foreground: &Foreground,
        cancel: &CancellationToken,
        prompt: impl FnOnce(festerm_tunnel_client::identity::DevicePrompt),
    ) -> io::Result<()>;
    async fn sign_in_at_stage(
        &self,
        cloud: &CloudHostAdapter<'_>,
        foreground: &Foreground,
        cancel: &CancellationToken,
        prompt: impl FnOnce(festerm_tunnel_client::identity::DevicePrompt),
        _stage: &TrustedStage,
    ) -> io::Result<()> {
        self.sign_in(cloud, foreground, cancel, prompt).await
    }
    fn initialize_owner(
        &self,
        owners: &OwnerEnrollment<'_>,
        account: String,
        epoch: u64,
        _stage: &TrustedStage,
    ) -> io::Result<EnrollmentSnapshot> {
        owners.initialize_at_epoch(Path::new(HOST_IDENTITY_FILE), account, epoch)
    }
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
    fn complete_setup(
        &self,
        store: &Store,
        state: &Path,
        stage: &TrustedStage,
        expected: &Snapshot,
        owner: &EnrollmentSnapshot,
        cancel: &CancellationToken,
    ) -> io::Result<Observation> {
        let _ = owner;
        if cancel.is_cancelled() {
            return Err(refused("host.cloud_cancelled"));
        }
        self.install(store, state, stage, expected.epoch)
    }
}

struct NativeBoundary;

impl SetupBoundary for NativeBoundary {
    fn host_item_admission(&self, store: &Store, stage: &TrustedStage) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        if store.snapshot()?.origin == crate::lifecycle::Origin::Desktop {
            stage.require_mac_host_access()?;
            stage.require_mac_host_creation_identity()?;
        }
        let _ = (store, stage);
        Ok(())
    }
    #[cfg(target_os = "macos")]
    async fn sign_in_at_stage(
        &self,
        cloud: &CloudHostAdapter<'_>,
        foreground: &Foreground,
        cancel: &CancellationToken,
        prompt: impl FnOnce(festerm_tunnel_client::identity::DevicePrompt),
        stage: &TrustedStage,
    ) -> io::Result<()> {
        if cloud.is_desktop()? {
            cloud
                .sign_in_desktop(foreground, cancel, prompt, stage)
                .await
        } else {
            cloud.sign_in(foreground, cancel, prompt).await
        }
    }
    #[cfg(target_os = "macos")]
    fn initialize_owner(
        &self,
        owners: &OwnerEnrollment<'_>,
        account: String,
        epoch: u64,
        stage: &TrustedStage,
    ) -> io::Result<EnrollmentSnapshot> {
        if owners.is_desktop()? {
            owners.initialize_desktop_at_epoch(Path::new(HOST_IDENTITY_FILE), account, epoch, stage)
        } else {
            owners.initialize_at_epoch(Path::new(HOST_IDENTITY_FILE), account, epoch)
        }
    }
    fn complete_setup(
        &self,
        store: &Store,
        state: &Path,
        stage: &TrustedStage,
        expected: &Snapshot,
        owner: &EnrollmentSnapshot,
        cancel: &CancellationToken,
    ) -> io::Result<Observation> {
        Services::current_user(store, state)?.complete_setup(stage, expected, owner, cancel)
    }
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
        Services::current_user(store, state)?.install_with_recovery(stage, epoch)
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

/// Complete Desktop foreground consent includes the private file folder and
/// permissions (or explicitly Off); generic/headless drafts cannot use it.
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
    if consent.is_some() && before.origin != crate::lifecycle::Origin::Desktop {
        return Err(refused("host.desktop_consent_required"));
    }
    if consent.is_none() && before.settings.filesystem.is_some() {
        return Err(refused("host.file_access_review_required"));
    }
    let settings = consent.map_or(&before.settings, |consent| &consent.settings);
    settings.validate()?;
    startup_admission(settings.startup, cancel).await?;
    if store.snapshot()? != before {
        return Err(refused("host.foreground_scope_changed"));
    }
    stage.sessiond_path()?;
    boundary.host_item_admission(store, stage)?;
    if account == 0 || cancel.is_cancelled() {
        return Err(refused("host.foreground_account_consent_required"));
    }
    file_helper_admission(store, &before, settings, cancel, || {
        stage.require_host_files(settings)
    })?;
    #[cfg(not(test))]
    let cloud = CloudHostAdapter::new(store);
    #[cfg(test)]
    let cloud = boundary.fixture_cloud(store);
    let expected_epoch = match before.phase {
        Phase::Disabled => {
            let ticket = match consent {
                Some(consent) => {
                    store.request_desktop_enable_if_current(&consent.expected, settings)?
                }
                None => store.request_enable(settings)?,
            };
            reserved(&ticket);
            if cancel.is_cancelled() {
                return Err(refused("host.cloud_cancelled"));
            }
            let foreground = cloud.foreground(&ticket, account)?;
            boundary
                .sign_in_at_stage(&cloud, &foreground, cancel, prompt, stage)
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
        .as_ref()
        .ok_or_else(|| refused("host.cloud_binding_changed"))?
        .account
        .clone();
    use crate::enrollment::{observe_owner_result, OwnerAdmissionStage as OwnerStage};
    let mut observe =
        |event| runtime_diagnostics::foreground_owner_event(state, expected_epoch, event);
    let owners = observe_owner_result(OwnerStage::StoreOpen, &mut observe, || {
        boundary.owners(store)
    })?;
    let initial = match owners.snapshot_observed(&mut observe) {
        Ok(existing) => existing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            observe_owner_result(OwnerStage::Initialization, &mut observe, || {
                boundary.initialize_owner(&owners, account, expected_epoch, stage)
            })?
        }
        Err(error) => return Err(error),
    };
    let native_registration =
        observe_owner_result(OwnerStage::NativeRegistration, &mut observe, || {
            boundary.complete_setup(store, state, stage, &current, &initial, cancel)
        })?;
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
        Services::stop_current_user(store, state, epoch)
    })
}

pub fn disable_if_current(
    store: &Store,
    state: &Path,
    expected: &Snapshot,
) -> io::Result<DisableResult> {
    disable_scoped_with(store, Some(expected), Duration::from_secs(10), |epoch| {
        Services::stop_current_user(store, state, epoch)
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
    FilePolicyUnavailable,
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

/// Foreground GUI replacement binds the exact snapshot reviewed by the user.
/// Shares the existing stopped updater; never changes origin or replacement policy.
pub async fn update_if_current(
    store: &Store,
    state: &Path,
    expected: &Snapshot,
    stage: &TrustedStage,
    from_version: &str,
    cancel: &CancellationToken,
) -> io::Result<crate::services::ReplacementReport> {
    update_current_with_admission(
        store,
        state,
        Some(expected),
        stage,
        from_version,
        cancel,
        |startup| startup_admission(startup, cancel),
    )
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
    update_current_with_admission(store, state, None, stage, from_version, cancel, admission).await
}

async fn update_current_with_admission<A: std::future::Future<Output = io::Result<()>>>(
    store: &Store,
    state: &Path,
    expected: Option<&Snapshot>,
    stage: &TrustedStage,
    from_version: &str,
    cancel: &CancellationToken,
    admission: impl FnOnce(Startup) -> A,
) -> io::Result<crate::services::ReplacementReport> {
    let snapshot = store.snapshot()?;
    if expected.is_some_and(|expected| expected != &snapshot) {
        return Err(refused("host.runtime_consent_changed"));
    }
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
    file_helper_admission(store, &snapshot, &snapshot.settings, cancel, || {
        stage.require_host_files(&snapshot.settings)
    })?;
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

// A read-only executable probe may wait. Its result grants no lifecycle
// authority: check cancellation and the complete protected scope afterwards.
fn file_helper_admission(
    store: &Store,
    expected: &Snapshot,
    settings: &crate::lifecycle::Settings,
    cancel: &CancellationToken,
    probe: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if settings.filesystem.is_none() {
        return Ok(());
    }
    if cancel.is_cancelled() {
        return Err(refused("host.cloud_cancelled"));
    }
    probe()?;
    if cancel.is_cancelled() {
        return Err(refused("host.cloud_cancelled"));
    }
    if store.snapshot()? != *expected {
        return Err(refused("host.foreground_scope_changed"));
    }
    Ok(())
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
                        return Err(refused("host.runtime_lock_missing_reconciliation_required"))
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
    run_to_completion(store, state, &snapshot, &mut record, cancel).await
}

async fn run_to_completion(
    store: Arc<Store>,
    state: &Path,
    snapshot: &Snapshot,
    record: &mut RunRecord,
    cancel: CancellationToken,
) -> io::Result<()> {
    let log = RuntimeLog::new(state, record);
    persist_diagnostic(&store, record, Some(&log))?;
    // Keep the serving state machine off callers' startup stacks, including
    // refusal paths whose outer futures still contain the maximum await state.
    let result = Box::pin(run_reserved(
        store.clone(),
        state,
        snapshot,
        record,
        cancel.clone(),
    ))
    .await;
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
    persist_diagnostic(&store, record, Some(&log))?;
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
    let Some(stage) = initial_run_admission(&store, expected, &cancel, |current| {
        authorize_run(&store, state, expected, record, current)
    })
    .await?
    else {
        return Ok(());
    };
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

// Native managers can return Running while their parent still owns the short
// service-operation lease. Retry only initial admission contention; never restart
// a manager or reuse a failed attempt's stage/policy. This bounds retry waits,
// not an already executing synchronous vault/publisher verification call.
const INITIAL_ADMISSION_CONTENTION: Duration = Duration::from_secs(2);

async fn initial_run_admission<T>(
    store: &Store,
    expected: &Snapshot,
    cancel: &CancellationToken,
    mut authorize: impl FnMut(&Snapshot) -> io::Result<T>,
) -> io::Result<Option<T>> {
    let deadline = tokio::time::Instant::now() + INITIAL_ADMISSION_CONTENTION;
    loop {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let result = store.snapshot().and_then(|current| {
            if current != *expected {
                return Err(refused("host.runtime_consent_changed"));
            }
            authorize(&current)
        });
        if cancel.is_cancelled() {
            return Ok(None);
        }
        match result {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(error);
                }
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Ok(None),
                    _ = tokio::time::sleep_until(
                        (tokio::time::Instant::now() + Duration::from_millis(25)).min(deadline)
                    ) => {}
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(error);
                }
            }
            result => return result.map(Some),
        }
    }
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
    let services = Services::current_user(store, state)?;
    let stage = services.reopen_stage(expected.epoch)?;
    services.require_runtime_execution(&stage)?;
    stage.sessiond_path()?;
    authorize_owner_admission(expected, record, state, || store.owner_enrollment())?;
    if expected.settings.ordinary_access == OrdinaryAccess::OwnerOnly {
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
    }
    Ok(stage)
}

// Keep the original selected-store read, owner-only check and errors. The
// observer is finite evidence only, never an alternate authorization path.
fn authorize_owner_admission<'a>(
    expected: &Snapshot,
    record: &mut RunRecord,
    state: &Path,
    open: impl FnOnce() -> io::Result<OwnerEnrollment<'a>>,
) -> io::Result<()> {
    use crate::enrollment::{
        OwnerAdmissionEvent as Event, OwnerAdmissionFailure as Failure,
        OwnerAdmissionStage as Stage,
    };
    record.failure = Some(RunFailure::CurrentOwnerConsentRequired);
    let log = RuntimeLog::new(state, record);
    log.owner_admission(Event::Begin(Stage::StoreOpen));
    let owner = open().inspect_err(|error| {
        log.owner_admission(Event::Fail(Stage::StoreOpen, Failure::from_io(error)));
    })?;
    log.owner_admission(Event::Pass(Stage::StoreOpen));
    let current = owner.snapshot_observed(&mut |event| log.owner_admission(event))?;
    require_owner_consent(expected, &current, &mut |event| log.owner_admission(event))
}

fn require_owner_consent(
    expected: &Snapshot,
    current: &EnrollmentSnapshot,
    observe: &mut dyn FnMut(crate::enrollment::OwnerAdmissionEvent),
) -> io::Result<()> {
    use crate::enrollment::{
        OwnerAdmissionEvent as Event, OwnerAdmissionFailure as Failure,
        OwnerAdmissionStage as Stage,
    };
    observe(Event::Begin(Stage::Consent));
    if expected.settings.ordinary_access == OrdinaryAccess::OwnerOnly
        && (!current.remote_access || !current.keys.iter().any(|key| !key.revoked))
    {
        observe(Event::Fail(Stage::Consent, Failure::MissingConsent));
        return Err(refused("host.runtime_current_owner_consent_required"));
    }
    observe(Event::Pass(Stage::Consent));
    Ok(())
}

/// Status-only evidence: never a grant, serving/liveness proof or retry request.
#[derive(Serialize)]
pub struct OwnerReadinessDiagnostic {
    schema: u8,
    scope: &'static str,
    status_only: bool,
    events: Vec<OwnerReadinessObservation>,
}

#[derive(Serialize)]
struct OwnerReadinessObservation {
    stage: &'static str,
    outcome: &'static str,
    category: &'static str,
}

/// Read exact selected native owner policy without initialization, high-water
/// writes, foreground vault interaction, cloud calls or manager operations.
pub fn owner_readiness_diagnostic(
    state: &Path,
    origin: crate::lifecycle::Origin,
) -> OwnerReadinessDiagnostic {
    owner_readiness_with(|| Store::open(state, origin), Store::owner_enrollment)
}

fn owner_readiness_with(
    open_store: impl FnOnce() -> io::Result<Store>,
    open_owner: impl for<'a> FnOnce(&'a Store) -> io::Result<OwnerEnrollment<'a>>,
) -> OwnerReadinessDiagnostic {
    use crate::enrollment::{
        OwnerAdmissionEvent as Event, OwnerAdmissionFailure as Failure,
        OwnerAdmissionStage as Stage,
    };
    let mut report = OwnerReadinessDiagnostic {
        schema: 1,
        scope: "local_owner_admission",
        status_only: true,
        events: Vec::new(),
    };
    let mut observe = |event| {
        // One synchronous snapshot has a fixed number of boundaries. Keep a
        // hard cap too; evidence exhaustion never authorizes anything.
        if report.events.len() == 24 {
            return;
        }
        let (stage, outcome, category) = match event {
            Event::Begin(stage) => (stage, "begin", "none"),
            Event::Pass(stage) => (stage, "ok", "none"),
            Event::Fail(stage, reason) => (stage, "error", reason.code()),
        };
        report.events.push(OwnerReadinessObservation {
            stage: stage.code(),
            outcome,
            category,
        });
    };
    observe(Event::Begin(Stage::StoreOpen));
    let _ = (|| -> io::Result<()> {
        let store = open_store().inspect_err(|error| {
            observe(Event::Fail(Stage::StoreOpen, Failure::from_io(error)));
        })?;
        let owner = open_owner(&store).inspect_err(|error| {
            observe(Event::Fail(Stage::StoreOpen, Failure::from_io(error)));
        })?;
        observe(Event::Pass(Stage::StoreOpen));
        let current = owner.current_snapshot_noninteractive_observed(&mut observe)?;
        require_owner_consent(current.installation(), current.enrollment(), &mut observe)
    })();
    report
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
    let policy_log = RuntimeLog::new(state, record);
    let helper = stage.sessiond_path();
    policy_log.event(
        DiagnosticPhase::HelperValidation,
        if helper.is_ok() {
            DiagnosticOutcome::Ok
        } else {
            DiagnosticOutcome::Error
        },
        helper.as_ref().err(),
        None,
        None,
    );
    let helper = helper?;
    let prepared = CurrentHostPolicySource::prepare(
        store.clone(),
        OwnerStorage::NativeVault,
        expected,
        &cancel,
    )
    .await;
    policy_log.event(
        DiagnosticPhase::InitialPolicyPrepare,
        if prepared.is_ok() {
            DiagnosticOutcome::Ok
        } else {
            DiagnosticOutcome::Error
        },
        prepared.as_ref().err(),
        None,
        None,
    );
    let policy = Arc::new(prepared?.with_diagnostic(Arc::new(move |event, error| {
        policy_log.readiness(event, error);
    })));
    record.failure = Some(RunFailure::DaemonBoundaryUnavailable);
    let sessions = SessionControl::with_helper(&state.join("daemon"), helper)
        .map_err(|_| refused("host.runtime_daemon_boundary_unavailable"))?;
    let runtime = if expected.settings.filesystem.is_some() {
        record.failure = Some(RunFailure::FilePolicyUnavailable);
        let files = festerm_tunnel_bridge::file_protocol::HostPolicy {
            scope: festerm_tunnel_bridge::file_protocol::HostScope {
                host: festerm_host_protocol::HostId::try_from(
                    expected
                        .binding
                        .as_ref()
                        .ok_or_else(|| refused("The current host identity is unavailable."))?
                        .host_id
                        .clone(),
                )
                .map_err(io::Error::from)?,
                installation: festerm_host_protocol::HostId::try_from(
                    expected.installation.clone(),
                )
                .map_err(io::Error::from)?,
                epoch: expected.epoch.to_string(),
            },
            filesystem: expected.settings.filesystem.clone(),
        };
        let runtime_policy = policy.clone();
        let preparing = tokio::task::spawn_blocking(move || {
            HostRuntime::new(sessions, runtime_policy).with_filesystem(files)
        });
        tokio::select! { biased;
            _ = cancel.cancelled() => return Ok(()),
            result = tokio::time::timeout(Duration::from_secs(30), preparing) => {
                result.map_err(|_| refused("host.filesystem_preparation_timed_out"))?
                    .map_err(|_| refused("host.filesystem_preparation_failed"))??
            }
        }
    } else {
        HostRuntime::new(sessions, policy.clone())
    };
    let host = RunningHost {
        store: &store,
        state,
        expected,
        stage,
        runtime: Arc::new(runtime),
        policy,
    };
    let shutdown = cancel.child_token();
    let log = RuntimeLog::new(state, record);
    let recovering = recover_relay_with_diagnostics(
        &store,
        expected,
        record,
        &shutdown,
        async |record| host.attempt(record, &shutdown).await,
        Some(&log),
    );
    supervise_with_diagnostics(
        &store,
        expected,
        cancel,
        shutdown.clone(),
        recovering,
        Some(&log),
    )
    .await
}

struct RunningHost<'a> {
    store: &'a Arc<Store>,
    state: &'a Path,
    expected: &'a Snapshot,
    stage: &'a TrustedStage,
    runtime: Arc<HostRuntime<CurrentHostPolicySource>>,
    policy: Arc<CurrentHostPolicySource>,
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
        let log = RuntimeLog::new(self.state, record);
        let opened = CloudHostAdapter::new(self.store).open_relay(cancel).await;
        log.event(
            DiagnosticPhase::RelayOpen,
            if opened.is_ok() {
                DiagnosticOutcome::Ok
            } else {
                DiagnosticOutcome::Error
            },
            opened.as_ref().err(),
            None,
            None,
        );
        let mut relay = opened?;
        let mut reached_runtime = false;
        let served = async {
            // Refresh the ready observer after relay credential refresh, before
            // accepting streams, without resetting aggregate runtime budgets.
            let refreshed = self
                .policy
                .refresh(self.expected, cancel)
                .await
                .map_err(crate::cloud::terminal_ordinary_renewal);
            log.event(
                DiagnosticPhase::PolicyRefresh,
                if refreshed.is_ok() {
                    DiagnosticOutcome::Ok
                } else {
                    DiagnosticOutcome::Error
                },
                refreshed.as_ref().err(),
                None,
                None,
            );
            refreshed?;
            startup_admission(self.expected.settings.startup, cancel).await?;
            self.authorize(record)?;
            if cancel.is_cancelled() {
                return Ok(Err(festerm_tunnel_client::host_control::Error::Cancelled));
            }
            record.state = RunState::Listening;
            record.failure = None;
            persist_diagnostic(self.store, record, Some(&log))?;
            reached_runtime = true;
            let serving_cancel = cancel.child_token();
            let served = self
                .runtime
                .clone()
                .serve(&mut relay, serving_cancel.clone());
            let maintained = self.policy.maintain(self.expected, &serving_cancel);
            serve_with_account_maintenance(served, maintained, &serving_cancel).await
        }
        .await;
        let before = (relay.failure_category(), relay.failure_disposition());
        finish_relay_attempt(
            &log,
            reached_runtime,
            served,
            before,
            async {
                let closed = relay.close().await;
                (
                    closed,
                    relay.failure_category(),
                    relay.failure_disposition(),
                )
            },
            cancel,
        )
        .await
    }
}

async fn serve_with_account_maintenance(
    served: impl std::future::Future<Output = Result<(), festerm_tunnel_client::host_control::Error>>,
    maintained: impl std::future::Future<Output = io::Result<()>>,
    cancel: &CancellationToken,
) -> io::Result<Result<(), festerm_tunnel_client::host_control::Error>> {
    tokio::pin!(served, maintained);
    tokio::select! {
        biased;
        result = &mut served => {
            cancel.cancel();
            // Cancellation cannot abort a native credential mutation. Retain
            // its owner through completion, and never retry uncertain writes.
            // Even cancellation may follow provider-side token rotation. It
            // stays final here; only the existing OUTER explicit shutdown can
            // normalize a typed cancellation to stopped, never relay recovery.
            maintained.await?;
            Ok(result)
        }
        result = &mut maintained => {
            cancel.cancel();
            let drained = served.await;
            result?;
            Ok(drained)
        }
    }
}

fn persist_diagnostic(
    store: &Store,
    record: &RunRecord,
    log: Option<&RuntimeLog>,
) -> io::Result<()> {
    let result = persist(store, record);
    if let (Some(log), Err(error)) = (log, &result) {
        log.event(
            DiagnosticPhase::HistoryPersist,
            DiagnosticOutcome::Error,
            Some(error),
            None,
            None,
        );
    }
    result
}

async fn finish_relay_attempt(
    log: &RuntimeLog,
    reached_runtime: bool,
    served: io::Result<Result<(), festerm_tunnel_client::host_control::Error>>,
    before: (
        Option<&str>,
        Option<festerm_tunnel_bridge::relay::HostFailureDisposition>,
    ),
    close: impl std::future::Future<
        Output = (
            io::Result<()>,
            Option<&'static str>,
            Option<festerm_tunnel_bridge::relay::HostFailureDisposition>,
        ),
    >,
    cancel: &CancellationToken,
) -> io::Result<()> {
    use festerm_tunnel_client::host_control::Error;
    if reached_runtime {
        let outcome = match &served {
            Err(_) => DiagnosticOutcome::Error,
            Ok(Err(Error::Cancelled)) => DiagnosticOutcome::Cancelled,
            Ok(Err(Error::Closed)) => DiagnosticOutcome::Closed,
            Ok(Err(_)) => DiagnosticOutcome::Error,
            Ok(Ok(())) => DiagnosticOutcome::Ok,
        };
        log.event(
            DiagnosticPhase::RuntimeServe,
            outcome,
            served.as_ref().err(),
            before.0,
            before.1,
        );
    }
    let (closed, category, disposition) = close.await;
    log.event(
        DiagnosticPhase::RelayCleanup,
        if closed.is_ok() {
            DiagnosticOutcome::Ok
        } else {
            DiagnosticOutcome::Error
        },
        closed.as_ref().err(),
        category,
        disposition,
    );
    // Cleanup uncertainty remains final. Observe disposition at the same POST-close
    // point as the original code; logging cannot authorize a new relay attempt.
    closed?;
    match served? {
        Err(Error::Cancelled) if cancel.is_cancelled() => Ok(()),
        Err(Error::Closed)
            if disposition
                == Some(festerm_tunnel_bridge::relay::HostFailureDisposition::Transient) =>
        {
            Err(crate::cloud::transient_relay("host.runtime_stream_closed"))
        }
        _ => Err(refused("host.runtime_stream_closed")),
    }
}

#[cfg(test)]
async fn recover_relay(
    store: &Store,
    expected: &Snapshot,
    record: &mut RunRecord,
    cancel: &CancellationToken,
    attempt: impl std::ops::AsyncFnMut(&mut RunRecord) -> io::Result<()>,
) -> io::Result<()> {
    recover_relay_with_diagnostics(store, expected, record, cancel, attempt, None).await
}

async fn recover_relay_with_diagnostics(
    store: &Store,
    expected: &Snapshot,
    record: &mut RunRecord,
    cancel: &CancellationToken,
    mut attempt: impl std::ops::AsyncFnMut(&mut RunRecord) -> io::Result<()>,
    log: Option<&RuntimeLog>,
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
        persist_diagnostic(store, record, log)?;
        match attempt(record).await {
            Err(error) if crate::cloud::is_transient_relay(&error) => {
                record.state = RunState::Starting;
                record.failure = None;
                persist_diagnostic(store, record, log)?;
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

#[cfg(test)]
async fn supervise(
    store: &Store,
    expected: &Snapshot,
    cancel: CancellationToken,
    shutdown: CancellationToken,
    served: impl std::future::Future<Output = io::Result<()>>,
) -> io::Result<()> {
    supervise_with_diagnostics(store, expected, cancel, shutdown, served, None).await
}

async fn supervise_with_diagnostics(
    store: &Store,
    expected: &Snapshot,
    cancel: CancellationToken,
    shutdown: CancellationToken,
    served: impl std::future::Future<Output = io::Result<()>>,
    log: Option<&RuntimeLog>,
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
                Ok(_) => {
                    let error = refused("host.runtime_consent_changed");
                    if let Some(log) = log { log.event(DiagnosticPhase::PolicyWatch, DiagnosticOutcome::Error, Some(&error), None, None); }
                    break Err(error);
                },
                // A short current-policy guard owns this same installation
                // lock. Contention is unobservable state, not revocation.
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => {
                    if let Some(log) = log { log.event(DiagnosticPhase::PolicyWatch, DiagnosticOutcome::Error, Some(&error), None, None); }
                    break Err(error);
                },
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

    #[tokio::test]
    async fn ordinary_successful_maintenance_keeps_same_serving_future_live() {
        let cancel = CancellationToken::new();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (renewed, replacement) = tokio::sync::oneshot::channel();
        let progressed = std::cell::Cell::new(false);
        let served = async {
            started.send(()).unwrap();
            replacement.await.unwrap();
            assert!(
                !cancel.is_cancelled(),
                "successful renewal must not disconnect the relay"
            );
            progressed.set(true);
            Ok(())
        };
        let maintained = async {
            ready.await.unwrap();
            renewed.send(()).unwrap();
            cancel.cancelled().await;
            Ok(())
        };
        assert!(matches!(
            serve_with_account_maintenance(served, maintained, &cancel).await,
            Ok(Ok(()))
        ));
        assert!(progressed.get());
    }

    #[tokio::test]
    async fn ordinary_serve_end_drains_inflight_native_work_before_context_ends() {
        use festerm_tunnel_client::host_control::Error;
        let cancel = CancellationToken::new();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (finish, finished) = std::sync::mpsc::channel();
        let drained = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finished_flag = drained.clone();
        let served = async {
            ready.await.unwrap();
            Err(Error::Closed)
        };
        let maintained = async {
            crate::cloud::owned_blocking(move || {
                started.send(()).unwrap();
                finished.recv().unwrap();
                finished_flag.store(true, std::sync::atomic::Ordering::Release);
                Ok(())
            })
            .await
        };
        let release = async {
            cancel.cancelled().await;
            assert!(!drained.load(std::sync::atomic::Ordering::Acquire));
            finish.send(()).unwrap();
        };
        let (result, ()) = tokio::join!(
            serve_with_account_maintenance(served, maintained, &cancel),
            release
        );
        assert!(matches!(result, Ok(Err(Error::Closed))));
        assert!(drained.load(std::sync::atomic::Ordering::Acquire));
    }

    #[tokio::test]
    async fn ordinary_failed_renewal_drains_workers_and_is_not_relay_retryable() {
        use festerm_tunnel_client::host_control::Error;
        let cancel = CancellationToken::new();
        let drained = std::cell::Cell::new(false);
        let served = async {
            cancel.cancelled().await;
            drained.set(true);
            Err(Error::Cancelled)
        };
        let original = crate::cloud::transient_relay("host.cloud_rate_limited");
        let maintained = async { Err(crate::cloud::terminal_ordinary_renewal(original)) };
        let error = serve_with_account_maintenance(served, maintained, &cancel)
            .await
            .unwrap_err();
        assert!(drained.get());
        assert!(!crate::cloud::is_transient_relay(&error));
        assert_eq!(error.to_string(), "host.ordinary_account_renewal_failed");
        let (root, store) = fixture();
        provision(&store);
        let (_lease, expected, mut record) = reserve(&store).unwrap();
        let log = RuntimeLog::new(&root.path().join("host"), &record);
        let calls = std::cell::Cell::new(0);
        let mut failure = Some(error);
        let result = recover_relay_with_diagnostics(
            &store,
            &expected,
            &mut record,
            &CancellationToken::new(),
            async |_| {
                calls.set(calls.get() + 1);
                finish_relay_attempt(
                    &log,
                    true,
                    Err(failure.take().expect("no renewal replay")),
                    (
                        Some("host.outer_peer_disconnected"),
                        Some(festerm_tunnel_bridge::relay::HostFailureDisposition::Transient),
                    ),
                    async {
                        (
                            Ok(()),
                            Some("host.outer_peer_disconnected"),
                            Some(festerm_tunnel_bridge::relay::HostFailureDisposition::Transient),
                        )
                    },
                    &CancellationToken::new(),
                )
                .await
            },
            Some(&log),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            calls.get(),
            1,
            "successful relay drain cannot authorize credential refresh replay"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn host_runtime_diagnostics_preserve_primary_cleanup_and_original_retry_policy() {
        use festerm_tunnel_bridge::relay::HostFailureDisposition::{Permanent, Transient};
        for (cleanup_ok, post, expected_calls) in [
            (true, Transient, 2),
            (false, Transient, 1),
            (true, Permanent, 1),
        ] {
            let (root, store) = fixture();
            provision(&store);
            let (_lease, expected, mut record) = reserve(&store).unwrap();
            let log = RuntimeLog::new(&root.path().join("host"), &record);
            let calls = std::cell::Cell::new(0);
            let cancel = CancellationToken::new();
            let result = recover_relay_with_diagnostics(
                &store,
                &expected,
                &mut record,
                &cancel,
                async |_| {
                    calls.set(calls.get() + 1);
                    if calls.get() > 1 {
                        return Err(refused("host.fixture_terminal"));
                    }
                    finish_relay_attempt(
                        &log,
                        true,
                        Ok(Err(festerm_tunnel_client::host_control::Error::Closed)),
                        (Some("host.outer_peer_disconnected"), Some(Transient)),
                        async {
                            (
                                if cleanup_ok {
                                    Ok(())
                                } else {
                                    Err(io::Error::other("PRIVATE cleanup https://invalid.example"))
                                },
                                Some("host.outer_peer_disconnected"),
                                Some(post),
                            )
                        },
                        &cancel,
                    )
                    .await
                },
                Some(&log),
            )
            .await;
            assert!(result.is_err());
            assert_eq!(calls.get(), expected_calls);
            assert_eq!(store.snapshot().unwrap(), expected);
            let records = runtime_diagnostics::read(&store, &root.path().join("host"));
            assert_eq!(records.len(), 2);
            assert_eq!(records[0].phase, DiagnosticPhase::RuntimeServe);
            assert_eq!(records[0].disposition, Some(Transient));
            assert_eq!(records[1].phase, DiagnosticPhase::RelayCleanup);
            assert_eq!(records[1].disposition, Some(post));
            assert_eq!(
                records[1].outcome,
                if cleanup_ok {
                    DiagnosticOutcome::Ok
                } else {
                    DiagnosticOutcome::Error
                }
            );
            let text = std::fs::read_to_string(root.path().join("host/worker.log")).unwrap();
            assert!(!text.contains("PRIVATE") && !text.contains("https://"));
        }
    }

    #[tokio::test]
    async fn host_runtime_diagnostics_pre_serve_failure_never_invents_runtime_execution() {
        for cleanup_ok in [true, false] {
            let (root, store) = fixture();
            provision(&store);
            let (_lease, _, record) = reserve(&store).unwrap();
            persist(&store, &record).unwrap();
            let state = root.path().join("host");
            let log = RuntimeLog::new(&state, &record);
            let primary = io::Error::from_raw_os_error(13);
            log.event(
                DiagnosticPhase::PolicyRefresh,
                DiagnosticOutcome::Error,
                Some(&primary),
                None,
                None,
            );
            let closes = std::cell::Cell::new(0);
            let error = finish_relay_attempt(
                &log,
                false,
                Err(primary),
                (None, None),
                async {
                    closes.set(closes.get() + 1);
                    (
                        if cleanup_ok {
                            Ok(())
                        } else {
                            Err(io::Error::from_raw_os_error(5))
                        },
                        None,
                        None,
                    )
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
            assert_eq!(error.raw_os_error(), Some(if cleanup_ok { 13 } else { 5 }));
            assert_eq!(closes.get(), 1);
            let rows = runtime_diagnostics::read(&store, &state);
            assert_eq!(
                rows.iter().map(|row| row.phase).collect::<Vec<_>>(),
                [
                    DiagnosticPhase::PolicyRefresh,
                    DiagnosticPhase::RelayCleanup
                ]
            );
            assert!(!rows
                .iter()
                .any(|row| row.phase == DiagnosticPhase::RuntimeServe));
        }
    }

    #[tokio::test]
    async fn host_runtime_initial_preparation_reports_policy_failure_after_valid_helper() {
        let (root, store) = fixture();
        provision(&store);
        let state = root.path().join("host");
        let (_lease, original, mut record) = reserve(&store).unwrap();
        persist(&store, &record).unwrap();
        let stage = crate::services::fixture_stage(root.path());
        let mut expected = original.clone();
        // Inject the real prepare scope refusal before any vault or network work.
        expected.settings.friendly_name = "Changed expected scope".into();
        let error = run_admitted(
            store.clone(),
            &state,
            &expected,
            &mut record,
            &stage,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(error.to_string(), "host.ordinary_scope_changed");
        assert_eq!(
            record.failure,
            Some(RunFailure::AuthenticatedBundleRequired)
        );
        let rows = runtime_diagnostics::read(&store, &state);
        assert_eq!(
            rows.iter()
                .map(|row| (row.phase.code(), row.outcome))
                .collect::<Vec<_>>(),
            [
                ("helper_validation", DiagnosticOutcome::Ok),
                ("initial_policy_prepare", DiagnosticOutcome::Error),
            ]
        );
        assert_eq!(store.snapshot().unwrap(), original);
        assert!(!state.join("daemon").exists());
        assert!(!state.join("host-cloud.json").exists());
    }

    #[tokio::test]
    async fn host_runtime_initial_preparation_log_failure_preserves_original_error() {
        let (root, store) = fixture();
        provision(&store);
        let state = root.path().join("missing-log-directory");
        let (_lease, original, mut record) = reserve(&store).unwrap();
        persist(&store, &record).unwrap();
        let stage = crate::services::fixture_stage(root.path());
        let mut expected = original.clone();
        expected.settings.friendly_name = "Changed expected scope".into();
        let error = run_admitted(
            store.clone(),
            &state,
            &expected,
            &mut record,
            &stage,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(error.to_string(), "host.ordinary_scope_changed");
        assert_eq!(
            record.failure,
            Some(RunFailure::AuthenticatedBundleRequired)
        );
        assert!(runtime_diagnostics::read(&store, &state).is_empty());
        assert!(!state.exists());
        assert_eq!(store.snapshot().unwrap(), original);
    }

    #[tokio::test]
    async fn host_runtime_initial_preparation_reports_helper_failure_before_policy() {
        let (root, store) = fixture();
        provision(&store);
        let state = root.path().join("host");
        let (_lease, expected, mut record) = reserve(&store).unwrap();
        persist(&store, &record).unwrap();
        let stage = crate::services::fixture_stage(root.path());
        std::fs::remove_file(stage.sessiond_path().unwrap()).unwrap();
        let error = run_admitted(
            store.clone(),
            &state,
            &expected,
            &mut record,
            &stage,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(
            record.failure,
            Some(RunFailure::AuthenticatedBundleRequired)
        );
        let rows = runtime_diagnostics::read(&store, &state);
        assert_eq!(
            rows.iter()
                .map(|row| (row.phase.code(), row.outcome))
                .collect::<Vec<_>>(),
            [("helper_validation", DiagnosticOutcome::Error),]
        );
        assert_eq!(store.snapshot().unwrap(), expected);
        assert!(!state.join("daemon").exists());
        assert!(!state.join("host-cloud.json").exists());
    }

    #[tokio::test]
    async fn host_runtime_diagnostics_use_post_cleanup_observation_without_inventing_primary() {
        let (root, store) = fixture();
        provision(&store);
        let (_lease, _, record) = reserve(&store).unwrap();
        persist(&store, &record).unwrap();
        let log = RuntimeLog::new(&root.path().join("host"), &record);
        let result = finish_relay_attempt(
            &log,
            true,
            Ok(Err(festerm_tunnel_client::host_control::Error::Closed)),
            (None, None),
            async {
                (
                    Ok(()),
                    Some("host.outer_timeout"),
                    Some(festerm_tunnel_bridge::relay::HostFailureDisposition::Transient),
                )
            },
            &CancellationToken::new(),
        )
        .await;
        assert!(crate::cloud::is_transient_relay(&result.unwrap_err()));
        let rows = runtime_diagnostics::read(&store, &root.path().join("host"));
        assert_eq!(rows[0].outer, None);
        assert_eq!(rows[0].disposition, None);
        assert_eq!(rows[1].outer, Some("host.outer_timeout"));
    }

    #[tokio::test]
    async fn host_runtime_diagnostics_log_failure_never_changes_product_error_or_retries() {
        let (root, store) = fixture();
        provision(&store);
        let (_lease, _, record) = reserve(&store).unwrap();
        let log = RuntimeLog::new(&root.path().join("not-created"), &record);
        let error = finish_relay_attempt(
            &log,
            true,
            Ok(Err(festerm_tunnel_client::host_control::Error::Closed)),
            (None, None),
            async { (Err(io::Error::from_raw_os_error(5)), None, None) },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(5));
        assert!(!root.path().join("not-created").exists());
        assert!(runtime_diagnostics::read(&store, &root.path().join("not-created")).is_empty());
    }

    #[tokio::test]
    async fn host_runtime_diagnostics_watch_and_history_errors_remain_fail_closed() {
        let (root, store) = fixture();
        provision(&store);
        let (_lease, expected, record) = reserve(&store).unwrap();
        persist(&store, &record).unwrap();
        let state = root.path().join("host");
        let log = RuntimeLog::new(&state, &record);
        let mut foreign = record.clone();
        foreign.installation = uuid::Uuid::new_v4().to_string();
        let error = persist_diagnostic(&store, &foreign, Some(&log)).unwrap_err();
        assert_eq!(error.to_string(), "host.runtime_installation_changed");
        assert_eq!(last_run(&store).unwrap(), Some(record.clone()));
        assert_eq!(store.snapshot().unwrap(), expected);
        let directory = storage::private_directory(&state, false).unwrap();
        let original = std::fs::read(state.join("host-lifecycle.json")).unwrap();
        storage::replace_private(
            &directory,
            Path::new("host-lifecycle.json"),
            b"invalid fixture",
        )
        .unwrap();
        let kind = store.snapshot().unwrap_err().kind();
        let shutdown = CancellationToken::new();
        let drained = std::cell::Cell::new(false);
        let serving = async {
            shutdown.cancelled().await;
            drained.set(true);
            Ok(())
        };
        let error = supervise_with_diagnostics(
            &store,
            &expected,
            CancellationToken::new(),
            shutdown.clone(),
            serving,
            Some(&log),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), kind);
        assert!(drained.get());
        assert!(shutdown.is_cancelled());
        storage::replace_private(&directory, Path::new("host-lifecycle.json"), &original).unwrap();
        let rows = runtime_diagnostics::read(&store, &state);
        assert_eq!(
            rows.iter().map(|row| row.phase).collect::<Vec<_>>(),
            [
                DiagnosticPhase::HistoryPersist,
                DiagnosticPhase::PolicyWatch
            ]
        );
        assert_eq!(store.snapshot().unwrap(), expected);
    }

    #[test]
    fn ordinary_readiness_reasons_and_inplace_renewal_roundtrip_runtime_history_privately() {
        use crate::ordinary::ReadinessEvent;
        let (root, store) = fixture();
        provision(&store);
        let (_lease, _, record) = reserve(&store).unwrap();
        persist(&store, &record).unwrap();
        let state = root.path().join("host");
        let log = RuntimeLog::new(&state, &record);
        let error = io::Error::other("PRIVATE token https://invalid.example/account");
        for event in [
            ReadinessEvent::OwnerDigestChanged,
            ReadinessEvent::CloudDigestChanged,
            ReadinessEvent::IdentityNotReadyBefore,
            ReadinessEvent::IdentityNotReadyAfter,
            ReadinessEvent::RenewalOk,
            ReadinessEvent::RenewalError,
        ] {
            log.readiness(event, Some(&error));
        }
        let rows = runtime_diagnostics::read(&store, &state);
        assert_eq!(rows.len(), 6);
        assert_eq!(
            rows.iter().map(|row| row.phase).collect::<Vec<_>>(),
            vec![
                DiagnosticPhase::OrdinaryOwnerDigest,
                DiagnosticPhase::OrdinaryCloudDigest,
                DiagnosticPhase::OrdinaryIdentityBefore,
                DiagnosticPhase::OrdinaryIdentityAfter,
                DiagnosticPhase::OrdinaryRenewal,
                DiagnosticPhase::OrdinaryRenewal
            ]
        );
        assert_eq!(rows[4].outcome, DiagnosticOutcome::Ok);
        assert_eq!(rows[5].outcome, DiagnosticOutcome::Error);
        assert!(rows
            .iter()
            .all(|row| row.run_id.to_string() == record.run_id && row.epoch == record.epoch));
        let text = std::fs::read_to_string(state.join("worker.log")).unwrap();
        assert!(
            !text.contains("PRIVATE")
                && !text.contains("https://")
                && !text.contains("invalid.example")
        );
    }

    #[test]
    fn host_runtime_diagnostics_are_typed_private_bounded_and_correlated_history_only() {
        use runtime_diagnostics::Correlation;
        let (root, store) = fixture();
        provision(&store);
        let (_lease, _, mut record) = reserve(&store).unwrap();
        persist(&store, &record).unwrap();
        let state = root.path().join("host");
        let old = record.clone();
        let log = RuntimeLog::new(&state, &record);
        let error = crate::diagnostics::attach(
            io::Error::other("ghp_PRIVATE https://invalid.example/password"),
            crate::diagnostics::HostSetupDiagnostic {
                stage: crate::diagnostics::Stage::HostToken,
                http_status: Some(403),
                category: crate::diagnostics::Category::Denied,
                recovery: crate::diagnostics::Recovery::CheckAccount,
            },
        );
        log.event(
            DiagnosticPhase::RelayOpen,
            DiagnosticOutcome::Error,
            Some(&error),
            Some("private.lowercase.secret"),
            None,
        );
        let original = std::fs::read_to_string(state.join("worker.log")).unwrap();
        assert!(
            !original.contains("PRIVATE")
                && !original.contains("https://")
                && !original.contains("private.lowercase.secret")
        );
        let rows = runtime_diagnostics::read(&store, &state);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].category, Some(crate::diagnostics::Category::Denied));
        assert_eq!(rows[0].http_status, Some(403));
        assert_eq!(rows[0].outer, None);
        record.run_id = uuid::Uuid::new_v4().to_string();
        persist(&store, &record).unwrap();
        assert!(runtime_diagnostics::read(&store, &state).is_empty());
        store.with_locked(|directory, _| storage::replace_private(directory, Path::new("host-managed-result.json"), &serde_json::to_vec(&serde_json::json!({
            "version":1,"installation":old.installation,"origin":"headless","epoch":old.epoch,"run_id":old.run_id,"disposition":"failed","failure":"runtime-failed","product_success":false
        })).unwrap())).unwrap();
        let rows = runtime_diagnostics::read(&store, &state);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].correlation, Correlation::ManagedHistory);
        let current = RuntimeLog::new(&state, &record);
        for _ in 0..70 {
            current.event(
                DiagnosticPhase::PolicyWatch,
                DiagnosticOutcome::Error,
                None,
                None,
                None,
            );
        }
        assert_eq!(runtime_diagnostics::read(&store, &state).len(), 64);
        // Recognized event names do not make unknown/malformed fields safe.
        for bad in [
            original.replace("phase=relay_open", "phase=PRIVATE"),
            original.replace("http=403", "http=999"),
            original.replace("outcome=error", "outcome=error extra=private"),
            original.replace("category=denied", "category=unknown"),
            original.replace("outer=not_observed", "outer=private.secret"),
            original.replace("os=none", "os=9999999999999999999999999999"),
        ] {
            store
                .with_locked(|directory, _| {
                    storage::replace_private(directory, Path::new("worker.log"), bad.as_bytes())
                })
                .unwrap();
            assert!(runtime_diagnostics::read(&store, &state).is_empty());
        }
        let bad = original.replace("phase=relay_open", "phase=PRIVATE");
        store
            .with_locked(|directory, _| {
                storage::replace_private(directory, Path::new("worker.log"), bad.as_bytes())
            })
            .unwrap();
        assert!(runtime_diagnostics::read(&store, &state).is_empty());
        store
            .with_locked(|directory, _| {
                storage::replace_private(directory, Path::new("worker.log"), &vec![b'x'; 265_000])
            })
            .unwrap();
        assert!(runtime_diagnostics::read(&store, &state).is_empty());
        assert!(store.snapshot().is_ok());
    }

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
                    assert!(tokio::time::timeout(Duration::from_secs(3), &mut watched)
                        .await
                        .is_err());
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

    #[test]
    fn host_owner_readiness_is_noninteractive_read_only_and_never_advances_high_water() {
        use crate::enrollment::tests::{fixture_owners, setup_origin};
        fn files(path: &Path) -> std::collections::BTreeMap<std::ffi::OsString, Vec<u8>> {
            std::fs::read_dir(path)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (entry.file_name(), std::fs::read(entry.path()).unwrap())
                })
                .collect()
        }
        for case in ["current", "missing", "locked", "stale"] {
            let (root, store, vault) = setup_origin(Origin::Desktop);
            if case != "missing" {
                fixture_owners(&store, vault.clone())
                    .initialize(Path::new(HOST_IDENTITY_FILE), "github:1234".into())
                    .unwrap();
            }
            if case == "stale" {
                store.revoke().unwrap();
            }
            if case == "locked" {
                vault
                    .locked
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            let before = files(&root.path().join("installation"));
            let interactive = vault
                .interactive_reads
                .load(std::sync::atomic::Ordering::Acquire);
            let noninteractive = vault
                .noninteractive_reads
                .load(std::sync::atomic::Ordering::Acquire);
            let report = owner_readiness_with(
                || Ok(store),
                |store| Ok(fixture_owners(store, vault.clone())),
            );
            assert_eq!(files(&root.path().join("installation")), before);
            assert_eq!(
                vault
                    .interactive_reads
                    .load(std::sync::atomic::Ordering::Acquire),
                interactive
            );
            assert_eq!(
                vault
                    .noninteractive_reads
                    .load(std::sync::atomic::Ordering::Acquire),
                noninteractive + usize::from(case != "missing")
            );
            let wire = serde_json::to_string(&report).unwrap();
            assert!(report.status_only);
            assert_eq!(report.scope, "local_owner_admission");
            assert!(report.events.len() <= 24 && wire.len() < 4096);
            assert!(!wire.contains("github:1234"));
            assert!(!wire.contains("reference"));
            assert!(!wire.contains("private-canary"));
            let (stage, category) = match case {
                "current" => ("consent", "missing_consent"),
                "missing" => ("policy_read", "missing"),
                "locked" => ("anchor_read", "locked_or_unavailable"),
                _ => ("high_water_update", "scope_changed"),
            };
            assert!(report.events.iter().any(|event| event.stage == stage
                && event.outcome == "error"
                && event.category == category));
            assert!(!root.path().join("installation/worker.log").exists());
        }
    }

    #[test]
    fn host_owner_admission_preserves_provider_owner_consent_and_primary_failures() {
        use crate::enrollment::tests::{fixture_owners, setup_origin};
        for case in [
            "provider",
            "owner",
            "locked",
            "store",
            "log-failure",
            "owner-log-failure",
        ] {
            let (root, store, vault) = setup_origin(Origin::Desktop);
            if matches!(case, "provider" | "log-failure") {
                store.revoke().unwrap();
                let stopped = store.snapshot().unwrap();
                store
                    .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
                    .unwrap();
                let mut settings = stopped.settings;
                settings.ordinary_access = OrdinaryAccess::ProviderOwner;
                store.configure(settings.clone()).unwrap();
                let ticket = store.request_enable(&settings).unwrap();
                store
                    .record_provisioned(&ticket, stopped.binding.unwrap())
                    .unwrap();
            }
            let store = Arc::new(store);
            fixture_owners(&store, vault.clone())
                .initialize(Path::new(HOST_IDENTITY_FILE), "github:1234".into())
                .unwrap();
            let (_lease, expected, mut record) = reserve(&store).unwrap();
            let original_record = record.clone();
            let state = root.path().join("diagnostic-state");
            if case.ends_with("log-failure") {
                std::fs::write(&state, b"owned-file-not-a-log-directory").unwrap();
            } else {
                storage::private_directory(&state, true).unwrap();
            }
            if case == "locked" {
                vault
                    .locked
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            let reads = vault
                .interactive_reads
                .load(std::sync::atomic::Ordering::Acquire);
            let result = authorize_owner_admission(&expected, &mut record, &state, || {
                if case == "store" {
                    Err(io::Error::other(
                        "private-canary-account-reference-path-token",
                    ))
                } else {
                    Ok(fixture_owners(&store, vault.clone()))
                }
            });
            match case {
                "provider" | "log-failure" => assert!(result.is_ok()),
                "owner" | "owner-log-failure" => assert_eq!(
                    result.unwrap_err().to_string(),
                    "host.runtime_current_owner_consent_required"
                ),
                "locked" => assert_eq!(
                    result.unwrap_err().to_string(),
                    "owner.vault_locked_or_unavailable_unlock_native_store_and_retry"
                ),
                _ => assert_eq!(
                    result.unwrap_err().to_string(),
                    "private-canary-account-reference-path-token"
                ),
            }
            assert_eq!(store.snapshot().unwrap(), expected);
            assert_eq!(
                record.failure,
                Some(RunFailure::CurrentOwnerConsentRequired)
            );
            assert_eq!(record.run_id, original_record.run_id);
            assert_eq!(
                vault
                    .interactive_reads
                    .load(std::sync::atomic::Ordering::Acquire),
                reads + usize::from(case != "store")
            );
            if !case.ends_with("log-failure") {
                let log = std::fs::read_to_string(state.join("worker.log")).unwrap();
                assert!(log.contains("host.owner_admission"));
                assert!(log.contains(&format!("pid={} ", std::process::id())));
                assert!(!log.contains("private-canary"));
                assert!(!log.contains("github:1234"));
                assert!(!log.contains("host.runtime "));
                if case == "owner" {
                    assert!(log.contains("stage=consent outcome=error category=missing_consent"));
                } else if case == "locked" {
                    assert!(log.contains(
                        "stage=anchor_read outcome=error category=locked_or_unavailable"
                    ));
                    assert!(!log.contains("stage=consent"));
                } else if case == "store" {
                    assert!(log.contains("stage=store_open outcome=begin"));
                    assert!(log.contains("stage=store_open outcome=error category=unclassified"));
                    assert!(!log.contains("stage=snapshot"));
                }
            }
        }
    }

    fn fixture() -> (storage::PrivateTestDirectory, Arc<Store>) {
        fixture_startup(Startup::Manual)
    }

    #[test]
    fn host_file_helper_admission_never_reserves_or_changes_consent_and_fences_late_results() {
        for boundary in [
            "unsupported",
            "cancel-before",
            "cancel-during",
            "scope",
            "accepted",
            "off",
        ] {
            let (root, store) = fixture();
            let before = store.snapshot().unwrap();
            let mut reviewed = before.settings.clone();
            reviewed.ordinary_access = OrdinaryAccess::ProviderOwner;
            reviewed.filesystem = Some(festerm_tunnel_bridge::file_protocol::Policy {
                start_in: Some("/child".into()),
                delete: false,
                root: root.path().into(),
                write: false,
            });
            let cancel = CancellationToken::new();
            if boundary == "cancel-before" {
                cancel.cancel();
            }
            if boundary == "off" {
                reviewed.filesystem = None;
            }
            let calls = std::cell::Cell::new(0);
            let result = file_helper_admission(&store, &before, &reviewed, &cancel, || {
                calls.set(calls.get() + 1);
                match boundary {
                    "unsupported" => Err(refused("host.file_helper_unsupported")),
                    "cancel-during" => {
                        cancel.cancel();
                        Ok(())
                    }
                    "scope" => {
                        store.configure(crate::lifecycle::Settings {
                            friendly_name: "Different foreground editor".into(),
                            ..before.settings.clone()
                        })?;
                        Ok(())
                    }
                    _ => Ok(()),
                }
            });
            assert_eq!(result.is_ok(), matches!(boundary, "accepted" | "off"));
            assert_eq!(
                calls.get(),
                usize::from(!matches!(boundary, "off" | "cancel-before"))
            );
            let current = store.snapshot().unwrap();
            assert_eq!(current.phase, Phase::Disabled);
            assert!(current.pending_ticket().is_none());
            assert_eq!(current.epoch, before.epoch);
            assert_eq!(current.settings.filesystem, before.settings.filesystem);
            if boundary != "scope" {
                assert_eq!(current, before);
            }
            assert!(!root.path().join("host/native-service.json").exists());
        }
    }

    fn fixture_startup(startup: Startup) -> (storage::PrivateTestDirectory, Arc<Store>) {
        let root = storage::private_test_directory();
        let store = Arc::new(
            Store::create(
                &root.path().join("host"),
                Origin::Headless,
                Settings {
                    filesystem: None,
                    ordinary_access: Default::default(),
                    friendly_name: "Runtime fixture".into(),
                    startup,
                    allow_creation: false,
                    allow_session_termination: false,
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
    async fn host_update_reviewed_snapshot_refuses_changes_before_shared_admission() {
        let (root, store) = fixture();
        let state = root.path().join("host");
        provision(&store);
        fixture_disable(&store, &state).unwrap();
        let expected = store.snapshot().unwrap();
        let stage = crate::services::fixture_stage(&root.path().join("candidate"));
        let cancel = CancellationToken::new();
        for change in ["epoch", "settings", "origin", "binding"] {
            let mut stale = expected.clone();
            match change {
                "epoch" => stale.epoch += 1,
                "settings" => stale.settings.allow_creation = !stale.settings.allow_creation,
                "origin" => stale.origin = crate::lifecycle::Origin::Desktop,
                "binding" => stale.binding.as_mut().unwrap().account.push_str("-changed"),
                _ => unreachable!(),
            }
            let calls = std::cell::Cell::new(0);
            let error = update_current_with_admission(
                &store,
                &state,
                Some(&stale),
                &stage,
                "0.15.0",
                &cancel,
                |_| async {
                    calls.set(calls.get() + 1);
                    Ok(())
                },
            )
            .await
            .unwrap_err();
            assert_eq!(error.to_string(), "host.runtime_consent_changed");
            assert_eq!(calls.get(), 0);
            assert_eq!(store.snapshot().unwrap(), expected);
        }
        let calls = std::cell::Cell::new(0);
        let error = update_current_with_admission(
            &store,
            &state,
            Some(&expected),
            &stage,
            "0.15.0",
            &cancel,
            |_| async {
                calls.set(calls.get() + 1);
                Err(refused("fixture.shared_admission_reached"))
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "fixture.shared_admission_reached");
        assert_eq!(calls.get(), 1);
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
        assert!(tokio::time::timeout(Duration::from_secs(2), &mut watched)
            .await
            .is_err());
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
        assert!(authorize_run(
            &store,
            &root.path().join("host"),
            &expected,
            &mut record,
            &current
        )
        .is_err());
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
        assert!(tokio::time::timeout(Duration::from_secs(4), &mut watched)
            .await
            .is_err());
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
        assert!(disable_with(&store, Duration::ZERO, |epoch| {
            let mut observed = observed_stop(&store, epoch);
            observed.installation = uuid::Uuid::new_v4().to_string();
            Ok(observed)
        })
        .is_err());
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
        assert!(uninstall(&store, &root.path().join("host"), &cancel)
            .await
            .is_err());
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
        assert!(!root
            .path()
            .join("host")
            .join("native-service.json")
            .exists());
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
        assert!(!root
            .path()
            .join("host")
            .join("owner-enrollment.json")
            .exists());
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

    fn startup_service_file(store: &Store, create: bool) -> std::fs::File {
        store
            .with_locked(|directory, _| {
                storage::open_private_file(directory, Path::new("native-service.lock"), create)
            })
            .unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn host_runtime_initial_admission_waits_for_real_service_lease_then_succeeds() {
        let (_root, store) = fixture();
        provision(&store);
        let (_run_lease, expected, _) = reserve(&store).unwrap();
        let parent_service_lease = Lease::exclusive(startup_service_file(&store, true)).unwrap();
        let cancel = CancellationToken::new();
        // Model only the exact native service-operation gate, not owner trust.
        let admitted = initial_run_admission(&store, &expected, &cancel, |_| {
            Lease::exclusive(startup_service_file(&store, false))
        });
        tokio::pin!(admitted);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut admitted)
                .await
                .is_err()
        );
        assert!(
            reserve(&store).is_err(),
            "original run lease remains owned while waiting"
        );
        drop(parent_service_lease);
        let result = admitted.await.unwrap();
        assert!(result.is_some());
        assert_eq!(store.snapshot().unwrap(), expected);
        assert!(last_run(&store).unwrap().is_none());
    }

    // These fixtures enter the real supported Unix service context. Windows must
    // reject elevated or service-account contexts before reaching the service lease;
    // the platform-independent admission/lease tests remain enabled there.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test(start_paused = true)]
    async fn host_runtime_initial_admission_reopens_actual_stage_after_service_lease_drains() {
        let (root, store) = fixture();
        provision(&store);
        let (_run_lease, expected, mut record) = reserve(&store).unwrap();
        let parent_service_lease = Lease::exclusive(startup_service_file(&store, true)).unwrap();
        let cancel = CancellationToken::new();
        let state = root.path().join("host");
        let original_error = authorize_run(
            &store,
            &state,
            &expected,
            &mut record,
            &store.snapshot().unwrap(),
        )
        .err()
        .unwrap();
        assert_eq!(
            original_error.kind(),
            io::ErrorKind::WouldBlock,
            "old one-shot startup loses the same parent/child collision"
        );
        let error = {
            let admitted = initial_run_admission(&store, &expected, &cancel, |current| {
                authorize_run(&store, &state, &expected, &mut record, current)
            });
            tokio::pin!(admitted);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut admitted)
                    .await
                    .is_err()
            );
            drop(parent_service_lease);
            admitted
                .await
                .err()
                .expect("unregistered fixture must still fail closed")
        };
        assert_ne!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(error.to_string(), "host.service_not_installed");
        assert_eq!(
            record.failure,
            Some(RunFailure::AuthenticatedBundleRequired)
        );
        assert!(last_run(&store).unwrap().is_none());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test(start_paused = true)]
    async fn host_runtime_initial_admission_permanent_busy_keeps_original_error_and_bound() {
        let (root, store) = fixture();
        provision(&store);
        let (_run_lease, expected, mut record) = reserve(&store).unwrap();
        let _parent_service_lease = Lease::exclusive(startup_service_file(&store, true)).unwrap();
        let cancel = CancellationToken::new();
        let state = root.path().join("host");
        let started = tokio::time::Instant::now();
        let error = initial_run_admission(&store, &expected, &cancel, |current| {
            authorize_run(&store, &state, &expected, &mut record, current)
        })
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(error.to_string(), "host.installation_busy");
        assert_eq!(started.elapsed(), INITIAL_ADMISSION_CONTENTION);
        assert_eq!(store.snapshot().unwrap(), expected);
        assert!(last_run(&store).unwrap().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn host_runtime_initial_admission_refuses_changed_full_snapshot_while_waiting() {
        for change in ["revoke", "same-epoch-settings", "same-epoch-binding"] {
            let (_root, store) = fixture();
            provision(&store);
            let (_run_lease, expected, _) = reserve(&store).unwrap();
            let _parent_service_lease =
                Lease::exclusive(startup_service_file(&store, true)).unwrap();
            let cancel = CancellationToken::new();
            let calls = std::cell::Cell::new(0);
            let admitted = initial_run_admission(&store, &expected, &cancel, |_| {
                calls.set(calls.get() + 1);
                Lease::exclusive(startup_service_file(&store, false))
            });
            tokio::pin!(admitted);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut admitted)
                    .await
                    .is_err()
            );
            let original_calls = calls.get();
            if change == "revoke" {
                store.revoke().unwrap();
            } else {
                store
                    .with_locked(|directory, current| {
                        let mut changed = current.clone();
                        if change == "same-epoch-settings" {
                            changed.settings.friendly_name.push_str("-changed");
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
            let changed = store.snapshot().unwrap();
            let error = admitted.await.err().unwrap();
            assert_eq!(error.to_string(), "host.runtime_consent_changed");
            assert_eq!(
                calls.get(),
                original_calls,
                "changed scope never reaches native authorization"
            );
            assert_eq!(store.snapshot().unwrap(), changed);
            assert!(last_run(&store).unwrap().is_none());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn host_runtime_initial_admission_cancel_and_policy_reduction_do_not_publish() {
        for cancel_wait in [true, false] {
            let (_root, store) = fixture();
            provision(&store);
            let (_run_lease, expected, _) = reserve(&store).unwrap();
            let parent_service_lease =
                Lease::exclusive(startup_service_file(&store, true)).unwrap();
            let cancel = CancellationToken::new();
            let allowed = std::cell::Cell::new(true);
            let admitted = initial_run_admission(&store, &expected, &cancel, |_| {
                let _guard = Lease::exclusive(startup_service_file(&store, false))?;
                if !allowed.get() {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "current-owner-denied",
                    ));
                }
                Ok(())
            });
            tokio::pin!(admitted);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut admitted)
                    .await
                    .is_err()
            );
            if cancel_wait {
                cancel.cancel();
                assert!(admitted.await.unwrap().is_none());
            } else {
                allowed.set(false);
                drop(parent_service_lease);
                let error = admitted.await.unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                assert_eq!(error.to_string(), "current-owner-denied");
            }
            assert_eq!(store.snapshot().unwrap(), expected);
            assert!(last_run(&store).unwrap().is_none());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn host_runtime_initial_admission_nonbusy_errors_never_retry() {
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::InvalidData,
            io::ErrorKind::Other,
        ] {
            let (_root, store) = fixture();
            provision(&store);
            let expected = store.snapshot().unwrap();
            let calls = std::cell::Cell::new(0);
            let cancel = CancellationToken::new();
            let started = tokio::time::Instant::now();
            let error = initial_run_admission::<()>(&store, &expected, &cancel, |_| {
                calls.set(calls.get() + 1);
                Err(io::Error::new(kind, "original-native-error"))
            })
            .await
            .unwrap_err();
            assert_eq!(calls.get(), 1);
            assert_eq!(error.kind(), kind);
            assert_eq!(error.to_string(), "original-native-error");
            assert_eq!(started.elapsed(), Duration::ZERO);
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test(start_paused = true)]
    async fn host_runtime_initial_admission_real_child_entry_persists_cancel_or_busy_outcome() {
        for cancel_wait in [true, false] {
            let (root, store) = fixture();
            provision(&store);
            let (_run_lease, expected, mut record) = reserve(&store).unwrap();
            let _parent_service_lease =
                Lease::exclusive(startup_service_file(&store, true)).unwrap();
            let original_run = record.run_id.clone();
            let cancel = CancellationToken::new();
            let state = root.path().join("host");
            let result = {
                let child = run_to_completion(
                    store.clone(),
                    &state,
                    &expected,
                    &mut record,
                    cancel.clone(),
                );
                tokio::pin!(child);
                assert!(tokio::time::timeout(Duration::from_millis(50), &mut child)
                    .await
                    .is_err());
                assert_eq!(last_run(&store).unwrap().unwrap().state, RunState::Starting);
                assert!(
                    reserve(&store).is_err(),
                    "waiting child keeps the original run lease"
                );
                if cancel_wait {
                    cancel.cancel();
                }
                child.await
            };
            if cancel_wait {
                result.unwrap();
                assert_eq!(record.state, RunState::Stopped);
                assert_eq!(record.failure, None);
            } else {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
                assert_eq!(record.state, RunState::Failed);
                assert_eq!(record.failure, Some(RunFailure::InstallationBusy));
            }
            let saved = last_run(&store).unwrap().unwrap();
            assert_eq!(saved.run_id, original_run);
            assert_eq!(saved.epoch, expected.epoch);
            assert_eq!(saved.state, record.state);
            assert_eq!(saved.failure, record.failure);
            assert_eq!(store.snapshot().unwrap(), expected);
        }
    }
}
