//! Local-shell session backend built on [`portable_pty`].
//!
//! `portable-pty` selects Unix PTYs on Unix platforms and Windows ConPTY on
//! Windows. This crate never imports or mutates `festerm-core`; callers drain
//! [`SessionEvent`] values and decide when to ingest terminal bytes.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fmt,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

#[cfg(test)]
use std::time::Instant;

use festerm_session::{
    noop_session_event_notifier, FlowDirection, Session, SessionError, SessionErrorKind,
    SessionEvent, SessionEventNotifier, SessionExit, SessionId, SessionLifecycle, SessionMetrics,
    SessionOperation, SessionSendError, SessionTryReceiveError, ShutdownError, ShutdownResult,
    TerminalSize, DEFAULT_COMMAND_QUEUE_CAPACITY, DEFAULT_EVENT_QUEUE_CAPACITY, MAX_IO_CHUNK_BYTES,
};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::signal::{kill, Signal},
    unistd::Pid,
};

#[cfg(windows)]
use festerm_windows_job::WindowsJob;
#[cfg(windows)]
pub use festerm_windows_runtime::{
    prepare_conpty_runtime as prepare_windows_conpty_runtime, ConptyRuntimeError,
    ConptyRuntimeSelection,
};

const POLL_INTERVAL: Duration = Duration::from_millis(20);
const BACKPRESSURE_RETRY: Duration = Duration::from_millis(5);
const READER_STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// How the child process environment is established.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum EnvironmentPolicy {
    /// Preserve the environment inherited by fesTerm.
    #[default]
    Inherit,
    /// Start with an empty environment, then add these explicit variables.
    Clear(BTreeMap<OsString, OsString>),
    /// Preserve the inherited environment and override or add these variables.
    InheritWith(BTreeMap<OsString, OsString>),
}

/// An explicit local command launch configuration.
///
/// Arguments are passed directly to the spawned executable. This type does
/// not interpret a shell command string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalProfile {
    executable: PathBuf,
    arguments: Vec<OsString>,
    working_directory: Option<PathBuf>,
    environment: EnvironmentPolicy,
}

impl LocalProfile {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            arguments: Vec::new(),
            working_directory: None,
            environment: EnvironmentPolicy::Inherit,
        }
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    pub fn working_directory(&self) -> Option<&Path> {
        self.working_directory.as_deref()
    }

    pub fn environment(&self) -> &EnvironmentPolicy {
        &self.environment
    }

    pub fn with_arguments<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.arguments = arguments.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_working_directory(mut self, working_directory: impl Into<PathBuf>) -> Self {
        self.working_directory = Some(working_directory.into());
        self
    }

    pub fn with_environment(mut self, environment: EnvironmentPolicy) -> Self {
        self.environment = environment;
        self
    }

    pub fn validate(&self) -> Result<(), LocalProfileError> {
        if self.executable.as_os_str().is_empty() {
            return Err(LocalProfileError::EmptyExecutable);
        }
        if let Some(working_directory) = &self.working_directory {
            if !working_directory.is_dir() {
                return Err(LocalProfileError::InvalidWorkingDirectory(
                    working_directory.clone(),
                ));
            }
        }
        let environment = match &self.environment {
            EnvironmentPolicy::Inherit => None,
            EnvironmentPolicy::Clear(environment) | EnvironmentPolicy::InheritWith(environment) => {
                Some(environment)
            }
        };
        if let Some(environment) = environment {
            if environment
                .keys()
                .any(|key| key.is_empty() || key.as_encoded_bytes().contains(&b'='))
            {
                return Err(LocalProfileError::InvalidEnvironmentKey);
            }
        }
        Ok(())
    }
}

/// Failure to discover, validate, or start a local shell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalProfileError {
    EmptyExecutable,
    InvalidWorkingDirectory(PathBuf),
    InvalidEnvironmentKey,
    NoDefaultShell,
}

impl fmt::Display for LocalProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyExecutable => formatter.write_str("local profile executable is empty"),
            Self::InvalidWorkingDirectory(directory) => {
                write!(
                    formatter,
                    "local profile working directory is not a directory: {directory:?}"
                )
            }
            Self::InvalidEnvironmentKey => {
                formatter.write_str("local profile contains an invalid environment variable name")
            }
            Self::NoDefaultShell => formatter.write_str("no safe platform default shell was found"),
        }
    }
}

impl std::error::Error for LocalProfileError {}

/// Returns an interactive platform default shell without using command parsing.
pub fn default_local_profile() -> Result<LocalProfile, LocalProfileError> {
    #[cfg(unix)]
    {
        discover_unix_shell(std::env::var_os("SHELL").as_deref(), |path| {
            path.is_absolute() && path.is_file()
        })
    }

    #[cfg(windows)]
    {
        discover_windows_shell(
            std::env::var_os("COMSPEC").as_deref(),
            std::env::var_os("SystemRoot").as_deref(),
            |path| path.is_absolute() && path.is_file(),
        )
    }
}

#[cfg(unix)]
fn discover_unix_shell(
    shell: Option<&OsStr>,
    exists: impl Fn(&Path) -> bool,
) -> Result<LocalProfile, LocalProfileError> {
    if let Some(shell) = shell
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && exists(path))
    {
        return Ok(LocalProfile::new(shell).with_arguments(["-l"]));
    }
    let fallback = PathBuf::from("/bin/sh");
    exists(&fallback)
        .then(|| LocalProfile::new(fallback).with_arguments(["-l"]))
        .ok_or(LocalProfileError::NoDefaultShell)
}

#[cfg(windows)]
fn discover_windows_shell(
    comspec: Option<&OsStr>,
    system_root: Option<&OsStr>,
    exists: impl Fn(&Path) -> bool,
) -> Result<LocalProfile, LocalProfileError> {
    if let Some(command_processor) = comspec
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && exists(path))
    {
        return Ok(LocalProfile::new(command_processor).with_arguments(["/Q"]));
    }

    let powershell = system_root.map(PathBuf::from).map(|root| {
        root.join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe")
    });
    powershell
        .filter(|path| path.is_absolute() && exists(path))
        .map(|path| LocalProfile::new(path).with_arguments(["-NoLogo"]))
        .ok_or(LocalProfileError::NoDefaultShell)
}

/// Searches `PATH` for executables whose file name starts with `query`
/// (case-insensitive), returning up to `limit` absolute paths in
/// alphabetical order.
///
/// Intended for the Local profile editor's executable field: as the user
/// types a bare command name (e.g. `cmd`), this offers the concrete
/// absolute paths on `PATH` (e.g. `C:\Windows\System32\cmd.exe`) so the
/// user can pin one down rather than relying on the search order in effect
/// when fesTerm itself launched. Leaving the field as a bare name is still
/// valid: the launched process resolves it against `PATH` at spawn time,
/// exactly as it does today.
pub fn search_path_executables(query: &str, limit: usize) -> Vec<PathBuf> {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    search_path_executables_in(query, std::env::split_paths(&path_var), limit)
}

fn search_path_executables_in(
    query: &str,
    directories: impl Iterator<Item = PathBuf>,
    limit: usize,
) -> Vec<PathBuf> {
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }
    let query = query.to_lowercase();
    let mut seen = std::collections::BTreeSet::new();
    let mut matches = Vec::new();
    for directory in directories {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(OsStr::to_str) else {
                continue;
            };
            if !name.to_lowercase().starts_with(&query) {
                continue;
            }
            if !is_executable_candidate(&path) {
                continue;
            }
            if seen.insert(path.clone()) {
                matches.push(path);
            }
        }
    }
    matches.sort();
    matches.truncate(limit);
    matches
}

#[cfg(unix)]
fn is_executable_candidate(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable_candidate(path: &Path) -> bool {
    // Windows has no execute permission bit; instead an executable is any
    // file whose extension appears in `PATHEXT` (falling back to the
    // standard default if the environment variable is unset), mirroring
    // how `cmd.exe`/`CreateProcess` resolve a bare command name. Without
    // this check, any same-prefixed file (e.g. `cmd-readme.txt`) would be
    // offered as a launchable suggestion.
    if !path.is_file() {
        return false;
    }
    let Some(extension) = path.extension().and_then(OsStr::to_str) else {
        return false;
    };
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
    pathext.split(';').any(|candidate| {
        candidate
            .trim_start_matches('.')
            .eq_ignore_ascii_case(extension)
    })
}

/// Errors returned while allocating or launching a local PTY session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalPtyError {
    message: String,
}

impl LocalPtyError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for LocalPtyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for LocalPtyError {}

enum SessionCommand {
    Input(Vec<u8>),
    Resize(TerminalSize),
    Shutdown,
}

/// Platform-owned process tree for one local session.
///
/// `portable-pty` creates a new Unix session before `exec`; the initial
/// foreground process group is consequently the shell's process group. Its
/// descendants inherit that group unless a program explicitly opts out. On
/// Windows, a Job Object owns all descendants created by the assigned process.
struct ProcessTree {
    #[cfg(unix)]
    process_group: i32,
    #[cfg(windows)]
    job: WindowsJob,
}

impl ProcessTree {
    #[cfg(unix)]
    fn from_master(master: &dyn MasterPty) -> Result<Self, LocalPtyError> {
        master
            .process_group_leader()
            .map(|process_group| Self { process_group })
            .ok_or_else(|| {
                LocalPtyError::new(
                    "could not determine the local PTY session process group for shutdown",
                )
            })
    }

    #[cfg(windows)]
    fn from_child(child: &dyn Child) -> Result<Self, LocalPtyError> {
        let handle = child.as_raw_handle().ok_or_else(|| {
            LocalPtyError::new("could not obtain the ConPTY child process handle for Job Object")
        })?;
        WindowsJob::assign_to_process(handle)
            .map(|job| Self { job })
            .map_err(|error| {
                LocalPtyError::new(format!(
                    "could not assign the ConPTY child to a Windows Job Object: {error}"
                ))
            })
    }

    #[cfg(not(any(unix, windows)))]
    fn unsupported() -> Result<Self, LocalPtyError> {
        Err(LocalPtyError::new(
            "local PTY process-tree ownership is unsupported on this platform",
        ))
    }

    fn terminate(&self) -> Result<(), String> {
        #[cfg(unix)]
        {
            match kill(Pid::from_raw(-self.process_group), Signal::SIGTERM) {
                Ok(()) | Err(Errno::ESRCH) => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        }

        #[cfg(windows)]
        {
            self.job.terminate().map_err(|error| error.to_string())
        }

        #[cfg(not(any(unix, windows)))]
        {
            Err("local PTY process-tree ownership is unsupported on this platform".to_owned())
        }
    }
}

struct Shared {
    id: SessionId,
    lifecycle: Mutex<SessionLifecycle>,
    metrics: Mutex<SessionMetrics>,
    event_sender: SyncSender<SessionEvent>,
    event_notifier: Arc<dyn SessionEventNotifier>,
    cancel: AtomicBool,
    termination_requested: AtomicBool,
    process_tree: Option<ProcessTree>,
    completion_receiver: Mutex<Receiver<Result<ShutdownResult, SessionError>>>,
    completion: Mutex<Option<Result<ShutdownResult, SessionError>>>,
}

impl Shared {
    fn lifecycle(&self) -> SessionLifecycle {
        self.lifecycle
            .lock()
            .expect("session lifecycle lock is not poisoned")
            .clone()
    }

    fn set_lifecycle(&self, lifecycle: SessionLifecycle) {
        *self
            .lifecycle
            .lock()
            .expect("session lifecycle lock is not poisoned") = lifecycle.clone();
        let _ = self.try_emit(SessionEvent::Lifecycle(lifecycle));
    }

    fn record_error(&self, error: SessionError) {
        {
            let mut metrics = self
                .metrics
                .lock()
                .expect("session metrics lock is not poisoned");
            metrics.error_count = metrics.error_count.saturating_add(1);
        }
        let _ = self.try_emit(SessionEvent::Error(error));
    }

    fn fail(&self, error: SessionError) {
        self.record_error(error.clone());
        self.set_lifecycle(SessionLifecycle::Failed(error));
    }

    fn try_emit(&self, event: SessionEvent) -> bool {
        let mut metrics = self
            .metrics
            .lock()
            .expect("session metrics lock is not poisoned");
        match self.event_sender.try_send(event) {
            Ok(()) => {
                metrics.event_queue_depth = metrics.event_queue_depth.saturating_add(1);
                metrics.event_queue_high_watermark = metrics
                    .event_queue_high_watermark
                    .max(metrics.event_queue_depth);
                drop(metrics);
                self.event_notifier.notify();
                true
            }
            Err(TrySendError::Full(_)) => {
                metrics.backpressure_count = metrics.backpressure_count.saturating_add(1);
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    fn emit_output(&self, bytes: Vec<u8>) -> bool {
        let output_len = bytes.len();
        let mut output_event = SessionEvent::Output(bytes);
        let mut backpressure_pending = false;
        let mut backpressure_event_sent = false;
        loop {
            if self.cancel.load(Ordering::Acquire) {
                return false;
            }

            if backpressure_pending && !backpressure_event_sent {
                let metrics = self.metrics();
                if self.try_emit(SessionEvent::Backpressure {
                    direction: FlowDirection::Output,
                    queued: metrics.event_queue_depth,
                    capacity: metrics.event_queue_capacity,
                }) {
                    backpressure_event_sent = true;
                    continue;
                }
            }

            let mut metrics = self
                .metrics
                .lock()
                .expect("session metrics lock is not poisoned");
            match self.event_sender.try_send(output_event) {
                Ok(()) => {
                    metrics.output_bytes = metrics.output_bytes.saturating_add(output_len as u64);
                    metrics.event_queue_depth = metrics.event_queue_depth.saturating_add(1);
                    metrics.event_queue_high_watermark = metrics
                        .event_queue_high_watermark
                        .max(metrics.event_queue_depth);
                    drop(metrics);
                    self.event_notifier.notify();
                    return true;
                }
                Err(TrySendError::Full(event)) => {
                    output_event = event;
                    metrics.backpressure_count = metrics.backpressure_count.saturating_add(1);
                    drop(metrics);
                    backpressure_pending = true;
                    thread::sleep(BACKPRESSURE_RETRY);
                }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
    }

    fn metrics(&self) -> SessionMetrics {
        *self
            .metrics
            .lock()
            .expect("session metrics lock is not poisoned")
    }

    fn request_stop(&self) {
        let lifecycle = self.lifecycle();
        if matches!(lifecycle, SessionLifecycle::Stopped) {
            return;
        }
        if matches!(
            lifecycle,
            SessionLifecycle::Starting | SessionLifecycle::Running
        ) {
            self.set_lifecycle(SessionLifecycle::Stopping);
        }
        self.cancel.store(true, Ordering::Release);
        self.terminate_process_tree();
    }

    fn terminate_process_tree(&self) {
        if self.termination_requested.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(process_tree) = &self.process_tree else {
            return;
        };
        if let Err(error) = process_tree.terminate() {
            self.record_error(SessionError::new(
                SessionErrorKind::Shutdown,
                format!("could not terminate the local process tree: {error}"),
            ));
        }
    }

    fn complete(
        &self,
        result: Result<ShutdownResult, SessionError>,
        sender: &mpsc::SyncSender<Result<ShutdownResult, SessionError>>,
    ) {
        let _ = sender.send(result);
    }
}

/// A native local-shell session driven by bounded worker queues.
pub struct LocalPtySession {
    shared: Arc<Shared>,
    command_sender: SyncSender<SessionCommand>,
    event_receiver: Mutex<Receiver<SessionEvent>>,
}

impl LocalPtySession {
    pub fn start(profile: LocalProfile, size: TerminalSize) -> Result<Self, LocalPtyError> {
        Self::start_with_notifier(profile, size, noop_session_event_notifier())
    }

    /// Starts a local PTY session and wakes `notifier` whenever an event arrives.
    pub fn start_with_notifier(
        profile: LocalProfile,
        size: TerminalSize,
        event_notifier: Arc<dyn SessionEventNotifier>,
    ) -> Result<Self, LocalPtyError> {
        profile
            .validate()
            .map_err(|error| LocalPtyError::new(error.to_string()))?;

        #[cfg(windows)]
        {
            prepare_windows_conpty_runtime().map_err(|error| {
                LocalPtyError::new(format!(
                    "could not safely select the Windows ConPTY runtime: {error}"
                ))
            })?;
        }

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(to_portable_size(size))
            .map_err(|error| {
                LocalPtyError::new(format!("could not allocate local PTY: {error}"))
            })?;
        let mut command = command_builder(&profile);
        command.set_controlling_tty(true);
        let reader = pair.master.try_clone_reader().map_err(|error| {
            LocalPtyError::new(format!("could not open local PTY reader: {error}"))
        })?;
        let writer = pair.master.take_writer().map_err(|error| {
            LocalPtyError::new(format!("could not open local PTY writer: {error}"))
        })?;
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| LocalPtyError::new(format!("could not start local shell: {error}")))?;
        let process_tree_result = {
            #[cfg(unix)]
            {
                ProcessTree::from_master(&*pair.master)
            }
            #[cfg(windows)]
            {
                ProcessTree::from_child(&*child)
            }
            #[cfg(not(any(unix, windows)))]
            {
                ProcessTree::unsupported()
            }
        };
        let process_tree = match process_tree_result {
            Ok(process_tree) => process_tree,
            Err(error) => {
                let _ = child.kill();
                return Err(error);
            }
        };

        let (event_sender, event_receiver) = mpsc::sync_channel(DEFAULT_EVENT_QUEUE_CAPACITY);
        let (command_sender, command_receiver) = mpsc::sync_channel(DEFAULT_COMMAND_QUEUE_CAPACITY);
        let (completion_sender, completion_receiver) = mpsc::sync_channel(1);
        let (reader_done_sender, reader_done_receiver) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            id: SessionId::next(),
            lifecycle: Mutex::new(SessionLifecycle::Starting),
            metrics: Mutex::new(SessionMetrics {
                event_queue_capacity: DEFAULT_EVENT_QUEUE_CAPACITY,
                ..SessionMetrics::default()
            }),
            event_sender,
            event_notifier,
            cancel: AtomicBool::new(false),
            termination_requested: AtomicBool::new(false),
            process_tree: Some(process_tree),
            completion_receiver: Mutex::new(completion_receiver),
            completion: Mutex::new(None),
        });
        shared.set_lifecycle(SessionLifecycle::Starting);
        shared.set_lifecycle(SessionLifecycle::Running);

        let control_shared = Arc::clone(&shared);
        let master = pair.master;
        let control_worker = thread::Builder::new()
            .name(format!("festerm-pty-control-{}", shared.id))
            .spawn(move || {
                control_worker(
                    control_shared,
                    command_receiver,
                    master,
                    writer,
                    child,
                    reader_done_receiver,
                    completion_sender,
                );
            })
            .map_err(|error| {
                LocalPtyError::new(format!("could not start local PTY worker: {error}"))
            });
        if let Err(error) = control_worker {
            shared.request_stop();
            return Err(error);
        }

        let reader_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name(format!("festerm-pty-reader-{}", shared.id))
            .spawn(move || reader_worker(reader_shared, reader, reader_done_sender))
            .map_err(|error| {
                shared.request_stop();
                LocalPtyError::new(format!("could not start local PTY reader worker: {error}"))
            })?;

        Ok(Self {
            shared,
            command_sender,
            event_receiver: Mutex::new(event_receiver),
        })
    }

    /// Starts the safe platform default interactive shell.
    pub fn start_default(size: TerminalSize) -> Result<Self, LocalPtyError> {
        Self::start_default_with_notifier(size, noop_session_event_notifier())
    }

    /// Starts the default local shell and wakes `notifier` for each session event.
    pub fn start_default_with_notifier(
        size: TerminalSize,
        event_notifier: Arc<dyn SessionEventNotifier>,
    ) -> Result<Self, LocalPtyError> {
        let profile =
            default_local_profile().map_err(|error| LocalPtyError::new(error.to_string()))?;
        Self::start_with_notifier(profile, size, event_notifier)
    }
}

impl Session for LocalPtySession {
    fn id(&self) -> SessionId {
        self.shared.id
    }

    fn lifecycle(&self) -> SessionLifecycle {
        self.shared.lifecycle()
    }

    fn metrics(&self) -> SessionMetrics {
        self.shared.metrics()
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
            &self.command_sender,
            SessionCommand::Input(bytes.to_vec()),
            SessionOperation::Input,
        )
    }

    fn try_resize(&self, size: TerminalSize) -> Result<(), SessionSendError> {
        send_command(
            &self.command_sender,
            SessionCommand::Resize(size),
            SessionOperation::Resize,
        )
    }

    fn try_shutdown(&self) -> Result<(), SessionSendError> {
        self.shared.request_stop();
        send_command(
            &self.command_sender,
            SessionCommand::Shutdown,
            SessionOperation::Shutdown,
        )
    }

    fn try_recv_event(&self) -> Result<SessionEvent, SessionTryReceiveError> {
        match self
            .event_receiver
            .lock()
            .expect("session event receiver lock is not poisoned")
            .try_recv()
        {
            Ok(event) => {
                let mut metrics = self
                    .shared
                    .metrics
                    .lock()
                    .expect("session metrics lock is not poisoned");
                metrics.event_queue_depth = metrics.event_queue_depth.saturating_sub(1);
                Ok(event)
            }
            Err(TryRecvError::Empty) => Err(SessionTryReceiveError::Empty),
            Err(TryRecvError::Disconnected) => Err(SessionTryReceiveError::Closed),
        }
    }

    fn shutdown(&self, timeout: Duration) -> Result<ShutdownResult, ShutdownError> {
        let _ = self.try_shutdown();
        let mut completion = self
            .shared
            .completion
            .lock()
            .expect("session completion lock is not poisoned");
        if let Some(result) = completion.clone() {
            return result.map_err(ShutdownError::Failed);
        }
        match self
            .shared
            .completion_receiver
            .lock()
            .expect("session completion receiver lock is not poisoned")
            .recv_timeout(timeout)
        {
            Ok(result) => {
                *completion = Some(result.clone());
                result.map_err(ShutdownError::Failed)
            }
            Err(RecvTimeoutError::Timeout) => Err(ShutdownError::TimedOut { timeout }),
            Err(RecvTimeoutError::Disconnected) => Err(ShutdownError::Failed(SessionError::new(
                SessionErrorKind::Shutdown,
                "local PTY worker ended without reporting shutdown completion",
            ))),
        }
    }
}

impl Drop for LocalPtySession {
    fn drop(&mut self) {
        // Never block destructors. Explicit `Session::shutdown` performs the
        // bounded wait; this wake-up and child termination keeps detached
        // workers from retaining a local shell after the application exits.
        let _ = self.try_shutdown();
    }
}

fn command_builder(profile: &LocalProfile) -> CommandBuilder {
    let mut command = CommandBuilder::new(&profile.executable);
    command.args(profile.arguments.iter());
    if let Some(working_directory) = &profile.working_directory {
        command.cwd(working_directory);
    }
    match &profile.environment {
        EnvironmentPolicy::Inherit => {}
        EnvironmentPolicy::Clear(environment) => {
            command.env_clear();
            for (key, value) in environment {
                command.env(key, value);
            }
        }
        EnvironmentPolicy::InheritWith(environment) => {
            for (key, value) in environment {
                command.env(key, value);
            }
        }
    }
    // M5's core implements the currently selected xterm-compatible subset.
    // This is an argv/environment assignment, never shell interpolation.
    command.env("TERM", "xterm-256color");
    // A GUI-launched fesTerm can inherit another terminal's identity. On
    // macOS, inheriting `TERM_PROGRAM=Apple_Terminal` makes Apple's zsh
    // startup hooks load that terminal's saved shell-session transcript and
    // print "Restored session" in an otherwise fresh fesTerm shell. Identify
    // the actual terminal owner and clear the stale source-session ID.
    command.env("TERM_PROGRAM", "fesTerm");
    command.env_remove("TERM_SESSION_ID");
    command
}

fn to_portable_size(size: TerminalSize) -> PtySize {
    PtySize {
        rows: size.rows(),
        cols: size.columns(),
        pixel_width: size.pixel_width().unwrap_or(0),
        pixel_height: size.pixel_height().unwrap_or(0),
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

fn reader_worker(
    shared: Arc<Shared>,
    mut reader: Box<dyn Read + Send>,
    done_sender: SyncSender<Result<(), SessionError>>,
) {
    let mut buffer = vec![0_u8; MAX_IO_CHUNK_BYTES.min(16 * 1024)];
    loop {
        if shared.cancel.load(Ordering::Acquire) {
            let _ = done_sender.send(Ok(()));
            return;
        }
        match reader.read(&mut buffer) {
            Ok(0) => {
                let _ = done_sender.send(Ok(()));
                return;
            }
            Ok(read) => {
                if !shared.emit_output(buffer[..read].to_vec()) {
                    let _ = done_sender.send(Ok(()));
                    return;
                }
            }
            Err(error) => {
                if shared.cancel.load(Ordering::Acquire) {
                    let _ = done_sender.send(Ok(()));
                    return;
                }
                let error = SessionError::new(
                    SessionErrorKind::Output,
                    format!("local PTY read failed: {error}"),
                );
                shared.record_error(error.clone());
                shared.cancel.store(true, Ordering::Release);
                let _ = done_sender.send(Err(error));
                return;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn control_worker(
    shared: Arc<Shared>,
    command_receiver: Receiver<SessionCommand>,
    master: Box<dyn MasterPty + Send>,
    mut writer: Box<dyn Write + Send>,
    mut child: Box<dyn Child + Send + Sync>,
    reader_done_receiver: Receiver<Result<(), SessionError>>,
    completion_sender: SyncSender<Result<ShutdownResult, SessionError>>,
) {
    let mut stopping = false;
    let mut worker_failure = None;
    loop {
        if shared.cancel.load(Ordering::Acquire) {
            stopping = true;
            shared.terminate_process_tree();
        }

        match command_receiver.recv_timeout(POLL_INTERVAL) {
            Ok(SessionCommand::Input(bytes)) if !stopping => {
                if let Err(error) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
                    let error = SessionError::new(
                        SessionErrorKind::Input,
                        format!("local PTY write failed: {error}"),
                    );
                    shared.fail(error.clone());
                    worker_failure = Some(error);
                    shared.cancel.store(true, Ordering::Release);
                    stopping = true;
                    shared.terminate_process_tree();
                } else {
                    let mut metrics = shared
                        .metrics
                        .lock()
                        .expect("session metrics lock is not poisoned");
                    metrics.input_bytes = metrics.input_bytes.saturating_add(bytes.len() as u64);
                }
            }
            Ok(SessionCommand::Resize(size)) if !stopping => {
                match master.resize(to_portable_size(size)) {
                    Ok(()) => {
                        let mut metrics = shared
                            .metrics
                            .lock()
                            .expect("session metrics lock is not poisoned");
                        metrics.resize_count = metrics.resize_count.saturating_add(1);
                        drop(metrics);
                        let _ = shared.try_emit(SessionEvent::ResizeApplied(size));
                    }
                    Err(error) => shared.record_error(SessionError::new(
                        SessionErrorKind::Resize,
                        format!("local PTY resize failed: {error}"),
                    )),
                }
            }
            Ok(SessionCommand::Shutdown) => {
                stopping = true;
                shared.cancel.store(true, Ordering::Release);
                shared.terminate_process_tree();
            }
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                stopping = true;
                shared.cancel.store(true, Ordering::Release);
                shared.terminate_process_tree();
            }
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                let terminal_exit = match status.signal() {
                    Some(signal) => SessionExit::with_signal(status.exit_code(), signal),
                    None => SessionExit::with_exit_code(status.exit_code()),
                };
                if !stopping {
                    shared.set_lifecycle(SessionLifecycle::Exited(terminal_exit));
                }
                shared.terminate_process_tree();
                break;
            }
            Ok(None) => {}
            Err(error) => {
                let error = SessionError::new(
                    SessionErrorKind::Internal,
                    format!("could not observe local child exit: {error}"),
                );
                shared.fail(error.clone());
                shared.cancel.store(true, Ordering::Release);
                shared.terminate_process_tree();
                drop(writer);
                drop(master);
                shared.complete(Err(error), &completion_sender);
                return;
            }
        }
    }

    drop(writer);
    drop(master);
    let reader_result = reader_done_receiver.recv_timeout(READER_STOP_TIMEOUT);
    let result = if let Some(error) = worker_failure {
        Err(error)
    } else {
        match reader_result {
            Ok(Ok(())) if stopping || shared.cancel.load(Ordering::Acquire) => {
                shared.set_lifecycle(SessionLifecycle::Stopped);
                Ok(ShutdownResult::Stopped)
            }
            Ok(Ok(())) => Ok(ShutdownResult::AlreadyStopped),
            Ok(Err(error)) => {
                shared.fail(error.clone());
                Err(error)
            }
            Err(RecvTimeoutError::Timeout) => {
                shared.cancel.store(true, Ordering::Release);
                let error = SessionError::new(
                    SessionErrorKind::Shutdown,
                    "local PTY reader did not stop within the bounded shutdown interval",
                );
                shared.fail(error.clone());
                Err(error)
            }
            Err(RecvTimeoutError::Disconnected) => {
                let error = SessionError::new(
                    SessionErrorKind::Internal,
                    "local PTY reader ended without reporting completion",
                );
                shared.fail(error.clone());
                Err(error)
            }
        }
    };
    shared.complete(result, &completion_sender);
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use super::*;

    #[cfg(unix)]
    #[test]
    fn unix_default_shell_requires_an_absolute_existing_shell() {
        let shell =
            discover_unix_shell(Some(OsStr::new("sh")), |path| path == Path::new("/bin/sh"))
                .unwrap();
        assert_eq!(shell.executable(), Path::new("/bin/sh"));

        let shell = discover_unix_shell(Some(OsStr::new("sh")), |_| false);
        assert_eq!(shell, Err(LocalProfileError::NoDefaultShell));

        let shell = discover_unix_shell(Some(OsStr::new("/custom/sh")), |path| {
            path == Path::new("/custom/sh")
        })
        .unwrap();
        assert_eq!(shell.executable(), Path::new("/custom/sh"));
        assert_eq!(shell.arguments(), [OsString::from("-l")]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_default_shell_prefers_an_absolute_comspec_then_powershell() {
        let command_processor = discover_windows_shell(
            Some(OsStr::new(r"C:\Windows\System32\cmd.exe")),
            Some(OsStr::new(r"C:\Windows")),
            |path| path == Path::new(r"C:\Windows\System32\cmd.exe"),
        )
        .unwrap();
        assert_eq!(
            command_processor.executable(),
            Path::new(r"C:\Windows\System32\cmd.exe")
        );
        assert_eq!(command_processor.arguments(), [OsString::from("/Q")]);

        let powershell = discover_windows_shell(
            Some(OsStr::new("cmd.exe")),
            Some(OsStr::new(r"C:\Windows")),
            |path| path.ends_with(r"WindowsPowerShell\v1.0\powershell.exe"),
        )
        .unwrap();
        assert_eq!(powershell.arguments(), [OsString::from("-NoLogo")]);
    }

    #[test]
    fn profile_rejects_invalid_working_directory() {
        let profile =
            LocalProfile::new("shell").with_working_directory("definitely-not-a-directory");
        assert!(matches!(
            profile.validate(),
            Err(LocalProfileError::InvalidWorkingDirectory(_))
        ));
    }

    #[test]
    fn local_children_identify_festerm_and_drop_an_inherited_terminal_session() {
        let command = command_builder(&LocalProfile::new("shell"));

        assert_eq!(command.get_env("TERM"), Some(OsStr::new("xterm-256color")));
        assert_eq!(command.get_env("TERM_PROGRAM"), Some(OsStr::new("fesTerm")));
        assert_eq!(command.get_env("TERM_SESSION_ID"), None);
    }

    fn make_executable(path: &Path) {
        std::fs::write(path, b"#!/bin/sh\n").expect("test file can be created");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .expect("test file permissions can be set");
        }
    }

    /// Appends the platform's recognized executable extension to a bare
    /// test fixture name (`.exe` on Windows, where `PATHEXT` — not a
    /// permission bit — decides whether a file is launchable; unchanged
    /// elsewhere, where the Unix execute-permission bit already suffices).
    fn executable_fixture_name(base: &str) -> String {
        if cfg!(windows) {
            format!("{base}.exe")
        } else {
            base.to_owned()
        }
    }

    #[test]
    fn path_search_matches_case_insensitive_prefixes_across_directories_and_is_sorted_and_capped() {
        let root = std::env::temp_dir().join(format!(
            "festerm-path-search-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir_all(&first).expect("test directory can be created");
        std::fs::create_dir_all(&second).expect("test directory can be created");

        let cargo_name = executable_fixture_name("cargo");
        let cmdlet_name = executable_fixture_name("cmdlet");
        make_executable(&first.join("Cmd.exe"));
        make_executable(&first.join(&cargo_name));
        make_executable(&second.join(&cmdlet_name));
        // Not executable: must be excluded even though the name matches.
        std::fs::write(second.join("cmd-readme.txt"), b"not executable")
            .expect("test file can be created");

        let directories = [first.clone(), second.clone()].into_iter();
        let matches = search_path_executables_in("cmd", directories, 10);
        assert_eq!(
            matches,
            vec![first.join("Cmd.exe"), second.join(&cmdlet_name)],
            "matches are case-insensitive-prefix filtered, executable-only, and sorted"
        );

        let directories = [first.clone(), second.clone()].into_iter();
        let capped = search_path_executables_in("cmd", directories, 1);
        assert_eq!(capped.len(), 1, "the limit truncates the result set");

        let directories = [first, second].into_iter();
        assert!(
            search_path_executables_in("", directories, 10).is_empty(),
            "an empty query returns no suggestions"
        );

        std::fs::remove_dir_all(&root).expect("test directory can be removed");
    }

    #[test]
    fn bounded_output_queue_reports_pressure_before_resuming_output() {
        let (event_sender, event_receiver) = mpsc::sync_channel(1);
        let (_completion_sender, completion_receiver) = mpsc::sync_channel(1);
        let notifier = Arc::new(CountingNotifier::default());
        let shared = Arc::new(Shared {
            id: SessionId::next(),
            lifecycle: Mutex::new(SessionLifecycle::Running),
            metrics: Mutex::new(SessionMetrics {
                event_queue_capacity: 1,
                ..SessionMetrics::default()
            }),
            event_sender,
            event_notifier: notifier.clone(),
            cancel: AtomicBool::new(false),
            termination_requested: AtomicBool::new(false),
            process_tree: None,
            completion_receiver: Mutex::new(completion_receiver),
            completion: Mutex::new(None),
        });
        assert!(shared.emit_output(b"first".to_vec()));

        let producer_shared = Arc::clone(&shared);
        let producer = thread::spawn(move || producer_shared.emit_output(b"second".to_vec()));

        let deadline = Instant::now() + Duration::from_secs(1);
        while shared.metrics().backpressure_count == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(shared.metrics().backpressure_count > 0);
        assert!(matches!(
            event_receiver.recv_timeout(Duration::from_secs(1)),
            Ok(SessionEvent::Output(bytes)) if bytes == b"first"
        ));
        assert!(matches!(
            event_receiver.recv_timeout(Duration::from_secs(1)),
            Ok(SessionEvent::Backpressure {
                direction: FlowDirection::Output,
                queued: 1,
                capacity: 1,
            })
        ));
        assert!(matches!(
            event_receiver.recv_timeout(Duration::from_secs(1)),
            Ok(SessionEvent::Output(bytes)) if bytes == b"second"
        ));
        assert!(producer.join().expect("producer joins"));
        assert!(shared.metrics().backpressure_count > 0);
        assert_eq!(notifier.notifications(), 3);
    }

    #[derive(Default)]
    struct CountingNotifier(AtomicUsize);

    impl CountingNotifier {
        fn notifications(&self) -> usize {
            self.0.load(Ordering::Relaxed)
        }
    }

    impl SessionEventNotifier for CountingNotifier {
        fn notify(&self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[cfg(unix)]
    #[test]
    fn controlled_shell_transfers_bytes_resizes_exits_and_stops() {
        // Uses the repository-owned test child instead of shell built-ins.
        let profile = LocalProfile::new(test_child_path()).with_arguments([
            "emit:READY",
            "read-line",
            "echo:INPUT",
            "report-size",
            "exit:0",
        ]);
        let session = LocalPtySession::start(profile, TerminalSize::new(80, 24).unwrap()).unwrap();

        let mut output = Vec::new();
        wait_for(&session, Duration::from_secs(4), &mut output, |bytes| {
            bytes
                .windows(b"READY".len())
                .any(|window| window == b"READY")
        });
        session
            .try_resize(TerminalSize::new(100, 40).unwrap())
            .unwrap();
        session.try_send_input(b"hello\n").unwrap();
        wait_for(&session, Duration::from_secs(4), &mut output, |bytes| {
            bytes
                .windows(b"INPUT:hello".len())
                .any(|window| window == b"INPUT:hello")
                && bytes
                    .windows(b"40 100".len())
                    .any(|window| window == b"40 100")
        });
        assert!(session.metrics().resize_count >= 1);
        assert!(matches!(
            session.shutdown(Duration::from_secs(4)),
            Ok(ShutdownResult::AlreadyStopped) | Ok(ShutdownResult::Stopped)
        ));
        assert!(output
            .windows(b"INPUT:hello".len())
            .any(|window| window == b"INPUT:hello"));
    }

    #[cfg(unix)]
    #[test]
    fn controlled_shell_preserves_output_between_consecutive_resizes() {
        // Uses the repository-owned test child instead of shell built-ins.
        let profile = LocalProfile::new(test_child_path()).with_arguments([
            "emit:READY",
            "read-line",
            "echo:FRAME",
            "report-size",
            "read-line",
            "echo:FRAME",
            "report-size",
            "exit:0",
        ]);
        let session = LocalPtySession::start(profile, TerminalSize::new(80, 24).unwrap()).unwrap();
        let mut output = Vec::new();
        wait_for(&session, Duration::from_secs(4), &mut output, |bytes| {
            bytes
                .windows(b"READY".len())
                .any(|window| window == b"READY")
        });

        for (size, input, expected_size) in [
            (
                TerminalSize::new(37, 13).unwrap(),
                b"first\n".as_slice(),
                b"13 37".as_slice(),
            ),
            (
                TerminalSize::new(73, 26).unwrap(),
                b"second\n".as_slice(),
                b"26 73".as_slice(),
            ),
        ] {
            session.try_resize(size).unwrap();
            session.try_send_input(input).unwrap();
            wait_for(&session, Duration::from_secs(4), &mut output, |bytes| {
                bytes
                    .windows(expected_size.len())
                    .any(|window| window == expected_size)
            });
        }

        assert!(output
            .windows(b"FRAME:first".len())
            .any(|window| window == b"FRAME:first"));
        assert!(output
            .windows(b"FRAME:second".len())
            .any(|window| window == b"FRAME:second"));
        assert!(matches!(
            session.shutdown(Duration::from_secs(4)),
            Ok(ShutdownResult::AlreadyStopped) | Ok(ShutdownResult::Stopped)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_terminates_a_running_controlled_shell() {
        // Uses the repository-owned test child instead of a shell spin loop.
        let profile = LocalProfile::new(test_child_path()).with_arguments(["emit:RUNNING", "spin"]);
        let session = LocalPtySession::start(profile, TerminalSize::new(80, 24).unwrap()).unwrap();
        let mut output = Vec::new();
        wait_for(&session, Duration::from_secs(4), &mut output, |bytes| {
            bytes
                .windows(b"RUNNING".len())
                .any(|window| window == b"RUNNING")
        });

        assert_eq!(
            session.shutdown(Duration::from_secs(2)),
            Ok(ShutdownResult::Stopped)
        );
        assert_eq!(session.lifecycle(), SessionLifecycle::Stopped);
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_terminates_a_shell_descendant_in_its_process_group() {
        // Uses the repository-owned test child instead of /bin/sh + sleep.
        let profile = LocalProfile::new(test_child_path()).with_arguments(["spawn"]);
        let session = LocalPtySession::start(profile, TerminalSize::new(80, 24).unwrap()).unwrap();
        let mut output = Vec::new();
        wait_for(&session, Duration::from_secs(4), &mut output, |bytes| {
            bytes
                .windows(b"CHILD:".len())
                .any(|window| window == b"CHILD:")
        });
        let child = child_pid(&output).expect("test child reports its descendant PID");

        assert_eq!(
            session.shutdown(Duration::from_secs(2)),
            Ok(ShutdownResult::Stopped)
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        while process_is_running(child) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !process_is_running(child),
            "descendant process {child} remained after session shutdown"
        );
    }

    #[cfg(windows)]
    #[test]
    fn controlled_conpty_transfers_bytes_resizes_exits_and_stops() {
        // Uses the repository-owned test child instead of cmd.exe built-ins.
        let profile = LocalProfile::new(test_child_path()).with_arguments([
            "emit:READY",
            "read-line",
            "echo:INPUT",
            "exit:0",
        ]);
        let session = LocalPtySession::start(profile, TerminalSize::new(80, 24).unwrap()).unwrap();
        let mut output = Vec::new();
        wait_for_windows(&session, Duration::from_secs(5), &mut output, |_, bytes| {
            bytes
                .windows(b"READY".len())
                .any(|window| window == b"READY")
        });

        session
            .try_resize(TerminalSize::new(100, 40).unwrap())
            .unwrap();
        wait_for_windows(&session, Duration::from_secs(5), &mut output, |event, _| {
            matches!(
                event,
                SessionEvent::ResizeApplied(size)
                    if size.columns() == 100 && size.rows() == 40
            )
        });
        session.try_send_input(b"hello\r\n").unwrap();
        wait_for_windows(&session, Duration::from_secs(5), &mut output, |_, bytes| {
            bytes
                .windows(b"INPUT:hello".len())
                .any(|window| window == b"INPUT:hello")
        });
        wait_for_windows_exit(&session, Duration::from_secs(3));
        assert!(
            matches!(
                session.shutdown(Duration::from_secs(2)),
                Ok(ShutdownResult::AlreadyStopped | ShutdownResult::Stopped)
            ),
            "ConPTY cleanup completes whether the exit worker or shutdown request wins"
        );
    }

    /// Returns the path to the `festerm-pty-test-child` binary built by cargo.
    ///
    /// Cargo places all workspace binaries in the same `target/{profile}/`
    /// directory as the test executable.  The test executable itself lives in
    /// `target/{profile}/deps/`, so we walk up one level when necessary.
    ///
    /// `festerm-pty-test-child` is a subprocess dependency of these tests,
    /// not a linked (`[dev-dependencies]`) one -- it is a bin-only crate, and
    /// Cargo has no build-graph edge to force it to finish building before
    /// this package's own tests start running. `cargo test --workspace`
    /// pipelines package builds and test runs, so it may start these tests
    /// while that unrelated package is still linking, especially under CI's
    /// noisier, more contended build parallelism (this was observed causing
    /// intermittent failures here). If the binary isn't there yet, build it
    /// on demand instead of assuming a race-free ordering; Cargo's own
    /// target-directory locking makes this safe to call even if that
    /// package's own build is still in flight elsewhere.
    #[cfg(any(unix, windows))]
    fn test_child_path() -> std::path::PathBuf {
        let mut path = std::env::current_exe()
            .expect("test executable has a known path")
            .canonicalize()
            .expect("test executable path is accessible");
        path.pop(); // remove the test executable name
        if path.ends_with("deps") {
            path.pop(); // step up from target/{profile}/deps/ to target/{profile}/
        }
        let name = if cfg!(windows) {
            "festerm-pty-test-child.exe"
        } else {
            "festerm-pty-test-child"
        };
        path.push(name);
        if !path.exists() {
            static BUILD_ON_DEMAND: std::sync::Once = std::sync::Once::new();
            BUILD_ON_DEMAND.call_once(|| {
                let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
                let status = std::process::Command::new(cargo)
                    .args(["build", "--package", "festerm-pty-test-child"])
                    .status();
                assert!(
                    matches!(status, Ok(status) if status.success()),
                    "on-demand `cargo build --package festerm-pty-test-child` failed: {status:?}"
                );
            });
        }
        assert!(
            path.exists(),
            "festerm-pty-test-child not found at {path:?} even after an \
             on-demand build; run `cargo build -p festerm-pty-test-child` \
             manually and re-run these tests"
        );
        path
    }

    #[cfg(unix)]
    fn wait_for(
        session: &LocalPtySession,
        timeout: Duration,
        output: &mut Vec<u8>,
        matches: impl Fn(&[u8]) -> bool,
    ) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match session.try_recv_event() {
                Ok(SessionEvent::Output(bytes)) => {
                    output.extend(bytes);
                    if matches(output) {
                        return;
                    }
                }
                Ok(_) | Err(SessionTryReceiveError::Empty) => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(SessionTryReceiveError::Closed) => break,
            }
        }
        panic!("timed out waiting for controlled PTY output: {output:?}");
    }

    #[cfg(unix)]
    fn child_pid(output: &[u8]) -> Option<u32> {
        let prefix = b"CHILD:";
        let start = output
            .windows(prefix.len())
            .position(|window| window == prefix)?
            .saturating_add(prefix.len());
        let digits = output[start..]
            .iter()
            .copied()
            .take_while(|byte| byte.is_ascii_digit())
            .collect::<Vec<_>>();
        std::str::from_utf8(&digits).ok()?.parse().ok()
    }

    #[cfg(unix)]
    fn process_is_running(process: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &process.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("platform supplies kill")
            .success()
    }

    #[cfg(windows)]
    fn wait_for_windows(
        session: &LocalPtySession,
        timeout: Duration,
        output: &mut Vec<u8>,
        matches: impl Fn(&SessionEvent, &[u8]) -> bool,
    ) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match session.try_recv_event() {
                Ok(event) => {
                    if let SessionEvent::Output(bytes) = &event {
                        respond_to_cursor_position_queries(session, bytes);
                        output.extend(bytes);
                    }
                    if matches(&event, output) {
                        return;
                    }
                }
                Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(5)),
                Err(SessionTryReceiveError::Closed) => break,
            }
        }
        panic!("timed out waiting for controlled ConPTY event: {output:?}");
    }

    #[cfg(windows)]
    fn respond_to_cursor_position_queries(session: &LocalPtySession, bytes: &[u8]) {
        for _ in bytes.windows(4).filter(|sequence| *sequence == b"\x1b[6n") {
            session
                .try_send_input(b"\x1b[1;1R")
                .expect("ConPTY cursor-position reply fits the command queue");
        }
    }

    #[cfg(windows)]
    fn wait_for_windows_exit(session: &LocalPtySession, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if matches!(
                session.lifecycle(),
                SessionLifecycle::Exited(exit) if exit.success()
            ) {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!(
            "timed out waiting for controlled ConPTY exit: {:?}",
            session.lifecycle()
        );
    }
}
