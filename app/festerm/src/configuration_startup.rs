use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex, OnceLock, TryLockError,
    },
};

use directories::ProjectDirs;
use festerm_config::{Configuration, ConfigurationFileErrorKind};

const CONFIG_PATH_ENV: &str = "FESTERM_CONFIG_PATH";
const CONFIG_FILE_NAME: &str = "config.toml";

/// Content-free reason why configuration could not be selected or loaded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConfigurationLoadFailure {
    Invalid,
    Unreadable,
    OverrideUnavailable,
    NativeLocationUnavailable,
    OutcomeUnknown,
    ReconciliationRequired,
    SaveBusy,
}

pub(crate) const UNKNOWN_SAVE_MESSAGE: &str = "Configuration may have changed on disk, but the save outcome is unknown. Further saves are disabled; reconcile manually and restart all windows to reload.";
pub(crate) const UNKNOWN_RESOURCE_SAVE_MESSAGE: &str = "Configuration may have changed on disk, but the save outcome is unknown. Potentially referenced resources from this attempt were retained. Further saves are disabled; reconcile manually and restart all windows to reload.";
pub(crate) const BLOCKED_SAVE_MESSAGE: &str = "This save was blocked before writing because an earlier configuration save may have an unknown outcome; reconcile manually and restart all windows to reload.";
pub(crate) const RECONCILE_SAVE_MESSAGE: &str = "A configuration save may have changed the disk. Further saves are disabled; reconcile manually and restart all windows to reload.";
pub(crate) const SAVE_BUSY_MESSAGE: &str =
    "This save was blocked before writing because another configuration save was in progress.";
pub(crate) const SESSION_NAMES_PRE_WRITE_FAILURE_MESSAGE: &str = "This session naming change applied locally, but this save did not write its candidate. Check configuration access.";
const SAVE_IN_PROGRESS_MESSAGE: &str = "A configuration save is in progress; its outcome is not yet known. If its outcome is unknown or further saves are disabled, reconcile manually and restart all windows.";
pub(crate) const SAVE_GATE_HEALTHY_MESSAGE: &str =
    "At the last check, no in-process save was unresolved or in progress.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SaveGateState {
    Healthy,
    InProgress,
    ReconciliationRequired,
}

#[derive(Clone, Copy)]
pub(crate) enum SecretCleanupNotice {
    ProfilePending,
    ProfileChanged,
    ProfilePrevious,
    ProfileSaveFailed,
    Tunnel,
    BrowserPending,
    BrowserPrevious,
    BrowserSaveFailed,
    ConfigurationUnknown,
    ConfigurationResourceUnknown,
    ConfigurationBlocked,
    ProfileRetained,
    TunnelRetained,
    BrowserRetained,
    ImportRetained,
    ImportCleanupFailed,
}

impl SecretCleanupNotice {
    pub(crate) const ALL: [Self; 16] = [
        Self::ProfilePending,
        Self::ProfileChanged,
        Self::ProfilePrevious,
        Self::ProfileSaveFailed,
        Self::Tunnel,
        Self::BrowserPending,
        Self::BrowserPrevious,
        Self::BrowserSaveFailed,
        Self::ConfigurationUnknown,
        Self::ConfigurationResourceUnknown,
        Self::ConfigurationBlocked,
        Self::ProfileRetained,
        Self::TunnelRetained,
        Self::BrowserRetained,
        Self::ImportRetained,
        Self::ImportCleanupFailed,
    ];

    pub(crate) const fn bit(self) -> u16 {
        1 << (self as u16)
    }

    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::ProfilePending => "A profile credential could not be removed after cancellation or window closure. Reconcile secure storage manually.",
            Self::ProfileChanged => "Profile changed while its credential was being saved, and the new native secret could not be removed. Reconcile secure storage manually.",
            Self::ProfilePrevious => "Profile credential saved, but the previous native secret could not be removed. Reconcile secure storage manually.",
            Self::ProfileSaveFailed => "A profile credential was not linked because configuration could not be saved; native-secret cleanup also failed. Reconcile secure storage manually.",
            Self::Tunnel => "A Dev Tunnel workflow credential could not be removed after cancellation or failure. Reconcile secure storage manually.",
            Self::BrowserPending => "A Dev Tunnel account credential could not be removed after cancellation or window closure. Reconcile secure storage manually.",
            Self::BrowserPrevious => "The account was saved, but the previous native credential could not be removed. Reconcile secure storage manually.",
            Self::BrowserSaveFailed => "The account could not be saved, and the new native credential could not be removed. Reconcile secure storage manually.",
            Self::ConfigurationUnknown => UNKNOWN_SAVE_MESSAGE,
            Self::ConfigurationResourceUnknown => UNKNOWN_RESOURCE_SAVE_MESSAGE,
            Self::ConfigurationBlocked => BLOCKED_SAVE_MESSAGE,
            Self::ProfileRetained => "The blocked save retained a candidate profile credential because exclusive ownership is uncertain. Reconcile secure storage manually; if saves remain disabled, also reconcile configuration and restart all windows.",
            Self::TunnelRetained => "The blocked save retained candidate Dev Tunnel workflow credentials because exclusive ownership is uncertain. Reconcile secure storage manually; if saves remain disabled, also reconcile configuration and restart all windows.",
            Self::BrowserRetained => "The blocked save retained a candidate Dev Tunnel account credential because exclusive ownership is uncertain. Reconcile secure storage manually; if saves remain disabled, also reconcile configuration and restart all windows.",
            Self::ImportRetained => "The blocked import retained a candidate destination file because exclusive ownership is uncertain; reconcile the managed tunnel profiles and configuration manually. If saves remain disabled, restart all windows.",
            Self::ImportCleanupFailed => "A new public tunnel destination file could not be removed after import failed. Inspect managed tunnel profiles and reconcile manually; do not remove files still referenced by any configuration.",
        }
    }
}

/// Content-free outcome of selecting, loading, or automatically saving
/// configuration.
///
/// This deliberately retains neither the selected path nor source TOML. The
/// application can show it safely in Settings and diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConfigurationStartupStatus {
    Loaded,
    Missing,
    InitialFailure(ConfigurationLoadFailure),
    WorkspaceSaved,
    WorkspaceSaveFailure(ConfigurationLoadFailure),
    SessionNamesSaved,
    SessionNamesSaveFailure(ConfigurationLoadFailure),
    PasswordCredentialSaved,
    PasswordCredentialSaveFailure(ConfigurationLoadFailure),
    InterfaceSettingsSaved,
    InterfaceSettingsSaveFailure(ConfigurationLoadFailure),
    KnownHostTrustSaved,
    KnownHostTrustSaveFailure(ConfigurationLoadFailure),
    ProfileSaved,
    ProfileSaveFailure(ConfigurationLoadFailure),
    HostRecipeSaved,
    HostRecipeExported,
    HostRecipeCommandFailure(festerm_config::host_recipes::RecipeError),
    HostRecipeSaveFailure(ConfigurationLoadFailure),
    ProfileDeleted,
    ProfileDeleteFailure(ConfigurationLoadFailure),
    ProfilesReordered,
    ProfilesReorderFailure(ConfigurationLoadFailure),
}

impl ConfigurationStartupStatus {
    pub(crate) const fn save_outcome_unknown(self) -> bool {
        matches!(
            self,
            Self::WorkspaceSaveFailure(ConfigurationLoadFailure::OutcomeUnknown)
                | Self::SessionNamesSaveFailure(ConfigurationLoadFailure::OutcomeUnknown)
                | Self::PasswordCredentialSaveFailure(ConfigurationLoadFailure::OutcomeUnknown)
                | Self::InterfaceSettingsSaveFailure(ConfigurationLoadFailure::OutcomeUnknown)
                | Self::KnownHostTrustSaveFailure(ConfigurationLoadFailure::OutcomeUnknown)
                | Self::ProfileSaveFailure(ConfigurationLoadFailure::OutcomeUnknown)
                | Self::HostRecipeSaveFailure(ConfigurationLoadFailure::OutcomeUnknown)
                | Self::ProfileDeleteFailure(ConfigurationLoadFailure::OutcomeUnknown)
                | Self::ProfilesReorderFailure(ConfigurationLoadFailure::OutcomeUnknown)
        )
    }

    pub(crate) const fn save_reconciliation_required(self) -> bool {
        self.save_outcome_unknown()
            || matches!(
                self,
                Self::WorkspaceSaveFailure(ConfigurationLoadFailure::ReconciliationRequired)
                    | Self::SessionNamesSaveFailure(
                        ConfigurationLoadFailure::ReconciliationRequired
                    )
                    | Self::PasswordCredentialSaveFailure(
                        ConfigurationLoadFailure::ReconciliationRequired
                    )
                    | Self::InterfaceSettingsSaveFailure(
                        ConfigurationLoadFailure::ReconciliationRequired
                    )
                    | Self::KnownHostTrustSaveFailure(
                        ConfigurationLoadFailure::ReconciliationRequired
                    )
                    | Self::ProfileSaveFailure(ConfigurationLoadFailure::ReconciliationRequired)
                    | Self::HostRecipeSaveFailure(ConfigurationLoadFailure::ReconciliationRequired)
                    | Self::ProfileDeleteFailure(ConfigurationLoadFailure::ReconciliationRequired)
                    | Self::ProfilesReorderFailure(
                        ConfigurationLoadFailure::ReconciliationRequired
                    )
            )
    }

    pub(crate) const fn save_was_busy(self) -> bool {
        matches!(
            self,
            Self::WorkspaceSaveFailure(ConfigurationLoadFailure::SaveBusy)
                | Self::SessionNamesSaveFailure(ConfigurationLoadFailure::SaveBusy)
                | Self::PasswordCredentialSaveFailure(ConfigurationLoadFailure::SaveBusy)
                | Self::InterfaceSettingsSaveFailure(ConfigurationLoadFailure::SaveBusy)
                | Self::KnownHostTrustSaveFailure(ConfigurationLoadFailure::SaveBusy)
                | Self::ProfileSaveFailure(ConfigurationLoadFailure::SaveBusy)
                | Self::HostRecipeSaveFailure(ConfigurationLoadFailure::SaveBusy)
                | Self::ProfileDeleteFailure(ConfigurationLoadFailure::SaveBusy)
                | Self::ProfilesReorderFailure(ConfigurationLoadFailure::SaveBusy)
        )
    }

    /// True for exactly the variants that mean an automatic-save call
    /// actually wrote the replacement configuration to disk, i.e. the
    /// in-memory `AppState` may now safely commit to that replacement too.
    ///
    /// This centralizes the "commit only on success" invariant that every
    /// `FesTermApp::save_*`/`persist_*`/`reorder_profiles`/`delete_profile`
    /// caller previously re-derived by hand with its own
    /// `matches!(status, ConfigurationStartupStatus::WhicheverSaved)` check
    /// (#53 "workspace/configuration orchestration" seam) - a single
    /// shared source of truth instead of one manually kept in sync per call
    /// site.
    pub(crate) const fn was_saved(self) -> bool {
        matches!(
            self,
            Self::WorkspaceSaved
                | Self::SessionNamesSaved
                | Self::PasswordCredentialSaved
                | Self::InterfaceSettingsSaved
                | Self::KnownHostTrustSaved
                | Self::ProfileSaved
                | Self::HostRecipeSaved
                | Self::ProfileDeleted
                | Self::ProfilesReordered
        )
    }

    /// Human-readable diagnostic text for each startup/save status. No
    /// longer surfaced in the Settings UI (the "Configuration" card was
    /// removed there), but retained as the basis for regression tests that
    /// verify these messages never leak file paths or configuration
    /// content.
    #[allow(dead_code)]
    pub(crate) const fn settings_message(self) -> &'static str {
        if self.save_outcome_unknown() {
            return UNKNOWN_SAVE_MESSAGE;
        }
        if self.save_reconciliation_required() {
            return BLOCKED_SAVE_MESSAGE;
        }
        if self.save_was_busy() {
            return SAVE_BUSY_MESSAGE;
        }
        match self {
            Self::Loaded => {
                "Configuration was loaded at startup. Changes made in fesTerm save automatically; edit the file directly and restart fesTerm to pick up external changes."
            }
            Self::Missing => {
                "No configuration file was found at startup. fesTerm is using its defaults and will not create one automatically."
            }
            Self::InitialFailure(ConfigurationLoadFailure::Invalid) => {
                "Configuration was ignored at startup because it is invalid. Fix it, then restart fesTerm."
            }
            Self::InitialFailure(ConfigurationLoadFailure::Unreadable) => {
                "Configuration could not be read at startup. Check that it is readable, then restart fesTerm."
            }
            Self::InitialFailure(ConfigurationLoadFailure::OverrideUnavailable) => {
                "FESTERM_CONFIG_PATH could not be used. Set it to a non-empty Unicode file path, then restart fesTerm."
            }
            Self::InitialFailure(ConfigurationLoadFailure::NativeLocationUnavailable) => {
                "The native configuration location is unavailable. Set FESTERM_CONFIG_PATH to a Unicode file path, then restart fesTerm."
            }
            Self::InitialFailure(ConfigurationLoadFailure::OutcomeUnknown) => UNKNOWN_SAVE_MESSAGE,
            Self::InitialFailure(ConfigurationLoadFailure::ReconciliationRequired) => BLOCKED_SAVE_MESSAGE,
            Self::InitialFailure(ConfigurationLoadFailure::SaveBusy) => SAVE_BUSY_MESSAGE,
            Self::WorkspaceSaved => {
                "Workspace metadata was saved. Only restorable tab order, focus, configured profile references, and explicit display aliases were written."
            }
            Self::WorkspaceSaveFailure(ConfigurationLoadFailure::Invalid) => {
                "Workspace metadata was not saved because the configuration is invalid. The active configuration remains unchanged."
            }
            Self::WorkspaceSaveFailure(ConfigurationLoadFailure::Unreadable) => {
                "Workspace metadata could not be saved. The active configuration remains unchanged; check access and try again."
            }
            Self::WorkspaceSaveFailure(ConfigurationLoadFailure::OverrideUnavailable) => {
                "Workspace metadata was not saved because FESTERM_CONFIG_PATH is unavailable. The active configuration remains unchanged."
            }
            Self::WorkspaceSaveFailure(ConfigurationLoadFailure::NativeLocationUnavailable) => {
                "Workspace metadata was not saved because its location is unavailable. The active configuration remains unchanged."
            }
            Self::WorkspaceSaveFailure(ConfigurationLoadFailure::OutcomeUnknown) => UNKNOWN_SAVE_MESSAGE,
            Self::WorkspaceSaveFailure(ConfigurationLoadFailure::ReconciliationRequired) => BLOCKED_SAVE_MESSAGE,
            Self::WorkspaceSaveFailure(ConfigurationLoadFailure::SaveBusy) => SAVE_BUSY_MESSAGE,
            Self::SessionNamesSaved => "Session display names were saved without changing profiles or provider attachment identities.",
            Self::SessionNamesSaveFailure(_) => SESSION_NAMES_PRE_WRITE_FAILURE_MESSAGE,
            Self::PasswordCredentialSaved => {
                "The saved profile credential reference was updated. The credential remains only in native secure storage."
            }
            Self::PasswordCredentialSaveFailure(ConfigurationLoadFailure::Invalid) => {
                "The saved profile credential was not linked because the configuration is invalid. The prior credential remains active."
            }
            Self::PasswordCredentialSaveFailure(_) => {
                "The saved profile credential was not linked because configuration could not be written. The prior credential remains active."
            }
            Self::InterfaceSettingsSaved => {
                "Interface settings were saved and will be restored the next time fesTerm starts."
            }
            Self::InterfaceSettingsSaveFailure(_) => {
                "Interface settings apply now but could not be saved, so they will not persist across restart."
            }
            Self::KnownHostTrustSaved => {
                "This host key was saved as trusted and will be accepted automatically next time."
            }
            Self::KnownHostTrustSaveFailure(_) => {
                "This host key was accepted for now, but could not be saved as trusted; it will prompt again next time."
            }
            Self::ProfileSaved => {
                "The profile was saved and is available the next time you connect or launch."
            }
            Self::ProfileSaveFailure(ConfigurationLoadFailure::Invalid) => {
                "The profile was not saved because the configuration is invalid. Fix the definition and try again."
            }
            Self::ProfileSaveFailure(_) => {
                "The profile was not saved because configuration could not be written."
            }
            Self::HostRecipeSaved => {
                "Public recipe metadata was saved locally. No connection or remote session was changed."
            }
            Self::HostRecipeExported => {
                "Public recipe metadata was exported. No configuration or remote session was changed."
            }
            Self::HostRecipeCommandFailure(error) => error.message(),
            Self::HostRecipeSaveFailure(ConfigurationLoadFailure::OutcomeUnknown) => {
                "The public recipe save outcome is unknown. Reconcile configuration before saving again."
            }
            Self::HostRecipeSaveFailure(ConfigurationLoadFailure::ReconciliationRequired) => {
                "Public recipe saving is blocked until configuration is reconciled."
            }
            Self::HostRecipeSaveFailure(ConfigurationLoadFailure::SaveBusy) => {
                "Public recipe saving is busy. Try again after the current save finishes."
            }
            Self::HostRecipeSaveFailure(ConfigurationLoadFailure::Invalid) => {
                "Public recipe metadata was not saved because configuration is invalid."
            }
            Self::HostRecipeSaveFailure(_) => {
                "Public recipe metadata could not be saved. No in-memory change was published."
            }
            Self::ProfileDeleted => "The profile was deleted.",
            Self::ProfileDeleteFailure(_) => {
                "The profile was not deleted because configuration could not be written."
            }
            Self::ProfilesReordered => "Profile order was saved.",
            Self::ProfilesReorderFailure(_) => {
                "Profile order was not saved because configuration could not be written. The previous order remains active."
            }
        }
    }
}

/// Configuration selected during process startup.
///
/// The selected location stays private to [`ConfigurationReloader`]; it is
/// never displayed, logged, or placed in application state.
pub(crate) struct StartupConfiguration {
    configuration: Configuration,
    status: ConfigurationStartupStatus,
    reloader: ConfigurationReloader,
}

impl StartupConfiguration {
    /// The geometry the primary window was last saved at, when the user has
    /// opted into workspace restore and the platform reported it.
    ///
    /// Read before the window exists, so the first window opens at its saved
    /// size instead of opening at the default and resizing afterwards.
    pub(crate) fn restored_window_geometry(
        &self,
    ) -> Option<festerm_config::WorkspaceWindowGeometry> {
        if !self.configuration.interface_settings().restore_workspace() {
            return None;
        }
        self.configuration
            .workspace()
            .and_then(|workspace| workspace.geometry().copied())
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        Configuration,
        ConfigurationStartupStatus,
        ConfigurationReloader,
    ) {
        (self.configuration, self.status, self.reloader)
    }
}

#[derive(Clone)]
struct SelectedConfigurationPath {
    path: PathBuf,
    /// Present only for the native default source. An explicit override never
    /// receives directory-creation behavior.
    native_directory: Option<PathBuf>,
}

/// Private retained source for startup loading and fesTerm's own automatic
/// writes (workspace, interface settings, profiles, host-key trust).
///
/// Its path is intentionally inaccessible outside this module. It performs no
/// watching, polling, or logging of external edits; writes occur only when
/// fesTerm itself changes something worth persisting, via
/// [`Self::save_workspace`] and its sibling `save_*` methods.
///
/// Clones share a save gate. An uncertain save latches it until process
/// restart; a contending save fails without waiting or touching the file.
#[derive(Clone)]
pub(crate) struct ConfigurationReloader {
    selected_path: Result<SelectedConfigurationPath, ConfigurationLoadFailure>,
    automation_preferences: Option<crate::automation::preferences::Shared>,
    recording_preference: Arc<AtomicU8>,
    #[cfg(test)]
    next_save_failure: std::rc::Rc<std::cell::Cell<Option<TestSaveFailure>>>,
    save_gate: Arc<Mutex<bool>>,
    secret_cleanup_failure: Arc<std::sync::atomic::AtomicU16>,
    #[cfg(test)]
    injected_save_failure: Option<ConfigurationLoadFailure>,
    #[cfg(test)]
    panic_after_installation: bool,
}

#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum TestSaveFailure {
    BeforeWrite,
    AfterWrite,
}

#[cfg(test)]
pub(crate) struct SessionNameConfigurationFixture {
    pub(crate) path: PathBuf,
}

#[cfg(test)]
impl SessionNameConfigurationFixture {
    pub(crate) fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            path: std::env::current_dir().unwrap().join(format!(
                ".festerm-session-name-fixture-{}-{id}.toml",
                std::process::id(),
            )),
        }
    }
}

#[cfg(test)]
impl Drop for SessionNameConfigurationFixture {
    fn drop(&mut self) {
        let cleanup = match fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.is_dir() => fs::remove_dir(&self.path),
            Ok(_) => fs::remove_file(&self.path),
            Err(error) => Err(error),
        };
        if let Err(error) = cleanup {
            if error.kind() == std::io::ErrorKind::NotFound {
                return;
            }
            if std::thread::panicking() {
                eprintln!("Session name fixture cleanup failed: {:?}", error.kind());
            } else {
                panic!("Session name fixture cleanup failed: {:?}", error.kind());
            }
        }
    }
}

impl ConfigurationReloader {
    #[cfg(test)]
    fn from_selection(selected_path: Result<PathBuf, ConfigurationLoadFailure>) -> Self {
        Self::from_source_selection(selected_path.map(|path| SelectedConfigurationPath {
            path,
            native_directory: None,
        }))
    }

    #[cfg(test)]
    pub(crate) fn from_path_for_test(path: PathBuf) -> Self {
        Self::from_selection(Ok(path))
    }

    #[cfg(test)]
    pub(crate) fn with_save_failure_for_test(mut self, failure: ConfigurationLoadFailure) -> Self {
        self.injected_save_failure = Some(failure);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_panic_after_installation_for_test(mut self) -> Self {
        self.panic_after_installation = true;
        self
    }

    #[cfg(test)]
    pub(crate) fn hold_save_gate_for_test(&self) -> std::sync::MutexGuard<'_, bool> {
        self.save_gate.lock().unwrap()
    }

    fn from_source_selection(
        selected_path: Result<SelectedConfigurationPath, ConfigurationLoadFailure>,
    ) -> Self {
        Self {
            selected_path,
            automation_preferences: None,
            recording_preference: Arc::new(AtomicU8::new(0)),
            #[cfg(test)]
            next_save_failure: Default::default(),
            save_gate: Arc::new(Mutex::new(false)),
            secret_cleanup_failure: Arc::default(),
            #[cfg(test)]
            injected_save_failure: None,
            #[cfg(test)]
            panic_after_installation: false,
        }
    }

    pub(crate) fn unavailable() -> Self {
        Self::from_source_selection(Err(ConfigurationLoadFailure::NativeLocationUnavailable))
    }

    pub(crate) fn automation_preferences(&self) -> Option<crate::automation::preferences::Shared> {
        self.automation_preferences.clone()
    }

    pub(crate) fn set_automation_preferences(
        &mut self,
        preferences: crate::automation::preferences::Shared,
    ) {
        self.automation_preferences = Some(preferences);
    }

    pub(crate) fn automation_configuration(
        &self,
        configuration: &Configuration,
    ) -> Result<Configuration, festerm_config::ConfigError> {
        match &self.automation_preferences {
            Some(preferences) => preferences.apply(configuration),
            None => Ok(configuration.clone()),
        }
    }

    pub(crate) fn initialize_recording_preference(&self, enabled: bool) {
        let value = if enabled { 2 } else { 1 };
        let _ = self.recording_preference.compare_exchange(
            0,
            value,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub(crate) fn disable_recording_preference(&self) {
        self.recording_preference.store(1, Ordering::Release);
    }

    pub(crate) fn recording_configuration(
        &self,
        configuration: &Configuration,
    ) -> Result<Configuration, festerm_config::ConfigError> {
        match self.recording_preference.load(Ordering::Acquire) {
            0 => Ok(configuration.clone()),
            1 => configuration.with_session_recording_enabled(false),
            2 => configuration.with_session_recording_enabled(true),
            _ => unreachable!("invalid recording preference"),
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_next_save_for_test(&self, failure: TestSaveFailure) {
        self.next_save_failure.set(Some(failure));
    }

    pub(crate) fn secret_cleanup_failure(&self) -> Arc<std::sync::atomic::AtomicU16> {
        Arc::clone(&self.secret_cleanup_failure)
    }

    pub(crate) fn save_gate_state(&self) -> SaveGateState {
        match self.save_gate.try_lock() {
            Ok(unresolved) if *unresolved => SaveGateState::ReconciliationRequired,
            Ok(_) => SaveGateState::Healthy,
            Err(TryLockError::Poisoned(_)) => SaveGateState::ReconciliationRequired,
            Err(TryLockError::WouldBlock) => SaveGateState::InProgress,
        }
    }

    pub(crate) fn save_gate_warning(&self) -> Option<&'static str> {
        match self.save_gate_state() {
            SaveGateState::Healthy => None,
            SaveGateState::InProgress => Some(SAVE_IN_PROGRESS_MESSAGE),
            SaveGateState::ReconciliationRequired => Some(RECONCILE_SAVE_MESSAGE),
        }
    }

    #[cfg(feature = "experimental-dev-tunnels")]
    pub(crate) fn tunnel_import_directory(&self) -> Result<PathBuf, ConfigurationLoadFailure> {
        let selected = self.selected_path.as_ref().map_err(|failure| *failure)?;
        if let Some(directory) = &selected.native_directory {
            if !directory.exists() {
                create_native_configuration_directory(directory)
                    .map_err(|_| ConfigurationLoadFailure::Unreadable)?;
            }
        }
        let parent = selected
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let directory = parent.join("tunnel-profiles");
        std::path::absolute(directory).map_err(|_| ConfigurationLoadFailure::Unreadable)
    }

    fn initial_load(&self) -> (Configuration, ConfigurationStartupStatus) {
        let path = match &self.selected_path {
            Ok(selected) => &selected.path,
            Err(failure) => {
                return (
                    Configuration::empty(),
                    ConfigurationStartupStatus::InitialFailure(*failure),
                );
            }
        };
        match Configuration::load_from_path(path) {
            Ok(configuration) => (configuration, ConfigurationStartupStatus::Loaded),
            Err(error) => match error.kind() {
                ConfigurationFileErrorKind::MissingFile => {
                    (Configuration::empty(), ConfigurationStartupStatus::Missing)
                }
                kind => (
                    Configuration::empty(),
                    ConfigurationStartupStatus::InitialFailure(failure_from_file_error(kind)),
                ),
            },
        }
    }

    /// Saves an already validated complete replacement automatically whenever
    /// the open tab list, its order, or the active tab changes (see
    /// `AppState::take_workspace_dirty`), with no explicit user action
    /// required. For the native source alone, a missing final config
    /// directory may be created with normal user/default permissions; no
    /// override directory and no configuration file is created otherwise.
    pub(crate) fn save_workspace(
        &self,
        configuration: &Configuration,
    ) -> (ConfigurationStartupStatus, Option<Configuration>) {
        match self.save_configuration(configuration) {
            Ok(saved) => (ConfigurationStartupStatus::WorkspaceSaved, Some(saved)),
            Err(failure) => (
                ConfigurationStartupStatus::WorkspaceSaveFailure(failure),
                None,
            ),
        }
    }

    pub(crate) fn save_session_names(
        &self,
        configuration: &Configuration,
    ) -> (ConfigurationStartupStatus, Option<Configuration>) {
        match self.save_configuration(configuration) {
            Ok(saved) => (ConfigurationStartupStatus::SessionNamesSaved, Some(saved)),
            Err(failure) => (
                ConfigurationStartupStatus::SessionNamesSaveFailure(failure),
                None,
            ),
        }
    }

    /// Saves an already validated complete replacement immediately after a
    /// Settings toggle or reset, matching the "applies immediately" design of
    /// the interface-preference toggles it supports (the same automatic,
    /// no-explicit-action pattern [`Self::save_workspace`] uses for tab-list
    /// changes).
    pub(crate) fn save_interface_settings(
        &self,
        configuration: &Configuration,
    ) -> (ConfigurationStartupStatus, Option<Configuration>) {
        match self.save_configuration(configuration) {
            Ok(saved) => (
                ConfigurationStartupStatus::InterfaceSettingsSaved,
                Some(saved),
            ),
            Err(failure) => (
                ConfigurationStartupStatus::InterfaceSettingsSaveFailure(failure),
                None,
            ),
        }
    }

    pub(crate) fn save_recording_preference(
        &self,
        configuration: &Configuration,
        enabled: bool,
    ) -> (ConfigurationStartupStatus, Option<Configuration>) {
        match self.save_configuration_with_recording_preference(configuration, Some(enabled)) {
            Ok(saved) => (
                ConfigurationStartupStatus::InterfaceSettingsSaved,
                Some(saved),
            ),
            Err(failure) => (
                ConfigurationStartupStatus::InterfaceSettingsSaveFailure(failure),
                None,
            ),
        }
    }

    /// Saves an already validated complete replacement immediately after the
    /// user accepts and remembers a host key from the host-key prompt (ADR
    /// 0020), the same automatic, no-explicit-action trigger
    /// [`Self::save_interface_settings`] and [`Self::save_workspace`] use.
    pub(crate) fn save_known_host_trust(
        &self,
        configuration: &Configuration,
    ) -> (ConfigurationStartupStatus, Option<Configuration>) {
        match self.save_configuration(configuration) {
            Ok(saved) => (ConfigurationStartupStatus::KnownHostTrustSaved, Some(saved)),
            Err(failure) => (
                ConfigurationStartupStatus::KnownHostTrustSaveFailure(failure),
                None,
            ),
        }
    }

    /// Saves an already validated complete replacement immediately after an
    /// explicit Save in the Profiles editor, whether creating a new profile
    /// or editing an existing one (both are the same upsert-by-identifier
    /// write, `Configuration::with_profile`).
    pub(crate) fn save_profile(
        &self,
        configuration: &Configuration,
    ) -> (ConfigurationStartupStatus, Option<Configuration>) {
        match self.save_configuration(configuration) {
            Ok(saved) => (ConfigurationStartupStatus::ProfileSaved, Some(saved)),
            Err(failure) => (
                ConfigurationStartupStatus::ProfileSaveFailure(failure),
                None,
            ),
        }
    }

    pub(crate) fn save_host_recipes(
        &self,
        configuration: &Configuration,
    ) -> (ConfigurationStartupStatus, Option<Configuration>) {
        match self.save_configuration(configuration) {
            Ok(saved) => (ConfigurationStartupStatus::HostRecipeSaved, Some(saved)),
            Err(failure) => (
                ConfigurationStartupStatus::HostRecipeSaveFailure(failure),
                None,
            ),
        }
    }

    /// Saves an already validated complete replacement immediately after an
    /// explicit, confirmed profile deletion (`Configuration::without_profile`).
    pub(crate) fn delete_profile(
        &self,
        configuration: &Configuration,
    ) -> (ConfigurationStartupStatus, Option<Configuration>) {
        match self.save_configuration(configuration) {
            Ok(saved) => (ConfigurationStartupStatus::ProfileDeleted, Some(saved)),
            Err(failure) => (
                ConfigurationStartupStatus::ProfileDeleteFailure(failure),
                None,
            ),
        }
    }

    /// Saves an already validated complete replacement immediately after a
    /// drag-to-reorder gesture on the Profiles surface
    /// (`Configuration::with_reordered_profiles`).
    pub(crate) fn reorder_profiles(
        &self,
        configuration: &Configuration,
    ) -> (ConfigurationStartupStatus, Option<Configuration>) {
        match self.save_configuration(configuration) {
            Ok(saved) => (ConfigurationStartupStatus::ProfilesReordered, Some(saved)),
            Err(failure) => (
                ConfigurationStartupStatus::ProfilesReorderFailure(failure),
                None,
            ),
        }
    }

    /// Persists an already validated full replacement through the same private
    /// source selected at startup. This is used by the password-reference
    /// workflow after the native store has accepted a new secret.
    pub(crate) fn save_configuration(
        &self,
        configuration: &Configuration,
    ) -> Result<Configuration, ConfigurationLoadFailure> {
        self.save_configuration_with_recording_preference(configuration, None)
    }

    fn save_configuration_with_recording_preference(
        &self,
        configuration: &Configuration,
        recording_preference: Option<bool>,
    ) -> Result<Configuration, ConfigurationLoadFailure> {
        let mut unresolved = match self.save_gate.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return Err(ConfigurationLoadFailure::SaveBusy),
            Err(TryLockError::Poisoned(_)) => {
                return Err(ConfigurationLoadFailure::ReconciliationRequired);
            }
        };
        if *unresolved {
            return Err(ConfigurationLoadFailure::ReconciliationRequired);
        }
        #[cfg(test)]
        if self
            .injected_save_failure
            .is_some_and(|failure| failure != ConfigurationLoadFailure::OutcomeUnknown)
        {
            return Err(self.injected_save_failure.expect("checked above"));
        }
        #[cfg(test)]
        let failure = self.next_save_failure.take();
        #[cfg(test)]
        if matches!(failure, Some(TestSaveFailure::BeforeWrite)) {
            return Err(ConfigurationLoadFailure::Unreadable);
        }
        let configuration = self
            .automation_configuration(configuration)
            .map_err(|_| ConfigurationLoadFailure::Invalid)?;
        let configuration = match recording_preference {
            Some(enabled) => configuration.with_session_recording_enabled(enabled),
            None => self.recording_configuration(&configuration),
        }
        .map_err(|_| ConfigurationLoadFailure::Invalid)?;
        let selected = match &self.selected_path {
            Ok(selected) => selected,
            Err(failure) => {
                return Err(*failure);
            }
        };
        if let Some(directory) = &selected.native_directory {
            if !directory.exists() && create_native_configuration_directory(directory).is_err() {
                return Err(ConfigurationLoadFailure::Unreadable);
            }
        }
        match configuration.save_to_path(&selected.path) {
            Ok(()) => {
                #[cfg(test)]
                if self.panic_after_installation {
                    panic!("injected panic after configuration installation");
                }
                #[cfg(test)]
                if self.injected_save_failure == Some(ConfigurationLoadFailure::OutcomeUnknown) {
                    *unresolved = true;
                    return Err(ConfigurationLoadFailure::OutcomeUnknown);
                }
                #[cfg(test)]
                if matches!(failure, Some(TestSaveFailure::AfterWrite)) {
                    *unresolved = true;
                    return Err(ConfigurationLoadFailure::OutcomeUnknown);
                }
                if let Some(enabled) = recording_preference {
                    self.recording_preference
                        .store(if enabled { 2 } else { 1 }, Ordering::Release);
                }
                Ok(configuration)
            }
            Err(_) => {
                *unresolved = true;
                Err(ConfigurationLoadFailure::OutcomeUnknown)
            }
        }
    }
}

#[cfg(unix)]
fn create_native_configuration_directory(directory: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    // `recursive` also creates any missing parent components (e.g. the
    // organization directory), matching the Windows/other-platform path
    // below; a plain non-recursive `create` would fail here on a fresh
    // install where no ancestor of the config directory exists yet.
    builder.recursive(true);
    match builder.create(directory) {
        Ok(()) => Ok(()),
        Err(_error) if directory.is_dir() => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(not(unix))]
fn create_native_configuration_directory(directory: &std::path::Path) -> std::io::Result<()> {
    // `create_dir_all` (not `create_dir`) because on a fresh install no
    // ancestor of the config directory exists yet either (e.g. on Windows
    // this is `%APPDATA%\fes\fesTerm\config`, and neither `fes` nor
    // `fes\fesTerm` exist beforehand) — a single-level `create_dir` fails
    // with a not-found error whenever any parent is missing, which
    // previously made every automatic save (including the very first
    // profile) fail silently.
    match fs::create_dir_all(directory) {
        Ok(()) => Ok(()),
        Err(_error) if directory.is_dir() => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn load() -> StartupConfiguration {
    static SAVE_GATE: OnceLock<Arc<Mutex<bool>>> = OnceLock::new();
    let mut reloader = ConfigurationReloader::from_source_selection(select_configuration_source(
        std::env::var_os(CONFIG_PATH_ENV),
        native_configuration_directory(),
    ));
    reloader.save_gate = Arc::clone(SAVE_GATE.get_or_init(|| Arc::new(Mutex::new(false))));
    let (configuration, status) = reloader.initial_load();
    StartupConfiguration {
        configuration,
        status,
        reloader,
    }
}

fn native_configuration_directory() -> Option<PathBuf> {
    ProjectDirs::from("com", "fes", "fesTerm")
        .map(|directories| directories.config_dir().to_path_buf())
}

#[cfg(test)]
fn select_configuration_path(
    override_value: Option<OsString>,
    native_config_directory: Option<PathBuf>,
) -> Result<PathBuf, ConfigurationLoadFailure> {
    select_configuration_source(override_value, native_config_directory)
        .map(|selected| selected.path)
}

fn select_configuration_source(
    override_value: Option<OsString>,
    native_config_directory: Option<PathBuf>,
) -> Result<SelectedConfigurationPath, ConfigurationLoadFailure> {
    if let Some(override_value) = override_value {
        let override_value = override_value
            .into_string()
            .map_err(|_| ConfigurationLoadFailure::OverrideUnavailable)?;
        if override_value.is_empty() {
            return Err(ConfigurationLoadFailure::OverrideUnavailable);
        }
        return Ok(SelectedConfigurationPath {
            path: PathBuf::from(override_value),
            native_directory: None,
        });
    }

    native_config_directory
        .map(|directory| SelectedConfigurationPath {
            path: directory.join(CONFIG_FILE_NAME),
            native_directory: Some(directory),
        })
        .ok_or(ConfigurationLoadFailure::NativeLocationUnavailable)
}

#[cfg(test)]
fn load_from_path(path: &std::path::Path) -> StartupConfiguration {
    let reloader = ConfigurationReloader::from_selection(Ok(path.to_path_buf()));
    let (configuration, status) = reloader.initial_load();
    StartupConfiguration {
        configuration,
        status,
        reloader,
    }
}

fn failure_from_file_error(kind: ConfigurationFileErrorKind) -> ConfigurationLoadFailure {
    match kind {
        ConfigurationFileErrorKind::Parse => ConfigurationLoadFailure::Invalid,
        ConfigurationFileErrorKind::MissingFile => {
            unreachable!("missing configuration is a non-failure reload outcome")
        }
        ConfigurationFileErrorKind::Read
        | ConfigurationFileErrorKind::InvalidTargetPath
        | ConfigurationFileErrorKind::CreateTemporary
        | ConfigurationFileErrorKind::WriteTemporary
        | ConfigurationFileErrorKind::SyncTemporary
        | ConfigurationFileErrorKind::Serialization
        | ConfigurationFileErrorKind::Replace
        | ConfigurationFileErrorKind::RestorePrevious
        | ConfigurationFileErrorKind::CleanupPrevious
        | ConfigurationFileErrorKind::SyncParentDirectory => ConfigurationLoadFailure::Unreadable,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    #[test]
    fn new_window_save_gate_probe_is_nonblocking_and_truthful() {
        let reloader = ConfigurationReloader::unavailable();
        assert_eq!(reloader.save_gate_warning(), None);
        let _inflight = reloader.save_gate.lock().unwrap();
        assert_eq!(reloader.save_gate_warning(), Some(SAVE_IN_PROGRESS_MESSAGE));
    }

    #[test]
    fn unresolved_save_blocks_sibling_and_inflight_save_without_writing() {
        let directory = TestDirectory::new();
        let path = directory.file("configuration.toml");
        let original = Configuration::empty();
        let installed = Configuration::new(vec![festerm_config::Profile::local(
            "installed",
            "sh",
            vec![],
            None,
        )
        .unwrap()])
        .unwrap();
        let later = Configuration::new(vec![festerm_config::Profile::local(
            "later",
            "sh",
            vec![],
            None,
        )
        .unwrap()])
        .unwrap();
        original.save_to_path(&path).unwrap();
        let sibling = ConfigurationReloader::from_path_for_test(path.clone());
        let first = sibling
            .clone()
            .with_save_failure_for_test(ConfigurationLoadFailure::OutcomeUnknown);
        {
            let _inflight = sibling.save_gate.lock().unwrap();
            assert_eq!(
                sibling.save_configuration(&later),
                Err(ConfigurationLoadFailure::SaveBusy)
            );
            assert_eq!(Configuration::load_from_path(&path).unwrap(), original);
        }
        assert_eq!(
            first.save_configuration(&installed),
            Err(ConfigurationLoadFailure::OutcomeUnknown)
        );
        assert_eq!(Configuration::load_from_path(&path).unwrap(), installed);
        assert_eq!(
            sibling.save_configuration(&later),
            Err(ConfigurationLoadFailure::ReconciliationRequired)
        );
        assert_eq!(Configuration::load_from_path(&path).unwrap(), installed);
        assert_eq!(
            first.save_configuration(&later),
            Err(ConfigurationLoadFailure::ReconciliationRequired)
        );
        assert_eq!(Configuration::load_from_path(&path).unwrap(), installed);
    }

    #[test]
    fn recording_opt_out_survives_stale_saves_and_failed_writes() {
        let directory = TestDirectory::new();
        let path = directory.file("configuration.toml");
        let enabled = Configuration::empty()
            .with_session_recording_enabled(true)
            .unwrap();
        enabled.save_to_path(&path).unwrap();
        let first = ConfigurationReloader::from_path_for_test(path.clone());
        let sibling = first.clone();
        first.initialize_recording_preference(true);
        first.disable_recording_preference();
        first.fail_next_save_for_test(TestSaveFailure::BeforeWrite);
        assert!(!first
            .save_recording_preference(&enabled, false)
            .0
            .was_saved());
        assert!(Configuration::load_from_path(&path)
            .unwrap()
            .session_recording_enabled());

        let saved = sibling.save_configuration(&enabled).unwrap();
        assert!(!saved.session_recording_enabled());
        assert!(!Configuration::load_from_path(&path)
            .unwrap()
            .session_recording_enabled());
        assert!(!first
            .recording_configuration(&enabled)
            .unwrap()
            .session_recording_enabled());

        first.fail_next_save_for_test(TestSaveFailure::AfterWrite);
        assert!(first
            .save_recording_preference(&enabled, false)
            .0
            .save_outcome_unknown());
        assert_eq!(
            sibling.save_configuration(&enabled),
            Err(ConfigurationLoadFailure::ReconciliationRequired)
        );
        assert!(!sibling
            .recording_configuration(&enabled)
            .unwrap()
            .session_recording_enabled());
    }

    #[test]
    fn any_filesystem_save_error_latches_even_before_replacement() {
        let directory = TestDirectory::new();
        let path = directory.file("target-directory");
        fs::create_dir(&path).unwrap();
        let first = ConfigurationReloader::from_path_for_test(path.clone());
        let sibling = first.clone();
        let failure = first
            .save_configuration(&Configuration::empty())
            .unwrap_err();
        assert_eq!(failure, ConfigurationLoadFailure::OutcomeUnknown);
        assert!(path.is_dir());
        assert_eq!(
            sibling.save_configuration(&Configuration::empty()),
            Err(ConfigurationLoadFailure::ReconciliationRequired)
        );
        let status = ConfigurationStartupStatus::PasswordCredentialSaveFailure(failure);
        assert!(status.save_outcome_unknown());
        assert!(!status.was_saved());
        assert!(status.settings_message().contains("reconcile"));
    }

    static NEXT_TEST_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::current_dir()
                .expect("test working directory is available")
                .join(format!(
                    ".festerm-configuration-startup-test-{}-{id}",
                    std::process::id()
                ));
            fs::create_dir(&path).expect("test directory can be created");
            Self(path)
        }

        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("test directory can be removed");
        }
    }

    #[test]
    fn session_alias_fixture_cleanup_preserves_original_panics_for_files_and_empty_directories() {
        for directory in [false, true] {
            for unwind in [false, true] {
                let fixture = SessionNameConfigurationFixture::new();
                let path = fixture.path.clone();
                if directory {
                    fs::create_dir(&path).unwrap();
                } else {
                    fs::write(&path, "owned fixture").unwrap();
                }
                let result = std::panic::catch_unwind(move || {
                    let _fixture = fixture;
                    if unwind {
                        panic!("controlled session name fixture failure");
                    }
                });
                if unwind {
                    assert_eq!(
                        result.unwrap_err().downcast_ref::<&str>().copied(),
                        Some("controlled session name fixture failure")
                    );
                } else {
                    result.unwrap();
                }
                assert!(!path.exists());
            }
        }
    }

    #[test]
    fn was_saved_is_true_for_every_successful_save_variant_and_false_otherwise() {
        // #53: a single source of truth for "did this status just commit a
        // write", replacing seven independently hand-written matches!(...)
        // checks previously scattered across app.rs's save_*/persist_*
        // methods.
        for saved in [
            ConfigurationStartupStatus::WorkspaceSaved,
            ConfigurationStartupStatus::SessionNamesSaved,
            ConfigurationStartupStatus::PasswordCredentialSaved,
            ConfigurationStartupStatus::InterfaceSettingsSaved,
            ConfigurationStartupStatus::KnownHostTrustSaved,
            ConfigurationStartupStatus::ProfileSaved,
            ConfigurationStartupStatus::HostRecipeSaved,
            ConfigurationStartupStatus::ProfileDeleted,
            ConfigurationStartupStatus::ProfilesReordered,
        ] {
            assert!(saved.was_saved(), "{saved:?} should count as saved");
        }

        for not_saved in [
            ConfigurationStartupStatus::Loaded,
            ConfigurationStartupStatus::Missing,
            ConfigurationStartupStatus::InitialFailure(ConfigurationLoadFailure::Invalid),
            ConfigurationStartupStatus::WorkspaceSaveFailure(ConfigurationLoadFailure::Invalid),
            ConfigurationStartupStatus::SessionNamesSaveFailure(ConfigurationLoadFailure::Invalid),
            ConfigurationStartupStatus::PasswordCredentialSaveFailure(
                ConfigurationLoadFailure::Invalid,
            ),
            ConfigurationStartupStatus::InterfaceSettingsSaveFailure(
                ConfigurationLoadFailure::Invalid,
            ),
            ConfigurationStartupStatus::KnownHostTrustSaveFailure(
                ConfigurationLoadFailure::Invalid,
            ),
            ConfigurationStartupStatus::ProfileSaveFailure(ConfigurationLoadFailure::Invalid),
            ConfigurationStartupStatus::HostRecipeExported,
            ConfigurationStartupStatus::HostRecipeCommandFailure(
                festerm_config::host_recipes::RecipeError::NotFound,
            ),
            ConfigurationStartupStatus::HostRecipeSaveFailure(ConfigurationLoadFailure::Invalid),
            ConfigurationStartupStatus::ProfileDeleteFailure(ConfigurationLoadFailure::Invalid),
            ConfigurationStartupStatus::ProfilesReorderFailure(ConfigurationLoadFailure::Invalid),
        ] {
            assert!(
                !not_saved.was_saved(),
                "{not_saved:?} should not count as saved"
            );
        }
    }

    #[test]
    fn missing_configuration_uses_empty_configuration() {
        let directory = TestDirectory::new();
        let startup = load_from_path(&directory.file("missing.toml"));
        let (configuration, status, _) = startup.into_parts();

        assert_eq!(status, ConfigurationStartupStatus::Missing);
        assert_eq!(configuration, Configuration::empty());
    }

    #[test]
    fn valid_configuration_is_loaded() {
        let directory = TestDirectory::new();
        let path = directory.file("config.toml");
        fs::write(
            &path,
            "schema_version = 1\n\n[[profiles]]\nkind = \"local\"\nid = \"shell\"\nexecutable = \"/bin/sh\"\n",
        )
        .expect("test configuration can be written");

        let startup = load_from_path(&path);
        let (configuration, status, _) = startup.into_parts();

        assert_eq!(status, ConfigurationStartupStatus::Loaded);
        assert_eq!(configuration.profiles().len(), 1);
    }

    #[test]
    fn invalid_configuration_is_ignored_with_a_content_free_diagnostic() {
        let directory = TestDirectory::new();
        let path = directory.file("arbitrary-user-path.toml");
        let source_toml = "schema_version = [private source TOML]";
        fs::write(&path, source_toml).expect("test configuration can be written");

        let startup = load_from_path(&path);
        let (configuration, status, _) = startup.into_parts();
        let diagnostic = status.settings_message();

        assert_eq!(
            status,
            ConfigurationStartupStatus::InitialFailure(ConfigurationLoadFailure::Invalid)
        );
        assert_eq!(configuration, Configuration::empty());
        assert!(!diagnostic.contains(source_toml));
        assert!(!diagnostic.contains(path.to_string_lossy().as_ref()));
    }

    #[test]
    fn unreadable_configuration_is_ignored_with_a_content_free_diagnostic() {
        let directory = TestDirectory::new();
        let startup = load_from_path(directory.path());
        let (configuration, status, _) = startup.into_parts();
        let diagnostic = status.settings_message();

        assert_eq!(
            status,
            ConfigurationStartupStatus::InitialFailure(ConfigurationLoadFailure::Unreadable)
        );
        assert_eq!(configuration, Configuration::empty());
        assert!(!diagnostic.contains(directory.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn unicode_override_takes_precedence_over_native_location() {
        let native_directory = PathBuf::from("native-config");
        let selected = select_configuration_path(
            Some(OsString::from("support-config.toml")),
            Some(native_directory),
        )
        .expect("Unicode override is selected");

        assert_eq!(selected, PathBuf::from("support-config.toml"));
    }

    #[test]
    fn empty_override_is_unavailable() {
        assert_eq!(
            select_configuration_path(Some(OsString::new()), Some(PathBuf::from("native-config"))),
            Err(ConfigurationLoadFailure::OverrideUnavailable)
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_override_is_unavailable() {
        use std::os::unix::ffi::OsStringExt;

        assert_eq!(
            select_configuration_path(
                Some(OsString::from_vec(vec![b'/', 0xFF])),
                Some(PathBuf::from("native-config"))
            ),
            Err(ConfigurationLoadFailure::OverrideUnavailable)
        );
    }

    #[test]
    fn explicit_workspace_save_creates_only_the_missing_native_directory_and_loads() {
        let directory = TestDirectory::new();
        let native_directory = directory.file("native");
        let source = select_configuration_source(None, Some(native_directory.clone())).unwrap();
        let reloader = ConfigurationReloader::from_source_selection(Ok(source));
        let workspace = festerm_config::WorkspaceConfiguration::new(
            vec![festerm_config::WorkspaceTab::launcher("tab-1").unwrap()],
            Some("tab-1".to_owned()),
        )
        .unwrap();
        let configuration = Configuration::empty().with_workspace(workspace).unwrap();

        assert!(!native_directory.exists());
        assert_eq!(
            reloader.save_workspace(&configuration),
            (
                ConfigurationStartupStatus::WorkspaceSaved,
                Some(configuration.clone())
            )
        );
        assert!(native_directory.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                fs::metadata(&native_directory)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }
        let loaded =
            Configuration::load_from_path(native_directory.join(CONFIG_FILE_NAME)).unwrap();
        assert_eq!(loaded, configuration);
    }

    #[test]
    fn explicit_profile_save_creates_missing_ancestor_directories_and_loads() {
        // On a fresh install, `native_config_directory()`'s Windows path is
        // `%APPDATA%\fes\fesTerm\config`, where *no* ancestor exists yet
        // (not even the top-level organization directory), unlike the
        // single-missing-level case above. A non-recursive directory
        // creation call previously failed here, so saving even the very
        // first profile silently did nothing.
        let directory = TestDirectory::new();
        let native_directory = directory.path().join("org").join("app").join("config");
        let source = select_configuration_source(None, Some(native_directory.clone())).unwrap();
        let reloader = ConfigurationReloader::from_source_selection(Ok(source));
        let profile =
            festerm_config::Profile::local("dev-shell", "/bin/zsh", Vec::new(), None).unwrap();
        let configuration = Configuration::empty().with_profile(profile).unwrap();

        assert!(!native_directory.parent().unwrap().exists());
        assert_eq!(
            reloader.save_profile(&configuration),
            (
                ConfigurationStartupStatus::ProfileSaved,
                Some(configuration.clone())
            )
        );
        assert!(native_directory.is_dir());
        let loaded =
            Configuration::load_from_path(native_directory.join(CONFIG_FILE_NAME)).unwrap();
        assert_eq!(loaded, configuration);
    }

    #[test]
    fn failed_workspace_save_is_content_free() {
        let directory = TestDirectory::new();
        let source_path = directory.path().to_path_buf();
        let reloader = ConfigurationReloader::from_path_for_test(source_path.clone());
        let (status, persisted) = reloader.save_workspace(&Configuration::empty());
        assert!(persisted.is_none());
        let diagnostic = status.settings_message();

        assert!(matches!(
            status,
            ConfigurationStartupStatus::WorkspaceSaveFailure(
                ConfigurationLoadFailure::OutcomeUnknown
            )
        ));
        assert!(!diagnostic.contains(source_path.to_string_lossy().as_ref()));
        assert!(!diagnostic.contains("schema_version"));
    }
}
