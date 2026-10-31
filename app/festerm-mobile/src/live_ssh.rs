//! Opt-in transient SSH composition. The mobile app remains the terminal writer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use festerm_core::{Dimensions, Terminal};
use festerm_session::{
    HostKeyPrompt, PasswordPrompt, Session, SessionEvent, SessionEventNotifier, SessionLifecycle,
    SessionTryReceiveError, TerminalSize,
};
use festerm_ssh::{
    HostIdentity, HostKeyDecisionResolver, HostTrustDecision, PasswordDecisionResolver,
    SshAuthentication, SshConnectionProfile, SshSession, SshSessionOptions,
};
use zeroize::{Zeroize, Zeroizing};

const EVENT_BUDGET: usize = 32;

fn control_margin(ui: &egui::Ui) -> egui::Margin {
    let font = egui::FontSelection::Default.resolve(ui.style());
    let row_height = ui.fonts_mut(|fonts| fonts.row_height(&font));
    let line_height = row_height + ui.spacing().extra_text_line_spacing;
    let inset = ((44.0 - line_height) * 0.5).ceil().clamp(0.0, 127.0) as i8;
    egui::Margin::symmetric(4, inset)
}

#[derive(Default)]
struct PasswordBuffer(Zeroizing<String>);

impl PasswordBuffer {
    fn clear(&mut self) {
        self.0.zeroize();
    }

    fn take(&mut self) -> String {
        std::mem::take(&mut *self.0)
    }

    fn byte_index(&self, character: usize) -> usize {
        self.0
            .char_indices()
            .nth(character)
            .map_or(self.0.len(), |(index, _)| index)
    }
}

impl egui::TextBuffer for PasswordBuffer {
    fn is_mutable(&self) -> bool {
        true
    }
    fn as_str(&self) -> &str {
        &self.0
    }
    fn insert_text(&mut self, text: &str, character: egui::text::CharIndex) -> usize {
        let index = self.byte_index(character.0);
        let remaining = 4096_usize.saturating_sub(self.0.chars().count());
        let mut next = Zeroizing::new(String::with_capacity(self.0.len() + text.len().min(16384)));
        next.push_str(&self.0[..index]);
        let mut inserted = 0;
        for character in text.chars().take(remaining) {
            next.push(character);
            inserted += 1;
        }
        next.push_str(&self.0[index..]);
        self.0 = next; // Wipe the old allocation, including deleted/reallocated content.
        inserted
    }
    fn delete_char_range(&mut self, range: std::ops::Range<egui::text::CharIndex>) {
        let start = self.byte_index(range.start.0);
        let end = self.byte_index(range.end.0).max(start);
        let mut next = Zeroizing::new(String::with_capacity(self.0.len()));
        next.push_str(&self.0[..start]);
        next.push_str(&self.0[end..]);
        self.0 = next;
    }
    fn clear(&mut self) {
        PasswordBuffer::clear(self);
    }
    fn take(&mut self) -> String {
        PasswordBuffer::take(self)
    }
    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<Self>()
    }
}

fn session_size(dimensions: Dimensions) -> Result<TerminalSize, &'static str> {
    let columns = u16::try_from(dimensions.columns()).map_err(|_| "Terminal too wide")?;
    let rows = u16::try_from(dimensions.rows()).map_err(|_| "Terminal too tall")?;
    TerminalSize::new(columns, rows).map_err(|_| "Invalid terminal dimensions")
}

struct Repaint(egui::Context);

impl SessionEventNotifier for Repaint {
    fn notify(&self) {
        self.0.request_repaint();
    }
}

pub(super) struct LiveSsh {
    identity: HostIdentity,
    context: egui::Context,
    session: Option<Arc<dyn Session>>,
    host_resolver: Option<HostKeyDecisionResolver>,
    password_resolver: Option<PasswordDecisionResolver>,
    host_prompt: Option<HostKeyPrompt>,
    password_prompt: Option<PasswordPrompt>,
    password: PasswordBuffer,
    pending_resize: Option<(Dimensions, Instant)>,
    running: bool,
    pub(super) status: &'static str,
    pub(super) accepted_pin: Option<String>,
}

#[cfg(test)]
pub(super) fn disconnected_fixture() -> LiveSsh {
    LiveSsh {
        identity: HostIdentity::new("example.test", 22).expect("reserved fixture host"),
        context: egui::Context::default(),
        session: None,
        host_resolver: None,
        password_resolver: None,
        host_prompt: None,
        password_prompt: None,
        password: PasswordBuffer::default(),
        pending_resize: None,
        running: false,
        status: "Connection lost. Connect again for a fresh shell.",
        accepted_pin: None,
    }
}

impl LiveSsh {
    pub(super) fn connect(
        host: &str,
        port: u16,
        username: &str,
        dimensions: Dimensions,
        pin: Option<&str>,
        context: &egui::Context,
    ) -> Result<Self, &'static str> {
        let identity = HostIdentity::new(host, port).map_err(|_| "Invalid SSH host or port")?;
        let size = session_size(dimensions)?;
        let profile = SshConnectionProfile::new(
            identity.clone(),
            username,
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            size,
        )
        .map_err(|_| "Invalid SSH username")?;
        let mut options = SshSessionOptions::new();
        if let Some(pin) = pin {
            options = options.with_known_host_fingerprint(pin);
        }
        let session = SshSession::start_with_notifier_and_options(
            profile,
            SshAuthentication::Interactive,
            options,
            Arc::new(Repaint(context.clone())),
        )
        .map_err(|_| "Could not start SSH worker")?;
        let host_resolver = Some(session.host_key_decision_resolver());
        let password_resolver = Some(session.password_decision_resolver());
        Ok(Self {
            identity,
            context: context.clone(),
            session: Some(Arc::new(session)),
            host_resolver,
            password_resolver,
            host_prompt: None,
            password_prompt: None,
            password: PasswordBuffer::default(),
            pending_resize: None,
            running: false,
            status: "Connecting",
            accepted_pin: pin.map(str::to_owned),
        })
    }

    pub(super) fn host_label(&self) -> &str {
        self.identity.host()
    }

    pub(super) fn input_enabled(&self) -> bool {
        self.running && self.host_prompt.is_none() && self.password_prompt.is_none()
    }

    pub(super) fn can_reset(&self) -> bool {
        self.session.is_none()
    }

    pub(super) fn stop(&mut self, status: &'static str) {
        self.running = false;
        self.status = status;
        self.password.clear();
        self.host_prompt = None;
        self.password_prompt = None;
        self.pending_resize = None;
        self.host_resolver = None;
        self.password_resolver = None;
        if let Some(session) = self.session.take() {
            let _ = session.try_shutdown();
        }
    }

    pub(super) fn send(&mut self, bytes: &[u8]) {
        if !self.input_enabled() || bytes.is_empty() {
            return;
        }
        if let Some(session) = &self.session {
            if session.try_send_input(bytes).is_err() {
                // Never drop part of a command and then accept its continuation.
                // The backend admission is atomic; make refusal explicit and
                // retire this session rather than replay input on a new shell.
                self.stop(
                    "Input rejected; disconnected. Earlier accepted input may have executed.",
                );
            }
        }
    }

    pub(super) fn resize(&mut self, dimensions: Dimensions) {
        if self.pending_resize.map(|(size, _)| size) != Some(dimensions) {
            self.pending_resize = Some((dimensions, Instant::now()));
            if self.running {
                // Geometry is reported after this frame's event pump. Schedule
                // the debounce even if no output or further input wakes the UI.
                self.context
                    .request_repaint_after(Duration::from_millis(100));
            }
        }
    }

    pub(super) fn poll(&mut self, terminal: &mut Terminal, context: &egui::Context) {
        let Some(session) = self.session.clone() else {
            return;
        };
        for _ in 0..EVENT_BUDGET {
            match session.try_recv_event() {
                Ok(SessionEvent::Output(bytes)) => {
                    terminal.ingest(&bytes);
                    let replies = terminal.drain_replies();
                    if !replies.is_empty() && session.try_send_input(&replies).is_err() {
                        self.stop("Terminal reply rejected; disconnected");
                        return;
                    }
                }
                Ok(SessionEvent::HostKeyVerification(prompt)) => {
                    self.status = "Review SSH host key";
                    self.host_prompt = Some(prompt);
                }
                Ok(SessionEvent::PasswordRequested(prompt)) => {
                    self.status = "SSH password required";
                    self.password.clear();
                    self.password_prompt = Some(prompt);
                }
                Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) => {
                    self.running = true;
                    self.status = "Connected";
                }
                Ok(SessionEvent::Lifecycle(SessionLifecycle::Starting)) => {
                    self.running = false;
                    self.status = "Connecting";
                }
                Ok(SessionEvent::Lifecycle(_)) | Ok(SessionEvent::Error(_)) => {
                    self.stop("Session ended or connection lost. Connect again for a fresh shell.");
                    return;
                }
                Ok(SessionEvent::RecoverySync(_)) => {
                    self.stop("Unsupported recovery event; disconnected");
                    return;
                }
                Ok(_) => {}
                Err(SessionTryReceiveError::Empty) => break,
                Err(SessionTryReceiveError::Closed) => {
                    self.stop("SSH worker closed");
                    return;
                }
            }
        }
        if session.metrics().event_queue_depth > 0 {
            context.request_repaint();
        }
        if let Some((dimensions, changed)) = self.pending_resize {
            if self.running {
                if changed.elapsed() >= Duration::from_millis(100) {
                    let Ok(size) = session_size(dimensions) else {
                        self.stop("Terminal size cannot be represented by SSH; disconnected");
                        return;
                    };
                    if session.try_resize(size).is_ok() {
                        self.pending_resize = None;
                    }
                }
                // One coalesced pending size; no unbounded resize queue or busy loop.
                if self.pending_resize.is_some() {
                    context.request_repaint_after(Duration::from_millis(100));
                }
            }
        }
    }

    pub(super) fn prompts(&mut self, ui: &mut egui::Ui) {
        if let Some(prompt) = self.host_prompt.clone() {
            ui.label(format!("{}:{}", prompt.host(), prompt.port()));
            ui.monospace(prompt.sha256_fingerprint());
            if prompt.is_key_change() {
                ui.colored_label(
                    egui::Color32::RED,
                    "Host key changed. Verify independently; this attempt must be rejected.",
                );
            }
            ui.horizontal(|ui| {
                if !prompt.is_key_change() && ui.button("Trust for this app session").clicked() {
                    let resolved = self.host_resolver.as_ref().is_some_and(|resolver| {
                        resolver
                            .resolve(&prompt, HostTrustDecision::AcceptOnce)
                            .is_ok()
                    });
                    if resolved {
                        self.accepted_pin = Some(prompt.sha256_fingerprint().to_owned());
                        self.host_prompt = None;
                        self.status = "Authenticating";
                    } else {
                        self.stop("Host-key request expired; connect again");
                    }
                }
                if ui.button("Reject").clicked() {
                    self.stop("Host key rejected");
                }
            });
        }
        if let Some(prompt) = self.password_prompt.clone() {
            ui.label(format!(
                "Password for {}@{}",
                prompt.username(),
                prompt.host()
            ));
            if prompt.previous_attempt_failed() {
                ui.label("Authentication failed. Try again or disconnect.");
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.password)
                    .password(true)
                    .sensitive(true)
                    .margin(control_margin(ui))
                    .char_limit(4096),
            );
            if ui.button("Authenticate").clicked() {
                let mut password = Zeroizing::new(self.password.take());
                let resolved = self.password_resolver.as_ref().is_some_and(|resolver| {
                    resolver
                        .resolve(&prompt, std::mem::take(&mut *password))
                        .is_ok()
                });
                if resolved {
                    self.password_prompt = None;
                    self.status = "Authenticating";
                } else {
                    self.stop("Password request expired; connect again");
                }
            }
        }
    }
}

impl Drop for LiveSsh {
    fn drop(&mut self) {
        self.stop("Disconnected");
    }
}

#[derive(Default)]
pub(super) struct ConnectionForm {
    pub(super) open: bool,
    host: String,
    username: String,
    port: String,
    error: Option<&'static str>,
    pin: Option<(String, u16, String)>,
    #[cfg(test)]
    pub(super) controls: Option<ConnectionControlsGeometry>,
}

#[cfg(test)]
#[derive(Clone, Copy)]
pub(super) struct ConnectionControlsGeometry {
    pub(super) bounds: egui::Rect,
    pub(super) connect: egui::Rect,
    pub(super) clip: egui::Rect,
    pub(super) field_height: f32,
}

impl ConnectionForm {
    pub(super) fn toggle(&mut self) {
        self.open = !self.open;
        if self.port.is_empty() {
            self.port = "22".to_owned();
        }
    }

    pub(super) fn show_recovery(
        &mut self,
        ui: &mut egui::Ui,
        live: &LiveSsh,
        available_height: f32,
    ) {
        let compact = available_height < 260.0;
        let height = available_height * 0.45;
        egui::ScrollArea::vertical()
            .id_salt("mobile-recovery-content")
            .max_height(height)
            .show(ui, |ui| {
                egui::Frame::new()
                    .fill(ui.visuals().window_fill)
                    .stroke(ui.visuals().window_stroke)
                    .corner_radius(8)
                    .inner_margin(if compact { 6 } else { 12 })
                    .show(ui, |ui| {
                        if !compact {
                            ui.strong("Connection ended");
                            ui.label("Reconnect starts a new shell.");
                        }
                        if ui
                            .add_sized(
                                [ui.available_width(), 44.0],
                                egui::Button::new("Reconnect · new shell")
                                    .fill(egui::Color32::from_rgb(25, 120, 238)),
                            )
                            .clicked()
                        {
                            // Review the existing form first; no worker starts here.
                            self.toggle();
                        }
                        if live.status.starts_with("Input rejected") {
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 193, 112),
                                "Earlier accepted input may have executed.",
                            );
                        }
                        egui::CollapsingHeader::new("Details")
                            .id_salt("mobile-recovery-details")
                            .show(ui, |ui| {
                                ui.label(live.status);
                            });
                    });
            });
    }

    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        live: &mut Option<LiveSsh>,
        dimensions: Dimensions,
    ) -> bool {
        #[cfg(test)]
        {
            self.controls = None;
        }
        if let Some(connection) = live.as_mut() {
            if !connection.can_reset() {
                ui.label(connection.status);
            }
            if let Some(pin) = &connection.accepted_pin {
                self.pin = Some((
                    connection.identity.host().to_owned(),
                    connection.identity.port(),
                    pin.clone(),
                ));
            }
            connection.prompts(ui);
            if !connection.can_reset() && ui.button("Disconnect").clicked() {
                connection.stop("Disconnected. Connect again for a fresh shell.");
            }
        }
        if self.open {
            if live
                .as_ref()
                .is_some_and(|connection| !connection.can_reset())
            {
                ui.small("Connect starts a new shell.");
            }
            let margin = control_margin(ui);
            #[cfg(test)]
            let controls_start = ui.cursor().min;
            let width = ui.available_width();
            let port_width = 72.0_f32.min(width * 0.3);
            let host_width = (width - port_width - ui.spacing().item_spacing.x).max(1.0);
            let field_height = ui
                .horizontal(|ui| {
                    let host = ui.vertical(|ui| {
                        ui.set_width(host_width);
                        ui.weak("Host");
                        ui.add_sized(
                            [host_width, 44.0],
                            egui::TextEdit::singleline(&mut self.host)
                                .margin(margin)
                                .hint_text("example.net")
                                .char_limit(253),
                        )
                    });
                    ui.vertical(|ui| {
                        ui.set_width(port_width);
                        ui.weak("Port");
                        ui.add_sized(
                            [port_width, 44.0],
                            egui::TextEdit::singleline(&mut self.port)
                                .margin(margin)
                                .hint_text("22")
                                .char_limit(5),
                        );
                    });
                    host.inner.rect.height()
                })
                .inner;
            #[cfg(not(test))]
            let _ = field_height;
            ui.weak("Username");
            ui.add_sized(
                [width, 44.0],
                egui::TextEdit::singleline(&mut self.username)
                    .margin(margin)
                    .hint_text("Username")
                    .char_limit(255),
            );
            let connect = ui.add_sized(
                [width, 44.0],
                egui::Button::new("Connect").fill(egui::Color32::from_rgb(25, 120, 238)),
            );
            #[cfg(test)]
            {
                self.controls = Some(ConnectionControlsGeometry {
                    bounds: egui::Rect::from_min_max(controls_start, connect.rect.max),
                    connect: connect.rect,
                    clip: ui.clip_rect(),
                    field_height,
                });
            }
            if connect.clicked() {
                let port = self.port.parse::<u16>().unwrap_or(0);
                let identity = HostIdentity::new(&self.host, port).ok();
                let pin = self
                    .pin
                    .as_ref()
                    .filter(|(host, known_port, _)| {
                        identity.as_ref().is_some_and(|identity| {
                            identity.host() == host && *known_port == identity.port()
                        })
                    })
                    .map(|(_, _, pin)| pin.as_str());
                match LiveSsh::connect(&self.host, port, &self.username, dimensions, pin, ui.ctx())
                {
                    Ok(connection) => {
                        *live = Some(connection);
                        self.error = None;
                        self.open = false;
                        return true;
                    }
                    Err(error) => self.error = Some(error),
                }
            }
        }
        if let Some(error) = self.error {
            ui.label(error);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_session::{
        SessionId, SessionMetrics, SessionOperation, SessionSendError, ShutdownError,
        ShutdownResult,
    };
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSession {
        events: Mutex<VecDeque<SessionEvent>>,
        input: Mutex<Vec<Vec<u8>>>,
        resizes: Mutex<Vec<TerminalSize>>,
        rejected: bool,
        shutdowns: Mutex<usize>,
    }

    impl Session for FakeSession {
        fn id(&self) -> SessionId {
            SessionId::next()
        }
        fn lifecycle(&self) -> SessionLifecycle {
            SessionLifecycle::Running
        }
        fn metrics(&self) -> SessionMetrics {
            SessionMetrics {
                event_queue_depth: self.events.lock().unwrap().len(),
                ..Default::default()
            }
        }
        fn try_send_input(&self, bytes: &[u8]) -> Result<(), SessionSendError> {
            if self.rejected {
                return Err(SessionSendError::Full {
                    operation: SessionOperation::Input,
                    capacity: 1,
                });
            }
            self.input.lock().unwrap().push(bytes.to_vec());
            Ok(())
        }
        fn try_resize(&self, size: TerminalSize) -> Result<(), SessionSendError> {
            self.resizes.lock().unwrap().push(size);
            Ok(())
        }
        fn try_shutdown(&self) -> Result<(), SessionSendError> {
            *self.shutdowns.lock().unwrap() += 1;
            Ok(())
        }
        fn try_recv_event(&self) -> Result<SessionEvent, SessionTryReceiveError> {
            self.events
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(SessionTryReceiveError::Empty)
        }
        fn shutdown(&self, _timeout: Duration) -> Result<ShutdownResult, ShutdownError> {
            panic!("UI must never synchronously wait for shutdown")
        }
    }

    fn connection(session: Arc<FakeSession>) -> LiveSsh {
        LiveSsh {
            identity: HostIdentity::new("example.test", 22).unwrap(),
            context: egui::Context::default(),
            session: Some(session),
            host_resolver: None,
            password_resolver: None,
            host_prompt: None,
            password_prompt: None,
            password: PasswordBuffer::default(),
            pending_resize: None,
            running: true,
            status: "Connected",
            accepted_pin: None,
        }
    }

    fn terminal() -> Terminal {
        Terminal::with_scrollback_limit(Dimensions::new(80, 24).unwrap(), 4096).unwrap()
    }

    #[test]
    fn mobile_disconnected_history_scrolls_and_copies_without_transport_input() {
        use crate::{Lifecycle, MobileApp};
        use std::{cell::Cell, rc::Rc};

        let session = Arc::new(FakeSession::default());
        let mut live = connection(session.clone());
        live.stop("Connection lost. Connect again for a fresh shell.");
        let mut app = MobileApp::new(Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        })));
        app.control = true;
        app.alt = true;
        app.terminal.ingest(b"\x1b[?1003h\x1b[?1006h\x1b[?1004h");
        *app.sink.live.connection.borrow_mut() = Some(live);
        let context = egui::Context::default();
        let render = |app: &mut MobileApp, events| {
            context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(390.0, 844.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| app.show_with_occlusion(ui, 0.0),
            )
        };
        for _ in 0..4 {
            let output = render(&mut app, vec![]);
            assert!(output.platform_output.ime.is_none());
            output.drop_without_applying_deltas();
        }
        assert!(!app.control && !app.alt);
        assert!(app.toolbar_rect.is_none());
        assert!(app.gestures.helper().is_none());
        let grid = app.view.diagnostics().grid_rect.unwrap();
        render(
            &mut app,
            vec![
                egui::Event::WindowFocused(true),
                egui::Event::Text("must not run".into()),
                egui::Event::Paste("must not paste\n".into()),
                egui::Event::PointerMoved(grid.center()),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, 6.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
        .drop_without_applying_deltas();
        assert!(app.view.history_offset_rows() > 0);
        let grid = app.view.diagnostics().grid_rect.unwrap();
        let dimensions = app.terminal.dimensions();
        let cell = egui::vec2(
            grid.width() / dimensions.columns() as f32,
            grid.height() / dimensions.rows() as f32,
        );
        let start = grid.min + egui::vec2(cell.x * 0.2, cell.y * 0.5);
        let end = start + egui::vec2(cell.x * 2.0, 0.0);
        for (point, pressed) in [(start, true), (end, false)] {
            render(
                &mut app,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            )
            .drop_without_applying_deltas();
        }
        let selected = app
            .view
            .selected_text(&app.terminal)
            .expect("local selection");
        assert!(!selected.is_empty());
        let output = render(&mut app, vec![egui::Event::Copy]);
        assert!(output.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyText(text) if text == &selected)
        }));
        assert!(output.platform_output.ime.is_none());
        output.drop_without_applying_deltas();
        assert!(session.input.lock().unwrap().is_empty());
        assert_eq!(app.sink.diagnostics.byte_count, 0);
        assert_eq!(*session.shutdowns.lock().unwrap(), 1);
        assert_eq!(
            app.terminal.modes().mouse_tracking(),
            festerm_core::MouseTrackingMode::AnyMotion
        );

        app.connection.toggle();
        for _ in 0..4 {
            render(&mut app, vec![]).drop_without_applying_deltas();
        }
        let offset = app.view.history_offset_rows();
        render(
            &mut app,
            vec![
                egui::Event::PointerMoved(grid.center()),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, 6.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
        .drop_without_applying_deltas();
        assert_eq!(app.view.history_offset_rows(), offset);
        assert_eq!(app.sink.diagnostics.byte_count, 0);
    }

    #[test]
    fn mobile_reconnect_reviews_form_without_replay_or_early_history_clear() {
        use crate::{Lifecycle, MobileApp};
        use egui_kittest::{kittest::Queryable, Harness};
        use std::{cell::Cell, rc::Rc};

        let mut app = MobileApp::new(Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        })));
        app.terminal.ingest(b"retained-evidence");
        *app.sink.live.connection.borrow_mut() = Some(disconnected_fixture());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), app);
        let retained = |app: &MobileApp| {
            (0..app.terminal.dimensions().rows())
                .filter_map(|row| app.terminal.row_text(row))
                .collect::<Vec<_>>()
                .join("\n")
                .contains("retained-evidence")
        };
        harness.run();
        harness.get_by_label("Reconnect · new shell").click();
        harness.run();
        assert!(harness.state().connection.open);
        assert!(retained(harness.state()));
        assert!(harness
            .state()
            .sink
            .live
            .connection
            .borrow()
            .as_ref()
            .unwrap()
            .can_reset());
        // An empty host fails validation synchronously; it cannot replace history.
        harness.get_by_label("Connect").click();
        harness.run();
        assert!(retained(harness.state()));
        harness.get_by_label("Cancel").click();
        harness.run();
        assert!(!harness.state().connection.open);
        assert!(retained(harness.state()));
        assert_eq!(harness.state().sink.diagnostics.byte_count, 0);
        assert!(harness.query_by_label("Reconnect · new shell").is_some());
    }

    #[test]
    fn mobile_live_ssh_pump_uses_core_replies_and_bounded_event_work() {
        let session = Arc::new(FakeSession::default());
        session
            .events
            .lock()
            .unwrap()
            .push_back(SessionEvent::Output(b"hello\x1b[6n".to_vec()));
        let mut live = connection(session.clone());
        live.poll(&mut terminal(), &egui::Context::default());
        assert_eq!(*session.input.lock().unwrap(), vec![b"\x1b[1;6R".to_vec()]);
        for _ in 0..EVENT_BUDGET + 1 {
            session
                .events
                .lock()
                .unwrap()
                .push_back(SessionEvent::Lifecycle(SessionLifecycle::Running));
        }
        live.poll(&mut terminal(), &egui::Context::default());
        assert_eq!(session.events.lock().unwrap().len(), 1);
    }

    #[test]
    fn mobile_live_ssh_hidden_workspaces_keep_pumping_output_and_replies() {
        use crate::{workspaces::Workspace, Lifecycle, MobileApp};
        use std::{cell::Cell, rc::Rc};

        for workspace in [Workspace::Files, Workspace::Markdown] {
            let session = Arc::new(FakeSession::default());
            session
                .events
                .lock()
                .unwrap()
                .push_back(SessionEvent::Output(b"hidden\x1b[6n".to_vec()));
            for _ in 0..EVENT_BUDGET {
                session
                    .events
                    .lock()
                    .unwrap()
                    .push_back(SessionEvent::Lifecycle(SessionLifecycle::Running));
            }
            let mut app = MobileApp::new(Rc::new(Cell::new(Lifecycle {
                active: true,
                ..Default::default()
            })));
            app.terminal = terminal();
            *app.sink.live.connection.borrow_mut() = Some(connection(session.clone()));
            app.switch_workspace(workspace);
            let context = egui::Context::default();
            context
                .run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(390.0, 844.0),
                        )),
                        ..Default::default()
                    },
                    |ui| app.show(ui),
                )
                .drop_without_applying_deltas();
            assert_eq!(app.workspace, workspace);
            assert_eq!(*session.input.lock().unwrap(), vec![b"\x1b[1;7R".to_vec()]);
            assert_eq!(session.events.lock().unwrap().len(), 1);
            context
                .run_ui(egui::RawInput::default(), |ui| app.show(ui))
                .drop_without_applying_deltas();
            assert!(session.events.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn mobile_live_ssh_native_suspension_retires_without_a_ui_frame() {
        use crate::{Lifecycle, MobileApp, SuspensionHandle};
        use std::{cell::Cell, rc::Rc};

        let suspension = SuspensionHandle::default();
        let mut app = MobileApp::new(Rc::new(Cell::new(Lifecycle::default())))
            .with_suspension(suspension.clone());
        let session = Arc::new(FakeSession::default());
        for shutdowns in 1..=2 {
            app.sink.reset();
            let mut live = connection(session.clone());
            live.password.0.push_str("synthetic-secret");
            live.host_prompt = Some(HostKeyPrompt::new("example.test", 22, "SHA256:fixture"));
            live.resize(Dimensions::new(90, 30).unwrap());
            *app.sink.live.connection.borrow_mut() = Some(live);

            suspension.suspend();
            suspension.suspend();

            let connection = app.sink.live.connection.borrow();
            let live = connection.as_ref().unwrap();
            assert!(live.session.is_none());
            assert!(!live.input_enabled());
            assert!(live.password.0.is_empty());
            assert!(live.host_prompt.is_none());
            assert!(live.password_prompt.is_none());
            assert!(live.pending_resize.is_none());
            assert!(live.status.starts_with("Suspended"));
            assert_eq!(*session.shutdowns.lock().unwrap(), shutdowns);
        }
    }

    #[test]
    fn mobile_live_ssh_input_refusal_retires_session_without_replay() {
        let session = Arc::new(FakeSession {
            rejected: true,
            ..Default::default()
        });
        let mut live = connection(session.clone());
        live.send(b"command");
        assert!(live.session.is_none());
        assert!(!live.input_enabled());
        assert!(live.status.starts_with("Input rejected"));
        live.send(b"continuation\n");
        assert!(session.input.lock().unwrap().is_empty());
        assert_eq!(*session.shutdowns.lock().unwrap(), 1);
    }

    #[test]
    fn mobile_live_ssh_stop_clears_prompts_password_resize_and_stale_output() {
        let session = Arc::new(FakeSession::default());
        let mut live = connection(session.clone());
        live.password.0.push_str("synthetic-secret");
        live.host_prompt = Some(HostKeyPrompt::new("example.test", 22, "SHA256:fixture"));
        live.resize(Dimensions::new(90, 30).unwrap());
        live.send(b"blocked by trust prompt");
        assert!(session.input.lock().unwrap().is_empty());
        live.stop("Suspended");
        assert!(live.password.0.is_empty());
        assert!(live.host_prompt.is_none());
        assert!(live.pending_resize.is_none());
        session
            .events
            .lock()
            .unwrap()
            .push_back(SessionEvent::Output(b"stale\x1b[6n".to_vec()));
        live.poll(&mut terminal(), &egui::Context::default());
        assert_eq!(session.events.lock().unwrap().len(), 1);
        assert!(session.input.lock().unwrap().is_empty());
    }

    #[test]
    fn mobile_live_ssh_resize_coalesces_and_rejects_unrepresentable_grids() {
        let session = Arc::new(FakeSession::default());
        let mut live = connection(session.clone());
        live.resize(Dimensions::new(90, 30).unwrap());
        live.resize(Dimensions::new(100, 40).unwrap());
        live.pending_resize.as_mut().unwrap().1 = Instant::now() - Duration::from_millis(200);
        live.poll(&mut terminal(), &egui::Context::default());
        assert_eq!(
            *session.resizes.lock().unwrap(),
            vec![TerminalSize::new(100, 40).unwrap()]
        );
        assert!(session_size(Dimensions::new(65536, 1).unwrap()).is_err());
    }

    #[test]
    fn mobile_live_ssh_password_buffer_bounds_unicode_edits_and_take() {
        use egui::text::CharIndex;
        use egui::TextBuffer;
        let mut password = PasswordBuffer::default();
        assert_eq!(password.insert_text("a中é", CharIndex(0)), 3);
        password.delete_char_range(CharIndex(1)..CharIndex(2));
        assert_eq!(password.as_str(), "aé");
        assert_eq!(password.insert_text("x", CharIndex(1)), 1);
        assert_eq!(password.as_str(), "axé");
        let value = Zeroizing::new(password.take());
        assert_eq!(value.as_str(), "axé");
        assert!(password.as_str().is_empty());
        assert_eq!(password.insert_text(&"x".repeat(5000), CharIndex(0)), 4096);
        password.clear();
        assert!(password.as_str().is_empty());
    }
}
