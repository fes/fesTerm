//! Client backend for fesTerm's native local session persistence daemon.

use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    fs::OpenOptions,
    io::{self, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

use festerm_pty::{EnvironmentPolicy, LocalProfile};
use festerm_session::{
    noop_session_event_notifier, Session, SessionError, SessionErrorKind, SessionEvent,
    SessionEventNotifier, SessionExit, SessionId, SessionLifecycle, SessionMetrics,
    SessionOperation, SessionSendError, SessionTryReceiveError, ShutdownError, ShutdownResult,
    TerminalSize, DEFAULT_COMMAND_QUEUE_CAPACITY, DEFAULT_EVENT_QUEUE_CAPACITY, MAX_IO_CHUNK_BYTES,
};
use festerm_ssh::PersistentSessionName;
use fs2::FileExt;
use serde::Deserialize;

const POLL_INTERVAL: Duration = Duration::from_millis(20);
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(windows)]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const FRAME_MAGIC: &[u8; 4] = b"FSD1";
const FRAME_INPUT: u8 = 1;
const FRAME_RESIZE: u8 = 2;
const MAX_FRAME_BYTES: usize = 64 * 1024;
const STOLEN_NOTICE_BYTES: &[u8] =
    b"\n[festerm-sessiond] SESSION_STOLEN: reattached from another client\n";
const EXITED_NOTICE_BYTES: &[u8] = b"\n[festerm-sessiond] SESSION_EXITED\n";

trait SessionStream: Read + Write + Send {}
impl<T: Read + Write + Send> SessionStream for T {}

type Reconnector =
    dyn Fn(&AtomicBool) -> Result<Box<dyn SessionStream>, PersistentSessionError> + Send + Sync;

#[derive(Clone, Debug, Deserialize)]
struct SessionRecord {
    #[serde(default)]
    name: String,
    pid: u32,
    socket: String,
    #[serde(default)]
    shell: String,
    #[serde(default)]
    arguments: Vec<String>,
    #[serde(default)]
    working_directory: Option<String>,
    #[serde(default)]
    created_at_unix_ms: u128,
    #[serde(default)]
    attached: bool,
}

#[derive(Default, Deserialize)]
struct SessionRegistry {
    #[serde(default)]
    sessions: BTreeMap<String, SessionRecord>,
}

/// A locally running `festerm-sessiond` session with no attached client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnattachedSession {
    pub name: String,
    pub shell: String,
    pub arguments: Vec<String>,
    pub working_directory: Option<String>,
    pub created_at_unix_ms: u128,
}

/// Enumerates locally registered `festerm-sessiond` sessions that are alive
/// but currently have no attached client, suitable for surfacing as
/// one-click "resume" entries on the New Session/Launcher screen.
///
/// Returns an empty list (rather than an error) if the daemon's registry is
/// unavailable, absent, or otherwise unreadable, since the Launcher should
/// behave exactly as it does today when `festerm-sessiond` is disabled or
/// not present.
pub fn list_unattached_local_sessions() -> Vec<UnattachedSession> {
    let Ok(registry) = load_registry() else {
        return Vec::new();
    };
    registry
        .sessions
        .into_values()
        .filter(|record| !record.attached && process_alive(record.pid))
        .map(|record| UnattachedSession {
            name: record.name,
            shell: record.shell,
            arguments: record.arguments,
            working_directory: record.working_directory,
            created_at_unix_ms: record.created_at_unix_ms,
        })
        .collect()
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    match kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(Errno::EPERM) => true,
        Err(_) => false,
    }
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    festerm_windows_job::process_is_alive(pid)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistentSessionError {
    message: String,
}

impl PersistentSessionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for PersistentSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PersistentSessionError {}

enum SessionCommand {
    Input(Vec<u8>),
    Resize(TerminalSize),
    Shutdown,
}

struct Shared {
    id: SessionId,
    lifecycle: Mutex<SessionLifecycle>,
    metrics: Mutex<SessionMetrics>,
    events: SyncSender<SessionEvent>,
    notifier: Arc<dyn SessionEventNotifier>,
    cancelled: AtomicBool,
    reconnecting: AtomicBool,
    completion: Mutex<Option<ShutdownResult>>,
    completion_receiver: Mutex<Receiver<ShutdownResult>>,
}

impl Shared {
    fn lifecycle(&self) -> SessionLifecycle {
        self.lifecycle
            .lock()
            .expect("persistent session lifecycle lock is not poisoned")
            .clone()
    }

    fn set_lifecycle(&self, lifecycle: SessionLifecycle) {
        *self
            .lifecycle
            .lock()
            .expect("persistent session lifecycle lock is not poisoned") = lifecycle.clone();
        self.send_event(SessionEvent::Lifecycle(lifecycle));
    }

    fn send_event(&self, event: SessionEvent) {
        let mut event = event;
        loop {
            match self.try_send_event(event) {
                Ok(()) => return,
                Err(rejected) => {
                    if self.cancelled.load(Ordering::Acquire) {
                        return;
                    }
                    event = rejected;
                    thread::sleep(POLL_INTERVAL);
                }
            }
        }
    }

    /// Offers an event to the application without waiting for room, handing
    /// the event back when the queue is full.
    ///
    /// The client worker is the only thread that can write input to the
    /// daemon, so it must never park waiting for the GUI to drain output: the
    /// interrupt the user is trying to send is queued behind that wait. Giving
    /// the event back lets the worker hold it, keep servicing commands, and
    /// retry on the next pass.
    fn try_send_event(&self, event: SessionEvent) -> Result<(), SessionEvent> {
        match self.events.try_send(event) {
            Ok(()) => {
                let mut metrics = self
                    .metrics
                    .lock()
                    .expect("persistent session metrics lock is not poisoned");
                metrics.event_queue_depth += 1;
                metrics.event_queue_high_watermark = metrics
                    .event_queue_high_watermark
                    .max(metrics.event_queue_depth);
                drop(metrics);
                self.notifier.notify();
                Ok(())
            }
            Err(TrySendError::Full(event)) => Err(event),
            Err(TrySendError::Disconnected(_)) => Ok(()),
        }
    }

    fn record_error(&self, error: SessionError) {
        self.metrics
            .lock()
            .expect("persistent session metrics lock is not poisoned")
            .error_count += 1;
        self.send_event(SessionEvent::Error(error));
    }
}

/// A bounded `festerm-session` backend attached to one daemon-owned local PTY.
pub struct PersistentSession {
    shared: Arc<Shared>,
    worker: Mutex<ClientWorker>,
    events: Mutex<Receiver<SessionEvent>>,
    reconnector: Option<Arc<Reconnector>>,
}

struct ClientWorker {
    commands: SyncSender<SessionCommand>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for PersistentSession {
    fn drop(&mut self) {
        self.shared.cancelled.store(true, Ordering::Release);
    }
}

impl PersistentSession {
    pub fn start(
        name: &str,
        profile: &LocalProfile,
        size: TerminalSize,
    ) -> Result<Self, PersistentSessionError> {
        Self::start_with_notifier(name, profile, size, noop_session_event_notifier())
    }

    pub fn start_with_notifier(
        name: &str,
        profile: &LocalProfile,
        size: TerminalSize,
        notifier: Arc<dyn SessionEventNotifier>,
    ) -> Result<Self, PersistentSessionError> {
        let name = PersistentSessionName::new(name.to_owned())
            .map_err(|error| PersistentSessionError::new(error.to_string()))?;
        profile
            .validate()
            .map_err(|error| PersistentSessionError::new(error.to_string()))?;

        let stream = connect_or_start(name.as_str(), profile, size)?;
        Self::from_named_stream(stream, notifier, name.as_str().to_owned())
    }

    /// Attaches to an already-running, unattached `festerm-sessiond` session
    /// by name, without spawning a new session if one isn't already
    /// registered. Intended for resuming a session surfaced via
    /// [`list_unattached_local_sessions`] rather than starting one from a
    /// saved profile.
    pub fn resume(name: &str) -> Result<Self, PersistentSessionError> {
        Self::resume_with_notifier(name, noop_session_event_notifier())
    }

    /// Like [`PersistentSession::resume`], but delivers session events
    /// through the given notifier.
    pub fn resume_with_notifier(
        name: &str,
        notifier: Arc<dyn SessionEventNotifier>,
    ) -> Result<Self, PersistentSessionError> {
        let name = PersistentSessionName::new(name.to_owned())
            .map_err(|error| PersistentSessionError::new(error.to_string()))?;
        let stream = connect_existing(name.as_str())?;
        Self::from_named_stream(stream, notifier, name.as_str().to_owned())
    }

    fn from_named_stream(
        stream: Box<dyn SessionStream>,
        notifier: Arc<dyn SessionEventNotifier>,
        name: String,
    ) -> Result<Self, PersistentSessionError> {
        Self::from_stream_with_reconnector(
            stream,
            notifier,
            Some(Arc::new(move |cancelled| {
                connect_existing_with_cancel(&name, cancelled)
            })),
        )
    }

    #[cfg(test)]
    fn from_stream(
        stream: Box<dyn SessionStream>,
        notifier: Arc<dyn SessionEventNotifier>,
    ) -> Result<Self, PersistentSessionError> {
        Self::from_stream_with_reconnector(stream, notifier, None)
    }

    fn from_stream_with_reconnector(
        stream: Box<dyn SessionStream>,
        notifier: Arc<dyn SessionEventNotifier>,
        reconnector: Option<Arc<Reconnector>>,
    ) -> Result<Self, PersistentSessionError> {
        let (events_tx, events_rx) = mpsc::sync_channel(DEFAULT_EVENT_QUEUE_CAPACITY);
        let (commands_tx, commands_rx) = mpsc::sync_channel(DEFAULT_COMMAND_QUEUE_CAPACITY);
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            id: SessionId::next(),
            lifecycle: Mutex::new(SessionLifecycle::Starting),
            metrics: Mutex::new(SessionMetrics {
                event_queue_capacity: DEFAULT_EVENT_QUEUE_CAPACITY,
                ..SessionMetrics::default()
            }),
            events: events_tx,
            notifier,
            cancelled: AtomicBool::new(false),
            reconnecting: AtomicBool::new(false),
            completion: Mutex::new(None),
            completion_receiver: Mutex::new(completion_rx),
        });
        shared.set_lifecycle(SessionLifecycle::Starting);

        let worker_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name(format!("festerm-sessiond-client-{}", shared.id))
            .spawn(move || client_worker(worker_shared, stream, commands_rx, completion_tx))
            .map_err(|error| {
                PersistentSessionError::new(format!(
                    "could not start persistent-session worker: {error}"
                ))
            })?;

        Ok(Self {
            shared,
            worker: Mutex::new(ClientWorker {
                commands: commands_tx,
                thread: Some(thread),
            }),
            events: Mutex::new(events_rx),
            reconnector,
        })
    }

    /// Whether a manual, resume-only reconnect can currently be requested.
    pub fn reconnect_available(&self) -> bool {
        self.reconnector.is_some()
            && !self.shared.cancelled.load(Ordering::Acquire)
            && !self.shared.reconnecting.load(Ordering::Acquire)
            && matches!(
                self.lifecycle(),
                SessionLifecycle::Disconnected(_) | SessionLifecycle::Failed(_)
            )
    }

    /// Starts an asynchronous attachment to the existing named daemon.
    ///
    /// This never starts a shell. `Ok(())` acknowledges the request; connection
    /// failures arrive through the normal error/lifecycle events. Commands from
    /// the previous transport are discarded, and input is rejected while the
    /// connection is being established.
    pub fn try_reconnect(&self) -> Result<(), PersistentSessionError> {
        let mut worker = self
            .worker
            .lock()
            .expect("persistent worker lock is healthy");
        if !self.reconnect_available() {
            return Err(PersistentSessionError::new(
                "persistent session is not available for reconnect",
            ));
        }
        let reconnector = Arc::clone(self.reconnector.as_ref().expect("named session"));
        self.shared.reconnecting.store(true, Ordering::Release);
        *self
            .shared
            .lifecycle
            .lock()
            .expect("healthy lifecycle lock") = SessionLifecycle::Starting;
        self.shared.notifier.notify();

        let previous = Arc::new(Mutex::new(worker.thread.take()));
        let previous_worker = Arc::clone(&previous);
        let (commands, receiver) = mpsc::sync_channel(DEFAULT_COMMAND_QUEUE_CAPACITY);
        let (completion, completion_receiver) = mpsc::sync_channel(1);
        let failure_completion = completion.clone();
        worker.commands = commands;
        *self
            .shared
            .completion
            .lock()
            .expect("healthy completion lock") = None;
        *self
            .shared
            .completion_receiver
            .lock()
            .expect("healthy completion receiver lock") = completion_receiver;
        let shared = Arc::clone(&self.shared);
        match thread::Builder::new()
            .name(format!("festerm-sessiond-reconnect-{}", shared.id))
            .spawn(move || {
                let previous = previous_worker
                    .lock()
                    .expect("healthy previous worker lock")
                    .take();
                if let Some(previous) = previous {
                    if previous.join().is_err() {
                        reconnect_failed(&shared, "previous session worker panicked");
                        let _ = completion.send(ShutdownResult::AlreadyStopped);
                        return;
                    }
                }
                if shared.cancelled.load(Ordering::Acquire) {
                    finish_cancelled_reconnect(&shared, &completion);
                    return;
                }
                shared.set_lifecycle(SessionLifecycle::Starting);
                let connection = reconnector(&shared.cancelled);
                if shared.cancelled.load(Ordering::Acquire) {
                    finish_cancelled_reconnect(&shared, &completion);
                    return;
                }
                match connection {
                    Ok(stream) => {
                        shared.reconnecting.store(false, Ordering::Release);
                        client_worker(shared, stream, receiver, completion);
                    }
                    Err(error) => {
                        reconnect_failed(&shared, error.to_string());
                        let _ = completion.send(ShutdownResult::AlreadyStopped);
                    }
                }
            }) {
            Ok(thread) => {
                worker.thread = Some(thread);
                Ok(())
            }
            Err(error) => {
                worker.thread = previous
                    .lock()
                    .expect("healthy previous worker lock")
                    .take();
                self.shared.reconnecting.store(false, Ordering::Release);
                let error = PersistentSessionError::new(format!(
                    "could not start persistent-session reconnect worker: {error}"
                ));
                *self
                    .shared
                    .lifecycle
                    .lock()
                    .expect("healthy lifecycle lock") = SessionLifecycle::Failed(
                    SessionError::new(SessionErrorKind::Output, error.to_string()),
                );
                let _ = failure_completion.try_send(ShutdownResult::AlreadyStopped);
                self.shared.notifier.notify();
                Err(error)
            }
        }
    }

    fn send(
        &self,
        command: SessionCommand,
        operation: SessionOperation,
    ) -> Result<(), SessionSendError> {
        let worker = self
            .worker
            .lock()
            .expect("persistent worker lock is healthy");
        if operation != SessionOperation::Shutdown
            && (self.shared.cancelled.load(Ordering::Acquire)
                || self.shared.reconnecting.load(Ordering::Acquire)
                || !matches!(
                    self.lifecycle(),
                    SessionLifecycle::Starting | SessionLifecycle::Running
                ))
        {
            return Err(SessionSendError::Closed { operation });
        }
        send_command(&worker.commands, command, operation)
    }
}

fn reconnect_failed(shared: &Shared, message: impl Into<String>) {
    let error = SessionError::new(SessionErrorKind::Output, message.into());
    shared.record_error(error.clone());
    shared.set_lifecycle(SessionLifecycle::Disconnected(error));
    shared.reconnecting.store(false, Ordering::Release);
    shared.notifier.notify();
}

fn finish_cancelled_reconnect(shared: &Shared, completion: &SyncSender<ShutdownResult>) {
    shared.reconnecting.store(false, Ordering::Release);
    shared.set_lifecycle(SessionLifecycle::Stopped);
    let _ = completion.send(ShutdownResult::Stopped);
}

fn connect_existing(name: &str) -> Result<Box<dyn SessionStream>, PersistentSessionError> {
    connect_existing_with_cancel(name, &AtomicBool::new(false))
}

fn connect_existing_with_cancel(
    name: &str,
    cancelled: &AtomicBool,
) -> Result<Box<dyn SessionStream>, PersistentSessionError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(PersistentSessionError::new("session connection cancelled"));
    }
    let registry = load_registry()?;
    connect_existing_in_registry_with_cancel(&registry, name, cancelled)
}

#[cfg(test)]
fn connect_existing_in_registry(
    registry: &SessionRegistry,
    name: &str,
) -> Result<Box<dyn SessionStream>, PersistentSessionError> {
    connect_existing_in_registry_with_cancel(registry, name, &AtomicBool::new(false))
}

fn connect_existing_in_registry_with_cancel(
    registry: &SessionRegistry,
    name: &str,
    cancelled: &AtomicBool,
) -> Result<Box<dyn SessionStream>, PersistentSessionError> {
    let record = registry.sessions.get(name).ok_or_else(|| {
        PersistentSessionError::new(format!(
            "no locally running session named '{name}' is registered"
        ))
    })?;
    connect_record_with_cancel(record, cancelled).map_err(|error| {
        PersistentSessionError::new(format!(
            "session '{name}' is registered to process {} but is not accepting connections \
             ({error}); run `festerm-sessiond kill --name {name}` to clear it",
            record.pid,
        ))
    })
}

impl Session for PersistentSession {
    fn id(&self) -> SessionId {
        self.shared.id
    }

    fn lifecycle(&self) -> SessionLifecycle {
        self.shared.lifecycle()
    }

    fn metrics(&self) -> SessionMetrics {
        *self
            .shared
            .metrics
            .lock()
            .expect("persistent session metrics lock is not poisoned")
    }

    fn try_send_input(&self, bytes: &[u8]) -> Result<(), SessionSendError> {
        if bytes.len() > MAX_IO_CHUNK_BYTES {
            return Err(SessionSendError::TooLarge {
                operation: SessionOperation::Input,
                maximum: MAX_IO_CHUNK_BYTES,
                actual: bytes.len(),
            });
        }
        self.send(
            SessionCommand::Input(bytes.to_vec()),
            SessionOperation::Input,
        )
    }

    fn try_resize(&self, size: TerminalSize) -> Result<(), SessionSendError> {
        self.send(SessionCommand::Resize(size), SessionOperation::Resize)
    }

    fn try_shutdown(&self) -> Result<(), SessionSendError> {
        self.shared.cancelled.store(true, Ordering::Release);
        match self.send(SessionCommand::Shutdown, SessionOperation::Shutdown) {
            // Cancellation is out of band, so a saturated command queue
            // cannot prevent the worker from observing shutdown.
            Err(SessionSendError::Full { .. }) => Ok(()),
            result => result,
        }
    }

    fn try_recv_event(&self) -> Result<SessionEvent, SessionTryReceiveError> {
        match self
            .events
            .lock()
            .expect("persistent session event receiver lock is not poisoned")
            .try_recv()
        {
            Ok(event) => {
                let mut metrics = self
                    .shared
                    .metrics
                    .lock()
                    .expect("persistent session metrics lock is not poisoned");
                metrics.event_queue_depth = metrics.event_queue_depth.saturating_sub(1);
                Ok(event)
            }
            Err(TryRecvError::Empty) => Err(SessionTryReceiveError::Empty),
            Err(TryRecvError::Disconnected) => Err(SessionTryReceiveError::Closed),
        }
    }

    fn shutdown(&self, timeout: Duration) -> Result<ShutdownResult, ShutdownError> {
        if let Some(result) = *self
            .shared
            .completion
            .lock()
            .expect("persistent session completion lock is not poisoned")
        {
            return Ok(result);
        }
        match self.try_shutdown() {
            Ok(()) | Err(SessionSendError::Closed { .. }) => {}
            Err(error) => return Err(ShutdownError::Request(error)),
        }
        let result = self
            .shared
            .completion_receiver
            .lock()
            .expect("persistent session completion receiver lock is not poisoned")
            .recv_timeout(timeout)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => ShutdownError::TimedOut { timeout },
                RecvTimeoutError::Disconnected => ShutdownError::Failed(SessionError::new(
                    SessionErrorKind::Shutdown,
                    "persistent-session worker closed before shutdown completed",
                )),
            })?;
        *self
            .shared
            .completion
            .lock()
            .expect("persistent session completion lock is not poisoned") = Some(result);
        Ok(result)
    }
}

fn send_command(
    sender: &SyncSender<SessionCommand>,
    command: SessionCommand,
    operation: SessionOperation,
) -> Result<(), SessionSendError> {
    match sender.try_send(command) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => Err(SessionSendError::Full {
            operation,
            capacity: DEFAULT_COMMAND_QUEUE_CAPACITY,
        }),
        Err(TrySendError::Disconnected(_)) => Err(SessionSendError::Closed { operation }),
    }
}

/// One protocol frame waiting for room on the transport, and what the
/// application should be told once the daemon has all of it.
struct OutboundFrame {
    bytes: Vec<u8>,
    written: usize,
    input_bytes: usize,
    resize_applied: Option<TerminalSize>,
}

impl OutboundFrame {
    fn remaining(&self) -> &[u8] {
        &self.bytes[self.written..]
    }
}

/// Frames the client still owes the daemon, in the order they were requested.
///
/// Input is queued rather than written where it is requested so that a daemon
/// which is momentarily refusing bytes cannot stop this client reading. Both
/// ends of this transport write to each other, and both apply backpressure by
/// simply not reading, so a client that blocks inside a write until it
/// succeeds can deadlock against a daemon doing the same thing. Making partial
/// progress and coming back next pass is what breaks that cycle.
/// At most one command-channel's worth of frames is staged here; later
/// commands stay in that bounded channel until the transport makes room.
#[derive(Default)]
struct OutboundFrames {
    frames: VecDeque<OutboundFrame>,
}

impl OutboundFrames {
    fn is_full(&self) -> bool {
        self.frames.len() >= DEFAULT_COMMAND_QUEUE_CAPACITY
    }

    fn push_input(&mut self, bytes: &[u8]) {
        debug_assert!(!self.is_full());
        let input_bytes = bytes.len();
        self.frames.push_back(OutboundFrame {
            bytes: encode_frame(FRAME_INPUT, bytes),
            written: 0,
            input_bytes,
            resize_applied: None,
        });
    }

    fn push_resize(&mut self, size: TerminalSize) {
        debug_assert!(!self.is_full());
        self.frames.push_back(OutboundFrame {
            bytes: encode_frame(FRAME_RESIZE, &encode_resize(size)),
            written: 0,
            input_bytes: 0,
            resize_applied: Some(size),
        });
    }

    fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Writes as much as the transport will accept right now, recording the
    /// offset reached so a frame is never partially rewritten.
    ///
    /// The stream carries a write timeout so a wedged daemon cannot pin this
    /// thread forever, but a timeout is backpressure, not a failure: the
    /// daemon is busy, usually because it is trying to hand us output we have
    /// not read yet. Treating it as a transport failure used to end this
    /// worker, which dropped the command channel and left the session unable
    /// to accept another keystroke for the rest of its life.
    fn pump<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        shared: &Shared,
        events: &mut VecDeque<SessionEvent>,
    ) -> io::Result<()> {
        let mut budget = MAX_IO_CHUNK_BYTES;
        while let Some(frame) = self.frames.front_mut() {
            if frame.resize_applied.is_some() && events.len() >= DEFAULT_EVENT_QUEUE_CAPACITY {
                return Ok(());
            }
            while frame.written < frame.bytes.len() {
                if budget == 0 || shared.cancelled.load(Ordering::Acquire) {
                    return Ok(());
                }
                let count = budget.min(frame.remaining().len());
                match writer.write(&frame.remaining()[..count]) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "persistent-session daemon accepted no bytes",
                        ))
                    }
                    Ok(count) => {
                        frame.written += count;
                        budget -= count;
                    }
                    Err(error) if is_retryable_write(&error) => return Ok(()),
                    Err(error) => return Err(error),
                }
            }
            // Windows named-pipe `flush` maps to `FlushFileBuffers`, which
            // blocks until the peer has drained everything written - exactly
            // the wait this loop exists to avoid. The pipe delivers without
            // it; only the Unix socket needs the nudge.
            #[cfg(not(windows))]
            match writer.flush() {
                Ok(()) => {}
                Err(error) if is_retryable_write(&error) => {}
                Err(error) => return Err(error),
            }
            let frame = self
                .frames
                .pop_front()
                .expect("the front frame stays queued until it is fully written");
            if 0 < frame.input_bytes {
                shared
                    .metrics
                    .lock()
                    .expect("persistent session metrics lock is not poisoned")
                    .input_bytes += frame.input_bytes as u64;
            }
            if let Some(size) = frame.resize_applied {
                shared
                    .metrics
                    .lock()
                    .expect("persistent session metrics lock is not poisoned")
                    .resize_count += 1;
                events.push_back(SessionEvent::ResizeApplied(size));
            }
        }
        Ok(())
    }
}

fn is_retryable_write(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn client_worker(
    shared: Arc<Shared>,
    mut stream: Box<dyn SessionStream>,
    commands: Receiver<SessionCommand>,
    completion: SyncSender<ShutdownResult>,
) {
    shared.set_lifecycle(SessionLifecycle::Running);
    let mut scanner = OutputScanner::default();
    let mut buffer = [0u8; 4096];
    let mut outbound = OutboundFrames::default();
    // Events the application has not taken yet. Holding them here rather than
    // blocking inside the send is what keeps input flowing while the GUI is
    // behind. Resize acknowledgements also occupy this bounded queue; once
    // full, later commands wait in order rather than building a hidden backlog.
    let mut pending_events: VecDeque<SessionEvent> = VecDeque::new();
    loop {
        if shared.cancelled.load(Ordering::Acquire) {
            shared.set_lifecycle(SessionLifecycle::Stopped);
            let _ = completion.send(ShutdownResult::Stopped);
            return;
        }
        while !outbound.is_full() {
            match commands.try_recv() {
                Ok(SessionCommand::Input(bytes)) => outbound.push_input(&bytes),
                Ok(SessionCommand::Resize(size)) => outbound.push_resize(size),
                Ok(SessionCommand::Shutdown) => {
                    shared.set_lifecycle(SessionLifecycle::Stopped);
                    let _ = completion.send(ShutdownResult::Stopped);
                    return;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if let Err(error) = outbound.pump(&mut stream, &shared, &mut pending_events) {
            fail_transport(&shared, SessionErrorKind::Input, error);
            let _ = completion.send(ShutdownResult::Stopped);
            return;
        }

        while let Some(event) = pending_events.pop_front() {
            if let Err(rejected) = shared.try_send_event(event) {
                pending_events.push_front(rejected);
                break;
            }
        }

        // Output the application has not taken yet must reach it before any
        // more is read. Input can still advance above while ordinary output
        // waits, but a full resize-acknowledgement backlog also stalls later
        // commands to preserve their order without unbounded buffering.
        if !pending_events.is_empty() {
            thread::sleep(POLL_INTERVAL);
            continue;
        }

        match stream.read(&mut buffer) {
            Ok(0) => {
                if let Some(output) = scanner.close() {
                    send_output(&shared, output);
                }
                shared.set_lifecycle(SessionLifecycle::Disconnected(SessionError::new(
                    SessionErrorKind::Output,
                    "persistent-session daemon closed unexpectedly",
                )));
                let _ = completion.send(ShutdownResult::AlreadyStopped);
                return;
            }
            Ok(count) => match scanner.push(&buffer[..count]) {
                ScanResult::Output(output) => {
                    pending_events.push_back(count_output(&shared, output))
                }
                ScanResult::Pending => {}
                ScanResult::Stolen(output) => {
                    if !output.is_empty() {
                        send_output(&shared, output);
                    }
                    shared.set_lifecycle(SessionLifecycle::Disconnected(SessionError::new(
                        SessionErrorKind::Output,
                        "persistent session was attached by another client",
                    )));
                    let _ = completion.send(ShutdownResult::AlreadyStopped);
                    return;
                }
                ScanResult::Exited(output) => {
                    if !output.is_empty() {
                        send_output(&shared, output);
                    }
                    shared.set_lifecycle(SessionLifecycle::Exited(SessionExit::with_exit_code(0)));
                    let _ = completion.send(ShutdownResult::AlreadyStopped);
                    return;
                }
            },
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                // A read timeout is the idle case, but the daemon may still be
                // refusing input, so keep the loop hot until it is all gone.
                if outbound.is_empty() {
                    continue;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                shared.set_lifecycle(SessionLifecycle::Disconnected(SessionError::new(
                    SessionErrorKind::Output,
                    "persistent-session daemon closed unexpectedly",
                )));
                let _ = completion.send(ShutdownResult::AlreadyStopped);
                return;
            }
            Err(error) => {
                fail_transport(&shared, SessionErrorKind::Output, error);
                let _ = completion.send(ShutdownResult::AlreadyStopped);
                return;
            }
        }
    }
}

fn fail_transport(shared: &Shared, kind: SessionErrorKind, error: io::Error) {
    let error = SessionError::new(
        kind,
        format!("persistent-session transport failed: {error}"),
    );
    shared.record_error(error.clone());
    shared.set_lifecycle(SessionLifecycle::Disconnected(error));
}

fn send_output(shared: &Shared, output: Vec<u8>) {
    shared
        .metrics
        .lock()
        .expect("persistent session metrics lock is not poisoned")
        .output_bytes += output.len() as u64;
    shared.send_event(SessionEvent::Output(output));
}

/// Accounts for output the worker is about to hand the application, returning
/// the event to deliver.
///
/// Counting here rather than at delivery keeps the metric honest about what
/// the daemon actually sent us even while the application is behind and the
/// event is still queued.
fn count_output(shared: &Shared, output: Vec<u8>) -> SessionEvent {
    shared
        .metrics
        .lock()
        .expect("persistent session metrics lock is not poisoned")
        .output_bytes += output.len() as u64;
    SessionEvent::Output(output)
}

/// Environment variables the macOS launchd-environment correction
/// (`app/festerm/src/environment.rs::with_corrected_local_path`) is allowed
/// to backfill on a profile before it reaches a persistent session: `PATH`
/// plus the locale variables `LANG`, `LC_ALL`, and `LC_CTYPE`. These are the
/// only variables that correction ever adds, they carry no secrets, and
/// forwarding them through the `festerm-sessiond start` launch command
/// (whose own environment the daemon's spawned shell inherits) is what lets
/// a native persistent session render UTF-8 glyphs and resolve `PATH`
/// correctly when fesTerm itself was launched from Finder/Launchpad under
/// launchd. An explicit environment map containing any other key is
/// rejected below: persistent sessions do not otherwise support arbitrary
/// per-profile environment overrides.
fn is_launchd_correction_variable(key: &std::ffi::OsStr) -> bool {
    const ALLOWED: [&str; 4] = ["PATH", "LANG", "LC_ALL", "LC_CTYPE"];
    ALLOWED
        .iter()
        .any(|name| key.to_string_lossy().eq_ignore_ascii_case(name))
}

fn connect_or_start(
    name: &str,
    profile: &LocalProfile,
    size: TerminalSize,
) -> Result<Box<dyn SessionStream>, PersistentSessionError> {
    if let Some(record) = load_registry()?.sessions.get(name) {
        if let Ok(stream) = connect_record(record) {
            return Ok(stream);
        }
    }

    let daemon = daemon_executable()?;
    let mut command = Command::new(&daemon);
    command
        .arg("start")
        .arg("--name")
        .arg(name)
        .arg("--shell")
        .arg(profile.executable())
        .arg("--cols")
        .arg(size.columns().to_string())
        .arg("--rows")
        .arg(size.rows().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for argument in profile.arguments() {
        command.arg("--arg").arg(argument);
    }
    if let Some(directory) = profile.working_directory() {
        command.arg("--cwd").arg(directory);
    }
    match profile.environment() {
        EnvironmentPolicy::Inherit => {}
        EnvironmentPolicy::InheritWith(environment)
            if environment
                .keys()
                .all(|key| is_launchd_correction_variable(key)) =>
        {
            command.envs(environment);
        }
        EnvironmentPolicy::Clear(_) | EnvironmentPolicy::InheritWith(_) => {
            return Err(PersistentSessionError::new(
                "persistent local sessions do not support explicit environment maps",
            ))
        }
    }
    let output = command.output().map_err(|error| {
        PersistentSessionError::new(format!("could not start {}: {error}", daemon.display()))
    })?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        return Err(PersistentSessionError::new(format!(
            "session daemon could not start '{name}': {}",
            message.trim()
        )));
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(record) = load_registry()?.sessions.get(name) {
            if let Ok(stream) = connect_record(record) {
                return Ok(stream);
            }
        }
        if Instant::now() >= deadline {
            return Err(PersistentSessionError::new(format!(
                "timed out attaching to persistent session '{name}'"
            )));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn connect_record(
    record: &SessionRecord,
) -> Result<Box<dyn SessionStream>, PersistentSessionError> {
    connect_record_with_cancel(record, &AtomicBool::new(false))
}

fn connect_record_with_cancel(
    record: &SessionRecord,
    cancelled: &AtomicBool,
) -> Result<Box<dyn SessionStream>, PersistentSessionError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(PersistentSessionError::new("session connection cancelled"));
    }
    let _pid = record.pid;
    #[cfg(unix)]
    {
        let stream = UnixStream::connect(&record.socket).map_err(|error| {
            PersistentSessionError::new(format!("could not connect to session daemon: {error}"))
        })?;
        stream
            .set_read_timeout(Some(POLL_INTERVAL))
            .map_err(|error| PersistentSessionError::new(error.to_string()))?;
        stream
            .set_write_timeout(Some(WRITE_TIMEOUT))
            .map_err(|error| PersistentSessionError::new(error.to_string()))?;
        Ok(Box::new(stream))
    }

    #[cfg(windows)]
    {
        let mut stream = festerm_windows_security::named_pipe::Pipe::connect(
            &record.socket,
            CONNECT_TIMEOUT,
            cancelled,
        )
        .map_err(|error| {
            PersistentSessionError::new(format!("could not connect to session daemon: {error}"))
        })?;
        stream.set_read_timeout(POLL_INTERVAL);
        stream.set_write_timeout(WRITE_TIMEOUT);
        Ok(Box::new(stream))
    }
}

fn daemon_executable() -> Result<PathBuf, PersistentSessionError> {
    let current = std::env::current_exe().map_err(|error| {
        PersistentSessionError::new(format!("could not locate fesTerm executable: {error}"))
    })?;
    let directory = current.parent().ok_or_else(|| {
        PersistentSessionError::new("fesTerm executable has no containing directory")
    })?;
    let daemon = directory.join(if cfg!(windows) {
        "festerm-sessiond.exe"
    } else {
        "festerm-sessiond"
    });
    if daemon.is_file() {
        Ok(daemon)
    } else {
        Err(PersistentSessionError::new(format!(
            "persistent-session helper is not installed beside fesTerm: {}",
            daemon.display()
        )))
    }
}

fn load_registry() -> Result<SessionRegistry, PersistentSessionError> {
    let root = runtime_root()?;
    let lock_path = root.join("registry.lock");
    let registry_path = root.join("registry.json");
    let lock = match OpenOptions::new().read(true).open(&lock_path) {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return read_registry(&registry_path)
        }
        Err(error) => return Err(PersistentSessionError::new(error.to_string())),
    };
    lock.lock_shared()
        .map_err(|error| PersistentSessionError::new(error.to_string()))?;
    let registry = read_registry(&registry_path);
    FileExt::unlock(&lock).map_err(|error| PersistentSessionError::new(error.to_string()))?;
    registry
}

fn read_registry(path: &PathBuf) -> Result<SessionRegistry, PersistentSessionError> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.is_empty() => Ok(SessionRegistry::default()),
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            PersistentSessionError::new(format!("could not parse session registry: {error}"))
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SessionRegistry::default()),
        Err(error) => Err(PersistentSessionError::new(error.to_string())),
    }
}

fn runtime_root() -> Result<PathBuf, PersistentSessionError> {
    #[cfg(unix)]
    {
        if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
            return Ok(PathBuf::from(root).join("festerm").join("sessiond"));
        }
        if let Some(home) = std::env::var_os("HOME") {
            return Ok(PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("festerm")
                .join("sessiond"));
        }
        Err(PersistentSessionError::new(
            "neither XDG_STATE_HOME nor HOME is set; refusing an unscoped runtime directory",
        ))
    }

    #[cfg(windows)]
    {
        if let Some(root) = std::env::var_os("LOCALAPPDATA") {
            Ok(PathBuf::from(root).join("fesTerm").join("sessiond"))
        } else if let Some(root) = std::env::var_os("USERPROFILE") {
            Ok(PathBuf::from(root)
                .join("AppData")
                .join("Local")
                .join("fesTerm")
                .join("sessiond"))
        } else {
            Err(PersistentSessionError::new(
                "neither LOCALAPPDATA nor USERPROFILE is set; refusing an unscoped runtime directory",
            ))
        }
    }
}

/// Serializes one protocol frame into a single buffer.
///
/// Frames are built whole rather than written field by field so a write that
/// only partially completes can be resumed from an offset. Writing the header
/// with one call and the payload with another leaves no way to tell how much
/// of the frame the peer already has, which is how a timed-out write used to
/// desynchronize the stream.
fn encode_frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    debug_assert!(
        payload.len() <= MAX_FRAME_BYTES,
        "session command exceeds the protocol limit"
    );
    let mut frame = Vec::with_capacity(FRAME_MAGIC.len() + 5 + payload.len());
    frame.extend_from_slice(FRAME_MAGIC);
    frame.push(kind);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

fn encode_resize(size: TerminalSize) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8);
    for value in [
        size.columns(),
        size.rows(),
        size.pixel_width().unwrap_or(0),
        size.pixel_height().unwrap_or(0),
    ] {
        payload.extend_from_slice(&value.to_be_bytes());
    }
    payload
}

#[derive(Default)]
struct OutputScanner {
    pending: Vec<u8>,
}

enum ScanResult {
    Output(Vec<u8>),
    Pending,
    Exited(Vec<u8>),
    Stolen(Vec<u8>),
}

impl OutputScanner {
    fn push(&mut self, bytes: &[u8]) -> ScanResult {
        self.pending.extend_from_slice(bytes);
        if let Some(position) = find_bytes(&self.pending, STOLEN_NOTICE_BYTES) {
            let output = self.pending[..position].to_vec();
            self.pending.clear();
            return ScanResult::Stolen(output);
        }
        if let Some(position) = find_bytes(&self.pending, EXITED_NOTICE_BYTES) {
            let output = self.pending[..position].to_vec();
            self.pending.clear();
            return ScanResult::Exited(output);
        }
        let retained = partial_marker_suffix_len(&self.pending);
        let flush_count = self.pending.len() - retained;
        if flush_count == 0 {
            ScanResult::Pending
        } else {
            let output = self.pending.drain(..flush_count).collect();
            ScanResult::Output(output)
        }
    }

    fn close(&mut self) -> Option<Vec<u8>> {
        (!self.pending.is_empty()).then(|| std::mem::take(&mut self.pending))
    }
}

fn partial_marker_suffix_len(data: &[u8]) -> usize {
    [STOLEN_NOTICE_BYTES, EXITED_NOTICE_BYTES]
        .into_iter()
        .map(|marker| {
            let maximum = data.len().min(marker.len().saturating_sub(1));
            (1..=maximum)
                .rev()
                .find(|&length| data.ends_with(&marker[..length]))
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Client-worker regression tests that need no real transport, so they cover
/// the Windows named-pipe build as well as the Unix socket one.
#[cfg(test)]
mod client_worker_tests {
    use super::*;

    /// A transport whose writes can be made to stall or dribble, recording
    /// everything the worker actually put on the wire.
    #[derive(Clone, Default)]
    struct ScriptedStream {
        state: Arc<Mutex<ScriptedState>>,
    }

    #[derive(Default)]
    struct ScriptedState {
        /// Write outcomes consumed in order. `Err` fails the attempt, `Ok(n)`
        /// caps how many bytes that attempt accepts. Once empty, writes take
        /// everything offered.
        write_script: VecDeque<Result<usize, io::ErrorKind>>,
        written: Vec<u8>,
        readable: VecDeque<Vec<u8>>,
        writes_blocked: bool,
        write_attempts: usize,
        eof: bool,
    }

    impl ScriptedStream {
        fn script_writes(&self, script: impl IntoIterator<Item = Result<usize, io::ErrorKind>>) {
            self.lock().write_script = script.into_iter().collect();
        }

        fn queue_readable(&self, chunk: Vec<u8>) {
            self.lock().readable.push_back(chunk);
        }

        fn written(&self) -> Vec<u8> {
            self.lock().written.clone()
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, ScriptedState> {
            self.state.lock().expect("scripted stream lock is healthy")
        }
    }

    impl Read for ScriptedStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let mut state = self.lock();
            let Some(chunk) = state.readable.pop_front() else {
                if state.eof {
                    return Ok(0);
                }
                // The real transports carry a read timeout, so an idle
                // transport reports `TimedOut` rather than blocking.
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            };
            let count = chunk.len().min(buffer.len());
            buffer[..count].copy_from_slice(&chunk[..count]);
            if count < chunk.len() {
                state.readable.push_front(chunk[count..].to_vec());
            }
            Ok(count)
        }
    }

    impl Write for ScriptedStream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut state = self.lock();
            state.write_attempts += 1;
            if state.writes_blocked {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let allowed = match state.write_script.pop_front() {
                Some(Err(kind)) => return Err(io::Error::from(kind)),
                Some(Ok(limit)) => limit.min(bytes.len()),
                None => bytes.len(),
            };
            state.written.extend_from_slice(&bytes[..allowed]);
            Ok(allowed)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Spelled out here rather than borrowed from the encoder under test, so
    /// these tests describe the wire and not the implementation.
    fn input_frame(payload: &[u8]) -> Vec<u8> {
        let mut frame = b"FSD1".to_vec();
        frame.push(1);
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    fn wait_for(mut ready: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if ready() {
                return true;
            }
            thread::sleep(POLL_INTERVAL);
        }
        ready()
    }

    #[test]
    fn reconnect_discards_old_pending_input_and_preserves_notifier_and_identity() {
        struct Notifier(std::sync::atomic::AtomicUsize);
        impl SessionEventNotifier for Notifier {
            fn notify(&self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let old = ScriptedStream::default();
        old.lock().writes_blocked = true;
        let replacement = ScriptedStream::default();
        let connector_stream = replacement.clone();
        let notifier = Arc::new(Notifier(std::sync::atomic::AtomicUsize::new(0)));
        let session = PersistentSession::from_stream_with_reconnector(
            Box::new(old.clone()),
            notifier.clone(),
            Some(Arc::new(move |_| Ok(Box::new(connector_stream.clone())))),
        )
        .unwrap();
        let id = session.id();
        assert!(wait_for(|| matches!(
            session.lifecycle(),
            SessionLifecycle::Running
        )));
        assert!(!session.reconnect_available());
        assert!(session.try_reconnect().is_err());
        session.try_send_input(b"must not replay").unwrap();
        assert!(wait_for(|| old.lock().write_attempts > 0));
        let mut admitted = 1;
        assert!(wait_for(|| {
            while session.try_send_input(b"also stale").is_ok() {
                admitted += 1;
                assert!(admitted <= 2 * DEFAULT_COMMAND_QUEUE_CAPACITY);
            }
            admitted == 2 * DEFAULT_COMMAND_QUEUE_CAPACITY
        }));
        old.lock().eof = true;
        assert!(wait_for(|| session.reconnect_available()));
        assert!(session.try_send_input(b"while disconnected").is_err());
        let notifications = notifier.0.load(Ordering::Relaxed);
        session.try_reconnect().unwrap();
        assert!(wait_for(|| matches!(
            session.lifecycle(),
            SessionLifecycle::Running
        )));
        assert_eq!(session.id(), id);
        assert!(notifier.0.load(Ordering::Relaxed) > notifications);
        session.try_send_input(b"fresh").unwrap();
        assert!(wait_for(|| replacement.written() == input_frame(b"fresh")));
        assert!(old.written().is_empty());
        assert_eq!(
            session.shutdown(Duration::from_secs(2)).unwrap(),
            ShutdownResult::Stopped
        );
    }

    #[test]
    fn reconnect_is_nonblocking_rejects_duplicates_and_honors_shutdown() {
        let old = ScriptedStream::default();
        old.lock().eof = true;
        let (entered, connecting) = mpsc::sync_channel(1);
        let caller = thread::current().id();
        let session = PersistentSession::from_stream_with_reconnector(
            Box::new(old),
            noop_session_event_notifier(),
            Some(Arc::new(move |cancelled| {
                assert_ne!(thread::current().id(), caller);
                entered.send(()).unwrap();
                while !cancelled.load(Ordering::Acquire) {
                    thread::sleep(POLL_INTERVAL);
                }
                Err(PersistentSessionError::new("cancelled while connecting"))
            })),
        )
        .unwrap();
        assert!(wait_for(|| session.reconnect_available()));
        session.try_reconnect().unwrap();
        connecting.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(session.lifecycle(), SessionLifecycle::Starting));
        assert!(!session.reconnect_available());
        assert!(session.try_reconnect().is_err());
        assert!(session.try_send_input(b"while connecting").is_err());
        assert_eq!(
            session.shutdown(Duration::from_secs(2)).unwrap(),
            ShutdownResult::Stopped
        );
        assert!(matches!(session.lifecycle(), SessionLifecycle::Stopped));
        assert!(!session.reconnect_available());
    }

    #[cfg(windows)]
    #[test]
    fn windows_reconnect_shutdown_cancels_a_busy_native_pipe_without_releasing_its_client() {
        use festerm_windows_security::named_pipe::{Pipe, PipeListener};
        let name = format!(
            r"\\.\pipe\festerm-reconnect-cancel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let listener = PipeListener::bind(&name, true).unwrap();
        let _existing_client =
            Pipe::connect(&name, Duration::from_secs(1), &AtomicBool::new(false)).unwrap();
        let _occupied_server = listener.accept(&AtomicBool::new(false)).unwrap();
        let record: SessionRecord = serde_json::from_value(serde_json::json!({
            "pid": std::process::id(), "socket": name
        }))
        .unwrap();
        let old = ScriptedStream::default();
        old.lock().eof = true;
        let (entered, connecting) = mpsc::sync_channel(1);
        let session = PersistentSession::from_stream_with_reconnector(
            Box::new(old),
            noop_session_event_notifier(),
            Some(Arc::new(move |cancelled| {
                entered.send(()).unwrap();
                connect_record_with_cancel(&record, cancelled)
            })),
        )
        .unwrap();
        assert!(wait_for(|| session.reconnect_available()));
        session.try_reconnect().unwrap();
        connecting.recv_timeout(Duration::from_secs(1)).unwrap();
        thread::sleep(2 * POLL_INTERVAL);
        assert!(matches!(session.lifecycle(), SessionLifecycle::Starting));
        assert_eq!(
            session.shutdown(Duration::from_secs(1)).unwrap(),
            ShutdownResult::Stopped
        );
        assert!(matches!(session.lifecycle(), SessionLifecycle::Stopped));
    }

    #[test]
    fn reconnect_request_does_not_wait_for_the_old_full_event_queue() {
        let old = ScriptedStream::default();
        for _ in 0..DEFAULT_EVENT_QUEUE_CAPACITY - 2 {
            old.queue_readable(b"old output".to_vec());
        }
        old.lock().eof = true;
        let session = PersistentSession::from_stream_with_reconnector(
            Box::new(old),
            noop_session_event_notifier(),
            Some(Arc::new(|_| Ok(Box::new(ScriptedStream::default())))),
        )
        .unwrap();
        assert!(wait_for(|| session.reconnect_available()));
        assert_eq!(
            session.metrics().event_queue_depth,
            DEFAULT_EVENT_QUEUE_CAPACITY
        );
        session.try_reconnect().unwrap();
        assert!(matches!(session.lifecycle(), SessionLifecycle::Starting));
        let mut output = Vec::new();
        let mut lifecycles = Vec::new();
        assert!(wait_for(|| {
            while let Ok(event) = session.try_recv_event() {
                match event {
                    SessionEvent::Output(bytes) => output.extend(bytes),
                    SessionEvent::Lifecycle(state) => lifecycles.push(state),
                    _ => {}
                }
            }
            lifecycles.len() == 5
        }));
        assert_eq!(
            output,
            b"old output".repeat(DEFAULT_EVENT_QUEUE_CAPACITY - 2)
        );
        assert!(matches!(
            lifecycles.as_slice(),
            [
                SessionLifecycle::Starting,
                SessionLifecycle::Running,
                SessionLifecycle::Disconnected(_),
                SessionLifecycle::Starting,
                SessionLifecycle::Running
            ]
        ));
        session.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn reconnect_missing_daemon_reports_failure_without_starting_a_shell() {
        let old = ScriptedStream::default();
        old.lock().eof = true;
        let session = PersistentSession::from_stream_with_reconnector(
            Box::new(old),
            noop_session_event_notifier(),
            Some(Arc::new(|_| {
                connect_existing_in_registry(&SessionRegistry::default(), "missing")
            })),
        )
        .unwrap();
        assert!(wait_for(|| session.reconnect_available()));
        session.try_reconnect().unwrap();
        assert!(wait_for(|| session.reconnect_available()));
        assert!(matches!(
            session.lifecycle(),
            SessionLifecycle::Disconnected(error) if error.message().contains("no locally running session named 'missing'")
        ));
        let events: Vec<_> = std::iter::from_fn(|| session.try_recv_event().ok()).collect();
        assert!(events.iter().any(|event| matches!(
            event,
            SessionEvent::Error(error) if error.message().contains("missing")
        )));
        assert_eq!(session.metrics().error_count, 1);
        session
            .shared
            .set_lifecycle(SessionLifecycle::Failed(SessionError::new(
                SessionErrorKind::Output,
                "failed attached transport",
            )));
        assert!(session.reconnect_available());
        session.try_reconnect().unwrap();
        assert!(wait_for(|| session.reconnect_available()));
        assert_eq!(session.metrics().error_count, 2);
    }

    #[test]
    fn unnamed_and_exited_sessions_do_not_offer_reconnect() {
        let old = ScriptedStream::default();
        old.lock().eof = true;
        let session =
            PersistentSession::from_stream(Box::new(old), noop_session_event_notifier()).unwrap();
        assert!(wait_for(|| matches!(
            session.lifecycle(),
            SessionLifecycle::Disconnected(_)
        )));
        assert!(!session.reconnect_available());
        assert!(session.try_reconnect().is_err());

        let old = ScriptedStream::default();
        old.queue_readable(EXITED_NOTICE_BYTES.to_vec());
        let session = PersistentSession::from_stream_with_reconnector(
            Box::new(old),
            noop_session_event_notifier(),
            Some(Arc::new(|_| panic!("an exited shell must not reconnect"))),
        )
        .unwrap();
        assert!(wait_for(|| matches!(
            session.lifecycle(),
            SessionLifecycle::Exited(_)
        )));
        assert!(!session.reconnect_available());
        assert!(session.try_reconnect().is_err());
    }

    #[test]
    fn stalled_writes_keep_outbound_commands_bounded_and_ordered() {
        let stream = ScriptedStream::default();
        stream.lock().writes_blocked = true;
        let session =
            PersistentSession::from_stream(Box::new(stream.clone()), noop_session_event_notifier())
                .unwrap();
        let mut expected = Vec::new();
        let mut admitted = 0;
        for _ in 0..8 {
            while let Ok(()) = session.try_send_input(&[admitted as u8]) {
                expected.extend(input_frame(&[admitted as u8]));
                admitted += 1;
                assert!(
                    admitted <= 2 * DEFAULT_COMMAND_QUEUE_CAPACITY,
                    "the worker is hiding an unbounded backlog behind the bounded command channel"
                );
            }
            let attempts = stream.lock().write_attempts;
            assert!(wait_for(|| stream.lock().write_attempts > attempts));
        }
        stream.lock().writes_blocked = false;
        assert!(wait_for(|| stream.written() == expected));
        assert!(matches!(session.lifecycle(), SessionLifecycle::Running));
        session.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn pending_resize_events_are_bounded_without_blocking_prior_input() {
        let stream = ScriptedStream::default();
        let session =
            PersistentSession::from_stream(Box::new(stream), noop_session_event_notifier())
                .unwrap();
        let size = TerminalSize::new(80, 24).unwrap();
        let mut pending: VecDeque<_> = (0..DEFAULT_EVENT_QUEUE_CAPACITY)
            .map(|_| SessionEvent::ResizeApplied(size))
            .collect();
        let mut outbound = OutboundFrames::default();
        outbound.push_input(b"before");
        outbound.push_resize(size);
        outbound.push_input(b"after");
        let mut wire = Vec::new();
        outbound
            .pump(&mut wire, &session.shared, &mut pending)
            .unwrap();
        assert_eq!(wire, input_frame(b"before"));
        assert_eq!(pending.len(), DEFAULT_EVENT_QUEUE_CAPACITY);
        assert_eq!(outbound.frames.len(), 2);

        pending.pop_front();
        outbound
            .pump(&mut wire, &session.shared, &mut pending)
            .unwrap();
        assert_eq!(pending.len(), DEFAULT_EVENT_QUEUE_CAPACITY);
        assert!(outbound.is_empty());
        assert_eq!(
            wire,
            [
                input_frame(b"before"),
                encode_frame(FRAME_RESIZE, &encode_resize(size)),
                input_frame(b"after"),
            ]
            .concat()
        );
        session.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn shutdown_and_drop_remain_responsive_when_outbound_and_commands_are_full() {
        for drop_session in [false, true] {
            let stream = ScriptedStream::default();
            stream.lock().writes_blocked = true;
            let session = PersistentSession::from_stream(
                Box::new(stream.clone()),
                noop_session_event_notifier(),
            )
            .unwrap();
            let mut admitted = 0;
            assert!(wait_for(|| {
                while session.try_send_input(b"x").is_ok() {
                    admitted += 1;
                    assert!(admitted <= 2 * DEFAULT_COMMAND_QUEUE_CAPACITY);
                }
                admitted == 2 * DEFAULT_COMMAND_QUEUE_CAPACITY
            }));
            if drop_session {
                let shared = Arc::clone(&session.shared);
                drop(session);
                assert!(wait_for(|| matches!(
                    shared.lifecycle(),
                    SessionLifecycle::Stopped
                )));
            } else {
                assert_eq!(
                    session.shutdown(Duration::from_secs(2)).unwrap(),
                    ShutdownResult::Stopped
                );
            }
        }
    }

    #[test]
    fn large_outbound_frames_yield_between_bounded_write_batches() {
        let session = PersistentSession::from_stream(
            Box::new(ScriptedStream::default()),
            noop_session_event_notifier(),
        )
        .unwrap();
        let mut outbound = OutboundFrames::default();
        outbound.push_input(&vec![b'x'; MAX_IO_CHUNK_BYTES]);
        outbound.push_input(b"later");
        let mut wire = Vec::new();
        let mut events = VecDeque::new();
        outbound
            .pump(&mut wire, &session.shared, &mut events)
            .unwrap();
        assert_eq!(wire.len(), MAX_IO_CHUNK_BYTES);
        assert_eq!(outbound.frames.len(), 2);
        outbound
            .pump(&mut wire, &session.shared, &mut events)
            .unwrap();
        assert!(outbound.is_empty());
        assert_eq!(
            wire,
            [
                input_frame(&vec![b'x'; MAX_IO_CHUNK_BYTES]),
                input_frame(b"later")
            ]
            .concat()
        );
        session.shutdown(Duration::from_secs(2)).unwrap();
    }

    /// The transport carries a write timeout so a wedged daemon cannot pin the
    /// worker forever, but a timeout means the daemon is busy - usually
    /// because it is trying to hand us output we have not read yet. Treating
    /// it as a transport failure ended the worker, dropped the command
    /// channel, and left the session unable to accept another keystroke: the
    /// interrupt appeared to land and then the terminal went deaf.
    #[test]
    fn a_write_timeout_is_backpressure_rather_than_a_dead_session() {
        let stream = ScriptedStream::default();
        stream.script_writes([
            Err(io::ErrorKind::TimedOut),
            Err(io::ErrorKind::TimedOut),
            Err(io::ErrorKind::WouldBlock),
        ]);
        let session =
            PersistentSession::from_stream(Box::new(stream.clone()), noop_session_event_notifier())
                .expect("the worker should start");

        session.try_send_input(b"\x03").expect("input is accepted");
        assert!(
            wait_for(|| stream.written() == input_frame(b"\x03")),
            "the interrupt never reached the daemon; the wire carries {:?}",
            stream.written()
        );

        // The session has to still be usable afterwards, which is the half the
        // user actually notices.
        session
            .try_send_input(b"later")
            .expect("the session still accepts input");
        let expected = [input_frame(b"\x03"), input_frame(b"later")].concat();
        assert!(
            wait_for(|| stream.written() == expected),
            "input sent after the timeout was lost; the wire carries {:?}",
            stream.written()
        );
        assert!(
            matches!(session.lifecycle(), SessionLifecycle::Running),
            "a write timeout disconnected the session: {:?}",
            session.lifecycle()
        );
    }

    /// A frame used to be written field by field with `write_all`, which
    /// cannot report how much it wrote. A write that stopped part way through
    /// therefore left a partial frame on the wire with no way to resume it, so
    /// a retry would either repeat bytes the daemon already had or drop the
    /// rest of the frame.
    #[test]
    fn a_partially_written_frame_resumes_without_repeating_bytes() {
        let stream = ScriptedStream::default();
        stream.script_writes([
            Ok(3),
            Err(io::ErrorKind::TimedOut),
            Ok(2),
            Err(io::ErrorKind::WouldBlock),
            Ok(1),
        ]);
        let session =
            PersistentSession::from_stream(Box::new(stream.clone()), noop_session_event_notifier())
                .expect("the worker should start");

        session
            .try_send_input(b"interrupt")
            .expect("input accepted");
        let expected = input_frame(b"interrupt");
        assert!(
            wait_for(|| stream.written() == expected),
            "the resumed frame is not byte-for-byte the original; the wire carries {:?}",
            stream.written()
        );
    }

    /// The worker is the only thread that can write input, so parking it until
    /// the GUI drains output means the interrupt the user is trying to send is
    /// queued behind the very flood they are trying to stop. `dir /s` on a
    /// large tree fills the event queue for seconds at a time.
    #[test]
    fn input_reaches_the_daemon_while_the_application_is_behind_on_output() {
        let stream = ScriptedStream::default();
        // Far more output than the application's event queue can hold, and
        // nothing in this test ever drains it.
        for _ in 0..(DEFAULT_EVENT_QUEUE_CAPACITY * 4) {
            stream.queue_readable(vec![b'x'; 512]);
        }
        let session =
            PersistentSession::from_stream(Box::new(stream.clone()), noop_session_event_notifier())
                .expect("the worker should start");

        assert!(
            wait_for(|| session.metrics().event_queue_depth >= DEFAULT_EVENT_QUEUE_CAPACITY),
            "the event queue never filled, so this is not testing a backed-up application"
        );

        session.try_send_input(b"\x03").expect("input is accepted");
        assert!(
            wait_for(|| stream.written() == input_frame(b"\x03")),
            "the interrupt never reached the daemon while output was backed up; \
             the wire carries {:?}",
            stream.written()
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn native_socket_eof_allows_manual_resume_on_the_same_session() {
        let (old_client, old_server) = UnixStream::pair().unwrap();
        let (new_client, mut new_server) = UnixStream::pair().unwrap();
        for socket in [&old_client, &new_client, &new_server] {
            socket.set_read_timeout(Some(POLL_INTERVAL)).unwrap();
            socket.set_write_timeout(Some(WRITE_TIMEOUT)).unwrap();
        }
        new_server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let replacement = Mutex::new(Some(new_client));
        let session = PersistentSession::from_stream_with_reconnector(
            Box::new(old_client),
            noop_session_event_notifier(),
            Some(Arc::new(move |_| {
                Ok(Box::new(replacement.lock().unwrap().take().unwrap()))
            })),
        )
        .unwrap();
        let id = session.id();
        wait_for_lifecycle(&session, |state| matches!(state, SessionLifecycle::Running));
        drop(old_server);
        wait_for_lifecycle(&session, |state| {
            matches!(state, SessionLifecycle::Disconnected(_))
        });
        assert!(session.reconnect_available());
        session.try_reconnect().unwrap();
        wait_for_lifecycle(&session, |state| {
            matches!(state, SessionLifecycle::Starting)
        });
        wait_for_lifecycle(&session, |state| matches!(state, SessionLifecycle::Running));
        assert_eq!(session.id(), id);
        session.try_send_input(b"fresh native input").unwrap();
        assert_eq!(
            read_frame(&mut new_server),
            (FRAME_INPUT, b"fresh native input".to_vec())
        );
        new_server.write_all(b"resumed output").unwrap();
        new_server.write_all(EXITED_NOTICE_BYTES).unwrap();
        let mut output = Vec::new();
        wait_for_lifecycle_with_output(&session, &mut output, |state| {
            matches!(state, SessionLifecycle::Exited(_))
        });
        assert_eq!(output, b"resumed output");
        assert!(!session.reconnect_available());
    }

    #[test]
    fn session_backend_forwards_input_resize_output_and_takeover() {
        let (client, mut server) = UnixStream::pair().unwrap();
        client.set_read_timeout(Some(POLL_INTERVAL)).unwrap();
        client.set_write_timeout(Some(WRITE_TIMEOUT)).unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let server_thread = thread::spawn(move || {
            let input = read_frame(&mut server);
            assert_eq!(input.0, FRAME_INPUT);
            assert_eq!(input.1, b"typed");

            let resize = read_frame(&mut server);
            assert_eq!(resize.0, FRAME_RESIZE);
            assert_eq!(
                resize.1,
                [120u16, 40, 1200, 800]
                    .into_iter()
                    .flat_map(u16::to_be_bytes)
                    .collect::<Vec<_>>()
            );

            server.write_all(b"shell output").unwrap();
            server.write_all(&STOLEN_NOTICE_BYTES[..17]).unwrap();
            thread::sleep(POLL_INTERVAL);
            server.write_all(&STOLEN_NOTICE_BYTES[17..]).unwrap();
            server.flush().unwrap();
        });

        let session =
            PersistentSession::from_stream(Box::new(client), noop_session_event_notifier())
                .unwrap();
        wait_for_lifecycle(&session, |lifecycle| {
            matches!(lifecycle, SessionLifecycle::Running)
        });
        session.try_send_input(b"typed").unwrap();
        session
            .try_resize(TerminalSize::with_pixels(120, 40, 1200, 800).unwrap())
            .unwrap();

        let mut output = Vec::new();
        let lifecycle = wait_for_lifecycle_with_output(&session, &mut output, |lifecycle| {
            matches!(lifecycle, SessionLifecycle::Disconnected(_))
        });
        assert_eq!(output, b"shell output");
        let SessionLifecycle::Disconnected(error) = lifecycle else {
            unreachable!()
        };
        assert!(error.message().contains("another client"));
        server_thread.join().unwrap();
    }

    #[test]
    fn session_backend_distinguishes_shell_exit_from_transport_loss() {
        let (client, mut server) = UnixStream::pair().unwrap();
        client.set_read_timeout(Some(POLL_INTERVAL)).unwrap();
        client.set_write_timeout(Some(WRITE_TIMEOUT)).unwrap();
        let session =
            PersistentSession::from_stream(Box::new(client), noop_session_event_notifier())
                .unwrap();
        wait_for_lifecycle(&session, |lifecycle| {
            matches!(lifecycle, SessionLifecycle::Running)
        });
        server.write_all(EXITED_NOTICE_BYTES).unwrap();
        server.flush().unwrap();
        assert!(matches!(
            wait_for_lifecycle(&session, SessionLifecycle::is_terminal),
            SessionLifecycle::Exited(exit) if exit.success()
        ));
    }

    #[test]
    fn launchd_correction_allowlist_accepts_path_and_locale_only() {
        for allowed in ["PATH", "path", "LANG", "LC_ALL", "LC_CTYPE"] {
            assert!(
                is_launchd_correction_variable(std::ffi::OsStr::new(allowed)),
                "{allowed} should be forwardable to a persistent session"
            );
        }
        for rejected in ["HOME", "SHELL", "MY_SECRET", "LC_TIME"] {
            assert!(
                !is_launchd_correction_variable(std::ffi::OsStr::new(rejected)),
                "{rejected} should not be forwardable to a persistent session"
            );
        }
    }

    fn read_frame(reader: &mut impl Read) -> (u8, Vec<u8>) {
        let mut header = [0u8; 9];
        reader.read_exact(&mut header).unwrap();
        assert_eq!(&header[..4], FRAME_MAGIC);
        let length = u32::from_be_bytes(header[5..9].try_into().unwrap()) as usize;
        let mut payload = vec![0u8; length];
        reader.read_exact(&mut payload).unwrap();
        (header[4], payload)
    }

    fn wait_for_lifecycle(
        session: &PersistentSession,
        predicate: impl Fn(&SessionLifecycle) -> bool,
    ) -> SessionLifecycle {
        wait_for_lifecycle_with_output(session, &mut Vec::new(), predicate)
    }

    fn wait_for_lifecycle_with_output(
        session: &PersistentSession,
        output: &mut Vec<u8>,
        predicate: impl Fn(&SessionLifecycle) -> bool,
    ) -> SessionLifecycle {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match session.try_recv_event() {
                Ok(SessionEvent::Output(bytes)) => output.extend_from_slice(&bytes),
                Ok(SessionEvent::Lifecycle(lifecycle)) if predicate(&lifecycle) => {
                    return lifecycle
                }
                Ok(_) | Err(SessionTryReceiveError::Empty) => {}
                Err(SessionTryReceiveError::Closed) => panic!("session event channel closed"),
            }
            assert!(Instant::now() < deadline, "timed out waiting for lifecycle");
            thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod registry_filtering_tests {
    use super::*;

    #[test]
    fn registry_round_trip_ignores_unknown_fields_and_defaults_new_ones() {
        let json = r#"{"sessions":{"demo":{"pid":42,"socket":"demo.sock"}}}"#;
        let registry: SessionRegistry = serde_json::from_str(json).unwrap();
        let record = registry.sessions.get("demo").unwrap();
        assert_eq!(record.pid, 42);
        assert_eq!(record.socket, "demo.sock");
        assert_eq!(record.name, "");
        assert!(!record.attached);
    }

    #[test]
    fn unattached_session_carries_expected_metadata() {
        let record = SessionRecord {
            name: "demo".to_owned(),
            pid: 1,
            socket: "demo.sock".to_owned(),
            shell: "/bin/bash".to_owned(),
            arguments: vec!["-l".to_owned()],
            working_directory: Some("/tmp".to_owned()),
            created_at_unix_ms: 123,
            attached: false,
        };
        let mut registry = SessionRegistry::default();
        registry.sessions.insert(record.name.clone(), record);

        let unattached: Vec<_> = registry
            .sessions
            .into_values()
            .filter(|record| !record.attached)
            .map(|record| UnattachedSession {
                name: record.name,
                shell: record.shell,
                arguments: record.arguments,
                working_directory: record.working_directory,
                created_at_unix_ms: record.created_at_unix_ms,
            })
            .collect();

        assert_eq!(unattached.len(), 1);
        assert_eq!(unattached[0].name, "demo");
        assert_eq!(unattached[0].shell, "/bin/bash");
        assert_eq!(unattached[0].working_directory.as_deref(), Some("/tmp"));
    }

    #[test]
    fn attached_sessions_are_excluded_from_the_unattached_view() {
        let mut registry = SessionRegistry::default();
        registry.sessions.insert(
            "attached-demo".to_owned(),
            SessionRecord {
                name: "attached-demo".to_owned(),
                pid: 1,
                socket: "demo.sock".to_owned(),
                shell: "/bin/bash".to_owned(),
                arguments: Vec::new(),
                working_directory: None,
                created_at_unix_ms: 0,
                attached: true,
            },
        );

        let unattached: Vec<_> = registry
            .sessions
            .into_values()
            .filter(|record| !record.attached)
            .collect();
        assert!(unattached.is_empty());
    }
}
