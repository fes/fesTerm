//! Native-created authority for the fixed HOST receipt child, not a path grant.
use crate::SecretStoreError;
use festerm_unix_security as unix;
use std::{fs::File, io, os::unix::fs::MetadataExt, path::Path};

/// Pinned current-application storage. No path, descriptor, deserializer or
/// caller-supplied owner can construct this capability.
pub struct NativeAppPrivateRoot {
    directory: File,
    identity: (u64, u64),
}

impl NativeAppPrivateRoot {
    /// Obtains the current native application root, never imported metadata.
    pub fn current() -> Result<Self, SecretStoreError> {
        #[cfg(target_os = "android")]
        {
            let directory = festerm_android_platform::application_private_directory()
                .map_err(|_| SecretStoreError::LockedOrUnavailable)?;
            Self::pin(directory).map_err(|_| SecretStoreError::LockedOrUnavailable)
        }
        #[cfg(not(target_os = "android"))]
        {
            Err(SecretStoreError::Unsupported)
        }
    }

    #[cfg(any(target_os = "android", test))]
    fn pin(directory: File) -> io::Result<Self> {
        validate(&directory)?;
        let metadata = directory.metadata()?;
        Ok(Self {
            identity: (metadata.dev(), metadata.ino()),
            directory,
        })
    }

    fn validate(&self) -> io::Result<()> {
        validate(&self.directory)?;
        let metadata = self.directory.metadata()?;
        if (metadata.dev(), metadata.ino()) != self.identity {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }

    /// Opens only the fixed receipt child. The native application root itself
    /// is never exported; all receipt I/O remains descriptor-relative.
    pub fn open_host_approval_directory(&self, create: bool) -> io::Result<File> {
        self.validate()?;
        let name = Path::new("host-client-approvals");
        let child = match unix::open_directory_read_nofollow(&self.directory, name) {
            Ok(child) => child,
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                match unix::create_private_directory(&self.directory, name) {
                    Ok(child) => child,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        unix::open_directory_read_nofollow(&self.directory, name)?
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        validate(&child)?;
        if create {
            self.directory.sync_all()?;
        }
        Ok(child)
    }

    /// Compares pinned native namespace identity, never an ordinary path.
    pub fn same_root(&self, other: &Self) -> bool {
        self.identity == other.identity && self.validate().is_ok() && other.validate().is_ok()
    }
}

fn validate(directory: &File) -> io::Result<()> {
    if directory.metadata()?.nlink() == 0 || !unix::is_current_user_only(directory, true)? {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::current_dir()
                .unwrap()
                .join("target/native-root-tests")
                .join(uuid::Uuid::new_v4().to_string());
            fs::create_dir_all(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
        fn pin(&self) -> NativeAppPrivateRoot {
            NativeAppPrivateRoot::pin(File::open(&self.0).unwrap()).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn native_root_current_owner_and_fixed_child_reopen_without_reset() {
        let fixture = Fixture::new();
        let root = fixture.pin();
        assert_eq!(
            root.open_host_approval_directory(false).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        root.open_host_approval_directory(true).unwrap();
        let path = fixture.0.join("host-client-approvals/issued");
        fs::write(&path, b"retained original").unwrap();
        fixture.pin().open_host_approval_directory(true).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"retained original");
        assert!(root.same_root(&fixture.pin()));
        assert!(!root.same_root(&Fixture::new().pin()));
    }

    #[test]
    fn native_root_rejects_nonprivate_root_child_and_symlink_without_repair() {
        let fixture = Fixture::new();
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(NativeAppPrivateRoot::pin(File::open(&fixture.0).unwrap()).is_err());
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
        let root = fixture.pin();
        let child = fixture.0.join("host-client-approvals");
        fs::create_dir(&child).unwrap();
        fs::set_permissions(&child, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(root.open_host_approval_directory(true).is_err());
        fs::remove_dir(&child).unwrap();
        let other = Fixture::new();
        std::os::unix::fs::symlink(&other.0, &child).unwrap();
        assert!(root.open_host_approval_directory(true).is_err());
        assert!(fs::symlink_metadata(child).unwrap().is_symlink());
    }

    #[test]
    fn native_root_revoked_permissions_and_unlinked_root_fail_closed() {
        let fixture = Fixture::new();
        let root = fixture.pin();
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o770)).unwrap();
        assert!(root.open_host_approval_directory(true).is_err());
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir(&fixture.0).unwrap();
        assert!(root.open_host_approval_directory(true).is_err());
        fs::create_dir(&fixture.0).unwrap();
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!root.same_root(&fixture.pin()));
    }
}
