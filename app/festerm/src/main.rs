mod diagnostics;

use std::{collections::VecDeque, sync::Arc, time::Duration};

use festerm_core::{Dimensions, Terminal};
use festerm_pty::LocalPtySession;
use festerm_session::{
    FlowDirection, Session, SessionError, SessionEvent, SessionLifecycle, SessionSendError,
    SessionTryReceiveError, TerminalSize, DEFAULT_COMMAND_QUEUE_CAPACITY, MAX_IO_CHUNK_BYTES,
};
use festerm_ui_egui::{EncodedInputSink, InputRoute, InputSinkDiagnostics, TerminalView};

#[cfg(all(test, unix))]
use festerm_pty::LocalProfile;
#[cfg(test)]
use festerm_session::SessionMetrics;

const MAX_SESSION_EVENTS_PER_FRAME: usize = 64;
const SESSION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PENDING_COMMAND_BYTES: usize = DEFAULT_COMMAND_QUEUE_CAPACITY * MAX_IO_CHUNK_BYTES;
const APPLICATION_TITLE: &str = "fesTerm";

fn main() -> eframe::Result<()> {
    diagnostics::init();
    tracing::info!(target: "festerm::app", "starting fesTerm");

    let options = eframe::NativeOptions::default();
    eframe::run_native(
        APPLICATION_TITLE,
        options,
        Box::new(|creation_context| Ok(Box::new(FesTermApp::new(&creation_context.egui_ctx)))),
    )
}

struct FesTermApp {
    terminal: Terminal,
    terminal_view: TerminalView,
    local_session: LocalSessionSink,
    window_title: String,
}

impl FesTermApp {
    fn new(context: &eframe::egui::Context) -> Self {
        let mut terminal =
            Terminal::new(Dimensions::new(80, 24).expect("default dimensions are valid"))
                .expect("default terminal allocation should succeed");
        let notifier: Arc<dyn festerm_session::SessionEventNotifier> =
            Arc::new(EguiRepaintNotifier(context.clone()));
        let local_session = LocalSessionSink::start(terminal.dimensions(), notifier);
        if let Some(error) = local_session.start_error() {
            seed_session_failure(&mut terminal, error);
        }
        Self {
            terminal,
            terminal_view: TerminalView::default(),
            local_session,
            window_title: APPLICATION_TITLE.to_owned(),
        }
    }

    fn update_window_title(&mut self, context: &eframe::egui::Context) {
        let title = Self::window_title(self.terminal.title());
        if self.window_title != title {
            context.send_viewport_cmd(eframe::egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    fn window_title(terminal_title: &str) -> String {
        match terminal_title {
            "" => APPLICATION_TITLE.to_owned(),
            terminal_title => format!("{terminal_title} - {APPLICATION_TITLE}"),
        }
    }
}

/// Uses egui's thread-safe wake mechanism instead of polling for PTY output.
struct EguiRepaintNotifier(eframe::egui::Context);

impl festerm_session::SessionEventNotifier for EguiRepaintNotifier {
    fn notify(&self) {
        self.0.request_repaint();
    }
}

impl eframe::App for FesTermApp {
    fn logic(&mut self, context: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        if self.local_session.pump_events(&mut self.terminal) {
            context.request_repaint();
        }
        self.local_session
            .forward_terminal_replies(&mut self.terminal);
        self.local_session.flush_pending_writes();
        self.local_session.flush_pending_resize();
        self.update_window_title(context);
    }

    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        let session_status = self.local_session.status_line();
        let session_diagnostics = self.local_session.diagnostics_line();
        self.terminal_view.show_with_status(
            ui,
            &mut self.terminal,
            &mut self.local_session,
            &session_status,
            &session_diagnostics,
        );
        self.local_session
            .forward_terminal_replies(&mut self.terminal);
        self.local_session.flush_pending_writes();
        self.local_session.flush_pending_resize();
        if self.local_session.pump_events(&mut self.terminal) {
            ui.ctx().request_repaint();
        }
    }
}

impl Drop for FesTermApp {
    fn drop(&mut self) {
        if let Some(session) = &self.local_session.session {
            match session.shutdown(SESSION_SHUTDOWN_TIMEOUT) {
                Ok(result) => tracing::info!(
                    target: "festerm::session",
                    session = %session.id(),
                    ?result,
                    "local session shut down"
                ),
                Err(error) => tracing::error!(
                    target: "festerm::session",
                    session = %session.id(),
                    %error,
                    "local session did not finish bounded shutdown"
                ),
            }
        }
    }
}

struct PendingCommandBuffer {
    writes: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    capacity: usize,
}

impl PendingCommandBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            writes: VecDeque::new(),
            queued_bytes: 0,
            capacity,
        }
    }

    fn enqueue(&mut self, bytes: &[u8]) -> Result<(), PendingCommandError> {
        if bytes.len() > MAX_IO_CHUNK_BYTES {
            return Err(PendingCommandError::TooLarge {
                maximum: MAX_IO_CHUNK_BYTES,
                actual: bytes.len(),
            });
        }
        let queued_bytes = self
            .queued_bytes
            .checked_add(bytes.len())
            .filter(|queued_bytes| *queued_bytes <= self.capacity)
            .ok_or(PendingCommandError::Full {
                capacity: self.capacity,
                queued: self.queued_bytes,
                attempted: bytes.len(),
            })?;
        if !bytes.is_empty() {
            self.writes.push_back(bytes.to_vec());
            self.queued_bytes = queued_bytes;
        }
        Ok(())
    }

    fn flush(&mut self, session: &impl Session) -> PendingFlush {
        while let Some(bytes) = self.writes.front() {
            match session.try_send_input(bytes) {
                Ok(()) => {
                    let bytes = self
                        .writes
                        .pop_front()
                        .expect("front element remains until a successful send");
                    self.queued_bytes = self.queued_bytes.saturating_sub(bytes.len());
                }
                Err(SessionSendError::Full { .. }) => return PendingFlush::Backpressured,
                Err(error) => {
                    let dropped_bytes = self.queued_bytes;
                    self.writes.clear();
                    self.queued_bytes = 0;
                    return PendingFlush::Unrecoverable {
                        error,
                        dropped_bytes,
                    };
                }
            }
        }
        PendingFlush::Drained
    }

    fn queued_bytes(&self) -> usize {
        self.queued_bytes
    }

    fn queued_writes(&self) -> usize {
        self.writes.len()
    }

    fn capacity(&self) -> usize {
        self.capacity
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PendingCommandError {
    Full {
        capacity: usize,
        queued: usize,
        attempted: usize,
    },
    TooLarge {
        maximum: usize,
        actual: usize,
    },
}

impl std::fmt::Display for PendingCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full {
                capacity,
                queued,
                attempted,
            } => write!(
                formatter,
                "pending local-session writes exceeded their {capacity}-byte bound \
                 ({queued} queued, {attempted} additional)"
            ),
            Self::TooLarge { maximum, actual } => write!(
                formatter,
                "local-session write is {actual} bytes and exceeds the {maximum}-byte limit"
            ),
        }
    }
}

enum PendingFlush {
    Drained,
    Backpressured,
    Unrecoverable {
        error: SessionSendError,
        dropped_bytes: usize,
    },
}

/// Application-owned bridge between UI-encoded bytes and the active local session.
struct LocalSessionSink {
    session: Option<LocalPtySession>,
    startup_error: Option<String>,
    diagnostics: InputSinkDiagnostics,
    pending_writes: PendingCommandBuffer,
    pending_resize: Option<TerminalSize>,
    last_lifecycle: Option<SessionLifecycle>,
    last_error: Option<String>,
    last_backpressure: Option<FlowDirection>,
    last_resize: Option<TerminalSize>,
}

impl LocalSessionSink {
    fn start(
        dimensions: Dimensions,
        event_notifier: Arc<dyn festerm_session::SessionEventNotifier>,
    ) -> Self {
        let mut sink = Self {
            session: None,
            startup_error: None,
            diagnostics: InputSinkDiagnostics::default(),
            pending_writes: PendingCommandBuffer::new(MAX_PENDING_COMMAND_BYTES),
            pending_resize: None,
            last_lifecycle: Some(SessionLifecycle::Starting),
            last_error: None,
            last_backpressure: None,
            last_resize: None,
        };
        match terminal_size(dimensions).and_then(|size| {
            LocalPtySession::start_default_with_notifier(size, event_notifier)
                .map_err(|error| error.to_string())
        }) {
            Ok(session) => {
                tracing::info!(
                    target: "festerm::session",
                    session = %session.id(),
                    "started default local shell session"
                );
                sink.last_lifecycle = Some(session.lifecycle());
                sink.session = Some(session);
            }
            Err(error) => {
                tracing::error!(
                    target: "festerm::session",
                    %error,
                    "could not start default local shell"
                );
                sink.startup_error = Some(error);
            }
        }
        sink
    }

    fn start_error(&self) -> Option<&str> {
        self.startup_error.as_deref()
    }

    /// Returns whether another frame is required to continue a bounded drain.
    fn pump_events(&mut self, terminal: &mut Terminal) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        let mut observed = Vec::new();
        let result =
            pump_session_events(session, terminal, MAX_SESSION_EVENTS_PER_FRAME, |event| {
                observed.push(event.clone());
            });
        for event in observed {
            self.observe_session_event(event);
        }
        if result.hit_limit {
            self.last_backpressure = Some(FlowDirection::Output);
        }
        result.hit_limit
    }

    fn observe_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::Lifecycle(lifecycle) => {
                tracing::info!(target: "festerm::session", ?lifecycle, "local session lifecycle");
                self.last_lifecycle = Some(lifecycle);
            }
            SessionEvent::ResizeApplied(size) => {
                tracing::debug!(
                    target: "festerm::session",
                    columns = size.columns(),
                    rows = size.rows(),
                    "local PTY resized"
                );
                self.last_resize = Some(size);
            }
            SessionEvent::Backpressure { direction, .. } => {
                tracing::warn!(
                    target: "festerm::session",
                    ?direction,
                    "local session queue pressure"
                );
                self.last_backpressure = Some(direction);
            }
            SessionEvent::Error(error) => self.record_session_error(error),
            SessionEvent::Output(_) => {}
        }
    }

    fn forward_terminal_replies(&mut self, terminal: &mut Terminal) {
        if terminal.take_reply_queue_overflowed() {
            self.last_error = Some("terminal reply queue overflowed".to_owned());
            tracing::warn!(
                target: "festerm::session",
                "terminal reply queue overflowed before local-session forwarding"
            );
        }
        let replies = terminal.drain_replies();
        if !replies.is_empty() {
            self.queue_bytes(&replies, "terminal reply");
        }
    }

    fn flush_pending_writes(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        match self.pending_writes.flush(session) {
            PendingFlush::Drained => {}
            PendingFlush::Backpressured => {
                self.last_backpressure = Some(FlowDirection::Input);
            }
            PendingFlush::Unrecoverable {
                error,
                dropped_bytes,
            } => {
                self.record_pending_failure(
                    format!(
                        "local-session pending writes became unrecoverable after {error}; \
                         discarded {dropped_bytes} bytes"
                    ),
                    Some(error),
                );
            }
        }
    }

    fn flush_pending_resize(&mut self) {
        let Some(size) = self.pending_resize.take() else {
            return;
        };
        self.try_resize(size);
    }

    fn try_resize(&mut self, size: TerminalSize) {
        let Some(session) = &self.session else {
            return;
        };
        match session.try_resize(size) {
            Ok(()) => {}
            Err(SessionSendError::Full { .. }) => {
                self.pending_resize = Some(size);
                self.last_backpressure = Some(FlowDirection::Resize);
            }
            Err(error) => self.record_send_error(error),
        }
    }

    fn queue_bytes(&mut self, bytes: &[u8], source: &str) {
        if self.session.is_none() {
            return;
        }
        match self.pending_writes.enqueue(bytes) {
            Ok(()) => self.flush_pending_writes(),
            Err(error) => {
                self.record_pending_failure(error.to_string(), None);
            }
        }
        tracing::trace!(
            target: "festerm::session",
            byte_count = bytes.len(),
            source,
            "queued content-free local session write"
        );
    }

    fn record_pending_failure(&mut self, message: String, error: Option<SessionSendError>) {
        if let Some(error) = error {
            tracing::warn!(
                target: "festerm::session",
                %error,
                "local session command became unrecoverable"
            );
        } else {
            tracing::warn!(
                target: "festerm::session",
                %message,
                "local session pending-write bound exceeded"
            );
        }
        self.last_error = Some(message);
    }

    fn record_send_error(&mut self, error: SessionSendError) {
        tracing::warn!(target: "festerm::session", %error, "local session command rejected");
        self.last_error = Some(error.to_string());
    }

    fn record_session_error(&mut self, error: SessionError) {
        tracing::error!(target: "festerm::session", %error, "local session backend error");
        self.last_error = Some(error.to_string());
    }

    fn status_line(&self) -> String {
        if let Some(error) = &self.startup_error {
            return format!("Local shell unavailable: {error}");
        }
        let Some(_session) = &self.session else {
            return "No local session".to_owned();
        };
        let lifecycle = self
            .last_lifecycle
            .as_ref()
            .unwrap_or(&SessionLifecycle::Starting);
        let latest_error = self
            .last_error
            .as_deref()
            .map(|error| format!("; error: {error}"))
            .unwrap_or_default();
        let latest_resize = self
            .last_resize
            .map(|size| format!("; resize {}x{}", size.columns(), size.rows()))
            .unwrap_or_default();
        format!(
            "Local shell {lifecycle:?}{latest_resize}{latest_error}",
            lifecycle = lifecycle
        )
    }

    fn diagnostics_line(&self) -> String {
        if let Some(error) = &self.startup_error {
            return format!("Local shell unavailable: {error}");
        }
        let Some(session) = &self.session else {
            return "No local session".to_owned();
        };
        let metrics = session.metrics();
        let lifecycle = self
            .last_lifecycle
            .as_ref()
            .unwrap_or(&SessionLifecycle::Starting);
        let latest_error = self
            .last_error
            .as_deref()
            .map(|error| format!("; error: {error}"))
            .unwrap_or_default();
        let queue_pressure = self
            .last_backpressure
            .map(|direction| format!("; pressure: {direction:?}"))
            .unwrap_or_default();
        let latest_resize = self
            .last_resize
            .map(|size| format!("; resize {}x{}", size.columns(), size.rows()))
            .unwrap_or_default();
        format!(
            "Local shell {lifecycle:?}; in {} B, out {} B; events {}/{} (high {}); \
             pending writes {} entries, {} / {} B; pressure {}; errors {}; resizes {}\
             {latest_resize}{queue_pressure}{latest_error}",
            metrics.input_bytes,
            metrics.output_bytes,
            metrics.event_queue_depth,
            metrics.event_queue_capacity,
            metrics.event_queue_high_watermark,
            self.pending_writes.queued_writes(),
            self.pending_writes.queued_bytes(),
            self.pending_writes.capacity(),
            metrics.backpressure_count,
            metrics.error_count,
            metrics.resize_count,
        )
    }
}

impl EncodedInputSink for LocalSessionSink {
    fn record_encoded_input(&mut self, bytes: &[u8]) {
        self.diagnostics.byte_count = self
            .diagnostics
            .byte_count
            .saturating_add(bytes.len() as u64);
        self.queue_bytes(bytes, "UI input");
    }

    fn observe_input_route(&mut self, route: InputRoute) {
        self.diagnostics.event_count = self.diagnostics.event_count.saturating_add(1);
        self.diagnostics.last_outcome = Some(route.outcome);
        self.diagnostics.last_queue_depth = route.queue_depth;
    }

    fn record_terminal_resize(&mut self, dimensions: Dimensions) {
        match terminal_size(dimensions) {
            Ok(size) => self.try_resize(size),
            Err(error) => {
                self.last_error = Some(error);
            }
        }
    }

    fn input_diagnostics(&self) -> Option<InputSinkDiagnostics> {
        Some(self.diagnostics)
    }
}

fn terminal_size(dimensions: Dimensions) -> Result<TerminalSize, String> {
    let columns = u16::try_from(dimensions.columns()).map_err(|_| {
        format!(
            "terminal columns {} exceed PTY limits",
            dimensions.columns()
        )
    })?;
    let rows = u16::try_from(dimensions.rows())
        .map_err(|_| format!("terminal rows {} exceed PTY limits", dimensions.rows()))?;
    TerminalSize::new(columns, rows).map_err(|error| error.to_string())
}

struct PumpResult {
    hit_limit: bool,
}

fn pump_session_events(
    session: &impl Session,
    terminal: &mut Terminal,
    maximum: usize,
    mut observe: impl FnMut(&SessionEvent),
) -> PumpResult {
    for _ in 0..maximum {
        match session.try_recv_event() {
            Ok(SessionEvent::Output(bytes)) => terminal.ingest(&bytes),
            Ok(event) => observe(&event),
            Err(SessionTryReceiveError::Empty | SessionTryReceiveError::Closed) => {
                return PumpResult { hit_limit: false };
            }
        }
    }
    PumpResult { hit_limit: true }
}

fn seed_session_failure(terminal: &mut Terminal, error: &str) {
    terminal.ingest(
        "\x1b[2J\x1b[H\
\x1b[1;31mLocal shell could not start.\x1b[0m\r\n\
\r\n\
\x1b[1;33mfesTerm remains in no-session mode; this is not a shell.\x1b[0m\r\n\
\r\n\
Check the session status line for the content-free launch error.\r\n\
No commands are executed until a local shell can be created.\r\n"
            .as_bytes(),
    );
    tracing::error!(
        target: "festerm::session",
        error,
        "showing no-session fallback after local shell startup failure"
    );
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard};
    #[cfg(any(unix, windows))]
    use std::time::Instant;

    use super::*;

    #[test]
    fn no_session_fallback_retains_only_content_free_input_metadata() {
        let mut sink = LocalSessionSink {
            session: None,
            startup_error: Some("controlled startup failure".to_owned()),
            diagnostics: InputSinkDiagnostics::default(),
            pending_writes: PendingCommandBuffer::new(MAX_PENDING_COMMAND_BYTES),
            pending_resize: None,
            last_lifecycle: None,
            last_error: None,
            last_backpressure: None,
            last_resize: None,
        };
        sink.record_encoded_input(b"private paste contents");
        sink.observe_input_route(InputRoute {
            outcome: festerm_core::InputEventOutcome::Encoded { bytes: 22 },
            queue_depth: 22,
            delivered_bytes: 22,
        });

        assert_eq!(
            sink.input_diagnostics(),
            Some(InputSinkDiagnostics {
                event_count: 1,
                byte_count: 22,
                last_outcome: Some(festerm_core::InputEventOutcome::Encoded { bytes: 22 }),
                last_queue_depth: 22,
            })
        );
    }

    #[test]
    fn no_session_fallback_is_not_a_prompt() {
        let mut terminal =
            Terminal::new(Dimensions::new(80, 24).unwrap()).expect("terminal allocation");
        seed_session_failure(&mut terminal, "controlled failure");
        assert!(terminal
            .row_text(2)
            .is_some_and(|row| row.starts_with("fesTerm remains in no-session mode")));
    }

    #[test]
    fn terminal_title_is_scoped_to_the_application_window() {
        assert_eq!(FesTermApp::window_title(""), APPLICATION_TITLE);
        assert_eq!(FesTermApp::window_title("editor"), "editor - fesTerm");
    }

    #[test]
    fn fake_session_pump_keeps_the_application_as_terminal_writer() {
        let session = FakeSession::new([
            SessionEvent::Output(b"fake backend output".to_vec()),
            SessionEvent::Lifecycle(SessionLifecycle::Exited(
                festerm_session::SessionExit::with_exit_code(0),
            )),
        ]);
        let mut terminal =
            Terminal::new(Dimensions::new(80, 24).unwrap()).expect("terminal allocation");
        let mut observed = Vec::new();

        let result = pump_session_events(&session, &mut terminal, 8, |event| {
            observed.push(event.clone());
        });

        assert!(!result.hit_limit);
        assert!(terminal
            .row_text(0)
            .is_some_and(|row| row.starts_with("fake backend output")));
        assert_eq!(observed.len(), 1);
        assert!(matches!(
            observed.first(),
            Some(SessionEvent::Lifecycle(SessionLifecycle::Exited(_)))
        ));
    }

    #[test]
    fn capped_event_pump_requests_a_follow_up_frame() {
        let session = FakeSession::new([
            SessionEvent::Output(b"first".to_vec()),
            SessionEvent::Output(b"second".to_vec()),
        ]);
        let mut terminal =
            Terminal::new(Dimensions::new(80, 24).unwrap()).expect("terminal allocation");

        let result = pump_session_events(&session, &mut terminal, 1, |_| {});

        assert!(result.hit_limit);
        assert!(terminal
            .row_text(0)
            .is_some_and(|row| row.starts_with("first")));
    }

    #[test]
    fn pending_commands_survive_backpressure_and_preserve_reply_input_order() {
        let session = FakeSession::with_input_results(
            [],
            [
                Err(SessionSendError::Full {
                    operation: festerm_session::SessionOperation::Input,
                    capacity: 2,
                }),
                Err(SessionSendError::Full {
                    operation: festerm_session::SessionOperation::Input,
                    capacity: 2,
                }),
                Ok(()),
                Ok(()),
            ],
        );
        let mut pending = PendingCommandBuffer::new(64);

        pending.enqueue(b"reply").unwrap();
        assert!(matches!(
            pending.flush(&session),
            PendingFlush::Backpressured
        ));
        pending.enqueue(b"input").unwrap();
        assert!(matches!(
            pending.flush(&session),
            PendingFlush::Backpressured
        ));
        assert_eq!(pending.queued_writes(), 2);
        assert_eq!(pending.queued_bytes(), b"replyinput".len());

        assert!(matches!(pending.flush(&session), PendingFlush::Drained));
        assert_eq!(session.sent(), vec![b"reply".to_vec(), b"input".to_vec()]);
        assert_eq!(pending.queued_writes(), 0);
    }

    #[test]
    fn pending_command_bound_reports_unrecoverable_input_explicitly() {
        let mut pending = PendingCommandBuffer::new(4);
        pending.enqueue(b"four").unwrap();

        let error = pending.enqueue(b"more").unwrap_err();

        assert_eq!(
            error,
            PendingCommandError::Full {
                capacity: 4,
                queued: 4,
                attempted: 4,
            }
        );
        assert!(error.to_string().contains("exceeded their 4-byte bound"));
        assert_eq!(pending.queued_bytes(), 4);
    }

    #[cfg(unix)]
    fn controlled_local_sink(session: LocalPtySession) -> LocalSessionSink {
        LocalSessionSink {
            session: Some(session),
            startup_error: None,
            diagnostics: InputSinkDiagnostics::default(),
            pending_writes: PendingCommandBuffer::new(MAX_PENDING_COMMAND_BYTES),
            pending_resize: None,
            last_lifecycle: Some(SessionLifecycle::Running),
            last_error: None,
            last_backpressure: None,
            last_resize: None,
        }
    }

    #[cfg(unix)]
    fn pump_controlled_session_until(
        sink: &mut LocalSessionSink,
        terminal: &mut Terminal,
        predicate: impl Fn(&Terminal) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            sink.pump_events(terminal);
            sink.forward_terminal_replies(terminal);
            sink.flush_pending_writes();
            if predicate(terminal) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for controlled application session state");
    }

    #[cfg(unix)]
    #[test]
    fn controlled_session_routes_m6_modes_replies_input_resize_and_alternate_screen() {
        let profile = LocalProfile::new("/bin/sh").with_arguments([
            "-c",
            "stty raw -echo; \
             printf 'PRIMARY\\033[?1049hALT\\033[6n'; \
             dd bs=1 count=6 2>/dev/null | od -An -tx1 | tr -d ' \\n'; printf '\\n'; \
             printf '\\033[?2004h\\033[?1004h\\033[?1000h\\033[?1006h'; \
             dd bs=1 count=25 2>/dev/null | od -An -tx1 | tr -d ' \\n'; printf '\\n'; \
             printf '\\033[?1049lDONE\\n'; exit",
        ]);
        let session = LocalPtySession::start(profile, TerminalSize::new(80, 24).unwrap()).unwrap();
        let mut sink = controlled_local_sink(session);
        let mut terminal =
            Terminal::new(Dimensions::new(80, 24).unwrap()).expect("terminal allocation");

        pump_controlled_session_until(&mut sink, &mut terminal, |terminal| {
            let modes = terminal.modes();
            modes.alternate_screen()
                && modes.bracketed_paste()
                && modes.focus_reporting()
                && modes.sgr_mouse()
        });

        terminal.resize(Dimensions::new(73, 26).unwrap()).unwrap();
        sink.record_terminal_resize(terminal.dimensions());
        assert_eq!(
            terminal.handle_input(festerm_core::InputEvent::Focus(
                festerm_core::FocusEvent::In,
            )),
            festerm_core::InputEventOutcome::Encoded { bytes: 3 }
        );
        assert_eq!(
            terminal.handle_input(festerm_core::InputEvent::Paste("z".to_owned())),
            festerm_core::InputEventOutcome::Encoded { bytes: 13 }
        );
        assert_eq!(
            terminal.handle_input(festerm_core::InputEvent::Mouse(festerm_core::MouseEvent {
                kind: festerm_core::MouseEventKind::Press(festerm_core::MouseButton::Left),
                column: 1,
                row: 2,
                modifiers: festerm_core::Modifiers::NONE,
            },)),
            festerm_core::InputEventOutcome::Encoded { bytes: 9 }
        );
        let input = terminal.drain_input();
        assert_eq!(input, b"\x1b[I\x1b[200~z\x1b[201~\x1b[<0;2;3M");
        sink.record_encoded_input(&input);

        pump_controlled_session_until(&mut sink, &mut terminal, |terminal| {
            !terminal.modes().alternate_screen()
                && terminal
                    .row_text(0)
                    .is_some_and(|row| row.starts_with("PRIMARYDONE"))
        });
        assert!(sink
            .session
            .as_ref()
            .is_some_and(|session| session.metrics().resize_count >= 1));
        assert!(matches!(
            sink.session
                .as_ref()
                .expect("controlled session remains available")
                .shutdown(Duration::from_secs(2)),
            Ok(festerm_session::ShutdownResult::AlreadyStopped)
                | Ok(festerm_session::ShutdownResult::Stopped)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn conpty_banner_survives_repeated_app_owned_resizes() {
        let session = LocalPtySession::start_default(TerminalSize::new(80, 24).unwrap())
            .expect("default Windows shell starts");
        let mut terminal =
            Terminal::new(Dimensions::new(80, 24).unwrap()).expect("terminal allocation");
        let deadline = Instant::now() + Duration::from_secs(3);

        while Instant::now() < deadline
            && !terminal
                .row_text(0)
                .is_some_and(|row| row.starts_with("Microsoft Windows"))
        {
            pump_session_events(
                &session,
                &mut terminal,
                MAX_SESSION_EVENTS_PER_FRAME,
                |_| {},
            );
            let replies = terminal.drain_replies();
            if !replies.is_empty() {
                session
                    .try_send_input(&replies)
                    .expect("cursor-position reply reaches ConPTY");
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        assert!(
            terminal
                .row_text(0)
                .is_some_and(|row| row.starts_with("Microsoft Windows")),
            "Windows banner should be available before resize"
        );

        for (index, dimensions) in [
            Dimensions::new(37, 13).unwrap(),
            Dimensions::new(73, 26).unwrap(),
            Dimensions::new(50, 18).unwrap(),
            Dimensions::new(73, 26).unwrap(),
        ]
        .into_iter()
        .enumerate()
        {
            terminal.resize(dimensions).unwrap();
            session
                .try_resize(terminal_size(dimensions).unwrap())
                .expect("resize reaches ConPTY");
            let reply_deadline = Instant::now() + Duration::from_millis(100);
            while Instant::now() < reply_deadline {
                pump_session_events(
                    &session,
                    &mut terminal,
                    MAX_SESSION_EVENTS_PER_FRAME,
                    |_| {},
                );
                let replies = terminal.drain_replies();
                if !replies.is_empty() {
                    session
                        .try_send_input(&replies)
                        .expect("resize cursor-position reply reaches ConPTY");
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let marker = format!("P0FRAME:{index}");
            session
                .try_send_input(format!("echo {marker}\r\n").as_bytes())
                .expect("controlled output reaches ConPTY");
            let resize_deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < resize_deadline {
                let result = pump_session_events(
                    &session,
                    &mut terminal,
                    MAX_SESSION_EVENTS_PER_FRAME,
                    |_| {},
                );
                let replies = terminal.drain_replies();
                if !replies.is_empty() {
                    session
                        .try_send_input(&replies)
                        .expect("cursor-position reply reaches ConPTY");
                }
                if (0..terminal.dimensions().rows()).any(|row| {
                    terminal
                        .row_text(row)
                        .is_some_and(|text| text.contains(&marker))
                }) {
                    break;
                }
                if !result.hit_limit {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            assert!(
                (0..terminal.dimensions().rows()).any(|row| {
                    terminal
                        .row_text(row)
                        .is_some_and(|text| text.contains(&marker))
                }),
                "resize to {}x{} lost output emitted after the resize",
                dimensions.columns(),
                dimensions.rows()
            );
        }

        assert_eq!(
            session.shutdown(Duration::from_secs(2)),
            Ok(festerm_session::ShutdownResult::Stopped)
        );
    }

    struct FakeSession {
        events: Mutex<VecDeque<SessionEvent>>,
        input_results: Mutex<VecDeque<Result<(), SessionSendError>>>,
        sent: Mutex<Vec<Vec<u8>>>,
    }

    impl FakeSession {
        fn new(events: impl IntoIterator<Item = SessionEvent>) -> Self {
            Self::with_input_results(events, [])
        }

        fn with_input_results(
            events: impl IntoIterator<Item = SessionEvent>,
            input_results: impl IntoIterator<Item = Result<(), SessionSendError>>,
        ) -> Self {
            Self {
                events: Mutex::new(events.into_iter().collect()),
                input_results: Mutex::new(input_results.into_iter().collect()),
                sent: Mutex::new(Vec::new()),
            }
        }

        fn events(&self) -> MutexGuard<'_, VecDeque<SessionEvent>> {
            self.events.lock().expect("fake session event lock")
        }

        fn sent(&self) -> Vec<Vec<u8>> {
            self.sent.lock().expect("fake session sent lock").clone()
        }
    }

    impl Session for FakeSession {
        fn id(&self) -> festerm_session::SessionId {
            festerm_session::SessionId::next()
        }

        fn lifecycle(&self) -> SessionLifecycle {
            SessionLifecycle::Running
        }

        fn metrics(&self) -> SessionMetrics {
            SessionMetrics::default()
        }

        fn try_send_input(&self, bytes: &[u8]) -> Result<(), SessionSendError> {
            let result = self
                .input_results
                .lock()
                .expect("fake session input result lock")
                .pop_front()
                .unwrap_or(Ok(()));
            if result.is_ok() {
                self.sent
                    .lock()
                    .expect("fake session sent lock")
                    .push(bytes.to_vec());
            }
            result
        }

        fn try_resize(&self, _size: TerminalSize) -> Result<(), SessionSendError> {
            Ok(())
        }

        fn try_shutdown(&self) -> Result<(), SessionSendError> {
            Ok(())
        }

        fn try_recv_event(&self) -> Result<SessionEvent, SessionTryReceiveError> {
            self.events()
                .pop_front()
                .ok_or(SessionTryReceiveError::Empty)
        }

        fn shutdown(
            &self,
            _timeout: Duration,
        ) -> Result<festerm_session::ShutdownResult, festerm_session::ShutdownError> {
            Ok(festerm_session::ShutdownResult::Stopped)
        }
    }
}
