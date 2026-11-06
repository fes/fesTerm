use super::*;
use festerm_android_platform as native;

struct AndroidSecretStore;

pub(super) fn create() -> Result<Box<dyn SecretStore>, SecretStoreError> {
    native::storage_directory().map_err(|_| SecretStoreError::LockedOrUnavailable)?;
    Ok(Box::new(AndroidSecretStore))
}

impl SecretStore for AndroidSecretStore {
    fn put(&self, secret: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
        check_size(secret)?;
        let reference = SecretReference::generate();
        let stored = native::secret_write(&reference.account_name(), secret.as_slice(), false)
            .map_err(|_| SecretStoreError::LockedOrUnavailable)?;
        if !stored {
            return Err(SecretStoreError::BackendFailure);
        }
        Ok(reference)
    }

    fn get(&self, reference: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
        let mut bytes = native::secret_read(&reference.account_name())
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
        match native::secret_write(&reference.account_name(), secret.as_slice(), true) {
            Ok(true) => Ok(()),
            Ok(false) => Err(SecretStoreError::Missing),
            Err(_) => Err(SecretStoreError::LockedOrUnavailable),
        }
    }

    fn delete(&self, reference: &SecretReference) -> Result<bool, SecretStoreError> {
        native::secret_delete(&reference.account_name())
            .map_err(|_| SecretStoreError::LockedOrUnavailable)
    }
}
