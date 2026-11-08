use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

use super::platform;

const LEASE_NAME: &str = "recording-active.lock";

/// Coordinates capture, export and recovery across fesTerm processes sharing
/// one private recording directory. The caller must keep the lease for the
/// entire operation, including final publication and source cleanup.
pub struct RecordingLease {
    directory_path: PathBuf,
    directory: File,
    lock: File,
    identity: (u64, u64),
}

impl RecordingLease {
    pub fn acquire(directory_path: &Path) -> io::Result<Self> {
        let directory = platform::open_directory(directory_path)?;
        let name = Path::new(LEASE_NAME);
        let lock = match platform::create_file(&directory, name) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                platform::open_file_write(&directory, name)?
            }
            Err(error) => return Err(error),
        };
        platform::require_private_file(&lock)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "recording storage is in use by another operation",
                )
            } else {
                error
            }
        })?;
        let identity = platform::identity(&lock)?;
        let lease = Self {
            directory_path: directory_path.to_owned(),
            directory,
            lock,
            identity,
        };
        lease.verify()?;
        Ok(lease)
    }

    /// Refuses to proceed if the lock or its private directory was replaced.
    pub fn verify(&self) -> io::Result<()> {
        platform::check_directory(&self.directory_path, &self.directory)?;
        platform::require_private_file(&self.lock)?;
        let current = platform::open_file_read(&self.directory, Path::new(LEASE_NAME))?;
        platform::require_private_file(&current)?;
        if platform::identity(&self.lock)? != self.identity
            || platform::identity(&current)? != self.identity
        {
            return Err(io::Error::other("recording storage lock changed"));
        }
        Ok(())
    }

    pub fn verify_for(&self, directory_path: &Path) -> io::Result<()> {
        self.verify()?;
        let directory = platform::open_directory(directory_path)?;
        if platform::identity(&directory)? != platform::identity(&self.directory)? {
            return Err(io::Error::other(
                "recording storage lease belongs to a different directory",
            ));
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn lease_excludes_other_operations_and_detects_replacement() {
        let root = tempfile::tempdir().unwrap();
        festerm_unix_security::make_private_directory(&File::open(root.path()).unwrap()).unwrap();
        let lease = RecordingLease::acquire(root.path()).unwrap();
        assert_eq!(
            RecordingLease::acquire(root.path()).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        std::fs::rename(root.path().join(LEASE_NAME), root.path().join("old-lock")).unwrap();
        assert!(RecordingLease::acquire(root.path()).is_ok());
        assert!(lease.verify().is_err());
    }
}
