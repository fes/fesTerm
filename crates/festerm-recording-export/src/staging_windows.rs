use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

use festerm_recording::{RecordingLease, MAX_STREAMS};
use festerm_windows_security as security;
use uuid::Uuid;

use crate::{source_windows, ExportError};

/// One private export staging directory, pinned while its pathname is used.
pub struct PrivateStaging {
    parent_path: PathBuf,
    parent: File,
    directory: File,
    identity: (u32, u64),
    name: String,
}

impl PrivateStaging {
    pub fn create(parent_path: &Path) -> Result<Self, ExportError> {
        let parent =
            source_windows::open_private_directory(parent_path, "recording storage directory")?;
        let name = format!("recording-stage-{}", Uuid::new_v4().simple());
        let created = security::create_current_user_only_directory(&parent, Path::new(&name))
            .map_err(|error| {
                ExportError::new(format!("could not create private video staging: {error}"))
            })?;
        let identity = security::file_identity(&created).map_err(|error| {
            ExportError::new(format!("could not identify private video staging: {error}"))
        })?;
        drop(created);
        let directory =
            security::lock_directory_path_without_delete_sharing(&parent_path.join(&name))
                .map_err(|error| {
                    ExportError::new(format!("could not pin private video staging: {error}"))
                })?;
        let staging = Self {
            parent_path: parent_path.to_owned(),
            parent,
            directory,
            identity,
            name,
        };
        staging.check()?;
        sync_directory(&staging.parent, "recording storage")?;
        Ok(staging)
    }

    pub fn path(&self) -> PathBuf {
        self.parent_path.join(&self.name)
    }

    /// Finds interrupted staging directories under an exclusive storage lease.
    pub fn recover(parent_path: &Path, lease: &RecordingLease) -> Result<Vec<Self>, ExportError> {
        lease.verify_for(parent_path).map_err(|error| {
            ExportError::new(format!("recording storage is unavailable: {error}"))
        })?;
        let parent =
            source_windows::open_private_directory(parent_path, "recording storage directory")?;
        let mut names = Vec::new();
        for entry in fs::read_dir(parent_path).map_err(|error| {
            ExportError::new(format!("could not inspect recording storage: {error}"))
        })? {
            let entry = entry.map_err(|error| {
                ExportError::new(format!("could not inspect recording storage: {error}"))
            })?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Some(id) = name.strip_prefix("recording-stage-") {
                if Uuid::parse_str(id)
                    .ok()
                    .is_none_or(|value| value.simple().to_string() != id)
                {
                    return Err(ExportError::new("invalid private video staging name"));
                }
                names.push(name.to_owned());
                if names.len() > 4096 {
                    return Err(ExportError::new(
                        "too many private video staging directories",
                    ));
                }
            }
        }
        names.sort();
        let mut recovered = Vec::with_capacity(names.len());
        for name in names {
            let directory =
                security::lock_directory_path_without_delete_sharing(&parent_path.join(&name))
                    .map_err(|error| {
                        ExportError::new(format!("could not pin private video staging: {error}"))
                    })?;
            let identity = security::file_identity(&directory).map_err(|error| {
                ExportError::new(format!("could not identify private video staging: {error}"))
            })?;
            let staging = Self {
                parent_path: parent_path.to_owned(),
                parent: parent.try_clone().map_err(|error| {
                    ExportError::new(format!("could not retain recording storage: {error}"))
                })?,
                directory,
                identity,
                name,
            };
            staging.check()?;
            recovered.push(staging);
        }
        lease
            .verify_for(parent_path)
            .map_err(|error| ExportError::new(format!("recording storage changed: {error}")))?;
        Ok(recovered)
    }

    /// Removes only known, unchanged private video files and this directory.
    pub fn cleanup(self) -> Result<(), ExportError> {
        self.check()?;
        let mut files = Vec::new();
        for entry in fs::read_dir(self.path()).map_err(|error| {
            ExportError::new(format!("could not inspect private video staging: {error}"))
        })? {
            let entry = entry.map_err(|error| {
                ExportError::new(format!("could not inspect private video staging: {error}"))
            })?;
            let name = entry.file_name();
            let valid = name.to_str().is_some_and(|name| {
                (1..=MAX_STREAMS).any(|index| name == format!("stream-{index:02}.mp4"))
            });
            if !valid {
                return Err(ExportError::new(format!(
                    "private video staging {} contains an unexpected entry and was retained",
                    self.name
                )));
            }
            let file = security::open_file_no_reparse(&self.directory, Path::new(&name)).map_err(
                |error| ExportError::new(format!("could not inspect staged MP4: {error}")),
            )?;
            source_windows::require_private(&file, false, "staged MP4")?;
            files.push((name, file));
        }
        self.check()?;
        for (name, original) in files {
            let pinned =
                security::open_file_no_reparse_for_verification(&self.directory, Path::new(&name))
                    .map_err(|error| {
                        ExportError::new(format!("could not pin staged MP4: {error}"))
                    })?;
            source_windows::require_private(&pinned, false, "staged MP4")?;
            if !security::same_file_identity(&original, &pinned).map_err(|error| {
                ExportError::new(format!("could not identify staged MP4: {error}"))
            })? {
                return Err(ExportError::new("a staged MP4 changed and was retained"));
            }
            drop(original);
            security::remove_private_staging_file(&pinned).map_err(|error| {
                ExportError::new(format!("could not remove staged MP4: {error}"))
            })?;
            drop(pinned);
        }
        sync_directory(&self.directory, "video staging")?;
        self.check()?;
        let Self {
            parent,
            directory,
            identity,
            name,
            ..
        } = self;
        drop(directory);
        let pinned = security::open_private_directory_for_move(&parent, Path::new(&name)).map_err(
            |error| ExportError::new(format!("could not pin video staging for removal: {error}")),
        )?;
        if security::file_identity(&pinned).map_err(|error| {
            ExportError::new(format!("could not identify video staging: {error}"))
        })? != identity
        {
            return Err(ExportError::new(
                "private video staging changed and was retained",
            ));
        }
        security::delete_directory_by_handle(&pinned).map_err(|error| {
            ExportError::new(format!("could not remove private video staging: {error}"))
        })?;
        drop(pinned);
        sync_directory(&parent, "recording storage")
    }

    fn check(&self) -> Result<(), ExportError> {
        source_windows::require_private(&self.parent, true, "recording storage directory")?;
        source_windows::require_private(&self.directory, true, "video staging directory")?;
        let current_parent =
            security::lock_directory_path_without_delete_sharing(&self.parent_path).map_err(
                |error| ExportError::new(format!("could not recheck recording storage: {error}")),
            )?;
        if !security::same_file_identity(&self.parent, &current_parent).map_err(|error| {
            ExportError::new(format!("could not identify recording storage: {error}"))
        })? {
            return Err(ExportError::new("recording storage changed"));
        }
        let current = security::open_directory_no_reparse(&self.parent, Path::new(&self.name))
            .map_err(|error| {
                ExportError::new(format!("could not inspect private video staging: {error}"))
            })?;
        if security::file_identity(&self.directory).map_err(|error| {
            ExportError::new(format!("could not identify video staging: {error}"))
        })? != self.identity
            || !security::same_file_identity(&self.directory, &current).map_err(|error| {
                ExportError::new(format!("could not identify video staging: {error}"))
            })?
        {
            return Err(ExportError::new(
                "private video staging changed and was retained",
            ));
        }
        Ok(())
    }
}

fn sync_directory(directory: &File, subject: &str) -> Result<(), ExportError> {
    security::lock_directory_for_sync(directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| ExportError::new(format!("could not sync {subject}: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn private_parent() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = security::lock_directory_path_without_delete_sharing(temp.path()).unwrap();
        security::create_current_user_only_directory(&root, Path::new("private")).unwrap();
        let path = temp.path().join("private");
        (temp, path)
    }

    #[test]
    fn cleans_only_expected_private_staging_entries() {
        let (_temp, root) = private_parent();
        let staging = PrivateStaging::create(&root).unwrap();
        let path = staging.path();
        let mut file =
            security::create_current_user_only_file(&staging.directory, Path::new("stream-01.mp4"))
                .unwrap();
        file.write_all(b"video").unwrap();
        drop(file);
        staging.cleanup().unwrap();
        assert!(!path.exists());

        let staging = PrivateStaging::create(&root).unwrap();
        let path = staging.path();
        fs::write(path.join("unrelated"), b"keep").unwrap();
        assert!(staging.cleanup().is_err());
        assert_eq!(fs::read(path.join("unrelated")).unwrap(), b"keep");
    }

    #[test]
    fn pins_staging_against_replacement_during_export() {
        let (_temp, root) = private_parent();
        let staging = PrivateStaging::create(&root).unwrap();
        assert!(fs::rename(staging.path(), root.join("renamed")).is_err());
        staging.cleanup().unwrap();
    }

    #[test]
    fn recovers_interrupted_private_staging_under_lease() {
        let (_temp, root) = private_parent();
        let lease = RecordingLease::acquire(&root).unwrap();
        let staging = PrivateStaging::create(&root).unwrap();
        let path = staging.path();
        drop(staging);
        let mut recovered = PrivateStaging::recover(&root, &lease).unwrap();
        assert_eq!(recovered.len(), 1);
        recovered.remove(0).cleanup().unwrap();
        assert!(!path.exists());
    }
}
