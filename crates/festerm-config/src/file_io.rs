use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process,
    sync::atomic::Ordering,
};

use crate::{
    ConfigError, Configuration, ConfigurationFileError, ConfigurationFileErrorKind,
    NEXT_TEMPORARY_FILE_ID, TEMPORARY_FILE_ATTEMPTS,
};

pub(crate) fn read_file_error(error: std::io::Error) -> ConfigurationFileError {
    let kind = if error.kind() == std::io::ErrorKind::NotFound {
        ConfigurationFileErrorKind::MissingFile
    } else {
        ConfigurationFileErrorKind::Read
    };
    ConfigurationFileError::new(kind)
}

pub(crate) fn parent_directory(path: &Path) -> Result<&Path, ConfigurationFileError> {
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

pub(crate) fn validate_target_file(path: &Path) -> Result<(), ConfigurationFileError> {
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

pub(crate) struct TemporaryFile {
    path: PathBuf,
    file: Option<File>,
    persist: bool,
}

impl TemporaryFile {
    pub(crate) fn create(parent: &Path) -> Result<Self, ConfigurationFileError> {
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

    pub(crate) fn file_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("temporary configuration file is open until replacement")
    }

    pub(crate) fn close_file(&mut self) {
        self.file.take();
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn persist(&mut self) {
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
pub(crate) fn replace_file(temporary: &Path, target: &Path) -> Result<(), ConfigurationFileError> {
    fs::rename(temporary, target)
        .map_err(|_| ConfigurationFileError::new(ConfigurationFileErrorKind::Replace))
}

#[cfg(windows)]
pub(crate) fn replace_file(temporary: &Path, target: &Path) -> Result<(), ConfigurationFileError> {
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
pub(crate) fn sync_parent_directory(parent: &Path) -> Result<(), ConfigurationFileError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ConfigurationFileError::new(ConfigurationFileErrorKind::SyncParentDirectory))
}

#[cfg(not(unix))]
pub(crate) fn sync_parent_directory(_parent: &Path) -> Result<(), ConfigurationFileError> {
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
}
