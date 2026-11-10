//! Owner credentials: native by default, explicitly selected protected files for
//! headless only. No provider-token borrowing or automatic/config fallback.
use super::{OwnerSigningKey, PinnedHost};
use festerm_host_protocol::{ContractError, HostId, PrincipalId, Validate};
use festerm_secret_store::{
    native_store, SecretBytes, SecretReference, SecretStore, SecretStoreError,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const KEY_RECORD: &[u8] = b"fesTerm owner signing key v1\0";

/// Exact locally approved binding, not authority supplied by a challenge.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerBinding {
    pub installation: String,
    pub host: PinnedHost,
    pub account: String,
    pub principal: PrincipalId,
}

impl Validate for OwnerBinding {
    fn validate(&self) -> Result<(), ContractError> {
        HostId::try_from(self.installation.clone())?;
        self.host.validate()?;
        if self.account.trim().is_empty()
            || self.account.len() > 256
            || self.account.chars().any(char::is_control)
        {
            return Err(ContractError::Malformed);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerKeyError {
    Vault(SecretStoreError),
    InvalidBinding,
    KeyGenerationUnavailable,
    ChannelBindingUnavailable,
}

impl std::fmt::Display for OwnerKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Vault(SecretStoreError::LockedOrUnavailable) =>
                "owner vault locked or unavailable; unlock the native credential store and retry",
            Self::Vault(SecretStoreError::Missing) =>
                "owner credential missing; explicitly enroll a new key locally",
            Self::Vault(SecretStoreError::Unsupported) =>
                "native owner credential storage unsupported; no plaintext fallback",
            Self::Vault(_) => "native owner credential operation failed; no fallback",
            Self::InvalidBinding => "owner credential does not match the approved installation, host, account or owner",
            Self::KeyGenerationUnavailable => "owner key generation unavailable; retry when system randomness is available",
            Self::ChannelBindingUnavailable =>
                "HOST owner authentication disabled: verified SSH transport lacks a shared connection-and-channel binding exporter",
        })
    }
}
impl std::error::Error for OwnerKeyError {}
impl From<SecretStoreError> for OwnerKeyError {
    fn from(error: SecretStoreError) -> Self {
        Self::Vault(error)
    }
}

/// The seed and approved binding live together in the selected credential store.
/// Persist only this opaque reference in protected local credential metadata,
/// never in a recipe, connection JSON, ordinary configuration or logs.
pub struct ClientOwnerKey {
    reference: SecretReference,
    public_key: [u8; 32],
    headless_installation: Option<String>,
}

impl ClientOwnerKey {
    #[cfg(feature = "relay")]
    pub fn generate_headless(
        selected: &super::ExplicitHeadlessFileStore,
        approved: &OwnerBinding,
    ) -> Result<Self, OwnerKeyError> {
        if selected.installation() != approved.installation {
            return Err(OwnerKeyError::InvalidBinding);
        }
        let mut key = Self::generate(selected, approved)?;
        key.headless_installation = Some(approved.installation.clone());
        Ok(key)
    }

    #[cfg(feature = "relay")]
    pub fn open_headless(
        selected: &super::ExplicitHeadlessFileStore,
        reference: SecretReference,
        approved: &OwnerBinding,
    ) -> Result<Self, OwnerKeyError> {
        if selected.installation() != approved.installation {
            return Err(OwnerKeyError::InvalidBinding);
        }
        let mut key = Self::open(selected, reference, approved)?;
        key.headless_installation = Some(approved.installation.clone());
        Ok(key)
    }

    #[cfg(feature = "relay")]
    pub fn remove_headless(
        &self,
        selected: &super::ExplicitHeadlessFileStore,
    ) -> Result<(), OwnerKeyError> {
        if self.headless_installation.as_deref() != Some(selected.installation()) {
            return Err(OwnerKeyError::InvalidBinding);
        }
        selected.delete(&self.reference)?;
        Ok(())
    }

    pub fn generate_native(approved: &OwnerBinding) -> Result<Self, OwnerKeyError> {
        Self::generate(native_store()?.as_ref(), approved)
    }

    pub fn open_native(
        reference: SecretReference,
        approved: &OwnerBinding,
    ) -> Result<Self, OwnerKeyError> {
        Self::open(native_store()?.as_ref(), reference, approved)
    }

    pub fn reference(&self) -> SecretReference {
        self.reference.duplicate_for_transport()
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.public_key
    }

    /// Delete local signing material; host-side revocation is a separate local
    /// consent operation. Retain the reference on failure so deletion is retryable.
    pub fn remove_native(&self) -> Result<(), OwnerKeyError> {
        if self.headless_installation.is_some() {
            return Err(OwnerKeyError::InvalidBinding);
        }
        native_store()?.delete(&self.reference)?;
        Ok(())
    }

    fn generate(vault: &dyn SecretStore, approved: &OwnerBinding) -> Result<Self, OwnerKeyError> {
        approved
            .validate()
            .map_err(|_| OwnerKeyError::InvalidBinding)?;
        let key =
            OwnerSigningKey::generate().map_err(|_| OwnerKeyError::KeyGenerationUnavailable)?;
        let mut bytes = Zeroizing::new(KEY_RECORD.to_vec());
        bytes.extend_from_slice(key.secret_seed().as_ref());
        serde_json::to_writer(&mut *bytes, approved).map_err(|_| OwnerKeyError::InvalidBinding)?;
        let reference = vault.put(&SecretBytes::copy_from_slice(&bytes))?;
        Ok(Self {
            reference,
            public_key: key.public_key(),
            headless_installation: None,
        })
    }

    fn open(
        vault: &dyn SecretStore,
        reference: SecretReference,
        approved: &OwnerBinding,
    ) -> Result<Self, OwnerKeyError> {
        approved
            .validate()
            .map_err(|_| OwnerKeyError::InvalidBinding)?;
        let secret = vault.get(&reference)?;
        let public_key = secret.with_bytes(|bytes| {
            if bytes.len() > 2048 || !bytes.starts_with(KEY_RECORD) {
                return Err(OwnerKeyError::InvalidBinding);
            }
            let bytes = &bytes[KEY_RECORD.len()..];
            let seed: &[u8; 32] = bytes
                .get(..32)
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or(OwnerKeyError::InvalidBinding)?;
            let stored: OwnerBinding =
                serde_json::from_slice(&bytes[32..]).map_err(|_| OwnerKeyError::InvalidBinding)?;
            if stored != *approved {
                return Err(OwnerKeyError::InvalidBinding);
            }
            Ok(OwnerSigningKey::from_seed(seed).public_key())
        })?;
        Ok(Self {
            reference,
            public_key,
            headless_installation: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_secret_store::MemorySecretStore;

    fn binding() -> OwnerBinding {
        OwnerBinding {
            installation: HostId::new().to_string(),
            host: PinnedHost {
                host: HostId::new(),
                host_key_fingerprint: format!("SHA256:{}", "a".repeat(43)),
            },
            account: "github:1234".into(),
            principal: PrincipalId::new(),
        }
    }

    #[test]
    fn native_owner_reference_reopens_only_exact_approved_binding() {
        let vault = MemorySecretStore::new();
        let approved = binding();
        let key = ClientOwnerKey::generate(&vault, &approved).unwrap();
        let unrelated = vault
            .put(&SecretBytes::copy_from_slice(
                b"provider credential is not owner signing material",
            ))
            .unwrap();
        assert!(matches!(
            ClientOwnerKey::open(&vault, unrelated, &approved),
            Err(OwnerKeyError::InvalidBinding)
        ));
        let reopened = ClientOwnerKey::open(&vault, key.reference(), &approved).unwrap();
        assert_eq!(key.public_key(), reopened.public_key());
        for field in 0..5 {
            let mut foreign = approved.clone();
            match field {
                0 => foreign.account = "github:foreign".into(),
                1 => foreign.host.host = HostId::new(),
                2 => foreign.host.host_key_fingerprint = format!("SHA256:{}", "b".repeat(43)),
                3 => foreign.principal = PrincipalId::new(),
                _ => foreign.installation = HostId::new().to_string(),
            }
            assert!(matches!(
                ClientOwnerKey::open(&vault, key.reference(), &foreign),
                Err(OwnerKeyError::InvalidBinding)
            ));
        }
        vault.delete(&key.reference()).unwrap();
        assert!(matches!(
            ClientOwnerKey::open(&vault, key.reference(), &approved),
            Err(OwnerKeyError::Vault(SecretStoreError::Missing))
        ));
    }

    struct Locked;
    impl SecretStore for Locked {
        fn put(&self, _: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
            Err(SecretStoreError::LockedOrUnavailable)
        }
        fn get(&self, _: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
            Err(SecretStoreError::LockedOrUnavailable)
        }
        fn update(&self, _: &SecretReference, _: &SecretBytes) -> Result<(), SecretStoreError> {
            Err(SecretStoreError::LockedOrUnavailable)
        }
        fn delete(&self, _: &SecretReference) -> Result<bool, SecretStoreError> {
            Err(SecretStoreError::LockedOrUnavailable)
        }
    }

    #[test]
    fn native_owner_vault_failure_never_creates_a_fallback_key() {
        assert!(matches!(
            ClientOwnerKey::generate(&Locked, &binding()),
            Err(OwnerKeyError::Vault(SecretStoreError::LockedOrUnavailable))
        ));
        assert!(OwnerKeyError::Vault(SecretStoreError::LockedOrUnavailable)
            .to_string()
            .contains("unlock"));
    }
}
