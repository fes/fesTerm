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

#[cfg(any(windows, test))]
use std::{
    fs::{self, File},
    path::Path,
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
/// Compatibility epoch for registry records and the `FSD1` client protocol.
///
/// This changes only when the daemon introduces an incompatible wire format.
pub const PROTOCOL_VERSION: u16 = 1;
/// Oldest daemon protocol this client can attach to safely.
///
/// A release that increments [`PROTOCOL_VERSION`] must retain adapters down to
/// this version for every daemon expected to survive a supported upgrade.
pub const MIN_SUPPORTED_PROTOCOL_VERSION: u16 = 1;
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
    #[serde(default = "legacy_protocol_version")]
    protocol_version: u16,
    /// File name of the helper image this generation executes.
    ///
    /// Only Windows keeps per-release runtime copies, but every platform
    /// records the identity so one registry schema serves all of them.
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    #[serde(default)]
    helper_identity: Option<String>,
}

const fn legacy_protocol_version() -> u16 {
    1
}

#[derive(Default)]
struct SessionRegistry {
    sessions: BTreeMap<String, SessionRecord>,
    /// Sessions this build could not interpret, by name and declared epoch.
    ///
    /// A future helper may write records this release cannot deserialize. One
    /// such record must cost its own session, not the whole registry, so the
    /// name is kept to explain the gap when someone asks for it.
    unreadable: BTreeMap<String, Option<u16>>,
}

impl<'de> Deserialize<'de> for SessionRegistry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Default, Deserialize)]
        struct Raw {
            #[serde(default)]
            sessions: BTreeMap<String, serde_json::Value>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let mut registry = SessionRegistry::default();
        for (name, value) in raw.sessions {
            match serde_json::from_value::<SessionRecord>(value.clone()) {
                Ok(record) => {
                    registry.sessions.insert(name, record);
                }
                Err(_) => {
                    let declared = value
                        .get("protocol_version")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|version| u16::try_from(version).ok());
                    registry.unreadable.insert(name, declared);
                }
            }
        }
        Ok(registry)
    }
}

/// A locally running `festerm-sessiond` session with no attached client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnattachedSession {
    pub name: String,
    pub shell: String,
    pub arguments: Vec<String>,
    pub working_directory: Option<String>,
    pub created_at_unix_ms: u128,
    /// Daemon generation, not just the reusable display name.
    pub pid: u32,
    pub endpoint: String,
}

/// Enumerates locally registered `festerm-sessiond` sessions that are alive
/// but currently have no attached client, suitable for surfacing as
/// one-click "resume" entries on the New Session/Launcher screen.
///
/// A missing registry is ordinary absence; corrupt, inaccessible, or busy
/// registries are explicit errors. The registry lock has a bounded deadline.
pub fn list_unattached_local_sessions() -> Result<Vec<UnattachedSession>, PersistentSessionError> {
    list_unattached_sessions_in(&runtime_root()?)
}

/// Explicit runtime-root seam for isolated native validation.
pub fn list_unattached_sessions_in(
    root: &std::path::Path,
) -> Result<Vec<UnattachedSession>, PersistentSessionError> {
    let mut sessions = Vec::new();
    for (name, record) in load_registry_in(root)?.sessions {
        if record.name != name || PersistentSessionName::new(&name).is_err() {
            return Err(PersistentSessionError::new(
                "session registry contains an invalid or inconsistent session identity",
            ));
        }
        if record.attached || !record_is_live(root, &record)? {
            continue;
        }
        sessions.push(UnattachedSession {
            name: record.name,
            shell: record.shell,
            arguments: record.arguments,
            working_directory: record.working_directory,
            created_at_unix_ms: record.created_at_unix_ms,
            pid: record.pid,
            endpoint: record.socket,
        });
    }
    sessions.sort_by(|a, b| a.name.cmp(&b.name).then(a.endpoint.cmp(&b.endpoint)));
    Ok(sessions)
}

fn record_is_live(
    root: &std::path::Path,
    record: &SessionRecord,
) -> Result<bool, PersistentSessionError> {
    if cfg!(unix) && !std::path::Path::new(&record.socket).exists() {
        return Ok(false);
    }
    daemon_generation_is_live(root, record.pid, record.created_at_unix_ms, &record.socket)
}

/// Shared by Launcher discovery and the helper's list/start/kill operations.
/// New generations hold a lifetime lease; a reused PID is not a live daemon.
/// Legacy endpoints retain their older PID-based compatibility behavior.
pub fn daemon_generation_is_live(
    root: &std::path::Path,
    pid: u32,
    created_at_unix_ms: u128,
    endpoint: &str,
) -> Result<bool, PersistentSessionError> {
    if !process_alive(pid) {
        return Ok(false);
    }
    let generation = format!("{pid}-{created_at_unix_ms}");
    // Older helpers used name-based endpoints and have no lifetime lease.
    if !endpoint.ends_with(&generation) && !endpoint.ends_with(&format!("{generation}.sock")) {
        return Ok(true);
    }
    let path = root.join(format!("lease-{generation}"));
    let lease = match OpenOptions::new().read(true).open(path) {
        Ok(lease) => lease,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(PersistentSessionError::new(format!(
                "could not inspect daemon lifetime: {error}"
            )))
        }
    };
    match FileExt::try_lock_shared(&lease) {
        Ok(()) => Ok(false),
        Err(error) if lock_is_contended(&error) => Ok(true),
        Err(error) => Err(PersistentSessionError::new(format!(
            "could not inspect daemon lifetime: {error}"
        ))),
    }
}

pub fn lock_is_contended(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock
        || error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
}

#[cfg(unix)]
pub fn process_alive(pid: u32) -> bool {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    match kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(Errno::EPERM) => true,
        Err(_) => false,
    }
}

#[cfg(windows)]
pub fn process_alive(pid: u32) -> bool {
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

    /// Attach the selected generation only. A replaced name or newly attached
    /// session is not permission to create a shell or steal another GUI client.
    pub fn resume_discovered_with_notifier(
        selected: &UnattachedSession,
        notifier: Arc<dyn SessionEventNotifier>,
    ) -> Result<Self, PersistentSessionError> {
        Self::resume_discovered_in(selected, &runtime_root()?, notifier)
    }

    pub fn resume_discovered_in(
        selected: &UnattachedSession,
        root: &std::path::Path,
        notifier: Arc<dyn SessionEventNotifier>,
    ) -> Result<Self, PersistentSessionError> {
        let stream = connect_discovered_with_cancel(selected, root, true, &AtomicBool::new(false))?;
        let selected = selected.clone();
        let root = root.to_owned();
        Self::from_stream_with_reconnector(
            stream,
            notifier,
            Some(Arc::new(move |cancelled| {
                // Manual reconnect retains steal-on-reconnect, but only for
                // the generation selected from this registry.
                connect_discovered_with_cancel(&selected, &root, false, cancelled)
            })),
        )
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

fn connect_discovered_with_cancel(
    selected: &UnattachedSession,
    root: &std::path::Path,
    require_unattached: bool,
    cancelled: &AtomicBool,
) -> Result<Box<dyn SessionStream>, PersistentSessionError> {
    let registry = load_registry_in_with_cancel(root, cancelled)?;
    let record = registry.sessions.get(&selected.name).filter(|record| {
        record.pid == selected.pid
            && record.created_at_unix_ms == selected.created_at_unix_ms
            && record.socket == selected.endpoint
            && (!require_unattached || !record.attached)
    }).ok_or_else(|| PersistentSessionError::new(
        "The selected native session exited, changed, or attached elsewhere. Refresh Running Sessions and try again."
    ))?;
    if !record_is_live(root, record)? {
        return Err(PersistentSessionError::new(
            "The selected native daemon is no longer running. Refresh Running Sessions.",
        ));
    }
    connect_record_with_cancel(record, cancelled)
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
    let record = match registry.sessions.get(name) {
        Some(record) => record,
        None => {
            if let Some(declared) = registry.unreadable.get(name) {
                return Err(unreadable_record_error(name, *declared));
            }
            return Err(PersistentSessionError::new(format!(
                "no locally running session named '{name}' is registered"
            )));
        }
    };
    ensure_protocol_compatible(record)?;
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
    let registry = load_registry()?;
    if let Some(declared) = registry.unreadable.get(name) {
        // Starting a replacement would overwrite a record this build cannot
        // read, so an unreadable name is a refusal rather than a fresh shell.
        return Err(unreadable_record_error(name, *declared));
    }
    if let Some(record) = registry.sessions.get(name) {
        ensure_protocol_compatible(record)?;
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
    ensure_protocol_compatible(record)?;
    let _pid = record.pid;
    #[cfg(unix)]
    {
        let connect = || -> io::Result<UnixStream> {
            let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
            socket.connect_timeout(
                &socket2::SockAddr::unix(&record.socket)?,
                Duration::from_secs(2),
            )?;
            Ok(socket.into())
        };
        let stream = connect().map_err(|error| {
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

fn ensure_protocol_compatible(record: &SessionRecord) -> Result<(), PersistentSessionError> {
    if protocol_is_supported(record.protocol_version) {
        return Ok(());
    }
    Err(PersistentSessionError::new(format!(
        "session '{}' uses persistent-session protocol {}, but this fesTerm supports protocol {}; \
         keep using a compatible fesTerm version or terminate that session before replacing it",
        record.name, record.protocol_version, PROTOCOL_VERSION
    )))
}

/// Returns whether this client can attach to a daemon protocol epoch.
pub const fn protocol_is_supported(version: u16) -> bool {
    version >= MIN_SUPPORTED_PROTOCOL_VERSION && version <= PROTOCOL_VERSION
}

fn unreadable_record_error(name: &str, declared: Option<u16>) -> PersistentSessionError {
    match declared {
        Some(version) => PersistentSessionError::new(format!(
            "session '{name}' was registered by persistent-session protocol {version}, but this \
             fesTerm supports protocol {PROTOCOL_VERSION}; keep using a compatible fesTerm \
             version or terminate that session before replacing it"
        )),
        None => PersistentSessionError::new(format!(
            "session '{name}' has a registry record this fesTerm cannot read; keep using the \
             fesTerm version that created it or terminate that session before replacing it"
        )),
    }
}

fn daemon_executable() -> Result<PathBuf, PersistentSessionError> {
    let current = std::env::current_exe().map_err(|error| {
        PersistentSessionError::new(format!("could not locate fesTerm executable: {error}"))
    })?;
    let directory = current.parent().ok_or_else(|| {
        PersistentSessionError::new("fesTerm executable has no containing directory")
    })?;
    #[cfg(windows)]
    let packaged = resolve_windows_packaged_daemon(directory);
    #[cfg(not(windows))]
    let packaged = directory.join("festerm-sessiond");
    if !packaged.is_file() {
        return Err(PersistentSessionError::new(format!(
            "persistent-session helper is not installed beside fesTerm: {}",
            packaged.display()
        )));
    }

    #[cfg(windows)]
    {
        let root = runtime_root()?;
        let staged = stage_windows_daemon(&packaged, &root)?;
        prune_packaged_windows_daemons(directory, &packaged, &root)?;
        Ok(staged)
    }
    #[cfg(not(windows))]
    {
        Ok(packaged)
    }
}

/// Removes superseded Windows package helper sources when no legacy daemon
/// still owns the stable executable.
///
/// Development and non-Windows builds have no versioned package source and
/// therefore have nothing to clean.
pub fn cleanup_superseded_package_helpers() -> Result<(), PersistentSessionError> {
    #[cfg(windows)]
    {
        let executable = std::env::current_exe().map_err(|error| {
            PersistentSessionError::new(format!("could not locate fesTerm executable: {error}"))
        })?;
        let directory = executable.parent().ok_or_else(|| {
            PersistentSessionError::new("fesTerm executable has no containing directory")
        })?;
        let current = directory.join(windows_packaged_daemon_name());
        if current.is_file() {
            prune_packaged_windows_daemons(directory, &current, &runtime_root()?)?;
        }
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn windows_packaged_daemon_name() -> String {
    format!("festerm-sessiond-{}.exe", env!("CARGO_PKG_VERSION"))
}

#[cfg(any(windows, test))]
fn resolve_windows_packaged_daemon(directory: &Path) -> PathBuf {
    let versioned = directory.join(windows_packaged_daemon_name());
    if versioned.is_file() {
        versioned
    } else {
        directory.join("festerm-sessiond.exe")
    }
}

#[cfg(any(windows, test))]
/// Copies the packaged Windows helper to its immutable per-release runtime path.
///
/// Long-lived daemons execute this copy so an installer can replace the
/// package-owned source executable while compatible sessions remain alive.
///
/// The copy is a *directory* rather than a bare executable because the helper
/// needs more than its own image. ADR-0011 resolves the ConPTY sidecar at the
/// fixed path `runtime\conpty` relative to the canonical executable, so a
/// helper staged on its own finds no sidecar and silently falls back to the
/// inbox ConPTY - losing the resize fix that ADR-0011 exists for. Pointing the
/// helper back at the installed sidecar instead would keep `conpty.dll` mapped
/// out of the install directory for the lifetime of the daemon, which is
/// exactly the upgrade conflict this staging was introduced to avoid. Each
/// generation therefore gets its own complete, self-contained copy.
pub fn stage_windows_daemon(
    packaged: &Path,
    runtime_root: &Path,
) -> Result<PathBuf, PersistentSessionError> {
    let helpers = runtime_root.join("helpers");
    let identity = windows_helper_identity();
    let generation = helpers.join(helper_generation_directory(&identity));
    fs::create_dir_all(&generation).map_err(|error| {
        PersistentSessionError::new(format!(
            "could not create persistent-session helper directory '{}': {error}",
            generation.display()
        ))
    })?;
    let staged = generation.join(&identity);
    if staged.is_file() && files_match(packaged, &staged)? {
        stage_conpty_sidecar(packaged, &generation)?;
        prune_windows_daemons(&helpers, &identity)?;
        return Ok(staged);
    }
    if staged.is_file() {
        // A rebuild or a re-signed package can change the bytes without
        // changing the release identity. Replacing the copy is safe whenever
        // no live generation is still executing it; when one is, its image is
        // both locked and still needed, so refuse with actionable guidance.
        if helper_identity_is_live(runtime_root, &identity)? {
            return Err(PersistentSessionError::new(format!(
                "persistent-session helper '{}' differs from the packaged {} build and a live \
                 session is still running it; terminate sessions using that build before \
                 starting a new one",
                staged.display(),
                env!("CARGO_PKG_VERSION")
            )));
        }
    }

    let temporary = generation.join(format!(".{identity}.{}.tmp", std::process::id()));
    match fs::copy(packaged, &temporary) {
        Ok(_) => {}
        Err(error) => {
            return Err(PersistentSessionError::new(format!(
                "could not stage persistent-session helper '{}': {error}",
                staged.display()
            )))
        }
    }
    if let Err(error) = fs::rename(&temporary, &staged) {
        let _ = fs::remove_file(&temporary);
        if !(staged.is_file() && files_match(packaged, &staged)?) {
            return Err(PersistentSessionError::new(format!(
                "could not publish persistent-session helper '{}': {error}",
                staged.display()
            )));
        }
    }
    stage_conpty_sidecar(packaged, &generation)?;
    prune_windows_daemons(&helpers, &identity)?;
    Ok(staged)
}

#[cfg(any(windows, test))]
fn windows_helper_identity() -> String {
    format!(
        "festerm-sessiond-{}-{}.exe",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH
    )
}

/// The per-generation directory name for a helper identity.
///
/// The identity itself is unchanged and is still what the registry records, so
/// a daemon from a release that staged its helper as a bare file stays
/// recognisable - and therefore stays retained while it is alive.
#[cfg(any(windows, test))]
fn helper_generation_directory(identity: &str) -> &str {
    identity.strip_suffix(".exe").unwrap_or(identity)
}

/// Copies the verified ConPTY sidecar next to a staged helper.
///
/// The source is the directory holding the executable being staged: the
/// install directory when fesTerm stages the packaged helper, and the
/// generation directory itself when the staged helper re-stages on startup,
/// which makes the second call a no-op.
///
/// A missing source sidecar is not an error - an installation may legitimately
/// have none, and the helper then uses the inbox ConPTY exactly as fesTerm
/// does. A sidecar that exists but cannot be copied *is* an error: silently
/// downgrading a durable session to a different ConPTY implementation than the
/// one fesTerm itself selected is precisely the failure this staging prevents.
#[cfg(any(windows, test))]
fn stage_conpty_sidecar(packaged: &Path, generation: &Path) -> Result<(), PersistentSessionError> {
    let Some(source_root) = packaged.parent() else {
        return Ok(());
    };
    if source_root == generation {
        return Ok(());
    }

    for relative in festerm_windows_runtime::bundled_runtime_relative_paths() {
        let source = source_root.join(&relative);
        let destination = generation.join(&relative);
        if !source.is_file() {
            // An incomplete sidecar is the same as none: ADR-0011 requires the
            // matched pair, and the loader rejects a partial one anyway.
            return Ok(());
        }
        if destination.is_file() && files_match(&source, &destination)? {
            continue;
        }
        let parent = destination
            .parent()
            .expect("sidecar destination has a parent directory");
        fs::create_dir_all(parent).map_err(|error| {
            PersistentSessionError::new(format!(
                "could not create ConPTY sidecar directory '{}': {error}",
                parent.display()
            ))
        })?;
        let temporary = parent.join(format!(
            ".{}.{}.tmp",
            relative
                .file_name()
                .expect("sidecar path has a file name")
                .to_string_lossy(),
            std::process::id()
        ));
        fs::copy(&source, &temporary).map_err(|error| {
            PersistentSessionError::new(format!(
                "could not stage the ConPTY sidecar file '{}': {error}",
                destination.display()
            ))
        })?;
        if let Err(error) = fs::rename(&temporary, &destination) {
            let _ = fs::remove_file(&temporary);
            // A concurrent start may have published the same bytes first,
            // which is the one way this can fail harmlessly.
            if !(destination.is_file() && files_match(&source, &destination)?) {
                return Err(PersistentSessionError::new(format!(
                    "could not publish the ConPTY sidecar file '{}': {error}",
                    destination.display()
                )));
            }
        }
    }

    // The bytes were verified when fesTerm loaded them; this confirms the copy
    // that the daemon will actually load, rather than trusting that `copy`
    // returning success means the destination is intact.
    #[cfg(windows)]
    if festerm_windows_runtime::bundled_runtime_is_verified_in(source_root)
        && !festerm_windows_runtime::bundled_runtime_is_verified_in(generation)
    {
        return Err(PersistentSessionError::new(format!(
            "the ConPTY sidecar staged in '{}' does not match the pinned hashes; refusing to \
             start a durable session on a different ConPTY runtime than fesTerm selected",
            generation.display()
        )));
    }

    Ok(())
}

#[cfg(any(windows, test))]
fn files_match(left: &Path, right: &Path) -> Result<bool, PersistentSessionError> {
    let left_metadata = fs::metadata(left).map_err(|error| {
        PersistentSessionError::new(format!("could not inspect '{}': {error}", left.display()))
    })?;
    let right_metadata = fs::metadata(right).map_err(|error| {
        PersistentSessionError::new(format!("could not inspect '{}': {error}", right.display()))
    })?;
    if left_metadata.len() != right_metadata.len() {
        return Ok(false);
    }
    let mut left =
        File::open(left).map_err(|error| PersistentSessionError::new(error.to_string()))?;
    let mut right =
        File::open(right).map_err(|error| PersistentSessionError::new(error.to_string()))?;
    let mut left_buffer = [0_u8; 64 * 1024];
    let mut right_buffer = [0_u8; 64 * 1024];
    loop {
        // `read` may legally return a short chunk for reasons unrelated to
        // content, so each side is filled before the contents are compared.
        let left_read = fill(&mut left, &mut left_buffer)?;
        let right_read = fill(&mut right, &mut right_buffer)?;
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

#[cfg(any(windows, test))]
fn fill(file: &mut File, buffer: &mut [u8]) -> Result<usize, PersistentSessionError> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = file
            .read(&mut buffer[filled..])
            .map_err(|error| PersistentSessionError::new(error.to_string()))?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

/// Returns whether a live registry generation still executes a helper copy.
#[cfg(any(windows, test))]
fn helper_identity_is_live(
    runtime_root: &Path,
    identity: &str,
) -> Result<bool, PersistentSessionError> {
    for record in load_registry_in(runtime_root)?.sessions.values() {
        let matches = record
            .helper_identity
            .as_deref()
            .is_some_and(|recorded| recorded.eq_ignore_ascii_case(identity));
        if matches && record_is_live(runtime_root, record)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(any(windows, test))]
fn prune_windows_daemons(
    helpers: &Path,
    current_identity: &str,
) -> Result<(), PersistentSessionError> {
    let registry = load_registry_in(helpers.parent().ok_or_else(|| {
        PersistentSessionError::new("persistent-session helper directory has no runtime root")
    })?)?;
    let root = helpers.parent().expect("helper directory parent checked");
    let mut retained = std::collections::BTreeSet::new();
    for record in registry.sessions.values() {
        if record_is_live(root, record)? {
            if let Some(identity) = record.helper_identity.as_deref() {
                retained.insert(identity.to_ascii_lowercase());
            }
        }
    }
    for entry in
        fs::read_dir(helpers).map_err(|error| PersistentSessionError::new(error.to_string()))?
    {
        let entry = entry.map_err(|error| PersistentSessionError::new(error.to_string()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("festerm-sessiond-") {
            continue;
        }
        // A generation directory is named for its identity without the
        // extension; releases before the sidecar was staged alongside the
        // helper left bare executables here instead.
        let is_generation_directory = entry.path().is_dir();
        let identity = if is_generation_directory {
            format!("{name}.exe")
        } else if name.ends_with(".exe") {
            name.to_string()
        } else {
            continue;
        };
        // Windows compares file names case-insensitively, so retention must
        // too or a live generation's image becomes a deletion candidate.
        if identity.eq_ignore_ascii_case(current_identity)
            || retained.contains(&identity.to_ascii_lowercase())
        {
            continue;
        }
        let removed = if is_generation_directory {
            fs::remove_dir_all(entry.path())
        } else {
            fs::remove_file(entry.path())
        };
        match removed {
            Ok(()) => {}
            // A start helper may be executing before its daemon publishes the
            // registry record. Preserve that locked image and retry pruning
            // on the next launch rather than making the concurrent start fail.
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                tracing::debug!(
                    helper = %entry.path().display(),
                    "leaving a locked persistent-session helper for a later launch"
                );
            }
            Err(error) => {
                return Err(PersistentSessionError::new(format!(
                    "could not remove stale persistent-session helper '{}': {error}",
                    entry.path().display()
                )))
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn prune_packaged_windows_daemons(
    directory: &Path,
    current: &Path,
    runtime_root: &Path,
) -> Result<(), PersistentSessionError> {
    let registry = load_registry_in(runtime_root)?;
    let mut live_legacy_daemon = false;
    for record in registry.sessions.values() {
        let legacy = record
            .helper_identity
            .as_deref()
            .is_none_or(|identity| identity.eq_ignore_ascii_case("festerm-sessiond.exe"));
        if legacy && record_is_live(runtime_root, record)? {
            live_legacy_daemon = true;
            break;
        }
    }

    for entry in
        fs::read_dir(directory).map_err(|error| PersistentSessionError::new(error.to_string()))?
    {
        let entry = entry.map_err(|error| PersistentSessionError::new(error.to_string()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| PersistentSessionError::new(error.to_string()))?;
        if path == current || !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let legacy = name.eq_ignore_ascii_case("festerm-sessiond.exe");
        let versioned = name.starts_with("festerm-sessiond-") && name.ends_with(".exe");
        if !versioned && !legacy {
            continue;
        }
        if legacy && live_legacy_daemon {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            // A process can appear between the registry snapshot and deletion.
            // A reboot or later launch releases/retries this obsolete image.
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                tracing::debug!(
                    helper = %path.display(),
                    "leaving a locked superseded package helper for a later launch"
                );
            }
            Err(error) => {
                return Err(PersistentSessionError::new(format!(
                    "could not remove superseded packaged session helper '{}': {error}",
                    path.display()
                )))
            }
        }
    }
    Ok(())
}

fn load_registry() -> Result<SessionRegistry, PersistentSessionError> {
    let root = runtime_root()?;
    load_registry_in(&root)
}

fn load_registry_in(root: &std::path::Path) -> Result<SessionRegistry, PersistentSessionError> {
    load_registry_in_with_cancel(root, &AtomicBool::new(false))
}

fn load_registry_in_with_cancel(
    root: &std::path::Path,
    cancelled: &AtomicBool,
) -> Result<SessionRegistry, PersistentSessionError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(PersistentSessionError::new("session connection cancelled"));
    }
    let lock_path = root.join("registry.lock");
    let registry_path = root.join("registry.json");
    let lock = match OpenOptions::new().read(true).open(&lock_path) {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return read_registry(&registry_path)
        }
        Err(error) => return Err(PersistentSessionError::new(error.to_string())),
    };
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(PersistentSessionError::new("session connection cancelled"));
        }
        match FileExt::try_lock_shared(&lock) {
            Ok(()) => break,
            Err(error) if lock_is_contended(&error) && Instant::now() < deadline => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(error) => {
                return Err(PersistentSessionError::new(format!(
                    "session registry unavailable (lock deadline 500 ms): {error}"
                )))
            }
        }
    }
    let registry = read_registry(&registry_path);
    FileExt::unlock(&lock).map_err(|error| PersistentSessionError::new(error.to_string()))?;
    registry
}

fn read_registry(path: &PathBuf) -> Result<SessionRegistry, PersistentSessionError> {
    let bytes = std::fs::File::open(path).and_then(|file| {
        let mut bytes = Vec::new();
        file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(io::Error::other("session registry exceeds 4 MiB"));
        }
        Ok(bytes)
    });
    match bytes {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            PersistentSessionError::new(format!("could not parse session registry: {error}"))
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SessionRegistry::default()),
        Err(error) => Err(PersistentSessionError::new(error.to_string())),
    }
}

pub fn runtime_root() -> Result<PathBuf, PersistentSessionError> {
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

    pub(super) fn wait_for_lifecycle(
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

    struct RegistryFixture(PathBuf);
    impl RegistryFixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::current_dir().unwrap().join(format!(
                ".registry-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for RegistryFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn registry_absence_corruption_and_lock_contention_have_distinct_bounded_results() {
        let fixture = RegistryFixture::new();
        assert!(list_unattached_sessions_in(&fixture.0).unwrap().is_empty());
        std::fs::write(fixture.0.join("registry.json"), b"broken").unwrap();
        assert!(list_unattached_sessions_in(&fixture.0)
            .unwrap_err()
            .to_string()
            .contains("parse"));
        let lock = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(fixture.0.join("registry.lock"))
            .unwrap();
        lock.lock_exclusive().unwrap();
        let started = Instant::now();
        assert!(list_unattached_sessions_in(&fixture.0)
            .unwrap_err()
            .to_string()
            .contains("lock deadline"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn discovered_reconnect_after_eof_keeps_root_and_rejects_each_changed_identity() {
        use super::tests::wait_for_lifecycle;
        use std::os::unix::net::UnixListener;

        let fixture = RegistryFixture::new();
        let pid = std::process::id();
        // A relative endpoint stays within Unix socket path limits while
        // the explicitly selected registry remains an absolute path.
        let endpoint =
            PathBuf::from(fixture.0.file_name().unwrap()).join(format!("{pid}-123.sock"));
        let listener = UnixListener::bind(&endpoint).unwrap();
        listener.set_nonblocking(true).unwrap();
        let lease = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(fixture.0.join(format!("lease-{pid}-123")))
            .unwrap();
        lease.lock_exclusive().unwrap();
        let original = serde_json::json!({
            "name":"demo", "pid":pid, "socket":endpoint,
            "created_at_unix_ms":123, "attached":false
        });
        let write = |record: &serde_json::Value| {
            std::fs::write(
                fixture.0.join("registry.json"),
                serde_json::to_vec(&serde_json::json!({"sessions":{"demo":record}})).unwrap(),
            )
            .unwrap();
        };
        write(&original);
        let selected = list_unattached_sessions_in(&fixture.0).unwrap().remove(0);
        let session = PersistentSession::resume_discovered_in(
            &selected,
            &fixture.0,
            noop_session_event_notifier(),
        )
        .unwrap();
        let id = session.id();
        let (server, _) = listener.accept().unwrap();
        wait_for_lifecycle(&session, |state| matches!(state, SessionLifecycle::Running));
        drop(server);
        wait_for_lifecycle(&session, |state| {
            matches!(state, SessionLifecycle::Disconnected(_))
        });
        let mut attached = original.clone();
        attached["attached"] = true.into();
        write(&attached);
        session.try_reconnect().unwrap();
        wait_for_lifecycle(&session, |state| matches!(state, SessionLifecycle::Running));
        let (server, _) = listener.accept().unwrap();
        assert_eq!(session.id(), id);
        drop(server);
        wait_for_lifecycle(&session, |state| {
            matches!(state, SessionLifecycle::Disconnected(_))
        });

        for (field, replacement) in [
            ("pid", serde_json::json!(pid + 1)),
            ("created_at_unix_ms", serde_json::json!(124)),
            ("socket", serde_json::json!("replacement.sock")),
        ] {
            let mut replaced = original.clone();
            replaced[field] = replacement;
            write(&replaced);
            session.try_reconnect().unwrap();
            wait_for_lifecycle(
                &session,
                |state| matches!(state, SessionLifecycle::Disconnected(error) if error.message().contains("changed")),
            );
            assert!(format!("{:?}", session.lifecycle()).contains("changed"));
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
        write(&original);
        FileExt::unlock(&lease).unwrap();
        session.try_reconnect().unwrap();
        wait_for_lifecycle(
            &session,
            |state| matches!(state, SessionLifecycle::Disconnected(error) if error.message().contains("no longer running")),
        );
        assert!(format!("{:?}", session.lifecycle()).contains("no longer running"));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn selected_registry_load_honors_cancellation_before_reading_or_waiting() {
        let fixture = RegistryFixture::new();
        std::fs::write(fixture.0.join("registry.json"), b"broken").unwrap();
        let cancelled = AtomicBool::new(true);
        assert!(load_registry_in_with_cancel(&fixture.0, &cancelled)
            .err()
            .unwrap()
            .to_string()
            .contains("cancelled"));
        let lock = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(fixture.0.join("registry.lock"))
            .unwrap();
        lock.lock_exclusive().unwrap();
        assert!(load_registry_in_with_cancel(&fixture.0, &cancelled)
            .err()
            .unwrap()
            .to_string()
            .contains("cancelled"));
    }

    #[test]
    fn generation_lease_excludes_stale_pid_records_and_attached_native_sessions() {
        let fixture = RegistryFixture::new();
        let pid = std::process::id();
        let endpoint = fixture.0.join(format!("{pid}-123.sock"));
        std::fs::write(&endpoint, b"").unwrap();
        let lease = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(fixture.0.join(format!("lease-{pid}-123")))
            .unwrap();
        let write = |attached| {
            let registry = serde_json::json!({"sessions":{"demo":{"name":"demo","pid":pid,"socket":endpoint,"shell":"test-shell","created_at_unix_ms":123,"attached":attached}}});
            std::fs::write(
                fixture.0.join("registry.json"),
                serde_json::to_vec(&registry).unwrap(),
            )
            .unwrap();
        };
        write(false);
        assert!(
            list_unattached_sessions_in(&fixture.0).unwrap().is_empty(),
            "a reused live PID is not a live daemon lease"
        );
        lease.lock_exclusive().unwrap();
        let sessions = list_unattached_sessions_in(&fixture.0).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].pid, pid);
        assert_eq!(sessions[0].shell, "test-shell");
        write(true);
        assert!(list_unattached_sessions_in(&fixture.0).unwrap().is_empty());
        write(false);
        FileExt::unlock(&lease).unwrap();
        assert!(list_unattached_sessions_in(&fixture.0).unwrap().is_empty());
        assert!(PersistentSession::resume_discovered_in(
            &sessions[0],
            &fixture.0,
            noop_session_event_notifier()
        )
        .is_err());
    }

    #[test]
    fn registry_round_trip_ignores_unknown_fields_and_defaults_new_ones() {
        let json = r#"{"sessions":{"demo":{"pid":42,"socket":"demo.sock"}}}"#;
        let registry: SessionRegistry = serde_json::from_str(json).unwrap();
        let record = registry.sessions.get("demo").unwrap();
        assert_eq!(record.pid, 42);
        assert_eq!(record.socket, "demo.sock");
        assert_eq!(record.name, "");
        assert!(!record.attached);
        assert_eq!(record.protocol_version, PROTOCOL_VERSION);
        assert!(record.helper_identity.is_none());
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
            protocol_version: PROTOCOL_VERSION,
            helper_identity: Some("festerm-sessiond-0.2.2.exe".to_owned()),
        };
        let mut registry = SessionRegistry::default();
        registry.sessions.insert(record.name.clone(), record);

        let unattached: Vec<_> = registry
            .sessions
            .into_values()
            .filter(|record| !record.attached)
            .map(|record| UnattachedSession {
                pid: record.pid,
                endpoint: record.socket,
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
                protocol_version: PROTOCOL_VERSION,
                helper_identity: None,
            },
        );

        let unattached: Vec<_> = registry
            .sessions
            .into_values()
            .filter(|record| !record.attached)
            .collect();
        assert!(unattached.is_empty());
    }

    #[test]
    fn incompatible_registry_record_is_rejected_before_connecting() {
        let record = SessionRecord {
            name: "future".to_owned(),
            pid: 1,
            socket: "future.sock".to_owned(),
            shell: String::new(),
            arguments: Vec::new(),
            working_directory: None,
            created_at_unix_ms: 0,
            attached: false,
            protocol_version: PROTOCOL_VERSION + 1,
            helper_identity: None,
        };
        let registry = SessionRegistry {
            sessions: BTreeMap::from([(record.name.clone(), record)]),
            ..Default::default()
        };
        let error = connect_existing_in_registry(&registry, "future")
            .err()
            .expect("incompatible protocol must fail before connecting")
            .to_string();
        assert!(error.contains("protocol 2"));
        assert!(error.contains("supports protocol 1"));
        assert!(error.contains("compatible fesTerm version"));
    }

    #[test]
    fn windows_helper_staging_is_versioned_and_prunes_only_unreferenced_builds() {
        let fixture = RegistryFixture::new();
        let packaged = fixture.0.join("festerm-sessiond.exe");
        std::fs::write(&packaged, b"signed helper bytes").unwrap();
        let helpers = fixture.0.join("helpers");
        std::fs::create_dir(&helpers).unwrap();
        let retained = helpers.join("festerm-sessiond-retained.exe");
        let stale = helpers.join("festerm-sessiond-stale.exe");
        std::fs::write(&retained, b"retained").unwrap();
        std::fs::write(&stale, b"stale").unwrap();
        let endpoint = fixture.0.join("legacy-endpoint");
        std::fs::write(&endpoint, b"live endpoint").unwrap();
        let registry = serde_json::json!({
            "sessions": {
                "legacy-live": {
                    "name": "legacy-live",
                    "pid": std::process::id(),
                    "socket": endpoint.to_string_lossy(),
                    "helper_identity": "festerm-sessiond-retained.exe"
                }
            }
        });
        std::fs::write(
            fixture.0.join("registry.json"),
            serde_json::to_vec(&registry).unwrap(),
        )
        .unwrap();

        let staged = stage_windows_daemon(&packaged, &fixture.0).unwrap();
        let identity = windows_helper_identity();
        assert_eq!(staged.file_name().unwrap().to_string_lossy(), identity);
        // The helper lives in its own generation directory so that the ConPTY
        // sidecar it loads cannot be shared with, or replaced by, another
        // generation's.
        assert_eq!(
            staged.parent().unwrap().file_name().unwrap(),
            helper_generation_directory(&identity)
        );
        assert_eq!(std::fs::read(staged).unwrap(), b"signed helper bytes");
        assert!(retained.is_file());
        assert!(!stale.exists());
    }

    #[test]
    fn the_conpty_sidecar_is_staged_beside_the_helper_that_loads_it() {
        let fixture = RegistryFixture::new();
        let packaged = fixture.0.join("festerm-sessiond.exe");
        std::fs::write(&packaged, b"signed helper bytes").unwrap();
        let sidecar = festerm_windows_runtime::bundled_runtime_relative_paths();
        assert!(
            !sidecar.is_empty(),
            "ADR-0011 pins at least one sidecar file"
        );
        for (index, relative) in sidecar.iter().enumerate() {
            let source = fixture.0.join(relative);
            std::fs::create_dir_all(source.parent().unwrap()).unwrap();
            std::fs::write(&source, format!("sidecar {index}")).unwrap();
        }

        let staged = stage_windows_daemon(&packaged, &fixture.0).unwrap();

        // ADR-0011 resolves the sidecar relative to the running executable, so
        // these paths are exactly what the staged helper will load.
        let generation = staged.parent().unwrap();
        for (index, relative) in sidecar.iter().enumerate() {
            assert_eq!(
                std::fs::read(generation.join(relative)).unwrap(),
                format!("sidecar {index}").into_bytes(),
                "{} must be staged with the helper",
                relative.display()
            );
        }
    }

    #[test]
    fn an_installation_without_a_sidecar_stages_the_helper_anyway() {
        // Development builds and installations that predate the sidecar have
        // none; the helper then uses the inbox ConPTY exactly as fesTerm does,
        // which ADR-0011 defines as a safe fallback.
        let fixture = RegistryFixture::new();
        let packaged = fixture.0.join("festerm-sessiond.exe");
        std::fs::write(&packaged, b"signed helper bytes").unwrap();

        let staged = stage_windows_daemon(&packaged, &fixture.0).unwrap();

        assert!(staged.is_file());
        let generation = staged.parent().unwrap();
        for relative in festerm_windows_runtime::bundled_runtime_relative_paths() {
            assert!(!generation.join(relative).exists());
        }
    }

    #[test]
    fn re_staging_an_already_staged_helper_is_a_no_op() {
        // The daemon re-stages from its own `current_exe` on startup, so the
        // source and destination directories are the same file set.
        let fixture = RegistryFixture::new();
        let packaged = fixture.0.join("festerm-sessiond.exe");
        std::fs::write(&packaged, b"signed helper bytes").unwrap();
        for relative in festerm_windows_runtime::bundled_runtime_relative_paths() {
            let source = fixture.0.join(&relative);
            std::fs::create_dir_all(source.parent().unwrap()).unwrap();
            std::fs::write(&source, b"sidecar").unwrap();
        }
        let staged = stage_windows_daemon(&packaged, &fixture.0).unwrap();

        let restaged = stage_windows_daemon(&staged, &fixture.0).unwrap();

        assert_eq!(restaged, staged);
        for relative in festerm_windows_runtime::bundled_runtime_relative_paths() {
            assert_eq!(
                std::fs::read(staged.parent().unwrap().join(relative)).unwrap(),
                b"sidecar"
            );
        }
    }

    #[test]
    fn a_stale_generation_directory_is_pruned_while_a_live_one_is_kept() {
        let fixture = RegistryFixture::new();
        let packaged = fixture.0.join("festerm-sessiond.exe");
        std::fs::write(&packaged, b"signed helper bytes").unwrap();
        let helpers = fixture.0.join("helpers");
        let live = helpers.join("festerm-sessiond-0.2.0-x86_64");
        let stale = helpers.join("festerm-sessiond-0.1.0-x86_64");
        for generation in [&live, &stale] {
            std::fs::create_dir_all(generation.join("runtime/conpty")).unwrap();
            std::fs::write(generation.join("runtime/conpty/conpty.dll"), b"dll").unwrap();
        }
        let endpoint = fixture.0.join("live-endpoint");
        std::fs::write(&endpoint, b"live endpoint").unwrap();
        let registry = serde_json::json!({
            "sessions": {
                "live": {
                    "name": "live",
                    "pid": std::process::id(),
                    "socket": endpoint.to_string_lossy(),
                    // Recorded identities keep the executable name, so an
                    // older daemon stays recognisable to a newer pruner.
                    "helper_identity": "festerm-sessiond-0.2.0-x86_64.exe"
                }
            }
        });
        std::fs::write(
            fixture.0.join("registry.json"),
            serde_json::to_vec(&registry).unwrap(),
        )
        .unwrap();

        stage_windows_daemon(&packaged, &fixture.0).unwrap();

        assert!(live.is_dir(), "a live generation keeps its whole directory");
        assert!(!stale.exists(), "an unreferenced generation is removed");
    }

    #[test]
    fn windows_packaged_helper_prefers_the_immutable_release_name() {
        let fixture = RegistryFixture::new();
        let legacy = fixture.0.join("festerm-sessiond.exe");
        let versioned = fixture.0.join(windows_packaged_daemon_name());
        std::fs::write(&legacy, b"legacy").unwrap();
        assert_eq!(resolve_windows_packaged_daemon(&fixture.0), legacy);

        std::fs::write(&versioned, b"current").unwrap();
        assert_eq!(resolve_windows_packaged_daemon(&fixture.0), versioned);
    }

    #[cfg(windows)]
    #[test]
    fn windows_packaged_helper_cleanup_waits_for_legacy_daemon_exit() {
        let fixture = RegistryFixture::new();
        let package_directory = fixture.0.join("package");
        std::fs::create_dir(&package_directory).unwrap();
        let current = package_directory.join(windows_packaged_daemon_name());
        let superseded = package_directory.join("festerm-sessiond-0.2.0.exe");
        let legacy = package_directory.join("festerm-sessiond.exe");
        std::fs::write(&current, b"current").unwrap();
        std::fs::write(&superseded, b"superseded").unwrap();
        std::fs::write(&legacy, b"legacy").unwrap();
        let live_registry = serde_json::json!({
            "sessions": {
                "legacy-live": {
                    "name": "legacy-live",
                    "pid": std::process::id(),
                    "socket": "legacy-endpoint"
                }
            }
        });
        std::fs::write(
            fixture.0.join("registry.json"),
            serde_json::to_vec(&live_registry).unwrap(),
        )
        .unwrap();

        prune_packaged_windows_daemons(&package_directory, &current, &fixture.0).unwrap();
        assert!(current.is_file());
        assert!(legacy.is_file());
        assert!(!superseded.exists());

        std::fs::write(fixture.0.join("registry.json"), br#"{"sessions":{}}"#).unwrap();
        prune_packaged_windows_daemons(&package_directory, &current, &fixture.0).unwrap();
        assert!(current.is_file());
        assert!(!legacy.exists());
    }

    #[test]
    fn a_rebuilt_helper_replaces_an_unreferenced_staged_copy() {
        let fixture = RegistryFixture::new();
        let packaged = fixture.0.join("festerm-sessiond.exe");
        std::fs::write(&packaged, b"second build").unwrap();
        let helpers = fixture.0.join("helpers");
        let identity = windows_helper_identity();
        let generation = helpers.join(helper_generation_directory(&identity));
        std::fs::create_dir_all(&generation).unwrap();
        std::fs::write(generation.join(&identity), b"first build").unwrap();

        let staged = stage_windows_daemon(&packaged, &fixture.0).unwrap();
        assert_eq!(std::fs::read(staged).unwrap(), b"second build");
    }

    #[test]
    fn a_rebuilt_helper_is_refused_while_a_live_session_still_runs_the_staged_copy() {
        let fixture = RegistryFixture::new();
        let packaged = fixture.0.join("festerm-sessiond.exe");
        std::fs::write(&packaged, b"second build").unwrap();
        let helpers = fixture.0.join("helpers");
        let identity = windows_helper_identity();
        let generation = helpers.join(helper_generation_directory(&identity));
        std::fs::create_dir_all(&generation).unwrap();
        std::fs::write(generation.join(&identity), b"first build").unwrap();
        let endpoint = fixture.0.join("live-endpoint");
        std::fs::write(&endpoint, b"live endpoint").unwrap();
        let registry = serde_json::json!({
            "sessions": {
                "live": {
                    "name": "live",
                    "pid": std::process::id(),
                    "socket": endpoint.to_string_lossy(),
                    "helper_identity": identity
                }
            }
        });
        std::fs::write(
            fixture.0.join("registry.json"),
            serde_json::to_vec(&registry).unwrap(),
        )
        .unwrap();

        let error = stage_windows_daemon(&packaged, &fixture.0)
            .expect_err("a live generation must keep its own helper image")
            .to_string();
        assert!(error.contains("a live session is still running it"));
        assert_eq!(
            std::fs::read(generation.join(&identity)).unwrap(),
            b"first build"
        );
    }

    #[test]
    fn byte_identical_helpers_match_across_read_chunk_boundaries() {
        let fixture = RegistryFixture::new();
        let left = fixture.0.join("left.bin");
        let right = fixture.0.join("right.bin");
        let bytes: Vec<u8> = (0..(192 * 1024 + 7)).map(|index| index as u8).collect();
        std::fs::write(&left, &bytes).unwrap();
        std::fs::write(&right, &bytes).unwrap();
        assert!(files_match(&left, &right).unwrap());

        let mut divergent = bytes.clone();
        *divergent.last_mut().unwrap() ^= 0xff;
        std::fs::write(&right, &divergent).unwrap();
        assert!(!files_match(&left, &right).unwrap());
    }

    #[test]
    fn an_unreadable_record_costs_only_its_own_session() {
        let registry: SessionRegistry = serde_json::from_value(serde_json::json!({
            "sessions": {
                "readable": {
                    "name": "readable",
                    "pid": 1,
                    "socket": "readable.sock"
                },
                "future": {
                    "name": "future",
                    "socket": { "transport": "quic" },
                    "protocol_version": 9
                }
            }
        }))
        .expect("one unreadable record must not fail the whole registry");
        assert!(registry.sessions.contains_key("readable"));
        assert_eq!(registry.unreadable.get("future"), Some(&Some(9)));

        let error = connect_existing_in_registry(&registry, "future")
            .err()
            .expect("an unreadable record must not look like a missing session")
            .to_string();
        assert!(error.contains("protocol 9"));
        assert!(error.contains("supports protocol 1"));
    }

    #[test]
    fn an_unreadable_record_without_an_epoch_still_explains_itself() {
        let registry: SessionRegistry = serde_json::from_value(serde_json::json!({
            "sessions": { "future": { "socket": 42 } }
        }))
        .unwrap();
        assert_eq!(registry.unreadable.get("future"), Some(&None));
        let error = connect_existing_in_registry(&registry, "future")
            .err()
            .expect("an unreadable record must fail before connecting")
            .to_string();
        assert!(error.contains("cannot read"));
    }
}
