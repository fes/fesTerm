//! Ephemeral Enterprise Dev Center discovery tab.
//!
//! Integration contract for the composition root:
//! - create with [`EnterpriseTab::new`]; render with [`EnterpriseTab::show`];
//! - route returned [`EnterpriseAction`] through `AppCommand::Enterprise` and call
//!   [`EnterpriseTab::dispatch`] only for the still-active tab;
//! - call [`EnterpriseTab::drain_messages`] during normal background draining;
//! - on tab/window close call [`EnterpriseTab::request_close`] and retain
//!   [`EnterpriseShutdownHandle`] until `is_finished()` is true.
//!
//! The tab persists no account, token, URL, or inventory state. A worker owns
//! the loopback listener, browser handoff, token, Dev Center client, and cleanup.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui;
use festerm_enterprise::{
    begin_authorization, AuthConfiguration, DevBox, DevCenterClient, DevCenterErrorKind,
    DevCenterUri, EntraTenantId, OperationControl, Project, ProjectName, PublicClientId,
};
use festerm_ui_egui::theme;

const WORKER_QUEUE: usize = 32;
const MAX_FRAME_MESSAGES: usize = 32;
const MAX_PRESENTED_EVENTS: usize = 256;
const MAX_PRESENTATION_BYTES: usize = 192 * 1024;
const MAX_DISPLAY_CHARS: usize = 8 * 1024;
const MAX_INPUT_BYTES: usize = 4096;
const MAX_OPTIONAL_PROJECT_BYTES: usize = 63;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_PRESENTED_PROJECTS: usize = 128;
const MAX_PRESENTED_DEV_BOXES: usize = 256;
const MAX_PRESENTED_TOTAL_ROWS: usize = 320;
const MAX_INVENTORY_BYTES: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EnterpriseState {
    Setup,
    SigningIn,
    Discovering,
    Ready,
    Failed,
    Closing,
    Closed,
}

impl EnterpriseState {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Setup => "Enterprise setup",
            Self::SigningIn => "Signing in",
            Self::Discovering => "Discovering",
            Self::Ready => "Inventory ready",
            Self::Failed => "Failed",
            Self::Closing => "Closing",
            Self::Closed => "Closed",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum EnterpriseAction {
    BeginSignIn,
    Cancel,
    SignOut,
    Close,
}

impl EnterpriseAction {
    pub(crate) const fn starts_worker(&self) -> bool {
        matches!(self, Self::BeginSignIn)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct EnterpriseDraft {
    tenant_id: String,
    client_id: String,
    dev_center_uri: String,
    project: String,
}

impl EnterpriseDraft {
    fn sanitized_project(&self) -> Option<String> {
        let project = self.project.trim();
        (!project.is_empty()).then(|| project.to_owned())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EnterpriseConfigurationInput {
    tenant_id: String,
    client_id: String,
    dev_center_uri: String,
    project: Option<String>,
}

impl EnterpriseConfigurationInput {
    fn from_draft(draft: &EnterpriseDraft) -> Self {
        Self {
            tenant_id: draft.tenant_id.trim().to_owned(),
            client_id: draft.client_id.trim().to_owned(),
            dev_center_uri: draft.dev_center_uri.trim().to_owned(),
            project: draft.sanitized_project(),
        }
    }

    fn fingerprint(&self) -> String {
        format!(
            "{}\n{}\n{}\n{}",
            self.tenant_id,
            self.client_id,
            self.dev_center_uri,
            self.project.as_deref().unwrap_or("")
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProjectSummary {
    name: String,
    display_name: Option<String>,
    dev_box_readable: bool,
    note: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DevBoxSummary {
    project_name: String,
    name: String,
    provisioning_state: Option<String>,
    pool_name: Option<String>,
    location: Option<String>,
    os_type: Option<String>,
    user: Option<String>,
}

impl ProjectSummary {
    fn presentation_bytes(&self) -> usize {
        self.name.len()
            + self.display_name.as_deref().map_or(0, str::len)
            + self.note.as_deref().map_or(0, str::len)
    }
}

impl DevBoxSummary {
    fn presentation_bytes(&self) -> usize {
        self.project_name.len()
            + self.name.len()
            + self.provisioning_state.as_deref().map_or(0, str::len)
            + self.pool_name.as_deref().map_or(0, str::len)
            + self.location.as_deref().map_or(0, str::len)
            + self.os_type.as_deref().map_or(0, str::len)
            + self.user.as_deref().map_or(0, str::len)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct EnterpriseInventory {
    projects: Vec<ProjectSummary>,
    dev_boxes: Vec<DevBoxSummary>,
    truncated: bool,
    stopped_fetching_at_budget: bool,
    omitted_projects: usize,
    omitted_dev_boxes: usize,
}

impl EnterpriseInventory {
    fn presentation_bytes(&self) -> usize {
        let project_bytes = self.projects.iter().map(|project| {
            project.name.len()
                + project.display_name.as_deref().map_or(0, str::len)
                + project.note.as_deref().map_or(0, str::len)
        });
        let dev_box_bytes = self.dev_boxes.iter().map(|dev_box| {
            dev_box.project_name.len()
                + dev_box.name.len()
                + dev_box.provisioning_state.as_deref().map_or(0, str::len)
                + dev_box.pool_name.as_deref().map_or(0, str::len)
                + dev_box.location.as_deref().map_or(0, str::len)
                + dev_box.os_type.as_deref().map_or(0, str::len)
                + dev_box.user.as_deref().map_or(0, str::len)
        });
        project_bytes.chain(dev_box_bytes).sum()
    }

    fn row_count(&self) -> usize {
        self.projects.len().saturating_add(self.dev_boxes.len())
    }

    fn has_project_capacity(&self, project: &ProjectSummary) -> bool {
        self.projects.len() < MAX_PRESENTED_PROJECTS
            && self.row_count() < MAX_PRESENTED_TOTAL_ROWS
            && self
                .presentation_bytes()
                .saturating_add(project.presentation_bytes())
                <= MAX_INVENTORY_BYTES
    }

    fn has_dev_box_capacity(&self, dev_box: &DevBoxSummary) -> bool {
        self.dev_boxes.len() < MAX_PRESENTED_DEV_BOXES
            && self.row_count() < MAX_PRESENTED_TOTAL_ROWS
            && self
                .presentation_bytes()
                .saturating_add(dev_box.presentation_bytes())
                <= MAX_INVENTORY_BYTES
    }

    fn push_project_bounded(&mut self, project: ProjectSummary) -> bool {
        if self.has_project_capacity(&project) {
            self.projects.push(project);
            true
        } else {
            self.truncated = true;
            self.stopped_fetching_at_budget = true;
            false
        }
    }

    fn push_dev_box_bounded(&mut self, dev_box: DevBoxSummary) -> bool {
        if self.has_dev_box_capacity(&dev_box) {
            self.dev_boxes.push(dev_box);
            true
        } else {
            self.truncated = true;
            self.stopped_fetching_at_budget = true;
            false
        }
    }

    fn is_at_budget(&self) -> bool {
        self.projects.len() >= MAX_PRESENTED_PROJECTS
            || self.dev_boxes.len() >= MAX_PRESENTED_DEV_BOXES
            || self.row_count() >= MAX_PRESENTED_TOTAL_ROWS
            || self.presentation_bytes() >= MAX_INVENTORY_BYTES
    }

    fn enforce_bounds(&mut self) {
        let mut rows = 0_usize;
        let mut bytes = 0_usize;
        let mut keep_projects = 0_usize;
        for project in &self.projects {
            let row_bytes = project.name.len()
                + project.display_name.as_deref().map_or(0, str::len)
                + project.note.as_deref().map_or(0, str::len);
            if keep_projects >= MAX_PRESENTED_PROJECTS
                || rows >= MAX_PRESENTED_TOTAL_ROWS
                || bytes.saturating_add(row_bytes) > MAX_INVENTORY_BYTES
            {
                break;
            }
            keep_projects += 1;
            rows += 1;
            bytes = bytes.saturating_add(row_bytes);
        }
        if keep_projects < self.projects.len() {
            self.omitted_projects = self
                .omitted_projects
                .saturating_add(self.projects.len() - keep_projects);
            self.projects.truncate(keep_projects);
            self.truncated = true;
        }

        let mut keep_dev_boxes = 0_usize;
        for dev_box in &self.dev_boxes {
            let row_bytes = dev_box.project_name.len()
                + dev_box.name.len()
                + dev_box.provisioning_state.as_deref().map_or(0, str::len)
                + dev_box.pool_name.as_deref().map_or(0, str::len)
                + dev_box.location.as_deref().map_or(0, str::len)
                + dev_box.os_type.as_deref().map_or(0, str::len)
                + dev_box.user.as_deref().map_or(0, str::len);
            if keep_dev_boxes >= MAX_PRESENTED_DEV_BOXES
                || rows >= MAX_PRESENTED_TOTAL_ROWS
                || bytes.saturating_add(row_bytes) > MAX_INVENTORY_BYTES
            {
                break;
            }
            keep_dev_boxes += 1;
            rows += 1;
            bytes = bytes.saturating_add(row_bytes);
        }
        if keep_dev_boxes < self.dev_boxes.len() {
            self.omitted_dev_boxes = self
                .omitted_dev_boxes
                .saturating_add(self.dev_boxes.len() - keep_dev_boxes);
            self.dev_boxes.truncate(keep_dev_boxes);
            self.truncated = true;
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PresentedEvent {
    text: String,
}

impl PresentedEvent {
    fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    fn presentation_bytes(&self) -> usize {
        self.text.len()
    }
}

struct WorkerControl {
    operation: OperationControl,
    close_requested: AtomicBool,
    mailbox_drops: AtomicUsize,
    finished: AtomicBool,
    pending_lifecycle: Mutex<VecDeque<WorkerMessage>>,
}

impl WorkerControl {
    fn new() -> Self {
        Self {
            operation: OperationControl::with_timeout(OPERATION_TIMEOUT)
                .expect("enterprise timeout is non-zero and representable"),
            close_requested: AtomicBool::new(false),
            mailbox_drops: AtomicUsize::new(0),
            finished: AtomicBool::new(false),
            pending_lifecycle: Mutex::new(VecDeque::new()),
        }
    }

    fn cancel(&self) {
        self.close_requested.store(true, Ordering::Release);
        self.operation.cancel();
    }
}

#[derive(Clone)]
pub(crate) struct EnterpriseShutdownHandle {
    control: Arc<WorkerControl>,
    sender: Option<SyncSender<WorkerRequest>>,
}

impl EnterpriseShutdownHandle {
    pub(crate) fn request_shutdown(&self) {
        self.control.cancel();
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(WorkerRequest::Close);
        }
    }

    #[allow(dead_code)]
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
    pub(crate) fn is_cancelled_for_test(&self) -> bool {
        self.control.operation.is_cancelled()
            && self.control.close_requested.load(Ordering::Acquire)
    }
}

struct WorkerFinishedGuard(Arc<WorkerControl>, egui::Context);

impl Drop for WorkerFinishedGuard {
    fn drop(&mut self) {
        self.0.finished.store(true, Ordering::Release);
        self.1.request_repaint();
    }
}

pub(crate) struct EnterpriseTab {
    draft: EnterpriseDraft,
    last_started_fingerprint: Option<String>,
    state: EnterpriseState,
    inventory: EnterpriseInventory,
    events: VecDeque<PresentedEvent>,
    presentation_bytes: usize,
    evicted_events: usize,
    worker_mailbox_drops: usize,
    sender: Option<SyncSender<WorkerRequest>>,
    receiver: Option<Receiver<WorkerMessage>>,
    control: Option<Arc<WorkerControl>>,
    worker_generation: u64,
    pending_close: bool,
    input_feedback: Option<String>,
    accept_worker_results: bool,
    invalidated_worker_completion_state: EnterpriseState,
}

impl EnterpriseTab {
    pub(crate) fn new() -> Self {
        Self {
            draft: EnterpriseDraft::default(),
            last_started_fingerprint: None,
            state: EnterpriseState::Setup,
            inventory: EnterpriseInventory::default(),
            events: VecDeque::new(),
            presentation_bytes: 0,
            evicted_events: 0,
            worker_mailbox_drops: 0,
            sender: None,
            receiver: None,
            control: None,
            worker_generation: 1,
            pending_close: false,
            input_feedback: None,
            accept_worker_results: false,
            invalidated_worker_completion_state: EnterpriseState::Closed,
        }
    }

    pub(crate) fn title(&self) -> &'static str {
        "Enterprise Dev Center"
    }

    pub(crate) fn status_label(&self) -> &'static str {
        self.state.label()
    }

    pub(crate) fn is_busy(&self) -> bool {
        matches!(
            self.state,
            EnterpriseState::SigningIn | EnterpriseState::Discovering | EnterpriseState::Closing
        ) || self
            .control
            .as_ref()
            .is_some_and(|control| !control.finished.load(Ordering::Acquire))
    }

    pub(crate) fn shutdown_handle(&self) -> Option<EnterpriseShutdownHandle> {
        self.control
            .as_ref()
            .map(|control| EnterpriseShutdownHandle {
                control: Arc::clone(control),
                sender: self.sender.clone(),
            })
    }

    #[cfg(test)]
    pub(crate) fn attach_shutdown_handle_for_test(&mut self, handle: EnterpriseShutdownHandle) {
        self.control = Some(Arc::clone(&handle.control));
        self.sender = handle.sender.clone();
    }

    pub(crate) fn request_close(&mut self) {
        if self.pending_close {
            return;
        }
        self.pending_close = true;
        self.invalidate_active_worker_results(EnterpriseState::Closed);
        self.request_worker_close();
        if self
            .control
            .as_ref()
            .is_none_or(|control| control.finished.load(Ordering::Acquire))
        {
            self.state = EnterpriseState::Closed;
            return;
        }
        self.state = EnterpriseState::Closing;
        self.push_event(PresentedEvent::new(
            "Enterprise discovery close requested; cancelling worker cleanup.",
        ));
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui) -> Option<EnterpriseAction> {
        self.drain_messages(ui.ctx());
        let mut action = None;
        crate::screens::show_bounded_content_scroll(ui, "enterprise-surface", |ui| {
            ui.vertical(|ui| {
                ui.add_space(12.0);
                ui.heading(self.title());
                ui.label(
                    egui::RichText::new(
                        "Read-only Dev Center discovery. Tokens are transient; Dev Box handoff URLs, broker/device-compliance claims, Windows logon, RDP launch, SSH launch, and remoting listeners are not provided here.",
                    )
                    .color(theme::TEXT_SECONDARY),
                );
                ui.separator();
                match self.state {
                    EnterpriseState::Setup | EnterpriseState::Failed | EnterpriseState::Closed => {
                        action = self.show_setup(ui);
                    }
                    EnterpriseState::SigningIn | EnterpriseState::Discovering => {
                        ui.spinner();
                        ui.label(self.state.label());
                        if ui.button("Cancel sign-in").clicked() {
                            action = Some(EnterpriseAction::Cancel);
                        }
                    }
                    EnterpriseState::Ready => {
                        action = self.show_inventory(ui);
                    }
                    EnterpriseState::Closing => {
                        ui.spinner();
                        ui.label("Cancelling enterprise discovery on the worker…");
                    }
                }
                ui.separator();
                self.show_events(ui);
            });
        });
        action
    }

    fn show_setup(&mut self, ui: &mut egui::Ui) -> Option<EnterpriseAction> {
        if matches!(
            self.state,
            EnterpriseState::Failed | EnterpriseState::Closed
        ) {
            ui.label("Start a new explicit sign-in to continue. No account or token is restored.");
        }
        let mut changed = false;
        changed |= limited_text_edit(
            ui,
            "Tenant GUID",
            &mut self.draft.tenant_id,
            MAX_INPUT_BYTES,
        );
        changed |= limited_text_edit(
            ui,
            "Public client GUID",
            &mut self.draft.client_id,
            MAX_INPUT_BYTES,
        );
        changed |= limited_text_edit(
            ui,
            "Dev Center URI",
            &mut self.draft.dev_center_uri,
            MAX_INPUT_BYTES,
        );
        changed |= limited_text_edit(
            ui,
            "Project (optional)",
            &mut self.draft.project,
            MAX_OPTIONAL_PROJECT_BYTES,
        );
        if changed {
            self.handle_configuration_edit();
        }
        self.input_feedback = validate_draft(&self.draft).err();
        if let Some(feedback) = &self.input_feedback {
            ui.colored_label(theme::STATUS_ERROR, feedback);
        }
        let closing = self
            .control
            .as_ref()
            .is_some_and(|control| !control.finished.load(Ordering::Acquire));
        if closing {
            ui.label("Waiting for the previous enterprise worker to finish cleanup.");
        }
        if ui
            .add_enabled(
                !closing && self.input_feedback.is_none(),
                egui::Button::new("Begin sign-in"),
            )
            .clicked()
        {
            return Some(EnterpriseAction::BeginSignIn);
        }
        None
    }

    fn show_inventory(&self, ui: &mut egui::Ui) -> Option<EnterpriseAction> {
        ui.horizontal(|ui| {
            ui.label("Signed in for this discovery only; no token is saved.");
        });
        if self.inventory.truncated {
            ui.colored_label(
                theme::STATUS_STARTING,
                format!(
                    "Inventory display was bounded; omitted {} fetched projects and {} fetched Dev Boxes.",
                    self.inventory.omitted_projects, self.inventory.omitted_dev_boxes
                ),
            );
            if self.inventory.stopped_fetching_at_budget {
                ui.colored_label(
                    theme::STATUS_STARTING,
                    "Additional project or Dev Box queries were not fetched after the display budget was reached.",
                );
            }
        }
        ui.heading("Authorized projects");
        if self.inventory.projects.is_empty() {
            ui.label("No authorized projects were returned for this account/configuration.");
        } else {
            egui::Grid::new("enterprise-projects-grid")
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("Project");
                    ui.strong("Display name");
                    ui.strong("Dev Boxes");
                    ui.strong("Note");
                    ui.end_row();
                    for project in &self.inventory.projects {
                        ui.label(sanitize_display(&project.name));
                        ui.label(
                            project
                                .display_name
                                .as_deref()
                                .map(sanitize_display)
                                .unwrap_or_else(|| "—".to_owned()),
                        );
                        ui.label(if project.dev_box_readable {
                            "Readable"
                        } else {
                            "Not authorized"
                        });
                        ui.label(
                            project
                                .note
                                .as_deref()
                                .map(sanitize_display)
                                .unwrap_or_else(|| "—".to_owned()),
                        );
                        ui.end_row();
                    }
                });
        }
        ui.add_space(8.0);
        ui.heading("Owned Dev Boxes");
        if self.inventory.dev_boxes.is_empty() {
            ui.label("No owned Dev Boxes were returned.");
        } else {
            egui::Grid::new("enterprise-devboxes-grid")
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("Project");
                    ui.strong("Name");
                    ui.strong("Status");
                    ui.strong("Pool");
                    ui.strong("Location");
                    ui.strong("OS");
                    ui.strong("User");
                    ui.end_row();
                    for dev_box in &self.inventory.dev_boxes {
                        ui.label(sanitize_display(&dev_box.project_name));
                        ui.label(sanitize_display(&dev_box.name));
                        ui.label(optional_display(&dev_box.provisioning_state));
                        ui.label(optional_display(&dev_box.pool_name));
                        ui.label(optional_display(&dev_box.location));
                        ui.label(optional_display(&dev_box.os_type));
                        ui.label(optional_display(&dev_box.user));
                        ui.end_row();
                    }
                });
        }
        ui.add_space(8.0);
        let mut action = None;
        ui.horizontal(|ui| {
            if ui.button("Sign out and clear").clicked() {
                action = Some(EnterpriseAction::SignOut);
            }
            if ui.button("Close").clicked() {
                action = Some(EnterpriseAction::Close);
            }
        });
        action
    }

    fn show_events(&self, ui: &mut egui::Ui) {
        if self.evicted_events > 0 {
            ui.colored_label(
                theme::STATUS_STARTING,
                format!(
                    "Presentation limit reached: {} older enterprise events were evicted.",
                    self.evicted_events
                ),
            );
        }
        if self.worker_mailbox_drops > 0 {
            ui.colored_label(
                theme::STATUS_STARTING,
                format!(
                    "Worker message backlog exceeded the GUI queue; {} updates were omitted.",
                    self.worker_mailbox_drops
                ),
            );
        }
        egui::ScrollArea::vertical()
            .id_salt("enterprise-events")
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for event in &self.events {
                    ui.group(|ui| {
                        ui.label(egui::RichText::new(&event.text).monospace());
                    });
                }
            });
    }

    pub(crate) fn dispatch(&mut self, action: EnterpriseAction, context: &egui::Context) {
        match action {
            EnterpriseAction::BeginSignIn => self.begin_sign_in(context),
            EnterpriseAction::Cancel => self.cancel_active_worker(),
            EnterpriseAction::SignOut => self.sign_out(),
            EnterpriseAction::Close => self.request_close(),
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
                    self.push_event(PresentedEvent::new(
                        "Ignored stale enterprise worker completion.",
                    ));
                }
                Ok(message) if !self.accept_worker_results => {
                    if matches!(message.kind, WorkerMessageKind::Closed) {
                        self.state = self.invalidated_worker_completion_state;
                        self.sender = None;
                        self.inventory = EnterpriseInventory::default();
                        self.last_started_fingerprint = None;
                        self.pending_close = false;
                        self.push_event(PresentedEvent::new(
                            "Enterprise worker closed; transient account state was cleared.",
                        ));
                    } else {
                        self.push_event(PresentedEvent::new(
                            "Ignored enterprise worker outcome after cancellation or configuration change.",
                        ));
                    }
                    context.request_repaint();
                }
                Ok(message)
                    if Some(message.fingerprint.as_str())
                        != self.last_started_fingerprint.as_deref() =>
                {
                    self.push_event(PresentedEvent::new(
                        "Ignored enterprise worker completion for a previous configuration.",
                    ));
                }
                Ok(message) => {
                    self.apply_worker_message(message.kind);
                    context.request_repaint();
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.sender = None;
                    receiver = None;
                    if self.state == EnterpriseState::Closing {
                        self.state = self.invalidated_worker_completion_state;
                        self.inventory = EnterpriseInventory::default();
                        self.last_started_fingerprint = None;
                        self.pending_close = false;
                    } else if !matches!(
                        self.state,
                        EnterpriseState::Ready
                            | EnterpriseState::Failed
                            | EnterpriseState::Closed
                            | EnterpriseState::Closing
                    ) {
                        self.state = EnterpriseState::Failed;
                        self.push_event(PresentedEvent::new(
                            "Enterprise worker stopped before reporting an outcome.",
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
                EnterpriseState::Closed | EnterpriseState::Failed | EnterpriseState::Ready
            )
        {
            self.sender = None;
        }
    }

    fn begin_sign_in(&mut self, context: &egui::Context) {
        if !matches!(
            self.state,
            EnterpriseState::Setup | EnterpriseState::Failed | EnterpriseState::Closed
        ) {
            self.push_event(PresentedEvent::new(
                "Enterprise sign-in is already active; cancel it before starting another.",
            ));
            return;
        }
        if self
            .control
            .as_ref()
            .is_some_and(|control| !control.finished.load(Ordering::Acquire))
        {
            self.push_event(PresentedEvent::new(
                "The previous enterprise worker must finish cleanup before a new sign-in can start.",
            ));
            return;
        }
        let input = EnterpriseConfigurationInput::from_draft(&self.draft);
        if let Err(error) = validate_input(&input) {
            self.state = EnterpriseState::Failed;
            self.push_event(PresentedEvent::new(error));
            return;
        }
        self.reset_for_explicit_sign_in();
        self.start_worker(input, context);
    }

    fn reset_for_explicit_sign_in(&mut self) {
        self.pending_close = false;
        self.sender = None;
        self.receiver = None;
        self.control = None;
        self.inventory = EnterpriseInventory::default();
        self.presentation_bytes = 0;
        self.evicted_events = 0;
        self.worker_mailbox_drops = 0;
        self.events.clear();
        self.accept_worker_results = true;
        self.invalidated_worker_completion_state = EnterpriseState::Closed;
    }

    fn start_worker(&mut self, input: EnterpriseConfigurationInput, context: &egui::Context) {
        let (request_tx, request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let (message_tx, message_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let control = Arc::new(WorkerControl::new());
        let worker_control = Arc::clone(&control);
        let repaint = context.clone();
        self.worker_generation = self.worker_generation.saturating_add(1);
        let generation = self.worker_generation;
        let fingerprint = input.fingerprint();
        self.last_started_fingerprint = Some(fingerprint.clone());
        self.accept_worker_results = true;
        self.invalidated_worker_completion_state = EnterpriseState::Closed;
        match thread::Builder::new()
            .name("festerm-enterprise-ui-worker".into())
            .spawn(move || {
                run_worker(
                    input,
                    request_rx,
                    message_tx,
                    worker_control,
                    repaint,
                    generation,
                    fingerprint,
                );
            }) {
            Ok(_) => {
                self.sender = Some(request_tx);
                self.receiver = Some(message_rx);
                self.control = Some(control);
                self.state = EnterpriseState::SigningIn;
                self.push_event(PresentedEvent::new(
                    "Binding loopback callback and opening browser on enterprise worker…",
                ));
                context.request_repaint();
            }
            Err(_) => {
                self.last_started_fingerprint = None;
                self.state = EnterpriseState::Failed;
                self.push_event(PresentedEvent::new(
                    "Enterprise worker thread could not be started.",
                ));
            }
        }
    }

    fn apply_worker_message(&mut self, kind: WorkerMessageKind) {
        match kind {
            WorkerMessageKind::ListenerReady => {
                self.state = if self.pending_close {
                    EnterpriseState::Closing
                } else {
                    EnterpriseState::SigningIn
                };
                self.push_event(PresentedEvent::new(
                    "Loopback callback listener is ready; browser sign-in can proceed.",
                ));
            }
            WorkerMessageKind::Authenticated => {
                self.state = if self.pending_close {
                    EnterpriseState::Closing
                } else {
                    EnterpriseState::Discovering
                };
                self.push_event(PresentedEvent::new(
                    "Authentication completed; discovering authorized projects and owned Dev Boxes.",
                ));
            }
            WorkerMessageKind::InventoryReady(mut inventory) => {
                inventory.enforce_bounds();
                if self.pending_close {
                    self.inventory = EnterpriseInventory::default();
                    self.state = EnterpriseState::Closed;
                } else {
                    self.inventory = inventory;
                    self.state = EnterpriseState::Ready;
                }
                self.sender = None;
                self.push_event(PresentedEvent::new(
                    "Enterprise discovery completed. No token or remote connection URL was saved.",
                ));
            }
            WorkerMessageKind::Failed(error) => {
                self.state = if self.pending_close {
                    EnterpriseState::Closed
                } else {
                    EnterpriseState::Failed
                };
                self.sender = None;
                self.inventory = EnterpriseInventory::default();
                self.push_event(PresentedEvent::new(format!(
                    "Enterprise discovery failed: {error}"
                )));
            }
            WorkerMessageKind::Closed => {
                self.state = EnterpriseState::Closed;
                self.sender = None;
                self.inventory = EnterpriseInventory::default();
                self.pending_close = false;
                self.push_event(PresentedEvent::new(
                    "Enterprise worker closed; transient account state was cleared.",
                ));
            }
        }
    }

    fn request_worker_close(&mut self) {
        if let Some(control) = &self.control {
            control.cancel();
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

    fn invalidate_active_worker_results(&mut self, completion_state: EnterpriseState) {
        self.accept_worker_results = false;
        self.invalidated_worker_completion_state = completion_state;
        self.last_started_fingerprint = None;
        self.inventory = EnterpriseInventory::default();
    }

    fn cancel_active_worker(&mut self) {
        if !self.is_busy() {
            self.push_event(PresentedEvent::new(
                "There is no enterprise sign-in or discovery to cancel.",
            ));
            return;
        }
        self.invalidate_active_worker_results(EnterpriseState::Closed);
        self.request_worker_close();
        self.state = EnterpriseState::Closing;
        self.push_event(PresentedEvent::new(
            "Cancellation requested; waiting for enterprise worker cleanup.",
        ));
    }

    fn sign_out(&mut self) {
        let worker_active = self
            .control
            .as_ref()
            .is_some_and(|control| !control.finished.load(Ordering::Acquire));
        self.invalidate_active_worker_results(EnterpriseState::Setup);
        self.request_worker_close();
        if worker_active {
            self.pending_close = true;
            self.state = EnterpriseState::Closing;
            self.push_event(PresentedEvent::new(
                "Enterprise account state cleared; waiting for worker cleanup.",
            ));
        } else {
            self.sender = None;
            self.receiver = None;
            self.control = None;
            self.last_started_fingerprint = None;
            self.state = EnterpriseState::Setup;
            self.push_event(PresentedEvent::new(
                "Enterprise account state cleared. No token was persisted.",
            ));
        }
    }

    fn handle_configuration_edit(&mut self) {
        let current_fingerprint =
            EnterpriseConfigurationInput::from_draft(&self.draft).fingerprint();
        if self.last_started_fingerprint.as_deref() != Some(current_fingerprint.as_str()) {
            self.inventory = EnterpriseInventory::default();
            if self.is_busy() {
                self.invalidate_active_worker_results(EnterpriseState::Setup);
                self.request_worker_close();
                self.state = EnterpriseState::Closing;
                self.push_event(PresentedEvent::new(
                    "Configuration changed; cancelled the previous enterprise worker.",
                ));
            } else if matches!(self.state, EnterpriseState::Ready) {
                self.state = EnterpriseState::Setup;
                self.push_event(PresentedEvent::new(
                    "Configuration changed; previous enterprise discovery results were cleared.",
                ));
            }
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

    fn push_event(&mut self, event: PresentedEvent) {
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

impl Drop for EnterpriseTab {
    fn drop(&mut self) {
        self.request_worker_close();
    }
}

#[derive(Clone, Debug)]
enum WorkerRequest {
    Close,
}

#[derive(Clone, Debug)]
struct WorkerMessage {
    generation: u64,
    fingerprint: String,
    kind: WorkerMessageKind,
}

#[derive(Clone, Debug)]
enum WorkerMessageKind {
    ListenerReady,
    Authenticated,
    InventoryReady(EnterpriseInventory),
    Failed(String),
    Closed,
}

trait EnterpriseBackend {
    fn discover(
        &self,
        input: &EnterpriseConfigurationInput,
        control: &OperationControl,
        phase: &mut dyn FnMut(WorkerMessageKind) -> bool,
    ) -> Result<EnterpriseInventory, String>;
}

struct RealEnterpriseBackend;

impl EnterpriseBackend for RealEnterpriseBackend {
    fn discover(
        &self,
        input: &EnterpriseConfigurationInput,
        control: &OperationControl,
        phase: &mut dyn FnMut(WorkerMessageKind) -> bool,
    ) -> Result<EnterpriseInventory, String> {
        let tenant_id =
            EntraTenantId::parse(&input.tenant_id).map_err(|error| error.to_string())?;
        let client_id =
            PublicClientId::parse(&input.client_id).map_err(|error| error.to_string())?;
        let endpoint =
            DevCenterUri::parse(&input.dev_center_uri).map_err(|error| error.to_string())?;
        let auth_configuration = AuthConfiguration::new(tenant_id, client_id);
        let session = begin_authorization(auth_configuration, control.clone())
            .map_err(|error| error.to_string())?;
        if !phase(WorkerMessageKind::ListenerReady) {
            return Err("receiver closed before browser sign-in".to_owned());
        }
        let token = session
            .open_browser_and_complete()
            .map_err(|error| error.to_string())?;
        if control.is_cancelled() {
            return Err("operation cancelled".to_owned());
        }
        if !phase(WorkerMessageKind::Authenticated) {
            return Err("receiver closed after authentication".to_owned());
        }
        let client = DevCenterClient::new(endpoint, token).map_err(|error| error.to_string())?;
        discover_inventory(&client, input, control)
    }
}

fn discover_inventory(
    client: &DevCenterClient,
    input: &EnterpriseConfigurationInput,
    control: &OperationControl,
) -> Result<EnterpriseInventory, String> {
    if let Some(project) = &input.project {
        let project_name = ProjectName::parse(project).map_err(|error| error.to_string())?;
        let dev_boxes = client
            .list_owned_dev_boxes(&project_name, control)
            .map_err(|error| error.to_string())?;
        let mut inventory = EnterpriseInventory::default();
        let _ = inventory.push_project_bounded(ProjectSummary {
            name: project.clone(),
            display_name: None,
            dev_box_readable: true,
            note: Some("Explicit project filter".to_owned()),
        });
        for dev_box in dev_boxes.into_iter().map(DevBoxSummary::from) {
            if !inventory.push_dev_box_bounded(dev_box) {
                break;
            }
        }
        return Ok(inventory);
    }

    let projects = client
        .list_projects(control)
        .map_err(|error| error.to_string())?;
    let mut inventory = EnterpriseInventory::default();
    'projects: for project in projects {
        if inventory.is_at_budget() {
            inventory.truncated = true;
            inventory.stopped_fetching_at_budget = true;
            break;
        }
        let project_name = match ProjectName::parse(&project.name) {
            Ok(project_name) => project_name,
            Err(error) => {
                if !inventory.push_project_bounded(ProjectSummary::from_project(
                    project,
                    false,
                    Some(error.to_string()),
                )) {
                    break;
                }
                continue;
            }
        };
        if !inventory.has_project_capacity(&ProjectSummary::from_project(
            project.clone(),
            true,
            None,
        )) {
            inventory.truncated = true;
            inventory.stopped_fetching_at_budget = true;
            break;
        }
        match client.list_owned_dev_boxes(&project_name, control) {
            Ok(dev_boxes) => {
                if !inventory
                    .push_project_bounded(ProjectSummary::from_project(project, true, None))
                {
                    break;
                }
                for dev_box in dev_boxes.into_iter().map(DevBoxSummary::from) {
                    if !inventory.push_dev_box_bounded(dev_box) {
                        break 'projects;
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    DevCenterErrorKind::AbilityDenied | DevCenterErrorKind::Forbidden
                ) =>
            {
                if !inventory.push_project_bounded(ProjectSummary::from_project(
                    project,
                    false,
                    Some(error.to_string()),
                )) {
                    break;
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(inventory)
}

impl ProjectSummary {
    fn from_project(project: Project, dev_box_readable: bool, note: Option<String>) -> Self {
        Self {
            name: project.name,
            display_name: project.display_name,
            dev_box_readable,
            note,
        }
    }
}

impl From<DevBox> for DevBoxSummary {
    fn from(value: DevBox) -> Self {
        Self {
            project_name: value.project_name,
            name: value.name,
            provisioning_state: value.provisioning_state,
            pool_name: value.pool_name,
            location: value.location,
            os_type: value.os_type,
            user: value.user,
        }
    }
}

fn run_worker(
    input: EnterpriseConfigurationInput,
    request_rx: Receiver<WorkerRequest>,
    message_tx: SyncSender<WorkerMessage>,
    control: Arc<WorkerControl>,
    repaint: egui::Context,
    generation: u64,
    fingerprint: String,
) {
    run_worker_with_backend(
        RealEnterpriseBackend,
        input,
        request_rx,
        message_tx,
        control,
        repaint,
        generation,
        fingerprint,
    );
}

#[allow(clippy::too_many_arguments)]
fn run_worker_with_backend<B: EnterpriseBackend>(
    backend: B,
    input: EnterpriseConfigurationInput,
    request_rx: Receiver<WorkerRequest>,
    message_tx: SyncSender<WorkerMessage>,
    control: Arc<WorkerControl>,
    repaint: egui::Context,
    generation: u64,
    fingerprint: String,
) {
    let _finished = WorkerFinishedGuard(Arc::clone(&control), repaint.clone());
    let send = |kind: WorkerMessageKind| -> bool {
        let message = WorkerMessage {
            generation,
            fingerprint: fingerprint.clone(),
            kind,
        };
        let mut pending = control.pending_lifecycle.lock().expect("lifecycle mutex");
        let result = if !pending.is_empty() {
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
                if pending.len() >= 4 {
                    control.cancel();
                    pending.clear();
                    pending.push_back(WorkerMessage {
                        generation,
                        fingerprint: fingerprint.clone(),
                        kind: WorkerMessageKind::Failed(
                            "Enterprise lifecycle backlog exceeded its bound; the operation was cancelled."
                                .to_owned(),
                        ),
                    });
                } else {
                    pending.push_back(message);
                }
                repaint.request_repaint();
                true
            }
            Err(TrySendError::Disconnected(_)) => {
                control.cancel();
                false
            }
        }
    };

    if control.close_requested.load(Ordering::Acquire) || control.operation.is_cancelled() {
        let _ = send(WorkerMessageKind::Closed);
        return;
    }
    if request_rx.try_recv().is_ok() {
        control.cancel();
        let _ = send(WorkerMessageKind::Closed);
        return;
    }

    let mut phase = |kind| {
        if control.close_requested.load(Ordering::Acquire) || control.operation.is_cancelled() {
            return false;
        }
        match request_rx.try_recv() {
            Ok(WorkerRequest::Close) | Err(TryRecvError::Disconnected) => {
                control.cancel();
                false
            }
            Err(TryRecvError::Empty) => send(kind),
        }
    };
    let result = backend.discover(&input, &control.operation, &mut phase);
    match request_rx.try_recv() {
        Ok(WorkerRequest::Close) => control.cancel(),
        Err(TryRecvError::Disconnected) => control.cancel(),
        Err(TryRecvError::Empty) => {}
    }
    if control.close_requested.load(Ordering::Acquire) || control.operation.is_cancelled() {
        let _ = send(WorkerMessageKind::Closed);
        return;
    }
    match result {
        Ok(mut inventory) => {
            inventory.enforce_bounds();
            if inventory.presentation_bytes() > MAX_INVENTORY_BYTES {
                inventory.enforce_bounds();
            }
            let _ = send(WorkerMessageKind::InventoryReady(inventory));
        }
        Err(error) => {
            let _ = send(WorkerMessageKind::Failed(sanitize_display(&error)));
        }
    }
}

fn validate_draft(draft: &EnterpriseDraft) -> Result<(), String> {
    validate_input(&EnterpriseConfigurationInput::from_draft(draft))
}

fn validate_input(input: &EnterpriseConfigurationInput) -> Result<(), String> {
    EntraTenantId::parse(&input.tenant_id).map_err(|error| error.to_string())?;
    PublicClientId::parse(&input.client_id).map_err(|error| error.to_string())?;
    DevCenterUri::parse(&input.dev_center_uri).map_err(|error| error.to_string())?;
    if let Some(project) = &input.project {
        ProjectName::parse(project).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn limited_text_edit(ui: &mut egui::Ui, label: &str, value: &mut String, limit: usize) -> bool {
    let response = ui.add(
        egui::TextEdit::singleline(value)
            .char_limit(limit + 1)
            .hint_text(label),
    );
    ui.label(
        egui::RichText::new(label)
            .small()
            .color(theme::TEXT_SECONDARY),
    );
    if response.changed() {
        enforce_byte_limit(value, limit);
        true
    } else {
        false
    }
}

fn enforce_byte_limit(text: &mut String, limit: usize) {
    if text.len() <= limit {
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
}

fn optional_display(value: &Option<String>) -> String {
    value
        .as_deref()
        .map(sanitize_display)
        .unwrap_or_else(|| "—".to_owned())
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

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    #[derive(Clone)]
    struct FakeBackend {
        state: Arc<FakeBackendState>,
    }

    struct FakeBackendState {
        inventory: Mutex<Option<EnterpriseInventory>>,
        error: Mutex<Option<String>>,
        entered: AtomicBool,
        blocked: AtomicBool,
        saw_cancel: AtomicBool,
        tokens_dropped: AtomicUsize,
        started: AtomicUsize,
    }

    impl FakeBackend {
        fn with_inventory(inventory: EnterpriseInventory) -> Self {
            Self {
                state: Arc::new(FakeBackendState {
                    inventory: Mutex::new(Some(inventory)),
                    error: Mutex::new(None),
                    entered: AtomicBool::new(false),
                    blocked: AtomicBool::new(false),
                    saw_cancel: AtomicBool::new(false),
                    tokens_dropped: AtomicUsize::new(0),
                    started: AtomicUsize::new(0),
                }),
            }
        }

        fn blocked(inventory: EnterpriseInventory) -> Self {
            let fake = Self::with_inventory(inventory);
            fake.state.blocked.store(true, Ordering::Release);
            fake
        }

        fn failing(error: &str) -> Self {
            let fake = Self::with_inventory(EnterpriseInventory::default());
            *fake.state.error.lock().unwrap() = Some(error.to_owned());
            fake
        }

        fn release(&self) {
            self.state.blocked.store(false, Ordering::Release);
        }
    }

    struct FakeToken(Arc<FakeBackendState>);

    impl Drop for FakeToken {
        fn drop(&mut self) {
            self.0.tokens_dropped.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl EnterpriseBackend for FakeBackend {
        fn discover(
            &self,
            _input: &EnterpriseConfigurationInput,
            control: &OperationControl,
            phase: &mut dyn FnMut(WorkerMessageKind) -> bool,
        ) -> Result<EnterpriseInventory, String> {
            self.state.started.fetch_add(1, Ordering::AcqRel);
            self.state.entered.store(true, Ordering::Release);
            if !phase(WorkerMessageKind::ListenerReady) {
                return Err("listener phase abandoned".to_owned());
            }
            let _token = FakeToken(Arc::clone(&self.state));
            if !phase(WorkerMessageKind::Authenticated) {
                return Err("authenticated phase abandoned".to_owned());
            }
            while self.state.blocked.load(Ordering::Acquire) {
                if control.is_cancelled() {
                    self.state.saw_cancel.store(true, Ordering::Release);
                    return Err("operation cancelled".to_owned());
                }
                thread::sleep(Duration::from_millis(5));
            }
            if control.is_cancelled() {
                self.state.saw_cancel.store(true, Ordering::Release);
                return Err("operation cancelled".to_owned());
            }
            if let Some(error) = self.state.error.lock().unwrap().clone() {
                return Err(error);
            }
            Ok(self
                .state
                .inventory
                .lock()
                .unwrap()
                .take()
                .unwrap_or_default())
        }
    }

    fn input(project: Option<&str>) -> EnterpriseConfigurationInput {
        EnterpriseConfigurationInput {
            tenant_id: "11111111-1111-1111-1111-111111111111".to_owned(),
            client_id: "22222222-2222-2222-2222-222222222222".to_owned(),
            dev_center_uri: "https://example.devcenter.azure.com".to_owned(),
            project: project.map(str::to_owned),
        }
    }

    fn inventory() -> EnterpriseInventory {
        EnterpriseInventory {
            projects: vec![ProjectSummary {
                name: "proj".to_owned(),
                display_name: Some("Project".to_owned()),
                dev_box_readable: true,
                note: None,
            }],
            dev_boxes: vec![DevBoxSummary {
                project_name: "proj".to_owned(),
                name: "box".to_owned(),
                provisioning_state: Some("Succeeded".to_owned()),
                pool_name: Some("pool".to_owned()),
                location: Some("westus".to_owned()),
                os_type: Some("Windows".to_owned()),
                user: Some("alice".to_owned()),
            }],
            truncated: false,
            stopped_fetching_at_budget: false,
            omitted_projects: 0,
            omitted_dev_boxes: 0,
        }
    }

    fn spawn_fake_worker(
        fake: FakeBackend,
        request_rx: Receiver<WorkerRequest>,
        message_tx: SyncSender<WorkerMessage>,
        control: Arc<WorkerControl>,
        generation: u64,
        fingerprint: String,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            run_worker_with_backend(
                fake,
                input(None),
                request_rx,
                message_tx,
                control,
                egui::Context::default(),
                generation,
                fingerprint,
            );
        })
    }

    fn recv_kind(rx: &Receiver<WorkerMessage>) -> WorkerMessageKind {
        rx.recv_timeout(Duration::from_secs(2))
            .expect("worker message")
            .kind
    }

    fn drain_tab_for(tab: &mut EnterpriseTab, duration: Duration) {
        let context = egui::Context::default();
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            tab.drain_messages(&context);
            thread::sleep(Duration::from_millis(5));
        }
        tab.drain_messages(&context);
    }

    fn active_tab_with_message_sender() -> (EnterpriseTab, SyncSender<WorkerMessage>) {
        let mut tab = EnterpriseTab::new();
        let (request_tx, _request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let (message_tx, message_rx) = mpsc::sync_channel(WORKER_QUEUE);
        tab.sender = Some(request_tx);
        tab.receiver = Some(message_rx);
        tab.control = Some(Arc::new(WorkerControl::new()));
        tab.worker_generation = 7;
        tab.last_started_fingerprint = Some("fp".to_owned());
        tab.accept_worker_results = true;
        tab.state = EnterpriseState::Discovering;
        tab.inventory = inventory();
        (tab, message_tx)
    }

    fn worker_message(kind: WorkerMessageKind) -> WorkerMessage {
        WorkerMessage {
            generation: 7,
            fingerprint: "fp".to_owned(),
            kind,
        }
    }

    #[test]
    fn worker_reports_success_and_drops_transient_token() {
        let fake = FakeBackend::with_inventory(inventory());
        let (request_tx, request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let (message_tx, message_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let control = Arc::new(WorkerControl::new());
        let handle = spawn_fake_worker(
            fake.clone(),
            request_rx,
            message_tx,
            control,
            7,
            "fp".to_owned(),
        );
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::ListenerReady
        ));
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::Authenticated
        ));
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::InventoryReady(_)
        ));
        drop(request_tx);
        handle.join().unwrap();
        assert_eq!(fake.state.tokens_dropped.load(Ordering::Acquire), 1);
    }

    #[test]
    fn cancellation_reaches_blocked_worker_and_finishes() {
        let fake = FakeBackend::blocked(inventory());
        let (request_tx, request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let (message_tx, message_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let control = Arc::new(WorkerControl::new());
        let handle = spawn_fake_worker(
            fake.clone(),
            request_rx,
            message_tx,
            Arc::clone(&control),
            7,
            "fp".to_owned(),
        );
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::ListenerReady
        ));
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::Authenticated
        ));
        control.cancel();
        request_tx.send(WorkerRequest::Close).unwrap();
        fake.release();
        handle.join().unwrap();
        assert!(control.finished.load(Ordering::Acquire));
        assert!(fake.state.saw_cancel.load(Ordering::Acquire));
        assert_eq!(fake.state.tokens_dropped.load(Ordering::Acquire), 1);
    }

    #[test]
    fn abandoned_receiver_cancels_after_auth_without_leaking_token() {
        let fake = FakeBackend::blocked(inventory());
        let (request_tx, request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let (message_tx, message_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let control = Arc::new(WorkerControl::new());
        let handle = spawn_fake_worker(
            fake.clone(),
            request_rx,
            message_tx,
            Arc::clone(&control),
            7,
            "fp".to_owned(),
        );
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::ListenerReady
        ));
        drop(message_rx);
        drop(request_tx);
        control.cancel();
        fake.release();
        handle.join().unwrap();
        assert!(control.operation.is_cancelled());
        assert_eq!(fake.state.tokens_dropped.load(Ordering::Acquire), 1);
    }

    #[test]
    fn stale_completion_is_ignored() {
        let mut tab = EnterpriseTab::new();
        tab.worker_generation = 2;
        tab.last_started_fingerprint = Some("current".to_owned());
        tab.apply_worker_message(WorkerMessageKind::ListenerReady);
        let (tx, rx) = mpsc::sync_channel(WORKER_QUEUE);
        tab.receiver = Some(rx);
        tab.control = Some(Arc::new(WorkerControl::new()));
        tx.send(WorkerMessage {
            generation: 1,
            fingerprint: "current".to_owned(),
            kind: WorkerMessageKind::InventoryReady(inventory()),
        })
        .unwrap();
        tab.drain_messages(&egui::Context::default());
        assert!(tab.inventory.projects.is_empty());
        assert!(tab
            .events
            .iter()
            .any(|event| event.text.contains("Ignored stale enterprise")));
    }

    #[test]
    fn queued_inventory_after_cancel_is_discarded_and_closes() {
        let (mut tab, tx) = active_tab_with_message_sender();
        tab.cancel_active_worker();
        tx.send(worker_message(WorkerMessageKind::InventoryReady(
            inventory(),
        )))
        .unwrap();
        drop(tx);
        drain_tab_for(&mut tab, Duration::from_millis(10));
        assert_eq!(tab.state, EnterpriseState::Closed);
        assert!(tab.inventory.projects.is_empty());
        assert!(tab
            .events
            .iter()
            .any(|event| event.text.contains("Ignored enterprise worker outcome")));
    }

    #[test]
    fn queued_results_after_config_edit_are_discarded_and_return_to_setup() {
        let (mut tab, tx) = active_tab_with_message_sender();
        tab.draft.project = "changed".to_owned();
        tab.handle_configuration_edit();
        tx.send(worker_message(WorkerMessageKind::Authenticated))
            .unwrap();
        tx.send(worker_message(WorkerMessageKind::InventoryReady(
            inventory(),
        )))
        .unwrap();
        drop(tx);
        drain_tab_for(&mut tab, Duration::from_millis(10));
        assert_eq!(tab.state, EnterpriseState::Setup);
        assert!(tab.inventory.projects.is_empty());
    }

    #[test]
    fn queued_inventory_after_signout_is_discarded_and_returns_to_setup() {
        let (mut tab, tx) = active_tab_with_message_sender();
        tab.sign_out();
        tx.send(worker_message(WorkerMessageKind::InventoryReady(
            inventory(),
        )))
        .unwrap();
        drop(tx);
        drain_tab_for(&mut tab, Duration::from_millis(10));
        assert_eq!(tab.state, EnterpriseState::Setup);
        assert!(tab.inventory.projects.is_empty());
    }

    #[test]
    fn config_switch_isolation_clears_results() {
        let mut tab = EnterpriseTab::new();
        tab.inventory = inventory();
        tab.state = EnterpriseState::Ready;
        tab.last_started_fingerprint = Some(input(None).fingerprint());
        tab.draft = EnterpriseDraft {
            tenant_id: input(None).tenant_id,
            client_id: input(None).client_id,
            dev_center_uri: input(None).dev_center_uri,
            project: "other".to_owned(),
        };
        tab.handle_configuration_edit();
        assert!(tab.inventory.projects.is_empty());
        assert_eq!(tab.state, EnterpriseState::Setup);
    }

    #[test]
    fn output_bounds_limit_large_inventory() {
        let mut inventory = EnterpriseInventory::default();
        for index in 0..1000 {
            inventory.projects.push(ProjectSummary {
                name: format!("proj-{index}"),
                display_name: Some("x".repeat(1024)),
                dev_box_readable: true,
                note: None,
            });
            inventory.dev_boxes.push(DevBoxSummary {
                project_name: format!("proj-{index}"),
                name: format!("box-{index}"),
                provisioning_state: Some("Succeeded".to_owned()),
                pool_name: Some("pool".to_owned()),
                location: None,
                os_type: None,
                user: None,
            });
        }
        inventory.enforce_bounds();
        assert!(inventory.projects.len() <= MAX_PRESENTED_PROJECTS);
        assert!(inventory.dev_boxes.len() <= MAX_PRESENTED_DEV_BOXES);
        assert!(inventory.projects.len() + inventory.dev_boxes.len() <= MAX_PRESENTED_TOTAL_ROWS);
        assert!(inventory.presentation_bytes() <= MAX_INVENTORY_BYTES);
        assert!(inventory.truncated);
    }

    #[test]
    fn bounded_push_stops_without_claiming_unfetched_omissions() {
        let mut inventory = EnterpriseInventory::default();
        for index in 0..1000 {
            if !inventory.push_project_bounded(ProjectSummary {
                name: format!("proj-{index}"),
                display_name: Some("x".repeat(2048)),
                dev_box_readable: true,
                note: None,
            }) {
                break;
            }
        }
        assert!(inventory.truncated);
        assert!(inventory.stopped_fetching_at_budget);
        assert_eq!(inventory.omitted_projects, 0);
        assert_eq!(inventory.omitted_dev_boxes, 0);
        assert!(inventory.projects.len() <= MAX_PRESENTED_PROJECTS);
        assert!(inventory.row_count() <= MAX_PRESENTED_TOTAL_ROWS);
        assert!(inventory.presentation_bytes() <= MAX_INVENTORY_BYTES);
    }

    #[test]
    fn shutdown_handle_requests_cancel_without_blocking() {
        let handle = EnterpriseShutdownHandle::pending_for_test();
        assert!(!handle.is_finished());
        handle.request_shutdown();
        assert!(handle.is_cancelled_for_test());
    }

    #[test]
    fn tab_retains_shutdown_handle_after_request_close_until_finished() {
        let mut tab = EnterpriseTab::new();
        let handle = EnterpriseShutdownHandle::pending_for_test();
        tab.attach_shutdown_handle_for_test(handle.clone());
        tab.state = EnterpriseState::Discovering;
        tab.request_close();
        let retained = tab.shutdown_handle().expect("shutdown handle retained");
        assert!(retained.is_cancelled_for_test());
        assert!(!retained.is_finished());
        handle.finish_for_test();
        assert!(retained.is_finished());
    }

    #[test]
    fn narrow_surface_renders_setup_controls() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(360.0, 516.0))
            .build_ui_state(
                |ui, tab: &mut EnterpriseTab| {
                    let _ = tab.show(ui);
                },
                EnterpriseTab::new(),
            );
        harness.run_ok();
        assert!(harness.query_by_label("Enterprise Dev Center").is_some());
        assert!(harness.query_by_label("Begin sign-in").is_some());
    }

    #[test]
    fn failed_backend_clears_inventory() {
        let fake = FakeBackend::failing("network broke");
        let (request_tx, request_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let (message_tx, message_rx) = mpsc::sync_channel(WORKER_QUEUE);
        let control = Arc::new(WorkerControl::new());
        let handle = spawn_fake_worker(fake, request_rx, message_tx, control, 7, "fp".to_owned());
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::ListenerReady
        ));
        assert!(matches!(
            recv_kind(&message_rx),
            WorkerMessageKind::Authenticated
        ));
        assert!(
            matches!(recv_kind(&message_rx), WorkerMessageKind::Failed(error) if error.contains("network"))
        );
        drop(request_tx);
        handle.join().unwrap();
    }

    #[test]
    fn tab_drains_successful_worker_messages() {
        let mut tab = EnterpriseTab::new();
        tab.worker_generation = 7;
        tab.last_started_fingerprint = Some("fp".to_owned());
        tab.accept_worker_results = true;
        let (tx, rx) = mpsc::sync_channel(WORKER_QUEUE);
        tab.receiver = Some(rx);
        tab.control = Some(Arc::new(WorkerControl::new()));
        tx.send(WorkerMessage {
            generation: 7,
            fingerprint: "fp".to_owned(),
            kind: WorkerMessageKind::InventoryReady(inventory()),
        })
        .unwrap();
        drain_tab_for(&mut tab, Duration::from_millis(10));
        assert_eq!(tab.state, EnterpriseState::Ready);
        assert_eq!(tab.inventory.projects.len(), 1);
    }
}
