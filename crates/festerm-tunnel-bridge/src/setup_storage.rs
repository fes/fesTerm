//! Owner-only, handle-relative storage for the explicitly managed host.
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
        festerm_windows_security::secure_staging_parent(directory)?
            .map(|_| ())
            .ok_or_else(|| io::Error::from(io::ErrorKind::PermissionDenied))
    }
}

/// Opens an absolute owner-only directory without following any symlink/reparse
/// component. Only the final directory may be created; its parent must exist.
/// Existing permissions are verified, never repaired.
pub fn private_directory(path: &Path, create: bool) -> io::Result<File> {
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
    private(&directory, true)?;
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
    file_name(name)?;
    private(directory, true)?;
    match open_private_file(directory, name, false) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let staging = std::path::PathBuf::from(format!(".checkpoint-{}", uuid::Uuid::new_v4()));
    let mut file = open_private_file(directory, &staging, true)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        // Recheck immediately before publication; don't replace a symlink or
        // foreign/permissive entry that appeared during the write.
        match open_private_file(directory, name, false) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        #[cfg(unix)]
        festerm_unix_security::replace_private_file(directory, &staging, name)?;
        #[cfg(windows)]
        festerm_windows_security::replace_private_file(&file, directory, name)?;
        #[cfg(unix)]
        directory.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        #[cfg(unix)]
        let _ = festerm_unix_security::remove_private_staging_file(directory, &staging);
        #[cfg(windows)]
        let _ = festerm_windows_security::remove_private_staging_file(&file);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_state_creation_and_replacement_are_bounded_and_exclusive() {
        #[cfg(windows)]
        let parent = std::env::temp_dir();
        #[cfg(not(windows))]
        let parent = std::env::current_dir().unwrap();
        let path = parent.join(format!(".bridge-storage-test-{}", uuid::Uuid::new_v4()));
        let directory = private_directory(&path, true).unwrap();
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
