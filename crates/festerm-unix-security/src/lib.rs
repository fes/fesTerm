//! Handle-bound Unix file-security metadata and atomic publication.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(unix)]
mod imp {
    use std::{ffi::OsString, fs::File, io, os::unix::fs::MetadataExt, path::Path};
    use xattr::FileExt;

    const MAX_SECURITY_ATTRIBUTE_BYTES: usize = 8 * 1024 * 1024;

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct SecurityMetadata {
        uid: u32,
        gid: u32,
        mode: u32,
        attributes: Vec<(OsString, Vec<u8>)>,
        #[cfg(target_os = "macos")]
        acl: Vec<u8>,
    }

    pub fn security_metadata(file: &File) -> io::Result<SecurityMetadata> {
        let metadata = file.metadata()?;
        let mut total = 0usize;
        let mut attributes = file
            .list_xattr()?
            .map(|name| {
                let value = file
                    .get_xattr(&name)?
                    .ok_or_else(|| io::Error::other("security attribute disappeared"))?;
                total = total
                    .checked_add(name.len())
                    .and_then(|total| total.checked_add(value.len()))
                    .ok_or_else(|| io::Error::other("security metadata is too large"))?;
                if total > MAX_SECURITY_ATTRIBUTE_BYTES {
                    return Err(io::Error::other("security metadata is too large"));
                }
                Ok((name, value))
            })
            .collect::<io::Result<Vec<_>>>()?;
        attributes.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(SecurityMetadata {
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode() & 0o7777,
            attributes,
            #[cfg(target_os = "macos")]
            acl: macos_acl(file)?,
        })
    }

    pub fn security_metadata_matches(file: &File, expected: &SecurityMetadata) -> io::Result<bool> {
        Ok(&security_metadata(file)? == expected)
    }

    pub fn make_private(file: &File) -> io::Result<()> {
        use nix::sys::stat::{fchmod, Mode};

        #[cfg(target_os = "linux")]
        if file
            .list_xattr()?
            .any(|name| name == "system.posix_acl_access")
        {
            file.remove_xattr("system.posix_acl_access")?;
        }
        #[cfg(target_os = "macos")]
        clear_macos_acl(file)?;
        fchmod(file, Mode::from_bits_truncate(0o600)).map_err(io::Error::from)?;
        let private = security_metadata(file)?;
        if private.mode != 0o600 || {
            #[cfg(target_os = "macos")]
            {
                !private.acl.is_empty()
            }
            #[cfg(not(target_os = "macos"))]
            {
                false
            }
        } {
            return Err(io::Error::other(format!(
                "could not make recovery file private: {private:?}"
            )));
        }
        file.sync_all()
    }

    pub fn make_private_directory(directory: &File) -> io::Result<()> {
        use nix::sys::stat::{fchmod, Mode};

        #[cfg(target_os = "linux")]
        for attribute in ["system.posix_acl_access", "system.posix_acl_default"] {
            if directory.list_xattr()?.any(|name| name == attribute) {
                directory.remove_xattr(attribute)?;
            }
        }
        #[cfg(target_os = "macos")]
        clear_macos_acl(directory)?;
        fchmod(directory, Mode::from_bits_truncate(0o700)).map_err(io::Error::from)?;
        let private = security_metadata(directory)?;
        let inherited_acl = {
            #[cfg(target_os = "linux")]
            {
                private.attributes.iter().any(|(name, _)| {
                    name == "system.posix_acl_access" || name == "system.posix_acl_default"
                })
            }
            #[cfg(target_os = "macos")]
            {
                !private.acl.is_empty()
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                false
            }
        };
        if private.mode != 0o700 || inherited_acl {
            return Err(io::Error::other(format!(
                "could not make save staging directory private: {private:?}"
            )));
        }
        directory.sync_all()
    }

    #[cfg(target_os = "macos")]
    pub fn preserve_security_metadata(
        original: &File,
        temporary: &File,
    ) -> io::Result<SecurityMetadata> {
        use std::os::{fd::AsRawFd, unix::fs::PermissionsExt};

        let expected = security_metadata(original)?;
        let copied = unsafe {
            nix::libc::fcopyfile(
                original.as_raw_fd(),
                temporary.as_raw_fd(),
                std::ptr::null_mut(),
                nix::libc::COPYFILE_METADATA,
            )
        };
        if copied != 0 {
            return Err(io::Error::last_os_error());
        }
        let actual = temporary.metadata()?;
        if expected.uid != actual.uid()
            || expected.gid != actual.gid()
            || expected.mode != actual.permissions().mode() & 0o7777
            || !security_metadata_matches(original, &expected)?
            || !security_metadata_matches(temporary, &expected)?
        {
            return Err(io::Error::other(
                "security metadata changed while it was copied",
            ));
        }
        temporary.sync_all()?;
        Ok(expected)
    }

    #[cfg(target_os = "linux")]
    pub fn preserve_security_metadata(
        original: &File,
        temporary: &File,
    ) -> io::Result<SecurityMetadata> {
        use nix::{
            sys::stat::{fchmod, Mode},
            unistd::{fchown, Gid, Uid},
        };

        let expected = security_metadata(original)?;
        let actual = temporary.metadata()?;
        if expected.uid != actual.uid() || expected.gid != actual.gid() {
            fchown(
                temporary,
                Some(Uid::from_raw(expected.uid)),
                Some(Gid::from_raw(expected.gid)),
            )
            .map_err(io::Error::from)?;
        }

        let inherited = temporary.list_xattr()?.collect::<Vec<_>>();
        for name in inherited {
            temporary.remove_xattr(&name)?;
        }
        for (name, value) in &expected.attributes {
            temporary.set_xattr(name, value)?;
        }
        fchmod(
            temporary,
            Mode::from_bits_truncate(expected.mode as nix::libc::mode_t),
        )
        .map_err(io::Error::from)?;

        if !security_metadata_matches(original, &expected)?
            || !security_metadata_matches(temporary, &expected)?
        {
            return Err(io::Error::other(
                "security metadata changed while it was copied",
            ));
        }
        temporary.sync_all()?;
        Ok(expected)
    }

    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    pub fn preserve_security_metadata(
        _original: &File,
        _temporary: &File,
    ) -> io::Result<SecurityMetadata> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "exact security metadata preservation is unavailable on this platform",
        ))
    }

    #[cfg(target_os = "macos")]
    fn macos_acl(file: &File) -> io::Result<Vec<u8>> {
        use std::{
            ffi::c_void,
            os::fd::{AsRawFd, RawFd},
        };

        type Acl = *mut c_void;
        const ACL_TYPE_EXTENDED: i32 = 0x0000_0100;
        unsafe extern "C" {
            fn acl_get_fd_np(fd: RawFd, acl_type: i32) -> Acl;
            fn acl_get_entry(acl: Acl, entry_id: i32, entry: *mut *mut c_void) -> i32;
            fn acl_size(acl: Acl) -> isize;
            fn acl_copy_ext(buffer: *mut c_void, acl: Acl, size: isize) -> isize;
            fn acl_free(object: *mut c_void) -> i32;
        }

        let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
        if acl.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(nix::libc::ENOENT) {
                return Ok(Vec::new());
            }
            return Err(error);
        }
        let mut entry = std::ptr::null_mut();
        let has_entry = unsafe { acl_get_entry(acl, 0, &raw mut entry) };
        if has_entry <= 0 {
            let freed = unsafe { acl_free(acl) };
            if has_entry == 0 && freed == 0 {
                return Ok(Vec::new());
            }
            return Err(io::Error::last_os_error());
        }
        let size = unsafe { acl_size(acl) };
        if size < 0 || size as usize > MAX_SECURITY_ATTRIBUTE_BYTES {
            let _ = unsafe { acl_free(acl) };
            return Err(io::Error::other("ACL metadata is too large"));
        }
        let mut bytes = vec![0u8; size as usize];
        let copied = unsafe { acl_copy_ext(bytes.as_mut_ptr().cast(), acl, size) };
        let freed = unsafe { acl_free(acl) };
        if copied < 0 || freed != 0 {
            return Err(io::Error::last_os_error());
        }
        bytes.truncate(copied as usize);
        Ok(bytes)
    }

    #[cfg(target_os = "macos")]
    fn clear_macos_acl(file: &File) -> io::Result<()> {
        use std::{
            ffi::c_void,
            os::fd::{AsRawFd, RawFd},
        };

        type Acl = *mut c_void;
        const ACL_TYPE_EXTENDED: i32 = 0x0000_0100;
        unsafe extern "C" {
            fn acl_init(count: i32) -> Acl;
            fn acl_set_fd_np(fd: RawFd, acl: Acl, acl_type: i32) -> i32;
            fn acl_free(object: *mut c_void) -> i32;
        }

        let acl = unsafe { acl_init(0) };
        if acl.is_null() {
            return Err(io::Error::last_os_error());
        }
        let set = unsafe { acl_set_fd_np(file.as_raw_fd(), acl, ACL_TYPE_EXTENDED) };
        let freed = unsafe { acl_free(acl) };
        if set != 0 || freed != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    pub fn rename_noreplace(
        source_directory: &File,
        source: &Path,
        target_directory: &File,
        target: &Path,
    ) -> io::Result<()> {
        use nix::fcntl::{renameat2, RenameFlags};

        renameat2(
            source_directory,
            source,
            target_directory,
            target,
            RenameFlags::RENAME_NOREPLACE,
        )
        .map_err(io::Error::from)
    }

    #[cfg(target_os = "macos")]
    pub fn rename_noreplace(
        source_directory: &File,
        source: &Path,
        target_directory: &File,
        target: &Path,
    ) -> io::Result<()> {
        renameatx(
            source_directory,
            source,
            target_directory,
            target,
            nix::libc::RENAME_EXCL,
        )
    }

    #[cfg(target_os = "macos")]
    fn renameatx(
        source_directory: &File,
        source: &Path,
        target_directory: &File,
        target: &Path,
        flags: u32,
    ) -> io::Result<()> {
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
                source_directory.as_raw_fd(),
                source.as_ptr(),
                target_directory.as_raw_fd(),
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
    pub fn rename_noreplace(
        _source_directory: &File,
        _source: &Path,
        _target_directory: &File,
        _target: &Path,
    ) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace rename is unavailable on this platform",
        ))
    }
}

#[cfg(unix)]
pub use imp::{
    make_private, make_private_directory, preserve_security_metadata, rename_noreplace,
    security_metadata, security_metadata_matches, SecurityMetadata,
};

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
    fn noreplace_is_atomic_within_one_directory() {
        let directory = TemporaryDirectory::new();
        let handle = directory.open();

        fs::write(directory.path("third"), b"third").unwrap();
        rename_noreplace(&handle, Path::new("third"), &handle, Path::new("fourth")).unwrap();
        assert_eq!(fs::read(directory.path("fourth")).unwrap(), b"third");
        fs::write(directory.path("third"), b"replacement").unwrap();
        assert_eq!(
            rename_noreplace(&handle, Path::new("third"), &handle, Path::new("fourth"),)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(directory.path("fourth")).unwrap(), b"third");
    }

    #[test]
    fn security_snapshot_detects_xattr_changes_and_private_reset() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        use xattr::FileExt;

        let directory = TemporaryDirectory::new();
        let path = directory.path("metadata");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&path)
            .unwrap();
        file.set_xattr("user.festerm-security-test", b"before")
            .unwrap();
        let snapshot = security_metadata(&file).unwrap();

        file.set_xattr("user.festerm-security-test", b"after")
            .unwrap();

        assert!(!security_metadata_matches(&file, &snapshot).unwrap());
        make_private(&file).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn private_directory_clears_inherited_access() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TemporaryDirectory::new();
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o777)).unwrap();
        let handle = directory.open();
        #[cfg(target_os = "linux")]
        {
            use xattr::FileExt;
            handle
                .set_xattr("system.posix_acl_access", &named_user_acl(65_534))
                .unwrap();
            handle
                .set_xattr("system.posix_acl_default", &named_user_acl(65_534))
                .unwrap();
        }
        #[cfg(target_os = "macos")]
        {
            use std::process::Command;
            let status = Command::new("chmod")
                .args([
                    "+a",
                    "everyone allow read,write,execute,file_inherit,directory_inherit",
                ])
                .arg(&directory.0)
                .status()
                .unwrap();
            assert!(status.success());
        }

        make_private_directory(&handle).unwrap();

        assert_eq!(
            handle.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
        #[cfg(target_os = "linux")]
        {
            use xattr::FileExt;
            assert!(handle
                .get_xattr("system.posix_acl_access")
                .unwrap()
                .is_none());
            assert!(handle
                .get_xattr("system.posix_acl_default")
                .unwrap()
                .is_none());
        }
        #[cfg(target_os = "macos")]
        assert!(acl_lines(&directory.0).is_empty());
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
