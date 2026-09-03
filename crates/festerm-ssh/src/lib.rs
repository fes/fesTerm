//! Native SSH transport policy and bounded `russh` session lifecycle.

use std::{
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
        Arc, Condvar, Mutex,
    },
    thread,
    time::Duration,
};

use festerm_secret_store::{SecretBytes, SecretReference, SecretStore, SecretStoreError};
use festerm_session::{
    noop_session_event_notifier, FlowDirection, Session, SessionError, SessionErrorKind,
    SessionEvent, SessionEventNotifier, SessionId, SessionLifecycle, SessionMetrics,
    SessionOperation, SessionSendError, SessionTryReceiveError, ShutdownError, ShutdownResult,
    TerminalSize, DEFAULT_COMMAND_QUEUE_CAPACITY, DEFAULT_EVENT_QUEUE_CAPACITY, MAX_IO_CHUNK_BYTES,
};
use zeroize::Zeroize;

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

/// Authentication selected for one new SSH session.
///
/// Authentication material is moved into the worker and never belongs in a
/// connection profile or imported OpenSSH metadata. Public keys supplied here
/// are parsed in memory before the session starts. Agents, key-file references,
/// and persisted/UI profile integration are deliberately deferred.
pub enum SshAuthentication {
    Password(SshPassword),
    StoredPassword(StoredPasswordAuthentication),
    PublicKey(SshPrivateKey),
    StoredPrivateKey(StoredPrivateKeyAuthentication),
    /// No credential is supplied upfront. The worker connects and verifies
    /// the host key first (exactly as it would for any other credential),
    /// then requests a password from the application only once the
    /// connection actually needs one — mirroring `ssh`'s own ordering
    /// (host-key confirmation, then `user@host's password:`) instead of
    /// collecting a password blind before a connection even exists. A
    /// rejected password is retried in place, on the same connection, up to
    /// [`MAX_INTERACTIVE_PASSWORD_ATTEMPTS`].
    Interactive,
}

impl SshAuthentication {
    /// Selects password authentication for this session.
    ///
    /// The password is moved directly to the worker and is never exposed
    /// through this API, persisted, or included in debug output. An explicit
    /// reconnect policy retains it in that worker for its bounded fresh
    /// authentication attempts only.
    pub fn password(password: impl Into<String>) -> Self {
        Self::Password(SshPassword {
            password: password.into(),
        })
    }

    /// Selects a password held by the platform native secure store.
    ///
    /// The reference is copied only into the SSH worker's authentication
    /// source. Its secret is resolved there immediately before each
    /// authentication attempt; neither the UI nor the connection profile can
    /// retrieve it. This M8 variant represents an SSH password only, not a
    /// private key, passphrase, agent response, or arbitrary credential.
    pub fn stored_password(store: Arc<dyn SecretStore>, reference: &SecretReference) -> Self {
        Self::StoredPassword(StoredPasswordAuthentication {
            store,
            reference: reference.duplicate_for_transport(),
        })
    }

    /// Selects a private key + optional passphrase held by the platform
    /// native secure store, encoded together by
    /// [`encode_stored_private_key`].
    ///
    /// The reference is copied only into the SSH worker's authentication
    /// source. Its secret is resolved and parsed there immediately before
    /// each authentication attempt; neither the UI nor the connection
    /// profile can retrieve it. This mirrors [`Self::stored_password`] but
    /// for private-key material instead of a password.
    pub fn stored_private_key(store: Arc<dyn SecretStore>, reference: &SecretReference) -> Self {
        Self::StoredPrivateKey(StoredPrivateKeyAuthentication {
            store,
            reference: reference.duplicate_for_transport(),
        })
    }

    /// Selects public-key authentication for this session.
    ///
    /// `private_key` is moved directly to the worker. An explicit reconnect
    /// policy retains only its parsed key in that worker for bounded fresh
    /// authentication attempts.
    pub fn public_key(private_key: SshPrivateKey) -> Self {
        Self::PublicKey(private_key)
    }

    /// Selects interactive password authentication: no credential is
    /// supplied upfront, so the worker prompts for one (through
    /// [`SessionEvent::PasswordRequested`]) only after the host key has
    /// already been verified.
    pub const fn interactive() -> Self {
        Self::Interactive
    }

    fn into_worker_authentication(self) -> WorkerAuthentication {
        match self {
            Self::Password(password) => WorkerAuthentication::Password(password.password),
            Self::StoredPassword(password) => WorkerAuthentication::StoredPassword(password),
            Self::PublicKey(private_key) => WorkerAuthentication::PublicKey(private_key.key),
            Self::StoredPrivateKey(private_key) => {
                WorkerAuthentication::StoredPrivateKey(private_key)
            }
            Self::Interactive => WorkerAuthentication::Interactive,
        }
    }
}

impl fmt::Debug for SshAuthentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password(_) => formatter.write_str("SshAuthentication::Password([REDACTED])"),
            Self::StoredPassword(_) => {
                formatter.write_str("SshAuthentication::StoredPassword([REDACTED])")
            }
            Self::PublicKey(_) => formatter.write_str("SshAuthentication::PublicKey([REDACTED])"),
            Self::StoredPrivateKey(_) => {
                formatter.write_str("SshAuthentication::StoredPrivateKey([REDACTED])")
            }
            Self::Interactive => formatter.write_str("SshAuthentication::Interactive"),
        }
    }
}

/// Password material consumed by [`SshAuthentication`] by the worker.
///
/// This type intentionally has no getter, `Clone`, or derived `Debug`
/// implementation. It is not a persistent connection-profile field.
pub struct SshPassword {
    password: String,
}

/// Opaque native-store password source consumed only by the SSH worker.
///
/// It intentionally has no accessors or `Debug` implementation. The stored
/// credential is constrained to SSH password authentication by
/// [`SshAuthentication::stored_password`].
pub struct StoredPasswordAuthentication {
    store: Arc<dyn SecretStore>,
    reference: SecretReference,
}

/// Opaque native-store private-key source consumed only by the SSH worker.
///
/// It intentionally has no accessors or `Debug` implementation. The stored
/// bytes are constrained to SSH public-key authentication by
/// [`SshAuthentication::stored_private_key`] and are encoded/decoded by
/// [`encode_stored_private_key`]/[`decode_stored_private_key`].
pub struct StoredPrivateKeyAuthentication {
    store: Arc<dyn SecretStore>,
    reference: SecretReference,
}

/// Encodes an OpenSSH private key and its optional passphrase into one
/// [`SecretBytes`] blob for storage under a single [`SecretReference`].
///
/// Format: a 4-byte little-endian passphrase length, followed by the raw
/// passphrase bytes (empty when there is no passphrase), followed by the
/// OpenSSH private-key text. A length prefix (rather than a delimiter byte)
/// is used so the key text's own bytes never need to be constrained.
pub fn encode_stored_private_key(key_text: &str, passphrase: Option<&str>) -> SecretBytes {
    let passphrase = passphrase.unwrap_or("");
    let mut bytes = Vec::with_capacity(4 + passphrase.len() + key_text.len());
    bytes.extend_from_slice(&(passphrase.len() as u32).to_le_bytes());
    bytes.extend_from_slice(passphrase.as_bytes());
    bytes.extend_from_slice(key_text.as_bytes());
    SecretBytes::copy_from_slice(&bytes)
}

fn decode_stored_private_key(
    bytes: &[u8],
) -> Result<(String, String), StoredPrivateKeyResolutionError> {
    if bytes.len() < 4 {
        return Err(StoredPrivateKeyResolutionError::InvalidEncoding);
    }
    let (length_prefix, remainder) = bytes.split_at(4);
    let passphrase_length =
        u32::from_le_bytes(length_prefix.try_into().expect("checked length")) as usize;
    if remainder.len() < passphrase_length {
        return Err(StoredPrivateKeyResolutionError::InvalidEncoding);
    }
    let (passphrase, key_text) = remainder.split_at(passphrase_length);
    let passphrase = std::str::from_utf8(passphrase)
        .map_err(|_| StoredPrivateKeyResolutionError::InvalidEncoding)?
        .to_owned();
    let key_text = std::str::from_utf8(key_text)
        .map_err(|_| StoredPrivateKeyResolutionError::InvalidEncoding)?
        .to_owned();
    Ok((key_text, passphrase))
}

/// Content-free failures while resolving a native stored SSH private key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredPrivateKeyResolutionError {
    Missing,
    LockedOrUnavailable,
    BackendFailure,
    InvalidEncoding,
    InvalidKey,
}

impl StoredPrivateKeyResolutionError {
    const fn message(self) -> &'static str {
        match self {
            Self::Missing => {
                "stored SSH private key is missing; replace it for this saved profile and try again"
            }
            Self::LockedOrUnavailable => {
                "native secure storage is locked or unavailable; unlock it and try again"
            }
            Self::BackendFailure => {
                "native secure storage could not read the stored SSH private key; try again or replace it"
            }
            Self::InvalidEncoding | Self::InvalidKey => {
                "stored SSH private key could not be read; replace it for this saved profile and try again"
            }
        }
    }
}

impl fmt::Display for StoredPrivateKeyResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for StoredPrivateKeyResolutionError {}

fn resolve_stored_private_key(
    authentication: &StoredPrivateKeyAuthentication,
) -> Result<Arc<russh::keys::PrivateKey>, StoredPrivateKeyResolutionError> {
    let secret = authentication
        .store
        .get(&authentication.reference)
        .map_err(|error| match error {
            SecretStoreError::Missing => StoredPrivateKeyResolutionError::Missing,
            SecretStoreError::LockedOrUnavailable | SecretStoreError::Unsupported => {
                StoredPrivateKeyResolutionError::LockedOrUnavailable
            }
            SecretStoreError::BackendFailure | SecretStoreError::InvalidReference => {
                StoredPrivateKeyResolutionError::BackendFailure
            }
        })?;
    let (key_text, passphrase) = secret.with_bytes(decode_stored_private_key)?;
    let parsed = if passphrase.is_empty() {
        SshPrivateKey::from_openssh(key_text.as_bytes())
    } else {
        SshPrivateKey::from_openssh(key_text.as_bytes()).or_else(|_| {
            SshPrivateKey::from_encrypted_openssh(
                key_text.as_bytes(),
                SshKeyPassphrase::new(passphrase),
            )
        })
    }
    .map_err(|_| StoredPrivateKeyResolutionError::InvalidKey)?;
    Ok(parsed.key)
}

impl fmt::Debug for SshPassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SshPassword([REDACTED])")
    }
}

/// Transient passphrase consumed while parsing an encrypted OpenSSH private key.
///
/// This type intentionally has no getter, `Clone`, or derived `Debug`
/// implementation. [`SshPrivateKey::from_encrypted_openssh`] consumes it while
/// parsing, and the resulting private key retains no passphrase.
pub struct SshKeyPassphrase {
    passphrase: String,
}

impl SshKeyPassphrase {
    /// Creates a transient passphrase for one encrypted-key parse attempt.
    pub fn new(passphrase: impl Into<String>) -> Self {
        Self {
            passphrase: passphrase.into(),
        }
    }
}

impl fmt::Debug for SshKeyPassphrase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SshKeyPassphrase([REDACTED])")
    }
}

/// Parsed private key material consumed by [`SshAuthentication`] by the worker.
///
/// This type intentionally has no getter, `Clone`, or derived `Debug`
/// implementation. It retains only the parsed private key; input encodings and
/// encrypted-key passphrases are not retained.
pub struct SshPrivateKey {
    key: Arc<russh::keys::PrivateKey>,
}

impl SshPrivateKey {
    /// Parses an unencrypted in-memory OpenSSH private key.
    ///
    /// The supplied bytes are validated immediately and are not retained after
    /// parsing. Encrypted keys must use [`Self::from_encrypted_openssh`] so
    /// passphrase handling is explicit at the call site.
    pub fn from_openssh(encoded: impl AsRef<[u8]>) -> Result<Self, SshPrivateKeyError> {
        let encoded = openssh_private_key_text(encoded.as_ref())?;
        let key = russh::keys::decode_secret_key(encoded, None).map_err(|error| match error {
            russh::keys::Error::KeyIsEncrypted => SshPrivateKeyError::Encrypted,
            _ => SshPrivateKeyError::InvalidKey,
        })?;
        Ok(Self { key: Arc::new(key) })
    }

    /// Parses an encrypted in-memory OpenSSH private key with a transient passphrase.
    ///
    /// The bytes and `passphrase` are consumed during this call. On success,
    /// this type retains only the parsed private key. Unencrypted keys must use
    /// [`Self::from_openssh`] instead.
    pub fn from_encrypted_openssh(
        encoded: impl AsRef<[u8]>,
        passphrase: SshKeyPassphrase,
    ) -> Result<Self, SshPrivateKeyError> {
        let encoded = openssh_private_key_text(encoded.as_ref())?;
        match russh::keys::decode_secret_key(encoded, None) {
            Err(russh::keys::Error::KeyIsEncrypted) => {}
            Ok(_) => return Err(SshPrivateKeyError::Unencrypted),
            Err(_) => return Err(SshPrivateKeyError::InvalidKey),
        }
        let key = russh::keys::decode_secret_key(encoded, Some(&passphrase.passphrase))
            .map_err(|_| SshPrivateKeyError::InvalidKey)?;
        Ok(Self { key: Arc::new(key) })
    }
}

fn openssh_private_key_text(encoded: &[u8]) -> Result<&str, SshPrivateKeyError> {
    let encoded = std::str::from_utf8(encoded).map_err(|_| SshPrivateKeyError::InvalidEncoding)?;
    if !encoded
        .trim_start()
        .starts_with("-----BEGIN OPENSSH PRIVATE KEY-----")
    {
        return Err(SshPrivateKeyError::NotOpenSsh);
    }
    Ok(encoded)
}

impl fmt::Debug for SshPrivateKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SshPrivateKey([REDACTED])")
    }
}

/// Failure to accept an in-memory OpenSSH private key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshPrivateKeyError {
    InvalidEncoding,
    NotOpenSsh,
    Encrypted,
    Unencrypted,
    InvalidKey,
}

impl fmt::Display for SshPrivateKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEncoding => formatter.write_str("SSH private key is not valid UTF-8"),
            Self::NotOpenSsh => formatter.write_str("SSH private key is not in OpenSSH format"),
            Self::Encrypted => formatter
                .write_str("encrypted SSH private keys require an explicit transient passphrase"),
            Self::Unencrypted => formatter
                .write_str("unencrypted SSH private keys must use the unencrypted OpenSSH parser"),
            Self::InvalidKey => formatter.write_str("SSH private key is invalid or unsupported"),
        }
    }
}

impl std::error::Error for SshPrivateKeyError {}

enum WorkerAuthentication {
    Password(String),
    StoredPassword(StoredPasswordAuthentication),
    PublicKey(Arc<russh::keys::PrivateKey>),
    StoredPrivateKey(StoredPrivateKeyAuthentication),
    Interactive,
}

/// Content-free failures while resolving a native stored SSH password.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredPasswordResolutionError {
    Missing,
    LockedOrUnavailable,
    BackendFailure,
    InvalidPasswordEncoding,
}

impl StoredPasswordResolutionError {
    const fn message(self) -> &'static str {
        match self {
            Self::Missing => {
                "stored SSH password is missing; replace it for this saved profile and try again"
            }
            Self::LockedOrUnavailable => {
                "native secure storage is locked or unavailable; unlock it and try again"
            }
            Self::BackendFailure => {
                "native secure storage could not read the stored SSH password; try again or replace it"
            }
            Self::InvalidPasswordEncoding => {
                "stored SSH password could not be read; replace it for this saved profile and try again"
            }
        }
    }
}

impl fmt::Display for StoredPasswordResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for StoredPasswordResolutionError {}

fn resolve_stored_password(
    authentication: &StoredPasswordAuthentication,
) -> Result<String, StoredPasswordResolutionError> {
    let secret = authentication
        .store
        .get(&authentication.reference)
        .map_err(|error| match error {
            SecretStoreError::Missing => StoredPasswordResolutionError::Missing,
            SecretStoreError::LockedOrUnavailable | SecretStoreError::Unsupported => {
                StoredPasswordResolutionError::LockedOrUnavailable
            }
            SecretStoreError::BackendFailure | SecretStoreError::InvalidReference => {
                StoredPasswordResolutionError::BackendFailure
            }
        })?;
    secret.with_bytes(|bytes| {
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| StoredPasswordResolutionError::InvalidPasswordEncoding)
    })
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

    /// A conservative default bounded-exponential backoff for automatic
    /// recovery, offered so a user opting a persistent session into
    /// automatic recovery (ADR 0018 requires that opt-in itself) is not also
    /// required to choose numeric backoff parameters. This intentionally
    /// matches the cadence fesTerm already uses when retrying a
    /// user-initiated manual reconnect after a transient failure.
    pub fn default_automatic() -> Self {
        Self::new(
            MANUAL_RECONNECT_MAX_ATTEMPTS,
            MANUAL_RECONNECT_INITIAL_DELAY,
            MANUAL_RECONNECT_MAX_DELAY,
        )
        .expect("the default automatic-recovery policy's fixed parameters are valid")
    }
}

/// A remote durable-session provider fesTerm can attach to or create for a
/// [`SessionStrategy::Persistent`] session (ADR 0018).
///
/// A provider's contract is deliberately small: report a user-displayable
/// name, provide the command used to lazily and explicitly probe for its
/// remote executable, and construct the command that attaches to (or
/// creates, if absent) the durable session in place of an interactive login
/// shell. There is no public plugin system; this enum is the whole boundary
/// until multiple real implementations justify more abstraction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistenceProvider {
    Tmux,
    Screen,
}

impl PersistenceProvider {
    /// A short, user-displayable name for this provider.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Screen => "GNU Screen",
        }
    }

    /// The remote command used to lazily and explicitly probe for this
    /// provider's executable. fesTerm never runs this speculatively or in
    /// the background; it only runs when a user opts a profile into
    /// persistence (ADR 0018).
    pub const fn capability_probe_command(self) -> &'static str {
        match self {
            Self::Tmux => "command -v tmux",
            Self::Screen => "command -v screen",
        }
    }

    /// The remote command fesTerm execs, in place of an interactive login
    /// shell, to attach to the durable session named by `session_name`, or
    /// create it if it does not yet exist.
    ///
    /// Both commands are idempotent/re-entrant: running them again after an
    /// unintentional transport loss reattaches to the same durable session
    /// rather than creating a second one, which is what makes this strategy
    /// safe to pair with [`RecoveryPolicy::Automatic`].
    fn attach_or_create_command(
        self,
        session_name: &PersistentSessionName,
        initial_size: TerminalSize,
    ) -> String {
        match self {
            Self::Tmux => format!(
                "exec tmux new-session -A -s {} -x {} -y {} \\; set-option -t {} status off \
                 \\; set-window-option -t {} window-size latest",
                session_name.as_str(),
                initial_size.columns(),
                initial_size.rows(),
                session_name.as_str(),
                session_name.as_str(),
            ),
            Self::Screen => format!("exec screen -c /dev/null -xRR {}", session_name.as_str()),
        }
    }
}

/// A validated remote durable-session name for a [`PersistenceProvider`].
///
/// This is deliberately restricted to a conservative, shell-metacharacter-free
/// character set: the name is interpolated directly into a remote command
/// string (see [`PersistenceProvider::attach_or_create_command`]), and
/// fesTerm has no reliable, portable way to shell-quote a value for whatever
/// login shell the remote host happens to run. Restricting the input
/// character set by construction is simpler and safer than trying to escape
/// it after the fact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistentSessionName(String);

impl PersistentSessionName {
    const MAXIMUM_BYTES: usize = 64;

    /// Validates and wraps a candidate durable-session name.
    pub fn new(name: impl Into<String>) -> Result<Self, PersistentSessionNameError> {
        let name = name.into();
        if name.is_empty() {
            return Err(PersistentSessionNameError::Empty);
        }
        if name.len() > Self::MAXIMUM_BYTES {
            return Err(PersistentSessionNameError::TooLong {
                maximum: Self::MAXIMUM_BYTES,
                actual: name.len(),
            });
        }
        if !name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        }) {
            return Err(PersistentSessionNameError::InvalidCharacter);
        }
        Ok(Self(name))
    }

    /// Returns the validated name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A rejected candidate [`PersistentSessionName`].
///
/// This error intentionally contains no part of the rejected name itself, so
/// applications can present it safely even though the name may have come
/// from untrusted input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistentSessionNameError {
    Empty,
    TooLong { maximum: usize, actual: usize },
    InvalidCharacter,
}

impl fmt::Display for PersistentSessionNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("a persistent session name must not be empty"),
            Self::TooLong { maximum, actual } => write!(
                formatter,
                "a persistent session name must be at most {maximum} bytes, got {actual}"
            ),
            Self::InvalidCharacter => formatter.write_str(
                "a persistent session name may only contain ASCII letters, digits, '-', '_', or '.'",
            ),
        }
    }
}

impl std::error::Error for PersistentSessionNameError {}

/// The kind of remote session fesTerm creates or attaches to for a live SSH
/// connection (ADR 0018).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum SessionStrategy {
    /// An ordinary remote shell with no durable-session provider attached.
    /// Losing the transport always loses that shell, so there is nothing to
    /// safely recover without an explicit user action.
    #[default]
    PlainShell,
    /// A durable remote session created by, or attached to via, `provider`
    /// and identified by `session_name`. Losing the transport does not lose
    /// this remote session: reattaching (manually, or, once explicitly
    /// opted in, automatically) recovers it (ADR 0018).
    Persistent {
        provider: PersistenceProvider,
        session_name: PersistentSessionName,
    },
}

impl SessionStrategy {
    /// Whether this strategy can safely recover durable remote state after an
    /// unintentional transport loss, and so may be paired with
    /// [`RecoveryPolicy::Automatic`] (ADR 0018).
    ///
    /// A plain shell cannot: it is not attached to anything but the dead
    /// transport, so an automatic reconnect could only create a new,
    /// unrelated shell rather than recover the old one. A persistent
    /// strategy's whole point is that reattaching is safe and idempotent.
    pub const fn supports_automatic_recovery(&self) -> bool {
        match self {
            Self::PlainShell => false,
            Self::Persistent { .. } => true,
        }
    }
}

/// Whether a live SSH session may replace its transport without an explicit
/// user action after an unintentional connection loss (ADR 0018).
///
/// A user-requested manual reconnect is always available regardless of this
/// policy; it governs only *unintentional*-loss retries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecoveryPolicy {
    /// The only reconnect that happens is an explicit, user-requested one.
    #[default]
    Manual,
    /// An unintentional transport loss may schedule bounded, cancellable
    /// reconnect attempts using the given policy. Only valid for a
    /// [`SessionStrategy`] that reports
    /// [`SessionStrategy::supports_automatic_recovery`].
    Automatic(ReconnectPolicy),
}

/// A [`RecoveryPolicy::Automatic`] policy paired with a [`SessionStrategy`]
/// that cannot safely support it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryPolicyError;

impl fmt::Display for RecoveryPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "automatic recovery requires a session strategy that can safely recover durable state",
        )
    }
}

impl std::error::Error for RecoveryPolicyError {}

/// Explicit optional behavior for a live [`SshSession`].
///
/// A user-requested manual reconnect is always available regardless of these
/// options (ADR 0018): only automatic, unintentional-loss retry is governed
/// here, and only a [`SessionStrategy`] that can safely recover durable state
/// may enable it. Every reconnect, manual or automatic, is a new SSH
/// transport, channel, PTY, and shell; remote process state and unsent or
/// in-flight input are never restored.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SshSessionOptions {
    strategy: SessionStrategy,
    recovery: RecoveryPolicy,
    known_host_fingerprint: Option<String>,
}

impl SshSessionOptions {
    /// Creates options for an ordinary plain-shell session with manual-only
    /// recovery — the only combination valid before ADR 0018's persistent
    /// strategies existed, and still the default today.
    pub const fn new() -> Self {
        Self {
            strategy: SessionStrategy::PlainShell,
            recovery: RecoveryPolicy::Manual,
            known_host_fingerprint: None,
        }
    }

    /// Builds options from an explicit strategy/recovery pair, rejecting an
    /// automatic policy the strategy cannot safely support (ADR 0018).
    pub fn with_recovery_policy(
        strategy: SessionStrategy,
        recovery: RecoveryPolicy,
    ) -> Result<Self, RecoveryPolicyError> {
        if matches!(recovery, RecoveryPolicy::Automatic(_))
            && !strategy.supports_automatic_recovery()
        {
            return Err(RecoveryPolicyError);
        }
        Ok(Self {
            strategy,
            recovery,
            known_host_fingerprint: None,
        })
    }

    /// Attaches a persistent-trust-store fingerprint the application already
    /// has on file for this destination (ADR 0020). When the server presents
    /// this exact fingerprint, the worker accepts it without prompting,
    /// mirroring `ssh`'s own already-in-`known_hosts` behavior; when it
    /// presents any other fingerprint, the worker still prompts, but flags
    /// the request as a changed-key warning rather than a first-seen host.
    #[must_use]
    pub fn with_known_host_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.known_host_fingerprint = Some(fingerprint.into());
        self
    }

    /// Returns the session strategy in effect.
    pub fn strategy(&self) -> SessionStrategy {
        self.strategy.clone()
    }

    /// Returns the selected automatic reconnect policy, if automatic
    /// recovery is on. `None` means every reconnect is manual/explicit.
    pub const fn reconnect_policy(&self) -> Option<ReconnectPolicy> {
        match self.recovery {
            RecoveryPolicy::Automatic(policy) => Some(policy),
            RecoveryPolicy::Manual => None,
        }
    }

    /// Returns the persistent-trust-store fingerprint attached to these
    /// options, if any (ADR 0020).
    pub fn known_host_fingerprint(&self) -> Option<&str> {
        self.known_host_fingerprint.as_deref()
    }

    /// Builds options for `strategy` with manual-only recovery.
    ///
    /// Manual recovery is always valid for any [`SessionStrategy`] (ADR
    /// 0018: a persistent-session strategy makes automatic recovery *safe
    /// enough to offer*, it does not enable it by itself), so this
    /// constructor is infallible, unlike [`Self::with_recovery_policy`].
    /// Callers that resolve a strategy from saved profile metadata without
    /// yet exposing an automatic-recovery opt-in should use this.
    pub const fn manual_recovery(strategy: SessionStrategy) -> Self {
        Self {
            strategy,
            recovery: RecoveryPolicy::Manual,
            known_host_fingerprint: None,
        }
    }
}

/// A rejected nonblocking request to reconnect a live SSH session.
///
/// This error intentionally contains no destination, credential, terminal,
/// or protocol data, so applications can present it safely.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshReconnectError {
    NotRunning,
    AlreadyRequested,
    QueueFull,
    Closed,
}

impl fmt::Display for SshReconnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning => {
                formatter.write_str("SSH reconnect is only available while connected")
            }
            Self::AlreadyRequested => formatter.write_str("an SSH reconnect is already pending"),
            Self::QueueFull => formatter.write_str("SSH reconnect request queue is full"),
            Self::Closed => {
                formatter.write_str("SSH reconnect request was rejected: session closed")
            }
        }
    }
}

impl std::error::Error for SshReconnectError {}

/// A rejected nonblocking request to actively verify a live SSH session's
/// transport (ADR 0018's liveness probe).
///
/// Like [`SshReconnectError`], this intentionally contains no destination,
/// credential, terminal, or protocol data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshLivenessCheckError {
    NotRunning,
    AlreadyRequested,
}

impl fmt::Display for SshLivenessCheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning => {
                formatter.write_str("an SSH liveness probe is only available while connected")
            }
            Self::AlreadyRequested => {
                formatter.write_str("an SSH liveness probe is already pending")
            }
        }
    }
}

impl std::error::Error for SshLivenessCheckError {}

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

/// The result of one deterministic reconnect-planning transition.
///
/// This planner never starts a timer or a network operation. Its caller owns
/// waiting for [`Self::ScheduleAttempt`] and creating a new session for
/// [`Self::StartFreshConnection`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconnectAction {
    None,
    ScheduleAttempt {
        attempt: u8,
        delay: Duration,
    },
    StartFreshConnection {
        attempt: u8,
        host_verification: FreshHostVerification,
    },
    Exhausted,
    Cancelled,
    Reset,
}

/// Host-trust work required before every reconnect attempt.
///
/// A reconnect is always a new transport, PTY, and remote shell. This type
/// deliberately provides no state-restoration action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FreshHostVerification {
    Required,
}

/// Observable state of a [`ReconnectPlanner`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconnectState {
    Idle,
    Waiting { attempt: u8, delay: Duration },
    Connecting { attempt: u8 },
    Exhausted,
    Cancelled,
}

/// Pure, bounded planner for application-owned SSH reconnect behavior.
///
/// The application reports disconnects, elapsed waits, and connection results
/// to this type. It must perform each returned action itself; in particular,
/// every [`ReconnectAction::StartFreshConnection`] requires a fresh host-key
/// verification and creates no claim that a prior remote shell was restored.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconnectPlanner {
    policy: ReconnectPolicy,
    state: ReconnectState,
}

impl ReconnectPlanner {
    pub const fn new(policy: ReconnectPolicy) -> Self {
        Self {
            policy,
            state: ReconnectState::Idle,
        }
    }

    pub const fn state(&self) -> ReconnectState {
        self.state
    }

    /// Schedules the first bounded reconnect attempt after a disconnect.
    pub fn disconnected(&mut self) -> ReconnectAction {
        if !matches!(self.state, ReconnectState::Idle) {
            return ReconnectAction::None;
        }
        self.schedule_attempt(1)
    }

    /// Advances a caller-owned elapsed reconnect delay.
    pub fn delay_elapsed(&mut self) -> ReconnectAction {
        let ReconnectState::Waiting { attempt, .. } = self.state else {
            return ReconnectAction::None;
        };
        self.state = ReconnectState::Connecting { attempt };
        ReconnectAction::StartFreshConnection {
            attempt,
            host_verification: FreshHostVerification::Required,
        }
    }

    /// Records that the fresh connection attempt did not establish a session.
    pub fn connection_failed(&mut self) -> ReconnectAction {
        let ReconnectState::Connecting { attempt } = self.state else {
            return ReconnectAction::None;
        };
        if attempt >= self.policy.maximum_attempts {
            self.state = ReconnectState::Exhausted;
            ReconnectAction::Exhausted
        } else {
            self.schedule_attempt(attempt.saturating_add(1))
        }
    }

    /// Stops planning after a fresh connection has established.
    pub fn connection_established(&mut self) -> ReconnectAction {
        if !matches!(self.state, ReconnectState::Connecting { .. }) {
            return ReconnectAction::None;
        }
        self.state = ReconnectState::Idle;
        ReconnectAction::None
    }

    /// Cancels a pending wait or connection attempt without scheduling more.
    pub fn cancel(&mut self) -> ReconnectAction {
        if matches!(
            self.state,
            ReconnectState::Waiting { .. } | ReconnectState::Connecting { .. }
        ) {
            self.state = ReconnectState::Cancelled;
            ReconnectAction::Cancelled
        } else {
            ReconnectAction::None
        }
    }

    /// Clears cancellation or exhaustion before a user-directed new cycle.
    pub fn reset(&mut self) -> ReconnectAction {
        if matches!(self.state, ReconnectState::Idle) {
            return ReconnectAction::None;
        }
        self.state = ReconnectState::Idle;
        ReconnectAction::Reset
    }

    fn schedule_attempt(&mut self, attempt: u8) -> ReconnectAction {
        let delay = self.delay_for_attempt(attempt);
        self.state = ReconnectState::Waiting { attempt, delay };
        ReconnectAction::ScheduleAttempt { attempt, delay }
    }

    fn delay_for_attempt(&self, attempt: u8) -> Duration {
        let mut delay = self.policy.initial_delay;
        for _ in 1..attempt {
            if delay >= self.policy.maximum_delay {
                return self.policy.maximum_delay;
            }
            delay = delay
                .checked_mul(2)
                .unwrap_or(self.policy.maximum_delay)
                .min(self.policy.maximum_delay);
        }
        delay
    }
}

const MAX_OPENSSH_CONFIG_BYTES: usize = 128 * 1024;
const MAX_OPENSSH_CONFIG_LINES: usize = 2_048;
const MAX_OPENSSH_CONFIG_LINE_BYTES: usize = 4 * 1024;
const MAX_OPENSSH_CONFIG_TOKENS: usize = 16;
const MAX_IMPORTED_SSH_PROFILES: usize = 256;
const MAX_OPENSSH_IMPORT_DIAGNOSTICS: usize = 256;
const DEFAULT_SSH_PORT: u16 = 22;
const MAX_IDENTITY_FILE_METADATA_BYTES: usize = 4 * 1024;

/// A secret-free `IdentityFile` path copied literally from OpenSSH metadata.
///
/// The importer neither expands this string nor reads the referenced file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentityFileMetadata {
    path: String,
}

impl IdentityFileMetadata {
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// Secret-free connection metadata imported from one exact OpenSSH `Host`.
///
/// This is not a persisted profile and does not contain authentication
/// material. An [`IdentityFileMetadata`] is only an uninterpreted path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedSshProfile {
    host_alias: String,
    identity: HostIdentity,
    username: Option<String>,
    identity_file: Option<IdentityFileMetadata>,
}

impl ImportedSshProfile {
    pub fn host_alias(&self) -> &str {
        &self.host_alias
    }

    pub fn identity(&self) -> &HostIdentity {
        &self.identity
    }

    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }

    pub fn identity_file(&self) -> Option<&IdentityFileMetadata> {
        self.identity_file.as_ref()
    }
}

/// Severity of a safe OpenSSH-import diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenSshConfigDiagnosticSeverity {
    Warning,
    Error,
}

/// A structured reason why an OpenSSH setting was not imported.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenSshConfigDiagnosticKind {
    ConfigTooLarge { maximum: usize, actual: usize },
    TooManyLines { maximum: usize },
    LineTooLong { maximum: usize },
    TooManyTokens { maximum: usize },
    UnterminatedQuote,
    InvalidDirectiveSyntax,
    UnsupportedDirective { directive: String },
    DirectiveOutsideHost { directive: String },
    DirectiveInUnsupportedMatch { directive: String },
    MultipleHostPatterns,
    NegatedHostPattern,
    WildcardHostPattern,
    InvalidHostAlias,
    DuplicateHostAlias { alias: String },
    DuplicateDirective { directive: String },
    InvalidValue { directive: String },
    TooManyProfiles { maximum: usize },
    DiagnosticLimitReached { maximum: usize },
}

impl fmt::Display for OpenSshConfigDiagnosticKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigTooLarge { maximum, actual } => {
                write!(formatter, "config is {actual} bytes; maximum is {maximum}")
            }
            Self::TooManyLines { maximum } => {
                write!(formatter, "config exceeds the maximum of {maximum} lines")
            }
            Self::LineTooLong { maximum } => {
                write!(
                    formatter,
                    "config line exceeds the maximum of {maximum} bytes"
                )
            }
            Self::TooManyTokens { maximum } => {
                write!(
                    formatter,
                    "config line exceeds the maximum of {maximum} tokens"
                )
            }
            Self::UnterminatedQuote => formatter.write_str("config line has an unterminated quote"),
            Self::InvalidDirectiveSyntax => formatter.write_str("config line has invalid syntax"),
            Self::UnsupportedDirective { directive } => {
                write!(formatter, "unsupported OpenSSH directive {directive}")
            }
            Self::DirectiveOutsideHost { directive } => {
                write!(
                    formatter,
                    "OpenSSH directive {directive} appears outside an exact Host"
                )
            }
            Self::DirectiveInUnsupportedMatch { directive } => {
                write!(
                    formatter,
                    "OpenSSH directive {directive} appears in an unsupported Match section"
                )
            }
            Self::MultipleHostPatterns => {
                formatter.write_str("Host must contain exactly one exact alias")
            }
            Self::NegatedHostPattern => {
                formatter.write_str("negated Host patterns are not supported")
            }
            Self::WildcardHostPattern => {
                formatter.write_str("wildcard Host patterns are not supported")
            }
            Self::InvalidHostAlias => formatter.write_str("Host alias is not a simple exact alias"),
            Self::DuplicateHostAlias { alias } => {
                write!(
                    formatter,
                    "Host alias {alias} is ambiguous because it is duplicated"
                )
            }
            Self::DuplicateDirective { directive } => {
                write!(formatter, "OpenSSH directive {directive} is duplicated")
            }
            Self::InvalidValue { directive } => {
                write!(
                    formatter,
                    "OpenSSH directive {directive} has an invalid value"
                )
            }
            Self::TooManyProfiles { maximum } => {
                write!(
                    formatter,
                    "config exceeds the maximum of {maximum} imported profiles"
                )
            }
            Self::DiagnosticLimitReached { maximum } => {
                write!(
                    formatter,
                    "config diagnostics reached the maximum of {maximum}"
                )
            }
        }
    }
}

/// One source-positioned OpenSSH import diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenSshConfigDiagnostic {
    line: usize,
    severity: OpenSshConfigDiagnosticSeverity,
    kind: OpenSshConfigDiagnosticKind,
}

impl OpenSshConfigDiagnostic {
    pub const fn line(&self) -> usize {
        self.line
    }

    pub const fn severity(&self) -> OpenSshConfigDiagnosticSeverity {
        self.severity
    }

    pub fn kind(&self) -> &OpenSshConfigDiagnosticKind {
        &self.kind
    }
}

/// Bounded result of [`import_openssh_config`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenSshConfigImportReport {
    profiles: Vec<ImportedSshProfile>,
    diagnostics: Vec<OpenSshConfigDiagnostic>,
}

impl OpenSshConfigImportReport {
    pub fn profiles(&self) -> &[ImportedSshProfile] {
        &self.profiles
    }

    pub fn diagnostics(&self) -> &[OpenSshConfigDiagnostic] {
        &self.diagnostics
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == OpenSshConfigDiagnosticSeverity::Error)
    }
}

#[derive(Default)]
struct OpenSshHostBlock {
    alias: String,
    line: usize,
    hostname: Option<String>,
    port: Option<u16>,
    username: Option<String>,
    identity_file: Option<IdentityFileMetadata>,
    valid: bool,
}

impl OpenSshHostBlock {
    fn new(alias: String, line: usize) -> Self {
        Self {
            alias,
            line,
            valid: true,
            ..Self::default()
        }
    }
}

/// Imports the intentionally small, non-executing OpenSSH-config subset.
///
/// Only `Host`, `HostName`, `Port`, `User`, and `IdentityFile` are considered.
/// The parser does not expand shells, environment variables, or `~`; it does
/// not process `Include`, run commands, or open identity files. Any ambiguous,
/// unsupported, or unsafe directive is reported and never applied.
pub fn import_openssh_config(config: &str) -> OpenSshConfigImportReport {
    let mut diagnostics = Vec::new();
    if config.len() > MAX_OPENSSH_CONFIG_BYTES {
        push_openssh_diagnostic(
            &mut diagnostics,
            1,
            OpenSshConfigDiagnosticKind::ConfigTooLarge {
                maximum: MAX_OPENSSH_CONFIG_BYTES,
                actual: config.len(),
            },
        );
        return OpenSshConfigImportReport {
            profiles: Vec::new(),
            diagnostics,
        };
    }

    let mut blocks = Vec::new();
    let mut current_block = None;
    let mut configuration_incomplete = false;
    let mut in_unsupported_match = false;
    for (index, line) in config.split('\n').enumerate() {
        let line_number = index.saturating_add(1);
        if line_number > MAX_OPENSSH_CONFIG_LINES {
            push_openssh_diagnostic(
                &mut diagnostics,
                line_number,
                OpenSshConfigDiagnosticKind::TooManyLines {
                    maximum: MAX_OPENSSH_CONFIG_LINES,
                },
            );
            configuration_incomplete = true;
            break;
        }
        if line.len() > MAX_OPENSSH_CONFIG_LINE_BYTES {
            push_openssh_diagnostic(
                &mut diagnostics,
                line_number,
                OpenSshConfigDiagnosticKind::LineTooLong {
                    maximum: MAX_OPENSSH_CONFIG_LINE_BYTES,
                },
            );
            configuration_incomplete = true;
            continue;
        }
        let tokens = match tokenize_openssh_config_line(line) {
            Ok(tokens) => tokens,
            Err(kind) => {
                push_openssh_diagnostic(&mut diagnostics, line_number, kind);
                configuration_incomplete = true;
                continue;
            }
        };
        let Some((directive, values)) = split_openssh_directive(tokens) else {
            continue;
        };

        if directive == "host" {
            if let Some(block) = current_block.take() {
                blocks.push(block);
            }
            in_unsupported_match = false;
            match parse_exact_host_alias(&values) {
                Ok(alias) => current_block = Some(OpenSshHostBlock::new(alias, line_number)),
                Err(kind) => push_openssh_diagnostic(&mut diagnostics, line_number, kind),
            }
            continue;
        }

        if directive == "match" {
            if let Some(block) = current_block.take() {
                blocks.push(block);
            }
            push_openssh_diagnostic(
                &mut diagnostics,
                line_number,
                OpenSshConfigDiagnosticKind::UnsupportedDirective { directive },
            );
            configuration_incomplete = true;
            in_unsupported_match = true;
            continue;
        }

        if in_unsupported_match {
            push_openssh_diagnostic(
                &mut diagnostics,
                line_number,
                OpenSshConfigDiagnosticKind::DirectiveInUnsupportedMatch { directive },
            );
            configuration_incomplete = true;
            continue;
        }

        if directive == "include" && current_block.is_none() {
            push_openssh_diagnostic(
                &mut diagnostics,
                line_number,
                OpenSshConfigDiagnosticKind::UnsupportedDirective { directive },
            );
            configuration_incomplete = true;
            continue;
        }

        let Some(block) = current_block.as_mut() else {
            push_openssh_diagnostic(
                &mut diagnostics,
                line_number,
                OpenSshConfigDiagnosticKind::DirectiveOutsideHost { directive },
            );
            configuration_incomplete = true;
            continue;
        };
        apply_openssh_host_directive(block, line_number, directive, values, &mut diagnostics);
    }
    if let Some(block) = current_block {
        blocks.push(block);
    }

    if configuration_incomplete {
        return OpenSshConfigImportReport {
            profiles: Vec::new(),
            diagnostics,
        };
    }
    build_imported_profiles(blocks, &mut diagnostics)
}

fn tokenize_openssh_config_line(line: &str) -> Result<Vec<String>, OpenSshConfigDiagnosticKind> {
    let mut tokens = Vec::new();
    let mut value = String::new();
    let mut quote = None;
    let mut token_started = false;
    let mut preceded_by_whitespace = true;

    for character in line.chars() {
        if let Some(quote_character) = quote {
            if character == quote_character {
                quote = None;
            } else {
                value.push(character);
            }
            token_started = true;
            preceded_by_whitespace = false;
            continue;
        }
        match character {
            '"' | '\'' => {
                quote = Some(character);
                token_started = true;
                preceded_by_whitespace = false;
            }
            '#' if preceded_by_whitespace => break,
            character if character.is_whitespace() => {
                if token_started {
                    push_openssh_token(&mut tokens, &mut value)?;
                    token_started = false;
                }
                preceded_by_whitespace = true;
            }
            _ => {
                value.push(character);
                token_started = true;
                preceded_by_whitespace = false;
            }
        }
    }
    if quote.is_some() {
        return Err(OpenSshConfigDiagnosticKind::UnterminatedQuote);
    }
    if token_started {
        push_openssh_token(&mut tokens, &mut value)?;
    }
    Ok(tokens)
}

fn push_openssh_token(
    tokens: &mut Vec<String>,
    value: &mut String,
) -> Result<(), OpenSshConfigDiagnosticKind> {
    if tokens.len() == MAX_OPENSSH_CONFIG_TOKENS {
        return Err(OpenSshConfigDiagnosticKind::TooManyTokens {
            maximum: MAX_OPENSSH_CONFIG_TOKENS,
        });
    }
    tokens.push(std::mem::take(value));
    Ok(())
}

fn split_openssh_directive(mut tokens: Vec<String>) -> Option<(String, Vec<String>)> {
    let first = tokens.first_mut()?;
    if let Some((directive, inline_value)) = first.split_once('=') {
        let directive = directive.to_ascii_lowercase();
        let inline_value = inline_value.to_owned();
        if inline_value.is_empty() {
            tokens.remove(0);
        } else {
            *first = inline_value;
        }
        Some((directive, tokens))
    } else {
        let directive = first.to_ascii_lowercase();
        tokens.remove(0);
        Some((directive, tokens))
    }
}

fn parse_exact_host_alias(values: &[String]) -> Result<String, OpenSshConfigDiagnosticKind> {
    if values.len() != 1 {
        return Err(OpenSshConfigDiagnosticKind::MultipleHostPatterns);
    }
    let alias = &values[0];
    if alias.starts_with('!') {
        return Err(OpenSshConfigDiagnosticKind::NegatedHostPattern);
    }
    if alias.contains(['*', '?', '[', ']']) {
        return Err(OpenSshConfigDiagnosticKind::WildcardHostPattern);
    }
    if !is_simple_host_alias(alias) {
        return Err(OpenSshConfigDiagnosticKind::InvalidHostAlias);
    }
    Ok(alias.to_ascii_lowercase())
}

fn is_simple_host_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn apply_openssh_host_directive(
    block: &mut OpenSshHostBlock,
    line: usize,
    directive: String,
    values: Vec<String>,
    diagnostics: &mut Vec<OpenSshConfigDiagnostic>,
) {
    if directive == "include" {
        block.valid = false;
        push_openssh_diagnostic(
            diagnostics,
            line,
            OpenSshConfigDiagnosticKind::UnsupportedDirective { directive },
        );
        return;
    }
    if !matches!(
        directive.as_str(),
        "hostname" | "port" | "user" | "identityfile"
    ) {
        block.valid = false;
        push_openssh_diagnostic(
            diagnostics,
            line,
            OpenSshConfigDiagnosticKind::UnsupportedDirective { directive },
        );
        return;
    }
    if values.len() != 1 {
        block.valid = false;
        push_openssh_diagnostic(
            diagnostics,
            line,
            OpenSshConfigDiagnosticKind::InvalidDirectiveSyntax,
        );
        return;
    }

    let value = &values[0];
    let duplicate = match directive.as_str() {
        "hostname" => block.hostname.is_some(),
        "port" => block.port.is_some(),
        "user" => block.username.is_some(),
        "identityfile" => block.identity_file.is_some(),
        _ => unreachable!("supported directive was checked above"),
    };
    if duplicate {
        block.valid = false;
        push_openssh_diagnostic(
            diagnostics,
            line,
            OpenSshConfigDiagnosticKind::DuplicateDirective { directive },
        );
        return;
    }

    match directive.as_str() {
        "hostname" if HostIdentity::new(value, DEFAULT_SSH_PORT).is_ok() => {
            block.hostname = Some(value.clone());
        }
        "port" => match value.parse::<u16>() {
            Ok(port) if port != 0 => block.port = Some(port),
            Ok(_) | Err(_) => {
                block.valid = false;
                push_openssh_diagnostic(
                    diagnostics,
                    line,
                    OpenSshConfigDiagnosticKind::InvalidValue { directive },
                );
            }
        },
        "user" if validate_username(value).is_ok() => block.username = Some(value.clone()),
        "identityfile"
            if !value.is_empty()
                && value.len() <= MAX_IDENTITY_FILE_METADATA_BYTES
                && !value.chars().any(char::is_control) =>
        {
            block.identity_file = Some(IdentityFileMetadata {
                path: value.clone(),
            });
        }
        _ => {
            block.valid = false;
            push_openssh_diagnostic(
                diagnostics,
                line,
                OpenSshConfigDiagnosticKind::InvalidValue { directive },
            );
        }
    }
}

fn build_imported_profiles(
    blocks: Vec<OpenSshHostBlock>,
    diagnostics: &mut Vec<OpenSshConfigDiagnostic>,
) -> OpenSshConfigImportReport {
    let mut duplicate_aliases = std::collections::BTreeSet::new();
    let mut seen_aliases = std::collections::BTreeSet::new();
    for block in &blocks {
        if !seen_aliases.insert(block.alias.clone()) {
            duplicate_aliases.insert(block.alias.clone());
        }
    }
    for block in &blocks {
        if duplicate_aliases.contains(&block.alias) {
            push_openssh_diagnostic(
                diagnostics,
                block.line,
                OpenSshConfigDiagnosticKind::DuplicateHostAlias {
                    alias: block.alias.clone(),
                },
            );
        }
    }

    let mut profiles = Vec::new();
    for block in blocks {
        if !block.valid || duplicate_aliases.contains(&block.alias) {
            continue;
        }
        if profiles.len() == MAX_IMPORTED_SSH_PROFILES {
            push_openssh_diagnostic(
                diagnostics,
                block.line,
                OpenSshConfigDiagnosticKind::TooManyProfiles {
                    maximum: MAX_IMPORTED_SSH_PROFILES,
                },
            );
            break;
        }
        let host = block.hostname.unwrap_or_else(|| block.alias.clone());
        let port = block.port.unwrap_or(DEFAULT_SSH_PORT);
        let Ok(identity) = HostIdentity::new(host, port) else {
            push_openssh_diagnostic(
                diagnostics,
                block.line,
                OpenSshConfigDiagnosticKind::InvalidValue {
                    directive: "hostname".to_owned(),
                },
            );
            continue;
        };
        profiles.push(ImportedSshProfile {
            host_alias: block.alias,
            identity,
            username: block.username,
            identity_file: block.identity_file,
        });
    }
    OpenSshConfigImportReport {
        profiles,
        diagnostics: std::mem::take(diagnostics),
    }
}

fn push_openssh_diagnostic(
    diagnostics: &mut Vec<OpenSshConfigDiagnostic>,
    line: usize,
    kind: OpenSshConfigDiagnosticKind,
) {
    if diagnostics.len() < MAX_OPENSSH_IMPORT_DIAGNOSTICS.saturating_sub(1) {
        diagnostics.push(OpenSshConfigDiagnostic {
            line,
            severity: OpenSshConfigDiagnosticSeverity::Error,
            kind,
        });
    } else if diagnostics.len() == MAX_OPENSSH_IMPORT_DIAGNOSTICS.saturating_sub(1) {
        diagnostics.push(OpenSshConfigDiagnostic {
            line,
            severity: OpenSshConfigDiagnosticSeverity::Error,
            kind: OpenSshConfigDiagnosticKind::DiagnosticLimitReached {
                maximum: MAX_OPENSSH_IMPORT_DIAGNOSTICS,
            },
        });
    }
}

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

/// Resolves the single pending interactive password request for one SSH
/// session, mirroring [`HostKeyDecisionResolver`]. This handle contains no
/// password material: the GUI supplies one from its event handler, and only
/// the future network worker awaits and consumes it.
#[derive(Clone)]
pub struct PasswordDecisionResolver {
    gate: Arc<PasswordDecisionGate>,
}

impl PasswordDecisionResolver {
    pub fn resolve(
        &self,
        prompt: &festerm_session::PasswordPrompt,
        password: String,
    ) -> Result<(), PasswordDecisionResolutionError> {
        self.gate.resolve(prompt, password)
    }

    /// Cancels the current prompt. Cancellation always ends the connection
    /// attempt, exactly as closing the tab mid-prompt would.
    pub fn cancel(
        &self,
        prompt: &festerm_session::PasswordPrompt,
    ) -> Result<(), PasswordDecisionResolutionError> {
        self.gate.cancel(prompt)
    }
}

impl fmt::Debug for PasswordDecisionResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PasswordDecisionResolver")
    }
}

/// A rejected or stale attempt to resolve an interactive password prompt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PasswordDecisionResolutionError {
    NoPendingPrompt,
    AlreadyResolved,
    PromptMismatch,
}

impl fmt::Display for PasswordDecisionResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPendingPrompt => formatter.write_str("no password prompt is pending"),
            Self::AlreadyResolved => formatter.write_str("password prompt is already resolved"),
            Self::PromptMismatch => {
                formatter.write_str("password does not match the pending prompt")
            }
        }
    }
}

impl std::error::Error for PasswordDecisionResolutionError {}

#[allow(dead_code)]
enum PasswordGateState {
    Idle,
    Waiting(festerm_session::PasswordPrompt),
    Resolved(String),
    Cancelled,
}

/// Pauses an SSH worker immediately before password authentication and
/// resumes it once the application supplies (or cancels) one, mirroring
/// [`HostKeyDecisionGate`]'s pause-and-resolve pattern.
struct PasswordDecisionGate {
    state: Mutex<PasswordGateState>,
    changed: Condvar,
    notified: tokio::sync::Notify,
}

#[allow(dead_code)]
impl PasswordDecisionGate {
    fn new() -> Self {
        Self {
            state: Mutex::new(PasswordGateState::Idle),
            changed: Condvar::new(),
            notified: tokio::sync::Notify::new(),
        }
    }

    fn begin(
        &self,
        prompt: festerm_session::PasswordPrompt,
    ) -> Result<PasswordDecisionWaiter, PasswordDecisionResolutionError> {
        let mut state = self
            .state
            .lock()
            .expect("password gate lock is not poisoned");
        match *state {
            PasswordGateState::Idle => {
                *state = PasswordGateState::Waiting(prompt.clone());
                Ok(PasswordDecisionWaiter { prompt })
            }
            PasswordGateState::Resolved(_) => Err(PasswordDecisionResolutionError::AlreadyResolved),
            PasswordGateState::Waiting(_) | PasswordGateState::Cancelled => {
                Err(PasswordDecisionResolutionError::NoPendingPrompt)
            }
        }
    }

    fn resolve(
        &self,
        prompt: &festerm_session::PasswordPrompt,
        password: String,
    ) -> Result<(), PasswordDecisionResolutionError> {
        let mut state = self
            .state
            .lock()
            .expect("password gate lock is not poisoned");
        match &*state {
            PasswordGateState::Waiting(current) if current == prompt => {
                *state = PasswordGateState::Resolved(password);
                self.changed.notify_all();
                self.notified.notify_waiters();
                Ok(())
            }
            PasswordGateState::Waiting(_) => Err(PasswordDecisionResolutionError::PromptMismatch),
            PasswordGateState::Resolved(_) => Err(PasswordDecisionResolutionError::AlreadyResolved),
            PasswordGateState::Idle | PasswordGateState::Cancelled => {
                Err(PasswordDecisionResolutionError::NoPendingPrompt)
            }
        }
    }

    fn cancel(
        &self,
        prompt: &festerm_session::PasswordPrompt,
    ) -> Result<(), PasswordDecisionResolutionError> {
        let mut state = self
            .state
            .lock()
            .expect("password gate lock is not poisoned");
        match &*state {
            PasswordGateState::Waiting(current) if current == prompt => {
                *state = PasswordGateState::Cancelled;
                self.changed.notify_all();
                self.notified.notify_waiters();
                Ok(())
            }
            PasswordGateState::Waiting(_) => Err(PasswordDecisionResolutionError::PromptMismatch),
            PasswordGateState::Resolved(_) => Err(PasswordDecisionResolutionError::AlreadyResolved),
            PasswordGateState::Idle | PasswordGateState::Cancelled => {
                Err(PasswordDecisionResolutionError::NoPendingPrompt)
            }
        }
    }

    fn wait_for_decision(&self, timeout: Duration) -> Option<String> {
        let state = self
            .state
            .lock()
            .expect("password gate lock is not poisoned");
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| {
                matches!(state, PasswordGateState::Waiting(_))
            })
            .expect("password gate lock is not poisoned");
        let password = match &mut *state {
            PasswordGateState::Resolved(password) => Some(std::mem::take(password)),
            PasswordGateState::Idle
            | PasswordGateState::Waiting(_)
            | PasswordGateState::Cancelled => None,
        };
        *state = PasswordGateState::Idle;
        self.notified.notify_waiters();
        password
    }

    fn reject_pending(&self) {
        let mut state = self
            .state
            .lock()
            .expect("password gate lock is not poisoned");
        if matches!(*state, PasswordGateState::Waiting(_)) {
            *state = PasswordGateState::Cancelled;
            self.changed.notify_all();
            self.notified.notify_waiters();
        }
    }

    async fn wait_for_decision_async(&self, timeout: Duration) -> Option<String> {
        let password = tokio::time::timeout(timeout, async {
            loop {
                let notified = self.notified.notified();
                {
                    let mut state = self
                        .state
                        .lock()
                        .expect("password gate lock is not poisoned");
                    match &mut *state {
                        PasswordGateState::Resolved(password) => {
                            return Some(std::mem::take(password))
                        }
                        PasswordGateState::Idle | PasswordGateState::Cancelled => return None,
                        PasswordGateState::Waiting(_) => {}
                    }
                }
                notified.await;
            }
        })
        .await
        .unwrap_or(None);
        *self
            .state
            .lock()
            .expect("password gate lock is not poisoned") = PasswordGateState::Idle;
        self.changed.notify_all();
        self.notified.notify_waiters();
        password
    }
}

/// Worker-only proof that a prompt has been emitted and may now be awaited.
#[allow(dead_code)]
struct PasswordDecisionWaiter {
    prompt: festerm_session::PasswordPrompt,
}

#[allow(dead_code)]
impl PasswordDecisionWaiter {
    fn wait(self, gate: &PasswordDecisionGate, timeout: Duration) -> Option<String> {
        gate.wait_for_decision(timeout)
    }

    async fn wait_async(self, gate: &PasswordDecisionGate, timeout: Duration) -> Option<String> {
        gate.wait_for_decision_async(timeout).await
    }
}

enum WorkerCommand {
    Input(Vec<u8>),
    Resize(TerminalSize),
    Reconnect,
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
    desired_terminal_size: Mutex<TerminalSize>,
    pre_running_resize_pending: AtomicBool,
    reconnecting: AtomicBool,
    reconnect_requested: AtomicBool,
    liveness_check_requested: AtomicBool,
    shutdown_requested: AtomicBool,
    metrics: Mutex<SessionMetrics>,
    event_sender: SyncSender<SessionEvent>,
    event_notifier: Arc<dyn SessionEventNotifier>,
}

impl WorkerShared {
    fn desired_terminal_size(&self) -> TerminalSize {
        *self
            .desired_terminal_size
            .lock()
            .expect("SSH desired terminal size lock is not poisoned")
    }

    fn retain_pre_running_resize(&self, size: TerminalSize) {
        *self
            .desired_terminal_size
            .lock()
            .expect("SSH desired terminal size lock is not poisoned") = size;
        self.pre_running_resize_pending
            .store(true, Ordering::Release);
    }

    fn record_running_resize(&self, size: TerminalSize) {
        *self
            .desired_terminal_size
            .lock()
            .expect("SSH desired terminal size lock is not poisoned") = size;
        self.record_resize_applied(size);
    }

    fn take_pre_running_resize(&self) -> Option<TerminalSize> {
        self.pre_running_resize_pending
            .swap(false, Ordering::AcqRel)
            .then(|| self.desired_terminal_size())
    }

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

    fn set_reconnecting(&self, reconnecting: bool) {
        self.reconnecting.store(reconnecting, Ordering::Release);
    }

    fn is_reconnecting(&self) -> bool {
        self.reconnecting.load(Ordering::Acquire)
    }

    fn request_reconnect(&self) -> bool {
        self.reconnect_requested
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn clear_reconnect_request(&self) {
        self.reconnect_requested.store(false, Ordering::Release);
    }

    fn reconnect_requested(&self) -> bool {
        self.reconnect_requested.load(Ordering::Acquire)
    }

    /// Coalesces one pending on-demand liveness-probe request (ADR 0018):
    /// a future wake/network-change hook, or an explicit application
    /// request, sets this without blocking on network I/O; the worker
    /// consumes it via [`Self::take_liveness_check_requested`]. Returns
    /// `false` if a request is already pending.
    fn request_liveness_check(&self) -> bool {
        self.liveness_check_requested
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Consumes a pending on-demand liveness-probe request, if any.
    fn take_liveness_check_requested(&self) -> bool {
        self.liveness_check_requested.swap(false, Ordering::AcqRel)
    }

    fn request_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Release);
    }

    fn shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::Acquire)
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
    password_gate: Arc<PasswordDecisionGate>,
}

impl SshWorkerFoundation {
    #[cfg(test)]
    fn new(
        profile: SshConnectionProfile,
    ) -> (
        Self,
        WorkerCommandReceiver,
        HostKeyDecisionResolver,
        PasswordDecisionResolver,
    ) {
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
    ) -> (
        Self,
        WorkerCommandReceiver,
        HostKeyDecisionResolver,
        PasswordDecisionResolver,
    ) {
        let initial_size = profile.initial_size();
        #[cfg(not(test))]
        let _ = &profile;
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
        let password_gate = Arc::new(PasswordDecisionGate::new());
        let shared = Arc::new(WorkerShared {
            id: SessionId::next(),
            lifecycle: Mutex::new(SessionLifecycle::Starting),
            desired_terminal_size: Mutex::new(initial_size),
            pre_running_resize_pending: AtomicBool::new(false),
            reconnecting: AtomicBool::new(false),
            reconnect_requested: AtomicBool::new(false),
            liveness_check_requested: AtomicBool::new(false),
            shutdown_requested: AtomicBool::new(false),
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
                password_gate: Arc::clone(&password_gate),
            },
            WorkerCommandReceiver {
                receiver: command_receiver,
            },
            HostKeyDecisionResolver {
                gate: host_key_gate,
            },
            PasswordDecisionResolver {
                gate: password_gate,
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

    #[cfg(test)]
    fn set_disconnected(&self) {
        self.shared
            .set_lifecycle(SessionLifecycle::Disconnected(SessionError::new(
                SessionErrorKind::Spawn,
                "test transport loss",
            )));
    }

    fn try_send_input(&self, bytes: &[u8]) -> Result<(), SessionSendError> {
        if bytes.len() > MAX_IO_CHUNK_BYTES {
            return Err(SessionSendError::TooLarge {
                operation: SessionOperation::Input,
                maximum: MAX_IO_CHUNK_BYTES,
                actual: bytes.len(),
            });
        }
        if self.shared.is_reconnecting() {
            return Err(SessionSendError::Closed {
                operation: SessionOperation::Input,
            });
        }
        self.try_send_command(
            WorkerCommand::Input(bytes.to_vec()),
            SessionOperation::Input,
        )
    }

    fn try_resize(&self, size: TerminalSize) -> Result<(), SessionSendError> {
        if self.shared.is_reconnecting() {
            return Err(SessionSendError::Closed {
                operation: SessionOperation::Resize,
            });
        }
        self.try_send_command(WorkerCommand::Resize(size), SessionOperation::Resize)
    }

    fn try_shutdown(&self) -> Result<(), SessionSendError> {
        self.shared.request_shutdown();
        self.try_send_command(WorkerCommand::Shutdown, SessionOperation::Shutdown)
    }

    fn reconnect_available(&self) -> bool {
        reconnect_request_is_available(
            &self.lifecycle(),
            self.shared.is_reconnecting() || self.shared.reconnect_requested(),
        )
    }

    fn try_reconnect(&self) -> Result<(), SshReconnectError> {
        // A session remains reconnect-eligible both while still connected
        // (a proactive, user-initiated reconnect) and after an unintentional
        // transport loss has moved it to `Disconnected` (ADR 0018's
        // "disconnected/recovery-eligible state"). Any other lifecycle
        // (Starting, Stopping, Failed for a non-transport reason, Exited,
        // Stopped) is not reconnect-eligible.
        if !matches!(
            self.lifecycle(),
            SessionLifecycle::Running | SessionLifecycle::Disconnected(_)
        ) {
            return Err(SshReconnectError::NotRunning);
        }
        if !self.shared.request_reconnect() {
            return Err(SshReconnectError::AlreadyRequested);
        }
        match self.command_sender.try_send(WorkerCommand::Reconnect) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.shared.clear_reconnect_request();
                Err(SshReconnectError::QueueFull)
            }
            Err(TrySendError::Disconnected(_)) => {
                self.shared.clear_reconnect_request();
                Err(SshReconnectError::Closed)
            }
        }
    }

    /// Coalesces an on-demand liveness-probe request. Unlike
    /// [`Self::try_reconnect`] this never queues a worker command: the
    /// worker itself notices the flag at its next command-poll tick
    /// (typically within [`COMMAND_POLL_INTERVAL`]) and performs the probe,
    /// so there is no queue to fill or close out from under the caller.
    fn try_check_liveness(&self) -> Result<(), SshLivenessCheckError> {
        // Only a live transport has anything to probe; a session that is
        // `Disconnected`/terminal has no handle to send a keepalive over
        // (ADR 0018 distinguishes the liveness probe itself from the
        // recovery that follows a probe failure).
        if !matches!(self.lifecycle(), SessionLifecycle::Running) {
            return Err(SshLivenessCheckError::NotRunning);
        }
        if !self.shared.request_liveness_check() {
            return Err(SshLivenessCheckError::AlreadyRequested);
        }
        Ok(())
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
            None,
        )
    }

    #[cfg(test)]
    fn request_password_verification(
        &self,
        attempt: u8,
        previous_attempt_failed: bool,
    ) -> Result<PasswordDecisionWaiter, PasswordVerificationRequestError> {
        request_password_verification(
            &self.profile,
            &self.shared,
            &self.password_gate,
            attempt,
            previous_attempt_failed,
        )
    }
}

fn request_host_key_verification(
    identity: &HostIdentity,
    shared: &WorkerShared,
    host_key_gate: &HostKeyDecisionGate,
    sha256_fingerprint: &str,
    previously_trusted_fingerprint: Option<&str>,
) -> Result<HostKeyDecisionWaiter, HostKeyVerificationRequestError> {
    if !is_sha256_fingerprint(sha256_fingerprint) {
        return Err(HostKeyVerificationRequestError::InvalidFingerprint);
    }
    let mut prompt =
        festerm_session::HostKeyPrompt::new(identity.host(), identity.port(), sha256_fingerprint);
    if let Some(previous) = previously_trusted_fingerprint {
        prompt = prompt.with_previously_trusted_fingerprint(previous);
    }
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

/// Validates the canonical `SHA256:<base64>` fingerprint format this crate
/// emits and expects, without asserting the fingerprint names a specific
/// key. Exposed so callers that persist trusted fingerprints (outside this
/// crate's own worker/host-key-gate path) can validate them the same way.
pub fn is_sha256_fingerprint(fingerprint: &str) -> bool {
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

/// Requests an interactive password prompt through `password_gate`,
/// mirroring [`request_host_key_verification`]. `attempt` is 1-based;
/// `previous_attempt_failed` lets the UI show `ssh`'s own "Permission
/// denied, please try again." line above a retry.
fn request_password_verification(
    profile: &SshConnectionProfile,
    shared: &WorkerShared,
    password_gate: &PasswordDecisionGate,
    attempt: u8,
    previous_attempt_failed: bool,
) -> Result<PasswordDecisionWaiter, PasswordVerificationRequestError> {
    let prompt = festerm_session::PasswordPrompt::new(
        profile.username(),
        profile.identity.host(),
        attempt,
        previous_attempt_failed,
    );
    let waiter = password_gate
        .begin(prompt.clone())
        .map_err(PasswordVerificationRequestError::Resolution)?;
    if shared.try_emit(SessionEvent::PasswordRequested(prompt.clone())) {
        Ok(waiter)
    } else {
        let _ = password_gate.cancel(&prompt);
        // No waiter escaped on this path, so reset the rejection before a
        // future retry attempt reprompts.
        let _ = password_gate.wait_for_decision(Duration::ZERO);
        Err(PasswordVerificationRequestError::EventQueueFull)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
enum PasswordVerificationRequestError {
    EventQueueFull,
    Resolution(PasswordDecisionResolutionError),
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HOST_KEY_DECISION_TIMEOUT: Duration = Duration::from_secs(30);
/// How long an interactive password prompt waits for the application to
/// resolve it. Generous relative to [`HOST_KEY_DECISION_TIMEOUT`] since
/// typing a password (unlike clicking Accept/Reject) can reasonably take a
/// while.
const PASSWORD_DECISION_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum number of interactive password prompts on one connection before
/// giving up, matching `ssh`'s own default `NumberOfPasswordPrompts`.
pub const MAX_INTERACTIVE_PASSWORD_ATTEMPTS: u8 = 3;
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(10);
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(1);
/// Ordinary SSH liveness/keepalive cadence (ADR 0018): how often a running
/// session actively verifies its transport even without an explicit
/// wake/network-change trigger or on-demand [`SshSession::try_check_liveness`]
/// request.
///
/// This is a deliberate default (issue #55), not an unexamined leftover
/// constant:
/// - It stays enabled by default because network-interface/route-change
///   detection is not yet implemented on any platform (#48), so the
///   automatic cadence is currently the only way a genuinely idle session
///   (no user input, no wake event) ever notices a black-holed connection
///   and moves to `Disconnected`.
/// - 60 seconds balances that detection latency against the network/NAT
///   keepalive traffic and desktop battery cost of a periodic probe; it is
///   in the same range commonly used for interactive SSH client keepalive
///   (`ServerAliveInterval`-style) settings.
/// - It is intentionally a non-configurable implementation constant for
///   0.1 rather than a global/profile setting: per ADR 0018's own
///   constraints, low-level keepalive knobs are not exposed without a
///   concrete product requirement, and a probe failure never grants
///   permission to auto-reconnect regardless of cadence, so the product
///   risk of a fixed default is low. Revisit if #48 lands reliable
///   network-change detection on all platforms, or if a concrete
///   battery/network complaint justifies a setting.
const LIVENESS_PROBE_INTERVAL: Duration = Duration::from_secs(60);
/// Bound on how long a single liveness probe waits for the remote peer to
/// reply before the transport is treated as unresponsive.
const LIVENESS_PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Bounded, cancellable backoff for retries that follow one explicit
/// reconnect request (ADR 0018).
///
/// This is independent of any [`ReconnectPolicy`] the session may have: that
/// policy only governs whether an *unintentional* transport loss retries on
/// its own. A user-requested reconnect is a repeatable action regardless of
/// that policy — if the fresh attempt it triggers only hits a transient
/// transport problem (for example, the host briefly unreachable right after
/// the user asked to reconnect), it keeps retrying with this bounded
/// backoff before giving up and returning to `Disconnected` for another
/// explicit user action, rather than requiring the user to notice the
/// failure and click reconnect again for every transient hiccup.
const MANUAL_RECONNECT_MAX_ATTEMPTS: u8 = 8;
const MANUAL_RECONNECT_INITIAL_DELAY: Duration = Duration::from_millis(500);
const MANUAL_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(4);

/// Doubling backoff delay before the given 1-based manual-reconnect retry
/// attempt, capped at [`MANUAL_RECONNECT_MAX_DELAY`].
fn manual_reconnect_delay(attempt: u8) -> Duration {
    let mut delay = MANUAL_RECONNECT_INITIAL_DELAY;
    for _ in 1..attempt {
        if delay >= MANUAL_RECONNECT_MAX_DELAY {
            return MANUAL_RECONNECT_MAX_DELAY;
        }
        delay = delay
            .checked_mul(2)
            .unwrap_or(MANUAL_RECONNECT_MAX_DELAY)
            .min(MANUAL_RECONNECT_MAX_DELAY);
    }
    delay
}

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
/// The worker performs TCP connection, strict host-key verification, selected
/// transient authentication, and interactive session-channel setup.
pub struct SshSession {
    foundation: SshWorkerFoundation,
    host_key_resolver: HostKeyDecisionResolver,
    host_key_gate: Arc<HostKeyDecisionGate>,
    password_resolver: PasswordDecisionResolver,
    password_gate: Arc<PasswordDecisionGate>,
    completion_receiver: Mutex<Receiver<Result<ShutdownResult, SessionError>>>,
    completion: Mutex<Option<Result<ShutdownResult, SessionError>>>,
}

impl SshSession {
    /// Starts a session with the default no-op event notifier.
    pub fn start(
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
    ) -> Result<Self, SshSessionStartError> {
        Self::start_with_options(profile, authentication, SshSessionOptions::new())
    }

    /// Starts a session with explicit optional live-session behavior.
    pub fn start_with_options(
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
        options: SshSessionOptions,
    ) -> Result<Self, SshSessionStartError> {
        Self::start_with_notifier_and_options(
            profile,
            authentication,
            options,
            noop_session_event_notifier(),
        )
    }

    /// Starts a session and wakes `event_notifier` after every queued event.
    pub fn start_with_notifier(
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
        event_notifier: Arc<dyn SessionEventNotifier>,
    ) -> Result<Self, SshSessionStartError> {
        Self::start_with_notifier_and_options(
            profile,
            authentication,
            SshSessionOptions::new(),
            event_notifier,
        )
    }

    /// Starts a session with an event notifier and explicit live-session behavior.
    pub fn start_with_notifier_and_options(
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
        options: SshSessionOptions,
        event_notifier: Arc<dyn SessionEventNotifier>,
    ) -> Result<Self, SshSessionStartError> {
        let (foundation, command_receiver, host_key_resolver, password_resolver) =
            SshWorkerFoundation::new_with_capacities(
                profile.clone(),
                DEFAULT_COMMAND_QUEUE_CAPACITY,
                DEFAULT_EVENT_QUEUE_CAPACITY,
                event_notifier,
            );
        let shared = Arc::clone(&foundation.shared);
        let host_key_gate = Arc::clone(&foundation.host_key_gate);
        let password_gate = Arc::clone(&foundation.password_gate);
        let worker_host_key_gate = Arc::clone(&host_key_gate);
        let worker_password_gate = Arc::clone(&password_gate);
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
                            options.strategy(),
                            options.reconnect_policy(),
                            options.known_host_fingerprint().map(str::to_owned),
                            shared,
                            command_receiver,
                            worker_host_key_gate,
                            worker_password_gate,
                        ))
                    });
                let _ = completion_sender.send(result);
            })
            .map_err(|_| SshSessionStartError)?;

        Ok(Self {
            foundation,
            host_key_resolver,
            host_key_gate,
            password_resolver,
            password_gate,
            completion_receiver: Mutex::new(completion_receiver),
            completion: Mutex::new(None),
        })
    }

    /// Returns a resolver for the current host-key verification request.
    pub fn host_key_decision_resolver(&self) -> HostKeyDecisionResolver {
        self.host_key_resolver.clone()
    }

    /// Returns a resolver for the current interactive password request.
    pub fn password_decision_resolver(&self) -> PasswordDecisionResolver {
        self.password_resolver.clone()
    }

    /// Returns whether this connected session can accept one reconnect request.
    ///
    /// A manual reconnect is always available for a running session,
    /// independent of any automatic recovery policy (ADR 0018). It does not
    /// block on network I/O.
    pub fn reconnect_available(&self) -> bool {
        self.foundation.reconnect_available()
    }

    /// Asks the worker to replace the current SSH transport with a fresh one.
    ///
    /// This is nonblocking and always honored once requested, independent of
    /// any automatic recovery policy: an explicit reconnect is a deliberate
    /// user action, not an unintentional-loss retry. A successful request
    /// only queues worker work; the normal lifecycle and host-key event paths
    /// report its result. The fresh connection re-verifies host trust and has
    /// a new PTY and shell, with no remote-state restoration.
    pub fn try_reconnect(&self) -> Result<(), SshReconnectError> {
        self.foundation.try_reconnect()
    }

    /// Actively verifies the current SSH transport is still responsive
    /// (ADR 0018's liveness probe), independent of ordinary read/write
    /// activity.
    ///
    /// This is nonblocking: it only requests that the worker perform a
    /// benign SSH-level probe (a keepalive/ping) at its next opportunity,
    /// typically within tens of milliseconds. Intended for callers that
    /// detect a plausible network-assumption change (system wake, network
    /// interface/route change, Wi-Fi reconnect) and want to confirm the
    /// transport promptly rather than wait for the next failed write or the
    /// ordinary keepalive cadence.
    ///
    /// A probe success is silent and leaves the session unchanged. A probe
    /// failure is reported through the same lifecycle path as any other
    /// unintentional transport loss: the session moves to `Disconnected`
    /// (or, if an automatic recovery policy applies, attempts bounded
    /// recovery) exactly as ADR 0018 requires — this method never reconnects
    /// by itself.
    pub fn try_check_liveness(&self) -> Result<(), SshLivenessCheckError> {
        self.foundation.try_check_liveness()
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

fn reconnect_request_is_available(lifecycle: &SessionLifecycle, request_pending: bool) -> bool {
    matches!(
        lifecycle,
        SessionLifecycle::Running | SessionLifecycle::Disconnected(_)
    ) && !request_pending
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
        self.password_gate.reject_pending();
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
        self.password_gate.reject_pending();
        let _ = self.foundation.try_shutdown();
    }
}

struct SshClientHandler {
    identity: HostIdentity,
    shared: Arc<WorkerShared>,
    host_key_gate: Arc<HostKeyDecisionGate>,
    host_key_rejected: Arc<AtomicBool>,
    /// A persistent-trust-store fingerprint already on file for this
    /// destination (ADR 0020), if any. An exact match is accepted silently,
    /// mirroring `ssh`'s own already-in-`known_hosts` behavior; any other
    /// presented key still prompts, flagged as a changed-key warning.
    expected_fingerprint: Option<String>,
}

impl russh::client::Handler for SshClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fingerprint = sha256_fingerprint(server_public_key);
        if self
            .expected_fingerprint
            .as_deref()
            .is_some_and(|expected| expected == fingerprint)
        {
            return Ok(true);
        }
        let previously_trusted = self.expected_fingerprint.as_deref();
        let waiter = match request_host_key_verification(
            &self.identity,
            &self.shared,
            &self.host_key_gate,
            &fingerprint,
            previously_trusted,
        ) {
            Ok(waiter) => waiter,
            Err(_) => {
                self.host_key_rejected.store(true, Ordering::Release);
                return Ok(false);
            }
        };
        let accepted = matches!(
            waiter
                .wait_async(&self.host_key_gate, HOST_KEY_DECISION_TIMEOUT)
                .await,
            HostTrustDecision::AcceptOnce | HostTrustDecision::AcceptAndPersist
        );
        if !accepted {
            self.host_key_rejected.store(true, Ordering::Release);
        }
        Ok(accepted)
    }
}

/// Runs the worker's connection/reconnect loop for one session's lifetime.
/// Bundling `host_key_gate`/`password_gate` into a shared struct isn't worth
/// it here: every other worker helper (`wait_for_ssh_operation`,
/// `process_commands_before_running`, `wait_for_manual_recovery`, ...)
/// already threads `host_key_gate` alone, and this is the only site that
/// additionally needs `password_gate`.
#[allow(clippy::too_many_arguments)]
async fn ssh_worker(
    profile: SshConnectionProfile,
    authentication: SshAuthentication,
    strategy: SessionStrategy,
    reconnect_policy: Option<ReconnectPolicy>,
    known_host_fingerprint: Option<String>,
    shared: Arc<WorkerShared>,
    command_receiver: WorkerCommandReceiver,
    host_key_gate: Arc<HostKeyDecisionGate>,
    password_gate: Arc<PasswordDecisionGate>,
) -> Result<ShutdownResult, SessionError> {
    let authentication = authentication.into_worker_authentication();
    let mut planner = reconnect_policy.map(ReconnectPlanner::new);
    // Counts the *next* `establish_connection` attempt below as the Nth
    // attempt (1-based) directly following an explicit, user-initiated
    // reconnect (as opposed to the session's very first connection attempt,
    // or an automatic-policy attempt); `0` means no manual-reconnect episode
    // is in progress. A manual reconnect that itself fails to transiently
    // reach the host retries with a bounded backoff (see
    // `manual_reconnect_delay`) rather than returning to `Disconnected` or
    // ending the session outright on the very first failure (ADR 0018:
    // explicit reconnect is a repeatable user action, not a one-shot
    // attempt) - first connections keep their existing fail-outright
    // behavior.
    let mut manual_recovery_attempts: u8 = 0;

    loop {
        match establish_connection(
            &profile,
            &authentication,
            &strategy,
            known_host_fingerprint.as_deref(),
            &shared,
            &command_receiver,
            &host_key_gate,
            &password_gate,
        )
        .await
        {
            ConnectionAttempt::Established(handle, channel) => {
                manual_recovery_attempts = 0;
                if let Some(planner) = planner.as_mut() {
                    let _ = planner.connection_established();
                }
                shared.set_reconnecting(false);
                shared.set_lifecycle(SessionLifecycle::Running);
                match run_authenticated_channel(
                    handle,
                    channel,
                    &command_receiver,
                    &shared,
                    &host_key_gate,
                )
                .await
                {
                    RunningOutcome::Shutdown(result) => return Ok(result),
                    RunningOutcome::Exited(exit) => {
                        shared.set_lifecycle(SessionLifecycle::Exited(exit));
                        return Ok(ShutdownResult::AlreadyStopped);
                    }
                    RunningOutcome::ConnectionLost(reason) => {
                        match schedule_reconnect(
                            &mut planner,
                            ConnectionFailure::Transport,
                            &command_receiver,
                            &shared,
                            &host_key_gate,
                        )
                        .await
                        {
                            ReconnectSchedule::Reconnect => continue,
                            ReconnectSchedule::Shutdown => return Ok(ShutdownResult::Stopped),
                            ReconnectSchedule::Unavailable => {}
                        }
                        // No automatic recovery resumed the connection (either
                        // there is no durable-session policy, or its bounded
                        // backoff was exhausted). Per ADR 0018, unintentional
                        // transport loss must never be terminal by itself: the
                        // session moves to a disconnected/recovery-eligible
                        // state and waits here for the user to either request
                        // an explicit reconnect or shut the session down.
                        shared.set_lifecycle(SessionLifecycle::Disconnected(SessionError::new(
                            SessionErrorKind::Spawn,
                            reason,
                        )));
                        match wait_for_manual_recovery(&command_receiver, &shared, &host_key_gate)
                            .await
                        {
                            ManualRecoveryOutcome::Reconnect => {
                                if let Some(planner) = planner.as_mut() {
                                    let _ = planner.connection_established();
                                }
                                manual_recovery_attempts = 1;
                                shared.set_reconnecting(true);
                                shared.clear_reconnect_request();
                                shared.set_lifecycle(SessionLifecycle::Starting);
                                continue;
                            }
                            ManualRecoveryOutcome::Shutdown => {
                                shared.set_lifecycle(SessionLifecycle::Stopped);
                                return Ok(ShutdownResult::Stopped);
                            }
                        }
                    }
                    RunningOutcome::ReconnectRequested => {
                        // A manual reconnect is always honored once explicitly
                        // requested: it is a deliberate user action, not an
                        // unintentional-loss retry, so it does not consult
                        // (and is never blocked by) any automatic recovery
                        // policy (ADR 0018). Best-effort reset the planner's
                        // own bookkeeping so a subsequent unintentional loss
                        // starts its bounded backoff from a clean state.
                        if let Some(planner) = planner.as_mut() {
                            let _ = planner.connection_established();
                        }
                        manual_recovery_attempts = 1;
                        shared.set_reconnecting(true);
                        shared.clear_reconnect_request();
                        shared.set_lifecycle(SessionLifecycle::Starting);
                        continue;
                    }
                }
            }
            ConnectionAttempt::Retryable(failure, message) => {
                match schedule_reconnect(
                    &mut planner,
                    failure,
                    &command_receiver,
                    &shared,
                    &host_key_gate,
                )
                .await
                {
                    ReconnectSchedule::Reconnect => continue,
                    ReconnectSchedule::Shutdown => return Ok(ShutdownResult::Stopped),
                    ReconnectSchedule::Unavailable => {}
                }
                // An explicit reconnect is a repeatable user action (ADR
                // 0018): if the fresh attempt it triggered hit only a
                // transient transport problem (e.g. the host briefly
                // unreachable right after the reconnect was requested),
                // retry with a short bounded backoff before falling back to
                // `Disconnected`, instead of giving up on the first failure.
                if manual_recovery_attempts > 0 && failure == ConnectionFailure::Transport {
                    if manual_recovery_attempts < MANUAL_RECONNECT_MAX_ATTEMPTS {
                        let delay = manual_reconnect_delay(manual_recovery_attempts);
                        manual_recovery_attempts += 1;
                        if wait_for_reconnect_delay(
                            delay,
                            &command_receiver,
                            &shared,
                            &host_key_gate,
                        )
                        .await
                        {
                            shared.set_lifecycle(SessionLifecycle::Stopped);
                            return Ok(ShutdownResult::Stopped);
                        }
                        // Stay in the reconnecting state across retries; the
                        // user only sees `Disconnected` once the whole
                        // bounded retry budget is exhausted below.
                        continue;
                    }
                    shared.set_lifecycle(SessionLifecycle::Disconnected(SessionError::new(
                        SessionErrorKind::Spawn,
                        message,
                    )));
                    match wait_for_manual_recovery(&command_receiver, &shared, &host_key_gate).await
                    {
                        ManualRecoveryOutcome::Reconnect => {
                            manual_recovery_attempts = 1;
                            shared.set_reconnecting(true);
                            shared.clear_reconnect_request();
                            shared.set_lifecycle(SessionLifecycle::Starting);
                            continue;
                        }
                        ManualRecoveryOutcome::Shutdown => {
                            shared.set_lifecycle(SessionLifecycle::Stopped);
                            return Ok(ShutdownResult::Stopped);
                        }
                    }
                }
                return Err(ssh_failure_with_kind(
                    &shared,
                    session_error_kind_for_failure(failure),
                    message,
                ));
            }
            ConnectionAttempt::Permanent(failure, message) => {
                debug_assert!(!reconnect_is_eligible(planner.is_some(), failure));
                return Err(ssh_failure_with_kind(
                    &shared,
                    session_error_kind_for_failure(failure),
                    message,
                ));
            }
            ConnectionAttempt::Shutdown => {
                shared.set_reconnecting(false);
                shared.set_lifecycle(SessionLifecycle::Stopped);
                return Ok(ShutdownResult::Stopped);
            }
        }
    }
}

/// These categories deliberately retry only network loss. Host-trust
/// rejection, authentication failure, and PTY/shell setup failures are
/// security, credential, or configuration outcomes and never retry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionFailure {
    Transport,
    HostTrust,
    Authentication,
    Setup,
    /// A persistent strategy's provider capability probe found the
    /// configured provider missing (or could not be run at all) on the
    /// remote host. This is a stable, non-transient condition: retrying the
    /// same transport will not make an absent executable appear, so
    /// [`reconnect_is_eligible`] never retries it (ADR 0018, Issue #49).
    ProviderUnavailable,
}

fn reconnect_is_eligible(policy_enabled: bool, failure: ConnectionFailure) -> bool {
    policy_enabled && matches!(failure, ConnectionFailure::Transport)
}

/// Maps a connection-establishment failure to the generic, content-free
/// category the application layer can react to. Only `Authentication` is
/// distinguished today: it lets the UI reprompt for a password in-tab
/// (mimicking `ssh`'s own retry) instead of showing a raw failed session.
const fn session_error_kind_for_failure(failure: ConnectionFailure) -> SessionErrorKind {
    match failure {
        ConnectionFailure::Authentication => SessionErrorKind::Authentication,
        ConnectionFailure::Transport
        | ConnectionFailure::HostTrust
        | ConnectionFailure::Setup
        | ConnectionFailure::ProviderUnavailable => SessionErrorKind::Spawn,
    }
}

enum ConnectionAttempt {
    Established(
        russh::client::Handle<SshClientHandler>,
        russh::Channel<russh::client::Msg>,
    ),
    Retryable(ConnectionFailure, &'static str),
    Permanent(ConnectionFailure, &'static str),
    Shutdown,
}

enum ReconnectSchedule {
    Reconnect,
    Unavailable,
    Shutdown,
}

async fn schedule_reconnect(
    planner: &mut Option<ReconnectPlanner>,
    failure: ConnectionFailure,
    command_receiver: &WorkerCommandReceiver,
    shared: &WorkerShared,
    host_key_gate: &HostKeyDecisionGate,
) -> ReconnectSchedule {
    let action = reconnect_action_for_failure(planner, failure);
    let ReconnectAction::ScheduleAttempt { delay, .. } = action else {
        return ReconnectSchedule::Unavailable;
    };
    shared.set_reconnecting(true);
    shared.clear_reconnect_request();
    shared.set_lifecycle(SessionLifecycle::Starting);
    if wait_for_reconnect_delay(delay, command_receiver, shared, host_key_gate).await {
        if let Some(planner) = planner.as_mut() {
            let _ = planner.cancel();
        }
        shared.set_reconnecting(false);
        shared.set_lifecycle(SessionLifecycle::Stopped);
        return ReconnectSchedule::Shutdown;
    }
    let Some(planner) = planner.as_mut() else {
        return ReconnectSchedule::Unavailable;
    };
    if matches!(
        planner.delay_elapsed(),
        ReconnectAction::StartFreshConnection {
            host_verification: FreshHostVerification::Required,
            ..
        }
    ) {
        ReconnectSchedule::Reconnect
    } else {
        ReconnectSchedule::Unavailable
    }
}

fn reconnect_action_for_failure(
    planner: &mut Option<ReconnectPlanner>,
    failure: ConnectionFailure,
) -> ReconnectAction {
    if !reconnect_is_eligible(planner.is_some(), failure) {
        return ReconnectAction::None;
    }
    let planner = planner
        .as_mut()
        .expect("enabled reconnect policy must have a planner");
    match planner.state() {
        ReconnectState::Idle => planner.disconnected(),
        ReconnectState::Connecting { .. } => planner.connection_failed(),
        _ => ReconnectAction::None,
    }
}

async fn wait_for_reconnect_delay(
    delay: Duration,
    command_receiver: &WorkerCommandReceiver,
    shared: &WorkerShared,
    host_key_gate: &HostKeyDecisionGate,
) -> bool {
    let elapsed = tokio::time::sleep(delay);
    tokio::pin!(elapsed);
    loop {
        tokio::select! {
            _ = &mut elapsed => return false,
            _ = tokio::time::sleep(COMMAND_POLL_INTERVAL) => {
                if process_commands_before_running(command_receiver, shared, host_key_gate) {
                    return true;
                }
            }
        }
    }
}

/// Establishes one fresh transport. Every invocation creates a new handler;
/// its host-key callback begins a new gate sequence and emits a new prompt.
#[allow(clippy::too_many_arguments)]
async fn establish_connection(
    profile: &SshConnectionProfile,
    authentication: &WorkerAuthentication,
    strategy: &SessionStrategy,
    known_host_fingerprint: Option<&str>,
    shared: &Arc<WorkerShared>,
    command_receiver: &WorkerCommandReceiver,
    host_key_gate: &Arc<HostKeyDecisionGate>,
    password_gate: &Arc<PasswordDecisionGate>,
) -> ConnectionAttempt {
    if process_commands_before_running(command_receiver, shared, host_key_gate) {
        return ConnectionAttempt::Shutdown;
    }
    let config = Arc::new(russh::client::Config {
        nodelay: true,
        ..Default::default()
    });
    let host_key_rejected = Arc::new(AtomicBool::new(false));
    let handler = SshClientHandler {
        identity: profile.identity.clone(),
        shared: Arc::clone(shared),
        host_key_gate: Arc::clone(host_key_gate),
        host_key_rejected: Arc::clone(&host_key_rejected),
        expected_fingerprint: known_host_fingerprint.map(str::to_owned),
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
                Err(_) if host_key_rejected.load(Ordering::Acquire) => {
                    return ConnectionAttempt::Permanent(
                        ConnectionFailure::HostTrust,
                        "SSH host key was rejected",
                    );
                }
                Err(_) => {
                    return ConnectionAttempt::Retryable(
                        ConnectionFailure::Transport,
                        "SSH connection failed",
                    )
                }
            },
            _ = &mut connection_timeout => return ConnectionAttempt::Retryable(
                ConnectionFailure::Transport,
                "SSH connection timed out",
            ),
            _ = tokio::time::sleep(COMMAND_POLL_INTERVAL) => {
                if process_commands_before_running(command_receiver, shared, host_key_gate) {
                    return ConnectionAttempt::Shutdown;
                }
            }
        }
    };

    let authentication_result = match authentication {
        WorkerAuthentication::Password(password) => {
            wait_for_ssh_operation(
                handle.authenticate_password(profile.username(), password),
                command_receiver,
                shared,
                host_key_gate,
            )
            .await
        }
        WorkerAuthentication::StoredPassword(authentication) => {
            // Resolve only after the transport and host-key gate are ready,
            // immediately before password authentication on this worker.
            let mut password = match resolve_stored_password(authentication) {
                Ok(password) => password,
                Err(error) => {
                    return ConnectionAttempt::Permanent(
                        ConnectionFailure::Authentication,
                        error.message(),
                    );
                }
            };
            let result = wait_for_ssh_operation(
                handle.authenticate_password(profile.username(), &password),
                command_receiver,
                shared,
                host_key_gate,
            )
            .await;
            password.zeroize();
            result
        }
        WorkerAuthentication::PublicKey(private_key) => {
            let hash_algorithm = match wait_for_ssh_operation(
                handle.best_supported_rsa_hash(),
                command_receiver,
                shared,
                host_key_gate,
            )
            .await
            {
                WorkerWait::Completed(Ok(hash_algorithm)) => hash_algorithm.flatten(),
                WorkerWait::Completed(Err(_)) => {
                    return ConnectionAttempt::Permanent(
                        ConnectionFailure::Authentication,
                        "SSH public-key authentication could not select a signature algorithm",
                    );
                }
                WorkerWait::Shutdown => {
                    let _ = stop_handle(handle, shared).await;
                    return ConnectionAttempt::Shutdown;
                }
            };
            wait_for_ssh_operation(
                handle.authenticate_publickey(
                    profile.username(),
                    russh::keys::PrivateKeyWithHashAlg::new(
                        Arc::clone(private_key),
                        hash_algorithm,
                    ),
                ),
                command_receiver,
                shared,
                host_key_gate,
            )
            .await
        }
        WorkerAuthentication::StoredPrivateKey(authentication) => {
            // Resolve and parse only after the transport and host-key gate
            // are ready, immediately before public-key authentication on
            // this worker (mirrors `StoredPassword` above).
            let private_key = match resolve_stored_private_key(authentication) {
                Ok(private_key) => private_key,
                Err(error) => {
                    return ConnectionAttempt::Permanent(
                        ConnectionFailure::Authentication,
                        error.message(),
                    );
                }
            };
            let hash_algorithm = match wait_for_ssh_operation(
                handle.best_supported_rsa_hash(),
                command_receiver,
                shared,
                host_key_gate,
            )
            .await
            {
                WorkerWait::Completed(Ok(hash_algorithm)) => hash_algorithm.flatten(),
                WorkerWait::Completed(Err(_)) => {
                    return ConnectionAttempt::Permanent(
                        ConnectionFailure::Authentication,
                        "SSH public-key authentication could not select a signature algorithm",
                    );
                }
                WorkerWait::Shutdown => {
                    let _ = stop_handle(handle, shared).await;
                    return ConnectionAttempt::Shutdown;
                }
            };
            wait_for_ssh_operation(
                handle.authenticate_publickey(
                    profile.username(),
                    russh::keys::PrivateKeyWithHashAlg::new(private_key, hash_algorithm),
                ),
                command_receiver,
                shared,
                host_key_gate,
            )
            .await
        }
        WorkerAuthentication::Interactive => {
            // No credential was supplied upfront: the host key has already
            // been verified above (via `check_server_key`, exactly like any
            // other credential), so this is the first moment a password is
            // actually needed — matching `ssh`'s own ordering instead of
            // collecting one blind before a connection even exists. A wrong
            // password is retried in place, on this same connection, up to
            // `MAX_INTERACTIVE_PASSWORD_ATTEMPTS`.
            let mut attempt: u8 = 1;
            let mut previous_attempt_failed = false;
            loop {
                let waiter = match request_password_verification(
                    profile,
                    shared,
                    password_gate,
                    attempt,
                    previous_attempt_failed,
                ) {
                    Ok(waiter) => waiter,
                    Err(_) => {
                        let _ = stop_handle(handle, shared).await;
                        return ConnectionAttempt::Shutdown;
                    }
                };
                let mut password = match waiter
                    .wait_async(password_gate, PASSWORD_DECISION_TIMEOUT)
                    .await
                {
                    Some(password) => password,
                    None => {
                        let _ = stop_handle(handle, shared).await;
                        return ConnectionAttempt::Shutdown;
                    }
                };
                let result = wait_for_ssh_operation(
                    handle.authenticate_password(profile.username(), &password),
                    command_receiver,
                    shared,
                    host_key_gate,
                )
                .await;
                password.zeroize();
                let retry_exhausted = attempt >= MAX_INTERACTIVE_PASSWORD_ATTEMPTS;
                match &result {
                    WorkerWait::Completed(Ok(auth)) if auth.success() => break result,
                    WorkerWait::Completed(Ok(_)) | WorkerWait::Completed(Err(_))
                        if !retry_exhausted =>
                    {
                        attempt += 1;
                        previous_attempt_failed = true;
                    }
                    WorkerWait::Completed(_) | WorkerWait::Shutdown => break result,
                }
            }
        }
    };

    match authentication_result {
        WorkerWait::Completed(Ok(result)) if result.success() => {}
        WorkerWait::Completed(Ok(_)) | WorkerWait::Completed(Err(_)) => {
            return ConnectionAttempt::Permanent(
                ConnectionFailure::Authentication,
                "SSH authentication failed",
            );
        }
        WorkerWait::Shutdown => {
            let _ = stop_handle(handle, shared).await;
            return ConnectionAttempt::Shutdown;
        }
    }

    // A persistent strategy's capability is probed lazily, immediately
    // before it would otherwise be relied on, and never speculatively or in
    // the background (ADR 0018, Issue #49 "run the provider capability
    // probe lazily and surface unavailable-provider errors clearly"). This
    // uses its own throwaway channel so a missing provider is reported
    // distinctly from an ordinary shell/exec setup failure, without ever
    // touching the PTY channel opened below.
    if let SessionStrategy::Persistent { provider, .. } = strategy {
        match probe_persistence_provider(
            &handle,
            *provider,
            command_receiver,
            shared,
            host_key_gate,
        )
        .await
        {
            ProviderProbeOutcome::Available => {}
            ProviderProbeOutcome::Unavailable => {
                return ConnectionAttempt::Permanent(
                    ConnectionFailure::ProviderUnavailable,
                    "the configured durable-session provider is not available on the remote host",
                );
            }
            ProviderProbeOutcome::ProbeFailed => {
                return ConnectionAttempt::Permanent(
                    ConnectionFailure::ProviderUnavailable,
                    "the durable-session provider could not be probed on the remote host",
                );
            }
            ProviderProbeOutcome::Shutdown => {
                let _ = stop_handle(handle, shared).await;
                return ConnectionAttempt::Shutdown;
            }
        }
    }

    let channel = match wait_for_ssh_operation(
        handle.channel_open_session(),
        command_receiver,
        shared,
        host_key_gate,
    )
    .await
    {
        WorkerWait::Completed(Ok(channel)) => channel,
        WorkerWait::Completed(Err(_)) => {
            return ConnectionAttempt::Permanent(
                ConnectionFailure::Setup,
                "SSH session channel could not open",
            );
        }
        WorkerWait::Shutdown => {
            let _ = stop_handle(handle, shared).await;
            return ConnectionAttempt::Shutdown;
        }
    };
    let mut channel = channel;
    let initial_size = shared.desired_terminal_size();
    let dimensions = ssh_terminal_dimensions(initial_size);
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
        command_receiver,
        shared,
        host_key_gate,
    )
    .await
    {
        WorkerWait::Completed(Ok(())) => {}
        WorkerWait::Completed(Err(_)) => {
            return ConnectionAttempt::Permanent(ConnectionFailure::Setup, "SSH PTY request failed")
        }
        WorkerWait::Shutdown => {
            let _ = stop_handle(handle, shared).await;
            return ConnectionAttempt::Shutdown;
        }
    }
    match wait_for_channel_request_reply(&mut channel, command_receiver, shared, host_key_gate)
        .await
    {
        ChannelRequestReply::Accepted => {}
        ChannelRequestReply::Rejected => {
            return ConnectionAttempt::Permanent(
                ConnectionFailure::Setup,
                "SSH PTY request was rejected",
            );
        }
        ChannelRequestReply::Shutdown => {
            let _ = stop_handle(handle, shared).await;
            return ConnectionAttempt::Shutdown;
        }
    }

    let shell_launch_result = match strategy {
        SessionStrategy::PlainShell => {
            wait_for_ssh_operation(
                channel.request_shell(true),
                command_receiver,
                shared,
                host_key_gate,
            )
            .await
        }
        SessionStrategy::Persistent {
            provider,
            session_name,
        } => {
            let attach_size = shared.desired_terminal_size();
            wait_for_ssh_operation(
                channel.exec(
                    true,
                    provider.attach_or_create_command(session_name, attach_size),
                ),
                command_receiver,
                shared,
                host_key_gate,
            )
            .await
        }
    };
    match shell_launch_result {
        WorkerWait::Completed(Ok(())) => {}
        WorkerWait::Completed(Err(_)) => {
            return ConnectionAttempt::Permanent(
                ConnectionFailure::Setup,
                "SSH shell request failed",
            )
        }
        WorkerWait::Shutdown => {
            let _ = stop_handle(handle, shared).await;
            return ConnectionAttempt::Shutdown;
        }
    }
    match wait_for_channel_request_reply(&mut channel, command_receiver, shared, host_key_gate)
        .await
    {
        ChannelRequestReply::Accepted => {
            let mut applied_size = initial_size;
            while let Some(latest_size) = shared.take_pre_running_resize() {
                if latest_size != applied_size {
                    let dimensions = ssh_terminal_dimensions(latest_size);
                    match wait_for_ssh_operation(
                        channel.window_change(
                            dimensions.0,
                            dimensions.1,
                            dimensions.2,
                            dimensions.3,
                        ),
                        command_receiver,
                        shared,
                        host_key_gate,
                    )
                    .await
                    {
                        WorkerWait::Completed(Ok(())) => {}
                        WorkerWait::Completed(Err(_)) => {
                            return ConnectionAttempt::Permanent(
                                ConnectionFailure::Setup,
                                "SSH initial resize failed",
                            );
                        }
                        WorkerWait::Shutdown => {
                            let _ = stop_handle(handle, shared).await;
                            return ConnectionAttempt::Shutdown;
                        }
                    }
                }
                applied_size = latest_size;
                shared.record_resize_applied(latest_size);
            }
            ConnectionAttempt::Established(handle, channel)
        }
        ChannelRequestReply::Rejected => {
            ConnectionAttempt::Permanent(ConnectionFailure::Setup, "SSH shell request was rejected")
        }
        ChannelRequestReply::Shutdown => {
            let _ = stop_handle(handle, shared).await;
            ConnectionAttempt::Shutdown
        }
    }
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

/// The result of lazily probing whether `provider` is available on the
/// remote host, run only immediately before a persistent strategy would
/// otherwise rely on it (ADR 0018, Issue #49). This never runs
/// speculatively, in the background, or for a plain shell.
enum ProviderProbeOutcome {
    /// `provider`'s capability-probe command exited zero: it is present.
    Available,
    /// `provider`'s capability-probe command exited non-zero: it is
    /// missing, so the caller must not attempt to attach or create a
    /// durable session with it.
    Unavailable,
    /// The probe command itself could not be run to completion (the probe
    /// channel failed to open, the exec request was rejected, or the
    /// remote end closed the channel before an exit status arrived). This
    /// is distinct from `Unavailable`: the host may or may not have the
    /// provider, but fesTerm could not find out.
    ProbeFailed,
    Shutdown,
}

/// Runs `provider`'s capability-probe command on a throwaway channel and
/// waits for its exit status. Probe output is discarded rather than routed
/// to the terminal: this channel never becomes the session's PTY.
async fn probe_persistence_provider(
    handle: &russh::client::Handle<SshClientHandler>,
    provider: PersistenceProvider,
    command_receiver: &WorkerCommandReceiver,
    shared: &WorkerShared,
    host_key_gate: &HostKeyDecisionGate,
) -> ProviderProbeOutcome {
    let mut channel = match wait_for_ssh_operation(
        handle.channel_open_session(),
        command_receiver,
        shared,
        host_key_gate,
    )
    .await
    {
        WorkerWait::Completed(Ok(channel)) => channel,
        WorkerWait::Completed(Err(_)) => return ProviderProbeOutcome::ProbeFailed,
        WorkerWait::Shutdown => return ProviderProbeOutcome::Shutdown,
    };
    match wait_for_ssh_operation(
        channel.exec(true, provider.capability_probe_command()),
        command_receiver,
        shared,
        host_key_gate,
    )
    .await
    {
        WorkerWait::Completed(Ok(())) => {}
        WorkerWait::Completed(Err(_)) => return ProviderProbeOutcome::ProbeFailed,
        WorkerWait::Shutdown => return ProviderProbeOutcome::Shutdown,
    }
    loop {
        tokio::select! {
            message = channel.wait() => match message {
                Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                    return if exit_status == 0 {
                        ProviderProbeOutcome::Available
                    } else {
                        ProviderProbeOutcome::Unavailable
                    };
                }
                Some(russh::ChannelMsg::ExitSignal { .. }) => return ProviderProbeOutcome::ProbeFailed,
                // `Eof` alone must not end the probe: servers are not
                // required to send the exit-status channel request before
                // eof, and for a near-instant command like a capability
                // probe it is common to observe eof arrive first. Only a
                // fully closed channel that never delivered an exit status
                // is a genuine probe failure.
                Some(russh::ChannelMsg::Eof) => {}
                Some(russh::ChannelMsg::Close) | None => {
                    return ProviderProbeOutcome::ProbeFailed;
                }
                // The probe's own stdout/stderr is deliberately discarded:
                // this channel is never the session's PTY, so its output
                // must never reach the terminal.
                Some(_) => {}
            },
            _ = tokio::time::sleep(COMMAND_POLL_INTERVAL) => {
                if process_commands_before_running(command_receiver, shared, host_key_gate) {
                    return ProviderProbeOutcome::Shutdown;
                }
            }
        }
    }
}

enum RunningOutcome {
    Shutdown(ShutdownResult),
    Exited(festerm_session::SessionExit),
    ConnectionLost(&'static str),
    ReconnectRequested,
}

/// Sends a benign SSH-level liveness probe (ADR 0018: "a supported SSH-level
/// keepalive/global request... whose only purpose is to determine whether
/// the existing transport still responds") and waits up to
/// [`LIVENESS_PROBE_TIMEOUT`] for the remote peer's reply.
///
/// Returns `true` only if the reply arrived in time. A dead transport
/// typically fails fast (the underlying send fails once the connection task
/// has already noticed the loss); a merely slow or partially-black-holed
/// path is treated as failed once the bound elapses, since ADR 0018 defines
/// liveness as "still responds", not "may eventually respond".
async fn probe_liveness(handle: &russh::client::Handle<SshClientHandler>) -> bool {
    matches!(
        tokio::time::timeout(LIVENESS_PROBE_TIMEOUT, handle.send_ping()).await,
        Ok(Ok(()))
    )
}

/// Pure decision for whether an automatic/on-demand liveness probe is due
/// right now, extracted from the main connection loop so the cadence policy
/// (ADR 0018, issue #55) can be exercised in tests against synthetic
/// `Instant`s instead of real wall-clock sleeps.
///
/// A probe is due if either an on-demand request is pending (wake hook or
/// `SshSession::try_check_liveness`) or the automatic cadence deadline has
/// elapsed. Setting `next_probe` far in the future (e.g. `now + Duration::MAX`)
/// models a "disabled" automatic cadence while still allowing on-demand
/// requests to fire, which is how tests cover both halves of #55's
/// acceptance criteria without an always-enabled/always-disabled setting.
fn liveness_probe_due(
    now: tokio::time::Instant,
    next_probe: tokio::time::Instant,
    on_demand_requested: bool,
) -> bool {
    on_demand_requested || now >= next_probe
}

async fn run_authenticated_channel(
    mut handle: russh::client::Handle<SshClientHandler>,
    mut channel: russh::Channel<russh::client::Msg>,
    command_receiver: &WorkerCommandReceiver,
    shared: &Arc<WorkerShared>,
    host_key_gate: &Arc<HostKeyDecisionGate>,
) -> RunningOutcome {
    // Tracks the next *automatic* liveness probe (ADR 0018's "ordinary SSH
    // liveness/keepalive cadence"), independent of any on-demand request a
    // caller makes via `SshSession::try_check_liveness`/
    // `WorkerShared::request_liveness_check` (e.g. a future wake/network-
    // change hook), which is checked on every tick regardless of this
    // deadline.
    let mut next_liveness_probe = tokio::time::Instant::now() + LIVENESS_PROBE_INTERVAL;
    loop {
        let probe_due = liveness_probe_due(
            tokio::time::Instant::now(),
            next_liveness_probe,
            shared.take_liveness_check_requested(),
        );
        if probe_due {
            if !probe_liveness(&handle).await {
                return RunningOutcome::ConnectionLost(
                    "SSH liveness probe did not receive a response",
                );
            }
            next_liveness_probe = tokio::time::Instant::now() + LIVENESS_PROBE_INTERVAL;
        }
        tokio::select! {
            result = &mut handle => match result {
                Ok(()) | Err(_) => return RunningOutcome::ConnectionLost("SSH connection ended unexpectedly"),
            },
            message = channel.wait() => match message {
                Some(russh::ChannelMsg::Data { data }) => emit_channel_output(shared, data.as_ref()),
                Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                    return RunningOutcome::Exited(festerm_session::SessionExit::with_exit_code(exit_status));
                }
                Some(russh::ChannelMsg::ExitSignal { .. }) => {
                    return RunningOutcome::Exited(festerm_session::SessionExit::with_signal(0, "remote signal"));
                }
                Some(russh::ChannelMsg::Eof | russh::ChannelMsg::Close) | None => {
                    return RunningOutcome::ConnectionLost("SSH connection ended unexpectedly");
                }
                Some(_) => {}
            },
            _ = tokio::time::sleep(COMMAND_POLL_INTERVAL) => {
                match process_authenticated_commands(&mut channel, command_receiver, shared, host_key_gate).await {
                    Ok(AuthenticatedCommandOutcome::Continue) => {}
                    Ok(AuthenticatedCommandOutcome::Shutdown) => return RunningOutcome::Shutdown(
                        stop_handle(handle, shared).await.unwrap_or(ShutdownResult::Stopped)
                    ),
                    Ok(AuthenticatedCommandOutcome::Reconnect) => {
                        let _ = stop_handle(handle, shared).await;
                        return RunningOutcome::ReconnectRequested;
                    }
                    Err(_) => return RunningOutcome::ConnectionLost("SSH connection ended unexpectedly"),
                }
            }
        }
    }
}

enum AuthenticatedCommandOutcome {
    Continue,
    Shutdown,
    Reconnect,
}

async fn process_authenticated_commands(
    channel: &mut russh::Channel<russh::client::Msg>,
    command_receiver: &WorkerCommandReceiver,
    shared: &WorkerShared,
    host_key_gate: &HostKeyDecisionGate,
) -> Result<AuthenticatedCommandOutcome, SessionError> {
    if shared.shutdown_requested() {
        host_key_gate.reject_pending();
        shared.set_lifecycle(SessionLifecycle::Stopping);
        return Ok(AuthenticatedCommandOutcome::Shutdown);
    }
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
                    WorkerWait::Shutdown => return Ok(AuthenticatedCommandOutcome::Shutdown),
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
                    WorkerWait::Completed(Ok(())) => shared.record_running_resize(size),
                    WorkerWait::Completed(Err(_)) => {
                        return Err(ssh_failure_with_kind(
                            shared,
                            SessionErrorKind::Resize,
                            "SSH resize failed",
                        ));
                    }
                    WorkerWait::Shutdown => return Ok(AuthenticatedCommandOutcome::Shutdown),
                }
            }
            Ok(WorkerCommand::Reconnect) => {
                return Ok(AuthenticatedCommandOutcome::Reconnect);
            }
            Ok(WorkerCommand::Shutdown) | Err(TryRecvError::Disconnected) => {
                host_key_gate.reject_pending();
                shared.set_lifecycle(SessionLifecycle::Stopping);
                return Ok(AuthenticatedCommandOutcome::Shutdown);
            }
            Err(TryRecvError::Empty) => return Ok(AuthenticatedCommandOutcome::Continue),
        }
    }
}

/// Outcome of waiting in the `Disconnected` (recovery-eligible) state after
/// an unintentional transport loss for a session with no automatic recovery
/// resuming it (ADR 0018).
enum ManualRecoveryOutcome {
    /// The user explicitly requested a reconnect via [`WorkerCommand::Reconnect`].
    Reconnect,
    /// The user shut the session down while it was disconnected.
    Shutdown,
}

/// Waits, polling at [`COMMAND_POLL_INTERVAL`], for the user to either
/// explicitly request a reconnect or shut down a session that is currently
/// `Disconnected`. Input and resize commands are reported as unsupported
/// (there is no live channel to apply them to) without ending the wait.
async fn wait_for_manual_recovery(
    command_receiver: &WorkerCommandReceiver,
    shared: &WorkerShared,
    host_key_gate: &HostKeyDecisionGate,
) -> ManualRecoveryOutcome {
    loop {
        if shared.shutdown_requested() {
            host_key_gate.reject_pending();
            return ManualRecoveryOutcome::Shutdown;
        }
        match command_receiver.try_recv() {
            Ok(WorkerCommand::Input(bytes)) => {
                let _ = bytes.len();
                report_unsupported(shared, "SSH input is not available");
            }
            Ok(WorkerCommand::Resize(size)) => {
                let _ = size.columns();
                report_unsupported(shared, "SSH resize is not available");
            }
            Ok(WorkerCommand::Reconnect) => {
                shared.clear_reconnect_request();
                return ManualRecoveryOutcome::Reconnect;
            }
            Ok(WorkerCommand::Shutdown) | Err(TryRecvError::Disconnected) => {
                host_key_gate.reject_pending();
                return ManualRecoveryOutcome::Shutdown;
            }
            Err(TryRecvError::Empty) => {
                tokio::time::sleep(COMMAND_POLL_INTERVAL).await;
            }
        }
    }
}

fn process_commands_before_running(
    command_receiver: &WorkerCommandReceiver,
    shared: &WorkerShared,
    host_key_gate: &HostKeyDecisionGate,
) -> bool {
    if shared.shutdown_requested() {
        host_key_gate.reject_pending();
        shared.set_lifecycle(SessionLifecycle::Stopping);
        return true;
    }
    loop {
        match command_receiver.try_recv() {
            Ok(WorkerCommand::Input(bytes)) => {
                let _ = bytes.len();
                report_unsupported(shared, "SSH input is not available");
            }
            Ok(WorkerCommand::Resize(size)) => {
                shared.retain_pre_running_resize(size);
            }
            Ok(WorkerCommand::Reconnect) => {
                shared.clear_reconnect_request();
                report_unsupported(shared, "SSH reconnect is not available")
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
    use festerm_secret_store::{MemorySecretStore, SecretBytes, SecretStore};
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
    fn session_error_kind_for_failure_distinguishes_only_authentication() {
        assert_eq!(
            session_error_kind_for_failure(ConnectionFailure::Authentication),
            SessionErrorKind::Authentication,
            "a rejected credential must be classified distinctly so the UI can reprompt in-tab"
        );
        for other in [
            ConnectionFailure::Transport,
            ConnectionFailure::HostTrust,
            ConnectionFailure::Setup,
            ConnectionFailure::ProviderUnavailable,
        ] {
            assert_eq!(
                session_error_kind_for_failure(other),
                SessionErrorKind::Spawn,
                "every other connection failure must keep the prior generic classification"
            );
        }
    }

    #[test]
    fn manual_reconnect_delay_doubles_and_caps_at_the_maximum() {
        assert_eq!(manual_reconnect_delay(1), MANUAL_RECONNECT_INITIAL_DELAY);
        assert_eq!(
            manual_reconnect_delay(2),
            MANUAL_RECONNECT_INITIAL_DELAY * 2
        );
        assert_eq!(
            manual_reconnect_delay(3),
            MANUAL_RECONNECT_INITIAL_DELAY * 4
        );
        // Keeps doubling until it reaches the cap, then stays there rather
        // than overflowing or exceeding it, even for attempt counts well
        // past `MANUAL_RECONNECT_MAX_ATTEMPTS`.
        assert_eq!(manual_reconnect_delay(20), MANUAL_RECONNECT_MAX_DELAY);
        assert!(MANUAL_RECONNECT_MAX_DELAY >= MANUAL_RECONNECT_INITIAL_DELAY);
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
    fn stored_password_authentication_resolves_only_through_the_worker_source() {
        let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let password = "stored-password-not-for-debug-output";
        let reference = store
            .put(&SecretBytes::copy_from_slice(password.as_bytes()))
            .expect("memory store accepts test password");
        let authentication = SshAuthentication::stored_password(Arc::clone(&store), &reference);

        assert_eq!(
            format!("{authentication:?}"),
            "SshAuthentication::StoredPassword([REDACTED])"
        );
        assert!(!format!("{authentication:?}").contains(password));
        assert!(!format!("{authentication:?}").contains(&reference.to_persisted_string()));

        let WorkerAuthentication::StoredPassword(source) =
            authentication.into_worker_authentication()
        else {
            panic!("stored password must retain the worker-only source");
        };
        assert_eq!(
            resolve_stored_password(&source).expect("memory password resolves"),
            password
        );
    }

    #[test]
    fn stored_password_resolution_errors_are_actionable_and_redacted() {
        let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let reference = SecretReference::generate();
        let error = resolve_stored_password(&StoredPasswordAuthentication {
            store,
            reference: reference.duplicate_for_transport(),
        })
        .expect_err("unstored reference must be missing");

        assert_eq!(error, StoredPasswordResolutionError::Missing);
        assert!(error.to_string().contains("replace"));
        assert!(!error.to_string().contains(&reference.to_persisted_string()));
    }

    fn generated_private_key_text() -> String {
        let mut random = russh::keys::key::safe_rng();
        let key = russh::keys::PrivateKey::random(&mut random, russh::keys::Algorithm::Ed25519)
            .expect("could not generate test SSH key");
        key.to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("could not encode test SSH key")
            .to_string()
    }

    #[test]
    fn stored_private_key_authentication_resolves_only_through_the_worker_source() {
        let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let key_text = generated_private_key_text();
        let secret = encode_stored_private_key(&key_text, None);
        let reference = store
            .put(&secret)
            .expect("memory store accepts test private key");
        let authentication = SshAuthentication::stored_private_key(Arc::clone(&store), &reference);

        assert_eq!(
            format!("{authentication:?}"),
            "SshAuthentication::StoredPrivateKey([REDACTED])"
        );
        assert!(!format!("{authentication:?}").contains(&key_text));
        assert!(!format!("{authentication:?}").contains(&reference.to_persisted_string()));

        let WorkerAuthentication::StoredPrivateKey(source) =
            authentication.into_worker_authentication()
        else {
            panic!("stored private key must retain the worker-only source");
        };
        assert!(resolve_stored_private_key(&source).is_ok());
    }

    #[test]
    fn stored_private_key_round_trips_with_a_passphrase() {
        let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let mut random = russh::keys::key::safe_rng();
        let key = russh::keys::PrivateKey::random(&mut random, russh::keys::Algorithm::Ed25519)
            .expect("could not generate test SSH key");
        let encrypted = key
            .encrypt(&mut random, "correct horse battery staple")
            .expect("could not encrypt test SSH key")
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("could not encode encrypted test SSH key");
        let secret = encode_stored_private_key(&encrypted, Some("correct horse battery staple"));
        let reference = store
            .put(&secret)
            .expect("memory store accepts test private key");
        let authentication = SshAuthentication::stored_private_key(store, &reference);
        let WorkerAuthentication::StoredPrivateKey(source) =
            authentication.into_worker_authentication()
        else {
            panic!("stored private key must retain the worker-only source");
        };
        assert!(resolve_stored_private_key(&source).is_ok());
    }

    #[test]
    fn stored_private_key_resolution_errors_are_actionable_and_redacted() {
        let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let reference = SecretReference::generate();
        let error = resolve_stored_private_key(&StoredPrivateKeyAuthentication {
            store,
            reference: reference.duplicate_for_transport(),
        })
        .expect_err("unstored reference must be missing");

        assert_eq!(error, StoredPrivateKeyResolutionError::Missing);
        assert!(error.to_string().contains("replace"));
        assert!(!error.to_string().contains(&reference.to_persisted_string()));
    }

    fn generated_private_key() -> SshPrivateKey {
        let mut random = russh::keys::key::safe_rng();
        let key = russh::keys::PrivateKey::random(&mut random, russh::keys::Algorithm::Ed25519)
            .expect("could not generate test SSH key");
        let encoded = key
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("could not encode test SSH key");
        SshPrivateKey::from_openssh(encoded.as_bytes()).expect("could not parse test SSH key")
    }

    #[test]
    fn public_key_authentication_redacts_and_dispatches_the_parsed_key() {
        let private_key = generated_private_key();
        assert_eq!(format!("{private_key:?}"), "SshPrivateKey([REDACTED])");

        let authentication = SshAuthentication::public_key(private_key);
        assert_eq!(
            format!("{authentication:?}"),
            "SshAuthentication::PublicKey([REDACTED])"
        );
        assert!(matches!(
            authentication.into_worker_authentication(),
            WorkerAuthentication::PublicKey(_)
        ));
    }

    #[test]
    fn public_key_parser_rejects_invalid_and_non_openssh_material_without_echoing_it() {
        assert!(matches!(
            SshPrivateKey::from_openssh([0xff]),
            Err(SshPrivateKeyError::InvalidEncoding)
        ));
        assert!(matches!(
            SshPrivateKey::from_openssh("-----BEGIN PRIVATE KEY-----\ninvalid"),
            Err(SshPrivateKeyError::NotOpenSsh)
        ));
        let malformed = "-----BEGIN OPENSSH PRIVATE KEY-----\nmalformed-private-material";
        let error = SshPrivateKey::from_openssh(malformed)
            .expect_err("malformed OpenSSH private key must not parse");
        assert_eq!(error, SshPrivateKeyError::InvalidKey);
        assert!(!error.to_string().contains(malformed));
    }

    fn encrypted_private_key_fixture() -> String {
        let mut random = russh::keys::key::safe_rng();
        let key = russh::keys::PrivateKey::random(&mut random, russh::keys::Algorithm::Ed25519)
            .expect("could not generate encrypted-key test input");
        let encrypted = key
            .encrypt(&mut random, "test encrypted-key passphrase")
            .expect("could not encrypt test SSH key");
        encrypted
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("could not encode encrypted test SSH key")
            .to_string()
    }

    #[test]
    fn public_key_parser_rejects_encrypted_openssh_keys_without_a_passphrase() {
        let encoded = encrypted_private_key_fixture();

        assert!(matches!(
            SshPrivateKey::from_openssh(encoded.as_bytes()),
            Err(SshPrivateKeyError::Encrypted)
        ));
    }

    #[test]
    fn encrypted_private_key_parser_consumes_and_redacts_its_passphrase() {
        let encoded = encrypted_private_key_fixture();
        let passphrase = "test encrypted-key passphrase";
        let secret = SshKeyPassphrase::new(passphrase);
        assert_eq!(format!("{secret:?}"), "SshKeyPassphrase([REDACTED])");

        let private_key = SshPrivateKey::from_encrypted_openssh(encoded.as_bytes(), secret)
            .expect("could not parse encrypted test SSH key");
        assert_eq!(format!("{private_key:?}"), "SshPrivateKey([REDACTED])");

        let error = SshPrivateKey::from_encrypted_openssh(
            encoded.as_bytes(),
            SshKeyPassphrase::new("wrong encrypted-key passphrase"),
        )
        .expect_err("wrong passphrase must not parse an encrypted SSH key");
        assert_eq!(error, SshPrivateKeyError::InvalidKey);
        assert!(!error.to_string().contains(passphrase));
        assert!(!error.to_string().contains("wrong encrypted-key passphrase"));
        assert!(!error.to_string().contains(encoded.trim()));
    }

    #[test]
    fn encrypted_private_key_parser_rejects_unencrypted_keys() {
        let mut random = russh::keys::key::safe_rng();
        let key = russh::keys::PrivateKey::random(&mut random, russh::keys::Algorithm::Ed25519)
            .expect("could not generate test SSH key");
        let encoded = key
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("could not encode test SSH key");

        assert!(matches!(
            SshPrivateKey::from_encrypted_openssh(
                encoded.as_bytes(),
                SshKeyPassphrase::new("unused passphrase")
            ),
            Err(SshPrivateKeyError::Unencrypted)
        ));
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
    fn default_automatic_reconnect_policy_is_bounded_and_valid() {
        let policy = ReconnectPolicy::default_automatic();

        assert!(policy.maximum_attempts() > 0);
    }

    #[test]
    fn reconnect_options_default_to_manual_plain_shell_recovery() {
        let policy =
            ReconnectPolicy::new(2, Duration::from_millis(10), Duration::from_millis(20)).unwrap();

        assert_eq!(SshSessionOptions::new().reconnect_policy(), None);
        assert_eq!(
            SshSessionOptions::new().strategy(),
            SessionStrategy::PlainShell
        );
        assert_eq!(
            SshSessionOptions::with_recovery_policy(
                SessionStrategy::PlainShell,
                RecoveryPolicy::Automatic(policy)
            ),
            Err(RecoveryPolicyError),
            "a plain shell cannot safely support automatic recovery (ADR 0018)"
        );
        assert_eq!(
            SshSessionOptions::with_recovery_policy(
                SessionStrategy::PlainShell,
                RecoveryPolicy::Manual
            )
            .expect("manual recovery is always valid")
            .reconnect_policy(),
            None
        );
    }

    #[test]
    fn persistent_session_name_accepts_a_conservative_character_set() {
        assert_eq!(
            PersistentSessionName::new("main-session_1.local")
                .unwrap()
                .as_str(),
            "main-session_1.local"
        );
    }

    #[test]
    fn persistent_session_name_rejects_empty_names() {
        assert_eq!(
            PersistentSessionName::new(""),
            Err(PersistentSessionNameError::Empty)
        );
    }

    #[test]
    fn persistent_session_name_rejects_names_over_the_byte_limit() {
        let too_long = "a".repeat(65);
        assert_eq!(
            PersistentSessionName::new(too_long),
            Err(PersistentSessionNameError::TooLong {
                maximum: 64,
                actual: 65
            })
        );
    }

    #[test]
    fn persistent_session_name_rejects_shell_metacharacters() {
        for candidate in ["a b", "a;b", "a$b", "a`b`", "a&&b", "../etc", "a\"b"] {
            assert_eq!(
                PersistentSessionName::new(candidate),
                Err(PersistentSessionNameError::InvalidCharacter),
                "expected {candidate:?} to be rejected"
            );
        }
    }

    #[test]
    fn persistence_provider_commands_embed_the_session_name_and_stay_re_entrant() {
        let name = PersistentSessionName::new("work").unwrap();

        assert_eq!(
            PersistenceProvider::Tmux
                .attach_or_create_command(&name, TerminalSize::new(132, 43).unwrap()),
            "exec tmux new-session -A -s work -x 132 -y 43 \\; set-option -t work status off \
             \\; set-window-option -t work window-size latest"
        );
        assert_eq!(
            PersistenceProvider::Screen
                .attach_or_create_command(&name, TerminalSize::new(132, 43).unwrap()),
            "exec screen -c /dev/null -xRR work"
        );
        assert_eq!(
            PersistenceProvider::Tmux.capability_probe_command(),
            "command -v tmux"
        );
        assert_eq!(
            PersistenceProvider::Screen.capability_probe_command(),
            "command -v screen"
        );
    }

    #[test]
    fn a_missing_persistence_provider_is_never_retried() {
        // A missing remote executable is a stable condition (ADR 0018,
        // Issue #49): retrying the same transport can never make it
        // appear, unlike an ordinary transient transport loss.
        assert!(!reconnect_is_eligible(
            true,
            ConnectionFailure::ProviderUnavailable
        ));
    }

    #[test]
    fn only_persistent_strategies_support_automatic_recovery() {
        let persistent = SessionStrategy::Persistent {
            provider: PersistenceProvider::Tmux,
            session_name: PersistentSessionName::new("work").unwrap(),
        };

        assert!(!SessionStrategy::PlainShell.supports_automatic_recovery());
        assert!(persistent.supports_automatic_recovery());
    }

    #[test]
    fn automatic_recovery_is_valid_for_a_persistent_strategy() {
        let policy =
            ReconnectPolicy::new(2, Duration::from_millis(10), Duration::from_millis(20)).unwrap();
        let persistent = SessionStrategy::Persistent {
            provider: PersistenceProvider::Screen,
            session_name: PersistentSessionName::new("work").unwrap(),
        };

        let options =
            SshSessionOptions::with_recovery_policy(persistent, RecoveryPolicy::Automatic(policy))
                .expect("a persistent strategy can safely support automatic recovery (ADR 0018)");

        assert_eq!(options.reconnect_policy(), Some(policy));
    }

    #[test]
    fn reconnect_request_is_limited_to_running_sessions() {
        assert!(reconnect_request_is_available(
            &SessionLifecycle::Running,
            false
        ));
        assert!(!reconnect_request_is_available(
            &SessionLifecycle::Starting,
            false
        ));
        assert!(!reconnect_request_is_available(
            &SessionLifecycle::Running,
            true
        ));
    }

    #[test]
    fn reconnect_request_is_available_after_unintentional_transport_loss() {
        // ADR 0018: unintentional transport loss moves a plain SSH session
        // to `Disconnected`, not directly to a terminal state, and an
        // explicit reconnect must remain available from there.
        let disconnected = SessionLifecycle::Disconnected(SessionError::new(
            SessionErrorKind::Spawn,
            "test transport loss",
        ));
        assert!(reconnect_request_is_available(&disconnected, false));
        assert!(!reconnect_request_is_available(&disconnected, true));
        assert!(!disconnected.is_terminal());
    }

    #[test]
    fn reconnect_request_errors_are_content_free() {
        for error in [
            SshReconnectError::NotRunning,
            SshReconnectError::AlreadyRequested,
            SshReconnectError::QueueFull,
            SshReconnectError::Closed,
        ] {
            assert!(!error.to_string().contains("password"));
            assert!(!error.to_string().contains("private"));
        }
    }

    #[test]
    fn reconnect_request_queues_once_for_a_running_session() {
        let (foundation, command_receiver, _, _) = SshWorkerFoundation::new(profile());

        assert_eq!(
            foundation.try_reconnect(),
            Err(SshReconnectError::NotRunning)
        );

        foundation.set_running();
        assert!(foundation.reconnect_available());
        assert_eq!(foundation.try_reconnect(), Ok(()));
        assert!(!foundation.reconnect_available());
        assert_eq!(
            foundation.try_reconnect(),
            Err(SshReconnectError::AlreadyRequested)
        );
        assert!(matches!(
            command_receiver.try_recv(),
            Ok(WorkerCommand::Reconnect)
        ));
    }

    #[test]
    fn reconnect_request_queues_once_for_a_disconnected_session() {
        // ADR 0018: a plain session left `Disconnected` by an unintentional
        // transport loss must still accept exactly one explicit reconnect
        // request, the same as a still-`Running` session.
        let (foundation, command_receiver, _, _) = SshWorkerFoundation::new(profile());

        foundation.set_disconnected();
        assert!(foundation.reconnect_available());
        assert_eq!(foundation.try_reconnect(), Ok(()));
        assert!(!foundation.reconnect_available());
        assert_eq!(
            foundation.try_reconnect(),
            Err(SshReconnectError::AlreadyRequested)
        );
        assert!(matches!(
            command_receiver.try_recv(),
            Ok(WorkerCommand::Reconnect)
        ));
    }

    #[test]
    fn liveness_check_is_limited_to_running_sessions() {
        // Unlike a manual reconnect, an on-demand liveness probe has no
        // transport to probe once a session is `Disconnected` (ADR 0018
        // treats the probe and the recovery it may lead to as separate
        // steps): only `Running` is eligible.
        let (foundation, _command_receiver, _, _) = SshWorkerFoundation::new(profile());

        assert_eq!(
            foundation.try_check_liveness(),
            Err(SshLivenessCheckError::NotRunning)
        );

        foundation.set_disconnected();
        assert_eq!(
            foundation.try_check_liveness(),
            Err(SshLivenessCheckError::NotRunning)
        );

        foundation.set_running();
        assert_eq!(foundation.try_check_liveness(), Ok(()));
    }

    #[test]
    fn liveness_check_coalesces_a_single_pending_request() {
        let (foundation, _command_receiver, _, _) = SshWorkerFoundation::new(profile());
        foundation.set_running();

        assert_eq!(foundation.try_check_liveness(), Ok(()));
        assert_eq!(
            foundation.try_check_liveness(),
            Err(SshLivenessCheckError::AlreadyRequested)
        );

        // The worker consuming the pending request (as it does once per
        // command-poll tick) frees the next caller to request another probe.
        assert!(foundation.shared.take_liveness_check_requested());
        assert_eq!(foundation.try_check_liveness(), Ok(()));
    }

    #[test]
    fn liveness_check_errors_are_content_free() {
        for error in [
            SshLivenessCheckError::NotRunning,
            SshLivenessCheckError::AlreadyRequested,
        ] {
            assert!(!error.to_string().contains("password"));
            assert!(!error.to_string().contains("private"));
        }
    }

    #[test]
    fn liveness_probe_due_fires_on_demand_regardless_of_automatic_deadline() {
        // Issue #55 / ADR 0018: an on-demand request (wake hook or
        // `SshSession::try_check_liveness`) must fire immediately even while
        // the automatic cadence deadline is far in the future - the two
        // triggers are independent. Uses synthetic `Instant`s rather than a
        // real sleep, per #55's acceptance criteria.
        let now = tokio::time::Instant::now();
        let far_future_deadline = now + Duration::from_secs(3600);

        assert!(liveness_probe_due(now, far_future_deadline, true));
    }

    #[test]
    fn liveness_probe_due_fires_once_the_automatic_cadence_deadline_elapses() {
        // The documented always-enabled default (#55): once the automatic
        // deadline has passed, a probe is due even with no on-demand
        // request pending.
        let deadline = tokio::time::Instant::now();
        let after_deadline = deadline + Duration::from_millis(1);

        assert!(liveness_probe_due(after_deadline, deadline, false));
        assert!(liveness_probe_due(deadline, deadline, false));
    }

    #[test]
    fn liveness_probe_due_stays_false_before_the_deadline_without_a_request() {
        let now = tokio::time::Instant::now();
        let future_deadline = now + Duration::from_secs(1);

        assert!(!liveness_probe_due(now, future_deadline, false));
    }

    #[test]
    fn liveness_probe_due_models_a_disabled_automatic_cadence() {
        // Setting the automatic deadline far in the future (as if the
        // cadence were disabled) must not suppress on-demand requests, and
        // must not spuriously fire on its own - this is how #55's "disabled"
        // half of "tests cover disabled/enabled cadence behavior" is
        // exercised without an actual disable setting existing in 0.1.
        let now = tokio::time::Instant::now();
        let effectively_disabled = now + Duration::from_secs(u64::from(u32::MAX));

        assert!(!liveness_probe_due(now, effectively_disabled, false));
        assert!(liveness_probe_due(now, effectively_disabled, true));
    }

    #[test]
    fn only_transport_failures_wire_a_policy_into_planner_actions() {
        let policy =
            ReconnectPolicy::new(2, Duration::from_secs(1), Duration::from_secs(2)).unwrap();
        let mut disabled = None;
        assert_eq!(
            reconnect_action_for_failure(&mut disabled, ConnectionFailure::Transport),
            ReconnectAction::None
        );

        let mut planner = Some(ReconnectPlanner::new(policy));
        assert_eq!(
            reconnect_action_for_failure(&mut planner, ConnectionFailure::HostTrust),
            ReconnectAction::None
        );
        assert_eq!(
            reconnect_action_for_failure(&mut planner, ConnectionFailure::Authentication),
            ReconnectAction::None
        );
        assert_eq!(
            reconnect_action_for_failure(&mut planner, ConnectionFailure::Setup),
            ReconnectAction::None
        );
        assert_eq!(
            reconnect_action_for_failure(&mut planner, ConnectionFailure::Transport),
            ReconnectAction::ScheduleAttempt {
                attempt: 1,
                delay: Duration::from_secs(1),
            }
        );
        assert!(matches!(
            planner.as_mut().unwrap().delay_elapsed(),
            ReconnectAction::StartFreshConnection {
                host_verification: FreshHostVerification::Required,
                ..
            }
        ));
        assert_eq!(
            reconnect_action_for_failure(&mut planner, ConnectionFailure::Transport),
            ReconnectAction::ScheduleAttempt {
                attempt: 2,
                delay: Duration::from_secs(2),
            }
        );
    }

    #[test]
    fn reconnect_planner_schedules_bounded_exponential_attempts() {
        let policy =
            ReconnectPolicy::new(3, Duration::from_secs(1), Duration::from_secs(3)).unwrap();
        let mut planner = ReconnectPlanner::new(policy);

        assert_eq!(
            planner.disconnected(),
            ReconnectAction::ScheduleAttempt {
                attempt: 1,
                delay: Duration::from_secs(1),
            }
        );
        assert_eq!(
            planner.delay_elapsed(),
            ReconnectAction::StartFreshConnection {
                attempt: 1,
                host_verification: FreshHostVerification::Required,
            }
        );
        assert_eq!(
            planner.connection_failed(),
            ReconnectAction::ScheduleAttempt {
                attempt: 2,
                delay: Duration::from_secs(2),
            }
        );
        assert_eq!(
            planner.delay_elapsed(),
            ReconnectAction::StartFreshConnection {
                attempt: 2,
                host_verification: FreshHostVerification::Required,
            }
        );
        assert_eq!(
            planner.connection_failed(),
            ReconnectAction::ScheduleAttempt {
                attempt: 3,
                delay: Duration::from_secs(3),
            }
        );
        assert_eq!(
            planner.delay_elapsed(),
            ReconnectAction::StartFreshConnection {
                attempt: 3,
                host_verification: FreshHostVerification::Required,
            }
        );
        assert_eq!(planner.connection_failed(), ReconnectAction::Exhausted);
        assert_eq!(planner.state(), ReconnectState::Exhausted);
        assert_eq!(planner.disconnected(), ReconnectAction::None);
    }

    #[test]
    fn reconnect_planner_caps_delay_without_duration_overflow() {
        let maximum = Duration::from_secs(u64::MAX);
        let initial = Duration::from_secs(u64::MAX - 1);
        let policy = ReconnectPolicy::new(2, initial, maximum).unwrap();
        let mut planner = ReconnectPlanner::new(policy);

        assert_eq!(
            planner.disconnected(),
            ReconnectAction::ScheduleAttempt {
                attempt: 1,
                delay: initial,
            }
        );
        assert!(matches!(
            planner.delay_elapsed(),
            ReconnectAction::StartFreshConnection { attempt: 1, .. }
        ));
        assert_eq!(
            planner.connection_failed(),
            ReconnectAction::ScheduleAttempt {
                attempt: 2,
                delay: maximum,
            }
        );
    }

    #[test]
    fn reconnect_planner_cancels_and_resets_without_an_immediate_loop() {
        let policy =
            ReconnectPolicy::new(2, Duration::from_secs(1), Duration::from_secs(2)).unwrap();
        let mut planner = ReconnectPlanner::new(policy);

        assert!(matches!(
            planner.disconnected(),
            ReconnectAction::ScheduleAttempt { attempt: 1, .. }
        ));
        assert_eq!(planner.cancel(), ReconnectAction::Cancelled);
        assert_eq!(planner.state(), ReconnectState::Cancelled);
        assert_eq!(planner.delay_elapsed(), ReconnectAction::None);
        assert_eq!(planner.connection_failed(), ReconnectAction::None);
        assert_eq!(planner.reset(), ReconnectAction::Reset);
        assert_eq!(planner.state(), ReconnectState::Idle);
        assert_eq!(
            planner.disconnected(),
            ReconnectAction::ScheduleAttempt {
                attempt: 1,
                delay: Duration::from_secs(1),
            }
        );
    }

    #[test]
    fn reconnect_planner_requires_fresh_host_trust_for_every_attempt() {
        let policy =
            ReconnectPolicy::new(2, Duration::from_secs(1), Duration::from_secs(2)).unwrap();
        let mut planner = ReconnectPlanner::new(policy);

        let first = planner.disconnected();
        assert!(matches!(
            first,
            ReconnectAction::ScheduleAttempt { attempt: 1, .. }
        ));
        let first_connection = planner.delay_elapsed();
        assert_eq!(
            first_connection,
            ReconnectAction::StartFreshConnection {
                attempt: 1,
                host_verification: FreshHostVerification::Required,
            }
        );
        assert!(matches!(
            planner.connection_failed(),
            ReconnectAction::ScheduleAttempt { attempt: 2, .. }
        ));
        assert_eq!(
            planner.delay_elapsed(),
            ReconnectAction::StartFreshConnection {
                attempt: 2,
                host_verification: FreshHostVerification::Required,
            }
        );
    }

    #[test]
    fn openssh_import_handles_comments_quotes_and_alias_expansion() {
        let report = import_openssh_config(
            r#"
                # The literal identity path is metadata, not a file read.
                Host work # trailing comment
                    HostName "Example.COM"
                    Port=2200
                    User 'alice'
                    IdentityFile "$HOME/.ssh/work key" # no environment expansion
            "#,
        );

        assert!(!report.has_errors());
        assert!(report.diagnostics().is_empty());
        let profile = report.profiles().first().unwrap();
        assert_eq!(profile.host_alias(), "work");
        assert_eq!(profile.identity().host(), "example.com");
        assert_eq!(profile.identity().port(), 2200);
        assert_eq!(profile.username(), Some("alice"));
        assert_eq!(
            profile.identity_file().map(IdentityFileMetadata::path),
            Some("$HOME/.ssh/work key")
        );
    }

    #[test]
    fn openssh_import_uses_an_exact_alias_when_hostname_is_omitted() {
        let report = import_openssh_config(
            r#"
                Host build-server
                    User build
            "#,
        );

        assert!(!report.has_errors());
        let profile = report.profiles().first().unwrap();
        assert_eq!(profile.host_alias(), "build-server");
        assert_eq!(profile.identity().host(), "build-server");
        assert_eq!(profile.identity().port(), DEFAULT_SSH_PORT);
        assert_eq!(profile.username(), Some("build"));
    }

    #[test]
    fn openssh_import_reports_unsafe_directives_without_applying_them() {
        let report = import_openssh_config(
            r#"
                Host safe
                    HostName safe.example
                Host proxy
                    ProxyCommand ssh jump.example -W %h:%p
                Host multiplexed
                    ControlPath ~/.ssh/control-%r@%h:%p
            "#,
        );

        assert_eq!(report.profiles().len(), 1);
        assert_eq!(report.profiles()[0].host_alias(), "safe");
        assert!(report.has_errors());
        assert!(report.diagnostics().iter().any(|diagnostic| matches!(
            diagnostic.kind(),
            OpenSshConfigDiagnosticKind::UnsupportedDirective { directive }
                if directive == "proxycommand"
        )));
        assert!(report.diagnostics().iter().any(|diagnostic| matches!(
            diagnostic.kind(),
            OpenSshConfigDiagnosticKind::UnsupportedDirective { directive }
                if directive == "controlpath"
        )));
    }

    #[test]
    fn openssh_import_rejects_ambiguous_and_unprocessed_config_sections() {
        let wildcard = import_openssh_config("Host *.example\n    User alice\n");
        assert!(wildcard.profiles().is_empty());
        assert!(wildcard.diagnostics().iter().any(|diagnostic| matches!(
            diagnostic.kind(),
            OpenSshConfigDiagnosticKind::WildcardHostPattern
        )));

        let duplicate = import_openssh_config(
            "Host one\n    HostName first.example\nHost one\n    HostName second.example\n",
        );
        assert!(duplicate.profiles().is_empty());
        assert!(duplicate.diagnostics().iter().all(|diagnostic| matches!(
            diagnostic.kind(),
            OpenSshConfigDiagnosticKind::DuplicateHostAlias { .. }
        )));

        let include = import_openssh_config("Include ~/.ssh/conf.d/*\nHost one\n");
        assert!(include.profiles().is_empty());
        assert!(include.diagnostics().iter().any(|diagnostic| matches!(
            diagnostic.kind(),
            OpenSshConfigDiagnosticKind::UnsupportedDirective { directive }
                if directive == "include"
        )));

        let matched = import_openssh_config("Match host one\n    User alice\nHost one\n");
        assert!(matched.profiles().is_empty());
        assert!(matched.diagnostics().iter().any(|diagnostic| matches!(
            diagnostic.kind(),
            OpenSshConfigDiagnosticKind::UnsupportedDirective { directive }
                if directive == "match"
        )));
    }

    #[test]
    fn openssh_import_rejects_duplicate_directives_and_multiple_host_patterns() {
        let duplicate_directive = import_openssh_config("Host one\n    Port 22\n    Port 2200\n");
        assert!(duplicate_directive.profiles().is_empty());
        assert!(duplicate_directive
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(
                diagnostic.kind(),
                OpenSshConfigDiagnosticKind::DuplicateDirective { directive }
                    if directive == "port"
            )));

        let multiple_patterns = import_openssh_config("Host one two\n");
        assert!(multiple_patterns.profiles().is_empty());
        assert!(multiple_patterns
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(
                diagnostic.kind(),
                OpenSshConfigDiagnosticKind::MultipleHostPatterns
            )));
    }

    #[test]
    fn worker_command_queue_is_bounded_and_rejects_large_input() {
        let (worker, receiver, _, _) = SshWorkerFoundation::new_with_capacities(
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
    fn worker_rejects_input_and_resize_while_reconnecting() {
        let (worker, receiver, _, _) = SshWorkerFoundation::new_with_capacities(
            profile(),
            2,
            4,
            noop_session_event_notifier(),
        );
        worker.shared.set_reconnecting(true);

        assert_eq!(
            worker.try_send_input(b"must-not-be-queued"),
            Err(SessionSendError::Closed {
                operation: SessionOperation::Input,
            })
        );
        assert_eq!(
            worker.try_resize(TerminalSize::new(100, 40).unwrap()),
            Err(SessionSendError::Closed {
                operation: SessionOperation::Resize,
            })
        );
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn worker_retains_the_latest_resize_while_connecting_without_an_error() {
        let (worker, receiver, _, _) = SshWorkerFoundation::new_with_capacities(
            profile(),
            2,
            4,
            noop_session_event_notifier(),
        );
        let requested = TerminalSize::new(132, 43).unwrap();
        worker.try_resize(requested).unwrap();

        assert!(!process_commands_before_running(
            &receiver,
            &worker.shared,
            &HostKeyDecisionGate::new(),
        ));
        assert_eq!(worker.shared.desired_terminal_size(), requested);
        assert_eq!(worker.shared.take_pre_running_resize(), Some(requested));
        assert_eq!(worker.shared.take_pre_running_resize(), None);
        assert!(matches!(
            worker.try_recv_event(),
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Starting))
        ));
        assert!(matches!(
            worker.try_recv_event(),
            Err(SessionTryReceiveError::Empty)
        ));
    }

    #[test]
    fn worker_events_update_lifecycle_metrics_and_notifier() {
        let notifier = Arc::new(CountingNotifier::default());
        let (worker, _receiver, _, _) =
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
        let (worker, _receiver, resolver, _) = SshWorkerFoundation::new(profile());
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
        let (worker, _receiver, resolver, _) = SshWorkerFoundation::new(profile());
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
        let (worker, _receiver, resolver, _) = SshWorkerFoundation::new(profile());
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
        let (worker, _receiver, resolver, _) = SshWorkerFoundation::new_with_capacities(
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
    fn password_gate_rejects_missing_timeout_cancel_and_invalid_resolutions() {
        let (worker, _receiver, _host_key_resolver, resolver) = SshWorkerFoundation::new(profile());
        assert_eq!(
            resolver.resolve(
                &festerm_session::PasswordPrompt::new("fes", "example.com", 1, false),
                "irrelevant".to_owned()
            ),
            Err(PasswordDecisionResolutionError::NoPendingPrompt)
        );

        let timed_out = worker.request_password_verification(1, false).unwrap();
        assert_eq!(timed_out.wait(&worker.password_gate, Duration::ZERO), None);

        let cancelled = worker.request_password_verification(2, true).unwrap();
        resolver.cancel(&cancelled.prompt).unwrap();
        assert_eq!(cancelled.wait(&worker.password_gate, Duration::ZERO), None);

        let resolved = worker.request_password_verification(3, true).unwrap();
        resolver
            .resolve(&resolved.prompt, "typed-password".to_owned())
            .unwrap();
        assert_eq!(
            resolver.resolve(&resolved.prompt, "second-typed-password".to_owned()),
            Err(PasswordDecisionResolutionError::AlreadyResolved)
        );
        assert_eq!(
            resolved.wait(&worker.password_gate, Duration::ZERO),
            Some("typed-password".to_owned())
        );
    }

    #[test]
    fn stale_password_decision_cannot_resolve_a_later_prompt() {
        let (worker, _receiver, _host_key_resolver, resolver) = SshWorkerFoundation::new(profile());
        let expired = worker.request_password_verification(1, false).unwrap();
        let expired_prompt = expired.prompt.clone();
        assert_eq!(expired.wait(&worker.password_gate, Duration::ZERO), None);

        let current = worker.request_password_verification(2, true).unwrap();
        assert_eq!(
            resolver.resolve(&expired_prompt, "stale-password".to_owned()),
            Err(PasswordDecisionResolutionError::PromptMismatch)
        );
        resolver
            .resolve(&current.prompt, "current-password".to_owned())
            .unwrap();
        assert_eq!(
            current.wait(&worker.password_gate, Duration::ZERO),
            Some("current-password".to_owned())
        );
    }

    #[test]
    fn password_prompt_is_rejected_when_the_bounded_event_queue_is_full() {
        let (worker, _receiver, _host_key_resolver, resolver) =
            SshWorkerFoundation::new_with_capacities(
                profile(),
                1,
                1,
                noop_session_event_notifier(),
            );
        assert!(matches!(
            worker.request_password_verification(1, false),
            Err(PasswordVerificationRequestError::EventQueueFull)
        ));
        assert_eq!(
            resolver.cancel(&festerm_session::PasswordPrompt::new(
                "fes",
                "example.com",
                1,
                false
            )),
            Err(PasswordDecisionResolutionError::NoPendingPrompt)
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
        let (worker, _receiver, resolver, _) = SshWorkerFoundation::new(profile());
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
                host_key_rejected: Arc::new(AtomicBool::new(false)),
                expected_fingerprint: None,
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
    fn handler_silently_accepts_a_matching_known_host_fingerprint() {
        let (worker, _receiver, _resolver, _) = SshWorkerFoundation::new(profile());
        let public_key = russh::keys::PublicKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti",
        )
        .unwrap();
        let identity = worker.profile.identity.clone();
        let shared = Arc::clone(&worker.shared);
        let gate = Arc::clone(&worker.host_key_gate);
        let mut handler = SshClientHandler {
            identity,
            shared,
            host_key_gate: gate,
            host_key_rejected: Arc::new(AtomicBool::new(false)),
            expected_fingerprint: Some(
                "SHA256:UCUiLr7Pjs9wFFJMDByLgc3NrtdU344OgUM45wZPcIQ".to_owned(),
            ),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let accepted = runtime
            .block_on(handler.check_server_key(&public_key))
            .unwrap();

        assert!(
            accepted,
            "a matching known-host fingerprint must be accepted"
        );
        loop {
            match worker.try_recv_event() {
                Ok(SessionEvent::HostKeyVerification(_)) => {
                    panic!("a matching known-host fingerprint must never prompt")
                }
                Ok(_) => continue,
                Err(SessionTryReceiveError::Empty | SessionTryReceiveError::Closed) => break,
            }
        }
    }

    #[test]
    fn handler_flags_a_mismatched_known_host_fingerprint_as_a_changed_key_warning() {
        let (worker, _receiver, resolver, _) = SshWorkerFoundation::new(profile());
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
                host_key_rejected: Arc::new(AtomicBool::new(false)),
                expected_fingerprint: Some("SHA256:previouslyTrustedButDifferent".to_owned()),
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
        assert!(prompt.is_key_change());
        assert_eq!(
            prompt.previously_trusted_fingerprint(),
            Some("SHA256:previouslyTrustedButDifferent")
        );
        resolver
            .resolve(&prompt, HostTrustDecision::Reject)
            .unwrap();
        assert!(!callback.join().unwrap());
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
