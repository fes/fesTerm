//! Owner-only, handle-relative storage shared by daemon diagnostics and hosting.
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Component, Path},
};
use zeroize::Zeroizing;

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "private storage refused")
}

fn private(file: &File, directory: bool) -> io::Result<()> {
    #[cfg(unix)]
    let safe = festerm_unix_security::is_current_user_only(file, directory)?;
    #[cfg(windows)]
    let safe = file.metadata()?.is_dir() == directory
        && (directory || file.metadata()?.is_file())
        && festerm_windows_security::is_current_user_only(file)?;
    if safe {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private storage permissions refused",
        ))
    }
}

fn ancestor(directory: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        festerm_unix_security::trusted_private_storage_ancestor(directory)
    }
    #[cfg(windows)]
    {
        festerm_windows_security::trusted_private_storage_ancestor(directory)
    }
}

/// Opens an absolute owner-only directory without following any symlink/reparse
/// component. Only the final directory may be created; its parent must exist.
/// Existing permissions are verified, never repaired.
pub fn private_directory(path: &Path, create: bool) -> io::Result<File> {
    open_directory(path, create, true)
}

/// Opens a directory whose entries cannot be replaced by an untrusted user.
/// Existing permissions are verified, never repaired.
pub fn trusted_directory(path: &Path, create: bool) -> io::Result<File> {
    open_directory(path, create, false)
}

fn open_directory(path: &Path, create: bool, require_private: bool) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(invalid());
    }
    let mut parts = path.components();
    #[cfg(windows)]
    let mut parent = {
        let Some(Component::Prefix(prefix)) = parts.next() else {
            return Err(invalid());
        };
        if !matches!(prefix.kind(), std::path::Prefix::Disk(_))
            || parts.next() != Some(Component::RootDir)
        {
            return Err(invalid());
        }
        let mut root = prefix.as_os_str().to_os_string();
        root.push("\\");
        let root = std::path::PathBuf::from(root);
        festerm_windows_security::require_local_disk_root(&root)?;
        festerm_windows_security::lock_directory_path_without_delete_sharing(&root)?
    };
    #[cfg(unix)]
    let mut parent = {
        if parts.next() != Some(Component::RootDir) {
            return Err(invalid());
        }
        File::open("/")?
    };
    let names = parts
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            _ => Err(invalid()),
        })
        .collect::<io::Result<Vec<_>>>()?;
    let (name, ancestors) = names.split_last().ok_or_else(invalid)?;
    for name in ancestors {
        ancestor(&parent)?;
        #[cfg(unix)]
        {
            parent = festerm_unix_security::open_directory_read_nofollow(&parent, Path::new(name))?;
        }
        #[cfg(windows)]
        {
            parent = festerm_windows_security::open_directory_no_reparse(&parent, Path::new(name))?;
        }
    }
    ancestor(&parent)?;
    let name = Path::new(name);
    #[cfg(unix)]
    let opened = festerm_unix_security::open_directory_read_nofollow(&parent, name);
    #[cfg(windows)]
    let opened = festerm_windows_security::open_directory_no_reparse(&parent, name);
    let directory = match opened {
        Ok(directory) => directory,
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            #[cfg(unix)]
            let created = festerm_unix_security::create_private_directory(&parent, name);
            #[cfg(windows)]
            let created =
                festerm_windows_security::create_current_user_only_directory(&parent, name);
            match created {
                Ok(directory) => directory,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    #[cfg(unix)]
                    let winner =
                        festerm_unix_security::open_directory_read_nofollow(&parent, name)?;
                    #[cfg(windows)]
                    let winner =
                        festerm_windows_security::open_directory_no_reparse(&parent, name)?;
                    winner
                }
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    if require_private {
        private(&directory, true)?;
    } else {
        ancestor(&directory)?;
    }
    Ok(directory)
}

fn file_name(name: &Path) -> io::Result<()> {
    let mut parts = name.components();
    if !matches!(parts.next(), Some(Component::Normal(_))) || parts.next().is_some() {
        return Err(invalid());
    }
    #[cfg(windows)]
    {
        let name = name.to_str().ok_or_else(invalid)?;
        if name.contains(':') || name.ends_with(['.', ' ']) {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Opens an existing legacy lock without changing its permissions, or creates
/// a private lock in a verified trusted directory.
pub fn open_trusted_lock(directory: &File, name: &Path) -> io::Result<File> {
    file_name(name)?;
    ancestor(directory)?;
    #[cfg(unix)]
    fn existing(directory: &File, name: &Path) -> io::Result<File> {
        festerm_unix_security::open_file_nofollow(directory, name)
    }
    #[cfg(windows)]
    fn existing(directory: &File, name: &Path) -> io::Result<File> {
        let file =
            festerm_windows_security::open_lock_no_reparse_without_delete_sharing(directory, name)?;
        festerm_windows_security::trusted_legacy_lock_file(&file)?;
        Ok(file)
    }
    match existing(directory, name) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            #[cfg(unix)]
            let created = festerm_unix_security::open_private_file(directory, name, true);
            #[cfg(windows)]
            let created =
                festerm_windows_security::create_current_user_only_file_exclusive(directory, name);
            match created {
                Ok(file) => drop(file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            existing(directory, name)
        }
        Err(error) => Err(error),
    }
}

/// Opens a private regular file read/write, without truncation. `create_new`
/// requests exclusive creation, never opening/replacing an existing entry.
pub fn open_private_file(directory: &File, name: &Path, create_new: bool) -> io::Result<File> {
    file_name(name)?;
    private(directory, true)?;
    #[cfg(unix)]
    let file = festerm_unix_security::open_private_file(directory, name, create_new)?;
    #[cfg(windows)]
    let file = if create_new {
        festerm_windows_security::create_current_user_only_file(directory, name)?
    } else {
        festerm_windows_security::open_file_read_write_no_reparse(directory, name)?
    };
    private(&file, false)?;
    Ok(file)
}

pub fn read_private(directory: &File, name: &Path, max: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut file = open_private_file(directory, name, false)?;
    if file.metadata()?.len() > max as u64 {
        return Err(invalid());
    }
    // Reserve the complete bound up front so reading private material never
    // reallocates and leaves an earlier serialized copy in a freed buffer.
    let capacity = max.checked_add(1).ok_or_else(invalid)?;
    let mut bytes = Zeroizing::new(vec![0; capacity]);
    let mut length = 0;
    while length < capacity {
        match file.read(&mut bytes[length..]) {
            Ok(0) => break,
            Ok(read) => length += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    if length > max {
        return Err(invalid());
    }
    bytes.truncate(length);
    Ok(bytes)
}

/// Removes only an existing private regular file inside the retained directory.
/// The caller must fence every writer and prove the artifact is retired.
pub(crate) fn remove_private_file(directory: &File, name: &Path) -> io::Result<()> {
    let file = match open_private_file(directory, name, false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    #[cfg(unix)]
    {
        let _retained = file;
        festerm_unix_security::remove_private_staging_file(directory, name)?;
        directory.sync_all()
    }
    #[cfg(windows)]
    {
        let deleting = festerm_windows_security::open_file_no_reparse_for_move(directory, name)?;
        if !festerm_windows_security::same_file_identity(&file, &deleting)? {
            return Err(invalid());
        }
        festerm_windows_security::remove_private_staging_file(&deleting)
    }
}

#[cfg(unix)]
pub(crate) fn remove_private_socket(directory: &File, name: &Path) -> io::Result<()> {
    file_name(name)?;
    private(directory, true)?;
    let stat =
        match nix::sys::stat::fstatat(directory, name, nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(nix::errno::Errno::ENOENT) => return Ok(()),
            Err(error) => return Err(io::Error::from(error)),
        };
    if stat.st_uid != nix::unistd::geteuid().as_raw()
        || stat.st_mode & nix::libc::S_IFMT != nix::libc::S_IFSOCK
        || stat.st_mode & 0o077 != 0
    {
        return Err(invalid());
    }
    festerm_unix_security::remove_private_staging_file(directory, name)?;
    directory.sync_all()
}

/// Exclusively creates and synchronizes a private file. A partial failed write
/// is left in place for explicit owner recovery, never silently overwritten.
/// Concurrent readers can fail closed while creation is still in progress.
pub fn write_new_private(directory: &File, name: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = open_private_file(directory, name, true)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    #[cfg(unix)]
    directory.sync_all()?;
    Ok(())
}

/// Atomically checkpoints ordinary private state metadata, never host keys.
/// Existing targets must already be private regular files. Only a fresh
/// caller-owned staging entry is removed if publication fails.
pub fn replace_private(directory: &File, name: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut prepared = PreparedCheckpoint::prepare(directory, name, bytes)?;
    prepared.publish()?;
    prepared.sync()
}

/// Prepare slow durable I/O before taking a publication lock. Publication must
/// be serialized and the caller must revalidate its source immediately before
/// `publish`; `sync` follows after releasing the short publication lock.
pub struct PreparedCheckpoint {
    directory: File,
    name: std::path::PathBuf,
    // Unix publishes/unlinks relative to the directory; Windows uses `file`'s
    // exact retained handle for both operations, never the staging pathname.
    #[cfg(unix)]
    staging: std::path::PathBuf,
    file: File,
    published: bool,
}

impl PreparedCheckpoint {
    pub fn prepare(directory: &File, name: &Path, bytes: &[u8]) -> io::Result<Self> {
        file_name(name)?;
        private(directory, true)?;
        match open_private_file(directory, name, false) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let staging = std::path::PathBuf::from(format!(".checkpoint-{}", uuid::Uuid::new_v4()));
        let owned_directory = directory.try_clone()?;
        let file = open_private_file(directory, &staging, true)?;
        let mut prepared = Self {
            directory: owned_directory,
            name: name.into(),
            #[cfg(unix)]
            staging,
            file,
            published: false,
        };
        prepared.file.write_all(bytes)?;
        prepared.file.sync_all()?;
        Ok(prepared)
    }

    pub fn publish(&mut self) -> io::Result<()> {
        if self.published {
            return Err(io::Error::other("checkpoint already published"));
        }
        match open_private_file(&self.directory, &self.name, false) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        #[cfg(unix)]
        festerm_unix_security::replace_private_file(&self.directory, &self.staging, &self.name)?;
        #[cfg(windows)]
        festerm_windows_security::replace_private_file(&self.file, &self.directory, &self.name)?;
        self.published = true;
        Ok(())
    }

    pub fn sync(self) -> io::Result<()> {
        if !self.published {
            return Err(io::Error::other("checkpoint not published"));
        }
        #[cfg(unix)]
        self.directory.sync_all()?;
        Ok(())
    }
}

impl Drop for PreparedCheckpoint {
    fn drop(&mut self) {
        if !self.published {
            #[cfg(unix)]
            let _ =
                festerm_unix_security::remove_private_staging_file(&self.directory, &self.staging);
            #[cfg(windows)]
            let _ = festerm_windows_security::remove_private_staging_file(&self.file);
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn test_directory() -> std::path::PathBuf {
    #[cfg(windows)]
    let parents = [
        std::env::temp_dir(),
        std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("Temp"),
    ];
    #[cfg(unix)]
    let parents = [std::env::current_dir().unwrap()];
    for (index, parent) in parents.into_iter().enumerate() {
        let path = parent.join(format!("festerm-private-{}", uuid::Uuid::new_v4()));
        match private_directory(&path, true) {
            Ok(directory) => {
                drop(directory);
                return path;
            }
            Err(error) => {
                eprintln!("private fixture candidate {index} refused: {error}");
            }
        }
    }
    panic!("no policy-compliant private fixture directory");
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub struct PrivateTestDirectory(std::path::PathBuf);

#[cfg(any(test, feature = "test-support"))]
impl PrivateTestDirectory {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for PrivateTestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Test-only fixture selection uses the real private-storage policy, never
/// permission repair. Windows checkout/profile ancestors need not be trusted.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn private_test_directory() -> PrivateTestDirectory {
    PrivateTestDirectory(test_directory())
}

/// Keeps Unix socket fixture paths short without changing process-wide cwd.
/// Windows always retains the policy-validated candidate-parent selection.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn private_test_directory_in(unix_parent: &Path) -> PrivateTestDirectory {
    #[cfg(unix)]
    {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let path = unix_parent.join(format!("ft-{}", &nonce[..16]));
        drop(private_directory(&path, true).expect("private fixture directory refused"));
        PrivateTestDirectory(path)
    }
    #[cfg(windows)]
    {
        let _ = unix_parent;
        private_test_directory()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_fixture_short_unix_parent_supports_native_socket_and_owned_cleanup() {
        let root = private_test_directory_in(&std::env::current_dir().unwrap());
        let path = root.path().to_owned();
        let socket = std::os::unix::net::UnixListener::bind(path.join("host.sock")).unwrap();
        drop(socket);
        drop(root);
        assert!(!path.exists());
    }

    #[test]
    fn prepared_checkpoint_abort_and_failed_publication_clean_only_owned_staging() {
        let fixture = private_test_directory();
        let directory = private_directory(fixture.path(), false).unwrap();
        let name = Path::new("state.json");
        write_new_private(&directory, name, b"original").unwrap();
        let prepared = PreparedCheckpoint::prepare(&directory, name, b"aborted").unwrap();
        assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 2);
        drop(prepared);
        assert_eq!(&**read_private(&directory, name, 8).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 1);

        let refused = Path::new("refused");
        let mut prepared =
            PreparedCheckpoint::prepare(&directory, refused, b"unpublished").unwrap();
        std::fs::create_dir(fixture.path().join(refused)).unwrap();
        assert!(prepared.publish().is_err());
        drop(prepared);
        assert!(fixture.path().join(refused).is_dir());
        assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 2);

        let mut prepared = PreparedCheckpoint::prepare(&directory, name, b"published").unwrap();
        prepared.publish().unwrap();
        assert!(prepared.publish().is_err());
        prepared.sync().unwrap();
        assert_eq!(&**read_private(&directory, name, 9).unwrap(), b"published");
        assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 2);
    }

    #[test]
    fn private_state_creation_and_replacement_are_bounded_and_exclusive() {
        let path = test_directory();
        let directory = private_directory(&path, false).unwrap();
        let name = Path::new("state.json");
        write_new_private(&directory, name, b"first").unwrap();
        assert!(write_new_private(&directory, name, b"overwrite").is_err());
        assert!(read_private(&directory, name, 4).is_err());
        assert_eq!(&**read_private(&directory, name, 5).unwrap(), b"first");
        replace_private(&directory, name, b"second").unwrap();
        assert_eq!(&**read_private(&directory, name, 6).unwrap(), b"second");
        replace_private(&directory, Path::new("new.json"), b"new").unwrap();
        for name in ["..", ".", "../escape", "/absolute", "sub/file"] {
            assert!(open_private_file(&directory, Path::new(name), true).is_err());
        }
        let entries = std::fs::read_dir(&path).unwrap().count();
        assert_eq!(entries, 2);
        drop(directory);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_state_refuses_unsafe_parents_and_existing_replacement_targets() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let path = std::env::current_dir()
            .unwrap()
            .join(format!(".bridge-storage-test-{}", uuid::Uuid::new_v4()));
        let directory = private_directory(&path, true).unwrap();
        write_new_private(&directory, Path::new("original"), b"untouched").unwrap();
        symlink(path.join("original"), path.join("link")).unwrap();
        assert!(replace_private(&directory, Path::new("link"), b"replacement").is_err());
        assert_eq!(std::fs::read(path.join("original")).unwrap(), b"untouched");
        std::fs::set_permissions(
            path.join("original"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(replace_private(&directory, Path::new("original"), b"replacement").is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(private_directory(&path.join("child"), true).is_err());
        assert!(!path.join("child").exists());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        drop(directory);
        std::fs::remove_dir_all(path).unwrap();
    }
}
