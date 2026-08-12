//! Versioned, secret-free application configuration and reusable profiles.
//!
//! This crate deliberately owns document parsing and validation, but not file
//! watching, GUI editing, credentials, or workspace persistence. Configuration
//! documents contain only the safe metadata needed to construct local and SSH
//! connection profiles.

use std::{
    collections::HashSet,
    fmt,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use festerm_pty::LocalProfile;
use festerm_session::TerminalSize;
use festerm_ssh::{HostIdentity, SshConnectionProfile};
use serde::{Deserialize, Serialize};

/// The only document schema accepted by this initial configuration slice.
pub const SCHEMA_VERSION: u32 = 1;

const DEFAULT_SSH_PORT: u16 = 22;
const DEFAULT_COLUMNS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const TEMPORARY_FILE_ATTEMPTS: u32 = 128;

static NEXT_TEMPORARY_FILE_ID: AtomicU64 = AtomicU64::new(0);

/// A validated configuration document.
///
/// Profiles are reusable launch definitions. They intentionally do not encode
/// workspace state, authentication material, or secret-store references.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Configuration {
    schema_version: u32,
    #[serde(default)]
    profiles: Vec<Profile>,
}

impl Configuration {
    /// Creates and validates a document using the current schema version.
    pub fn new(profiles: Vec<Profile>) -> Result<Self, ConfigError> {
        let configuration = Self {
            schema_version: SCHEMA_VERSION,
            profiles,
        };
        configuration.validate()?;
        Ok(configuration)
    }

    /// Returns an empty, valid configuration document.
    pub const fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            profiles: Vec::new(),
        }
    }

    /// Parses a complete TOML candidate and validates it before returning it.
    pub fn parse(document: &str) -> Result<Self, ConfigError> {
        reject_secret_material(document)?;
        let raw: RawConfiguration = toml::from_str(document).map_err(parse_error)?;
        let configuration = Self {
            schema_version: raw.schema_version,
            profiles: raw.profiles,
        };
        configuration.validate()?;
        Ok(configuration)
    }

    /// Serializes this configuration as human-readable TOML after validation.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        self.validate()?;
        toml::to_string_pretty(self).map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))
    }

    /// Loads, parses, and validates a complete configuration file at `path`.
    ///
    /// The caller chooses `path`; this crate does not discover configuration
    /// locations. Returned errors never retain the supplied path or document
    /// contents.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ConfigurationFileError> {
        let document = fs::read_to_string(path.as_ref()).map_err(read_file_error)?;
        Self::parse(&document).map_err(ConfigurationFileError::parse)
    }

    /// Atomically replaces the configuration file at `path` with this document.
    ///
    /// The caller chooses `path`; this crate does not discover configuration
    /// locations. The complete replacement is written and synced in the
    /// target's parent directory before the target is renamed into place.
    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<(), ConfigurationFileError> {
        let path = path.as_ref();
        let document = self
            .to_toml()
            .map_err(ConfigurationFileError::serialization)?;
        let parent = parent_directory(path)?;
        let mut temporary = TemporaryFile::create(parent)?;

        temporary
            .file_mut()
            .write_all(document.as_bytes())
            .map_err(|_| ConfigurationFileError::new(ConfigurationFileErrorKind::WriteTemporary))?;
        temporary
            .file_mut()
            .sync_all()
            .map_err(|_| ConfigurationFileError::new(ConfigurationFileErrorKind::SyncTemporary))?;
        temporary.close_file();

        replace_file(temporary.path(), path)?;
        temporary.persist();
        sync_parent_directory(parent)?;
        Ok(())
    }

    /// Returns the current document schema version.
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns reusable local and SSH profile metadata in document order.
    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// Finds a profile by its validated identifier.
    pub fn profile(&self, identifier: &str) -> Option<&Profile> {
        self.profiles
            .iter()
            .find(|profile| profile.identifier() == identifier)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ConfigError::new(ConfigErrorKind::UnsupportedSchemaVersion));
        }

        let mut identifiers = HashSet::with_capacity(self.profiles.len());
        for profile in &self.profiles {
            validate_identifier(profile.identifier())?;
            if !identifiers.insert(profile.identifier()) {
                return Err(ConfigError::new(
                    ConfigErrorKind::DuplicateProfileIdentifier,
                ));
            }
            profile.validate()?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfiguration {
    schema_version: u32,
    #[serde(default)]
    profiles: Vec<Profile>,
}

impl<'de> Deserialize<'de> for Configuration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawConfiguration::deserialize(deserializer)?;
        let configuration = Self {
            schema_version: raw.schema_version,
            profiles: raw.profiles,
        };
        configuration.validate().map_err(serde::de::Error::custom)?;
        Ok(configuration)
    }
}

/// A reusable local-shell or SSH profile.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Profile {
    Local(LocalProfileConfiguration),
    Ssh(SshProfileConfiguration),
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
        });
        profile.validate()?;
        Ok(profile)
    }

    /// Returns this profile's stable reusable identifier.
    pub fn identifier(&self) -> &str {
        match self {
            Self::Local(profile) => profile.identifier(),
            Self::Ssh(profile) => profile.identifier(),
        }
    }

    /// Returns local metadata when this is a local-shell profile.
    pub fn as_local(&self) -> Option<&LocalProfileConfiguration> {
        match self {
            Self::Local(profile) => Some(profile),
            Self::Ssh(_) => None,
        }
    }

    /// Returns SSH metadata when this is an SSH profile.
    pub fn as_ssh(&self) -> Option<&SshProfileConfiguration> {
        match self {
            Self::Local(_) => None,
            Self::Ssh(profile) => Some(profile),
        }
    }

    fn validate(&self) -> Result<(), ConfigError> {
        match self {
            Self::Local(profile) => profile.validate(),
            Self::Ssh(profile) => profile.validate(),
        }
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

    /// Converts safe launch metadata into the PTY backend's launch profile.
    ///
    /// This does not test whether the executable or working directory exists;
    /// those are runtime concerns of the platform session backend.
    pub fn to_local_profile(&self) -> LocalProfile {
        let profile = LocalProfile::new(&self.executable).with_arguments(self.arguments.clone());
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
        Ok(())
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

    /// Converts safe metadata into the SSH backend's connection profile.
    pub fn to_connection_profile(&self) -> Result<SshConnectionProfile, ConfigError> {
        let identity = HostIdentity::new(&self.host, self.port)
            .map_err(|_| ConfigError::new(ConfigErrorKind::InvalidSshProfile))?;
        let size = TerminalSize::new(self.initial_columns, self.initial_rows)
            .map_err(|_| ConfigError::new(ConfigErrorKind::InvalidSshProfile))?;
        SshConnectionProfile::new(identity, &self.username, &self.terminal_type, size)
            .map_err(|_| ConfigError::new(ConfigErrorKind::InvalidSshProfile))
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
        self.to_connection_profile().map(|_| ())
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

fn validate_identifier(identifier: &str) -> Result<(), ConfigError> {
    let mut bytes = identifier.bytes();
    let Some(first) = bytes.next() else {
        return Err(ConfigError::new(ConfigErrorKind::InvalidProfileIdentifier));
    };
    if identifier.len() > 64
        || !first.is_ascii_lowercase()
        || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || identifier.ends_with('-')
    {
        return Err(ConfigError::new(ConfigErrorKind::InvalidProfileIdentifier));
    }
    Ok(())
}

fn contains_control_character(value: &str) -> bool {
    value.chars().any(char::is_control)
}

fn reject_secret_material(document: &str) -> Result<(), ConfigError> {
    let value: toml::Value = toml::from_str(document).map_err(parse_error)?;
    inspect_value_for_secret_material(&value)
}

fn inspect_value_for_secret_material(value: &toml::Value) -> Result<(), ConfigError> {
    match value {
        toml::Value::String(value) if contains_secret_bearing_value(value) => {
            Err(ConfigError::new(ConfigErrorKind::ForbiddenSecretValue))
        }
        toml::Value::Array(values) => {
            for value in values {
                inspect_value_for_secret_material(value)?;
            }
            Ok(())
        }
        toml::Value::Table(values) => {
            for (key, value) in values {
                if is_secret_bearing_key(key) {
                    return Err(ConfigError::new(ConfigErrorKind::ForbiddenSecretField));
                }
                inspect_value_for_secret_material(value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn is_secret_bearing_key(key: &str) -> bool {
    let normalized: String = key
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(char::from)
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "password"
            | "passphrase"
            | "secret"
            | "token"
            | "privatekey"
            | "credential"
            | "credentials"
            | "identityfile"
            | "keyfile"
    )
}

fn contains_secret_bearing_value(value: &str) -> bool {
    let uppercase = value.to_ascii_uppercase();
    if uppercase.contains("-----BEGIN") && uppercase.contains("PRIVATE KEY-----") {
        return true;
    }

    let option_name = value
        .split_once(['=', ':'])
        .map_or(value, |(name, _)| name)
        .trim_start_matches('-');
    is_secret_bearing_key(option_name)
}

fn parse_error(error: toml::de::Error) -> ConfigError {
    let location = error.span().map(|span| SourceLocation {
        byte_offset: span.start,
    });
    ConfigError {
        kind: ConfigErrorKind::Parse,
        location,
    }
}

/// A content-free location in a TOML candidate document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceLocation {
    byte_offset: usize,
}

impl SourceLocation {
    /// Returns the zero-based byte offset reported by the TOML parser.
    pub const fn byte_offset(self) -> usize {
        self.byte_offset
    }
}

/// Stable categories for configuration parse, validation, and serialization failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigErrorKind {
    Parse,
    UnsupportedSchemaVersion,
    ForbiddenSecretField,
    ForbiddenSecretValue,
    InvalidProfileIdentifier,
    DuplicateProfileIdentifier,
    InvalidLocalProfile,
    InvalidSshProfile,
    Serialization,
}

/// An actionable error that never retains document content or secret values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigError {
    kind: ConfigErrorKind,
    location: Option<SourceLocation>,
}

impl ConfigError {
    const fn new(kind: ConfigErrorKind) -> Self {
        Self {
            kind,
            location: None,
        }
    }

    /// Returns the stable error category for diagnostics and UI policy.
    pub const fn kind(self) -> ConfigErrorKind {
        self.kind
    }

    /// Returns a parser-provided source location when available.
    pub const fn location(self) -> Option<SourceLocation> {
        self.location
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ConfigErrorKind::Parse => {
                formatter.write_str("configuration TOML is invalid or does not match the supported schema")?;
                if let Some(location) = self.location {
                    write!(formatter, " near byte {}", location.byte_offset())?;
                }
                Ok(())
            }
            ConfigErrorKind::UnsupportedSchemaVersion => write!(
                formatter,
                "configuration schema_version is unsupported; expected {SCHEMA_VERSION}"
            ),
            ConfigErrorKind::ForbiddenSecretField => formatter.write_str(
                "configuration contains a forbidden secret-bearing field; keep credentials outside TOML",
            ),
            ConfigErrorKind::ForbiddenSecretValue => formatter.write_str(
                "configuration contains password or private-key material; keep credentials outside TOML",
            ),
            ConfigErrorKind::InvalidProfileIdentifier => formatter.write_str(
                "profiles[].id must be 1-64 lowercase ASCII letters, digits, or internal hyphens",
            ),
            ConfigErrorKind::DuplicateProfileIdentifier => {
                formatter.write_str("profiles[].id values must be unique")
            }
            ConfigErrorKind::InvalidLocalProfile => formatter.write_str(
                "local profile metadata must use non-empty, control-character-free executable, arguments, and working_directory values without secret-bearing options",
            ),
            ConfigErrorKind::InvalidSshProfile => formatter.write_str(
                "SSH profile metadata must contain a host, nonzero port, safe username and terminal type, and at least 2 columns by 1 row",
            ),
            ConfigErrorKind::Serialization => {
                formatter.write_str("configuration could not be serialized")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Stable categories for file loading and atomic-saving failures.
///
/// These categories deliberately exclude operating-system messages, document
/// contents, and caller-supplied paths so they are safe for ordinary
/// diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationFileErrorKind {
    /// The requested file was not present while loading.
    MissingFile,
    /// Reading the requested file failed for a reason other than absence.
    Read,
    /// The loaded TOML could not be parsed or validated.
    Parse,
    /// The supplied target cannot name a regular configuration file.
    InvalidTargetPath,
    /// A temporary file could not be created alongside the target.
    CreateTemporary,
    /// Writing the complete replacement to its temporary file failed.
    WriteTemporary,
    /// Syncing the temporary replacement file failed.
    SyncTemporary,
    /// Renaming the complete replacement into place failed.
    Replace,
    /// Restoring the prior Windows target after a failed replacement failed.
    RestorePrevious,
    /// Cleaning up a completed Windows replacement's prior target failed.
    CleanupPrevious,
    /// Syncing the target directory after replacement failed.
    SyncParentDirectory,
    /// Serializing the validated in-memory configuration failed.
    Serialization,
}

/// A content-free configuration-file diagnostic.
///
/// Parse and validation failures expose their existing [`ConfigError`] through
/// [`Self::configuration_error`]. I/O diagnostics intentionally retain only
/// stable categories, never an operating-system error or supplied path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigurationFileError {
    kind: ConfigurationFileErrorKind,
    configuration_error: Option<ConfigError>,
}

impl ConfigurationFileError {
    const fn new(kind: ConfigurationFileErrorKind) -> Self {
        Self {
            kind,
            configuration_error: None,
        }
    }

    const fn parse(error: ConfigError) -> Self {
        Self {
            kind: ConfigurationFileErrorKind::Parse,
            configuration_error: Some(error),
        }
    }

    const fn serialization(error: ConfigError) -> Self {
        Self {
            kind: ConfigurationFileErrorKind::Serialization,
            configuration_error: Some(error),
        }
    }

    /// Returns the stable category for loading or saving diagnostics.
    pub const fn kind(self) -> ConfigurationFileErrorKind {
        self.kind
    }

    /// Returns the parse, validation, or serialization diagnostic when one
    /// caused this file operation to fail.
    pub const fn configuration_error(self) -> Option<ConfigError> {
        self.configuration_error
    }
}

impl fmt::Display for ConfigurationFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self.kind {
            ConfigurationFileErrorKind::MissingFile => "configuration file is missing",
            ConfigurationFileErrorKind::Read => "configuration file could not be read",
            ConfigurationFileErrorKind::Parse => "configuration file contains invalid content",
            ConfigurationFileErrorKind::InvalidTargetPath => {
                "configuration target must name a file"
            }
            ConfigurationFileErrorKind::CreateTemporary => {
                "configuration replacement file could not be created"
            }
            ConfigurationFileErrorKind::WriteTemporary => {
                "configuration replacement file could not be written"
            }
            ConfigurationFileErrorKind::SyncTemporary => {
                "configuration replacement file could not be synced"
            }
            ConfigurationFileErrorKind::Replace => {
                "configuration replacement could not be installed"
            }
            ConfigurationFileErrorKind::RestorePrevious => {
                "configuration replacement failed and the prior file could not be restored"
            }
            ConfigurationFileErrorKind::CleanupPrevious => {
                "configuration replacement was installed but its prior file could not be cleaned up"
            }
            ConfigurationFileErrorKind::SyncParentDirectory => {
                "configuration replacement was installed but its directory could not be synced"
            }
            ConfigurationFileErrorKind::Serialization => {
                "configuration could not be serialized for saving"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ConfigurationFileError {}

fn read_file_error(error: std::io::Error) -> ConfigurationFileError {
    let kind = if error.kind() == std::io::ErrorKind::NotFound {
        ConfigurationFileErrorKind::MissingFile
    } else {
        ConfigurationFileErrorKind::Read
    };
    ConfigurationFileError::new(kind)
}

fn parent_directory(path: &Path) -> Result<&Path, ConfigurationFileError> {
    if path.file_name().is_none() {
        return Err(ConfigurationFileError::new(
            ConfigurationFileErrorKind::InvalidTargetPath,
        ));
    }
    Ok(path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new(".")))
}

struct TemporaryFile {
    path: PathBuf,
    file: Option<File>,
    persist: bool,
}

impl TemporaryFile {
    fn create(parent: &Path) -> Result<Self, ConfigurationFileError> {
        for _ in 0..TEMPORARY_FILE_ATTEMPTS {
            let path = temporary_path(parent, "tmp");
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                        persist: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => {
                    return Err(ConfigurationFileError::new(
                        ConfigurationFileErrorKind::CreateTemporary,
                    ));
                }
            }
        }
        Err(ConfigurationFileError::new(
            ConfigurationFileErrorKind::CreateTemporary,
        ))
    }

    fn file_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("temporary configuration file is open until replacement")
    }

    fn close_file(&mut self) {
        self.file.take();
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn persist(&mut self) {
        self.persist = true;
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if !self.persist {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn temporary_path(parent: &Path, extension: &str) -> PathBuf {
    let identifier = NEXT_TEMPORARY_FILE_ID.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".festerm-config-{}-{identifier}.{extension}",
        process::id()
    ))
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, target: &Path) -> Result<(), ConfigurationFileError> {
    fs::rename(temporary, target)
        .map_err(|_| ConfigurationFileError::new(ConfigurationFileErrorKind::Replace))
}

#[cfg(windows)]
fn replace_file(temporary: &Path, target: &Path) -> Result<(), ConfigurationFileError> {
    match fs::rename(temporary, target) {
        Ok(()) => Ok(()),
        Err(_) if target.exists() => replace_existing_windows_file(temporary, target),
        Err(_) => Err(ConfigurationFileError::new(
            ConfigurationFileErrorKind::Replace,
        )),
    }
}

#[cfg(windows)]
fn replace_existing_windows_file(
    temporary: &Path,
    target: &Path,
) -> Result<(), ConfigurationFileError> {
    let parent = parent_directory(target)?;
    let previous = rename_previous_windows_file(target, parent)?;

    if fs::rename(temporary, target).is_err() {
        return match fs::rename(&previous, target) {
            Ok(()) => Err(ConfigurationFileError::new(
                ConfigurationFileErrorKind::Replace,
            )),
            Err(_) => Err(ConfigurationFileError::new(
                ConfigurationFileErrorKind::RestorePrevious,
            )),
        };
    }

    fs::remove_file(previous)
        .map_err(|_| ConfigurationFileError::new(ConfigurationFileErrorKind::CleanupPrevious))
}

#[cfg(windows)]
fn rename_previous_windows_file(
    target: &Path,
    parent: &Path,
) -> Result<PathBuf, ConfigurationFileError> {
    for _ in 0..TEMPORARY_FILE_ATTEMPTS {
        let previous = temporary_path(parent, "previous");
        match fs::rename(target, &previous) {
            Ok(()) => return Ok(previous),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                return Err(ConfigurationFileError::new(
                    ConfigurationFileErrorKind::Replace,
                ));
            }
        }
    }
    Err(ConfigurationFileError::new(
        ConfigurationFileErrorKind::Replace,
    ))
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), ConfigurationFileError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ConfigurationFileError::new(ConfigurationFileErrorKind::SyncParentDirectory))
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<(), ConfigurationFileError> {
    Ok(())
}

/// Holds the last accepted configuration and applies complete replacements atomically.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigurationState {
    active: Configuration,
    last_error: Option<ConfigError>,
}

impl ConfigurationState {
    /// Starts with an already validated configuration.
    pub fn new(active: Configuration) -> Self {
        Self {
            active,
            last_error: None,
        }
    }

    /// Returns the last valid configuration.
    pub fn active(&self) -> &Configuration {
        &self.active
    }

    /// Returns the last rejected-reload diagnostic, if any.
    pub const fn last_error(&self) -> Option<ConfigError> {
        self.last_error
    }

    /// Parses and validates a full replacement before modifying active state.
    ///
    /// A rejected candidate leaves `active` untouched and records only a
    /// content-free diagnostic. A successful replacement clears that diagnostic.
    pub fn reload(&mut self, document: &str) -> Result<(), ConfigError> {
        match Configuration::parse(document) {
            Ok(candidate) => {
                self.active = candidate;
                self.last_error = None;
                Ok(())
            }
            Err(error) => {
                self.last_error = Some(error);
                Err(error)
            }
        }
    }

    /// Loads, parses, and validates a complete replacement before modifying
    /// active state.
    ///
    /// A failed file read or invalid candidate leaves `active` untouched. A
    /// rejected candidate records its content-free parse or validation
    /// diagnostic; read failures do not retain operating-system details.
    pub fn reload_from_path(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<(), ConfigurationFileError> {
        match Configuration::load_from_path(path) {
            Ok(candidate) => {
                self.active = candidate;
                self.last_error = None;
                Ok(())
            }
            Err(error) => {
                self.last_error = error.configuration_error();
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPLETE_CONFIGURATION: &str = r#"
schema_version = 1

[[profiles]]
kind = "local"
id = "dev-shell"
executable = "/bin/zsh"
arguments = ["-l"]
working_directory = "/work"

[[profiles]]
kind = "ssh"
id = "build-host"
host = "build.example"
port = 2200
username = "alice"
terminal_type = "xterm-256color"
initial_columns = 132
initial_rows = 43
"#;

    #[test]
    fn parses_serializes_and_converts_secret_free_profiles() {
        let configuration = Configuration::parse(COMPLETE_CONFIGURATION).unwrap();

        assert_eq!(configuration.schema_version(), SCHEMA_VERSION);
        assert_eq!(configuration.profiles().len(), 2);
        assert_eq!(
            configuration.profile("build-host").unwrap().identifier(),
            "build-host"
        );

        let local = configuration.profiles()[0]
            .as_local()
            .unwrap()
            .to_local_profile();
        assert_eq!(local.executable(), Path::new("/bin/zsh"));
        assert_eq!(local.arguments(), &["-l"]);
        assert_eq!(local.working_directory(), Some(Path::new("/work")));

        let ssh = configuration.profiles()[1]
            .as_ssh()
            .unwrap()
            .to_connection_profile()
            .unwrap();
        assert_eq!(ssh.identity().host(), "build.example");
        assert_eq!(ssh.identity().port(), 2200);
        assert_eq!(ssh.username(), "alice");
        assert_eq!(ssh.initial_size().columns(), 132);
        assert_eq!(ssh.initial_size().rows(), 43);

        let serialized = configuration.to_toml().unwrap();
        assert!(serialized.starts_with("schema_version = 1\n"));
        assert_eq!(Configuration::parse(&serialized).unwrap(), configuration);
    }

    #[test]
    fn ssh_defaults_are_explicit_after_parsing() {
        let configuration = Configuration::parse(
            r#"
schema_version = 1

[[profiles]]
kind = "ssh"
id = "default-ssh"
host = "example.test"
username = "alice"
"#,
        )
        .unwrap();

        let ssh = configuration.profiles()[0].as_ssh().unwrap();
        assert_eq!(ssh.port(), 22);
        assert_eq!(ssh.terminal_type(), "xterm-256color");
        assert_eq!(ssh.initial_size(), (80, 24));
    }

    #[test]
    fn rejects_unknown_fields() {
        let error = Configuration::parse(
            r#"
schema_version = 1
unsupported_setting = true
"#,
        )
        .unwrap_err();

        assert_eq!(error.kind(), ConfigErrorKind::Parse);
        assert!(!error.to_string().contains("unsupported_setting"));

        let error = Configuration::parse(
            r#"
schema_version = 1

[[profiles]]
kind = "ssh"
id = "remote"
host = "example.test"
username = "alice"
compression = true
"#,
        )
        .unwrap_err();
        assert_eq!(error.kind(), ConfigErrorKind::Parse);
        assert!(!error.to_string().contains("compression"));
    }

    #[test]
    fn rejects_secret_fields_and_values_without_echoing_them() {
        let secret = "not-for-diagnostics";
        let field_error = Configuration::parse(&format!(
            r#"
schema_version = 1
password = "{secret}"
"#
        ))
        .unwrap_err();
        assert_eq!(field_error.kind(), ConfigErrorKind::ForbiddenSecretField);
        assert!(!field_error.to_string().contains(secret));
        assert!(!format!("{field_error:?}").contains(secret));

        let key_material = "-----BEGIN OPENSSH PRIVATE KEY-----";
        let value_error = Configuration::parse(&format!(
            r#"
schema_version = 1

[[profiles]]
kind = "local"
id = "safe-id"
executable = "{key_material}"
"#
        ))
        .unwrap_err();
        assert_eq!(value_error.kind(), ConfigErrorKind::ForbiddenSecretValue);
        assert!(!value_error.to_string().contains(key_material));
        assert!(!format!("{value_error:?}").contains(key_material));
    }

    #[test]
    fn rejects_secret_bearing_local_options() {
        let error = Configuration::parse(
            r#"
schema_version = 1

[[profiles]]
kind = "local"
id = "unsafe"
executable = "tool"
arguments = ["--password=not-for-toml"]
"#,
        )
        .unwrap_err();

        assert_eq!(error.kind(), ConfigErrorKind::ForbiddenSecretValue);
    }

    #[test]
    fn validates_identifiers_uniqueness_and_ssh_metadata() {
        let invalid_identifier = Configuration::parse(
            r#"
schema_version = 1

[[profiles]]
kind = "local"
id = "Not-valid"
executable = "sh"
"#,
        )
        .unwrap_err();
        assert_eq!(
            invalid_identifier.kind(),
            ConfigErrorKind::InvalidProfileIdentifier
        );

        let duplicate_identifier = Configuration::parse(
            r#"
schema_version = 1

[[profiles]]
kind = "local"
id = "same"
executable = "sh"

[[profiles]]
kind = "ssh"
id = "same"
host = "example.test"
username = "alice"
"#,
        )
        .unwrap_err();
        assert_eq!(
            duplicate_identifier.kind(),
            ConfigErrorKind::DuplicateProfileIdentifier
        );

        let invalid_ssh = Configuration::parse(
            r#"
schema_version = 1

[[profiles]]
kind = "ssh"
id = "bad-ssh"
host = "ssh://alice:password@example.test"
username = "alice"
"#,
        )
        .unwrap_err();
        assert_eq!(invalid_ssh.kind(), ConfigErrorKind::InvalidSshProfile);
    }

    #[test]
    fn reload_is_transactional_and_clears_errors_after_valid_replacement() {
        let original = Configuration::parse(COMPLETE_CONFIGURATION).unwrap();
        let mut state = ConfigurationState::new(original.clone());

        let error = state
            .reload(
                r#"
schema_version = 99
"#,
            )
            .unwrap_err();
        assert_eq!(error.kind(), ConfigErrorKind::UnsupportedSchemaVersion);
        assert_eq!(state.active(), &original);
        assert_eq!(state.last_error(), Some(error));

        state
            .reload(
                r#"
schema_version = 1

[[profiles]]
kind = "local"
id = "replacement"
executable = "sh"
"#,
            )
            .unwrap();
        assert_eq!(state.active().profiles().len(), 1);
        assert_eq!(state.active().profiles()[0].identifier(), "replacement");
        assert_eq!(state.last_error(), None);
    }

    #[test]
    fn constructors_cannot_create_invalid_profiles() {
        assert_eq!(
            Profile::local("bad id", "sh", Vec::new(), None)
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
    fn saves_and_loads_a_complete_configuration() {
        let directory = TestDirectory::new();
        let path = directory.path().join("profiles.toml");
        let configuration = Configuration::parse(COMPLETE_CONFIGURATION).unwrap();

        configuration.save_to_path(&path).unwrap();

        assert_eq!(Configuration::load_from_path(&path).unwrap(), configuration);
    }

    #[test]
    fn reload_from_path_keeps_active_configuration_for_invalid_content() {
        let directory = TestDirectory::new();
        let path = directory.path().join("profiles.toml");
        fs::write(
            &path,
            r#"
schema_version = 99
"#,
        )
        .unwrap();
        let original = Configuration::parse(COMPLETE_CONFIGURATION).unwrap();
        let mut state = ConfigurationState::new(original.clone());

        let error = state.reload_from_path(&path).unwrap_err();

        assert_eq!(error.kind(), ConfigurationFileErrorKind::Parse);
        assert_eq!(
            error.configuration_error().map(ConfigError::kind),
            Some(ConfigErrorKind::UnsupportedSchemaVersion)
        );
        assert_eq!(state.active(), &original);
        assert_eq!(
            state.last_error().map(ConfigError::kind),
            Some(ConfigErrorKind::UnsupportedSchemaVersion)
        );
    }

    #[test]
    fn replacement_keeps_the_target_as_valid_complete_toml() {
        let directory = TestDirectory::new();
        let path = directory.path().join("profiles.toml");
        let original = Configuration::parse(COMPLETE_CONFIGURATION).unwrap();
        let replacement =
            Configuration::new(vec![
                Profile::local("replacement", "sh", Vec::new(), None).unwrap()
            ])
            .unwrap();
        original.save_to_path(&path).unwrap();

        replacement.save_to_path(&path).unwrap();

        let document = fs::read_to_string(&path).unwrap();
        assert_eq!(Configuration::parse(&document).unwrap(), replacement);
        assert_eq!(Configuration::load_from_path(&path).unwrap(), replacement);
    }

    #[test]
    fn missing_file_is_classified_without_echoing_its_path() {
        let directory = TestDirectory::new();
        let path = directory
            .path()
            .join("do-not-echo-this-sensitive-path.toml");

        let error = Configuration::load_from_path(&path).unwrap_err();

        assert_eq!(error.kind(), ConfigurationFileErrorKind::MissingFile);
        assert!(!error
            .to_string()
            .contains("do-not-echo-this-sensitive-path"));
        assert!(!format!("{error:?}").contains("do-not-echo-this-sensitive-path"));
    }

    #[test]
    fn relative_target_uses_the_current_directory_and_empty_target_is_rejected() {
        assert_eq!(
            parent_directory(Path::new("profiles.toml")).unwrap(),
            Path::new(".")
        );
        assert_eq!(
            parent_directory(Path::new("")).unwrap_err().kind(),
            ConfigurationFileErrorKind::InvalidTargetPath
        );
    }

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new() -> Self {
            let identifier = NEXT_TEMPORARY_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::current_dir().unwrap().join(format!(
                "festerm-config-test-{}-{identifier}",
                process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
