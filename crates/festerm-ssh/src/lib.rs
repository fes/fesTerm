//! Native SSH transport policy and bounded `russh` session lifecycle.

use std::{
    fmt,
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
        Arc, Condvar, Mutex,
    },
    thread,
    time::Duration,
};

use festerm_session::{
    noop_session_event_notifier, FlowDirection, Session, SessionError, SessionErrorKind,
    SessionEvent, SessionEventNotifier, SessionId, SessionLifecycle, SessionMetrics,
    SessionOperation, SessionSendError, SessionTryReceiveError, ShutdownError, ShutdownResult,
    TerminalSize, DEFAULT_COMMAND_QUEUE_CAPACITY, DEFAULT_EVENT_QUEUE_CAPACITY, MAX_IO_CHUNK_BYTES,
};

/// Canonical SSH destination identity used for trust decisions.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct HostIdentity {
    host: String,
    port: u16,
}

impl HostIdentity {
    pub fn new(host: impl Into<String>, port: u16) -> Result<Self, HostIdentityError> {
        let host = host.into();
        let host = host.trim();
        if host.is_empty() {
            return Err(HostIdentityError::EmptyHost);
        }
        if host.chars().any(char::is_whitespace) {
            return Err(HostIdentityError::Whitespace);
        }
        if port == 0 {
            return Err(HostIdentityError::ZeroPort);
        }
        Ok(Self {
            host: host.to_ascii_lowercase(),
            port,
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub const fn port(&self) -> u16 {
        self.port
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostIdentityError {
    EmptyHost,
    Whitespace,
    ZeroPort,
}

impl fmt::Display for HostIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyHost => formatter.write_str("SSH host must not be empty"),
            Self::Whitespace => formatter.write_str("SSH host must not contain whitespace"),
            Self::ZeroPort => formatter.write_str("SSH port must not be zero"),
        }
    }
}

impl std::error::Error for HostIdentityError {}

/// Application response to a host-key verification prompt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostTrustDecision {
    Reject,
    AcceptOnce,
    AcceptAndPersist,
}

/// Transient authentication selected for one new SSH session.
///
/// Password authentication is the only supported method in this slice.
/// Public-key, agent, and keyboard-interactive authentication are deliberately
/// not represented until their secret-handling lifetimes can be bounded
/// without retaining key material or challenge responses in configuration.
pub enum SshAuthentication {
    Password(SshPassword),
}

impl SshAuthentication {
    /// Selects password authentication for this session.
    ///
    /// The password is moved directly to the worker, used for one
    /// authentication attempt, and is never exposed through this API, cloned,
    /// persisted, or included in debug output.
    pub fn password(password: impl Into<String>) -> Self {
        Self::Password(SshPassword {
            password: password.into(),
        })
    }

    fn into_password(self) -> String {
        match self {
            Self::Password(password) => password.password,
        }
    }
}

impl fmt::Debug for SshAuthentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password(_) => formatter.write_str("SshAuthentication::Password([REDACTED])"),
        }
    }
}

/// Password material consumed by [`SshAuthentication`] for a single attempt.
///
/// This type intentionally has no getter, `Clone`, or derived `Debug`
/// implementation. It is not a persistent connection-profile field.
pub struct SshPassword {
    password: String,
}

impl fmt::Debug for SshPassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SshPassword([REDACTED])")
    }
}

/// Validated, secret-free connection inputs for a future interactive SSH PTY.
///
/// Authentication material is intentionally absent. In particular, callers
/// must not put passwords, private-key data, or agent responses in this type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshConnectionProfile {
    identity: HostIdentity,
    username: String,
    terminal_type: String,
    initial_size: TerminalSize,
}

impl SshConnectionProfile {
    pub const DEFAULT_TERMINAL_TYPE: &'static str = "xterm-256color";
    const MAX_USERNAME_BYTES: usize = 255;
    const MAX_TERMINAL_TYPE_BYTES: usize = 64;

    pub fn new(
        identity: HostIdentity,
        username: impl Into<String>,
        terminal_type: impl Into<String>,
        initial_size: TerminalSize,
    ) -> Result<Self, SshConnectionProfileError> {
        let username = username.into();
        validate_username(&username)?;
        let terminal_type = terminal_type.into();
        validate_terminal_type(&terminal_type)?;
        Ok(Self {
            identity,
            username,
            terminal_type,
            initial_size,
        })
    }

    pub fn identity(&self) -> &HostIdentity {
        &self.identity
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn terminal_type(&self) -> &str {
        &self.terminal_type
    }

    pub const fn initial_size(&self) -> TerminalSize {
        self.initial_size
    }
}

fn validate_username(username: &str) -> Result<(), SshConnectionProfileError> {
    if username.is_empty() {
        return Err(SshConnectionProfileError::EmptyUsername);
    }
    if username.len() > SshConnectionProfile::MAX_USERNAME_BYTES {
        return Err(SshConnectionProfileError::UsernameTooLong {
            maximum: SshConnectionProfile::MAX_USERNAME_BYTES,
            actual: username.len(),
        });
    }
    if username.chars().any(char::is_whitespace) {
        return Err(SshConnectionProfileError::UsernameWhitespace);
    }
    if username.chars().any(char::is_control) {
        return Err(SshConnectionProfileError::UsernameControlCharacter);
    }
    Ok(())
}

fn validate_terminal_type(terminal_type: &str) -> Result<(), SshConnectionProfileError> {
    if terminal_type.is_empty() {
        return Err(SshConnectionProfileError::EmptyTerminalType);
    }
    if terminal_type.len() > SshConnectionProfile::MAX_TERMINAL_TYPE_BYTES {
        return Err(SshConnectionProfileError::TerminalTypeTooLong {
            maximum: SshConnectionProfile::MAX_TERMINAL_TYPE_BYTES,
            actual: terminal_type.len(),
        });
    }
    if !terminal_type
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'))
    {
        return Err(SshConnectionProfileError::InvalidTerminalType);
    }
    Ok(())
}

/// Failure to validate non-secret SSH connection inputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshConnectionProfileError {
    EmptyUsername,
    UsernameWhitespace,
    UsernameControlCharacter,
    UsernameTooLong { maximum: usize, actual: usize },
    EmptyTerminalType,
    InvalidTerminalType,
    TerminalTypeTooLong { maximum: usize, actual: usize },
}

impl fmt::Display for SshConnectionProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyUsername => formatter.write_str("SSH username must not be empty"),
            Self::UsernameWhitespace => {
                formatter.write_str("SSH username must not contain whitespace")
            }
            Self::UsernameControlCharacter => {
                formatter.write_str("SSH username must not contain control characters")
            }
            Self::UsernameTooLong { maximum, actual } => {
                write!(
                    formatter,
                    "SSH username is {actual} bytes; maximum is {maximum}"
                )
            }
            Self::EmptyTerminalType => formatter.write_str("SSH terminal type must not be empty"),
            Self::InvalidTerminalType => {
                formatter.write_str("SSH terminal type contains unsupported characters")
            }
            Self::TerminalTypeTooLong { maximum, actual } => {
                write!(
                    formatter,
                    "SSH terminal type is {actual} bytes; maximum is {maximum}"
                )
            }
        }
    }
}

impl std::error::Error for SshConnectionProfileError {}

/// Bounded automatic reconnect behavior owned by the application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconnectPolicy {
    maximum_attempts: u8,
    initial_delay: Duration,
    maximum_delay: Duration,
}

impl ReconnectPolicy {
    pub fn new(
        maximum_attempts: u8,
        initial_delay: Duration,
        maximum_delay: Duration,
    ) -> Result<Self, ReconnectPolicyError> {
        if maximum_attempts == 0 {
            return Err(ReconnectPolicyError::ZeroAttempts);
        }
        if initial_delay.is_zero() {
            return Err(ReconnectPolicyError::ZeroInitialDelay);
        }
        if maximum_delay < initial_delay {
            return Err(ReconnectPolicyError::MaximumBeforeInitial);
        }
        Ok(Self {
            maximum_attempts,
            initial_delay,
            maximum_delay,
        })
    }

    pub const fn maximum_attempts(self) -> u8 {
        self.maximum_attempts
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconnectPolicyError {
    ZeroAttempts,
    ZeroInitialDelay,
    MaximumBeforeInitial,
}

impl fmt::Display for ReconnectPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroAttempts => formatter.write_str("reconnect requires at least one attempt"),
            Self::ZeroInitialDelay => formatter.write_str("reconnect delay must be nonzero"),
            Self::MaximumBeforeInitial => {
                formatter.write_str("maximum reconnect delay must not precede initial delay")
            }
        }
    }
}

impl std::error::Error for ReconnectPolicyError {}

/// Resolves the single pending host-key request for one future SSH session.
///
/// This handle contains no host-key material. The GUI can call it from its
/// event handler; the future network worker, not the GUI thread, awaits it.
#[derive(Clone)]
pub struct HostKeyDecisionResolver {
    gate: Arc<HostKeyDecisionGate>,
}

impl HostKeyDecisionResolver {
    pub fn resolve(
        &self,
        prompt: &festerm_session::HostKeyPrompt,
        decision: HostTrustDecision,
    ) -> Result<(), HostKeyDecisionResolutionError> {
        self.gate.resolve(prompt, decision)
    }

    /// Cancels the current prompt. Cancellation always rejects the host key.
    pub fn cancel(
        &self,
        prompt: &festerm_session::HostKeyPrompt,
    ) -> Result<(), HostKeyDecisionResolutionError> {
        self.gate.cancel(prompt)
    }
}

impl fmt::Debug for HostKeyDecisionResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HostKeyDecisionResolver")
    }
}

/// A rejected or stale attempt to resolve host-key trust.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostKeyDecisionResolutionError {
    NoPendingPrompt,
    AlreadyResolved,
    PromptMismatch,
}

impl fmt::Display for HostKeyDecisionResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPendingPrompt => formatter.write_str("no host-key prompt is pending"),
            Self::AlreadyResolved => formatter.write_str("host-key prompt is already resolved"),
            Self::PromptMismatch => {
                formatter.write_str("host-key decision does not match the pending prompt")
            }
        }
    }
}

impl std::error::Error for HostKeyDecisionResolutionError {}

#[allow(dead_code)]
enum HostKeyGateState {
    Idle,
    Waiting(festerm_session::HostKeyPrompt),
    Resolved(HostTrustDecision),
    Cancelled,
}

struct HostKeyDecisionGate {
    state: Mutex<HostKeyGateState>,
    changed: Condvar,
    notified: tokio::sync::Notify,
}

#[allow(dead_code)]
impl HostKeyDecisionGate {
    fn new() -> Self {
        Self {
            state: Mutex::new(HostKeyGateState::Idle),
            changed: Condvar::new(),
            notified: tokio::sync::Notify::new(),
        }
    }

    fn begin(
        &self,
        prompt: festerm_session::HostKeyPrompt,
    ) -> Result<HostKeyDecisionWaiter, HostKeyDecisionResolutionError> {
        let mut state = self
            .state
            .lock()
            .expect("host-key gate lock is not poisoned");
        match *state {
            HostKeyGateState::Idle => {
                *state = HostKeyGateState::Waiting(prompt.clone());
                Ok(HostKeyDecisionWaiter { prompt })
            }
            HostKeyGateState::Resolved(_) => Err(HostKeyDecisionResolutionError::AlreadyResolved),
            HostKeyGateState::Waiting(_) | HostKeyGateState::Cancelled => {
                Err(HostKeyDecisionResolutionError::NoPendingPrompt)
            }
        }
    }

    fn resolve(
        &self,
        prompt: &festerm_session::HostKeyPrompt,
        decision: HostTrustDecision,
    ) -> Result<(), HostKeyDecisionResolutionError> {
        let mut state = self
            .state
            .lock()
            .expect("host-key gate lock is not poisoned");
        match &*state {
            HostKeyGateState::Waiting(current) if current == prompt => {
                *state = HostKeyGateState::Resolved(decision);
                self.changed.notify_all();
                self.notified.notify_waiters();
                Ok(())
            }
            HostKeyGateState::Waiting(_) => Err(HostKeyDecisionResolutionError::PromptMismatch),
            HostKeyGateState::Resolved(_) => Err(HostKeyDecisionResolutionError::AlreadyResolved),
            HostKeyGateState::Idle | HostKeyGateState::Cancelled => {
                Err(HostKeyDecisionResolutionError::NoPendingPrompt)
            }
        }
    }

    fn cancel(
        &self,
        prompt: &festerm_session::HostKeyPrompt,
    ) -> Result<(), HostKeyDecisionResolutionError> {
        let mut state = self
            .state
            .lock()
            .expect("host-key gate lock is not poisoned");
        match &*state {
            HostKeyGateState::Waiting(current) if current == prompt => {
                *state = HostKeyGateState::Cancelled;
                self.changed.notify_all();
                self.notified.notify_waiters();
                Ok(())
            }
            HostKeyGateState::Waiting(_) => Err(HostKeyDecisionResolutionError::PromptMismatch),
            HostKeyGateState::Resolved(_) => Err(HostKeyDecisionResolutionError::AlreadyResolved),
            HostKeyGateState::Idle | HostKeyGateState::Cancelled => {
                Err(HostKeyDecisionResolutionError::NoPendingPrompt)
            }
        }
    }

    fn wait_for_decision(&self, timeout: Duration) -> HostTrustDecision {
        let state = self
            .state
            .lock()
            .expect("host-key gate lock is not poisoned");
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| {
                matches!(state, HostKeyGateState::Waiting(_))
            })
            .expect("host-key gate lock is not poisoned");
        let decision = match *state {
            HostKeyGateState::Resolved(decision) => decision,
            HostKeyGateState::Idle | HostKeyGateState::Waiting(_) | HostKeyGateState::Cancelled => {
                HostTrustDecision::Reject
            }
        };
        *state = HostKeyGateState::Idle;
        self.notified.notify_waiters();
        decision
    }

    fn reject_pending(&self) {
        let mut state = self
            .state
            .lock()
            .expect("host-key gate lock is not poisoned");
        if matches!(*state, HostKeyGateState::Waiting(_)) {
            *state = HostKeyGateState::Cancelled;
            self.changed.notify_all();
            self.notified.notify_waiters();
        }
    }

    async fn wait_for_decision_async(&self, timeout: Duration) -> HostTrustDecision {
        let decision = tokio::time::timeout(timeout, async {
            loop {
                let notified = self.notified.notified();
                {
                    let state = self
                        .state
                        .lock()
                        .expect("host-key gate lock is not poisoned");
                    match *state {
                        HostKeyGateState::Resolved(decision) => return decision,
                        HostKeyGateState::Idle | HostKeyGateState::Cancelled => {
                            return HostTrustDecision::Reject;
                        }
                        HostKeyGateState::Waiting(_) => {}
                    }
                }
                notified.await;
            }
        })
        .await
        .unwrap_or(HostTrustDecision::Reject);
        *self
            .state
            .lock()
            .expect("host-key gate lock is not poisoned") = HostKeyGateState::Idle;
        self.changed.notify_all();
        self.notified.notify_waiters();
        decision
    }
}

/// Worker-only proof that a prompt has been emitted and may now be awaited.
#[allow(dead_code)]
struct HostKeyDecisionWaiter {
    prompt: festerm_session::HostKeyPrompt,
}

#[allow(dead_code)]
impl HostKeyDecisionWaiter {
    fn wait(self, gate: &HostKeyDecisionGate, timeout: Duration) -> HostTrustDecision {
        gate.wait_for_decision(timeout)
    }

    async fn wait_async(self, gate: &HostKeyDecisionGate, timeout: Duration) -> HostTrustDecision {
        gate.wait_for_decision_async(timeout).await
    }
}

enum WorkerCommand {
    Input(Vec<u8>),
    Resize(TerminalSize),
    Shutdown,
}

/// Private receiver handed to the dedicated SSH worker.
struct WorkerCommandReceiver {
    receiver: Receiver<WorkerCommand>,
}

impl WorkerCommandReceiver {
    fn try_recv(&self) -> Result<WorkerCommand, TryRecvError> {
        self.receiver.try_recv()
    }
}

struct WorkerShared {
    id: SessionId,
    lifecycle: Mutex<SessionLifecycle>,
    metrics: Mutex<SessionMetrics>,
    event_sender: SyncSender<SessionEvent>,
    event_notifier: Arc<dyn SessionEventNotifier>,
}

impl WorkerShared {
    fn lifecycle(&self) -> SessionLifecycle {
        self.lifecycle
            .lock()
            .expect("SSH lifecycle lock is not poisoned")
            .clone()
    }

    fn set_lifecycle(&self, lifecycle: SessionLifecycle) {
        *self
            .lifecycle
            .lock()
            .expect("SSH lifecycle lock is not poisoned") = lifecycle.clone();
        let _ = self.try_emit(SessionEvent::Lifecycle(lifecycle));
    }

    fn metrics(&self) -> SessionMetrics {
        *self
            .metrics
            .lock()
            .expect("SSH metrics lock is not poisoned")
    }

    fn try_emit(&self, event: SessionEvent) -> bool {
        let output_bytes = match &event {
            SessionEvent::Output(bytes) if bytes.len() <= MAX_IO_CHUNK_BYTES => bytes.len(),
            SessionEvent::Output(_) => return false,
            _ => 0,
        };
        let is_error = matches!(&event, SessionEvent::Error(_));
        let mut metrics = self
            .metrics
            .lock()
            .expect("SSH metrics lock is not poisoned");
        match self.event_sender.try_send(event) {
            Ok(()) => {
                metrics.output_bytes = metrics.output_bytes.saturating_add(output_bytes as u64);
                if is_error {
                    metrics.error_count = metrics.error_count.saturating_add(1);
                }
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

    fn record_input_sent(&self, bytes: usize) {
        let mut metrics = self
            .metrics
            .lock()
            .expect("SSH metrics lock is not poisoned");
        metrics.input_bytes = metrics.input_bytes.saturating_add(bytes as u64);
    }

    fn record_resize_applied(&self, size: TerminalSize) {
        let mut metrics = self
            .metrics
            .lock()
            .expect("SSH metrics lock is not poisoned");
        metrics.resize_count = metrics.resize_count.saturating_add(1);
        drop(metrics);
        let _ = self.try_emit(SessionEvent::ResizeApplied(size));
    }

    #[cfg(test)]
    fn record_output(&self, bytes: Vec<u8>) -> bool {
        self.try_emit(SessionEvent::Output(bytes))
    }

    fn report_backpressure(&self, direction: FlowDirection, queued: usize, capacity: usize) {
        {
            let mut metrics = self
                .metrics
                .lock()
                .expect("SSH metrics lock is not poisoned");
            metrics.backpressure_count = metrics.backpressure_count.saturating_add(1);
        }
        let _ = self.try_emit(SessionEvent::Backpressure {
            direction,
            queued,
            capacity,
        });
    }
}

/// Bounded coordination owned by one SSH worker.
struct SshWorkerFoundation {
    #[cfg(test)]
    profile: SshConnectionProfile,
    shared: Arc<WorkerShared>,
    command_sender: SyncSender<WorkerCommand>,
    command_capacity: usize,
    event_receiver: Mutex<Receiver<SessionEvent>>,
    host_key_gate: Arc<HostKeyDecisionGate>,
}

impl SshWorkerFoundation {
    #[cfg(test)]
    fn new(
        profile: SshConnectionProfile,
    ) -> (Self, WorkerCommandReceiver, HostKeyDecisionResolver) {
        Self::new_with_capacities(
            profile,
            DEFAULT_COMMAND_QUEUE_CAPACITY,
            DEFAULT_EVENT_QUEUE_CAPACITY,
            noop_session_event_notifier(),
        )
    }

    fn new_with_capacities(
        profile: SshConnectionProfile,
        command_capacity: usize,
        event_capacity: usize,
        event_notifier: Arc<dyn SessionEventNotifier>,
    ) -> (Self, WorkerCommandReceiver, HostKeyDecisionResolver) {
        #[cfg(not(test))]
        let _ = profile;
        assert!(
            command_capacity > 0,
            "SSH command queue capacity must be nonzero"
        );
        assert!(
            event_capacity > 0,
            "SSH event queue capacity must be nonzero"
        );
        let (command_sender, command_receiver) = mpsc::sync_channel(command_capacity);
        let (event_sender, event_receiver) = mpsc::sync_channel(event_capacity);
        let host_key_gate = Arc::new(HostKeyDecisionGate::new());
        let shared = Arc::new(WorkerShared {
            id: SessionId::next(),
            lifecycle: Mutex::new(SessionLifecycle::Starting),
            metrics: Mutex::new(SessionMetrics {
                event_queue_capacity: event_capacity,
                ..SessionMetrics::default()
            }),
            event_sender,
            event_notifier,
        });
        shared.set_lifecycle(SessionLifecycle::Starting);
        (
            Self {
                #[cfg(test)]
                profile,
                shared,
                command_sender,
                command_capacity,
                event_receiver: Mutex::new(event_receiver),
                host_key_gate: Arc::clone(&host_key_gate),
            },
            WorkerCommandReceiver {
                receiver: command_receiver,
            },
            HostKeyDecisionResolver {
                gate: host_key_gate,
            },
        )
    }

    fn id(&self) -> SessionId {
        self.shared.id
    }

    fn lifecycle(&self) -> SessionLifecycle {
        self.shared.lifecycle()
    }

    fn metrics(&self) -> SessionMetrics {
        self.shared.metrics()
    }

    #[cfg(test)]
    fn set_running(&self) {
        self.shared.set_lifecycle(SessionLifecycle::Running);
    }

    fn try_send_input(&self, bytes: &[u8]) -> Result<(), SessionSendError> {
        if bytes.len() > MAX_IO_CHUNK_BYTES {
            return Err(SessionSendError::TooLarge {
                operation: SessionOperation::Input,
                maximum: MAX_IO_CHUNK_BYTES,
                actual: bytes.len(),
            });
        }
        self.try_send_command(
            WorkerCommand::Input(bytes.to_vec()),
            SessionOperation::Input,
        )
    }

    fn try_resize(&self, size: TerminalSize) -> Result<(), SessionSendError> {
        self.try_send_command(WorkerCommand::Resize(size), SessionOperation::Resize)
    }

    fn try_shutdown(&self) -> Result<(), SessionSendError> {
        self.try_send_command(WorkerCommand::Shutdown, SessionOperation::Shutdown)
    }

    fn try_send_command(
        &self,
        command: WorkerCommand,
        operation: SessionOperation,
    ) -> Result<(), SessionSendError> {
        match self.command_sender.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.shared.report_backpressure(
                    match operation {
                        SessionOperation::Input => FlowDirection::Input,
                        SessionOperation::Resize => FlowDirection::Resize,
                        SessionOperation::Shutdown => FlowDirection::Control,
                    },
                    self.command_capacity,
                    self.command_capacity,
                );
                Err(SessionSendError::Full {
                    operation,
                    capacity: self.command_capacity,
                })
            }
            Err(TrySendError::Disconnected(_)) => Err(SessionSendError::Closed { operation }),
        }
    }

    fn try_recv_event(&self) -> Result<SessionEvent, SessionTryReceiveError> {
        match self
            .event_receiver
            .lock()
            .expect("SSH event receiver lock is not poisoned")
            .try_recv()
        {
            Ok(event) => {
                let mut metrics = self
                    .shared
                    .metrics
                    .lock()
                    .expect("SSH metrics lock is not poisoned");
                metrics.event_queue_depth = metrics.event_queue_depth.saturating_sub(1);
                Ok(event)
            }
            Err(TryRecvError::Empty) => Err(SessionTryReceiveError::Empty),
            Err(TryRecvError::Disconnected) => Err(SessionTryReceiveError::Closed),
        }
    }

    #[cfg(test)]
    fn request_host_key_verification(
        &self,
        sha256_fingerprint: &str,
    ) -> Result<HostKeyDecisionWaiter, HostKeyVerificationRequestError> {
        request_host_key_verification(
            &self.profile.identity,
            &self.shared,
            &self.host_key_gate,
            sha256_fingerprint,
        )
    }
}

fn request_host_key_verification(
    identity: &HostIdentity,
    shared: &WorkerShared,
    host_key_gate: &HostKeyDecisionGate,
    sha256_fingerprint: &str,
) -> Result<HostKeyDecisionWaiter, HostKeyVerificationRequestError> {
    if !is_sha256_fingerprint(sha256_fingerprint) {
        return Err(HostKeyVerificationRequestError::InvalidFingerprint);
    }
    let prompt =
        festerm_session::HostKeyPrompt::new(identity.host(), identity.port(), sha256_fingerprint);
    let waiter = host_key_gate
        .begin(prompt.clone())
        .map_err(HostKeyVerificationRequestError::Resolution)?;
    if shared.try_emit(SessionEvent::HostKeyVerification(prompt.clone())) {
        Ok(waiter)
    } else {
        let _ = host_key_gate.cancel(&prompt);
        // No waiter escaped on this path, so reset the rejection before a
        // future reconnect retries host verification.
        let _ = host_key_gate.wait_for_decision(Duration::ZERO);
        Err(HostKeyVerificationRequestError::EventQueueFull)
    }
}

#[allow(dead_code)]
fn is_sha256_fingerprint(fingerprint: &str) -> bool {
    let Some(encoded) = fingerprint.strip_prefix("SHA256:") else {
        return false;
    };
    !encoded.is_empty()
        && encoded.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'-' | b'_' | b'=')
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
enum HostKeyVerificationRequestError {
    InvalidFingerprint,
    EventQueueFull,
    Resolution(HostKeyDecisionResolutionError),
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HOST_KEY_DECISION_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(10);
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// Failure to create the dedicated SSH worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SshSessionStartError;

impl fmt::Display for SshSessionStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("could not start SSH worker thread")
    }
}

impl std::error::Error for SshSessionStartError {}

/// A live SSH transport session with bounded application-facing queues.
///
/// Each instance owns exactly one dedicated OS thread and one current-thread
/// Tokio runtime. The runtime is created once at session start, never per
/// command, and is stopped when the worker completes. This is an interim
/// crate-local boundary until application runtime ownership is available.
///
/// The worker performs TCP connection, strict host-key verification, password
/// authentication, and interactive session-channel setup.
pub struct SshSession {
    foundation: SshWorkerFoundation,
    host_key_resolver: HostKeyDecisionResolver,
    host_key_gate: Arc<HostKeyDecisionGate>,
    completion_receiver: Mutex<Receiver<Result<ShutdownResult, SessionError>>>,
    completion: Mutex<Option<Result<ShutdownResult, SessionError>>>,
}

impl SshSession {
    /// Starts a session with the default no-op event notifier.
    pub fn start(
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
    ) -> Result<Self, SshSessionStartError> {
        Self::start_with_notifier(profile, authentication, noop_session_event_notifier())
    }

    /// Starts a session and wakes `event_notifier` after every queued event.
    pub fn start_with_notifier(
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
        event_notifier: Arc<dyn SessionEventNotifier>,
    ) -> Result<Self, SshSessionStartError> {
        let (foundation, command_receiver, host_key_resolver) =
            SshWorkerFoundation::new_with_capacities(
                profile.clone(),
                DEFAULT_COMMAND_QUEUE_CAPACITY,
                DEFAULT_EVENT_QUEUE_CAPACITY,
                event_notifier,
            );
        let shared = Arc::clone(&foundation.shared);
        let host_key_gate = Arc::clone(&foundation.host_key_gate);
        let worker_gate = Arc::clone(&host_key_gate);
        let (completion_sender, completion_receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name(format!("festerm-ssh-{}", foundation.id()))
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .build()
                    .map_err(|_| ssh_failure(&shared, "SSH runtime could not start"))
                    .and_then(|runtime| {
                        runtime.block_on(ssh_worker(
                            profile,
                            authentication,
                            shared,
                            command_receiver,
                            worker_gate,
                        ))
                    });
                let _ = completion_sender.send(result);
            })
            .map_err(|_| SshSessionStartError)?;

        Ok(Self {
            foundation,
            host_key_resolver,
            host_key_gate,
            completion_receiver: Mutex::new(completion_receiver),
            completion: Mutex::new(None),
        })
    }

    /// Returns a resolver for the current host-key verification request.
    pub fn host_key_decision_resolver(&self) -> HostKeyDecisionResolver {
        self.host_key_resolver.clone()
    }

    fn await_completion(&self, timeout: Duration) -> Result<ShutdownResult, ShutdownError> {
        let mut completion = self
            .completion
            .lock()
            .expect("SSH completion lock is not poisoned");
        if let Some(result) = completion.clone() {
            return result.map_err(ShutdownError::Failed);
        }
        match self
            .completion_receiver
            .lock()
            .expect("SSH completion receiver lock is not poisoned")
            .recv_timeout(timeout)
        {
            Ok(result) => {
                *completion = Some(result.clone());
                result.map_err(ShutdownError::Failed)
            }
            Err(RecvTimeoutError::Timeout) => Err(ShutdownError::TimedOut { timeout }),
            Err(RecvTimeoutError::Disconnected) => Err(ShutdownError::Failed(SessionError::new(
                SessionErrorKind::Shutdown,
                "SSH worker ended without reporting shutdown completion",
            ))),
        }
    }
}

impl Session for SshSession {
    fn id(&self) -> SessionId {
        self.foundation.id()
    }

    fn lifecycle(&self) -> SessionLifecycle {
        self.foundation.lifecycle()
    }

    fn metrics(&self) -> SessionMetrics {
        self.foundation.metrics()
    }

    fn try_send_input(&self, bytes: &[u8]) -> Result<(), SessionSendError> {
        self.foundation.try_send_input(bytes)
    }

    fn try_resize(&self, size: TerminalSize) -> Result<(), SessionSendError> {
        self.foundation.try_resize(size)
    }

    fn try_shutdown(&self) -> Result<(), SessionSendError> {
        self.host_key_gate.reject_pending();
        self.foundation.try_shutdown()
    }

    fn try_recv_event(&self) -> Result<SessionEvent, SessionTryReceiveError> {
        self.foundation.try_recv_event()
    }

    fn shutdown(&self, timeout: Duration) -> Result<ShutdownResult, ShutdownError> {
        let _ = self.try_shutdown();
        self.await_completion(timeout)
    }
}

impl Drop for SshSession {
    fn drop(&mut self) {
        // Explicit `Session::shutdown` performs the caller-bounded wait. This
        // destructor only wakes the worker and never blocks application exit.
        self.host_key_gate.reject_pending();
        let _ = self.foundation.try_shutdown();
    }
}

struct SshClientHandler {
    identity: HostIdentity,
    shared: Arc<WorkerShared>,
    host_key_gate: Arc<HostKeyDecisionGate>,
}

impl russh::client::Handler for SshClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fingerprint = sha256_fingerprint(server_public_key);
        let waiter = match request_host_key_verification(
            &self.identity,
            &self.shared,
            &self.host_key_gate,
            &fingerprint,
        ) {
            Ok(waiter) => waiter,
            Err(_) => return Ok(false),
        };
        Ok(matches!(
            waiter
                .wait_async(&self.host_key_gate, HOST_KEY_DECISION_TIMEOUT)
                .await,
            HostTrustDecision::AcceptOnce | HostTrustDecision::AcceptAndPersist
        ))
    }
}

async fn ssh_worker(
    profile: SshConnectionProfile,
    authentication: SshAuthentication,
    shared: Arc<WorkerShared>,
    command_receiver: WorkerCommandReceiver,
    host_key_gate: Arc<HostKeyDecisionGate>,
) -> Result<ShutdownResult, SessionError> {
    if process_commands_before_running(&command_receiver, &shared, &host_key_gate) {
        shared.set_lifecycle(SessionLifecycle::Stopped);
        return Ok(ShutdownResult::Stopped);
    }

    let config = Arc::new(russh::client::Config {
        nodelay: true,
        ..Default::default()
    });
    let handler = SshClientHandler {
        identity: profile.identity.clone(),
        shared: Arc::clone(&shared),
        host_key_gate: Arc::clone(&host_key_gate),
    };
    let connection = russh::client::connect(
        config,
        (profile.identity.host(), profile.identity.port()),
        handler,
    );
    tokio::pin!(connection);
    let connection_timeout = tokio::time::sleep(CONNECT_TIMEOUT);
    tokio::pin!(connection_timeout);

    let mut handle = loop {
        tokio::select! {
            result = &mut connection => match result {
                Ok(handle) => break handle,
                Err(_) => return Err(ssh_failure(&shared, "SSH connection failed")),
            },
            _ = &mut connection_timeout => {
                return Err(ssh_failure(&shared, "SSH connection timed out"));
            }
            _ = tokio::time::sleep(COMMAND_POLL_INTERVAL) => {
                if process_commands_before_running(&command_receiver, &shared, &host_key_gate) {
                    shared.set_lifecycle(SessionLifecycle::Stopped);
                    return Ok(ShutdownResult::Stopped);
                }
            }
        }
    };

    let password = authentication.into_password();
    let authentication_result = wait_for_ssh_operation(
        handle.authenticate_password(profile.username(), password),
        &command_receiver,
        &shared,
        &host_key_gate,
    )
    .await;
    let authenticated = match authentication_result {
        WorkerWait::Completed(Ok(result)) if result.success() => true,
        WorkerWait::Completed(Ok(_)) => false,
        WorkerWait::Completed(Err(_)) => {
            return Err(ssh_failure(&shared, "SSH authentication failed"));
        }
        WorkerWait::Shutdown => {
            return stop_handle(handle, &shared).await;
        }
    };
    if !authenticated {
        return Err(ssh_failure(&shared, "SSH authentication failed"));
    }

    let channel = match wait_for_ssh_operation(
        handle.channel_open_session(),
        &command_receiver,
        &shared,
        &host_key_gate,
    )
    .await
    {
        WorkerWait::Completed(Ok(channel)) => channel,
        WorkerWait::Completed(Err(_)) => {
            return Err(ssh_failure(&shared, "SSH session channel could not open"));
        }
        WorkerWait::Shutdown => {
            return stop_handle(handle, &shared).await;
        }
    };
    let mut channel = channel;
    let dimensions = ssh_terminal_dimensions(profile.initial_size());
    match wait_for_ssh_operation(
        channel.request_pty(
            true,
            profile.terminal_type(),
            dimensions.0,
            dimensions.1,
            dimensions.2,
            dimensions.3,
            &[],
        ),
        &command_receiver,
        &shared,
        &host_key_gate,
    )
    .await
    {
        WorkerWait::Completed(Ok(())) => {}
        WorkerWait::Completed(Err(_)) => {
            return Err(ssh_failure(&shared, "SSH PTY request failed"));
        }
        WorkerWait::Shutdown => {
            return stop_handle(handle, &shared).await;
        }
    }
    match wait_for_channel_request_reply(&mut channel, &command_receiver, &shared, &host_key_gate)
        .await
    {
        ChannelRequestReply::Accepted => {}
        ChannelRequestReply::Rejected => {
            return Err(ssh_failure(&shared, "SSH PTY request was rejected"));
        }
        ChannelRequestReply::Shutdown => {
            return stop_handle(handle, &shared).await;
        }
    }

    match wait_for_ssh_operation(
        channel.request_shell(true),
        &command_receiver,
        &shared,
        &host_key_gate,
    )
    .await
    {
        WorkerWait::Completed(Ok(())) => {}
        WorkerWait::Completed(Err(_)) => {
            return Err(ssh_failure(&shared, "SSH shell request failed"));
        }
        WorkerWait::Shutdown => {
            return stop_handle(handle, &shared).await;
        }
    }
    match wait_for_channel_request_reply(&mut channel, &command_receiver, &shared, &host_key_gate)
        .await
    {
        ChannelRequestReply::Accepted => {}
        ChannelRequestReply::Rejected => {
            return Err(ssh_failure(&shared, "SSH shell request was rejected"));
        }
        ChannelRequestReply::Shutdown => {
            return stop_handle(handle, &shared).await;
        }
    }

    shared.set_lifecycle(SessionLifecycle::Running);
    run_authenticated_channel(handle, channel, command_receiver, shared, host_key_gate).await
}

enum WorkerWait<T> {
    Completed(Result<T, russh::Error>),
    Shutdown,
}

async fn wait_for_ssh_operation<T, F>(
    operation: F,
    command_receiver: &WorkerCommandReceiver,
    shared: &WorkerShared,
    host_key_gate: &HostKeyDecisionGate,
) -> WorkerWait<T>
where
    F: std::future::Future<Output = Result<T, russh::Error>>,
{
    tokio::pin!(operation);
    loop {
        tokio::select! {
            result = &mut operation => return WorkerWait::Completed(result),
            _ = tokio::time::sleep(COMMAND_POLL_INTERVAL) => {
                if process_commands_before_running(command_receiver, shared, host_key_gate) {
                    return WorkerWait::Shutdown;
                }
            }
        }
    }
}

enum ChannelRequestReply {
    Accepted,
    Rejected,
    Shutdown,
}

async fn wait_for_channel_request_reply(
    channel: &mut russh::Channel<russh::client::Msg>,
    command_receiver: &WorkerCommandReceiver,
    shared: &WorkerShared,
    host_key_gate: &HostKeyDecisionGate,
) -> ChannelRequestReply {
    loop {
        tokio::select! {
            message = channel.wait() => match message {
                Some(russh::ChannelMsg::Success) => return ChannelRequestReply::Accepted,
                Some(russh::ChannelMsg::Data { data }) => emit_channel_output(shared, data.as_ref()),
                Some(russh::ChannelMsg::Failure | russh::ChannelMsg::Eof | russh::ChannelMsg::Close) | None => return ChannelRequestReply::Rejected,
                Some(_) => {}
            },
            _ = tokio::time::sleep(COMMAND_POLL_INTERVAL) => {
                if process_commands_before_running(command_receiver, shared, host_key_gate) {
                    return ChannelRequestReply::Shutdown;
                }
            }
        }
    }
}

async fn run_authenticated_channel(
    mut handle: russh::client::Handle<SshClientHandler>,
    mut channel: russh::Channel<russh::client::Msg>,
    command_receiver: WorkerCommandReceiver,
    shared: Arc<WorkerShared>,
    host_key_gate: Arc<HostKeyDecisionGate>,
) -> Result<ShutdownResult, SessionError> {
    loop {
        tokio::select! {
            result = &mut handle => match result {
                Ok(()) => {
                    shared.set_lifecycle(SessionLifecycle::Exited(festerm_session::SessionExit::with_exit_code(0)));
                    return Ok(ShutdownResult::AlreadyStopped);
                }
                Err(_) => return Err(ssh_failure(&shared, "SSH connection ended unexpectedly")),
            },
            message = channel.wait() => match message {
                Some(russh::ChannelMsg::Data { data }) => emit_channel_output(&shared, data.as_ref()),
                Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                    shared.set_lifecycle(SessionLifecycle::Exited(festerm_session::SessionExit::with_exit_code(exit_status)));
                    return Ok(ShutdownResult::AlreadyStopped);
                }
                Some(russh::ChannelMsg::ExitSignal { .. }) => {
                    shared.set_lifecycle(SessionLifecycle::Exited(festerm_session::SessionExit::with_signal(0, "remote signal")));
                    return Ok(ShutdownResult::AlreadyStopped);
                }
                Some(russh::ChannelMsg::Eof | russh::ChannelMsg::Close) | None => {
                    shared.set_lifecycle(SessionLifecycle::Exited(festerm_session::SessionExit::with_exit_code(0)));
                    return Ok(ShutdownResult::AlreadyStopped);
                }
                Some(_) => {}
            },
            _ = tokio::time::sleep(COMMAND_POLL_INTERVAL) => {
                match process_authenticated_commands(&mut channel, &command_receiver, &shared, &host_key_gate).await {
                    Ok(false) => {}
                    Ok(true) => return stop_handle(handle, &shared).await,
                    Err(error) => return Err(error),
                }
            }
        }
    }
}

async fn process_authenticated_commands(
    channel: &mut russh::Channel<russh::client::Msg>,
    command_receiver: &WorkerCommandReceiver,
    shared: &WorkerShared,
    host_key_gate: &HostKeyDecisionGate,
) -> Result<bool, SessionError> {
    loop {
        match command_receiver.try_recv() {
            Ok(WorkerCommand::Input(bytes)) => {
                let byte_count = bytes.len();
                match wait_for_ssh_operation(
                    channel.data_bytes(bytes),
                    command_receiver,
                    shared,
                    host_key_gate,
                )
                .await
                {
                    WorkerWait::Completed(Ok(())) => shared.record_input_sent(byte_count),
                    WorkerWait::Completed(Err(_)) => {
                        return Err(ssh_failure_with_kind(
                            shared,
                            SessionErrorKind::Input,
                            "SSH input failed",
                        ));
                    }
                    WorkerWait::Shutdown => return Ok(true),
                }
            }
            Ok(WorkerCommand::Resize(size)) => {
                let dimensions = ssh_terminal_dimensions(size);
                match wait_for_ssh_operation(
                    channel.window_change(dimensions.0, dimensions.1, dimensions.2, dimensions.3),
                    command_receiver,
                    shared,
                    host_key_gate,
                )
                .await
                {
                    WorkerWait::Completed(Ok(())) => shared.record_resize_applied(size),
                    WorkerWait::Completed(Err(_)) => {
                        return Err(ssh_failure_with_kind(
                            shared,
                            SessionErrorKind::Resize,
                            "SSH resize failed",
                        ));
                    }
                    WorkerWait::Shutdown => return Ok(true),
                }
            }
            Ok(WorkerCommand::Shutdown) | Err(TryRecvError::Disconnected) => {
                host_key_gate.reject_pending();
                shared.set_lifecycle(SessionLifecycle::Stopping);
                return Ok(true);
            }
            Err(TryRecvError::Empty) => return Ok(false),
        }
    }
}

fn process_commands_before_running(
    command_receiver: &WorkerCommandReceiver,
    shared: &WorkerShared,
    host_key_gate: &HostKeyDecisionGate,
) -> bool {
    loop {
        match command_receiver.try_recv() {
            Ok(WorkerCommand::Input(bytes)) => {
                let _ = bytes.len();
                report_unsupported(shared, "SSH input is not available");
            }
            Ok(WorkerCommand::Resize(size)) => {
                let _ = size.columns();
                report_unsupported(shared, "SSH resize is not available")
            }
            Ok(WorkerCommand::Shutdown) | Err(TryRecvError::Disconnected) => {
                host_key_gate.reject_pending();
                shared.set_lifecycle(SessionLifecycle::Stopping);
                return true;
            }
            Err(TryRecvError::Empty) => return false,
        }
    }
}

async fn stop_handle(
    mut handle: russh::client::Handle<SshClientHandler>,
    shared: &WorkerShared,
) -> Result<ShutdownResult, SessionError> {
    let _ = tokio::time::timeout(DISCONNECT_TIMEOUT, async {
        let _ = handle
            .disconnect(russh::Disconnect::ByApplication, "fesTerm shutdown", "")
            .await;
        let _ = (&mut handle).await;
    })
    .await;
    shared.set_lifecycle(SessionLifecycle::Stopped);
    Ok(ShutdownResult::Stopped)
}

fn report_unsupported(shared: &WorkerShared, message: &'static str) {
    let _ = shared.try_emit(SessionEvent::Error(SessionError::new(
        SessionErrorKind::Unsupported,
        message,
    )));
}

fn ssh_failure(shared: &WorkerShared, message: &'static str) -> SessionError {
    ssh_failure_with_kind(shared, SessionErrorKind::Spawn, message)
}

fn ssh_failure_with_kind(
    shared: &WorkerShared,
    kind: SessionErrorKind,
    message: &'static str,
) -> SessionError {
    let error = SessionError::new(kind, message);
    let _ = shared.try_emit(SessionEvent::Error(error.clone()));
    shared.set_lifecycle(SessionLifecycle::Failed(error.clone()));
    error
}

fn ssh_terminal_dimensions(size: TerminalSize) -> (u32, u32, u32, u32) {
    (
        u32::from(size.columns()),
        u32::from(size.rows()),
        u32::from(size.pixel_width().unwrap_or(0)),
        u32::from(size.pixel_height().unwrap_or(0)),
    )
}

fn emit_channel_output(shared: &WorkerShared, data: &[u8]) {
    for chunk in data.chunks(MAX_IO_CHUNK_BYTES) {
        if !shared.try_emit(SessionEvent::Output(chunk.to_vec())) {
            break;
        }
    }
}

fn sha256_fingerprint(public_key: &russh::keys::PublicKey) -> String {
    public_key
        .fingerprint(russh::keys::HashAlg::Sha256)
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::{
        net::TcpListener,
        sync::atomic::{AtomicUsize, Ordering},
        thread,
        time::Instant,
    };

    use super::*;
    use russh::client::Handler as _;

    fn profile() -> SshConnectionProfile {
        SshConnectionProfile::new(
            HostIdentity::new("Example.COM", 22).unwrap(),
            "alice",
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            TerminalSize::new(80, 24).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn host_identity_normalizes_and_rejects_ambiguous_input() {
        let host = HostIdentity::new(" Example.COM ", 22).unwrap();
        assert_eq!(host.host(), "example.com");
        assert_eq!(host.port(), 22);
        assert_eq!(
            HostIdentity::new(" ", 22),
            Err(HostIdentityError::EmptyHost)
        );
        assert_eq!(
            HostIdentity::new("example host", 22),
            Err(HostIdentityError::Whitespace)
        );
        assert_eq!(
            HostIdentity::new("example.com", 0),
            Err(HostIdentityError::ZeroPort)
        );
    }

    #[test]
    fn connection_profile_validates_safe_non_secret_inputs() {
        let profile = profile();
        assert_eq!(profile.identity().host(), "example.com");
        assert_eq!(profile.username(), "alice");
        assert_eq!(profile.terminal_type(), "xterm-256color");
        assert_eq!(profile.initial_size().columns(), 80);

        let identity = HostIdentity::new("example.com", 22).unwrap();
        assert_eq!(
            SshConnectionProfile::new(
                identity.clone(),
                "",
                "xterm-256color",
                TerminalSize::new(80, 24).unwrap()
            ),
            Err(SshConnectionProfileError::EmptyUsername)
        );
        assert_eq!(
            SshConnectionProfile::new(
                identity.clone(),
                "alice admin",
                "xterm-256color",
                TerminalSize::new(80, 24).unwrap()
            ),
            Err(SshConnectionProfileError::UsernameWhitespace)
        );
        assert_eq!(
            SshConnectionProfile::new(
                identity.clone(),
                "alice\u{7f}",
                "xterm-256color",
                TerminalSize::new(80, 24).unwrap()
            ),
            Err(SshConnectionProfileError::UsernameControlCharacter)
        );
        assert_eq!(
            SshConnectionProfile::new(
                identity.clone(),
                "alice",
                "xterm 256color",
                TerminalSize::new(80, 24).unwrap()
            ),
            Err(SshConnectionProfileError::InvalidTerminalType)
        );
        assert!(matches!(
            SshConnectionProfile::new(
                identity,
                "alice",
                "t".repeat(SshConnectionProfile::MAX_TERMINAL_TYPE_BYTES + 1),
                TerminalSize::new(80, 24).unwrap()
            ),
            Err(SshConnectionProfileError::TerminalTypeTooLong { .. })
        ));
    }

    #[test]
    fn password_authentication_redacts_the_secret() {
        let authentication = SshAuthentication::password("not-for-debug-output");

        assert_eq!(
            format!("{authentication:?}"),
            "SshAuthentication::Password([REDACTED])"
        );
    }

    #[test]
    fn terminal_size_converts_to_ssh_pty_dimensions() {
        assert_eq!(
            ssh_terminal_dimensions(TerminalSize::new(80, 24).unwrap()),
            (80, 24, 0, 0)
        );
        assert_eq!(
            ssh_terminal_dimensions(TerminalSize::with_pixels(120, 40, 1200, 800).unwrap()),
            (120, 40, 1200, 800)
        );
    }

    #[test]
    fn reconnect_policy_requires_finite_ordered_bounds() {
        assert_eq!(
            ReconnectPolicy::new(0, Duration::from_secs(1), Duration::from_secs(2)),
            Err(ReconnectPolicyError::ZeroAttempts)
        );
        assert_eq!(
            ReconnectPolicy::new(1, Duration::ZERO, Duration::from_secs(2)),
            Err(ReconnectPolicyError::ZeroInitialDelay)
        );
        assert_eq!(
            ReconnectPolicy::new(1, Duration::from_secs(2), Duration::from_secs(1)),
            Err(ReconnectPolicyError::MaximumBeforeInitial)
        );
    }

    #[test]
    fn worker_command_queue_is_bounded_and_rejects_large_input() {
        let (worker, receiver, _) = SshWorkerFoundation::new_with_capacities(
            profile(),
            2,
            4,
            noop_session_event_notifier(),
        );
        let too_large = vec![0; MAX_IO_CHUNK_BYTES + 1];
        assert_eq!(
            worker.try_send_input(&too_large),
            Err(SessionSendError::TooLarge {
                operation: SessionOperation::Input,
                maximum: MAX_IO_CHUNK_BYTES,
                actual: MAX_IO_CHUNK_BYTES + 1,
            })
        );
        worker.try_send_input(b"one").unwrap();
        worker
            .try_resize(TerminalSize::new(100, 40).unwrap())
            .unwrap();
        assert_eq!(
            worker.try_shutdown(),
            Err(SessionSendError::Full {
                operation: SessionOperation::Shutdown,
                capacity: 2,
            })
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::Input(bytes)) if bytes == b"one"
        ));
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerCommand::Resize(size)) if size == TerminalSize::new(100, 40).unwrap()
        ));
        assert!(matches!(
            worker.try_recv_event(),
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Starting))
        ));
        assert!(matches!(
            worker.try_recv_event(),
            Ok(SessionEvent::Backpressure {
                direction: FlowDirection::Control,
                queued: 2,
                capacity: 2,
            })
        ));
        assert_eq!(worker.metrics().backpressure_count, 1);
    }

    #[test]
    fn worker_events_update_lifecycle_metrics_and_notifier() {
        let notifier = Arc::new(CountingNotifier::default());
        let (worker, _receiver, _) =
            SshWorkerFoundation::new_with_capacities(profile(), 2, 4, notifier.clone());
        let id = worker.id();
        assert_eq!(worker.lifecycle(), SessionLifecycle::Starting);
        worker.set_running();
        assert!(worker.shared.record_output(b"ready".to_vec()));
        worker
            .shared
            .record_resize_applied(TerminalSize::new(100, 40).unwrap());
        worker.shared.record_input_sent(3);

        let metrics = worker.metrics();
        assert_eq!(metrics.input_bytes, 3);
        assert_eq!(metrics.output_bytes, 5);
        assert_eq!(metrics.resize_count, 1);
        assert_eq!(metrics.event_queue_depth, 4);
        assert_eq!(metrics.event_queue_high_watermark, 4);
        assert_eq!(metrics.event_queue_capacity, 4);
        assert_eq!(notifier.notifications(), 4);
        assert!(id.get() > 0);
        assert!(matches!(
            worker.try_recv_event(),
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Starting))
        ));
        assert!(matches!(
            worker.try_recv_event(),
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running))
        ));
        assert!(matches!(
            worker.try_recv_event(),
            Ok(SessionEvent::Output(bytes)) if bytes == b"ready"
        ));
        assert!(matches!(
            worker.try_recv_event(),
            Ok(SessionEvent::ResizeApplied(size)) if size == TerminalSize::new(100, 40).unwrap()
        ));
        assert_eq!(worker.metrics().event_queue_depth, 0);
        assert_eq!(worker.try_recv_event(), Err(SessionTryReceiveError::Empty));
    }

    #[test]
    fn host_key_gate_accepts_only_explicit_resolutions() {
        let (worker, _receiver, resolver) = SshWorkerFoundation::new(profile());
        let waiter = worker
            .request_host_key_verification("SHA256:abcDef012+/")
            .unwrap();
        assert!(matches!(
            worker.try_recv_event(),
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Starting))
        ));
        let prompt = match worker.try_recv_event() {
            Ok(SessionEvent::HostKeyVerification(prompt)) => prompt,
            event => panic!("expected host-key prompt, received {event:?}"),
        };
        assert_eq!(prompt.host(), "example.com");
        assert_eq!(prompt.port(), 22);
        assert_eq!(prompt.sha256_fingerprint(), "SHA256:abcDef012+/");
        resolver
            .resolve(&prompt, HostTrustDecision::AcceptOnce)
            .unwrap();
        assert_eq!(
            waiter.wait(&worker.host_key_gate, Duration::ZERO),
            HostTrustDecision::AcceptOnce
        );
    }

    #[test]
    fn stale_host_key_decision_cannot_approve_a_later_prompt() {
        let (worker, _receiver, resolver) = SshWorkerFoundation::new(profile());
        let expired = worker
            .request_host_key_verification("SHA256:expired")
            .unwrap();
        let expired_prompt = expired.prompt.clone();
        assert_eq!(
            expired.wait(&worker.host_key_gate, Duration::ZERO),
            HostTrustDecision::Reject
        );

        let current = worker
            .request_host_key_verification("SHA256:current")
            .unwrap();
        assert_eq!(
            resolver.resolve(&expired_prompt, HostTrustDecision::AcceptOnce),
            Err(HostKeyDecisionResolutionError::PromptMismatch)
        );
        resolver
            .resolve(&current.prompt, HostTrustDecision::AcceptOnce)
            .unwrap();
        assert_eq!(
            current.wait(&worker.host_key_gate, Duration::ZERO),
            HostTrustDecision::AcceptOnce
        );
    }

    #[test]
    fn host_key_gate_rejects_missing_timeout_cancel_and_invalid_resolutions() {
        let (worker, _receiver, resolver) = SshWorkerFoundation::new(profile());
        assert_eq!(
            resolver.resolve(
                &festerm_session::HostKeyPrompt::new("example.com", 22, "SHA256:missing"),
                HostTrustDecision::AcceptAndPersist
            ),
            Err(HostKeyDecisionResolutionError::NoPendingPrompt)
        );
        assert!(matches!(
            worker.request_host_key_verification("not-a-fingerprint"),
            Err(HostKeyVerificationRequestError::InvalidFingerprint)
        ));

        let timed_out = worker
            .request_host_key_verification("SHA256:timeOut")
            .unwrap();
        assert_eq!(
            timed_out.wait(&worker.host_key_gate, Duration::ZERO),
            HostTrustDecision::Reject
        );

        let cancelled = worker
            .request_host_key_verification("SHA256:cancel")
            .unwrap();
        resolver.cancel(&cancelled.prompt).unwrap();
        assert_eq!(
            cancelled.wait(&worker.host_key_gate, Duration::ZERO),
            HostTrustDecision::Reject
        );

        let resolved = worker
            .request_host_key_verification("SHA256:reject")
            .unwrap();
        resolver
            .resolve(&resolved.prompt, HostTrustDecision::Reject)
            .unwrap();
        assert_eq!(
            resolver.resolve(&resolved.prompt, HostTrustDecision::AcceptOnce),
            Err(HostKeyDecisionResolutionError::AlreadyResolved)
        );
        assert_eq!(
            resolved.wait(&worker.host_key_gate, Duration::ZERO),
            HostTrustDecision::Reject
        );
    }

    #[test]
    fn host_key_prompt_is_rejected_when_the_bounded_event_queue_is_full() {
        let (worker, _receiver, resolver) = SshWorkerFoundation::new_with_capacities(
            profile(),
            1,
            1,
            noop_session_event_notifier(),
        );
        assert!(matches!(
            worker.request_host_key_verification("SHA256:queueFull"),
            Err(HostKeyVerificationRequestError::EventQueueFull)
        ));
        assert_eq!(
            resolver.cancel(&festerm_session::HostKeyPrompt::new(
                "example.com",
                22,
                "SHA256:queueFull"
            )),
            Err(HostKeyDecisionResolutionError::NoPendingPrompt)
        );
        assert_eq!(worker.metrics().backpressure_count, 1);
    }

    #[test]
    fn sha256_fingerprint_uses_canonical_unpadded_ssh_format() {
        let public_key = russh::keys::PublicKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti",
        )
        .unwrap();

        assert_eq!(
            sha256_fingerprint(&public_key),
            "SHA256:UCUiLr7Pjs9wFFJMDByLgc3NrtdU344OgUM45wZPcIQ"
        );
    }

    #[test]
    fn handler_accepts_only_resolved_host_key_decisions() {
        let (worker, _receiver, resolver) = SshWorkerFoundation::new(profile());
        let public_key = russh::keys::PublicKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti",
        )
        .unwrap();
        let identity = worker.profile.identity.clone();
        let shared = Arc::clone(&worker.shared);
        let gate = Arc::clone(&worker.host_key_gate);
        let callback = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap();
            let mut handler = SshClientHandler {
                identity,
                shared,
                host_key_gate: gate,
            };
            runtime
                .block_on(handler.check_server_key(&public_key))
                .unwrap()
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        let prompt = loop {
            match worker.try_recv_event() {
                Ok(SessionEvent::HostKeyVerification(prompt)) => break prompt,
                Ok(_) | Err(SessionTryReceiveError::Empty) => {
                    assert!(
                        Instant::now() < deadline,
                        "host-key callback did not prompt"
                    );
                    thread::sleep(Duration::from_millis(1));
                }
                Err(SessionTryReceiveError::Closed) => panic!("host-key callback closed early"),
            }
        };
        resolver
            .resolve(&prompt, HostTrustDecision::AcceptAndPersist)
            .unwrap();
        assert!(callback.join().unwrap());
    }

    #[test]
    fn closed_loopback_connection_fails_within_a_bounded_timeout() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let profile = SshConnectionProfile::new(
            HostIdentity::new("127.0.0.1", port).unwrap(),
            "alice",
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            TerminalSize::new(80, 24).unwrap(),
        )
        .unwrap();
        let session =
            SshSession::start(profile, SshAuthentication::password("test-password")).unwrap();

        let deadline = Instant::now() + Duration::from_secs(3);
        let error = loop {
            match session.try_recv_event() {
                Ok(SessionEvent::Error(error)) => break error,
                Ok(_) | Err(SessionTryReceiveError::Empty) => {
                    assert!(
                        Instant::now() < deadline,
                        "closed loopback connection did not fail promptly"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                Err(SessionTryReceiveError::Closed) => panic!("SSH worker closed without an error"),
            }
        };
        assert_eq!(error.kind(), SessionErrorKind::Spawn);
        assert!(matches!(
            session.lifecycle(),
            SessionLifecycle::Failed(SessionError { .. })
        ));
        match session.shutdown(Duration::from_secs(1)) {
            Err(ShutdownError::Failed(error)) => {
                assert_eq!(error.kind(), SessionErrorKind::Spawn);
            }
            result => panic!("expected failed SSH shutdown, received {result:?}"),
        }
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
}
