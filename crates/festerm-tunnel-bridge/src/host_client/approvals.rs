//! App-local foreground owner receipts. Neither the index nor its opaque
//! references are portable recipe metadata or proof of host-local enrollment.

use crate::owner_auth::{ClientOwnerKey, OwnerBinding, OwnerKeyError};
use festerm_host_protocol::{HostId, Validate};
use festerm_secret_store::{native_store, SecretReference, SecretStore, SecretStoreError};
#[cfg(feature = "relay")]
use festerm_sessiond::private_storage as storage;
#[cfg(all(not(feature = "relay"), unix))]
#[path = "approval_storage.rs"]
mod storage;
#[cfg(all(not(feature = "relay"), not(unix)))]
mod storage {
    use std::{fs::File, io, path::Path};
    fn unsupported<T>() -> io::Result<T> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub(super) fn trusted_directory(_: &Path, _: bool) -> io::Result<File> {
        unsupported()
    }
    pub(super) fn private_directory(_: &Path, _: bool) -> io::Result<File> {
        unsupported()
    }
    pub(super) fn open_trusted_lock(_: &File, _: &Path) -> io::Result<File> {
        unsupported()
    }
    pub(super) fn read_private(_: &File, _: &Path, _: usize) -> io::Result<Vec<u8>> {
        unsupported()
    }
    pub(super) fn publish_new(_: &File, _: &Path, _: &[u8]) -> io::Result<()> {
        unsupported()
    }
    pub(super) fn replace_private(_: &File, _: &Path, _: &[u8]) -> io::Result<()> {
        unsupported()
    }
}
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

const MAX_RECEIPT_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Missing,
    Incomplete,
    Busy,
    Storage,
    BindingMismatch,
    OwnerKey(OwnerKeyError),
}

impl Error {
    pub fn message(self) -> &'static str {
        match self {
            Self::Missing => "No protected owner receipt exists for this host and account. Review the host-local approval prerequisite first.",
            Self::Incomplete => "Owner-key preparation is incomplete. Explicitly resume the same foreground preparation; authentication will not generate or replace a key.",
            Self::Busy => "This host's protected owner receipt is busy. Retry the same foreground action.",
            Self::Storage => "The protected owner receipt could not be read or published. No ordinary configuration or plaintext fallback was used.",
            Self::BindingMismatch => "The host, account, installation, principal or confirmed fingerprint does not match the protected receipt. No key was replaced.",
            Self::OwnerKey(OwnerKeyError::Vault(SecretStoreError::Missing)) => "The protected receipt's native owner key is missing. A ready receipt never generates a replacement; reconcile the same host's explicit owner approval.",
            Self::OwnerKey(OwnerKeyError::Vault(SecretStoreError::LockedOrUnavailable)) => "The native owner vault is locked or unavailable. Unlock it and retry the same foreground action; no fallback or replacement was attempted.",
            Self::OwnerKey(OwnerKeyError::Vault(SecretStoreError::Unsupported)) => "Native owner storage is unsupported on this platform. No plaintext or headless-file fallback is allowed.",
            Self::OwnerKey(_) => "The native owner key operation failed for this exact protected binding. No fallback or automatic replacement was attempted.",
        }
    }
}

/// Created only after independent foreground review of the full host-local
/// prerequisite and fingerprint. Parsing public metadata alone is insufficient.
pub struct ReviewedOwner {
    binding: OwnerBinding,
}

impl ReviewedOwner {
    pub fn confirm(
        prerequisite: &str,
        expected_host: &HostId,
        expected_account: &str,
        independently_confirmed_fingerprint: &str,
    ) -> Result<Self, Error> {
        if prerequisite.len() > MAX_RECEIPT_BYTES {
            return Err(Error::BindingMismatch);
        }
        let binding: OwnerBinding =
            serde_json::from_str(prerequisite).map_err(|_| Error::BindingMismatch)?;
        binding.validate().map_err(|_| Error::BindingMismatch)?;
        if &binding.host.host != expected_host
            || binding.account != expected_account
            || binding.host.host_key_fingerprint != independently_confirmed_fingerprint
        {
            return Err(Error::BindingMismatch);
        }
        Ok(Self { binding })
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    binding: OwnerBinding,
    reference: String,
    ready: bool,
}

/// A reopened local receipt still requires an explicit connection action and
/// fresh owner authorization on the selected host. It cannot grant permissions.
pub struct Receipt {
    binding: OwnerBinding,
    reference: SecretReference,
    public_key: [u8; 32],
}

impl Receipt {
    pub fn binding(&self) -> &OwnerBinding {
        &self.binding
    }

    pub fn reference(&self) -> SecretReference {
        self.reference.duplicate_for_transport()
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.public_key
    }
}

/// The root must be selected by the application, never by imported recipes,
/// remote data, or a credential reference supplied by a user.
pub struct ReceiptIndex {
    root: PathBuf,
}

struct Lease(File);
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

impl ReceiptIndex {
    /// App-selected private sandbox, never a path from imported metadata.
    pub fn in_app_sandbox(root: &Path) -> Result<Self, Error> {
        // The native composition root, never imported metadata, supplies this
        // existing application directory. Normalize OS aliases such as iOS /var
        // once; all receipt traversal beneath it remains no-follow.
        let root = root.canonicalize().map_err(storage_error)?;
        storage::trusted_directory(&root, false).map_err(storage_error)?;
        Ok(Self {
            root: root.join("host-client-approvals"),
        })
    }

    #[cfg(feature = "relay")]
    pub fn native() -> Result<Self, Error> {
        let runtime = festerm_sessiond::runtime_root().map_err(|_| Error::Storage)?;
        let root = runtime
            .parent()
            .ok_or(Error::Storage)?
            .join("host-client-approvals");
        Ok(Self { root })
    }

    /// This action may create native signing material, but never contacts or
    /// enrolls the host. A durable pending reference precedes the vault write.
    pub fn prepare(&self, reviewed: ReviewedOwner) -> Result<Receipt, Error> {
        let vault = native_store().map_err(|error| Error::OwnerKey(error.into()))?;
        self.prepare_with(vault.as_ref(), reviewed)
    }

    /// Never generates keys, selects arbitrary vault entries, or repairs a
    /// pending record. The reference comes solely from this local private index.
    pub fn load(&self, host: &HostId, account: &str) -> Result<Receipt, Error> {
        let vault = native_store().map_err(|error| Error::OwnerKey(error.into()))?;
        self.load_with(vault.as_ref(), host, account)
    }

    fn directory(&self, create: bool) -> Result<File, Error> {
        if create {
            ensure_parent(self.root.parent().ok_or(Error::Storage)?, 32).map_err(storage_error)?;
        }
        storage::private_directory(&self.root, create).map_err(storage_error)
    }

    fn lease(directory: &File, host: &HostId, account: &str) -> Result<Lease, Error> {
        let name = Self::name(host, account, "lock")?;
        let file = storage::open_trusted_lock(directory, &name).map_err(storage_error)?;
        file.try_lock_exclusive().map_err(|error| {
            if fs2::lock_contended_error()
                .raw_os_error()
                .is_some_and(|code| error.raw_os_error() == Some(code))
            {
                Error::Busy
            } else {
                Error::Storage
            }
        })?;
        Ok(Lease(file))
    }

    fn name(host: &HostId, account: &str, extension: &str) -> Result<PathBuf, Error> {
        let id = account
            .strip_prefix("github:")
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 20
                    && id.bytes().all(|byte| byte.is_ascii_digit())
                    && !id.starts_with('0')
            })
            .ok_or(Error::BindingMismatch)?;
        Ok(PathBuf::from(format!("{host}-github-{id}.{extension}")))
    }

    fn read(directory: &File, host: &HostId, account: &str) -> Result<Record, Error> {
        let bytes = storage::read_private(
            directory,
            &Self::name(host, account, "json")?,
            MAX_RECEIPT_BYTES,
        )
        .map_err(storage_error)?;
        let record: Record = serde_json::from_slice(&bytes).map_err(|_| Error::Storage)?;
        record.binding.validate().map_err(|_| Error::Storage)?;
        if record.version != 1
            || &record.binding.host.host != host
            || record.binding.account != account
        {
            return Err(Error::BindingMismatch);
        }
        SecretReference::parse(&record.reference).map_err(|_| Error::Storage)?;
        Ok(record)
    }

    fn receipt(vault: &dyn SecretStore, record: &Record) -> Result<Receipt, Error> {
        let reference = SecretReference::parse(&record.reference).map_err(|_| Error::Storage)?;
        let public_key =
            ClientOwnerKey::open(vault, reference.duplicate_for_transport(), &record.binding)
                .map_err(Error::OwnerKey)?
                .public_key();
        Ok(Receipt {
            binding: record.binding.clone(),
            reference,
            public_key,
        })
    }

    fn prepare_with(
        &self,
        vault: &dyn SecretStore,
        reviewed: ReviewedOwner,
    ) -> Result<Receipt, Error> {
        let directory = self.directory(true)?;
        let binding = reviewed.binding;
        let _lease = Self::lease(&directory, &binding.host.host, &binding.account)?;
        let name = Self::name(&binding.host.host, &binding.account, "json")?;
        let mut record = match Self::read(&directory, &binding.host.host, &binding.account) {
            Ok(record) if record.binding == binding => record,
            Ok(_) => return Err(Error::BindingMismatch),
            Err(Error::Missing) => {
                let record = Record {
                    version: 1,
                    binding,
                    reference: SecretReference::generate().to_persisted_string(),
                    ready: false,
                };
                let bytes = serde_json::to_vec(&record).map_err(|_| Error::Storage)?;
                Self::publish_new(&directory, &name, &bytes).map_err(storage_error)?;
                record
            }
            Err(error) => return Err(error),
        };
        let receipt = match Self::receipt(vault, &record) {
            Ok(receipt) => receipt,
            Err(Error::OwnerKey(OwnerKeyError::Vault(SecretStoreError::Missing)))
                if !record.ready =>
            {
                let reference =
                    SecretReference::parse(&record.reference).map_err(|_| Error::Storage)?;
                let key = ClientOwnerKey::generate_reserved(vault, reference, &record.binding)
                    .map_err(Error::OwnerKey)?;
                Receipt {
                    binding: record.binding.clone(),
                    reference: key.reference(),
                    public_key: key.public_key(),
                }
            }
            Err(error) => return Err(error),
        };
        if !record.ready {
            record.ready = true;
            let bytes = serde_json::to_vec(&record).map_err(|_| Error::Storage)?;
            storage::replace_private(&directory, &name, &bytes).map_err(storage_error)?;
        }
        Ok(receipt)
    }

    #[cfg(feature = "relay")]
    fn publish_new(directory: &File, name: &Path, bytes: &[u8]) -> io::Result<()> {
        crate::setup_storage::publish_new_private(directory, name, bytes)
    }

    #[cfg(not(feature = "relay"))]
    fn publish_new(directory: &File, name: &Path, bytes: &[u8]) -> io::Result<()> {
        storage::publish_new(directory, name, bytes)
    }

    fn load_with(
        &self,
        vault: &dyn SecretStore,
        host: &HostId,
        account: &str,
    ) -> Result<Receipt, Error> {
        let directory = self.directory(false)?;
        let _lease = Self::lease(&directory, host, account)?;
        let record = Self::read(&directory, host, account)?;
        if !record.ready {
            return Err(Error::Incomplete);
        }
        Self::receipt(vault, &record)
    }
}

fn storage_error(error: io::Error) -> Error {
    if error.kind() == io::ErrorKind::NotFound {
        Error::Missing
    } else {
        Error::Storage
    }
}

fn ensure_parent(path: &Path, remaining: u8) -> io::Result<()> {
    match storage::trusted_directory(path, false) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound && remaining > 0 => {
            ensure_parent(
                path.parent()
                    .ok_or_else(|| io::Error::other("receipt.parent"))?,
                remaining - 1,
            )?;
            storage::private_directory(path, true).map(|_| ())
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(test, any(feature = "relay", unix)))]
#[path = "approval_tests.rs"]
mod tests;
