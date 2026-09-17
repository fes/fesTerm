use std::fmt;

use crate::SCHEMA_VERSION;

pub(crate) fn parse_error(error: toml::de::Error) -> ConfigError {
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
    /// A recorded profile launch names a profile the document does not define.
    UnknownProfileReference,
    InvalidLocalProfile,
    InvalidSshProfile,
    InvalidSshPortForwardConfiguration,
    DuplicateSshPortForward,
    InvalidSerialProfile,
    InvalidPersistenceConfiguration,
    InvalidCredentialReference,
    CredentialReferenceRequiresSshProfile,
    PersistenceRequiresLocalOrSshProfile,
    NativePersistenceRequiresLocalProfile,
    WorkspacePresentWhenDisabled,
    WorkspaceMissingWhenEnabled,
    EmptyWorkspace,
    InvalidWorkspaceTabIdentifier,
    DuplicateWorkspaceTabIdentifier,
    InvalidWorkspaceProfileReference,
    UnknownWorkspaceProfileReference,
    WorkspaceProfileKindMismatch,
    UnknownFocusedWorkspaceTab,
    InvalidWorkspaceWindowGeometry,
    InvalidKnownHost,
    DuplicateKnownHost,
    InvalidInterfaceSettings,
    Serialization,
}

/// An actionable error that never retains document content or secret values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigError {
    kind: ConfigErrorKind,
    location: Option<SourceLocation>,
}

impl ConfigError {
    pub(crate) const fn new(kind: ConfigErrorKind) -> Self {
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
                "profiles[].id must be a non-empty, control-character-free string of at most 200 characters with no leading or trailing whitespace",
            ),
            ConfigErrorKind::DuplicateProfileIdentifier => {
                formatter.write_str("profiles[].id values must be unique")
            }
            ConfigErrorKind::UnknownProfileReference => formatter.write_str(
                "profile_usage[].profile must name a profile defined in profiles[]",
            ),
            ConfigErrorKind::InvalidLocalProfile => formatter.write_str(
                "local profile metadata must use non-empty, control-character-free executable, arguments, and working_directory values without secret-bearing options",
            ),
            ConfigErrorKind::InvalidSshProfile => formatter.write_str(
                "SSH profile metadata must contain a host, nonzero port, safe username and terminal type, and at least 2 columns by 1 row",
            ),
            ConfigErrorKind::InvalidSshPortForwardConfiguration => formatter.write_str(
                "SSH port forwards must use non-empty, safe bind and destination hosts with nonzero ports",
            ),
            ConfigErrorKind::DuplicateSshPortForward => formatter.write_str(
                "SSH port forwards must not repeat the same direction, bind host, and bind port within one profile",
            ),
            ConfigErrorKind::InvalidSerialProfile => formatter.write_str(
                "serial profile metadata must contain a non-empty, control-character-free device identifier and a nonzero baud rate",
            ),
            ConfigErrorKind::InvalidPersistenceConfiguration => formatter.write_str(
                "a persistent session name may only contain ASCII letters, digits, '-', '_', or '.', and must be 1-64 bytes",
            ),
            ConfigErrorKind::InvalidCredentialReference => formatter.write_str(
                "SSH credential_id must be a canonical opaque UUID-v4 reference",
            ),
            ConfigErrorKind::CredentialReferenceRequiresSshProfile => formatter.write_str(
                "opaque credential references may be attached only to SSH profiles",
            ),
            ConfigErrorKind::PersistenceRequiresLocalOrSshProfile => formatter.write_str(
                "durable-session persistence may only be attached to local or SSH profiles",
            ),
            ConfigErrorKind::NativePersistenceRequiresLocalProfile => formatter.write_str(
                "fesTerm's native persistence provider may only be attached to local profiles",
            ),
            ConfigErrorKind::WorkspacePresentWhenDisabled => formatter.write_str(
                "workspace metadata requires workspace_enabled = true",
            ),
            ConfigErrorKind::WorkspaceMissingWhenEnabled => formatter.write_str(
                "workspace_enabled = true requires metadata-only workspace state",
            ),
            ConfigErrorKind::EmptyWorkspace => {
                formatter.write_str("workspace metadata must contain at least one tab")
            }
            ConfigErrorKind::InvalidWorkspaceTabIdentifier => formatter.write_str(
                "workspace tab IDs must be a non-empty, control-character-free string of at most 200 characters with no leading or trailing whitespace",
            ),
            ConfigErrorKind::DuplicateWorkspaceTabIdentifier => {
                formatter.write_str("workspace tab IDs must be unique")
            }
            ConfigErrorKind::InvalidWorkspaceProfileReference => formatter.write_str(
                "workspace session profile references must be valid profile identifiers",
            ),
            ConfigErrorKind::UnknownWorkspaceProfileReference => formatter.write_str(
                "workspace session tabs must reference an existing profile",
            ),
            ConfigErrorKind::WorkspaceProfileKindMismatch => formatter.write_str(
                "workspace session tab kind must match its referenced profile",
            ),
            ConfigErrorKind::UnknownFocusedWorkspaceTab => {
                formatter.write_str("workspace focus must reference a saved tab")
            }
            ConfigErrorKind::InvalidWorkspaceWindowGeometry => formatter.write_str(
                "workspace window geometry must be finite, with a positive size",
            ),
            ConfigErrorKind::InvalidKnownHost => formatter.write_str(
                "known_hosts[] entries must have a valid host, nonzero port, and a canonical SHA256: fingerprint",
            ),
            ConfigErrorKind::DuplicateKnownHost => {
                formatter.write_str("known_hosts[] entries must be unique per host:port")
            }
            ConfigErrorKind::InvalidInterfaceSettings => formatter.write_str(
                    "interface settings must use non-empty, control-character-free default_sftp_local_directory values without secret-bearing text and safe values for every other field",
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
    pub(crate) const fn new(kind: ConfigurationFileErrorKind) -> Self {
        Self {
            kind,
            configuration_error: None,
        }
    }

    pub(crate) const fn parse(error: ConfigError) -> Self {
        Self {
            kind: ConfigurationFileErrorKind::Parse,
            configuration_error: Some(error),
        }
    }

    pub(crate) const fn serialization(error: ConfigError) -> Self {
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
