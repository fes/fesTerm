use std::{ffi::OsString, fs::File, os::unix::fs::MetadataExt, path::Path};

use festerm_recording::MAX_ARTIFACT_BYTES;
use festerm_unix_security::{is_current_user_only, make_private, open_file_nofollow};
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "android"
))]
use nix::unistd::{unlinkat, UnlinkatFlags};
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "android"
))]
use std::io::Read;

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "android"
))]
use crate::fingerprint;
use crate::ExportError;

#[derive(Debug)]
pub(crate) struct SourceBinding {
    directory: File,
    name: OsString,
    device: u64,
    inode: u64,
    length: u64,
}

impl SourceBinding {
    pub(crate) fn name(&self) -> &std::ffi::OsStr {
        &self.name
    }

    pub(crate) fn open(path: &Path) -> Result<(Self, File), ExportError> {
        let name = path
            .file_name()
            .ok_or_else(|| ExportError::new("recording source has no filename"))?
            .to_owned();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let directory = File::open(parent).map_err(|error| {
            ExportError::new(format!("could not open private recording storage: {error}"))
        })?;
        require_private(&directory, true, "recording storage directory")?;
        let file = open_file_nofollow(&directory, Path::new(&name)).map_err(|error| {
            ExportError::new(format!("could not open recording source: {error}"))
        })?;
        require_private(&file, false, "recording source")?;
        let metadata = file.metadata().map_err(|error| {
            ExportError::new(format!("could not inspect recording source: {error}"))
        })?;
        if metadata.len() > MAX_ARTIFACT_BYTES {
            return Err(ExportError::new("recording source exceeds its size limit"));
        }
        Ok((
            Self {
                directory,
                name,
                device: metadata.dev(),
                inode: metadata.ino(),
                length: metadata.len(),
            },
            file,
        ))
    }

    pub(crate) fn open_current(&self) -> Result<File, ExportError> {
        require_private(&self.directory, true, "recording storage directory")?;
        let file = open_file_nofollow(&self.directory, Path::new(&self.name)).map_err(|error| {
            ExportError::new(format!("could not reopen recording source: {error}"))
        })?;
        require_private(&file, false, "recording source")?;
        let metadata = file.metadata().map_err(|error| {
            ExportError::new(format!("could not inspect recording source: {error}"))
        })?;
        if metadata.dev() != self.device
            || metadata.ino() != self.inode
            || metadata.len() != self.length
        {
            return Err(ExportError::new(
                "the recording source changed or was replaced",
            ));
        }
        Ok(file)
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))]
    pub(crate) fn remove_verified(&self, expected: [u8; 32]) -> Result<(), ExportError> {
        let mut source = self.open_current()?;
        let before = source.metadata().map_err(|error| {
            ExportError::new(format!("could not inspect recording source: {error}"))
        })?;
        let (actual, length) = fingerprint(&mut source.by_ref().take(MAX_ARTIFACT_BYTES + 1))
            .map_err(|error| {
                ExportError::new(format!("could not verify recording source: {error}"))
            })?;
        let after = source.metadata().map_err(|error| {
            ExportError::new(format!("could not inspect recording source: {error}"))
        })?;
        if actual != expected
            || length != self.length
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return Err(ExportError::new(
                "the recording source changed after export and was retained",
            ));
        }
        let current = self.open_current()?.metadata().map_err(|error| {
            ExportError::new(format!("could not inspect recording source: {error}"))
        })?;
        if after.mtime() != current.mtime()
            || after.mtime_nsec() != current.mtime_nsec()
            || after.ctime() != current.ctime()
            || after.ctime_nsec() != current.ctime_nsec()
        {
            return Err(ExportError::new(
                "the recording source changed after export and was retained",
            ));
        }
        unlinkat(
            &self.directory,
            Path::new(&self.name),
            UnlinkatFlags::NoRemoveDir,
        )
        .map_err(|error| {
            ExportError::new(format!(
                "published videos are intact, but the recording source could not be deleted: {error}"
            ))
        })?;
        self.directory.sync_all().map_err(|error| {
            ExportError::new(format!(
                "the source was removed, but its storage directory could not be synced: {error}"
            ))
        })
    }
}

pub(crate) fn require_private(
    file: &File,
    directory: bool,
    subject: &str,
) -> Result<(), ExportError> {
    match is_current_user_only(file, directory) {
        Ok(true) => Ok(()),
        Ok(false) => Err(ExportError::new(format!(
            "the {subject} is not private to the current user"
        ))),
        Err(error) => Err(ExportError::new(format!(
            "could not verify the {subject}'s privacy: {error}"
        ))),
    }
}

pub(crate) fn make_staged_file_private(directory: &File, file: &File) -> Result<(), ExportError> {
    let parent = directory
        .metadata()
        .map_err(|error| ExportError::new(format!("could not inspect video staging: {error}")))?;
    let metadata = file
        .metadata()
        .map_err(|error| ExportError::new(format!("could not inspect staged MP4: {error}")))?;
    if metadata.uid() != parent.uid() || metadata.nlink() != 1 {
        return Err(ExportError::new(
            "the staged MP4 is not an exclusively owned private file",
        ));
    }
    make_private(file)
        .map_err(|error| ExportError::new(format!("could not secure staged MP4: {error}")))?;
    require_private(file, false, "staged MP4")
}
