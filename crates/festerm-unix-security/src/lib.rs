//! Handle-bound Unix file-security metadata and atomic publication.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(unix)]
mod imp {
    use std::{fs::File, io, path::Path};

    #[cfg(target_os = "macos")]
    pub fn preserve_security_metadata(original: &File, temporary: &File) -> io::Result<()> {
        use std::os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, PermissionsExt},
        };

        let copied = unsafe {
            nix::libc::fcopyfile(
                original.as_raw_fd(),
                temporary.as_raw_fd(),
                std::ptr::null_mut(),
                nix::libc::COPYFILE_SECURITY,
            )
        };
        if copied != 0 {
            return Err(io::Error::last_os_error());
        }
        let expected = original.metadata()?;
        let actual = temporary.metadata()?;
        if expected.uid() != actual.uid()
            || expected.gid() != actual.gid()
            || expected.permissions().mode() != actual.permissions().mode()
        {
            return Err(io::Error::other(
                "security metadata changed while it was copied",
            ));
        }
        temporary.sync_all()
    }

    #[cfg(target_os = "linux")]
    pub fn preserve_security_metadata(original: &File, temporary: &File) -> io::Result<()> {
        use nix::{
            sys::stat::{fchmod, Mode},
            unistd::{fchown, Gid, Uid},
        };
        use std::{ffi::OsString, os::unix::fs::MetadataExt};
        use xattr::FileExt;

        let expected = original.metadata()?;
        let actual = temporary.metadata()?;
        if expected.uid() != actual.uid() || expected.gid() != actual.gid() {
            fchown(
                temporary,
                Some(Uid::from_raw(expected.uid())),
                Some(Gid::from_raw(expected.gid())),
            )
            .map_err(io::Error::from)?;
        }

        let inherited = temporary.list_xattr()?.collect::<Vec<_>>();
        for name in inherited {
            temporary.remove_xattr(&name)?;
        }
        let attributes = original
            .list_xattr()?
            .map(|name| original.get_xattr(&name).map(|value| (name, value)))
            .collect::<io::Result<Vec<(OsString, Option<Vec<u8>>)>>>()?;
        for (name, value) in &attributes {
            let value = value
                .as_deref()
                .ok_or_else(|| io::Error::other("security attribute disappeared"))?;
            temporary.set_xattr(name, value)?;
        }
        fchmod(
            temporary,
            Mode::from_bits_truncate(expected.mode() as nix::libc::mode_t),
        )
        .map_err(io::Error::from)?;

        let actual = temporary.metadata()?;
        if expected.uid() != actual.uid()
            || expected.gid() != actual.gid()
            || (expected.mode() & 0o7777) != (actual.mode() & 0o7777)
        {
            return Err(io::Error::other(
                "security metadata changed while it was copied",
            ));
        }
        let actual_attributes = temporary
            .list_xattr()?
            .map(|name| temporary.get_xattr(&name).map(|value| (name, value)))
            .collect::<io::Result<Vec<(OsString, Option<Vec<u8>>)>>>()?;
        if attributes.len() != actual_attributes.len()
            || attributes
                .iter()
                .any(|attribute| !actual_attributes.contains(attribute))
        {
            return Err(io::Error::other(
                "extended attributes changed while they were copied",
            ));
        }
        temporary.sync_all()
    }

    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    pub fn preserve_security_metadata(_original: &File, _temporary: &File) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "exact security metadata preservation is unavailable on this platform",
        ))
    }

    #[cfg(target_os = "linux")]
    pub fn atomic_exchange(directory: &File, first: &Path, second: &Path) -> io::Result<()> {
        use nix::fcntl::{renameat2, RenameFlags};

        renameat2(
            directory,
            first,
            directory,
            second,
            RenameFlags::RENAME_EXCHANGE,
        )
        .map_err(io::Error::from)
    }

    #[cfg(target_os = "linux")]
    pub fn rename_noreplace(directory: &File, source: &Path, target: &Path) -> io::Result<()> {
        use nix::fcntl::{renameat2, RenameFlags};

        renameat2(
            directory,
            source,
            directory,
            target,
            RenameFlags::RENAME_NOREPLACE,
        )
        .map_err(io::Error::from)
    }

    #[cfg(target_os = "macos")]
    pub fn atomic_exchange(directory: &File, first: &Path, second: &Path) -> io::Result<()> {
        renameatx(directory, first, second, nix::libc::RENAME_SWAP)
    }

    #[cfg(target_os = "macos")]
    pub fn rename_noreplace(directory: &File, source: &Path, target: &Path) -> io::Result<()> {
        renameatx(directory, source, target, nix::libc::RENAME_EXCL)
    }

    #[cfg(target_os = "macos")]
    fn renameatx(directory: &File, source: &Path, target: &Path, flags: u32) -> io::Result<()> {
        use std::{
            ffi::CString,
            os::{fd::AsRawFd, unix::ffi::OsStrExt},
        };

        let source = CString::new(source.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let target = CString::new(target.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let renamed = unsafe {
            nix::libc::renameatx_np(
                directory.as_raw_fd(),
                source.as_ptr(),
                directory.as_raw_fd(),
                target.as_ptr(),
                flags,
            )
        };
        if renamed == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    pub fn atomic_exchange(_directory: &File, _first: &Path, _second: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic file exchange is unavailable on this platform",
        ))
    }

    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    pub fn rename_noreplace(_directory: &File, _source: &Path, _target: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace rename is unavailable on this platform",
        ))
    }
}

#[cfg(unix)]
pub use imp::{atomic_exchange, preserve_security_metadata, rename_noreplace};

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::{self, File, OpenOptions},
        path::{Path, PathBuf},
        process,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn new() -> Self {
            let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("festerm-unix-security-{}-{id}", process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn open(&self) -> File {
            File::open(&self.0).unwrap()
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn exchange_and_noreplace_are_atomic_within_one_directory() {
        let directory = TemporaryDirectory::new();
        fs::write(directory.path("first"), b"first").unwrap();
        fs::write(directory.path("second"), b"second").unwrap();
        let handle = directory.open();

        atomic_exchange(&handle, Path::new("first"), Path::new("second")).unwrap();
        assert_eq!(fs::read(directory.path("first")).unwrap(), b"second");
        assert_eq!(fs::read(directory.path("second")).unwrap(), b"first");

        fs::write(directory.path("third"), b"third").unwrap();
        rename_noreplace(&handle, Path::new("third"), Path::new("fourth")).unwrap();
        assert_eq!(fs::read(directory.path("fourth")).unwrap(), b"third");
        fs::write(directory.path("third"), b"replacement").unwrap();
        assert_eq!(
            rename_noreplace(&handle, Path::new("third"), Path::new("fourth"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(directory.path("fourth")).unwrap(), b"third");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_security_copy_preserves_owner_mode_and_posix_acl() {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        use xattr::FileExt;

        let directory = TemporaryDirectory::new();
        let original_path = directory.path("original");
        let temporary_path = directory.path("temporary");
        fs::write(&original_path, b"original").unwrap();
        fs::set_permissions(&original_path, fs::Permissions::from_mode(0o640)).unwrap();
        let original = File::open(&original_path).unwrap();
        original
            .set_xattr("user.festerm-security-test", b"kept")
            .unwrap();
        original
            .set_xattr("system.posix_acl_access", &named_user_acl(65_534))
            .unwrap();
        let temporary = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary_path)
            .unwrap();

        preserve_security_metadata(&original, &temporary).unwrap();

        let expected = original.metadata().unwrap();
        let actual = temporary.metadata().unwrap();
        assert_eq!(actual.uid(), expected.uid());
        assert_eq!(actual.gid(), expected.gid());
        assert_eq!(actual.mode() & 0o7777, expected.mode() & 0o7777);
        assert_eq!(
            temporary
                .get_xattr("system.posix_acl_access")
                .unwrap()
                .unwrap(),
            original
                .get_xattr("system.posix_acl_access")
                .unwrap()
                .unwrap()
        );
        assert_eq!(
            temporary
                .get_xattr("user.festerm-security-test")
                .unwrap()
                .as_deref(),
            Some(b"kept".as_slice())
        );
    }

    #[cfg(target_os = "linux")]
    fn named_user_acl(uid: u32) -> Vec<u8> {
        const ACL_USER_OBJ: u16 = 0x01;
        const ACL_USER: u16 = 0x02;
        const ACL_GROUP_OBJ: u16 = 0x04;
        const ACL_MASK: u16 = 0x10;
        const ACL_OTHER: u16 = 0x20;

        let mut acl = 2u32.to_ne_bytes().to_vec();
        for (tag, permissions, id) in [
            (ACL_USER_OBJ, 0o6u16, u32::MAX),
            (ACL_USER, 0o4u16, uid),
            (ACL_GROUP_OBJ, 0o0u16, u32::MAX),
            (ACL_MASK, 0o4u16, u32::MAX),
            (ACL_OTHER, 0o0u16, u32::MAX),
        ] {
            acl.extend_from_slice(&tag.to_ne_bytes());
            acl.extend_from_slice(&permissions.to_ne_bytes());
            acl.extend_from_slice(&id.to_ne_bytes());
        }
        acl
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_security_copy_preserves_extended_acl() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        use std::process::Command;

        let directory = TemporaryDirectory::new();
        let original_path = directory.path("original");
        let temporary_path = directory.path("temporary");
        fs::write(&original_path, b"original").unwrap();
        fs::set_permissions(&original_path, fs::Permissions::from_mode(0o640)).unwrap();
        let status = Command::new("chmod")
            .args(["+a", "everyone deny write"])
            .arg(&original_path)
            .status()
            .unwrap();
        assert!(status.success());
        let original = File::open(&original_path).unwrap();
        let temporary = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary_path)
            .unwrap();

        preserve_security_metadata(&original, &temporary).unwrap();

        assert_eq!(acl_lines(&original_path), acl_lines(&temporary_path));
    }

    #[cfg(target_os = "macos")]
    fn acl_lines(path: &Path) -> Vec<String> {
        use std::process::Command;

        let output = Command::new("ls").arg("-le").arg(path).output().unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .skip(1)
            .map(str::trim)
            .map(str::to_owned)
            .collect()
    }
}
