//! Foreground desktop consent, never a recipe loader or a daemon authority.
#[path = "desktop_remote_access/native.rs"]
mod native;
#[cfg(test)]
#[path = "desktop_remote_access/tests.rs"]
mod tests;
#[path = "desktop_remote_access/view.rs"]
mod view;
pub(crate) use view::show;
pub(crate) use view::show_session_sharing;

use eframe::egui;
use festerm_host_setup::{
    cloud::CleanupContext,
    lifecycle::{Phase, Settings, Snapshot, Startup},
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, AtomicU8, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
        Arc, Mutex, OnceLock,
    },
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Draft {
    pub friendly_name: String,
    pub allow_creation: bool,
    pub startup_at_sign_in: bool,
    pub account: String,
    pub manual_bundle: bool,
    pub manifest: String,
    pub signature: String,
    pub archive: String,
    pub owner_public_key: String,
    pub owner_fingerprint: String,
    pub approve_creation: bool,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            friendly_name: "This computer".into(),
            allow_creation: false,
            startup_at_sign_in: false,
            account: String::new(),
            manual_bundle: false,
            manifest: String::new(),
            signature: String::new(),
            archive: String::new(),
            owner_public_key: String::new(),
            owner_fingerprint: String::new(),
            approve_creation: false,
        }
    }
}

impl Draft {
    fn settings(&self) -> Settings {
        Settings {
            friendly_name: self.friendly_name.clone(),
            allow_creation: self.allow_creation,
            startup: if self.startup_at_sign_in {
                Startup::UserSignIn
            } else {
                Startup::Manual
            },
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Scope {
    #[default]
    Unobserved,
    Absent,
    Present(Arc<Snapshot>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShareTarget {
    pub tab: crate::tabs::TabId,
    pub profile: Option<String>,
    pub lifecycle_generation: u64,
    pub identity: festerm_sessiond::NativeSessionIdentity,
    pub source: Option<festerm_sessiond::LocalSessionTarget>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShareStep {
    Bind,
    Switch,
    Resume,
    Release,
    Rollback,
}

impl ShareStep {
    fn grants(self) -> bool {
        matches!(self, Self::Bind | Self::Switch)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharePhase {
    Prepared,
    Active,
    Revoked,
    Released,
    Uncertain,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShareStatus {
    pub target: ShareTarget,
    pub operation: String,
    pub phase: SharePhase,
}

#[derive(Clone)]
struct ShareTask {
    target: ShareTarget,
    operation: String,
    current: Arc<Mutex<Vec<ShareTarget>>>,
    epoch: Option<(Arc<AtomicU64>, u64)>,
}

impl ShareTask {
    fn is_current(&self, cancel: &CancellationToken) -> bool {
        !cancel.is_cancelled()
            && self
                .epoch
                .as_ref()
                .is_some_and(|(epoch, expected)| epoch.load(Ordering::Acquire) == *expected)
            && self
                .current
                .lock()
                .is_ok_and(|targets| targets.contains(&self.target))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Refresh,
    ObserveNative,
    InspectOwner,
    Reconcile,
    Enable,
    ApproveOwner,
    Start,
    Disable,
    Uninstall,
    Share(ShareStep),
}

impl Operation {
    fn needs_cancel_fence(self) -> bool {
        matches!(
            self,
            Self::Enable | Self::ApproveOwner | Self::Start | Self::Disable | Self::Uninstall
        )
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Edit(Draft),
    SignInAccount,
    RequestAccess(bool),
    Refresh,
    Review(Operation),
    Confirm,
    Dismiss,
    Cancel,
    ReviewShare(ShareTarget),
    Share(ShareTarget),
    Unshare(ShareTarget),
    SwitchShare,
    ResumeShare,
    ReleaseShare,
    RollbackShare,
    RecoverShare(String),
    RecoverReviewedShare(ShareStep),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Busy,
    Cancelled,
    RefreshFirst,
    ScopeChanged,
    PrivateStorage,
    SignedBundle,
    BundleNotPublished,
    AcquisitionUnavailable,
    NativeVault,
    SignIn,
    OwnerApproval,
    PlatformPrerequisite,
    Reconciliation,
    ShareCapability,
    SessionChanged,
    NoNativeObservation,
    InvalidDraft,
    CloudUncertain,
}

impl Failure {
    pub fn notice(self) -> &'static str {
        match self {
            Self::SignIn => {
                "Host account verification is required. Review access settings to sign in."
            }
            Self::NativeVault => "Unlock the credential store, then retry.",
            Self::RefreshFirst | Self::ScopeChanged => "Refresh local status, then review again.",
            Self::OwnerApproval => "Review owner approval in Advanced / Recovery.",
            Self::Busy | Self::Cancelled => {
                "Revocation may still be draining. Wait before retrying."
            }
            Self::ShareCapability => "This running shell does not support HOST sharing.",
            Self::SessionChanged => "The running session changed. Review it again.",
            Self::BundleNotPublished => "The signed host bundle for this build is not published.",
            Self::AcquisitionUnavailable => "Host component download failed. Retry explicitly.",
            Self::SignedBundle => "Host component verification failed. Nothing was installed.",
            Self::CloudUncertain | Self::Reconciliation => {
                "Cleanup is incomplete. Keep access off and review Recovery."
            }
            Self::PrivateStorage => "Protected host storage is unavailable. Review Recovery.",
            Self::PlatformPrerequisite => {
                "Host setup is not qualified for this startup policy or platform."
            }
            Self::NoNativeObservation => "No native host registration was observed.",
            Self::InvalidDraft => "Check the host settings before reviewing.",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Busy => "An operation is still draining. Retry after it actually finishes.",
            Self::Cancelled => "Cancelled. Mutating work must finish revocation before retry.",
            Self::RefreshFirst => "Refresh this installation before reviewing an action.",
            Self::ScopeChanged => "Installation or consent changed. Refresh and review again.",
            Self::PrivateStorage => "Protected desktop state is unavailable or foreign. Do not adopt a headless installation or weaken its permissions.",
            Self::SignedBundle => "This build's authenticated native host bundle was refused. Changed bytes, invalid signatures and unsupported publishers are not installed. No older or unsigned fallback is attempted.",
            Self::BundleNotPublished => "The exact current-version native host bundle is not published. Genuine schema-2 release assets remain unqualified. Nothing is installed; retry explicitly after publication. No older/latest/schema-1 fallback.",
            Self::AcquisitionUnavailable => "Authenticated bundle acquisition is unavailable or exceeded its bounded deadline. Nothing is installed; retry explicitly. No automatic manual-path, older-version or unsigned fallback.",
            Self::NativeVault => "Unlock the native credential store and retry. There is no file-credential fallback.",
            Self::SignIn => "Sign in to the selected GitHub account and retry. Your settings and sharing choice are retained.",
            Self::OwnerApproval => "Review the public owner key and its independently confirmed fingerprint. Access and creation permissions require separate approval.",
            Self::PlatformPrerequisite => "The signed native helper or selected startup policy is not qualified on this platform. Do not bypass admission or enable a different OS service.",
            Self::Reconciliation => "Cleanup or revocation is incomplete. Keep access fenced; refresh and retry the exact installation. Do not delete resources from an uncertain receipt.",
            Self::ShareCapability => "This shell has no authenticated connected-local source capability. Older or remote helpers remain unsupported; metadata, a PID or a guessed runtime directory cannot grant sharing.",
            Self::SessionChanged => "The selected profile, native session or generation changed. Nothing was shared, stopped or restarted.",
            Self::NoNativeObservation => "No native registration receipt was published. Status has no manager observation; this is not fabricated native absence.",
            Self::InvalidDraft => "A field exceeds its bounded input limit. Shorten it before review.",
            Self::CloudUncertain => "Cloud access is denied, offline or uncertain. Retain the protected receipt and access fence; retry explicitly with the original account. There is no verified removal target.",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct View {
    /// Public identity only; these accounts never supply host credentials.
    pub accounts: Vec<HostAccount>,
    pub access_requested: Option<bool>,
    pub draft: Draft,
    pub scope: Scope,
    pub review: Option<Operation>,
    pub busy: bool,
    pub cancelling: bool,
    pub failure: Option<Failure>,
    pub process: Option<String>,
    pub owner_binding: Option<String>,
    pub owner_approved: bool,
    pub owner_revision: Option<u64>,
    pub cleanup: Option<CleanupContext>,
    pub cleanup_verified_for_manual_review: bool,
    pub device_code: Option<DeviceCode>,
    pub share_review: Option<ShareTarget>,
    pub sharing: Option<ShareStatus>,
    pub share_operation_input: String,
    pub bundle_status: BundleStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostAccount {
    pub account_id: u64,
    pub login: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BundleStatus {
    #[default]
    Idle,
    Acquiring,
    Authenticated,
    Qualifying,
    Complete,
    Blocked,
    Cancelled,
}

impl BundleStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "Not requested. Only explicit Enable confirmation acquires a bundle.",
            Self::Acquiring => {
                "Acquiring / verifying exact current host bundle on the bounded worker…"
            }
            Self::Authenticated => "Owned bundle bytes authenticated; rechecking current consent.",
            Self::Qualifying => "Qualifying the native publisher and protected installation.",
            Self::Complete => {
                "Authenticated bundle setup returned. This is NOT endpoint readiness."
            }
            Self::Blocked => {
                "Bundle setup blocked. Review the actionable error; nothing selects a fallback."
            }
            Self::Cancelled => {
                "Acceptance cancelled. Synchronous verification may still be draining."
            }
        }
    }
}

impl View {
    pub fn access_enabled(&self) -> bool {
        matches!(&self.scope, Scope::Present(snapshot) if matches!(
            snapshot.phase, Phase::EnableRequested | Phase::Provisioned | Phase::Reconciling
        ))
    }

    fn bound_account(&self) -> Option<&str> {
        match &self.scope {
            Scope::Present(snapshot) if snapshot.phase != Phase::Removed => snapshot
                .binding
                .as_ref()
                .and_then(|binding| binding.account.strip_prefix("github:")),
            _ => None,
        }
    }

    pub fn observation_label(&self) -> &'static str {
        match &self.scope {
            Scope::Unobserved => "Not observed. No readiness assertion.",
            Scope::Absent => "Last observation: no desktop installation.",
            Scope::Present(snapshot) => match snapshot.phase {
                Phase::Disabled => {
                    "Last recorded Disabled. Persistent shells are not stop targets."
                }
                Phase::CleanupPending => "Last recorded CleanupPending; keep access fenced.",
                Phase::Removed => "Recorded Removed; not a fresh provider observation.",
                _ => "Last local observation only. Endpoint readiness is NOT verified.",
            },
        }
    }
}

struct Request {
    operation: Operation,
    expected: Scope,
    draft: Draft,
    root: PathBuf,
    cancel_fence: bool,
    owned: Arc<Mutex<Option<Snapshot>>>,
    owner_revision: Option<u64>,
    sharing: Option<ShareTask>,
}

#[derive(Default)]
struct Report {
    scope: Scope,
    process: Option<String>,
    owner_binding: Option<String>,
    owner_approved: bool,
    owner_revision: Option<u64>,
    cleanup: Option<CleanupContext>,
    cleanup_verified_for_manual_review: bool,
    failure: Option<Failure>,
    sharing: Option<ShareStatus>,
}

#[derive(Clone)]
pub struct DeviceCode(zeroize::Zeroizing<String>);

impl std::fmt::Debug for DeviceCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DeviceCode(<private>)")
    }
}

enum Progress {
    DeviceCode(DeviceCode),
    Bundle(BundleStatus),
}

trait Backend: Send + Sync {
    fn execute(
        &self,
        request: Request,
        cancel: &CancellationToken,
        progress: SyncSender<Progress>,
    ) -> Result<Report, Failure>;
}

struct Permit {
    slots: Arc<AtomicU8>,
    bit: u8,
}

impl Permit {
    fn take(slots: &Arc<AtomicU8>, bit: u8) -> Result<Self, Failure> {
        if slots.fetch_or(bit, Ordering::AcqRel) & bit != 0 {
            return Err(Failure::Busy);
        }
        Ok(Self {
            slots: slots.clone(),
            bit,
        })
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.slots.fetch_and(!self.bit, Ordering::Release);
    }
}

struct Pending {
    generation: u64,
    operation: Operation,
    cancel: CancellationToken,
    result: Receiver<Result<Report, Failure>>,
    progress: Receiver<Progress>,
    sharing: Option<ShareTask>,
}

pub(crate) struct Controller {
    view: View,
    root: Option<PathBuf>,
    backend: Arc<dyn Backend>,
    slots: Arc<AtomicU8>,
    generation: u64,
    pending: Option<Pending>,
    fence: Option<Pending>,
    owned: Arc<Mutex<Option<Snapshot>>>,
    draft_edited: bool,
    share_targets: Arc<Mutex<Vec<ShareTarget>>>,
    share_request: Option<ShareTask>,
    share_epoch: Option<Arc<AtomicU64>>,
    share_review_epoch: Option<u64>,
}

impl Controller {
    pub fn native() -> Self {
        static SLOTS: OnceLock<Arc<AtomicU8>> = OnceLock::new();
        let root = directories::ProjectDirs::from("com", "fes", "fesTerm")
            .map(|dirs| dirs.data_local_dir().join("remote-access-desktop-v1"));
        Self::new(
            root,
            Arc::new(native::Native),
            SLOTS.get_or_init(|| Arc::new(AtomicU8::new(0))).clone(),
        )
    }

    fn new(root: Option<PathBuf>, backend: Arc<dyn Backend>, slots: Arc<AtomicU8>) -> Self {
        Self {
            view: View::default(),
            root,
            backend,
            slots,
            generation: 0,
            pending: None,
            fence: None,
            owned: Arc::new(Mutex::new(None)),
            draft_edited: false,
            share_targets: Arc::new(Mutex::new(Vec::new())),
            share_request: None,
            share_epoch: None,
            share_review_epoch: None,
        }
    }

    pub fn view(&self) -> View {
        self.view.clone()
    }

    pub fn observe_accounts(&mut self, accounts: Vec<HostAccount>) {
        self.view.accounts = accounts;
    }

    pub fn reject_changed_session(&mut self) {
        self.view.failure = Some(Failure::SessionChanged);
        self.view.share_review = None;
    }

    pub fn observe_share_targets(&mut self, targets: Vec<ShareTarget>) {
        let changed = self.view.sharing.as_ref().is_some_and(|sharing| {
            !targets.contains(&sharing.target)
                && self.pending.as_ref().is_some_and(
                    |pending| matches!(pending.operation, Operation::Share(step) if step.grants()),
                )
        });
        *self
            .share_targets
            .lock()
            .expect("healthy sharing selection") = targets;
        if changed {
            if let Some(pending) = &self.pending {
                pending.cancel.cancel();
            }
        }
    }

    pub fn observe_share_epoch(&mut self, epoch: Arc<AtomicU64>) {
        if self.view.share_review.is_some()
            && self.share_review_epoch != Some(epoch.load(Ordering::Acquire))
        {
            self.view.share_review = None;
            self.share_review_epoch = None;
        }
        self.share_epoch = Some(epoch);
    }

    pub fn action(&mut self, action: Action, context: &egui::Context) {
        if matches!(action, Action::Cancel) {
            self.cancel(context);
            return;
        }
        if self.pending.is_some() || self.fence.is_some() {
            self.view.failure = Some(Failure::Busy);
            return;
        }
        self.view.failure = None;
        match action {
            Action::SignInAccount => self.view.failure = Some(Failure::SignIn),
            Action::RequestAccess(requested) => {
                self.view.access_requested = Some(requested);
                self.view.share_review = None;
                self.view.review = None;
                if matches!(self.view.scope, Scope::Unobserved) {
                    self.view.failure = Some(Failure::RefreshFirst);
                } else {
                    self.view.review = (!requested).then_some(Operation::Disable);
                }
            }
            Action::Edit(draft) => {
                if self
                    .view
                    .bound_account()
                    .is_some_and(|account| account != draft.account)
                {
                    self.view.failure = Some(Failure::ScopeChanged);
                    return;
                }
                if [
                    &draft.friendly_name,
                    &draft.account,
                    &draft.manifest,
                    &draft.signature,
                    &draft.archive,
                    &draft.owner_public_key,
                    &draft.owner_fingerprint,
                ]
                .iter()
                .any(|value| value.len() > 4096)
                {
                    self.view.failure = Some(Failure::InvalidDraft);
                    return;
                }
                self.view.draft = draft;
                self.draft_edited = true;
                self.view.review = None;
                self.view.share_review = None;
            }
            Action::Dismiss => {
                if matches!(
                    self.view.review,
                    Some(Operation::Enable | Operation::Disable)
                ) {
                    self.view.access_requested = None;
                }
                self.view.review = None;
                self.view.share_review = None;
            }
            Action::Refresh => self.start(Operation::Refresh, false, context),
            Action::Review(operation) => {
                if matches!(operation, Operation::Share(_)) {
                    self.view.failure = Some(Failure::InvalidDraft);
                } else if matches!(self.view.scope, Scope::Unobserved) {
                    self.view.failure = Some(Failure::RefreshFirst);
                } else {
                    self.view.review = Some(operation);
                }
            }
            Action::Confirm => {
                if let Some(operation) = self.view.review.take() {
                    self.start(operation, false, context);
                }
            }
            Action::ReviewShare(target) => {
                self.view.review = None;
                self.view.share_review = Some(target);
                self.share_review_epoch = self
                    .share_epoch
                    .as_ref()
                    .map(|epoch| epoch.load(Ordering::Acquire));
            }
            Action::Share(target) => self.start_share(ShareStep::Bind, target, None, context),
            Action::Unshare(target) => self.start_share(ShareStep::Release, target, None, context),
            Action::SwitchShare => self.start_retained_share(ShareStep::Switch, context),
            Action::ResumeShare => self.start_retained_share(ShareStep::Resume, context),
            Action::ReleaseShare => self.start_retained_share(ShareStep::Release, context),
            Action::RollbackShare => self.start_retained_share(ShareStep::Rollback, context),
            Action::RecoverReviewedShare(step) => self.recover_reviewed_share(step, context),
            Action::RecoverShare(operation) => {
                if operation.len() > 128 || operation.chars().any(char::is_control) {
                    self.view.failure = Some(Failure::InvalidDraft);
                } else {
                    self.view.share_operation_input = operation;
                }
            }
            Action::Cancel => unreachable!(),
        }
    }

    fn start_retained_share(&mut self, step: ShareStep, context: &egui::Context) {
        let Some(sharing) = self.view.sharing.clone() else {
            self.view.failure = Some(Failure::SessionChanged);
            return;
        };
        self.start_share(step, sharing.target, Some(sharing.operation), context);
    }

    fn recover_reviewed_share(&mut self, step: ShareStep, context: &egui::Context) {
        if step.grants() {
            self.view.failure = Some(Failure::InvalidDraft);
            return;
        }
        let Some(target) = self.view.share_review.clone() else {
            self.view.failure = Some(Failure::SessionChanged);
            return;
        };
        if self
            .share_epoch
            .as_ref()
            .is_none_or(|epoch| self.share_review_epoch != Some(epoch.load(Ordering::Acquire)))
            || !self
                .share_targets
                .lock()
                .expect("healthy sharing selection")
                .contains(&target)
        {
            self.view.failure = Some(Failure::SessionChanged);
            return;
        }
        let original = self.view.share_operation_input.clone();
        if original.is_empty() {
            self.view.failure = Some(Failure::Reconciliation);
            return;
        }
        self.start_share(step, target, Some(original), context);
    }

    fn start_share(
        &mut self,
        step: ShareStep,
        target: ShareTarget,
        original: Option<String>,
        context: &egui::Context,
    ) {
        if step.grants() {
            if target.source.is_none() {
                self.view.failure = Some(Failure::ShareCapability);
                return;
            }
            if self.view.share_review.as_ref() != Some(&target) {
                self.view.failure = Some(Failure::SessionChanged);
                return;
            }
            if self
                .share_epoch
                .as_ref()
                .is_none_or(|epoch| self.share_review_epoch != Some(epoch.load(Ordering::Acquire)))
            {
                self.view.failure = Some(Failure::SessionChanged);
                return;
            }
            if self.view.owner_revision.is_none()
                || !matches!(&self.view.scope, Scope::Present(snapshot) if snapshot.phase == Phase::Provisioned)
            {
                self.view.failure = Some(Failure::OwnerApproval);
                return;
            }
        }
        let retained = self.view.sharing.as_ref().filter(|sharing| {
            sharing.target == target
                && (step != ShareStep::Bind || sharing.phase != SharePhase::Released)
        });
        let operation = if let Some(original) = original {
            original
        } else if let Some(sharing) = retained {
            sharing.operation.clone()
        } else if step == ShareStep::Bind && self.view.share_operation_input.is_empty() {
            if self
                .view
                .sharing
                .as_ref()
                .is_some_and(|sharing| sharing.phase != SharePhase::Released)
            {
                self.view.failure = Some(Failure::Reconciliation);
                return;
            }
            uuid::Uuid::new_v4().to_string()
        } else {
            if self.view.share_operation_input.is_empty() {
                self.view.failure = Some(Failure::Reconciliation);
                return;
            }
            self.view.share_operation_input.clone()
        };
        self.view.sharing = Some(ShareStatus {
            target: target.clone(),
            operation: operation.clone(),
            phase: SharePhase::Uncertain,
        });
        self.share_request = Some(ShareTask {
            target,
            operation,
            current: self.share_targets.clone(),
            epoch: self
                .share_epoch
                .as_ref()
                .zip(self.share_review_epoch)
                .map(|(epoch, expected)| (epoch.clone(), expected)),
        });
        self.start(Operation::Share(step), false, context);
    }

    fn start(&mut self, operation: Operation, fence: bool, context: &egui::Context) {
        self.view.review = None;
        if !matches!(operation, Operation::Share(_)) {
            self.view.share_review = None;
            self.share_review_epoch = None;
        }
        let Some(root) = self.root.clone() else {
            self.view.failure = Some(Failure::PrivateStorage);
            return;
        };
        let permit = match Permit::take(&self.slots, if fence { 2 } else { 1 }) {
            Ok(permit) => permit,
            Err(error) => {
                self.view.failure = Some(error);
                return;
            }
        };
        let request = Request {
            operation,
            expected: self.view.scope.clone(),
            draft: self.view.draft.clone(),
            root,
            cancel_fence: fence,
            owned: self.owned.clone(),
            owner_revision: self.view.owner_revision,
            sharing: self.share_request.take(),
        };
        self.view.cleanup_verified_for_manual_review = false;
        if operation == Operation::Enable {
            self.view.bundle_status = BundleStatus::Acquiring;
        }
        if !fence && !matches!(operation, Operation::Share(_)) {
            *self.owned.lock().expect("healthy owned scope") = None;
        }
        let (sender, result) = mpsc::sync_channel(1);
        // Two bundle transitions plus the single foreground device prompt;
        // status must never displace the code needed to finish explicit sign-in.
        let (progress_sender, progress) = mpsc::sync_channel(3);
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let backend = self.backend.clone();
        let repaint = context.clone();
        self.generation = self.generation.wrapping_add(1);
        let pending = Pending {
            generation: self.generation,
            operation,
            cancel,
            result,
            progress,
            sharing: request.sharing.clone(),
        };
        match std::thread::Builder::new()
            .name("desktop-remote-access".into())
            .spawn(move || {
                let _permit = permit;
                let report = backend.execute(request, &worker_cancel, progress_sender);
                let _ = sender.send(report);
                repaint.request_repaint();
            }) {
            Ok(_) => {
                if fence {
                    self.fence = Some(pending);
                } else {
                    self.pending = Some(pending);
                }
                self.view.busy = true;
            }
            Err(_) => self.view.failure = Some(Failure::Busy),
        }
    }

    fn cancel(&mut self, context: &egui::Context) {
        if self.view.cancelling {
            return;
        }
        let Some(pending) = &self.pending else {
            self.view.review = None;
            return;
        };
        pending.cancel.cancel();
        let needs_fence = pending.operation.needs_cancel_fence();
        let share_grant = matches!(pending.operation, Operation::Share(step) if step.grants());
        self.generation = self.generation.wrapping_add(1);
        self.view.cancelling = true;
        self.view.device_code = None;
        self.view.failure = Some(Failure::Cancelled);
        if pending.operation == Operation::Enable {
            self.view.bundle_status = BundleStatus::Cancelled;
        }
        if needs_fence && self.fence.is_none() {
            self.start(Operation::Disable, true, context);
        } else if share_grant && self.fence.is_none() {
            if let Some(sharing) = &mut self.view.sharing {
                sharing.phase = SharePhase::Uncertain;
                self.share_request = Some(ShareTask {
                    target: sharing.target.clone(),
                    operation: sharing.operation.clone(),
                    current: self.share_targets.clone(),
                    epoch: None,
                });
                self.start(Operation::Share(ShareStep::Release), true, context);
            }
        }
        // Admission remains held until the actual worker exits. A cancellation
        // token or a UI deadline is not evidence that native work stopped.
    }

    pub fn poll(&mut self, context: &egui::Context) {
        if !self.view.cancelling
            && self.pending.as_ref().is_some_and(|pending| {
                matches!(pending.operation, Operation::Share(step) if step.grants())
                    && pending
                        .sharing
                        .as_ref()
                        .is_none_or(|task| !task.is_current(&pending.cancel))
            })
        {
            self.cancel(context);
        }
        for fence in [false, true] {
            let slot = if fence {
                &mut self.fence
            } else {
                &mut self.pending
            };
            let Some(pending) = slot else { continue };
            if pending.generation == self.generation {
                while let Ok(progress) = pending.progress.try_recv() {
                    match progress {
                        Progress::DeviceCode(code) => self.view.device_code = Some(code),
                        Progress::Bundle(status) => self.view.bundle_status = status,
                    }
                }
            }
            let result = match pending.result.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Disconnected) => Some(Err(Failure::Reconciliation)),
                Err(TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                if pending.generation == self.generation {
                    self.view.device_code = None;
                    if pending.operation == Operation::Enable {
                        self.view.bundle_status = match &result {
                            Ok(report) if report.failure.is_none() => BundleStatus::Complete,
                            Ok(report) if report.failure == Some(Failure::Cancelled) => {
                                BundleStatus::Cancelled
                            }
                            Err(Failure::Cancelled) => BundleStatus::Cancelled,
                            _ => BundleStatus::Blocked,
                        };
                    }
                    match result {
                        Ok(report) => {
                            if pending.operation == Operation::Refresh && !self.draft_edited {
                                if let Scope::Present(snapshot) = &report.scope {
                                    self.view.draft.friendly_name =
                                        snapshot.settings.friendly_name.clone();
                                    self.view.draft.allow_creation =
                                        snapshot.settings.allow_creation;
                                    self.view.draft.startup_at_sign_in =
                                        snapshot.settings.startup == Startup::UserSignIn;
                                    if let Some(binding) = &snapshot.binding {
                                        if let Some(account) =
                                            binding.account.strip_prefix("github:")
                                        {
                                            self.view.draft.account = account.into();
                                        }
                                    }
                                }
                            }
                            self.view.scope = report.scope;
                            self.view.process = report.process;
                            self.view.owner_binding = report.owner_binding;
                            self.view.owner_approved = report.owner_approved;
                            self.view.owner_revision = report.owner_revision;
                            self.view.cleanup = report.cleanup;
                            self.view.cleanup_verified_for_manual_review =
                                report.cleanup_verified_for_manual_review;
                            self.view.failure = report.failure;
                            if let Some(sharing) = report.sharing {
                                self.view.sharing = Some(sharing);
                            }
                        }
                        Err(error) => self.view.failure = Some(error),
                    }
                }
                *slot = None;
            }
        }
        self.view.busy = self.pending.is_some() || self.fence.is_some();
        if self.view.busy {
            context.request_repaint_after(std::time::Duration::from_millis(100));
        } else {
            self.view.cancelling = false;
        }
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.cancel(&egui::Context::default());
    }
}

impl super::FesTermApp {
    pub(crate) fn dispatch_local_remote_access(&mut self, action: Action, context: &egui::Context) {
        let target = match &action {
            Action::ReviewShare(target) | Action::Share(target) | Action::Unshare(target) => {
                Some(target)
            }
            _ => None,
        };
        let targets = self.state.local_share_targets();
        if target.is_some_and(|target| !targets.contains(target)) {
            self.local_remote_access.reject_changed_session();
            return;
        }
        self.local_remote_access.observe_share_targets(targets);
        self.local_remote_access
            .observe_share_epoch(self.state.local_share_epoch());
        self.local_remote_access.action(action, context);
    }
}
