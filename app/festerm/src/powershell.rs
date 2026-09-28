use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui;
use festerm_config::{LocalBindPolicy, PowerShellProfileConfiguration};
use festerm_powershell::{
    PipelineEvent, PipelineState, PowerShellAuthMethod, PowerShellCommand,
    PowerShellCommandFailureKind, PowerShellCommandMessage, PowerShellCredentials,
    PowerShellEndpoint, PowerShellOptions, PowerShellSession, PowerShellSessionError, PsValue,
};
use festerm_secret_store::{SecretBytes, SecretReference, SecretStore};
use festerm_ui_egui::theme;

use crate::local_bind::{LocalBindDraft, ResolvedLocalBind};

const MAX_PRESENTED_EVENTS: usize = 512;
const MAX_DISPLAY_CHARS: usize = 8 * 1024;
const MAX_PRESENTATION_BYTES: usize = 256 * 1024;
const MAX_VALUE_NODES: usize = 512;
const MAX_VALUE_DEPTH: usize = 8;
const MAX_SCRIPT_BYTES: usize = 64 * 1024;
const MAX_PASSWORD_BYTES: usize = 16 * 1024;
const MAX_FRAME_MESSAGES: usize = 64;
#[allow(dead_code)]
const MAX_TRUSTED_CA_PEM_BYTES: usize = 1024 * 1024;
#[allow(dead_code)]
const WORKER_QUEUE: usize = 128;
const CONNECTED_STATUS: &str =
    "Experimental HTTPS/NTLM PSRP. TLS verification stays enabled; NTLM is not Entra-compatible.";

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PowerShellState {
    Setup,
    Connecting,
    Ready,
    Running,
    Cancelling,
    Failed,
    Closing,
    Closed,
}

impl PowerShellState {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Setup => "PowerShell setup",
            Self::Connecting => "Connecting",
            Self::Ready => "Ready",
            Self::Running => "Running",
            Self::Cancelling => "Cancelling",
            Self::Failed => "Failed",
            Self::Closing => "Closing",
            Self::Closed => "Closed",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PowerShellStream {
    Lifecycle,
    Output,
    Error,
    Warning,
    Verbose,
    Debug,
    Information,
    Progress,
    State,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PowerShellPresentedEvent {
    pub(crate) stream: PowerShellStream,
    pub(crate) text: String,
    pub(crate) expandable: Vec<(String, String)>,
}

pub(crate) enum PowerShellAction {
    Connect,
    Run(String),
    Cancel,
    Close,
}

impl std::fmt::Debug for PowerShellAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect => f.write_str("Connect"),
            Self::Run(script) => f
                .debug_tuple("Run")
                .field(&format_args!("<{} bytes>", script.len()))
                .finish(),
            Self::Cancel => f.write_str("Cancel"),
            Self::Close => f.write_str("Close"),
        }
    }
}

impl PowerShellPresentedEvent {
    fn lifecycle(text: impl Into<String>) -> Self {
        Self {
            stream: PowerShellStream::Lifecycle,
            text: text.into(),
            expandable: Vec::new(),
        }
    }

    fn presentation_bytes(&self) -> usize {
        self.text.len()
            + self
                .expandable
                .iter()
                .map(|(name, value)| name.len().saturating_add(value.len()))
                .sum::<usize>()
    }
}

struct WorkerControl {
    cancel_requested: AtomicBool,
    close_requested: AtomicBool,
    mailbox_drops: AtomicUsize,
    finished: AtomicBool,
    pending_lifecycle: Mutex<VecDeque<WorkerMessage>>,
}

impl WorkerControl {
    #[allow(dead_code)]
    fn new() -> Self {
        Self {
            cancel_requested: AtomicBool::new(false),
            close_requested: AtomicBool::new(false),
            mailbox_drops: AtomicUsize::new(0),
            finished: AtomicBool::new(false),
            pending_lifecycle: Mutex::new(VecDeque::new()),
        }
    }
}

#[derive(Clone)]
pub(crate) struct PowerShellShutdownHandle {
    control: Arc<WorkerControl>,
    sender: Option<SyncSender<WorkerRequest>>,
}

impl PowerShellShutdownHandle {
    pub(crate) fn request_shutdown(&self) {
        self.control.close_requested.store(true, Ordering::Release);
        self.control.cancel_requested.store(true, Ordering::Release);
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(WorkerRequest::Close);
        }
    }

    pub(crate) fn request_close(&self) {
        self.request_shutdown();
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.control.finished.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn pending_for_test() -> Self {
        Self {
            control: Arc::new(WorkerControl::new()),
            sender: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn finish_for_test(&self) {
        self.control.finished.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn close_requested_for_test(&self) -> bool {
        self.control.close_requested.load(Ordering::Acquire)
    }
}

struct WorkerFinishedGuard(Arc<WorkerControl>, egui::Context);

impl Drop for WorkerFinishedGuard {
    fn drop(&mut self) {
        self.0.finished.store(true, Ordering::Release);
        self.1.request_repaint();
    }
}

pub(crate) struct PowerShellTab {
    profile: PowerShellProfileConfiguration,
    state: PowerShellState,
    password: String,
    source: LocalBindDraft,
    command: String,
    events: VecDeque<PowerShellPresentedEvent>,
    presentation_bytes: usize,
    evicted_events: usize,
    worker_mailbox_drops: usize,
    sender: Option<SyncSender<WorkerRequest>>,
    receiver: Option<Receiver<WorkerMessage>>,
    control: Option<Arc<WorkerControl>>,
    worker_generation: u64,
    pending_close: bool,
    input_feedback: Option<String>,
}

impl PowerShellTab {
    pub(crate) fn new(profile: PowerShellProfileConfiguration) -> Self {
        Self {
            source: LocalBindDraft::from_policy(profile.local_bind_policy()),
            profile,
            state: PowerShellState::Setup,
            password: String::new(),
            command: String::new(),
            events: VecDeque::new(),
            presentation_bytes: 0,
            evicted_events: 0,
            worker_mailbox_drops: 0,
            sender: None,
            receiver: None,
            control: None,
            // Non-zero so close-confirmation owners can use 0 as "no
            // PowerShell lifecycle" if desired. Each explicit Connect bumps
            // this before the worker is published, so a stale confirmation
            // cannot close a newer runspace.
            worker_generation: 1,
            pending_close: false,
            input_feedback: None,
        }
    }

    pub(crate) fn profile_identifier(&self) -> &str {
        self.profile.identifier()
    }

    pub(crate) const fn state(&self) -> PowerShellState {
        self.state
    }

    pub(crate) fn title(&self) -> String {
        format!("PowerShell {}", self.profile.identifier())
    }

    pub(crate) fn close_requires_confirmation(&self) -> bool {
        matches!(
            self.state,
            PowerShellState::Connecting
                | PowerShellState::Ready
                | PowerShellState::Running
                | PowerShellState::Cancelling
        )
    }

    #[cfg(test)]
    pub(crate) fn set_state_for_test(&mut self, state: PowerShellState) {
        self.state = state;
    }

    #[allow(dead_code)]
    pub(crate) fn request_close(&mut self) {
        if self.pending_close {
            return;
        }
        self.pending_close = true;
        self.request_worker_close();
        if self
            .control
            .as_ref()
            .is_none_or(|control| control.finished.load(Ordering::Acquire))
        {
            self.state = PowerShellState::Closed;
            return;
        }
        if matches!(self.state, PowerShellState::Running) {
            self.state = PowerShellState::Cancelling;
            self.push_event(PowerShellPresentedEvent::lifecycle(
                "Close requested: cancelling the running command before shutdown.",
            ));
        } else if !matches!(self.state, PowerShellState::Closed) {
            self.state = PowerShellState::Closing;
        }
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui) -> Option<PowerShellAction> {
        self.drain_messages(ui.ctx());
        let mut action = None;
        crate::screens::show_bounded_content_scroll(
            ui,
            ("powershell-surface", self.profile.identifier().to_owned()),
            |ui| {
                ui.vertical(|ui| {
                    ui.add_space(12.0);
                    ui.heading(self.title());
                    ui.label(egui::RichText::new(CONNECTED_STATUS).color(theme::TEXT_SECONDARY));
                    ui.label(format!(
                        "{}:{} · {} · configuration {}",
                        self.profile.host(),
                        self.profile.port(),
                        self.profile.username(),
                        self.profile.configuration_name()
                    ));
                    if let Some(domain) = self.profile.domain() {
                        ui.label(format!("Domain: {domain}"));
                    }
                    ui.separator();
                    match self.state {
                        PowerShellState::Setup
                        | PowerShellState::Failed
                        | PowerShellState::Closed => {
                            action = self.show_setup(ui);
                        }
                        PowerShellState::Connecting => {
                            ui.spinner();
                            ui.label("Connecting on the PowerShell worker…");
                            if ui.button("Cancel connection").clicked() {
                                action = Some(PowerShellAction::Close);
                            }
                        }
                        PowerShellState::Ready
                        | PowerShellState::Running
                        | PowerShellState::Cancelling => {
                            action = self.show_command(ui);
                        }
                        PowerShellState::Closing => {
                            ui.spinner();
                            ui.label("Closing PowerShell session on the worker…");
                        }
                    }
                    ui.separator();
                    self.show_events(ui);
                });
            },
        );
        action
    }

    fn show_setup(&mut self, ui: &mut egui::Ui) -> Option<PowerShellAction> {
        if matches!(
            self.state,
            PowerShellState::Failed | PowerShellState::Closed
        ) {
            ui.label("Start a new explicit connection to continue. No reconnect is implied.");
        }
        let has_stored = self.profile.credential_reference().is_some();
        if has_stored {
            ui.label("Credential: native secure-store password reference saved.");
        } else {
            ui.label("Credential: enter a transient password for this session.");
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.password)
                    .password(true)
                    .char_limit(MAX_PASSWORD_BYTES + 1)
                    .hint_text("Password (not saved)"),
            );
            if response.changed() {
                enforce_byte_limit(
                    &mut self.password,
                    MAX_PASSWORD_BYTES,
                    &mut self.input_feedback,
                    "Password",
                );
            }
        }
        let ask = self.profile.local_bind_policy() == LocalBindPolicy::Ask;
        if ask {
            crate::local_bind::show_local_bind_draft(
                ui,
                &mut self.source,
                false,
                self.profile.identifier(),
            );
        } else {
            ui.label(format!(
                "Source address policy: {}",
                match self.profile.local_bind_policy() {
                    LocalBindPolicy::Automatic => "Automatic".to_owned(),
                    LocalBindPolicy::Address(address) => address.to_string(),
                    LocalBindPolicy::Ask => "Ask".to_owned(),
                }
            ));
        }
        if let Some(feedback) = &self.input_feedback {
            ui.colored_label(theme::STATUS_ERROR, feedback);
        }
        let closing = self
            .control
            .as_ref()
            .is_some_and(|control| !control.finished.load(Ordering::Acquire));
        if closing {
            ui.label("Waiting for the previous connection to finish cleanup.");
        }
        let connect = ui
            .add_enabled(
                !closing && self.input_feedback.is_none(),
                egui::Button::new("Connect"),
            )
            .clicked();
        if connect {
            return Some(PowerShellAction::Connect);
        }
        None
    }

    fn show_command(&mut self, ui: &mut egui::Ui) -> Option<PowerShellAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label(format!("State: {}", self.state.label()));
            if self.state == PowerShellState::Running && ui.button("Stop").clicked() {
                action = Some(PowerShellAction::Cancel);
            }
            if ui.button("Close").clicked() {
                action = Some(PowerShellAction::Close);
            }
        });
        let response = ui.add(
            egui::TextEdit::multiline(&mut self.command)
                .hint_text("PowerShell script")
                .char_limit(MAX_SCRIPT_BYTES + 1)
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
        if response.changed() {
            enforce_byte_limit(
                &mut self.command,
                MAX_SCRIPT_BYTES,
                &mut self.input_feedback,
                "Command",
            );
        }
        if let Some(feedback) = &self.input_feedback {
            ui.colored_label(theme::STATUS_ERROR, feedback);
        }
        let can_run = self.state == PowerShellState::Ready
            && !self.command.trim().is_empty()
            && self.input_feedback.is_none();
        if ui
            .add_enabled(can_run, egui::Button::new("Run command"))
            .clicked()
        {
            action = Some(PowerShellAction::Run(std::mem::take(&mut self.command)));
        }
        action
    }

    fn show_events(&self, ui: &mut egui::Ui) {
        if self.evicted_events > 0 {
            ui.colored_label(
                theme::STATUS_STARTING,
                format!(
                    "Presentation limit reached: {} older PowerShell events were evicted.",
                    self.evicted_events
                ),
            );
        }
        if self.worker_mailbox_drops > 0 {
            ui.colored_label(
                theme::STATUS_STARTING,
                format!(
                    "Worker message backlog exceeded the GUI queue; {} output updates were omitted. Command outcomes are retained.",
                    self.worker_mailbox_drops
                ),
            );
        }
        egui::ScrollArea::vertical()
            .id_salt(("powershell-events", self.profile.identifier()))
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for event in &self.events {
                    ui.group(|ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(format!("{:?}", event.stream))
                                    .color(stream_color(&event.stream)),
                            );
                            ui.label(egui::RichText::new(&event.text).monospace());
                        });
                        if !event.expandable.is_empty() {
                            egui::CollapsingHeader::new("Structured value")
                                .default_open(false)
                                .show(ui, |ui| {
                                    for (name, value) in &event.expandable {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.monospace(name);
                                            ui.label(value);
                                        });
                                    }
                                });
                        }
                    });
                }
            });
    }

    fn resolve_source(&self) -> Result<ResolvedLocalBind, String> {
        match self.profile.local_bind_policy() {
            LocalBindPolicy::Automatic => Ok(ResolvedLocalBind::Automatic),
            LocalBindPolicy::Address(address) => Ok(ResolvedLocalBind::Address(address)),
            LocalBindPolicy::Ask => self.source.resolved(),
        }
    }

    pub(crate) fn dispatch(
        &mut self,
        action: PowerShellAction,
        store: Result<&Arc<dyn SecretStore>, festerm_secret_store::SecretStoreError>,
        context: &egui::Context,
    ) {
        match action {
            PowerShellAction::Connect => {
                if !matches!(
                    self.state,
                    PowerShellState::Setup | PowerShellState::Closed | PowerShellState::Failed
                ) || self
                    .control
                    .as_ref()
                    .is_some_and(|control| !control.finished.load(Ordering::Acquire))
                {
                    self.push_event(PowerShellPresentedEvent::lifecycle("The previous PowerShell worker must finish before a new connection can start."));
                    return;
                }
                if self.input_feedback.is_some() {
                    return;
                }
                let local_bind = match self.resolve_source() {
                    Ok(local_bind) => local_bind,
                    Err(error) => {
                        self.state = PowerShellState::Failed;
                        self.push_event(PowerShellPresentedEvent::lifecycle(error));
                        return;
                    }
                };
                let transient_password = if self.profile.credential_reference().is_some() {
                    None
                } else if self.password.is_empty() {
                    self.state = PowerShellState::Failed;
                    self.push_event(PowerShellPresentedEvent::lifecycle(
                        "Enter a password or save an opaque credential reference before connecting.",
                    ));
                    return;
                } else {
                    Some(SecretBytes::from_secret_string(std::mem::take(
                        &mut self.password,
                    )))
                };
                let credential = match (self.profile.credential_reference(), transient_password) {
                    (Some(reference), _) => match store {
                        Ok(store) => CredentialSource::Stored {
                            store: Arc::clone(store),
                            reference: reference.duplicate_for_transport(),
                        },
                        Err(error) => {
                            self.state = PowerShellState::Failed;
                            self.push_event(PowerShellPresentedEvent::lifecycle(format!(
                                "Native secure storage is unavailable: {error:?}"
                            )));
                            return;
                        }
                    },
                    (None, Some(password)) => CredentialSource::Transient(password),
                    (None, None) => {
                        self.state = PowerShellState::Failed;
                        self.push_event(PowerShellPresentedEvent::lifecycle(
                            "Enter a password or save an opaque credential reference before connecting.",
                        ));
                        return;
                    }
                };
                self.reset_for_explicit_connection();
                self.connect(local_bind, credential, context);
            }
            PowerShellAction::Run(script) => self.run_script(script),
            PowerShellAction::Cancel => self.cancel_command(),
            PowerShellAction::Close => self.request_close(),
        }
    }

    pub(crate) const fn lifecycle_generation(&self) -> u64 {
        self.worker_generation
    }

    pub(crate) fn refresh_profile(&mut self, profile: &PowerShellProfileConfiguration) {
        if !matches!(
            self.state,
            PowerShellState::Setup | PowerShellState::Closed | PowerShellState::Failed
        ) {
            return;
        }
        let source_policy_changed = self.profile.local_bind_policy() != profile.local_bind_policy();
        self.profile = profile.clone();
        if source_policy_changed
            && matches!(
                self.state,
                PowerShellState::Setup | PowerShellState::Closed | PowerShellState::Failed
            )
        {
            self.source = LocalBindDraft::from_policy(self.profile.local_bind_policy());
        }
    }

    pub(crate) fn shutdown_handle(&self) -> Option<PowerShellShutdownHandle> {
        self.control
            .as_ref()
            .map(|control| PowerShellShutdownHandle {
                control: Arc::clone(control),
                sender: self.sender.clone(),
            })
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn attach_shutdown_handle_for_test(&mut self, handle: PowerShellShutdownHandle) {
        self.control = Some(Arc::clone(&handle.control));
        self.sender = handle.sender.clone();
    }

    #[allow(dead_code)]
    fn reset_for_explicit_connection(&mut self) {
        self.pending_close = false;
        self.sender = None;
        self.receiver = None;
        self.control = None;
        self.source = LocalBindDraft::from_policy(self.profile.local_bind_policy());
        self.presentation_bytes = 0;
        self.evicted_events = 0;
        self.worker_mailbox_drops = 0;
        self.events.clear();
    }

    #[allow(dead_code)]
    fn run_script(&mut self, script: String) {
        if self.state != PowerShellState::Ready || script.trim().is_empty() {
            self.push_event(PowerShellPresentedEvent::lifecycle(
                "A nonempty command requires a ready PowerShell session.",
            ));
            return;
        }
        if script.len() > MAX_SCRIPT_BYTES {
            self.push_event(PowerShellPresentedEvent::lifecycle(format!(
                "Command rejected: scripts are limited to {MAX_SCRIPT_BYTES} bytes."
            )));
            return;
        }
        let Some(sender) = &self.sender else {
            self.push_event(PowerShellPresentedEvent::lifecycle(
                "Command could not be sent: PowerShell worker is not available.",
            ));
            return;
        };
        match sender.try_send(WorkerRequest::Run(script)) {
            Ok(()) => self.state = PowerShellState::Running,
            Err(TrySendError::Full(_)) => self.push_event(PowerShellPresentedEvent::lifecycle(
                "Command could not be sent: worker request queue is full.",
            )),
            Err(TrySendError::Disconnected(_)) => {
                self.sender = None;
                self.state = PowerShellState::Failed;
                self.push_event(PowerShellPresentedEvent::lifecycle(
                    "Command could not be sent: PowerShell worker has stopped.",
                ));
            }
        }
    }

    #[allow(dead_code)]
    fn cancel_command(&mut self) {
        if self.state != PowerShellState::Running {
            self.push_event(PowerShellPresentedEvent::lifecycle(
                "There is no running PowerShell command to stop.",
            ));
            return;
        }
        let Some(control) = &self.control else {
            self.push_event(PowerShellPresentedEvent::lifecycle(
                "Stop could not be sent: PowerShell worker is not available.",
            ));
            return;
        };
        control.cancel_requested.store(true, Ordering::Release);
        if let Some(sender) = &self.sender {
            match sender.try_send(WorkerRequest::Cancel) {
                Ok(()) | Err(TrySendError::Full(_)) => {
                    self.state = PowerShellState::Cancelling;
                    self.push_event(PowerShellPresentedEvent::lifecycle(
                        "Stop requested; waiting for backend cancellation acknowledgement.",
                    ));
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.sender = None;
                    self.state = PowerShellState::Failed;
                    self.push_event(PowerShellPresentedEvent::lifecycle(
                        "Stop could not be sent: PowerShell worker has stopped.",
                    ));
                }
            }
        }
    }

    fn request_worker_close(&mut self) {
        if let Some(control) = &self.control {
            control.close_requested.store(true, Ordering::Release);
            control.cancel_requested.store(true, Ordering::Release);
        }
        if let Some(sender) = &self.sender {
            match sender.try_send(WorkerRequest::Close) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => {
                    self.sender = None;
                }
            }
        }
    }

    #[allow(dead_code)]
    fn connect(
        &mut self,
        local_bind: ResolvedLocalBind,
        credential: CredentialSource,
        context: &egui::Context,
    ) {
        let (request_tx, request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let (message_tx, message_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let control = Arc::new(WorkerControl::new());
        let worker_control = Arc::clone(&control);
        let profile = self.profile.clone();
        let repaint = context.clone();
        self.worker_generation = self.worker_generation.saturating_add(1);
        let generation = self.worker_generation;
        match thread::Builder::new()
            .name("festerm-powershell-ui-worker".into())
            .spawn(move || {
                run_worker(
                    profile,
                    local_bind.address(),
                    credential,
                    request_rx,
                    message_tx,
                    worker_control,
                    repaint,
                    generation,
                )
            }) {
            Ok(_) => {
                self.sender = Some(request_tx);
                self.receiver = Some(message_rx);
                self.control = Some(control);
                self.state = PowerShellState::Connecting;
                self.push_event(PowerShellPresentedEvent::lifecycle(
                    "Connecting to native PowerShell endpoint…",
                ));
                context.request_repaint();
            }
            Err(_) => {
                self.state = PowerShellState::Failed;
                self.push_event(PowerShellPresentedEvent::lifecycle(
                    "PowerShell worker thread could not be started.",
                ));
            }
        }
    }

    pub(crate) fn drain_messages(&mut self, context: &egui::Context) {
        self.drain_worker_drop_indicator();
        let Some(receiver) = self.receiver.take() else {
            return;
        };
        let mut receiver = Some(receiver);
        for _ in 0..MAX_FRAME_MESSAGES {
            let rx = receiver
                .as_ref()
                .expect("receiver is retained during draining");
            let next = match rx.try_recv() {
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                    let lifecycle = self.control.as_ref().and_then(|control| {
                        control
                            .pending_lifecycle
                            .lock()
                            .expect("lifecycle mutex")
                            .pop_front()
                    });
                    if let Some(message) = lifecycle {
                        Ok(message)
                    } else {
                        rx.try_recv()
                    }
                }
                other => other,
            };
            match next {
                Ok(message) if message.generation != self.worker_generation => {
                    self.push_event(PowerShellPresentedEvent::lifecycle(
                        "Ignored stale PowerShell worker completion.",
                    ));
                }
                Ok(message) => {
                    self.apply_worker_message(message);
                    context.request_repaint();
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.sender = None;
                    receiver = None;
                    if !matches!(
                        self.state,
                        PowerShellState::Closed | PowerShellState::Failed
                    ) {
                        self.state = PowerShellState::Failed;
                        self.push_event(PowerShellPresentedEvent::lifecycle(
                            "PowerShell worker stopped unexpectedly.",
                        ));
                    }
                    break;
                }
            }
        }
        self.receiver = receiver.take();
        if self.pending_close
            && matches!(
                self.state,
                PowerShellState::Closed | PowerShellState::Failed
            )
        {
            self.sender = None;
        }
    }

    fn drain_worker_drop_indicator(&mut self) {
        let Some(control) = &self.control else {
            return;
        };
        let drops = control.mailbox_drops.swap(0, Ordering::AcqRel);
        if drops > 0 {
            self.worker_mailbox_drops = self.worker_mailbox_drops.saturating_add(drops);
        }
    }

    fn apply_worker_message(&mut self, message: WorkerMessage) {
        match message.kind {
            WorkerMessageKind::Connected => {
                if self.pending_close {
                    self.state = PowerShellState::Closing;
                } else {
                    self.state = PowerShellState::Ready;
                }
                self.push_event(PowerShellPresentedEvent::lifecycle("Connected."));
            }
            WorkerMessageKind::ConnectFailed(error) => {
                self.state = PowerShellState::Failed;
                self.sender = None;
                self.push_event(PowerShellPresentedEvent::lifecycle(format!(
                    "Connection failed: {error}"
                )));
            }
            WorkerMessageKind::CommandEvent(event) => self.push_event(event),
            WorkerMessageKind::CommandFinished(text) => {
                self.state = if self.pending_close {
                    PowerShellState::Closing
                } else {
                    PowerShellState::Ready
                };
                self.push_event(PowerShellPresentedEvent::lifecycle(text));
            }
            WorkerMessageKind::Cancelled(text) => {
                self.state = if self.pending_close {
                    PowerShellState::Closing
                } else {
                    PowerShellState::Ready
                };
                self.push_event(PowerShellPresentedEvent::lifecycle(text));
            }
            WorkerMessageKind::RecoverableFailure(text) => {
                self.state = if self.pending_close {
                    PowerShellState::Closing
                } else {
                    PowerShellState::Ready
                };
                self.push_event(PowerShellPresentedEvent::lifecycle(text));
            }
            WorkerMessageKind::Invalidated(text) => {
                self.state = PowerShellState::Failed;
                self.sender = None;
                self.push_event(PowerShellPresentedEvent::lifecycle(text));
            }
            WorkerMessageKind::Closed(result) => {
                self.sender = None;
                match result {
                    Ok(()) => {
                        self.state = PowerShellState::Closed;
                        self.push_event(PowerShellPresentedEvent::lifecycle(
                            "PowerShell session closed.",
                        ));
                    }
                    Err(error) => {
                        self.state = PowerShellState::Failed;
                        self.push_event(PowerShellPresentedEvent::lifecycle(format!(
                            "PowerShell close failed: {error}"
                        )));
                    }
                }
            }
        }
    }

    fn push_event(&mut self, event: PowerShellPresentedEvent) {
        let event_bytes = event.presentation_bytes();
        while self.events.len() >= MAX_PRESENTED_EVENTS
            || (!self.events.is_empty()
                && self.presentation_bytes.saturating_add(event_bytes) > MAX_PRESENTATION_BYTES)
        {
            let Some(evicted) = self.events.pop_front() else {
                break;
            };
            self.presentation_bytes = self
                .presentation_bytes
                .saturating_sub(evicted.presentation_bytes());
            self.evicted_events = self.evicted_events.saturating_add(1);
        }
        self.presentation_bytes = self.presentation_bytes.saturating_add(event_bytes);
        self.events.push_back(event);
    }
}

impl Drop for PowerShellTab {
    fn drop(&mut self) {
        self.request_worker_close();
    }
}

#[allow(dead_code)]
enum WorkerRequest {
    Run(String),
    Cancel,
    Close,
}

#[allow(dead_code)]
pub(crate) enum CredentialSource {
    Transient(SecretBytes),
    Stored {
        store: Arc<dyn SecretStore>,
        reference: SecretReference,
    },
}

struct WorkerMessage {
    generation: u64,
    kind: WorkerMessageKind,
}

#[allow(dead_code)]
enum WorkerMessageKind {
    Connected,
    ConnectFailed(String),
    CommandEvent(PowerShellPresentedEvent),
    CommandFinished(String),
    Cancelled(String),
    RecoverableFailure(String),
    Invalidated(String),
    Closed(Result<(), String>),
}

trait WorkerBackend {
    type Session;
    type Command;

    fn connect(
        &self,
        profile: &PowerShellProfileConfiguration,
        local_bind_address: Option<IpAddr>,
        password: SecretBytes,
    ) -> Result<Self::Session, String>;
    fn start_script(
        &self,
        session: &Self::Session,
        script: String,
    ) -> Result<Self::Command, String>;
    fn recv_timeout(
        &self,
        command: &mut Self::Command,
        timeout: Duration,
    ) -> Result<Option<PowerShellCommandMessage>, PowerShellSessionError>;
    fn cancel(&self, command: &Self::Command);
    fn close(&self, session: &Self::Session) -> Result<(), String>;
    fn ready(&self, _session: &Self::Session) -> bool {
        true
    }
}

struct RealBackend;

impl WorkerBackend for RealBackend {
    type Session = PowerShellSession;
    type Command = PowerShellCommand;

    fn connect(
        &self,
        profile: &PowerShellProfileConfiguration,
        local_bind_address: Option<IpAddr>,
        password: SecretBytes,
    ) -> Result<Self::Session, String> {
        connect_session(profile, local_bind_address, password).map_err(|error| error.to_string())
    }

    fn start_script(
        &self,
        session: &Self::Session,
        script: String,
    ) -> Result<Self::Command, String> {
        session
            .start_script(script)
            .map_err(|error| error.to_string())
    }

    fn recv_timeout(
        &self,
        command: &mut Self::Command,
        timeout: Duration,
    ) -> Result<Option<PowerShellCommandMessage>, PowerShellSessionError> {
        command.recv_timeout(timeout)
    }

    fn cancel(&self, command: &Self::Command) {
        command.cancel();
    }

    fn close(&self, session: &Self::Session) -> Result<(), String> {
        session.close().map_err(|error| error.to_string())
    }

    fn ready(&self, session: &Self::Session) -> bool {
        session.status() == festerm_powershell::PowerShellSessionStatus::Ready
    }
}

#[allow(clippy::too_many_arguments)]
fn run_worker(
    profile: PowerShellProfileConfiguration,
    local_bind_address: Option<IpAddr>,
    credential: CredentialSource,
    request_rx: Receiver<WorkerRequest>,
    message_tx: SyncSender<WorkerMessage>,
    control: Arc<WorkerControl>,
    repaint: egui::Context,
    generation: u64,
) {
    run_worker_with_backend(
        RealBackend,
        profile,
        local_bind_address,
        credential,
        request_rx,
        message_tx,
        control,
        repaint,
        generation,
    );
}

#[allow(clippy::too_many_arguments)]
fn run_worker_with_backend<B: WorkerBackend>(
    backend: B,
    profile: PowerShellProfileConfiguration,
    local_bind_address: Option<IpAddr>,
    credential: CredentialSource,
    request_rx: Receiver<WorkerRequest>,
    message_tx: SyncSender<WorkerMessage>,
    control: Arc<WorkerControl>,
    repaint: egui::Context,
    generation: u64,
) {
    let _finished = WorkerFinishedGuard(Arc::clone(&control), repaint.clone());
    let send = |kind| {
        let output = matches!(kind, WorkerMessageKind::CommandEvent(_));
        let message = WorkerMessage { generation, kind };
        let mut pending = control.pending_lifecycle.lock().expect("lifecycle mutex");
        let result = if !output && !pending.is_empty() {
            Err(TrySendError::Full(message))
        } else {
            message_tx.try_send(message)
        };
        match result {
            Ok(()) => {
                repaint.request_repaint();
                true
            }
            Err(TrySendError::Full(message)) => {
                if output {
                    control.mailbox_drops.fetch_add(1, Ordering::AcqRel);
                } else {
                    // One active command permits only its outcome plus a final close.
                    // Keep these outside the lossy output queue, in generation order.
                    if pending.len() >= 4 {
                        control.close_requested.store(true, Ordering::Release);
                        pending.clear();
                        pending.push_back(WorkerMessage { generation, kind: WorkerMessageKind::Invalidated(
                        "PowerShell lifecycle backlog exceeded its bound; the session was closed.".to_owned(),
                    )});
                    } else {
                        pending.push_back(message);
                    }
                }
                repaint.request_repaint();
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    };
    let password = match credential {
        CredentialSource::Transient(password) => password,
        CredentialSource::Stored { store, reference } => match store.get(&reference) {
            Ok(password) => password,
            Err(error) => {
                let _ = send(WorkerMessageKind::ConnectFailed(format!(
                    "native secure-store credential could not be read: {error:?}"
                )));
                return;
            }
        },
    };
    let session = match backend.connect(&profile, local_bind_address, password) {
        Ok(session) => session,
        Err(error) => {
            let _ = send(WorkerMessageKind::ConnectFailed(error));
            return;
        }
    };
    if control.close_requested.load(Ordering::Acquire) {
        let close = backend.close(&session);
        let _ = send(WorkerMessageKind::Closed(close));
        return;
    }
    if !send(WorkerMessageKind::Connected) {
        let _ = backend.close(&session);
        return;
    }
    let mut active: Option<B::Command> = None;
    let mut closing = false;
    loop {
        if control.close_requested.load(Ordering::Acquire) {
            closing = true;
            if let Some(command) = &active {
                backend.cancel(command);
            } else {
                let close = backend.close(&session);
                let _ = send(WorkerMessageKind::Closed(close));
                return;
            }
        } else if control.cancel_requested.swap(false, Ordering::AcqRel) {
            if let Some(command) = &active {
                backend.cancel(command);
                let _ = send(WorkerMessageKind::CommandEvent(
                    PowerShellPresentedEvent::lifecycle(
                        "Cancellation requested; draining backend events.",
                    ),
                ));
            }
        }
        if let Some(command) = active.as_mut() {
            match backend.recv_timeout(command, Duration::from_millis(40)) {
                Ok(Some(message)) => {
                    let terminal = matches!(
                        message,
                        PowerShellCommandMessage::Completed { .. }
                            | PowerShellCommandMessage::Failed(_)
                    );
                    let mut kind = present_command_message(message);
                    if terminal && !backend.ready(&session) {
                        kind = WorkerMessageKind::Invalidated(
                            "The command ended with uncertain runspace state; open a new connection explicitly.".to_owned(),
                        );
                    }
                    let invalidated = matches!(kind, WorkerMessageKind::Invalidated(_));
                    if !send(kind) {
                        backend.cancel(command);
                        let _ = backend.close(&session);
                        return;
                    }
                    if invalidated {
                        let close = backend.close(&session);
                        let _ = send(WorkerMessageKind::Closed(close));
                        return;
                    }
                    if terminal {
                        active = None;
                        if closing {
                            let close = backend.close(&session);
                            let _ = send(WorkerMessageKind::Closed(close));
                            return;
                        }
                    }
                }
                Ok(None) => {
                    let _ = send(WorkerMessageKind::Invalidated(
                        "PowerShell command ended without an outcome.".to_owned(),
                    ));
                    let close = backend.close(&session);
                    let _ = send(WorkerMessageKind::Closed(close));
                    return;
                }
                Err(PowerShellSessionError::TimedOut) => {}
                Err(error) => {
                    let _ = send(WorkerMessageKind::Invalidated(format!(
                        "PowerShell command stream failed and the session was invalidated: {error}"
                    )));
                    let close = backend.close(&session);
                    let _ = send(WorkerMessageKind::Closed(close));
                    return;
                }
            }
        }
        match request_rx.try_recv() {
            Ok(WorkerRequest::Run(script)) if active.is_none() => {
                match backend.start_script(&session, script) {
                    Ok(command) => active = Some(command),
                    Err(error) => {
                        if !backend.ready(&session) {
                            let _ = send(WorkerMessageKind::Invalidated(format!(
                                "PowerShell command could not start: {error}"
                            )));
                            let close = backend.close(&session);
                            let _ = send(WorkerMessageKind::Closed(close));
                            return;
                        }
                        if !send(WorkerMessageKind::RecoverableFailure(format!(
                            "PowerShell command could not start: {error}"
                        ))) {
                            return;
                        }
                    }
                }
            }
            Ok(WorkerRequest::Run(_)) => {
                let _ = send(WorkerMessageKind::RecoverableFailure(
                    "A PowerShell pipeline is already running.".to_owned(),
                ));
            }
            Ok(WorkerRequest::Cancel) => {
                control.cancel_requested.store(false, Ordering::Release);
                if let Some(command) = &active {
                    backend.cancel(command);
                    let _ = send(WorkerMessageKind::CommandEvent(
                        PowerShellPresentedEvent::lifecycle(
                            "Cancellation requested; draining backend events.",
                        ),
                    ));
                }
            }
            Ok(WorkerRequest::Close) => {
                control.close_requested.store(true, Ordering::Release);
                closing = true;
                if let Some(command) = &active {
                    backend.cancel(command);
                } else {
                    let close = backend.close(&session);
                    let _ = send(WorkerMessageKind::Closed(close));
                    return;
                }
            }
            Err(TryRecvError::Empty) => {
                if active.is_none() {
                    match request_rx.recv() {
                        Ok(request) => match request {
                            WorkerRequest::Run(script) => {
                                if control.close_requested.load(Ordering::Acquire) {
                                    let close = backend.close(&session);
                                    let _ = send(WorkerMessageKind::Closed(close));
                                    return;
                                }
                                match backend.start_script(&session, script) {
                                    Ok(command) => active = Some(command),
                                    Err(error) => {
                                        if !backend.ready(&session) {
                                            let _ = send(WorkerMessageKind::Invalidated(format!(
                                                "PowerShell command could not start: {error}"
                                            )));
                                            let close = backend.close(&session);
                                            let _ = send(WorkerMessageKind::Closed(close));
                                            return;
                                        }
                                        if !send(WorkerMessageKind::RecoverableFailure(format!(
                                            "PowerShell command could not start: {error}"
                                        ))) {
                                            return;
                                        }
                                    }
                                }
                            }
                            WorkerRequest::Cancel => {
                                control.cancel_requested.store(false, Ordering::Release);
                            }
                            WorkerRequest::Close => {
                                let close = backend.close(&session);
                                let _ = send(WorkerMessageKind::Closed(close));
                                return;
                            }
                        },
                        Err(_) => {
                            let _ = backend.close(&session);
                            return;
                        }
                    }
                }
            }
            Err(TryRecvError::Disconnected) => {
                if let Some(command) = &active {
                    backend.cancel(command);
                }
                let _ = backend.close(&session);
                return;
            }
        }
    }
}

#[allow(dead_code)]
fn connect_session(
    profile: &PowerShellProfileConfiguration,
    local_bind_address: Option<IpAddr>,
    password: SecretBytes,
) -> Result<PowerShellSession, PowerShellSessionError> {
    let mut endpoint = PowerShellEndpoint::https(profile.host())?
        .with_port(profile.port())
        .with_configuration_name(profile.configuration_name())?
        .with_local_bind_address(local_bind_address)?;
    if let Some(path) = profile.trusted_ca_file() {
        let pem = read_trusted_ca_pem_bounded(path)?;
        endpoint = endpoint.with_trusted_ca_pem(pem)?;
    }
    let credentials = PowerShellCredentials::with_domain(
        profile.username(),
        profile.domain().map(str::to_owned),
        password,
    )?;
    PowerShellSession::connect(
        endpoint,
        PowerShellOptions {
            auth: PowerShellAuthMethod::Ntlm,
            ..PowerShellOptions::default()
        },
        credentials,
    )
}

#[allow(dead_code)]
fn read_trusted_ca_pem_bounded(path: &std::path::Path) -> Result<String, PowerShellSessionError> {
    let file = File::open(path).map_err(|_| {
        PowerShellSessionError::InvalidConfiguration("trusted CA file could not be read")
    })?;
    let limit = u64::try_from(MAX_TRUSTED_CA_PEM_BYTES)
        .expect("trusted CA limit fits u64")
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).map_err(|_| {
        PowerShellSessionError::InvalidConfiguration("trusted CA file could not be read")
    })?;
    if bytes.len() > MAX_TRUSTED_CA_PEM_BYTES {
        return Err(PowerShellSessionError::InvalidConfiguration(
            "trusted CA file exceeded the configured size limit",
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        PowerShellSessionError::InvalidConfiguration("trusted CA file must be valid UTF-8 PEM")
    })
}

fn enforce_byte_limit(text: &mut String, limit: usize, feedback: &mut Option<String>, label: &str) {
    if text.len() <= limit {
        *feedback = None;
        return;
    }
    let mut end = 0;
    for (index, ch) in text.char_indices() {
        let next = index + ch.len_utf8();
        if next > limit {
            break;
        }
        end = next;
    }
    text.truncate(end);
    *feedback = Some(format!("{label} input is limited to {limit} bytes."));
}

#[allow(dead_code)]
fn present_command_message(message: PowerShellCommandMessage) -> WorkerMessageKind {
    match message {
        PowerShellCommandMessage::Event(event) => {
            WorkerMessageKind::CommandEvent(present_pipeline_event(event))
        }
        PowerShellCommandMessage::Completed { state } => {
            WorkerMessageKind::CommandFinished(format!("Command completed: {state:?}"))
        }
        PowerShellCommandMessage::Failed(failure) => {
            let mut text = match failure.kind {
                PowerShellCommandFailureKind::Cancelled => {
                    format!("Cancellation acknowledged by backend: {:?}.", failure.state)
                }
                _ => format!("Command failed: {:?} {:?}.", failure.kind, failure.state),
            };
            if !failure.errors.is_empty() {
                text.push_str(" Errors: ");
                for (index, error) in failure.errors.iter().take(8).enumerate() {
                    if index > 0 {
                        text.push_str("; ");
                    }
                    text.push_str(&bounded_value_text(error).text);
                }
                if failure.errors.len() > 8 {
                    text.push_str("; … <additional errors truncated>");
                }
            }
            if failure.kind == PowerShellCommandFailureKind::Cancelled {
                WorkerMessageKind::Cancelled(text)
            } else if matches!(
                failure.kind,
                PowerShellCommandFailureKind::Invalidated
                    | PowerShellCommandFailureKind::TransportFailure
                    | PowerShellCommandFailureKind::ProtocolFailure
            ) {
                WorkerMessageKind::Invalidated(text)
            } else {
                WorkerMessageKind::RecoverableFailure(text)
            }
        }
    }
}

fn present_pipeline_event(event: PipelineEvent) -> PowerShellPresentedEvent {
    match event {
        PipelineEvent::Output(value) => value_event(PowerShellStream::Output, &value),
        PipelineEvent::Error(value) => value_event(PowerShellStream::Error, &value),
        PipelineEvent::Warning(value) => value_event(PowerShellStream::Warning, &value),
        PipelineEvent::Verbose(value) => value_event(PowerShellStream::Verbose, &value),
        PipelineEvent::Debug(value) => value_event(PowerShellStream::Debug, &value),
        PipelineEvent::Information(value) => value_event(PowerShellStream::Information, &value),
        PipelineEvent::Progress(value) => value_event(PowerShellStream::Progress, &value),
        PipelineEvent::State(state) => PowerShellPresentedEvent {
            stream: PowerShellStream::State,
            text: state_text(state),
            expandable: Vec::new(),
        },
    }
}

fn value_event(stream: PowerShellStream, value: &PsValue) -> PowerShellPresentedEvent {
    let bounded = bounded_value_text(value);
    PowerShellPresentedEvent {
        stream,
        text: bounded.text,
        expandable: expandable_value(value),
    }
}

fn expandable_value(value: &PsValue) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let mut bytes = 0;
    let mut push = |name: String, value: &PsValue| {
        let value = bounded_value_text(value).text;
        if rows.len() >= 32 || bytes + name.len() + value.len() > MAX_DISPLAY_CHARS * 2 {
            rows.push((
                "…".to_owned(),
                "Additional properties truncated by the display limit.".to_owned(),
            ));
            return false;
        }
        bytes += name.len() + value.len();
        rows.push((name, value));
        true
    };
    match value {
        PsValue::Object(object) => {
            for (name, value) in &object.properties {
                if !push(sanitize_display(name), value) {
                    break;
                }
            }
        }
        PsValue::Dict(entries) => {
            for (key, value) in entries {
                if !push(bounded_value_text(key).text, value) {
                    break;
                }
            }
        }
        PsValue::List(values) => {
            for (index, value) in values.iter().enumerate() {
                if !push(format!("[{index}]"), value) {
                    break;
                }
            }
        }
        _ => {}
    }
    rows
}

struct BoundedValueText {
    text: String,
    bytes: usize,
    nodes: usize,
    truncated: bool,
}

fn bounded_value_text(value: &PsValue) -> BoundedValueText {
    let mut text = BoundedValueText {
        text: String::new(),
        bytes: 0,
        nodes: 0,
        truncated: false,
    };
    append_value(value, 0, &mut text);
    if text.truncated {
        let marker = " … <truncated>";
        let mut end = text.text.len().min(MAX_DISPLAY_CHARS - marker.len());
        while !text.text.is_char_boundary(end) {
            end -= 1;
        }
        text.text.truncate(end);
        text.text.push_str(marker);
        text.bytes = text.text.len();
    }
    text
}

fn append_value(value: &PsValue, depth: usize, out: &mut BoundedValueText) {
    if out.bytes >= MAX_DISPLAY_CHARS || out.nodes >= MAX_VALUE_NODES || depth >= MAX_VALUE_DEPTH {
        out.truncated = true;
        return;
    }
    out.nodes = out.nodes.saturating_add(1);
    match value {
        PsValue::Null => append_bounded(out, "$null"),
        PsValue::Bool(value) => append_bounded(out, if *value { "true" } else { "false" }),
        PsValue::I8(value) => append_bounded(out, &format!("{value}i8")),
        PsValue::U8(value) => append_bounded(out, &format!("{value}u8")),
        PsValue::I16(value) => append_bounded(out, &format!("{value}i16")),
        PsValue::U16(value) => append_bounded(out, &format!("{value}u16")),
        PsValue::I32(value) => append_bounded(out, &value.to_string()),
        PsValue::U32(value) => append_bounded(out, &format!("{value}u32")),
        PsValue::I64(value) => append_bounded(out, &format!("{value}i64")),
        PsValue::U64(value) => append_bounded(out, &format!("{value}u64")),
        PsValue::F32(value) => append_bounded(out, &format!("{value}f")),
        PsValue::Double(value) => append_bounded(out, &value.to_string()),
        PsValue::Decimal(value) => append_typed_str(out, "decimal", value),
        PsValue::Char(value) => append_typed_str(out, "char", &value.to_string()),
        PsValue::String(value) => append_typed_str(out, "string", value),
        PsValue::Bytes(bytes) => {
            append_bounded(out, &format!("<{} bytes>", bytes.len()));
        }
        PsValue::DateTime(value) => append_typed_str(out, "datetime", value),
        PsValue::Duration(value) => append_typed_str(out, "duration", value),
        PsValue::Guid(value) => append_typed_str(out, "guid", &value.to_string()),
        PsValue::Version(value) => append_typed_str(out, "version", value),
        PsValue::Uri(value) => append_typed_str(out, "uri", value),
        PsValue::Xml(value) => append_typed_str(out, "xml", value),
        PsValue::ScriptBlock(value) => append_typed_str(out, "scriptblock", value),
        PsValue::SecureString(_) => append_bounded(out, "<secure-string>"),
        PsValue::List(values) => {
            append_bounded(out, "[");
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    append_bounded(out, ", ");
                }
                append_value(value, depth + 1, out);
                if out.truncated {
                    break;
                }
            }
            append_bounded(out, "]");
        }
        PsValue::Dict(entries) => {
            append_bounded(out, "{");
            for (index, (key, value)) in entries.iter().enumerate() {
                if index > 0 {
                    append_bounded(out, ", ");
                }
                append_value(key, depth + 1, out);
                append_bounded(out, ": ");
                append_value(value, depth + 1, out);
                if out.truncated {
                    break;
                }
            }
            append_bounded(out, "}");
        }
        PsValue::Object(object) => {
            append_bounded(out, "Object");
            if let Some(name) = object.type_names.first() {
                append_typed_str(out, "<type>", name);
            }
            append_bounded(out, "{");
            if let Some(display) = &object.to_string {
                append_typed_str(out, "ToString", display);
                append_bounded(out, "; ");
            }
            for (index, (name, value)) in object.properties.iter().enumerate() {
                if index > 0 {
                    append_bounded(out, ", ");
                }
                append_bounded(out, &sanitize_display(name));
                append_bounded(out, "=");
                append_value(value, depth + 1, out);
                if out.truncated {
                    break;
                }
            }
            append_bounded(out, "}");
        }
    }
}

fn append_typed_str(out: &mut BoundedValueText, kind: &str, value: &str) {
    append_bounded(out, kind);
    append_bounded(out, ":\"");
    append_bounded(out, &sanitize_display(value));
    append_bounded(out, "\"");
}

fn append_bounded(out: &mut BoundedValueText, text: &str) {
    if out.bytes >= MAX_DISPLAY_CHARS {
        out.truncated = true;
        return;
    }
    for ch in text.chars() {
        let ch_len = ch.len_utf8();
        if out.bytes.saturating_add(ch_len) > MAX_DISPLAY_CHARS {
            out.truncated = true;
            break;
        }
        out.text.push(ch);
        out.bytes = out.bytes.saturating_add(ch_len);
    }
}

fn sanitize_display(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars().take(MAX_DISPLAY_CHARS + 1) {
        if out.len() + 12 > MAX_DISPLAY_CHARS {
            out.push_str("<truncated>");
            break;
        }
        if (ch.is_control() && ch != '\n' && ch != '\t')
            || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            use std::fmt::Write as _;
            let _ = write!(out, "\\u{{{:x}}}", u32::from(ch));
        } else {
            out.push(ch);
        }
    }
    out
}

fn state_text(state: PipelineState) -> String {
    format!("Pipeline state: {state:?}")
}

fn stream_color(stream: &PowerShellStream) -> egui::Color32 {
    match stream {
        PowerShellStream::Lifecycle | PowerShellStream::State => theme::TEXT_SECONDARY,
        PowerShellStream::Output => theme::TEXT_PRIMARY,
        PowerShellStream::Error => theme::STATUS_ERROR,
        PowerShellStream::Warning => theme::STATUS_STARTING,
        PowerShellStream::Verbose | PowerShellStream::Debug | PowerShellStream::Information => {
            theme::TEXT_SECONDARY
        }
        PowerShellStream::Progress => theme::ACCENT_PRIMARY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};
    use festerm_powershell::PsObject;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;
    use std::time::Instant;

    #[derive(Clone)]
    struct FakeBackend {
        state: Arc<FakeBackendState>,
    }

    struct FakeBackendState {
        messages: Mutex<VecDeque<Result<Option<PowerShellCommandMessage>, PowerShellSessionError>>>,
        started: AtomicUsize,
        cancelled: AtomicUsize,
        closed: AtomicUsize,
        connect_entered: AtomicBool,
        connect_blocked: AtomicBool,
        close_error: Mutex<Option<String>>,
    }

    impl FakeBackend {
        fn new(messages: Vec<PowerShellCommandMessage>) -> Self {
            Self {
                state: Arc::new(FakeBackendState {
                    messages: Mutex::new(
                        messages
                            .into_iter()
                            .map(|message| Ok(Some(message)))
                            .collect(),
                    ),
                    started: AtomicUsize::new(0),
                    cancelled: AtomicUsize::new(0),
                    closed: AtomicUsize::new(0),
                    connect_entered: AtomicBool::new(false),
                    connect_blocked: AtomicBool::new(false),
                    close_error: Mutex::new(None),
                }),
            }
        }

        fn gated_connect() -> Self {
            let fake = Self::new(Vec::new());
            fake.state.connect_blocked.store(true, Ordering::Release);
            fake
        }

        fn release_connect(&self) {
            self.state.connect_blocked.store(false, Ordering::Release);
        }

        fn with_close_error(error: &str) -> Self {
            let fake = Self::new(Vec::new());
            *fake.state.close_error.lock().unwrap() = Some(error.to_owned());
            fake
        }
    }

    struct FakeSession;

    #[derive(Clone)]
    struct FakeCommand {
        state: Arc<FakeBackendState>,
    }

    impl WorkerBackend for FakeBackend {
        type Session = FakeSession;
        type Command = FakeCommand;

        fn connect(
            &self,
            _profile: &PowerShellProfileConfiguration,
            _local_bind_address: Option<IpAddr>,
            _password: SecretBytes,
        ) -> Result<Self::Session, String> {
            self.state.connect_entered.store(true, Ordering::Release);
            while self.state.connect_blocked.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
            }
            Ok(FakeSession)
        }

        fn start_script(
            &self,
            _session: &Self::Session,
            _script: String,
        ) -> Result<Self::Command, String> {
            self.state.started.fetch_add(1, Ordering::AcqRel);
            Ok(FakeCommand {
                state: Arc::clone(&self.state),
            })
        }

        fn recv_timeout(
            &self,
            command: &mut Self::Command,
            _timeout: Duration,
        ) -> Result<Option<PowerShellCommandMessage>, PowerShellSessionError> {
            if command.state.cancelled.load(Ordering::Acquire) > 0 {
                return Ok(Some(PowerShellCommandMessage::Failed(
                    festerm_powershell::PowerShellCommandFailure {
                        kind: PowerShellCommandFailureKind::Cancelled,
                        state: Some(PipelineState::Stopped),
                        errors: Vec::new(),
                    },
                )));
            }
            command
                .state
                .messages
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(PowerShellSessionError::TimedOut))
        }

        fn cancel(&self, command: &Self::Command) {
            command.state.cancelled.fetch_add(1, Ordering::AcqRel);
        }

        fn close(&self, _session: &Self::Session) -> Result<(), String> {
            self.state.closed.fetch_add(1, Ordering::AcqRel);
            if let Some(error) = self.state.close_error.lock().unwrap().clone() {
                Err(error)
            } else {
                Ok(())
            }
        }
    }

    fn test_profile() -> PowerShellProfileConfiguration {
        festerm_config::Profile::powershell("ps", "host.test", 5986, "alice")
            .unwrap()
            .as_powershell()
            .unwrap()
            .clone()
    }

    fn spawn_fake_worker(
        fake: FakeBackend,
        request_rx: Receiver<WorkerRequest>,
        message_tx: SyncSender<WorkerMessage>,
        control: Arc<WorkerControl>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            run_worker_with_backend(
                fake,
                test_profile(),
                None,
                CredentialSource::Transient(SecretBytes::from_secret_string("pw".into())),
                request_rx,
                message_tx,
                control,
                egui::Context::default(),
                7,
            );
        })
    }

    fn recv_kind(rx: &Receiver<WorkerMessage>) -> WorkerMessageKind {
        rx.recv_timeout(Duration::from_secs(2))
            .expect("worker message")
            .kind
    }

    fn drain_tab_for(tab: &mut PowerShellTab, duration: Duration) {
        let context = egui::Context::default();
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            tab.drain_messages(&context);
            thread::sleep(Duration::from_millis(5));
        }
        tab.drain_messages(&context);
    }

    #[test]
    fn structured_control_characters_are_escaped() {
        assert_eq!(sanitize_display("ok\u{1b}[31m"), "ok\\u{1b}[31m");
    }

    #[test]
    fn value_presentation_preserves_stream_and_expandable_object() {
        let event = present_pipeline_event(PipelineEvent::Warning(PsValue::Object(
            PsObject::new().with("Name", PsValue::String("server\u{7}".into())),
        )));
        assert_eq!(event.stream, PowerShellStream::Warning);
        assert_eq!(event.expandable[0].1, "string:\"server\\u{7}\"");
    }

    #[test]
    fn presentation_queue_reports_overflow() {
        let mut tab = PowerShellTab::new(test_profile());
        for index in 0..(MAX_PRESENTED_EVENTS + 3) {
            tab.push_event(PowerShellPresentedEvent::lifecycle(format!(
                "event {index}"
            )));
        }
        assert!(tab.events.len() <= MAX_PRESENTED_EVENTS);
        assert!(tab.evicted_events > 0);
    }

    #[test]
    fn presentation_queue_enforces_cumulative_byte_budget() {
        let mut tab = PowerShellTab::new(test_profile());
        for index in 0..2000 {
            tab.push_event(PowerShellPresentedEvent::lifecycle(format!(
                "{index}:{}",
                "x".repeat(1024)
            )));
        }
        assert!(tab.events.len() <= MAX_PRESENTED_EVENTS);
        assert!(tab.presentation_bytes <= MAX_PRESENTATION_BYTES + 2048);
        assert!(tab.evicted_events > 0);
    }

    #[test]
    fn bounded_value_renderer_truncates_deep_and_large_values() {
        let mut value = PsValue::String("leaf".to_owned());
        for _ in 0..(MAX_VALUE_DEPTH + 4) {
            value = PsValue::List(vec![value]);
        }
        let rendered = bounded_value_text(&value);
        assert!(rendered.text.contains("<truncated>"));
        assert!(rendered.bytes <= MAX_DISPLAY_CHARS + " … <truncated>".len());
    }

    #[test]
    fn worker_keeps_session_ready_after_recoverable_pipeline_failure() {
        let failure = festerm_powershell::PowerShellCommandFailure {
            kind: PowerShellCommandFailureKind::PipelineFailed,
            state: Some(PipelineState::Failed),
            errors: vec![PsValue::String("bad".into())],
        };
        let fake = FakeBackend::new(vec![PowerShellCommandMessage::Failed(failure)]);
        let (request_tx, request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let (message_tx, message_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let control = Arc::new(WorkerControl::new());
        let handle = spawn_fake_worker(fake.clone(), request_rx, message_tx, control);
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::Connected
        ));
        request_tx
            .send(WorkerRequest::Run("Get-Thing".into()))
            .unwrap();
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::RecoverableFailure(_)
        ));
        request_tx.send(WorkerRequest::Close).unwrap();
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::Closed(Ok(()))
        ));
        handle.join().unwrap();
        assert_eq!(fake.state.closed.load(Ordering::Acquire), 1);
    }

    #[test]
    fn worker_invalidates_and_closes_on_transport_failure() {
        let failure = festerm_powershell::PowerShellCommandFailure {
            kind: PowerShellCommandFailureKind::Invalidated,
            state: Some(PipelineState::Disconnected),
            errors: Vec::new(),
        };
        let fake = FakeBackend::new(vec![PowerShellCommandMessage::Failed(failure)]);
        let (request_tx, request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let (message_tx, message_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let control = Arc::new(WorkerControl::new());
        let handle = spawn_fake_worker(fake.clone(), request_rx, message_tx, control);
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::Connected
        ));
        request_tx.send(WorkerRequest::Run("boom".into())).unwrap();
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::Invalidated(_)
        ));
        handle.join().unwrap();
        assert_eq!(fake.state.closed.load(Ordering::Acquire), 1);
    }

    #[test]
    fn worker_close_running_cancels_drains_and_closes() {
        let fake = FakeBackend::new(Vec::new());
        let (request_tx, request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let (message_tx, message_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let control = Arc::new(WorkerControl::new());
        let handle = spawn_fake_worker(fake.clone(), request_rx, message_tx, Arc::clone(&control));
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::Connected
        ));
        request_tx.send(WorkerRequest::Run("sleep".into())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while fake.state.started.load(Ordering::Acquire) == 0 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        control.close_requested.store(true, Ordering::Release);
        loop {
            if matches!(recv_kind(&message_rx), WorkerMessageKind::Closed(Ok(()))) {
                break;
            }
        }
        handle.join().unwrap();
        assert!(fake.state.cancelled.load(Ordering::Acquire) > 0);
        assert_eq!(fake.state.closed.load(Ordering::Acquire), 1);
    }

    #[test]
    fn worker_nonblocking_mailbox_allows_close_when_gui_queue_full() {
        let fake = FakeBackend::new(Vec::new());
        let (_request_tx, request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let (message_tx, _message_rx) = mpsc::sync_channel(0);
        let control = Arc::new(WorkerControl::new());
        control.close_requested.store(true, Ordering::Release);
        let handle = spawn_fake_worker(fake.clone(), request_rx, message_tx, Arc::clone(&control));
        handle.join().unwrap();
        assert_eq!(control.mailbox_drops.load(Ordering::Acquire), 0);
        assert_eq!(fake.state.closed.load(Ordering::Acquire), 1);
    }

    #[test]
    fn full_output_queue_still_delivers_completion_and_close() {
        let mut messages = Vec::new();
        for index in 0..256 {
            messages.push(PowerShellCommandMessage::Event(PipelineEvent::Output(
                PsValue::String(format!("output {index}")),
            )));
        }
        messages.push(PowerShellCommandMessage::Completed {
            state: PipelineState::Completed,
        });
        let fake = FakeBackend::new(messages);
        let (request_tx, request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let (message_tx, message_rx) = mpsc::sync_channel(8);
        let control = Arc::new(WorkerControl::new());
        let handle = spawn_fake_worker(fake.clone(), request_rx, message_tx, Arc::clone(&control));
        let mut tab = PowerShellTab::new(test_profile());
        tab.worker_generation = 7;
        tab.state = PowerShellState::Connecting;
        tab.sender = Some(request_tx.clone());
        tab.receiver = Some(message_rx);
        tab.control = Some(Arc::clone(&control));

        drain_tab_for(&mut tab, Duration::from_millis(50));
        assert_eq!(tab.state, PowerShellState::Ready);
        request_tx.send(WorkerRequest::Run("lots".into())).unwrap();
        thread::sleep(Duration::from_millis(100));
        request_tx.send(WorkerRequest::Close).unwrap();
        drain_tab_for(&mut tab, Duration::from_millis(500));

        assert_eq!(tab.state, PowerShellState::Closed);
        assert!(tab
            .events
            .iter()
            .any(|event| event.text.contains("Command completed")));
        assert!(tab
            .events
            .iter()
            .any(|event| event.text.contains("PowerShell session closed")));
        assert!(tab.worker_mailbox_drops > 0);
        handle.join().unwrap();
        assert_eq!(fake.state.closed.load(Ordering::Acquire), 1);
    }

    #[test]
    fn close_during_gated_connect_closes_without_connected_state() {
        let fake = FakeBackend::gated_connect();
        let (_request_tx, request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let (message_tx, message_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let control = Arc::new(WorkerControl::new());
        let handle = spawn_fake_worker(fake.clone(), request_rx, message_tx, Arc::clone(&control));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !fake.state.connect_entered.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        control.close_requested.store(true, Ordering::Release);
        fake.release_connect();
        let kind = recv_kind(&message_rx);
        assert!(matches!(kind, WorkerMessageKind::Closed(Ok(()))));
        handle.join().unwrap();
        assert!(control.finished.load(Ordering::Acquire));
        assert_eq!(fake.state.closed.load(Ordering::Acquire), 1);
    }

    #[test]
    fn dropped_ui_consumer_closes_worker_runspace() {
        let fake = FakeBackend::new(Vec::new());
        let (_request_tx, request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let (message_tx, message_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let control = Arc::new(WorkerControl::new());
        // Force Connected publication to observe the dropped consumer rather
        // than race into an idle worker with a still-live request sender.
        drop(message_rx);
        let handle = spawn_fake_worker(fake.clone(), request_rx, message_tx, control);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !handle.is_finished() {
            assert!(
                Instant::now() < deadline,
                "dropped consumer cleanup timed out"
            );
            thread::sleep(Duration::from_millis(5));
        }
        handle.join().unwrap();
        assert_eq!(fake.state.closed.load(Ordering::Acquire), 1);
    }

    #[test]
    fn invalidation_retains_shutdown_handle_and_forbids_reconnect_until_finished() {
        let mut tab = PowerShellTab::new(test_profile());
        let control = Arc::new(WorkerControl::new());
        let (request_tx, _request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        tab.control = Some(Arc::clone(&control));
        tab.sender = Some(request_tx);
        tab.state = PowerShellState::Ready;
        tab.apply_worker_message(WorkerMessage {
            generation: tab.worker_generation,
            kind: WorkerMessageKind::Invalidated("invalidated".to_owned()),
        });
        let handle = tab
            .shutdown_handle()
            .expect("invalidated worker remains tracked");
        assert!(!handle.is_finished());
        tab.password = "pw".to_owned();
        let generation = tab.lifecycle_generation();
        tab.dispatch(
            PowerShellAction::Connect,
            Err(festerm_secret_store::SecretStoreError::Unsupported),
            &egui::Context::default(),
        );
        assert_eq!(tab.lifecycle_generation(), generation);
        assert!(tab
            .events
            .iter()
            .any(|event| event.text.contains("previous PowerShell worker")));
        handle.finish_for_test();
        assert!(handle.is_finished());
    }

    #[test]
    fn run_and_connect_wrong_state_are_rejected_without_worker_mutation() {
        let mut tab = PowerShellTab::new(test_profile());
        tab.dispatch(
            PowerShellAction::Run("Get-Date".to_owned()),
            Err(festerm_secret_store::SecretStoreError::Unsupported),
            &egui::Context::default(),
        );
        assert_eq!(tab.state, PowerShellState::Setup);
        assert!(tab
            .events
            .iter()
            .any(|event| event.text.contains("requires a ready PowerShell session")));

        tab.state = PowerShellState::Running;
        tab.password = "pw".to_owned();
        let generation = tab.lifecycle_generation();
        tab.dispatch(
            PowerShellAction::Connect,
            Err(festerm_secret_store::SecretStoreError::Unsupported),
            &egui::Context::default(),
        );
        assert_eq!(tab.lifecycle_generation(), generation);
        assert!(tab
            .events
            .iter()
            .any(|event| event.text.contains("previous PowerShell worker")));
    }

    #[test]
    fn large_object_expandable_rows_are_bounded_and_report_truncation() {
        let mut object = PsObject::new();
        for index in 0..128 {
            object = object.with(
                format!("Property{index}"),
                PsValue::String("x".repeat(2048)),
            );
        }
        let event = present_pipeline_event(PipelineEvent::Output(PsValue::Object(object)));
        let total: usize = event
            .expandable
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum();
        assert!(total <= MAX_DISPLAY_CHARS * 2 + 256);
        assert!(event
            .expandable
            .iter()
            .any(|(name, value)| name == "…" && value.contains("truncated")));
    }

    #[test]
    fn narrow_full_surface_renders_without_terminal_assumptions() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(360.0, 516.0))
            .build_ui_state(
                |ui, tab: &mut PowerShellTab| {
                    let _ = tab.show(ui);
                },
                PowerShellTab::new(test_profile()),
            );
        harness.run_ok();
        assert!(harness.query_by_label("PowerShell ps").is_some());
        assert!(harness.query_by_label("Connect").is_some());
    }

    #[test]
    fn close_failure_is_reported_not_shaped_as_success() {
        let fake = FakeBackend::with_close_error("close exploded");
        let (request_tx, request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let (message_tx, message_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let control = Arc::new(WorkerControl::new());
        let handle = spawn_fake_worker(fake, request_rx, message_tx, control);
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::Connected
        ));
        request_tx.send(WorkerRequest::Close).unwrap();
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::Closed(Err(error)) if error == "close exploded"
        ));
        handle.join().unwrap();
    }

    #[test]
    fn stale_generation_completion_does_not_change_current_state() {
        let mut tab = PowerShellTab::new(test_profile());
        tab.worker_generation = 2;
        tab.state = PowerShellState::Connecting;
        let (tx, rx) = mpsc::sync_channel(WORKER_QUEUE);
        tab.receiver = Some(rx);
        tx.send(WorkerMessage {
            generation: 1,
            kind: WorkerMessageKind::Connected,
        })
        .unwrap();
        drop(tx);
        tab.drain_messages(&egui::Context::default());
        assert_eq!(tab.state, PowerShellState::Failed);
        assert!(tab
            .events
            .iter()
            .any(|event| event.text.contains("stale PowerShell worker")));
    }

    #[test]
    fn explicit_connection_reset_requires_fresh_ask_source_choice() {
        let profile = test_profile()
            .with_local_bind_policy(LocalBindPolicy::Ask)
            .unwrap();
        let mut tab = PowerShellTab::new(profile);
        tab.state = PowerShellState::Closed;
        tab.pending_close = true;
        tab.source =
            LocalBindDraft::from_policy(LocalBindPolicy::Address("127.0.0.1".parse().unwrap()));
        tab.reset_for_explicit_connection();
        assert!(!tab.pending_close);
        assert!(tab.resolve_source().is_err());
    }

    #[test]
    fn drain_messages_consumes_large_backlog_without_per_frame_starvation() {
        let mut tab = PowerShellTab::new(test_profile());
        tab.worker_generation = 9;
        let (tx, rx) = mpsc::sync_channel(WORKER_QUEUE * 2);
        tab.receiver = Some(rx);
        for index in 0..(WORKER_QUEUE * 2) {
            tx.send(WorkerMessage {
                generation: 9,
                kind: WorkerMessageKind::CommandEvent(PowerShellPresentedEvent::lifecycle(
                    format!("event {index}"),
                )),
            })
            .unwrap();
        }
        drop(tx);
        tab.drain_messages(&egui::Context::default());
        assert_eq!(tab.events.len(), MAX_FRAME_MESSAGES);
        tab.drain_messages(&egui::Context::default());
        assert_eq!(tab.events.len(), MAX_FRAME_MESSAGES * 2);
    }

    #[test]
    fn ask_source_requires_explicit_choice_before_connect() {
        let profile = test_profile()
            .with_local_bind_policy(LocalBindPolicy::Ask)
            .unwrap();
        let tab = PowerShellTab::new(profile);
        assert!(tab.resolve_source().is_err());
    }

    #[test]
    fn fixed_source_resolves_without_mutating_metadata() {
        let address: IpAddr = "127.0.0.2".parse().unwrap();
        let profile = test_profile()
            .with_local_bind_policy(LocalBindPolicy::Address(address))
            .unwrap();
        let tab = PowerShellTab::new(profile);
        assert_eq!(tab.resolve_source().unwrap().address(), Some(address));
    }
}
