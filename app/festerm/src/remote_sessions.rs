use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;

use eframe::egui;
use festerm_session::TerminalSize;
use festerm_sessiond::{RemoteSessionInventory, RemoteSessionTarget, RemoteSshEndpoint};
use festerm_ssh::{HostIdentity, SshAuthentication, SshConnectionProfile};
use festerm_ui_egui::theme;

use crate::local_bind::{LocalBindDraft, ResolvedLocalBind};

const MAX_INPUT_BYTES: usize = 1024;
const MAX_PASSWORD_BYTES: usize = 16 * 1024;
const MAX_HELPER_BYTES: usize = 128;
const MAX_MESSAGES: usize = 32;
const MAX_MESSAGE_BYTES: usize = 512;
const WORKER_QUEUE: usize = 1;

#[derive(Debug)]
pub(crate) enum RemoteSessionsAction {
    Discover,
    CancelDiscovery,
    SelectSession(String),
    BeginTakeoverReview,
    ConfirmTakeover,
}

impl RemoteSessionsAction {
    pub(crate) fn starts_worker(&self) -> bool {
        matches!(self, Self::Discover)
    }
}

pub(crate) struct RemoteAttachRequest {
    pub(crate) target: RemoteSessionTarget,
    pub(crate) authentication: SshAuthentication,
}

impl fmt::Debug for RemoteAttachRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteAttachRequest")
            .field("target", &self.target)
            .field("authentication", &format_args!("[REDACTED]"))
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct RemoteSessionsShutdownHandle {
    control: Arc<WorkerControl>,
}

impl RemoteSessionsShutdownHandle {
    pub(crate) fn request_shutdown(&self) {
        self.control.cancel_requested.store(true, Ordering::Release);
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.control.finished.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn cancel_requested_for_test(&self) -> bool {
        self.control.cancel_requested.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn pending_for_test() -> Self {
        Self {
            control: Arc::new(WorkerControl::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn finish_for_test(&self) {
        self.control.finished.store(true, Ordering::Release);
    }
}

struct WorkerControl {
    cancel_requested: Arc<AtomicBool>,
    finished: AtomicBool,
}

impl WorkerControl {
    fn new() -> Self {
        Self {
            cancel_requested: Arc::new(AtomicBool::new(false)),
            finished: AtomicBool::new(false),
        }
    }
}

struct WorkerFinishedGuard {
    control: Arc<WorkerControl>,
    context: egui::Context,
}

impl Drop for WorkerFinishedGuard {
    fn drop(&mut self) {
        self.control.finished.store(true, Ordering::Release);
        self.context.request_repaint();
    }
}

enum WorkerMessage {
    DiscoveryFinished {
        generation: u64,
        result: Result<RemoteSessionInventory, String>,
    },
}

#[derive(Clone)]
struct EndpointDraft {
    host: String,
    port: String,
    username: String,
    fingerprint: String,
    helper: String,
}

impl Default for EndpointDraft {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: "22".to_owned(),
            username: String::new(),
            fingerprint: String::new(),
            helper: "festerm-sessiond".to_owned(),
        }
    }
}

pub(crate) struct RemoteSessionsTab {
    endpoint: EndpointDraft,
    source: LocalBindDraft,
    discovery_password: String,
    takeover_password: String,
    inventory: Option<RemoteSessionInventory>,
    selected_name: Option<String>,
    selected_target: Option<RemoteSessionTarget>,
    takeover_review: bool,
    messages: VecDeque<String>,
    input_feedback: Option<String>,
    receiver: Option<Receiver<WorkerMessage>>,
    control: Option<Arc<WorkerControl>>,
    worker_generation: u64,
    pending_close: bool,
    status: String,
}

impl RemoteSessionsTab {
    pub(crate) fn new() -> Self {
        Self {
            endpoint: EndpointDraft::default(),
            source: LocalBindDraft::default(),
            discovery_password: String::new(),
            takeover_password: String::new(),
            inventory: None,
            selected_name: None,
            selected_target: None,
            takeover_review: false,
            messages: VecDeque::new(),
            input_feedback: None,
            receiver: None,
            control: None,
            worker_generation: 0,
            pending_close: false,
            status: "Setup".to_owned(),
        }
    }

    pub(crate) fn status_label(&self) -> &str {
        &self.status
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.control
            .as_ref()
            .is_some_and(|control| !control.finished.load(Ordering::Acquire))
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui) -> Option<RemoteSessionsAction> {
        self.drain_messages(ui.ctx());
        let mut action = None;
        crate::screens::show_bounded_content_scroll(ui, "remote-sessions-surface", |ui| {
            ui.vertical(|ui| {
                ui.add_space(12.0);
                ui.heading("Remote sessiond discovery");
                ui.label(
                    egui::RichText::new(
                        "Pinned-host-key SSH discovery for existing festerm-sessiond sessions. Discovery is read-only; attach requires an explicit takeover review.",
                    )
                    .color(theme::TEXT_SECONDARY),
                );
                ui.separator();
                if self.inventory.is_some() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Endpoint fields are locked to the discovered inventory.");
                        if ui.button("Edit endpoint and clear inventory").clicked() {
                            self.invalidate_inventory("Endpoint editing cleared the previous inventory.");
                        }
                    });
                }
                self.show_endpoint(ui);
                ui.separator();
                self.show_discovery_controls(ui, &mut action);
                ui.separator();
                self.show_inventory(ui, &mut action);
                ui.separator();
                self.show_messages(ui);
            });
        });
        action
    }

    fn show_endpoint(&mut self, ui: &mut egui::Ui) {
        let locked = self.inventory.is_some() || self.is_busy();
        ui.label(egui::RichText::new("SSH endpoint").color(theme::TEXT_PRIMARY));
        ui.add_enabled_ui(!locked, |ui| {
            endpoint_text(
                ui,
                "Host",
                &mut self.endpoint.host,
                MAX_INPUT_BYTES,
                "remote-host",
            );
            endpoint_text(ui, "Port", &mut self.endpoint.port, 5, "remote-port");
            endpoint_text(
                ui,
                "User",
                &mut self.endpoint.username,
                MAX_INPUT_BYTES,
                "remote-user",
            );
            endpoint_text(
                ui,
                "SHA256 host-key fingerprint",
                &mut self.endpoint.fingerprint,
                MAX_INPUT_BYTES,
                "remote-fingerprint",
            );
            endpoint_text(
                ui,
                "Remote helper executable",
                &mut self.endpoint.helper,
                MAX_HELPER_BYTES,
                "remote-helper",
            );
            crate::local_bind::show_local_bind_draft(
                ui,
                &mut self.source,
                false,
                "remote-sessiond",
            );
        });
        if locked && self.is_busy() {
            ui.label("Discovery is running; endpoint edits are disabled until it finishes or is cancelled.");
        }
        if let Some(feedback) = &self.input_feedback {
            ui.colored_label(theme::STATUS_ERROR, feedback);
        }
    }

    fn show_discovery_controls(
        &mut self,
        ui: &mut egui::Ui,
        action: &mut Option<RemoteSessionsAction>,
    ) {
        ui.label(egui::RichText::new("Discovery credential").color(theme::TEXT_PRIMARY));
        let busy = self.is_busy();
        let response = ui.add_enabled(
            !busy,
            egui::TextEdit::singleline(&mut self.discovery_password)
                .password(true)
                .char_limit(MAX_PASSWORD_BYTES + 1)
                .hint_text("Transient SSH password (not saved)"),
        );
        if response.changed() {
            enforce_byte_limit(
                &mut self.discovery_password,
                MAX_PASSWORD_BYTES,
                &mut self.input_feedback,
                "Password",
            );
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !busy && self.input_feedback.is_none(),
                    egui::Button::new("Discover sessions"),
                )
                .clicked()
            {
                *action = Some(RemoteSessionsAction::Discover);
            }
            if ui
                .add_enabled(busy, egui::Button::new("Cancel discovery"))
                .clicked()
            {
                *action = Some(RemoteSessionsAction::CancelDiscovery);
            }
        });
        ui.label(
            egui::RichText::new(
                "The password is consumed by this discovery worker only and is not reused for attach.",
            )
            .size(11.0)
            .color(theme::TEXT_SECONDARY),
        );
    }

    fn show_inventory(&mut self, ui: &mut egui::Ui, action: &mut Option<RemoteSessionsAction>) {
        let Some(inventory) = &self.inventory else {
            ui.label("No inventory loaded.");
            return;
        };
        let mut rows = 0usize;
        ui.label(format!(
            "Discovered {} record(s) for {}@{}:{} via {}.",
            inventory.sessions().len(),
            inventory.endpoint().profile().username(),
            inventory.endpoint().profile().identity().host(),
            inventory.endpoint().profile().identity().port(),
            inventory.endpoint().helper()
        ));
        egui::ScrollArea::vertical()
            .id_salt("remote-session-inventory")
            .max_height(260.0)
            .show(ui, |ui| {
                for session in inventory.sessions() {
                    rows = rows.saturating_add(1);
                    let selected = self.selected_name.as_deref() == Some(session.name.as_str());
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .selectable_label(selected, bounded_label(&session.name))
                            .clicked()
                        {
                            *action =
                                Some(RemoteSessionsAction::SelectSession(session.name.clone()));
                        }
                        ui.label(format!("{:?}", session.status));
                        if session.attached == Some(true) {
                            ui.label("attached");
                        }
                        if let Some(pid) = session.pid {
                            ui.label(format!("pid {pid}"));
                        }
                        if let Some(generation) = session.created_at_unix_ms {
                            ui.label(format!("generation {generation}"));
                        }
                    });
                }
            });
        if rows == 0 {
            ui.label("No sessions were reported by the helper.");
        }
        if let Some(target) = &self.selected_target {
            ui.separator();
            ui.label(egui::RichText::new("Takeover review").color(theme::TEXT_PRIMARY));
            ui.label(format!(
                "Selected exact generation: {} pid {} generation {}.",
                target.name(),
                target.pid(),
                target.generation()
            ));
            ui.label("Takeover is required even if discovery reported the session as unattached, because the registry is advisory.");
            ui.label(
                "An attached frontend will be disconnected. The daemon and its shell keep running.",
            );
            if !self.takeover_review {
                if ui.button("Review takeover").clicked() {
                    *action = Some(RemoteSessionsAction::BeginTakeoverReview);
                }
                return;
            }
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.takeover_password)
                    .password(true)
                    .char_limit(MAX_PASSWORD_BYTES + 1)
                    .hint_text("Fresh transient SSH password for attach"),
            );
            if response.changed() {
                enforce_byte_limit(
                    &mut self.takeover_password,
                    MAX_PASSWORD_BYTES,
                    &mut self.input_feedback,
                    "Attach password",
                );
            }
            if ui
                .add_enabled(
                    self.input_feedback.is_none(),
                    egui::Button::new("Confirm takeover and attach"),
                )
                .clicked()
            {
                *action = Some(RemoteSessionsAction::ConfirmTakeover);
            }
        }
    }

    fn show_messages(&self, ui: &mut egui::Ui) {
        for message in &self.messages {
            ui.label(egui::RichText::new(message).monospace());
        }
    }

    pub(crate) fn dispatch(
        &mut self,
        action: RemoteSessionsAction,
        context: &egui::Context,
    ) -> Option<RemoteAttachRequest> {
        if self.pending_close {
            self.push_error("Remote session picker is closing; action was cancelled.");
            return None;
        }
        match action {
            RemoteSessionsAction::Discover => {
                self.start_discovery(context);
                None
            }
            RemoteSessionsAction::CancelDiscovery => {
                self.cancel_discovery();
                None
            }
            RemoteSessionsAction::SelectSession(name) => {
                self.select_session(name);
                None
            }
            RemoteSessionsAction::BeginTakeoverReview => {
                if self.selected_target.is_some() {
                    self.takeover_review = true;
                    self.push_message(
                        "Review the exact generation and enter a fresh attach password.",
                    );
                }
                None
            }
            RemoteSessionsAction::ConfirmTakeover => self.confirm_takeover(),
        }
    }

    pub(crate) fn drain_messages(&mut self, context: &egui::Context) {
        let mut retried_after_finished = false;
        loop {
            let message = match self.receiver.as_ref().map(Receiver::try_recv) {
                Some(Ok(message)) => {
                    retried_after_finished = false;
                    message
                }
                Some(Err(TryRecvError::Empty)) => {
                    if !retried_after_finished
                        && self
                            .control
                            .as_ref()
                            .is_some_and(|control| control.finished.load(Ordering::Acquire))
                    {
                        retried_after_finished = true;
                        continue;
                    }
                    break;
                }
                None => break,
                Some(Err(TryRecvError::Disconnected)) => {
                    self.receiver = None;
                    if !self.pending_close {
                        self.status = "Discovery failed".to_owned();
                        self.push_error("Discovery worker stopped without reporting an outcome.");
                    }
                    break;
                }
            };
            self.handle_worker_message(message);
            context.request_repaint();
        }
        if self.receiver.is_none()
            && self
                .control
                .as_ref()
                .is_some_and(|control| control.finished.load(Ordering::Acquire))
        {
            self.control = None;
            if self.pending_close {
                self.status = "Closed".to_owned();
            }
        }
    }

    pub(crate) fn request_close(&mut self) {
        self.pending_close = true;
        self.cancel_discovery();
        self.clear_ephemeral_state();
        if !self.is_busy() {
            self.status = "Closed".to_owned();
        }
    }

    pub(crate) fn shutdown_handle(&self) -> Option<RemoteSessionsShutdownHandle> {
        self.control
            .as_ref()
            .map(|control| RemoteSessionsShutdownHandle {
                control: Arc::clone(control),
            })
    }

    fn start_discovery(&mut self, context: &egui::Context) {
        if self.is_busy() {
            self.push_message("Discovery is already running.");
            return;
        }
        if self.input_feedback.is_some() {
            return;
        }
        let endpoint = match self.build_endpoint() {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.push_error(error);
                return;
            }
        };
        if self.discovery_password.is_empty() {
            self.push_error("Enter a transient SSH password for discovery.");
            return;
        }
        let password = std::mem::take(&mut self.discovery_password);
        self.inventory = None;
        self.selected_name = None;
        self.selected_target = None;
        self.takeover_review = false;
        self.takeover_password.clear();
        self.worker_generation = self.worker_generation.saturating_add(1);
        let generation = self.worker_generation;
        let (sender, receiver) = mpsc::sync_channel(WORKER_QUEUE);
        let control = Arc::new(WorkerControl::new());
        let worker_control = Arc::clone(&control);
        let repaint = context.clone();
        let spawn = thread::Builder::new()
            .name("festerm-remote-session-discovery".into())
            .spawn(move || {
                let _guard = WorkerFinishedGuard {
                    control: Arc::clone(&worker_control),
                    context: repaint,
                };
                let result = endpoint
                    .discover_with_cancellation(
                        SshAuthentication::password(password),
                        Arc::clone(&worker_control.cancel_requested),
                    )
                    .map_err(|error| error.to_string());
                let _ = sender.try_send(WorkerMessage::DiscoveryFinished { generation, result });
            });
        match spawn {
            Ok(_) => {
                self.receiver = Some(receiver);
                self.control = Some(control);
                self.status = "Discovering".to_owned();
                self.push_message("Discovery started.");
            }
            Err(error) => {
                self.status = "Discovery failed".to_owned();
                self.push_error(format!("Could not start discovery worker: {error}"));
            }
        }
    }

    fn cancel_discovery(&mut self) {
        if let Some(control) = &self.control {
            control.cancel_requested.store(true, Ordering::Release);
            if !control.finished.load(Ordering::Acquire) {
                self.status = "Cancelling".to_owned();
                self.push_message("Discovery cancellation requested.");
            }
        }
    }

    fn handle_worker_message(&mut self, message: WorkerMessage) {
        match message {
            WorkerMessage::DiscoveryFinished { generation, result } => {
                if generation != self.worker_generation {
                    self.push_message("Ignored stale discovery completion.");
                    return;
                }
                let cancelled = self
                    .control
                    .as_ref()
                    .is_some_and(|control| control.cancel_requested.load(Ordering::Acquire));
                self.receiver = None;
                if cancelled || self.pending_close {
                    self.clear_ephemeral_state();
                    self.status = if self.pending_close {
                        "Closed".to_owned()
                    } else {
                        "Cancelled".to_owned()
                    };
                    self.push_message("Ignored discovery completion after cancellation.");
                    return;
                }
                match result {
                    Ok(inventory) => {
                        let count = inventory.sessions().len();
                        self.inventory = Some(inventory);
                        self.status = format!("{count} discovered");
                        self.push_message(format!("Discovery completed with {count} record(s)."));
                    }
                    Err(error) => {
                        self.status = "Discovery failed".to_owned();
                        self.push_error(error);
                    }
                }
            }
        }
    }

    fn select_session(&mut self, name: String) {
        let Some(inventory) = &self.inventory else {
            self.push_error("No discovery inventory is available.");
            return;
        };
        match inventory.select(&name) {
            Ok(target) => {
                self.selected_name = Some(name);
                self.selected_target = Some(target);
                self.takeover_review = false;
                self.takeover_password.clear();
                self.status = "Session selected".to_owned();
                self.push_message("Selected session. Explicit takeover review is still required.");
            }
            Err(error) => {
                self.selected_name = None;
                self.selected_target = None;
                self.takeover_review = false;
                self.push_error(error.to_string());
            }
        }
    }

    fn confirm_takeover(&mut self) -> Option<RemoteAttachRequest> {
        if self.pending_close {
            self.push_error("Remote session picker is closing; attach was cancelled.");
            return None;
        }
        if !self.takeover_review {
            self.push_error("Review takeover before attaching.");
            return None;
        }
        let Some(target) = self.selected_target.clone() else {
            self.push_error("Select a compatible discovered session before attaching.");
            return None;
        };
        if self.takeover_password.is_empty() {
            self.push_error("Enter a fresh transient SSH password for attach.");
            return None;
        }
        self.status = "Attach requested".to_owned();
        self.push_message("Takeover confirmed; handing exact target to the application.");
        self.takeover_review = false;
        Some(RemoteAttachRequest {
            target,
            authentication: SshAuthentication::password(std::mem::take(
                &mut self.takeover_password,
            )),
        })
    }

    fn build_endpoint(&self) -> Result<RemoteSshEndpoint, String> {
        let host = bounded_trimmed(&self.endpoint.host, MAX_INPUT_BYTES, "Host")?;
        let username = bounded_trimmed(&self.endpoint.username, MAX_INPUT_BYTES, "User")?;
        let fingerprint =
            bounded_trimmed(&self.endpoint.fingerprint, MAX_INPUT_BYTES, "Fingerprint")?;
        let helper = bounded_trimmed(&self.endpoint.helper, MAX_HELPER_BYTES, "Helper")?;
        let port_text = bounded_trimmed(&self.endpoint.port, 5, "Port")?;
        let port = port_text
            .parse::<u16>()
            .map_err(|_| "Enter a valid SSH port from 1 to 65535.".to_owned())?;
        let identity = HostIdentity::new(host, port).map_err(|error| error.to_string())?;
        let size = TerminalSize::new(80, 24).map_err(|error| error.to_string())?;
        let profile = SshConnectionProfile::new(
            identity,
            username,
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            size,
        )
        .map_err(|error| error.to_string())?;
        let profile = match self.source.resolved()? {
            ResolvedLocalBind::Automatic => profile,
            ResolvedLocalBind::Address(address) => profile
                .with_local_bind_address(Some(address))
                .map_err(|error| error.to_string())?,
        };
        RemoteSshEndpoint::new(profile, fingerprint, helper).map_err(|error| error.to_string())
    }

    fn invalidate_inventory(&mut self, message: impl Into<String>) {
        self.clear_selection_state();
        self.status = "Setup".to_owned();
        self.push_message(message);
    }

    fn clear_ephemeral_state(&mut self) {
        self.discovery_password.clear();
        self.clear_selection_state();
    }

    fn clear_selection_state(&mut self) {
        self.inventory = None;
        self.selected_name = None;
        self.selected_target = None;
        self.takeover_review = false;
        self.takeover_password.clear();
    }

    fn push_error(&mut self, message: impl Into<String>) {
        self.push_message(message);
    }

    fn push_message(&mut self, message: impl Into<String>) {
        let mut message = message.into();
        enforce_string_byte_limit(&mut message, MAX_MESSAGE_BYTES);
        if self.messages.len() == MAX_MESSAGES {
            self.messages.pop_front();
        }
        self.messages.push_back(message);
    }

    #[cfg(test)]
    fn inject_discovery_for_test(
        &mut self,
        generation: u64,
        result: Result<RemoteSessionInventory, String>,
    ) {
        self.handle_worker_message(WorkerMessage::DiscoveryFinished { generation, result });
    }

    #[cfg(test)]
    fn set_generation_for_test(&mut self, generation: u64) {
        self.worker_generation = generation;
    }

    #[cfg(test)]
    fn set_endpoint_for_test(&mut self, endpoint: EndpointDraft) {
        self.endpoint = endpoint;
    }

    #[cfg(test)]
    fn set_takeover_password_for_test(&mut self, password: &str) {
        self.takeover_password = password.to_owned();
    }

    #[cfg(test)]
    fn message_count_for_test(&self) -> usize {
        self.messages.len()
    }

    #[cfg(test)]
    pub(crate) fn attach_shutdown_handle_for_test(&mut self, handle: RemoteSessionsShutdownHandle) {
        self.control = Some(handle.control);
    }

    #[cfg(test)]
    fn attach_receiver_for_test(
        &mut self,
        receiver: Receiver<WorkerMessage>,
        handle: RemoteSessionsShutdownHandle,
    ) {
        self.receiver = Some(receiver);
        self.control = Some(handle.control);
    }
}

impl Drop for RemoteSessionsTab {
    fn drop(&mut self) {
        if let Some(control) = &self.control {
            control.cancel_requested.store(true, Ordering::Release);
        }
        self.discovery_password.clear();
        self.takeover_password.clear();
    }
}

fn endpoint_text(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    max_bytes: usize,
    id: &'static str,
) {
    let response = ui.horizontal_wrapped(|ui| {
        let label_response = ui.label(label);
        ui.add(
            egui::TextEdit::singleline(value)
                .id(ui.make_persistent_id(id))
                .char_limit(max_bytes + 1)
                .desired_width(260.0),
        )
        .labelled_by(label_response.id)
    });
    if response.inner.changed() {
        enforce_string_byte_limit(value, max_bytes);
    }
}

fn bounded_trimmed(input: &str, max_bytes: usize, label: &str) -> Result<String, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} is required."));
    }
    if trimmed.len() > max_bytes {
        return Err(format!("{label} is limited to {max_bytes} bytes."));
    }
    Ok(trimmed.to_owned())
}

fn bounded_label(input: &str) -> String {
    let mut value = input.to_owned();
    enforce_string_byte_limit(&mut value, 96);
    value
}

fn enforce_byte_limit(
    value: &mut String,
    max_bytes: usize,
    feedback: &mut Option<String>,
    label: &str,
) {
    if value.len() <= max_bytes {
        *feedback = None;
        return;
    }
    enforce_string_byte_limit(value, max_bytes);
    *feedback = Some(format!("{label} is limited to {max_bytes} bytes."));
}

fn enforce_string_byte_limit(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value.truncate(end);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn endpoint() -> RemoteSshEndpoint {
        RemoteSshEndpoint::new(
            SshConnectionProfile::new(
                HostIdentity::new("example.test", 22).unwrap(),
                "deploy",
                SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
                TerminalSize::new(80, 24).unwrap(),
            )
            .unwrap(),
            "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "festerm-sessiond",
        )
        .unwrap()
    }

    fn inventory(status: &str, protocol: u16, snapshot: u16) -> RemoteSessionInventory {
        let wire = json!({
            "schema_version": 1,
            "inventory": {"record_count": 1, "serialized_bytes": 256, "status_counts": {status: 1}},
            "sessions": [{
                "name": "existing", "validated_name": true, "pid": 77,
                "created_at_unix_ms": 987654321_u128, "attached": status == "attached",
                "status": status,
                "daemon_protocol": {"version": protocol, "supported": protocol == festerm_sessiond::PROTOCOL_VERSION},
                "recovery_snapshot_schema": {"version": snapshot, "supported": snapshot == festerm_sessiond::RECOVERY_SNAPSHOT_SCHEMA_VERSION}
            }]
        });
        RemoteSessionInventory::decode(endpoint(), &serde_json::to_vec(&wire).unwrap()).unwrap()
    }

    #[test]
    fn remote_picker_click_flow_requires_review_before_emitting_exact_target() {
        use egui_kittest::{kittest::Queryable, Harness};

        for size in [egui::vec2(360.0, 516.0), egui::vec2(752.0, 516.0)] {
            let mut tab = RemoteSessionsTab::new();
            tab.inject_discovery_for_test(0, Ok(inventory("available", 2, 2)));
            let mut harness = Harness::builder().with_size(size).build_ui_state(
                |ui, state: &mut (RemoteSessionsTab, Option<RemoteAttachRequest>)| {
                    if let Some(action) = state.0.show(ui) {
                        if let Some(request) = state.0.dispatch(action, ui.ctx()) {
                            state.1 = Some(request);
                        }
                    }
                },
                (tab, None),
            );
            harness.run();
            harness
                .get_by_label("Discovery completed with 1 record(s).")
                .scroll_to_me();
            harness.run();
            harness.get_by_label("existing").click();
            harness.run();
            assert!(harness.state().1.is_none());
            assert!(harness
                .query_by_label("Confirm takeover and attach")
                .is_none());
            harness.get_by_label("Review takeover").scroll_to_me();
            harness.run();
            harness.get_by_label("Review takeover").click();
            harness.run();
            harness.state_mut().0.takeover_password = "fixture-password".to_owned();
            harness
                .get_by_label("Confirm takeover and attach")
                .scroll_to_me();
            harness.run();
            harness.get_by_label("Confirm takeover and attach").click();
            harness.run();
            let request = harness.state().1.as_ref().expect("explicit attach request");
            assert_eq!(request.target.name(), "existing");
            assert_eq!(request.target.pid(), 77);
            assert_eq!(request.target.generation(), 987654321);
            assert!(!harness.state().0.takeover_review);
            assert!(harness.state().0.takeover_password.is_empty());
        }
    }

    #[test]
    fn stale_and_cancelled_discovery_results_do_not_replace_current_inventory() {
        let mut tab = RemoteSessionsTab::new();
        tab.set_generation_for_test(2);

        tab.inject_discovery_for_test(1, Ok(inventory("available", 2, 2)));
        assert!(tab.inventory.is_none());

        tab.inject_discovery_for_test(2, Err("remote discovery was cancelled".to_owned()));
        assert!(tab.inventory.is_none());
        assert_eq!(tab.status_label(), "Discovery failed");

        let handle = RemoteSessionsShutdownHandle::pending_for_test();
        tab.attach_shutdown_handle_for_test(handle.clone());
        handle.request_shutdown();
        tab.inject_discovery_for_test(2, Ok(inventory("available", 2, 2)));
        assert!(tab.inventory.is_none());
        assert_eq!(tab.status_label(), "Cancelled");
    }

    #[test]
    fn selected_target_pins_exact_generation_and_requires_review() {
        let mut tab = RemoteSessionsTab::new();
        tab.set_generation_for_test(1);
        tab.inject_discovery_for_test(1, Ok(inventory("attached", 2, 2)));
        tab.dispatch(
            RemoteSessionsAction::SelectSession("existing".to_owned()),
            &egui::Context::default(),
        );

        assert!(tab
            .dispatch(
                RemoteSessionsAction::ConfirmTakeover,
                &egui::Context::default(),
            )
            .is_none());
        tab.dispatch(
            RemoteSessionsAction::BeginTakeoverReview,
            &egui::Context::default(),
        );
        assert!(tab
            .dispatch(
                RemoteSessionsAction::ConfirmTakeover,
                &egui::Context::default(),
            )
            .is_none());
        tab.set_takeover_password_for_test("attach-secret");
        let request = tab
            .dispatch(
                RemoteSessionsAction::ConfirmTakeover,
                &egui::Context::default(),
            )
            .unwrap();
        assert!(!tab.takeover_review);
        assert!(tab
            .dispatch(
                RemoteSessionsAction::ConfirmTakeover,
                &egui::Context::default(),
            )
            .is_none());
        assert_eq!(request.target.name(), "existing");
        assert_eq!(request.target.pid(), 77);
        assert_eq!(request.target.generation(), 987654321);
        assert_eq!(
            format!("{request:?}"),
            format!(
                "RemoteAttachRequest {{ target: {:?}, authentication: [REDACTED] }}",
                request.target
            )
        );
    }

    #[test]
    fn incompatible_or_unavailable_metadata_cannot_be_selected() {
        for (status, protocol, snapshot) in
            [("stale", 2, 2), ("available", 1, 2), ("available", 2, 1)]
        {
            let mut tab = RemoteSessionsTab::new();
            tab.set_generation_for_test(1);
            tab.inject_discovery_for_test(1, Ok(inventory(status, protocol, snapshot)));
            tab.dispatch(
                RemoteSessionsAction::SelectSession("existing".to_owned()),
                &egui::Context::default(),
            );
            assert!(
                tab.selected_target.is_none(),
                "{status} {protocol} {snapshot}"
            );
        }
    }

    #[test]
    fn endpoint_edits_invalidate_inventory_and_input_is_bounded() {
        let mut tab = RemoteSessionsTab::new();
        tab.set_generation_for_test(1);
        tab.inject_discovery_for_test(1, Ok(inventory("available", 2, 2)));
        assert!(tab.inventory.is_some());
        tab.invalidate_inventory("changed");
        assert!(tab.inventory.is_none());

        let mut long = "é".repeat(MAX_INPUT_BYTES);
        enforce_string_byte_limit(&mut long, MAX_INPUT_BYTES - 1);
        assert!(long.len() < MAX_INPUT_BYTES);
        assert!(long.is_char_boundary(long.len()));

        for index in 0..(MAX_MESSAGES + 5) {
            tab.push_message(format!("message {index}"));
        }
        assert_eq!(tab.message_count_for_test(), MAX_MESSAGES);
    }

    #[test]
    fn endpoint_requires_strict_pin_and_does_not_persist_password() {
        let mut tab = RemoteSessionsTab::new();
        tab.set_endpoint_for_test(EndpointDraft {
            host: "example.test".to_owned(),
            port: "22".to_owned(),
            username: "deploy".to_owned(),
            fingerprint: "not-a-pin".to_owned(),
            helper: "festerm-sessiond".to_owned(),
        });
        assert!(tab.build_endpoint().is_err());
        let auth = SshAuthentication::password("super-secret");
        assert_eq!(
            format!("{auth:?}"),
            "SshAuthentication::Password([REDACTED])"
        );
    }

    #[test]
    fn shutdown_lifecycle_requests_cancellation_until_finished() {
        let mut tab = RemoteSessionsTab::new();
        tab.set_generation_for_test(1);
        tab.inject_discovery_for_test(1, Ok(inventory("available", 2, 2)));
        tab.dispatch(
            RemoteSessionsAction::SelectSession("existing".to_owned()),
            &egui::Context::default(),
        );
        tab.dispatch(
            RemoteSessionsAction::BeginTakeoverReview,
            &egui::Context::default(),
        );
        tab.set_takeover_password_for_test("attach-secret");
        let handle = RemoteSessionsShutdownHandle::pending_for_test();
        tab.attach_shutdown_handle_for_test(handle.clone());
        assert!(tab.is_busy());
        tab.request_close();
        assert!(handle.cancel_requested_for_test());
        assert!(tab.discovery_password.is_empty());
        assert!(tab.takeover_password.is_empty());
        assert!(tab.inventory.is_none());
        assert!(tab.selected_target.is_none());
        assert!(!tab.takeover_review);
        assert!(tab
            .dispatch(
                RemoteSessionsAction::ConfirmTakeover,
                &egui::Context::default()
            )
            .is_none());
        assert!(!handle.is_finished());
        handle.finish_for_test();
        tab.drain_messages(&egui::Context::default());
        assert!(tab.shutdown_handle().is_none());
        assert!(handle.is_finished());
    }

    #[test]
    fn drain_retains_finished_empty_receiver_for_late_terminal_message() {
        let mut tab = RemoteSessionsTab::new();
        tab.set_generation_for_test(3);
        let (sender, receiver) = mpsc::sync_channel(WORKER_QUEUE);
        let handle = RemoteSessionsShutdownHandle::pending_for_test();
        handle.finish_for_test();
        tab.attach_receiver_for_test(receiver, handle.clone());

        tab.drain_messages(&egui::Context::default());
        assert!(
            tab.receiver.is_some(),
            "receiver must not be dropped just because finished became visible after Empty"
        );

        assert!(sender
            .try_send(WorkerMessage::DiscoveryFinished {
                generation: 3,
                result: Ok(inventory("available", 2, 2)),
            })
            .is_ok());
        tab.drain_messages(&egui::Context::default());
        assert!(tab.inventory.is_some());
        assert!(tab.receiver.is_none());
        assert!(tab.control.is_none());
    }

    #[test]
    fn terminal_message_keeps_shutdown_ownership_until_guard_finishes() {
        let mut tab = RemoteSessionsTab::new();
        tab.set_generation_for_test(4);
        let (sender, receiver) = mpsc::sync_channel(WORKER_QUEUE);
        let handle = RemoteSessionsShutdownHandle::pending_for_test();
        tab.attach_receiver_for_test(receiver, handle.clone());
        assert!(sender
            .try_send(WorkerMessage::DiscoveryFinished {
                generation: 4,
                result: Ok(inventory("available", 2, 2)),
            })
            .is_ok());

        tab.drain_messages(&egui::Context::default());
        assert!(tab.inventory.is_some());
        assert!(tab.receiver.is_none());
        assert!(tab.shutdown_handle().is_some());
        assert!(tab.is_busy());

        handle.finish_for_test();
        tab.drain_messages(&egui::Context::default());
        assert!(tab.shutdown_handle().is_none());
        assert!(!tab.is_busy());
    }

    #[test]
    fn finished_worker_without_terminal_message_reports_failure() {
        let mut tab = RemoteSessionsTab::new();
        let handle = RemoteSessionsShutdownHandle::pending_for_test();
        let (sender, receiver) = mpsc::sync_channel(WORKER_QUEUE);
        tab.attach_shutdown_handle_for_test(handle.clone());
        tab.receiver = Some(receiver);
        tab.status = "Discovering".to_owned();
        drop(sender);
        handle.finish_for_test();
        tab.drain_messages(&egui::Context::default());
        assert_eq!(tab.status_label(), "Discovery failed");
        assert!(tab
            .messages
            .back()
            .unwrap()
            .contains("without reporting an outcome"));
        assert!(tab.shutdown_handle().is_none());
    }

    #[test]
    fn drop_cancels_worker_when_close_was_not_explicit() {
        let handle = RemoteSessionsShutdownHandle::pending_for_test();
        {
            let mut tab = RemoteSessionsTab::new();
            tab.discovery_password = "discover-secret".to_owned();
            tab.takeover_password = "attach-secret".to_owned();
            tab.attach_shutdown_handle_for_test(handle.clone());
        }
        assert!(handle.cancel_requested_for_test());
    }
}
