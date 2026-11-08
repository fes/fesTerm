use std::{
    fs::{self, File},
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use super::CaptureError;

pub(super) struct PrivateSource {
    directory: File,
    directory_path: PathBuf,
    partial_path: PathBuf,
    final_path: PathBuf,
    device: u64,
    inode: u64,
}

impl PrivateSource {
    pub(super) fn create(
        partial_path: &Path,
        final_path: &Path,
    ) -> Result<(File, Self), CaptureError> {
        let directory_path = partial_path
            .parent()
            .filter(|parent| final_path.parent() == Some(*parent))
            .ok_or_else(|| CaptureError::new("invalid private recording storage"))?;
        let partial = partial_path
            .file_name()
            .ok_or_else(|| CaptureError::new("invalid recording source name"))?;
        let directory = File::open(directory_path).map_err(|error| {
            CaptureError::new(format!(
                "private recording storage could not be opened: {error}"
            ))
        })?;
        check_directory(directory_path, &directory).map_err(|error| {
            CaptureError::new(format!("private recording storage is unavailable: {error}"))
        })?;
        let file = festerm_unix_security::open_private_file(&directory, Path::new(partial), true)
            .map_err(|error| {
            CaptureError::new(format!("private recording could not be created: {error}"))
        })?;
        let metadata = file.metadata().map_err(|error| {
            CaptureError::new(format!("private recording could not be inspected: {error}"))
        })?;
        Ok((
            file,
            Self {
                directory,
                directory_path: directory_path.to_owned(),
                partial_path: partial_path.to_owned(),
                final_path: final_path.to_owned(),
                device: metadata.dev(),
                inode: metadata.ino(),
            },
        ))
    }

    pub(super) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            directory: self.directory.try_clone()?,
            directory_path: self.directory_path.clone(),
            partial_path: self.partial_path.clone(),
            final_path: self.final_path.clone(),
            device: self.device,
            inode: self.inode,
        })
    }

    pub(super) fn remove_partial(&self) -> io::Result<()> {
        self.verify_partial()?;
        festerm_unix_security::remove_private_staging_file(&self.directory, self.partial_name()?)?;
        self.directory.sync_all()
    }

    pub(super) fn finalize(&self) -> Result<(), (PathBuf, String)> {
        let before = |error: io::Error| {
            (
                self.partial_path.clone(),
                format!("recording could not be finalized: {error}"),
            )
        };
        self.verify_partial().map_err(before)?;
        let final_name = self
            .final_path
            .file_name()
            .map(Path::new)
            .ok_or_else(|| before(io::Error::other("invalid recording destination name")))?;
        festerm_unix_security::rename_noreplace(
            &self.directory,
            self.partial_name().map_err(before)?,
            &self.directory,
            final_name,
        )
        .map_err(before)?;
        self.directory
            .sync_all()
            .and_then(|()| check_directory(&self.directory_path, &self.directory))
            .map_err(|error| {
                (
                    self.final_path.clone(),
                    format!(
                        "recording was finalized but its storage could not be verified and synced: {error}"
                    ),
                )
            })
    }

    fn partial_name(&self) -> io::Result<&Path> {
        self.partial_path
            .file_name()
            .map(Path::new)
            .ok_or_else(|| io::Error::other("invalid recording source name"))
    }

    fn verify_partial(&self) -> io::Result<()> {
        check_directory(&self.directory_path, &self.directory)?;
        let file =
            festerm_unix_security::open_file_nofollow(&self.directory, self.partial_name()?)?;
        if !festerm_unix_security::is_current_user_only(&file, false)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "recording source is not private",
            ));
        }
        let metadata = file.metadata()?;
        if metadata.dev() != self.device || metadata.ino() != self.inode {
            return Err(io::Error::other("recording source changed"));
        }
        Ok(())
    }
}

fn check_directory(path: &Path, directory: &File) -> io::Result<()> {
    if !festerm_unix_security::is_current_user_only(directory, true)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "recording storage is not private",
        ));
    }
    let pinned = directory.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if !current.is_dir() || current.dev() != pinned.dev() || current.ino() != pinned.ino() {
        return Err(io::Error::other("recording storage changed"));
    }
    Ok(())
}
