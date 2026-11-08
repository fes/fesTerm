use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

use festerm_windows_security as security;

use super::CaptureError;

pub(super) struct PrivateSource {
    directory: File,
    file: File,
    partial_path: PathBuf,
    final_path: PathBuf,
    identity: (u32, u64),
}

impl PrivateSource {
    pub(super) fn create(
        partial_path: &Path,
        final_path: &Path,
    ) -> Result<(File, Self), CaptureError> {
        let parent = partial_path
            .parent()
            .filter(|parent| final_path.parent() == Some(*parent))
            .ok_or_else(|| CaptureError::new("invalid private recording storage"))?;
        let name = partial_path
            .file_name()
            .ok_or_else(|| CaptureError::new("invalid recording source name"))?;
        let directory =
            security::lock_directory_path_without_delete_sharing(parent).map_err(|error| {
                CaptureError::new(format!(
                    "private recording storage could not be opened: {error}"
                ))
            })?;
        check_directory(&directory).map_err(|error| {
            CaptureError::new(format!("private recording storage is unavailable: {error}"))
        })?;
        let file = security::create_current_user_only_file_exclusive(&directory, Path::new(name))
            .map_err(|error| {
            CaptureError::new(format!("private recording could not be created: {error}"))
        })?;
        let retained = file.try_clone().map_err(|error| {
            CaptureError::new(format!(
                "private recording could not be retained; the empty source may remain: {error}"
            ))
        })?;
        let identity = security::file_identity(&retained).map_err(|error| {
            CaptureError::new(format!(
                "private recording could not be identified; the empty source may remain: {error}"
            ))
        })?;
        Ok((
            file,
            Self {
                directory,
                file: retained,
                partial_path: partial_path.to_owned(),
                final_path: final_path.to_owned(),
                identity,
            },
        ))
    }

    pub(super) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            directory: self.directory.try_clone()?,
            file: self.file.try_clone()?,
            partial_path: self.partial_path.clone(),
            final_path: self.final_path.clone(),
            identity: self.identity,
        })
    }

    pub(super) fn remove_partial(self) -> io::Result<()> {
        let pinned = self.pin_partial()?;
        security::remove_private_staging_file(&pinned)?;
        drop(pinned);
        drop(self.file);
        sync_directory(&self.directory)
    }

    pub(super) fn finalize(self) -> Result<(), (PathBuf, String)> {
        let before = |error: io::Error| {
            (
                self.partial_path.clone(),
                format!("recording could not be finalized: {error}"),
            )
        };
        let pinned = self.pin_partial().map_err(before)?;
        let final_name = self
            .final_path
            .file_name()
            .map(Path::new)
            .ok_or_else(|| before(io::Error::other("invalid recording destination name")))?;
        security::rename_file_noreplace(&pinned, &self.directory, final_name).map_err(before)?;
        drop(pinned);
        drop(self.file);
        let verify = || -> io::Result<()> {
            check_directory(&self.directory)?;
            let current = security::open_file_no_reparse(&self.directory, final_name)?;
            require_private_file(&current)?;
            if security::file_identity(&current)? != self.identity {
                return Err(io::Error::other("recording source changed"));
            }
            sync_directory(&self.directory)
        };
        verify().map_err(|error| {
            (
                self.final_path.clone(),
                format!(
                    "recording was finalized but its storage could not be verified and synced: {error}"
                ),
            )
        })
    }

    fn pin_partial(&self) -> io::Result<File> {
        check_directory(&self.directory)?;
        require_private_file(&self.file)?;
        let name = self
            .partial_path
            .file_name()
            .map(Path::new)
            .ok_or_else(|| io::Error::other("invalid recording source name"))?;
        let pinned = security::open_file_no_reparse_for_verification(&self.directory, name)?;
        require_private_file(&pinned)?;
        if security::file_identity(&self.file)? != self.identity
            || security::file_identity(&pinned)? != self.identity
        {
            return Err(io::Error::other("recording source changed"));
        }
        Ok(pinned)
    }
}

fn check_directory(directory: &File) -> io::Result<()> {
    if directory.metadata()?.is_dir() && security::is_current_user_only(directory)? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "recording storage is not private",
        ))
    }
}

fn require_private_file(file: &File) -> io::Result<()> {
    if file.metadata()?.is_file()
        && security::file_link_count(file)? == 1
        && security::is_current_user_only(file)?
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "recording source is not private",
        ))
    }
}

fn sync_directory(directory: &File) -> io::Result<()> {
    security::lock_directory_for_sync(directory)?.sync_all()
}
