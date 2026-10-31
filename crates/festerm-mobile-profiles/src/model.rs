use std::fmt;

use festerm_secret_store::SecretReference;
use festerm_ssh::{
    is_sha256_fingerprint, HostIdentity, PersistenceProvider, PersistentSessionName,
    SessionStrategy,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::error::ValidationError;
use crate::tunnel_import::{
    is_identifier, validate_tunnel_name, PublicDestination, MAX_TRUSTED_HOST_FIELD,
};

pub const MAX_NAME_CHARS: usize = 80;
pub const MAX_HOST_BYTES: usize = 253;
pub const MAX_USERNAME_BYTES: usize = 255;
pub const MAX_START_DIR_BYTES: usize = 4096;
pub const DEFAULT_SSH_PORT: u16 = 22;

/// Stable, random identity of a profile. It is not derived from user data.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProfileId(String);

impl ProfileId {
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::new_v4().hyphenated().to_string())
    }

    pub fn parse(value: &str) -> Result<Self, ValidationError> {
        match Uuid::parse_str(value) {
            Ok(id) if id.get_version_num() == 4 && id.hyphenated().to_string() == value => {
                Ok(Self(value.to_owned()))
            }
            _ => Err(ValidationError::new("id", "is not a canonical random id")),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProfileId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for ProfileId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProfileId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// A persisted opaque reference to one secret-store item.
///
/// It is an identifier, not a secret, but it is redacted from `Debug` so
/// logs never correlate profiles with Keychain items.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct SecretRef(String);

impl SecretRef {
    #[must_use]
    pub fn from_reference(reference: &SecretReference) -> Self {
        Self(reference.to_persisted_string())
    }

    /// Converts back to the secret-store type.
    #[must_use]
    pub fn to_reference(&self) -> SecretReference {
        SecretReference::parse(&self.0).expect("SecretRef is validated on construction")
    }

    pub fn parse(value: &str) -> Result<Self, ValidationError> {
        SecretReference::parse(value)
            .map(|reference| Self::from_reference(&reference))
            .map_err(|_| ValidationError::new("secret", "is not a valid opaque reference"))
    }
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretRef(REDACTED)")
    }
}

impl Serialize for SecretRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Public facts about a stored SSH private key, kept in the profile so the UI
/// can show them without touching the Keychain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyInfo {
    /// For example `ssh-ed25519`.
    pub algorithm: String,
    /// `SHA256:<base64>` as shown by `ssh-keygen -l`.
    pub fingerprint: String,
    /// The key's own comment (often `user@host`); may be empty.
    pub comment: String,
    /// The one-line OpenSSH public key to paste into `authorized_keys`.
    pub public_key: String,
}

impl KeyInfo {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.algorithm.is_empty()
            || self.algorithm.len() > 64
            || !self.algorithm.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(ValidationError::new("key.algorithm", "is invalid"));
        }
        if !is_sha256_fingerprint(&self.fingerprint) || self.fingerprint.len() > 128 {
            return Err(ValidationError::new("key.fingerprint", "is invalid"));
        }
        if self.comment.len() > 256 || self.comment.chars().any(char::is_control) {
            return Err(ValidationError::new("key.comment", "is invalid"));
        }
        if self.public_key.is_empty()
            || self.public_key.len() > 2048
            || self.public_key.chars().any(char::is_control)
        {
            return Err(ValidationError::new("key.public_key", "is invalid"));
        }
        Ok(())
    }
}

/// How an SSH or SFTP profile authenticates. Secrets live only in the
/// secret store; `Ask` keeps nothing and prompts at connect time.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum SshAuth {
    Ask,
    Password { secret: SecretRef },
    Key { secret: SecretRef, key: KeyInfo },
}

impl SshAuth {
    pub(crate) fn secret(&self) -> Option<&SecretRef> {
        match self {
            Self::Ask => None,
            Self::Password { secret } | Self::Key { secret, .. } => Some(secret),
        }
    }

    fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Key { key, .. } => key.validate(),
            Self::Ask | Self::Password { .. } => Ok(()),
        }
    }
}

/// The remote session an SSH profile attaches to (ADR 0018).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RemoteSession {
    #[default]
    Plain,
    Tmux {
        name: String,
    },
    Screen {
        name: String,
    },
}

impl RemoteSession {
    /// The `festerm-ssh` strategy for this selection.
    pub fn strategy(&self) -> Result<SessionStrategy, ValidationError> {
        let (provider, name) = match self {
            Self::Plain => return Ok(SessionStrategy::PlainShell),
            Self::Tmux { name } => (PersistenceProvider::Tmux, name),
            Self::Screen { name } => (PersistenceProvider::Screen, name),
        };
        let session_name = PersistentSessionName::new(name.clone())
            .map_err(|_| ValidationError::new("remote_session.name", "is invalid"))?;
        Ok(SessionStrategy::Persistent {
            provider,
            session_name,
        })
    }
}

/// What to do after an unintentional connection loss ("Advanced recovery").
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recovery {
    /// Wait for an explicit user reconnect.
    #[default]
    Manual,
    /// Reattach automatically; only valid with a tmux/Screen session, since a
    /// plain shell cannot be recovered (ADR 0018).
    Automatic,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshProfile {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: String,
    pub auth: SshAuth,
    #[serde(default)]
    pub remote_session: RemoteSession,
    #[serde(default)]
    pub recovery: Recovery,
}

fn default_ssh_port() -> u16 {
    DEFAULT_SSH_PORT
}

impl SshProfile {
    /// A profile with port 22, prompted authentication and a plain shell.
    #[must_use]
    pub fn new(host: impl Into<String>, username: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: DEFAULT_SSH_PORT,
            username: username.into(),
            auth: SshAuth::Ask,
            remote_session: RemoteSession::Plain,
            recovery: Recovery::Manual,
        }
    }

    /// The identity used for host-key trust decisions.
    pub fn host_identity(&self) -> Result<HostIdentity, ValidationError> {
        HostIdentity::new(self.host.clone(), self.port)
            .map_err(|_| ValidationError::new("host", "is invalid"))
    }

    fn validate(&mut self) -> Result<(), ValidationError> {
        validate_destination(&mut self.host, self.port, &self.username)?;
        self.auth.validate()?;
        let strategy = self.remote_session.strategy()?;
        if self.recovery == Recovery::Automatic && !strategy.supports_automatic_recovery() {
            return Err(ValidationError::new(
                "recovery",
                "automatic recovery needs a tmux or Screen session",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SftpProfile {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: String,
    pub auth: SshAuth,
    /// Absolute remote directory (or `~`-relative) the browser opens first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_dir: Option<String>,
}

impl SftpProfile {
    #[must_use]
    pub fn new(host: impl Into<String>, username: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: DEFAULT_SSH_PORT,
            username: username.into(),
            auth: SshAuth::Ask,
            start_dir: None,
        }
    }

    pub fn host_identity(&self) -> Result<HostIdentity, ValidationError> {
        HostIdentity::new(self.host.clone(), self.port)
            .map_err(|_| ValidationError::new("host", "is invalid"))
    }

    fn validate(&mut self) -> Result<(), ValidationError> {
        validate_destination(&mut self.host, self.port, &self.username)?;
        self.auth.validate()?;
        if let Some(dir) = &mut self.start_dir {
            *dir = dir.trim().to_owned();
            if dir.is_empty() {
                self.start_dir = None;
            } else if dir.len() > MAX_START_DIR_BYTES
                || dir.chars().any(char::is_control)
                || !(dir.starts_with('/') || dir.starts_with('~'))
            {
                return Err(ValidationError::new(
                    "start_dir",
                    "must be an absolute or ~ path",
                ));
            }
        }
        Ok(())
    }
}

fn validate_destination(
    host: &mut String,
    port: u16,
    username: &str,
) -> Result<(), ValidationError> {
    let identity = HostIdentity::new(host.clone(), port)
        .map_err(|_| ValidationError::new("host", "is invalid"))?;
    if identity.host().len() > MAX_HOST_BYTES
        || !identity
            .host()
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-_:%[]".contains(&byte))
    {
        return Err(ValidationError::new("host", "is invalid"));
    }
    *host = identity.host().to_owned();
    if username.is_empty()
        || username.len() > MAX_USERNAME_BYTES
        || username
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(ValidationError::new("username", "is invalid"));
    }
    Ok(())
}

/// Which provider an account signs in with. Only GitHub device flow exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountProvider {
    Github,
}

/// A signed-in Dev Tunnels account. Its refresh credential lives in the
/// secret store; any number of profiles share it by `id`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TunnelAccount {
    pub id: String,
    pub provider: AccountProvider,
    pub account_id: u64,
    pub login: String,
    pub credential: SecretRef,
}

impl TunnelAccount {
    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if !is_identifier(&self.id, 64) {
            return Err(ValidationError::new("account.id", "is invalid"));
        }
        let login_ok = !self.login.is_empty()
            && self.login.len() <= 39
            && self
                .login
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        if self.account_id == 0 || !login_ok {
            return Err(ValidationError::new("account", "is invalid"));
        }
        Ok(())
    }
}

/// The host a named tunnel is pinned to, plus nothing secret.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedTunnelHost {
    pub tunnel_id: String,
    pub cluster: String,
    pub host_id: String,
    pub public_key: String,
    pub port: u16,
}

impl TrustedTunnelHost {
    fn validate(&self) -> Result<(), ValidationError> {
        let fields_ok = [
            &self.tunnel_id,
            &self.cluster,
            &self.host_id,
            &self.public_key,
        ]
        .iter()
        .all(|value| {
            !value.is_empty()
                && value.len() <= MAX_TRUSTED_HOST_FIELD
                && !value.chars().any(char::is_control)
        });
        if self.port == 0 || !fields_ok {
            return Err(ValidationError::new("trusted_host", "is invalid"));
        }
        Ok(())
    }
}

/// Where a Dev Tunnel profile finds its host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TunnelSource {
    /// A tunnel found by name through the signed-in account.
    Named {
        tunnel_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trusted_host: Option<TrustedTunnelHost>,
        /// The cached tunnel connect token, in the secret store.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connect_credential: Option<SecretRef>,
    },
    /// Public destination metadata from the one-line installer's
    /// `festerm-profile.json`; contains no credentials.
    Imported { destination: Box<PublicDestination> },
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevTunnelProfile {
    /// Id of the [`TunnelAccount`] to sign in with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub source: TunnelSource,
    /// Existing remote session to attach to; `None` uses the host's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Displace another client already attached to the session. On by default,
    /// matching the installer's default (ADR 0053).
    #[serde(default = "default_true")]
    pub take_over_if_attached: bool,
}

impl DevTunnelProfile {
    /// A named-tunnel profile with take-over enabled.
    #[must_use]
    pub fn named(tunnel_name: impl Into<String>, account: Option<String>) -> Self {
        Self {
            account,
            source: TunnelSource::Named {
                tunnel_name: tunnel_name.into(),
                trusted_host: None,
                connect_credential: None,
            },
            session: None,
            take_over_if_attached: true,
        }
    }

    fn validate(&self) -> Result<(), ValidationError> {
        if self
            .account
            .as_deref()
            .is_some_and(|account| !is_identifier(account, 64))
        {
            return Err(ValidationError::new("account", "is invalid"));
        }
        if self
            .session
            .as_deref()
            .is_some_and(|session| !is_identifier(session, 128))
        {
            return Err(ValidationError::new("session", "is invalid"));
        }
        match &self.source {
            TunnelSource::Named {
                tunnel_name,
                trusted_host,
                connect_credential,
            } => {
                validate_tunnel_name(tunnel_name)?;
                if let Some(host) = trusted_host {
                    host.validate()?;
                }
                if (trusted_host.is_some() || connect_credential.is_some())
                    && self.account.is_none()
                {
                    return Err(ValidationError::new(
                        "account",
                        "is required for a trusted tunnel host",
                    ));
                }
            }
            TunnelSource::Imported { destination } => destination.validate()?,
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProfileKind {
    Ssh(SshProfile),
    DevTunnel(DevTunnelProfile),
    Sftp(SftpProfile),
}

impl ProfileKind {
    pub(crate) fn validate(&mut self) -> Result<(), ValidationError> {
        match self {
            Self::Ssh(profile) => profile.validate(),
            Self::DevTunnel(profile) => profile.validate(),
            Self::Sftp(profile) => profile.validate(),
        }
    }

    /// Every secret-store item this profile owns.
    pub(crate) fn secrets(&self) -> Vec<&SecretRef> {
        match self {
            Self::Ssh(SshProfile { auth, .. }) | Self::Sftp(SftpProfile { auth, .. }) => {
                auth.secret().into_iter().collect()
            }
            Self::DevTunnel(DevTunnelProfile {
                source:
                    TunnelSource::Named {
                        connect_credential, ..
                    },
                ..
            }) => connect_credential.iter().collect(),
            Self::DevTunnel(_) => Vec::new(),
        }
    }
}

/// A saved connection. Documents contain no secret material.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: ProfileId,
    pub name: String,
    /// Seconds since the Unix epoch.
    pub created_at: u64,
    /// Seconds since the Unix epoch of the last successful launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<u64>,
    pub kind: ProfileKind,
}

pub(crate) fn validate_name(name: &mut String) -> Result<(), ValidationError> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > MAX_NAME_CHARS
        || trimmed.chars().any(char::is_control)
    {
        return Err(ValidationError::new("name", "is invalid"));
    }
    *name = trimmed.to_owned();
    Ok(())
}
