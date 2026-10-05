//! Command-line document activation and bounded same-user forwarding.
//!
//! This module is intentionally UI-agnostic. `main` should call
//! [`prepare_startup`] before restoring window state: a secondary process that
//! was launched only to open files can then forward the request and exit without
//! applying any title, window, or workspace side effects.

use std::{
    collections::VecDeque,
    env,
    ffi::{OsStr, OsString},
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;

#[cfg(unix)]
use std::os::unix::{
    ffi::OsStrExt,
    fs::{MetadataExt, PermissionsExt},
    net::{UnixListener, UnixStream},
};

#[cfg(windows)]
use festerm_windows_security::named_pipe::{Pipe, PipeListener};

const QUEUE_CAPACITY: usize = 128;
const MAX_BATCH_PATHS: usize = 128;
const MAX_PATH_BYTES: usize = 32 * 1024;
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
#[cfg(windows)]
const CONNECT_TIMEOUT: Duration = Duration::from_millis(750);
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const STARTUP_FORWARD_RETRY_TIMEOUT: Duration = Duration::from_secs(2);
const STARTUP_FORWARD_RETRY_INTERVAL: Duration = Duration::from_millis(20);
#[cfg(windows)]
const WINDOWS_ACCEPT_RETRY_INTERVAL: Duration = Duration::from_millis(20);
const FRAME_MAGIC: &[u8; 4] = b"FDO1";
const ACK_ENQUEUED: u8 = 1;
const ACK_QUEUE_FULL: u8 = 2;
const ACK_REJECTED: u8 = 3;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CliActivation {
    None,
    Help,
    Version,
    Open { paths: Vec<PathBuf> },
}

#[derive(Debug)]
pub enum StartupAction {
    /// This process owns the activation endpoint and should pass `queue` to the
    /// application command router. Any startup file batch is already queued.
    Primary {
        queue: ActivationQueue,
        server: DocumentActivationServer,
    },
    /// A file-open launch forwarded to another live process and should exit.
    ForwardedToPrimary,
    /// No file activation was requested and another process already owns the endpoint.
    IndependentNoActivation,
    Help,
    Version,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DocumentActivationBatch {
    paths: Vec<PathBuf>,
}

#[derive(Clone)]
pub struct ActivationQueue {
    shared: Arc<QueueShared>,
}

impl fmt::Debug for ActivationQueue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActivationQueue")
            .finish_non_exhaustive()
    }
}

impl ActivationQueue {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(QueueShared::new()),
        }
    }

    pub fn enqueue<I>(&self, paths: I) -> Result<(), ActivationError>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let batch = bounded_batch(paths)?;
        self.shared.enqueue(batch.paths)
    }

    pub fn pop_front(&self) -> Option<PathBuf> {
        self.shared.pop_front()
    }

    pub fn set_waker(&self, waker: Arc<dyn Fn() + Send + Sync>) {
        self.shared.set_waker(waker);
    }

    pub fn metrics(&self) -> ActivationMetrics {
        self.shared.metrics()
    }
}

impl Default for ActivationQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct DocumentActivationServer {
    cancelled: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    lock: File,
    #[cfg(unix)]
    socket_path: PathBuf,
}

impl Drop for DocumentActivationServer {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        #[cfg(unix)]
        {
            let _ = UnixStream::connect(&self.socket_path);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        #[cfg(unix)]
        {
            let _ = fs::remove_file(&self.socket_path);
        }
        let _ = self.lock.unlock();
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActivationMetrics {
    pub queue_capacity: usize,
    pub queue_depth: usize,
    pub queue_high_watermark: usize,
    pub enqueued_batches: u64,
    pub rejected_full: u64,
    pub rejected_limit: u64,
    pub ipc_errors: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivationError {
    UnsupportedFlag(OsString),
    MissingOpenPath,
    PathLimit { path: PathBuf, maximum: usize },
    BatchLimit { count: usize, maximum: usize },
    RuntimeUnavailable(String),
    BusyLiveReceiver(String),
    QueueFull,
    ConnectionFailed(String),
    Protocol(String),
    Io(String),
}

impl fmt::Display for ActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFlag(flag) => write!(
                formatter,
                "unsupported option {:?}; use --open [--] paths... or --help",
                flag
            ),
            Self::MissingOpenPath => formatter.write_str("--open requires at least one path"),
            Self::PathLimit { path, maximum } => write!(
                formatter,
                "document path {:?} exceeds the {maximum}-byte activation limit",
                path
            ),
            Self::BatchLimit { count, maximum } => write!(
                formatter,
                "document activation requested {count} paths; maximum is {maximum}"
            ),
            Self::RuntimeUnavailable(error) => {
                write!(formatter, "activation runtime unavailable: {error}")
            }
            Self::BusyLiveReceiver(error) => {
                write!(formatter, "activation queue owner is busy: {error}")
            }
            Self::QueueFull => formatter.write_str("activation queue is full"),
            Self::ConnectionFailed(error) => {
                write!(formatter, "could not deliver activation request: {error}")
            }
            Self::Protocol(error) => write!(formatter, "activation protocol error: {error}"),
            Self::Io(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for ActivationError {}

struct QueueShared {
    queue: Mutex<VecDeque<PathBuf>>,
    waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    metrics: Mutex<ActivationMetrics>,
}

impl fmt::Debug for QueueShared {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QueueShared")
            .finish_non_exhaustive()
    }
}

impl QueueShared {
    fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            waker: Mutex::new(None),
            metrics: Mutex::new(ActivationMetrics {
                queue_capacity: QUEUE_CAPACITY,
                ..ActivationMetrics::default()
            }),
        }
    }

    fn enqueue(&self, paths: Vec<PathBuf>) -> Result<(), ActivationError> {
        let waker = {
            let mut queue = self.queue.lock().expect("activation queue lock healthy");
            if queue.len().saturating_add(paths.len()) > QUEUE_CAPACITY {
                self.metrics
                    .lock()
                    .expect("activation metrics lock healthy")
                    .rejected_full += 1;
                return Err(ActivationError::QueueFull);
            }
            queue.extend(paths);
            let mut metrics = self
                .metrics
                .lock()
                .expect("activation metrics lock healthy");
            metrics.queue_depth = queue.len();
            metrics.queue_high_watermark = metrics.queue_high_watermark.max(metrics.queue_depth);
            metrics.enqueued_batches += 1;
            self.waker
                .lock()
                .expect("activation waker lock healthy")
                .clone()
        };
        if let Some(waker) = waker {
            waker();
        }
        Ok(())
    }

    fn note_ipc_error(&self) {
        self.metrics
            .lock()
            .expect("activation metrics lock healthy")
            .ipc_errors += 1;
    }

    #[cfg(test)]
    fn front(&self) -> Option<PathBuf> {
        self.queue
            .lock()
            .expect("activation queue lock healthy")
            .front()
            .cloned()
    }

    fn pop_front(&self) -> Option<PathBuf> {
        let path = self
            .queue
            .lock()
            .expect("activation queue lock healthy")
            .pop_front();
        let mut metrics = self
            .metrics
            .lock()
            .expect("activation metrics lock healthy");
        metrics.queue_depth = metrics
            .queue_depth
            .saturating_sub(usize::from(path.is_some()));
        path
    }

    fn set_waker(&self, waker: Arc<dyn Fn() + Send + Sync>) {
        *self.waker.lock().expect("activation waker lock healthy") = Some(waker);
        let should_wake = !self
            .queue
            .lock()
            .expect("activation queue lock healthy")
            .is_empty();
        if should_wake {
            if let Some(waker) = self
                .waker
                .lock()
                .expect("activation waker lock healthy")
                .clone()
            {
                waker();
            }
        }
    }

    fn metrics(&self) -> ActivationMetrics {
        self.metrics
            .lock()
            .expect("activation metrics lock healthy")
            .clone()
    }
}

pub fn help_text() -> &'static str {
    "Usage: festerm [--open [--] paths...] [--help] [--version]\n\nPaths passed positionally by the operating system are opened as documents. Use -- to open a filename that begins with '-'."
}

pub fn parse_activation_args<I, S>(
    args: I,
    originating_cwd: &Path,
) -> Result<CliActivation, ActivationError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut saw_open = false;
    let mut collecting_after_dashdash = false;
    let mut paths = Vec::new();
    for arg in args.into_iter().map(Into::into) {
        if !collecting_after_dashdash {
            if is_macos_process_serial_number_arg(&arg) {
                continue;
            }
            if arg == OsStr::new("--help") || arg == OsStr::new("-h") {
                if saw_open || !paths.is_empty() {
                    return Err(ActivationError::UnsupportedFlag(arg));
                }
                return Ok(CliActivation::Help);
            }
            if arg == OsStr::new("--version") {
                if saw_open || !paths.is_empty() {
                    return Err(ActivationError::UnsupportedFlag(arg));
                }
                return Ok(CliActivation::Version);
            }
            if arg == OsStr::new("--open") {
                if saw_open || !paths.is_empty() {
                    return Err(ActivationError::UnsupportedFlag(arg));
                }
                saw_open = true;
                continue;
            }
            if arg == OsStr::new("--") {
                collecting_after_dashdash = true;
                continue;
            }
            if arg.to_string_lossy().starts_with('-') {
                return Err(ActivationError::UnsupportedFlag(arg));
            }
        }
        paths.push(resolve_against_cwd(PathBuf::from(arg), originating_cwd));
    }

    if paths.is_empty() {
        if saw_open || collecting_after_dashdash {
            Err(ActivationError::MissingOpenPath)
        } else {
            Ok(CliActivation::None)
        }
    } else {
        Ok(CliActivation::Open { paths })
    }
}

#[cfg(target_os = "macos")]
fn is_macos_process_serial_number_arg(arg: &OsStr) -> bool {
    arg.to_string_lossy().starts_with("-psn_")
}

#[cfg(not(target_os = "macos"))]
fn is_macos_process_serial_number_arg(_arg: &OsStr) -> bool {
    false
}

pub fn parse_env_activation_args() -> Result<CliActivation, ActivationError> {
    let cwd = env::current_dir().map_err(|error| ActivationError::Io(error.to_string()))?;
    parse_activation_args(env::args_os().skip(1), &cwd)
}

pub fn prepare_startup(
    cli: CliActivation,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
) -> Result<StartupAction, ActivationError> {
    match cli {
        CliActivation::Help => Ok(StartupAction::Help),
        CliActivation::Version => Ok(StartupAction::Version),
        CliActivation::Open { paths } => {
            let batch = bounded_batch(paths)?;
            match forward_to_primary(&batch) {
                Ok(()) => Ok(StartupAction::ForwardedToPrimary),
                Err(ForwardError::NoPrimary) => match become_primary(Some(batch.clone()), waker) {
                    Err(ActivationError::BusyLiveReceiver(error)) => {
                        retry_forward_to_starting_primary(&batch, error)
                    }
                    result => result,
                },
                Err(ForwardError::QueueFull) => Err(ActivationError::QueueFull),
                Err(ForwardError::Rejected(error)) => Err(ActivationError::Protocol(error)),
                Err(ForwardError::ConnectionFailed(error)) => {
                    Err(ActivationError::ConnectionFailed(error))
                }
            }
        }
        CliActivation::None => match become_primary(None, waker) {
            Ok(action) => Ok(action),
            Err(ActivationError::BusyLiveReceiver(_)) => Ok(StartupAction::IndependentNoActivation),
            Err(ActivationError::ConnectionFailed(_)) => Ok(StartupAction::IndependentNoActivation),
            Err(error) => Err(error),
        },
    }
}

fn retry_forward_to_starting_primary(
    batch: &DocumentActivationBatch,
    busy_error: String,
) -> Result<StartupAction, ActivationError> {
    let deadline = Instant::now() + STARTUP_FORWARD_RETRY_TIMEOUT;
    loop {
        thread::sleep(STARTUP_FORWARD_RETRY_INTERVAL);
        match forward_to_primary(batch) {
            Ok(()) => return Ok(StartupAction::ForwardedToPrimary),
            Err(ForwardError::NoPrimary) if Instant::now() < deadline => {}
            Err(ForwardError::NoPrimary) => {
                return Err(ActivationError::BusyLiveReceiver(busy_error));
            }
            Err(ForwardError::QueueFull) => return Err(ActivationError::QueueFull),
            Err(ForwardError::Rejected(error)) => return Err(ActivationError::Protocol(error)),
            Err(ForwardError::ConnectionFailed(error)) => {
                return Err(ActivationError::ConnectionFailed(error));
            }
        }
    }
}

fn become_primary(
    initial: Option<DocumentActivationBatch>,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
) -> Result<StartupAction, ActivationError> {
    let endpoint = Endpoint::new()?;
    let lock = acquire_primary_lock(&endpoint)?;
    endpoint.remove_stale();
    let shared = Arc::new(QueueShared::new());
    if let Some(waker) = waker {
        shared.set_waker(waker);
    }
    let queue = ActivationQueue {
        shared: Arc::clone(&shared),
    };
    if let Some(batch) = initial.clone() {
        shared.enqueue(batch.paths)?;
    }
    let server = endpoint.spawn_server(Arc::clone(&shared), lock)?;
    Ok(StartupAction::Primary { queue, server })
}

fn acquire_primary_lock(endpoint: &Endpoint) -> Result<File, ActivationError> {
    #[cfg(unix)]
    let lock = create_private_file(&endpoint.lock_path)?;
    #[cfg(windows)]
    let lock = {
        let guard = festerm_windows_security::restrict_default_dacl_to_current_user()
            .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&endpoint.lock_path)
            .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
        guard
            .restore()
            .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
        lock
    };
    match lock.try_lock_exclusive() {
        Ok(()) => Ok(lock),
        Err(error) if lock_is_contended(&error) => Err(ActivationError::BusyLiveReceiver(
            "another fesTerm process owns document activation".to_owned(),
        )),
        Err(error) => Err(ActivationError::RuntimeUnavailable(error.to_string())),
    }
}

fn bounded_batch<I>(paths: I) -> Result<DocumentActivationBatch, ActivationError>
where
    I: IntoIterator<Item = PathBuf>,
{
    let paths: Vec<PathBuf> = paths.into_iter().collect();
    if paths.is_empty() {
        return Err(ActivationError::MissingOpenPath);
    }
    if paths.len() > MAX_BATCH_PATHS {
        return Err(ActivationError::BatchLimit {
            count: paths.len(),
            maximum: MAX_BATCH_PATHS,
        });
    }
    for path in &paths {
        let length = os_path_bytes(path).len();
        if length > MAX_PATH_BYTES {
            return Err(ActivationError::PathLimit {
                path: path.clone(),
                maximum: MAX_PATH_BYTES,
            });
        }
    }
    Ok(DocumentActivationBatch { paths })
}

fn resolve_against_cwd(path: PathBuf, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn lock_is_contended(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock
        || error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
}

#[derive(Debug)]
struct Endpoint {
    lock_path: PathBuf,
    #[cfg(unix)]
    socket_path: PathBuf,
    #[cfg(windows)]
    pipe_name: String,
}

impl Endpoint {
    fn new() -> Result<Self, ActivationError> {
        let root = activation_root()?;
        create_private_dir(&root)?;
        Self::for_root(root)
    }

    fn for_root(root: PathBuf) -> Result<Self, ActivationError> {
        let lock_path = root.join("activation.lock");
        #[cfg(unix)]
        {
            Ok(Self {
                socket_path: root.join("activation.sock"),
                lock_path,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                pipe_name: pipe_name_for_root(&root),
                lock_path,
            })
        }
    }

    fn remove_stale(&self) {
        #[cfg(unix)]
        {
            let _ = fs::remove_file(&self.socket_path);
        }
    }

    fn spawn_server(
        &self,
        shared: Arc<QueueShared>,
        lock: File,
    ) -> Result<DocumentActivationServer, ActivationError> {
        let cancelled = Arc::new(AtomicBool::new(false));
        #[cfg(unix)]
        {
            let listener = UnixListener::bind(&self.socket_path)
                .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
            set_private_file(&self.socket_path)?;
            let cancelled_thread = Arc::clone(&cancelled);
            let shared_thread = Arc::clone(&shared);
            let thread = thread::Builder::new()
                .name("festerm-document-activation".to_owned())
                .spawn(move || accept_unix_loop(listener, shared_thread, cancelled_thread))
                .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
            Ok(DocumentActivationServer {
                cancelled,
                thread: Some(thread),
                lock,
                socket_path: self.socket_path.clone(),
            })
        }
        #[cfg(windows)]
        {
            let listener = create_secure_pipe_listener(&self.pipe_name, true)?;
            let pipe_name = self.pipe_name.clone();
            let cancelled_thread = Arc::clone(&cancelled);
            let shared_thread = Arc::clone(&shared);
            let thread = thread::Builder::new()
                .name("festerm-document-activation".to_owned())
                .spawn(move || {
                    accept_windows_loop(&pipe_name, listener, shared_thread, cancelled_thread)
                })
                .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
            Ok(DocumentActivationServer {
                cancelled,
                thread: Some(thread),
                lock,
            })
        }
    }
}

fn activation_root() -> Result<PathBuf, ActivationError> {
    let sessiond = festerm_sessiond::runtime_root()
        .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
    Ok(sessiond.join("document-activation"))
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> Result<(), ActivationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_dir(path, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
            let metadata = fs::symlink_metadata(path)
                .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
            validate_private_dir(path, &metadata)
        }
        Err(error) => Err(ActivationError::RuntimeUnavailable(error.to_string())),
    }
}

#[cfg(unix)]
fn validate_private_dir(path: &Path, metadata: &fs::Metadata) -> Result<(), ActivationError> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ActivationError::RuntimeUnavailable(format!(
            "{} is not a private directory",
            path.display()
        )));
    }
    let current_uid = nix::unistd::Uid::current().as_raw();
    if metadata.uid() != current_uid {
        return Err(ActivationError::RuntimeUnavailable(format!(
            "{} is not owned by the current user",
            path.display()
        )));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o700 {
        return Err(ActivationError::RuntimeUnavailable(format!(
            "{} is not private (mode {mode:o})",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> Result<File, ActivationError> {
    let existed = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ActivationError::RuntimeUnavailable(format!(
                    "{} is not a private file",
                    path.display()
                )));
            }
            validate_private_file(path, &metadata)?;
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(ActivationError::RuntimeUnavailable(error.to_string())),
    };
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
    if !existed {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
    validate_private_file(path, &metadata)?;
    Ok(file)
}

#[cfg(unix)]
fn validate_private_file(path: &Path, metadata: &fs::Metadata) -> Result<(), ActivationError> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ActivationError::RuntimeUnavailable(format!(
            "{} is not a private file",
            path.display()
        )));
    }
    let current_uid = nix::unistd::Uid::current().as_raw();
    if metadata.uid() != current_uid {
        return Err(ActivationError::RuntimeUnavailable(format!(
            "{} is not owned by the current user",
            path.display()
        )));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o600 {
        return Err(ActivationError::RuntimeUnavailable(format!(
            "{} is not private (mode {mode:o})",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn create_private_dir(path: &Path) -> Result<(), ActivationError> {
    let guard = festerm_windows_security::restrict_default_dacl_to_current_user()
        .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
    fs::create_dir_all(path)
        .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
    guard
        .restore()
        .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> Result<(), ActivationError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))
}

#[cfg(windows)]
fn pipe_name_for_root(root: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    format!(
        r"\\.\pipe\festerm-document-activation-{:016x}",
        hasher.finish()
    )
}

#[cfg(windows)]
fn create_secure_pipe_listener(
    pipe_name: &str,
    first: bool,
) -> Result<PipeListener, ActivationError> {
    let guard = festerm_windows_security::restrict_default_dacl_to_current_user()
        .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
    let listener = PipeListener::bind(pipe_name, first)
        .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
    guard
        .restore()
        .map_err(|error| ActivationError::RuntimeUnavailable(error.to_string()))?;
    Ok(listener)
}

#[cfg(unix)]
fn accept_unix_loop(listener: UnixListener, shared: Arc<QueueShared>, cancelled: Arc<AtomicBool>) {
    while !cancelled.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if cancelled.load(Ordering::Acquire) {
                    break;
                }
                if stream.set_read_timeout(Some(IO_TIMEOUT)).is_err()
                    || stream.set_write_timeout(Some(IO_TIMEOUT)).is_err()
                {
                    shared.note_ipc_error();
                    continue;
                }
                handle_stream(stream, &shared);
            }
            Err(_) => {
                shared.note_ipc_error();
                if cancelled.load(Ordering::Acquire) {
                    break;
                }
            }
        }
    }
}

#[cfg(windows)]
fn accept_windows_loop(
    pipe_name: &str,
    initial_listener: PipeListener,
    shared: Arc<QueueShared>,
    cancelled: Arc<AtomicBool>,
) {
    let mut initial = Some(initial_listener);
    while !cancelled.load(Ordering::Acquire) {
        let listener = match initial.take() {
            Some(listener) => listener,
            None => match create_secure_pipe_listener(pipe_name, false) {
                Ok(listener) => listener,
                Err(_) => {
                    shared.note_ipc_error();
                    thread::sleep(WINDOWS_ACCEPT_RETRY_INTERVAL);
                    continue;
                }
            },
        };
        match listener.accept(&cancelled) {
            Ok(mut pipe) => {
                pipe.set_read_timeout(IO_TIMEOUT);
                pipe.set_write_timeout(IO_TIMEOUT);
                handle_stream(pipe, &shared);
            }
            Err(_) if cancelled.load(Ordering::Acquire) => break,
            Err(_) => shared.note_ipc_error(),
        }
    }
}

trait ActivationStream: Read + Write {}
impl<T: Read + Write> ActivationStream for T {}

fn handle_stream<S: ActivationStream>(mut stream: S, shared: &QueueShared) {
    let result = read_request(&mut stream)
        .and_then(|batch| {
            bounded_batch(batch.paths).map_err(|error| {
                if matches!(
                    error,
                    ActivationError::PathLimit { .. } | ActivationError::BatchLimit { .. }
                ) {
                    shared
                        .metrics
                        .lock()
                        .expect("activation metrics lock healthy")
                        .rejected_limit += 1;
                }
                error_to_io(error)
            })
        })
        .and_then(|batch| match shared.enqueue(batch.paths) {
            Ok(()) => write_ack(&mut stream, ACK_ENQUEUED, ""),
            Err(ActivationError::QueueFull) => write_ack(&mut stream, ACK_QUEUE_FULL, "queue full"),
            Err(error) => write_ack(&mut stream, ACK_REJECTED, &error.to_string()),
        });
    if result.is_err() {
        shared.note_ipc_error();
        let _ = write_ack(&mut stream, ACK_REJECTED, "invalid activation request");
    }
}

#[derive(Debug)]
enum ForwardError {
    NoPrimary,
    QueueFull,
    Rejected(String),
    ConnectionFailed(String),
}

fn forward_to_primary(batch: &DocumentActivationBatch) -> Result<(), ForwardError> {
    let endpoint =
        Endpoint::new().map_err(|error| ForwardError::ConnectionFailed(error.to_string()))?;
    forward_to_endpoint(&endpoint, batch)
}

fn forward_to_endpoint(
    endpoint: &Endpoint,
    batch: &DocumentActivationBatch,
) -> Result<(), ForwardError> {
    let frame =
        encode_request(batch).map_err(|error| ForwardError::ConnectionFailed(error.to_string()))?;
    #[cfg(unix)]
    {
        match UnixStream::connect(&endpoint.socket_path) {
            Ok(mut stream) => {
                stream
                    .set_read_timeout(Some(IO_TIMEOUT))
                    .map_err(|error| ForwardError::ConnectionFailed(error.to_string()))?;
                stream
                    .set_write_timeout(Some(IO_TIMEOUT))
                    .map_err(|error| ForwardError::ConnectionFailed(error.to_string()))?;
                send_frame_and_read_ack(&mut stream, &frame)
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                Err(ForwardError::NoPrimary)
            }
            Err(error) => Err(ForwardError::ConnectionFailed(error.to_string())),
        }
    }
    #[cfg(windows)]
    {
        let cancelled = AtomicBool::new(false);
        match Pipe::connect(&endpoint.pipe_name, CONNECT_TIMEOUT, &cancelled) {
            Ok(mut pipe) => {
                pipe.verify_server_user().map_err(|error| {
                    ForwardError::ConnectionFailed(format!(
                        "could not verify activation receiver identity: {error}"
                    ))
                })?;
                if let Err(error) = pipe.allow_server_foreground() {
                    tracing::warn!(
                        target: "festerm::app",
                        %error,
                        "document receiver may remain in the background: foreground permission was not granted"
                    );
                }
                pipe.set_read_timeout(IO_TIMEOUT);
                pipe.set_write_timeout(IO_TIMEOUT);
                send_frame_and_read_ack(&mut pipe, &frame)
            }
            Err(error) if matches!(error.kind(), io::ErrorKind::NotFound) => {
                Err(ForwardError::NoPrimary)
            }
            Err(error) if matches!(error.kind(), io::ErrorKind::TimedOut) => Err(
                ForwardError::ConnectionFailed("primary activation pipe is busy".to_owned()),
            ),
            Err(error) => Err(ForwardError::ConnectionFailed(error.to_string())),
        }
    }
}

fn send_frame_and_read_ack<S: ActivationStream>(
    stream: &mut S,
    frame: &[u8],
) -> Result<(), ForwardError> {
    stream
        .write_all(frame)
        .map_err(|error| ForwardError::ConnectionFailed(error.to_string()))?;
    let (status, message) =
        read_ack(stream).map_err(|error| ForwardError::ConnectionFailed(error.to_string()))?;
    match status {
        ACK_ENQUEUED => Ok(()),
        ACK_QUEUE_FULL => Err(ForwardError::QueueFull),
        ACK_REJECTED => Err(ForwardError::Rejected(message)),
        other => Err(ForwardError::ConnectionFailed(format!(
            "unknown ack status {other}"
        ))),
    }
}

fn encode_request(batch: &DocumentActivationBatch) -> Result<Vec<u8>, ActivationError> {
    let mut payload = Vec::new();
    push_u32(&mut payload, batch.paths.len())?;
    for path in &batch.paths {
        let bytes = os_path_bytes(path);
        if bytes.len() > MAX_PATH_BYTES {
            return Err(ActivationError::PathLimit {
                path: path.clone(),
                maximum: MAX_PATH_BYTES,
            });
        }
        push_u32(&mut payload, bytes.len())?;
        payload.extend_from_slice(&bytes);
    }
    if payload.len() > MAX_FRAME_BYTES {
        return Err(ActivationError::Protocol(
            "activation request is too large".to_owned(),
        ));
    }
    let mut frame = Vec::with_capacity(FRAME_MAGIC.len() + 4 + payload.len());
    frame.extend_from_slice(FRAME_MAGIC);
    push_u32(&mut frame, payload.len())?;
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn read_request<S: Read>(stream: &mut S) -> io::Result<DocumentActivationBatch> {
    let mut magic = [0; 4];
    stream.read_exact(&mut magic)?;
    if &magic != FRAME_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad activation magic",
        ));
    }
    let payload_len = read_u32(stream)? as usize;
    if payload_len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "activation frame too large",
        ));
    }
    let mut payload = vec![0; payload_len];
    stream.read_exact(&mut payload)?;
    let mut cursor = &payload[..];
    let count = read_u32(&mut cursor)? as usize;
    if count > MAX_BATCH_PATHS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "too many activation paths",
        ));
    }
    let mut paths = Vec::with_capacity(count);
    for _ in 0..count {
        let len = read_u32(&mut cursor)? as usize;
        if len > MAX_PATH_BYTES || len > cursor.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "activation path length invalid",
            ));
        }
        let (bytes, rest) = cursor.split_at(len);
        cursor = rest;
        paths.push(path_from_os_bytes(bytes)?);
    }
    if !cursor.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing activation bytes",
        ));
    }
    Ok(DocumentActivationBatch { paths })
}

fn write_ack<S: Write>(stream: &mut S, status: u8, message: &str) -> io::Result<()> {
    let message = message.as_bytes();
    let mut frame = Vec::with_capacity(5 + message.len());
    frame.push(status);
    push_u32(&mut frame, message.len()).map_err(error_to_io)?;
    frame.extend_from_slice(message);
    stream.write_all(&frame)
}

fn read_ack<S: Read>(stream: &mut S) -> io::Result<(u8, String)> {
    let mut status = [0; 1];
    stream.read_exact(&mut status)?;
    let len = read_u32(stream)? as usize;
    if len > 16 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "activation ack too large",
        ));
    }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes)?;
    let message = String::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    Ok((status[0], message))
}

fn push_u32(out: &mut Vec<u8>, value: usize) -> Result<(), ActivationError> {
    let value = u32::try_from(value)
        .map_err(|_| ActivationError::Protocol("activation integer overflow".to_owned()))?;
    out.extend_from_slice(&value.to_be_bytes());
    Ok(())
}

fn read_u32<R: Read>(reader: &mut R) -> io::Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

fn error_to_io(error: ActivationError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(unix)]
fn os_path_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
fn path_from_os_bytes(bytes: &[u8]) -> io::Result<PathBuf> {
    Ok(PathBuf::from(OsStr::from_bytes(bytes)))
}

#[cfg(windows)]
fn os_path_bytes(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(windows)]
fn path_from_os_bytes(bytes: &[u8]) -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    if !bytes.len().is_multiple_of(2) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "odd utf-16 path bytes",
        ));
    }
    let wide: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    Ok(PathBuf::from(OsString::from_wide(&wide)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn parses_open_and_dash_prefixed_paths_without_utf8_loss() {
        let cwd = PathBuf::from("/origin");
        let parsed = parse_activation_args(
            [
                OsString::from("--open"),
                OsString::from("--"),
                OsString::from("-file.md"),
            ],
            &cwd,
        )
        .unwrap();
        assert_eq!(
            parsed,
            CliActivation::Open {
                paths: vec![cwd.join("-file.md")]
            }
        );
    }

    #[test]
    fn positional_paths_are_document_activation_but_flags_are_rejected() {
        let cwd = PathBuf::from("/origin");
        assert_eq!(
            parse_activation_args([OsString::from("note.md")], &cwd).unwrap(),
            CliActivation::Open {
                paths: vec![cwd.join("note.md")]
            }
        );
        assert!(matches!(
            parse_activation_args([OsString::from("--unknown")], &cwd),
            Err(ActivationError::UnsupportedFlag(_))
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_legacy_process_serial_number_is_ignored_before_delimiter() {
        let cwd = PathBuf::from("/origin");
        assert_eq!(
            parse_activation_args(
                [OsString::from("-psn_0_12345"), OsString::from("note.md")],
                &cwd
            )
            .unwrap(),
            CliActivation::Open {
                paths: vec![cwd.join("note.md")]
            }
        );
        assert_eq!(
            parse_activation_args([OsString::from("--"), OsString::from("-psn_0_12345")], &cwd)
                .unwrap(),
            CliActivation::Open {
                paths: vec![cwd.join("-psn_0_12345")]
            }
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn mac_legacy_process_serial_number_is_not_special_elsewhere() {
        let cwd = PathBuf::from("/origin");
        assert!(matches!(
            parse_activation_args([OsString::from("-psn_0_12345")], &cwd),
            Err(ActivationError::UnsupportedFlag(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unix_paths_round_trip_non_utf8_bytes() {
        let path = PathBuf::from(OsStr::from_bytes(b"/origin/\xff.md"));
        let batch = DocumentActivationBatch {
            paths: vec![path.clone()],
        };
        let frame = encode_request(&batch).unwrap();
        let decoded = read_request(&mut &frame[..]).unwrap();
        assert_eq!(decoded.paths, vec![path]);
    }

    #[test]
    fn bounded_queue_reports_full_without_dropping() {
        let shared = QueueShared::new();
        let one = bounded_batch([PathBuf::from("/one")]).unwrap();
        let too_many = (0..=QUEUE_CAPACITY).map(|index| PathBuf::from(format!("/{index}")));
        shared.enqueue(one.paths).unwrap();
        assert_eq!(
            shared.enqueue(too_many.collect()),
            Err(ActivationError::QueueFull)
        );
        assert_eq!(shared.front(), Some(PathBuf::from("/one")));
        assert_eq!(shared.pop_front(), Some(PathBuf::from("/one")));
        assert_eq!(shared.pop_front(), None);
    }

    #[cfg(unix)]
    #[test]
    fn ipc_stream_ack_means_the_receiver_owns_the_batch() {
        let (mut client, server) = UnixStream::pair().unwrap();
        client.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
        client.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
        let shared = Arc::new(QueueShared::new());
        let server_shared = Arc::clone(&shared);
        let worker = thread::spawn(move || handle_stream(server, &server_shared));
        let batch = DocumentActivationBatch {
            paths: vec![PathBuf::from("/opened.md")],
        };
        let frame = encode_request(&batch).unwrap();
        send_frame_and_read_ack(&mut client, &frame).unwrap();
        worker.join().unwrap();
        assert_eq!(shared.front(), Some(PathBuf::from("/opened.md")));
        assert_eq!(shared.pop_front(), Some(PathBuf::from("/opened.md")));
    }

    #[test]
    fn endpoint_forwarding_ack_full_stale_and_shutdown_are_bounded() {
        // Unix socket paths must fit even when the checkout or TMPDIR is long.
        #[cfg(unix)]
        let directory = tempfile::Builder::new()
            .prefix("festerm-activation-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")
            .unwrap();
        #[cfg(windows)]
        let directory = tempfile::tempdir().unwrap();
        create_private_dir(directory.path()).unwrap();
        let endpoint = Endpoint::for_root(directory.path().to_owned()).unwrap();
        #[cfg(unix)]
        fs::write(&endpoint.socket_path, b"stale").unwrap();
        let lock = acquire_primary_lock(&endpoint).unwrap();
        endpoint.remove_stale();
        let shared = Arc::new(QueueShared::new());
        let server = endpoint.spawn_server(Arc::clone(&shared), lock).unwrap();

        let batch = DocumentActivationBatch {
            paths: vec![PathBuf::from("/opened.md")],
        };
        forward_to_endpoint(&endpoint, &batch).unwrap();
        assert_eq!(shared.pop_front(), Some(PathBuf::from("/opened.md")));

        let full = (0..QUEUE_CAPACITY)
            .map(|index| PathBuf::from(format!("/queued-{index}.md")))
            .collect();
        shared.enqueue(full).unwrap();
        let rejected = DocumentActivationBatch {
            paths: vec![PathBuf::from("/rejected.md")],
        };
        assert!(matches!(
            forward_to_endpoint(&endpoint, &rejected),
            Err(ForwardError::QueueFull)
        ));

        drop(server);
        #[cfg(unix)]
        assert!(!endpoint.socket_path.exists());
    }

    #[test]
    fn enqueue_wakes_without_polling() {
        let shared = QueueShared::new();
        let counter = Arc::new(Counter(AtomicUsize::new(0)));
        let batch = bounded_batch([PathBuf::from("/one")]).unwrap();
        shared.enqueue(batch.paths).unwrap();
        let waker: Arc<dyn Fn() + Send + Sync> = {
            let counter = Arc::clone(&counter);
            Arc::new(move || {
                counter.0.fetch_add(1, Ordering::SeqCst);
            })
        };
        shared.set_waker(waker);
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
    }

    struct Counter(AtomicUsize);
}
