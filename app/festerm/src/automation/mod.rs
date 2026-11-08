mod controller;
mod ipc;
pub(crate) mod javascript;
mod observation;
pub(crate) mod preferences;
pub(crate) mod presentation;
#[cfg(test)]
mod tests;
#[allow(dead_code)]
pub(crate) mod verification;
mod workflow_selection;

#[cfg(test)]
pub(crate) use controller::tests::{
    reviewed_controller_for_application, stable_settings as stable_controller_settings_for_test,
};
#[cfg(test)]
pub(crate) use tests::{application_disclosure_witness, preference_application_fixture};

use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use eframe::egui;
use festerm_automation::{
    Broker, Budget, Completion, ConnectionId, ControllerEnrollmentId, ControllerEnrollmentProposal,
    ControllerRegistrationId, Digest, DirectKind, GrantRequest, Invocation, InvocationBinding,
    MetadataRunSpec, NativeApproval, NativeControllerEnrollment, Operation as AuthorityOperation,
    RequestId, RunId, RunSpec, Scope, TargetRef,
};
use festerm_automation_mcp::{
    BackendRequest, BackendResult, ControllerId, Event, InputDisposition, Machine, Operation,
    PairingStatus, Refusal, RequestKey, Response, VerificationCode,
};
use festerm_core::ScrollbackStats;
use sha2::{Digest as _, Sha256};

use crate::{
    app::FesTermApp,
    application::WindowId,
    tabs::{SessionTab, TabId},
};

#[cfg(target_os = "macos")]
type PrivacyMonitor = festerm_macos_window::PrivacyMonitor;
#[cfg(windows)]
type PrivacyMonitor = festerm_windows_power::SharedPrivacyMonitor;
#[cfg(target_os = "linux")]
type PrivacyMonitor = festerm_linux_power::PrivacyMonitor;

pub(crate) use controller::DirectInputPreparation;
pub(crate) use ipc::run_stdio;
pub(crate) use presentation::WorkspaceRevision;
pub(crate) use workflow_selection::{Status as WorkflowStatus, View as WorkflowView};

pub(crate) const MAX_NATIVE_COMMANDS: usize = 32;
const WORKFLOW_UNWIND_STOPPED: &str =
    "Workflow selection stopped after an internal error. Turn JavaScript workflows off and on to resume.";
const MCP_UNWIND_STOPPED: &str =
    "MCP stopped after an internal error. Restart MCP explicitly; no previous grants were restored.";

#[derive(Clone)]
pub(crate) struct SelectedWorkflow(pub(crate) std::path::PathBuf);

impl std::fmt::Debug for SelectedWorkflow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SelectedWorkflow(<private path>)")
    }
}

#[derive(Clone)]
pub(crate) enum NativeCommand {
    RestartMcp,
    SafetyStop,
    PreferenceChanged {
        clear_workflow: bool,
        reenable_workflow: bool,
    },
    ApproveObservation {
        channel: u64,
        targets: Vec<TargetRef>,
    },
    Revoke {
        channel: u64,
    },
    CompareDirectPeer {
        generation: u64,
        fingerprint: String,
    },
    DenyDirectPeer {
        generation: u64,
    },
    EnableControllerEnrollment,
    ChooseController {
        channel: u64,
        registration: ControllerRegistrationId,
    },
    SelectControllerTarget {
        channel: u64,
        registration: ControllerRegistrationId,
        selection: u64,
        target: TargetRef,
    },
    ConfirmController {
        channel: u64,
        registration: ControllerRegistrationId,
        selection: u64,
    },
    CancelController {
        channel: u64,
        registration: ControllerRegistrationId,
    },
    UnregisterController {
        channel: u64,
        registration: ControllerRegistrationId,
    },
    ReviewControllerMetadata {
        channel: u64,
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
    },
    ConfirmControllerMetadata {
        channel: u64,
        registration: ControllerRegistrationId,
        review: u64,
    },
    CancelControllerMetadata {
        channel: u64,
        registration: ControllerRegistrationId,
        review: u64,
    },
    RevokeControllerMetadata {
        channel: u64,
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
    },
    ReviewControllerInput {
        channel: u64,
        registration: ControllerRegistrationId,
        input: u64,
    },
    ConfirmControllerInput {
        channel: u64,
        registration: ControllerRegistrationId,
        input: u64,
    },
    CancelControllerInput {
        channel: u64,
        registration: ControllerRegistrationId,
        input: u64,
    },
    DisarmControllerInput {
        channel: u64,
        registration: ControllerRegistrationId,
        input: u64,
    },
    CaptureWorkflow(SelectedWorkflow),
    ClearWorkflow,
    StartWorkflowMonitoring,
    StartWorkflowRun {
        digest: String,
        target: Option<TargetRef>,
        parameters: festerm_automation_js::snapshot::Parameters,
    },
    CancelWorkflowRun {
        token: u64,
    },
    ApproveWorkflowInput {
        token: u64,
    },
    DenyWorkflowInput {
        token: u64,
    },
}

impl std::fmt::Debug for NativeCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeCommand(<redacted>)")
    }
}

impl NativeCommand {
    pub(crate) fn batch_requires_safety_stop<'a>(
        commands: impl Iterator<Item = &'a Self> + Clone,
    ) -> bool {
        let count = commands.clone().count();
        count > MAX_NATIVE_COMMANDS
            || commands.clone().any(|command| matches!(command, Self::SafetyStop))
            || (count > 1
                && commands
                    .clone()
                    .any(|command| matches!(command, Self::PreferenceChanged { .. })))
            // Equal paths do not prove the same selection or frozen snapshot.
            || commands
                .clone()
                .filter(|command| matches!(command, Self::CaptureWorkflow(_)))
                .take(2)
                .count()
                == 2
            || commands
                .clone()
                .filter(|command| command.is_verification())
                .take(2)
                .count()
                == 2
            // Collection order must never choose between native workflow approvals.
            || commands
                .clone()
                .filter(|command| {
                    matches!(
                        command,
                        Self::StartWorkflowRun { .. } | Self::ApproveWorkflowInput { .. }
                    )
                })
                .take(2)
                .count()
                == 2
            // Collection order must never choose between native enrollment actions.
            || commands.filter(|command| command.is_controller()).take(2).count() == 2
    }

    fn is_javascript(&self) -> bool {
        matches!(
            self,
            Self::StartWorkflowMonitoring
                | Self::StartWorkflowRun { .. }
                | Self::CancelWorkflowRun { .. }
                | Self::ApproveWorkflowInput { .. }
                | Self::DenyWorkflowInput { .. }
        )
    }

    fn is_confirmation(&self) -> bool {
        matches!(
            self,
            Self::ApproveObservation { .. }
                | Self::CompareDirectPeer { .. }
                | Self::EnableControllerEnrollment
                | Self::ChooseController { .. }
                | Self::SelectControllerTarget { .. }
                | Self::ConfirmController { .. }
                | Self::ReviewControllerMetadata { .. }
                | Self::ConfirmControllerMetadata { .. }
                | Self::ReviewControllerInput { .. }
                | Self::ConfirmControllerInput { .. }
                | Self::StartWorkflowRun { .. }
                | Self::ApproveWorkflowInput { .. }
        )
    }

    fn is_cancellation(&self) -> bool {
        matches!(
            self,
            Self::RestartMcp
                | Self::PreferenceChanged { .. }
                | Self::Revoke { .. }
                | Self::DenyDirectPeer { .. }
                | Self::CancelController { .. }
                | Self::UnregisterController { .. }
                | Self::CancelControllerMetadata { .. }
                | Self::RevokeControllerMetadata { .. }
                | Self::CancelControllerInput { .. }
                | Self::DisarmControllerInput { .. }
                | Self::ClearWorkflow
                | Self::CancelWorkflowRun { .. }
                | Self::DenyWorkflowInput { .. }
        )
    }

    fn is_controller(&self) -> bool {
        matches!(
            self,
            Self::EnableControllerEnrollment
                | Self::ChooseController { .. }
                | Self::SelectControllerTarget { .. }
                | Self::ConfirmController { .. }
                | Self::CancelController { .. }
                | Self::UnregisterController { .. }
                | Self::ReviewControllerMetadata { .. }
                | Self::ConfirmControllerMetadata { .. }
                | Self::CancelControllerMetadata { .. }
                | Self::RevokeControllerMetadata { .. }
                | Self::ReviewControllerInput { .. }
                | Self::ConfirmControllerInput { .. }
                | Self::CancelControllerInput { .. }
                | Self::DisarmControllerInput { .. }
        )
    }

    fn is_verification(&self) -> bool {
        matches!(
            self,
            Self::CompareDirectPeer { .. } | Self::DenyDirectPeer { .. }
        )
    }
}

pub(crate) struct NativeWindowCommand {
    pub window: WindowId,
    pub command: NativeCommand,
}

#[derive(Clone)]
pub(crate) struct ChannelRow {
    pub channel: u64,
    pub verification_code: String,
    pub identity_fingerprint: Option<String>,
    pub approved: bool,
    pub approved_targets: Vec<TargetRef>,
    closed: Arc<AtomicBool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerSelectionView {
    Idle,
    Choosing {
        token: u64,
        window: WindowId,
    },
    Reviewing {
        token: u64,
        window: WindowId,
        target: TargetRef,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerMetadataView {
    Unavailable,
    Idle {
        enrollment: ControllerEnrollmentId,
        window: WindowId,
        target: TargetRef,
        granted: bool,
    },
    Reviewing {
        token: u64,
        window: WindowId,
        target: TargetRef,
    },
    Cancelling,
    Revoking,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum ControllerInputView {
    Unavailable,
    Requested {
        token: u64,
        window: WindowId,
        target: TargetRef,
    },
    Preparing {
        token: u64,
        window: WindowId,
    },
    Reviewing {
        token: u64,
        window: WindowId,
        target: TargetRef,
        action: presentation::DirectActionDisplay,
        metadata_sources: Vec<String>,
    },
    Submitting {
        token: u64,
        window: WindowId,
        may_be_admitted: bool,
    },
    Cancelling,
}

#[derive(Clone)]
pub(crate) struct ControllerRow {
    pub channel: u64,
    pub registration: ControllerRegistrationId,
    pub public_id: ControllerId,
    pub label: String,
    pub associated_target: Option<TargetRef>,
    pub selection: ControllerSelectionView,
    pub metadata: ControllerMetadataView,
    pub input: ControllerInputView,
    pub control: Option<ControllerArmedControl>,
    valid: Arc<AtomicBool>,
    // Review presentation and confirmation are confined to the UI thread.
    metadata_review_valid: Option<Rc<Cell<bool>>>,
    input_review_valid: Option<Rc<Cell<bool>>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ControllerArmedControl {
    pub token: u64,
    pub window: WindowId,
    pub target: TargetRef,
}

impl ControllerRow {
    pub(crate) fn metadata_review_is_current(&self) -> bool {
        self.metadata_review_valid
            .as_ref()
            .is_some_and(|valid| valid.get())
    }

    #[cfg(test)]
    pub(crate) fn set_metadata_review_for_test(
        &mut self,
        token: u64,
        window: WindowId,
        target: TargetRef,
    ) {
        self.metadata = ControllerMetadataView::Reviewing {
            token,
            window,
            target,
        };
        self.metadata_review_valid = Some(Rc::new(Cell::new(true)));
    }

    #[cfg(test)]
    pub(crate) fn set_input_review_for_test(
        &mut self,
        token: u64,
        window: WindowId,
        target: TargetRef,
        action: presentation::DirectActionDisplay,
        metadata_sources: Vec<String>,
    ) {
        self.input = ControllerInputView::Reviewing {
            token,
            window,
            target,
            action,
            metadata_sources,
        };
        self.input_review_valid = Some(Rc::new(Cell::new(true)));
    }

    #[cfg(test)]
    pub(crate) fn set_input_requested_for_test(
        &mut self,
        token: u64,
        window: WindowId,
        target: TargetRef,
    ) {
        self.input = ControllerInputView::Requested {
            token,
            window,
            target,
        };
        self.input_review_valid = Some(Rc::new(Cell::new(true)));
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ControllerTargetRow {
    pub reference: TargetRef,
    pub window: WindowId,
    pub label: String,
}

#[derive(Clone)]
pub(crate) struct SettingsSnapshot {
    pub status: &'static str,
    pub channels: Vec<ChannelRow>,
    pub direct_verification: Option<verification::View>,
    pub targets: Vec<(TargetRef, String)>,
    pub controllers: Vec<ControllerRow>,
    pub controller_targets: Vec<ControllerTargetRow>,
    pub controller_targets_incomplete: bool,
    pub controller_opt_in_available: bool,
    pub controller_opted_in: bool,
    pub workflow: WorkflowView,
    pub javascript: javascript::View,
    stop: Arc<AtomicBool>,
}

impl Default for SettingsSnapshot {
    fn default() -> Self {
        Self {
            status: "MCP is off. No local adapter is listening.",
            channels: Vec::new(),
            direct_verification: None,
            targets: Vec::new(),
            controllers: Vec::new(),
            controller_targets: Vec::new(),
            controller_targets_incomplete: false,
            controller_opt_in_available: false,
            controller_opted_in: false,
            workflow: WorkflowView::default(),
            javascript: javascript::View::default(),
            stop: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl SettingsSnapshot {
    pub(crate) fn same_presentation(&self, other: &Self) -> bool {
        self.status == other.status
            && match (&self.direct_verification, &other.direct_verification) {
                (Some(left), Some(right)) => left.same_presentation(right),
                (None, None) => true,
                _ => false,
            }
            && self.targets == other.targets
            && self.controller_targets == other.controller_targets
            && self.controller_targets_incomplete == other.controller_targets_incomplete
            && self.controller_opt_in_available == other.controller_opt_in_available
            && self.controller_opted_in == other.controller_opted_in
            && self.controllers.len() == other.controllers.len()
            && self
                .controllers
                .iter()
                .zip(&other.controllers)
                .all(|(left, right)| {
                    left.channel == right.channel
                        && left.registration == right.registration
                        && left.public_id == right.public_id
                        && left.label == right.label
                        && left.associated_target == right.associated_target
                        && left.selection == right.selection
                        && left.metadata == right.metadata
                        && left.input == right.input
                        && left.control == right.control
                })
            && self.workflow.same_presentation(&other.workflow)
            && self.javascript == other.javascript
            && self.channels.len() == other.channels.len()
            && self
                .channels
                .iter()
                .zip(&other.channels)
                .all(|(left, right)| {
                    left.channel == right.channel
                        && left.verification_code == right.verification_code
                        && left.identity_fingerprint == right.identity_fingerprint
                        && left.approved == right.approved
                        && left.approved_targets == right.approved_targets
                })
    }

    pub(crate) fn disable(&self) {
        for row in &self.controllers {
            if let Some(valid) = &row.metadata_review_valid {
                valid.set(false);
            }
            if let Some(valid) = &row.input_review_valid {
                valid.set(false);
            }
        }
        set_admission_flag(&self.stop, true);
    }

    pub(crate) fn revoke(&self, channel: u64) {
        for row in self.controllers.iter().filter(|row| row.channel == channel) {
            if let Some(valid) = &row.metadata_review_valid {
                valid.set(false);
            }
            if let Some(valid) = &row.input_review_valid {
                valid.set(false);
            }
        }
        if let Some(row) = self.channels.iter().find(|row| row.channel == channel) {
            set_admission_flag(&row.closed, true);
        }
    }

    pub(crate) fn unregister_controller(
        &self,
        channel: u64,
        registration: ControllerRegistrationId,
    ) {
        if let Some(row) = self
            .controllers
            .iter()
            .find(|row| row.channel == channel && row.registration == registration)
        {
            if let Some(valid) = &row.metadata_review_valid {
                valid.set(false);
            }
            if let Some(valid) = &row.input_review_valid {
                valid.set(false);
            }
            set_admission_flag(&row.valid, false);
        }
    }

    pub(crate) fn cancel_controller_metadata_review(
        &self,
        channel: u64,
        registration: ControllerRegistrationId,
        review: u64,
    ) {
        if let Some(row) = self.controllers.iter().find(|row| {
            row.channel == channel
                && row.registration == registration
                && matches!(
                    row.metadata,
                    ControllerMetadataView::Reviewing { token, .. } if token == review
                )
        }) {
            if let Some(valid) = &row.metadata_review_valid {
                valid.set(false);
            }
        }
    }

    pub(crate) fn controller_metadata_review_is_current(&self, row: &ControllerRow) -> bool {
        !self.stop.load(Ordering::Acquire)
            && row.valid.load(Ordering::Acquire)
            && row.metadata_review_is_current()
            && self.channels.iter().any(|channel| {
                channel.channel == row.channel && !channel.closed.load(Ordering::Acquire)
            })
    }

    pub(crate) fn cancel_controller_input_review(
        &self,
        channel: u64,
        registration: ControllerRegistrationId,
        input: u64,
    ) {
        if let Some(row) = self.controllers.iter().find(|row| {
            row.channel == channel
                && row.registration == registration
                && matches!(
                    &row.input,
                    ControllerInputView::Requested { token, .. }
                        | ControllerInputView::Preparing { token, .. }
                        | ControllerInputView::Reviewing { token, .. } if *token == input
                )
        }) {
            if let Some(valid) = &row.input_review_valid {
                valid.set(false);
            }
        }
    }

    pub(crate) fn controller_input_review_is_current(&self, row: &ControllerRow) -> bool {
        !self.stop.load(Ordering::Acquire)
            && row.valid.load(Ordering::Acquire)
            && row
                .input_review_valid
                .as_ref()
                .is_some_and(|valid| valid.get())
            && self.channels.iter().any(|channel| {
                channel.channel == row.channel && !channel.closed.load(Ordering::Acquire)
            })
    }

    #[cfg(test)]
    pub(crate) fn with_pending_channel_for_test(channel: u64) -> Self {
        Self {
            channels: vec![ChannelRow {
                channel,
                verification_code: "0123456789abcdef".into(),
                identity_fingerprint: None,
                approved: false,
                approved_targets: Vec::new(),
                closed: Arc::new(AtomicBool::new(false)),
            }],
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn with_controller_for_test() -> (
        Self,
        ControllerRegistrationId,
        TargetRef,
        TargetRef,
        TargetRef,
        ControllerEnrollmentId,
    ) {
        let mut broker = Broker::new([7; 16]);
        let principal = broker.accept_connection().unwrap();
        let guard = festerm_session::automation_admission_lock().unwrap();
        let registration = broker
            .register_controller_session(principal, b"untrusted", &guard)
            .unwrap();
        let first = broker.attach_target(101, 1, &guard).unwrap();
        let second = broker.attach_target(102, 1, &guard).unwrap();
        let other_window = broker.attach_target(103, 1, &guard).unwrap();
        let proposal = broker
            .prepare_controller_enrollment(principal, registration, second, &guard)
            .unwrap();
        let enrollment = broker
            .approve_controller_enrollment(
                principal,
                NativeControllerEnrollment::new(proposal),
                &guard,
            )
            .unwrap();
        drop(guard);
        (
            Self {
                channels: vec![ChannelRow {
                    channel: 7,
                    verification_code: "0123456789abcdef".into(),
                    identity_fingerprint: None,
                    approved: false,
                    approved_targets: Vec::new(),
                    closed: Arc::new(AtomicBool::new(false)),
                }],
                controllers: vec![ControllerRow {
                    channel: 7,
                    registration,
                    public_id: ControllerId::from_random_bytes([9; 16]),
                    label: "untrusted".into(),
                    associated_target: None,
                    selection: ControllerSelectionView::Idle,
                    metadata: ControllerMetadataView::Unavailable,
                    input: ControllerInputView::Unavailable,
                    control: None,
                    valid: Arc::new(AtomicBool::new(true)),
                    metadata_review_valid: None,
                    input_review_valid: None,
                }],
                controller_targets: vec![
                    ControllerTargetRow {
                        reference: first,
                        window: WindowId::PRIMARY,
                        label: "tab position 1 — untrusted name \"Same\"".into(),
                    },
                    ControllerTargetRow {
                        reference: second,
                        window: WindowId::PRIMARY,
                        label: "tab position 2 — untrusted name \"Same\"".into(),
                    },
                    ControllerTargetRow {
                        reference: other_window,
                        window: WindowId::for_test(2),
                        label: "tab position 3 — untrusted name \"Same\"".into(),
                    },
                ],
                controller_opt_in_available: true,
                controller_opted_in: true,
                stop: Arc::new(AtomicBool::new(false)),
                ..Self::default()
            },
            registration,
            first,
            second,
            other_window,
            enrollment,
        )
    }
}

pub(crate) fn set_admission_flag(flag: &AtomicBool, value: bool) {
    let guard = festerm_session::automation_admission_lock();
    let poisoned = guard.is_none();
    flag.store(value, Ordering::Release);
    drop(guard);
    if poisoned {
        tracing::error!("automation admission lock poisoned; disclosure remains closed");
    }
}

struct Grant {
    targets: BTreeSet<TargetRef>,
    target_liveness: Vec<ipc::TargetGuard>,
    expires: Instant,
}

struct Wait {
    key: RequestKey,
    run: RunId,
    request: RequestId,
    target: TargetRef,
    after_revision: u64,
    deadline: Instant,
    // The protocol event was consumed; retain its exact run until it is cut.
    cancel_pending: bool,
}

#[derive(Clone, Copy)]
struct PendingCompletion {
    run: RunId,
    request: Option<RequestId>,
}

struct Peer {
    connection: ipc::Connection,
    principal: ConnectionId,
    machine: Machine,
    code: VerificationCode,
    identity: Option<festerm_automation_mcp::identity::VerifiedClientIdentity>,
    grant: Option<Grant>,
    waits: Vec<Wait>,
    pending_completion: Option<PendingCompletion>,
    #[cfg(test)]
    after_finish: Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>,
    controller: controller::PeerState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    output_bytes: u64,
    screen_generation: u64,
    columns: usize,
    rows: usize,
    history: ScrollbackStats,
    local_revision: u64,
}

struct Target {
    reference: TargetRef,
    controller_instance: u64,
    controller_generation: u64,
    local_pty: bool,
    liveness: Arc<AtomicBool>,
    registration: Arc<AtomicBool>,
    attachment: Arc<festerm_session::AutomationEpoch>,
    attachment_generation: u64,
    stamp: Stamp,
    revision: u64,
}

impl Target {
    fn guard(&self) -> ipc::TargetGuard {
        ipc::TargetGuard {
            liveness: self.liveness.clone(),
            registration: self.registration.clone(),
            attachment: self.attachment.clone(),
            generation: self.attachment_generation,
        }
    }
}

struct Prepared {
    privacy: PrivacyMonitor,
    instance: [u8; 16],
}

#[derive(Debug, PartialEq, Eq)]
enum StartFailure {
    Privacy,
    PrivacyCleanup,
    PeerIdentity,
    Randomness,
    Endpoint,
    Cancelled,
}

struct Starting<T> {
    result: mpsc::Receiver<Result<T, StartFailure>>,
    thread: JoinHandle<()>,
}

type Cleanup = Box<dyn FnOnce() -> bool + Send>;

#[derive(Default)]
struct Retirement {
    thread: Option<JoinHandle<bool>>,
    stranded: Vec<Arc<Mutex<Option<Cleanup>>>>,
    failed: bool,
}

impl Drop for Retirement {
    fn drop(&mut self) {
        for cleanup in self.stranded.drain(..) {
            // A cleanup worker could not be started. Do not join its resources on the UI thread.
            std::mem::forget(cleanup);
        }
    }
}

// The early loss flag is also checked by transport admission and physical writes.
struct PrivacyGate {
    closed: Arc<AtomicBool>,
    mcp_stop: Mutex<Option<Arc<AtomicBool>>>,
}

// A tick temporarily owns the broker outside the coordinator. Never acquire
// admission while unwinding: the panic may have occurred while holding it.
struct TakenBrokerUnwind {
    privacy: Arc<PrivacyGate>,
    stop: Arc<AtomicBool>,
}

struct CoordinatorUnwind<'a> {
    coordinator: &'a mut Coordinator,
}

impl Drop for CoordinatorUnwind<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.coordinator.fail_closed_on_unwind();
        }
    }
}

impl Drop for TakenBrokerUnwind {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.privacy.closed.store(true, Ordering::Release);
            self.stop.store(true, Ordering::Release);
        }
    }
}

impl PrivacyGate {
    fn new(closed: bool) -> Self {
        Self {
            closed: Arc::new(AtomicBool::new(closed)),
            mcp_stop: Mutex::new(None),
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn closed_flag(&self) -> Arc<AtomicBool> {
        self.closed.clone()
    }

    fn signal_loss(&self) {
        self.closed.store(true, Ordering::Release);
    }

    fn try_admit(&self) -> bool {
        try_privacy_admission(&self.closed)
    }

    fn close(&self) {
        self.signal_loss();
        let guard = festerm_session::automation_admission_lock();
        let poisoned = guard.is_none();
        let stop = self
            .mcp_stop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(stop) = stop.as_ref() {
            stop.store(true, Ordering::Release);
        }
        drop(stop);
        drop(guard);
        if poisoned {
            tracing::error!("automation admission lock poisoned; disclosure remains closed");
        }
    }

    fn bind_mcp_under_admission(
        &self,
        stop: &Arc<AtomicBool>,
        _guard: &festerm_session::AutomationAdmissionGuard,
    ) -> bool {
        // Every non-poisoned access to this mutex holds the admission guard.
        let mut current = match self.mcp_stop.lock() {
            Ok(current) => current,
            Err(error) => {
                self.closed.store(true, Ordering::Release);
                if let Some(previous) = error.into_inner().as_ref() {
                    previous.store(true, Ordering::Release);
                }
                stop.store(true, Ordering::Release);
                return false;
            }
        };
        if self.is_closed() || stop.load(Ordering::Acquire) {
            stop.store(true, Ordering::Release);
            return false;
        }
        if let Some(previous) = current.replace(stop.clone()) {
            if !Arc::ptr_eq(&previous, stop) {
                previous.store(true, Ordering::Release);
            }
        }
        true
    }

    #[cfg(test)]
    fn bind_mcp(&self, stop: &Arc<AtomicBool>) -> bool {
        let Some(guard) = festerm_session::automation_admission_lock() else {
            self.close();
            stop.store(true, Ordering::Release);
            return false;
        };
        self.bind_mcp_under_admission(stop, &guard)
    }
}

fn try_privacy_admission(closed: &AtomicBool) -> bool {
    // This RMW orders the final admission point against the one-way loss cut.
    closed
        .compare_exchange(false, false, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

fn prepare_shared<P>(
    gate: &PrivacyGate,
    install: impl FnOnce() -> Result<P, StartFailure>,
    nonce: impl FnOnce() -> Result<[u8; 16], StartFailure>,
    retire: impl FnOnce(P) -> bool,
) -> Result<(P, [u8; 16]), StartFailure> {
    let privacy = match install() {
        Ok(privacy) => privacy,
        Err(failure) => {
            gate.close();
            return Err(failure);
        }
    };
    let instance = if gate.is_closed() {
        Err(StartFailure::Cancelled)
    } else {
        nonce().and_then(|instance| {
            if gate.is_closed() {
                Err(StartFailure::Cancelled)
            } else {
                Ok(instance)
            }
        })
    };
    match instance {
        Ok(instance) => Ok((privacy, instance)),
        Err(failure) => {
            gate.close();
            if retire(privacy) {
                Err(failure)
            } else {
                Err(StartFailure::PrivacyCleanup)
            }
        }
    }
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrivacyCleanupProgress {
    Pending,
    Complete,
    Unknown,
}

#[cfg(any(windows, test))]
fn await_privacy_cleanup(
    state: impl Fn() -> PrivacyCleanupProgress,
    quarantine: impl FnOnce() -> bool,
    timeout: Duration,
) -> bool {
    let started = Instant::now();
    loop {
        match state() {
            PrivacyCleanupProgress::Complete => return true,
            PrivacyCleanupProgress::Unknown => return false,
            PrivacyCleanupProgress::Pending => {}
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return quarantine();
        }
        thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

#[cfg(windows)]
pub(crate) fn finish_privacy_cleanup(status: &festerm_windows_power::PrivacyCleanupStatus) -> bool {
    use festerm_windows_power::{
        privacy_monitor_ownership, PrivacyCleanupState, PrivacyOwnershipState,
    };

    let complete = await_privacy_cleanup(
        || match status.state() {
            PrivacyCleanupState::Active | PrivacyCleanupState::Pending => {
                PrivacyCleanupProgress::Pending
            }
            PrivacyCleanupState::Complete => match privacy_monitor_ownership() {
                PrivacyOwnershipState::Available => PrivacyCleanupProgress::Complete,
                PrivacyOwnershipState::Occupied => PrivacyCleanupProgress::Pending,
                PrivacyOwnershipState::Unknown => PrivacyCleanupProgress::Unknown,
            },
            PrivacyCleanupState::Unknown => PrivacyCleanupProgress::Unknown,
        },
        || {
            status.quarantine_if_unresolved().is_ok()
                && privacy_monitor_ownership() == PrivacyOwnershipState::Available
        },
        Duration::from_secs(5),
    );
    if !complete {
        tracing::error!(
            "Windows privacy cleanup or slot ownership is unresolved; restart is required"
        );
    }
    complete
}

#[cfg(windows)]
fn retire_privacy(privacy: PrivacyMonitor) -> bool {
    privacy.close()
}

#[cfg(not(windows))]
fn retire_privacy(_privacy: PrivacyMonitor) -> bool {
    true
}

fn install_privacy(
    gate: &PrivacyGate,
    callback: Arc<dyn Fn() + Send + Sync>,
) -> Result<PrivacyMonitor, StartFailure> {
    #[cfg(windows)]
    let installed = PrivacyMonitor::install_with_loss_flag(callback, gate.closed_flag());
    #[cfg(not(windows))]
    let installed = PrivacyMonitor::install(callback);
    match installed {
        Ok(privacy) => Ok(privacy),
        Err(error) => {
            gate.close();
            tracing::warn!(
                "automation privacy monitor installation failed: {:?}",
                error.kind()
            );
            #[cfg(windows)]
            {
                handle_windows_privacy_failure(error)
            }
            #[cfg(not(windows))]
            {
                Err(StartFailure::Privacy)
            }
        }
    }
}

#[cfg(windows)]
fn handle_windows_privacy_failure(
    mut error: std::io::Error,
) -> Result<PrivacyMonitor, StartFailure> {
    use festerm_windows_power::{
        privacy_monitor_ownership, PrivacyInstallFailure, PrivacyOwnershipState,
    };

    let (cleanup, rejected) = error
        .get_mut()
        .and_then(|source| source.downcast_mut::<PrivacyInstallFailure>())
        .map(|failure| (failure.cleanup_status(), failure.take_rejected()))
        .unwrap_or((None, None));
    drop(error);
    drop(rejected);
    if cleanup
        .as_ref()
        .is_some_and(|status| !finish_privacy_cleanup(status))
        || privacy_monitor_ownership() == PrivacyOwnershipState::Unknown
    {
        Err(StartFailure::PrivacyCleanup)
    } else {
        Err(StartFailure::Privacy)
    }
}

fn retire_start<T>(starting: Starting<T>, retire: impl FnOnce(T) -> bool) -> bool {
    let result = starting.result.recv();
    let resource_ok = match result {
        Ok(Ok(started)) => retire(started),
        Ok(Err(failure)) => failure != StartFailure::PrivacyCleanup,
        Err(_) => {
            tracing::warn!("automation startup worker returned no result");
            false
        }
    };
    let worker_ok = if starting.thread.join().is_err() {
        tracing::warn!("automation startup worker failed");
        false
    } else {
        true
    };
    resource_ok && worker_ok
}

fn retire_shared_start(starting: Starting<Prepared>) -> bool {
    retire_start(starting, |prepared| retire_privacy(prepared.privacy))
}

enum ReconcileError {
    Admission(festerm_session::AutomationAdmissionTryError),
    Authority(festerm_automation::Error),
}

pub(crate) struct Coordinator {
    workflow: workflow_selection::Selection,
    javascript_enabled: bool,
    javascript: javascript::State,
    presentation_workspace: Option<WorkspaceRevision>,
    control_handoffs: BTreeMap<ConnectionId, presentation::BoundControlHandoff>,
    pending_control_reviews:
        BTreeMap<festerm_automation::ApprovalId, presentation::BoundControlReview>,
    metadata_grants: BTreeMap<ConnectionId, presentation::BoundMetadataGrant>,
    pending_metadata_reviews:
        BTreeMap<festerm_automation::ApprovalId, presentation::BoundMetadataReview>,
    enrollment_metadata_grants:
        BTreeMap<presentation::EnrollmentMetadataKey, presentation::BoundEnrollmentMetadataGrant>,
    pending_enrollment_metadata_reviews:
        BTreeMap<festerm_automation::ApprovalId, presentation::BoundEnrollmentMetadataReview>,
    broker: Option<Broker>,
    server: Option<ipc::Server>,
    verification: verification::Owner,
    privacy: Option<PrivacyMonitor>,
    starting: Option<Starting<Prepared>>,
    mcp_starting: Option<Starting<ipc::Server>>,
    retiring: Retirement,
    mcp_retiring: Retirement,
    want_shared_start: bool,
    want_start: bool,
    privacy_gate: Arc<PrivacyGate>,
    stop: Arc<AtomicBool>,
    enabled: bool,
    privacy_lost_this_tick: bool,
    reconciliation_deferred: bool,
    unwind_stopped: bool,
    unwind_cleanup_pending: bool,
    workflow_unwind_stopped: bool,
    workflow_unwind_disabled_seen: bool,
    #[cfg(test)]
    after_reconcile: Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>,
    #[cfg(test)]
    peer_message_busy_repaint: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pending_cleanup_busy_repaint: Option<Arc<AtomicBool>>,
    status: &'static str,
    peers: BTreeMap<u64, Peer>,
    targets: BTreeMap<u64, Target>,
    generation: u64,
    controller_selection_revision: u64,
    controller_opted_in: bool,
    #[cfg(test)]
    synthetic_controller_qualified: bool,
    #[cfg(test)]
    synthetic_enrollment_metadata_ui_qualified: bool,
    #[cfg(test)]
    synthetic_enrollment_metadata_delivery_qualified: bool,
    #[cfg(test)]
    synthetic_direct_input_qualified: bool,
    #[cfg(test)]
    synthetic_direct_input_lost_after_commit: bool,
    #[cfg(all(test, unix))]
    synthetic_direct_input_busy_after_admit_once: bool,
}

impl Default for Coordinator {
    fn default() -> Self {
        Self {
            workflow: workflow_selection::Selection::default(),
            javascript_enabled: false,
            javascript: javascript::State::default(),
            presentation_workspace: None,
            control_handoffs: BTreeMap::new(),
            pending_control_reviews: BTreeMap::new(),
            metadata_grants: BTreeMap::new(),
            pending_metadata_reviews: BTreeMap::new(),
            enrollment_metadata_grants: BTreeMap::new(),
            pending_enrollment_metadata_reviews: BTreeMap::new(),
            broker: None,
            server: None,
            verification: verification::Owner::default(),
            privacy: None,
            starting: None,
            mcp_starting: None,
            retiring: Retirement::default(),
            mcp_retiring: Retirement::default(),
            want_shared_start: false,
            want_start: false,
            privacy_gate: Arc::new(PrivacyGate::new(true)),
            stop: Arc::new(AtomicBool::new(true)),
            enabled: false,
            privacy_lost_this_tick: false,
            reconciliation_deferred: false,
            unwind_stopped: false,
            unwind_cleanup_pending: false,
            workflow_unwind_stopped: false,
            workflow_unwind_disabled_seen: false,
            #[cfg(test)]
            after_reconcile: None,
            #[cfg(test)]
            peer_message_busy_repaint: None,
            #[cfg(test)]
            pending_cleanup_busy_repaint: None,
            status: SettingsSnapshot::default().status,
            peers: BTreeMap::new(),
            targets: BTreeMap::new(),
            generation: 0,
            controller_selection_revision: 0,
            controller_opted_in: false,
            #[cfg(test)]
            synthetic_controller_qualified: false,
            #[cfg(test)]
            synthetic_enrollment_metadata_ui_qualified: false,
            #[cfg(test)]
            synthetic_enrollment_metadata_delivery_qualified: false,
            #[cfg(test)]
            synthetic_direct_input_qualified: false,
            #[cfg(test)]
            synthetic_direct_input_lost_after_commit: false,
            #[cfg(all(test, unix))]
            synthetic_direct_input_busy_after_admit_once: false,
        }
    }
}

impl Coordinator {
    #[allow(dead_code)]
    fn direct_verification_surface(&self) -> verification::Surface {
        self.verification.surface()
    }
}

#[cfg(test)]
pub(crate) fn retry_automation_busy<T>(
    mut operation: impl FnMut() -> festerm_automation::Result<T>,
) -> festerm_automation::Result<T> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let result = operation();
        if !matches!(result, Err(festerm_automation::Error::Busy)) || Instant::now() >= deadline {
            return result;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn random<const N: usize>() -> Result<[u8; N], Refusal> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|_| Refusal::Unavailable)?;
    Ok(bytes)
}

fn session<'a>(apps: &[&'a FesTermApp], target: TargetRef) -> Option<&'a SessionTab> {
    apps.iter()
        .find_map(|app| app.automation_session(TabId::from_chip_id(target.session())))
}

fn approval_label(value: &str) -> String {
    bounded_approval_label(value, false)
}

fn quoted_approval_label(value: &str) -> String {
    bounded_approval_label(value, true)
}

fn bounded_approval_label(value: &str, quoted: bool) -> String {
    let mut characters = value.chars();
    let mut label = String::new();
    for character in characters.by_ref().take(128) {
        append_approval_character(&mut label, character, quoted);
    }
    if characters.next().is_some() {
        label.push_str("...");
    }
    label
}

fn append_approval_character(output: &mut String, character: char, quoted: bool) {
    let spoofing_control = character.is_control()
        || matches!(
            character,
            '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}' | '\u{feff}'
        );
    if quoted && (spoofing_control || matches!(character, '"' | '\'' | '\\')) {
        output.extend(character.escape_default());
    } else if spoofing_control {
        output.extend(character.escape_unicode());
    } else {
        output.push(character);
    }
}

fn quoted_approval_input(value: &str) -> String {
    let mut display = String::new();
    for character in value.chars() {
        append_approval_character(&mut display, character, true);
    }
    display
}

fn stamp(session: &SessionTab) -> Stamp {
    let dimensions = session.terminal.screen().dimensions();
    Stamp {
        output_bytes: session.controller.resize_probe().observed_output_bytes(),
        screen_generation: session.terminal.screen_generation(),
        columns: dimensions.columns(),
        rows: dimensions.rows(),
        history: session.terminal.scrollback_stats(),
        local_revision: session.controller.observation_revision(),
    }
}

fn retire_target(
    broker: &mut Broker,
    target: Target,
    guard: &festerm_session::AutomationAdmissionGuard,
) -> bool {
    target.registration.store(false, Ordering::Release);
    broker.close_target(target.reference, guard).is_ok()
        && broker.forget_closed_target(target.reference).is_ok()
}

fn retire(retiring: &mut Retirement, name: &str, cleanup: impl FnOnce() -> bool + Send + 'static) {
    retire_with(retiring, name, cleanup, |name, task| {
        thread::Builder::new().name(name.into()).spawn(task)
    });
}

fn retire_with(
    retiring: &mut Retirement,
    name: &str,
    cleanup: impl FnOnce() -> bool + Send + 'static,
    spawn: impl FnOnce(&str, Cleanup) -> std::io::Result<JoinHandle<bool>>,
) {
    let previous = retiring.thread.take();
    let cleanup: Cleanup = Box::new(move || {
        let current_ok = cleanup();
        let previous_ok = previous.is_none_or(|thread| match thread.join() {
            Ok(ok) => ok,
            Err(_) => {
                tracing::error!("automation previous cleanup worker failed");
                false
            }
        });
        current_ok && previous_ok
    });
    let cleanup = Arc::new(Mutex::new(Some(cleanup)));
    let worker_cleanup = cleanup.clone();
    let task: Cleanup = Box::new(move || {
        let work = worker_cleanup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        match work {
            Some(work) => work(),
            None => {
                tracing::error!("automation cleanup ownership was lost");
                false
            }
        }
    });
    match spawn(name, task) {
        Ok(thread) => retiring.thread = Some(thread),
        Err(error) => {
            retiring.failed = true;
            retiring.stranded.push(cleanup);
            tracing::error!("automation cleanup worker unavailable: {error}");
        }
    }
}

fn cleanup_ready(retiring: &mut Retirement, context: &egui::Context, waiting: bool) -> bool {
    if retiring
        .thread
        .as_ref()
        .is_some_and(|thread| !thread.is_finished())
    {
        if waiting && !retiring.failed {
            context.request_repaint_after(Duration::from_millis(50));
        }
        return false;
    }
    if let Some(thread) = retiring.thread.take() {
        if !matches!(thread.join(), Ok(true)) {
            retiring.failed = true;
            tracing::error!("automation cleanup worker failed");
        }
    }
    !retiring.failed
}

const CLEANUP_FAILED: &str =
    "Automation cleanup failed. Restart fesTerm before starting this service.";

/// Beta qualification (ADR 0065) of controller enrollment, enrollment
/// metadata review/delivery and direct input for local terminals. The
/// coordinator still requires the setting, privacy monitor, verification
/// match, native approvals and every admission fence. Tests use synthetic
/// per-feature flags instead.
#[cfg(not(test))]
const BETA_NATIVE_QUALIFIED: bool = true;

fn start_failure_status(failure: StartFailure) -> &'static str {
    match failure {
        StartFailure::Privacy => {
            "MCP unavailable: no qualified OS lock/suspend privacy monitor."
        }
        StartFailure::PrivacyCleanup => CLEANUP_FAILED,
        StartFailure::PeerIdentity => "MCP beta is unavailable on this platform.",
        StartFailure::Randomness => "MCP unavailable: secure randomness failed.",
        StartFailure::Endpoint => {
            "MCP unavailable: private endpoint refused. Check another app instance or private state-directory permissions."
        }
        StartFailure::Cancelled => {
            "MCP stopped during startup. Restart explicitly; its grants were revoked."
        }
    }
}

impl Coordinator {
    // Application and coordinator guards may unwind together; these cuts are
    // idempotent and do not acquire admission or invoke callbacks.
    pub(crate) fn fail_closed_on_unwind(&mut self) {
        self.privacy_gate.closed.store(true, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        self.want_shared_start = false;
        self.want_start = false;
        self.controller_opted_in = false;
        self.javascript_enabled = false;
        self.workflow.disarm();
        self.javascript_unwind();
        controller::disarm_metadata_reviews(&self.peers);
        self.workflow_unwind_stopped = true;
        self.workflow_unwind_disabled_seen = false;
        self.privacy_lost_this_tick = true;
        self.unwind_stopped = true;
        self.unwind_cleanup_pending = true;
        self.status = "Automation stopped after privacy loss or an internal safety limit. Restart explicitly; all automation grants were revoked.";
        self.broker = None;
    }

    fn controller_qualified(&self) -> bool {
        #[cfg(test)]
        let native_qualified = self.synthetic_controller_qualified;
        #[cfg(not(test))]
        let native_qualified = BETA_NATIVE_QUALIFIED;
        native_qualified
            && self
                .presentation_workspace
                .as_ref()
                .and_then(WorkspaceRevision::current)
                .is_some()
    }

    fn enrollment_metadata_ui_qualified(&self) -> bool {
        #[cfg(test)]
        let native_qualified = self.synthetic_enrollment_metadata_ui_qualified;
        #[cfg(not(test))]
        let native_qualified = BETA_NATIVE_QUALIFIED;
        native_qualified && self.controller_qualified()
    }

    fn enrollment_metadata_delivery_qualified(&self) -> bool {
        #[cfg(test)]
        let delivery_qualified = self.synthetic_enrollment_metadata_delivery_qualified;
        // Metadata delivery has no qualified recipient fence; the beta keeps
        // it off while review and reviewed input are available.
        #[cfg(not(test))]
        let delivery_qualified = false;
        delivery_qualified && self.enrollment_metadata_ui_qualified()
    }

    fn direct_input_qualified(&self) -> bool {
        #[cfg(test)]
        let native_qualified = self.synthetic_direct_input_qualified;
        #[cfg(not(test))]
        let native_qualified = BETA_NATIVE_QUALIFIED;
        native_qualified && self.controller_qualified()
    }

    fn machine_for_new_peer(&self) -> Machine {
        if self.controller_opted_in && self.controller_qualified() {
            if self.direct_input_qualified() {
                Machine::with_direct_input_catalog()
            } else {
                Machine::with_controller_enrollment_and_unavailable_presentation()
            }
        } else {
            Machine::with_unavailable_presentation()
        }
    }

    fn stop_mcp(&mut self) {
        let needs_guard = !self.stop.load(Ordering::Acquire) || !self.peers.is_empty();
        let guard = needs_guard
            .then(festerm_session::automation_admission_lock)
            .flatten();
        self.want_start = false;
        self.controller_opted_in = false;
        controller::disarm_metadata_reviews(&self.peers);
        self.stop.store(true, Ordering::Release);
        self.control_handoffs.clear();
        self.pending_control_reviews.clear();
        self.metadata_grants.clear();
        self.pending_metadata_reviews.clear();
        self.enrollment_metadata_grants.clear();
        self.pending_enrollment_metadata_reviews.clear();
        let peers = std::mem::take(&mut self.peers);
        let mut wakes = Vec::with_capacity(peers.len());
        if let Some(guard) = guard.as_ref() {
            for peer in peers.values() {
                wakes.push(peer.connection.close_under_admission(guard));
                if let Some(broker) = &mut self.broker {
                    broker.disconnect(peer.principal, guard);
                }
            }
        } else if needs_guard {
            for peer in peers.values() {
                peer.connection.closed_flag().store(true, Ordering::Release);
            }
            self.broker = None;
        }
        let poisoned = needs_guard && guard.is_none();
        if let Some(guard) = guard {
            ipc::wake_after_admission(guard, wakes);
        } else if needs_guard {
            for peer in peers.values() {
                peer.connection.wake_output();
            }
        }
        self.verification.revoke();
        if poisoned {
            tracing::error!("automation admission lock poisoned; disclosure remains closed");
        }
        for (_, mut peer) in peers {
            peer.machine.close();
        }
        let starting = self.mcp_starting.take();
        let server = self.server.take();
        if starting.is_some() || server.is_some() {
            retire(&mut self.mcp_retiring, "festerm-mcp-cleanup", move || {
                let startup_ok = starting.is_none_or(|starting| {
                    drop(starting.result);
                    if starting.thread.join().is_err() {
                        tracing::warn!("MCP startup worker failed");
                        false
                    } else {
                        true
                    }
                });
                drop(server);
                startup_ok
            });
        }
    }

    fn close_shared_admission(&mut self) {
        let dormant = self.privacy_gate.is_closed()
            && self.stop.load(Ordering::Acquire)
            && self.broker.is_none()
            && self.targets.is_empty()
            && self.peers.is_empty()
            && self.server.is_none()
            && self.privacy.is_none()
            && self.starting.is_none()
            && self.mcp_starting.is_none();
        if !dormant {
            self.privacy_gate.close();
        }
        if self.broker.is_some() || !self.targets.is_empty() {
            let guard = festerm_session::automation_admission_lock();
            if let Some(guard) = guard.as_ref() {
                if let Some(broker) = &mut self.broker {
                    broker.revoke_all(guard);
                }
            } else {
                self.broker = None;
            }
            for target in self.targets.values() {
                target.registration.store(false, Ordering::Release);
            }
        }
        self.control_handoffs.clear();
        self.pending_control_reviews.clear();
        self.metadata_grants.clear();
        self.pending_metadata_reviews.clear();
        self.enrollment_metadata_grants.clear();
        self.pending_enrollment_metadata_reviews.clear();
    }

    fn shutdown(&mut self) {
        self.cancel_javascript("Workflow stopped because automation shut down.");
        self.want_shared_start = false;
        self.close_shared_admission();
        self.stop_mcp();
        self.targets.clear();
        self.broker = None;
        let starting = self.starting.take();
        let privacy = self.privacy.take();
        if starting.is_some() || privacy.is_some() {
            retire(
                &mut self.retiring,
                "festerm-automation-cleanup",
                move || {
                    let startup_ok = starting.is_none_or(retire_shared_start);
                    let privacy_ok = privacy.is_none_or(retire_privacy);
                    startup_ok && privacy_ok
                },
            );
        }
    }

    fn start(&mut self, context: &egui::Context) {
        self.restart_with_bootstrap(context, ipc::require_qualified_bootstrap());
    }

    fn restart_with_bootstrap(&mut self, context: &egui::Context, bootstrap: std::io::Result<()>) {
        if self.unwind_stopped {
            return;
        }
        if self.privacy_gate.is_closed() && (self.broker.is_some() || self.starting.is_some()) {
            self.privacy_lost();
            return;
        }
        self.stop_mcp();
        self.stop = Arc::new(AtomicBool::new(false));
        if let Err(error) = bootstrap {
            set_admission_flag(&self.stop, true);
            self.status =
                start_failure_status(if error.kind() == std::io::ErrorKind::Unsupported {
                    StartFailure::PeerIdentity
                } else {
                    StartFailure::Endpoint
                });
            tracing::warn!("{}", self.status);
            return;
        }
        self.want_start = true;
        self.status = "Starting MCP privacy monitoring; no access is available yet.";
        self.ensure_shared();
        self.advance_start(context);
    }

    fn ensure_shared(&mut self) {
        if self.broker.is_none() && self.starting.is_none() {
            self.want_shared_start = true;
        }
    }

    fn privacy_lost(&mut self) {
        self.privacy_lost_this_tick = true;
        self.shutdown();
        if !self.enabled {
            self.status = SettingsSnapshot::default().status;
        } else {
            self.status = "Automation stopped after privacy loss or an internal safety limit. Restart explicitly; all automation grants were revoked.";
        }
    }

    fn privacy_lost_with_taken_broker(&mut self, broker: &mut Broker) {
        // Keep ownership in tick until shutdown returns: logging or cleanup may
        // unwind, in which case the sentinel must run before broker drop.
        if let Some(guard) = festerm_session::automation_admission_lock() {
            broker.revoke_all(&guard);
        }
        self.privacy_lost();
    }

    fn finish_unwind_cleanup(&mut self) {
        self.privacy_lost();
        self.workflow.cancel();
        self.unwind_cleanup_pending = false;
    }

    fn mcp_failed(&mut self) {
        self.stop_mcp();
        self.status =
            "MCP stopped after transport failure. Restart explicitly; its grants were revoked.";
    }

    fn shared_start_failed(&mut self, failure: StartFailure) {
        self.privacy_lost_this_tick = true;
        self.privacy_gate.close();
        if failure == StartFailure::PrivacyCleanup {
            self.retiring.failed = true;
        }
        if self.want_start {
            self.stop_mcp();
            self.status = start_failure_status(failure);
            tracing::warn!("{}", self.status);
        }
    }

    fn advance_shared(&mut self, context: &egui::Context) {
        if self.unwind_stopped {
            return;
        }
        let result = self
            .starting
            .as_ref()
            .map(|starting| starting.result.try_recv());
        match result {
            Some(Ok(result)) => {
                self.starting = None;
                match result {
                    Ok(prepared) => {
                        self.privacy = Some(prepared.privacy);
                        if self.privacy_gate.is_closed() {
                            self.privacy_lost();
                            return;
                        }
                        self.broker = Some(Broker::new(prepared.instance));
                    }
                    Err(failure) => self.shared_start_failed(failure),
                }
            }
            Some(Err(mpsc::TryRecvError::Empty)) => {
                context.request_repaint_after(Duration::from_millis(50));
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.starting = None;
                self.privacy_lost_this_tick = true;
                self.privacy_gate.close();
                self.retiring.failed = true;
                if self.want_start {
                    self.stop_mcp();
                }
                self.status = CLEANUP_FAILED;
                tracing::error!("automation startup worker exited without a cleanup result");
            }
            None => {}
        }

        if !cleanup_ready(&mut self.retiring, context, self.want_shared_start) {
            if self.retiring.failed && self.want_shared_start {
                self.want_shared_start = false;
                self.privacy_gate.close();
                if self.want_start {
                    self.stop_mcp();
                }
                self.status = CLEANUP_FAILED;
            }
            return;
        }
        if self.want_shared_start && self.broker.is_none() && self.starting.is_none() {
            let guard = match festerm_session::try_automation_admission_lock() {
                Ok(guard) => guard,
                Err(festerm_session::AutomationAdmissionTryError::Busy) => {
                    self.reconciliation_deferred = true;
                    context.request_repaint_after(Duration::from_millis(50));
                    return;
                }
                Err(festerm_session::AutomationAdmissionTryError::Poisoned) => {
                    self.want_shared_start = false;
                    self.privacy_gate = Arc::new(PrivacyGate::new(false));
                    self.shared_start_failed(StartFailure::Privacy);
                    return;
                }
            };
            self.want_shared_start = false;
            self.privacy_gate = Arc::new(PrivacyGate::new(false));
            if self.want_start
                && !self.stop.load(Ordering::Acquire)
                && !self
                    .privacy_gate
                    .bind_mcp_under_admission(&self.stop, &guard)
                && self.privacy_gate.is_closed()
            {
                drop(guard);
                self.shared_start_failed(StartFailure::Privacy);
                return;
            }
            drop(guard);
            let gate = self.privacy_gate.clone();
            let notify = context.clone();
            let (send, result) = mpsc::sync_channel(1);
            match thread::Builder::new()
                .name("festerm-automation-startup".into())
                .spawn(move || {
                    let privacy_gate = gate.clone();
                    let privacy_notify = notify.clone();
                    let prepared = prepare_shared(
                        &gate,
                        || {
                            install_privacy(
                                &gate,
                                Arc::new(move || {
                                    privacy_gate.signal_loss();
                                    privacy_notify.request_repaint();
                                }),
                            )
                        },
                        || random().map_err(|_| StartFailure::Randomness),
                        retire_privacy,
                    )
                    .map(|(privacy, instance)| Prepared { privacy, instance });
                    if let Err(mpsc::SendError(result)) = send.send(prepared) {
                        gate.close();
                        match result {
                            Ok(prepared) => {
                                if !retire_privacy(prepared.privacy) {
                                    tracing::error!("automation startup privacy cleanup failed");
                                }
                            }
                            Err(StartFailure::PrivacyCleanup) => {
                                tracing::error!("automation startup privacy cleanup failed");
                            }
                            Err(_) => {}
                        }
                    }
                    notify.request_repaint();
                }) {
                Ok(thread) => {
                    self.starting = Some(Starting { result, thread });
                    context.request_repaint_after(Duration::from_millis(50));
                }
                Err(error) => {
                    tracing::warn!("automation startup worker unavailable: {error}");
                    self.privacy_lost_this_tick = true;
                    self.privacy_gate.close();
                    if self.want_start {
                        self.stop_mcp();
                        self.status = "MCP unavailable: startup worker failed.";
                    }
                }
            }
        }
    }

    fn advance_mcp(&mut self, context: &egui::Context) {
        if self.unwind_stopped {
            return;
        }
        let result = self
            .mcp_starting
            .as_ref()
            .map(|starting| starting.result.try_recv());
        match result {
            Some(Ok(result)) => {
                self.mcp_starting = None;
                match result {
                    Ok(server) => {
                        self.server = Some(server);
                        if self.privacy_gate.is_closed() {
                            self.privacy_lost();
                            return;
                        }
                        if self.stop.load(Ordering::Acquire) || !self.enabled {
                            self.stop_mcp();
                            if self.enabled {
                                self.status = start_failure_status(StartFailure::Cancelled);
                            }
                        } else {
                            self.want_start = false;
                            self.status = "MCP observation-only listener active. No terminal access until native approval.";
                        }
                    }
                    Err(failure) => {
                        self.stop_mcp();
                        if self.enabled {
                            self.status = start_failure_status(failure);
                            tracing::warn!("{}", self.status);
                        }
                    }
                }
            }
            Some(Err(mpsc::TryRecvError::Empty)) => {
                context.request_repaint_after(Duration::from_millis(50));
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.mcp_starting = None;
                self.stop_mcp();
                if self.enabled {
                    self.status = "MCP unavailable: startup worker failed.";
                    tracing::warn!("{}", self.status);
                }
            }
            None => {}
        }

        if !cleanup_ready(&mut self.mcp_retiring, context, self.want_start) {
            if self.mcp_retiring.failed && self.want_start {
                self.stop_mcp();
                self.status = CLEANUP_FAILED;
            }
            return;
        }
        if self.want_start
            && self.broker.is_some()
            && self.server.is_none()
            && self.mcp_starting.is_none()
        {
            let guard = match festerm_session::try_automation_admission_lock() {
                Ok(guard) => guard,
                Err(festerm_session::AutomationAdmissionTryError::Busy) => {
                    self.reconciliation_deferred = true;
                    context.request_repaint_after(Duration::from_millis(50));
                    return;
                }
                Err(festerm_session::AutomationAdmissionTryError::Poisoned) => {
                    self.privacy_lost();
                    return;
                }
            };
            let bound = self
                .privacy_gate
                .bind_mcp_under_admission(&self.stop, &guard);
            drop(guard);
            if !bound {
                if self.privacy_gate.is_closed() {
                    self.privacy_lost();
                } else {
                    self.stop_mcp();
                    self.status = start_failure_status(StartFailure::Cancelled);
                }
                return;
            }
            let stop = self.stop.clone();
            let privacy_closed = self.privacy_gate.closed_flag();
            let notify = context.clone();
            let (send, result) = mpsc::sync_channel(1);
            match thread::Builder::new()
                .name("festerm-mcp-startup".into())
                .spawn(move || {
                    let started = (|| {
                        if stop.load(Ordering::Acquire) {
                            return Err(StartFailure::Cancelled);
                        }
                        let server_notify = notify.clone();
                        let server = ipc::Server::start(
                            Arc::new(move || server_notify.request_repaint()),
                            stop.clone(),
                            privacy_closed,
                        )
                        .map_err(|error| {
                            if error.kind() == std::io::ErrorKind::Unsupported {
                                StartFailure::PeerIdentity
                            } else {
                                StartFailure::Endpoint
                            }
                        })?;
                        if stop.load(Ordering::Acquire) {
                            return Err(StartFailure::Cancelled);
                        }
                        Ok(server)
                    })();
                    let _ = send.send(started);
                    notify.request_repaint();
                }) {
                Ok(thread) => {
                    self.mcp_starting = Some(Starting { result, thread });
                    context.request_repaint_after(Duration::from_millis(50));
                }
                Err(error) => {
                    tracing::warn!("MCP startup worker unavailable: {error}");
                    self.stop_mcp();
                    self.status = "MCP unavailable: startup worker failed.";
                }
            }
        }
    }

    fn advance_start(&mut self, context: &egui::Context) {
        if self.unwind_stopped || self.reconciliation_deferred {
            return;
        }
        self.advance_shared(context);
        if self.reconciliation_deferred {
            return;
        }
        if self.broker.is_some() && self.privacy_gate.is_closed() {
            self.privacy_lost();
            return;
        }
        self.advance_mcp(context);
    }

    pub(crate) fn tick(&mut self, context: &egui::Context, apps: &[&FesTermApp]) {
        let tick = CoordinatorUnwind { coordinator: self };
        if tick.coordinator.stop.load(Ordering::Acquire)
            || tick.coordinator.privacy_gate.is_closed()
        {
            tick.coordinator.verification.revoke();
        } else {
            tick.coordinator.verification.advance();
        }
        tick.coordinator.tick_inner(context, apps);
    }

    fn advance_workflow(&mut self, context: &egui::Context, apps: &[&FesTermApp]) {
        if self.workflow_unwind_stopped {
            if !self.unwind_cleanup_pending
                && !apps.is_empty()
                && apps.iter().any(|app| !app.automation_javascript_enabled())
            {
                self.workflow_unwind_disabled_seen = true;
            }
            return;
        }
        let javascript_enabled =
            !apps.is_empty() && apps.iter().all(|app| app.automation_javascript_enabled());
        if self.javascript_enabled && !javascript_enabled {
            self.workflow.cancel();
            self.cancel_javascript("Workflow stopped because JavaScript workflows were disabled.");
        }
        self.javascript_enabled = javascript_enabled;
        self.workflow.poll(context);
    }

    fn tick_inner(&mut self, context: &egui::Context, apps: &[&FesTermApp]) {
        self.privacy_lost_this_tick = false;
        self.reconciliation_deferred = false;
        if self.unwind_stopped {
            self.enabled = apps.first().is_some_and(|app| app.automation_mcp_enabled());
            if self.unwind_cleanup_pending {
                self.finish_unwind_cleanup();
            }
            self.advance_workflow(context, apps);
            return;
        }
        self.advance_workflow(context, apps);
        let enabled = apps.first().is_some_and(|app| app.automation_mcp_enabled());
        if enabled != self.enabled {
            self.enabled = enabled;
            if enabled {
                self.start(context);
            } else {
                self.stop_mcp();
                self.status = SettingsSnapshot::default().status;
            }
        }
        self.advance_start(context);
        if self.reconciliation_deferred {
            return;
        }
        if self.server.as_ref().is_some_and(ipc::Server::failed) {
            self.mcp_failed();
        }
        let Some(mut broker) = self.broker.take() else {
            return;
        };
        let _unwind = TakenBrokerUnwind {
            privacy: self.privacy_gate.clone(),
            stop: self.stop.clone(),
        };
        match self.try_reconcile(&mut broker, apps) {
            Ok(()) => {}
            Err(ReconcileError::Admission(festerm_session::AutomationAdmissionTryError::Busy)) => {
                self.reconciliation_deferred = true;
                context.request_repaint_after(Duration::from_millis(50));
                self.broker = Some(broker);
                return;
            }
            Err(ReconcileError::Admission(
                festerm_session::AutomationAdmissionTryError::Poisoned,
            )) => {
                self.privacy_lost_with_taken_broker(&mut broker);
                return;
            }
            Err(ReconcileError::Authority(error)) => {
                self.privacy_lost_with_taken_broker(&mut broker);
                tracing::error!(?error, "stale enrollment metadata review cleanup failed");
                return;
            }
        }
        #[cfg(test)]
        if let Some((entered, release)) = self.after_reconcile.take() {
            entered.send(()).expect("reconciliation test observer");
            release
                .recv_timeout(Duration::from_secs(10))
                .expect("reconciliation test release");
        }
        if self.privacy_gate.is_closed() {
            self.privacy_lost_with_taken_broker(&mut broker);
            return;
        }
        let now = Instant::now();
        for peer in self.peers.values_mut() {
            controller::expire_direct_input(peer, now);
            if let Some(input) = &peer.controller.direct_input {
                let until_expiry = input.next_deadline().saturating_duration_since(now);
                context.request_repaint_after(if until_expiry.is_zero() {
                    Duration::from_millis(50)
                } else {
                    until_expiry
                });
            }
        }
        let cancellation = self.peers.values_mut().try_for_each(|peer| {
            retry_completion(&mut broker, peer, now)?;
            retry_cancelled_waits(&mut broker, peer, now)
        });
        match cancellation {
            Ok(()) => {}
            Err(festerm_session::AutomationAdmissionTryError::Busy) => {
                self.reconciliation_deferred = true;
                #[cfg(test)]
                if let Some(marker) = &self.pending_cleanup_busy_repaint {
                    marker.store(true, Ordering::Release);
                }
                context.request_repaint_after(Duration::from_millis(50));
                self.broker = Some(broker);
                return;
            }
            Err(festerm_session::AutomationAdmissionTryError::Poisoned) => {
                self.privacy_lost_with_taken_broker(&mut broker);
                return;
            }
        }
        broker.poll_expiry(now);
        self.metadata_grants.retain(|principal, bound| {
            broker.native_metadata_grant(*principal, now) == Ok(bound.grant)
        });
        self.pending_metadata_reviews
            .retain(|id, review| broker.pending_approval_is_active(review.principal, *id, now));
        self.enrollment_metadata_grants
            .retain(|(principal, registration, enrollment), bound| {
                broker.native_enrollment_metadata_grant(*principal, *registration, *enrollment, now)
                    == Ok(bound.grant)
            });
        self.pending_enrollment_metadata_reviews
            .retain(|id, review| broker.pending_approval_is_active(review.principal, *id, now));
        if self.privacy_gate.is_closed() {
            self.privacy_lost_with_taken_broker(&mut broker);
            return;
        }
        let closed: Vec<_> = self
            .peers
            .iter()
            .filter_map(|(key, peer)| {
                (peer.connection.is_closed()
                    || peer.grant.as_ref().is_some_and(|grant| {
                        now >= grant.expires
                            || grant.targets.iter().any(|reference| {
                                self.targets
                                    .get(&reference.session())
                                    .is_none_or(|target| target.reference != *reference)
                            })
                    }))
                .then_some(*key)
            })
            .collect();
        for key in closed {
            let Some(guard) = festerm_session::automation_admission_lock() else {
                self.privacy_lost_with_taken_broker(&mut broker);
                return;
            };
            if let Some(mut peer) = self.peers.remove(&key) {
                let wake = peer.connection.close_under_admission(&guard);
                broker.disconnect(peer.principal, &guard);
                ipc::wake_after_admission(guard, [wake]);
                peer.machine.close();
                self.control_handoffs.remove(&peer.principal);
                self.pending_control_reviews
                    .retain(|_, review| review.principal != peer.principal);
                self.metadata_grants.remove(&peer.principal);
                self.pending_metadata_reviews
                    .retain(|_, review| review.principal != peer.principal);
                self.enrollment_metadata_grants
                    .retain(|(principal, _, _), _| *principal != peer.principal);
                self.pending_enrollment_metadata_reviews
                    .retain(|_, review| review.principal != peer.principal);
            }
        }
        for _ in 0..32 {
            let Some(event) = self.server.as_ref().and_then(ipc::Server::try_recv) else {
                break;
            };
            match event {
                ipc::Event::Connected {
                    connection,
                    identity,
                } => {
                    if connection.is_closed() || self.stop.load(Ordering::Acquire) {
                        connection.close();
                        continue;
                    }
                    let code = random::<8>().and_then(|bytes| {
                        let code = bytes
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<String>();
                        VerificationCode::new(&code).ok_or(Refusal::Unavailable)
                    });
                    match (broker.accept_connection(), code) {
                        (Ok(principal), Ok(code))
                            if !self.peers.values().any(|peer| peer.code == code) =>
                        {
                            let machine = self.machine_for_new_peer();
                            self.peers.insert(
                                connection.key,
                                Peer {
                                    connection,
                                    principal,
                                    machine,
                                    code,
                                    identity,
                                    grant: None,
                                    waits: Vec::new(),
                                    pending_completion: None,
                                    #[cfg(test)]
                                    after_finish: None,
                                    controller: controller::PeerState::default(),
                                },
                            );
                        }
                        (Ok(principal), _) => {
                            let Some(guard) = festerm_session::automation_admission_lock() else {
                                connection.close();
                                self.privacy_lost_with_taken_broker(&mut broker);
                                return;
                            };
                            broker.disconnect(principal, &guard);
                            drop(guard);
                            connection.close();
                            tracing::warn!("automation connection identity refused");
                        }
                        _ => connection.close(),
                    }
                }
                ipc::Event::Message { key, bytes } => {
                    let controller_qualified =
                        self.controller_qualified() && self.controller_opted_in;
                    let input_qualified = self.direct_input_qualified() && controller_qualified;
                    let metadata = EnrollmentMetadataContext {
                        grants: &self.enrollment_metadata_grants,
                        qualified: self.enrollment_metadata_delivery_qualified()
                            && controller_qualified,
                    };
                    let result = if let Some(peer) = self.peers.get_mut(&key) {
                        let result = process_peer_message(
                            &mut broker,
                            &self.targets,
                            metadata,
                            peer,
                            &bytes,
                            apps,
                            &self.privacy_gate,
                            &self.stop,
                            controller_qualified,
                            input_qualified,
                            now,
                        );
                        if matches!(
                            &result,
                            Err(festerm_session::AutomationAdmissionTryError::Busy)
                        ) && peer.controller.direct_input.is_some()
                        {
                            // The frame was consumed before it could be processed.
                            // It may be a cancellation, so no pending input can proceed.
                            peer.connection.close();
                        }
                        if peer.connection.finish_message().is_err() {
                            peer.connection.close();
                        }
                        result
                    } else {
                        Ok(())
                    };
                    match result {
                        Ok(()) => {}
                        Err(festerm_session::AutomationAdmissionTryError::Busy) => {
                            self.reconciliation_deferred = true;
                            #[cfg(test)]
                            if let Some(marker) = &self.peer_message_busy_repaint {
                                marker.store(true, Ordering::Release);
                            }
                            context.request_repaint_after(Duration::from_millis(50));
                            self.broker = Some(broker);
                            return;
                        }
                        Err(festerm_session::AutomationAdmissionTryError::Poisoned) => {
                            self.privacy_lost_with_taken_broker(&mut broker);
                            return;
                        }
                    }
                }
            }
        }
        let cleanup_now = Instant::now();
        if presentation::has_cancelled_direct_reviews(&self.pending_control_reviews, cleanup_now)
            || presentation::has_cancelled_direct_handoffs(&self.control_handoffs, cleanup_now)
        {
            let guard = match festerm_session::try_automation_admission_lock() {
                Ok(guard) => guard,
                Err(festerm_session::AutomationAdmissionTryError::Busy) => {
                    self.reconciliation_deferred = true;
                    context.request_repaint_after(Duration::from_millis(50));
                    self.broker = Some(broker);
                    return;
                }
                Err(festerm_session::AutomationAdmissionTryError::Poisoned) => {
                    self.privacy_lost_with_taken_broker(&mut broker);
                    return;
                }
            };
            let result = presentation::prune_cancelled_direct_reviews_locked(
                &mut self.pending_control_reviews,
                &mut broker,
                cleanup_now,
                &guard,
            );
            let direct_qualified = self.direct_input_qualified() && self.controller_opted_in;
            let handoff_retired = presentation::revoke_invalid_direct_handoffs_locked(
                &mut self.control_handoffs,
                &mut broker,
                direct_qualified,
                cleanup_now,
                &guard,
            );
            drop(guard);
            if let Err(error) = result {
                self.privacy_lost_with_taken_broker(&mut broker);
                tracing::error!(?error, "cancelled direct input review cleanup failed");
                return;
            }
            if handoff_retired {
                self.status = "Control handoff ended; a new review is required.";
            }
        }
        let waits = self.peers.values_mut().try_for_each(|peer| {
            settle_waits(&mut broker, &self.targets, peer, now)?;
            for wait in &peer.waits {
                context.request_repaint_after(wait.deadline.saturating_duration_since(now));
            }
            if let Some(grant) = &peer.grant {
                context.request_repaint_after(grant.expires.saturating_duration_since(now));
            }
            Ok(())
        });
        if let Err(error) = waits {
            match error {
                festerm_session::AutomationAdmissionTryError::Busy => {
                    self.reconciliation_deferred = true;
                    context.request_repaint_after(Duration::from_millis(50));
                    self.broker = Some(broker);
                }
                festerm_session::AutomationAdmissionTryError::Poisoned => {
                    self.privacy_lost_with_taken_broker(&mut broker);
                }
            }
            return;
        }
        if self.privacy_gate.is_closed() {
            self.privacy_lost_with_taken_broker(&mut broker);
            return;
        }
        self.broker = Some(broker);
    }

    fn try_reconcile(
        &mut self,
        broker: &mut Broker,
        apps: &[&FesTermApp],
    ) -> Result<(), ReconcileError> {
        let direct_qualified = self.direct_input_qualified() && self.controller_opted_in;
        let mut candidates = Vec::new();
        for (id, session) in apps.iter().flat_map(|app| app.automation_sessions()) {
            let Some(controller_instance) = session.controller.automation_instance() else {
                continue;
            };
            let liveness = session.controller.automation_liveness();
            let Some(attachment) = session.controller.automation_attachment_epoch() else {
                continue;
            };
            if !matches!(
                session.controller.lifecycle(),
                Some(festerm_session::SessionLifecycle::Running)
            ) || session.controller.observation_revision() == u64::MAX
                || session.controller.lifecycle_generation() == u64::MAX
                || !liveness.load(Ordering::Acquire)
            {
                continue;
            }
            let id = id.chip_id();
            candidates.push((id, session, controller_instance, liveness, attachment));
        }
        let guard =
            festerm_session::try_automation_admission_lock().map_err(ReconcileError::Admission)?;
        presentation::prune_stale_enrollment_metadata_reviews_locked(
            &mut self.pending_enrollment_metadata_reviews,
            broker,
            self.presentation_workspace
                .as_ref()
                .and_then(WorkspaceRevision::current),
            Instant::now(),
            &guard,
        )
        .map_err(ReconcileError::Authority)?;
        let candidates: Vec<_> = candidates
            .into_iter()
            .filter_map(|(id, session, instance, liveness, attachment)| {
                if !liveness.load(Ordering::Acquire) {
                    return None;
                }
                let generation = attachment.capture_under_admission(&guard)?;
                Some((id, session, instance, liveness, attachment, generation))
            })
            .collect();
        let current: BTreeSet<_> = candidates.iter().map(|(id, ..)| *id).collect();
        let mut retirement_refused = false;
        let stale: Vec<_> = self
            .targets
            .keys()
            .filter(|id| !current.contains(id))
            .copied()
            .collect();
        for id in stale {
            if let Some(target) = self.targets.remove(&id) {
                retirement_refused |= !retire_target(broker, target, &guard);
            }
        }

        const REGISTRATION_REFUSED: &str =
            "MCP could not register all terminals; some cannot be shared.";
        let mut registration_refused = false;
        for (id, session, controller_instance, liveness, attachment, attachment_generation) in
            candidates
        {
            let stamp = stamp(session);
            let generation = session.controller.lifecycle_generation();
            let local_pty = controller::is_local_backend(session);
            let unchanged = self.targets.get(&id).is_some_and(|target| {
                target.controller_instance == controller_instance
                    && target.controller_generation == generation
                    && target.local_pty == local_pty
                    && Arc::ptr_eq(&target.liveness, &liveness)
                    && Arc::ptr_eq(&target.attachment, &attachment)
                    && target.attachment_generation == attachment_generation
            });
            if unchanged {
                let target = self.targets.get_mut(&id).expect("target was checked");
                if target.stamp != stamp {
                    if let Some(revision) = target.revision.checked_add(1) {
                        target.revision = revision;
                        target.stamp = stamp;
                    } else {
                        let target = self.targets.remove(&id).expect("target was checked");
                        retirement_refused |= !retire_target(broker, target, &guard);
                    }
                }
            } else {
                if let Some(target) = self.targets.remove(&id) {
                    retirement_refused |= !retire_target(broker, target, &guard);
                }
                let Some(next) = self.generation.checked_add(1) else {
                    drop(guard);
                    self.privacy_gate.close();
                    tracing::warn!("automation target generations exhausted");
                    return Ok(());
                };
                self.generation = next;
                match broker.attach_target(id, next, &guard) {
                    Ok(reference) => {
                        self.targets.insert(
                            id,
                            Target {
                                reference,
                                controller_instance,
                                controller_generation: generation,
                                local_pty,
                                liveness,
                                registration: Arc::new(AtomicBool::new(true)),
                                attachment,
                                attachment_generation,
                                stamp,
                                revision: 1,
                            },
                        );
                    }
                    Err(_) => {
                        registration_refused = true;
                    }
                }
            }
        }
        controller::reconcile(
            broker,
            &self.targets,
            &mut self.peers,
            apps,
            self.presentation_workspace
                .as_ref()
                .and_then(WorkspaceRevision::current),
            &guard,
        );
        presentation::prune_cancelled_direct_reviews_locked(
            &mut self.pending_control_reviews,
            broker,
            Instant::now(),
            &guard,
        )
        .map_err(ReconcileError::Authority)?;
        let handoff_retired = presentation::revoke_invalid_direct_handoffs_locked(
            &mut self.control_handoffs,
            broker,
            direct_qualified,
            Instant::now(),
            &guard,
        );
        let metadata_retired = controller::retire_metadata_actions_locked(
            &mut self.peers,
            &mut self.pending_enrollment_metadata_reviews,
            &mut self.enrollment_metadata_grants,
            broker,
            Instant::now(),
            &guard,
        )
        .map_err(ReconcileError::Authority)?;
        drop(guard);
        if metadata_retired {
            self.status = "Automation cancellation or revocation completed.";
        } else if handoff_retired {
            self.status = "Control handoff ended; a new review is required.";
        }
        if retirement_refused {
            tracing::warn!("automation target retirement refused");
        }
        if registration_refused && self.status != REGISTRATION_REFUSED {
            tracing::warn!("automation target registration refused");
        }
        if self.server.is_some() && !self.stop.load(Ordering::Acquire) {
            if registration_refused {
                self.status = REGISTRATION_REFUSED;
            } else if self.status == REGISTRATION_REFUSED {
                self.status =
                    "MCP observation-only listener active. No terminal access until native approval.";
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn reconcile(&mut self, broker: &mut Broker, apps: &[&FesTermApp]) {
        retry_automation_busy(|| {
            self.try_reconcile(broker, apps)
                .map_err(|error| match error {
                    ReconcileError::Admission(
                        festerm_session::AutomationAdmissionTryError::Busy,
                    ) => festerm_automation::Error::Busy,
                    ReconcileError::Admission(
                        festerm_session::AutomationAdmissionTryError::Poisoned,
                    ) => festerm_automation::Error::Unavailable,
                    ReconcileError::Authority(error) => error,
                })
        })
        .unwrap();
    }

    pub(crate) fn command(&mut self, command: NativeCommand, context: &egui::Context) {
        let guard = CoordinatorUnwind { coordinator: self };
        guard.coordinator.command_inner(command, context);
    }

    fn command_inner(&mut self, command: NativeCommand, context: &egui::Context) {
        if self.unwind_cleanup_pending {
            self.finish_unwind_cleanup();
        }
        if self.privacy_gate.is_closed() && (self.broker.is_some() || self.starting.is_some()) {
            self.privacy_lost();
        }
        match command {
            NativeCommand::SafetyStop => self.command_overflow(),
            NativeCommand::PreferenceChanged { clear_workflow, .. } => {
                if clear_workflow {
                    self.workflow.cancel();
                }
            }
            NativeCommand::RestartMcp if self.enabled && !self.privacy_lost_this_tick => {
                self.unwind_stopped = false;
                self.start(context);
            }
            NativeCommand::RestartMcp => {}
            NativeCommand::Revoke { channel } => {
                if let Some(peer) = self.peers.get(&channel) {
                    peer.connection.close();
                }
            }
            NativeCommand::ApproveObservation { .. } if self.privacy_lost_this_tick => {}
            NativeCommand::ApproveObservation { .. } if self.reconciliation_deferred => {
                self.status =
                    "MCP approval refused while automation admission or safety work is pending. Try again.";
                context.request_repaint();
            }
            NativeCommand::ApproveObservation { channel, targets } => {
                if self.approve(channel, targets).is_err() {
                    self.status = "MCP approval refused: channel, target, lifetime or authority limit changed. No access was granted.";
                    tracing::warn!("automation native observation approval refused");
                }
            }
            NativeCommand::CaptureWorkflow(SelectedWorkflow(path)) => {
                if self.javascript_enabled {
                    self.workflow.capture(path, context);
                } else if self.workflow_unwind_stopped {
                    self.workflow.refuse(WORKFLOW_UNWIND_STOPPED);
                } else {
                    tracing::warn!("workflow selection refused while JavaScript is disabled");
                    self.workflow
                        .refuse("Enable JavaScript workflows in all windows before selecting.");
                }
            }
            NativeCommand::ClearWorkflow => self.workflow.cancel(),
            NativeCommand::StartWorkflowMonitoring
            | NativeCommand::StartWorkflowRun { .. }
            | NativeCommand::CancelWorkflowRun { .. }
            | NativeCommand::ApproveWorkflowInput { .. }
            | NativeCommand::DenyWorkflowInput { .. } => {
                self.javascript_command(command, context, &[]);
            }
            NativeCommand::EnableControllerEnrollment
            | NativeCommand::ChooseController { .. }
            | NativeCommand::SelectControllerTarget { .. }
            | NativeCommand::ConfirmController { .. }
            | NativeCommand::CancelController { .. }
            | NativeCommand::UnregisterController { .. }
            | NativeCommand::ReviewControllerMetadata { .. }
            | NativeCommand::ConfirmControllerMetadata { .. }
            | NativeCommand::CancelControllerMetadata { .. }
            | NativeCommand::RevokeControllerMetadata { .. }
            | NativeCommand::ReviewControllerInput { .. }
            | NativeCommand::ConfirmControllerInput { .. }
            | NativeCommand::CancelControllerInput { .. }
            | NativeCommand::DisarmControllerInput { .. } => {
                self.status = "Controller action refused without its originating native window.";
            }
            NativeCommand::CompareDirectPeer { .. } | NativeCommand::DenyDirectPeer { .. } => {
                self.status =
                    "Direct MCP comparison refused without its originating native window.";
            }
        }
        context.request_repaint();
    }

    pub(crate) fn command_overflow(&mut self) {
        let guard = CoordinatorUnwind { coordinator: self };
        guard.coordinator.command_overflow_inner();
    }

    fn command_overflow_inner(&mut self) {
        self.workflow_unwind_disabled_seen = false;
        self.stop_mcp();
        self.workflow.cancel();
        self.cancel_javascript("Workflow stopped after conflicting or excessive actions.");
        self.status = "Automation actions overflowed or conflicted; channels and pending selections were closed and workflow previews cancelled. Settings changes may not have been applied; check toggles or reset and restart MCP explicitly.";
    }

    pub(crate) fn command_overflow_for_apps(&mut self, apps: &[&FesTermApp]) {
        let guard = CoordinatorUnwind { coordinator: self };
        guard.coordinator.command_overflow_for_apps_inner(apps);
    }

    fn command_overflow_for_apps_inner(&mut self, apps: &[&FesTermApp]) {
        self.enabled = apps.first().is_some_and(|app| app.automation_mcp_enabled());
        self.command_overflow();
    }

    pub(crate) fn close_overflow_admission(&mut self) {
        let guard = CoordinatorUnwind { coordinator: self };
        guard.coordinator.close_overflow_admission_inner();
    }

    fn close_overflow_admission_inner(&self) {
        set_admission_flag(&self.stop, true);
    }

    pub(crate) fn close_conflicting_admission(&mut self) {
        let guard = CoordinatorUnwind { coordinator: self };
        guard.coordinator.close_conflicting_admission_inner();
    }

    fn close_conflicting_admission_inner(&mut self) {
        self.workflow_unwind_disabled_seen = false;
        self.close_overflow_admission_inner();
        self.verification.revoke();
        self.workflow.cancel();
        self.cancel_javascript("Workflow stopped after conflicting or excessive actions.");
    }

    #[cfg(test)]
    pub(crate) fn pending_workflow_capture_for_test(
        &mut self,
    ) -> mpsc::SyncSender<Result<festerm_automation_js::snapshot::ApprovedBundle, String>> {
        self.workflow.pending_capture_for_test()
    }

    pub(crate) fn commands(
        &mut self,
        commands: Vec<NativeWindowCommand>,
        context: &egui::Context,
        apps: &[&FesTermApp],
    ) -> bool {
        let guard = CoordinatorUnwind { coordinator: self };
        guard.coordinator.commands_inner(commands, context, apps)
    }

    fn commands_inner(
        &mut self,
        mut commands: Vec<NativeWindowCommand>,
        context: &egui::Context,
        apps: &[&FesTermApp],
    ) -> bool {
        if NativeCommand::batch_requires_safety_stop(commands.iter().map(|item| &item.command)) {
            self.command_overflow_for_apps_inner(apps);
            return true;
        }
        let cancellation = commands.iter().any(|item| item.command.is_cancellation());
        let workflow_clear = commands
            .iter()
            .any(|item| matches!(item.command, NativeCommand::ClearWorkflow));
        let suppressed = commands.iter().any(|item| {
            let command = &item.command;
            (cancellation && command.is_confirmation())
                || (workflow_clear && matches!(command, NativeCommand::CaptureWorkflow(_)))
        });
        if suppressed {
            controller::cancel_selections(&mut self.peers);
        }
        commands.sort_by_key(|item| {
            let command = &item.command;
            if command.is_confirmation() {
                2
            } else if command.is_cancellation() {
                1
            } else {
                0
            }
        });
        for NativeWindowCommand { window, command } in commands {
            if (cancellation && command.is_confirmation())
                || (workflow_clear && matches!(&command, NativeCommand::CaptureWorkflow(_)))
            {
                continue;
            }
            if self.workflow_unwind_stopped
                && matches!(
                    &command,
                    NativeCommand::PreferenceChanged {
                        reenable_workflow: true,
                        ..
                    }
                )
            {
                if self.unwind_cleanup_pending {
                    self.finish_unwind_cleanup();
                }
                if self.workflow_unwind_disabled_seen
                    && !apps.is_empty()
                    && apps.iter().all(|app| app.automation_javascript_enabled())
                {
                    self.workflow.cancel();
                    self.workflow_unwind_stopped = false;
                    self.workflow_unwind_disabled_seen = false;
                    self.javascript_enabled = true;
                }
            }
            if command.is_verification() {
                let result = match command {
                    NativeCommand::CompareDirectPeer {
                        generation,
                        fingerprint,
                    } => self
                        .verification
                        .decide(window, generation, Some(&fingerprint)),
                    NativeCommand::DenyDirectPeer { generation } => {
                        self.verification.decide(window, generation, None)
                    }
                    _ => unreachable!(),
                };
                self.status = match result {
                    Ok(()) => "Direct MCP comparison submitted; no terminal access was granted.",
                    Err(verification::Error::Busy) => {
                        "Direct MCP comparison is busy. Review and retry explicitly."
                    }
                    Err(verification::Error::Unavailable) => {
                        "Direct MCP comparison refused or no longer available."
                    }
                };
                context.request_repaint();
            } else if command.is_controller() {
                if self.privacy_lost_this_tick {
                    continue;
                }
                if self.reconciliation_deferred && command.is_confirmation() {
                    self.status = controller::RECONCILIATION_PENDING_STATUS;
                    context.request_repaint();
                    continue;
                }
                match controller::command(self, window, command, apps) {
                    Ok(())
                        if matches!(
                            self.status,
                            controller::BUSY_STATUS | controller::RECONCILIATION_PENDING_STATUS
                        ) =>
                    {
                        self.status = controller::ACCEPTED_STATUS;
                    }
                    Ok(()) => {}
                    Err(controller::CommandError::Busy) => self.status = controller::BUSY_STATUS,
                    Err(controller::CommandError::Refused(reason)) => {
                        tracing::warn!(?reason, "automation controller action refused");
                        self.status = "Controller selection refused: the connection, registration, window or local terminal changed. No association was approved.";
                    }
                }
                context.request_repaint();
            } else if command.is_javascript() {
                let guard = CoordinatorUnwind { coordinator: self };
                guard.coordinator.javascript_command(command, context, apps);
            } else {
                self.command(command, context);
            }
        }
        if suppressed && !self.privacy_lost_this_tick && !self.privacy_gate.is_closed() {
            self.status = "Automation cancellation blocked a same-frame fingerprint comparison, workflow capture, controller selection or grant approval. Review and retry the blocked action explicitly when available.";
            context.request_repaint();
        }
        suppressed
    }

    #[cfg(test)]
    pub(crate) fn controller_reconciliation_deferred_for_test(&self) -> bool {
        self.reconciliation_deferred
    }

    #[cfg(test)]
    pub(crate) fn controller_action_retryable_for_test(&self) -> bool {
        matches!(
            self.status,
            controller::BUSY_STATUS | controller::RECONCILIATION_PENDING_STATUS
        )
    }

    #[cfg(test)]
    fn commands_for_test(
        &mut self,
        commands: Vec<NativeCommand>,
        context: &egui::Context,
        apps: &[&FesTermApp],
    ) -> bool {
        self.commands(
            commands
                .into_iter()
                .map(|command| NativeWindowCommand {
                    window: WindowId::PRIMARY,
                    command,
                })
                .collect(),
            context,
            apps,
        )
    }

    pub(crate) fn controller_target_moving(&mut self, tab: TabId) {
        let guard = CoordinatorUnwind { coordinator: self };
        guard.coordinator.controller_target_moving_inner(tab);
    }

    pub(crate) fn direct_input_preparations(&self) -> Vec<DirectInputPreparation> {
        controller::direct_input_preparations(self)
    }

    pub(crate) fn direct_input_dispatches(&self) -> Vec<DirectInputPreparation> {
        controller::direct_input_dispatches(self)
    }

    pub(crate) fn stage_reusable_direct_inputs(
        &mut self,
        apps: &[&FesTermApp],
        context: &egui::Context,
    ) {
        let guard = CoordinatorUnwind { coordinator: self };
        match controller::stage_reusable_direct_inputs(guard.coordinator, apps) {
            Ok(true) => context.request_repaint(),
            Ok(false) => {}
            Err(festerm_automation::Error::Busy) => {
                context.request_repaint_after(Duration::from_millis(50));
            }
            Err(error) => {
                tracing::warn!(?error, "reusable control handoff could not be reconciled");
                guard.coordinator.stop_mcp();
                guard.coordinator.status = "MCP stopped because control could not be reconciled.";
                context.request_repaint();
            }
        }
    }

    pub(crate) fn dispatch_direct_input(
        &mut self,
        preparation: DirectInputPreparation,
        tab: &SessionTab,
        apps: &[&FesTermApp],
        context: &egui::Context,
    ) {
        let guard = CoordinatorUnwind { coordinator: self };
        match controller::dispatch_direct_input(guard.coordinator, preparation, tab, apps) {
            Ok(()) => context.request_repaint(),
            Err(festerm_automation::Error::Busy) => {
                context.request_repaint_after(Duration::from_millis(50));
            }
            Err(error) => {
                tracing::warn!(?error, "approved direct input could not be dispatched");
                controller::refuse_direct_input_preparation(
                    guard.coordinator,
                    preparation,
                    refusal(error),
                );
                guard.coordinator.status =
                    "Direct input could not be dispatched; no input was admitted.";
                context.request_repaint();
            }
        }
    }

    pub(crate) fn settle_direct_inputs(&mut self, apps: &[&FesTermApp], context: &egui::Context) {
        let guard = CoordinatorUnwind { coordinator: self };
        for (channel, token) in controller::direct_input_settlements(guard.coordinator) {
            match controller::settle_direct_input(guard.coordinator, channel, token, apps) {
                Ok(true) => context.request_repaint(),
                Ok(false) => {}
                Err(festerm_automation::Error::Busy) => {
                    context.request_repaint_after(Duration::from_millis(50));
                }
                Err(error) => {
                    tracing::warn!(?error, "direct input settlement failed; closing the peer");
                    if let Some(peer) = guard.coordinator.peers.get(&channel) {
                        peer.connection.close();
                    }
                    guard.coordinator.status =
                        "Input settlement failed; its outcome may be unknown.";
                    context.request_repaint();
                }
            }
        }
    }

    pub(crate) fn prepare_direct_input_review(
        &mut self,
        preparation: DirectInputPreparation,
        tab: &mut SessionTab,
        context: &egui::Context,
    ) {
        let guard = CoordinatorUnwind { coordinator: self };
        match controller::prepare_direct_input_review(guard.coordinator, preparation, tab) {
            Ok(()) => context.request_repaint(),
            Err(festerm_automation::Error::Busy) => {
                context.request_repaint_after(Duration::from_millis(50));
            }
            Err(error) => {
                tracing::warn!(?error, "direct input review preparation refused");
                controller::refuse_direct_input_preparation(
                    guard.coordinator,
                    preparation,
                    refusal(error),
                );
                guard.coordinator.status =
                    "Direct input could not be prepared; no input was admitted.";
                context.request_repaint();
            }
        }
    }

    pub(crate) fn missing_direct_input_target(
        &mut self,
        preparation: DirectInputPreparation,
        context: &egui::Context,
    ) {
        let guard = CoordinatorUnwind { coordinator: self };
        controller::refuse_direct_input_preparation(
            guard.coordinator,
            preparation,
            Refusal::StaleTarget,
        );
        guard.coordinator.status = "The selected terminal changed; no input was admitted.";
        context.request_repaint();
    }

    fn controller_target_moving_inner(&mut self, tab: TabId) {
        if !controller::invalidate_location(self.broker.as_mut(), &mut self.peers, Some(tab), None)
        {
            self.close_overflow_admission_inner();
        }
    }

    pub(crate) fn controller_window_closing(&mut self, window: WindowId) {
        let guard = CoordinatorUnwind { coordinator: self };
        guard.coordinator.controller_window_closing_inner(window);
    }

    fn controller_window_closing_inner(&mut self, window: WindowId) {
        if !controller::invalidate_location(
            self.broker.as_mut(),
            &mut self.peers,
            None,
            Some(window),
        ) {
            self.close_overflow_admission_inner();
        }
    }

    fn approve(&mut self, channel: u64, targets: Vec<TargetRef>) -> Result<(), Refusal> {
        let guard =
            festerm_session::try_automation_admission_lock().map_err(|_| Refusal::Unavailable)?;
        if self.stop.load(Ordering::Acquire)
            || self.privacy_gate.is_closed()
            || targets.is_empty()
            || targets.len() > 32
        {
            return Err(Refusal::NotAuthorized);
        }
        let now = Instant::now();
        presentation::prune_stale_enrollment_metadata_reviews_locked(
            &mut self.pending_enrollment_metadata_reviews,
            self.broker.as_mut().ok_or(Refusal::Unavailable)?,
            self.presentation_workspace
                .as_ref()
                .and_then(WorkspaceRevision::current),
            now,
            &guard,
        )
        .map_err(refusal)?;
        let peer = self.peers.get_mut(&channel).ok_or(Refusal::Closed)?;
        if peer.connection.is_closed() || peer.grant.is_some() {
            return Err(Refusal::Closed);
        }
        if self
            .broker
            .as_ref()
            .is_some_and(|broker| broker.native_metadata_grant(peer.principal, now).is_ok())
        {
            return Err(Refusal::NotAuthorized);
        }
        let targets: BTreeSet<_> = targets.into_iter().collect();
        if targets.iter().any(|reference| {
            self.targets.get(&reference.session()).is_none_or(|target| {
                target.reference != *reference
                    || !target.liveness.load(Ordering::Acquire)
                    || !target.registration.load(Ordering::Acquire)
                    || target.attachment.capture_under_admission(&guard)
                        != Some(target.attachment_generation)
            })
        }) {
            return Err(Refusal::StaleTarget);
        }
        let target_liveness = targets
            .iter()
            .map(|reference| self.targets[&reference.session()].guard())
            .collect();
        let scope = Scope {
            discover: targets.clone(),
            observe: targets.clone(),
            ..Scope::default()
        };
        let request = GrantRequest {
            scope,
            bindings: BTreeSet::new(),
            direct_read_calls: BTreeSet::from([DirectKind::Discovery, DirectKind::Observation]),
        };
        let broker = self.broker.as_mut().ok_or(Refusal::Unavailable)?;
        let id = broker
            .request_approval(peer.principal, request.clone(), now)
            .map_err(refusal)?;
        broker
            .approve(
                peer.principal,
                id,
                NativeApproval::new(request, Duration::from_secs(300)),
                now,
                &guard,
            )
            .map_err(refusal)?;
        peer.grant = Some(Grant {
            targets,
            target_liveness,
            expires: now + Duration::from_secs(300),
        });
        Ok(())
    }

    pub(crate) fn settings(&self, apps: &[&FesTermApp]) -> SettingsSnapshot {
        let privacy_closed = self.privacy_gate.is_closed();
        let mcp_stopped = self.stop.load(Ordering::Acquire);
        let mcp_open = !mcp_stopped && !privacy_closed;
        let status = if self.verification.quarantined() && self.enabled {
            "Direct MCP comparison is unavailable after a cleanup failure. Restart the application."
        } else if self.unwind_stopped && self.enabled {
            MCP_UNWIND_STOPPED
        } else if self.workflow_unwind_stopped && !self.enabled {
            "MCP is disabled. Workflow selection stopped after an internal error; turn JavaScript workflows off and on to resume."
        } else if privacy_closed && self.broker.is_some() && self.enabled {
            "Automation is stopping after privacy loss or an internal safety limit; admission is closed."
        } else if mcp_stopped && self.server.is_some() {
            "MCP is stopping; no further access is available."
        } else {
            self.status
        };
        let channels = self
            .peers
            .iter()
            .filter(|(_, peer)| mcp_open && !peer.connection.is_closed())
            .map(|(key, peer)| ChannelRow {
                channel: *key,
                verification_code: peer.code.as_str().to_owned(),
                identity_fingerprint: peer
                    .identity
                    .as_ref()
                    .map(festerm_automation_mcp::identity::VerifiedClientIdentity::fingerprint),
                approved: peer.grant.is_some(),
                approved_targets: peer
                    .grant
                    .as_ref()
                    .map(|grant| grant.targets.iter().copied().collect())
                    .unwrap_or_default(),
                closed: peer.connection.closed_flag(),
            })
            .collect();
        let targets = self
            .targets
            .values()
            .filter(|_| mcp_open && (self.server.is_some() || !self.peers.is_empty()))
            .filter_map(|target| Some((target.reference, target_label(apps, target.reference)?)))
            .collect();
        let (controllers, controller_targets, controller_targets_incomplete) =
            controller::settings(self, apps, mcp_open);
        let mut workflow = self.workflow.view();
        if self.workflow_unwind_stopped {
            workflow.status = WorkflowStatus::Failed(WORKFLOW_UNWIND_STOPPED.into());
        }
        SettingsSnapshot {
            status,
            channels,
            direct_verification: self.verification.view(),
            targets,
            controllers,
            controller_targets,
            controller_targets_incomplete,
            controller_opt_in_available: mcp_open
                && self.broker.is_some()
                && self.controller_qualified(),
            controller_opted_in: mcp_open && self.controller_opted_in,
            workflow,
            javascript: self.javascript_view(apps),
            stop: self.stop.clone(),
        }
    }
}

fn target_label(apps: &[&FesTermApp], reference: TargetRef) -> Option<String> {
    apps.iter().enumerate().find_map(|(window, app)| {
        let session = app.automation_session(TabId::from_chip_id(reference.session()))?;
        let name = approval_label(&session.label);
        let detail = match &session.inspector_transport {
            crate::tabs::InspectorTransport::Local { .. } => "local shell".to_owned(),
            crate::tabs::InspectorTransport::Ssh {
                username,
                host,
                port,
                ..
            }
            | crate::tabs::InspectorTransport::Sftp {
                username,
                host,
                port,
            } => format!(
                "{}@{}:{}",
                approval_label(username),
                approval_label(host),
                port,
            ),
            crate::tabs::InspectorTransport::Serial { device, .. } => {
                format!("serial {}", approval_label(device))
            }
            crate::tabs::InspectorTransport::DevTunnel { host, port, .. } => {
                format!("tunnel {}:{}", approval_label(host), port)
            }
        };
        Some(format!(
            "Window {}: {} - {} (tab {}, attachment {})",
            window + 1,
            name,
            detail,
            reference.session(),
            reference.generation(),
        ))
    })
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn refusal(error: festerm_automation::Error) -> Refusal {
    use festerm_automation::Error;
    match error {
        Error::StaleTarget => Refusal::StaleTarget,
        Error::QuotaExceeded => Refusal::BudgetExceeded,
        Error::InvalidRequest => Refusal::InvalidArguments,
        Error::Closed => Refusal::Closed,
        _ => Refusal::NotAuthorized,
    }
}

fn send(peer: &mut Peer, response: Response, now: Instant, protect_targets: bool) {
    let expires = peer
        .grant
        .as_ref()
        .map_or(now + Duration::from_secs(2), |grant| grant.expires);
    let targets = if protect_targets {
        let Some(grant) = peer.grant.as_ref() else {
            peer.connection.close();
            tracing::warn!("automation disclosure lost its native grant before transport");
            return;
        };
        grant.target_liveness.as_slice()
    } else {
        &[]
    };
    if peer
        .connection
        .send(response.into_bytes(), expires, targets)
        .is_err()
    {
        peer.connection.close();
    }
}

fn finish(peer: &mut Peer, key: &RequestKey, result: BackendResult, now: Instant) {
    let protect_targets = matches!(&result, BackendResult::AuthorizedObservation(_));
    if let Some(response) = peer.machine.complete(key, result) {
        send(peer, response, now, protect_targets);
        #[cfg(test)]
        if let Some((entered, release)) = peer.after_finish.take() {
            entered.send(()).expect("finish test observer");
            release
                .recv_timeout(Duration::from_secs(10))
                .expect("finish test release");
        }
    }
}

fn retain_completion(peer: &mut Peer, run: RunId, request: Option<RequestId>) {
    assert!(
        peer.pending_completion.is_none(),
        "completion already pending"
    );
    peer.pending_completion = Some(PendingCompletion { run, request });
}

fn retry_completion(
    broker: &mut Broker,
    peer: &mut Peer,
    now: Instant,
) -> Result<(), festerm_session::AutomationAdmissionTryError> {
    let Some(pending) = peer.pending_completion else {
        return Ok(());
    };
    let guard = festerm_session::try_automation_admission_lock()?;
    let settlement = match pending.request {
        None => Ok(()),
        Some(request) => match broker.settle_request(peer.principal, request) {
            Ok(()) | Err(festerm_automation::Error::Closed) => Ok(()),
            Err(error) => Err(error),
        },
    };
    let completion = settlement
        .is_ok()
        .then(|| broker.complete(peer.principal, pending.run, Completion::Returned, now));
    let forget = settlement.and_then(|()| broker.forget_run(peer.principal, pending.run));
    let wake = if forget.is_err() {
        let wake = peer.connection.close_under_admission(&guard);
        broker.disconnect(peer.principal, &guard);
        Some(wake)
    } else {
        None
    };
    ipc::wake_after_admission(guard, wake);
    if let Some(result) = completion {
        match result {
            Ok(status) if status.state != festerm_automation::RunState::Completed => {
                tracing::warn!(?status, "automation completion did not succeed");
            }
            Ok(_) => {}
            Err(error) => match error {
                festerm_automation::Error::Expired
                | festerm_automation::Error::Revoked
                | festerm_automation::Error::Closed
                | festerm_automation::Error::StaleTarget => {
                    tracing::debug!(
                        ?error,
                        "automation completion run already closed or invalid"
                    );
                }
                _ => tracing::warn!(?error, "automation completion failed"),
            },
        }
    }
    if let Err(error) = forget {
        tracing::warn!(?error, "automation completion cleanup failed; closing peer");
        peer.machine.close();
        peer.waits.clear();
        peer.controller.direct_input = None;
    }
    peer.pending_completion = None;
    Ok(())
}

fn retry_cancelled_waits(
    broker: &mut Broker,
    peer: &mut Peer,
    now: Instant,
) -> Result<(), festerm_session::AutomationAdmissionTryError> {
    if !peer.waits.iter().any(|wait| wait.cancel_pending) {
        return Ok(());
    }
    let mut cancellation_errors = Vec::with_capacity(peer.waits.len());
    let mut cleanup_error = None;
    let mut wake = None;
    let guard = festerm_session::try_automation_admission_lock()?;
    while let Some(index) = peer.waits.iter().position(|wait| wait.cancel_pending) {
        let wait = peer.waits.remove(index);
        if let Err(error) = broker.cancel_run(peer.principal, wait.run, now, &guard) {
            cancellation_errors.push(error);
        }
        if let Err(error) = broker.forget_run(peer.principal, wait.run) {
            wake = Some(peer.connection.close_under_admission(&guard));
            broker.disconnect(peer.principal, &guard);
            cleanup_error = Some(error);
            break;
        }
    }
    ipc::wake_after_admission(guard, wake);
    for error in cancellation_errors {
        match error {
            festerm_automation::Error::Expired
            | festerm_automation::Error::Revoked
            | festerm_automation::Error::Closed
            | festerm_automation::Error::StaleTarget => {
                tracing::debug!(?error, "automation wait was already closed");
            }
            _ => tracing::warn!(?error, "automation wait cancellation failed"),
        }
    }
    if let Some(error) = cleanup_error {
        tracing::warn!(
            ?error,
            "automation cancelled wait cleanup failed; closing peer"
        );
        peer.machine.close();
        peer.waits.clear();
        peer.controller.direct_input = None;
    }
    Ok(())
}

fn cancel_wait(
    broker: &mut Broker,
    peer: &mut Peer,
    key: &RequestKey,
    now: Instant,
) -> Result<(), festerm_session::AutomationAdmissionTryError> {
    if let Some(wait) = peer.waits.iter_mut().find(|wait| wait.key == *key) {
        wait.cancel_pending = true;
        retry_cancelled_waits(broker, peer, now)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct EnrollmentMetadataContext<'a> {
    grants: &'a BTreeMap<
        presentation::EnrollmentMetadataKey,
        presentation::BoundEnrollmentMetadataGrant,
    >,
    qualified: bool,
}

#[allow(clippy::too_many_arguments)]
fn process_peer_message(
    broker: &mut Broker,
    targets: &BTreeMap<u64, Target>,
    metadata: EnrollmentMetadataContext<'_>,
    peer: &mut Peer,
    bytes: &[u8],
    apps: &[&FesTermApp],
    privacy: &PrivacyGate,
    stop: &AtomicBool,
    controller_qualified: bool,
    input_qualified: bool,
    now: Instant,
) -> Result<(), festerm_session::AutomationAdmissionTryError> {
    retry_completion(broker, peer, now)?;
    if peer.connection.is_closed() || stop.load(Ordering::Acquire) || privacy.is_closed() {
        return Ok(());
    }
    match peer.machine.process_bytes(bytes) {
        Event::Reply(response) => send(peer, response, now, false),
        Event::Backend(request) if controller::is_operation(&request.operation) => {
            controller::execute(
                broker,
                targets,
                peer,
                request,
                apps,
                privacy,
                stop,
                controller_qualified,
                metadata.qualified,
                input_qualified,
                now,
            );
        }
        Event::Backend(request)
            if matches!(
                &request.operation,
                Operation::TerminalSendText { .. } | Operation::TerminalSendKeys { .. }
            ) =>
        {
            controller::enqueue_direct_input(
                broker,
                targets,
                peer,
                request,
                apps,
                privacy,
                stop,
                input_qualified,
                now,
            );
        }
        Event::Backend(request)
            if metadata.qualified
                && peer.grant.is_none()
                && matches!(request.operation, Operation::SessionsGet { .. }) =>
        {
            controller::execute_metadata(
                broker, targets, metadata, peer, request, apps, privacy, stop,
            );
        }
        Event::Backend(request) => execute(broker, targets, peer, request, apps, now),
        Event::Cancelled(key) => {
            if !controller::cancel_direct_input(peer, &key) {
                return cancel_wait(broker, peer, &key, now);
            }
        }
        Event::NoReply => {}
    }
    retry_completion(broker, peer, Instant::now())
}

fn execute(
    broker: &mut Broker,
    targets: &BTreeMap<u64, Target>,
    peer: &mut Peer,
    request: BackendRequest,
    apps: &[&FesTermApp],
    now: Instant,
) {
    if matches!(request.operation, Operation::PairingStatus) {
        finish(
            peer,
            &request.key,
            BackendResult::PairingStatus(PairingStatus {
                paired: peer.grant.is_some(),
                observation_enabled: true,
                verification_code: peer.code,
            }),
            now,
        );
        return;
    }
    let result = admit_request(broker, targets, peer, &request.operation, now);
    let (run, ticket, reference) = match result {
        Ok(context) => context,
        Err(error) => {
            finish(peer, &request.key, BackendResult::Refused(error), now);
            return;
        }
    };
    if let Operation::TerminalWaitForChange {
        after_revision,
        timeout_ms,
        ..
    } = request.operation
    {
        let target = reference.expect("observation has a target");
        peer.waits.push(Wait {
            key: request.key,
            run,
            request: ticket,
            target,
            after_revision,
            deadline: now + Duration::from_millis(u64::from(timeout_ms)),
            cancel_pending: false,
        });
        return;
    }
    retain_completion(peer, run, Some(ticket));
    let data = snapshot(
        broker,
        targets,
        peer,
        ticket,
        &request.operation,
        reference,
        apps,
        now,
    );
    let result = match data.and_then(|data| {
        let bytes = serde_json::to_vec(&data).map_err(|_| Refusal::InvalidBackendResult)?;
        broker
            .commit_disclosure(peer.principal, ticket, bytes.len() as u32, Instant::now())
            .map_err(refusal)?;
        Ok(data)
    }) {
        Ok(data) => BackendResult::AuthorizedObservation(data),
        Err(error) => BackendResult::Refused(error),
    };
    finish(peer, &request.key, result, Instant::now());
}

fn direct_binding(
    operation: &Operation,
    reference: Option<TargetRef>,
) -> Result<InvocationBinding, Refusal> {
    let target = || reference.ok_or(Refusal::InvalidArguments);
    let (kind, parameters) = match operation {
        Operation::SessionsList => (DirectKind::Discovery, serde_json::json!(["sessions.list"])),
        Operation::SessionsGet { .. } => (
            DirectKind::Discovery,
            serde_json::json!(["sessions.get", target()?]),
        ),
        Operation::TerminalReadScreen { max_bytes, .. } => (
            DirectKind::Observation,
            serde_json::json!(["terminal.read_screen", target()?, max_bytes]),
        ),
        Operation::TerminalReadScrollback {
            cursor, max_bytes, ..
        } => {
            if cursor
                .as_deref()
                .is_some_and(|cursor| !observation::valid_cursor(cursor))
            {
                return Err(Refusal::InvalidArguments);
            }
            (
                DirectKind::Observation,
                serde_json::json!(["terminal.read_scrollback", target()?, cursor, max_bytes]),
            )
        }
        Operation::TerminalGetSelection { max_bytes, .. } => (
            DirectKind::Observation,
            serde_json::json!(["terminal.get_selection", target()?, max_bytes]),
        ),
        Operation::TerminalWaitForChange {
            after_revision,
            timeout_ms,
            ..
        } => (
            DirectKind::Observation,
            serde_json::json!([
                "terminal.wait_for_change",
                target()?,
                after_revision,
                timeout_ms
            ]),
        ),
        Operation::PairingStatus => return Err(Refusal::InvalidArguments),
        Operation::ControllerCapabilities
        | Operation::PresentationCapabilities
        | Operation::ControllerRegister { .. }
        | Operation::ControllerStatus { .. }
        | Operation::ControllerUnregister { .. }
        | Operation::TerminalSendText { .. }
        | Operation::TerminalSendKeys { .. } => return Err(Refusal::Unavailable),
    };
    let canonical = serde_json::to_vec(&parameters).map_err(|_| Refusal::InvalidArguments)?;
    Ok(InvocationBinding {
        api_version: 1,
        invocation: Invocation::Direct { operation: kind },
        parameters: Digest(Sha256::digest(canonical).into()),
    })
}

fn admit_request(
    broker: &mut Broker,
    targets: &BTreeMap<u64, Target>,
    peer: &mut Peer,
    operation: &Operation,
    now: Instant,
) -> Result<(RunId, RequestId, Option<TargetRef>), Refusal> {
    if matches!(
        operation,
        Operation::ControllerCapabilities
            | Operation::PresentationCapabilities
            | Operation::ControllerRegister { .. }
            | Operation::ControllerStatus { .. }
            | Operation::ControllerUnregister { .. }
            | Operation::TerminalSendText { .. }
            | Operation::TerminalSendKeys { .. }
    ) {
        return Err(Refusal::Unavailable);
    }
    let grant = peer.grant.as_ref().ok_or(Refusal::NotAuthorized)?;
    let discovery = matches!(
        operation,
        Operation::SessionsList | Operation::SessionsGet { .. }
    );
    let reference = match operation {
        Operation::SessionsList => None,
        Operation::SessionsGet { session_ref }
        | Operation::TerminalReadScreen { session_ref, .. }
        | Operation::TerminalReadScrollback { session_ref, .. }
        | Operation::TerminalGetSelection { session_ref, .. }
        | Operation::TerminalWaitForChange { session_ref, .. } => {
            let reference: TargetRef =
                serde_json::from_str(session_ref).map_err(|_| Refusal::InvalidArguments)?;
            if !grant.targets.contains(&reference) {
                return Err(Refusal::NotAuthorized);
            }
            if targets
                .get(&reference.session())
                .is_none_or(|target| target.reference != reference)
            {
                return Err(Refusal::StaleTarget);
            }
            Some(reference)
        }
        Operation::PairingStatus => return Err(Refusal::InvalidArguments),
        Operation::ControllerCapabilities
        | Operation::PresentationCapabilities
        | Operation::ControllerRegister { .. }
        | Operation::ControllerStatus { .. }
        | Operation::ControllerUnregister { .. }
        | Operation::TerminalSendText { .. }
        | Operation::TerminalSendKeys { .. } => return Err(Refusal::Unavailable),
    };
    let scope = if discovery {
        Scope {
            discover: grant.targets.clone(),
            ..Scope::default()
        }
    } else {
        Scope {
            observe: reference.into_iter().collect(),
            ..Scope::default()
        }
    };
    let binding = direct_binding(operation, reference)?;
    let run = broker
        .admit_run(
            peer.principal,
            None,
            RunSpec {
                binding,
                scope,
                budget: Budget::default(),
                deadline: grant.expires,
            },
            now,
        )
        .map_err(refusal)?;
    let max_bytes = match operation {
        Operation::TerminalReadScreen { max_bytes, .. }
        | Operation::TerminalReadScrollback { max_bytes, .. }
        | Operation::TerminalGetSelection { max_bytes, .. } => *max_bytes,
        Operation::TerminalWaitForChange { .. } => 512,
        _ => 32_768,
    };
    let operation = match reference.filter(|_| !discovery) {
        Some(target) => AuthorityOperation::Observe { target, max_bytes },
        None => AuthorityOperation::Discover { max_bytes },
    };
    match broker.prepare_request(peer.principal, run, operation, now) {
        Ok(ticket) => Ok((run, ticket, reference)),
        Err(error) => {
            retain_completion(peer, run, None);
            Err(refusal(error))
        }
    }
}

fn metadata(target: TargetRef, session: &SessionTab) -> Result<serde_json::Value, Refusal> {
    Ok(serde_json::json!({
        "session_ref": serde_json::to_string(&target).map_err(|_| Refusal::InvalidBackendResult)?,
        "label": session.label.chars().take(128).collect::<String>(),
        "lifecycle": "running",
        "untrusted": true,
    }))
}

#[allow(clippy::too_many_arguments)]
fn snapshot(
    broker: &Broker,
    targets: &BTreeMap<u64, Target>,
    peer: &Peer,
    ticket: RequestId,
    operation: &Operation,
    reference: Option<TargetRef>,
    apps: &[&FesTermApp],
    now: Instant,
) -> Result<serde_json::Value, Refusal> {
    use serde_json::json;
    if matches!(
        operation,
        Operation::SessionsList | Operation::SessionsGet { .. }
    ) {
        let visible = broker
            .discovery_targets(peer.principal, ticket, now)
            .map_err(refusal)?;
        let selected = visible
            .into_iter()
            .filter(|target| reference.is_none_or(|selected| selected == *target));
        let metadata = selected
            .map(|target| metadata(target, session(apps, target).ok_or(Refusal::StaleTarget)?))
            .collect::<Result<Vec<_>, _>>()?;
        return observation::bounded(json!({"sessions": metadata, "untrusted": true}), 32_768);
    }
    let reference = reference.ok_or(Refusal::InvalidArguments)?;
    let target = targets
        .get(&reference.session())
        .ok_or(Refusal::StaleTarget)?;
    let session = session(apps, reference).ok_or(Refusal::StaleTarget)?;
    match operation {
        Operation::TerminalReadScreen { max_bytes, .. } => {
            observation::screen(&session.terminal, target.revision, *max_bytes as usize)
        }
        Operation::TerminalReadScrollback {
            cursor, max_bytes, ..
        } => observation::history(
            &session.terminal,
            target.revision,
            cursor.as_deref(),
            *max_bytes as usize,
        ),
        Operation::TerminalGetSelection { max_bytes, .. } => {
            let text = session
                .view
                .selected_text_with_limit(&session.terminal, *max_bytes as usize)
                .map_err(|_| Refusal::ResponseTooLarge)?;
            observation::bounded(
                json!({
                    "revision": target.revision, "present": text.is_some(), "text": text,
                    "truncated": false, "untrusted": true,
                }),
                *max_bytes as usize,
            )
        }
        _ => Err(Refusal::InvalidArguments),
    }
}

fn settle_waits(
    broker: &mut Broker,
    targets: &BTreeMap<u64, Target>,
    peer: &mut Peer,
    now: Instant,
) -> Result<(), festerm_session::AutomationAdmissionTryError> {
    retry_completion(broker, peer, now)?;
    if peer.connection.is_closed() {
        return Ok(());
    }
    let mut index = 0;
    while index < peer.waits.len() {
        let wait = &peer.waits[index];
        if wait.cancel_pending {
            index += 1;
            continue;
        }
        let target = targets
            .get(&wait.target.session())
            .filter(|target| target.reference == wait.target);
        if target.is_some_and(|target| target.revision == wait.after_revision)
            && now < wait.deadline
        {
            index += 1;
            continue;
        }
        let (run, request) = (wait.run, wait.request);
        retain_completion(peer, run, Some(request));
        let wait = peer.waits.remove(index);
        let result = target.ok_or(Refusal::StaleTarget).and_then(|target| {
            if wait.after_revision > target.revision {
                return Err(Refusal::InvalidArguments);
            }
            let data = serde_json::json!({
                "revision": target.revision,
                "changed": target.revision != wait.after_revision,
                "timed_out": target.revision == wait.after_revision,
                "untrusted": true,
            });
            let bytes = serde_json::to_vec(&data).map_err(|_| Refusal::InvalidBackendResult)?;
            broker
                .commit_disclosure(
                    peer.principal,
                    wait.request,
                    bytes.len() as u32,
                    Instant::now(),
                )
                .map_err(refusal)?;
            Ok(data)
        });
        finish(
            peer,
            &wait.key,
            match result {
                Ok(data) => BackendResult::AuthorizedObservation(data),
                Err(error) => BackendResult::Refused(error),
            },
            Instant::now(),
        );
        retry_completion(broker, peer, Instant::now())?;
        if peer.connection.is_closed() {
            break;
        }
    }
    Ok(())
}
