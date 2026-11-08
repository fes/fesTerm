use std::{
    fs::{self, File},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use festerm_recording::{RecordingLease, MAX_STREAMS};
use festerm_unix_security::{
    create_private_directory, open_directory_read_nofollow, open_file_nofollow,
};
use nix::unistd::{unlinkat, UnlinkatFlags};
use uuid::Uuid;

use crate::{source_unix, ExportError};

/// A uniquely created, app-private directory for one export's temporary MP4s.
/// The caller must put its parent in backup-excluded storage and explicitly
/// clean up the directory, reporting any failure to the user.
pub struct PrivateStaging {
    parent_path: PathBuf,
    parent: File,
    directory: File,
    name: String,
}

impl PrivateStaging {
    pub fn create(parent_path: &Path) -> Result<Self, ExportError> {
        let parent = File::open(parent_path).map_err(|error| {
            ExportError::new(format!("could not open recording storage: {error}"))
        })?;
        check_path(parent_path, &parent, "recording storage")?;
        let name = format!("recording-stage-{}", Uuid::new_v4().simple());
        let directory = create_private_directory(&parent, Path::new(&name)).map_err(|error| {
            ExportError::new(format!("could not create private video staging: {error}"))
        })?;
        parent.sync_all().map_err(|error| {
            ExportError::new(format!(
                "private video staging {name} was created but could not be synced: {error}"
            ))
        })?;
        let staging = Self {
            parent_path: parent_path.to_owned(),
            parent,
            directory,
            name,
        };
        staging.check()?;
        Ok(staging)
    }

    pub fn path(&self) -> PathBuf {
        self.parent_path.join(&self.name)
    }

    /// Finds interrupted staging directories while no capture or export owns
    /// the private storage lease. Cleanup remains an explicit operation.
    pub fn recover(parent_path: &Path, lease: &RecordingLease) -> Result<Vec<Self>, ExportError> {
        lease.verify_for(parent_path).map_err(|error| {
            ExportError::new(format!("recording storage is unavailable: {error}"))
        })?;
        let parent = File::open(parent_path).map_err(|error| {
            ExportError::new(format!("could not open recording storage: {error}"))
        })?;
        check_path(parent_path, &parent, "recording storage")?;
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
                open_directory_read_nofollow(&parent, Path::new(&name)).map_err(|error| {
                    ExportError::new(format!("could not open private video staging: {error}"))
                })?;
            let staging = Self {
                parent_path: parent_path.to_owned(),
                parent: parent.try_clone().map_err(|error| {
                    ExportError::new(format!("could not retain recording storage: {error}"))
                })?,
                directory,
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

    /// Deletes only expected, private staged MP4s in this exact directory.
    /// Unexpected entries and replacements are preserved and reported.
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
            let file = open_file_nofollow(&self.directory, Path::new(&name)).map_err(|error| {
                ExportError::new(format!("could not inspect staged MP4: {error}"))
            })?;
            source_unix::make_staged_file_private(&self.directory, &file)?;
            files.push((name, file));
        }
        self.check()?;
        for (name, file) in files {
            let current =
                open_file_nofollow(&self.directory, Path::new(&name)).map_err(|error| {
                    ExportError::new(format!("could not recheck staged MP4: {error}"))
                })?;
            source_unix::require_private(&current, false, "staged MP4")?;
            let original = file.metadata().map_err(|error| {
                ExportError::new(format!("could not identify staged MP4: {error}"))
            })?;
            let current = current.metadata().map_err(|error| {
                ExportError::new(format!("could not identify staged MP4: {error}"))
            })?;
            if original.dev() != current.dev() || original.ino() != current.ino() {
                return Err(ExportError::new("a staged MP4 changed and was retained"));
            }
            unlinkat(
                &self.directory,
                Path::new(&name),
                UnlinkatFlags::NoRemoveDir,
            )
            .map_err(|error| ExportError::new(format!("could not remove staged MP4: {error}")))?;
        }
        self.directory.sync_all().map_err(|error| {
            ExportError::new(format!("could not sync video staging cleanup: {error}"))
        })?;
        self.check()?;
        unlinkat(
            &self.parent,
            Path::new(&self.name),
            UnlinkatFlags::RemoveDir,
        )
        .map_err(|error| {
            ExportError::new(format!("could not remove private video staging: {error}"))
        })?;
        self.parent.sync_all().map_err(|error| {
            ExportError::new(format!("could not sync recording storage cleanup: {error}"))
        })?;
        check_path(&self.parent_path, &self.parent, "recording storage")
    }

    fn check(&self) -> Result<(), ExportError> {
        check_path(&self.parent_path, &self.parent, "recording storage")?;
        source_unix::require_private(&self.directory, true, "video staging directory")?;
        let current =
            open_directory_read_nofollow(&self.parent, Path::new(&self.name)).map_err(|error| {
                ExportError::new(format!("could not inspect video staging: {error}"))
            })?;
        let original = self.directory.metadata().map_err(|error| {
            ExportError::new(format!("could not identify video staging: {error}"))
        })?;
        let current = current.metadata().map_err(|error| {
            ExportError::new(format!("could not identify video staging: {error}"))
        })?;
        if original.dev() != current.dev() || original.ino() != current.ino() {
            return Err(ExportError::new(
                "private video staging changed and was retained",
            ));
        }
        Ok(())
    }
}

fn check_path(path: &Path, pinned: &File, subject: &str) -> Result<(), ExportError> {
    source_unix::require_private(pinned, true, subject)?;
    let current = fs::symlink_metadata(path)
        .map_err(|error| ExportError::new(format!("could not inspect {subject}: {error}")))?;
    let original = pinned
        .metadata()
        .map_err(|error| ExportError::new(format!("could not identify {subject}: {error}")))?;
    if !current.is_dir() || current.dev() != original.dev() || current.ino() != original.ino() {
        return Err(ExportError::new(format!("{subject} changed")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_only_expected_private_staging_entries() {
        let root = tempfile::tempdir().unwrap();
        festerm_unix_security::make_private_directory(&File::open(root.path()).unwrap()).unwrap();
        let staging = PrivateStaging::create(root.path()).unwrap();
        let path = staging.path();
        fs::write(path.join("stream-01.mp4"), b"video").unwrap();
        staging.cleanup().unwrap();
        assert!(!path.exists());

        let staging = PrivateStaging::create(root.path()).unwrap();
        let path = staging.path();
        fs::write(path.join("unrelated"), b"keep").unwrap();
        assert!(staging.cleanup().is_err());
        assert_eq!(fs::read(path.join("unrelated")).unwrap(), b"keep");
    }

    #[test]
    fn refuses_replaced_staging_without_deleting_either_directory() {
        let root = tempfile::tempdir().unwrap();
        festerm_unix_security::make_private_directory(&File::open(root.path()).unwrap()).unwrap();
        let staging = PrivateStaging::create(root.path()).unwrap();
        let path = staging.path();
        let original = root.path().join("original");
        fs::rename(&path, &original).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(staging.cleanup().is_err());
        assert!(path.exists());
        assert!(original.exists());
    }

    #[test]
    fn recovers_interrupted_private_staging_under_lease() {
        let root = tempfile::tempdir().unwrap();
        festerm_unix_security::make_private_directory(&File::open(root.path()).unwrap()).unwrap();
        let lease = RecordingLease::acquire(root.path()).unwrap();
        let staging = PrivateStaging::create(root.path()).unwrap();
        let path = staging.path();
        fs::write(path.join("stream-01.mp4"), b"unfinished").unwrap();
        drop(staging);
        let mut recovered = PrivateStaging::recover(root.path(), &lease).unwrap();
        assert_eq!(recovered.len(), 1);
        recovered.remove(0).cleanup().unwrap();
        assert!(!path.exists());
    }
}
