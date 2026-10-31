#![forbid(unsafe_code)]

//! Native, opaque-reference secret storage.
//!
//! This crate never substitutes an insecure store when the native platform
//! store is unavailable. Callers must surface the returned error and keep
//! opaque references, rather than secret values, in ordinary configuration.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use keyring_core::{CredentialStore, Entry};
use uuid::Uuid;
use zeroize::Zeroize;

/// The fixed native-service namespace used by all fesTerm secrets.
pub const SERVICE_NAMESPACE: &str = "io.github.fes.festerm";

/// The largest secret accepted by any store. Private keys, passwords, and
/// OAuth refresh credentials are far smaller; the bound keeps a corrupted or
/// hostile caller from filling the platform store.
pub const MAX_SECRET_BYTES: usize = 64 * 1024;

/// A validated opaque identifier for a secret stored outside configuration.
#[derive(PartialEq, Eq, Hash)]
pub struct SecretReference(Uuid);

impl SecretReference {
    /// Creates a cryptographically random, opaque reference.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }

    /// Parses the canonical representation of a randomly generated reference.
    pub fn parse(value: &str) -> Result<Self, SecretStoreError> {
        let reference = Uuid::parse_str(value).map_err(|_| SecretStoreError::InvalidReference)?;
        if reference.get_version_num() != 4 || value != reference.hyphenated().to_string() {
            return Err(SecretStoreError::InvalidReference);
        }
        Ok(Self(reference))
    }

    /// Returns this reference in its canonical representation for persistence.
    #[must_use]
    pub fn to_persisted_string(&self) -> String {
        self.0.hyphenated().to_string()
    }

    /// Creates an owned copy for a narrowly scoped transport operation.
    ///
    /// References are opaque identifiers rather than secret values. This
    /// deliberately avoids implementing `Clone`, which would make copying
    /// them too easy in ordinary application metadata.
    #[must_use]
    pub fn duplicate_for_transport(&self) -> Self {
        Self(self.0)
    }

    fn account_name(&self) -> String {
        self.0.hyphenated().to_string()
    }
}

/// Secret byte material whose backing allocation is wiped when dropped.
pub struct SecretBytes(Box<[u8]>);

impl SecretBytes {
    /// Copies secret material into an owned, zeroizing allocation.
    #[must_use]
    pub fn copy_from_slice(bytes: &[u8]) -> Self {
        Self(bytes.into())
    }

    /// Moves UTF-8 secret text into a zeroizing allocation and wipes the
    /// source string before it is dropped.
    #[must_use]
    pub fn from_secret_string(mut secret: String) -> Self {
        let bytes = Self::copy_from_slice(secret.as_bytes());
        secret.zeroize();
        bytes
    }

    /// Invokes a caller-supplied operation with a limited borrowed view.
    pub fn with_bytes<T>(&self, operation: impl FnOnce(&[u8]) -> T) -> T {
        operation(&self.0)
    }

    /// Returns the number of secret bytes without exposing the bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Reports whether this secret has no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn as_slice(&self) -> &[u8] {
        &self.0
    }

    fn from_vec(bytes: Vec<u8>) -> Self {
        Self(bytes.into_boxed_slice())
    }

    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.zeroize();
    }
}

/// Content-free outcomes from native secret storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretStoreError {
    /// The opaque reference does not name a stored secret.
    Missing,
    /// The platform service is locked, unavailable, or cannot be contacted.
    LockedOrUnavailable,
    /// The native backend failed without a more actionable classification.
    BackendFailure,
    /// A caller supplied an invalid opaque reference.
    InvalidReference,
    /// The current target does not provide a supported native backend.
    Unsupported,
    /// The secret exceeds [`MAX_SECRET_BYTES`] and was not stored.
    TooLarge,
}

/// Stores secret values behind opaque references.
pub trait SecretStore: Send + Sync {
    /// Stores a new secret and returns a new opaque reference.
    fn put(&self, secret: &SecretBytes) -> Result<SecretReference, SecretStoreError>;

    /// Retrieves the secret named by `reference`.
    fn get(&self, reference: &SecretReference) -> Result<SecretBytes, SecretStoreError>;

    /// Replaces the secret named by `reference`.
    fn update(
        &self,
        reference: &SecretReference,
        secret: &SecretBytes,
    ) -> Result<(), SecretStoreError>;

    /// Deletes a secret, returning whether it was present.
    fn delete(&self, reference: &SecretReference) -> Result<bool, SecretStoreError>;
}

fn check_size(secret: &SecretBytes) -> Result<(), SecretStoreError> {
    if secret.len() > MAX_SECRET_BYTES {
        return Err(SecretStoreError::TooLarge);
    }
    Ok(())
}

/// A deterministic in-memory store for tests and explicit application injection.
///
/// It is deliberately not selected by [`native_store`].
pub struct MemorySecretStore {
    secrets: Mutex<HashMap<SecretReference, SecretBytes>>,
    next_reference: Mutex<u128>,
}

impl Default for MemorySecretStore {
    fn default() -> Self {
        Self {
            secrets: Mutex::new(HashMap::new()),
            next_reference: Mutex::new(0),
        }
    }
}

impl MemorySecretStore {
    /// Creates an empty deterministic store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemorySecretStore {
    fn put(&self, secret: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
        check_size(secret)?;
        let mut next_reference = self
            .next_reference
            .lock()
            .map_err(|_| SecretStoreError::BackendFailure)?;
        *next_reference = next_reference
            .checked_add(1)
            .ok_or(SecretStoreError::BackendFailure)?;
        let reference = SecretReference::from_memory_counter(*next_reference);
        let mut secrets = self
            .secrets
            .lock()
            .map_err(|_| SecretStoreError::BackendFailure)?;
        secrets.insert(
            reference.clone_for_store(),
            SecretBytes::copy_from_slice(secret.as_slice()),
        );
        Ok(reference)
    }

    fn get(&self, reference: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
        let secrets = self
            .secrets
            .lock()
            .map_err(|_| SecretStoreError::BackendFailure)?;
        secrets
            .get(reference)
            .map(|secret| SecretBytes::copy_from_slice(secret.as_slice()))
            .ok_or(SecretStoreError::Missing)
    }

    fn update(
        &self,
        reference: &SecretReference,
        secret: &SecretBytes,
    ) -> Result<(), SecretStoreError> {
        check_size(secret)?;
        let mut secrets = self
            .secrets
            .lock()
            .map_err(|_| SecretStoreError::BackendFailure)?;
        let existing = secrets
            .get_mut(reference)
            .ok_or(SecretStoreError::Missing)?;
        *existing = SecretBytes::copy_from_slice(secret.as_slice());
        Ok(())
    }

    fn delete(&self, reference: &SecretReference) -> Result<bool, SecretStoreError> {
        let mut secrets = self
            .secrets
            .lock()
            .map_err(|_| SecretStoreError::BackendFailure)?;
        Ok(secrets.remove(reference).is_some())
    }
}

impl SecretReference {
    fn clone_for_store(&self) -> Self {
        Self(self.0)
    }

    fn from_memory_counter(counter: u128) -> Self {
        const VERSION_MASK: u128 = 0xf << 76;
        const VARIANT_MASK: u128 = 0x3 << 62;
        const VERSION_4: u128 = 0x4 << 76;
        const RFC_4122_VARIANT: u128 = 0x2 << 62;

        Self(Uuid::from_u128(
            (counter & !(VERSION_MASK | VARIANT_MASK)) | VERSION_4 | RFC_4122_VARIANT,
        ))
    }
}

struct NativeSecretStore {
    store: Arc<CredentialStore>,
    modifiers: HashMap<&'static str, &'static str>,
    operation_lock: Mutex<()>,
}

impl NativeSecretStore {
    fn new(store: Arc<CredentialStore>, modifiers: HashMap<&'static str, &'static str>) -> Self {
        Self {
            store,
            modifiers,
            operation_lock: Mutex::new(()),
        }
    }

    fn entry(&self, reference: &SecretReference) -> Result<Entry, SecretStoreError> {
        let account = reference.account_name();
        self.store
            .build(
                SERVICE_NAMESPACE,
                &account,
                (!self.modifiers.is_empty()).then_some(&self.modifiers),
            )
            .map_err(map_keyring_error)
    }
}

impl SecretStore for NativeSecretStore {
    fn put(&self, secret: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
        check_size(secret)?;
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| SecretStoreError::BackendFailure)?;
        let reference = SecretReference::generate();
        self.entry(&reference)?
            .set_secret(secret.as_slice())
            .map_err(map_keyring_error)?;
        Ok(reference)
    }

    fn get(&self, reference: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| SecretStoreError::BackendFailure)?;
        self.entry(reference)?
            .get_secret()
            .map(SecretBytes::from_vec)
            .map_err(map_keyring_error)
    }

    fn update(
        &self,
        reference: &SecretReference,
        secret: &SecretBytes,
    ) -> Result<(), SecretStoreError> {
        check_size(secret)?;
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| SecretStoreError::BackendFailure)?;
        let entry = self.entry(reference)?;
        entry.get_secret().map_err(map_keyring_error)?.zeroize();
        entry
            .set_secret(secret.as_slice())
            .map_err(map_keyring_error)
    }

    fn delete(&self, reference: &SecretReference) -> Result<bool, SecretStoreError> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| SecretStoreError::BackendFailure)?;
        match self.entry(reference)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring_core::Error::NoEntry) => Ok(false),
            Err(error) => Err(map_keyring_error(error)),
        }
    }
}

fn map_keyring_error(error: keyring_core::Error) -> SecretStoreError {
    match error {
        keyring_core::Error::NoEntry => SecretStoreError::Missing,
        keyring_core::Error::NoStorageAccess(_) => SecretStoreError::LockedOrUnavailable,
        keyring_core::Error::PlatformFailure(ref platform) => map_platform_failure(platform),
        keyring_core::Error::NotSupportedByStore(_) | keyring_core::Error::NoDefaultStore => {
            SecretStoreError::Unsupported
        }
        keyring_core::Error::BadEncoding(mut bytes) => {
            bytes.zeroize();
            SecretStoreError::BackendFailure
        }
        keyring_core::Error::BadDataFormat(mut bytes, _) => {
            bytes.zeroize();
            SecretStoreError::BackendFailure
        }
        _ => SecretStoreError::BackendFailure,
    }
}

#[cfg(target_vendor = "apple")]
fn map_platform_failure(error: &keyring_core::error::PlatformError) -> SecretStoreError {
    // Security.framework result codes the keyring adapter does not classify.
    const ERR_SEC_USER_CANCELED: i32 = -128;
    const ERR_SEC_NOT_AVAILABLE: i32 = -25291;
    const ERR_SEC_AUTH_FAILED: i32 = -25293;
    const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;
    const ERR_SEC_MISSING_ENTITLEMENT: i32 = -34018;

    match error
        .downcast_ref::<security_framework::base::Error>()
        .map(|error| error.code())
    {
        Some(
            ERR_SEC_USER_CANCELED
            | ERR_SEC_NOT_AVAILABLE
            | ERR_SEC_AUTH_FAILED
            | ERR_SEC_INTERACTION_NOT_ALLOWED,
        ) => SecretStoreError::LockedOrUnavailable,
        Some(ERR_SEC_MISSING_ENTITLEMENT) => SecretStoreError::Unsupported,
        _ => SecretStoreError::BackendFailure,
    }
}

#[cfg(not(target_vendor = "apple"))]
fn map_platform_failure(_: &keyring_core::error::PlatformError) -> SecretStoreError {
    SecretStoreError::BackendFailure
}

/// Creates the target's native secret store.
///
/// This never falls back to a file, keyutils, plaintext, or
/// [`MemorySecretStore`].
/// iOS uses the app's own Keychain access group (ADR 0054); being Unix does
/// not imply a desktop D-Bus service.
pub fn native_store() -> Result<Box<dyn SecretStore>, SecretStoreError> {
    native::create()
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;

    pub(super) fn create() -> Result<Box<dyn SecretStore>, SecretStoreError> {
        let store =
            apple_native_keyring_store::keychain::Store::new().map_err(map_keyring_error)?;
        Ok(Box::new(NativeSecretStore::new(store, HashMap::new())))
    }
}

#[cfg(target_os = "ios")]
mod native {
    use super::*;

    pub(super) fn create() -> Result<Box<dyn SecretStore>, SecretStoreError> {
        let store =
            apple_native_keyring_store::protected::Store::new_with_configuration(&HashMap::from([
                ("cloud-sync", "false"),
            ]))
            .map_err(map_keyring_error)?;
        Ok(Box::new(NativeSecretStore::new(
            store,
            HashMap::from([("access-policy", "when-unlocked-this-device-only")]),
        )))
    }
}

#[cfg(windows)]
mod native {
    use super::*;

    pub(super) fn create() -> Result<Box<dyn SecretStore>, SecretStoreError> {
        let configuration = HashMap::from([
            ("prefix", "io.github.fes.festerm:"),
            ("divider", "/"),
            ("suffix", ""),
            ("service_no_divider", "true"),
        ]);
        let store = windows_native_keyring_store::Store::new_with_configuration(&configuration)
            .map_err(map_keyring_error)?;
        Ok(Box::new(NativeSecretStore::new(store, HashMap::new())))
    }
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios"))))]
mod native {
    use super::*;

    pub(super) fn create() -> Result<Box<dyn SecretStore>, SecretStoreError> {
        let store = zbus_secret_service_keyring_store::Store::new().map_err(map_keyring_error)?;
        // The service namespace and random account name isolate fesTerm
        // entries without triggering a
        // GUI prompt to create a separate collection.
        let modifiers = HashMap::from([("label", "fesTerm secret")]);
        Ok(Box::new(NativeSecretStore::new(store, modifiers)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::assert_not_impl_any;

    assert_not_impl_any!(SecretBytes: Clone, std::fmt::Debug, serde::Serialize);
    assert_not_impl_any!(SecretReference: Clone, std::fmt::Debug);

    type BuiltEntry = (String, String, Vec<(String, String)>);

    struct RecordingStore {
        inner: Arc<CredentialStore>,
        built: Mutex<Vec<BuiltEntry>>,
    }

    impl keyring_core::api::CredentialStoreApi for RecordingStore {
        fn vendor(&self) -> String {
            "recording test store".to_owned()
        }

        fn id(&self) -> String {
            "recording".to_owned()
        }

        fn build(
            &self,
            service: &str,
            user: &str,
            modifiers: Option<&HashMap<&str, &str>>,
        ) -> keyring_core::Result<Entry> {
            let mut recorded: Vec<_> = modifiers
                .into_iter()
                .flatten()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect();
            recorded.sort();
            self.built
                .lock()
                .unwrap()
                .push((service.to_owned(), user.to_owned(), recorded));
            self.inner.build(service, user, None)
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn mock_backed_store() -> NativeSecretStore {
        NativeSecretStore::new(
            keyring_core::mock::Store::new().expect("mock store"),
            HashMap::new(),
        )
    }

    #[test]
    fn native_wrapper_round_trips_updates_and_deletes_through_a_keyring_store() {
        let store = mock_backed_store();
        let reference = store
            .put(&SecretBytes::copy_from_slice(b"first"))
            .expect("put");
        assert_eq!(
            store
                .get(&reference)
                .expect("get")
                .with_bytes(<[u8]>::to_vec),
            b"first"
        );
        store
            .update(&reference, &SecretBytes::copy_from_slice(b"second"))
            .expect("update");
        assert_eq!(
            store
                .get(&reference)
                .expect("get")
                .with_bytes(<[u8]>::to_vec),
            b"second"
        );
        assert_eq!(store.delete(&reference), Ok(true));
        assert_eq!(store.delete(&reference), Ok(false));
        assert!(matches!(
            store.get(&reference),
            Err(SecretStoreError::Missing)
        ));
        assert_eq!(
            store.update(&reference, &SecretBytes::copy_from_slice(b"x")),
            Err(SecretStoreError::Missing)
        );
    }

    #[test]
    fn native_wrapper_namespaces_items_and_forwards_backend_modifiers() {
        let recording = Arc::new(RecordingStore {
            inner: keyring_core::mock::Store::new().expect("mock store"),
            built: Mutex::new(Vec::new()),
        });
        let store = NativeSecretStore::new(
            Arc::<RecordingStore>::clone(&recording) as Arc<CredentialStore>,
            HashMap::from([("access-policy", "when-unlocked-this-device-only")]),
        );
        let reference = store
            .put(&SecretBytes::copy_from_slice(b"secret"))
            .expect("put");

        let built = recording.built.lock().unwrap();
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].0, SERVICE_NAMESPACE);
        assert_eq!(built[0].1, reference.to_persisted_string());
        assert_eq!(
            built[0].2,
            [(
                "access-policy".to_owned(),
                "when-unlocked-this-device-only".to_owned()
            )]
        );
    }

    #[test]
    fn oversized_secrets_are_refused_by_every_store_without_storing_anything() {
        let oversized = SecretBytes::copy_from_slice(&vec![7; MAX_SECRET_BYTES + 1]);
        let exact = SecretBytes::copy_from_slice(&vec![7; MAX_SECRET_BYTES]);
        let stores: [Box<dyn SecretStore>; 2] = [
            Box::new(mock_backed_store()),
            Box::new(MemorySecretStore::new()),
        ];
        for store in &stores {
            assert_eq!(
                store.put(&oversized).err(),
                Some(SecretStoreError::TooLarge)
            );
            let reference = store.put(&exact).expect("limit is inclusive");
            assert_eq!(
                store.update(&reference, &oversized),
                Err(SecretStoreError::TooLarge)
            );
            assert_eq!(store.get(&reference).expect("unchanged").len(), exact.len());
        }
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn apple_security_codes_map_to_actionable_content_free_errors() {
        let failure = |code: i32| {
            map_keyring_error(keyring_core::Error::PlatformFailure(Box::new(
                security_framework::base::Error::from_code(code),
            )))
        };
        assert_eq!(failure(-25308), SecretStoreError::LockedOrUnavailable);
        assert_eq!(failure(-25293), SecretStoreError::LockedOrUnavailable);
        assert_eq!(failure(-34018), SecretStoreError::Unsupported);
        assert_eq!(failure(-50), SecretStoreError::BackendFailure);
        assert_eq!(
            map_keyring_error(keyring_core::Error::NoEntry),
            SecretStoreError::Missing
        );
    }

    #[test]
    fn references_are_random_canonical_uuid_v4_values() {
        let first = SecretReference::generate();
        let second = SecretReference::generate();

        assert_ne!(first.to_persisted_string(), second.to_persisted_string());
        assert_eq!(
            SecretReference::parse(&first.to_persisted_string())
                .expect("generated reference must validate")
                .to_persisted_string(),
            first.to_persisted_string()
        );
    }

    #[test]
    fn references_reject_noncanonical_or_nonrandom_values() {
        assert!(matches!(
            SecretReference::parse("not-a-reference"),
            Err(SecretStoreError::InvalidReference)
        ));
        assert!(matches!(
            SecretReference::parse("00000000-0000-0000-0000-000000000000"),
            Err(SecretStoreError::InvalidReference)
        ));
        assert!(matches!(
            SecretReference::parse("550E8400-E29B-41D4-A716-446655440000"),
            Err(SecretStoreError::InvalidReference)
        ));
    }

    #[test]
    fn secret_bytes_only_expose_borrowed_bytes_explicitly() {
        let secret = SecretBytes::copy_from_slice(b"opaque-secret");

        assert_eq!(secret.len(), 13);
        assert!(!secret.is_empty());
        assert_eq!(secret.with_bytes(|bytes| bytes.len()), 13);
    }

    #[test]
    fn secret_bytes_zeroize_their_backing_bytes() {
        let mut secret = SecretBytes::copy_from_slice(b"erase-me");

        secret.zeroize();

        assert!(secret.with_bytes(|bytes| bytes.iter().all(|byte| *byte == 0)));
    }

    #[test]
    fn memory_store_put_get_update_and_idempotent_delete() {
        let store = MemorySecretStore::new();
        let original = SecretBytes::copy_from_slice(b"first");
        let reference = store.put(&original).expect("put should succeed");
        assert_eq!(
            reference.to_persisted_string(),
            "00000000-0000-4000-8000-000000000001"
        );

        assert_eq!(
            store
                .get(&reference)
                .expect("stored secret should be readable")
                .with_bytes(|bytes| bytes.to_vec()),
            b"first"
        );

        store
            .update(&reference, &SecretBytes::copy_from_slice(b"second"))
            .expect("update should succeed");
        assert_eq!(
            store
                .get(&reference)
                .expect("updated secret should be readable")
                .with_bytes(|bytes| bytes.to_vec()),
            b"second"
        );

        assert_eq!(store.delete(&reference), Ok(true));
        assert_eq!(store.delete(&reference), Ok(false));
        assert!(matches!(
            store.get(&reference),
            Err(SecretStoreError::Missing)
        ));
    }

    #[test]
    fn memory_store_reports_missing_updates() {
        let store = MemorySecretStore::new();
        let reference = SecretReference::generate();

        assert_eq!(
            store.update(&reference, &SecretBytes::copy_from_slice(b"value")),
            Err(SecretStoreError::Missing)
        );
    }

    #[test]
    fn memory_store_reports_an_injected_mutex_failure() {
        let store = MemorySecretStore::new();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = store.secrets.lock().expect("fresh mutex should lock");
            panic!("poison test mutex");
        }));

        assert!(matches!(
            store.get(&SecretReference::generate()),
            Err(SecretStoreError::BackendFailure)
        ));
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios", windows, unix)))]
mod native {
    use super::*;

    pub(super) fn create() -> Result<Box<dyn SecretStore>, SecretStoreError> {
        Err(SecretStoreError::Unsupported)
    }
}
