//! Versioned, secret-free application configuration, profiles, and workspace metadata.
//!
//! This crate deliberately owns document parsing and validation, but not file
//! watching, GUI editing, credentials, or runtime session restoration.
//! Configuration documents contain only reusable launch metadata and safe,
//! metadata-only workspace tab descriptors.

use std::{collections::HashSet, fs, io::Write, path::Path, sync::atomic::AtomicU64};

use festerm_secret_store::SecretReference;
use serde::{Deserialize, Serialize};

use errors::parse_error;
use file_io::{
    parent_directory, read_file_error, replace_file, sync_parent_directory, validate_target_file,
    TemporaryFile,
};
use profiles::CredentialReference;

mod errors;
mod file_io;
mod keyboard;
mod profiles;
mod settings;
mod workspace;
pub use errors::{
    ConfigError, ConfigErrorKind, ConfigurationFileError, ConfigurationFileErrorKind,
    SourceLocation,
};
pub use file_io::ConfigurationState;
pub use keyboard::{Chord, KeyboardAction, KeyboardBindings, KeyboardOverride, KeyboardScope};
pub use profiles::{
    CredentialKind, KnownHostEntry, LocalProfileConfiguration, PersistenceConfiguration,
    PersistenceProviderKind, Profile, ProfileUsageEntry, RemoteProfileKind, SerialDataBits,
    SerialFlowControl, SerialParity, SerialProfileConfiguration, SerialStopBits,
    SshPortForwardConfiguration, SshPortForwardDirection, SshProfileConfiguration,
};
pub use settings::{
    ChipLayoutPreference, EditorSettings, EmojiPresentationPreference, InterfaceSettings,
    ScrollSpeedPreference,
    ScrollbackLimitPreference, SftpPaneOrderPreference, TerminalFontPreference,
};
pub use workspace::{
    LauncherTabConfiguration, ProfilesTabConfiguration, SessionTabConfiguration,
    SettingsTabConfiguration, WorkspaceConfiguration, WorkspaceTab, WorkspaceWindow,
    WorkspaceWindowGeometry,
};

/// The only document schema accepted by this initial configuration slice.
pub const SCHEMA_VERSION: u32 = 1;

pub(crate) const TEMPORARY_FILE_ATTEMPTS: u32 = 128;

pub(crate) static NEXT_TEMPORARY_FILE_ID: AtomicU64 = AtomicU64::new(0);

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    profile_usage: Vec<ProfileUsageEntry>,
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
            profile_usage: Vec::new(),
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
            profile_usage: Vec::new(),
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
        replacement.profile_usage = self.profile_usage.clone();
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
        replacement
            .profile_usage
            .retain(|entry| entry.profile != identifier);
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
            profile_usage: Vec::new(),
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
            profile_usage: raw.profile_usage,
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

    /// Returns when a profile was last launched, in whole seconds since the
    /// Unix epoch, if it has ever been launched on this installation.
    pub fn profile_last_used(&self, identifier: &str) -> Option<u64> {
        self.profile_usage
            .iter()
            .find(|entry| entry.profile == identifier)
            .map(|entry| entry.last_used_unix_seconds)
    }

    /// Returns a complete validated replacement recording that `identifier`
    /// was launched at `unix_seconds`.
    ///
    /// Usage is app-maintained observation, not user-authored definition, so
    /// it lives in its own section rather than inside the profile. That keeps
    /// a hand-edited profile table byte-stable across launches and lets the
    /// record be dropped without touching the definition it describes.
    /// Recording usage for an unknown profile is rejected so the section can
    /// never outlive the profiles it references.
    pub fn with_profile_last_used(
        &self,
        identifier: &str,
        unix_seconds: u64,
    ) -> Result<Self, ConfigError> {
        if self.profile(identifier).is_none() {
            return Err(ConfigError::new(ConfigErrorKind::UnknownProfileReference));
        }
        let mut replacement = self.clone();
        match replacement
            .profile_usage
            .iter_mut()
            .find(|entry| entry.profile == identifier)
        {
            Some(entry) => entry.last_used_unix_seconds = unix_seconds,
            None => replacement.profile_usage.push(ProfileUsageEntry {
                profile: identifier.to_owned(),
                last_used_unix_seconds: unix_seconds,
            }),
        }
        replacement.validate()?;
        Ok(replacement)
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
        let mut used_profiles = HashSet::with_capacity(self.profile_usage.len());
        for entry in &self.profile_usage {
            validate_identifier(&entry.profile)?;
            if !identifiers.contains(entry.profile.as_str()) {
                return Err(ConfigError::new(ConfigErrorKind::UnknownProfileReference));
            }
            if !used_profiles.insert(entry.profile.as_str()) {
                return Err(ConfigError::new(
                    ConfigErrorKind::DuplicateProfileIdentifier,
                ));
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
    #[serde(default)]
    profile_usage: Vec<ProfileUsageEntry>,
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
            profile_usage: raw.profile_usage,
        };
        configuration.validate().map_err(serde::de::Error::custom)?;
        Ok(configuration)
    }
}

pub(crate) const fn is_false(value: &bool) -> bool {
    !*value
}

pub(crate) const fn default_true() -> bool {
    true
}

pub(crate) const fn is_true(value: &bool) -> bool {
    *value
}

pub(crate) fn validate_identifier(identifier: &str) -> Result<(), ConfigError> {
    if identifier.is_empty()
        || identifier.chars().count() > 200
        || contains_control_character(identifier)
        || identifier.trim() != identifier
    {
        return Err(ConfigError::new(ConfigErrorKind::InvalidProfileIdentifier));
    }
    Ok(())
}

pub(crate) fn contains_control_character(value: &str) -> bool {
    value.chars().any(char::is_control)
}

pub(crate) fn validate_stored_path_setting(path: &str) -> Result<(), ConfigError> {
    if path.is_empty() || contains_control_character(path) || contains_secret_bearing_value(path) {
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

pub(crate) fn contains_secret_bearing_value(value: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, process, sync::atomic::Ordering};

    const CREDENTIAL_REFERENCE: &str = "550e8400-e29b-41d4-a716-446655440000";

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
    fn sftp_profiles_restore_only_into_sftp_workspace_surfaces() {
        let profile = Profile::sftp("files", "sftp.example.test", 22, "deploy", true).unwrap();
        let ssh_workspace = WorkspaceConfiguration::new(
            vec![WorkspaceTab::ssh_session("remote", "files").unwrap()],
            Some("remote".to_owned()),
        )
        .unwrap();
        assert_eq!(
            Configuration::new_with_workspace(vec![profile.clone()], ssh_workspace)
                .unwrap_err()
                .kind(),
            ConfigErrorKind::WorkspaceProfileKindMismatch
        );

        let sftp_workspace = WorkspaceConfiguration::new(
            vec![
                WorkspaceTab::sftp_file_manager("files-gui", "files").unwrap(),
                WorkspaceTab::sftp_session("files-terminal", "files").unwrap(),
            ],
            Some("files-gui".to_owned()),
        )
        .unwrap();
        assert!(Configuration::new_with_workspace(vec![profile], sftp_workspace).is_ok());
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

    /// A workspace covering several windows (ADR 0033) keeps the primary
    /// window's tabs where they have always been, so the file still restores
    /// correctly, and lists only the additional windows separately.
    #[test]
    fn restores_additional_windows_with_their_own_tabs_and_geometry() {
        let configuration = Configuration::parse(
            r#"
schema_version = 1
workspace_enabled = true

[workspace]
focused_tab_id = "primary-launcher"

[[workspace.tabs]]
kind = "launcher"
id = "primary-launcher"

[[workspace.windows]]
focused_tab_id = "second-settings"

[workspace.windows.geometry]
x = 120.0
y = 64.0
width = 900.0
height = 600.0

[[workspace.windows.tabs]]
kind = "settings"
id = "second-settings"
"#,
        )
        .expect("a workspace with an additional window is valid");

        let workspace = configuration.workspace().expect("a saved workspace");
        assert_eq!(workspace.tabs().len(), 1);
        assert_eq!(workspace.focused_tab_id(), Some("primary-launcher"));

        let windows = workspace.windows();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].tabs().len(), 1);
        assert_eq!(windows[0].tabs()[0].identifier(), "second-settings");
        assert_eq!(windows[0].focused_tab_id(), Some("second-settings"));
        let geometry = windows[0].geometry().expect("saved geometry");
        assert_eq!(geometry.position(), (120.0, 64.0));
        assert_eq!(geometry.size(), (900.0, 600.0));
    }

    /// A single-window workspace saved by an older build has no `windows`
    /// key at all and must keep restoring exactly as it did.
    #[test]
    fn a_workspace_without_additional_windows_still_restores_one_window() {
        let configuration = Configuration::parse(
            r#"
schema_version = 1
workspace_enabled = true

[workspace]

[[workspace.tabs]]
kind = "launcher"
id = "only"
"#,
        )
        .expect("a single-window workspace is valid");

        let workspace = configuration.workspace().expect("a saved workspace");
        assert_eq!(workspace.tabs().len(), 1);
        assert!(
            workspace.windows().is_empty(),
            "an absent windows key must mean no additional windows, not a parse failure"
        );

        let round_tripped = configuration.to_toml().expect("a workspace serialises");
        assert!(
            !round_tripped.contains("workspace.windows"),
            "a single-window workspace must not gain a windows table: {round_tripped}"
        );
    }

    /// Tab identifiers address tabs across the whole workspace, so a
    /// collision between two windows is as invalid as one inside a window.
    #[test]
    fn rejects_invalid_additional_windows() {
        let duplicate_across_windows = Configuration::parse(
            r#"
schema_version = 1
workspace_enabled = true

[workspace]

[[workspace.tabs]]
kind = "launcher"
id = "same"

[[workspace.windows]]

[[workspace.windows.tabs]]
kind = "settings"
id = "same"
"#,
        )
        .unwrap_err();
        assert_eq!(
            duplicate_across_windows.kind(),
            ConfigErrorKind::DuplicateWorkspaceTabIdentifier
        );

        // A window with no tabs would restore as an empty frame nobody asked
        // for; an emptied window collapses instead of being saved.
        let empty_window = Configuration::parse(
            r#"
schema_version = 1
workspace_enabled = true

[workspace]

[[workspace.tabs]]
kind = "launcher"
id = "primary"

[[workspace.windows]]
tabs = []
"#,
        )
        .unwrap_err();
        assert_eq!(empty_window.kind(), ConfigErrorKind::EmptyWorkspace);

        let unknown_focus = Configuration::parse(
            r#"
schema_version = 1
workspace_enabled = true

[workspace]

[[workspace.tabs]]
kind = "launcher"
id = "primary"

[[workspace.windows]]
focused_tab_id = "nowhere"

[[workspace.windows.tabs]]
kind = "settings"
id = "second"
"#,
        )
        .unwrap_err();
        assert_eq!(
            unknown_focus.kind(),
            ConfigErrorKind::UnknownFocusedWorkspaceTab
        );

        let bad_geometry = Configuration::parse(
            r#"
schema_version = 1
workspace_enabled = true

[workspace]

[[workspace.tabs]]
kind = "launcher"
id = "primary"

[[workspace.windows]]

[workspace.windows.geometry]
x = 0.0
y = 0.0
width = 0.0
height = 600.0

[[workspace.windows.tabs]]
kind = "settings"
id = "second"
"#,
        )
        .unwrap_err();
        assert_eq!(
            bad_geometry.kind(),
            ConfigErrorKind::InvalidWorkspaceWindowGeometry
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
    fn sftp_profiles_round_trip_gui_and_terminal_launch_modes() {
        for gui_mode in [true, false] {
            let configuration = Configuration::new(vec![Profile::sftp(
                "files",
                "sftp.example.test",
                22,
                "deploy",
                gui_mode,
            )
            .unwrap()])
            .unwrap();
            let document = configuration.to_toml().unwrap();
            let loaded = Configuration::parse(&document).unwrap();
            let sftp = loaded.profiles()[0].as_ssh().unwrap();

            assert_eq!(sftp.profile_kind(), RemoteProfileKind::Sftp);
            assert_eq!(sftp.sftp_gui_mode(), gui_mode);
            assert!(document.contains("profile_kind = \"sftp\""));
            assert_eq!(
                document.contains("sftp_gui_mode = false"),
                !gui_mode,
                "default GUI mode should be omitted while terminal mode is persisted"
            );
        }
    }

    #[test]
    fn legacy_ssh_profiles_default_to_ssh_with_gui_sftp_launches() {
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
        let ssh = configuration.profiles()[0].as_ssh().unwrap();

        assert_eq!(ssh.profile_kind(), RemoteProfileKind::Ssh);
        assert!(ssh.sftp_gui_mode());
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
    fn missing_default_sftp_local_directory_is_allowed() {
        let missing = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("does-not-exist-default-sftp-local-directory");
        let configuration = Configuration::empty()
            .with_interface_settings(
                InterfaceSettings::new(
                    ChipLayoutPreference::SingleRowScroll,
                    true,
                    true,
                    true,
                    false,
                )
                .with_default_sftp_local_directory(Some(missing.display().to_string())),
            )
            .expect("stored SFTP directory metadata must not require an existing path");

        assert_eq!(
            configuration
                .interface_settings()
                .default_sftp_local_directory(),
            Some(missing.as_path())
        );
    }

    #[test]
    fn interface_settings_parse_sftp_pane_order_additively() {
        let settings =
            InterfaceSettings::DEFAULT.with_sftp_pane_order(SftpPaneOrderPreference::RemoteLeft);
        let configuration = Configuration::empty()
            .with_interface_settings(settings.clone())
            .expect("remote-left pane order is valid interface metadata");

        let serialized = configuration.to_toml().unwrap();

        assert!(serialized.contains("sftp_pane_order"));
        assert_eq!(Configuration::parse(&serialized).unwrap(), configuration);
        assert_eq!(
            configuration.interface_settings().sftp_pane_order(),
            SftpPaneOrderPreference::RemoteLeft
        );
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
    fn powershell_preference_defaults_to_on_and_serializes_only_when_disabled() {
        let default_configuration = Configuration::empty()
            .with_interface_settings(InterfaceSettings::DEFAULT)
            .unwrap();
        let default_serialized = default_configuration.to_toml().unwrap();
        assert!(default_configuration
            .interface_settings()
            .prefer_powershell());
        assert!(!default_serialized.contains("prefer_powershell"));

        let disabled = InterfaceSettings::DEFAULT.with_prefer_powershell(false);
        let configuration = Configuration::empty()
            .with_interface_settings(disabled.clone())
            .unwrap();
        let serialized = configuration.to_toml().unwrap();
        assert!(serialized.contains("prefer_powershell = false"));
        assert_eq!(
            Configuration::parse(&serialized)
                .unwrap()
                .interface_settings(),
            &disabled
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
    fn durable_session_status_bar_preference_round_trips_through_toml_and_defaults_to_off() {
        // Feature request #168.
        let settings = InterfaceSettings::DEFAULT.with_show_durable_session_in_status_bar(true);
        let configuration = Configuration::empty()
            .with_interface_settings(settings.clone())
            .unwrap();

        let serialized = configuration.to_toml().unwrap();

        assert!(serialized.contains("show_durable_session_in_status_bar = true"));
        assert_eq!(
            Configuration::parse(&serialized)
                .unwrap()
                .interface_settings()
                .clone(),
            settings
        );

        // An older settings table with no key keeps today's status bar.
        let older_document = "schema_version = 1\n\n[settings]\nstatus_bar_visible = false\n";
        assert!(!Configuration::parse(older_document)
            .unwrap()
            .interface_settings()
            .show_durable_session_in_status_bar());

        let default_serialized = Configuration::empty()
            .with_interface_settings(InterfaceSettings::DEFAULT)
            .unwrap()
            .to_toml()
            .unwrap();
        assert!(!default_serialized.contains("show_durable_session_in_status_bar"));
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
