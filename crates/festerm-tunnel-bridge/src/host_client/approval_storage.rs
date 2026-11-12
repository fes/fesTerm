//! Portable private receipt metadata; signing material stays in the native vault.
#![cfg(unix)]
#[cfg(test)]
pub(super) use festerm_sessiond::private_storage::{
    private_test_directory_in, PrivateTestDirectory,
};
use festerm_unix_security as unix;
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Component, Path},
};

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "untrusted owner receipt storage",
    )
}

pub(super) fn trusted_directory(path: &Path, _create: bool) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(denied());
    }
    let mut directory = File::open("/")?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = unix::open_directory_read_nofollow(&directory, Path::new(name))?;
                unix::trusted_private_storage_ancestor(&directory)?;
            }
            _ => return Err(denied()),
        }
    }
    Ok(directory)
}

pub(super) fn private_directory(path: &Path, create: bool) -> io::Result<File> {
    let parent = trusted_directory(path.parent().ok_or_else(denied)?, false)?;
    let name = Path::new(path.file_name().ok_or_else(denied)?);
    let directory = match unix::open_directory_read_nofollow(&parent, name) {
        Ok(directory) => directory,
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            unix::create_private_directory(&parent, name)?
        }
        Err(error) => return Err(error),
    };
    if !unix::is_current_user_only(&directory, true)? {
        return Err(denied());
    }
    Ok(directory)
}

fn private_file(directory: &File, name: &Path, create: bool) -> io::Result<File> {
    let file = unix::open_private_file(directory, name, create)?;
    if !unix::is_current_user_only(&file, false)? {
        return Err(denied());
    }
    Ok(file)
}

pub(super) fn open_trusted_lock(directory: &File, name: &Path) -> io::Result<File> {
    match private_file(directory, name, false) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match private_file(directory, name, true) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    private_file(directory, name, false)
                }
                result => result,
            }
        }
        result => result,
    }
}

pub(super) fn read_private(directory: &File, name: &Path, max: usize) -> io::Result<Vec<u8>> {
    let file = private_file(directory, name, false)?;
    let mut bytes = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(denied());
    }
    Ok(bytes)
}

pub(super) fn publish_new(directory: &File, name: &Path, bytes: &[u8]) -> io::Result<()> {
    publish_new_with(directory, name, bytes, |file, bytes| file.write_all(bytes))
}

fn publish_new_with(
    directory: &File,
    name: &Path,
    bytes: &[u8],
    write: impl FnOnce(&mut File, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    match private_file(directory, name, false) {
        Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let staging = format!(".stage-{}", festerm_host_protocol::OperationId::new());
    let staging = Path::new(&staging);
    let mut file = private_file(directory, staging, true)?;
    let result = (|| {
        write(&mut file, bytes)?;
        file.sync_all()?;
        unix::rename_noreplace(directory, staging, directory, name)?;
        directory.sync_all()
    })();
    if result.is_err() {
        match unix::remove_private_staging_file(directory, staging) {
            Ok(()) => directory.sync_all()?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    result
}

#[cfg(test)]
pub(super) fn write_new_private(directory: &File, name: &Path, bytes: &[u8]) -> io::Result<()> {
    publish_new(directory, name, bytes)
}

pub(super) fn replace_private(directory: &File, name: &Path, bytes: &[u8]) -> io::Result<()> {
    private_file(directory, name, false)?;
    let temporary = format!("{}.pending", festerm_host_protocol::OperationId::new());
    let temporary = Path::new(&temporary);
    publish_new(directory, temporary, bytes)?;
    let result = unix::replace_private_file(directory, temporary, name);
    if result.is_err() {
        let _ = unix::remove_private_staging_file(directory, temporary);
    }
    result?;
    directory.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_receipt_interrupted_write_never_publishes_final() {
        let parent = std::env::current_dir().unwrap().join("target");
        std::fs::create_dir_all(&parent).unwrap();
        let root = private_test_directory_in(&parent);
        let directory = private_directory(root.path(), false).unwrap();
        let name = Path::new("receipt.json");
        assert!(
            publish_new_with(&directory, name, b"complete", |file, bytes| {
                file.write_all(&bytes[..3])?;
                Err(io::Error::other("fixture.interrupted_write"))
            })
            .is_err()
        );
        assert_eq!(
            read_private(&directory, name, 32).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        publish_new(&directory, name, b"complete").unwrap();
        assert_eq!(read_private(&directory, name, 32).unwrap(), b"complete");
    }

    #[test]
    fn portable_receipt_racing_final_is_never_overwritten_and_staging_is_removed() {
        let parent = std::env::current_dir().unwrap().join("target");
        std::fs::create_dir_all(&parent).unwrap();
        let root = private_test_directory_in(&parent);
        let directory = private_directory(root.path(), false).unwrap();
        let name = Path::new("receipt.json");
        let error = publish_new_with(&directory, name, b"replacement", |file, bytes| {
            let mut existing = private_file(&directory, name, true)?;
            existing.write_all(b"original")?;
            existing.sync_all()?;
            file.write_all(bytes)
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(read_private(&directory, name, 32).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        assert_eq!(
            publish_new(&directory, name, b"another")
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(read_private(&directory, name, 32).unwrap(), b"original");
    }
}
