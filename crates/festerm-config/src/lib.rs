//! Versioned, secret-free application configuration, profiles, and workspace metadata.
//!
//! This crate deliberately owns document parsing and validation, but not file
//! watching, GUI editing, credentials, or runtime session restoration.
//! Configuration documents contain only reusable launch metadata and safe,
//! metadata-only workspace tab descriptors.

use std::{
    collections::HashSet,
    fmt,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use festerm_pty::LocalProfile;
use festerm_secret_store::SecretReference;
use festerm_serial::LineSettings;
use festerm_session::TerminalSize;
use festerm_ssh::{
    is_sha256_fingerprint, HostIdentity, PersistenceProvider, PersistentSessionName,
    SessionStrategy, SshConnectionProfile, SshPortForwardSpec,
};
use serde::{Deserialize, Serialize};

/// The only document schema accepted by this initial configuration slice.
pub const SCHEMA_VERSION: u32 = 1;

const DEFAULT_SSH_PORT: u16 = 22;
const DEFAULT_COLUMNS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const DEFAULT_SERIAL_BAUD_RATE: u32 = 115_200;
const TEMPORARY_FILE_ATTEMPTS: u32 = 128;

static NEXT_TEMPORARY_FILE_ID: AtomicU64 = AtomicU64::new(0);

/// A validated configuration document.
///
/// Profiles are reusable launch definitions. They intentionally do not encode
/// workspace state or authentication material. An SSH profile may retain an
/// opaque native-store reference to an SSH password, never a secret value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Configuration {
    schema_version: u32,
    #[serde(default)]
    profiles: Vec<Profile>,
    #[serde(default, skip_serializing_if = "is_false")]
    workspace_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<WorkspaceConfiguration>,
    #[serde(default, skip_serializing_if = "InterfaceSettings::is_default")]
    settings: InterfaceSettings,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    known_hosts: Vec<KnownHostEntry>,
}

impl Configuration {
    /// Creates and validates a document using the current schema version.
    pub fn new(profiles: Vec<Profile>) -> Result<Self, ConfigError> {
        let configuration = Self {
            schema_version: SCHEMA_VERSION,
            profiles,
            workspace_enabled: false,
            workspace: None,
            settings: InterfaceSettings::DEFAULT,
            known_hosts: Vec::new(),
        };
        configuration.validate()?;
        Ok(configuration)
    }

    /// Creates and validates a document with enabled workspace persistence.
    pub fn new_with_workspace(
        profiles: Vec<Profile>,
        workspace: WorkspaceConfiguration,
    ) -> Result<Self, ConfigError> {
        let configuration = Self {
            schema_version: SCHEMA_VERSION,
            profiles,
            workspace_enabled: true,
            workspace: Some(workspace),
            settings: InterfaceSettings::DEFAULT,
            known_hosts: Vec::new(),
        };
        configuration.validate()?;
        Ok(configuration)
    }

    /// Creates a validated workspace-enabled replacement that preserves all
    /// reusable profile metadata from this document.
    ///
    /// This is intended for explicit workspace snapshots: callers cannot
    /// accidentally discard profiles while enabling workspace persistence.
    pub fn with_workspace(&self, workspace: WorkspaceConfiguration) -> Result<Self, ConfigError> {
        let mut replacement = Self::new_with_workspace(self.profiles.clone(), workspace)?;
        replacement.settings = self.settings.clone();
        replacement.known_hosts = self.known_hosts.clone();
        replacement.validate()?;
        Ok(replacement)
    }

    /// Returns a complete replacement with one SSH profile's native stored
    /// credential reference changed.
    ///
    /// `credential_id` is intentionally limited to the M8 SSH-password/M-cert
    /// credential slice tracked by `kind`. It must not name a passphrase,
    /// agent, key file, trust record, or arbitrary secret.
    pub fn with_ssh_credential(
        &self,
        identifier: &str,
        credential_reference: SecretReference,
        kind: CredentialKind,
    ) -> Result<Self, ConfigError> {
        let mut replacement = self.clone();
        let profile = replacement
            .profiles
            .iter_mut()
            .find(|profile| profile.identifier() == identifier)
            .ok_or_else(|| ConfigError::new(ConfigErrorKind::InvalidSshProfile))?;
        let Profile::Ssh(profile) = profile else {
            return Err(ConfigError::new(
                ConfigErrorKind::CredentialReferenceRequiresSshProfile,
            ));
        };
        profile.credential_id = Some(CredentialReference::new(credential_reference));
        profile.credential_kind = kind;
        replacement.validate()?;
        Ok(replacement)
    }

    /// Returns a complete replacement with `profile` inserted, or replacing
    /// any existing profile sharing its identifier.
    ///
    /// This is the single write path for profile creation and editing:
    /// creating a new profile and saving edits to an existing one are the
    /// same upsert-by-identifier operation, matching
    /// [`Self::with_known_host_trust`]'s replace-outright convention. The
    /// original document is left untouched (immutable-replacement pattern).
    pub fn with_profile(&self, profile: Profile) -> Result<Self, ConfigError> {
        let mut replacement = self.clone();
        replacement
            .profiles
            .retain(|existing| existing.identifier() != profile.identifier());
        replacement.profiles.push(profile);
        replacement.validate()?;
        Ok(replacement)
    }

    /// Returns a complete replacement with the profile named `moved`
    /// relocated to just before the profile named `before`, or to the end of
    /// the list when `before` is `None` (`docs/gui-design.md` "Profile
    /// reordering" - the Profiles surface's drag-to-reorder, reflected in
    /// the Launcher's own profile ordering since both read
    /// [`Self::profiles`] in document order).
    ///
    /// An unknown `moved` identifier, an unknown `before` identifier (which
    /// moves to the end instead), or `moved == before` are all treated as
    /// no-ops rather than errors, mirroring the tab-reorder convention this
    /// is modeled on.
    pub fn with_reordered_profiles(
        &self,
        moved: &str,
        before: Option<&str>,
    ) -> Result<Self, ConfigError> {
        if before == Some(moved) {
            return Ok(self.clone());
        }
        let mut replacement = self.clone();
        let Some(index) = replacement
            .profiles
            .iter()
            .position(|profile| profile.identifier() == moved)
        else {
            return Ok(replacement);
        };
        let profile = replacement.profiles.remove(index);
        let insert_at = match before {
            Some(before_id) => replacement
                .profiles
                .iter()
                .position(|profile| profile.identifier() == before_id)
                .unwrap_or(replacement.profiles.len()),
            None => replacement.profiles.len(),
        };
        replacement.profiles.insert(insert_at, profile);
        replacement.validate()?;
        Ok(replacement)
    }

    /// Returns a complete replacement with the profile named `identifier`
    /// removed.
    ///
    /// Deletion is rejected (rather than silently orphaning a reference) when
    /// any workspace tab still names this profile; callers should surface
    /// [`Self::workspace_tab_references`] to the user before attempting a
    /// delete so the confirmation can name the affected tabs up front.
    pub fn without_profile(&self, identifier: &str) -> Result<Self, ConfigError> {
        let mut replacement = self.clone();
        replacement
            .profiles
            .retain(|profile| profile.identifier() != identifier);
        replacement.validate()?;
        Ok(replacement)
    }

    /// Returns how many saved workspace tabs currently launch from the
    /// profile named `identifier`, for a delete-confirmation prompt
    /// ("Delete requires confirmation and reports workspace references",
    /// `docs/gui-design.md` "Profile editing").
    pub fn workspace_tab_references(&self, identifier: &str) -> usize {
        self.workspace
            .as_ref()
            .map(|workspace| {
                workspace
                    .tabs()
                    .iter()
                    .filter(|tab| tab.profile_id() == Some(identifier))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Returns a complete replacement recording `fingerprint` as the trusted
    /// host key for `host:port` (ADR 0020).
    ///
    /// An existing record for the same `host:port` is replaced outright;
    /// there is no silent merge. Host public-key fingerprints are not secret
    /// material, so this is ordinary configuration state, not a credential.
    pub fn with_known_host_trust(
        &self,
        host: &str,
        port: u16,
        fingerprint: &str,
    ) -> Result<Self, ConfigError> {
        let mut replacement = self.clone();
        replacement
            .known_hosts
            .retain(|entry| !entry.matches(host, port));
        replacement.known_hosts.push(KnownHostEntry {
            host: host.to_owned(),
            port,
            sha256_fingerprint: fingerprint.to_owned(),
        });
        replacement.validate()?;
        Ok(replacement)
    }

    /// Returns a complete replacement with any persistent trust record for
    /// `host:port` removed (ADR 0020's explicit revocation path).
    ///
    /// Infallible once cloned: removing an entry can never introduce a new
    /// validation failure.
    pub fn without_known_host(&self, host: &str, port: u16) -> Self {
        let mut replacement = self.clone();
        replacement
            .known_hosts
            .retain(|entry| !entry.matches(host, port));
        replacement
    }

    /// Returns the fingerprint persistently trusted for `host:port`, if any
    /// (ADR 0020).
    pub fn known_host_fingerprint(&self, host: &str, port: u16) -> Option<&str> {
        self.known_hosts
            .iter()
            .find(|entry| entry.matches(host, port))
            .map(|entry| entry.sha256_fingerprint.as_str())
    }

    /// Returns the metadata-only workspace when persistence is enabled.
    pub fn workspace(&self) -> Option<&WorkspaceConfiguration> {
        self.workspace.as_ref()
    }

    /// Returns a complete replacement with any saved workspace metadata
    /// removed and workspace persistence turned back off.
    ///
    /// Used when the user turns off the "Workspace restore" preference
    /// (`docs/gui-design.md` "Workspace restore" - explicit opt-in, not a
    /// silently-decaying leftover): without this, a previously saved tab
    /// list would keep sitting on disk, ready to resurface the moment the
    /// preference is re-enabled even though the user never asked for that
    /// specific stale snapshot back. Infallible, like
    /// [`Self::without_known_host`]: clearing a workspace can never
    /// introduce a new validation failure.
    pub fn without_workspace(&self) -> Self {
        let mut replacement = self.clone();
        replacement.workspace_enabled = false;
        replacement.workspace = None;
        replacement
    }

    /// Returns a complete replacement with these interface settings applied.
    ///
    /// Unlike profiles/workspace metadata, these preferences are intended to
    /// be saved automatically as the user changes them in Settings; there is
    /// no separate explicit save step for this narrow slice.
    pub fn with_interface_settings(
        &self,
        settings: InterfaceSettings,
    ) -> Result<Self, ConfigError> {
        let mut replacement = self.clone();
        replacement.settings = settings;
        replacement.validate()?;
        Ok(replacement)
    }

    /// Returns the current user-adjustable interface preferences.
    pub fn interface_settings(&self) -> &InterfaceSettings {
        &self.settings
    }

    /// Returns an empty, valid configuration document.
    pub const fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            profiles: Vec::new(),
            workspace_enabled: false,
            workspace: None,
            settings: InterfaceSettings::DEFAULT,
            known_hosts: Vec::new(),
        }
    }

    /// Parses a complete TOML candidate and validates it before returning it.
    pub fn parse(document: &str) -> Result<Self, ConfigError> {
        reject_secret_material(document)?;
        let raw: RawConfiguration = toml::from_str(document).map_err(parse_error)?;
        let configuration = Self {
            schema_version: raw.schema_version,
            profiles: raw.profiles,
            workspace_enabled: raw.workspace_enabled,
            workspace: raw.workspace,
            settings: raw.settings,
            known_hosts: raw.known_hosts,
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
        validate_target_file(path)?;
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

    /// Returns whether this document opts into workspace metadata persistence.
    pub const fn workspace_enabled(&self) -> bool {
        self.workspace_enabled
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
        let mut known_hosts = HashSet::with_capacity(self.known_hosts.len());
        for entry in &self.known_hosts {
            entry.validate()?;
            if !known_hosts.insert((entry.host.as_str(), entry.port)) {
                return Err(ConfigError::new(ConfigErrorKind::DuplicateKnownHost));
            }
        }
        self.settings.validate()?;
        match (self.workspace_enabled, &self.workspace) {
            (false, None) => Ok(()),
            (false, Some(_)) => Err(ConfigError::new(
                ConfigErrorKind::WorkspacePresentWhenDisabled,
            )),
            (true, None) => Err(ConfigError::new(
                ConfigErrorKind::WorkspaceMissingWhenEnabled,
            )),
            (true, Some(workspace)) => workspace.validate(&self.profiles),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfiguration {
    schema_version: u32,
    #[serde(default)]
    profiles: Vec<Profile>,
    #[serde(default)]
    workspace_enabled: bool,
    workspace: Option<WorkspaceConfiguration>,
    #[serde(default)]
    settings: InterfaceSettings,
    #[serde(default)]
    known_hosts: Vec<KnownHostEntry>,
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
            workspace_enabled: raw.workspace_enabled,
            workspace: raw.workspace,
            settings: raw.settings,
            known_hosts: raw.known_hosts,
        };
        configuration.validate().map_err(serde::de::Error::custom)?;
        Ok(configuration)
    }
}

const fn is_false(value: &bool) -> bool {
    !*value
}

/// User-adjustable interface preferences that apply immediately in the UI and
/// are intended to be saved automatically as they change
/// (`docs/gui-design.md` "Wrapping must remain user-configurable"). Unlike
/// profiles and workspace metadata, there is deliberately no separate
/// explicit save step for this slice: Settings applies each change live and
/// the application persists the same replacement immediately afterward.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceSettings {
    #[serde(default)]
    chip_layout: ChipLayoutPreference,
    #[serde(default = "default_status_bar_visible")]
    status_bar_visible: bool,
    #[serde(default = "default_show_session_details")]
    show_session_details: bool,
    /// Whether closing a live session asks for confirmation before
    /// terminating or disconnecting it. On by default to preserve the safe
    /// close behavior; users who prefer one-click closing may opt out.
    #[serde(default = "default_confirm_session_close")]
    confirm_session_close: bool,
    /// Whether the open-tab list and active tab persist across restarts.
    /// Off by default: unlike the other interface preferences here (which
    /// apply immediately and always autosave), workspace restoration is an
    /// explicit opt-in (`docs/gui-design.md` "Workspace restore") - tab
    /// contents are more sensitive/surprising to silently resurrect than a
    /// chip-layout or status-bar cosmetic choice.
    #[serde(default, skip_serializing_if = "is_false")]
    restore_workspace: bool,
    /// The bundled primary face used for terminal cells. Application chrome
    /// typography remains independent.
    #[serde(default, skip_serializing_if = "TerminalFontPreference::is_default")]
    terminal_font: TerminalFontPreference,
    /// Whether eligible adjacent terminal cells may be shaped as a run.
    /// Disabled by default until the renderer can preserve cell ownership
    /// across every supported face.
    #[serde(default, skip_serializing_if = "is_false")]
    terminal_ligatures: bool,
    /// Whether eligible emoji use the bundled color raster path or the
    /// deterministic monochrome fallback chain.
    #[serde(
        default,
        skip_serializing_if = "EmojiPresentationPreference::is_default"
    )]
    emoji_presentation: EmojiPresentationPreference,
    /// How many scrollback rows a single trackpad/wheel scroll step moves,
    /// relative to fesTerm's original fixed pixel-to-row mapping.
    #[serde(default, skip_serializing_if = "ScrollSpeedPreference::is_default")]
    scroll_speed: ScrollSpeedPreference,
    /// Retained primary-history payload budget for newly created sessions.
    #[serde(default, skip_serializing_if = "ScrollbackLimitPreference::is_default")]
    scrollback_limit: ScrollbackLimitPreference,
    /// Whether holding the quick-switch modifier (Cmd on macOS, Ctrl
    /// elsewhere) temporarily overlays each eligible chip's quick-switch
    /// number in place of its usual status presentation (feature request
    /// #69). Off by default, matching the other opt-in presentation
    /// preferences here.
    #[serde(default, skip_serializing_if = "is_false")]
    quick_switch_overlay: bool,
    /// Whether the Launcher's New Session list uses a responsive
    /// multi-column layout for saved profiles when the window is wide
    /// enough (feature request #64). Off by default: the single-column
    /// list remains the baseline presentation.
    #[serde(default, skip_serializing_if = "is_false")]
    compact_launcher_grid: bool,
    /// Whether a background session tab's chip status dot slow-pulses when
    /// that session has emitted output since the tab was last active
    /// (feature request #68). Off by default: chips render exactly as they
    /// do today with no behavior change.
    #[serde(default, skip_serializing_if = "is_false")]
    pulse_new_output_dot: bool,
    /// Whether the New Session/Launcher screen surfaces locally running,
    /// unattached `festerm-sessiond` persistence sessions as one-click
    /// "Resume" entries (feature request #70). Off by default: the
    /// Launcher lists only saved profiles and ad-hoc new-session options,
    /// exactly as it does today.
    #[serde(default, skip_serializing_if = "is_false")]
    show_resumable_sessions: bool,
    /// The default starting local directory for new SFTP sessions. When
    /// present it must resolve to an existing local directory so SFTP tabs
    /// never start with a broken `lpwd` baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_sftp_local_directory: Option<String>,
}

impl InterfaceSettings {
    /// The same defaults fesTerm has always started with; also the target of
    /// an explicit Settings reset.
    pub const DEFAULT: Self = Self {
        chip_layout: ChipLayoutPreference::SingleRowScroll,
        status_bar_visible: true,
        show_session_details: true,
        confirm_session_close: true,
        restore_workspace: false,
        terminal_font: TerminalFontPreference::JetBrainsMono,
        terminal_ligatures: false,
        emoji_presentation: EmojiPresentationPreference::Color,
        scroll_speed: ScrollSpeedPreference::Normal,
        scrollback_limit: ScrollbackLimitPreference::MiB64,
        quick_switch_overlay: false,
        compact_launcher_grid: false,
        pulse_new_output_dot: false,
        show_resumable_sessions: false,
        default_sftp_local_directory: None,
    };

    pub const fn new(
        chip_layout: ChipLayoutPreference,
        status_bar_visible: bool,
        show_session_details: bool,
        confirm_session_close: bool,
        restore_workspace: bool,
    ) -> Self {
        Self {
            chip_layout,
            status_bar_visible,
            show_session_details,
            confirm_session_close,
            restore_workspace,
            terminal_font: TerminalFontPreference::JetBrainsMono,
            terminal_ligatures: false,
            emoji_presentation: EmojiPresentationPreference::Color,
            scroll_speed: ScrollSpeedPreference::Normal,
            scrollback_limit: ScrollbackLimitPreference::MiB64,
            quick_switch_overlay: false,
            compact_launcher_grid: false,
            pulse_new_output_dot: false,
            show_resumable_sessions: false,
            default_sftp_local_directory: None,
        }
    }

    pub const fn with_terminal_typography(
        mut self,
        terminal_font: TerminalFontPreference,
        terminal_ligatures: bool,
    ) -> Self {
        self.terminal_font = terminal_font;
        self.terminal_ligatures = terminal_ligatures;
        self
    }

    pub const fn with_emoji_presentation(
        mut self,
        emoji_presentation: EmojiPresentationPreference,
    ) -> Self {
        self.emoji_presentation = emoji_presentation;
        self
    }

    /// Sets the scrollback scroll-speed clickstop (feature request #67).
    pub const fn with_scroll_speed(mut self, scroll_speed: ScrollSpeedPreference) -> Self {
        self.scroll_speed = scroll_speed;
        self
    }

    /// Sets the retained primary-history budget for newly created sessions.
    pub const fn with_scrollback_limit(
        mut self,
        scrollback_limit: ScrollbackLimitPreference,
    ) -> Self {
        self.scrollback_limit = scrollback_limit;
        self
    }

    /// Sets whether holding the quick-switch modifier overlays chip numbers
    /// (feature request #69).
    pub const fn with_quick_switch_overlay(mut self, quick_switch_overlay: bool) -> Self {
        self.quick_switch_overlay = quick_switch_overlay;
        self
    }

    /// Sets whether the Launcher's New Session list uses a responsive
    /// multi-column layout for saved profiles (feature request #64).
    pub const fn with_compact_launcher_grid(mut self, compact_launcher_grid: bool) -> Self {
        self.compact_launcher_grid = compact_launcher_grid;
        self
    }

    /// Sets whether a background tab's chip status dot slow-pulses when new
    /// output has arrived since it was last active (feature request #68).
    pub const fn with_pulse_new_output_dot(mut self, pulse_new_output_dot: bool) -> Self {
        self.pulse_new_output_dot = pulse_new_output_dot;
        self
    }

    /// Sets whether the Launcher surfaces locally running, unattached
    /// `festerm-sessiond` sessions as one-click "Resume" entries (feature
    /// request #70).
    pub const fn with_show_resumable_sessions(mut self, show_resumable_sessions: bool) -> Self {
        self.show_resumable_sessions = show_resumable_sessions;
        self
    }

    /// Sets the default starting local directory for new SFTP sessions.
    pub fn with_default_sftp_local_directory(
        mut self,
        default_sftp_local_directory: Option<String>,
    ) -> Self {
        self.default_sftp_local_directory = default_sftp_local_directory;
        self
    }

    pub const fn chip_layout(&self) -> ChipLayoutPreference {
        self.chip_layout
    }

    pub const fn status_bar_visible(&self) -> bool {
        self.status_bar_visible
    }

    pub const fn show_session_details(&self) -> bool {
        self.show_session_details
    }

    pub const fn confirm_session_close(&self) -> bool {
        self.confirm_session_close
    }

    pub const fn restore_workspace(&self) -> bool {
        self.restore_workspace
    }

    pub const fn terminal_font(&self) -> TerminalFontPreference {
        self.terminal_font
    }

    pub const fn terminal_ligatures(&self) -> bool {
        self.terminal_ligatures
    }

    pub const fn emoji_presentation(&self) -> EmojiPresentationPreference {
        self.emoji_presentation
    }

    pub const fn scroll_speed(&self) -> ScrollSpeedPreference {
        self.scroll_speed
    }

    pub const fn scrollback_limit(&self) -> ScrollbackLimitPreference {
        self.scrollback_limit
    }

    pub const fn quick_switch_overlay(&self) -> bool {
        self.quick_switch_overlay
    }

    pub const fn compact_launcher_grid(&self) -> bool {
        self.compact_launcher_grid
    }

    pub const fn pulse_new_output_dot(&self) -> bool {
        self.pulse_new_output_dot
    }

    pub const fn show_resumable_sessions(&self) -> bool {
        self.show_resumable_sessions
    }

    pub fn default_sftp_local_directory(&self) -> Option<&Path> {
        self.default_sftp_local_directory.as_deref().map(Path::new)
    }

    fn is_default(&self) -> bool {
        *self == Self::DEFAULT
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if let Some(directory) = &self.default_sftp_local_directory {
            validate_existing_directory_setting(directory)?;
        }
        Ok(())
    }
}

/// Bundled primary terminal families. The serialized names are stable
/// configuration values rather than platform font-discovery names.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalFontPreference {
    #[default]
    JetBrainsMono,
    IosevkaTerm,
    JuliaMono,
    MapleMono,
}

impl TerminalFontPreference {
    const fn is_default(&self) -> bool {
        matches!(self, Self::JetBrainsMono)
    }
}

/// Selects which repository-owned emoji presentation path terminal cells use.
///
/// Both choices preserve the same core-owned grapheme and cell geometry.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmojiPresentationPreference {
    #[default]
    Color,
    Monochrome,
}

impl EmojiPresentationPreference {
    const fn is_default(&self) -> bool {
        matches!(self, Self::Color)
    }
}

/// A discrete clickstop scaling how many scrollback rows one trackpad/wheel
/// scroll step moves, relative to fesTerm's original fixed pixel-to-row
/// mapping (`crates/festerm-ui-egui/src/view.rs`). Deliberately a small,
/// fixed set of named steps rather than a free-form numeric multiplier,
/// matching how the rest of Settings favors discrete, previewable choices
/// over open-ended numeric entry.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScrollSpeedPreference {
    VerySlow,
    Slow,
    #[default]
    Normal,
    Fast,
    VeryFast,
}

impl ScrollSpeedPreference {
    /// All clickstops in slowest-to-fastest order, for rendering a slider.
    pub const ALL: [Self; 5] = [
        Self::VerySlow,
        Self::Slow,
        Self::Normal,
        Self::Fast,
        Self::VeryFast,
    ];

    /// A short, user-displayable name for this clickstop.
    pub const fn label(self) -> &'static str {
        match self {
            Self::VerySlow => "Very slow",
            Self::Slow => "Slow",
            Self::Normal => "Normal",
            Self::Fast => "Fast",
            Self::VeryFast => "Very fast",
        }
    }

    /// The multiplier applied to the rows a scroll step would otherwise
    /// move. `Normal` is `1.0` and preserves fesTerm's original behavior.
    pub const fn multiplier(self) -> f32 {
        match self {
            Self::VerySlow => 0.05,
            Self::Slow => 0.35,
            Self::Normal => 1.0,
            Self::Fast => 1.75,
            Self::VeryFast => 2.5,
        }
    }

    /// This clickstop's position in [`Self::ALL`], for driving a slider by
    /// index.
    pub const fn index(self) -> usize {
        match self {
            Self::VerySlow => 0,
            Self::Slow => 1,
            Self::Normal => 2,
            Self::Fast => 3,
            Self::VeryFast => 4,
        }
    }

    /// The clickstop at `index` in [`Self::ALL`], saturating to the nearest
    /// valid end rather than panicking on an out-of-range slider position.
    pub const fn from_index(index: usize) -> Self {
        match index {
            0 => Self::VerySlow,
            1 => Self::Slow,
            3 => Self::Fast,
            n if n >= 4 => Self::VeryFast,
            _ => Self::Normal,
        }
    }

    const fn is_default(&self) -> bool {
        matches!(self, Self::Normal)
    }
}

/// A bounded retained primary-history budget for newly created sessions.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScrollbackLimitPreference {
    Disabled,
    #[serde(rename = "16-mib")]
    MiB16,
    #[default]
    #[serde(rename = "64-mib")]
    MiB64,
    #[serde(rename = "256-mib")]
    MiB256,
}

impl ScrollbackLimitPreference {
    pub const ALL: [Self; 4] = [Self::Disabled, Self::MiB16, Self::MiB64, Self::MiB256];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Disabled => "Disabled",
            Self::MiB16 => "16 MiB",
            Self::MiB64 => "64 MiB",
            Self::MiB256 => "256 MiB",
        }
    }

    pub const fn bytes(self) -> usize {
        match self {
            Self::Disabled => 0,
            Self::MiB16 => 16 * 1024 * 1024,
            Self::MiB64 => 64 * 1024 * 1024,
            Self::MiB256 => 256 * 1024 * 1024,
        }
    }

    const fn is_default(&self) -> bool {
        matches!(self, Self::MiB64)
    }
}

impl Default for InterfaceSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

const fn default_status_bar_visible() -> bool {
    true
}

const fn default_show_session_details() -> bool {
    true
}

const fn default_confirm_session_close() -> bool {
    true
}

/// The persisted chip-wrapping preference
/// (`docs/gui-design.md` "Tab overflow and wrapping"). This mirrors
/// `festerm_ui_egui::chrome::ChipLayout` without introducing a UI-crate
/// dependency into this configuration crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ChipLayoutPreference {
    /// Many chips wrap onto additional rows.
    Wrap,
    /// Chips stay on a single row; overflow scrolls horizontally instead of
    /// wrapping.
    #[default]
    SingleRowScroll,
}

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

    fn validate(&self, profiles: &[Profile]) -> Result<(), ConfigError> {
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
            | Self::SerialSession(tab) => tab.identifier(),
        }
    }

    /// Returns the referenced profile identifier for session tabs.
    pub fn profile_id(&self) -> Option<&str> {
        match self {
            Self::LocalSession(tab)
            | Self::SshSession(tab)
            | Self::SftpSession(tab)
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
            | Self::SerialSession(tab) => tab.validate(),
        }
    }

    fn validate_profile_reference(&self, profiles: &[Profile]) -> Result<(), ConfigError> {
        match self {
            Self::LocalSession(tab) => {
                validate_session_profile(profiles, tab.profile_id(), ExpectedProfileKind::Local)
            }
            Self::SshSession(tab) => {
                validate_session_profile(profiles, tab.profile_id(), ExpectedProfileKind::Ssh)
            }
            Self::SftpSession(tab) => {
                validate_session_profile(profiles, tab.profile_id(), ExpectedProfileKind::Ssh)
            }
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
    Ssh,
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
            | (ExpectedProfileKind::Ssh, Profile::Ssh(_))
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

/// A serializable configuration boundary around an opaque secret-store ID.
///
/// This type deliberately redacts debug output and exposes the inner value
/// only to the narrow SSH composition API.
#[derive(Clone, Eq, PartialEq)]
struct CredentialReference(Arc<SecretReference>);

impl CredentialReference {
    fn new(reference: SecretReference) -> Self {
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

    fn validate(&self) -> Result<(), ConfigError> {
        match self {
            Self::Local(profile) => profile.validate(),
            Self::Ssh(profile) => profile.validate(),
            Self::Serial(profile) => profile.validate(),
        }
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
    host: String,
    port: u16,
    sha256_fingerprint: String,
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

    fn matches(&self, host: &str, port: u16) -> bool {
        self.host == host && self.port == port
    }

    fn validate(&self) -> Result<(), ConfigError> {
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
    credential_id: Option<CredentialReference>,
    #[serde(default, skip_serializing_if = "is_default_credential_kind")]
    credential_kind: CredentialKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    persistence: Option<PersistenceConfiguration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    port_forwards: Vec<SshPortForwardConfiguration>,
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
            PersistenceProviderKind::Screen => {
                LocalProfile::new("screen").with_arguments(["-xRR", session_name.as_str()])
            }
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
    if identifier.is_empty()
        || identifier.chars().count() > 200
        || contains_control_character(identifier)
        || identifier.trim() != identifier
    {
        return Err(ConfigError::new(ConfigErrorKind::InvalidProfileIdentifier));
    }
    Ok(())
}

fn contains_control_character(value: &str) -> bool {
    value.chars().any(char::is_control)
}

fn validate_existing_directory_setting(path: &str) -> Result<(), ConfigError> {
    if path.is_empty()
        || contains_control_character(path)
        || contains_secret_bearing_value(path)
        || fs::metadata(path)
            .map(|metadata| !metadata.is_dir())
            .unwrap_or(true)
    {
        return Err(ConfigError::new(ConfigErrorKind::InvalidInterfaceSettings));
    }
    Ok(())
}

fn reject_secret_material(document: &str) -> Result<(), ConfigError> {
    let value: toml::Value = toml::from_str(document).map_err(parse_error)?;
    inspect_document_for_secret_material(&value)
}

fn inspect_document_for_secret_material(value: &toml::Value) -> Result<(), ConfigError> {
    let toml::Value::Table(document) = value else {
        return inspect_value_for_secret_material(value, false);
    };

    for (key, value) in document {
        if is_secret_bearing_key(key) {
            return Err(ConfigError::new(ConfigErrorKind::ForbiddenSecretField));
        }
        if key == "profiles" {
            let toml::Value::Array(profiles) = value else {
                inspect_value_for_secret_material(value, false)?;
                continue;
            };
            for profile in profiles {
                inspect_profile_for_secret_material(profile)?;
            }
        } else {
            inspect_value_for_secret_material(value, false)?;
        }
    }
    Ok(())
}

fn inspect_profile_for_secret_material(value: &toml::Value) -> Result<(), ConfigError> {
    let toml::Value::Table(profile) = value else {
        return inspect_value_for_secret_material(value, false);
    };
    let is_ssh = matches!(profile.get("kind"), Some(toml::Value::String(kind)) if kind == "ssh");
    inspect_table_for_secret_material(profile, is_ssh)
}

fn inspect_value_for_secret_material(
    value: &toml::Value,
    allow_credential_id: bool,
) -> Result<(), ConfigError> {
    match value {
        toml::Value::String(value) if contains_secret_bearing_value(value) => {
            Err(ConfigError::new(ConfigErrorKind::ForbiddenSecretValue))
        }
        toml::Value::Array(values) => {
            for value in values {
                inspect_value_for_secret_material(value, false)?;
            }
            Ok(())
        }
        toml::Value::Table(values) => {
            inspect_table_for_secret_material(values, allow_credential_id)
        }
        _ => Ok(()),
    }
}

fn inspect_table_for_secret_material(
    values: &toml::map::Map<String, toml::Value>,
    allow_credential_id: bool,
) -> Result<(), ConfigError> {
    for (key, value) in values {
        if key == "credential_id" && allow_credential_id {
            let toml::Value::String(reference) = value else {
                return Err(ConfigError::new(
                    ConfigErrorKind::InvalidCredentialReference,
                ));
            };
            SecretReference::parse(reference)
                .map_err(|_| ConfigError::new(ConfigErrorKind::InvalidCredentialReference))?;
            continue;
        }
        if key == "credential_kind" && allow_credential_id {
            let toml::Value::String(kind) = value else {
                return Err(ConfigError::new(
                    ConfigErrorKind::InvalidCredentialReference,
                ));
            };
            if kind != "password" && kind != "private_key" {
                return Err(ConfigError::new(
                    ConfigErrorKind::InvalidCredentialReference,
                ));
            }
            continue;
        }
        if is_secret_bearing_key(key) {
            return Err(ConfigError::new(ConfigErrorKind::ForbiddenSecretField));
        }
        inspect_value_for_secret_material(value, false)?;
    }
    Ok(())
}

fn is_secret_bearing_key(key: &str) -> bool {
    let normalized: String = key
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(char::from)
        .flat_map(char::to_lowercase)
        .collect();
    normalized.contains("password")
        || normalized.contains("passphrase")
        || normalized.contains("secret")
        || normalized.contains("token")
        || normalized.contains("privatekey")
        || normalized.contains("credential")
        || normalized == "identityfile"
        || normalized == "keyfile"
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
                "profiles[].id must be a non-empty, control-character-free string of at most 200 characters with no leading or trailing whitespace",
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
            ConfigErrorKind::InvalidKnownHost => formatter.write_str(
                "known_hosts[] entries must have a valid host, nonzero port, and a canonical SHA256: fingerprint",
            ),
            ConfigErrorKind::DuplicateKnownHost => {
                formatter.write_str("known_hosts[] entries must be unique per host:port")
            }
            ConfigErrorKind::InvalidInterfaceSettings => formatter.write_str(
                "interface settings must use existing local directories for default_sftp_local_directory and safe values for every other field",
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

fn validate_target_file(path: &Path) -> Result<(), ConfigurationFileError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(ConfigurationFileError::new(
            ConfigurationFileErrorKind::InvalidTargetPath,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ConfigurationFileError::new(
            ConfigurationFileErrorKind::Read,
        )),
    }
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

    fn ssh_profile_with_port_forwards(port_forwards: Vec<SshPortForwardConfiguration>) -> Profile {
        let ssh = Profile::ssh(
            "forwarded",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .expect("test SSH profile is valid")
        .as_ssh()
        .expect("profile remains SSH")
        .clone()
        .with_port_forwards(port_forwards)
        .expect("test SSH port-forward list is valid");
        Profile::Ssh(ssh)
    }

    const COMPLETE_CONFIGURATION: &str = r#"
schema_version = 1
workspace_enabled = true

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

[workspace]
focused_tab_id = "build-tab"

[[workspace.tabs]]
kind = "launcher"
id = "launcher"

[[workspace.tabs]]
kind = "local_session"
id = "dev-tab"
profile_id = "dev-shell"

[[workspace.tabs]]
kind = "ssh_session"
id = "build-tab"
profile_id = "build-host"

[[workspace.tabs]]
kind = "settings"
id = "settings"
"#;

    #[test]
    fn parses_serializes_and_converts_secret_free_profiles() {
        let configuration = Configuration::parse(COMPLETE_CONFIGURATION).unwrap();

        assert_eq!(configuration.schema_version(), SCHEMA_VERSION);
        assert_eq!(configuration.profiles().len(), 2);
        assert!(configuration.workspace_enabled());
        let workspace = configuration.workspace().unwrap();
        assert_eq!(
            workspace
                .tabs()
                .iter()
                .map(WorkspaceTab::identifier)
                .collect::<Vec<_>>(),
            ["launcher", "dev-tab", "build-tab", "settings"]
        );
        assert_eq!(workspace.focused_tab_id(), Some("build-tab"));
        assert_eq!(workspace.tabs()[1].profile_id(), Some("dev-shell"));
        assert_eq!(workspace.tabs()[2].profile_id(), Some("build-host"));
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
    fn persists_an_opaque_ssh_credential_reference_without_exposing_it_in_debug_output() {
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
        .with_credential_reference(SecretReference::parse(CREDENTIAL_REFERENCE).unwrap())
        .unwrap();
        let configuration = Configuration::new(vec![profile]).unwrap();

        let ssh = configuration.profiles()[0].as_ssh().unwrap();
        assert!(ssh.credential_reference().is_some());
        assert!(configuration.profiles()[0].credential_reference().is_some());
        assert!(!format!("{ssh:?}").contains(CREDENTIAL_REFERENCE));
        assert!(!format!("{configuration:?}").contains(CREDENTIAL_REFERENCE));

        let serialized = configuration.to_toml().unwrap();
        assert!(serialized.contains(&format!("credential_id = \"{CREDENTIAL_REFERENCE}\"")));
        assert_eq!(Configuration::parse(&serialized).unwrap(), configuration);
    }

    #[test]
    fn replaces_only_an_ssh_password_credential_reference() {
        let original = Configuration::new(vec![Profile::ssh(
            "production",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .expect("test SSH profile is valid")])
        .expect("test configuration is valid");
        let reference = SecretReference::parse(CREDENTIAL_REFERENCE).expect("reference is valid");

        let replacement = original
            .with_ssh_credential("production", reference, CredentialKind::Password)
            .expect("SSH credential replacement is valid");

        assert!(original
            .profile("production")
            .and_then(Profile::credential_reference)
            .is_none());
        assert!(replacement
            .profile("production")
            .and_then(Profile::credential_reference)
            .is_some());
        assert!(!format!("{replacement:?}").contains(CREDENTIAL_REFERENCE));
    }

    #[test]
    fn records_upserts_and_revokes_a_known_host_trust_entry() {
        let original = Configuration::empty();
        let fingerprint = "SHA256:UCUiLr7Pjs9wFFJMDByLgc3NrtdU344OgUM45wZPcIQ";

        assert!(original
            .known_host_fingerprint("ssh.example.test", 22)
            .is_none());

        let trusted = original
            .with_known_host_trust("ssh.example.test", 22, fingerprint)
            .expect("known-host trust is valid");
        assert_eq!(
            trusted.known_host_fingerprint("ssh.example.test", 22),
            Some(fingerprint)
        );
        // The original document is untouched (immutable-replacement pattern).
        assert!(original
            .known_host_fingerprint("ssh.example.test", 22)
            .is_none());

        let rotated_fingerprint = "SHA256:different0000000000000000000000000000000";
        let rotated = trusted
            .with_known_host_trust("ssh.example.test", 22, rotated_fingerprint)
            .expect("known-host trust replacement is valid");
        assert_eq!(
            rotated.known_host_fingerprint("ssh.example.test", 22),
            Some(rotated_fingerprint)
        );

        let revoked = rotated.without_known_host("ssh.example.test", 22);
        assert!(revoked
            .known_host_fingerprint("ssh.example.test", 22)
            .is_none());
    }

    #[test]
    fn known_host_trust_round_trips_through_toml_and_rejects_invalid_entries() {
        let fingerprint = "SHA256:UCUiLr7Pjs9wFFJMDByLgc3NrtdU344OgUM45wZPcIQ";
        let configuration = Configuration::empty()
            .with_known_host_trust("ssh.example.test", 2222, fingerprint)
            .expect("known-host trust is valid");
        let document = configuration.to_toml().expect("serializes as TOML");
        assert!(document.contains("known_hosts"));

        let reparsed = Configuration::parse(&document).expect("round trip parses");
        assert_eq!(
            reparsed.known_host_fingerprint("ssh.example.test", 2222),
            Some(fingerprint)
        );

        let invalid_fingerprint = Configuration::parse(
            r#"
schema_version = 1

[[known_hosts]]
host = "ssh.example.test"
port = 22
sha256_fingerprint = "not-a-fingerprint"
"#,
        )
        .unwrap_err();
        assert_eq!(
            invalid_fingerprint.kind(),
            ConfigErrorKind::InvalidKnownHost
        );

        let duplicate = Configuration::parse(&format!(
            r#"
schema_version = 1

[[known_hosts]]
host = "ssh.example.test"
port = 22
sha256_fingerprint = "{fingerprint}"

[[known_hosts]]
host = "ssh.example.test"
port = 22
sha256_fingerprint = "{fingerprint}"
"#
        ))
        .unwrap_err();
        assert_eq!(duplicate.kind(), ConfigErrorKind::DuplicateKnownHost);
    }

    #[test]
    fn with_profile_inserts_a_new_profile_and_replaces_an_existing_one_by_identifier() {
        let original = Configuration::empty();
        assert!(original.profile("development").is_none());

        let created = original
            .with_profile(Profile::local("development", "sh", Vec::new(), None).unwrap())
            .expect("new local profile is valid");
        assert_eq!(
            created
                .profile("development")
                .unwrap()
                .as_local()
                .unwrap()
                .executable(),
            "sh"
        );
        // The original document is untouched (immutable-replacement pattern).
        assert!(original.profile("development").is_none());

        let edited = created
            .with_profile(
                Profile::local(
                    "development",
                    "zsh",
                    vec!["-l".to_owned()],
                    Some("/tmp".to_owned()),
                )
                .unwrap(),
            )
            .expect("editing an existing profile by identifier is valid");
        assert_eq!(
            edited.profiles().len(),
            1,
            "edit replaces, does not duplicate"
        );
        let edited_profile = edited.profile("development").unwrap().as_local().unwrap();
        assert_eq!(edited_profile.executable(), "zsh");
        assert_eq!(edited_profile.arguments(), ["-l"]);
    }

    #[test]
    fn with_reordered_profiles_moves_a_profile_before_a_target_identifier() {
        let configuration = Configuration::new(vec![
            Profile::local("one", "sh", Vec::new(), None).unwrap(),
            Profile::local("two", "sh", Vec::new(), None).unwrap(),
            Profile::local("three", "sh", Vec::new(), None).unwrap(),
        ])
        .expect("three local profiles are valid");

        let reordered = configuration
            .with_reordered_profiles("three", Some("one"))
            .expect("reordering never invalidates the document");

        let identifiers: Vec<&str> = reordered
            .profiles()
            .iter()
            .map(Profile::identifier)
            .collect();
        assert_eq!(identifiers, ["three", "one", "two"]);
        // The original document is untouched (immutable-replacement pattern).
        assert_eq!(
            configuration
                .profiles()
                .iter()
                .map(Profile::identifier)
                .collect::<Vec<_>>(),
            ["one", "two", "three"]
        );
    }

    #[test]
    fn with_reordered_profiles_moves_to_the_end_when_before_is_none() {
        let configuration = Configuration::new(vec![
            Profile::local("one", "sh", Vec::new(), None).unwrap(),
            Profile::local("two", "sh", Vec::new(), None).unwrap(),
        ])
        .expect("two local profiles are valid");

        let reordered = configuration
            .with_reordered_profiles("one", None)
            .expect("reordering never invalidates the document");

        let identifiers: Vec<&str> = reordered
            .profiles()
            .iter()
            .map(Profile::identifier)
            .collect();
        assert_eq!(identifiers, ["two", "one"]);
    }

    #[test]
    fn with_reordered_profiles_ignores_an_unknown_moved_id_or_moving_before_itself() {
        let configuration = Configuration::new(vec![
            Profile::local("one", "sh", Vec::new(), None).unwrap(),
            Profile::local("two", "sh", Vec::new(), None).unwrap(),
        ])
        .expect("two local profiles are valid");

        let unknown_moved = configuration
            .with_reordered_profiles("missing", Some("one"))
            .expect("an unknown moved id is a no-op, not an error");
        assert_eq!(
            unknown_moved
                .profiles()
                .iter()
                .map(Profile::identifier)
                .collect::<Vec<_>>(),
            ["one", "two"]
        );

        let before_itself = configuration
            .with_reordered_profiles("one", Some("one"))
            .expect("moving before itself is a no-op, not an error");
        assert_eq!(
            before_itself
                .profiles()
                .iter()
                .map(Profile::identifier)
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
    }

    #[test]
    fn without_profile_deletes_an_unreferenced_profile_but_rejects_a_workspace_reference() {
        let configuration =
            Configuration::new(vec![
                Profile::local("development", "sh", Vec::new(), None).unwrap()
            ])
            .unwrap();

        let deleted = configuration
            .without_profile("development")
            .expect("deleting an unreferenced profile is valid");
        assert!(deleted.profile("development").is_none());
        // The original document is untouched (immutable-replacement pattern).
        assert!(configuration.profile("development").is_some());

        // Deleting a profile a workspace tab still references must fail
        // rather than silently orphaning that tab.
        let workspace = WorkspaceConfiguration::new(
            vec![WorkspaceTab::local_session("development-tab", "development").unwrap()],
            None,
        )
        .unwrap();
        let with_workspace = configuration.with_workspace(workspace).unwrap();
        assert_eq!(with_workspace.workspace_tab_references("development"), 1);
        assert_eq!(
            with_workspace
                .without_profile("development")
                .unwrap_err()
                .kind(),
            ConfigErrorKind::UnknownWorkspaceProfileReference
        );

        // Deleting a profile with no such tab returns zero references and
        // succeeds.
        assert_eq!(with_workspace.workspace_tab_references("unused"), 0);
    }

    #[test]
    fn rejects_malformed_noncanonical_and_non_v4_credential_references() {
        for reference in [
            "not-a-reference",
            "550E8400-E29B-41D4-A716-446655440000",
            "550e8400-e29b-11d4-a716-446655440000",
            "550e8400e29b41d4a716446655440000",
        ] {
            let error = Configuration::parse(&format!(
                r#"
schema_version = 1

[[profiles]]
kind = "ssh"
id = "remote"
host = "example.test"
username = "alice"
credential_id = "{reference}"
"#
            ))
            .unwrap_err();

            assert_eq!(error.kind(), ConfigErrorKind::InvalidCredentialReference);
            assert!(!error.to_string().contains(reference));
            assert!(!format!("{error:?}").contains(reference));
        }

        let error = Configuration::parse(
            r#"
schema_version = 1

[[profiles]]
kind = "ssh"
id = "remote"
host = "example.test"
username = "alice"
credential_id = 42
"#,
        )
        .unwrap_err();
        assert_eq!(error.kind(), ConfigErrorKind::InvalidCredentialReference);
    }

    #[test]
    fn only_ssh_profile_credential_id_is_permitted_by_secret_field_scanning() {
        for document in [
            r#"
schema_version = 1

[[profiles]]
kind = "ssh"
id = "remote"
host = "example.test"
username = "alice"
credential = "anything"
"#,
            r#"
schema_version = 1

[[profiles]]
kind = "ssh"
id = "remote"
host = "example.test"
username = "alice"
credential_metadata = "anything"
"#,
            r#"
schema_version = 1

[[profiles]]
kind = "local"
id = "local"
executable = "sh"
credential_id = "550e8400-e29b-41d4-a716-446655440000"
"#,
            r#"
schema_version = 1
workspace_enabled = true

[workspace]
credential_id = "550e8400-e29b-41d4-a716-446655440000"
"#,
        ] {
            assert_eq!(
                Configuration::parse(document).unwrap_err().kind(),
                ConfigErrorKind::ForbiddenSecretField
            );
        }
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
    fn constructs_metadata_only_workspace_with_profile_backed_session_tabs() {
        let workspace = WorkspaceConfiguration::new(
            vec![
                WorkspaceTab::launcher("launcher").unwrap(),
                WorkspaceTab::local_session("local-tab", "local").unwrap(),
                WorkspaceTab::ssh_session("ssh-tab", "remote").unwrap(),
                WorkspaceTab::sftp_session("sftp-tab", "remote").unwrap(),
                WorkspaceTab::settings("settings").unwrap(),
                WorkspaceTab::profiles("profiles").unwrap(),
            ],
            Some("sftp-tab".to_owned()),
        )
        .unwrap();
        let configuration = Configuration::new_with_workspace(
            vec![
                Profile::local("local", "sh", Vec::new(), None).unwrap(),
                Profile::ssh(
                    "remote",
                    "example.test",
                    22,
                    "alice",
                    "xterm-256color",
                    80,
                    24,
                )
                .unwrap(),
            ],
            workspace,
        )
        .unwrap();

        let serialized = configuration.to_toml().unwrap();
        assert!(serialized.contains("workspace_enabled = true"));
        assert!(serialized.contains("kind = \"sftp_session\""));
        assert_eq!(Configuration::parse(&serialized).unwrap(), configuration);
    }

    #[test]
    fn workspace_replacement_preserves_profiles_and_validates_references() {
        let configuration =
            Configuration::new(vec![
                Profile::local("development", "sh", Vec::new(), None).unwrap()
            ])
            .unwrap();
        let workspace = WorkspaceConfiguration::new(
            vec![WorkspaceTab::local_session("development-tab", "development").unwrap()],
            Some("development-tab".to_owned()),
        )
        .unwrap();

        let replacement = configuration.with_workspace(workspace).unwrap();

        assert!(replacement.workspace_enabled());
        assert_eq!(replacement.profiles(), configuration.profiles());
        assert_eq!(
            replacement.workspace().unwrap().focused_tab_id(),
            Some("development-tab")
        );

        let invalid_workspace = WorkspaceConfiguration::new(
            vec![WorkspaceTab::local_session("missing-tab", "missing").unwrap()],
            Some("missing-tab".to_owned()),
        )
        .unwrap();
        assert_eq!(
            configuration
                .with_workspace(invalid_workspace)
                .unwrap_err()
                .kind(),
            ConfigErrorKind::UnknownWorkspaceProfileReference
        );
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

        let error = Configuration::parse(
            r#"
schema_version = 1
workspace_enabled = true

[workspace]
launch_definition = "sh"
"#,
        )
        .unwrap_err();
        assert_eq!(error.kind(), ConfigErrorKind::Parse);
        assert!(!error.to_string().contains("launch_definition"));
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

        let workspace_secret = "workspace-secret-not-for-diagnostics";
        let workspace_error = Configuration::parse(&format!(
            r#"
schema_version = 1
workspace_enabled = true

[workspace]
password = "{workspace_secret}"
"#
        ))
        .unwrap_err();
        assert_eq!(
            workspace_error.kind(),
            ConfigErrorKind::ForbiddenSecretField
        );
        assert!(!workspace_error.to_string().contains(workspace_secret));
        assert!(!format!("{workspace_error:?}").contains(workspace_secret));
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
id = ""
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
    fn serial_profile_defaults_round_trip_and_convert_to_line_settings() {
        let profile = Profile::serial_with_defaults("bench-mcu", "/dev/ttyUSB0").unwrap();
        let serial = profile.as_serial().unwrap();
        assert_eq!(serial.device(), "/dev/ttyUSB0");
        assert_eq!(serial.baud_rate(), 115_200);
        assert_eq!(serial.data_bits(), SerialDataBits::Eight);
        assert_eq!(serial.parity(), SerialParity::None);
        assert_eq!(serial.stop_bits(), SerialStopBits::One);
        assert_eq!(serial.flow_control(), SerialFlowControl::None);

        let settings = serial.to_line_settings().unwrap();
        assert_eq!(settings.device(), "/dev/ttyUSB0");
        assert_eq!(settings.baud_rate(), 115_200);

        let configuration = Configuration::new(vec![profile]).unwrap();
        let serialized = configuration.to_toml().unwrap();
        assert!(serialized.contains("kind = \"serial\""));
        assert_eq!(Configuration::parse(&serialized).unwrap(), configuration);
    }

    #[test]
    fn validates_serial_profile_device_and_baud_rate() {
        let empty_device = Configuration::parse(
            r#"
schema_version = 1

[[profiles]]
kind = "serial"
id = "bad-serial"
device = ""
"#,
        )
        .unwrap_err();
        assert_eq!(empty_device.kind(), ConfigErrorKind::InvalidSerialProfile);

        let zero_baud = Configuration::parse(
            r#"
schema_version = 1

[[profiles]]
kind = "serial"
id = "bad-baud"
device = "/dev/ttyUSB0"
baud_rate = 0
"#,
        )
        .unwrap_err();
        assert_eq!(zero_baud.kind(), ConfigErrorKind::InvalidSerialProfile);
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
    fn workspace_serial_session_tab_requires_a_matching_serial_profile() {
        let serial_profile = Profile::serial_with_defaults("bench-mcu", "/dev/ttyUSB0").unwrap();
        let local_profile = Profile::local("shell", "/bin/sh", vec![], None).unwrap();
        let configuration = Configuration::new(vec![serial_profile, local_profile]).unwrap();

        let matching_workspace = WorkspaceConfiguration::new(
            vec![WorkspaceTab::serial_session("serial-tab", "bench-mcu").unwrap()],
            None,
        )
        .unwrap();
        configuration.with_workspace(matching_workspace).unwrap();

        let mismatched_workspace = WorkspaceConfiguration::new(
            vec![WorkspaceTab::serial_session("serial-tab", "shell").unwrap()],
            None,
        )
        .unwrap();
        let error = configuration
            .with_workspace(mismatched_workspace)
            .unwrap_err();
        assert_eq!(error.kind(), ConfigErrorKind::WorkspaceProfileKindMismatch);
    }

    #[test]
    fn validates_workspace_tab_references_order_and_focus() {
        let duplicate_tab = Configuration::parse(
            r#"
schema_version = 1
workspace_enabled = true

[workspace]

[[workspace.tabs]]
kind = "launcher"
id = "same"

[[workspace.tabs]]
kind = "settings"
id = "same"
"#,
        )
        .unwrap_err();
        assert_eq!(
            duplicate_tab.kind(),
            ConfigErrorKind::DuplicateWorkspaceTabIdentifier
        );

        let focused_tab = Configuration::parse(
            r#"
schema_version = 1
workspace_enabled = true

[workspace]
focused_tab_id = "not-saved"

[[workspace.tabs]]
kind = "launcher"
id = "launcher"
"#,
        )
        .unwrap_err();
        assert_eq!(
            focused_tab.kind(),
            ConfigErrorKind::UnknownFocusedWorkspaceTab
        );
        assert!(!focused_tab.to_string().contains("not-saved"));

        let unknown_profile = Configuration::parse(
            r#"
schema_version = 1
workspace_enabled = true

[workspace]

[[workspace.tabs]]
kind = "local_session"
id = "local-tab"
profile_id = "missing"
"#,
        )
        .unwrap_err();
        assert_eq!(
            unknown_profile.kind(),
            ConfigErrorKind::UnknownWorkspaceProfileReference
        );

        let wrong_kind = Configuration::parse(
            r#"
schema_version = 1
workspace_enabled = true

[[profiles]]
kind = "ssh"
id = "remote"
host = "example.test"
username = "alice"

[workspace]

[[workspace.tabs]]
kind = "local_session"
id = "local-tab"
profile_id = "remote"
"#,
        )
        .unwrap_err();
        assert_eq!(
            wrong_kind.kind(),
            ConfigErrorKind::WorkspaceProfileKindMismatch
        );
    }

    #[test]
    fn requires_an_enabled_nonempty_workspace_and_rejects_disabled_metadata() {
        let missing_workspace = Configuration::parse(
            r#"
schema_version = 1
workspace_enabled = true
"#,
        )
        .unwrap_err();
        assert_eq!(
            missing_workspace.kind(),
            ConfigErrorKind::WorkspaceMissingWhenEnabled
        );

        let empty_workspace = Configuration::parse(
            r#"
schema_version = 1
workspace_enabled = true

[workspace]
"#,
        )
        .unwrap_err();
        assert_eq!(empty_workspace.kind(), ConfigErrorKind::EmptyWorkspace);

        let disabled_workspace = Configuration::parse(
            r#"
schema_version = 1

[workspace]

[[workspace.tabs]]
kind = "launcher"
id = "launcher"
"#,
        )
        .unwrap_err();
        assert_eq!(
            disabled_workspace.kind(),
            ConfigErrorKind::WorkspacePresentWhenDisabled
        );
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
    fn invalid_workspace_reload_retains_the_previous_workspace() {
        let original = Configuration::parse(COMPLETE_CONFIGURATION).unwrap();
        let mut state = ConfigurationState::new(original.clone());

        let error = state
            .reload(
                r#"
schema_version = 1
workspace_enabled = true

[workspace]
focused_tab_id = "not-saved"

[[workspace.tabs]]
kind = "launcher"
id = "launcher"
"#,
            )
            .unwrap_err();

        assert_eq!(error.kind(), ConfigErrorKind::UnknownFocusedWorkspaceTab);
        assert_eq!(state.active(), &original);
        assert_eq!(state.active().workspace(), original.workspace());
        assert_eq!(state.last_error(), Some(error));
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
    fn saves_and_loads_a_complete_configuration() {
        let directory = TestDirectory::new();
        let path = directory.path().join("profiles.toml");
        let configuration = Configuration::parse(COMPLETE_CONFIGURATION).unwrap();

        configuration.save_to_path(&path).unwrap();

        assert_eq!(Configuration::load_from_path(&path).unwrap(), configuration);
    }

    #[test]
    fn atomically_saves_and_loads_an_opaque_credential_reference() {
        let directory = TestDirectory::new();
        let path = directory.path().join("profiles.toml");
        let configuration = Configuration::new(vec![Profile::ssh(
            "remote",
            "example.test",
            22,
            "alice",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_credential_reference(SecretReference::parse(CREDENTIAL_REFERENCE).unwrap())
        .unwrap()])
        .unwrap();

        configuration.save_to_path(&path).unwrap();

        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains(&format!("credential_id = \"{CREDENTIAL_REFERENCE}\"")));
        let loaded = Configuration::load_from_path(&path).unwrap();
        assert!(loaded.profiles()[0].credential_reference().is_some());
        assert_eq!(loaded, configuration);
    }

    #[test]
    fn stored_private_key_credential_kind_saves_loads_and_defaults_to_password() {
        let directory = TestDirectory::new();
        let path = directory.path().join("profiles.toml");
        let configuration = Configuration::new(vec![Profile::ssh(
            "remote",
            "example.test",
            22,
            "alice",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_credential_reference_kind(
            SecretReference::parse(CREDENTIAL_REFERENCE).unwrap(),
            CredentialKind::PrivateKey,
        )
        .unwrap()])
        .unwrap();

        configuration.save_to_path(&path).unwrap();

        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("credential_kind = \"private_key\""));
        let loaded = Configuration::load_from_path(&path).unwrap();
        let ssh = loaded.profiles()[0].as_ssh().unwrap();
        assert_eq!(ssh.credential_kind(), CredentialKind::PrivateKey);
        assert_eq!(loaded, configuration);

        // Existing saved profiles that predate `credential_kind` must still
        // load and default to Password, not fail or silently misclassify.
        let legacy = format!(
            r#"
schema_version = 1

[[profiles]]
kind = "ssh"
id = "legacy"
host = "example.test"
username = "alice"
credential_id = "{CREDENTIAL_REFERENCE}"
"#
        );
        fs::write(&path, legacy).unwrap();
        let loaded = Configuration::load_from_path(&path).unwrap();
        let ssh = loaded.profiles()[0].as_ssh().unwrap();
        assert_eq!(ssh.credential_kind(), CredentialKind::Password);
    }

    #[test]
    fn saves_and_loads_a_durable_session_configuration() {
        let directory = TestDirectory::new();
        let path = directory.path().join("profiles.toml");
        let configuration = Configuration::new(vec![
            Profile::ssh(
                "remote",
                "example.test",
                22,
                "alice",
                "xterm-256color",
                80,
                24,
            )
            .unwrap()
            .with_persistence(PersistenceProviderKind::Tmux, "build")
            .unwrap(),
            Profile::local("local-build", "/bin/sh", vec!["-l".to_owned()], None)
                .unwrap()
                .with_persistence(PersistenceProviderKind::Screen, "editor")
                .unwrap(),
        ])
        .unwrap();

        configuration.save_to_path(&path).unwrap();

        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("provider = \"tmux\""));
        assert!(saved.contains("session_name = \"build\""));
        let loaded = Configuration::load_from_path(&path).unwrap();
        let persistence = loaded.profiles()[0].persistence().unwrap();
        assert_eq!(persistence.provider(), PersistenceProviderKind::Tmux);
        assert_eq!(persistence.session_name(), "build");
        let local_persistence = loaded.profiles()[1].persistence().unwrap();
        assert_eq!(
            local_persistence.provider(),
            PersistenceProviderKind::Screen
        );
        assert_eq!(local_persistence.session_name(), "editor");
        assert_eq!(loaded, configuration);
    }

    #[test]
    fn ssh_profiles_with_zero_one_and_multiple_port_forwards_round_trip_through_toml() {
        let empty = Configuration::new(vec![ssh_profile_with_port_forwards(Vec::new())]).unwrap();
        let empty_document = empty.to_toml().unwrap();
        assert!(!empty_document.contains("port_forwards"));
        assert_eq!(Configuration::parse(&empty_document).unwrap(), empty);

        let single = Configuration::new(vec![ssh_profile_with_port_forwards(vec![
            ssh_port_forward(
                SshPortForwardDirection::Local,
                "127.0.0.1",
                8080,
                "app.internal",
                80,
            ),
        ])])
        .unwrap();
        let single_document = single.to_toml().unwrap();
        assert!(single_document.contains("direction = \"local\""));
        assert_eq!(Configuration::parse(&single_document).unwrap(), single);

        let multiple = Configuration::new(vec![ssh_profile_with_port_forwards(vec![
            ssh_port_forward(
                SshPortForwardDirection::Local,
                "127.0.0.1",
                15432,
                "db.internal",
                5432,
            ),
            ssh_port_forward(
                SshPortForwardDirection::Remote,
                "0.0.0.0",
                10022,
                "127.0.0.1",
                22,
            ),
        ])])
        .unwrap();
        let multiple_document = multiple.to_toml().unwrap();
        assert!(multiple_document.contains("direction = \"local\""));
        assert!(multiple_document.contains("direction = \"remote\""));
        assert_eq!(Configuration::parse(&multiple_document).unwrap(), multiple);
    }

    #[test]
    fn legacy_ssh_profiles_without_port_forwards_parse_as_an_empty_list() {
        let configuration = Configuration::parse(
            r#"
schema_version = 1

[[profiles]]
kind = "ssh"
id = "legacy"
host = "ssh.example.test"
username = "deploy"
"#,
        )
        .unwrap();

        assert!(configuration.profiles()[0]
            .as_ssh()
            .unwrap()
            .port_forwards()
            .is_empty());
    }

    #[test]
    fn duplicate_ssh_port_forward_bindings_are_rejected_within_one_profile() {
        let duplicate = ssh_port_forward(
            SshPortForwardDirection::Local,
            "127.0.0.1",
            8080,
            "app.internal",
            80,
        );
        let error = Profile::ssh(
            "remote",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .as_ssh()
        .unwrap()
        .clone()
        .with_port_forwards(vec![duplicate.clone(), duplicate])
        .unwrap_err();

        assert_eq!(error.kind(), ConfigErrorKind::DuplicateSshPortForward);
    }

    #[test]
    fn same_bind_host_and_port_are_allowed_when_ssh_port_forward_directions_differ() {
        let ssh = Profile::ssh(
            "remote",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .as_ssh()
        .unwrap()
        .clone()
        .with_port_forwards(vec![
            ssh_port_forward(
                SshPortForwardDirection::Local,
                "127.0.0.1",
                9000,
                "service.internal",
                9000,
            ),
            ssh_port_forward(
                SshPortForwardDirection::Remote,
                "127.0.0.1",
                9000,
                "127.0.0.1",
                9000,
            ),
        ])
        .unwrap();

        assert_eq!(ssh.port_forwards().len(), 2);
    }

    #[test]
    fn ssh_port_forwards_reject_zero_bind_and_destination_ports() {
        for (bind_port, destination_port) in [(0, 80), (8080, 0)] {
            let error = SshPortForwardConfiguration::new(
                SshPortForwardDirection::Local,
                "127.0.0.1",
                bind_port,
                "app.internal",
                destination_port,
            )
            .unwrap_err();
            assert_eq!(
                error.kind(),
                ConfigErrorKind::InvalidSshPortForwardConfiguration
            );
        }
    }

    #[test]
    fn ssh_port_forwards_reject_empty_control_character_and_secret_bearing_hosts() {
        for host in ["", "bad\nhost", "password:22"] {
            let bind_error = SshPortForwardConfiguration::new(
                SshPortForwardDirection::Local,
                host,
                8080,
                "app.internal",
                80,
            )
            .unwrap_err();
            assert_eq!(
                bind_error.kind(),
                ConfigErrorKind::InvalidSshPortForwardConfiguration
            );

            let destination_error = SshPortForwardConfiguration::new(
                SshPortForwardDirection::Local,
                "127.0.0.1",
                8080,
                host,
                80,
            )
            .unwrap_err();
            assert_eq!(
                destination_error.kind(),
                ConfigErrorKind::InvalidSshPortForwardConfiguration
            );
        }
    }

    #[test]
    fn schema_version_stays_one_when_an_ssh_profile_adds_port_forwards() {
        let configuration = Configuration::new(vec![ssh_profile_with_port_forwards(vec![
            ssh_port_forward(
                SshPortForwardDirection::Local,
                "127.0.0.1",
                8080,
                "app.internal",
                80,
            ),
        ])])
        .unwrap();

        assert_eq!(configuration.schema_version(), 1);
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
    fn rejects_an_invalid_persistent_session_name() {
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
        .with_persistence(PersistenceProviderKind::Tmux, "has spaces")
        .unwrap_err();

        assert_eq!(
            error.kind(),
            ConfigErrorKind::InvalidPersistenceConfiguration
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

    #[test]
    fn default_interface_settings_are_omitted_from_serialized_output() {
        let configuration = Configuration::empty();

        let serialized = configuration.to_toml().unwrap();

        assert!(!serialized.contains("[settings]"));
        assert_eq!(
            configuration.interface_settings().clone(),
            InterfaceSettings::DEFAULT
        );
    }

    #[test]
    fn non_default_interface_settings_round_trip_through_toml() {
        let configuration = Configuration::empty()
            .with_interface_settings(InterfaceSettings::new(
                ChipLayoutPreference::Wrap,
                false,
                true,
                false,
                true,
            ))
            .unwrap();

        let serialized = configuration.to_toml().unwrap();

        assert!(serialized.contains("[settings]"));
        assert_eq!(Configuration::parse(&serialized).unwrap(), configuration);
        assert_eq!(
            configuration.interface_settings().clone(),
            InterfaceSettings::new(ChipLayoutPreference::Wrap, false, true, false, true)
        );
    }

    #[test]
    fn default_sftp_local_directory_round_trips_through_toml() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .canonicalize()
            .expect("crate directory exists")
            .display()
            .to_string();
        let settings = InterfaceSettings::new(
            ChipLayoutPreference::SingleRowScroll,
            true,
            true,
            true,
            false,
        )
        .with_default_sftp_local_directory(Some(directory.clone()));
        let configuration = Configuration::empty()
            .with_interface_settings(settings.clone())
            .expect("existing SFTP directory is valid");

        let serialized = configuration.to_toml().unwrap();

        assert!(serialized.contains("default_sftp_local_directory"));
        assert_eq!(Configuration::parse(&serialized).unwrap(), configuration);
        assert_eq!(
            configuration
                .interface_settings()
                .default_sftp_local_directory()
                .map(|path| path.display().to_string()),
            Some(directory)
        );
    }

    #[test]
    fn missing_default_sftp_local_directory_is_rejected() {
        let missing = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("does-not-exist-default-sftp-local-directory");
        let configuration = Configuration::empty().with_interface_settings(
            InterfaceSettings::new(
                ChipLayoutPreference::SingleRowScroll,
                true,
                true,
                true,
                false,
            )
            .with_default_sftp_local_directory(Some(missing.display().to_string())),
        );

        let error = configuration.expect_err("missing SFTP directories must be rejected");
        assert_eq!(error.kind(), ConfigErrorKind::InvalidInterfaceSettings);
    }

    #[test]
    fn configuration_files_without_a_settings_table_parse_using_current_defaults() {
        let document = "schema_version = 1\n";

        let configuration = Configuration::parse(document).unwrap();

        assert_eq!(
            configuration.interface_settings().clone(),
            InterfaceSettings::DEFAULT
        );
    }

    #[test]
    fn older_settings_tables_default_close_confirmation_to_on() {
        let document = "schema_version = 1\n\n[settings]\nstatus_bar_visible = false\n";

        let configuration = Configuration::parse(document).unwrap();

        assert!(configuration.interface_settings().confirm_session_close());
        assert_eq!(
            configuration.interface_settings().terminal_font(),
            TerminalFontPreference::JetBrainsMono
        );
        assert!(!configuration.interface_settings().terminal_ligatures());
        assert_eq!(
            configuration.interface_settings().emoji_presentation(),
            EmojiPresentationPreference::Color
        );
    }

    #[test]
    fn terminal_typography_preferences_round_trip_and_reject_unknown_families() {
        let settings = InterfaceSettings::DEFAULT
            .with_terminal_typography(TerminalFontPreference::IosevkaTerm, true);
        let configuration = Configuration::empty()
            .with_interface_settings(settings.clone())
            .unwrap();

        let serialized = configuration.to_toml().unwrap();

        assert!(serialized.contains("terminal_font = \"iosevka-term\""));
        assert!(serialized.contains("terminal_ligatures = true"));
        assert_eq!(
            Configuration::parse(&serialized)
                .unwrap()
                .interface_settings()
                .clone(),
            settings
        );
        assert!(Configuration::parse(
            "schema_version = 1\n\n[settings]\nterminal_font = \"system\"\n"
        )
        .is_err());
    }

    #[test]
    fn emoji_presentation_round_trips_and_rejects_unknown_policies() {
        let settings = InterfaceSettings::DEFAULT
            .with_emoji_presentation(EmojiPresentationPreference::Monochrome);
        let configuration = Configuration::empty()
            .with_interface_settings(settings.clone())
            .unwrap();

        let serialized = configuration.to_toml().unwrap();

        assert!(serialized.contains("emoji_presentation = \"monochrome\""));
        assert_eq!(
            Configuration::parse(&serialized)
                .unwrap()
                .interface_settings()
                .clone(),
            settings
        );
        assert!(Configuration::parse(
            "schema_version = 1\n\n[settings]\nemoji_presentation = \"system\"\n"
        )
        .is_err());
    }

    #[test]
    fn scroll_speed_preference_round_trips_through_toml_and_defaults_to_normal() {
        // Feature request #67.
        let settings = InterfaceSettings::DEFAULT.with_scroll_speed(ScrollSpeedPreference::Fast);
        let configuration = Configuration::empty()
            .with_interface_settings(settings.clone())
            .unwrap();

        let serialized = configuration.to_toml().unwrap();

        assert!(serialized.contains("scroll_speed = \"fast\""));
        assert_eq!(
            Configuration::parse(&serialized)
                .unwrap()
                .interface_settings()
                .clone(),
            settings
        );

        // An older settings table with no `scroll_speed` key defaults to
        // `Normal`, preserving today's fixed pixel-to-row behavior.
        let older_document = "schema_version = 1\n\n[settings]\nstatus_bar_visible = false\n";
        assert_eq!(
            Configuration::parse(older_document)
                .unwrap()
                .interface_settings()
                .scroll_speed(),
            ScrollSpeedPreference::Normal
        );
    }

    #[test]
    fn scroll_speed_clickstop_index_mapping_is_stable_and_saturating() {
        for (index, expected) in ScrollSpeedPreference::ALL.into_iter().enumerate() {
            assert_eq!(expected.index(), index);
            assert_eq!(ScrollSpeedPreference::from_index(index), expected);
        }
        // Out-of-range indices saturate to the fastest clickstop rather than
        // panicking, since a slider position should never be able to select
        // a nonexistent step.
        assert_eq!(
            ScrollSpeedPreference::from_index(99),
            ScrollSpeedPreference::VeryFast
        );
    }

    #[test]
    fn scrollback_limit_round_trips_and_defaults_to_sixty_four_mib() {
        let settings =
            InterfaceSettings::DEFAULT.with_scrollback_limit(ScrollbackLimitPreference::MiB16);
        let configuration = Configuration::empty()
            .with_interface_settings(settings.clone())
            .unwrap();

        let serialized = configuration.to_toml().unwrap();

        assert!(serialized.contains("scrollback_limit = \"16-mib\""));
        assert_eq!(
            Configuration::parse(&serialized)
                .unwrap()
                .interface_settings()
                .clone(),
            settings
        );
        assert_eq!(
            Configuration::parse("schema_version = 1\n\n[settings]\nstatus_bar_visible = false\n")
                .unwrap()
                .interface_settings()
                .scrollback_limit(),
            ScrollbackLimitPreference::MiB64
        );
        assert!(Configuration::parse(
            "schema_version = 1\n\n[settings]\nscrollback_limit = \"unbounded\"\n"
        )
        .is_err());
    }

    #[test]
    fn scrollback_limit_clickstops_have_stable_labels_and_byte_values() {
        assert_eq!(
            ScrollbackLimitPreference::ALL.map(ScrollbackLimitPreference::label),
            ["Disabled", "16 MiB", "64 MiB", "256 MiB"]
        );
        assert_eq!(
            ScrollbackLimitPreference::ALL.map(ScrollbackLimitPreference::bytes),
            [0, 16 * 1024 * 1024, 64 * 1024 * 1024, 256 * 1024 * 1024]
        );
    }

    #[test]
    fn quick_switch_overlay_preference_round_trips_through_toml_and_defaults_to_off() {
        // Feature request #69.
        let settings = InterfaceSettings::DEFAULT.with_quick_switch_overlay(true);
        let configuration = Configuration::empty()
            .with_interface_settings(settings.clone())
            .unwrap();

        let serialized = configuration.to_toml().unwrap();

        assert!(serialized.contains("quick_switch_overlay = true"));
        assert_eq!(
            Configuration::parse(&serialized)
                .unwrap()
                .interface_settings()
                .clone(),
            settings
        );

        // An older settings table with no `quick_switch_overlay` key
        // defaults to off, preserving today's chip presentation.
        let older_document = "schema_version = 1\n\n[settings]\nstatus_bar_visible = false\n";
        assert!(!Configuration::parse(older_document)
            .unwrap()
            .interface_settings()
            .quick_switch_overlay());

        // The off state is the default and is omitted from serialization
        // entirely, matching the other opt-in booleans here.
        let default_serialized = Configuration::empty()
            .with_interface_settings(InterfaceSettings::DEFAULT)
            .unwrap()
            .to_toml()
            .unwrap();
        assert!(!default_serialized.contains("quick_switch_overlay"));
    }

    #[test]
    fn compact_launcher_grid_preference_round_trips_through_toml_and_defaults_to_off() {
        // Feature request #64.
        let settings = InterfaceSettings::DEFAULT.with_compact_launcher_grid(true);
        let configuration = Configuration::empty()
            .with_interface_settings(settings.clone())
            .unwrap();

        let serialized = configuration.to_toml().unwrap();

        assert!(serialized.contains("compact_launcher_grid = true"));
        assert_eq!(
            Configuration::parse(&serialized)
                .unwrap()
                .interface_settings()
                .clone(),
            settings
        );

        // An older settings table with no `compact_launcher_grid` key
        // defaults to off, preserving today's single-column Launcher list.
        let older_document = "schema_version = 1\n\n[settings]\nstatus_bar_visible = false\n";
        assert!(!Configuration::parse(older_document)
            .unwrap()
            .interface_settings()
            .compact_launcher_grid());

        // The off state is the default and is omitted from serialization
        // entirely, matching the other opt-in booleans here.
        let default_serialized = Configuration::empty()
            .with_interface_settings(InterfaceSettings::DEFAULT)
            .unwrap()
            .to_toml()
            .unwrap();
        assert!(!default_serialized.contains("compact_launcher_grid"));
    }

    #[test]
    fn pulse_new_output_dot_preference_round_trips_through_toml_and_defaults_to_off() {
        // Feature request #68.
        let settings = InterfaceSettings::DEFAULT.with_pulse_new_output_dot(true);
        let configuration = Configuration::empty()
            .with_interface_settings(settings.clone())
            .unwrap();

        let serialized = configuration.to_toml().unwrap();

        assert!(serialized.contains("pulse_new_output_dot = true"));
        assert_eq!(
            Configuration::parse(&serialized)
                .unwrap()
                .interface_settings()
                .clone(),
            settings
        );

        // An older settings table with no `pulse_new_output_dot` key
        // defaults to off, preserving today's static status dot.
        let older_document = "schema_version = 1\n\n[settings]\nstatus_bar_visible = false\n";
        assert!(!Configuration::parse(older_document)
            .unwrap()
            .interface_settings()
            .pulse_new_output_dot());

        // The off state is the default and is omitted from serialization
        // entirely, matching the other opt-in booleans here.
        let default_serialized = Configuration::empty()
            .with_interface_settings(InterfaceSettings::DEFAULT)
            .unwrap()
            .to_toml()
            .unwrap();
        assert!(!default_serialized.contains("pulse_new_output_dot"));
    }

    #[test]
    fn show_resumable_sessions_preference_round_trips_through_toml_and_defaults_to_off() {
        // Feature request #70.
        let settings = InterfaceSettings::DEFAULT.with_show_resumable_sessions(true);
        let configuration = Configuration::empty()
            .with_interface_settings(settings.clone())
            .unwrap();

        let serialized = configuration.to_toml().unwrap();

        assert!(serialized.contains("show_resumable_sessions = true"));
        assert_eq!(
            Configuration::parse(&serialized)
                .unwrap()
                .interface_settings()
                .clone(),
            settings
        );

        // An older settings table with no `show_resumable_sessions` key
        // defaults to off, preserving today's Launcher behavior.
        let older_document = "schema_version = 1\n\n[settings]\nstatus_bar_visible = false\n";
        assert!(!Configuration::parse(older_document)
            .unwrap()
            .interface_settings()
            .show_resumable_sessions());

        // The off state is the default and is omitted from serialization
        // entirely, matching the other opt-in booleans here.
        let default_serialized = Configuration::empty()
            .with_interface_settings(InterfaceSettings::DEFAULT)
            .unwrap()
            .to_toml()
            .unwrap();
        assert!(!default_serialized.contains("show_resumable_sessions"));
    }

    #[test]
    fn settings_table_rejects_unknown_fields() {
        let document = "schema_version = 1\n\n[settings]\ntheme = \"dark\"\n";

        let error = Configuration::parse(document).unwrap_err();

        assert_eq!(error.kind(), ConfigErrorKind::Parse);
    }

    #[test]
    fn with_workspace_preserves_previously_saved_interface_settings() {
        let configuration = Configuration::empty()
            .with_interface_settings(InterfaceSettings::new(
                ChipLayoutPreference::Wrap,
                false,
                true,
                false,
                true,
            ))
            .unwrap();
        let workspace =
            WorkspaceConfiguration::new(vec![WorkspaceTab::launcher("launcher").unwrap()], None)
                .unwrap();

        let replacement = configuration.with_workspace(workspace).unwrap();

        assert_eq!(
            replacement.interface_settings().clone(),
            InterfaceSettings::new(ChipLayoutPreference::Wrap, false, true, false, true)
        );
    }

    #[test]
    fn without_workspace_clears_saved_tabs_but_keeps_other_configuration_state() {
        let workspace =
            WorkspaceConfiguration::new(vec![WorkspaceTab::launcher("launcher").unwrap()], None)
                .unwrap();
        let configuration = Configuration::empty()
            .with_interface_settings(InterfaceSettings::new(
                ChipLayoutPreference::Wrap,
                false,
                true,
                false,
                true,
            ))
            .unwrap()
            .with_workspace(workspace)
            .unwrap();
        assert!(configuration.workspace_enabled());
        assert!(configuration.workspace().is_some());

        let cleared = configuration.without_workspace();

        assert!(!cleared.workspace_enabled());
        assert!(cleared.workspace().is_none());
        // Turning off workspace persistence must not also discard unrelated
        // settings/profile state.
        assert_eq!(
            cleared.interface_settings().clone(),
            configuration.interface_settings().clone()
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
