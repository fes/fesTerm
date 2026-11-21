//! Selected Desktop HOST purposes; ordinary account stores are unaffected.
use festerm_secret_store::{
    HostSecretPurpose, MacHostSecretItem, SecretBytes, SecretReference, SecretStore,
    SecretStoreError,
};

pub(crate) struct HostPurposeStore {
    native: Box<dyn SecretStore>,
    purpose: HostSecretPurpose,
}
impl HostPurposeStore {
    pub(crate) fn open(purpose: HostSecretPurpose) -> Result<Self, SecretStoreError> {
        Ok(Self {
            native: festerm_secret_store::native_store()?,
            purpose,
        })
    }
}
impl SecretStore for HostPurposeStore {
    fn create_reserved(
        &self,
        _: &SecretReference,
        _: &SecretBytes,
    ) -> Result<(), SecretStoreError> {
        // Creation needs a freshly consumed journal reservation AND a held,
        // authenticated TrustedStage, supplied by the foreground call below.
        Err(SecretStoreError::Unsupported)
    }
    fn put(&self, _: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
        Err(SecretStoreError::Unsupported)
    }
    fn get(&self, reference: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
        self.native.get(reference)
    }
    fn get_noninteractive(
        &self,
        reference: &SecretReference,
    ) -> Result<SecretBytes, SecretStoreError> {
        self.native.get_noninteractive(reference)
    }
    fn update(
        &self,
        reference: &SecretReference,
        secret: &SecretBytes,
    ) -> Result<(), SecretStoreError> {
        MacHostSecretItem::open_existing(self.purpose, reference)?.update(secret)
    }
    fn update_noninteractive(
        &self,
        reference: &SecretReference,
        secret: &SecretBytes,
    ) -> Result<(), SecretStoreError> {
        MacHostSecretItem::update_existing_noninteractive(self.purpose, reference, secret)
    }
    fn delete(&self, reference: &SecretReference) -> Result<bool, SecretStoreError> {
        self.native.delete(reference)
    }
}
