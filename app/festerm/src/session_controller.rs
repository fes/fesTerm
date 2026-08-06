use std::{collections::VecDeque, time::Duration};

use festerm_core::{Dimensions, Terminal};
use festerm_session::{
    FlowDirection, Session, SessionError, SessionEvent, SessionLifecycle, SessionSendError,
    SessionTryReceiveError, TerminalSize, DEFAULT_COMMAND_QUEUE_CAPACITY, MAX_IO_CHUNK_BYTES,
};
use festerm_ui_egui::{EncodedInputSink, InputRoute, InputSinkDiagnostics};

#[cfg(test)]
use festerm_session::SessionMetrics;

/// Maximum session events drained per frame before yielding for repaint.
pub const MAX_SESSION_EVENTS_PER_FRAME: usize = 64;

/// Bounded shutdown timeout for the local PTY session.
pub const SESSION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Maximum bytes retained in the ordered pending-write buffer.
pub const MAX_PENDING_COMMAND_BYTES: usize = DEFAULT_COMMAND_QUEUE_CAPACITY * MAX_IO_CHUNK_BYTES;

// ─── Pending Command Buffer ──────────────────────────────────────────────────

pub(crate) struct PendingCommandBuffer {
    writes: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    capacity: usize,
}

impl PendingCommandBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            writes: VecDeque::new(),
            queued_bytes: 0,
            capacity,
        }
    }

    pub fn enqueue(&mut self, bytes: &[u8]) -> Result<(), PendingCommandError> {
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

    pub fn flush(&mut self, session: &impl Session) -> PendingFlush {
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

    pub fn queued_bytes(&self) -> usize {
        self.queued_bytes
    }

    pub fn queued_writes(&self) -> usize {
        self.writes.len()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PendingCommandError {
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

pub(crate) enum PendingFlush {
    Drained,
    Backpressured,
    Unrecoverable {
        error: SessionSendError,
        dropped_bytes: usize,
    },
}

// ─── Session Controller ──────────────────────────────────────────────────────

/// Application-level controller that owns session event pumping, ordered pending
/// writes, terminal replies, resize forwarding, lifecycle state, and diagnostics.
///
/// The controller preserves the architectural invariant that only one logical
/// writer mutates a `Terminal` from session output.
pub struct SessionController<S: Session> {
    session: Option<S>,
    startup_error: Option<String>,
    diagnostics: InputSinkDiagnostics,
    pending_writes: PendingCommandBuffer,
    pending_resize: Option<TerminalSize>,
    last_lifecycle: Option<SessionLifecycle>,
    last_error: Option<String>,
    last_backpressure: Option<FlowDirection>,
    last_resize: Option<TerminalSize>,
}

impl<S: Session> SessionController<S> {
    /// Creates a controller from an already-started session.
    ///
    /// This is the production constructor when the session was started externally
    /// (e.g. by the bootstrap code that owns the notifier).
    pub fn with_session(session: S) -> Self {
        let lifecycle = session.lifecycle();
        Self {
            session: Some(session),
            startup_error: None,
            diagnostics: InputSinkDiagnostics::default(),
            pending_writes: PendingCommandBuffer::new(MAX_PENDING_COMMAND_BYTES),
            pending_resize: None,
            last_lifecycle: Some(lifecycle),
            last_error: None,
            last_backpressure: None,
            last_resize: None,
        }
    }

    /// Creates a controller in no-session mode with a startup error message.
    pub fn with_startup_error(error: String) -> Self {
        Self {
            session: None,
            startup_error: Some(error),
            diagnostics: InputSinkDiagnostics::default(),
            pending_writes: PendingCommandBuffer::new(MAX_PENDING_COMMAND_BYTES),
            pending_resize: None,
            last_lifecycle: None,
            last_error: None,
            last_backpressure: None,
            last_resize: None,
        }
    }

    /// Test constructor: creates a controller with an injected session double.
    ///
    /// This is the controlled test seam — callers supply a fake or mock `Session`
    /// implementation rather than constructing internal fields directly.
    pub fn for_test(session: S) -> Self {
        Self::with_session(session)
    }

    pub fn start_error(&self) -> Option<&str> {
        self.startup_error.as_deref()
    }

    pub fn session(&self) -> Option<&S> {
        self.session.as_ref()
    }

    /// Returns whether another frame is required to continue a bounded drain.
    pub fn pump_events(&mut self, terminal: &mut Terminal) -> bool {
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

    pub fn forward_terminal_replies(&mut self, terminal: &mut Terminal) {
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

    pub fn flush_pending_writes(&mut self) {
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

    pub fn flush_pending_resize(&mut self) {
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

    pub fn status_line(&self) -> String {
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

    pub fn diagnostics_line(&self) -> String {
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

    /// Bounded shutdown of the owned session. Called from `Drop` or explicit teardown.
    pub fn shutdown(&self) {
        if let Some(session) = &self.session {
            match session.shutdown(SESSION_SHUTDOWN_TIMEOUT) {
                Ok(result) => tracing::info!(
                    target: "festerm::session",
                    ?result,
                    "local session shut down"
                ),
                Err(error) => tracing::error!(
                    target: "festerm::session",
                    %error,
                    "local session did not finish bounded shutdown"
                ),
            }
        }
    }
}

impl<S: Session> EncodedInputSink for SessionController<S> {
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

impl<S: Session> Drop for SessionController<S> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ─── Free Functions ──────────────────────────────────────────────────────────

pub fn terminal_size(dimensions: Dimensions) -> Result<TerminalSize, String> {
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

pub struct PumpResult {
    pub hit_limit: bool,
}

pub fn pump_session_events(
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

pub fn seed_session_failure(terminal: &mut Terminal, error: &str) {
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

// ─── Test Support ────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::{Mutex, MutexGuard},
    };

    pub struct FakeSession {
        events: Mutex<VecDeque<SessionEvent>>,
        input_results: Mutex<VecDeque<Result<(), SessionSendError>>>,
        sent: Mutex<Vec<Vec<u8>>>,
    }

    impl FakeSession {
        pub fn new(events: impl IntoIterator<Item = SessionEvent>) -> Self {
            Self::with_input_results(events, [])
        }

        pub fn with_input_results(
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

        pub fn sent(&self) -> Vec<Vec<u8>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use fake::FakeSession;

    #[cfg(any(unix, windows))]
    use std::time::Instant;

    #[cfg(any(unix, windows))]
    use festerm_pty::{LocalProfile, LocalPtySession};

    #[test]
    fn no_session_fallback_retains_only_content_free_input_metadata() {
        let mut controller: SessionController<FakeSession> =
            SessionController::with_startup_error("controlled startup failure".to_owned());
        controller.record_encoded_input(b"private paste contents");
        controller.observe_input_route(InputRoute {
            outcome: festerm_core::InputEventOutcome::Encoded { bytes: 22 },
            queue_depth: 22,
            delivered_bytes: 22,
        });

        assert_eq!(
            controller.input_diagnostics(),
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
    fn fake_session_pump_keeps_the_application_as_terminal_writer() {
        let session = FakeSession::new([
            SessionEvent::Output(b"fake backend output".to_vec()),
            SessionEvent::Lifecycle(SessionLifecycle::Exited(
                festerm_session::SessionExit::with_exit_code(0),
            )),
        ]);
        let mut controller = SessionController::for_test(session);
        let mut terminal =
            Terminal::new(Dimensions::new(80, 24).unwrap()).expect("terminal allocation");

        let needs_repaint = controller.pump_events(&mut terminal);

        assert!(!needs_repaint);
        assert!(terminal
            .row_text(0)
            .is_some_and(|row| row.starts_with("fake backend output")));
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
    fn pump_controlled_session_until(
        controller: &mut SessionController<LocalPtySession>,
        terminal: &mut Terminal,
        predicate: impl Fn(&Terminal) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            controller.pump_events(terminal);
            controller.forward_terminal_replies(terminal);
            controller.flush_pending_writes();
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
        let mut controller = SessionController::with_session(session);
        let mut terminal =
            Terminal::new(Dimensions::new(80, 24).unwrap()).expect("terminal allocation");

        pump_controlled_session_until(&mut controller, &mut terminal, |terminal| {
            let modes = terminal.modes();
            modes.alternate_screen()
                && modes.bracketed_paste()
                && modes.focus_reporting()
                && modes.sgr_mouse()
        });

        terminal.resize(Dimensions::new(73, 26).unwrap()).unwrap();
        controller.record_terminal_resize(terminal.dimensions());
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
        controller.record_encoded_input(&input);

        pump_controlled_session_until(&mut controller, &mut terminal, |terminal| {
            !terminal.modes().alternate_screen()
                && terminal
                    .row_text(0)
                    .is_some_and(|row| row.starts_with("PRIMARYDONE"))
        });
        assert!(controller
            .session()
            .is_some_and(|session| session.metrics().resize_count >= 1));
        assert!(matches!(
            controller
                .session()
                .expect("controlled session remains available")
                .shutdown(Duration::from_secs(2)),
            Ok(festerm_session::ShutdownResult::AlreadyStopped)
                | Ok(festerm_session::ShutdownResult::Stopped)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn conpty_banner_survives_repeated_app_owned_resizes() {
        use festerm_pty::LocalPtySession;

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

    // ─── Tier 6 native-platform smoke tests ─────────────────────────────────
    //
    // All tests below are marked `#[ignore]` and are NOT part of the PR-blocking
    // CI matrix.  Run them explicitly via:
    //
    //   cargo test -p festerm -- --include-ignored
    //
    // or via `.github/workflows/native-smoke.yml` (scheduled + dispatch).
    //
    // Flaky-failure policy: tests here NEVER silently retry.  A single run
    // either passes or fails.  See `docs/native-smoke-policy.md`.

    /// Returns the path to the `festerm-pty-test-child` binary for smoke tests.
    #[cfg(any(unix, windows))]
    fn test_child_path_for_smoke() -> std::path::PathBuf {
        let mut path = std::env::current_exe()
            .expect("test executable path is known")
            .canonicalize()
            .expect("test executable path is accessible");
        path.pop();
        if path.ends_with("deps") {
            path.pop();
        }
        let name = if cfg!(windows) {
            "festerm-pty-test-child.exe"
        } else {
            "festerm-pty-test-child"
        };
        path.push(name);
        assert!(
            path.exists(),
            "festerm-pty-test-child not found at {path:?}; \
             run `cargo test --workspace` to build it first"
        );
        path
    }

    /// Poll `controller` until the session exits or the deadline passes.
    ///
    /// Avoids the closure-borrow conflict in `pump_controlled_until`.
    #[cfg(any(unix, windows))]
    fn wait_for_session_exit(
        controller: &mut SessionController<LocalPtySession>,
        terminal: &mut Terminal,
        timeout: Duration,
    ) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            controller.pump_events(terminal);
            controller.forward_terminal_replies(terminal);
            controller.flush_pending_writes();
            let exited = controller
                .session()
                .map(|s| {
                    matches!(
                        s.lifecycle(),
                        SessionLifecycle::Exited(_) | SessionLifecycle::Stopped
                    )
                })
                .unwrap_or(true);
            if exited {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("smoke-flow timeout: test child did not exit within {timeout:?}");
    }

    /// Pump `controller` + `terminal` until `predicate(terminal)` is true or
    /// `timeout` elapses.  Panics on timeout with `context` as the message.
    ///
    /// No retries — one run, one result.
    #[cfg(any(unix, windows))]
    fn pump_controlled_until(
        controller: &mut SessionController<LocalPtySession>,
        terminal: &mut Terminal,
        predicate: impl Fn(&Terminal) -> bool,
        timeout: Duration,
        context: &str,
    ) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            controller.pump_events(terminal);
            controller.forward_terminal_replies(terminal);
            controller.flush_pending_writes();
            if predicate(terminal) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("smoke-flow timeout: {context}");
    }

    /// Returns `true` if any terminal row contains `needle`.
    #[cfg(any(unix, windows))]
    fn smoke_row_contains(terminal: &Terminal, needle: &str) -> bool {
        (0..terminal.dimensions().rows())
            .filter_map(|r| terminal.row_text(r))
            .any(|text| text.contains(needle))
    }

    /// **Windows ConPTY smoke flow — issue #3 resize sequence.**
    ///
    /// Uses `festerm-pty-test-child` as the controlled shell.  The child emits
    /// a `MARKER`, blocks on `read-line` while we apply the four-step resize
    /// sequence (`37×13 → 73×26 → 50×18 → 73×26`), then we send input, verify
    /// the echo and `report-size` output, and assert bounded shutdown.
    ///
    /// Acceptance criterion: the sequence completes without a session error,
    /// output bytes arrive intact after all four resizes, and `report-size`
    /// reports the final `73×26` dimensions confirming the resize reached ConPTY.
    #[cfg(windows)]
    #[test]
    #[ignore = "native smoke — run via native-smoke.yml or with --include-ignored; not PR-blocking"]
    fn windows_conpty_smoke_flow_with_test_child_and_issue3_resizes() {
        let profile = LocalProfile::new(test_child_path_for_smoke()).with_arguments([
            "emit:LINE-A",
            "emit:LINE-B",
            "emit:MARKER",
            "read-line",
            "echo:ECHO",
            "report-size",
            "exit:0",
        ]);

        let session = LocalPtySession::start(profile, TerminalSize::new(73, 26).unwrap())
            .expect("ConPTY session starts with test child");

        let mut terminal =
            Terminal::new(Dimensions::new(73, 26).unwrap()).expect("terminal allocation");
        let mut controller = SessionController::for_test(session);

        // Step 1: wait for the deterministic MARKER.
        pump_controlled_until(
            &mut controller,
            &mut terminal,
            |t| smoke_row_contains(t, "MARKER"),
            Duration::from_secs(5),
            "MARKER from test child",
        );
        assert_eq!(terminal.dimensions().columns(), 73);
        assert_eq!(terminal.dimensions().rows(), 26);

        // Step 2: apply the issue #3 resize sequence while the child is blocked
        // on read-line (output is active — LINE-A and LINE-B are in the terminal).
        for &(cols, rows) in &[(37u16, 13u16), (73, 26), (50, 18), (73, 26)] {
            let dims =
                Dimensions::new(cols as usize, rows as usize).expect("resize dimensions are valid");
            terminal.resize(dims).expect("terminal resize succeeds");
            controller.record_terminal_resize(dims);
            // Pump for ≥200 ms to give ConPTY time to apply the resize.
            let settle = Instant::now() + Duration::from_millis(250);
            while Instant::now() < settle {
                controller.pump_events(&mut terminal);
                controller.forward_terminal_replies(&mut terminal);
                controller.flush_pending_writes();
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                terminal.dimensions().columns(),
                cols as usize,
                "columns after resize to {cols}×{rows}"
            );
            assert_eq!(
                terminal.dimensions().rows(),
                rows as usize,
                "rows after resize to {cols}×{rows}"
            );
        }

        // Step 3: send a line to unblock read-line.
        controller.record_encoded_input(b"hello\r\n");

        // Step 4: wait for the echo (proves bytes survived all four resizes).
        pump_controlled_until(
            &mut controller,
            &mut terminal,
            |t| smoke_row_contains(t, "ECHO:hello"),
            Duration::from_secs(5),
            "ECHO:hello from test child after resize sequence",
        );

        // Step 5: wait for report-size output.
        // report-size writes "{rows} {cols}\n"; at 73×26 that is "26 73".
        // This confirms the resize propagated all the way to the child's PTY.
        pump_controlled_until(
            &mut controller,
            &mut terminal,
            |t| smoke_row_contains(t, "26 73"),
            Duration::from_secs(5),
            "report-size showing 26 73 (rows × cols at final 73×26 size)",
        );

        // Step 6: wait for exit and assert bounded shutdown.
        wait_for_session_exit(&mut controller, &mut terminal, Duration::from_secs(5));
    }

    /// **Windows ConPTY — bounded shutdown terminates the process tree.**
    ///
    /// Verifies that dropping the `SessionController` (which calls `shutdown`)
    /// terminates a spinning test child within the documented 2-second bound.
    #[cfg(windows)]
    #[test]
    #[ignore = "native smoke — run via native-smoke.yml or with --include-ignored; not PR-blocking"]
    fn windows_conpty_bounded_shutdown_terminates_process_tree() {
        let profile =
            LocalProfile::new(test_child_path_for_smoke()).with_arguments(["emit:RUNNING", "spin"]);

        let session = LocalPtySession::start(profile, TerminalSize::new(73, 26).unwrap())
            .expect("ConPTY session starts");

        let mut terminal =
            Terminal::new(Dimensions::new(73, 26).unwrap()).expect("terminal allocation");
        let mut controller = SessionController::for_test(session);

        pump_controlled_until(
            &mut controller,
            &mut terminal,
            |t| smoke_row_contains(t, "RUNNING"),
            Duration::from_secs(5),
            "RUNNING marker from spinning test child",
        );

        let start = Instant::now();
        drop(controller); // triggers bounded shutdown via Drop impl
        assert!(
            start.elapsed() <= Duration::from_secs(3),
            "bounded ConPTY shutdown exceeded 3-second budget: {:?}",
            start.elapsed()
        );
    }

    /// **Unix PTY smoke flow — issue #3 resize sequence.**
    ///
    /// Mirrors the Windows ConPTY flow on the Unix PTY path.  Written for
    /// platform coverage; the first Linux CI run via `native-smoke.yml` will
    /// provide ground-truth results.
    ///
    /// **Cannot execute in the Windows sandbox** — results pending Linux CI.
    #[cfg(unix)]
    #[test]
    #[ignore = "native smoke — run via native-smoke.yml or with --include-ignored; not PR-blocking"]
    fn unix_pty_smoke_flow_with_test_child_and_issue3_resizes() {
        let profile = LocalProfile::new(test_child_path_for_smoke()).with_arguments([
            "emit:LINE-A",
            "emit:LINE-B",
            "emit:MARKER",
            "read-line",
            "echo:ECHO",
            "report-size",
            "exit:0",
        ]);

        let session = LocalPtySession::start(profile, TerminalSize::new(73, 26).unwrap())
            .expect("Unix PTY session starts with test child");

        let mut terminal =
            Terminal::new(Dimensions::new(73, 26).unwrap()).expect("terminal allocation");
        let mut controller = SessionController::for_test(session);

        pump_controlled_until(
            &mut controller,
            &mut terminal,
            |t| smoke_row_contains(t, "MARKER"),
            Duration::from_secs(5),
            "MARKER from test child",
        );
        assert_eq!(terminal.dimensions().columns(), 73);
        assert_eq!(terminal.dimensions().rows(), 26);

        for &(cols, rows) in &[(37u16, 13u16), (73, 26), (50, 18), (73, 26)] {
            let dims =
                Dimensions::new(cols as usize, rows as usize).expect("resize dimensions are valid");
            terminal.resize(dims).expect("terminal resize succeeds");
            controller.record_terminal_resize(dims);
            let settle = Instant::now() + Duration::from_millis(250);
            while Instant::now() < settle {
                controller.pump_events(&mut terminal);
                controller.forward_terminal_replies(&mut terminal);
                controller.flush_pending_writes();
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(terminal.dimensions().columns(), cols as usize);
            assert_eq!(terminal.dimensions().rows(), rows as usize);
        }

        controller.record_encoded_input(b"hello\r\n");

        pump_controlled_until(
            &mut controller,
            &mut terminal,
            |t| smoke_row_contains(t, "ECHO:hello"),
            Duration::from_secs(5),
            "ECHO:hello from test child after resize sequence",
        );

        pump_controlled_until(
            &mut controller,
            &mut terminal,
            |t| smoke_row_contains(t, "26 73"),
            Duration::from_secs(5),
            "report-size showing 26 73 at final 73×26 size",
        );

        wait_for_session_exit(&mut controller, &mut terminal, Duration::from_secs(5));
    }

    /// **Unix PTY — bounded shutdown terminates the process tree.**
    #[cfg(unix)]
    #[test]
    #[ignore = "native smoke — run via native-smoke.yml or with --include-ignored; not PR-blocking"]
    fn unix_pty_bounded_shutdown_terminates_process_tree() {
        let profile =
            LocalProfile::new(test_child_path_for_smoke()).with_arguments(["emit:RUNNING", "spin"]);

        let session = LocalPtySession::start(profile, TerminalSize::new(73, 26).unwrap())
            .expect("Unix PTY session starts");

        let mut terminal =
            Terminal::new(Dimensions::new(73, 26).unwrap()).expect("terminal allocation");
        let mut controller = SessionController::for_test(session);

        pump_controlled_until(
            &mut controller,
            &mut terminal,
            |t| smoke_row_contains(t, "RUNNING"),
            Duration::from_secs(5),
            "RUNNING marker from spinning test child",
        );

        let start = Instant::now();
        drop(controller);
        assert!(
            start.elapsed() <= Duration::from_secs(3),
            "bounded Unix PTY shutdown exceeded 3-second budget: {:?}",
            start.elapsed()
        );
    }
}
