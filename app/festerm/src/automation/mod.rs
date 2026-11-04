mod ipc;
mod observation;
pub(crate) mod presentation;
#[cfg(test)]
mod tests;
mod workflow_selection;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use eframe::egui;
use festerm_automation::{
    Broker, Budget, Completion, ConnectionId, Digest, DirectKind, GrantRequest, Invocation,
    InvocationBinding, NativeApproval, Operation as AuthorityOperation, RequestId, RunId, RunSpec,
    Scope, TargetRef,
};
use festerm_automation_mcp::{
    BackendRequest, BackendResult, Event, Machine, Operation, PairingStatus, Refusal, RequestKey,
    Response, VerificationCode,
};
use festerm_core::ScrollbackStats;
use sha2::{Digest as _, Sha256};

use crate::{
    app::FesTermApp,
    tabs::{SessionTab, TabId},
};

#[cfg(target_os = "macos")]
type PrivacyMonitor = festerm_macos_window::PrivacyMonitor;
#[cfg(windows)]
type PrivacyMonitor = festerm_windows_power::PrivacyMonitor;
#[cfg(target_os = "linux")]
type PrivacyMonitor = festerm_linux_power::PrivacyMonitor;

pub(crate) use ipc::run_stdio;
pub(crate) use presentation::WorkspaceRevision;
pub(crate) use workflow_selection::{Status as WorkflowStatus, View as WorkflowView};

#[derive(Clone)]
pub(crate) struct SelectedWorkflow(pub(crate) std::path::PathBuf);

impl std::fmt::Debug for SelectedWorkflow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SelectedWorkflow(<private path>)")
    }
}

#[derive(Clone, Debug)]
pub(crate) enum NativeCommand {
    RestartMcp,
    ApproveObservation {
        channel: u64,
        targets: Vec<TargetRef>,
    },
    Revoke {
        channel: u64,
    },
    CaptureWorkflow(SelectedWorkflow),
    ClearWorkflow,
}

#[derive(Clone)]
pub(crate) struct ChannelRow {
    pub channel: u64,
    pub verification_code: String,
    pub approved: bool,
    pub approved_targets: Vec<TargetRef>,
    closed: Arc<AtomicBool>,
}

#[derive(Clone)]
pub(crate) struct SettingsSnapshot {
    pub status: &'static str,
    pub channels: Vec<ChannelRow>,
    pub targets: Vec<(TargetRef, String)>,
    pub workflow: WorkflowView,
    stop: Arc<AtomicBool>,
}

impl Default for SettingsSnapshot {
    fn default() -> Self {
        Self {
            status: "MCP is off. No local adapter is listening.",
            channels: Vec::new(),
            targets: Vec::new(),
            workflow: WorkflowView::default(),
            stop: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl SettingsSnapshot {
    pub(crate) fn same_presentation(&self, other: &Self) -> bool {
        self.status == other.status
            && self.targets == other.targets
            && self.workflow.same_presentation(&other.workflow)
            && self.channels.len() == other.channels.len()
            && self
                .channels
                .iter()
                .zip(&other.channels)
                .all(|(left, right)| {
                    left.channel == right.channel
                        && left.verification_code == right.verification_code
                        && left.approved == right.approved
                        && left.approved_targets == right.approved_targets
                })
    }

    pub(crate) fn disable(&self) {
        set_admission_flag(&self.stop, true);
    }

    pub(crate) fn revoke(&self, channel: u64) {
        if let Some(row) = self.channels.iter().find(|row| row.channel == channel) {
            set_admission_flag(&row.closed, true);
        }
    }

    #[cfg(test)]
    pub(crate) fn with_pending_channel_for_test(channel: u64) -> Self {
        Self {
            channels: vec![ChannelRow {
                channel,
                verification_code: "0123456789abcdef".into(),
                approved: false,
                approved_targets: Vec::new(),
                closed: Arc::new(AtomicBool::new(false)),
            }],
            ..Self::default()
        }
    }
}

pub(crate) fn set_admission_flag(flag: &AtomicBool, value: bool) {
    let guard = festerm_session::automation_admission_lock();
    if guard.is_none() {
        tracing::error!("automation admission lock poisoned; disclosure remains closed");
    }
    flag.store(value, Ordering::Release);
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
}

struct Peer {
    connection: ipc::Connection,
    principal: ConnectionId,
    machine: Machine,
    code: VerificationCode,
    grant: Option<Grant>,
    waits: Vec<Wait>,
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

// Privacy loss and adapter-stop replacement share the write-admission lock.
struct PrivacyGate {
    closed: AtomicBool,
    mcp_stop: Mutex<Option<Arc<AtomicBool>>>,
}

impl PrivacyGate {
    fn new(closed: bool) -> Self {
        Self {
            closed: AtomicBool::new(closed),
            mcp_stop: Mutex::new(None),
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn close(&self) {
        let guard = festerm_session::automation_admission_lock();
        if guard.is_none() {
            tracing::error!("automation admission lock poisoned; disclosure remains closed");
        }
        self.closed.store(true, Ordering::Release);
        let stop = self
            .mcp_stop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(stop) = stop.as_ref() {
            stop.store(true, Ordering::Release);
        }
    }

    fn bind_mcp(&self, stop: &Arc<AtomicBool>) -> bool {
        let Some(_guard) = festerm_session::automation_admission_lock() else {
            self.close();
            stop.store(true, Ordering::Release);
            return false;
        };
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
}

fn prepare_shared<P>(
    gate: &PrivacyGate,
    install: impl FnOnce() -> Result<P, StartFailure>,
    nonce: impl FnOnce() -> Result<[u8; 16], StartFailure>,
) -> Result<(P, [u8; 16]), StartFailure> {
    let result = (|| {
        let privacy = install()?;
        if gate.is_closed() {
            return Err(StartFailure::Cancelled);
        }
        let instance = nonce()?;
        if gate.is_closed() {
            return Err(StartFailure::Cancelled);
        }
        Ok((privacy, instance))
    })();
    if result.is_err() {
        gate.close();
    }
    result
}

pub(crate) struct Coordinator {
    workflow: workflow_selection::Selection,
    javascript_enabled: bool,
    presentation_workspace: Option<WorkspaceRevision>,
    control_handoffs: BTreeMap<ConnectionId, presentation::BoundControlHandoff>,
    pending_control_reviews:
        BTreeMap<festerm_automation::ApprovalId, presentation::BoundControlReview>,
    metadata_grants: BTreeMap<ConnectionId, presentation::BoundMetadataGrant>,
    pending_metadata_reviews:
        BTreeMap<festerm_automation::ApprovalId, presentation::BoundMetadataReview>,
    broker: Option<Broker>,
    server: Option<ipc::Server>,
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
    status: &'static str,
    peers: BTreeMap<u64, Peer>,
    targets: BTreeMap<u64, Target>,
    generation: u64,
}

impl Default for Coordinator {
    fn default() -> Self {
        Self {
            workflow: workflow_selection::Selection::default(),
            javascript_enabled: false,
            presentation_workspace: None,
            control_handoffs: BTreeMap::new(),
            pending_control_reviews: BTreeMap::new(),
            metadata_grants: BTreeMap::new(),
            pending_metadata_reviews: BTreeMap::new(),
            broker: None,
            server: None,
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
            status: SettingsSnapshot::default().status,
            peers: BTreeMap::new(),
            targets: BTreeMap::new(),
            generation: 0,
        }
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
    let mut characters = value.chars();
    let mut label = String::new();
    for character in characters.by_ref().take(128) {
        if character.is_control()
            || matches!(
                character,
                '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
                    | '\u{2060}'..='\u{206f}' | '\u{feff}'
            )
        {
            label.extend(character.escape_unicode());
        } else {
            label.push(character);
        }
    }
    if characters.next().is_some() {
        label.push_str("...");
    }
    label
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

fn retire_target(broker: &mut Broker, target: Target) {
    set_admission_flag(&target.registration, false);
    if broker.close_target(target.reference).is_err() {
        tracing::warn!("automation target closure refused");
    } else if broker.forget_closed_target(target.reference).is_err() {
        tracing::warn!("automation target retirement refused");
    }
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

fn start_failure_status(failure: StartFailure) -> &'static str {
    match failure {
        StartFailure::Privacy => {
            "MCP unavailable: no qualified OS lock/suspend privacy monitor."
        }
        StartFailure::PeerIdentity => {
            "MCP unavailable: private peer identity is not yet qualified."
        }
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
    fn stop_mcp(&mut self) {
        self.want_start = false;
        set_admission_flag(&self.stop, true);
        self.control_handoffs.clear();
        self.pending_control_reviews.clear();
        self.metadata_grants.clear();
        self.pending_metadata_reviews.clear();
        for (_, mut peer) in std::mem::take(&mut self.peers) {
            peer.connection.close();
            peer.machine.close();
            if let Some(broker) = &mut self.broker {
                broker.disconnect(peer.principal);
            }
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
        self.privacy_gate.close();
        if let Some(broker) = &mut self.broker {
            broker.revoke_all();
        }
        self.control_handoffs.clear();
        self.pending_control_reviews.clear();
        self.metadata_grants.clear();
        self.pending_metadata_reviews.clear();
        for target in self.targets.values() {
            set_admission_flag(&target.registration, false);
        }
    }

    fn shutdown(&mut self) {
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
                    let startup_ok = starting.is_none_or(|starting| {
                        drop(starting.result);
                        if starting.thread.join().is_err() {
                            tracing::warn!("automation startup worker failed");
                            false
                        } else {
                            true
                        }
                    });
                    let _privacy = privacy;
                    startup_ok
                },
            );
        }
    }

    fn start(&mut self, context: &egui::Context) {
        self.restart_with_bootstrap(context, ipc::require_qualified_bootstrap());
    }

    fn restart_with_bootstrap(&mut self, context: &egui::Context, bootstrap: std::io::Result<()>) {
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

    fn mcp_failed(&mut self) {
        self.stop_mcp();
        self.status =
            "MCP stopped after transport failure. Restart explicitly; its grants were revoked.";
    }

    fn shared_start_failed(&mut self, failure: StartFailure) {
        self.privacy_lost_this_tick = true;
        self.privacy_gate.close();
        if self.want_start {
            self.stop_mcp();
            self.status = start_failure_status(failure);
            tracing::warn!("{}", self.status);
        }
    }

    fn advance_shared(&mut self, context: &egui::Context) {
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
                if self.want_start {
                    self.stop_mcp();
                    self.status = "MCP unavailable: startup worker failed.";
                    tracing::warn!("{}", self.status);
                }
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
            self.want_shared_start = false;
            self.privacy_gate = Arc::new(PrivacyGate::new(false));
            if festerm_session::automation_admission_lock().is_none() {
                self.shared_start_failed(StartFailure::Privacy);
                return;
            }
            if self.want_start
                && !self.stop.load(Ordering::Acquire)
                && !self.privacy_gate.bind_mcp(&self.stop)
                && self.privacy_gate.is_closed()
            {
                self.shared_start_failed(StartFailure::Privacy);
                return;
            }
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
                            PrivacyMonitor::install(Arc::new(move || {
                                privacy_gate.close();
                                privacy_notify.request_repaint();
                            }))
                            .map_err(|_| StartFailure::Privacy)
                        },
                        || random().map_err(|_| StartFailure::Randomness),
                    )
                    .map(|(privacy, instance)| Prepared { privacy, instance });
                    let _ = send.send(prepared);
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
            if !self.privacy_gate.bind_mcp(&self.stop) {
                if self.privacy_gate.is_closed() {
                    self.privacy_lost();
                } else {
                    self.stop_mcp();
                    self.status = start_failure_status(StartFailure::Cancelled);
                }
                return;
            }
            let stop = self.stop.clone();
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
        self.advance_shared(context);
        if self.broker.is_some() && self.privacy_gate.is_closed() {
            self.privacy_lost();
            return;
        }
        self.advance_mcp(context);
    }

    pub(crate) fn tick(&mut self, context: &egui::Context, apps: &[&FesTermApp]) {
        self.privacy_lost_this_tick = false;
        let javascript_enabled =
            !apps.is_empty() && apps.iter().all(|app| app.automation_javascript_enabled());
        if self.javascript_enabled && !javascript_enabled {
            self.workflow.cancel();
        }
        self.javascript_enabled = javascript_enabled;
        self.workflow.poll(context);
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
        if self.server.as_ref().is_some_and(ipc::Server::failed) {
            self.mcp_failed();
        }
        let Some(mut broker) = self.broker.take() else {
            return;
        };
        let now = Instant::now();
        broker.poll_expiry(now);
        self.reconcile(&mut broker, apps);
        self.metadata_grants.retain(|principal, bound| {
            broker.native_metadata_grant(*principal, now) == Ok(bound.grant)
        });
        self.pending_metadata_reviews
            .retain(|id, review| broker.pending_approval_is_active(review.principal, *id, now));
        if self.privacy_gate.is_closed() {
            self.broker = Some(broker);
            self.privacy_lost();
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
            if let Some(mut peer) = self.peers.remove(&key) {
                peer.connection.close();
                peer.machine.close();
                broker.disconnect(peer.principal);
                self.control_handoffs.remove(&peer.principal);
                self.pending_control_reviews
                    .retain(|_, review| review.principal != peer.principal);
                self.metadata_grants.remove(&peer.principal);
                self.pending_metadata_reviews
                    .retain(|_, review| review.principal != peer.principal);
            }
        }
        for _ in 0..32 {
            let Some(event) = self.server.as_ref().and_then(ipc::Server::try_recv) else {
                break;
            };
            match event {
                ipc::Event::Connected(connection) => {
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
                            self.peers.insert(
                                connection.key,
                                Peer {
                                    connection,
                                    principal,
                                    machine: Machine::with_unavailable_presentation(),
                                    code,
                                    grant: None,
                                    waits: Vec::new(),
                                },
                            );
                        }
                        (Ok(principal), _) => {
                            broker.disconnect(principal);
                            connection.close();
                            tracing::warn!("automation connection identity refused");
                        }
                        _ => connection.close(),
                    }
                }
                ipc::Event::Message { key, bytes } => {
                    if let Some(peer) = self.peers.get_mut(&key) {
                        if peer.connection.is_closed() || self.stop.load(Ordering::Acquire) {
                            continue;
                        }
                        match peer.machine.process_bytes(&bytes) {
                            Event::Reply(response) => send(peer, response, now, false),
                            Event::Backend(request) => {
                                execute(&mut broker, &self.targets, peer, request, apps, now);
                            }
                            Event::Cancelled(key) => cancel_wait(&mut broker, peer, &key, now),
                            Event::NoReply => {}
                        }
                    }
                }
            }
        }
        for peer in self.peers.values_mut() {
            settle_waits(&mut broker, &self.targets, peer, now);
            for wait in &peer.waits {
                context.request_repaint_after(wait.deadline.saturating_duration_since(now));
            }
            if let Some(grant) = &peer.grant {
                context.request_repaint_after(grant.expires.saturating_duration_since(now));
            }
        }
        self.broker = Some(broker);
        if self.privacy_gate.is_closed() {
            self.privacy_lost();
        }
    }

    fn reconcile(&mut self, broker: &mut Broker, apps: &[&FesTermApp]) {
        let mut current = BTreeSet::new();
        let mut candidates = Vec::new();
        for (id, session) in apps.iter().flat_map(|app| app.automation_sessions()) {
            let Some(controller_instance) = session.controller.automation_instance() else {
                continue;
            };
            let liveness = session.controller.automation_liveness();
            let Some(attachment) = session.controller.automation_attachment_epoch() else {
                continue;
            };
            let Some(attachment_generation) = attachment.capture() else {
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
            current.insert(id);
            candidates.push((
                id,
                session,
                controller_instance,
                liveness,
                attachment,
                attachment_generation,
            ));
        }
        let stale: Vec<_> = self
            .targets
            .keys()
            .filter(|id| !current.contains(id))
            .copied()
            .collect();
        for id in stale {
            if let Some(target) = self.targets.remove(&id) {
                retire_target(broker, target);
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
            let local_pty = matches!(
                session.controller.session(),
                Some(crate::tabs::ApplicationSession::Local(_))
            );
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
                        retire_target(broker, target);
                    }
                }
            } else {
                if let Some(target) = self.targets.remove(&id) {
                    retire_target(broker, target);
                }
                let Some(next) = self.generation.checked_add(1) else {
                    self.privacy_gate.close();
                    tracing::warn!("automation target generations exhausted");
                    return;
                };
                self.generation = next;
                match broker.attach_target(id, next) {
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
                        if !registration_refused && self.status != REGISTRATION_REFUSED {
                            tracing::warn!("automation target registration refused");
                        }
                        registration_refused = true;
                    }
                }
            }
        }
        if self.server.is_some() && !self.stop.load(Ordering::Acquire) {
            if registration_refused {
                self.status = REGISTRATION_REFUSED;
            } else if self.status == REGISTRATION_REFUSED {
                self.status =
                    "MCP observation-only listener active. No terminal access until native approval.";
            }
        }
    }

    pub(crate) fn command(&mut self, command: NativeCommand, context: &egui::Context) {
        if self.privacy_gate.is_closed() && (self.broker.is_some() || self.starting.is_some()) {
            self.privacy_lost();
        }
        match command {
            NativeCommand::RestartMcp if self.enabled && !self.privacy_lost_this_tick => {
                self.start(context);
            }
            NativeCommand::RestartMcp => {}
            NativeCommand::Revoke { channel } => {
                if let Some(peer) = self.peers.get(&channel) {
                    peer.connection.close();
                }
            }
            NativeCommand::ApproveObservation { .. } if self.privacy_lost_this_tick => {}
            NativeCommand::ApproveObservation { channel, targets } => {
                if self.approve(channel, targets).is_err() {
                    self.status = "MCP approval refused: channel, target, lifetime or authority limit changed. No access was granted.";
                    tracing::warn!("automation native observation approval refused");
                }
            }
            NativeCommand::CaptureWorkflow(SelectedWorkflow(path)) => {
                if self.javascript_enabled {
                    self.workflow.capture(path, context);
                } else {
                    tracing::warn!("workflow selection refused while JavaScript is disabled");
                    self.workflow
                        .refuse("Enable JavaScript workflows in all windows before selecting.");
                }
            }
            NativeCommand::ClearWorkflow => self.workflow.cancel(),
        }
        context.request_repaint();
    }

    fn approve(&mut self, channel: u64, targets: Vec<TargetRef>) -> Result<(), Refusal> {
        if self.stop.load(Ordering::Acquire)
            || self.privacy_gate.is_closed()
            || targets.is_empty()
            || targets.len() > 32
        {
            return Err(Refusal::NotAuthorized);
        }
        let peer = self.peers.get_mut(&channel).ok_or(Refusal::Closed)?;
        if peer.connection.is_closed() || peer.grant.is_some() {
            return Err(Refusal::Closed);
        }
        let now = Instant::now();
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
                    || target.attachment.capture() != Some(target.attachment_generation)
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
        let status = if privacy_closed && self.broker.is_some() && self.enabled {
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
            .filter_map(|target| {
                apps.iter().enumerate().find_map(|(window, app)| {
                    let session =
                        app.automation_session(TabId::from_chip_id(target.reference.session()))?;
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
                    Some((
                        target.reference,
                        format!(
                            "Window {}: {} - {} (tab {}, attachment {})",
                            window + 1,
                            name,
                            detail,
                            target.reference.session(),
                            target.reference.generation(),
                        ),
                    ))
                })
            })
            .collect();
        SettingsSnapshot {
            status,
            channels,
            targets,
            workflow: self.workflow.view(),
            stop: self.stop.clone(),
        }
    }
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
    }
}

fn finish_run(broker: &mut Broker, peer: &Peer, run: RunId, now: Instant) {
    let _ = broker.complete(peer.principal, run, Completion::Returned, now);
    if broker.forget_run(peer.principal, run).is_err() {
        tracing::debug!("automation closed context could not be forgotten");
    }
}

fn cancel_wait(broker: &mut Broker, peer: &mut Peer, key: &RequestKey, now: Instant) {
    if let Some(index) = peer.waits.iter().position(|wait| wait.key == *key) {
        let wait = peer.waits.remove(index);
        let _ = broker.cancel_run(peer.principal, wait.run, now);
        let _ = broker.forget_run(peer.principal, wait.run);
    }
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
        });
        return;
    }
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
    let _ = broker.settle_request(peer.principal, ticket);
    finish_run(broker, peer, run, Instant::now());
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
        | Operation::ControllerRegister { .. }
        | Operation::ControllerStatus { .. }
        | Operation::ControllerUnregister { .. } => return Err(Refusal::Unavailable),
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
    peer: &Peer,
    operation: &Operation,
    now: Instant,
) -> Result<(RunId, RequestId, Option<TargetRef>), Refusal> {
    if matches!(
        operation,
        Operation::ControllerCapabilities
            | Operation::ControllerRegister { .. }
            | Operation::ControllerStatus { .. }
            | Operation::ControllerUnregister { .. }
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
        | Operation::ControllerRegister { .. }
        | Operation::ControllerStatus { .. }
        | Operation::ControllerUnregister { .. } => return Err(Refusal::Unavailable),
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
            finish_run(broker, peer, run, now);
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
) {
    let mut index = 0;
    while index < peer.waits.len() {
        let wait = &peer.waits[index];
        let target = targets
            .get(&wait.target.session())
            .filter(|target| target.reference == wait.target);
        if target.is_some_and(|target| target.revision == wait.after_revision)
            && now < wait.deadline
        {
            index += 1;
            continue;
        }
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
        let _ = broker.settle_request(peer.principal, wait.request);
        finish_run(broker, peer, wait.run, Instant::now());
    }
}
