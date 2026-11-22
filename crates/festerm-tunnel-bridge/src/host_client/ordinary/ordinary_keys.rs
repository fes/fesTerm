use super::*;
use crate::host_client::approvals::{
    self, storage, OriginalCommitter, OriginalsAnchor, ReceiptIndex,
};
use festerm_secret_store::{
    native_store, SecretBytes, SecretReference, SecretStore, SecretStoreError,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

#[path = "key_diagnostics.rs"]
mod diagnostics;
pub use diagnostics::KeyDiagnostic;
use diagnostics::KeyStage;

const PURPOSE: &str = "fesTerm ordinary account route device v1";
const KEY_RECORD: &[u8] = b"fesTerm ordinary device signing seed v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyError {
    Diagnosed(KeyDiagnostic),
    Storage,
    Busy,
    BindingMismatch,
    Vault(SecretStoreError),
    RandomUnavailable,
}
impl KeyError {
    pub fn message(self) -> &'static str {
        match self {
        Self::Vault(SecretStoreError::LockedOrUnavailable)=>"Unlock native secure storage and retry the normal host connection.",
        Self::Vault(SecretStoreError::Missing)=>"The normal device key is missing. No replacement was generated for a ready key record.",
        Self::Vault(SecretStoreError::Unsupported)=>"Native device-key storage is unavailable; no plaintext fallback was used.",
        Self::Vault(_)=>"The native device-key operation failed; no fallback was used.",
        Self::Busy=>"This account and host device key is already being prepared. Retry after that work drains.",
        _=>"The protected normal device-key record could not be verified; no host or key was substituted.",
    }
    }
}
impl From<SecretStoreError> for KeyError {
    fn from(v: SecretStoreError) -> Self {
        Self::Vault(v)
    }
}
fn key_failure(v: KeyError) -> Failure {
    Failure::OrdinaryKey(v)
}
fn storage_failure(stage: KeyStage, error: io::Error) -> KeyError {
    KeyError::storage_at(stage, error)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::host_client) struct KeyBinding {
    purpose: String,
    pub(super) account: String,
    pub(super) route: OrdinaryRoute,
}
impl KeyBinding {
    pub(in crate::host_client) fn new(id: u64, route: OrdinaryRoute) -> Result<Self, Failure> {
        if id == 0 {
            return Err(Failure::BindingMismatch);
        }
        route.validate()?;
        Ok(Self {
            purpose: PURPOSE.into(),
            account: format!("github:{id}"),
            route,
        })
    }
    fn name(&self, extension: &str) -> Result<PathBuf, KeyError> {
        self.route
            .validate()
            .map_err(|_| KeyError::BindingMismatch)?;
        let id = self
            .account
            .strip_prefix("github:")
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v != 0)
            .ok_or(KeyError::BindingMismatch)?;
        if self.purpose != PURPOSE || self.account != format!("github:{id}") {
            return Err(KeyError::BindingMismatch);
        }
        let bytes = serde_json::to_vec(self).map_err(|_| KeyError::Storage)?;
        Ok(PathBuf::from(format!(
            "ordinary-v1-{}.{extension}",
            crate::owner_auth::hex(&Sha256::digest(bytes))
        )))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u16,
    binding: KeyBinding,
    reference: String,
    ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    originals: Option<OriginalsAnchor>,
}

#[derive(Clone)]
pub struct OrdinaryKeyIndex {
    pub(in crate::host_client) index: ReceiptIndex,
}
pub(in crate::host_client) struct NativeKey {
    pub(super) key: Arc<OrdinaryDeviceKey>,
    pub(super) reference: SecretReference,
    pub(super) index: OrdinaryKeyIndex,
    pub(super) vault: Arc<dyn SecretStore>,
}
struct Lease(File);
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

impl OrdinaryKeyIndex {
    #[cfg(feature = "relay")]
    pub fn native() -> Result<Self, Failure> {
        Ok(Self {
            index: ReceiptIndex::native().map_err(|_| key_failure(KeyError::Storage))?,
        })
    }
    pub fn in_app_sandbox(root: &Path) -> Result<Self, Failure> {
        Ok(Self {
            index: ReceiptIndex::in_app_sandbox(root)
                .map_err(|_| key_failure(KeyError::Storage))?,
        })
    }
    #[cfg(unix)]
    pub fn in_native_app_sandbox(root: Arc<festerm_secret_store::NativeAppPrivateRoot>) -> Self {
        Self {
            index: ReceiptIndex::in_native_app_sandbox(root),
        }
    }
    fn lease(directory: &File, binding: &KeyBinding) -> Result<Lease, KeyError> {
        let file = storage::open_trusted_lock(directory, &binding.name("lock")?)
            .map_err(|error| storage_failure(KeyStage::LockOpen, error))?;
        file.try_lock_exclusive().map_err(|e| {
            if fs2::lock_contended_error().raw_os_error() == e.raw_os_error() {
                KeyError::Busy
            } else {
                storage_failure(KeyStage::LockAcquire, e)
            }
        })?;
        Ok(Lease(file))
    }
    fn read(directory: &File, binding: &KeyBinding) -> Result<Record, KeyError> {
        let bytes = storage::read_private(directory, &binding.name("json")?, 8192)
            .map_err(|error| storage_failure(KeyStage::RecordRead, error))?;
        let record: Record = serde_json::from_slice(&bytes)
            .map_err(|_| KeyError::Storage.at_stage(KeyStage::RecordDecode))?;
        if record.version != 1 || record.binding != *binding {
            return Err(KeyError::BindingMismatch);
        }
        SecretReference::parse(&record.reference).map_err(|_| KeyError::Storage)?;
        Ok(record)
    }
    fn replace(directory: &File, binding: &KeyBinding, record: &Record) -> Result<(), KeyError> {
        let bytes = serde_json::to_vec(record).map_err(|_| KeyError::Storage)?;
        storage::replace_private(directory, &binding.name("json")?, &bytes)
            .map_err(|error| storage_failure(KeyStage::RecordReplace, error))
    }
    pub(super) fn prepare(
        &self,
        binding: &KeyBinding,
        guard: &OrdinaryAccountGuard,
    ) -> Result<NativeKey, Failure> {
        guard.check()?;
        let vault = native_store().map_err(|e| key_failure(e.into()))?;
        self.prepare_with(Arc::from(vault), binding, guard)
            .map_err(key_failure)
    }
    pub(super) fn prepare_with(
        &self,
        vault: Arc<dyn SecretStore>,
        binding: &KeyBinding,
        guard: &OrdinaryAccountGuard,
    ) -> Result<NativeKey, KeyError> {
        guard
            .check()
            .map_err(|_| KeyError::BindingMismatch.at_stage(KeyStage::AccountGuard))?;
        let directory = self
            .index
            .directory_io(true)
            .map_err(|error| storage_failure(KeyStage::Directory, error))?;
        let _lease = Self::lease(&directory, binding)?;
        let mut record = match storage::read_private(&directory, &binding.name("json")?, 8192) {
            Ok(bytes) => {
                let r: Record = serde_json::from_slice(&bytes)
                    .map_err(|_| KeyError::Storage.at_stage(KeyStage::RecordDecode))?;
                if r.version != 1 || r.binding != *binding {
                    return Err(KeyError::BindingMismatch.at_stage(KeyStage::RecordBinding));
                }
                r
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let r = Record {
                    version: 1,
                    binding: binding.clone(),
                    reference: SecretReference::generate().to_persisted_string(),
                    ready: false,
                    originals: None,
                };
                guard
                    .check()
                    .map_err(|_| KeyError::BindingMismatch.at_stage(KeyStage::AccountGuard))?;
                let bytes = serde_json::to_vec(&r)
                    .map_err(|_| KeyError::Storage.at_stage(KeyStage::RecordEncode))?;
                ReceiptIndex::publish_new(&directory, &binding.name("json")?, &bytes)
                    .map_err(|error| storage_failure(KeyStage::RecordPublish, error))?;
                r
            }
            Err(error) => return Err(storage_failure(KeyStage::RecordRead, error)),
        };
        let reference = SecretReference::parse(&record.reference)
            .map_err(|_| KeyError::Storage.at_stage(KeyStage::ReferenceDecode))?;
        guard
            .check()
            .map_err(|_| KeyError::BindingMismatch.at_stage(KeyStage::AccountGuard))?;
        let seed = match vault.get(&reference) {
            Ok(secret) => decode_key(&secret, binding)
                .map_err(|error| error.at_stage(KeyStage::SeedBinding))?,
            Err(SecretStoreError::Missing) if !record.ready => {
                let mut seed = Zeroizing::new([0; 32]);
                getrandom::fill(seed.as_mut())
                    .map_err(|error| KeyError::random_failure(error.raw_os_error()))?;
                let mut payload = Zeroizing::new(KEY_RECORD.to_vec());
                payload.extend_from_slice(seed.as_ref());
                serde_json::to_writer(&mut *payload, binding)
                    .map_err(|_| KeyError::Storage.at_stage(KeyStage::RecordEncode))?;
                guard
                    .check()
                    .map_err(|_| KeyError::BindingMismatch.at_stage(KeyStage::AccountGuard))?;
                vault.create_reserved(&reference, &SecretBytes::copy_from_slice(&payload))?;
                seed
            }
            Err(e) => return Err(e.into()),
        };
        guard
            .check()
            .map_err(|_| KeyError::BindingMismatch.at_stage(KeyStage::AccountGuard))?;
        if !record.ready {
            record.ready = true;
            Self::replace(&directory, binding, &record)?;
        }
        guard
            .check()
            .map_err(|_| KeyError::BindingMismatch.at_stage(KeyStage::AccountGuard))?;
        Ok(NativeKey {
            key: Arc::new(OrdinaryDeviceKey::from_seed(&seed)),
            reference,
            index: self.clone(),
            vault,
        })
    }

    pub(in crate::host_client) fn with_originals<T>(
        &self,
        vault: &dyn SecretStore,
        binding: &KeyBinding,
        key: &SecretReference,
        initialize: bool,
        guard: &OrdinaryAccountGuard,
        action: impl FnOnce(
            &SecretReference,
            bool,
            &mut dyn OriginalCommitter,
        ) -> Result<T, approvals::Error>,
    ) -> Result<T, approvals::Error> {
        guard
            .check()
            .map_err(|_| approvals::Error::BindingMismatch)?;
        let directory = self.index.directory(false)?;
        let _lease = Self::lease(&directory, binding).map_err(|_| approvals::Error::Storage)?;
        let mut record = Self::read(&directory, binding).map_err(|_| approvals::Error::Storage)?;
        if !record.ready || record.reference != key.to_persisted_string() {
            return Err(approvals::Error::BindingMismatch);
        }
        guard
            .check()
            .map_err(|_| approvals::Error::BindingMismatch)?;
        let secret = vault.get(key).map_err(|_| approvals::Error::Storage)?;
        guard
            .check()
            .map_err(|_| approvals::Error::BindingMismatch)?;
        decode_key(&secret, binding).map_err(|_| approvals::Error::BindingMismatch)?;
        let newly_reserved = record.originals.is_none();
        if newly_reserved {
            if !initialize {
                return Err(approvals::Error::Incomplete);
            }
            record.originals = Some(OriginalsAnchor {
                reference: SecretReference::generate().to_persisted_string(),
                ready: false,
                pending: None,
            });
            Self::replace(&directory, binding, &record).map_err(|_| approvals::Error::Storage)?;
        }
        let anchor = record.originals.as_ref().ok_or(approvals::Error::Storage)?;
        let reference =
            SecretReference::parse(&anchor.reference).map_err(|_| approvals::Error::Storage)?;
        let ready = anchor.ready;
        guard
            .check()
            .map_err(|_| approvals::Error::BindingMismatch)?;
        let result = action(
            &reference,
            newly_reserved,
            &mut Commit {
                directory: &directory,
                binding,
                record: &mut record,
            },
        )?;
        guard
            .check()
            .map_err(|_| approvals::Error::BindingMismatch)?;
        if !ready {
            record
                .originals
                .as_mut()
                .ok_or(approvals::Error::Storage)?
                .ready = true;
            Self::replace(&directory, binding, &record).map_err(|_| approvals::Error::Storage)?;
        }
        guard
            .check()
            .map_err(|_| approvals::Error::BindingMismatch)?;
        Ok(result)
    }
}
fn decode_key(secret: &SecretBytes, binding: &KeyBinding) -> Result<Zeroizing<[u8; 32]>, KeyError> {
    secret.with_bytes(|bytes| {
        if bytes.len() > 8192 || !bytes.starts_with(KEY_RECORD) {
            return Err(KeyError::BindingMismatch);
        }
        let bytes = &bytes[KEY_RECORD.len()..];
        let seed = bytes
            .get(..32)
            .and_then(|b| b.try_into().ok())
            .ok_or(KeyError::BindingMismatch)?;
        let captured: KeyBinding =
            serde_json::from_slice(bytes.get(32..).ok_or(KeyError::BindingMismatch)?)
                .map_err(|_| KeyError::BindingMismatch)?;
        if captured != *binding {
            return Err(KeyError::BindingMismatch);
        }
        Ok(Zeroizing::new(seed))
    })
}
struct Commit<'a> {
    directory: &'a File,
    binding: &'a KeyBinding,
    record: &'a mut Record,
}
impl OriginalCommitter for Commit<'_> {
    fn pending(&self) -> Option<&str> {
        self.record
            .originals
            .as_ref()
            .and_then(|a| a.pending.as_deref())
    }
    fn mark(&mut self, pending: Option<String>) -> Result<(), approvals::Error> {
        self.record
            .originals
            .as_mut()
            .ok_or(approvals::Error::Storage)?
            .pending = pending;
        OrdinaryKeyIndex::replace(self.directory, self.binding, self.record)
            .map_err(|_| approvals::Error::Storage)
    }
}

#[cfg(test)]
#[path = "key_tests.rs"]
mod tests;
