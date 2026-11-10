use festerm_sessiond::private_storage as storage;
use fs2::FileExt;
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

pub(crate) struct Lease(File);

impl Lease {
    pub(crate) fn exclusive(file: File) -> io::Result<Self> {
        file.try_lock_exclusive().map_err(|error| {
            if fs2::lock_contended_error()
                .raw_os_error()
                .is_some_and(|code| error.raw_os_error() == Some(code))
            {
                io::Error::new(io::ErrorKind::WouldBlock, "host.installation_busy")
            } else {
                error
            }
        })?;
        Ok(Self(file))
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // Release even if a concurrent process spawn briefly inherited the
        // descriptor before exec closed it.
        let _ = FileExt::unlock(&self.0);
    }
}

pub(crate) fn publish_new(directory: &File, name: &Path, bytes: &[u8]) -> io::Result<()> {
    festerm_tunnel_bridge::setup_storage::publish_new_private(directory, name, bytes)
}

pub(crate) fn publish_immutable(
    directory: &File,
    name: &Path,
    bytes: &[u8],
    mismatch: &'static str,
) -> io::Result<()> {
    match storage::open_private_file(directory, name, false) {
        Ok(file) => {
            if file.metadata()?.len() != bytes.len() as u64 {
                return Err(io::Error::other(mismatch));
            }
            let mut existing = Vec::new();
            file.take(bytes.len() as u64 + 1)
                .read_to_end(&mut existing)?;
            if existing == bytes {
                Ok(())
            } else {
                Err(io::Error::other(mismatch))
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            publish_new(directory, name, bytes)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_publication_preserves_shared_atomic_no_replace_boundary() {
        let root = storage::private_test_directory();
        for name in ["host-lifecycle.json", "devtunnel"] {
            let path = root.path().join(format!("{name}-stage"));
            let directory = storage::private_directory(&path, true).unwrap();
            let name = Path::new(name);
            assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
            publish_new(&directory, name, b"complete").unwrap();
            assert!(publish_new(&directory, name, b"replacement").is_err());
            assert_eq!(
                storage::read_private(&directory, name, 32)
                    .unwrap()
                    .as_slice(),
                b"complete"
            );
        }
    }
}
