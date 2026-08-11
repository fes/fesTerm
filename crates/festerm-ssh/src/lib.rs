//! Native SSH transport policy and worker foundations.
//!
//! This crate deliberately does not create a remote connection yet.  It
//! defines the validated, secret-free profile and bounded worker seams that a
//! dedicated Tokio/`russh` worker will use.

use std::{
    fmt,
    sync::{
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc, Condvar, Mutex,
    },
    time::Duration,
};

use festerm_session::{
    noop_session_event_notifier, FlowDirection, SessionEvent, SessionEventNotifier, SessionId,
    SessionLifecycle, SessionMetrics, SessionOperation, SessionSendError, SessionTryReceiveError,
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
}

#[allow(dead_code)]
impl HostKeyDecisionGate {
    fn new() -> Self {
        Self {
            state: Mutex::new(HostKeyGateState::Idle),
            changed: Condvar::new(),
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
}

#[allow(dead_code)]
enum WorkerCommand {
    Input(Vec<u8>),
    Resize(TerminalSize),
    Shutdown,
}

/// Private receiver handed to the eventual dedicated SSH worker.
#[allow(dead_code)]
struct WorkerCommandReceiver {
    receiver: Receiver<WorkerCommand>,
}

#[allow(dead_code)]
impl WorkerCommandReceiver {
    fn try_recv(&self) -> Result<WorkerCommand, TryRecvError> {
        self.receiver.try_recv()
    }
}

#[allow(dead_code)]
struct WorkerShared {
    id: SessionId,
    lifecycle: Mutex<SessionLifecycle>,
    metrics: Mutex<SessionMetrics>,
    event_sender: SyncSender<SessionEvent>,
    event_notifier: Arc<dyn SessionEventNotifier>,
}

#[allow(dead_code)]
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

/// Bounded, runtime-independent coordination owned by a future SSH worker.
///
/// It is intentionally private until a live `russh` worker can implement the
/// complete `festerm_session::Session` contract without pretending to connect.
#[allow(dead_code)]
struct SshWorkerFoundation {
    profile: SshConnectionProfile,
    shared: Arc<WorkerShared>,
    command_sender: SyncSender<WorkerCommand>,
    command_capacity: usize,
    event_receiver: Mutex<Receiver<SessionEvent>>,
    host_key_gate: Arc<HostKeyDecisionGate>,
}

#[allow(dead_code)]
impl SshWorkerFoundation {
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

    fn request_host_key_verification(
        &self,
        sha256_fingerprint: &str,
    ) -> Result<HostKeyDecisionWaiter, HostKeyVerificationRequestError> {
        if !is_sha256_fingerprint(sha256_fingerprint) {
            return Err(HostKeyVerificationRequestError::InvalidFingerprint);
        }
        let prompt = festerm_session::HostKeyPrompt::new(
            self.profile.identity.host(),
            self.profile.identity.port(),
            sha256_fingerprint,
        );
        let waiter = self
            .host_key_gate
            .begin(prompt.clone())
            .map_err(HostKeyVerificationRequestError::Resolution)?;
        if self
            .shared
            .try_emit(SessionEvent::HostKeyVerification(prompt.clone()))
        {
            Ok(waiter)
        } else {
            let _ = self.host_key_gate.cancel(&prompt);
            // No waiter escaped on this path, so reset the rejection before a
            // future reconnect retries host verification.
            let _ = self.host_key_gate.wait_for_decision(Duration::ZERO);
            Err(HostKeyVerificationRequestError::EventQueueFull)
        }
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

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
