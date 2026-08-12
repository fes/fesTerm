use std::collections::VecDeque;

#[cfg(test)]
use std::time::Duration;

use festerm_core::{Dimensions, Terminal};
use festerm_session::{
    FlowDirection, HostKeyPrompt, Session, SessionError, SessionEvent, SessionLifecycle,
    SessionSendError, SessionTryReceiveError, TerminalSize, DEFAULT_COMMAND_QUEUE_CAPACITY,
    MAX_IO_CHUNK_BYTES,
};
use festerm_ui_egui::{EncodedInputSink, InputRoute, InputSinkDiagnostics};

#[cfg(test)]
use festerm_session::SessionMetrics;

/// Maximum session events drained per frame before yielding for repaint.
pub const MAX_SESSION_EVENTS_PER_FRAME: usize = 64;

/// Maximum bytes retained in the ordered pending-write buffer.
pub const MAX_PENDING_COMMAND_BYTES: usize = DEFAULT_COMMAND_QUEUE_CAPACITY * MAX_IO_CHUNK_BYTES;

/// The number of content-free resize observations retained for diagnostics.
const RESIZE_PROBE_HISTORY: usize = 16;

/// A content-free observation for one accepted PTY resize request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResizeProbeGeneration {
    /// Monotonically increasing local sequence number for an accepted resize.
    pub generation: u64,
    pub dimensions: TerminalSize,
    pub applied: bool,
    /// Output bytes delivered to the app before this resize was queued.
    pub output_bytes_at_request: u64,
    /// Output bytes delivered after this resize was queued and before a newer
    /// resize request superseded this observation.
    pub output_bytes_since_request: u64,
    /// Exact `CSI 6 n` cursor-position queries recognized after this request.
    pub cursor_position_queries_since_request: u64,
    /// Current visible nonblank-cell count, never the cells' text.
    pub visible_nonblank_cells: usize,
}

/// Bounded content-free diagnostics for output around PTY resizes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResizeProbe {
    next_generation: u64,
    output_bytes: u64,
    cursor_position_queries: u64,
    scanner: CursorPositionQueryScanner,
    generations: VecDeque<ResizeProbeGeneration>,
}

impl ResizeProbe {
    fn record_resize_request(&mut self, dimensions: TerminalSize) {
        if self.generations.len() == RESIZE_PROBE_HISTORY {
            self.generations.pop_front();
        }
        self.next_generation = self.next_generation.saturating_add(1);
        self.generations.push_back(ResizeProbeGeneration {
            generation: self.next_generation,
            dimensions,
            applied: false,
            output_bytes_at_request: self.output_bytes,
            output_bytes_since_request: 0,
            cursor_position_queries_since_request: 0,
            visible_nonblank_cells: 0,
        });
    }

    fn record_resize_applied(&mut self, size: TerminalSize) {
        if let Some(generation) = self
            .generations
            .iter_mut()
            .find(|generation| !generation.applied && generation.dimensions == size)
        {
            generation.applied = true;
        }
    }

    fn record_output(&mut self, bytes: &[u8]) {
        let byte_count = bytes.len() as u64;
        let cursor_position_queries = self.scanner.observe(bytes);
        self.output_bytes = self.output_bytes.saturating_add(byte_count);
        self.cursor_position_queries = self
            .cursor_position_queries
            .saturating_add(cursor_position_queries);
        if let Some(generation) = self.generations.back_mut() {
            generation.output_bytes_since_request = generation
                .output_bytes_since_request
                .saturating_add(byte_count);
            generation.cursor_position_queries_since_request = generation
                .cursor_position_queries_since_request
                .saturating_add(cursor_position_queries);
        }
    }

    fn record_visible_nonblank_cells(&mut self, terminal: &Terminal) {
        let count = (0..terminal.dimensions().rows())
            .flat_map(|row| {
                (0..terminal.dimensions().columns())
                    .filter_map(move |column| terminal.cell_ref(column, row))
            })
            .filter(|cell| cell.character() != ' ')
            .count();
        if let Some(generation) = self.generations.back_mut() {
            generation.visible_nonblank_cells = count;
        }
    }

    pub const fn observed_output_bytes(&self) -> u64 {
        self.output_bytes
    }

    pub const fn cursor_position_queries(&self) -> u64 {
        self.cursor_position_queries
    }

    pub const fn requested_generations(&self) -> u64 {
        self.next_generation
    }

    pub fn applied_generations(&self) -> u64 {
        self.generations
            .iter()
            .filter(|generation| generation.applied)
            .count() as u64
    }

    /// Copies only bounded numeric resize observations; no terminal text is
    /// retained or exposed by this diagnostic API.
    pub fn generations(&self) -> Vec<ResizeProbeGeneration> {
        self.generations.iter().copied().collect()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CursorPositionQueryScanner {
    #[default]
    Ground,
    Escape,
    CsiStart,
    CsiSix,
    CsiOther,
}

impl CursorPositionQueryScanner {
    fn observe(&mut self, bytes: &[u8]) -> u64 {
        let mut recognized = 0_u64;
        for &byte in bytes {
            *self = match (*self, byte) {
                (_, 0x1b) => Self::Escape,
                (Self::Ground, _) => Self::Ground,
                (Self::Escape, b'[') => Self::CsiStart,
                (Self::Escape, _) => Self::Ground,
                (Self::CsiStart, b'6') => Self::CsiSix,
                (Self::CsiStart, 0x40..=0x7e) => Self::Ground,
                (Self::CsiStart, _) => Self::CsiOther,
                (Self::CsiSix, b'n') => {
                    recognized = recognized.saturating_add(1);
                    Self::Ground
                }
                (Self::CsiSix, 0x40..=0x7e) => Self::Ground,
                (Self::CsiSix, _) => Self::CsiOther,
                (Self::CsiOther, 0x40..=0x7e) => Self::Ground,
                (Self::CsiOther, _) => Self::CsiOther,
            };
        }
        recognized
    }
}

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
    host_key_prompt: Option<HostKeyPrompt>,
    last_resize: Option<TerminalSize>,
    resize_probe: ResizeProbe,
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
            host_key_prompt: None,
            last_resize: None,
            resize_probe: ResizeProbe::default(),
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
            host_key_prompt: None,
            last_resize: None,
            resize_probe: ResizeProbe::default(),
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

    /// Returns the last observed session lifecycle, without terminal content.
    ///
    /// Used by chip-status presentation (`docs/gui-design.md` "Connection
    /// states"); it never exposes bytes or terminal text.
    pub fn lifecycle(&self) -> Option<SessionLifecycle> {
        self.last_lifecycle.clone()
    }

    /// Returns the current remote host-key decision request, if any.
    pub fn host_key_prompt(&self) -> Option<&HostKeyPrompt> {
        self.host_key_prompt.as_ref()
    }

    /// Returns whether another frame is required to continue a bounded drain.
    pub fn pump_events(&mut self, terminal: &mut Terminal) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        let mut observed = Vec::new();
        let resize_probe = &mut self.resize_probe;
        let result = pump_session_events(
            session,
            terminal,
            MAX_SESSION_EVENTS_PER_FRAME,
            |event| match event {
                PumpedSessionEvent::Output(bytes) => resize_probe.record_output(bytes),
                PumpedSessionEvent::Event(event) => observed.push(event.clone()),
            },
        );
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
                self.resize_probe.record_resize_applied(size);
            }
            SessionEvent::Backpressure { direction, .. } => {
                tracing::warn!(
                    target: "festerm::session",
                    ?direction,
                    "local session queue pressure"
                );
                self.last_backpressure = Some(direction);
            }
            SessionEvent::HostKeyVerification(prompt) => {
                self.host_key_prompt = Some(prompt);
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
            Ok(()) => self.resize_probe.record_resize_request(size),
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
        let resize_probe = self.resize_probe.generations.back().copied();
        let resize_probe = resize_probe
            .map(|generation| {
                format!(
                    "; resize probe g{} {}/{} B, cpr {}, cells {}",
                    generation.generation,
                    generation.output_bytes_since_request,
                    generation.output_bytes_at_request,
                    generation.cursor_position_queries_since_request,
                    generation.visible_nonblank_cells,
                )
            })
            .unwrap_or_default();
        format!(
            "Local shell {lifecycle:?}; in {} B, out {} B; events {}/{} (high {}); \
             pending writes {} entries, {} / {} B; pressure {}; errors {}; resizes {}\
             {latest_resize}{resize_probe}{queue_pressure}{latest_error}",
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

    /// Records a content-free visible-cell count for the most recently
    /// accepted resize. The count never retains or logs application text.
    pub fn observe_resize_probe_terminal_state(&mut self, terminal: &Terminal) {
        self.resize_probe.record_visible_nonblank_cells(terminal);
    }

    pub fn resize_probe(&self) -> &ResizeProbe {
        &self.resize_probe
    }

    /// Requests shutdown of the owned session without blocking the GUI thread.
    ///
    /// The session worker terminates the process tree and performs its bounded
    /// reader cleanup in the background. This is called for both tab removal
    /// and application teardown, where waiting would stall the native close
    /// animation.
    pub fn shutdown(&self) {
        if let Some(session) = &self.session {
            match session.try_shutdown() {
                Ok(()) => tracing::debug!(
                    target: "festerm::session",
                    "requested local session shutdown"
                ),
                Err(error) => tracing::error!(
                    target: "festerm::session",
                    %error,
                    "could not request local session shutdown"
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

/// A borrowed session event delivered while the terminal is pumped.
///
/// Output is borrowed so observers can collect content-free metrics without
/// cloning or retaining application bytes.
pub enum PumpedSessionEvent<'a> {
    Output(&'a [u8]),
    Event(&'a SessionEvent),
}

pub fn pump_session_events(
    session: &impl Session,
    terminal: &mut Terminal,
    maximum: usize,
    mut observe: impl FnMut(PumpedSessionEvent<'_>),
) -> PumpResult {
    for _ in 0..maximum {
        match session.try_recv_event() {
            Ok(event) => match &event {
                SessionEvent::Output(bytes) => {
                    observe(PumpedSessionEvent::Output(bytes));
                    terminal.ingest(bytes);
                }
                _ => observe(PumpedSessionEvent::Event(&event)),
            },
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
    fn resize_probe_recognizes_fragmented_cursor_queries_without_retaining_output() {
        let mut scanner = CursorPositionQueryScanner::default();
        assert_eq!(scanner.observe(b"\x1b["), 0);
        assert_eq!(scanner.observe(b"6n"), 1);
        assert_eq!(scanner.observe(b"\x1b[16n"), 0);
        assert_eq!(scanner.observe(b"\x1b[6n"), 1);
    }

    #[test]
    fn resize_probe_correlates_output_with_an_applied_generation() {
        let session = FakeSession::new([
            SessionEvent::Output(b"\x1b[".to_vec()),
            SessionEvent::Output(b"6nX".to_vec()),
            SessionEvent::ResizeApplied(TerminalSize::new(73, 26).unwrap()),
        ]);
        let mut controller = SessionController::for_test(session);
        let mut terminal =
            Terminal::new(Dimensions::new(73, 26).unwrap()).expect("terminal allocation");
        controller.record_terminal_resize(terminal.dimensions());
        assert!(!controller.pump_events(&mut terminal));
        controller.observe_resize_probe_terminal_state(&terminal);

        let generations = controller.resize_probe().generations();
        assert_eq!(controller.resize_probe().observed_output_bytes(), 5);
        assert_eq!(controller.resize_probe().cursor_position_queries(), 1);
        assert_eq!(generations.len(), 1);
        assert!(generations[0].applied);
        assert_eq!(generations[0].output_bytes_since_request, 5);
        assert_eq!(generations[0].cursor_position_queries_since_request, 1);
        assert_eq!(generations[0].visible_nonblank_cells, 1);
    }

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
    fn host_key_prompt_crosses_the_session_boundary_without_terminal_output() {
        let session = FakeSession::new([SessionEvent::HostKeyVerification(HostKeyPrompt::new(
            "ssh.example.test",
            2222,
            "SHA256:content-free-fingerprint",
        ))]);
        let mut controller = SessionController::for_test(session);
        let mut terminal =
            Terminal::new(Dimensions::new(80, 24).unwrap()).expect("terminal allocation");

        assert!(!controller.pump_events(&mut terminal));
        let prompt = controller
            .host_key_prompt()
            .expect("host-key prompt retained");
        assert_eq!(prompt.host(), "ssh.example.test");
        assert_eq!(prompt.port(), 2222);
        assert_eq!(
            prompt.sha256_fingerprint(),
            "SHA256:content-free-fingerprint"
        );
        assert!(terminal
            .row_text(0)
            .is_some_and(|row| row.trim().is_empty()));
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

    #[cfg(windows)]
    #[test]
    fn conpty_default_shell_keeps_the_prompt_column_across_a_command_newline() {
        use festerm_pty::LocalPtySession;

        let session = LocalPtySession::start_default(TerminalSize::new(87, 26).unwrap())
            .expect("default Windows shell starts");
        let mut controller = SessionController::with_session(session);
        let mut terminal =
            Terminal::new(Dimensions::new(87, 26).unwrap()).expect("terminal allocation");

        pump_controlled_until(
            &mut controller,
            &mut terminal,
            |terminal| {
                (0..terminal.dimensions().rows()).any(|row| {
                    terminal
                        .row_text(row)
                        .is_some_and(|text| text.starts_with("C:\\"))
                })
            },
            Duration::from_secs(3),
            "initial default-shell prompt",
        );

        let initial_cursor = terminal.cursor();
        controller.record_encoded_input(b"ver\r");
        controller.flush_pending_writes();
        pump_controlled_until(
            &mut controller,
            &mut terminal,
            |terminal| terminal.cursor().row() > initial_cursor.row(),
            Duration::from_secs(3),
            "default-shell command output",
        );

        let cursor = terminal.cursor();
        let prompt = terminal
            .row_text(cursor.row())
            .expect("visible cursor row exists");
        assert!(
            prompt.starts_with("C:\\"),
            "command output must return to a left-aligned cmd prompt, got {prompt:?}"
        );
        assert_eq!(
            cursor.column(),
            prompt.trim_end().chars().count(),
            "the cursor must follow the returned prompt rather than retaining a prior output column"
        );
        assert!(matches!(
            controller
                .session()
                .expect("session remains available")
                .shutdown(Duration::from_secs(2)),
            Ok(festerm_session::ShutdownResult::Stopped)
                | Ok(festerm_session::ShutdownResult::AlreadyStopped)
        ));
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

    /// Pumps a controlled session until a content-free controller observation
    /// satisfies `predicate`, without inspecting terminal text.
    #[cfg(any(unix, windows))]
    fn pump_content_free_until(
        controller: &mut SessionController<LocalPtySession>,
        terminal: &mut Terminal,
        predicate: impl Fn(&SessionController<LocalPtySession>) -> bool,
        timeout: Duration,
        context: &str,
    ) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            controller.pump_events(terminal);
            controller.observe_resize_probe_terminal_state(terminal);
            controller.forward_terminal_replies(terminal);
            controller.flush_pending_writes();
            if predicate(controller) {
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

    /// **Windows inbox ConPTY fallback smoke.**
    ///
    /// This deliberately runs before CI stages the optional verified sidecar.
    /// It establishes that the secure inbox fallback can start, resize, and
    /// resume output. The pinned sidecar smoke below is the Windows
    /// content-continuity acceptance test.
    #[cfg(windows)]
    #[test]
    #[ignore = "native smoke — run via native-smoke.yml before staging the pinned runtime"]
    fn windows_inbox_conpty_fallback_starts_resizes_and_resumes_output() {
        assert_eq!(
            festerm_pty::prepare_windows_conpty_runtime()
                .expect("secure inbox runtime selection succeeds"),
            festerm_pty::ConptyRuntimeSelection::Inbox,
            "this workflow step must run before the bundled runtime is staged"
        );

        let profile = LocalProfile::new(test_child_path_for_smoke()).with_arguments([
            "emit:READY",
            "read-line",
            "echo:ECHO",
            "exit:0",
        ]);
        let session = LocalPtySession::start(profile, TerminalSize::new(73, 26).unwrap())
            .expect("inbox ConPTY session starts");
        let mut terminal =
            Terminal::new(Dimensions::new(73, 26).unwrap()).expect("terminal allocation");
        let mut controller = SessionController::for_test(session);

        pump_content_free_until(
            &mut controller,
            &mut terminal,
            |controller| controller.resize_probe().observed_output_bytes() > 0,
            Duration::from_secs(5),
            "initial inbox ConPTY output",
        );
        let output_before_resize = controller.resize_probe().observed_output_bytes();
        terminal
            .resize(Dimensions::new(50, 18).unwrap())
            .expect("terminal resize succeeds");
        controller.record_terminal_resize(terminal.dimensions());
        pump_content_free_until(
            &mut controller,
            &mut terminal,
            |controller| {
                controller
                    .resize_probe()
                    .generations()
                    .last()
                    .is_some_and(|generation| generation.applied)
            },
            Duration::from_secs(5),
            "inbox ConPTY resize application",
        );
        let output_after_resize = controller.resize_probe().observed_output_bytes();
        assert!(output_after_resize >= output_before_resize);

        controller.record_encoded_input(b"resume\r\n");
        pump_content_free_until(
            &mut controller,
            &mut terminal,
            |controller| controller.resize_probe().observed_output_bytes() > output_after_resize,
            Duration::from_secs(5),
            "inbox ConPTY post-resize output",
        );
        wait_for_session_exit(&mut controller, &mut terminal, Duration::from_secs(5));
    }

    /// **Windows ConPTY smoke flow — issue #3 resize sequence.**
    ///
    /// Uses `festerm-pty-test-child` as the controlled shell.  The child emits
    /// output before `read-line` while we apply the four-step resize sequence
    /// (`37×13 → 73×26 → 50×18 → 73×26`), then verifies a content-free resize
    /// probe and output arriving after the final resize.
    ///
    /// Acceptance criterion: the sequence completes without a session error,
    /// every resize produces an applied generation with visible cells, and
    /// output bytes resume after the final resize.
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

        // Step 1: wait for repository-owned output without retaining its text.
        pump_content_free_until(
            &mut controller,
            &mut terminal,
            |controller| controller.resize_probe().observed_output_bytes() > 0,
            Duration::from_secs(5),
            "initial output from test child",
        );
        assert_eq!(terminal.dimensions().columns(), 73);
        assert_eq!(terminal.dimensions().rows(), 26);
        let output_before_resize = controller.resize_probe().observed_output_bytes();
        let first_resize_generation = controller
            .resize_probe()
            .requested_generations()
            .saturating_add(1);

        // Step 2: apply the issue #3 sequence while the child is blocked on
        // input. Only numeric probe observations are retained.
        for &(cols, rows) in &[(37u16, 13u16), (73, 26), (50, 18), (73, 26)] {
            let dims =
                Dimensions::new(cols as usize, rows as usize).expect("resize dimensions are valid");
            terminal.resize(dims).expect("terminal resize succeeds");
            controller.record_terminal_resize(dims);
            // Pump for ≥200 ms to give ConPTY time to apply the resize.
            let settle = Instant::now() + Duration::from_millis(250);
            while Instant::now() < settle {
                controller.pump_events(&mut terminal);
                controller.observe_resize_probe_terminal_state(&terminal);
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

        let generations = controller
            .resize_probe()
            .generations()
            .into_iter()
            .filter(|generation| generation.generation >= first_resize_generation)
            .collect::<Vec<_>>();
        assert_eq!(generations.len(), 4, "one probe generation per resize");
        assert!(
            generations.iter().all(|generation| generation.applied),
            "every resize must reach ConPTY"
        );
        assert!(
            generations
                .iter()
                .all(|generation| generation.visible_nonblank_cells > 0),
            "content-free visible-cell observations must remain nonzero"
        );
        assert!(
            controller.resize_probe().observed_output_bytes() >= output_before_resize,
            "output accounting must not regress across resizes"
        );
        let output_after_resize = controller.resize_probe().observed_output_bytes();

        // Step 3: send a line to unblock read-line.
        controller.record_encoded_input(b"hello\r\n");

        // Step 4: wait for post-resize bytes without matching application text.
        pump_content_free_until(
            &mut controller,
            &mut terminal,
            |controller| controller.resize_probe().observed_output_bytes() > output_after_resize,
            Duration::from_secs(5),
            "post-resize output from test child",
        );

        // Step 5: wait for exit and assert bounded shutdown.
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

    /// **P5 reference-application PTY probe.**
    ///
    /// This opt-in probe runs exactly one allowlisted locally installed
    /// application, applies two PTY resizes, and sends its fixed quit sequence.
    /// It deliberately retains only output counts and resize observations; the
    /// native-window smoke and headless UI tests own focus and key-routing
    /// coverage. Invoke it through `scripts/run-p5-reference.*`.
    #[cfg(any(unix, windows))]
    #[test]
    #[ignore = "P5 optional reference-application probe — run through scripts/run-p5-reference.*"]
    fn p5_reference_application_pty_probe() {
        let reference = std::env::var("FESTERM_P5_REFERENCE_APP")
            .expect("FESTERM_P5_REFERENCE_APP selects one allowlisted reference application");
        let mut cleanup_path = None;
        let (profile, quit_sequence) = match reference.as_str() {
            "less" => {
                let path = std::env::temp_dir()
                    .join(format!("festerm-p5-less-{}.txt", std::process::id()));
                std::fs::write(&path, "repository-owned P5 fixture\n".repeat(64))
                    .expect("P5 less fixture is writable");
                cleanup_path = Some(path.clone());
                (
                    LocalProfile::new("less").with_arguments([path]),
                    b"q".as_slice(),
                )
            }
            "nvim" => (
                LocalProfile::new("nvim").with_arguments(["--clean", "-u", "NONE", "-n"]),
                b"\x1b:qa!\r".as_slice(),
            ),
            "htop" => (LocalProfile::new("htop"), b"q".as_slice()),
            "tmux" => {
                let socket = format!("festerm-p5-{}", std::process::id());
                (
                    LocalProfile::new("tmux").with_arguments([
                        "-L",
                        socket.as_str(),
                        "-f",
                        "/dev/null",
                        "new-session",
                        "-s",
                        "p5",
                    ]),
                    b"\x02d".as_slice(),
                )
            }
            _ => panic!("unsupported P5 reference application selector: {reference}"),
        };

        let initial_size = TerminalSize::new(80, 24).expect("initial PTY size is valid");
        let session = LocalPtySession::start(profile, initial_size)
            .unwrap_or_else(|error| panic!("P5 {reference} session did not start: {error}"));
        let mut terminal =
            Terminal::new(Dimensions::new(80, 24).unwrap()).expect("terminal allocation");
        let mut controller = SessionController::for_test(session);

        pump_content_free_until(
            &mut controller,
            &mut terminal,
            |controller| controller.resize_probe().observed_output_bytes() > 0,
            Duration::from_secs(5),
            "initial reference-application output",
        );
        let output_before_resize = controller.resize_probe().observed_output_bytes();
        for dimensions in [
            Dimensions::new(100, 30).unwrap(),
            Dimensions::new(50, 18).unwrap(),
        ] {
            terminal
                .resize(dimensions)
                .expect("terminal resize succeeds");
            controller.record_terminal_resize(dimensions);
            pump_content_free_until(
                &mut controller,
                &mut terminal,
                |controller| {
                    controller
                        .resize_probe()
                        .generations()
                        .last()
                        .is_some_and(|generation| generation.applied)
                },
                Duration::from_secs(5),
                "reference-application PTY resize application",
            );
        }
        assert!(
            controller.resize_probe().observed_output_bytes() >= output_before_resize,
            "reference application output count must not regress across resize"
        );

        controller.record_encoded_input(quit_sequence);
        wait_for_session_exit(&mut controller, &mut terminal, Duration::from_secs(5));
        if let Some(path) = cleanup_path {
            std::fs::remove_file(path).expect("P5 less fixture is removable");
        }
        #[cfg(unix)]
        if reference == "tmux" {
            let socket = format!("festerm-p5-{}", std::process::id());
            let _ = std::process::Command::new("tmux")
                .args(["-L", socket.as_str(), "kill-server"])
                .status();
        }
    }
}
