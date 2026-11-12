use super::*;
#[cfg(target_os = "android")]
use festerm_android_platform as native;

trait NativeIo: Send + Sync {
    fn read(&self, reference: &str) -> Result<Option<Vec<u8>>, &'static str>;
    fn write(&self, reference: &str, bytes: &[u8], replace: bool) -> Result<bool, &'static str>;
    fn delete(&self, reference: &str) -> Result<bool, &'static str>;
}

struct AndroidSecretStore<N>(N);

#[cfg(target_os = "android")]
struct AndroidNative;

#[cfg(target_os = "android")]
impl NativeIo for AndroidNative {
    fn read(&self, reference: &str) -> Result<Option<Vec<u8>>, &'static str> {
        native::secret_read(reference)
    }

    fn write(&self, reference: &str, bytes: &[u8], replace: bool) -> Result<bool, &'static str> {
        native::secret_write(reference, bytes, replace)
    }

    fn delete(&self, reference: &str) -> Result<bool, &'static str> {
        native::secret_delete(reference)
    }
}

#[cfg(target_os = "android")]
pub(super) fn create() -> Result<Box<dyn SecretStore>, SecretStoreError> {
    native::storage_directory().map_err(|_| SecretStoreError::LockedOrUnavailable)?;
    Ok(Box::new(AndroidSecretStore(AndroidNative)))
}

impl<N: NativeIo> SecretStore for AndroidSecretStore<N> {
    fn create_reserved(
        &self,
        reference: &SecretReference,
        secret: &SecretBytes,
    ) -> Result<(), SecretStoreError> {
        check_size(secret)?;
        match self
            .0
            .write(&reference.account_name(), secret.as_slice(), false)
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(SecretStoreError::BackendFailure),
            Err(_) => Err(SecretStoreError::LockedOrUnavailable),
        }
    }

    fn put(&self, secret: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
        check_size(secret)?;
        let reference = SecretReference::generate();
        let stored = self
            .0
            .write(&reference.account_name(), secret.as_slice(), false)
            .map_err(|_| SecretStoreError::LockedOrUnavailable)?;
        if !stored {
            return Err(SecretStoreError::BackendFailure);
        }
        Ok(reference)
    }

    fn get(&self, reference: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
        let mut bytes = self
            .0
            .read(&reference.account_name())
            .map_err(|_| SecretStoreError::LockedOrUnavailable)?
            .ok_or(SecretStoreError::Missing)?;
        if bytes.len() > MAX_SECRET_BYTES {
            bytes.zeroize();
            return Err(SecretStoreError::TooLarge);
        }
        Ok(SecretBytes::from_vec(bytes))
    }

    fn update(
        &self,
        reference: &SecretReference,
        secret: &SecretBytes,
    ) -> Result<(), SecretStoreError> {
        check_size(secret)?;
        match self
            .0
            .write(&reference.account_name(), secret.as_slice(), true)
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(SecretStoreError::Missing),
            Err(_) => Err(SecretStoreError::LockedOrUnavailable),
        }
    }

    fn delete(&self, reference: &SecretReference) -> Result<bool, SecretStoreError> {
        self.0
            .delete(&reference.account_name())
            .map_err(|_| SecretStoreError::LockedOrUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct MockNative {
        entries: Mutex<HashMap<String, Vec<u8>>>,
        unavailable: AtomicBool,
        writes: AtomicUsize,
    }

    impl NativeIo for MockNative {
        fn read(&self, reference: &str) -> Result<Option<Vec<u8>>, &'static str> {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err("unavailable");
            }
            Ok(self.entries.lock().unwrap().get(reference).cloned())
        }

        fn write(
            &self,
            reference: &str,
            bytes: &[u8],
            replace: bool,
        ) -> Result<bool, &'static str> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.unavailable.load(Ordering::SeqCst) {
                return Err("unavailable");
            }
            let mut entries = self.entries.lock().unwrap();
            if entries.contains_key(reference) != replace {
                return Ok(false);
            }
            entries.insert(reference.into(), bytes.to_vec());
            Ok(true)
        }

        fn delete(&self, reference: &str) -> Result<bool, &'static str> {
            Ok(self.entries.lock().unwrap().remove(reference).is_some())
        }
    }

    #[test]
    fn android_reserved_create_is_exact_nonreplacing_and_reopens_existing() {
        let store = AndroidSecretStore(MockNative::default());
        let reference = SecretReference::generate();
        let first = SecretBytes::from_vec(b"protected original".to_vec());
        store.create_reserved(&reference, &first).unwrap();
        assert_eq!(
            store.create_reserved(&reference, &SecretBytes::from_vec(b"replacement".to_vec())),
            Err(SecretStoreError::BackendFailure)
        );
        assert_eq!(store.get(&reference).unwrap().as_slice(), first.as_slice());
        assert_eq!(
            store.get(&SecretReference::generate()).err(),
            Some(SecretStoreError::Missing)
        );
        assert_eq!(store.0.entries.lock().unwrap().len(), 1);
    }

    #[test]
    fn android_reserved_create_unavailable_and_size_failure_never_become_absence() {
        let store = AndroidSecretStore(MockNative::default());
        let reference = SecretReference::generate();
        assert_eq!(
            store.create_reserved(
                &reference,
                &SecretBytes::from_vec(vec![0; MAX_SECRET_BYTES + 1])
            ),
            Err(SecretStoreError::TooLarge)
        );
        assert_eq!(store.0.writes.load(Ordering::SeqCst), 0);
        store.0.unavailable.store(true, Ordering::SeqCst);
        assert_eq!(
            store.create_reserved(&reference, &SecretBytes::from_vec(vec![1])),
            Err(SecretStoreError::LockedOrUnavailable)
        );
        assert_eq!(
            store.get(&reference).err(),
            Some(SecretStoreError::LockedOrUnavailable)
        );
        assert!(store.0.entries.lock().unwrap().is_empty());
    }
}
