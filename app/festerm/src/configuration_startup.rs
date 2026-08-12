use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use directories::ProjectDirs;
use festerm_config::{Configuration, ConfigurationFileErrorKind};

const CONFIG_PATH_ENV: &str = "FESTERM_CONFIG_PATH";
const CONFIG_FILE_NAME: &str = "config.toml";

/// Content-free outcome of selecting and loading startup configuration.
///
/// This deliberately retains neither the selected path nor source TOML. The
/// application can show it safely in Settings and diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConfigurationStartupStatus {
    Loaded,
    Missing,
    Invalid,
    Unreadable,
    OverrideUnavailable,
    NativeLocationUnavailable,
}

impl ConfigurationStartupStatus {
    pub(crate) const fn settings_message(self) -> &'static str {
        match self {
            Self::Loaded => {
                "Configuration was loaded at startup. Changes to config.toml require restarting fesTerm."
            }
            Self::Missing => {
                "No configuration file was found at startup. fesTerm is using its defaults and will not create one automatically."
            }
            Self::Invalid => {
                "Configuration was ignored because it is invalid. Fix config.toml and restart fesTerm."
            }
            Self::Unreadable => {
                "Configuration could not be read. Check that config.toml is readable, then restart fesTerm."
            }
            Self::OverrideUnavailable => {
                "FESTERM_CONFIG_PATH could not be used. Set it to a non-empty Unicode file path, then restart fesTerm."
            }
            Self::NativeLocationUnavailable => {
                "The native configuration location is unavailable. Set FESTERM_CONFIG_PATH to a Unicode file path, then restart fesTerm."
            }
        }
    }

    pub(crate) const fn is_problem(self) -> bool {
        matches!(
            self,
            Self::Invalid
                | Self::Unreadable
                | Self::OverrideUnavailable
                | Self::NativeLocationUnavailable
        )
    }
}

/// Configuration selected during process startup.
///
/// The configuration is supplied to the application once; this slice does not
/// watch, reload, or write configuration files.
pub(crate) struct StartupConfiguration {
    configuration: Configuration,
    status: ConfigurationStartupStatus,
}

impl StartupConfiguration {
    pub(crate) fn configuration(self) -> Configuration {
        self.configuration
    }

    pub(crate) const fn status(&self) -> ConfigurationStartupStatus {
        self.status
    }
}

pub(crate) fn load() -> StartupConfiguration {
    let selected_path = select_configuration_path(
        std::env::var_os(CONFIG_PATH_ENV),
        native_configuration_directory(),
    );
    match selected_path {
        Ok(path) => load_from_path(&path),
        Err(status) => empty_with_status(status),
    }
}

fn native_configuration_directory() -> Option<PathBuf> {
    ProjectDirs::from("com", "fes", "fesTerm")
        .map(|directories| directories.config_dir().to_path_buf())
}

fn select_configuration_path(
    override_value: Option<OsString>,
    native_config_directory: Option<PathBuf>,
) -> Result<PathBuf, ConfigurationStartupStatus> {
    if let Some(override_value) = override_value {
        let override_value = override_value
            .into_string()
            .map_err(|_| ConfigurationStartupStatus::OverrideUnavailable)?;
        if override_value.is_empty() {
            return Err(ConfigurationStartupStatus::OverrideUnavailable);
        }
        return Ok(PathBuf::from(override_value));
    }

    native_config_directory
        .map(|directory| directory.join(CONFIG_FILE_NAME))
        .ok_or(ConfigurationStartupStatus::NativeLocationUnavailable)
}

fn load_from_path(path: &Path) -> StartupConfiguration {
    match Configuration::load_from_path(path) {
        Ok(configuration) => StartupConfiguration {
            configuration,
            status: ConfigurationStartupStatus::Loaded,
        },
        Err(error) => {
            let status = match error.kind() {
                ConfigurationFileErrorKind::MissingFile => ConfigurationStartupStatus::Missing,
                ConfigurationFileErrorKind::Parse => ConfigurationStartupStatus::Invalid,
                ConfigurationFileErrorKind::Read
                | ConfigurationFileErrorKind::InvalidTargetPath
                | ConfigurationFileErrorKind::CreateTemporary
                | ConfigurationFileErrorKind::WriteTemporary
                | ConfigurationFileErrorKind::SyncTemporary
                | ConfigurationFileErrorKind::Replace
                | ConfigurationFileErrorKind::RestorePrevious
                | ConfigurationFileErrorKind::CleanupPrevious
                | ConfigurationFileErrorKind::SyncParentDirectory
                | ConfigurationFileErrorKind::Serialization => {
                    ConfigurationStartupStatus::Unreadable
                }
            };
            empty_with_status(status)
        }
    }
}

fn empty_with_status(status: ConfigurationStartupStatus) -> StartupConfiguration {
    StartupConfiguration {
        configuration: Configuration::empty(),
        status,
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

        assert_eq!(startup.status(), ConfigurationStartupStatus::Missing);
        assert_eq!(startup.configuration(), Configuration::empty());
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

        assert_eq!(startup.status(), ConfigurationStartupStatus::Loaded);
        assert_eq!(startup.configuration().profiles().len(), 1);
    }

    #[test]
    fn invalid_configuration_is_ignored_with_a_content_free_diagnostic() {
        let directory = TestDirectory::new();
        let path = directory.file("arbitrary-user-path.toml");
        let source_toml = "schema_version = [private source TOML]";
        fs::write(&path, source_toml).expect("test configuration can be written");

        let startup = load_from_path(&path);
        let diagnostic = startup.status().settings_message();

        assert_eq!(startup.status(), ConfigurationStartupStatus::Invalid);
        assert_eq!(startup.configuration(), Configuration::empty());
        assert!(!diagnostic.contains(source_toml));
        assert!(!diagnostic.contains(path.to_string_lossy().as_ref()));
    }

    #[test]
    fn unreadable_configuration_is_ignored_with_a_content_free_diagnostic() {
        let directory = TestDirectory::new();
        let startup = load_from_path(directory.path());
        let diagnostic = startup.status().settings_message();

        assert_eq!(startup.status(), ConfigurationStartupStatus::Unreadable);
        assert_eq!(startup.configuration(), Configuration::empty());
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
            Err(ConfigurationStartupStatus::OverrideUnavailable)
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
            Err(ConfigurationStartupStatus::OverrideUnavailable)
        );
    }
}
