use std::{ffi::OsString, fs::File, io::Read, path::Path};

use festerm_recording::MAX_ARTIFACT_BYTES;
use festerm_windows_security::{
    file_change_time, file_identity, file_link_count, is_current_user_only,
    lock_directory_for_sync, lock_directory_path_without_delete_sharing, open_file_no_reparse,
    open_file_no_reparse_for_verification, remove_private_staging_file, same_file_identity,
};

use crate::{fingerprint, ExportError};

#[derive(Debug)]
pub(crate) struct SourceBinding {
    directory: File,
    name: OsString,
    identity: (u32, u64),
    length: u64,
    change_time: i64,
}

impl SourceBinding {
    pub(crate) fn open(path: &Path) -> Result<(Self, File), ExportError> {
        let name = path
            .file_name()
            .ok_or_else(|| ExportError::new("recording source has no filename"))?
            .to_owned();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let directory = open_private_directory(parent, "recording storage directory")?;
        let file = open_file_no_reparse(&directory, Path::new(&name)).map_err(|error| {
            ExportError::new(format!("could not open recording source: {error}"))
        })?;
        require_private(&file, false, "recording source")?;
        let metadata = file.metadata().map_err(|error| {
            ExportError::new(format!("could not inspect recording source: {error}"))
        })?;
        if metadata.len() > MAX_ARTIFACT_BYTES {
            return Err(ExportError::new("recording source exceeds its size limit"));
        }
        let identity = file_identity(&file).map_err(|error| {
            ExportError::new(format!("could not identify recording source: {error}"))
        })?;
        let change_time = file_change_time(&file).map_err(|error| {
            ExportError::new(format!("could not inspect recording source: {error}"))
        })?;
        Ok((
            Self {
                directory,
                name,
                identity,
                length: metadata.len(),
                change_time,
            },
            file,
        ))
    }

    pub(crate) fn open_current(&self) -> Result<File, ExportError> {
        require_private(&self.directory, true, "recording storage directory")?;
        let file =
            open_file_no_reparse(&self.directory, Path::new(&self.name)).map_err(|error| {
                ExportError::new(format!("could not reopen recording source: {error}"))
            })?;
        self.require_original(&file)?;
        Ok(file)
    }

    fn require_original(&self, file: &File) -> Result<(), ExportError> {
        require_private(file, false, "recording source")?;
        let metadata = file.metadata().map_err(|error| {
            ExportError::new(format!("could not inspect recording source: {error}"))
        })?;
        let identity = file_identity(file).map_err(|error| {
            ExportError::new(format!("could not identify recording source: {error}"))
        })?;
        let change_time = file_change_time(file).map_err(|error| {
            ExportError::new(format!("could not inspect recording source: {error}"))
        })?;
        if identity != self.identity
            || metadata.len() != self.length
            || change_time != self.change_time
        {
            return Err(ExportError::new(
                "the recording source changed or was replaced",
            ));
        }
        Ok(())
    }

    pub(crate) fn remove_verified(&self, expected: [u8; 32]) -> Result<(), ExportError> {
        require_private(&self.directory, true, "recording storage directory")?;
        let sync = lock_directory_for_sync(&self.directory).map_err(|error| {
            ExportError::new(format!(
                "could not prepare recording storage for syncing: {error}"
            ))
        })?;
        sync.sync_all().map_err(|error| {
            ExportError::new(format!("recording storage cannot be synced: {error}"))
        })?;
        let pinned = open_file_no_reparse_for_verification(&self.directory, Path::new(&self.name))
            .map_err(|error| {
                ExportError::new(format!(
                    "could not pin recording source for deletion: {error}"
                ))
            })?;
        self.require_original(&pinned)?;
        let mut source = self.open_current()?;
        if !same_file_identity(&pinned, &source).map_err(|error| {
            ExportError::new(format!("could not identify recording source: {error}"))
        })? {
            return Err(ExportError::new(
                "the recording source was replaced and retained",
            ));
        }
        let (actual, length) = fingerprint(&mut source.by_ref().take(MAX_ARTIFACT_BYTES + 1))
            .map_err(|error| {
                ExportError::new(format!("could not verify recording source: {error}"))
            })?;
        self.require_original(&source)?;
        self.require_original(&pinned)?;
        if actual != expected || length != self.length {
            return Err(ExportError::new(
                "the recording source changed after export and was retained",
            ));
        }
        drop(source);
        remove_private_staging_file(&pinned).map_err(|error| {
            ExportError::new(format!(
                "published videos are intact, but the recording source could not be deleted: {error}"
            ))
        })?;
        drop(pinned);
        sync.sync_all().map_err(|error| {
            ExportError::new(format!(
                "the source was removed, but its storage directory could not be synced: {error}"
            ))
        })
    }
}

pub(crate) fn open_private_directory(path: &Path, subject: &str) -> Result<File, ExportError> {
    let directory = lock_directory_path_without_delete_sharing(path)
        .map_err(|error| ExportError::new(format!("could not open {subject}: {error}")))?;
    require_private(&directory, true, subject)?;
    Ok(directory)
}

pub(crate) fn require_private(
    file: &File,
    directory: bool,
    subject: &str,
) -> Result<(), ExportError> {
    let metadata = file
        .metadata()
        .map_err(|error| ExportError::new(format!("could not inspect {subject}: {error}")))?;
    let private = is_current_user_only(file).map_err(|error| {
        ExportError::new(format!("could not verify {subject} privacy: {error}"))
    })?;
    let valid_type = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
            && file_link_count(file).map_err(|error| {
                ExportError::new(format!("could not inspect {subject}: {error}"))
            })? == 1
    };
    if private && valid_type {
        Ok(())
    } else {
        Err(ExportError::new(format!(
            "the {subject} is not an exclusively owned private {}",
            if directory { "directory" } else { "file" }
        )))
    }
}

pub(crate) fn remove_staged_file(directory: &File, name: &Path) -> std::io::Result<()> {
    let file = open_file_no_reparse_for_verification(directory, name)?;
    if !is_current_user_only(&file)? || file_link_count(&file)? != 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the staged file is no longer private",
        ));
    }
    remove_private_staging_file(&file)
}
