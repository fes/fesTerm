use std::{
    ffi::{c_char, CStr, CString},
    fmt,
    fs::{self, File},
    os::unix::fs::MetadataExt,
    path::Path,
};

const ERROR_BYTES: usize = 1024;

unsafe extern "C" {
    fn festerm_apple_exclude_recording_directory_from_backup(
        path: *const c_char,
        error: *mut c_char,
        error_size: usize,
    ) -> bool;
}

/// Failure to establish the OS backup policy for private recording storage.
#[derive(Debug)]
pub struct BackupError(String);

impl fmt::Display for BackupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for BackupError {}

/// Excludes an exclusively owned private directory before any recording is
/// written. The caller must retain the directory and its parent authority.
pub fn exclude_directory_from_backup(path: &Path, directory: &File) -> Result<(), BackupError> {
    fn same_directory(path: &Path, directory: &File) -> Result<bool, BackupError> {
        let current = fs::symlink_metadata(path).map_err(|error| {
            BackupError(format!("could not inspect recording storage: {error}"))
        })?;
        let pinned = directory.metadata().map_err(|error| {
            BackupError(format!("could not inspect recording storage: {error}"))
        })?;
        Ok(current.is_dir()
            && !current.file_type().is_symlink()
            && pinned.is_dir()
            && current.dev() == pinned.dev()
            && current.ino() == pinned.ino())
    }

    if !same_directory(path, directory)? {
        return Err(BackupError(
            "recording storage changed or is not a directory".into(),
        ));
    }
    let c_path = path
        .to_str()
        .and_then(|path| CString::new(path).ok())
        .ok_or_else(|| BackupError("the recording storage path is invalid".into()))?;
    let mut error = [0; ERROR_BYTES];
    // SAFETY: the C strings and fixed error buffer remain valid for the call.
    let excluded = unsafe {
        festerm_apple_exclude_recording_directory_from_backup(
            c_path.as_ptr(),
            error.as_mut_ptr(),
            error.len(),
        )
    };
    if !excluded {
        // SAFETY: the native boundary always NUL-terminates its error buffer.
        let text = unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy();
        return Err(BackupError(if text.is_empty() {
            "could not exclude recording storage from backup".into()
        } else {
            text.into_owned()
        }));
    }
    if !same_directory(path, directory)? {
        return Err(BackupError(
            "recording storage changed while configuring backup exclusion".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::symlink};

    use super::*;

    #[test]
    fn backup_exclusion_requires_the_retained_directory() {
        let directory = tempfile::tempdir().unwrap();
        let handle = File::open(directory.path()).unwrap();
        exclude_directory_from_backup(directory.path(), &handle).unwrap();
        let link = directory.path().with_extension("link");
        symlink(directory.path(), &link).unwrap();
        assert!(exclude_directory_from_backup(&link, &handle).is_err());
        fs::remove_file(link).unwrap();
        let other = tempfile::tempdir().unwrap();
        assert!(exclude_directory_from_backup(other.path(), &handle).is_err());
    }
}
