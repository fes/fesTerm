use std::{collections::HashSet, path::Path};
use std::{fmt, sync::Arc};

use festerm_pty::LocalProfile;
use festerm_secret_store::SecretReference;
use festerm_serial::LineSettings;
use festerm_session::TerminalSize;
use festerm_ssh::{
    is_sha256_fingerprint, HostIdentity, PersistenceProvider, PersistentSessionName,
    SessionStrategy, SshConnectionProfile, SshPortForwardSpec,
};
use serde::{Deserialize, Serialize};

use crate::{
    contains_control_character, contains_secret_bearing_value, default_true, is_true,
    validate_identifier, ConfigError, ConfigErrorKind,
};

const DEFAULT_SSH_PORT: u16 = 22;
const DEFAULT_COLUMNS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const DEFAULT_SERIAL_BAUD_RATE: u32 = 115_200;

/// A serializable configuration boundary around an opaque secret-store ID.
///
/// This type deliberately redacts debug output and exposes the inner value
/// only to the narrow SSH composition API.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct CredentialReference(Arc<SecretReference>);

impl CredentialReference {
    pub(crate) fn new(reference: SecretReference) -> Self {
        Self(Arc::new(reference))
    }

    fn as_secret_reference(&self) -> &SecretReference {
        self.0.as_ref()
    }
}

impl fmt::Debug for CredentialReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialReference(REDACTED)")
    }
}

impl Serialize for CredentialReference {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0.to_persisted_string())
    }
}

impl<'de> Deserialize<'de> for CredentialReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        SecretReference::parse(&value)
            .map(Self::new)
            .map_err(|_| serde::de::Error::custom("invalid opaque credential reference"))
    }
}

/// A reusable local-shell or SSH profile.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Profile {
    Local(LocalProfileConfiguration),
    Ssh(SshProfileConfiguration),
    Serial(SerialProfileConfiguration),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteProfileKind {
    #[default]
    Ssh,
    Sftp,
}

fn is_default_remote_profile_kind(kind: &RemoteProfileKind) -> bool {
    *kind == RemoteProfileKind::default()
}

impl Profile {
    /// Creates a local-shell profile with direct executable arguments.
    pub fn local(
        identifier: impl Into<String>,
        executable: impl Into<String>,
        arguments: Vec<String>,
        working_directory: Option<String>,
    ) -> Result<Self, ConfigError> {
        let profile = Self::Local(LocalProfileConfiguration {
            id: identifier.into(),
            executable: executable.into(),
            arguments,
            working_directory,
            persistence: None,
        });
        profile.validate()?;
        Ok(profile)
    }

    /// Creates an SSH profile with non-secret connection metadata.
    pub fn ssh(
        identifier: impl Into<String>,
        host: impl Into<String>,
        port: u16,
        username: impl Into<String>,
        terminal_type: impl Into<String>,
        initial_columns: u16,
        initial_rows: u16,
    ) -> Result<Self, ConfigError> {
        let profile = Self::Ssh(SshProfileConfiguration {
            id: identifier.into(),
            host: host.into(),
            port,
            username: username.into(),
            terminal_type: terminal_type.into(),
            initial_columns,
            initial_rows,
            credential_id: None,
            credential_kind: CredentialKind::default(),
            persistence: None,
            port_forwards: Vec::new(),
            profile_kind: RemoteProfileKind::Ssh,
            sftp_gui_mode: true,
        });
        profile.validate()?;
        Ok(profile)
    }

    /// Creates an SFTP profile using the shared SSH transport metadata.
    pub fn sftp(
        identifier: impl Into<String>,
        host: impl Into<String>,
        port: u16,
        username: impl Into<String>,
        gui_mode: bool,
    ) -> Result<Self, ConfigError> {
        let profile = Self::Ssh(SshProfileConfiguration {
            id: identifier.into(),
            host: host.into(),
            port,
            username: username.into(),
            terminal_type: default_terminal_type(),
            initial_columns: default_columns(),
            initial_rows: default_rows(),
            credential_id: None,
            credential_kind: CredentialKind::default(),
            persistence: None,
            port_forwards: Vec::new(),
            profile_kind: RemoteProfileKind::Sftp,
            sftp_gui_mode: gui_mode,
        });
        profile.validate()?;
        Ok(profile)
    }

    /// Creates a serial-port profile with default line settings (115200
    /// baud, 8 data bits, no parity, 1 stop bit, no flow control), as
    /// documented in `docs/gui-design.md`.
    pub fn serial_with_defaults(
        identifier: impl Into<String>,
        device: impl Into<String>,
    ) -> Result<Self, ConfigError> {
        Self::serial(
            identifier,
            device,
            DEFAULT_SERIAL_BAUD_RATE,
            SerialDataBits::default(),
            SerialParity::default(),
            SerialStopBits::default(),
            SerialFlowControl::default(),
        )
    }

    /// Creates a serial-port profile with explicit line settings.
    #[allow(clippy::too_many_arguments)]
    pub fn serial(
        identifier: impl Into<String>,
        device: impl Into<String>,
        baud_rate: u32,
        data_bits: SerialDataBits,
        parity: SerialParity,
        stop_bits: SerialStopBits,
        flow_control: SerialFlowControl,
    ) -> Result<Self, ConfigError> {
        let profile = Self::Serial(SerialProfileConfiguration {
            id: identifier.into(),
            device: device.into(),
            baud_rate,
            data_bits,
            parity,
            stop_bits,
            flow_control,
        });
        profile.validate()?;
        Ok(profile)
    }
    ///
    /// This accepts only the validated reference type, so callers cannot put a
    /// raw identifier or a secret value into profile metadata.
    pub fn with_credential_reference(
        self,
        credential_reference: SecretReference,
    ) -> Result<Self, ConfigError> {
        self.with_credential_reference_kind(credential_reference, CredentialKind::Password)
    }

    /// Associates an SSH profile with an opaque native-store credential
    /// reference of the given kind (password or private key). This is the
    /// general form of [`Self::with_credential_reference`], which always
    /// uses [`CredentialKind::Password`].
    pub fn with_credential_reference_kind(
        mut self,
        credential_reference: SecretReference,
        kind: CredentialKind,
    ) -> Result<Self, ConfigError> {
        let Self::Ssh(profile) = &mut self else {
            return Err(ConfigError::new(
                ConfigErrorKind::CredentialReferenceRequiresSshProfile,
            ));
        };
        profile.credential_id = Some(CredentialReference::new(credential_reference));
        profile.credential_kind = kind;
        self.validate()?;
        Ok(self)
    }

    /// Configures a saved local or SSH profile's durable-session provider and name
    /// (ADR 0018). Serial profiles have no durable-session concept (ADR 0023)
    /// and return [`ConfigErrorKind::PersistenceRequiresLocalOrSshProfile`].
    ///
    /// This only changes which remote session a *future* connection for this
    /// profile creates or attaches to; it never claims to convert an
    /// already-live plain shell into a persistent session.
    pub fn with_persistence(
        mut self,
        provider: PersistenceProviderKind,
        session_name: impl Into<String>,
    ) -> Result<Self, ConfigError> {
        let persistence = Some(PersistenceConfiguration::new(provider, session_name));
        match &mut self {
            Self::Local(profile) => profile.persistence = persistence,
            Self::Ssh(_) if provider == PersistenceProviderKind::FestermSessiond => {
                return Err(ConfigError::new(
                    ConfigErrorKind::NativePersistenceRequiresLocalProfile,
                ))
            }
            Self::Ssh(profile) => profile.persistence = persistence,
            Self::Serial(_) => {
                return Err(ConfigError::new(
                    ConfigErrorKind::PersistenceRequiresLocalOrSshProfile,
                ))
            }
        }
        self.validate()?;
        Ok(self)
    }

    /// Returns this profile's stable reusable identifier.
    pub fn identifier(&self) -> &str {
        match self {
            Self::Local(profile) => profile.identifier(),
            Self::Ssh(profile) => profile.identifier(),
            Self::Serial(profile) => profile.identifier(),
        }
    }

    /// Returns local metadata when this is a local-shell profile.
    pub fn as_local(&self) -> Option<&LocalProfileConfiguration> {
        match self {
            Self::Local(profile) => Some(profile),
            Self::Ssh(_) | Self::Serial(_) => None,
        }
    }

    /// Returns SSH metadata when this is an SSH profile.
    pub fn as_ssh(&self) -> Option<&SshProfileConfiguration> {
        match self {
            Self::Ssh(profile) => Some(profile),
            Self::Local(_) | Self::Serial(_) => None,
        }
    }

    /// Returns serial metadata when this is a serial-port profile.
    pub fn as_serial(&self) -> Option<&SerialProfileConfiguration> {
        match self {
            Self::Serial(profile) => Some(profile),
            Self::Local(_) | Self::Ssh(_) => None,
        }
    }

    /// Returns this SSH profile's opaque native-store SSH-password reference, if set.
    pub fn credential_reference(&self) -> Option<&SecretReference> {
        self.as_ssh()
            .and_then(SshProfileConfiguration::credential_reference)
    }

    /// Returns this profile's durable-session provider and name,
    /// if persistence is configured (ADR 0018). Always `None` for serial
    /// profiles, which have no durable-session concept (ADR 0023).
    pub fn persistence(&self) -> Option<&PersistenceConfiguration> {
        match self {
            Self::Local(profile) => profile.persistence(),
            Self::Ssh(profile) => profile.persistence(),
            Self::Serial(_) => None,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        match self {
            Self::Local(profile) => profile.validate(),
            Self::Ssh(profile) => profile.validate(),
            Self::Serial(profile) => profile.validate(),
        }
    }
}

/// When a saved profile was last launched.
///
/// Only a coarse whole-second timestamp is retained, and only for profiles
/// the user actually launches. This is the minimum needed to order and label
/// the launcher's "Last Used" column; it is deliberately not a launch history,
/// a counter, or anything that could reconstruct a session timeline.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileUsageEntry {
    pub(crate) profile: String,
    pub(crate) last_used_unix_seconds: u64,
}

impl ProfileUsageEntry {
    /// Returns the identifier of the profile this record describes.
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Returns the launch time in whole seconds since the Unix epoch.
    pub const fn last_used_unix_seconds(&self) -> u64 {
        self.last_used_unix_seconds
    }
}

/// A persistently trusted SSH host key record (ADR 0020).
///
/// Host public keys and their fingerprints are not secret: this is ordinary
/// non-secret configuration state, unlike [`CredentialReference`], and is
/// stored directly in the configuration document rather than a native
/// secret store. `host`/`port` are stored as plain fields rather than a
/// [`HostIdentity`] because that type has no `Serialize`/`Deserialize`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnownHostEntry {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) sha256_fingerprint: String,
}

impl KnownHostEntry {
    /// Returns the SSH host name or numeric address this record trusts.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Returns the SSH port this record trusts.
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Returns the trusted `SHA256:`-prefixed host-key fingerprint.
    pub fn sha256_fingerprint(&self) -> &str {
        &self.sha256_fingerprint
    }

    pub(crate) fn matches(&self, host: &str, port: u16) -> bool {
        self.host == host && self.port == port
    }

    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        let identity = HostIdentity::new(&self.host, self.port)
            .map_err(|_| ConfigError::new(ConfigErrorKind::InvalidKnownHost))?;
        if contains_control_character(identity.host())
            || contains_secret_bearing_value(identity.host())
        {
            return Err(ConfigError::new(ConfigErrorKind::InvalidKnownHost));
        }
        if !is_sha256_fingerprint(&self.sha256_fingerprint) {
            return Err(ConfigError::new(ConfigErrorKind::InvalidKnownHost));
        }
        Ok(())
    }
}

/// Secret-free metadata for a local PTY launch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalProfileConfiguration {
    id: String,
    executable: String,
    #[serde(default)]
    arguments: Vec<String>,
    #[serde(default)]
    working_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    persistence: Option<PersistenceConfiguration>,
}

impl LocalProfileConfiguration {
    /// Returns this profile's stable reusable identifier.
    pub fn identifier(&self) -> &str {
        &self.id
    }

    /// Returns the direct executable path or command name.
    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// Returns arguments passed directly to the executable.
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    /// Returns the optional working directory without resolving it.
    pub fn working_directory(&self) -> Option<&Path> {
        self.working_directory.as_deref().map(Path::new)
    }

    /// Returns the explicitly configured durable local-session provider/name.
    pub fn persistence(&self) -> Option<&PersistenceConfiguration> {
        self.persistence.as_ref()
    }

    /// Converts safe launch metadata into the PTY backend's launch profile.
    ///
    /// This does not test whether the executable or working directory exists;
    /// those are runtime concerns of the platform session backend.
    pub fn to_local_profile(&self) -> LocalProfile {
        let profile = match &self.persistence {
            Some(persistence)
                if persistence.provider() == PersistenceProviderKind::FestermSessiond =>
            {
                LocalProfile::new(&self.executable).with_arguments(self.arguments.clone())
            }
            Some(persistence) => persistence
                .to_local_profile()
                .expect("validated local persistence must remain valid"),
            None => LocalProfile::new(&self.executable).with_arguments(self.arguments.clone()),
        };
        match &self.working_directory {
            Some(working_directory) => profile.with_working_directory(working_directory),
            None => profile,
        }
    }

    fn validate(&self) -> Result<(), ConfigError> {
        validate_identifier(&self.id)?;
        if self.executable.is_empty()
            || contains_control_character(&self.executable)
            || contains_secret_bearing_value(&self.executable)
        {
            return Err(ConfigError::new(ConfigErrorKind::InvalidLocalProfile));
        }
        if self.arguments.iter().any(|argument| {
            argument.is_empty()
                || contains_control_character(argument)
                || contains_secret_bearing_value(argument)
        }) {
            return Err(ConfigError::new(ConfigErrorKind::InvalidLocalProfile));
        }
        if self.working_directory.as_ref().is_some_and(|directory| {
            directory.is_empty()
                || contains_control_character(directory)
                || contains_secret_bearing_value(directory)
        }) {
            return Err(ConfigError::new(ConfigErrorKind::InvalidLocalProfile));
        }
        if let Some(persistence) = &self.persistence {
            persistence.validate_session_name()?;
        }
        Ok(())
    }
}

/// Distinguishes the kind of secret a profile's native-store credential
/// reference points at, so a stored credential is resolved with the right
/// authentication method instead of always being treated as a password.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    #[default]
    Password,
    PrivateKey,
}

/// Which side of the SSH connection listens for a saved port forward.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshPortForwardDirection {
    Local,
    Remote,
}

/// Secret-free metadata for one saved SSH port-forward rule.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshPortForwardConfiguration {
    direction: SshPortForwardDirection,
    bind_host: String,
    bind_port: u16,
    destination_host: String,
    destination_port: u16,
}

impl SshPortForwardConfiguration {
    /// Creates one validated saved SSH port-forward rule.
    pub fn new(
        direction: SshPortForwardDirection,
        bind_host: impl Into<String>,
        bind_port: u16,
        destination_host: impl Into<String>,
        destination_port: u16,
    ) -> Result<Self, ConfigError> {
        let forward = Self {
            direction,
            bind_host: bind_host.into(),
            bind_port,
            destination_host: destination_host.into(),
            destination_port,
        };
        forward.validate()?;
        Ok(forward)
    }

    /// Returns whether this is a local (`-L`) or remote (`-R`) forward.
    pub const fn direction(&self) -> SshPortForwardDirection {
        self.direction
    }

    /// Returns the listening host/interface for the forward.
    pub fn bind_host(&self) -> &str {
        &self.bind_host
    }

    /// Returns the listening port for the forward.
    pub const fn bind_port(&self) -> u16 {
        self.bind_port
    }

    /// Returns the target host reached after the SSH hop.
    pub fn destination_host(&self) -> &str {
        &self.destination_host
    }

    /// Returns the target port reached after the SSH hop.
    pub const fn destination_port(&self) -> u16 {
        self.destination_port
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.bind_host.is_empty()
            || self.destination_host.is_empty()
            || contains_control_character(&self.bind_host)
            || contains_control_character(&self.destination_host)
            || contains_secret_bearing_value(&self.bind_host)
            || contains_secret_bearing_value(&self.destination_host)
        {
            return Err(ConfigError::new(
                ConfigErrorKind::InvalidSshPortForwardConfiguration,
            ));
        }
        HostIdentity::new(&self.bind_host, self.bind_port)
            .map_err(|_| ConfigError::new(ConfigErrorKind::InvalidSshPortForwardConfiguration))?;
        HostIdentity::new(&self.destination_host, self.destination_port)
            .map_err(|_| ConfigError::new(ConfigErrorKind::InvalidSshPortForwardConfiguration))?;
        Ok(())
    }
}

impl SshPortForwardSpec for SshPortForwardConfiguration {
    fn direction(&self) -> festerm_session::SshPortForwardDirection {
        match self.direction {
            SshPortForwardDirection::Local => festerm_session::SshPortForwardDirection::Local,
            SshPortForwardDirection::Remote => festerm_session::SshPortForwardDirection::Remote,
        }
    }

    fn bind_host(&self) -> &str {
        self.bind_host()
    }

    fn bind_port(&self) -> u16 {
        self.bind_port()
    }

    fn destination_host(&self) -> &str {
        self.destination_host()
    }

    fn destination_port(&self) -> u16 {
        self.destination_port()
    }
}

/// Secret-free metadata for a native SSH connection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshProfileConfiguration {
    id: String,
    host: String,
    #[serde(default = "default_ssh_port")]
    port: u16,
    username: String,
    #[serde(default = "default_terminal_type")]
    terminal_type: String,
    #[serde(default = "default_columns")]
    initial_columns: u16,
    #[serde(default = "default_rows")]
    initial_rows: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) credential_id: Option<CredentialReference>,
    #[serde(default, skip_serializing_if = "is_default_credential_kind")]
    pub(crate) credential_kind: CredentialKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    persistence: Option<PersistenceConfiguration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    port_forwards: Vec<SshPortForwardConfiguration>,
    #[serde(default, skip_serializing_if = "is_default_remote_profile_kind")]
    pub(crate) profile_kind: RemoteProfileKind,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    sftp_gui_mode: bool,
}

fn is_default_credential_kind(kind: &CredentialKind) -> bool {
    *kind == CredentialKind::default()
}

impl SshProfileConfiguration {
    /// Returns this profile's stable reusable identifier.
    pub fn identifier(&self) -> &str {
        &self.id
    }

    /// Returns the SSH host name or numeric address.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Returns the SSH port.
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Returns the SSH user name.
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Returns the remote terminal type.
    pub fn terminal_type(&self) -> &str {
        &self.terminal_type
    }

    /// Returns the initial terminal cell dimensions.
    pub const fn initial_size(&self) -> (u16, u16) {
        (self.initial_columns, self.initial_rows)
    }

    /// Returns the opaque native-store SSH-password reference, if this profile has one.
    ///
    /// The reference identifies a native-store SSH-password record but does
    /// not contain authentication material. It is exposed only for
    /// composition immediately before an operation that needs the password.
    pub fn credential_reference(&self) -> Option<&SecretReference> {
        self.credential_id
            .as_ref()
            .map(CredentialReference::as_secret_reference)
    }

    /// Returns which kind of secret the stored credential reference (if
    /// any) points at. Meaningless when [`Self::credential_reference`] is
    /// `None`.
    pub const fn credential_kind(&self) -> CredentialKind {
        self.credential_kind
    }

    /// Returns this profile's configured durable-session provider and name,
    /// if persistence is enabled (ADR 0018). `None` means this profile is an
    /// ordinary plain-shell SSH session.
    pub fn persistence(&self) -> Option<&PersistenceConfiguration> {
        self.persistence.as_ref()
    }

    /// Returns the saved SSH port-forward rules for future launches of this profile.
    pub fn port_forwards(&self) -> &[SshPortForwardConfiguration] {
        &self.port_forwards
    }

    /// Distinguishes ordinary shell profiles from saved SFTP destinations.
    pub const fn profile_kind(&self) -> RemoteProfileKind {
        self.profile_kind
    }

    /// Whether SFTP launches use the graphical file manager instead of the
    /// terminal transcript.
    pub const fn sftp_gui_mode(&self) -> bool {
        self.sftp_gui_mode
    }

    /// Converts safe metadata into the SSH backend's connection profile.
    pub fn to_connection_profile(&self) -> Result<SshConnectionProfile, ConfigError> {
        let size = TerminalSize::new(self.initial_columns, self.initial_rows)
            .map_err(|_| ConfigError::new(ConfigErrorKind::InvalidSshProfile))?;
        self.to_connection_profile_with_size(size)
    }

    /// Converts safe metadata into the SSH backend's connection profile,
    /// overriding the profile's own stored terminal size. Used when
    /// launching a saved profile into an already-sized window: the launch
    /// should inherit that window's current dimensions rather than the
    /// profile's stored (and often stale) initial size.
    pub fn to_connection_profile_with_size(
        &self,
        size: TerminalSize,
    ) -> Result<SshConnectionProfile, ConfigError> {
        let identity = HostIdentity::new(&self.host, self.port)
            .map_err(|_| ConfigError::new(ConfigErrorKind::InvalidSshProfile))?;
        SshConnectionProfile::new(identity, &self.username, &self.terminal_type, size)
            .map_err(|_| ConfigError::new(ConfigErrorKind::InvalidSshProfile))
    }

    /// Converts this profile's persistence configuration, if any, into the
    /// SSH backend's session strategy (ADR 0018). Returns
    /// [`SessionStrategy::PlainShell`] when no persistence is configured.
    pub fn session_strategy(&self) -> Result<SessionStrategy, ConfigError> {
        self.persistence
            .as_ref()
            .map_or(Ok(SessionStrategy::PlainShell), |persistence| {
                persistence.to_session_strategy()
            })
    }

    /// Returns a validated replacement with a new saved port-forward list.
    pub fn with_port_forwards(
        mut self,
        port_forwards: Vec<SshPortForwardConfiguration>,
    ) -> Result<Self, ConfigError> {
        self.port_forwards = port_forwards;
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        validate_identifier(&self.id)?;
        if self.host.contains("://")
            || self.host.contains('@')
            || contains_control_character(&self.host)
            || contains_secret_bearing_value(&self.host)
            || contains_secret_bearing_value(&self.username)
            || contains_secret_bearing_value(&self.terminal_type)
        {
            return Err(ConfigError::new(ConfigErrorKind::InvalidSshProfile));
        }
        self.to_connection_profile().map(|_| ())?;
        self.session_strategy().map(|_| ())?;
        if self.profile_kind == RemoteProfileKind::Sftp
            && (self.persistence.is_some() || !self.port_forwards.is_empty())
        {
            return Err(ConfigError::new(ConfigErrorKind::InvalidSshProfile));
        }
        let mut bindings = HashSet::with_capacity(self.port_forwards.len());
        for forward in &self.port_forwards {
            forward.validate()?;
            let direction = match forward.direction {
                SshPortForwardDirection::Local => "local",
                SshPortForwardDirection::Remote => "remote",
            };
            if !bindings.insert((direction, forward.bind_host.as_str(), forward.bind_port)) {
                return Err(ConfigError::new(ConfigErrorKind::DuplicateSshPortForward));
            }
        }
        Ok(())
    }
}

/// Secret-free metadata for a serial-port connection (REQ-SESS-011).
///
/// Serial profiles never carry a credential reference: attaching to a local
/// device has no authentication step, unlike SSH.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerialProfileConfiguration {
    id: String,
    device: String,
    #[serde(default = "default_serial_baud_rate")]
    baud_rate: u32,
    #[serde(default)]
    data_bits: SerialDataBits,
    #[serde(default)]
    parity: SerialParity,
    #[serde(default)]
    stop_bits: SerialStopBits,
    #[serde(default)]
    flow_control: SerialFlowControl,
}

impl SerialProfileConfiguration {
    /// Returns this profile's stable reusable identifier.
    pub fn identifier(&self) -> &str {
        &self.id
    }

    /// Returns the serial device path or port name (e.g. `/dev/ttyUSB0`,
    /// `COM3`), exactly as the user or discovery entered it.
    pub fn device(&self) -> &str {
        &self.device
    }

    /// Returns the configured baud rate.
    pub const fn baud_rate(&self) -> u32 {
        self.baud_rate
    }

    /// Returns the configured data-bit width.
    pub const fn data_bits(&self) -> SerialDataBits {
        self.data_bits
    }

    /// Returns the configured parity checking.
    pub const fn parity(&self) -> SerialParity {
        self.parity
    }

    /// Returns the configured stop-bit count.
    pub const fn stop_bits(&self) -> SerialStopBits {
        self.stop_bits
    }

    /// Returns the configured flow-control strategy.
    pub const fn flow_control(&self) -> SerialFlowControl {
        self.flow_control
    }

    /// Converts safe metadata into the serial backend's line settings.
    pub fn to_line_settings(&self) -> Result<LineSettings, ConfigError> {
        LineSettings::new(
            self.device.clone(),
            self.baud_rate,
            self.data_bits.into(),
            self.parity.into(),
            self.stop_bits.into(),
            self.flow_control.into(),
        )
        .map_err(|_| ConfigError::new(ConfigErrorKind::InvalidSerialProfile))
    }

    fn validate(&self) -> Result<(), ConfigError> {
        validate_identifier(&self.id)?;
        if self.device.trim().is_empty()
            || contains_control_character(&self.device)
            || contains_secret_bearing_value(&self.device)
        {
            return Err(ConfigError::new(ConfigErrorKind::InvalidSerialProfile));
        }
        self.to_line_settings().map(|_| ())
    }
}

fn default_serial_baud_rate() -> u32 {
    DEFAULT_SERIAL_BAUD_RATE
}

/// Serializable mirror of [`festerm_serial::DataBits`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialDataBits {
    Five,
    Six,
    Seven,
    #[default]
    Eight,
}

impl From<SerialDataBits> for festerm_serial::DataBits {
    fn from(value: SerialDataBits) -> Self {
        match value {
            SerialDataBits::Five => Self::Five,
            SerialDataBits::Six => Self::Six,
            SerialDataBits::Seven => Self::Seven,
            SerialDataBits::Eight => Self::Eight,
        }
    }
}

/// Serializable mirror of [`festerm_serial::Parity`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialParity {
    #[default]
    None,
    Odd,
    Even,
}

impl From<SerialParity> for festerm_serial::Parity {
    fn from(value: SerialParity) -> Self {
        match value {
            SerialParity::None => Self::None,
            SerialParity::Odd => Self::Odd,
            SerialParity::Even => Self::Even,
        }
    }
}

/// Serializable mirror of [`festerm_serial::StopBits`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialStopBits {
    #[default]
    One,
    Two,
}

impl From<SerialStopBits> for festerm_serial::StopBits {
    fn from(value: SerialStopBits) -> Self {
        match value {
            SerialStopBits::One => Self::One,
            SerialStopBits::Two => Self::Two,
        }
    }
}

/// Serializable mirror of [`festerm_serial::FlowControl`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialFlowControl {
    #[default]
    None,
    Software,
    Hardware,
}

impl From<SerialFlowControl> for festerm_serial::FlowControl {
    fn from(value: SerialFlowControl) -> Self {
        match value {
            SerialFlowControl::None => Self::None,
            SerialFlowControl::Software => Self::Software,
            SerialFlowControl::Hardware => Self::Hardware,
        }
    }
}

impl From<festerm_serial::DataBits> for SerialDataBits {
    fn from(value: festerm_serial::DataBits) -> Self {
        match value {
            festerm_serial::DataBits::Five => Self::Five,
            festerm_serial::DataBits::Six => Self::Six,
            festerm_serial::DataBits::Seven => Self::Seven,
            festerm_serial::DataBits::Eight => Self::Eight,
        }
    }
}

impl From<festerm_serial::Parity> for SerialParity {
    fn from(value: festerm_serial::Parity) -> Self {
        match value {
            festerm_serial::Parity::None => Self::None,
            festerm_serial::Parity::Odd => Self::Odd,
            festerm_serial::Parity::Even => Self::Even,
        }
    }
}

impl From<festerm_serial::StopBits> for SerialStopBits {
    fn from(value: festerm_serial::StopBits) -> Self {
        match value {
            festerm_serial::StopBits::One => Self::One,
            festerm_serial::StopBits::Two => Self::Two,
        }
    }
}

impl From<festerm_serial::FlowControl> for SerialFlowControl {
    fn from(value: festerm_serial::FlowControl) -> Self {
        match value {
            festerm_serial::FlowControl::None => Self::None,
            festerm_serial::FlowControl::Software => Self::Software,
            festerm_serial::FlowControl::Hardware => Self::Hardware,
        }
    }
}

/// Non-secret configuration selecting a durable-session provider and name
/// for a Local or SSH profile (ADRs 0018 and 0025).
///
/// Absent by default: an SSH profile with no `PersistenceConfiguration` is an
/// ordinary plain-shell session (`SessionStrategy::PlainShell`). Storing
/// this only changes which remote session a *future* connection creates or
/// attaches to; it never retroactively claims to wrap or capture an
/// already-live plain shell.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistenceConfiguration {
    provider: PersistenceProviderKind,
    session_name: String,
}

impl PersistenceConfiguration {
    /// Creates persistence metadata for `provider`/`session_name`.
    ///
    /// This does not itself validate `session_name`; validation happens on
    /// first use via [`Self::to_session_strategy`] (and, transitively,
    /// whenever the owning profile is constructed or validated), consistent
    /// with how other profile fields are validated.
    pub fn new(provider: PersistenceProviderKind, session_name: impl Into<String>) -> Self {
        Self {
            provider,
            session_name: session_name.into(),
        }
    }

    /// Returns the configured durable-session provider.
    pub const fn provider(&self) -> PersistenceProviderKind {
        self.provider
    }

    /// Returns the configured durable-session name.
    pub fn session_name(&self) -> &str {
        &self.session_name
    }

    /// Converts this configuration into the SSH backend's session strategy,
    /// validating the durable-session name against
    /// [`PersistentSessionName`]'s conservative character-set restriction.
    pub fn to_session_strategy(&self) -> Result<SessionStrategy, ConfigError> {
        let session_name = self.validate_session_name()?;
        let provider = self.provider.to_backend().ok_or_else(|| {
            ConfigError::new(ConfigErrorKind::NativePersistenceRequiresLocalProfile)
        })?;
        Ok(SessionStrategy::Persistent {
            provider,
            session_name,
        })
    }

    /// Builds the direct local provider command for an explicitly persistent
    /// saved local profile. The built-in Local Shell never calls this path.
    pub fn to_local_profile(&self) -> Result<LocalProfile, ConfigError> {
        let session_name = self.validate_session_name()?;
        let profile = match self.provider {
            // The trailing `;` is passed as its own argv element (not a
            // shell string), which tmux recognizes as a command separator
            // even without a shell in between. This keeps the local session
            // bare in the same way as the remote SSH provider (ADR 0018):
            // no status bar, matching an otherwise undecorated fesTerm tab.
            PersistenceProviderKind::Tmux => LocalProfile::new("tmux").with_arguments([
                "new-session",
                "-A",
                "-s",
                session_name.as_str(),
                ";",
                "set-option",
                "-t",
                session_name.as_str(),
                "status",
                "off",
            ]),
            PersistenceProviderKind::Screen => LocalProfile::new("screen")
                .with_arguments(["-xRR", session_name.as_str()])
                .with_unix_hangup_on_shutdown(),
            PersistenceProviderKind::FestermSessiond => {
                return Err(ConfigError::new(
                    ConfigErrorKind::NativePersistenceRequiresLocalProfile,
                ))
            }
        };
        Ok(profile)
    }

    /// Validates and returns the provider-independent durable-session name.
    pub fn validate_session_name(&self) -> Result<PersistentSessionName, ConfigError> {
        PersistentSessionName::new(self.session_name.clone())
            .map_err(|_| ConfigError::new(ConfigErrorKind::InvalidPersistenceConfiguration))
    }
}

/// Which durable-session provider a profile uses (ADRs 0018 and 0025).
///
/// This mirrors `festerm_ssh::PersistenceProvider`, which is deliberately not
/// `Serialize`/`Deserialize` itself so the SSH backend's protocol/session
/// types stay independent of the configuration document format.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PersistenceProviderKind {
    Tmux,
    Screen,
    FestermSessiond,
}

impl PersistenceProviderKind {
    const fn to_backend(self) -> Option<PersistenceProvider> {
        match self {
            Self::Tmux => Some(PersistenceProvider::Tmux),
            Self::Screen => Some(PersistenceProvider::Screen),
            Self::FestermSessiond => None,
        }
    }

    /// A short, user-displayable name for this provider.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Tmux => PersistenceProvider::Tmux.label(),
            Self::Screen => PersistenceProvider::Screen.label(),
            Self::FestermSessiond => "fesTerm session daemon",
        }
    }

    /// The default durable-session provider for a newly created Local
    /// profile, given whether `tmux`/GNU `screen` were found on the local
    /// `PATH`.
    ///
    /// Prefers tmux, then screen, then falls back to fesTerm's own bundled
    /// session daemon (always available since it ships with fesTerm itself,
    /// unlike tmux/screen which the user must have installed separately).
    /// This mirrors the remote SSH profile default (tmux, then screen) but
    /// adds the native fallback tier that remote persistence, which has no
    /// local-daemon equivalent, does not need.
    pub const fn default_for_local_session(tmux_available: bool, screen_available: bool) -> Self {
        if tmux_available {
            Self::Tmux
        } else if screen_available {
            Self::Screen
        } else {
            Self::FestermSessiond
        }
    }
}

const fn default_ssh_port() -> u16 {
    DEFAULT_SSH_PORT
}

fn default_terminal_type() -> String {
    SshConnectionProfile::DEFAULT_TERMINAL_TYPE.to_owned()
}

const fn default_columns() -> u16 {
    DEFAULT_COLUMNS
}

const fn default_rows() -> u16 {
    DEFAULT_ROWS
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_ssh::{PersistenceProvider, PersistentSessionName, SessionStrategy};

    const CREDENTIAL_REFERENCE: &str = "550e8400-e29b-41d4-a716-446655440000";

    fn ssh_port_forward(
        direction: SshPortForwardDirection,
        bind_host: &str,
        bind_port: u16,
        destination_host: &str,
        destination_port: u16,
    ) -> SshPortForwardConfiguration {
        SshPortForwardConfiguration::new(
            direction,
            bind_host,
            bind_port,
            destination_host,
            destination_port,
        )
        .expect("test SSH port forward is valid")
    }

    #[test]
    fn default_for_local_session_prefers_tmux_then_screen_then_native() {
        assert_eq!(
            PersistenceProviderKind::default_for_local_session(true, true),
            PersistenceProviderKind::Tmux
        );
        assert_eq!(
            PersistenceProviderKind::default_for_local_session(true, false),
            PersistenceProviderKind::Tmux
        );
        assert_eq!(
            PersistenceProviderKind::default_for_local_session(false, true),
            PersistenceProviderKind::Screen
        );
        assert_eq!(
            PersistenceProviderKind::default_for_local_session(false, false),
            PersistenceProviderKind::FestermSessiond
        );
    }

    #[test]
    fn credential_references_require_an_ssh_profile() {
        let error = Profile::local("local", "sh", Vec::new(), None)
            .unwrap()
            .with_credential_reference(SecretReference::parse(CREDENTIAL_REFERENCE).unwrap())
            .unwrap_err();
        assert_eq!(
            error.kind(),
            ConfigErrorKind::CredentialReferenceRequiresSshProfile
        );
    }

    #[test]
    fn serial_persistence_and_credential_reference_are_rejected() {
        let profile = Profile::serial_with_defaults("bench-mcu", "/dev/ttyUSB0").unwrap();
        let error = profile
            .with_persistence(PersistenceProviderKind::Tmux, "session")
            .unwrap_err();
        assert_eq!(
            error.kind(),
            ConfigErrorKind::PersistenceRequiresLocalOrSshProfile
        );

        let credential_error = Profile::serial_with_defaults("bench-mcu", "/dev/ttyUSB0")
            .unwrap()
            .with_credential_reference(SecretReference::parse(CREDENTIAL_REFERENCE).unwrap())
            .unwrap_err();
        assert_eq!(
            credential_error.kind(),
            ConfigErrorKind::CredentialReferenceRequiresSshProfile
        );
    }

    #[test]
    fn constructors_cannot_create_invalid_profiles() {
        assert_eq!(
            Profile::local("", "sh", Vec::new(), None)
                .unwrap_err()
                .kind(),
            ConfigErrorKind::InvalidProfileIdentifier
        );
        assert_eq!(
            Profile::ssh(
                "remote",
                "example.test",
                0,
                "alice",
                "xterm-256color",
                80,
                24
            )
            .unwrap_err()
            .kind(),
            ConfigErrorKind::InvalidSshProfile
        );
    }

    #[test]
    fn sftp_profiles_reject_ssh_only_persistence_and_port_forwards() {
        let profile = Profile::sftp("files", "sftp.example.test", 22, "deploy", true).unwrap();
        assert!(profile
            .clone()
            .with_persistence(PersistenceProviderKind::Tmux, "files")
            .is_err());
        let forward = ssh_port_forward(
            SshPortForwardDirection::Local,
            "127.0.0.1",
            8080,
            "app.internal",
            80,
        );
        let Profile::Ssh(sftp) = profile else {
            unreachable!("Profile::sftp returns SSH transport metadata");
        };
        assert!(sftp.with_port_forwards(vec![forward]).is_err());
    }

    #[test]
    fn ssh_profile_without_persistence_reports_a_plain_shell_strategy() {
        let profile = Profile::ssh(
            "remote",
            "example.test",
            22,
            "alice",
            "xterm-256color",
            80,
            24,
        )
        .unwrap();

        let strategy = profile.as_ssh().unwrap().session_strategy().unwrap();

        assert_eq!(strategy, SessionStrategy::PlainShell);
    }

    #[test]
    fn ssh_profile_with_persistence_reports_a_persistent_strategy() {
        let profile = Profile::ssh(
            "remote",
            "example.test",
            22,
            "alice",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_persistence(PersistenceProviderKind::Screen, "editor")
        .unwrap();

        let strategy = profile.as_ssh().unwrap().session_strategy().unwrap();

        assert_eq!(
            strategy,
            SessionStrategy::Persistent {
                provider: PersistenceProvider::Screen,
                session_name: PersistentSessionName::new("editor").unwrap(),
            }
        );
    }

    #[test]
    fn saved_local_profile_can_explicitly_use_named_tmux_persistence() {
        let profile = Profile::local("local", "/bin/sh", vec!["-l".to_owned()], None)
            .unwrap()
            .with_persistence(PersistenceProviderKind::Tmux, "build")
            .unwrap();
        let local = profile.as_local().unwrap();

        assert_eq!(
            local.persistence().unwrap().provider(),
            PersistenceProviderKind::Tmux
        );
        let launch = local.to_local_profile();
        assert_eq!(launch.executable(), Path::new("tmux"));
        assert_eq!(
            launch
                .arguments()
                .iter()
                .map(|argument| argument.to_str())
                .collect::<Vec<_>>(),
            vec![
                Some("new-session"),
                Some("-A"),
                Some("-s"),
                Some("build"),
                Some(";"),
                Some("set-option"),
                Some("-t"),
                Some("build"),
                Some("status"),
                Some("off"),
            ]
        );
    }

    #[test]
    fn saved_local_screen_profiles_alone_request_graceful_hangup() {
        let fresh = Profile::local(
            "local",
            "/bin/sh",
            vec!["-l".to_owned()],
            Some("/tmp".to_owned()),
        )
        .unwrap();
        assert!(!fresh
            .as_local()
            .unwrap()
            .to_local_profile()
            .hangup_on_shutdown());
        for provider in [
            PersistenceProviderKind::Tmux,
            PersistenceProviderKind::Screen,
            PersistenceProviderKind::FestermSessiond,
        ] {
            let profile = fresh.clone().with_persistence(provider, "build").unwrap();
            let launch = profile.as_local().unwrap().to_local_profile();
            assert_eq!(
                launch.hangup_on_shutdown(),
                provider == PersistenceProviderKind::Screen
            );
            assert_eq!(launch.working_directory(), Some(Path::new("/tmp")));
            if provider == PersistenceProviderKind::Screen {
                assert_eq!(launch.executable(), Path::new("screen"));
                assert_eq!(
                    launch.arguments(),
                    [
                        std::ffi::OsString::from("-xRR"),
                        std::ffi::OsString::from("build"),
                    ]
                );
            }
        }
    }

    #[test]
    fn native_persistence_is_local_only_and_preserves_the_shell_profile() {
        let local = Profile::local(
            "local",
            "/bin/sh",
            vec!["-l".to_owned()],
            Some("/tmp".to_owned()),
        )
        .unwrap()
        .with_persistence(PersistenceProviderKind::FestermSessiond, "editor")
        .unwrap();
        let local = local.as_local().unwrap();
        assert_eq!(
            local.persistence().unwrap().provider(),
            PersistenceProviderKind::FestermSessiond
        );
        let launch = local.to_local_profile();
        assert_eq!(launch.executable(), Path::new("/bin/sh"));
        assert_eq!(launch.arguments(), [std::ffi::OsString::from("-l")]);
        assert_eq!(launch.working_directory(), Some(Path::new("/tmp")));

        let error = Profile::ssh(
            "remote",
            "example.test",
            22,
            "alice",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_persistence(PersistenceProviderKind::FestermSessiond, "editor")
        .unwrap_err();
        assert_eq!(
            error.kind(),
            ConfigErrorKind::NativePersistenceRequiresLocalProfile
        );
    }
}
