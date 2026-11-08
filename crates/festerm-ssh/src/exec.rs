//! Bounded raw SSH exec and subsystem streams.
//!
//! This module opens an SSH session channel with an exec or subsystem request
//! and no PTY. It exposes stdout as `Read`, stdin as `Write`, and keeps stderr
//! as a bounded diagnostic side channel.

use std::{
    collections::VecDeque,
    fmt, io,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use festerm_session::{
    noop_session_event_notifier, SessionErrorKind, DEFAULT_COMMAND_QUEUE_CAPACITY,
};

use crate::{
    establish_authenticated_handle, is_sha256_fingerprint, session_error_kind_for_failure,
    stop_handle, wait_for_ssh_operation, AuthenticatedHandleAttempt, HostKeyPolicy,
    SshAuthentication, SshConnectionProfile, SshWorkerFoundation, WorkerCommand,
    WorkerCommandReceiver, WorkerShared, WorkerWait, COMMAND_POLL_INTERVAL,
};

const DEFAULT_STDOUT_QUEUE_CAPACITY: usize = 64;
const DEFAULT_STDIN_QUEUE_CAPACITY: usize = 64;
const DEFAULT_STDERR_CAPACITY: usize = 16 * 1024;
const DEFAULT_STREAM_TIMEOUT: Duration = Duration::from_millis(20);
const DEFAULT_EXEC_QUEUE_CHUNK_BYTES: usize = 16 * 1024;
const MAX_RAW_EXEC_COMMAND_BYTES: usize = 4096;
const MAX_SUBSYSTEM_NAME_BYTES: usize = 128;

enum RawRequest {
    Exec(String),
    Subsystem(String),
}

impl RawRequest {
    fn name(&self) -> &str {
        match self {
            Self::Exec(name) | Self::Subsystem(name) => name,
        }
    }
}

/// Bounded options for one raw SSH exec stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshRawExecOptions {
    known_host_fingerprint: Option<String>,
    stdout_queue_capacity: usize,
    stdin_queue_capacity: usize,
    stderr_capacity: usize,
    read_timeout: Duration,
    write_timeout: Duration,
}

impl SshRawExecOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pins the connection to an already verified host-key fingerprint.
    ///
    /// This initial raw-exec backend is pinned-only: callers must supply this
    /// value, and the SSH worker rejects unknown or mismatched host keys
    /// without prompting.
    pub fn with_known_host_fingerprint(
        mut self,
        fingerprint: impl Into<String>,
    ) -> Result<Self, SshRawExecOptionsError> {
        let fingerprint = fingerprint.into();
        if !is_strict_sha256_fingerprint(&fingerprint) {
            return Err(SshRawExecOptionsError::InvalidHostKeyFingerprint);
        }
        self.known_host_fingerprint = Some(fingerprint);
        Ok(self)
    }

    pub fn with_queue_capacities(
        mut self,
        stdout_queue_capacity: usize,
        stdin_queue_capacity: usize,
    ) -> Result<Self, SshRawExecOptionsError> {
        if stdout_queue_capacity == 0 || stdin_queue_capacity == 0 {
            return Err(SshRawExecOptionsError::ZeroQueueCapacity);
        }
        self.stdout_queue_capacity = stdout_queue_capacity;
        self.stdin_queue_capacity = stdin_queue_capacity;
        Ok(self)
    }

    pub fn with_stderr_capacity(mut self, capacity: usize) -> Self {
        self.stderr_capacity = capacity;
        self
    }

    pub fn with_io_timeouts(
        mut self,
        read_timeout: Duration,
        write_timeout: Duration,
    ) -> Result<Self, SshRawExecOptionsError> {
        if read_timeout.is_zero() || write_timeout.is_zero() {
            return Err(SshRawExecOptionsError::ZeroTimeout);
        }
        self.read_timeout = read_timeout;
        self.write_timeout = write_timeout;
        Ok(self)
    }

    pub fn known_host_fingerprint(&self) -> Option<&str> {
        self.known_host_fingerprint.as_deref()
    }

    pub const fn stdout_queue_capacity(&self) -> usize {
        self.stdout_queue_capacity
    }

    pub const fn stdin_queue_capacity(&self) -> usize {
        self.stdin_queue_capacity
    }

    pub const fn stderr_capacity(&self) -> usize {
        self.stderr_capacity
    }

    pub const fn read_timeout(&self) -> Duration {
        self.read_timeout
    }

    pub const fn write_timeout(&self) -> Duration {
        self.write_timeout
    }
}

impl Default for SshRawExecOptions {
    fn default() -> Self {
        Self {
            known_host_fingerprint: None,
            stdout_queue_capacity: DEFAULT_STDOUT_QUEUE_CAPACITY,
            stdin_queue_capacity: DEFAULT_STDIN_QUEUE_CAPACITY,
            stderr_capacity: DEFAULT_STDERR_CAPACITY,
            read_timeout: DEFAULT_STREAM_TIMEOUT,
            write_timeout: DEFAULT_STREAM_TIMEOUT,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshRawExecOptionsError {
    MissingHostKeyFingerprint,
    InvalidHostKeyFingerprint,
    ZeroQueueCapacity,
    ZeroTimeout,
}

impl fmt::Display for SshRawExecOptionsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHostKeyFingerprint => formatter.write_str(
                "SSH raw exec requires a previously verified SHA256 host-key fingerprint",
            ),
            Self::InvalidHostKeyFingerprint => {
                formatter.write_str("SSH host-key fingerprint must be SHA256:<base64>")
            }
            Self::ZeroQueueCapacity => {
                formatter.write_str("SSH raw exec queue capacities must be nonzero")
            }
            Self::ZeroTimeout => formatter.write_str("SSH raw exec I/O timeouts must be nonzero"),
        }
    }
}

impl std::error::Error for SshRawExecOptionsError {}

/// Small pinned-host-key raw exec stream used by remote `sessiond` attachment.
///
/// This is the high-level API intended for callers that need only a bounded
/// `Read + Write + Send` bridge stream. It requires a previously verified host
/// key fingerprint and never prompts for trust.
pub struct SshExecStream {
    inner: SshRawExecSession,
}

impl SshExecStream {
    pub fn start(
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
        command: impl Into<String>,
        known_host_fingerprint: impl Into<String>,
    ) -> Result<Self, crate::SshSessionStartError> {
        let options = SshRawExecOptions::new()
            .with_known_host_fingerprint(known_host_fingerprint)
            .map_err(|_| crate::SshSessionStartError)?;
        let inner = SshRawExecSession::connect(profile, authentication, command, options)
            .map_err(|_| crate::SshSessionStartError)?;
        Ok(Self { inner })
    }

    pub fn command(&self) -> &str {
        self.inner.command()
    }

    pub fn stderr_diagnostic(&self) -> Vec<u8> {
        self.inner.stderr_diagnostic()
    }

    pub fn cancel(&mut self) {
        self.inner.cancel();
    }

    pub fn try_finish(&self) -> Option<Result<SshRawExecExit, SshRawExecError>> {
        self.inner.try_finish()
    }

    pub fn wait_finish(&mut self, timeout: Duration) -> Result<SshRawExecExit, SshRawExecError> {
        self.inner.wait_finish(timeout)
    }
}

impl io::Read for SshExecStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buffer)
    }
}

impl io::Write for SshExecStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Successful remote exec completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SshRawExecExit {
    status: u32,
}

impl SshRawExecExit {
    pub const fn status(self) -> u32 {
        self.status
    }
}

/// Public, content-free raw exec failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SshRawExecError {
    Connection {
        kind: SessionErrorKind,
        message: String,
    },
    ExecRejected,
    NonZeroExit {
        status: u32,
    },
    ExitSignal,
    ClosedBeforeExit,
    Cancelled,
    Timeout,
}

impl fmt::Display for SshRawExecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection { message, .. } => formatter.write_str(message),
            Self::ExecRejected => formatter.write_str("SSH raw exec request was rejected"),
            Self::NonZeroExit { status } => {
                write!(formatter, "SSH raw exec exited with status {status}")
            }
            Self::ExitSignal => formatter.write_str("SSH raw exec ended after a remote signal"),
            Self::ClosedBeforeExit => {
                formatter.write_str("SSH raw exec channel closed before exit status")
            }
            Self::Cancelled => formatter.write_str("SSH raw exec was cancelled"),
            Self::Timeout => formatter.write_str("SSH raw exec timed out"),
        }
    }
}

impl std::error::Error for SshRawExecError {}

impl SshRawExecError {
    fn io_error(&self) -> io::Error {
        let kind = match self {
            Self::Timeout => io::ErrorKind::TimedOut,
            Self::Cancelled => io::ErrorKind::Interrupted,
            Self::NonZeroExit { .. }
            | Self::ExecRejected
            | Self::ExitSignal
            | Self::ClosedBeforeExit
            | Self::Connection { .. } => io::ErrorKind::ConnectionAborted,
        };
        io::Error::new(kind, self.to_string())
    }
}

/// A bounded, blocking `Read + Write + Send` SSH exec or subsystem channel.
pub struct SshRawExecSession {
    command: String,
    stdout_receiver: Receiver<Vec<u8>>,
    stdout_buffer: VecDeque<u8>,
    stdin_sender: Option<SyncSender<Vec<u8>>>,
    command_sender: SyncSender<WorkerCommand>,
    shared: Arc<WorkerShared>,
    host_key_gate: Arc<crate::HostKeyDecisionGate>,
    result: Arc<ExecCompletion>,
    stderr: Arc<Mutex<BoundedDiagnostic>>,
    cancelled: Arc<AtomicBool>,
    read_timeout: Duration,
    write_timeout: Duration,
    join: Option<thread::JoinHandle<()>>,
}

type ExecCompletion = (
    Mutex<Option<Result<SshRawExecExit, SshRawExecError>>>,
    Condvar,
);

impl SshRawExecSession {
    /// Starts connecting in a dedicated worker thread and returns immediately.
    /// A previously verified host-key fingerprint is required in `options`;
    /// unknown or mismatched keys fail closed without prompting. Connection,
    /// auth, and exec failures are surfaced by `Read`, `Write`, `try_finish`,
    /// or `wait_finish`, never as success-shaped EOF.
    pub fn connect(
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
        command: impl Into<String>,
        options: SshRawExecOptions,
    ) -> io::Result<Self> {
        let command = command.into();
        if command.is_empty() || command.as_bytes().contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SSH raw exec command must be nonempty and must not contain NUL",
            ));
        }
        if command.len() > MAX_RAW_EXEC_COMMAND_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "SSH raw exec command is {} bytes; maximum is {MAX_RAW_EXEC_COMMAND_BYTES}",
                    command.len()
                ),
            ));
        }
        Self::start(profile, authentication, RawRequest::Exec(command), options)
    }

    /// Opens a named SSH subsystem without a shell, exec request or PTY.
    /// Authentication, host-key pinning, local binding and cancellation have
    /// the same semantics as [`Self::connect`].
    pub fn connect_subsystem(
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
        subsystem: impl Into<String>,
        options: SshRawExecOptions,
    ) -> io::Result<Self> {
        let subsystem = subsystem.into();
        if subsystem.is_empty()
            || subsystem.len() > MAX_SUBSYSTEM_NAME_BYTES
            || !subsystem
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SSH subsystem must be a 1-128 byte ASCII name using letters, digits, dot, dash or underscore",
            ));
        }
        Self::start(
            profile,
            authentication,
            RawRequest::Subsystem(subsystem),
            options,
        )
    }

    fn start(
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
        request: RawRequest,
        options: SshRawExecOptions,
    ) -> io::Result<Self> {
        let known_host_fingerprint = options.known_host_fingerprint.clone().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                SshRawExecOptionsError::MissingHostKeyFingerprint,
            )
        })?;
        let (foundation, command_receiver, host_key_resolver, _password_resolver) =
            SshWorkerFoundation::new_with_capacities(
                profile.clone(),
                DEFAULT_COMMAND_QUEUE_CAPACITY,
                16,
                noop_session_event_notifier(),
            );
        let SshWorkerFoundation {
            #[cfg(test)]
                profile: _foundation_profile,
            shared,
            command_sender,
            command_capacity: _command_capacity,
            event_receiver,
            host_key_gate,
            password_gate,
        } = foundation;
        drop(event_receiver);
        drop(host_key_resolver);
        let (stdout_sender, stdout_receiver) = mpsc::sync_channel(options.stdout_queue_capacity);
        let (stdin_sender, stdin_receiver) = mpsc::sync_channel(options.stdin_queue_capacity);
        let result = Arc::new((Mutex::new(None), Condvar::new()));
        let stderr = Arc::new(Mutex::new(BoundedDiagnostic::new(options.stderr_capacity)));
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_result = Arc::clone(&result);
        let worker_stderr = Arc::clone(&stderr);
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_shared = Arc::clone(&shared);
        let worker_host_key_gate = Arc::clone(&host_key_gate);
        let session_command = request.name().to_owned();
        let join = thread::Builder::new()
            .name("festerm-ssh-raw-exec".to_owned())
            .spawn(move || {
                // Publish the outcome before closing stdout, even if runtime
                // cleanup takes longer than a caller's read timeout.
                let stdout_lifetime = stdout_sender.clone();
                let worker_outcome = run_exec_thread(
                    profile,
                    authentication,
                    request,
                    known_host_fingerprint,
                    stdout_sender,
                    stdin_receiver,
                    worker_stderr,
                    worker_shared,
                    command_receiver,
                    worker_host_key_gate,
                    password_gate,
                    Arc::clone(&worker_cancelled),
                );
                let final_outcome = if worker_cancelled.load(Ordering::Acquire) {
                    Err(SshRawExecError::Cancelled)
                } else {
                    worker_outcome
                };
                let (lock, condvar) = &*worker_result;
                *lock
                    .lock()
                    .expect("SSH raw exec result lock is not poisoned") = Some(final_outcome);
                condvar.notify_all();
                drop(stdout_lifetime);
            })?;

        Ok(Self {
            command: session_command,
            stdout_receiver,
            stdout_buffer: VecDeque::new(),
            stdin_sender: Some(stdin_sender),
            command_sender,
            shared,
            host_key_gate,
            result,
            stderr,
            cancelled,
            read_timeout: options.read_timeout,
            write_timeout: options.write_timeout,
            join: Some(join),
        })
    }

    pub fn command(&self) -> &str {
        &self.command
    }

    pub fn stderr_diagnostic(&self) -> Vec<u8> {
        self.stderr
            .lock()
            .expect("SSH raw exec stderr lock is not poisoned")
            .bytes()
    }

    pub fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.shared.request_shutdown();
        self.host_key_gate.reject_pending();
        let _ = self.command_sender.try_send(WorkerCommand::Shutdown);
        let _ = self.stdin_sender.take();
    }

    pub fn try_finish(&self) -> Option<Result<SshRawExecExit, SshRawExecError>> {
        self.result
            .0
            .lock()
            .expect("SSH raw exec result lock is not poisoned")
            .clone()
    }

    pub fn wait_finish(&mut self, timeout: Duration) -> Result<SshRawExecExit, SshRawExecError> {
        let deadline = Instant::now() + timeout;
        let (lock, condvar) = &*self.result;
        let mut result = lock
            .lock()
            .expect("SSH raw exec result lock is not poisoned");
        while result.is_none() {
            let now = Instant::now();
            if now >= deadline {
                return Err(SshRawExecError::Timeout);
            }
            let wait = deadline.saturating_duration_since(now);
            let (next_result, timeout) = condvar
                .wait_timeout(result, wait)
                .expect("SSH raw exec result lock is not poisoned");
            result = next_result;
            if timeout.timed_out() && result.is_none() {
                return Err(SshRawExecError::Timeout);
            }
        }
        let outcome = result.clone().expect("checked result presence");
        drop(result);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        outcome
    }

    fn finished_error(&self) -> Option<SshRawExecError> {
        self.try_finish().and_then(Result::err)
    }
}

impl Drop for SshRawExecSession {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl io::Read for SshRawExecSession {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.stdout_buffer.is_empty() {
            match self.stdout_receiver.recv_timeout(self.read_timeout) {
                Ok(bytes) => self.stdout_buffer.extend(bytes),
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(error) = self.finished_error() {
                        return Err(error.io_error());
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "SSH raw exec stdout is pending",
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => match self.try_finish() {
                    Some(Ok(_)) => return Ok(0),
                    Some(Err(error)) => return Err(error.io_error()),
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionAborted,
                            "SSH raw exec stdout closed before completion",
                        ))
                    }
                },
            }
        }
        let count = buffer.len().min(self.stdout_buffer.len());
        for slot in &mut buffer[..count] {
            *slot = self
                .stdout_buffer
                .pop_front()
                .expect("count bounded by buffer length");
        }
        Ok(count)
    }
}

fn is_strict_sha256_fingerprint(fingerprint: &str) -> bool {
    let Some(encoded) = fingerprint.strip_prefix("SHA256:") else {
        return false;
    };
    encoded.len() == 43
        && encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
        && is_sha256_fingerprint(fingerprint)
}

impl io::Write for SshRawExecSession {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if let Some(error) = self.finished_error() {
            return Err(error.io_error());
        }
        let count = buffer.len().min(DEFAULT_EXEC_QUEUE_CHUNK_BYTES);
        let bytes = buffer[..count].to_vec();
        let deadline = Instant::now() + self.write_timeout;
        let Some(sender) = &self.stdin_sender else {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "SSH raw exec stdin is closed",
            ));
        };
        let mut pending = bytes;
        loop {
            match sender.try_send(pending) {
                Ok(()) => return Ok(count),
                Err(TrySendError::Full(bytes)) => {
                    pending = bytes;
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "SSH raw exec stdin queue is full",
                        ));
                    }
                    thread::sleep(COMMAND_POLL_INTERVAL.min(Duration::from_millis(5)));
                    if let Some(error) = self.finished_error() {
                        return Err(error.io_error());
                    }
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "SSH raw exec stdin is closed",
                    ));
                }
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct BoundedDiagnostic {
    bytes: VecDeque<u8>,
    capacity: usize,
}

impl BoundedDiagnostic {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::with_capacity(capacity.min(4096)),
            capacity,
        }
    }

    fn push(&mut self, data: &[u8]) {
        if self.capacity == 0 {
            return;
        }
        for byte in data {
            if self.bytes.len() == self.capacity {
                let _ = self.bytes.pop_front();
            }
            self.bytes.push_back(*byte);
        }
    }

    fn bytes(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn run_exec_thread(
    profile: SshConnectionProfile,
    authentication: SshAuthentication,
    request: RawRequest,
    known_host_fingerprint: String,
    stdout_sender: SyncSender<Vec<u8>>,
    stdin_receiver: Receiver<Vec<u8>>,
    stderr: Arc<Mutex<BoundedDiagnostic>>,
    shared: Arc<WorkerShared>,
    command_receiver: WorkerCommandReceiver,
    host_key_gate: Arc<crate::HostKeyDecisionGate>,
    password_gate: Arc<crate::PasswordDecisionGate>,
    cancelled: Arc<AtomicBool>,
) -> Result<SshRawExecExit, SshRawExecError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| SshRawExecError::Connection {
            kind: SessionErrorKind::Spawn,
            message: format!("SSH raw exec worker could not start: {error}"),
        })?;
    runtime.block_on(async move {
        run_exec_worker(
            profile,
            authentication,
            request,
            known_host_fingerprint,
            stdout_sender,
            stdin_receiver,
            stderr,
            shared,
            command_receiver,
            host_key_gate,
            password_gate,
            cancelled,
        )
        .await
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_exec_worker(
    profile: SshConnectionProfile,
    authentication: SshAuthentication,
    request: RawRequest,
    known_host_fingerprint: String,
    stdout_sender: SyncSender<Vec<u8>>,
    stdin_receiver: Receiver<Vec<u8>>,
    stderr: Arc<Mutex<BoundedDiagnostic>>,
    shared: Arc<WorkerShared>,
    command_receiver: WorkerCommandReceiver,
    host_key_gate: Arc<crate::HostKeyDecisionGate>,
    password_gate: Arc<crate::PasswordDecisionGate>,
    cancelled: Arc<AtomicBool>,
) -> Result<SshRawExecExit, SshRawExecError> {
    shared.begin_transport();
    let authentication = authentication.into_worker_authentication();
    let handle = match establish_authenticated_handle(
        &profile,
        &authentication,
        HostKeyPolicy::Pinned(&known_host_fingerprint),
        &shared,
        &command_receiver,
        &host_key_gate,
        &password_gate,
    )
    .await
    {
        AuthenticatedHandleAttempt::Established(handle) => handle,
        AuthenticatedHandleAttempt::Retryable(failure, message)
        | AuthenticatedHandleAttempt::Permanent(failure, message) => {
            return Err(SshRawExecError::Connection {
                kind: session_error_kind_for_failure(failure),
                message,
            });
        }
        AuthenticatedHandleAttempt::Shutdown => return Err(SshRawExecError::Cancelled),
    };

    let mut channel = match wait_for_ssh_operation(
        handle.channel_open_session(),
        &command_receiver,
        &shared,
        &host_key_gate,
    )
    .await
    {
        WorkerWait::Completed(Ok(channel)) => channel,
        WorkerWait::Completed(Err(_)) => {
            let _ = stop_handle(handle, &shared).await;
            return Err(SshRawExecError::Connection {
                kind: SessionErrorKind::Spawn,
                message: "SSH raw exec session channel could not open".to_owned(),
            });
        }
        WorkerWait::Shutdown => {
            let _ = stop_handle(handle, &shared).await;
            return Err(SshRawExecError::Cancelled);
        }
    };

    match wait_for_ssh_operation(
        async {
            match request {
                RawRequest::Exec(command) => channel.exec(true, command).await,
                RawRequest::Subsystem(name) => channel.request_subsystem(true, name).await,
            }
        },
        &command_receiver,
        &shared,
        &host_key_gate,
    )
    .await
    {
        WorkerWait::Completed(Ok(())) => {}
        WorkerWait::Completed(Err(_)) => {
            let _ = stop_handle(handle, &shared).await;
            return Err(SshRawExecError::Connection {
                kind: SessionErrorKind::Spawn,
                message: "SSH raw exec request failed".to_owned(),
            });
        }
        WorkerWait::Shutdown => {
            let _ = stop_handle(handle, &shared).await;
            return Err(SshRawExecError::Cancelled);
        }
    }

    if !wait_for_exec_acceptance(
        &mut channel,
        &stdout_sender,
        &stderr,
        &command_receiver,
        &shared,
        &host_key_gate,
        &cancelled,
    )
    .await?
    {
        let _ = stop_handle(handle, &shared).await;
        return Err(SshRawExecError::ExecRejected);
    }

    let outcome = run_exec_channel(
        &mut channel,
        &stdout_sender,
        &stdin_receiver,
        &stderr,
        &command_receiver,
        &shared,
        &host_key_gate,
        &cancelled,
    )
    .await;
    let _ = channel.eof().await;
    let _ = channel.close().await;
    let _ = stop_handle(handle, &shared).await;
    outcome
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_exec_acceptance(
    channel: &mut russh::Channel<russh::client::Msg>,
    stdout_sender: &SyncSender<Vec<u8>>,
    stderr: &Arc<Mutex<BoundedDiagnostic>>,
    command_receiver: &WorkerCommandReceiver,
    shared: &WorkerShared,
    host_key_gate: &crate::HostKeyDecisionGate,
    cancelled: &AtomicBool,
) -> Result<bool, SshRawExecError> {
    loop {
        tokio::select! {
            message = channel.wait() => match message {
                Some(russh::ChannelMsg::Success) => return Ok(true),
                Some(russh::ChannelMsg::Failure) => return Ok(false),
                Some(russh::ChannelMsg::Data { data }) => send_bounded(stdout_sender, data.to_vec(), cancelled).await?,
                Some(russh::ChannelMsg::ExtendedData { data, ext: 1 }) => stderr.lock().expect("SSH raw exec stderr lock is not poisoned").push(data.as_ref()),
                Some(russh::ChannelMsg::Eof | russh::ChannelMsg::Close) | None => return Err(SshRawExecError::ClosedBeforeExit),
                Some(_) => {}
            },
            _ = tokio::time::sleep(COMMAND_POLL_INTERVAL) => {
                if cancelled.load(Ordering::Acquire) || crate::process_commands_before_running(command_receiver, shared, host_key_gate) {
                    return Err(SshRawExecError::Cancelled);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_exec_channel(
    channel: &mut russh::Channel<russh::client::Msg>,
    stdout_sender: &SyncSender<Vec<u8>>,
    stdin_receiver: &Receiver<Vec<u8>>,
    stderr: &Arc<Mutex<BoundedDiagnostic>>,
    command_receiver: &WorkerCommandReceiver,
    shared: &WorkerShared,
    host_key_gate: &crate::HostKeyDecisionGate,
    cancelled: &AtomicBool,
) -> Result<SshRawExecExit, SshRawExecError> {
    let mut exit_status = None;
    loop {
        tokio::select! {
            message = channel.wait() => match message {
                Some(russh::ChannelMsg::Data { data }) => send_bounded(stdout_sender, data.to_vec(), cancelled).await?,
                Some(russh::ChannelMsg::ExtendedData { data, ext: 1 }) => stderr.lock().expect("SSH raw exec stderr lock is not poisoned").push(data.as_ref()),
                Some(russh::ChannelMsg::ExitStatus { exit_status: status }) => exit_status = Some(status),
                Some(russh::ChannelMsg::ExitSignal { .. }) => return Err(SshRawExecError::ExitSignal),
                Some(russh::ChannelMsg::Eof) => {}
                Some(russh::ChannelMsg::Close) | None => {
                    let Some(status) = exit_status else {
                        return Err(SshRawExecError::ClosedBeforeExit);
                    };
                    return if status == 0 {
                        Ok(SshRawExecExit { status })
                    } else {
                        Err(SshRawExecError::NonZeroExit { status })
                    };
                }
                Some(_) => {}
            },
            _ = tokio::time::sleep(COMMAND_POLL_INTERVAL) => {
                if cancelled.load(Ordering::Acquire) || crate::process_commands_before_running(command_receiver, shared, host_key_gate) {
                    return Err(SshRawExecError::Cancelled);
                }
                loop {
                    match stdin_receiver.try_recv() {
                        Ok(bytes) => match wait_for_ssh_operation(channel.data_bytes(bytes), command_receiver, shared, host_key_gate).await {
                            WorkerWait::Completed(Ok(())) => {}
                            WorkerWait::Completed(Err(_)) => return Err(SshRawExecError::Connection {
                                kind: SessionErrorKind::Input,
                                message: "SSH raw exec stdin write failed".to_owned(),
                            }),
                            WorkerWait::Shutdown => return Err(SshRawExecError::Cancelled),
                        },
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => break,
                    }
                }
            }
        }
    }
}

async fn send_bounded(
    sender: &SyncSender<Vec<u8>>,
    mut bytes: Vec<u8>,
    cancelled: &AtomicBool,
) -> Result<(), SshRawExecError> {
    while !bytes.is_empty() {
        match sender.try_send(bytes) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(returned)) => {
                bytes = returned;
                if cancelled.load(Ordering::Acquire) {
                    return Err(SshRawExecError::Cancelled);
                }
                tokio::time::sleep(COMMAND_POLL_INTERVAL).await;
            }
            Err(TrySendError::Disconnected(_)) => return Err(SshRawExecError::Cancelled),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_session::TerminalSize;
    use std::{
        io::{Read, Write},
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::{mpsc, Arc, Mutex},
    };

    #[derive(Clone)]
    struct ExecFixtureState {
        pty_count: Arc<Mutex<usize>>,
        commands: Arc<Mutex<Vec<String>>>,
        subsystems: Arc<Mutex<Vec<String>>>,
    }

    struct ExecHandler {
        state: ExecFixtureState,
        channel: Option<russh::Channel<russh::server::Msg>>,
        reject_exec: bool,
        exit_status: u32,
    }

    impl russh::server::Handler for ExecHandler {
        type Error = russh::Error;

        async fn auth_password(
            &mut self,
            user: &str,
            password: &str,
        ) -> Result<russh::server::Auth, Self::Error> {
            Ok(if user == "alice" && password == "test-password" {
                russh::server::Auth::Accept
            } else {
                russh::server::Auth::reject()
            })
        }

        async fn channel_open_session(
            &mut self,
            channel: russh::Channel<russh::server::Msg>,
            reply: russh::server::ChannelOpenHandle,
            _session: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            self.channel = Some(channel);
            reply.accept().await;
            Ok(())
        }

        async fn pty_request(
            &mut self,
            _channel: russh::ChannelId,
            _term: &str,
            _cols: u32,
            _rows: u32,
            _width: u32,
            _height: u32,
            _modes: &[(russh::Pty, u32)],
            _session: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            *self.state.pty_count.lock().unwrap() += 1;
            Ok(())
        }

        async fn subsystem_request(
            &mut self,
            id: russh::ChannelId,
            name: &str,
            session: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            self.state.subsystems.lock().unwrap().push(name.to_owned());
            if self.reject_exec {
                session.channel_failure(id)?;
                return Ok(());
            }
            session.channel_success(id)?;
            let mut channel = self.channel.take().unwrap();
            let exit_status = self.exit_status;
            tokio::spawn(async move {
                channel
                    .extended_data(1, &b"subsystem diagnostic"[..])
                    .await
                    .unwrap();
                channel.data(&b"ready:"[..]).await.unwrap();
                while let Some(message) = channel.wait().await {
                    match message {
                        russh::ChannelMsg::Data { data } => {
                            channel.data(&data[..]).await.unwrap();
                            if data.last() == Some(&b'\n') {
                                break;
                            }
                        }
                        russh::ChannelMsg::Eof | russh::ChannelMsg::Close => return,
                        _ => {}
                    }
                }
                channel.exit_status(exit_status).await.unwrap();
                channel.eof().await.unwrap();
                channel.close().await.unwrap();
            });
            Ok(())
        }

        async fn exec_request(
            &mut self,
            id: russh::ChannelId,
            command: &[u8],
            session: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            let command = String::from_utf8(command.to_vec()).unwrap();
            self.state.commands.lock().unwrap().push(command.clone());
            if self.reject_exec {
                session.channel_failure(id)?;
                return Ok(());
            }
            session.channel_success(id)?;
            let mut channel = self.channel.take().unwrap();
            let exit_status = self.exit_status;
            tokio::spawn(async move {
                if command.contains("silent") {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    let _ = channel.close().await;
                    return;
                }
                if command.contains("flood") {
                    let chunk = vec![b'x'; DEFAULT_EXEC_QUEUE_CHUNK_BYTES];
                    for _ in 0..32 {
                        channel.data(&chunk[..]).await.unwrap();
                    }
                    channel.exit_status(0).await.unwrap();
                    channel.eof().await.unwrap();
                    channel.close().await.unwrap();
                    return;
                }
                channel.extended_data(1, &b"diagnostic"[..]).await.unwrap();
                channel.data(&b"ready:"[..]).await.unwrap();
                let mut seen = Vec::new();
                loop {
                    match channel.wait().await {
                        Some(russh::ChannelMsg::Data { data }) => {
                            seen.extend_from_slice(&data);
                            if seen.ends_with(b"\n") {
                                break;
                            }
                        }
                        Some(russh::ChannelMsg::Eof | russh::ChannelMsg::Close) | None => break,
                        _ => {}
                    }
                }
                channel.data(&seen[..]).await.unwrap();
                channel.exit_status(exit_status).await.unwrap();
                channel.eof().await.unwrap();
                channel.close().await.unwrap();
            });
            Ok(())
        }
    }

    struct ExecServer {
        stop: Option<tokio::sync::oneshot::Sender<()>>,
        join: Option<thread::JoinHandle<()>>,
        pub state: ExecFixtureState,
        pub fingerprint: String,
    }

    impl Drop for ExecServer {
        fn drop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(join) = self.join.take() {
                join.join().unwrap();
            }
        }
    }

    fn start_server(reject_exec: bool, exit_status: u32) -> (ExecServer, u16) {
        let (port_sender, port_receiver) = mpsc::channel();
        let (fingerprint_sender, fingerprint_receiver) = mpsc::channel();
        let (stop, mut stop_receiver) = tokio::sync::oneshot::channel();
        let state = ExecFixtureState {
            pty_count: Arc::new(Mutex::new(0)),
            commands: Arc::new(Mutex::new(Vec::new())),
            subsystems: Arc::new(Mutex::new(Vec::new())),
        };
        let server_state = state.clone();
        let join = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await.unwrap();
                port_sender.send(listener.local_addr().unwrap().port()).unwrap();
                let key = russh::keys::PrivateKey::random(&mut russh::keys::key::safe_rng(), russh::keys::Algorithm::Ed25519).unwrap();
                fingerprint_sender.send(key.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string()).unwrap();
                let config = Arc::new(russh::server::Config { keys: vec![key], auth_rejection_time: Duration::ZERO, ..Default::default() });
                let mut sessions = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        _ = &mut stop_receiver => break,
                        accepted = listener.accept() => {
                            let (stream, _) = accepted.unwrap();
                            let config = Arc::clone(&config);
                            let state = server_state.clone();
                            sessions.spawn(async move {
                                let session = russh::server::run_stream(config, stream, ExecHandler { state, channel: None, reject_exec, exit_status }).await.unwrap();
                                let _ = session.await;
                            });
                        }
                    }
                }
                sessions.abort_all();
            });
        });
        let port = port_receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        let fingerprint = fingerprint_receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert!(is_sha256_fingerprint(&fingerprint));
        (
            ExecServer {
                stop: Some(stop),
                join: Some(join),
                state,
                fingerprint,
            },
            port,
        )
    }

    fn profile(port: u16) -> SshConnectionProfile {
        SshConnectionProfile::new(
            crate::HostIdentity::new("127.0.0.1", port).unwrap(),
            "alice",
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            TerminalSize::new(80, 24).unwrap(),
        )
        .unwrap()
        .with_local_bind_address(Some(IpAddr::V4(Ipv4Addr::LOCALHOST)))
        .unwrap()
    }

    fn pinned_options(server: &ExecServer) -> SshRawExecOptions {
        SshRawExecOptions::new()
            .with_known_host_fingerprint(server.fingerprint.clone())
            .unwrap()
            .with_io_timeouts(Duration::from_secs(5), Duration::from_secs(5))
            .unwrap()
    }

    #[test]
    fn raw_exec_uses_no_pty_and_preserves_stdout_stderr_and_bytes() {
        let (server, port) = start_server(false, 0);
        let mut stream = SshRawExecSession::connect(
            profile(port),
            SshAuthentication::password("test-password"),
            "festerm-sessiond bridge",
            pinned_options(&server),
        )
        .unwrap();
        assert_eq!(stream.command(), "festerm-sessiond bridge");
        let mut ready = [0; 6];
        stream.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"ready:");
        stream.write_all(b"abc\0def\n").unwrap();
        let mut echoed = [0; 8];
        stream.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"abc\0def\n");
        stream.read_timeout = Duration::from_nanos(1);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match stream.read(&mut [0; 1]) {
                Ok(0) => break,
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                    assert!(Instant::now() < deadline, "exec completion deadline");
                    thread::sleep(Duration::from_millis(1));
                }
                other => panic!("stdout completion must follow result publication: {other:?}"),
            }
        }
        assert_eq!(
            stream.wait_finish(Duration::from_secs(5)).unwrap().status(),
            0
        );
        assert_eq!(stream.stderr_diagnostic(), b"diagnostic");
        assert_eq!(*server.state.pty_count.lock().unwrap(), 0);
        assert_eq!(
            server.state.commands.lock().unwrap()[0],
            "festerm-sessiond bridge"
        );
    }

    #[test]
    fn raw_subsystem_uses_native_request_without_exec_or_pty() {
        let (server, port) = start_server(false, 0);
        let mut stream = SshRawExecSession::connect_subsystem(
            profile(port),
            SshAuthentication::password("test-password"),
            "powershell",
            pinned_options(&server),
        )
        .unwrap();
        let mut ready = [0; 6];
        stream.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"ready:");
        stream.write_all(b"abc\0def\n").unwrap();
        let mut echoed = [0; 8];
        stream.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"abc\0def\n");
        assert_eq!(
            stream.wait_finish(Duration::from_secs(5)).unwrap().status(),
            0
        );
        assert_eq!(stream.stderr_diagnostic(), b"subsystem diagnostic");
        assert_eq!(*server.state.pty_count.lock().unwrap(), 0);
        assert!(server.state.commands.lock().unwrap().is_empty());
        assert_eq!(*server.state.subsystems.lock().unwrap(), ["powershell"]);
    }

    #[test]
    fn raw_subsystem_rejects_invalid_names_and_server_refusal() {
        for name in ["", "powershell;sh", "pwsh\n", "pwsh\0", "pwsh target"] {
            assert!(SshRawExecSession::connect_subsystem(
                profile(22),
                SshAuthentication::password("test-password"),
                name,
                SshRawExecOptions::new(),
            )
            .is_err());
        }
        assert!(SshRawExecSession::connect_subsystem(
            profile(22),
            SshAuthentication::password("test-password"),
            "x".repeat(MAX_SUBSYSTEM_NAME_BYTES + 1),
            SshRawExecOptions::new(),
        )
        .is_err());
        let (server, port) = start_server(true, 0);
        let mut stream = SshRawExecSession::connect_subsystem(
            profile(port),
            SshAuthentication::password("test-password"),
            "powershell",
            pinned_options(&server),
        )
        .unwrap();
        assert_eq!(
            stream.wait_finish(Duration::from_secs(5)),
            Err(SshRawExecError::ExecRejected)
        );
        assert!(server.state.commands.lock().unwrap().is_empty());
    }

    #[test]
    fn raw_exec_rejects_changed_pinned_host_key_without_unbound_fallback() {
        let (_server, port) = start_server(false, 0);
        let mut stream = SshRawExecSession::connect(
            profile(port),
            SshAuthentication::password("test-password"),
            "cmd",
            SshRawExecOptions::new()
                .with_known_host_fingerprint("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
                .unwrap(),
        )
        .unwrap();
        let error = stream.wait_finish(Duration::from_secs(5)).unwrap_err();
        assert!(matches!(error, SshRawExecError::Connection { .. }));
    }

    #[test]
    fn raw_exec_requires_a_pinned_host_key_and_bounded_command() {
        for fingerprint in [
            "SHA256:fixture",
            "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA_",
        ] {
            assert_eq!(
                SshRawExecOptions::new().with_known_host_fingerprint(fingerprint),
                Err(SshRawExecOptionsError::InvalidHostKeyFingerprint)
            );
        }
        let profile = profile(22);
        let error = match SshRawExecSession::connect(
            profile.clone(),
            SshAuthentication::password("test-password"),
            "cmd",
            SshRawExecOptions::new(),
        ) {
            Ok(_) => panic!("missing pinned host key must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("previously verified"));

        let long_command = "x".repeat(MAX_RAW_EXEC_COMMAND_BYTES + 1);
        let error = match SshRawExecSession::connect(
            profile,
            SshAuthentication::password("test-password"),
            long_command,
            SshRawExecOptions::new()
                .with_known_host_fingerprint("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
                .unwrap(),
        ) {
            Ok(_) => panic!("overlong command must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("maximum"));
    }

    #[test]
    fn raw_exec_reports_rejection_and_nonzero_exit() {
        let (server, port) = start_server(true, 0);
        let mut rejected = SshRawExecSession::connect(
            profile(port),
            SshAuthentication::password("test-password"),
            "cmd",
            pinned_options(&server),
        )
        .unwrap();
        assert_eq!(
            rejected.wait_finish(Duration::from_secs(5)),
            Err(SshRawExecError::ExecRejected)
        );

        let (server, port) = start_server(false, 7);
        let mut nonzero = SshRawExecSession::connect(
            profile(port),
            SshAuthentication::password("test-password"),
            "cmd",
            pinned_options(&server),
        )
        .unwrap();
        let mut ready = [0; 6];
        nonzero.read_exact(&mut ready).unwrap();
        nonzero.write_all(b"\n").unwrap();
        let mut echoed = [0; 1];
        nonzero.read_exact(&mut echoed).unwrap();
        assert_eq!(
            nonzero.wait_finish(Duration::from_secs(5)),
            Err(SshRawExecError::NonZeroExit { status: 7 })
        );
    }

    #[test]
    fn raw_exec_pending_read_times_out_and_cancel_while_queue_full() {
        let (server, port) = start_server(false, 0);
        let mut stream = SshRawExecSession::connect(
            profile(port),
            SshAuthentication::password("test-password"),
            "silent",
            pinned_options(&server)
                .with_queue_capacities(1, 1)
                .unwrap()
                .with_io_timeouts(Duration::from_millis(10), Duration::from_millis(10))
                .unwrap(),
        )
        .unwrap();
        let mut pending = [0; 1];
        assert_eq!(
            stream.read(&mut pending).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        stream.cancel();
        assert_eq!(
            stream.wait_finish(Duration::from_secs(5)),
            Err(SshRawExecError::Cancelled)
        );

        let (server, port) = start_server(false, 0);
        let mut stream = SshRawExecSession::connect(
            profile(port),
            SshAuthentication::password("test-password"),
            "flood",
            pinned_options(&server)
                .with_queue_capacities(1, 1)
                .unwrap()
                .with_io_timeouts(Duration::from_millis(10), Duration::from_millis(10))
                .unwrap(),
        )
        .unwrap();
        thread::sleep(Duration::from_millis(50));
        stream.cancel();
        assert_eq!(
            stream.wait_finish(Duration::from_secs(5)),
            Err(SshRawExecError::Cancelled)
        );
    }
}
