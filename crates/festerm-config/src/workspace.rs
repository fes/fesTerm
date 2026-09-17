use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::{
    validate_identifier, ConfigError, ConfigErrorKind, Profile, RemoteProfileKind,
    SshProfileConfiguration,
};

/// Metadata-only state used to restore every window's ordered tab surfaces.
///
/// The workspace never contains terminal contents, processes, transport
/// attempts, authentication, key material, host trust, or mutable ad-hoc
/// launch definitions. A missing focus means restoration selects the first
/// tab in document order.
///
/// `tabs` and `focused_tab_id` describe the **primary** window, and
/// [`Self::windows`] describes each additional window (ADR 0033). Keeping the
/// primary window in the original fields means a fesTerm build that predates
/// multi-window restores it unchanged instead of failing or restoring an
/// arbitrary window's tabs. Tab identifiers are unique across the whole
/// workspace, so a focus reference is never ambiguous.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfiguration {
    #[serde(default)]
    tabs: Vec<WorkspaceTab>,
    #[serde(default)]
    focused_tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    windows: Vec<WorkspaceWindow>,
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
        Self::with_windows(tabs, focused_tab_id, Vec::new())
    }

    /// Creates a validated workspace whose primary window holds `tabs` and
    /// which reopens one further window per entry in `windows` (ADR 0033).
    pub fn with_windows(
        tabs: Vec<WorkspaceTab>,
        focused_tab_id: Option<String>,
        windows: Vec<WorkspaceWindow>,
    ) -> Result<Self, ConfigError> {
        let workspace = Self {
            tabs,
            focused_tab_id,
            windows,
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

    /// Returns the additional windows to reopen beside the primary one, in
    /// their saved order.
    pub fn windows(&self) -> &[WorkspaceWindow] {
        &self.windows
    }

    pub(crate) fn validate(&self, profiles: &[Profile]) -> Result<(), ConfigError> {
        self.validate_structure()?;
        for tab in self.all_tabs() {
            tab.validate_profile_reference(profiles)?;
        }
        Ok(())
    }

    /// Every tab in the workspace, primary window first.
    fn all_tabs(&self) -> impl Iterator<Item = &WorkspaceTab> {
        self.tabs
            .iter()
            .chain(self.windows.iter().flat_map(|window| window.tabs.iter()))
    }

    fn validate_structure(&self) -> Result<(), ConfigError> {
        // An additional window with no tabs would restore as an empty window,
        // which no application state can represent (ADR 0033: a window that
        // loses its last tab collapses instead of persisting).
        if self.tabs.is_empty() || self.windows.iter().any(|window| window.tabs.is_empty()) {
            return Err(ConfigError::new(ConfigErrorKind::EmptyWorkspace));
        }

        let mut identifiers = HashSet::new();
        for tab in self.all_tabs() {
            validate_tab_identifier(tab.identifier())?;
            if !identifiers.insert(tab.identifier()) {
                return Err(ConfigError::new(
                    ConfigErrorKind::DuplicateWorkspaceTabIdentifier,
                ));
            }
            tab.validate_metadata()?;
        }

        for focused_tab_id in std::iter::once(&self.focused_tab_id)
            .chain(self.windows.iter().map(|window| &window.focused_tab_id))
            .flatten()
        {
            validate_tab_identifier(focused_tab_id)?;
            if !identifiers.contains(focused_tab_id.as_str()) {
                return Err(ConfigError::new(
                    ConfigErrorKind::UnknownFocusedWorkspaceTab,
                ));
            }
        }

        for window in &self.windows {
            if let Some(geometry) = &window.geometry {
                geometry.validate()?;
            }
        }
        Ok(())
    }
}

/// One additional window to reopen beside the primary one (ADR 0033).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceWindow {
    tabs: Vec<WorkspaceTab>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    focused_tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    geometry: Option<WorkspaceWindowGeometry>,
}

impl WorkspaceWindow {
    /// Creates one additional window's restorable state. Validation of the
    /// whole workspace - identifier uniqueness across windows, focus
    /// references, and geometry - happens in
    /// [`WorkspaceConfiguration::with_windows`], which is the only place that
    /// can see every window at once.
    pub const fn new(
        tabs: Vec<WorkspaceTab>,
        focused_tab_id: Option<String>,
        geometry: Option<WorkspaceWindowGeometry>,
    ) -> Self {
        Self {
            tabs,
            focused_tab_id,
            geometry,
        }
    }

    /// Returns this window's restorable tabs in their saved display order.
    pub fn tabs(&self) -> &[WorkspaceTab] {
        &self.tabs
    }

    /// Returns this window's saved focused tab identifier, if any.
    pub fn focused_tab_id(&self) -> Option<&str> {
        self.focused_tab_id.as_deref()
    }

    /// Returns this window's saved position and size, if the platform
    /// reported them when it was saved.
    pub const fn geometry(&self) -> Option<&WorkspaceWindowGeometry> {
        self.geometry.as_ref()
    }
}

/// A window's saved outer position and inner size, in logical points.
///
/// Optional at every level: a platform that refuses to report a window's own
/// screen position (Wayland) simply saves no geometry, and restoration opens
/// that window at the default size wherever the platform puts it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceWindowGeometry {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl WorkspaceWindowGeometry {
    /// Creates saved geometry from a window's outer position and inner size.
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn position(&self) -> (f32, f32) {
        (self.x, self.y)
    }

    pub const fn size(&self) -> (f32, f32) {
        (self.width, self.height)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let finite = self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite();
        if finite && self.width > 0.0 && self.height > 0.0 {
            Ok(())
        } else {
            Err(ConfigError::new(
                ConfigErrorKind::InvalidWorkspaceWindowGeometry,
            ))
        }
    }
}

/// Saved geometry compares by value: two windows restore identically when
/// their numbers match. Derived `PartialEq` would be enough, but `Eq` lets
/// the enclosing workspace types keep their `Eq` bound even though the fields
/// are floats, and every stored value is validated finite above.
impl PartialEq for WorkspaceWindowGeometry {
    fn eq(&self, other: &Self) -> bool {
        self.x.to_bits() == other.x.to_bits()
            && self.y.to_bits() == other.y.to_bits()
            && self.width.to_bits() == other.width.to_bits()
            && self.height.to_bits() == other.height.to_bits()
    }
}

impl Eq for WorkspaceWindowGeometry {}

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
