//! Owner-only, handle-relative storage for the explicitly managed host.
pub use festerm_sessiond::private_storage::*;

use std::{
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Publishes complete private bytes without replacing an existing entry.
/// Only this call's staging entry may be removed on failure.
pub fn publish_new_private(directory: &File, name: &Path, bytes: &[u8]) -> io::Result<()> {
    publish_new_private_with(directory, name, bytes, |file, bytes| file.write_all(bytes))
}

fn publish_new_private_with(
    directory: &File,
    name: &Path,
    bytes: &[u8],
    write: impl FnOnce(&mut File, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    match open_private_file(directory, name, false) {
        Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let staging = PathBuf::from(format!(".stage-{}", uuid::Uuid::new_v4()));
    let mut output = open_private_file(directory, &staging, true)?;
    let result = (|| {
        write(&mut output, bytes)?;
        output.sync_all()?;
        #[cfg(unix)]
        {
            festerm_unix_security::rename_noreplace(directory, &staging, directory, name)?;
            directory.sync_all()?;
        }
        #[cfg(windows)]
        festerm_windows_security::rename_file_noreplace(&output, directory, name)?;
        Ok(())
    })();
    if result.is_err() {
        #[cfg(unix)]
        let _ = festerm_unix_security::remove_private_staging_file(directory, &staging);
        #[cfg(windows)]
        let _ = festerm_windows_security::remove_private_staging_file(&output);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_private_publication_never_exposes_torn_final_bytes() {
        let root = private_test_directory();
        for name in ["host-lifecycle.json", "devtunnel", "credential"] {
            let path = root.path().join(format!("{name}-stage"));
            let directory = private_directory(&path, true).unwrap();
            let name = Path::new(name);
            assert!(
                publish_new_private_with(&directory, name, b"complete", |file, bytes| {
                    file.write_all(&bytes[..3])?;
                    Err(io::Error::other("fixture.interrupted_write"))
                })
                .is_err()
            );
            assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
            publish_new_private(&directory, name, b"complete").unwrap();
            assert!(publish_new_private(&directory, name, b"replacement").is_err());
            assert_eq!(
                read_private(&directory, name, 32).unwrap().as_slice(),
                b"complete"
            );
        }
    }
}
