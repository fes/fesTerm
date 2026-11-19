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
    if detail.contains("vault")
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
        Err(error) => Err(failure(error)),
        Ok(_) => Store::open(root, Origin::Desktop)
            .map(Some)
            .map_err(failure),
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
    let Some(store) = store else {
        return Ok(Report {
            scope: Scope::Absent,
            ..Report::default()
        });
    };
    let snapshot = store.snapshot().map_err(failure)?;
    let cleanup = if snapshot.phase == Phase::CleanupPending {
        CloudHostAdapter::new(store)
            .pending_cleanup_context(&snapshot)
            .ok()
    } else {
        None
    };
    let last_run = if snapshot.phase == Phase::Provisioned {
        current_run_state(&snapshot, controller::last_run(store).map_err(failure)?)
    } else {
        None
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
        scope: Scope::Present(snapshot.into()),
        cleanup,
        ..Report::default()
    })
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
    runtime.block_on(async {
        match tokio::time::timeout(std::time::Duration::from_secs(300), future).await {
            Ok(result) => result.map_err(|error| {
                *diagnostic = festerm_host_setup::diagnostics::from_io_error(&error);
                failure(error)
            }),
            Err(_) => {
                cancel.cancel();
                Err(Failure::Cancelled)
            }
        }
    })
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
            // A blocked native call may outlive cancellation. Fence once it
            // really returns as well as through the independent revocation slot.
            let failure = match cancellation_scope(&store, &request) {
                Ok(expected) => {
                    match controller::disable_if_current(&store, &request.root, &expected) {
                        Ok(_) => Failure::Cancelled,
                        Err(error) => failure(error),
                    }
                }
                Err(error) => error,
            };
            let mut observed = report(Some(&store), &request.root)?;
            observed.failure = Some(failure);
            return Ok(observed);
        }
        match result {
            Ok(report) => Ok(report),
            Err(error) => {
                // Reobserve local protected partial progress without another
                // credential/network/native operation or a success assertion.
                let mut observed = report(Some(&store), &request.root)?;
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
                let mut result = report(Some(store), &request.root)?;
                let Scope::Present(snapshot) = &result.scope else {
                    return Err(Failure::ScopeChanged);
                };
                if setup.native_registration.installation != snapshot.installation
                    || setup.native_registration.epoch != snapshot.epoch
                {
                    return Err(Failure::ScopeChanged);
                }
                let process = if snapshot.settings.ordinary_access == OrdinaryAccess::ProviderOwner
                {
                    if cancel.is_cancelled() {
                        return Err(Failure::Cancelled);
                    }
                    // Normal Enable is consent to listen now. The independent
                    // sign-in startup preference controls future OS logins.
                    let services = Services::current_user(store, &request.root).map_err(failure)?;
                    let stage = services.reopen_stage(snapshot.epoch).map_err(failure)?;
                    let observation = services.start(&stage, snapshot.epoch).map_err(failure)?;
                    if store.snapshot().map_err(failure)? != **snapshot {
                        return Err(Failure::ScopeChanged);
                    }
                    observation.process
                } else {
                    setup.native_registration.process
                };
                result.process = Some(format!("{process:?}"));
                public_owner(&mut result, setup.owner_approval_prerequisite)?;
                Ok(result)
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
                let observation = services.start(&stage, expected.epoch).map_err(failure)?;
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
                .map_err(|error| match error {
                    Failure::Cancelled | Failure::ScopeChanged => error,
                    _ => Failure::UpdateIncomplete,
                })?;
                report(Some(store), &request.root)
            }
            Operation::Disable => {
                let Scope::Present(expected) = &request.expected else {
                    return Err(Failure::ScopeChanged);
                };
                controller::disable_if_current(store, &request.root, expected).map_err(failure)?;
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
