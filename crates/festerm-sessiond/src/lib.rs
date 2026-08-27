//! Client backend for fesTerm's native local session persistence daemon.

use std::{
    collections::BTreeMap,
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
const FRAME_MAGIC: &[u8; 4] = b"FSD1";
const FRAME_INPUT: u8 = 1;
const FRAME_RESIZE: u8 = 2;
const MAX_FRAME_BYTES: usize = 64 * 1024;
const STOLEN_NOTICE_BYTES: &[u8] =
    b"\n[festerm-sessiond] SESSION_STOLEN: reattached from another client\n";
const EXITED_NOTICE_BYTES: &[u8] = b"\n[festerm-sessiond] SESSION_EXITED\n";

trait SessionStream: Read + Write + Send {}
impl<T: Read + Write + Send> SessionStream for T {}

#[derive(Clone, Debug, Deserialize)]
struct SessionRecord {
    pid: u32,
    socket: String,
}

#[derive(Default, Deserialize)]
struct SessionRegistry {
    #[serde(default)]
    sessions: BTreeMap<String, SessionRecord>,
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
        loop {
            match self.events.try_send(event.clone()) {
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
                    return;
                }
                Err(TrySendError::Full(_)) => {
                    if self.cancelled.load(Ordering::Acquire) {
                        return;
                    }
                    thread::sleep(POLL_INTERVAL);
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
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
    commands: SyncSender<SessionCommand>,
    events: Mutex<Receiver<SessionEvent>>,
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
        Self::from_stream(stream, notifier)
    }

    fn from_stream(
        stream: Box<dyn SessionStream>,
        notifier: Arc<dyn SessionEventNotifier>,
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
            completion: Mutex::new(None),
            completion_receiver: Mutex::new(completion_rx),
        });
        shared.set_lifecycle(SessionLifecycle::Starting);

        let worker_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name(format!("festerm-sessiond-client-{}", shared.id))
            .spawn(move || client_worker(worker_shared, stream, commands_rx, completion_tx))
            .map_err(|error| {
                PersistentSessionError::new(format!(
                    "could not start persistent-session worker: {error}"
                ))
            })?;

        Ok(Self {
            shared,
            commands: commands_tx,
            events: Mutex::new(events_rx),
        })
    }
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
        send_command(
            &self.commands,
            SessionCommand::Input(bytes.to_vec()),
            SessionOperation::Input,
        )
    }

    fn try_resize(&self, size: TerminalSize) -> Result<(), SessionSendError> {
        send_command(
            &self.commands,
            SessionCommand::Resize(size),
            SessionOperation::Resize,
        )
    }

    fn try_shutdown(&self) -> Result<(), SessionSendError> {
        self.shared.cancelled.store(true, Ordering::Release);
        send_command(
            &self.commands,
            SessionCommand::Shutdown,
            SessionOperation::Shutdown,
        )
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

fn client_worker(
    shared: Arc<Shared>,
    mut stream: Box<dyn SessionStream>,
    commands: Receiver<SessionCommand>,
    completion: SyncSender<ShutdownResult>,
) {
    shared.set_lifecycle(SessionLifecycle::Running);
    let mut scanner = OutputScanner::default();
    let mut buffer = [0u8; 4096];
    loop {
        loop {
            match commands.try_recv() {
                Ok(SessionCommand::Input(bytes)) => {
                    if let Err(error) = write_frame(&mut stream, FRAME_INPUT, &bytes) {
                        fail_transport(&shared, SessionErrorKind::Input, error);
                        let _ = completion.send(ShutdownResult::Stopped);
                        return;
                    }
                    shared
                        .metrics
                        .lock()
                        .expect("persistent session metrics lock is not poisoned")
                        .input_bytes += bytes.len() as u64;
                }
                Ok(SessionCommand::Resize(size)) => {
                    if let Err(error) = write_resize(&mut stream, size) {
                        fail_transport(&shared, SessionErrorKind::Resize, error);
                        let _ = completion.send(ShutdownResult::Stopped);
                        return;
                    }
                    shared
                        .metrics
                        .lock()
                        .expect("persistent session metrics lock is not poisoned")
                        .resize_count += 1;
                    shared.send_event(SessionEvent::ResizeApplied(size));
                }
                Ok(SessionCommand::Shutdown) => {
                    shared.set_lifecycle(SessionLifecycle::Stopped);
                    let _ = completion.send(ShutdownResult::Stopped);
                    return;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
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
                ScanResult::Output(output) => send_output(&shared, output),
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
                ) => {}
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

fn connect_or_start(
    name: &str,
    profile: &LocalProfile,
    size: TerminalSize,
) -> Result<Box<dyn SessionStream>, PersistentSessionError> {
    if let Some(record) = load_registry()?.sessions.get(name) {
        if let Ok(stream) = connect_record(record, size) {
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
    append_environment_options(&mut command, profile.environment())?;
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
            if let Ok(stream) = connect_record(record, size) {
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
    size: TerminalSize,
) -> Result<Box<dyn SessionStream>, PersistentSessionError> {
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
        let mut stream: Box<dyn SessionStream> = Box::new(stream);
        write_resize(&mut stream, size).map_err(|error| {
            PersistentSessionError::new(format!(
                "could not initialize persistent session geometry: {error}"
            ))
        })?;
        Ok(stream)
    }

    #[cfg(windows)]
    {
        let mut stream = named_pipe::PipeClient::connect(&record.socket).map_err(|error| {
            PersistentSessionError::new(format!("could not connect to session daemon: {error}"))
        })?;
        stream.set_read_timeout(Some(POLL_INTERVAL));
        stream.set_write_timeout(Some(WRITE_TIMEOUT));
        let mut stream: Box<dyn SessionStream> = Box::new(stream);
        write_resize(&mut stream, size).map_err(|error| {
            PersistentSessionError::new(format!(
                "could not initialize persistent session geometry: {error}"
            ))
        })?;
        Ok(stream)
    }
}

fn append_environment_options(
    command: &mut Command,
    policy: &EnvironmentPolicy,
) -> Result<(), PersistentSessionError> {
    let (policy_name, environment) = match policy {
        EnvironmentPolicy::Inherit => return Ok(()),
        EnvironmentPolicy::Clear(environment) => ("clear", environment),
        EnvironmentPolicy::InheritWith(environment) => ("inherit-with", environment),
    };
    command.arg("--env-policy").arg(policy_name);
    for (key, value) in environment {
        let key = key.to_str().ok_or_else(|| {
            PersistentSessionError::new(
                "persistent local session environment keys must be valid Unicode",
            )
        })?;
        let value = value.to_str().ok_or_else(|| {
            PersistentSessionError::new(
                "persistent local session environment values must be valid Unicode",
            )
        })?;
        command.arg("--env").arg(key).arg(value);
    }
    Ok(())
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

fn write_frame<W: Write + ?Sized>(writer: &mut W, kind: u8, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session command exceeds the protocol limit",
        ));
    }
    writer.write_all(FRAME_MAGIC)?;
    writer.write_all(&[kind])?;
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)?;
    #[cfg(not(windows))]
    writer.flush()?;
    Ok(())
}

fn write_resize<W: Write + ?Sized>(writer: &mut W, size: TerminalSize) -> io::Result<()> {
    let mut payload = Vec::with_capacity(8);
    for value in [
        size.columns(),
        size.rows(),
        size.pixel_width().unwrap_or(0),
        size.pixel_height().unwrap_or(0),
    ] {
        payload.extend_from_slice(&value.to_be_bytes());
    }
    write_frame(writer, FRAME_RESIZE, &payload)
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

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
    fn daemon_command_carries_explicit_environment_policy() {
        let policy = EnvironmentPolicy::InheritWith(std::collections::BTreeMap::from([
            ("LANG".into(), "en_US.UTF-8".into()),
            ("PATH".into(), "/opt/homebrew/bin:/usr/bin".into()),
        ]));
        let mut command = Command::new("festerm-sessiond");

        append_environment_options(&mut command, &policy).unwrap();

        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            arguments,
            [
                "--env-policy",
                "inherit-with",
                "--env",
                "LANG",
                "en_US.UTF-8",
                "--env",
                "PATH",
                "/opt/homebrew/bin:/usr/bin",
            ]
        );
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
