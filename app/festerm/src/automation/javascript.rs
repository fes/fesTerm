//! Desktop beta JavaScript workflow runs.
//!
//! One native-approved run at a time. The Run click is the native approval of
//! the exact captured snapshot, parameters, terminal and requested scopes.
//! Reads are disclosed after broker admission; every input is shown in
//! Settings and needs a separate human approval before it is committed.

use std::collections::VecDeque;

use festerm_automation::{ConnectionId, LeaseRef};
use festerm_automation_js::{
    protocol::{
        HostError, HostRequest, HostResult, Limits, Outcome, Target as WireTarget, TargetInfo,
    },
    snapshot::{Capability, ParameterType, Parameters},
    DisclosureGuard, DisclosurePermit, Event as WorkerEvent, Supervisor, WorkerProgram,
};
use festerm_automation_workflow::{terminal_input, FrozenInvocation, ProjectedCall};
use festerm_core::{PreparedInput, Terminal};

use super::*;
use crate::session_controller::AutomationInputFence;

/// Opaque handle given to the script for the single approved terminal.
pub(crate) const TARGET_ID: &str = "terminal";
const WORKER_RUN_MS: u64 = 295_000;
const MAX_EVENTS_PER_TICK: usize = 32;
const MAX_QUEUED_INPUTS: usize = 8;
const MAX_VISITED_CELLS: usize = 65_536;
const DISCOVERY_BYTES: u32 = 4_096;
const WAIT_BYTES: u32 = 512;
const FENCE_RETRY: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(100);

#[derive(Default)]
pub(super) struct State {
    next_token: u64,
    run: Option<Run>,
    outcome: Option<String>,
}

struct Run {
    token: u64,
    name: String,
    target: Option<TargetRef>,
    target_label: String,
    principal: ConnectionId,
    invocation: FrozenInvocation,
    open: Arc<AtomicBool>,
    expires: Instant,
    phase: Phase,
    /// Latest per-input failure; kept visible and carried into the outcome.
    notice: Option<String>,
}

enum Phase {
    Starting(mpsc::Receiver<Result<Supervisor, festerm_automation_js::Error>>),
    Running(Box<Running>),
}

struct Running {
    supervisor: Supervisor,
    run: RunId,
    lease: Option<LeaseRef>,
    waits: Vec<Wait>,
    inputs: VecDeque<Input>,
}

struct Wait {
    id: u32,
    request: RequestId,
    target: TargetRef,
    after: u64,
    deadline: Instant,
}

struct Input {
    token: u64,
    id: u32,
    target: TargetRef,
    description: String,
    prepared: PreparedInput,
    approved: bool,
    fence: Option<AutomationInputFence>,
    fence_deadline: Option<Instant>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Monitoring {
    #[default]
    Off,
    Starting,
    Ready,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct View {
    pub monitoring: Monitoring,
    pub targets: Vec<(TargetRef, String)>,
    pub run: Option<RunView>,
    pub outcome: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunView {
    pub token: u64,
    pub name: String,
    pub target: String,
    pub starting: bool,
    pub input: Option<InputView>,
    pub queued_inputs: usize,
    pub notice: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InputView {
    pub token: u64,
    pub target: String,
    pub description: String,
    pub bytes: usize,
    pub approved: bool,
}

/// Rechecked by the worker supervisor at every physical write of a result.
struct Disclosure {
    open: Arc<AtomicBool>,
    privacy: Arc<AtomicBool>,
    targets: Vec<ipc::TargetGuard>,
}

impl DisclosureGuard for Disclosure {
    fn try_enter(
        &self,
    ) -> Result<Option<Box<dyn DisclosurePermit + '_>>, festerm_automation_js::Error> {
        let guard = match festerm_session::try_automation_admission_lock() {
            Ok(guard) => guard,
            Err(festerm_session::AutomationAdmissionTryError::Busy) => return Ok(None),
            Err(festerm_session::AutomationAdmissionTryError::Poisoned) => {
                return Err(festerm_automation_js::Error::DisclosureRevoked)
            }
        };
        if !self.open.load(Ordering::Acquire)
            || !try_privacy_admission(&self.privacy)
            || !self.targets.iter().all(ipc::TargetGuard::is_valid)
        {
            return Err(festerm_automation_js::Error::DisclosureRevoked);
        }
        Ok(Some(Box::new(guard)))
    }
}

fn host_error(error: festerm_automation::Error) -> HostError {
    use festerm_automation::Error;
    match error {
        Error::StaleTarget => HostError::StaleTarget,
        Error::Expired => HostError::Expired,
        Error::QuotaExceeded => HostError::Budget,
        Error::Closed => HostError::Cancelled,
        Error::Revoked | Error::Unauthorized | Error::BindingMismatch => HostError::Revoked,
        _ => HostError::Failed,
    }
}

fn retire_supervisor(supervisor: Supervisor) {
    supervisor.cancel();
    // Drop joins the bounded worker watchdog; never on the UI thread.
    let spawned = thread::Builder::new()
        .name("festerm-workflow-cleanup".into())
        .spawn(move || {
            if supervisor.shutdown().is_err() {
                tracing::warn!("workflow worker cleanup reported a failure");
            }
        });
    if spawned.is_err() {
        tracing::error!("workflow worker cleanup thread could not start");
    }
}

fn retire_run(run: Run) {
    run.open.store(false, Ordering::Release);
    match run.phase {
        Phase::Running(running) => retire_supervisor(running.supervisor),
        Phase::Starting(receiver) => {
            // A supervisor already buffered must not be dropped (joined) here;
            // one sent after this receiver closes is retired by its thread.
            if let Ok(Ok(supervisor)) = receiver.try_recv() {
                retire_supervisor(supervisor);
            }
        }
    }
}

fn worker_program() -> Result<WorkerProgram, &'static str> {
    #[cfg(test)]
    let executable = std::env::var_os("FESTERM_TEST_JS_WORKER")
        .map(std::path::PathBuf::from)
        .map_or_else(std::env::current_exe, Ok);
    #[cfg(not(test))]
    let executable = std::env::current_exe();
    let executable = executable
        .and_then(|path| path.canonicalize())
        .map_err(|_| "Workflow worker executable could not be resolved.")?;
    WorkerProgram::from_install_owned_absolute(&executable)
        .map_err(|_| "Workflow worker executable is not a protected installed file.")
}

/// Plain rows without trailing blanks, bounded to `max_bytes` UTF-8 bytes.
pub(super) fn screen_text(terminal: &Terminal, max_bytes: usize) -> (String, bool) {
    let dimensions = terminal.screen().dimensions();
    let mut text = String::new();
    let mut visited = 0;
    let mut truncated = false;
    'rows: for row in 0..dimensions.rows() {
        let mut line = String::new();
        for column in 0..dimensions.columns() {
            visited += 1;
            if visited > MAX_VISITED_CELLS {
                truncated = true;
                break;
            }
            if let Some(cell) = terminal.cell_ref(column, row) {
                if !cell.is_continuation() {
                    line.push_str(cell.text());
                }
            }
        }
        if row > 0 {
            if text.len() + 1 > max_bytes {
                truncated = true;
                break;
            }
            text.push('\n');
        }
        for character in line.trim_end().chars() {
            if text.len() + character.len_utf8() > max_bytes {
                truncated = true;
                break 'rows;
            }
            text.push(character);
        }
        if truncated {
            break;
        }
    }
    let end = text.trim_end_matches('\n').len();
    text.truncate(end);
    (text, truncated)
}

/// The most recent `max_lines` retained history lines, keeping the newest
/// bytes when the result exceeds `max_bytes`.
pub(super) fn scrollback_text(
    terminal: &Terminal,
    max_lines: usize,
    max_bytes: usize,
) -> (String, bool) {
    let lines = terminal.scrollback_lines();
    let count = lines.len();
    let selected: Vec<_> = lines.skip(count.saturating_sub(max_lines)).collect();
    let mut rendered = Vec::new();
    let mut total = 0;
    let mut visited = 0;
    let mut truncated = false;
    for line in selected.iter().rev() {
        let mut value = String::new();
        for cell in line.cells() {
            visited += 1;
            if visited > MAX_VISITED_CELLS {
                truncated = true;
                break;
            }
            if !cell.is_continuation() {
                value.push_str(cell.text());
            }
        }
        let mut value = value.trim_end().to_owned();
        if line.has_hard_break() {
            value.push('\n');
        }
        total += value.len();
        rendered.push(value);
        if truncated || total > max_bytes {
            truncated |= total > max_bytes;
            break;
        }
    }
    let mut text: String = rendered.into_iter().rev().collect();
    if text.len() > max_bytes {
        let mut start = text.len() - max_bytes;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text.drain(..start);
        truncated = true;
    }
    (text, truncated)
}

fn describe_text(text: &str) -> String {
    let mut description = String::from("Text: \"");
    for character in text.chars() {
        match character {
            '\n' => description.push_str("\\n"),
            '\r' => description.push_str("\\r"),
            '\t' => description.push_str("\\t"),
            '"' => description.push_str("\\\""),
            '\\' => description.push_str("\\\\"),
            character if character.is_control() || is_format_character(character) => {
                description.push_str(&format!("\\u{{{:x}}}", u32::from(character)));
            }
            character => description.push(character),
        }
    }
    description.push('"');
    description
}

fn is_format_character(character: char) -> bool {
    matches!(
        u32::from(character),
        0x00ad | 0x061c | 0x180e | 0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x206f | 0xfeff
    )
}

fn describe_keys(keys: &[String]) -> String {
    format!("Keys: {}", keys.join(" "))
}

fn uses_target(manifest: &festerm_automation_js::snapshot::Manifest) -> bool {
    manifest
        .parameters
        .values()
        .any(|kind| matches!(kind, ParameterType::Target {}))
        || manifest
            .capabilities
            .iter()
            .any(|capability| !matches!(capability, Capability::Open | Capability::InvokeWorkflow))
}

impl Coordinator {
    fn javascript_status(&mut self, status: impl Into<String>) {
        self.javascript.outcome = Some(status.into());
    }

    fn javascript_notice(&mut self, notice: &str) {
        match self.javascript.run.as_mut() {
            Some(run) => run.notice = Some(notice.into()),
            None => self.javascript_status(notice),
        }
    }

    fn javascript_final_status(notice: Option<String>, status: impl Into<String>) -> String {
        let status = status.into();
        match notice {
            Some(notice) => format!("{status} Last input problem: {notice}"),
            None => status,
        }
    }

    pub(super) fn javascript_monitoring(&self) -> Monitoring {
        if self.broker.is_some() && !self.privacy_gate.is_closed() {
            Monitoring::Ready
        } else if self.starting.is_some() || self.want_shared_start {
            Monitoring::Starting
        } else {
            Monitoring::Off
        }
    }

    pub(super) fn javascript_command(
        &mut self,
        command: NativeCommand,
        context: &egui::Context,
        apps: &[&FesTermApp],
    ) {
        if self.unwind_cleanup_pending {
            self.finish_unwind_cleanup();
        }
        if self.privacy_gate.is_closed() && (self.broker.is_some() || self.starting.is_some()) {
            self.privacy_lost();
        }
        match command {
            NativeCommand::StartWorkflowMonitoring => self.start_javascript_monitoring(context),
            NativeCommand::StartWorkflowRun { .. } if apps.is_empty() => {
                self.javascript_status("Workflow refused without its originating window.");
            }
            NativeCommand::StartWorkflowRun {
                digest,
                target,
                parameters,
            } => self.start_javascript_run(digest, target, parameters, apps, context),
            NativeCommand::CancelWorkflowRun { token } => {
                self.cancel_javascript_run(token, context)
            }
            NativeCommand::ApproveWorkflowInput { token } => {
                self.review_javascript_input(token, true, context)
            }
            NativeCommand::DenyWorkflowInput { token } => {
                self.review_javascript_input(token, false, context)
            }
            _ => {}
        }
        context.request_repaint();
    }

    pub(super) fn start_javascript_monitoring(&mut self, context: &egui::Context) {
        if !self.javascript_enabled || self.unwind_stopped || self.workflow_unwind_stopped {
            return;
        }
        self.ensure_shared();
        context.request_repaint();
    }

    fn live_target_locked(
        &self,
        reference: TargetRef,
        guard: &festerm_session::AutomationAdmissionGuard,
    ) -> bool {
        self.targets
            .get(&reference.session())
            .is_some_and(|target| {
                target.reference == reference
                    && target.local_pty
                    && target.liveness.load(Ordering::Acquire)
                    && target.registration.load(Ordering::Acquire)
                    && target.attachment.capture_under_admission(guard)
                        == Some(target.attachment_generation)
            })
    }

    pub(super) fn start_javascript_run(
        &mut self,
        digest: String,
        target: Option<TargetRef>,
        parameters: Parameters,
        apps: &[&FesTermApp],
        context: &egui::Context,
    ) {
        context.request_repaint();
        if self.javascript.run.is_some() {
            return self.javascript_status("A workflow is already running. Stop it first.");
        }
        if !self.javascript_enabled || self.workflow_unwind_stopped || self.unwind_stopped {
            return self.javascript_status("Enable JavaScript workflows before running.");
        }
        if self.privacy_lost_this_tick
            || self.reconciliation_deferred
            || self.javascript_monitoring() != Monitoring::Ready
        {
            return self.javascript_status(
                "Workflow refused: privacy monitoring is not ready. Start it and retry.",
            );
        }
        let Some(bundle) = self
            .workflow
            .view()
            .current_bundle()
            .filter(|bundle| bundle.digest() == digest)
            .cloned()
        else {
            return self.javascript_status(
                "Workflow refused: the captured snapshot changed. Review it again.",
            );
        };
        let manifest = bundle.manifest().clone();
        let capabilities = &manifest.capabilities;
        let target = if uses_target(&manifest) {
            let Some(target) = target else {
                return self.javascript_status("Select a local terminal before running.");
            };
            Some(target)
        } else {
            None
        };
        let Ok(guard) = festerm_session::try_automation_admission_lock() else {
            return self.javascript_status("Automation is busy. Try running the workflow again.");
        };
        if target.is_some_and(|target| !self.live_target_locked(target, &guard)) {
            return self.javascript_status(
                "Workflow refused: the selected terminal changed or is not a local shell.",
            );
        }
        let selected: BTreeSet<_> = target.into_iter().collect();
        let has = |wanted: &[Capability]| wanted.iter().any(|item| capabilities.contains(item));
        let scope = Scope {
            discover: if has(&[Capability::Observe]) {
                selected.clone()
            } else {
                BTreeSet::new()
            },
            observe: if has(&[
                Capability::ReadScreen,
                Capability::ReadScrollback,
                Capability::GetSelection,
                Capability::WaitForChange,
            ]) {
                selected.clone()
            } else {
                BTreeSet::new()
            },
            control: if has(&[Capability::SendText, Capability::SendKeys]) {
                selected.clone()
            } else {
                BTreeSet::new()
            },
            invoke_workflows: true,
            return_results: false,
        };
        let scoped =
            !scope.discover.is_empty() || !scope.observe.is_empty() || !scope.control.is_empty();
        let mapping: Vec<_> = target
            .filter(|_| scoped)
            .map(|target| (TARGET_ID.to_owned(), target))
            .into_iter()
            .collect();
        let invocation =
            match FrozenInvocation::new((*bundle).clone(), parameters, mapping, scope.clone()) {
                Ok(invocation) => invocation,
                Err(_) => {
                    return self.javascript_status(
                        "Workflow refused: parameters or capabilities do not match this terminal.",
                    )
                }
            };
        let now = Instant::now();
        let Some(broker) = self.broker.as_mut() else {
            return self.javascript_status("Workflow refused: automation is not running.");
        };
        let Ok(principal) = broker.accept_connection() else {
            return self.javascript_status("Workflow refused: automation connection limit.");
        };
        let request = invocation.grant_request();
        let mut approval =
            NativeApproval::new(request.clone(), festerm_automation::MAX_GRANT_LIFETIME);
        let sources: BTreeSet<_> = scope.discover.union(&scope.observe).copied().collect();
        for control in &scope.control {
            approval = approval.accept_control_handoff(*control, sources.clone());
        }
        let approved = broker
            .request_approval(principal, request, now)
            .and_then(|id| broker.approve(principal, id, approval, now, &guard));
        if let Err(error) = approved {
            broker.disconnect(principal, &guard);
            tracing::warn!(?error, "workflow approval refused");
            return self.javascript_status(
                "Workflow refused by the automation broker. No access was granted.",
            );
        }
        drop(guard);
        let program = match worker_program() {
            Ok(program) => program,
            Err(status) => {
                self.disconnect_javascript_principal(principal);
                return self.javascript_status(status);
            }
        };
        let (wake, woken) = festerm_automation_js::event_wake_channel();
        let repaint = context.clone();
        let _ = thread::Builder::new()
            .name("festerm-workflow-wake".into())
            .spawn(move || {
                while woken.recv().is_ok() {
                    repaint.request_repaint();
                }
            });
        let (sender, receiver) = mpsc::sync_channel(1);
        let snapshot = invocation.bundle().clone();
        let bound = invocation.worker_parameters().clone();
        let repaint = context.clone();
        let spawned = thread::Builder::new()
            .name("festerm-workflow-start".into())
            .spawn(move || {
                let limits = Limits {
                    run_ms: WORKER_RUN_MS,
                    ..Limits::default()
                };
                let result =
                    Supervisor::spawn_bound_with_wake(&program, &snapshot, &bound, limits, wake);
                if let Err(mpsc::TrySendError::Disconnected(Ok(supervisor))) =
                    sender.try_send(result)
                {
                    retire_supervisor(supervisor);
                }
                repaint.request_repaint();
            });
        if spawned.is_err() {
            self.disconnect_javascript_principal(principal);
            return self.javascript_status("Workflow refused: the worker could not start.");
        }
        let target_label = target
            .and_then(|target| target_label(apps, target))
            .unwrap_or_else(|| "no terminal".to_owned());
        self.javascript.next_token += 1;
        self.javascript.outcome = None;
        self.javascript.run = Some(Run {
            token: self.javascript.next_token,
            name: approval_label(&manifest.name),
            target,
            target_label,
            principal,
            invocation,
            open: Arc::new(AtomicBool::new(true)),
            expires: now + festerm_automation::MAX_GRANT_LIFETIME,
            phase: Phase::Starting(receiver),
            notice: None,
        });
    }

    fn disconnect_javascript_principal(&mut self, principal: ConnectionId) {
        if let Some(broker) = self.broker.as_mut() {
            if let Some(guard) = festerm_session::automation_admission_lock() {
                broker.disconnect(principal, &guard);
            } else {
                self.broker = None;
                self.privacy_gate.signal_loss();
            }
        }
    }

    /// Synchronously cut broker authority, then stop the worker off-thread.
    pub(super) fn cancel_javascript(&mut self, status: &str) {
        let Some(run) = self.javascript.run.take() else {
            return;
        };
        run.open.store(false, Ordering::Release);
        let guard = festerm_session::automation_admission_lock();
        let broker = &mut self.broker;
        let cut = |broker: &mut Option<Broker>| match (broker.as_mut(), guard.as_ref()) {
            (Some(broker), Some(guard)) => {
                if let Phase::Running(running) = &run.phase {
                    let _ = broker.cancel_run(run.principal, running.run, Instant::now(), guard);
                }
                broker.disconnect(run.principal, guard);
            }
            (Some(_), None) => *broker = None,
            (None, _) => {}
        };
        match &run.phase {
            Phase::Running(running) => running.supervisor.cancel_with(|| cut(broker)),
            Phase::Starting(_) => cut(broker),
        }
        if guard.is_none() {
            self.privacy_gate.signal_loss();
        }
        drop(guard);
        let notice = run.notice.clone();
        retire_run(run);
        self.javascript_status(Self::javascript_final_status(notice, status));
    }

    /// Unwind path: no admission, no blocking, no callbacks.
    pub(super) fn javascript_unwind(&mut self) {
        let notice = self.javascript.run.take().and_then(|run| {
            let notice = run.notice.clone();
            retire_run(run);
            notice
        });
        self.javascript.outcome = Some(Self::javascript_final_status(
            notice,
            "Workflow stopped after an internal error. No further input was sent.",
        ));
    }

    fn finish_javascript(&mut self, completion: Completion, status: String) {
        let Some(run) = self.javascript.run.take() else {
            return;
        };
        run.open.store(false, Ordering::Release);
        if let Some(guard) = festerm_session::automation_admission_lock() {
            if let (Some(broker), Phase::Running(running)) = (self.broker.as_mut(), &run.phase) {
                let _ = broker.complete(run.principal, running.run, completion, Instant::now());
                broker.disconnect(run.principal, &guard);
            }
        } else {
            self.broker = None;
            self.privacy_gate.signal_loss();
        }
        let notice = run.notice.clone();
        retire_run(run);
        self.javascript_status(Self::javascript_final_status(notice, status));
    }

    pub(crate) fn drive_javascript(&mut self, apps: &[&FesTermApp], context: &egui::Context) {
        let guard = CoordinatorUnwind { coordinator: self };
        guard.coordinator.drive_javascript_inner(apps, context);
    }

    fn drive_javascript_inner(&mut self, apps: &[&FesTermApp], context: &egui::Context) {
        let Some(run) = self.javascript.run.as_ref() else {
            return;
        };
        if !self.javascript_enabled || self.workflow_unwind_stopped || self.unwind_stopped {
            return self
                .cancel_javascript("Workflow stopped because JavaScript workflows were disabled.");
        }
        if self.broker.is_none() || self.privacy_gate.is_closed() || self.privacy_lost_this_tick {
            return self.cancel_javascript(
                "Workflow stopped after privacy loss or an automation restart. No further input was sent.",
            );
        }
        if run.target.is_some_and(|target| {
            self.targets
                .get(&target.session())
                .is_none_or(|current| current.reference != target)
        }) {
            return self
                .cancel_javascript("Workflow stopped because its terminal closed or restarted.");
        }
        if Instant::now() >= run.expires {
            return self.cancel_javascript("Workflow stopped at its 5 minute limit.");
        }
        context.request_repaint_after(POLL);
        if let Phase::Starting(receiver) = &run.phase {
            match receiver.try_recv() {
                Ok(Ok(supervisor)) => self.admit_javascript_run(supervisor),
                Ok(Err(error)) => {
                    tracing::warn!(?error, "workflow worker failed to start");
                    #[cfg(test)]
                    eprintln!("DEBUG worker start error: {error:?}");
                    let status = match error {
                        festerm_automation_js::Error::Unavailable(_) => {
                            "Workflow refused: isolated JavaScript execution is unavailable on this system."
                        }
                        _ => "Workflow worker failed to start. No terminal access was used.",
                    };
                    self.cancel_javascript(status);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.cancel_javascript(
                    "Workflow worker failed to start. No terminal access was used.",
                ),
            }
            return;
        }
        for _ in 0..MAX_EVENTS_PER_TICK {
            let Some(Phase::Running(running)) = self.javascript.run.as_ref().map(|run| &run.phase)
            else {
                return;
            };
            match running.supervisor.try_event() {
                Ok(None) => break,
                Ok(Some(WorkerEvent::HostCall { id, request })) => {
                    if let Err(error) = self.javascript_host_call(id, request, apps) {
                        tracing::warn!(?error, "workflow host result could not be queued");
                        return self.cancel_javascript(
                            "Workflow stopped after a worker protocol failure.",
                        );
                    }
                }
                Ok(Some(WorkerEvent::Finished { outcome })) => {
                    let (completion, status) = match outcome {
                        Outcome::Completed { .. } => (
                            Completion::Returned,
                            "Workflow completed. Its return value is not shown in this beta.",
                        ),
                        Outcome::ScriptFailed {} => (Completion::Failed, "Workflow script failed."),
                        Outcome::ResourceLimit {} => (
                            Completion::Failed,
                            "Workflow stopped at a CPU, memory, time or call limit.",
                        ),
                        Outcome::UnsettledWork {} => (
                            Completion::Failed,
                            "Workflow returned with unfinished terminal requests.",
                        ),
                        Outcome::Cancelled {} => (Completion::Failed, "Workflow cancelled."),
                        Outcome::ProtocolFailed {} => {
                            (Completion::Failed, "Workflow worker protocol failure.")
                        }
                        Outcome::Unavailable {} => (
                            Completion::Failed,
                            "Isolated JavaScript execution is unavailable on this system.",
                        ),
                    };
                    return self.finish_javascript(completion, status.to_owned());
                }
                Ok(Some(WorkerEvent::Failed(error))) | Err(error) => {
                    tracing::warn!(?error, "workflow worker failed");
                    return self.finish_javascript(
                        Completion::Failed,
                        "Workflow worker failed. No further input was sent.".to_owned(),
                    );
                }
            }
        }
        if let Err(error) = self.settle_javascript_waits() {
            tracing::warn!(?error, "workflow wait result could not be queued");
            self.cancel_javascript("Workflow stopped after a worker protocol failure.");
        }
    }

    fn admit_javascript_run(&mut self, supervisor: Supervisor) {
        let Some(run) = self.javascript.run.as_mut() else {
            return retire_supervisor(supervisor);
        };
        let deadline = supervisor.deadline().min(run.expires);
        let spec = run.invocation.run_spec(Budget::default(), deadline);
        let admitted = match (
            festerm_session::try_automation_admission_lock(),
            self.broker.as_mut(),
        ) {
            (Ok(_guard), Some(broker)) => {
                broker.admit_run(run.principal, None, spec, Instant::now())
            }
            _ => Err(festerm_automation::Error::Busy),
        };
        match admitted {
            Ok(id) => {
                run.phase = Phase::Running(Box::new(Running {
                    supervisor,
                    run: id,
                    lease: None,
                    waits: Vec::new(),
                    inputs: VecDeque::new(),
                }));
            }
            Err(error) => {
                tracing::warn!(?error, "workflow run admission refused");
                retire_supervisor(supervisor);
                self.cancel_javascript(
                    "Workflow refused by the automation broker before it started.",
                );
            }
        }
    }

    fn javascript_disclosure(&self, target: Option<TargetRef>) -> Arc<dyn DisclosureGuard> {
        let open = self
            .javascript
            .run
            .as_ref()
            .map_or_else(|| Arc::new(AtomicBool::new(false)), |run| run.open.clone());
        Arc::new(Disclosure {
            open,
            privacy: self.privacy_gate.closed_flag(),
            targets: target
                .and_then(|target| self.targets.get(&target.session()))
                .map(Target::guard)
                .into_iter()
                .collect(),
        })
    }

    fn javascript_reply(
        &self,
        id: u32,
        result: HostResult,
        target: Option<TargetRef>,
    ) -> Result<(), festerm_automation_js::Error> {
        let disclosure = self.javascript_disclosure(target);
        let Some(Phase::Running(running)) = self.javascript.run.as_ref().map(|run| &run.phase)
        else {
            return Ok(());
        };
        running.supervisor.host_result(id, result, disclosure)
    }

    fn javascript_host_call(
        &mut self,
        id: u32,
        request: HostRequest,
        apps: &[&FesTermApp],
    ) -> Result<(), festerm_automation_js::Error> {
        let Some(run) = self.javascript.run.as_ref() else {
            return Ok(());
        };
        let projected = match run.invocation.project(&request) {
            Ok(projected) => projected,
            Err(festerm_automation_workflow::Error::InvalidInput) => {
                return self.javascript_reply(
                    id,
                    HostResult::Error {
                        code: HostError::Failed,
                    },
                    None,
                )
            }
            Err(_) => {
                return self.javascript_reply(
                    id,
                    HostResult::Error {
                        code: HostError::Denied,
                    },
                    None,
                )
            }
        };
        match (projected, request) {
            (ProjectedCall::Control(target), HostRequest::SendText { text, .. }) => self
                .queue_javascript_input(id, target, apps, |terminal| {
                    terminal_input::prepare_send_text(terminal, &text)
                        .map(|input| (input, describe_text(&text)))
                }),
            (ProjectedCall::Control(target), HostRequest::SendKeys { keys, .. }) => self
                .queue_javascript_input(id, target, apps, |terminal| {
                    terminal_input::prepare_send_keys(terminal, &keys)
                        .map(|input| (input, describe_keys(&keys)))
                }),
            (ProjectedCall::Discover, HostRequest::Observe {}) => self.javascript_discover(id),
            (
                ProjectedCall::Observe(target),
                HostRequest::WaitForChange {
                    after_revision,
                    timeout_ms,
                    ..
                },
            ) => self.javascript_wait(id, target, after_revision, timeout_ms),
            (ProjectedCall::Observe(target), request) => {
                self.javascript_read(id, target, &request, apps)
            }
            _ => self.javascript_reply(
                id,
                HostResult::Error {
                    code: HostError::Denied,
                },
                None,
            ),
        }
    }

    fn javascript_discover(&mut self, id: u32) -> Result<(), festerm_automation_js::Error> {
        let result = self.javascript_admitted(
            DISCOVERY_BYTES,
            None,
            |coordinator, broker, principal, ticket| {
                let visible = broker
                    .discovery_targets(principal, ticket, Instant::now())
                    .map_err(host_error)?;
                let targets = visible
                    .into_iter()
                    .filter_map(|reference| {
                        let target = coordinator.targets.get(&reference.session())?;
                        (target.reference == reference).then(|| TargetInfo {
                            target: WireTarget {
                                id: TARGET_ID.to_owned(),
                                generation: reference.generation(),
                            },
                            revision: target.revision,
                        })
                    })
                    .collect();
                Ok(HostResult::Observation { targets })
            },
        );
        self.javascript_reply(id, result, None)
    }

    fn javascript_read(
        &mut self,
        id: u32,
        target: TargetRef,
        request: &HostRequest,
        apps: &[&FesTermApp],
    ) -> Result<(), festerm_automation_js::Error> {
        let max_bytes = match request {
            HostRequest::ReadScreen { max_bytes, .. }
            | HostRequest::ReadScrollback { max_bytes, .. }
            | HostRequest::GetSelection { max_bytes, .. } => usize::from(*max_bytes),
            _ => {
                return self.javascript_reply(
                    id,
                    HostResult::Error {
                        code: HostError::Denied,
                    },
                    None,
                )
            }
        };
        let reserve = u32::try_from(max_bytes.saturating_mul(6).saturating_add(256))
            .unwrap_or(u32::MAX)
            .min(festerm_automation::MAX_OBSERVATION_BYTES);
        let result = self.javascript_admitted(reserve, Some(target), |coordinator, _, _, _| {
            let current = coordinator
                .targets
                .get(&target.session())
                .filter(|current| current.reference == target)
                .ok_or(HostError::StaleTarget)?;
            let session = session(apps, target).ok_or(HostError::StaleTarget)?;
            let revision = current.revision;
            let (text, truncated) = match request {
                HostRequest::ReadScreen { .. } => screen_text(&session.terminal, max_bytes),
                HostRequest::ReadScrollback { max_lines, .. } => {
                    scrollback_text(&session.terminal, usize::from(*max_lines), max_bytes)
                }
                HostRequest::GetSelection { .. } => (
                    session
                        .view
                        .selected_text_with_limit(&session.terminal, max_bytes)
                        .map_err(|_| HostError::Budget)?
                        .unwrap_or_default(),
                    false,
                ),
                _ => return Err(HostError::Denied),
            };
            Ok(HostResult::Text {
                text,
                revision,
                truncated,
            })
        });
        let target = matches!(result, HostResult::Text { .. }).then_some(target);
        self.javascript_reply(id, result, target)
    }

    /// Prepare, build, commit and settle one disclosure request. The result is
    /// built only after broker admission and disclosed only if it fits.
    fn javascript_admitted(
        &mut self,
        reserve: u32,
        target: Option<TargetRef>,
        build: impl FnOnce(&Self, &mut Broker, ConnectionId, RequestId) -> Result<HostResult, HostError>,
    ) -> HostResult {
        let Some(mut broker) = self.broker.take() else {
            return HostResult::Error {
                code: HostError::Revoked,
            };
        };
        let _unwind = TakenBrokerUnwind {
            privacy: self.privacy_gate.clone(),
            stop: self.stop.clone(),
        };
        let result = self.javascript_admitted_with(&mut broker, reserve, target, build);
        self.broker = Some(broker);
        result.unwrap_or_else(|code| HostResult::Error { code })
    }

    fn javascript_admitted_with(
        &self,
        broker: &mut Broker,
        reserve: u32,
        target: Option<TargetRef>,
        build: impl FnOnce(&Self, &mut Broker, ConnectionId, RequestId) -> Result<HostResult, HostError>,
    ) -> Result<HostResult, HostError> {
        let Some(run) = self.javascript.run.as_ref() else {
            return Err(HostError::Cancelled);
        };
        let Phase::Running(running) = &run.phase else {
            return Err(HostError::Cancelled);
        };
        let guard =
            festerm_session::try_automation_admission_lock().map_err(|_| HostError::Failed)?;
        if !self.privacy_gate.try_admit()
            || target.is_some_and(|target| !self.live_target_locked(target, &guard))
        {
            return Err(HostError::Revoked);
        }
        let operation = match target {
            Some(target) => AuthorityOperation::Observe {
                target,
                max_bytes: reserve,
            },
            None => AuthorityOperation::Discover { max_bytes: reserve },
        };
        let ticket = broker
            .prepare_request(run.principal, running.run, operation, Instant::now())
            .map_err(host_error)?;
        let result = build(self, broker, run.principal, ticket).and_then(|result| {
            let bytes = serde_json::to_vec(&result).map_err(|_| HostError::Failed)?;
            let bytes = u32::try_from(bytes.len()).map_err(|_| HostError::Budget)?;
            if bytes > reserve {
                return Err(HostError::Budget);
            }
            running
                .supervisor
                .with_admission(|permit| {
                    permit.commit(|now| broker.commit_disclosure(run.principal, ticket, bytes, now))
                })
                .map_err(|_| HostError::Cancelled)?
                .map_err(|_| HostError::Cancelled)?
                .map_err(host_error)?;
            Ok(result)
        });
        let _ = broker.settle_request(run.principal, ticket);
        drop(guard);
        result
    }

    fn javascript_wait(
        &mut self,
        id: u32,
        target: TargetRef,
        after: u64,
        timeout_ms: u32,
    ) -> Result<(), festerm_automation_js::Error> {
        let admitted = (|| {
            let run = self.javascript.run.as_ref().ok_or(HostError::Cancelled)?;
            let Phase::Running(running) = &run.phase else {
                return Err(HostError::Cancelled);
            };
            let guard =
                festerm_session::try_automation_admission_lock().map_err(|_| HostError::Failed)?;
            if !self.privacy_gate.try_admit() || !self.live_target_locked(target, &guard) {
                return Err(HostError::Revoked);
            }
            let broker = self.broker.as_mut().ok_or(HostError::Revoked)?;
            let ticket = running
                .supervisor
                .with_admission(|permit| {
                    permit.commit(|now| {
                        broker.prepare_request(
                            run.principal,
                            running.run,
                            AuthorityOperation::Observe {
                                target,
                                max_bytes: WAIT_BYTES,
                            },
                            now,
                        )
                    })
                })
                .map_err(|_| HostError::Cancelled)?
                .map_err(|_| HostError::Cancelled)?
                .map_err(host_error)?;
            drop(guard);
            Ok(ticket)
        })();
        let ticket = match admitted {
            Ok(ticket) => ticket,
            Err(code) => return self.javascript_reply(id, HostResult::Error { code }, None),
        };
        let Some(Phase::Running(running)) = self.javascript.run.as_mut().map(|run| &mut run.phase)
        else {
            return Ok(());
        };
        let deadline = (Instant::now() + Duration::from_millis(u64::from(timeout_ms)))
            .min(running.supervisor.deadline());
        running.waits.push(Wait {
            id,
            request: ticket,
            target,
            after,
            deadline,
        });
        running.supervisor.host_admitted(id)
    }

    fn settle_javascript_waits(&mut self) -> Result<(), festerm_automation_js::Error> {
        let now = Instant::now();
        let Some(Phase::Running(running)) = self.javascript.run.as_mut().map(|run| &mut run.phase)
        else {
            return Ok(());
        };
        let mut ready = Vec::new();
        let targets = &self.targets;
        running.waits.retain(|wait| {
            let revision = targets
                .get(&wait.target.session())
                .filter(|target| target.reference == wait.target)
                .map(|target| target.revision);
            if revision == Some(wait.after) && now < wait.deadline {
                return true;
            }
            ready.push((wait.id, wait.request, wait.target, revision, wait.after));
            false
        });
        for (id, ticket, target, revision, after) in ready {
            let result = self.settle_javascript_wait(ticket, target, revision, after);
            let reply_target = matches!(result, HostResult::Change { .. }).then_some(target);
            self.javascript_reply(id, result, reply_target)?;
        }
        Ok(())
    }

    fn settle_javascript_wait(
        &mut self,
        ticket: RequestId,
        target: TargetRef,
        revision: Option<u64>,
        after: u64,
    ) -> HostResult {
        let outcome = (|| {
            let run = self.javascript.run.as_ref().ok_or(HostError::Cancelled)?;
            let Phase::Running(running) = &run.phase else {
                return Err(HostError::Cancelled);
            };
            let revision = revision.ok_or(HostError::StaleTarget)?;
            let result = HostResult::Change {
                revision,
                changed: revision != after,
            };
            let bytes = serde_json::to_vec(&result)
                .map_err(|_| HostError::Failed)?
                .len() as u32;
            let guard = festerm_session::automation_admission_lock().ok_or(HostError::Failed)?;
            if !self.privacy_gate.try_admit() || !self.live_target_locked(target, &guard) {
                return Err(HostError::Revoked);
            }
            let broker = self.broker.as_mut().ok_or(HostError::Revoked)?;
            running
                .supervisor
                .with_admission(|permit| {
                    permit.commit(|now| broker.commit_disclosure(run.principal, ticket, bytes, now))
                })
                .map_err(|_| HostError::Cancelled)?
                .map_err(|_| HostError::Cancelled)?
                .map_err(host_error)?;
            Ok(result)
        })();
        if let (Some(run), Some(broker)) = (self.javascript.run.as_ref(), self.broker.as_mut()) {
            let _ = broker.settle_request(run.principal, ticket);
        }
        outcome.unwrap_or_else(|code| HostResult::Error { code })
    }

    fn queue_javascript_input(
        &mut self,
        id: u32,
        target: TargetRef,
        apps: &[&FesTermApp],
        prepare: impl FnOnce(&Terminal) -> Result<(PreparedInput, String), terminal_input::Error>,
    ) -> Result<(), festerm_automation_js::Error> {
        let Some(session) = session(apps, target) else {
            return self.javascript_reply(
                id,
                HostResult::Error {
                    code: HostError::StaleTarget,
                },
                None,
            );
        };
        let Ok((prepared, description)) = prepare(&session.terminal) else {
            return self.javascript_reply(
                id,
                HostResult::Error {
                    code: HostError::Failed,
                },
                None,
            );
        };
        let token = self.javascript.next_token + 1;
        let Some(Phase::Running(running)) = self.javascript.run.as_mut().map(|run| &mut run.phase)
        else {
            return Ok(());
        };
        if running.inputs.len() >= MAX_QUEUED_INPUTS {
            return self.javascript_reply(
                id,
                HostResult::Error {
                    code: HostError::Budget,
                },
                None,
            );
        }
        self.javascript.next_token = token;
        running.inputs.push_back(Input {
            token,
            id,
            target,
            description,
            prepared,
            approved: false,
            fence: None,
            fence_deadline: None,
        });
        Ok(())
    }

    pub(super) fn review_javascript_input(
        &mut self,
        token: u64,
        approve: bool,
        context: &egui::Context,
    ) {
        context.request_repaint();
        let Some(Phase::Running(running)) = self.javascript.run.as_mut().map(|run| &mut run.phase)
        else {
            return;
        };
        let Some(front) = running
            .inputs
            .front_mut()
            .filter(|input| input.token == token)
        else {
            return;
        };
        if approve {
            if !self.privacy_lost_this_tick {
                front.approved = true;
                front.fence_deadline = Some(Instant::now() + FENCE_RETRY);
            }
            return;
        }
        let input = running.inputs.pop_front().expect("front input exists");
        if self
            .javascript_reply(
                input.id,
                HostResult::Error {
                    code: HostError::Denied,
                },
                None,
            )
            .is_err()
        {
            self.cancel_javascript("Workflow stopped after a worker protocol failure.");
        }
    }

    pub(super) fn cancel_javascript_run(&mut self, token: u64, context: &egui::Context) {
        if self
            .javascript
            .run
            .as_ref()
            .is_some_and(|run| run.token == token)
        {
            self.cancel_javascript("Workflow stopped. No further input was sent.");
        }
        context.request_repaint();
    }

    /// The terminal that needs a fresh input fence this frame, if any.
    pub(crate) fn javascript_input_target(&self) -> Option<TargetRef> {
        let Some(Phase::Running(running)) = self.javascript.run.as_ref().map(|run| &run.phase)
        else {
            return None;
        };
        running
            .inputs
            .front()
            .filter(|input| input.approved)
            .map(|input| input.target)
    }

    fn fail_javascript_input(&mut self, code: HostError, status: &str) {
        let Some(Phase::Running(running)) = self.javascript.run.as_mut().map(|run| &mut run.phase)
        else {
            return;
        };
        let Some(input) = running.inputs.pop_front() else {
            return;
        };
        self.javascript_notice(status);
        if self
            .javascript_reply(input.id, HostResult::Error { code }, None)
            .is_err()
        {
            self.cancel_javascript("Workflow stopped after a worker protocol failure.");
        }
    }

    pub(crate) fn missing_javascript_input_target(&mut self, context: &egui::Context) {
        let guard = CoordinatorUnwind { coordinator: self };
        guard.coordinator.fail_javascript_input(
            HostError::StaleTarget,
            "Workflow input refused: the terminal is no longer open.",
        );
        context.request_repaint();
    }

    /// Mutable phase: capture the human-input fence for the approved input.
    pub(crate) fn prepare_javascript_input(
        &mut self,
        tab: &mut SessionTab,
        context: &egui::Context,
    ) {
        let guard = CoordinatorUnwind { coordinator: self };
        guard
            .coordinator
            .prepare_javascript_input_inner(tab, context);
    }

    fn prepare_javascript_input_inner(&mut self, tab: &mut SessionTab, context: &egui::Context) {
        let Some(target) = self.javascript_input_target() else {
            return;
        };
        let instance = self
            .targets
            .get(&target.session())
            .map(|current| current.controller_instance);
        let fence = match festerm_session::try_automation_admission_lock() {
            Ok(guard) if instance.is_some() && tab.controller.automation_instance() == instance => {
                tab.controller.capture_automation_input_fence_locked(&guard)
            }
            Ok(_) => {
                return self.fail_javascript_input(
                    HostError::StaleTarget,
                    "Workflow input refused: the terminal changed.",
                )
            }
            Err(_) => None,
        };
        let Some(Phase::Running(running)) = self.javascript.run.as_mut().map(|run| &mut run.phase)
        else {
            return;
        };
        let Some(input) = running.inputs.front_mut() else {
            return;
        };
        match fence {
            Some(fence) => input.fence = Some(fence),
            None if input.fence_deadline.is_some_and(|deadline| Instant::now() < deadline) => {
                context.request_repaint_after(Duration::from_millis(50));
            }
            None => self.fail_javascript_input(
                HostError::Denied,
                "Workflow input refused: local input or a paste was still pending. Retry when the terminal is idle.",
            ),
        }
    }

    /// Shared phase: lease, reserve, recheck and commit exactly the approved bytes.
    pub(crate) fn dispatch_javascript_input(&mut self, tab: &SessionTab, context: &egui::Context) {
        let guard = CoordinatorUnwind { coordinator: self };
        guard
            .coordinator
            .dispatch_javascript_input_inner(tab, context);
    }

    fn dispatch_javascript_input_inner(&mut self, tab: &SessionTab, context: &egui::Context) {
        use festerm_session::CommitAutomationInputError;
        let Some(Phase::Running(running)) = self.javascript.run.as_ref().map(|run| &run.phase)
        else {
            return;
        };
        if running
            .inputs
            .front()
            .is_none_or(|input| !input.approved || input.fence.is_none())
        {
            return;
        }
        context.request_repaint();
        let outcome = self.commit_javascript_input(tab);
        let Some(Phase::Running(running)) = self.javascript.run.as_mut().map(|run| &mut run.phase)
        else {
            return;
        };
        let (result, status) = match outcome {
            Ok(()) => (HostResult::Accepted {}, None),
            Err(InputFailure::Retry) => {
                if let Some(input) = running.inputs.front_mut() {
                    input.fence = None;
                }
                context.request_repaint_after(Duration::from_millis(50));
                return;
            }
            Err(InputFailure::Commit(CommitAutomationInputError::LostAfterCommit)) => (
                HostResult::Error {
                    code: HostError::Failed,
                },
                Some("Workflow input outcome is unknown: the terminal may have received it."),
            ),
            Err(InputFailure::Commit(CommitAutomationInputError::Refused(error))) => (
                HostResult::Error {
                    code: host_error(error),
                },
                Some("Workflow input refused: the terminal, input state or approval changed."),
            ),
            Err(InputFailure::Commit(CommitAutomationInputError::ClosedOrExpired)) => (
                HostResult::Error {
                    code: HostError::Expired,
                },
                Some("Workflow input refused: the terminal input reservation expired."),
            ),
        };
        let Some(input) = running.inputs.pop_front() else {
            return;
        };
        if let Some(status) = status {
            self.javascript_notice(status);
        }
        if self.javascript_reply(input.id, result, None).is_err() {
            self.cancel_javascript("Workflow stopped after a worker protocol failure.");
        }
    }

    fn commit_javascript_input(&mut self, tab: &SessionTab) -> Result<(), InputFailure> {
        use festerm_automation::Error;
        use festerm_session::CommitAutomationInputError;
        let refused = |error| InputFailure::Commit(CommitAutomationInputError::Refused(error));
        let retry = |_| InputFailure::Retry;

        let guard = festerm_session::try_automation_admission_lock().map_err(retry)?;
        let run = self.javascript.run.as_mut().ok_or(refused(Error::Closed))?;
        let principal = run.principal;
        let Phase::Running(running) = &mut run.phase else {
            return Err(refused(Error::Closed));
        };
        let input = running.inputs.front().ok_or(refused(Error::Closed))?;
        let target = input.target;
        let fence = input.fence.as_ref().ok_or(InputFailure::Retry)?;
        let current = self
            .targets
            .get(&target.session())
            .filter(|current| current.reference == target)
            .ok_or(refused(Error::StaleTarget))?;
        let live = current.local_pty
            && current.liveness.load(Ordering::Acquire)
            && current.registration.load(Ordering::Acquire)
            && current.attachment.is_current(current.attachment_generation);
        if !run.open.load(Ordering::Acquire) || !self.privacy_gate.try_admit() || !live {
            return Err(refused(Error::Revoked));
        }
        if !fence.is_current()
            || !fence.matches_attachment(
                current.controller_instance,
                &current.attachment,
                current.attachment_generation,
            )
            || tab.controller.automation_instance() != Some(current.controller_instance)
            || tab.terminal.modes() != input.prepared.modes()
        {
            return Err(refused(Error::BindingMismatch));
        }
        let bytes = input.prepared.bytes().to_vec();
        let length = u32::try_from(bytes.len()).map_err(|_| refused(Error::InvalidRequest))?;
        let digest = Digest(Sha256::digest(&bytes).into());
        let broker = self.broker.as_mut().ok_or(refused(Error::Unavailable))?;
        let lease = match running.lease {
            Some(lease) => lease,
            None => {
                let transition = broker
                    .prepare_lease(principal, running.run, target, Instant::now())
                    .map_err(refused)?;
                let lease = broker
                    .commit_lease(transition, Instant::now())
                    .map_err(refused)?;
                running.lease = Some(lease);
                lease
            }
        };
        let ticket = broker
            .prepare_request(
                principal,
                running.run,
                AuthorityOperation::Control {
                    lease,
                    input: digest,
                    bytes: length,
                    presentation: None,
                },
                Instant::now(),
            )
            .map_err(refused)?;
        drop(guard);
        let attempt = (|| {
            let reservation = tab
                .controller
                .reserve_automation_input(&bytes)
                .map_err(|_| refused(Error::Unavailable))?;
            let guard = festerm_session::try_automation_admission_lock().map_err(retry)?;
            let current = self
                .targets
                .get(&target.session())
                .filter(|current| current.reference == target)
                .ok_or(refused(Error::StaleTarget))?;
            let run = self.javascript.run.as_ref().ok_or(refused(Error::Closed))?;
            let Phase::Running(running) = &run.phase else {
                return Err(refused(Error::Closed));
            };
            let fence = running
                .inputs
                .front()
                .and_then(|input| input.fence.as_ref())
                .ok_or(refused(Error::Closed))?;
            if !run.open.load(Ordering::Acquire)
                || !fence.is_current()
                || !reservation.matches_attachment(
                    current.controller_instance,
                    &current.attachment,
                    current.attachment_generation,
                    &guard,
                )
                || tab.terminal.modes() != running.inputs[0].prepared.modes()
            {
                return Err(refused(Error::BindingMismatch));
            }
            let privacy = self.privacy_gate.clone();
            let broker = self.broker.as_mut().ok_or(refused(Error::Unavailable))?;
            let committed = running
                .supervisor
                .with_admission(|permit| {
                    permit.commit(|now| {
                        reservation.commit_with(|committed| {
                            if committed != bytes.as_slice() {
                                return Err(Error::BindingMismatch);
                            }
                            if !privacy.try_admit() {
                                return Err(Error::Revoked);
                            }
                            broker.commit_control(principal, ticket, digest, length, now)
                        })
                    })
                })
                .map_err(|_| refused(Error::Closed))?
                .map_err(|_| refused(Error::Closed))?;
            drop(guard);
            committed.map_err(InputFailure::Commit)
        })();
        if let Some(broker) = self.broker.as_mut() {
            let _ = broker.settle_request(principal, ticket);
        }
        attempt
    }

    pub(super) fn javascript_view(&self, apps: &[&FesTermApp]) -> View {
        let monitoring = self.javascript_monitoring();
        let targets = if monitoring == Monitoring::Ready && self.javascript_enabled {
            self.targets
                .values()
                .filter(|target| {
                    target.local_pty
                        && target.liveness.load(Ordering::Acquire)
                        && target.registration.load(Ordering::Acquire)
                })
                .filter_map(|target| {
                    Some((target.reference, target_label(apps, target.reference)?))
                })
                .collect()
        } else {
            Vec::new()
        };
        let run = self.javascript.run.as_ref().map(|run| {
            let (starting, input, queued_inputs) = match &run.phase {
                Phase::Starting(_) => (true, None, 0),
                Phase::Running(running) => (
                    false,
                    running.inputs.front().map(|input| InputView {
                        token: input.token,
                        target: target_label(apps, input.target)
                            .unwrap_or_else(|| run.target_label.clone()),
                        description: input.description.clone(),
                        bytes: input.prepared.bytes().len(),
                        approved: input.approved,
                    }),
                    running.inputs.len(),
                ),
            };
            RunView {
                token: run.token,
                name: run.name.clone(),
                target: run.target_label.clone(),
                starting,
                input,
                queued_inputs,
                notice: run.notice.clone(),
            }
        });
        View {
            monitoring,
            targets,
            run,
            outcome: self.javascript.outcome.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn javascript_run_active_for_test(&self) -> bool {
        self.javascript.run.is_some()
    }
}

enum InputFailure {
    Retry,
    Commit(festerm_session::CommitAutomationInputError<festerm_automation::Error>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_core::Dimensions;

    fn terminal(columns: usize, rows: usize) -> Terminal {
        Terminal::new(Dimensions::new(columns, rows).unwrap()).unwrap()
    }

    #[test]
    fn screen_text_trims_rows_and_respects_utf8_byte_bounds() {
        let mut terminal = terminal(10, 3);
        terminal.ingest("héllo  \r\nworld".as_bytes());
        assert_eq!(screen_text(&terminal, 1024), ("héllo\nworld".into(), false));
        let (text, truncated) = screen_text(&terminal, 2);
        assert_eq!((text.as_str(), truncated), ("h", true));
    }

    #[test]
    fn scrollback_text_keeps_the_newest_lines_and_bytes() {
        let mut terminal = terminal(10, 2);
        terminal.ingest(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        let (all, truncated) = scrollback_text(&terminal, 1000, 1024);
        assert!(!truncated);
        assert!(all.ends_with("three\n"), "{all:?}");
        let (last, _) = scrollback_text(&terminal, 1, 1024);
        assert_eq!(last, "three\n");
        let (tail, truncated) = scrollback_text(&terminal, 1000, 4);
        assert_eq!((tail.as_str(), truncated), ("ree\n", true));
    }

    fn example_bundle() -> festerm_automation_js::snapshot::ApprovedBundle {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/examples/hello-terminal-workflow.json")
            .canonicalize()
            .unwrap();
        #[cfg(windows)]
        let path = {
            let path = path.to_str().unwrap();
            std::path::PathBuf::from(path.strip_prefix(r"\\?\").unwrap_or(path))
        };
        festerm_automation_workflow::intake::load_user_selected_bundle(&path).unwrap()
    }

    fn run_fixture() -> (
        Coordinator,
        FesTermApp,
        crate::tabs::TabId,
        crate::session_controller::fake::FakeSshSession,
        TargetRef,
        egui::Context,
    ) {
        let (mut app, id) = FesTermApp::automation_fixture();
        let fake = crate::session_controller::fake::FakeSshSession::new([]);
        fake.enable_ordered_test_input();
        let session = app.automation_session_mut(id);
        session.controller = crate::session_controller::SessionController::for_test(
            crate::tabs::ApplicationSession::TestLocal(fake.clone()),
        );
        session
            .controller
            .set_lifecycle_for_test(festerm_session::SessionLifecycle::Running);
        let ime = Arc::new(festerm_session::AutomationEpoch::new());
        ime.transition(true);
        app.set_automation_ime_epoch(&ime);
        std::mem::forget(ime);
        let mut coordinator = Coordinator::default();
        coordinator.stop = Arc::new(AtomicBool::new(false));
        coordinator.privacy_gate = Arc::new(PrivacyGate::new(false));
        let mut broker = Broker::new([2; 16]);
        coordinator.reconcile(&mut broker, &[&app]);
        coordinator.broker = Some(broker);
        coordinator.javascript_enabled = true;
        let target = coordinator.targets[&id.chip_id()].reference;
        assert!(coordinator.targets[&id.chip_id()].local_pty);
        let context = egui::Context::default();
        coordinator
            .pending_workflow_capture_for_test()
            .send(Ok(example_bundle()))
            .unwrap();
        coordinator.workflow.poll(&context);
        assert!(coordinator.workflow.view().current_bundle().is_some());
        (coordinator, app, id, fake, target, context)
    }

    fn parameters(target: bool) -> Parameters {
        let mut parameters = Parameters::new();
        parameters.insert(
            "greeting".into(),
            festerm_automation_js::snapshot::ParameterValue::Text("hi".into()),
        );
        if target {
            parameters.insert(
                "terminal".into(),
                festerm_automation_js::snapshot::ParameterValue::Target(TARGET_ID.into()),
            );
        }
        parameters
    }

    #[test]
    fn start_refuses_stale_snapshots_missing_targets_and_closed_privacy() {
        let (mut coordinator, app, _id, _fake, target, context) = run_fixture();
        let digest = example_bundle().digest().to_owned();
        coordinator.start_javascript_run(
            "0".repeat(64),
            Some(target),
            parameters(true),
            &[&app],
            &context,
        );
        assert!(!coordinator.javascript_run_active_for_test());
        coordinator.start_javascript_run(digest.clone(), None, parameters(true), &[&app], &context);
        assert!(!coordinator.javascript_run_active_for_test());
        coordinator.start_javascript_run(
            digest.clone(),
            Some(target),
            parameters(false),
            &[&app],
            &context,
        );
        assert!(!coordinator.javascript_run_active_for_test());
        coordinator.privacy_gate.signal_loss();
        coordinator.start_javascript_run(digest, Some(target), parameters(true), &[&app], &context);
        assert!(!coordinator.javascript_run_active_for_test());
    }

    #[test]
    fn concurrent_workflow_approvals_require_a_safety_stop() {
        let start = NativeCommand::StartWorkflowRun {
            digest: String::new(),
            target: None,
            parameters: Parameters::new(),
        };
        let approve = NativeCommand::ApproveWorkflowInput { token: 1 };
        assert!(NativeCommand::batch_requires_safety_stop(
            [start.clone(), approve.clone()].iter()
        ));
        assert!(!NativeCommand::batch_requires_safety_stop([start].iter()));
        assert!(!NativeCommand::batch_requires_safety_stop([approve].iter()));
    }

    /// Runs the example bundle in a real contained worker. Build the app first,
    /// then run with FESTERM_TEST_JS_WORKER=<absolute path to target/debug/festerm>.
    #[test]
    #[ignore = "needs FESTERM_TEST_JS_WORKER pointing at a built festerm binary"]
    fn example_workflow_reads_and_sends_only_approved_input_through_a_real_worker() {
        let (mut coordinator, mut app, id, fake, target, context) = run_fixture();
        let digest = example_bundle().digest().to_owned();
        coordinator.start_javascript_run(digest, Some(target), parameters(true), &[&app], &context);
        assert!(
            coordinator.javascript_run_active_for_test(),
            "{:?}",
            coordinator.javascript.outcome
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut approved = 0;
        while coordinator.javascript_run_active_for_test() {
            assert!(Instant::now() < deadline, "workflow did not finish");
            coordinator.drive_javascript(&[&app], &context);
            let view = coordinator.javascript_view(&[&app]);
            if let Some(input) = view
                .run
                .and_then(|run| run.input)
                .filter(|input| !input.approved)
            {
                assert!(
                    input.description.contains("echo hi") || input.description == "Keys: Enter"
                );
                coordinator.review_javascript_input(input.token, true, &context);
                approved += 1;
            }
            if coordinator.javascript_input_target() == Some(target) {
                coordinator.prepare_javascript_input(app.automation_session_mut(id), &context);
                coordinator
                    .dispatch_javascript_input(app.automation_session(id).unwrap(), &context);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(approved, 2, "{:?}", coordinator.javascript.outcome);
        assert_eq!(
            coordinator.javascript.outcome.as_deref(),
            Some("Workflow completed. Its return value is not shown in this beta.")
        );
        assert_eq!(
            fake.drain_ordered_test_input(),
            [b"echo hi".to_vec(), b"\r".to_vec()]
        );
    }

    #[test]
    fn input_problem_notices_stay_visible_and_survive_cancellation() {
        let (mut coordinator, app, _id, _fake, target, context) = run_fixture();
        let digest = example_bundle().digest().to_owned();
        coordinator.start_javascript_run(digest, Some(target), parameters(true), &[&app], &context);
        assert!(coordinator.javascript_run_active_for_test());
        let notice = "Workflow input outcome is unknown: the terminal may have received it.";
        coordinator.javascript_notice(notice);
        let view = coordinator.javascript_view(&[&app]);
        assert_eq!(view.run.and_then(|run| run.notice).as_deref(), Some(notice));
        coordinator.cancel_javascript("Workflow stopped.");
        let outcome = coordinator.javascript.outcome.clone().unwrap();
        assert!(outcome.starts_with("Workflow stopped.") && outcome.contains(notice));
    }

    #[test]
    fn input_descriptions_escape_controls_and_invisible_formatting() {
        assert_eq!(
            describe_text("ls\r\u{202e}\"x\""),
            "Text: \"ls\\r\\u{202e}\\\"x\\\"\""
        );
        assert_eq!(
            describe_keys(&["Ctrl+C".into(), "Enter".into()]),
            "Keys: Ctrl+C Enter"
        );
    }
}
