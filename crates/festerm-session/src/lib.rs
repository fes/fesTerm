//! Runtime-independent session lifecycle and bounded transport contracts.
//!
//! A session backend owns process or connection I/O. It emits bytes and
//! lifecycle events only; the application owns terminal-core mutation.

use std::{
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

/// Maximum number of commands a backend accepts before reporting backpressure.
pub const DEFAULT_COMMAND_QUEUE_CAPACITY: usize = 64;
/// Maximum number of application events a backend retains before pausing reads.
pub const DEFAULT_EVENT_QUEUE_CAPACITY: usize = 128;
/// Maximum bytes in one input command or output event.
pub const MAX_IO_CHUNK_BYTES: usize = 64 * 1024;

/// Identifies one application-created session.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionId(u64);

impl SessionId {
    /// Allocates a process-local, monotonically increasing session identifier.
    pub fn next() -> Self {
        static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);
        Self(NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// A terminal size expressed in cells, with optional pixel dimensions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSize {
    columns: u16,
    rows: u16,
    pixel_width: Option<u16>,
    pixel_height: Option<u16>,
}

impl TerminalSize {
    pub fn new(columns: u16, rows: u16) -> Result<Self, TerminalSizeError> {
        if columns < 2 {
            return Err(TerminalSizeError::TooFewColumns { columns });
        }
        if rows == 0 {
            return Err(TerminalSizeError::ZeroRows);
        }
        Ok(Self {
            columns,
            rows,
            pixel_width: None,
            pixel_height: None,
        })
    }

    pub fn with_pixels(
        columns: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
    ) -> Result<Self, TerminalSizeError> {
        let mut size = Self::new(columns, rows)?;
        size.pixel_width = Some(pixel_width);
        size.pixel_height = Some(pixel_height);
        Ok(size)
    }

    pub const fn columns(self) -> u16 {
        self.columns
    }

    pub const fn rows(self) -> u16 {
        self.rows
    }

    pub const fn pixel_width(self) -> Option<u16> {
        self.pixel_width
    }

    pub const fn pixel_height(self) -> Option<u16> {
        self.pixel_height
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalSizeError {
    TooFewColumns { columns: u16 },
    ZeroRows,
}

impl fmt::Display for TerminalSizeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewColumns { columns } => {
                write!(
                    formatter,
                    "terminal requires at least 2 columns, received {columns}"
                )
            }
            Self::ZeroRows => formatter.write_str("terminal requires at least 1 row, received 0"),
        }
    }
}

impl std::error::Error for TerminalSizeError {}

/// A child or remote-process exit result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionExit {
    exit_code: u32,
    signal: Option<String>,
}

impl SessionExit {
    pub const fn with_exit_code(exit_code: u32) -> Self {
        Self {
            exit_code,
            signal: None,
        }
    }

    pub fn with_signal(exit_code: u32, signal: impl Into<String>) -> Self {
        Self {
            exit_code,
            signal: Some(signal.into()),
        }
    }

    pub const fn exit_code(&self) -> u32 {
        self.exit_code
    }

    pub fn signal(&self) -> Option<&str> {
        self.signal.as_deref()
    }

    pub const fn success(&self) -> bool {
        self.exit_code == 0 && self.signal.is_none()
    }
}

/// Stable category for a backend failure without retaining private terminal data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionErrorKind {
    Spawn,
    Input,
    Output,
    Resize,
    Shutdown,
    Unsupported,
    Internal,
    /// The remote host rejected the credentials offered for this session
    /// (wrong password, rejected key, etc). Distinguished from `Spawn` so a
    /// password-authenticated SSH session can reprompt for a fresh password
    /// in-tab instead of surfacing a raw failed session.
    Authentication,
}

/// A user-displayable, content-free backend error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionError {
    kind: SessionErrorKind,
    message: String,
}

impl SessionError {
    pub fn new(kind: SessionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub const fn kind(&self) -> SessionErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SessionError {}

/// The externally observable lifecycle of a session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionLifecycle {
    Starting,
    Running,
    Stopping,
    /// Transport was lost unintentionally and no durable-session recovery
    /// resumed it automatically (ADR 0018). This is deliberately distinct
    /// from `Failed`: it is not terminal, and an explicit, user-initiated
    /// reconnect remains available from this state.
    Disconnected(SessionError),
    Exited(SessionExit),
    Failed(SessionError),
    Stopped,
}

impl SessionLifecycle {
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Exited(_) | Self::Failed(_) | Self::Stopped)
    }
}

/// Identifies the direction that reached a bounded queue limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlowDirection {
    Input,
    Output,
    Resize,
    Control,
}

/// A content-free host-key verification request emitted by a remote session.
///
/// The public key itself remains in the SSH transport. The application may
/// display this identity and fingerprint, then resolve the request through the
/// transport-specific session API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostKeyPrompt {
    host: String,
    port: u16,
    sha256_fingerprint: String,
}

impl HostKeyPrompt {
    pub fn new(host: impl Into<String>, port: u16, sha256_fingerprint: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port,
            sha256_fingerprint: sha256_fingerprint.into(),
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub const fn port(&self) -> u16 {
        self.port
    }

    pub fn sha256_fingerprint(&self) -> &str {
        &self.sha256_fingerprint
    }
}

/// Events emitted by a session backend for application coordination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionEvent {
    Lifecycle(SessionLifecycle),
    Output(Vec<u8>),
    HostKeyVerification(HostKeyPrompt),
    ResizeApplied(TerminalSize),
    Backpressure {
        direction: FlowDirection,
        queued: usize,
        capacity: usize,
    },
    Error(SessionError),
}

/// Wakes an application's event loop after a session makes an event available.
///
/// Backends invoke this only after successfully enqueuing an event. The
/// application supplies an event-loop-safe implementation, such as egui's
/// repaint request, without coupling session I/O to a GUI toolkit.
pub trait SessionEventNotifier: Send + Sync {
    fn notify(&self);
}

/// The default notifier for callers without an event loop to wake.
#[derive(Default)]
pub struct NoopSessionEventNotifier;

impl SessionEventNotifier for NoopSessionEventNotifier {
    fn notify(&self) {}
}

/// Creates a notifier that intentionally performs no wake-up.
pub fn noop_session_event_notifier() -> Arc<dyn SessionEventNotifier> {
    Arc::new(NoopSessionEventNotifier)
}

/// Operation rejected by a bounded or closed session transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionOperation {
    Input,
    Resize,
    Shutdown,
}

/// A failed nonblocking request to a session backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionSendError {
    Closed {
        operation: SessionOperation,
    },
    Full {
        operation: SessionOperation,
        capacity: usize,
    },
    TooLarge {
        operation: SessionOperation,
        maximum: usize,
        actual: usize,
    },
}

impl fmt::Display for SessionSendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed { operation } => {
                write!(formatter, "{operation:?} rejected: session closed")
            }
            Self::Full {
                operation,
                capacity,
            } => write!(
                formatter,
                "{operation:?} rejected: bounded session queue is full ({capacity} entries)"
            ),
            Self::TooLarge {
                operation,
                maximum,
                actual,
            } => write!(
                formatter,
                "{operation:?} rejected: {actual} bytes exceeds the {maximum}-byte limit"
            ),
        }
    }
}

impl std::error::Error for SessionSendError {}

/// The result of a nonblocking event poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionTryReceiveError {
    Empty,
    Closed,
}

/// Content-free counters for diagnostics and flow-control visibility.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionMetrics {
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub resize_count: u64,
    pub error_count: u64,
    pub backpressure_count: u64,
    pub event_queue_depth: usize,
    pub event_queue_high_watermark: usize,
    pub event_queue_capacity: usize,
}

/// A bounded shutdown result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownResult {
    Stopped,
    AlreadyStopped,
}

/// Failure to complete shutdown within the requested bound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShutdownError {
    Request(SessionSendError),
    TimedOut { timeout: Duration },
    Failed(SessionError),
}

impl fmt::Display for ShutdownError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(error) => write!(formatter, "could not request shutdown: {error}"),
            Self::TimedOut { timeout } => {
                write!(
                    formatter,
                    "session shutdown exceeded {} ms",
                    timeout.as_millis()
                )
            }
            Self::Failed(error) => write!(formatter, "session shutdown failed: {error}"),
        }
    }
}

impl std::error::Error for ShutdownError {}

/// Runtime-independent session boundary used by local and future remote backends.
///
/// All calls are nonblocking except [`Self::shutdown`], whose caller chooses a
/// finite timeout. Events never mutate a terminal directly.
pub trait Session: Send + Sync {
    fn id(&self) -> SessionId;
    fn lifecycle(&self) -> SessionLifecycle;
    fn metrics(&self) -> SessionMetrics;
    fn try_send_input(&self, bytes: &[u8]) -> Result<(), SessionSendError>;
    fn try_resize(&self, size: TerminalSize) -> Result<(), SessionSendError>;
    fn try_shutdown(&self) -> Result<(), SessionSendError>;
    fn try_recv_event(&self) -> Result<SessionEvent, SessionTryReceiveError>;
    fn shutdown(&self, timeout: Duration) -> Result<ShutdownResult, ShutdownError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_size_requires_a_real_grid() {
        assert_eq!(
            TerminalSize::new(1, 24),
            Err(TerminalSizeError::TooFewColumns { columns: 1 })
        );
        assert_eq!(TerminalSize::new(80, 0), Err(TerminalSizeError::ZeroRows));
        assert_eq!(
            TerminalSize::with_pixels(80, 24, 800, 480)
                .unwrap()
                .columns(),
            80
        );
    }

    #[test]
    fn lifecycle_terminal_states_are_explicit() {
        assert!(!SessionLifecycle::Running.is_terminal());
        assert!(SessionLifecycle::Exited(SessionExit::with_exit_code(0)).is_terminal());
        assert!(SessionLifecycle::Stopped.is_terminal());
    }
}
