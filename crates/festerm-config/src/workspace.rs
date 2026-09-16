use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::{
    validate_identifier, ConfigError, ConfigErrorKind, Profile, RemoteProfileKind,
    SshProfileConfiguration,
};

/// Metadata-only state used to restore one window's ordered tab surfaces.
///
/// The workspace never contains terminal contents, processes, transport
/// attempts, authentication, key material, host trust, or mutable ad-hoc
/// launch definitions. A missing focus means restoration selects the first
/// tab in document order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfiguration {
    #[serde(default)]
    tabs: Vec<WorkspaceTab>,
    #[serde(default)]
    focused_tab_id: Option<String>,
}

impl WorkspaceConfiguration {
    /// Creates a validated ordered workspace.
    ///
    /// A workspace must retain at least one tab. Closing the final tab is an
    /// application action that replaces it with a Launcher tab before a later
    /// workspace snapshot is saved.
    pub fn new(
        tabs: Vec<WorkspaceTab>,
        focused_tab_id: Option<String>,
    ) -> Result<Self, ConfigError> {
        let workspace = Self {
            tabs,
            focused_tab_id,
        };
        workspace.validate_structure()?;
        Ok(workspace)
    }

    /// Returns the restorable tabs in their saved display order.
    pub fn tabs(&self) -> &[WorkspaceTab] {
        &self.tabs
    }

    /// Returns the saved focused tab identifier, if one was explicitly saved.
    ///
    /// When this is `None`, restoration deterministically focuses the first
    /// item returned by [`Self::tabs`].
    pub fn focused_tab_id(&self) -> Option<&str> {
        self.focused_tab_id.as_deref()
    }

    pub(crate) fn validate(&self, profiles: &[Profile]) -> Result<(), ConfigError> {
        self.validate_structure()?;
        for tab in &self.tabs {
            tab.validate_profile_reference(profiles)?;
        }
        Ok(())
    }

    fn validate_structure(&self) -> Result<(), ConfigError> {
        if self.tabs.is_empty() {
            return Err(ConfigError::new(ConfigErrorKind::EmptyWorkspace));
        }

        let mut identifiers = HashSet::with_capacity(self.tabs.len());
        for tab in &self.tabs {
            validate_tab_identifier(tab.identifier())?;
            if !identifiers.insert(tab.identifier()) {
                return Err(ConfigError::new(
                    ConfigErrorKind::DuplicateWorkspaceTabIdentifier,
                ));
            }
            tab.validate_metadata()?;
        }

        if let Some(focused_tab_id) = &self.focused_tab_id {
            validate_tab_identifier(focused_tab_id)?;
            if !identifiers.contains(focused_tab_id.as_str()) {
                return Err(ConfigError::new(
                    ConfigErrorKind::UnknownFocusedWorkspaceTab,
                ));
            }
        }
        Ok(())
    }
}

/// One stable, restorable workspace surface.
///
/// Local and SSH session tabs reference reusable profiles by identifier. The
/// schema deliberately has no ad-hoc session variant, so mutable launch
/// definitions cannot enter persisted workspace metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspaceTab {
    /// The application Launcher surface, with no session.
    Launcher(LauncherTabConfiguration),
    /// The application Settings surface, with no session.
    Settings(SettingsTabConfiguration),
    /// The application Profiles management surface, with no session.
    Profiles(ProfilesTabConfiguration),
    /// A local session recreated from a local profile.
    LocalSession(SessionTabConfiguration),
    /// An SSH session recreated from an SSH profile.
    SshSession(SessionTabConfiguration),
    /// An SFTP session recreated from an SSH profile.
    SftpSession(SessionTabConfiguration),
    /// A GUI SFTP file-manager tab recreated from an SSH profile.
    SftpFileManager(SessionTabConfiguration),
    /// A serial session recreated from a serial profile.
    SerialSession(SessionTabConfiguration),
}

impl WorkspaceTab {
    /// Creates a Launcher application-surface tab.
    pub fn launcher(identifier: impl Into<String>) -> Result<Self, ConfigError> {
        let tab = Self::Launcher(LauncherTabConfiguration {
            id: identifier.into(),
        });
        tab.validate_metadata()?;
        Ok(tab)
    }

    /// Creates a Settings application-surface tab.
    pub fn settings(identifier: impl Into<String>) -> Result<Self, ConfigError> {
        let tab = Self::Settings(SettingsTabConfiguration {
            id: identifier.into(),
        });
        tab.validate_metadata()?;
        Ok(tab)
    }

    /// Creates a Profiles application-surface tab.
    pub fn profiles(identifier: impl Into<String>) -> Result<Self, ConfigError> {
        let tab = Self::Profiles(ProfilesTabConfiguration {
            id: identifier.into(),
        });
        tab.validate_metadata()?;
        Ok(tab)
    }

    /// Creates a local-session tab which will reference a local profile.
    pub fn local_session(
        identifier: impl Into<String>,
        profile_id: impl Into<String>,
    ) -> Result<Self, ConfigError> {
        let tab = Self::LocalSession(SessionTabConfiguration {
            id: identifier.into(),
            profile_id: profile_id.into(),
        });
        tab.validate_metadata()?;
        Ok(tab)
    }

    /// Creates an SSH-session tab which will reference an SSH profile.
    pub fn ssh_session(
        identifier: impl Into<String>,
        profile_id: impl Into<String>,
    ) -> Result<Self, ConfigError> {
        let tab = Self::SshSession(SessionTabConfiguration {
            id: identifier.into(),
            profile_id: profile_id.into(),
        });
        tab.validate_metadata()?;
        Ok(tab)
    }

    /// Creates an SFTP-session tab which will reference an SSH profile.
    pub fn sftp_session(
        identifier: impl Into<String>,
        profile_id: impl Into<String>,
    ) -> Result<Self, ConfigError> {
        let tab = Self::SftpSession(SessionTabConfiguration {
            id: identifier.into(),
            profile_id: profile_id.into(),
        });
        tab.validate_metadata()?;
        Ok(tab)
    }

    /// Creates a GUI SFTP file-manager tab which will reference an SSH profile.
    pub fn sftp_file_manager(
        identifier: impl Into<String>,
        profile_id: impl Into<String>,
    ) -> Result<Self, ConfigError> {
        let tab = Self::SftpFileManager(SessionTabConfiguration {
            id: identifier.into(),
            profile_id: profile_id.into(),
        });
        tab.validate_metadata()?;
        Ok(tab)
    }

    /// Creates a serial-session tab which will reference a serial profile.
    pub fn serial_session(
        identifier: impl Into<String>,
        profile_id: impl Into<String>,
    ) -> Result<Self, ConfigError> {
        let tab = Self::SerialSession(SessionTabConfiguration {
            id: identifier.into(),
            profile_id: profile_id.into(),
        });
        tab.validate_metadata()?;
        Ok(tab)
    }

    /// Returns this tab's stable, serialized application identifier.
    pub fn identifier(&self) -> &str {
        match self {
            Self::Launcher(tab) => tab.identifier(),
            Self::Settings(tab) => tab.identifier(),
            Self::Profiles(tab) => tab.identifier(),
            Self::LocalSession(tab)
            | Self::SshSession(tab)
            | Self::SftpSession(tab)
            | Self::SftpFileManager(tab)
            | Self::SerialSession(tab) => tab.identifier(),
        }
    }

    /// Returns the referenced profile identifier for session tabs.
    pub fn profile_id(&self) -> Option<&str> {
        match self {
            Self::LocalSession(tab)
            | Self::SshSession(tab)
            | Self::SftpSession(tab)
            | Self::SftpFileManager(tab)
            | Self::SerialSession(tab) => Some(tab.profile_id()),
            Self::Launcher(_) | Self::Settings(_) | Self::Profiles(_) => None,
        }
    }

    fn validate_metadata(&self) -> Result<(), ConfigError> {
        match self {
            Self::Launcher(tab) => validate_tab_identifier(tab.identifier()),
            Self::Settings(tab) => validate_tab_identifier(tab.identifier()),
            Self::Profiles(tab) => validate_tab_identifier(tab.identifier()),
            Self::LocalSession(tab)
            | Self::SshSession(tab)
            | Self::SftpSession(tab)
            | Self::SftpFileManager(tab)
            | Self::SerialSession(tab) => tab.validate(),
        }
    }

    fn validate_profile_reference(&self, profiles: &[Profile]) -> Result<(), ConfigError> {
        match self {
            Self::LocalSession(tab) => {
                validate_session_profile(profiles, tab.profile_id(), ExpectedProfileKind::Local)
            }
            Self::SshSession(tab) => {
                validate_session_profile(profiles, tab.profile_id(), ExpectedProfileKind::SshShell)
            }
            Self::SftpSession(tab) => validate_session_profile(
                profiles,
                tab.profile_id(),
                ExpectedProfileKind::SftpTransport,
            ),
            Self::SftpFileManager(tab) => validate_session_profile(
                profiles,
                tab.profile_id(),
                ExpectedProfileKind::SftpTransport,
            ),
            Self::SerialSession(tab) => {
                validate_session_profile(profiles, tab.profile_id(), ExpectedProfileKind::Serial)
            }
            Self::Launcher(_) | Self::Settings(_) | Self::Profiles(_) => Ok(()),
        }
    }
}

/// Serialized metadata for a Launcher tab.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LauncherTabConfiguration {
    id: String,
}

impl LauncherTabConfiguration {
    /// Returns this tab's stable application identifier.
    pub fn identifier(&self) -> &str {
        &self.id
    }
}

/// Serialized metadata for a Settings tab.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsTabConfiguration {
    id: String,
}

impl SettingsTabConfiguration {
    /// Returns this tab's stable application identifier.
    pub fn identifier(&self) -> &str {
        &self.id
    }
}

/// Serialized metadata for a Profiles management tab.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilesTabConfiguration {
    id: String,
}

impl ProfilesTabConfiguration {
    /// Returns this tab's stable application identifier.
    pub fn identifier(&self) -> &str {
        &self.id
    }
}

/// Serialized metadata for a profile-backed session tab.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionTabConfiguration {
    id: String,
    profile_id: String,
}

impl SessionTabConfiguration {
    /// Returns this tab's stable application identifier.
    pub fn identifier(&self) -> &str {
        &self.id
    }

    /// Returns the reusable profile identifier used to recreate this session.
    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    fn validate(&self) -> Result<(), ConfigError> {
        validate_tab_identifier(&self.id)?;
        validate_identifier(&self.profile_id)
            .map_err(|_| ConfigError::new(ConfigErrorKind::InvalidWorkspaceProfileReference))
    }
}

fn validate_tab_identifier(identifier: &str) -> Result<(), ConfigError> {
    validate_identifier(identifier)
        .map_err(|_| ConfigError::new(ConfigErrorKind::InvalidWorkspaceTabIdentifier))
}

/// Which profile kind a workspace session tab expects its `profile_id` to
/// resolve to. Prevents e.g. a serial-session tab from silently launching a
/// local-shell profile that happens to share an identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExpectedProfileKind {
    Local,
    SshShell,
    SftpTransport,
    Serial,
}

fn validate_session_profile(
    profiles: &[Profile],
    profile_id: &str,
    expected: ExpectedProfileKind,
) -> Result<(), ConfigError> {
    let Some(profile) = profiles
        .iter()
        .find(|profile| profile.identifier() == profile_id)
    else {
        return Err(ConfigError::new(
            ConfigErrorKind::UnknownWorkspaceProfileReference,
        ));
    };

    let kind_matches = matches!(
        (expected, profile),
        (ExpectedProfileKind::Local, Profile::Local(_))
            | (
                ExpectedProfileKind::SshShell,
                Profile::Ssh(SshProfileConfiguration {
                    profile_kind: RemoteProfileKind::Ssh,
                    ..
                })
            )
            | (ExpectedProfileKind::SftpTransport, Profile::Ssh(_))
            | (ExpectedProfileKind::Serial, Profile::Serial(_))
    );
    if kind_matches {
        Ok(())
    } else {
        Err(ConfigError::new(
            ConfigErrorKind::WorkspaceProfileKindMismatch,
        ))
    }
}
