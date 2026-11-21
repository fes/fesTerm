//! HOST-only Keychain facade. Ordinary account-store selection is unchanged.
use super::{check_size, SecretBytes, SecretReference, SecretStoreError, SERVICE_NAMESPACE};
use festerm_unix_security::macos_keychain as keychain;
use std::{fs::File, path::Path};

/// The two separately journaled HOST secret purposes. This is not configuration
/// metadata or authority to select a credential; host-setup owns that binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostSecretPurpose {
    OwnerAnchor,
    OAuthIdentity,
}

/// In-memory creation context for this executing app and one authenticated HOST.
/// No Debug/serialization, arbitrary GUI path, item migration or shared UI fence.
pub struct MacHostCreationContext {
    applications: keychain::Applications,
    binding: Binding,
}
struct Binding {
    purpose: HostSecretPurpose,
    reference: SecretReference,
}
impl MacHostCreationContext {
    /// Metadata-only preflight before HOST setup reserves state or starts OAuth.
    /// No Keychain is opened and no item or credential is read or changed.
    pub fn validate_identities(
        authenticated_host_path: &Path,
        authenticated_host: &File,
    ) -> Result<(), SecretStoreError> {
        keychain::Applications::capture(authenticated_host_path, authenticated_host)
            .map(drop)
            .map_err(map)
    }
    /// The caller must authenticate and retain the immutable HOST TrustedStage,
    /// and bind creation to reviewed full-snapshot/purpose/reference consent.
    /// This only captures identities; it confers no publisher or lifecycle grant.
    pub fn capture(
        purpose: HostSecretPurpose,
        reserved_reference: &SecretReference,
        authenticated_host_path: &Path,
        authenticated_host: &File,
    ) -> Result<Self, SecretStoreError> {
        Ok(Self {
            applications: keychain::Applications::capture(
                authenticated_host_path,
                authenticated_host,
            )
            .map_err(map)?,
            binding: Binding {
                purpose,
                reference: reserved_reference.duplicate_for_transport(),
            },
        })
    }
    /// Consumes this context for its one bound purpose/reference under the caller's lease.
    /// On any failure the reference remains reserved; do not retry/recreate or
    /// discard interrupted-publication evidence. Host-setup owns the consumed
    /// fresh reservation and authenticated stage binding.
    pub fn create_reserved(
        self,
        secret: &SecretBytes,
    ) -> Result<MacHostSecretItem, SecretStoreError> {
        check_size(secret)?;
        let account = self.binding.reference.account_name();
        let item = secret
            .with_bytes(|bytes| {
                self.applications.create_new(
                    SERVICE_NAMESPACE.as_bytes(),
                    account.as_bytes(),
                    bytes,
                )
            })
            .map_err(map)?;
        Ok(MacHostSecretItem {
            item,
            purpose: self.binding.purpose,
        })
    }
}

/// Retained selected logical item, without secret reads, delete or ACL mutation.
pub struct MacHostSecretItem {
    item: keychain::Item,
    purpose: HostSecretPurpose,
}
impl MacHostSecretItem {
    /// Reopen only the protected journal's exact purpose/reference. Missing,
    /// denied or ambiguous state never creates or changes an item.
    pub fn open_existing(
        purpose: HostSecretPurpose,
        reference: &SecretReference,
    ) -> Result<Self, SecretStoreError> {
        let account = reference.account_name();
        let item = keychain::Item::open_existing(SERVICE_NAMESPACE.as_bytes(), account.as_bytes())
            .map_err(map)?;
        Ok(Self { item, purpose })
    }
    /// Dedicated noninteractive process only. Verify its installed no-UI fence
    /// before opening the selected item; update retains that exact logical item
    /// without a get/set upsert, ACL repair or missing-item recreation.
    pub fn update_existing_noninteractive(
        purpose: HostSecretPurpose,
        reference: &SecretReference,
        secret: &SecretBytes,
    ) -> Result<(), SecretStoreError> {
        after_noninteractive_update_admission(
            secret,
            super::noninteractive::headless_keychain_fenced(),
            || Self::open_existing(purpose, reference)?.update(secret),
        )
    }
    #[must_use]
    pub fn purpose(&self) -> HostSecretPurpose {
        self.purpose
    }
    /// Update the retained exact item, never set/upsert or repair access policy.
    pub fn update(&self, secret: &SecretBytes) -> Result<(), SecretStoreError> {
        check_size(secret)?;
        secret
            .with_bytes(|bytes| self.item.update(bytes))
            .map_err(map)
    }
}
fn after_noninteractive_update_admission(
    secret: &SecretBytes,
    fenced: bool,
    update: impl FnOnce() -> Result<(), SecretStoreError>,
) -> Result<(), SecretStoreError> {
    check_size(secret)?;
    if !fenced {
        return Err(SecretStoreError::Unsupported);
    }
    update()
}

fn map(error: keychain::Error) -> SecretStoreError {
    match error {
        keychain::Error::Missing => SecretStoreError::Missing,
        keychain::Error::Unavailable => SecretStoreError::LockedOrUnavailable,
        keychain::Error::UnsupportedIdentity => SecretStoreError::Unsupported,
        keychain::Error::Collision
        | keychain::Error::InvalidInput
        | keychain::Error::Policy
        | keychain::Error::Native => SecretStoreError::BackendFailure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static_assertions::assert_not_impl_any!(MacHostCreationContext: std::fmt::Debug, Send, Sync);
    static_assertions::assert_not_impl_any!(MacHostSecretItem: std::fmt::Debug, Send, Sync);
    #[test]
    fn host_noninteractive_update_admits_before_open_and_preserves_single_attempt_failure() {
        let secret = SecretBytes::copy_from_slice(b"fixture");
        assert_eq!(
            after_noninteractive_update_admission(&secret, false, || panic!("no native open")),
            Err(SecretStoreError::Unsupported)
        );
        let oversized = SecretBytes::copy_from_slice(&vec![0; super::super::MAX_SECRET_BYTES + 1]);
        assert_eq!(
            after_noninteractive_update_admission(&oversized, true, || panic!("no native open")),
            Err(SecretStoreError::TooLarge)
        );
        let attempts = std::cell::Cell::new(0);
        for error in [
            SecretStoreError::Missing,
            SecretStoreError::LockedOrUnavailable,
        ] {
            assert_eq!(
                after_noninteractive_update_admission(&secret, true, || {
                    attempts.set(attempts.get() + 1);
                    Err(error)
                }),
                Err(error)
            );
        }
        assert_eq!(attempts.get(), 2);
    }
    #[test]
    fn host_item_creation_binding_retains_only_selected_purpose_and_reference() {
        let selected = SecretReference::generate();
        let unrelated = SecretReference::generate();
        let binding = Binding {
            purpose: HostSecretPurpose::OwnerAnchor,
            reference: selected.duplicate_for_transport(),
        };
        assert!(binding.reference == selected);
        assert!(binding.reference != unrelated);
        assert_eq!(binding.purpose, HostSecretPurpose::OwnerAnchor);
    }
    #[test]
    fn host_item_errors_remain_finite_and_no_reference_is_a_display_value() {
        for (from, expected) in [
            (keychain::Error::Missing, SecretStoreError::Missing),
            (
                keychain::Error::Unavailable,
                SecretStoreError::LockedOrUnavailable,
            ),
            (
                keychain::Error::UnsupportedIdentity,
                SecretStoreError::Unsupported,
            ),
            (keychain::Error::Collision, SecretStoreError::BackendFailure),
            (keychain::Error::Policy, SecretStoreError::BackendFailure),
        ] {
            assert_eq!(map(from), expected);
        }
    }
}
