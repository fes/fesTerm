use super::*;
use festerm_host_setup::{
    acquisition::host_bundle::{acquire_current_host_bundle, HostBundleError},
    artifacts::VerifiedArtifact,
    cloud::CloudHostAdapter,
    controller::{self, OwnerConsent, UninstallResult},
    enrollment::OwnerStorage,
    lifecycle::{Origin, Store},
    services::{Services, TrustedStage},
};
use std::{io, path::Path};

#[path = "sharing.rs"]
mod sharing;

pub(super) struct Native;

// Foreground completion evidence only; no runtime reservation or new authority.
#[derive(Clone, Copy)]
enum CompletionBoundary {
    Report,
    ServiceContext,
    ServiceStage,
    ServiceStart,
    FinalScope,
}
impl CompletionBoundary {
    fn code(self) -> &'static str {
        match self {
            Self::Report => "report",
            Self::ServiceContext => "service_context",
            Self::ServiceStage => "service_stage",
            Self::ServiceStart => "service_start",
            Self::FinalScope => "final_scope",
        }
    }
}

#[derive(Clone, Copy)]
enum CompletionOutcome {
    Begin,
    Ok,
    Error,
}
impl CompletionOutcome {
    fn code(self) -> &'static str {
        match self {
            Self::Begin => "begin",
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }
}

fn completion_failure_code(error: Failure) -> &'static str {
    match error {
        Failure::Busy => "busy",
        Failure::Observation { .. } => "observation",
        Failure::Cancelled => "cancelled",
        Failure::Timeout => "timeout",
        Failure::NativeTimeout => "native_timeout",
        Failure::RefreshFirst => "refresh_first",
        Failure::ScopeChanged => "scope_changed",
        Failure::PrivateStorage => "private_storage",
        Failure::FileAccessReview => "file_access_review",
        Failure::MacHostHelperUnsupported => "mac_host_helper_unsupported",
        Failure::MacHostIdentityUnsupported => "mac_host_identity_unsupported",
        Failure::FileHelperUnsupported => "file_helper_unsupported",
        Failure::SignedBundle => "signed_bundle",
        Failure::BundleNotPublished => "bundle_not_published",
        Failure::AcquisitionUnavailable => "acquisition_unavailable",
        Failure::NativeVault => "native_vault",
        Failure::SignIn => "sign_in",
        Failure::OwnerApproval => "owner_approval",
        Failure::PlatformPrerequisite => "platform_prerequisite",
        Failure::Reconciliation => "reconciliation",
        Failure::ShareCapability => "share_capability",
        Failure::SessionChanged => "session_changed",
        Failure::NoNativeObservation => "no_native_observation",
        Failure::UpdateBlocked => "update_blocked",
        Failure::UpdateIncomplete => "update_incomplete",
        Failure::InvalidDraft => "invalid_draft",
        Failure::CloudUncertain => "cloud_uncertain",
    }
}
fn completion_kind(kind: io::ErrorKind) -> &'static str {
    match kind {
        io::ErrorKind::NotFound => "missing",
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::WouldBlock => "busy",
        io::ErrorKind::TimedOut => "timed_out",
        io::ErrorKind::Interrupted => "interrupted",
        io::ErrorKind::InvalidData => "invalid_data",
        io::ErrorKind::InvalidInput => "invalid_input",
        io::ErrorKind::Unsupported => "unsupported",
        io::ErrorKind::AlreadyExists => "already_exists",
        _ => "unclassified",
    }
}
#[derive(Clone, Copy)]
enum CompletionReason {
    NativeOperationFailed,
    NativeTimeout,
    StartNotObservedRunning,
    ExecutableChanged,
    AuthenticatedBundleChanged,
    StagedArtifactMismatch,
    Unknown,
}
impl CompletionReason {
    fn from_error(error: &io::Error) -> Self {
        // Exact known static errors only. Never emit or classify arbitrary text
        // by substring, or walk a possibly credential-bearing source chain.
        match error.to_string().as_str() {
            "host.service_native_operation_failed" => Self::NativeOperationFailed,
            "host.service_native_timeout" => Self::NativeTimeout,
            "host.service_start_not_observed_running" => Self::StartNotObservedRunning,
            "host.service_executable_changed" => Self::ExecutableChanged,
            "host.service_authenticated_bundle_changed" => Self::AuthenticatedBundleChanged,
            "host.staged_artifact_mismatch" => Self::StagedArtifactMismatch,
            _ => Self::Unknown,
        }
    }
    fn code(self) -> &'static str {
        match self {
            Self::NativeOperationFailed => "native_operation_failed",
            Self::NativeTimeout => "native_timeout",
            Self::StartNotObservedRunning => "start_not_observed_running",
            Self::ExecutableChanged => "executable_changed",
            Self::AuthenticatedBundleChanged => "authenticated_bundle_changed",
            Self::StagedArtifactMismatch => "staged_artifact_mismatch",
            Self::Unknown => "unknown",
        }
    }
}
struct CompletionError {
    kind: Option<io::ErrorKind>,
    os: Option<i32>,
    reason: Option<CompletionReason>,
    failure: Failure,
}
fn completion_event(
    root: &Path,
    epoch: u64,
    boundary: CompletionBoundary,
    outcome: CompletionOutcome,
    error: Option<CompletionError>,
) {
    let os = error
        .as_ref()
        .and_then(|error| error.os)
        .map_or_else(|| "none".to_owned(), |code| code.to_string());
    festerm_tunnel_bridge::worker_log::WorkerLog::new(root).event(
        "host.enable_completion",
        &[
            ("epoch", &epoch.to_string()),
            ("stage", boundary.code()),
            ("outcome", outcome.code()),
            (
                "kind",
                error
                    .as_ref()
                    .and_then(|error| error.kind)
                    .map_or("none", completion_kind),
            ),
            (
                "failure",
                error
                    .as_ref()
                    .map_or("none", |error| completion_failure_code(error.failure)),
            ),
            ("os", &os),
            (
                "reason",
                error
                    .as_ref()
                    .and_then(|error| error.reason)
                    .map_or("none", CompletionReason::code),
            ),
        ],
    );
}
fn completion_io<T>(
    root: &Path,
    epoch: u64,
    boundary: CompletionBoundary,
    observation: ObservationBoundary,
    operation: impl FnOnce() -> io::Result<T>,
) -> Result<T, Failure> {
    completion_event(root, epoch, boundary, CompletionOutcome::Begin, None);
    completion_io_result(root, epoch, boundary, observation, operation())
}

fn completion_io_result<T>(
    root: &Path,
    epoch: u64,
    boundary: CompletionBoundary,
    observation: ObservationBoundary,
    result: io::Result<T>,
) -> Result<T, Failure> {
    match result {
        Ok(value) => {
            completion_event(root, epoch, boundary, CompletionOutcome::Ok, None);
            Ok(value)
        }
        Err(error) => {
            let kind = error.kind();
            let os = error.raw_os_error();
            let reason = CompletionReason::from_error(&error);
            let failure = observation_failure(error, observation);
            completion_event(
                root,
                epoch,
                boundary,
                CompletionOutcome::Error,
                Some(CompletionError {
                    kind: Some(kind),
                    os,
                    reason: Some(reason),
                    failure,
                }),
            );
            Err(failure)
        }
    }
}
// The initial read and full consent comparison are unchanged. Only contention
// may wait, bounded by the still-owned native start operation's original cap.
#[cfg(any(test, not(target_os = "macos")))]
fn final_enable_scope(
    root: &Path,
    expected: &Snapshot,
    cancel: &CancellationToken,
    until: std::time::Instant,
    mut read: impl FnMut() -> io::Result<Snapshot>,
) -> Result<(), Failure> {
    completion_event(
        root,
        expected.epoch,
        CompletionBoundary::FinalScope,
        CompletionOutcome::Begin,
        None,
    );
    let mut last_busy = None;
    let result = loop {
        if cancel.is_cancelled() {
            completion_event(
                root,
                expected.epoch,
                CompletionBoundary::FinalScope,
                CompletionOutcome::Error,
                Some(CompletionError {
                    kind: None,
                    os: None,
                    reason: None,
                    failure: Failure::Cancelled,
                }),
            );
            return Err(Failure::Cancelled);
        }
        if last_busy.is_some() && std::time::Instant::now() >= until {
            break Err(last_busy.take().expect("retained contention error"));
        }
        match read() {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                last_busy = Some(error);
                std::thread::sleep(
                    until
                        .saturating_duration_since(std::time::Instant::now())
                        .min(std::time::Duration::from_millis(25)),
                );
            }
            result => break result,
        }
    };
    let current = completion_io_result(
        root,
        expected.epoch,
        CompletionBoundary::FinalScope,
        ObservationBoundary::FinalScope,
        result,
    )?;
    if current != *expected {
        return Err(Failure::ScopeChanged);
    }
    Ok(())
}

// Diagnostic only: preserve the controller's exact Result before its existing
// failure mapping. No epoch/account/run IDs or free-form error values are logged.
fn disable_completion_result<T>(root: &Path, result: io::Result<T>) -> io::Result<T> {
    if let Err(error) = &result {
        let os = error
            .raw_os_error()
            .map_or_else(|| "none".to_owned(), |code| code.to_string());
        festerm_tunnel_bridge::worker_log::WorkerLog::new(root).event(
            "host.disable_completion",
            &[
                ("outcome", "error"),
                ("kind", completion_kind(error.kind())),
                ("os", &os),
                ("reason", disable_completion_reason(error)),
            ],
        );
    }
    result
}

fn disable_completion_reason(error: &io::Error) -> &'static str {
    match error.to_string().as_str() {
        "host.service_invalid_observation" => "invalid_observation",
        "host.service_observation_unqualified" => "observation_unqualified",
        "host.service_launchd_observation_unavailable" => "launchd_observation_unavailable",
        "host.service_foreign_native_definition" => "foreign_native_definition",
        "host.service_foreign_definition" => "foreign_definition",
        "host.service_definition_permissions" => "definition_permissions",
        "host.service_invalid_receipt" => "invalid_receipt",
        "host.service_foreign_receipt" => "foreign_receipt",
        "host.service_stop_not_observed" => "stop_not_observed",
        "host.runtime_stop_pending" => "runtime_stop_pending",
        "host.installation_busy" => "installation_busy",
        "host.service_native_output_limit" => "native_output_limit",
        "host.service_native_spawn_failed" => "native_spawn_failed",
        "host.service_foreign_state_directory" => "foreign_state_directory",
        "host.service_lock_missing_reconciliation_required" => "service_lock_missing",
        "host.runtime_lock_missing_reconciliation_required" => "runtime_lock_missing",
        "host.runtime_consent_changed" => "runtime_consent_changed",
        "host.service_stale_or_unapproved_operation" => "stale_operation",
        _ => CompletionReason::from_error(error).code(),
    }
}

fn completion_report(
    root: &Path,
    epoch: u64,
    operation: impl FnOnce() -> Result<Report, Failure>,
) -> Result<Report, Failure> {
    let boundary = CompletionBoundary::Report;
    completion_event(root, epoch, boundary, CompletionOutcome::Begin, None);
    let result = operation();
    match &result {
        Ok(_) => completion_event(root, epoch, boundary, CompletionOutcome::Ok, None),
        Err(error) => completion_event(
            root,
            epoch,
            boundary,
            CompletionOutcome::Error,
            Some(CompletionError {
                kind: None,
                os: None,
                reason: None,
                failure: *error,
            }),
        ),
    }
    result
}

fn acquisition_failure(error: HostBundleError) -> Failure {
    match error {
        HostBundleError::Cancelled => Failure::Cancelled,
        HostBundleError::NotPublished(_) => Failure::BundleNotPublished,
        HostBundleError::UnsupportedTarget => Failure::PlatformPrerequisite,
        HostBundleError::Verification(_) | HostBundleError::Identity => Failure::SignedBundle,
        _ => Failure::AcquisitionUnavailable,
    }
}

fn acquire(request: &Request, cancel: &CancellationToken) -> Result<VerifiedArtifact, Failure> {
    if !request.draft.manual_bundle {
        return acquire_current_host_bundle(cancel).map_err(acquisition_failure);
    }
    let paths = [
        &request.draft.manifest,
        &request.draft.signature,
        &request.draft.archive,
    ];
    if paths.iter().any(|path| !Path::new(path).is_absolute()) {
        return Err(Failure::SignedBundle);
    }
    festerm_host_setup::staging::verify_release_files(
        Path::new(paths[0]),
        Path::new(paths[1]),
        Path::new(paths[2]),
    )
    .map_err(|_| Failure::SignedBundle)
}

fn enable_scope(request: &Request, cancel: &CancellationToken) -> Result<(), Failure> {
    if cancel.is_cancelled() {
        return Err(Failure::Cancelled);
    }
    let account = request
        .draft
        .account
        .parse::<u64>()
        .ok()
        .filter(|id| *id != 0)
        .ok_or(Failure::SignIn)?;
    request
        .draft
        .settings()
        .validate()
        .map_err(|_| Failure::InvalidDraft)?;
    let current = observed(&request.root)?;
    verify(current.as_ref(), &request.expected)?;
    if let Some(store) = current {
        let snapshot = store.snapshot().map_err(failure)?;
        if !matches!(snapshot.phase, Phase::Disabled | Phase::Provisioned) {
            return Err(Failure::Reconciliation);
        }
        if snapshot.phase == Phase::Provisioned && snapshot.settings != request.draft.settings() {
            return Err(Failure::FileAccessReview);
        }
        if snapshot
            .binding
            .as_ref()
            .is_some_and(|binding| binding.account != format!("github:{account}"))
        {
            return Err(Failure::SignIn);
        }
        if let Some(revision) = request.owner_revision {
            let owner = store
                .owner_enrollment()
                .and_then(|owners| owners.snapshot())
                .map_err(failure)?;
            if owner.storage != OwnerStorage::NativeVault || owner.origin != Origin::Desktop {
                return Err(Failure::NativeVault);
            }
            if owner.revision.get() != revision {
                return Err(Failure::ScopeChanged);
            }
        }
    }
    Ok(())
}

fn update_scope(request: &Request, cancel: &CancellationToken) -> Result<(), Failure> {
    if cancel.is_cancelled() {
        return Err(Failure::Cancelled);
    }
    let Scope::Present(expected) = &request.expected else {
        return Err(Failure::UpdateBlocked);
    };
    if expected.phase != Phase::Disabled || expected.binding.is_none() {
        return Err(Failure::UpdateBlocked);
    }
    let store = observed(&request.root)?.ok_or(Failure::ScopeChanged)?;
    verify(Some(&store), &request.expected)?;
    let current = Services::current_user(&store, &request.root)
        .and_then(|services| services.release_metadata())
        .map_err(|_| Failure::UpdateBlocked)?;
    check_update_release(request.release.as_ref(), current.as_ref())
}

fn check_update_release(
    expected: Option<&festerm_host_setup::services::ReleaseMetadata>,
    current: Option<&festerm_host_setup::services::ReleaseMetadata>,
) -> Result<(), Failure> {
    let Some(current) = current else {
        return Err(Failure::UpdateBlocked);
    };
    if expected != Some(current) {
        return Err(Failure::ScopeChanged);
    }
    if !current.update_available(festerm_host_setup::VERSION) {
        return Err(Failure::UpdateBlocked);
    }
    Ok(())
}

fn bundle_scope(request: &Request, cancel: &CancellationToken) -> Result<(), Failure> {
    match request.operation {
        Operation::Enable => enable_scope(request, cancel),
        Operation::Update => update_scope(request, cancel),
        _ => Err(Failure::InvalidDraft),
    }
}

fn prepare_bundle<T>(
    request: &Request,
    cancel: &CancellationToken,
    progress: &SyncSender<Progress>,
    acquire: impl FnOnce(&Request, &CancellationToken) -> Result<T, Failure>,
) -> Result<T, Failure> {
    bundle_scope(request, cancel)?;
    let artifact = acquire(request, cancel)?;
    // Verification is synchronous and bounded, not CPU-preempted. Never accept
    // its owned bytes against a cancelled or replaced consent scope.
    bundle_scope(request, cancel)?;
    let _ = progress.try_send(Progress::Bundle(BundleStatus::Authenticated));
    Ok(artifact)
}

fn failure(error: io::Error) -> Failure {
    if error.kind() == io::ErrorKind::WouldBlock {
        return Failure::Busy;
    }
    // Error details can contain credential diagnostics or resource URLs. Only
    // classify them; never forward native/cloud error strings to a widget.
    let detail = error.to_string();
    if error.kind() == io::ErrorKind::Other
        && detail == "host.service_native_timeout"
        && !error
            .get_ref()
            .is_some_and(|inner| inner.is::<io::Error>() || inner.source().is_some())
    {
        Failure::NativeTimeout
    } else if error.kind() == io::ErrorKind::Other
        && detail == "host.mac_host_identity_incompatible"
        && !error
            .get_ref()
            .is_some_and(|inner| inner.is::<io::Error>() || inner.source().is_some())
    {
        Failure::MacHostIdentityUnsupported
    } else if error.kind() == io::ErrorKind::Other
        && detail == "host.mac_host_access_helper_required"
        && !error
            .get_ref()
            .is_some_and(|inner| inner.is::<io::Error>() || inner.source().is_some())
    {
        Failure::MacHostHelperUnsupported
    } else if detail == "host.file_helper_unsupported" {
        Failure::FileHelperUnsupported
    } else if detail.contains("vault")
        || detail.contains("native_store")
        || detail.contains("secure_store")
    {
        Failure::NativeVault
    } else if detail.contains("startup")
        || detail.contains("unsupported_platform")
        || detail.contains("windows_authenticated_graceful_stop")
    {
        Failure::PlatformPrerequisite
    } else if detail.contains("artifact")
        || detail.contains("signature")
        || detail.contains("publisher")
        || detail.contains("bundle")
    {
        Failure::SignedBundle
    } else if detail.contains("changed") || detail.contains("stale") {
        Failure::ScopeChanged
    } else if detail.contains("account") || detail.contains("sign_in") {
        Failure::SignIn
    } else if detail.contains("owner") {
        Failure::OwnerApproval
    } else if detail.contains("cleanup") || detail.contains("reconcil") {
        Failure::Reconciliation
    } else if detail.contains("cloud") {
        Failure::CloudUncertain
    } else {
        Failure::PrivateStorage
    }
}

fn observed(root: &Path) -> Result<Option<Store>, Failure> {
    match std::fs::symlink_metadata(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(observation_failure(error, ObservationBoundary::Scope)),
        Ok(_) => Store::open(root, Origin::Desktop)
            .map(Some)
            .map_err(|error| observation_failure(error, ObservationBoundary::Scope)),
    }
}

fn verify(store: Option<&Store>, expected: &Scope) -> Result<(), Failure> {
    match (store, expected) {
        (None, Scope::Absent) => Ok(()),
        (Some(store), Scope::Present(expected))
            if store.snapshot().map_err(failure)? == **expected =>
        {
            Ok(())
        }
        _ => Err(Failure::ScopeChanged),
    }
}

fn report(store: Option<&Store>, root: &Path) -> Result<Report, Failure> {
    report_with_run_history(store, root, controller::last_run)
}

fn report_with_run_history(
    store: Option<&Store>,
    root: &Path,
    read_run: impl FnOnce(&Store) -> io::Result<Option<controller::RunRecord>>,
) -> Result<Report, Failure> {
    let Some(store) = store else {
        return Ok(Report {
            scope: Scope::Absent,
            ..Report::default()
        });
    };
    let snapshot = store
        .snapshot()
        .map_err(|error| observation_failure(error, ObservationBoundary::Scope))?;
    let cleanup = if snapshot.phase == Phase::CleanupPending {
        CloudHostAdapter::new(store)
            .pending_cleanup_context(&snapshot)
            .ok()
    } else {
        None
    };
    let (last_run, warning) = if snapshot.phase == Phase::Provisioned {
        match read_run(store) {
            Ok(record) => (current_run_state(&snapshot, record), None),
            Err(error) => (
                None,
                Some(observation_warning(error, ObservationBoundary::RunHistory)),
            ),
        }
    } else {
        (None, None)
    };
    // Recorded version is display metadata, never old-bundle execution trust.
    // Missing/corrupt provenance must not block ordinary Disable or recovery.
    let release = Services::current_user(store, root)
        .and_then(|services| services.release_metadata())
        .ok()
        .flatten();
    Ok(Report {
        release,
        last_run,
        runtime_history: controller::runtime_diagnostics::read(store, root),
        scope: Scope::Present(snapshot.into()),
        cleanup,
        failure: warning,
        ..Report::default()
    })
}

fn observation_warning(error: io::Error, boundary: ObservationBoundary) -> Failure {
    Failure::Observation {
        boundary,
        kind: error.kind(),
    }
}

// Only contention is an observation warning on service/consent paths. Other
// errors retain the existing owner, scope, artifact and platform refusals.
fn observation_failure(error: io::Error, boundary: ObservationBoundary) -> Failure {
    if error.kind() == io::ErrorKind::WouldBlock {
        observation_warning(error, boundary)
    } else {
        failure(error)
    }
}

fn retain_observation_failure(mut report: Report, error: Failure) -> Result<Report, Failure> {
    if !matches!(error, Failure::Observation { .. }) {
        return Err(error);
    }
    report.failure = Some(error);
    report.process = None;
    report.last_run = None;
    report.runtime_history.clear();
    report.owner_binding = None;
    report.owner_approved = false;
    report.owner_revision = None;
    Ok(report)
}

fn current_run_state(
    snapshot: &Snapshot,
    record: Option<controller::RunRecord>,
) -> Option<controller::RunState> {
    record
        .filter(|record| {
            snapshot.phase == Phase::Provisioned
                && record.installation == snapshot.installation
                && record.epoch == snapshot.epoch
        })
        .map(|record| record.state)
}

fn public_owner(
    report: &mut Report,
    owner: festerm_host_setup::enrollment::EnrollmentSnapshot,
) -> Result<(), Failure> {
    if owner.storage != OwnerStorage::NativeVault || owner.origin != Origin::Desktop {
        return Err(Failure::NativeVault);
    }
    let Scope::Present(snapshot) = &report.scope else {
        return Err(Failure::ScopeChanged);
    };
    if owner.binding.installation != snapshot.installation || snapshot.phase != Phase::Provisioned {
        return Err(Failure::ScopeChanged);
    }
    report.owner_binding =
        Some(serde_json::to_string_pretty(&owner.binding).map_err(|_| Failure::OwnerApproval)?);
    report.owner_approved = owner.remote_access && owner.keys.iter().any(|key| !key.revoked);
    report.owner_revision = Some(owner.revision.get());
    Ok(())
}

fn cancellation_scope(store: &Store, request: &Request) -> Result<Snapshot, Failure> {
    let owned = request.owned.lock().map_err(|_| Failure::ScopeChanged)?;
    let Some(owned) = owned.as_ref() else {
        return Err(Failure::ScopeChanged);
    };
    let current = store.snapshot().map_err(failure)?;
    if current.installation != owned.installation
        || current.origin != owned.origin
        || current.epoch != owned.epoch
    {
        return Err(Failure::ScopeChanged);
    }
    Ok(current)
}

// Local reobservation can discover a replacement while a cloud call drains.
// Bind diagnostic metadata to the epoch this worker actually owned, including
// a first Enable whose original expected scope was Absent.
fn scoped_diagnostic(
    diagnostic: Option<HostSetupDiagnostic>,
    request: &Request,
    scope: &Scope,
) -> Option<HostSetupDiagnostic> {
    let diagnostic = diagnostic?;
    let owned = request.owned.lock().ok()?;
    let owned = owned.as_ref()?;
    match scope {
        Scope::Present(current)
            if current.installation == owned.installation
                && current.origin == owned.origin
                && current.epoch == owned.epoch =>
        {
            Some(diagnostic)
        }
        _ => None,
    }
}

fn run_async<T>(
    runtime: &tokio::runtime::Runtime,
    cancel: &CancellationToken,
    diagnostic: &mut Option<HostSetupDiagnostic>,
    future: impl std::future::Future<Output = io::Result<T>>,
) -> Result<T, Failure> {
    run_async_with_deadline(
        runtime,
        cancel,
        diagnostic,
        std::time::Duration::from_secs(300),
        future,
    )
}

// Private injection keeps deadline/cancellation/error tests deterministic without
// changing the production timeout, the future's authority, or cleanup fences.
fn run_async_with_deadline<T>(
    runtime: &tokio::runtime::Runtime,
    cancel: &CancellationToken,
    diagnostic: &mut Option<HostSetupDiagnostic>,
    deadline: std::time::Duration,
    future: impl std::future::Future<Output = io::Result<T>>,
) -> Result<T, Failure> {
    runtime.block_on(async {
        match tokio::time::timeout(deadline, future).await {
            Ok(result) => result.map_err(|error| {
                *diagnostic = festerm_host_setup::diagnostics::from_io_error(&error);
                failure(error)
            }),
            Err(_) => {
                // An already-requested operator cancellation is not reclassified
                // as a timer failure. Ready backend errors retain their own cause.
                if cancel.is_cancelled() {
                    Err(Failure::Cancelled)
                } else {
                    cancel.cancel();
                    Err(Failure::Timeout)
                }
            }
        }
    })
}

fn update_failure(error: Failure) -> Failure {
    match error {
        Failure::Cancelled | Failure::Timeout | Failure::NativeTimeout | Failure::ScopeChanged => {
            error
        }
        _ => Failure::UpdateIncomplete,
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, Failure> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| Failure::Busy)
}

impl Backend for Native {
    fn execute(
        &self,
        request: Request,
        cancel: &CancellationToken,
        progress: SyncSender<Progress>,
    ) -> Result<Report, Failure> {
        if let Operation::Share(step) = request.operation {
            return sharing::execute(request, step, cancel);
        }
        self.execute_with(request, cancel, progress, acquire, |artifact, root| {
            TrustedStage::publish(&artifact, root).map_err(|_| Failure::SignedBundle)
        })
    }
}

impl Native {
    fn execute_with<T>(
        &self,
        mut request: Request,
        cancel: &CancellationToken,
        progress: SyncSender<Progress>,
        acquire: impl FnOnce(&Request, &CancellationToken) -> Result<T, Failure>,
        publish: impl FnOnce(T, &Path) -> Result<TrustedStage, Failure>,
    ) -> Result<Report, Failure> {
        if cancel.is_cancelled() {
            return Err(Failure::Cancelled);
        }
        let existing = observed(&request.root)?;
        if request.operation == Operation::Refresh {
            return report(existing.as_ref(), &request.root);
        }
        if request.cancel_fence {
            let store = existing.as_ref().ok_or(Failure::ScopeChanged)?;
            request.expected = Scope::Present(cancellation_scope(store, &request)?.into());
        } else {
            verify(existing.as_ref(), &request.expected)?;
        }
        let artifact = if matches!(request.operation, Operation::Enable | Operation::Update) {
            Some(prepare_bundle(&request, cancel, &progress, acquire)?)
        } else {
            None
        };
        let existing = observed(&request.root)?;
        verify(existing.as_ref(), &request.expected)?;
        let store = match existing {
            Some(store) => store,
            None if request.operation == Operation::Enable => {
                Store::create(&request.root, Origin::Desktop, request.draft.settings())
                    .map_err(failure)?
            }
            None => return Err(Failure::ScopeChanged),
        };
        if !request.cancel_fence {
            let scope = store.snapshot().map_err(failure)?;
            if matches!(request.expected, Scope::Absent) {
                request.expected = Scope::Present(scope.clone().into());
            } else if request.expected != Scope::Present(scope.clone().into()) {
                return Err(Failure::ScopeChanged);
            }
            *request.owned.lock().map_err(|_| Failure::ScopeChanged)? = Some(scope);
        }
        let mut diagnostic = None;
        let result = (|| {
            if cancel.is_cancelled() {
                return Err(Failure::Cancelled);
            }
            let stage = match artifact {
                Some(artifact) => {
                    bundle_scope(&request, cancel)?;
                    let _ = progress.try_send(Progress::Bundle(BundleStatus::Qualifying));
                    Some(publish(
                        artifact,
                        &request.root.join("signed-release-cache"),
                    )?)
                }
                None => None,
            };
            if cancel.is_cancelled() {
                return Err(Failure::Cancelled);
            }
            if matches!(request.operation, Operation::Enable | Operation::Update) {
                bundle_scope(&request, cancel)?;
            }
            self.perform(
                &store,
                &request,
                cancel,
                progress,
                stage.as_ref(),
                &mut diagnostic,
            )
        })();
        if cancel.is_cancelled() && request.operation.needs_cancel_fence() {
            let deadline_expired = matches!(result, Err(Failure::Timeout));
            // A blocked native call may outlive cancellation. Fence once it
            // really returns as well as through the independent revocation slot.
            let failure = match cancellation_scope(&store, &request) {
                Ok(expected) => {
                    match controller::disable_if_current(&store, &request.root, &expected) {
                        Ok(_) if deadline_expired => Failure::Timeout,
                        Ok(_) => Failure::Cancelled,
                        Err(error) => failure(error),
                    }
                }
                Err(error) => error,
            };
            // A failed observation cannot replace the completed fence result.
            // Default scope is Unobserved and carries no readiness authority.
            let mut observed = report(Some(&store), &request.root).unwrap_or_default();
            observed.failure = Some(failure);
            return Ok(observed);
        }
        match result {
            Ok(report) => Ok(report),
            Err(error) => {
                // Reobserve local protected partial progress without another
                // credential/network/native operation or a success assertion.
                // Keep the primary refusal even if protected reobservation is
                // unavailable; never publish the old pre-operation scope.
                let mut observed = report(Some(&store), &request.root).unwrap_or_default();
                observed.diagnostic = scoped_diagnostic(diagnostic, &request, &observed.scope);
                observed.failure = Some(error);
                Ok(observed)
            }
        }
    }
    fn perform(
        &self,
        store: &Store,
        request: &Request,
        cancel: &CancellationToken,
        progress: SyncSender<Progress>,
        stage: Option<&TrustedStage>,
        diagnostic: &mut Option<HostSetupDiagnostic>,
    ) -> Result<Report, Failure> {
        match request.operation {
            Operation::Share(_) => Err(Failure::InvalidDraft),
            Operation::ObserveNative => {
                let Scope::Present(expected) = &request.expected else {
                    return Err(Failure::ScopeChanged);
                };
                let observation = Services::current_user(store, &request.root)
                    .and_then(|services| services.status())
                    .map_err(|error| {
                        if error.to_string() == "host.service_not_installed" {
                            Failure::NoNativeObservation
                        } else {
                            failure(error)
                        }
                    })?;
                let mut result = report(Some(store), &request.root)?;
                if result.scope != request.expected
                    || observation.installation != expected.installation
                    || observation.epoch != expected.epoch
                {
                    return Err(Failure::ScopeChanged);
                }
                result.process = Some(format!("{:?}", observation.process));
                Ok(result)
            }
            Operation::InspectOwner => {
                let owner = store
                    .owner_enrollment()
                    .and_then(|owners| owners.snapshot())
                    .map_err(failure)?;
                let mut result = report(Some(store), &request.root)?;
                if result.scope != request.expected {
                    return Err(Failure::ScopeChanged);
                }
                public_owner(&mut result, owner)?;
                Ok(result)
            }
            Operation::Reconcile => {
                let runtime = runtime()?;
                run_async(
                    &runtime,
                    cancel,
                    diagnostic,
                    CloudHostAdapter::new(store).reconcile(cancel),
                )?;
                report(Some(store), &request.root)
            }
            Operation::Enable => {
                let account = request
                    .draft
                    .account
                    .parse::<u64>()
                    .ok()
                    .filter(|id| *id != 0)
                    .ok_or(Failure::SignIn)?;
                let stage = stage.ok_or(Failure::SignedBundle)?;
                if cancel.is_cancelled() {
                    return Err(Failure::Cancelled);
                }
                let Scope::Present(expected) = &request.expected else {
                    return Err(Failure::ScopeChanged);
                };
                let consent = controller::SetupConsent {
                    expected: expected.as_ref().clone(),
                    settings: request.draft.settings(),
                    account,
                };
                let runtime = runtime()?;
                let setup = run_async(
                    &runtime,
                    cancel,
                    diagnostic,
                    controller::setup_if_current(
                        store,
                        &request.root,
                        stage,
                        &consent,
                        cancel,
                        |prompt| {
                            if prompt.user_code.len() <= 32
                                && prompt
                                    .user_code
                                    .bytes()
                                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                            {
                                let _ = progress.try_send(Progress::DeviceCode(DeviceCode(
                                    zeroize::Zeroizing::new(prompt.user_code.clone()),
                                )));
                            }
                        },
                        |ticket| {
                            if let Ok(scope) = store.snapshot() {
                                if scope.epoch == ticket.epoch()
                                    && scope.pending_ticket() == Some(ticket)
                                {
                                    if let Ok(mut owned) = request.owned.lock() {
                                        *owned = Some(scope);
                                    }
                                }
                            }
                        },
                    ),
                )?;
                if cancel.is_cancelled() {
                    return Err(Failure::Cancelled);
                }
                let mut result =
                    match completion_report(&request.root, setup.native_registration.epoch, || {
                        report(Some(store), &request.root)
                    }) {
                        Ok(report) => report,
                        Err(error @ Failure::Observation { .. }) => {
                            return Ok(Report {
                                setup_complete: true,
                                failure: Some(error),
                                ..Report::default()
                            });
                        }
                        Err(error) => return Err(error),
                    };
                let Scope::Present(snapshot) = result.scope.clone() else {
                    return Err(Failure::ScopeChanged);
                };
                if setup.native_registration.installation != snapshot.installation
                    || setup.native_registration.epoch != snapshot.epoch
                {
                    return Err(Failure::ScopeChanged);
                }
                public_owner(&mut result, setup.owner_approval_prerequisite)?;
                result.setup_complete = true;
                let completion = (|| {
                    let process =
                        if snapshot.settings.ordinary_access == OrdinaryAccess::ProviderOwner {
                            if cancel.is_cancelled() {
                                return Err(Failure::Cancelled);
                            }
                            // Normal Enable is consent to listen now. The independent
                            // sign-in startup preference controls future OS logins.
                            let services = completion_io(
                                &request.root,
                                snapshot.epoch,
                                CompletionBoundary::ServiceContext,
                                ObservationBoundary::ServiceContext,
                                || Services::current_user(store, &request.root),
                            )?;
                            let stage = completion_io(
                                &request.root,
                                snapshot.epoch,
                                CompletionBoundary::ServiceStage,
                                ObservationBoundary::ServiceStage,
                                || services.reopen_stage(snapshot.epoch),
                            )?;
                            #[cfg(target_os = "macos")]
                            let observation = {
                                let observation = completion_io(
                                    &request.root,
                                    snapshot.epoch,
                                    CompletionBoundary::ServiceStart,
                                    ObservationBoundary::ServiceStart,
                                    || {
                                        festerm_host_setup::desktop_start::start(
                                            store,
                                            &request.root,
                                            &stage,
                                            snapshot.as_ref(),
                                            cancel,
                                        )
                                    },
                                )?;
                                if completion_io(
                                    &request.root,
                                    snapshot.epoch,
                                    CompletionBoundary::FinalScope,
                                    ObservationBoundary::FinalScope,
                                    || store.snapshot(),
                                )? != *snapshot
                                {
                                    return Err(Failure::ScopeChanged);
                                }
                                observation
                            };
                            #[cfg(not(target_os = "macos"))]
                            let observation = {
                                completion_event(
                                    &request.root,
                                    snapshot.epoch,
                                    CompletionBoundary::ServiceStart,
                                    CompletionOutcome::Begin,
                                    None,
                                );
                                match services.start_with_completion(
                                    &stage,
                                    snapshot.epoch,
                                    |observation, until| {
                                        completion_event(
                                            &request.root,
                                            snapshot.epoch,
                                            CompletionBoundary::ServiceStart,
                                            CompletionOutcome::Ok,
                                            None,
                                        );
                                        final_enable_scope(
                                            &request.root,
                                            snapshot.as_ref(),
                                            cancel,
                                            until,
                                            || store.snapshot(),
                                        )
                                        .map(|()| observation)
                                    },
                                ) {
                                    Ok(result) => result?,
                                    Err(error) => completion_io_result(
                                        &request.root,
                                        snapshot.epoch,
                                        CompletionBoundary::ServiceStart,
                                        ObservationBoundary::ServiceStart,
                                        Err(error),
                                    )?,
                                }
                            };
                            observation.process
                        } else {
                            setup.native_registration.process
                        };
                    result.process = Some(format!("{process:?}"));
                    Ok(())
                })();
                match completion {
                    Err(error) => retain_observation_failure(result, error),
                    Ok(()) => match result.failure {
                        Some(error @ Failure::Observation { .. }) => {
                            retain_observation_failure(result, error)
                        }
                        _ => Ok(result),
                    },
                }
            }
            Operation::ApproveOwner => {
                let Scope::Present(expected) = &request.expected else {
                    return Err(Failure::ScopeChanged);
                };
                let key = request.draft.owner_public_key.trim();
                if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(Failure::OwnerApproval);
                }
                let mut public_key = [0; 32];
                for (index, byte) in public_key.iter_mut().enumerate() {
                    *byte = u8::from_str_radix(&key[index * 2..index * 2 + 2], 16)
                        .map_err(|_| Failure::OwnerApproval)?;
                }
                let mut consent = OwnerConsent {
                    owner_public_key: public_key,
                    confirmed_owner_fingerprint: request.draft.owner_fingerprint.trim().into(),
                    permissions: Default::default(),
                };
                consent.permissions.inventory = true;
                consent.permissions.attach = true;
                consent.permissions.detach = true;
                consent.permissions.profiles = true;
                consent.permissions.ensure_profile = true;
                consent.permissions.ensure = request.draft.approve_creation;
                consent.permissions.create = request.draft.approve_creation;
                consent.permissions.creation_allowed = request.draft.approve_creation;
                let revision = request.owner_revision.ok_or(Failure::OwnerApproval)?;
                let owner =
                    controller::approve_owner_if_current(store, expected, revision, consent)
                        .map_err(failure)?;
                let mut result = report(Some(store), &request.root)?;
                if result.scope != request.expected {
                    return Err(Failure::ScopeChanged);
                }
                public_owner(&mut result, owner)?;
                Ok(result)
            }
            Operation::Start => {
                let Scope::Present(expected) = &request.expected else {
                    return Err(Failure::ScopeChanged);
                };
                let owners = store.owner_enrollment().map_err(failure)?;
                let owner = owners.snapshot().map_err(failure)?;
                if owner.storage != OwnerStorage::NativeVault
                    || expected.settings.ordinary_access == OrdinaryAccess::OwnerOnly
                        && (!owner.remote_access || !owner.keys.iter().any(|key| !key.revoked))
                {
                    return Err(Failure::OwnerApproval);
                }
                verify(Some(store), &request.expected)?;
                let services = Services::current_user(store, &request.root).map_err(failure)?;
                let stage = services.reopen_stage(expected.epoch).map_err(failure)?;
                let observation = {
                    #[cfg(target_os = "macos")]
                    {
                        festerm_host_setup::desktop_start::start(
                            store,
                            &request.root,
                            &stage,
                            expected.as_ref(),
                            cancel,
                        )
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        services.start(&stage, expected.epoch)
                    }
                }
                .map_err(failure)?;
                let mut result = report(Some(store), &request.root)?;
                if result.scope != request.expected {
                    return Err(Failure::ScopeChanged);
                }
                result.process = Some(format!("{:?}", observation.process));
                public_owner(&mut result, owner)?;
                Ok(result)
            }
            Operation::Update => {
                update_scope(request, cancel)?;
                let Scope::Present(expected) = &request.expected else {
                    return Err(Failure::ScopeChanged);
                };
                let stage = stage.ok_or(Failure::SignedBundle)?;
                let release = request.release.as_ref().ok_or(Failure::UpdateBlocked)?;
                let runtime = runtime()?;
                run_async(
                    &runtime,
                    cancel,
                    diagnostic,
                    controller::update_if_current(
                        store,
                        &request.root,
                        expected,
                        stage,
                        release.from_version(),
                        cancel,
                    ),
                )
                .map_err(update_failure)?;
                report(Some(store), &request.root)
            }
            Operation::Disable => {
                let Scope::Present(expected) = &request.expected else {
                    return Err(Failure::ScopeChanged);
                };
                disable_completion_result(
                    &request.root,
                    controller::disable_if_current(store, &request.root, expected),
                )
                .map_err(failure)?;
                report(Some(store), &request.root)
            }
            Operation::Uninstall => {
                let Scope::Present(expected) = &request.expected else {
                    return Err(Failure::ScopeChanged);
                };
                let runtime = runtime()?;
                let outcome = run_async(
                    &runtime,
                    cancel,
                    diagnostic,
                    controller::uninstall_if_current(store, &request.root, expected, cancel),
                )?;
                let mut result = report(Some(store), &request.root)?;
                if let UninstallResult::NativeRemovedCloudCleanupPending { context } = outcome {
                    if !matches!(&result.scope, Scope::Present(snapshot) if
                        snapshot.installation == context.installation && snapshot.epoch == context.epoch)
                    {
                        return Err(Failure::ScopeChanged);
                    }
                    result.cleanup = Some(context);
                    result.cleanup_verified_for_manual_review = true;
                }
                Ok(result)
            }
            Operation::Refresh => unreachable!(),
        }
    }
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
