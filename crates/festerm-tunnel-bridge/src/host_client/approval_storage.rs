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
    let mut file = private_file(directory, name, true)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    directory.sync_all()
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
