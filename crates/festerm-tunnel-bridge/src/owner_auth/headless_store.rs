//! Deliberately selected headless-only private-file credentials. Not a fallback.
use crate::setup_storage as storage;
use festerm_host_protocol::HostId;
use festerm_secret_store::{SecretBytes, SecretReference, SecretStore, SecretStoreError};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{fs::File, io, path::Path};

const MARKER: &str = "headless-owner-storage.json";
const LOCK: &str = "headless-owner-storage.lock";
const LIMIT: usize = 2048;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    version: u16,
    origin: String,
    installation: String,
}

/// Owner-only files provide weaker rollback protection than the native vault:
/// restoring this whole private directory can restore old anchor/secret state.
/// Desktop composition roots must not select this backend.
/// This library cannot identify a GUI/CLI caller: selection is a composition-root
/// responsibility. Its marker is a namespace binding, not verified headless
/// origin. The host Store wrapper separately enforces protected journal origin.
pub struct ExplicitHeadlessFileStore {
    directory: File,
    installation: String,
}

impl ExplicitHeadlessFileStore {
    /// Explicit local selection, never invoked after a native-vault failure.
    /// The caller must verify protected headless origin and deliberate opt-in.
    /// This method only checks private storage and namespace consistency.
    pub fn select(directory: &File, installation: &str) -> io::Result<Self> {
        HostId::try_from(installation.to_owned())?;
        let expected = Binding {
            version: 1,
            origin: "headless".into(),
            installation: installation.into(),
        };
        let bytes = serde_json::to_vec(&expected)
            .map_err(|_| io::Error::other("owner.invalid_storage_binding"))?;
        match storage::read_private(directory, Path::new(MARKER), 256) {
            Ok(actual) if actual.as_slice() == bytes => {}
            Ok(_) => return Err(io::Error::other("owner.foreign_headless_storage_binding")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                storage::publish_new_private(directory, Path::new(MARKER), &bytes)?
            }
            Err(error) => return Err(error),
        }
        match storage::open_private_file(directory, Path::new(LOCK), false) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                storage::publish_new_private(directory, Path::new(LOCK), b"")?
            }
            Err(error) => return Err(error),
        }
        Ok(Self {
            directory: directory.try_clone()?,
            installation: installation.into(),
        })
    }

    pub fn installation(&self) -> &str {
        &self.installation
    }

    fn lease(&self) -> Result<Lease, SecretStoreError> {
        let lock = storage::open_private_file(&self.directory, Path::new(LOCK), false)
            .map_err(map_error)?;
        lock.try_lock_exclusive().map_err(map_error)?;
        Ok(Lease(lock))
    }

    fn name(reference: &SecretReference) -> std::path::PathBuf {
        format!("headless-owner-secret-{}", reference.to_persisted_string()).into()
    }
}

struct Lease(File);

impl Drop for Lease {
    fn drop(&mut self) {
        // Release even if a concurrent process spawn briefly inherited the
        // descriptor before exec closed it; closing alone would keep the lock.
        let _ = FileExt::unlock(&self.0);
    }
}

fn map_error(error: io::Error) -> SecretStoreError {
    match error.kind() {
        io::ErrorKind::NotFound => SecretStoreError::Missing,
        io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock => {
            SecretStoreError::LockedOrUnavailable
        }
        _ => SecretStoreError::BackendFailure,
    }
}

impl SecretStore for ExplicitHeadlessFileStore {
    fn create_reserved(
        &self,
        reference: &SecretReference,
        secret: &SecretBytes,
    ) -> Result<(), SecretStoreError> {
        if secret.len() > LIMIT {
            return Err(SecretStoreError::TooLarge);
        }
        let _lease = self.lease()?;
        secret
            .with_bytes(|bytes| {
                storage::publish_new_private(&self.directory, &Self::name(reference), bytes)
            })
            .map_err(map_error)
    }

    fn put(&self, secret: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
        if secret.len() > LIMIT {
            return Err(SecretStoreError::TooLarge);
        }
        let _lease = self.lease()?;
        let reference = SecretReference::generate();
        secret
            .with_bytes(|bytes| {
                storage::publish_new_private(&self.directory, &Self::name(&reference), bytes)
            })
            .map_err(map_error)?;
        Ok(reference)
    }
    fn get(&self, reference: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
        let _lease = self.lease()?;
        let bytes = storage::read_private(&self.directory, &Self::name(reference), LIMIT)
            .map_err(map_error)?;
        Ok(SecretBytes::copy_from_slice(&bytes))
    }
    fn update(
        &self,
        reference: &SecretReference,
        secret: &SecretBytes,
    ) -> Result<(), SecretStoreError> {
        if secret.len() > LIMIT {
            return Err(SecretStoreError::TooLarge);
        }
        let _lease = self.lease()?;
        storage::open_private_file(&self.directory, &Self::name(reference), false)
            .map_err(map_error)?;
        secret
            .with_bytes(|bytes| {
                storage::replace_private(&self.directory, &Self::name(reference), bytes)
            })
            .map_err(map_error)
    }
    fn delete(&self, reference: &SecretReference) -> Result<bool, SecretStoreError> {
        let _lease = self.lease()?;
        let name = Self::name(reference);
        #[cfg(unix)]
        let opened = storage::open_private_file(&self.directory, &name, false);
        #[cfg(windows)]
        let opened =
            festerm_windows_security::open_file_no_reparse_for_verification(&self.directory, &name);
        let file = match opened {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(map_error(error)),
        };
        #[cfg(unix)]
        {
            let _file = file;
            festerm_unix_security::remove_private_staging_file(&self.directory, &name)
                .map_err(map_error)?;
        }
        #[cfg(windows)]
        {
            if !file.metadata().map_err(map_error)?.is_file()
                || !festerm_windows_security::is_current_user_only(&file).map_err(map_error)?
            {
                return Err(SecretStoreError::LockedOrUnavailable);
            }
            festerm_windows_security::remove_private_staging_file(&file).map_err(map_error)?;
        }
        Ok(true)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn headless_lease_is_released_while_a_concurrent_spawn_inherits_it() {
        let root = crate::test_support::private_directory("festerm-headless-lease");
        let directory = storage::private_directory(&root, false).unwrap();
        let store =
            ExplicitHeadlessFileStore::select(&directory, &uuid::Uuid::new_v4().to_string())
                .unwrap();
        let reference = store.put(&SecretBytes::copy_from_slice(b"anchor")).unwrap();
        let stop = AtomicBool::new(false);
        let mut failures = 0;
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| {
                    while !stop.load(Ordering::Relaxed) {
                        let _ = std::process::Command::new("true").status();
                    }
                });
            }
            for _ in 0..500 {
                if store.get(&reference).is_err() {
                    failures += 1;
                }
            }
            stop.store(true, Ordering::Relaxed);
        });
        drop(store);
        drop(directory);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(failures, 0);
    }
}
