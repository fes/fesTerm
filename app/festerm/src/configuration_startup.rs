use std::{ffi::OsString, fs, path::PathBuf};

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
}

/// Content-free outcome of selecting, loading, or explicitly reloading
/// configuration.
///
/// This deliberately retains neither the selected path nor source TOML. The
/// application can show it safely in Settings and diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConfigurationStartupStatus {
    Loaded,
    Missing,
    InitialFailure(ConfigurationLoadFailure),
    Reloaded,
    ReloadedMissing,
    ReloadFailure(ConfigurationLoadFailure),
    WorkspaceSaved,
    WorkspaceSaveFailure(ConfigurationLoadFailure),
}

impl ConfigurationStartupStatus {
    pub(crate) const fn settings_message(self) -> &'static str {
        match self {
            Self::Loaded => {
                "Configuration was loaded at startup. Reload configuration to apply later edits."
            }
            Self::Missing => {
                "No configuration file was found at startup. fesTerm is using its defaults and will not create one automatically."
            }
            Self::InitialFailure(ConfigurationLoadFailure::Invalid) => {
                "Configuration was ignored at startup because it is invalid. Fix it, then use Reload configuration."
            }
            Self::InitialFailure(ConfigurationLoadFailure::Unreadable) => {
                "Configuration could not be read at startup. Check that it is readable, then use Reload configuration."
            }
            Self::InitialFailure(ConfigurationLoadFailure::OverrideUnavailable) => {
                "FESTERM_CONFIG_PATH could not be used. Set it to a non-empty Unicode file path, then restart fesTerm."
            }
            Self::InitialFailure(ConfigurationLoadFailure::NativeLocationUnavailable) => {
                "The native configuration location is unavailable. Set FESTERM_CONFIG_PATH to a Unicode file path, then restart fesTerm."
            }
            Self::Reloaded => {
                "Configuration was reloaded. Future Launcher choices use it; existing sessions are unchanged."
            }
            Self::ReloadedMissing => {
                "Configuration file is missing. fesTerm is using its defaults; existing sessions are unchanged."
            }
            Self::ReloadFailure(ConfigurationLoadFailure::Invalid) => {
                "Configuration was not reloaded because it is invalid. The previous configuration remains active; fix it and try again."
            }
            Self::ReloadFailure(ConfigurationLoadFailure::Unreadable) => {
                "Configuration was not reloaded because it could not be read. The previous configuration remains active; check access and try again."
            }
            Self::ReloadFailure(ConfigurationLoadFailure::OverrideUnavailable) => {
                "Configuration was not reloaded because FESTERM_CONFIG_PATH is unavailable. The previous configuration remains active; set it to a non-empty Unicode path and restart fesTerm."
            }
            Self::ReloadFailure(ConfigurationLoadFailure::NativeLocationUnavailable) => {
                "Configuration was not reloaded because its location is unavailable. The previous configuration remains active; set FESTERM_CONFIG_PATH and restart fesTerm."
            }
            Self::WorkspaceSaved => {
                "Workspace metadata was saved. Only restorable tab order, focus, and configured profile references were written."
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
        }
    }

    pub(crate) const fn is_problem(self) -> bool {
        matches!(
            self,
            Self::InitialFailure(_) | Self::ReloadFailure(_) | Self::WorkspaceSaveFailure(_)
        )
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

struct SelectedConfigurationPath {
    path: PathBuf,
    /// Present only for the native default source. An explicit override never
    /// receives directory-creation behavior.
    native_directory: Option<PathBuf>,
}

/// Private retained source for an explicit, user-triggered reload or save.
///
/// Its path is intentionally inaccessible outside this module. It performs no
/// watching, polling, or logging; writes occur only through
/// [`Self::save_workspace`] after an explicit Settings action.
pub(crate) struct ConfigurationReloader {
    selected_path: Result<SelectedConfigurationPath, ConfigurationLoadFailure>,
}

impl ConfigurationReloader {
    #[cfg(test)]
    fn from_selection(selected_path: Result<PathBuf, ConfigurationLoadFailure>) -> Self {
        Self {
            selected_path: selected_path.map(|path| SelectedConfigurationPath {
                path,
                native_directory: None,
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_path_for_test(path: PathBuf) -> Self {
        Self::from_selection(Ok(path))
    }

    fn from_source_selection(
        selected_path: Result<SelectedConfigurationPath, ConfigurationLoadFailure>,
    ) -> Self {
        Self { selected_path }
    }

    pub(crate) fn unavailable() -> Self {
        Self::from_source_selection(Err(ConfigurationLoadFailure::NativeLocationUnavailable))
    }

    /// Loads one complete candidate from the already-selected location.
    ///
    /// `Some` is returned only for a valid replacement or a normal missing
    /// file (which deliberately replaces the configuration with defaults).
    /// All other outcomes retain the caller's active configuration.
    pub(crate) fn reload(&self) -> (Option<Configuration>, ConfigurationStartupStatus) {
        let path = match &self.selected_path {
            Ok(selected) => &selected.path,
            Err(failure) => {
                return (None, ConfigurationStartupStatus::ReloadFailure(*failure));
            }
        };
        match Configuration::load_from_path(path) {
            Ok(configuration) => (Some(configuration), ConfigurationStartupStatus::Reloaded),
            Err(error) => match error.kind() {
                ConfigurationFileErrorKind::MissingFile => (
                    Some(Configuration::empty()),
                    ConfigurationStartupStatus::ReloadedMissing,
                ),
                kind => (
                    None,
                    ConfigurationStartupStatus::ReloadFailure(failure_from_file_error(kind)),
                ),
            },
        }
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

    /// Saves an already validated complete replacement only after an explicit
    /// Settings action. For the native source alone, a missing final config
    /// directory may be created with normal user/default permissions; no
    /// override directory and no configuration file is created otherwise.
    pub(crate) fn save_workspace(
        &self,
        configuration: &Configuration,
    ) -> ConfigurationStartupStatus {
        let selected = match &self.selected_path {
            Ok(selected) => selected,
            Err(failure) => {
                return ConfigurationStartupStatus::WorkspaceSaveFailure(*failure);
            }
        };
        if let Some(directory) = &selected.native_directory {
            if !directory.exists() && create_native_configuration_directory(directory).is_err() {
                return ConfigurationStartupStatus::WorkspaceSaveFailure(
                    ConfigurationLoadFailure::Unreadable,
                );
            }
        }
        match configuration.save_to_path(&selected.path) {
            Ok(()) => ConfigurationStartupStatus::WorkspaceSaved,
            Err(error) => ConfigurationStartupStatus::WorkspaceSaveFailure(
                failure_from_file_error(error.kind()),
            ),
        }
    }
}

#[cfg(unix)]
fn create_native_configuration_directory(directory: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(directory) {
        Ok(()) => Ok(()),
        Err(_error) if directory.is_dir() => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(not(unix))]
fn create_native_configuration_directory(directory: &std::path::Path) -> std::io::Result<()> {
    match fs::create_dir(directory) {
        Ok(()) => Ok(()),
        Err(error) if directory.is_dir() => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn load() -> StartupConfiguration {
    let reloader = ConfigurationReloader::from_source_selection(select_configuration_source(
        std::env::var_os(CONFIG_PATH_ENV),
        native_configuration_directory(),
    ));
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
        | ConfigurationFileErrorKind::Replace
        | ConfigurationFileErrorKind::RestorePrevious
        | ConfigurationFileErrorKind::CleanupPrevious
        | ConfigurationFileErrorKind::SyncParentDirectory
        | ConfigurationFileErrorKind::Serialization => ConfigurationLoadFailure::Unreadable,
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
    fn reload_valid_configuration_replaces_the_complete_candidate() {
        let directory = TestDirectory::new();
        let path = directory.file("config.toml");
        fs::write(
            &path,
            "schema_version = 1\n\n[[profiles]]\nkind = \"local\"\nid = \"old\"\nexecutable = \"/bin/sh\"\n",
        )
        .expect("initial test configuration can be written");
        let startup = load_from_path(&path);
        let (active, _, reloader) = startup.into_parts();

        fs::write(
            &path,
            "schema_version = 1\n\n[[profiles]]\nkind = \"local\"\nid = \"new\"\nexecutable = \"/bin/sh\"\n",
        )
        .expect("replacement test configuration can be written");
        let (replacement, status) = reloader.reload();

        assert_eq!(status, ConfigurationStartupStatus::Reloaded);
        assert_eq!(
            active
                .profile("old")
                .map(festerm_config::Profile::identifier),
            Some("old")
        );
        assert_eq!(
            replacement
                .as_ref()
                .and_then(|configuration| configuration.profile("new"))
                .map(festerm_config::Profile::identifier),
            Some("new")
        );
    }

    #[test]
    fn invalid_reload_retains_the_last_known_configuration() {
        let directory = TestDirectory::new();
        let path = directory.file("config.toml");
        fs::write(
            &path,
            "schema_version = 1\n\n[[profiles]]\nkind = \"local\"\nid = \"working\"\nexecutable = \"/bin/sh\"\n",
        )
        .expect("initial test configuration can be written");
        let startup = load_from_path(&path);
        let (active, _, reloader) = startup.into_parts();

        let source_toml = "schema_version = [private source TOML]";
        fs::write(&path, source_toml).expect("invalid replacement can be written");
        let (replacement, status) = reloader.reload();
        let diagnostic = status.settings_message();

        assert_eq!(
            status,
            ConfigurationStartupStatus::ReloadFailure(ConfigurationLoadFailure::Invalid)
        );
        assert!(replacement.is_none());
        assert_eq!(
            active
                .profile("working")
                .map(festerm_config::Profile::identifier),
            Some("working")
        );
        assert!(!diagnostic.contains(source_toml));
        assert!(!diagnostic.contains(path.to_string_lossy().as_ref()));
    }

    #[test]
    fn missing_reload_replaces_configuration_with_defaults() {
        let directory = TestDirectory::new();
        let path = directory.file("config.toml");
        fs::write(
            &path,
            "schema_version = 1\n\n[[profiles]]\nkind = \"local\"\nid = \"working\"\nexecutable = \"/bin/sh\"\n",
        )
        .expect("initial test configuration can be written");
        let startup = load_from_path(&path);
        let (active, _, reloader) = startup.into_parts();

        fs::remove_file(&path).expect("test configuration can be removed");
        let (replacement, status) = reloader.reload();

        assert_eq!(status, ConfigurationStartupStatus::ReloadedMissing);
        assert_eq!(
            active
                .profile("working")
                .map(festerm_config::Profile::identifier),
            Some("working")
        );
        assert_eq!(replacement, Some(Configuration::empty()));
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
            ConfigurationStartupStatus::WorkspaceSaved
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
    fn failed_workspace_save_is_content_free() {
        let directory = TestDirectory::new();
        let source_path = directory.path().to_path_buf();
        let reloader = ConfigurationReloader::from_path_for_test(source_path.clone());
        let status = reloader.save_workspace(&Configuration::empty());
        let diagnostic = status.settings_message();

        assert!(matches!(
            status,
            ConfigurationStartupStatus::WorkspaceSaveFailure(ConfigurationLoadFailure::Unreadable)
        ));
        assert!(!diagnostic.contains(source_path.to_string_lossy().as_ref()));
        assert!(!diagnostic.contains("schema_version"));
    }
}
