//! Explicit host-local enrollment, separate from portable setup and bearer access.
//! A compact selected-store anchor fences policy revisions. Prepared private
//! records allow validated forward recovery, never startup/config adoption.
use crate::{
    lifecycle::{Origin, Phase, Snapshot, Store},
    protected_state,
};
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use festerm_host_protocol::{Capabilities, ContractError, Epoch, PrincipalId, Validate};
use festerm_secret_store::{native_store, SecretBytes, SecretReference, SecretStore};
use festerm_sessiond::private_storage as storage;
use festerm_tunnel_bridge::host_identity::HostIdentity;
use festerm_tunnel_bridge::owner_auth::{
    validate_owner_public_key, OwnerBinding, OwnerPolicy, OwnerVerifier, PinnedHost, MAX_OWNER_KEYS,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, io, path::Path, sync::Arc};

const RECORD: &str = "owner-enrollment.json";
const ANCHOR: &str = "owner-enrollment-vault-ref";
const PREPARED: &str = "owner-enrollment-prepared.json";
const INITIALIZING: &str = "owner-enrollment-initializing-ref";
const ANCHOR_MAGIC: &[u8] = b"fesTerm owner policy anchor v2\0";
const LIMIT: usize = 16 * 1024;
const MAX_HISTORICAL_KEYS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OwnerStorage {
    NativeVault,
    HeadlessProtectedFile,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InitializationReceipt {
    version: u16,
    storage: OwnerStorage,
    origin: Origin,
    installation: String,
    reference: String,
}

fn initializing_reference(
    directory: &File,
    installation: &Snapshot,
    selected: OwnerStorage,
) -> io::Result<SecretReference> {
    let bytes = storage::read_private(directory, Path::new(INITIALIZING), 512)?;
    let receipt: InitializationReceipt = serde_json::from_slice(&bytes)
        .map_err(|_| refused("owner.invalid_initialization_receipt"))?;
    if receipt.version != 1
        || receipt.storage != selected
        || receipt.origin != installation.origin
        || receipt.installation != installation.installation
    {
        return Err(refused("owner.storage_selection_changed_requires_review"));
    }
    SecretReference::parse(&receipt.reference).map_err(vault_error)
}

fn refused(message: &'static str) -> io::Error {
    io::Error::other(message)
}

fn vault_error(error: festerm_secret_store::SecretStoreError) -> io::Error {
    refused(match error {
        festerm_secret_store::SecretStoreError::LockedOrUnavailable => {
            "owner.vault_locked_or_unavailable_unlock_native_store_and_retry"
        }
        festerm_secret_store::SecretStoreError::Missing => {
            "owner.vault_anchor_missing_local_reconciliation_required"
        }
        festerm_secret_store::SecretStoreError::Unsupported => {
            "owner.native_vault_unsupported_no_plaintext_fallback"
        }
        _ => "owner.native_vault_failure_no_fallback",
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrolledKey {
    pub public_key: [u8; 32],
    pub revoked: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentSnapshot {
    version: u16,
    pub storage: OwnerStorage,
    pub origin: Origin,
    pub binding: OwnerBinding,
    identity_file: std::path::PathBuf,
    pub revision: Epoch,
    installation_epoch: u64,
    pub keys: Vec<EnrolledKey>,
    pub remote_access: bool,
    pub capabilities: Capabilities,
}

impl EnrollmentSnapshot {
    fn validate(&self, installation: &Snapshot) -> io::Result<()> {
        self.binding.validate()?;
        self.capabilities.validate_permissions()?;
        if self.capabilities.issuance_epoch.is_some() {
            return Err(refused("owner.runtime_issuer_is_not_persistent_consent"));
        }
        if self.version != 1
            || self.binding.installation != installation.installation
            || self.origin != installation.origin
            || self.storage == OwnerStorage::HeadlessProtectedFile
                && self.origin != Origin::Headless
            || self.installation_epoch > installation.epoch
            || self.keys.len() > MAX_HISTORICAL_KEYS
            || self.keys.iter().filter(|key| !key.revoked).count() > MAX_OWNER_KEYS
        {
            return Err(refused("owner.foreign_installation_or_policy"));
        }
        if let Some(binding) = &installation.binding {
            if binding.host_id != self.binding.host.host.as_str()
                || binding.account != self.binding.account
                || binding.host_key_fingerprint != self.binding.host.host_key_fingerprint
            {
                return Err(refused(
                    "owner.host_or_account_changed_reconfirmation_required",
                ));
            }
        }
        for (index, key) in self.keys.iter().enumerate() {
            validate_key(&key.public_key)?;
            if self.keys[..index]
                .iter()
                .any(|old| old.public_key == key.public_key)
            {
                return Err(refused("owner.duplicate_key"));
            }
        }
        Ok(())
    }

    fn verifier(&self, installation: &Snapshot) -> io::Result<OwnerVerifier> {
        let keys: Vec<_> = self
            .keys
            .iter()
            .filter(|key| !key.revoked)
            .map(|key| key.public_key)
            .collect();
        let mut capabilities = self.capabilities;
        capabilities.creation_allowed &= installation.settings.allow_creation;
        let policy = OwnerPolicy {
            host: self.binding.host.clone(),
            principal: self.binding.principal.clone(),
            host_epoch: Epoch::new(
                installation
                    .epoch
                    .checked_add(1)
                    .ok_or_else(|| refused("owner.epoch_exhausted"))?,
            )?,
            grant_epoch: self.revision,
            remote_access: self.remote_access && installation.phase == Phase::Provisioned,
            capabilities,
        };
        OwnerVerifier::from_protected_permissions(policy, &keys).map_err(Into::into)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PolicyAnchor {
    revision: Epoch,
    digest: [u8; 32],
    previous: [u8; 32],
}

impl PolicyAnchor {
    fn for_record(record: &EnrollmentSnapshot, previous: [u8; 32]) -> io::Result<Self> {
        Ok(Self {
            revision: record.revision,
            digest: record_digest(record)?,
            previous,
        })
    }

    fn encode(&self) -> Vec<u8> {
        let mut bytes = ANCHOR_MAGIC.to_vec();
        bytes.extend_from_slice(&self.revision.get().to_be_bytes());
        bytes.extend_from_slice(&self.digest);
        bytes.extend_from_slice(&self.previous);
        bytes
    }

    fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() != ANCHOR_MAGIC.len() + 72 || !bytes.starts_with(ANCHOR_MAGIC) {
            return Err(refused(
                "owner.policy_anchor_version_or_encoding_requires_review",
            ));
        }
        let bytes = &bytes[ANCHOR_MAGIC.len()..];
        Ok(Self {
            revision: Epoch::new(u64::from_be_bytes(bytes[..8].try_into().unwrap()))?,
            digest: bytes[8..40].try_into().unwrap(),
            previous: bytes[40..72].try_into().unwrap(),
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedPolicy {
    previous: [u8; 32],
    next: EnrollmentSnapshot,
}

fn record_bytes(record: &EnrollmentSnapshot) -> io::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(record).map_err(|_| refused("owner.invalid_policy"))?;
    if bytes.len() > LIMIT {
        return Err(refused("owner.policy_limit"));
    }
    Ok(bytes)
}

fn record_digest(record: &EnrollmentSnapshot) -> io::Result<[u8; 32]> {
    Ok(Sha256::digest(record_bytes(record)?).into())
}

fn validate_identity(
    directory: &File,
    record: &EnrollmentSnapshot,
    installation: &Snapshot,
) -> io::Result<()> {
    record.validate(installation)?;
    let identity = HostIdentity::load_from_private_directory(directory, &record.identity_file)?;
    if identity.host_id() != record.binding.host.host.as_str()
        || identity.fingerprint()? != record.binding.host.host_key_fingerprint
    {
        return Err(refused(
            "owner.host_identity_changed_reconfirmation_required",
        ));
    }
    Ok(())
}

fn validate_key(key: &[u8; 32]) -> io::Result<()> {
    validate_owner_public_key(key).map_err(Into::into)
}

/// Fingerprint of the raw enrolled Ed25519 public key (not an SSH host key).
pub fn owner_key_fingerprint(key: &[u8; 32]) -> io::Result<String> {
    validate_key(key)?;
    Ok(format!(
        "ED25519-SHA256:{}",
        STANDARD_NO_PAD.encode(Sha256::digest(key))
    ))
}

/// Only local composition roots may call these APIs. No network request or
/// configuration loader may initialize/enroll/rotate/approve this policy.
pub struct OwnerEnrollment<'a> {
    installation: &'a Store,
    vault: Box<dyn SecretStore>,
    storage: OwnerStorage,
}

/// Scoped protected policy guard for the backend's atomic admission callback.
/// Drop before native startup waits or network I/O; reacquire for final effects.
pub struct CurrentOwnerPolicy {
    _lease: protected_state::Lease,
    verifier: OwnerVerifier,
}

pub(crate) struct MigrationOwnerPolicy {
    pub(crate) _lease: protected_state::Lease,
    pub(crate) enrollment: EnrollmentSnapshot,
    pub(crate) installation: Snapshot,
}

pub(crate) struct PrivateSessionPolicy {
    _lease: protected_state::Lease,
    pub(crate) enrollment: EnrollmentSnapshot,
    pub(crate) installation: Snapshot,
}

impl CurrentOwnerPolicy {
    pub fn verifier(&self) -> &OwnerVerifier {
        &self.verifier
    }
}

/// Fresh selected-store authority for runtime admission, never a cached verifier.
/// The returned lease must be dropped before network or native startup waits.
pub struct CurrentOwnerPolicySource {
    installation: Arc<Store>,
    storage: OwnerStorage,
}

impl CurrentOwnerPolicySource {
    pub fn new(installation: Arc<Store>, storage: OwnerStorage) -> Self {
        Self {
            installation,
            storage,
        }
    }

    pub fn current_policy(&self) -> Result<CurrentOwnerPolicy, ContractError> {
        self.installation
            .owner_enrollment_with_storage(self.storage)
            .and_then(|enrollment| enrollment.current_policy())
            .map_err(|error| {
                if error.kind() == io::ErrorKind::WouldBlock {
                    ContractError::Busy
                } else {
                    ContractError::Denied
                }
            })
    }
}

#[cfg(feature = "host-runtime")]
impl festerm_tunnel_bridge::host_control::PolicyGuard for CurrentOwnerPolicy {
    fn verifier(&self) -> &OwnerVerifier {
        CurrentOwnerPolicy::verifier(self)
    }
}

#[cfg(feature = "host-runtime")]
impl festerm_tunnel_bridge::host_control::ProtectedPolicySource for CurrentOwnerPolicySource {
    type Guard = CurrentOwnerPolicy;

    fn current_policy(&self) -> Result<Self::Guard, ContractError> {
        CurrentOwnerPolicySource::current_policy(self)
    }
}

impl Store {
    pub fn owner_enrollment(&self) -> io::Result<OwnerEnrollment<'_>> {
        self.owner_enrollment_with_storage(OwnerStorage::NativeVault)
    }

    /// Explicit local selection; never retry a native-vault error using files.
    pub fn owner_enrollment_with_storage(
        &self,
        selected: OwnerStorage,
    ) -> io::Result<OwnerEnrollment<'_>> {
        self.with_locked(|directory, installation| {
            if selected == OwnerStorage::HeadlessProtectedFile
                && installation.origin != Origin::Headless
            {
                return Err(refused("owner.file_storage_is_headless_only"));
            }
            match initializing_reference(directory, installation, selected) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            match storage::read_private(directory, Path::new(RECORD), LIMIT) {
                Ok(bytes) => {
                    let record: EnrollmentSnapshot = serde_json::from_slice(&bytes)
                        .map_err(|_| refused("owner.invalid_policy"))?;
                    if record.storage != selected {
                        return Err(refused("owner.storage_selection_changed_requires_review"));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let vault: Box<dyn SecretStore> = match selected {
                OwnerStorage::NativeVault => native_store().map_err(vault_error)?,
                OwnerStorage::HeadlessProtectedFile => Box::new(
                    festerm_tunnel_bridge::owner_auth::ExplicitHeadlessFileStore::select(
                        directory,
                        &installation.installation,
                    )?,
                ),
            };
            Ok(OwnerEnrollment {
                installation: self,
                vault,
                storage: selected,
            })
        })
    }
}

impl OwnerEnrollment<'_> {
    pub(crate) fn private_session_policy(&self, state: &Path) -> io::Result<PrivateSessionPolicy> {
        self.installation
            .with_owned_lease(|directory, installation, lease| {
                if self.storage != OwnerStorage::NativeVault
                    || installation.origin != Origin::Desktop
                {
                    return Err(refused(
                        "owner.private_sharing_requires_desktop_native_vault",
                    ));
                }
                let selected = storage::private_directory(state, false)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let actual = directory.metadata()?;
                    let current = selected.metadata()?;
                    if (actual.dev(), actual.ino()) != (current.dev(), current.ino()) {
                        return Err(refused("owner.private_sharing_state_replaced"));
                    }
                }
                #[cfg(windows)]
                if !festerm_windows_security::same_file_identity(directory, &selected)? {
                    return Err(refused("owner.private_sharing_state_replaced"));
                }
                let (_, enrollment) = self.read(directory, installation)?;
                Ok(PrivateSessionPolicy {
                    _lease: lease,
                    enrollment,
                    installation: installation.clone(),
                })
            })
    }

    fn storage_error(&self, error: festerm_secret_store::SecretStoreError) -> io::Error {
        match self.storage {
            OwnerStorage::NativeVault => vault_error(error),
            OwnerStorage::HeadlessProtectedFile => refused(match error {
                festerm_secret_store::SecretStoreError::Missing => {
                    "owner.selected_headless_private_anchor_missing_reconciliation_required"
                }
                _ => "owner.selected_headless_private_store_unavailable_check_permissions_and_path",
            }),
        }
    }

    /// Explicit local approval of an exact installation, pinned host and account.
    /// This generates an owner principal locally; account strings confer no role.
    /// No remote access, creation permission or key is granted by initialization.
    pub fn initialize(
        &self,
        identity_file: &Path,
        account: String,
    ) -> io::Result<EnrollmentSnapshot> {
        self.initialize_with(identity_file, account, |directory, reference| {
            protected_state::publish_new(directory, Path::new(ANCHOR), reference)
        })
    }

    /// Fence a foreground setup completion against its original consent epoch.
    pub fn initialize_at_epoch(
        &self,
        identity_file: &Path,
        account: String,
        expected_epoch: u64,
    ) -> io::Result<EnrollmentSnapshot> {
        self.initialize_fenced_with(
            identity_file,
            account,
            Some(expected_epoch),
            None,
            |directory, reference| {
                protected_state::publish_new(directory, Path::new(ANCHOR), reference)
            },
        )
    }

    fn initialize_with(
        &self,
        identity_file: &Path,
        account: String,
        publish_reference: impl FnOnce(&File, &[u8]) -> io::Result<()>,
    ) -> io::Result<EnrollmentSnapshot> {
        self.initialize_fenced_with(identity_file, account, None, None, publish_reference)
    }

    /// Separate explicit owner initialization for an exact private migration
    /// reservation. It grants no key, access, provisioning or remote capability.
    pub fn initialize_for_migration(
        &self,
        operation: &str,
        account: String,
    ) -> io::Result<EnrollmentSnapshot> {
        self.initialize_fenced_with(
            Path::new("host-identity"),
            account,
            None,
            Some(operation),
            |directory, reference| {
                protected_state::publish_new(directory, Path::new(ANCHOR), reference)
            },
        )
    }

    fn initialize_fenced_with(
        &self,
        identity_file: &Path,
        account: String,
        expected_epoch: Option<u64>,
        migration: Option<&str>,
        publish_reference: impl FnOnce(&File, &[u8]) -> io::Result<()>,
    ) -> io::Result<EnrollmentSnapshot> {
        self.installation.with_locked(|directory, installation| {
            if expected_epoch.is_some_and(|epoch| installation.epoch != epoch) {
                return Err(refused("owner.stale_operation"));
            }
            let provisioned = installation.binding.as_ref().filter(|binding| {
                installation.phase == Phase::Provisioned && binding.account == account
            });
            if let Some(operation) = migration {
                crate::owned_migration::check_owner_initialization(
                    directory,
                    installation,
                    operation,
                    &account,
                )?;
            } else if provisioned.is_none() {
                return Err(refused("owner.exact_provisioned_account_consent_required"));
            }
            for name in [ANCHOR, RECORD] {
                match storage::open_private_file(directory, Path::new(name), false) {
                    Ok(_) => return Err(refused("owner.already_initialized_or_incomplete")),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            let identity = HostIdentity::load_from_private_directory(directory, identity_file)?;
            let host = PinnedHost {
                host: identity.host_id().try_into()?,
                host_key_fingerprint: identity.fingerprint()?,
            };
            if let Some(provisioned) = provisioned {
                if provisioned.host_id != host.host.as_str()
                    || provisioned.host_key_fingerprint != host.host_key_fingerprint
                {
                    return Err(refused("owner.provisioned_host_identity_mismatch"));
                }
            }
            let record = EnrollmentSnapshot {
                version: 1,
                origin: installation.origin,
                storage: self.storage,
                binding: OwnerBinding {
                    installation: installation.installation.clone(),
                    host,
                    account,
                    principal: PrincipalId::new(),
                },
                identity_file: identity_file.to_owned(),
                revision: Epoch::new(1)?,
                installation_epoch: installation.epoch,
                keys: Vec::new(),
                remote_access: false,
                capabilities: Capabilities::default(),
            };
            record.validate(installation)?;
            let reference = match initializing_reference(directory, installation, self.storage) {
                Ok(reference) => {
                    match self.vault.get(&reference) {
                        Err(festerm_secret_store::SecretStoreError::Missing) => {}
                        Ok(_) => {
                            return Err(refused("owner.initialization_requires_reconciliation"))
                        }
                        Err(error) => return Err(self.storage_error(error)),
                    }
                    reference
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let reference = SecretReference::generate();
                    protected_state::publish_new(
                        directory,
                        Path::new(INITIALIZING),
                        &serde_json::to_vec(&InitializationReceipt {
                            version: 1,
                            storage: self.storage,
                            origin: installation.origin,
                            installation: installation.installation.clone(),
                            reference: reference.to_persisted_string(),
                        })
                        .map_err(|_| refused("owner.invalid_initialization_receipt"))?,
                    )?;
                    reference
                }
                Err(error) => return Err(error),
            };
            self.prepare(directory, &record, [0; 32])?;
            let anchor = PolicyAnchor::for_record(&record, [0; 32])?;
            self.vault
                .create_reserved(&reference, &SecretBytes::copy_from_slice(&anchor.encode()))
                .map_err(|error| self.storage_error(error))?;
            // The exact reference and prepared zero-grant record are durable
            // before native creation. No failure can orphan an unknown reference.
            publish_reference(directory, reference.to_persisted_string().as_bytes())?;
            protected_state::publish_new(directory, Path::new(RECORD), &record_bytes(&record)?)?;
            Ok(record)
        })
    }

    fn prepare(
        &self,
        directory: &File,
        record: &EnrollmentSnapshot,
        previous: [u8; 32],
    ) -> io::Result<()> {
        record_bytes(record)?;
        let bytes = serde_json::to_vec(&PreparedPolicy {
            previous,
            next: record.clone(),
        })
        .map_err(|_| refused("owner.invalid_prepared_policy"))?;
        storage::replace_private(directory, Path::new(PREPARED), &bytes)
    }

    fn commit(
        &self,
        directory: &File,
        reference: &SecretReference,
        record: &EnrollmentSnapshot,
        previous: [u8; 32],
    ) -> io::Result<()> {
        self.commit_with(
            directory,
            reference,
            record,
            previous,
            |directory, bytes| storage::replace_private(directory, Path::new(RECORD), bytes),
        )
    }

    fn commit_with(
        &self,
        directory: &File,
        reference: &SecretReference,
        record: &EnrollmentSnapshot,
        previous: [u8; 32],
        publish: impl FnOnce(&File, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        self.prepare(directory, record, previous)?;
        let anchor = PolicyAnchor::for_record(record, previous)?;
        self.vault
            .update(reference, &SecretBytes::copy_from_slice(&anchor.encode()))
            .map_err(|error| self.storage_error(error))?;
        publish(directory, &record_bytes(record)?)
    }

    fn reference(&self, directory: &File, name: &str) -> io::Result<SecretReference> {
        let bytes = storage::read_private(directory, Path::new(name), 36)?;
        SecretReference::parse(
            std::str::from_utf8(&bytes).map_err(|_| refused("owner.invalid_anchor"))?,
        )
        .map_err(vault_error)
    }

    fn read(
        &self,
        directory: &File,
        installation: &Snapshot,
    ) -> io::Result<(SecretReference, EnrollmentSnapshot)> {
        let reference = self.reference(directory, ANCHOR)?;
        let bytes = storage::read_private(directory, Path::new(RECORD), LIMIT)?;
        let protected = self
            .vault
            .get(&reference)
            .map_err(|error| self.storage_error(error))?;
        let anchor = protected.with_bytes(PolicyAnchor::decode)?;
        if anchor.digest != <[u8; 32]>::from(Sha256::digest(bytes.as_slice())) {
            return Err(refused(
                "owner.policy_rollback_or_interrupted_commit_local_reconciliation_required",
            ));
        }
        let mut record: EnrollmentSnapshot =
            serde_json::from_slice(&bytes).map_err(|_| refused("owner.invalid_policy"))?;
        validate_identity(directory, &record, installation)?;
        if record.storage != self.storage {
            return Err(refused("owner.storage_selection_changed_requires_review"));
        }
        if record.revision != anchor.revision {
            return Err(refused("owner.anchor_revision_mismatch"));
        }
        if record.installation_epoch < installation.epoch {
            // Persist each observed lifecycle high-water before issuing authority.
            record.installation_epoch = installation.epoch;
            record.revision = Epoch::new(
                record
                    .revision
                    .get()
                    .checked_add(1)
                    .ok_or_else(|| refused("owner.epoch_exhausted"))?,
            )?;
            self.commit(directory, &reference, &record, anchor.digest)?;
        }
        Ok((reference, record))
    }

    pub fn snapshot(&self) -> io::Result<EnrollmentSnapshot> {
        self.installation.with_locked(|directory, installation| {
            self.read(directory, installation).map(|(_, record)| record)
        })
    }

    /// Explicit local forward recovery only. The prepared state must exactly
    /// match the native revision/digest/predecessor and current installation,
    /// stable identity and account. A merely newer disk record is not consent.
    pub fn reconcile(&self) -> io::Result<EnrollmentSnapshot> {
        self.installation.with_locked(|directory, installation| {
            let reserved = initializing_reference(directory, installation, self.storage)?;
            let (reference, missing_pointer) = match self.reference(directory, ANCHOR) {
                Ok(reference) if reference == reserved => (reference, false),
                Ok(_) => return Err(refused("owner.initialization_reference_mismatch")),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (reserved, true),
                Err(error) => return Err(error),
            };
            let protected = self
                .vault
                .get(&reference)
                .map_err(|error| self.storage_error(error))?;
            let anchor = protected.with_bytes(PolicyAnchor::decode)?;
            let current = match storage::read_private(directory, Path::new(RECORD), LIMIT) {
                Ok(bytes) => {
                    let current: EnrollmentSnapshot = serde_json::from_slice(&bytes)
                        .map_err(|_| refused("owner.invalid_policy_preserve_for_local_review"))?;
                    validate_identity(directory, &current, installation)?;
                    Some(current)
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            let already_current = current.as_ref().is_some_and(|record| {
                record.revision == anchor.revision
                    && record_digest(record).is_ok_and(|digest| digest == anchor.digest)
            });
            if already_current {
                if missing_pointer {
                    protected_state::publish_new(
                        directory,
                        Path::new(ANCHOR),
                        reference.to_persisted_string().as_bytes(),
                    )?;
                }
                return self.read(directory, installation).map(|(_, record)| record);
            }
            let prepared: PreparedPolicy = serde_json::from_slice(&storage::read_private(
                directory,
                Path::new(PREPARED),
                LIMIT + 512,
            )?)
            .map_err(|_| refused("owner.invalid_prepared_policy"))?;
            if anchor != PolicyAnchor::for_record(&prepared.next, prepared.previous)? {
                return Err(refused("owner.prepared_policy_not_native_committed"));
            }
            validate_identity(directory, &prepared.next, installation)?;
            if prepared.next.storage != self.storage {
                return Err(refused("owner.storage_selection_changed_requires_review"));
            }
            if !already_current {
                match &current {
                    Some(old) => {
                        if record_digest(old)? != anchor.previous
                            || old.revision.get().checked_add(1)
                                != Some(prepared.next.revision.get())
                            || old.binding != prepared.next.binding
                            || old.storage != prepared.next.storage
                            || old.identity_file != prepared.next.identity_file
                            || old.installation_epoch > prepared.next.installation_epoch
                            || old.keys.len() > prepared.next.keys.len()
                            || old.keys.iter().zip(&prepared.next.keys).any(|(old, next)| {
                                old.public_key != next.public_key || old.revoked && !next.revoked
                            })
                        {
                            return Err(refused(
                                "owner.recovery_would_rollback_or_replace_consent",
                            ));
                        }
                    }
                    None if anchor.previous == [0; 32]
                        && anchor.revision.get() == 1
                        && prepared.next.keys.is_empty()
                        && !prepared.next.remote_access
                        && prepared.next.capabilities == Capabilities::default() => {}
                    None => {
                        return Err(refused(
                            "owner.missing_committed_policy_requires_local_review",
                        ))
                    }
                }
            }
            if missing_pointer {
                protected_state::publish_new(
                    directory,
                    Path::new(ANCHOR),
                    reference.to_persisted_string().as_bytes(),
                )?;
            }
            if !already_current {
                storage::replace_private(
                    directory,
                    Path::new(RECORD),
                    &record_bytes(&prepared.next)?,
                )?;
            }
            self.read(directory, installation).map(|(_, record)| record)
        })
    }

    /// Compare-and-swap every consent operation; stale foreground results cannot
    /// re-add an old key or overwrite later revocation. Vault first, then disk.
    fn change(
        &self,
        revision: Epoch,
        update: impl FnOnce(&mut EnrollmentSnapshot, &Snapshot) -> io::Result<()>,
    ) -> io::Result<EnrollmentSnapshot> {
        self.installation.with_locked(|directory, installation| {
            let (reference, mut record) = self.read(directory, installation)?;
            let previous = record_digest(&record)?;
            if record.revision != revision {
                return Err(refused("owner.stale_operation"));
            }
            update(&mut record, installation)?;
            record.revision = Epoch::new(
                record
                    .revision
                    .get()
                    .checked_add(1)
                    .ok_or_else(|| refused("owner.epoch_exhausted"))?,
            )?;
            record.validate(installation)?;
            self.commit(directory, &reference, &record, previous)?;
            Ok(record)
        })
    }

    pub fn enroll(
        &self,
        revision: Epoch,
        public_key: [u8; 32],
        confirmed_fingerprint: &str,
    ) -> io::Result<EnrollmentSnapshot> {
        self.change(revision, |record, _| {
            add_key(record, public_key, confirmed_fingerprint)
        })
    }

    pub fn revoke(&self, revision: Epoch, public_key: [u8; 32]) -> io::Result<EnrollmentSnapshot> {
        self.change(revision, |record, _| revoke_key(record, public_key))
    }

    pub fn rotate(
        &self,
        revision: Epoch,
        old: [u8; 32],
        new: [u8; 32],
        confirmed_fingerprint: &str,
    ) -> io::Result<EnrollmentSnapshot> {
        self.change(revision, |record, _| {
            revoke_key(record, old)?;
            add_key(record, new, confirmed_fingerprint)
        })
    }

    /// Separate protected local permission consent. Importing a key never calls
    /// this. Creation additionally requires the existing host settings grant.
    /// Runtime issuer epochs are never stored as consent.
    pub fn approve_permissions(
        &self,
        revision: Epoch,
        remote_access: bool,
        capabilities: Capabilities,
    ) -> io::Result<EnrollmentSnapshot> {
        self.change(revision, |record, installation| {
            if capabilities.creation_allowed && !installation.settings.allow_creation {
                return Err(refused("owner.creation_settings_consent_required"));
            }
            record.remote_access = remote_access;
            record.capabilities = capabilities;
            Ok(())
        })
    }

    /// Keep the short exclusive installation/policy lease through fresh
    /// reauthorization and the session-control admission boundary. Never hold it
    /// across network I/O; no cached verifier is current authority.
    pub fn with_current_verifier<T>(
        &self,
        action: impl FnOnce(&OwnerVerifier) -> io::Result<T>,
    ) -> io::Result<T> {
        let guard = self.current_policy()?;
        action(guard.verifier())
    }

    /// Return this guard from the durable backend's admission callback so its
    /// journal owns the exact protected-policy lifetime around publication.
    pub fn current_policy(&self) -> io::Result<CurrentOwnerPolicy> {
        self.installation
            .with_owned_lease(|directory, installation, lease| {
                let (_, record) = self.read(directory, installation)?;
                Ok(CurrentOwnerPolicy {
                    _lease: lease,
                    verifier: record.verifier(installation)?,
                })
            })
    }

    pub(crate) fn migration_policy(
        &self,
        operation: &str,
        reduction: bool,
    ) -> io::Result<MigrationOwnerPolicy> {
        self.installation
            .with_owned_lease(|directory, installation, lease| {
                let (_, record) = self.read(directory, installation)?;
                crate::owned_migration::check_migration_owner(
                    directory,
                    installation,
                    operation,
                    &record.binding.account,
                    reduction,
                )?;
                if !reduction
                    && (!record.remote_access
                        || !record.capabilities.inventory
                        || !record.capabilities.attach
                        || !record.capabilities.detach
                        || !record.keys.iter().any(|key| !key.revoked))
                {
                    return Err(refused(
                        "owner.migration_current_explicit_owner_consent_required",
                    ));
                }
                Ok(MigrationOwnerPolicy {
                    _lease: lease,
                    enrollment: record,
                    installation: installation.clone(),
                })
            })
    }
}

fn revoke_key(record: &mut EnrollmentSnapshot, public_key: [u8; 32]) -> io::Result<()> {
    let key = record
        .keys
        .iter_mut()
        .find(|key| key.public_key == public_key && !key.revoked)
        .ok_or_else(|| refused("owner.active_key_not_found"))?;
    key.revoked = true;
    Ok(())
}

fn add_key(
    record: &mut EnrollmentSnapshot,
    public_key: [u8; 32],
    confirmed_fingerprint: &str,
) -> io::Result<()> {
    if owner_key_fingerprint(&public_key)? != confirmed_fingerprint {
        return Err(refused("owner.explicit_fingerprint_confirmation_required"));
    }
    if record.keys.len() >= MAX_HISTORICAL_KEYS
        || record.keys.iter().filter(|key| !key.revoked).count() >= MAX_OWNER_KEYS
        || record.keys.iter().any(|key| key.public_key == public_key)
    {
        return Err(refused("owner.key_limit_duplicate_or_revoked_key"));
    }
    record.keys.push(EnrolledKey {
        public_key,
        revoked: false,
    });
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::lifecycle::{Origin, Settings, Startup};
    use festerm_host_protocol::{Envelope, OperationId, Request};
    use festerm_secret_store::{MemorySecretStore, SecretStoreError};
    use festerm_tunnel_bridge::owner_auth::OwnerSigningKey;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    pub(crate) struct Vault {
        memory: MemorySecretStore,
        pub(crate) locked: AtomicBool,
    }
    struct SharedVault(Arc<Vault>);
    impl SecretStore for SharedVault {
        fn create_reserved(
            &self,
            reference: &SecretReference,
            value: &SecretBytes,
        ) -> Result<(), SecretStoreError> {
            if value.len() > 2560 {
                return Err(SecretStoreError::TooLarge);
            }
            if self.0.locked.load(Ordering::Acquire) {
                return Err(SecretStoreError::LockedOrUnavailable);
            }
            self.0.memory.create_reserved(reference, value)
        }

        fn put(&self, value: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
            if value.len() > 2560 {
                return Err(SecretStoreError::TooLarge);
            }
            if self.0.locked.load(Ordering::Acquire) {
                return Err(SecretStoreError::LockedOrUnavailable);
            }
            self.0.memory.put(value)
        }
        fn get(&self, reference: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
            if self.0.locked.load(Ordering::Acquire) {
                return Err(SecretStoreError::LockedOrUnavailable);
            }
            self.0.memory.get(reference)
        }
        fn update(
            &self,
            reference: &SecretReference,
            value: &SecretBytes,
        ) -> Result<(), SecretStoreError> {
            if value.len() > 2560 {
                return Err(SecretStoreError::TooLarge);
            }
            if self.0.locked.load(Ordering::Acquire) {
                return Err(SecretStoreError::LockedOrUnavailable);
            }
            self.0.memory.update(reference, value)
        }
        fn delete(&self, reference: &SecretReference) -> Result<bool, SecretStoreError> {
            self.0.memory.delete(reference)
        }
    }
    fn setup() -> (storage::PrivateTestDirectory, Store, Arc<Vault>) {
        setup_origin(Origin::Headless)
    }

    pub(crate) fn fixture_owners(installation: &Store, vault: Arc<Vault>) -> OwnerEnrollment<'_> {
        OwnerEnrollment {
            installation,
            vault: Box::new(SharedVault(vault)),
            storage: OwnerStorage::NativeVault,
        }
    }

    pub(crate) fn setup_origin(
        origin: Origin,
    ) -> (storage::PrivateTestDirectory, Store, Arc<Vault>) {
        let root = storage::private_test_directory();
        let store = Store::create(
            &root.path().join("installation"),
            origin,
            Settings {
                friendly_name: "owner fixture".into(),
                allow_creation: false,
                startup: Startup::Manual,
            },
        )
        .unwrap();
        let identity =
            HostIdentity::load_or_create(&root.path().join("installation/host-identity")).unwrap();
        let ticket = store
            .request_enable(&store.snapshot().unwrap().settings)
            .unwrap();
        store
            .record_provisioned(
                &ticket,
                crate::lifecycle::Binding {
                    host_id: identity.host_id(),
                    account: "github:1234".into(),
                    qualified_tunnel: "fixture.invalid/exact".into(),
                    cloud_incarnation: "synthetic-incarnation".into(),
                    host_key_fingerprint: identity.fingerprint().unwrap(),
                },
            )
            .unwrap();
        let vault = Arc::new(Vault {
            memory: MemorySecretStore::new(),
            locked: AtomicBool::new(false),
        });
        (root, store, vault)
    }
    fn initialize(enrollment: &OwnerEnrollment<'_>) -> EnrollmentSnapshot {
        enrollment
            .initialize(Path::new("host-identity"), "github:1234".into())
            .unwrap()
    }
    fn enroll(
        enrollment: &OwnerEnrollment<'_>,
        state: &EnrollmentSnapshot,
        key: &OwnerSigningKey,
    ) -> EnrollmentSnapshot {
        enrollment
            .enroll(
                state.revision,
                key.public_key(),
                &owner_key_fingerprint(&key.public_key()).unwrap(),
            )
            .unwrap()
    }

    #[test]
    fn protected_owner_initialization_fences_original_foreground_epoch_before_publication() {
        let (_root, store, vault) = setup();
        let enrollment = OwnerEnrollment {
            installation: &store,
            vault: Box::new(SharedVault(vault)),
            storage: OwnerStorage::NativeVault,
        };
        let epoch = store.snapshot().unwrap().epoch;
        assert!(enrollment
            .initialize_at_epoch(Path::new("host-identity"), "github:1234".into(), epoch + 1,)
            .is_err());
        let initial = enrollment
            .initialize_at_epoch(Path::new("host-identity"), "github:1234".into(), epoch)
            .unwrap();
        assert!(initial.keys.is_empty());
        assert!(!initial.remote_access);
    }

    #[test]
    fn protected_owner_enrollment_is_explicit_persistent_and_not_creation_consent() {
        let (root, store, vault) = setup();
        let enrollment = OwnerEnrollment {
            installation: &store,
            vault: Box::new(SharedVault(vault.clone())),
            storage: OwnerStorage::NativeVault,
        };
        let initial = initialize(&enrollment);
        assert!(initial.keys.is_empty());
        assert!(!initial.remote_access);
        assert_eq!(initial.capabilities, Capabilities::default());
        let key = OwnerSigningKey::from_seed(&[21; 32]);
        assert!(enrollment
            .enroll(initial.revision, key.public_key(), "unconfirmed")
            .is_err());
        let enrolled = enroll(&enrollment, &initial, &key);
        assert!(!enrolled.remote_access);
        assert!(enrollment
            .with_current_verifier(|current| Ok(current.challenge().is_ok()))
            .is_ok_and(|allowed| !allowed));
        assert!(enrollment
            .approve_permissions(
                enrolled.revision,
                true,
                Capabilities {
                    attach: true,
                    create: true,
                    creation_allowed: true,
                    ..Default::default()
                }
            )
            .is_err());
        assert!(enrollment
            .enroll(
                initial.revision,
                OwnerSigningKey::from_seed(&[22; 32]).public_key(),
                "stale"
            )
            .is_err());
        let reopened = Store::open(&root.path().join("installation"), Origin::Headless).unwrap();
        let reopened = OwnerEnrollment {
            installation: &reopened,
            vault: Box::new(SharedVault(vault)),
            storage: OwnerStorage::NativeVault,
        };
        assert_eq!(reopened.snapshot().unwrap(), enrolled);
        assert!(reopened
            .initialize(Path::new("host-identity"), enrolled.binding.account)
            .is_err());
    }

    #[test]
    fn protected_owner_rotation_revokes_pending_and_live_admission_and_stale_results() {
        let (_root, store, vault) = setup();
        let enrollment = OwnerEnrollment {
            installation: &store,
            vault: Box::new(SharedVault(vault)),
            storage: OwnerStorage::NativeVault,
        };
        let initial = initialize(&enrollment);
        let key = OwnerSigningKey::from_seed(&[31; 32]);
        let state = enroll(&enrollment, &initial, &key);
        let state = enrollment
            .approve_permissions(
                state.revision,
                true,
                Capabilities {
                    inventory: true,
                    ..Default::default()
                },
            )
            .unwrap();
        // Primitive integration only: the production transport exporter remains
        // unavailable, and no stream or listener can enter through this fixture.
        let pending = enrollment
            .with_current_verifier(|current| Ok(current.challenge()?))
            .unwrap();
        let proof = key.prove(pending.challenge(), &state.binding.host).unwrap();
        let (owner, _guard) = enrollment
            .with_current_verifier(|current| Ok(pending.verify(&proof, current)?))
            .unwrap();
        assert_eq!(owner.principal(), &state.binding.principal);
        let request = Envelope::new(
            state.binding.host.host.clone(),
            OperationId::new(),
            Request::List { cursor: None },
        );
        let operation_epoch = Epoch::new(1).unwrap();
        let admitted = enrollment
            .with_current_verifier(|current| {
                Ok(owner.authorize(current, &request, operation_epoch, None)?)
            })
            .unwrap();
        let guard = enrollment.current_policy().unwrap();
        assert!(enrollment.revoke(state.revision, key.public_key()).is_err());
        drop(guard);
        let pending = enrollment
            .with_current_verifier(|current| Ok(current.challenge()?))
            .unwrap();
        let proof = key.prove(pending.challenge(), &state.binding.host).unwrap();
        let new_key = OwnerSigningKey::from_seed(&[32; 32]);
        let rotated = enrollment
            .rotate(
                state.revision,
                key.public_key(),
                new_key.public_key(),
                &owner_key_fingerprint(&new_key.public_key()).unwrap(),
            )
            .unwrap();
        assert!(rotated.revision.get() > admitted.grant_epoch.get());
        assert!(enrollment
            .with_current_verifier(|current| Ok(pending.verify(&proof, current)?))
            .is_err());
        assert!(enrollment
            .with_current_verifier(|current| Ok(owner.authorize(
                current,
                &request,
                operation_epoch,
                None
            )?))
            .is_err());
        assert!(enrollment
            .enroll(
                state.revision,
                key.public_key(),
                &owner_key_fingerprint(&key.public_key()).unwrap()
            )
            .is_err());
        assert!(enrollment
            .enroll(
                rotated.revision,
                key.public_key(),
                &owner_key_fingerprint(&key.public_key()).unwrap()
            )
            .is_err());
        let pending = enrollment
            .with_current_verifier(|current| Ok(current.challenge()?))
            .unwrap();
        let foreign = key
            .prove(pending.challenge(), &rotated.binding.host)
            .unwrap();
        assert!(enrollment
            .with_current_verifier(|current| Ok(pending.verify(&foreign, current)?))
            .is_err());
        let pending = enrollment
            .with_current_verifier(|current| Ok(current.challenge()?))
            .unwrap();
        let proof = new_key
            .prove(pending.challenge(), &rotated.binding.host)
            .unwrap();
        assert!(enrollment
            .with_current_verifier(|current| Ok(pending.verify(&proof, current)?))
            .is_ok());
        enrollment
            .revoke(rotated.revision, new_key.public_key())
            .unwrap();
        assert!(enrollment.with_current_verifier(|_| Ok(())).is_err());
    }

    #[test]
    fn protected_owner_vault_fences_disk_rollback_foreign_installation_and_missing_anchor() {
        let (root, store, vault) = setup();
        let enrollment = OwnerEnrollment {
            installation: &store,
            vault: Box::new(SharedVault(vault.clone())),
            storage: OwnerStorage::NativeVault,
        };
        let initial = initialize(&enrollment);
        let key = OwnerSigningKey::from_seed(&[41; 32]);
        let state = enroll(&enrollment, &initial, &key);
        let old = store
            .with_locked(|directory, _| storage::read_private(directory, Path::new(RECORD), LIMIT))
            .unwrap();
        vault.locked.store(true, Ordering::Release);
        assert!(enrollment
            .snapshot()
            .unwrap_err()
            .to_string()
            .contains("unlock_native_store"));
        assert!(enrollment.revoke(state.revision, key.public_key()).is_err());
        vault.locked.store(false, Ordering::Release);
        assert_eq!(enrollment.snapshot().unwrap(), state);
        let revoked = enrollment.revoke(state.revision, key.public_key()).unwrap();
        let foreign = Store::create(
            &root.path().join("foreign"),
            Origin::Headless,
            store.snapshot().unwrap().settings,
        )
        .unwrap();
        let anchor = store
            .with_locked(|directory, _| storage::read_private(directory, Path::new(ANCHOR), 36))
            .unwrap();
        foreign
            .with_locked(|directory, _| {
                protected_state::publish_new(directory, Path::new(ANCHOR), &anchor)?;
                protected_state::publish_new(
                    directory,
                    Path::new(RECORD),
                    &serde_json::to_vec(&revoked).unwrap(),
                )
            })
            .unwrap();
        let foreign = OwnerEnrollment {
            installation: &foreign,
            vault: Box::new(SharedVault(vault.clone())),
            storage: OwnerStorage::NativeVault,
        };
        assert!(foreign.snapshot().is_err());
        store
            .with_locked(|directory, _| {
                storage::replace_private(directory, Path::new(RECORD), &old)
            })
            .unwrap();
        assert!(enrollment
            .snapshot()
            .unwrap_err()
            .to_string()
            .contains("rollback_or_interrupted_commit"));
        let reopened = Store::open(&root.path().join("installation"), Origin::Headless).unwrap();
        let reopened = OwnerEnrollment {
            installation: &reopened,
            vault: Box::new(SharedVault(vault.clone())),
            storage: OwnerStorage::NativeVault,
        };
        assert!(reopened.snapshot().is_err());
        let reference = SecretReference::parse(std::str::from_utf8(&anchor).unwrap()).unwrap();
        vault.memory.delete(&reference).unwrap();
        assert!(reopened
            .snapshot()
            .unwrap_err()
            .to_string()
            .contains("anchor_missing"));
    }

    #[test]
    fn protected_owner_observed_lifecycle_revision_cannot_roll_back() {
        let (_root, store, vault) = setup();
        let enrollment = OwnerEnrollment {
            installation: &store,
            vault: Box::new(SharedVault(vault)),
            storage: OwnerStorage::NativeVault,
        };
        let initial = initialize(&enrollment);
        let old = store
            .with_locked(|directory, _| {
                storage::read_private(directory, Path::new("host-lifecycle.json"), 32 * 1024)
            })
            .unwrap();
        store.revoke().unwrap();
        let advanced = enrollment.snapshot().unwrap();
        assert!(advanced.revision.get() > initial.revision.get());
        store
            .with_locked(|directory, _| {
                storage::replace_private(directory, Path::new("host-lifecycle.json"), &old)
            })
            .unwrap();
        assert!(enrollment.snapshot().is_err());
    }

    #[test]
    fn protected_owner_detects_changed_stable_host_key_or_provisioning_account() {
        for change_account in [false, true] {
            let (root, store, vault) = setup();
            let enrollment = OwnerEnrollment {
                installation: &store,
                vault: Box::new(SharedVault(vault)),
                storage: OwnerStorage::NativeVault,
            };
            initialize(&enrollment);
            if change_account {
                store
                    .with_locked(|directory, installation| {
                        let mut changed = installation.clone();
                        changed.binding.as_mut().unwrap().account = "github:foreign".into();
                        storage::replace_private(
                            directory,
                            Path::new("host-lifecycle.json"),
                            &serde_json::to_vec(&changed).unwrap(),
                        )
                    })
                    .unwrap();
            } else {
                HostIdentity::load_or_create(&root.path().join("installation/replacement"))
                    .unwrap();
                store
                    .with_locked(|directory, _| {
                        let bytes =
                            storage::read_private(directory, Path::new("replacement"), 4096)?;
                        storage::replace_private(directory, Path::new("host-identity"), &bytes)
                    })
                    .unwrap();
            }
            assert!(enrollment.snapshot().is_err());
        }
    }

    #[test]
    fn protected_owner_interrupted_policy_publication_requires_reconciliation() {
        let (_root, store, vault) = setup();
        let enrollment = OwnerEnrollment {
            installation: &store,
            vault: Box::new(SharedVault(vault.clone())),
            storage: OwnerStorage::NativeVault,
        };
        let initial = initialize(&enrollment);
        let key = OwnerSigningKey::from_seed(&[51; 32]);
        let mut state = enroll(&enrollment, &initial, &key);
        // Model interruption after the native high-water advances, before the
        // final disk publication. Startup must never silently use old consent.
        store
            .with_locked(|directory, _| {
                let anchor = storage::read_private(directory, Path::new(ANCHOR), 36)?;
                let reference =
                    SecretReference::parse(std::str::from_utf8(&anchor).unwrap()).unwrap();
                let previous = record_digest(&state)?;
                state.keys[0].revoked = true;
                state.revision = Epoch::new(state.revision.get() + 1)?;
                enrollment.prepare(directory, &state, previous)?;
                vault
                    .memory
                    .update(
                        &reference,
                        &SecretBytes::copy_from_slice(
                            &PolicyAnchor::for_record(&state, previous)?.encode(),
                        ),
                    )
                    .map_err(vault_error)
            })
            .unwrap();
        assert!(enrollment
            .snapshot()
            .unwrap_err()
            .to_string()
            .contains("reconciliation_required"));
        assert!(enrollment
            .enroll(
                state.revision,
                key.public_key(),
                &owner_key_fingerprint(&key.public_key()).unwrap()
            )
            .is_err());
        store
            .with_locked(|directory, _| {
                let prepared = storage::read_private(directory, Path::new(PREPARED), LIMIT + 512)?;
                let mut tampered: PreparedPolicy = serde_json::from_slice(&prepared).unwrap();
                tampered.next.keys[0].revoked = false;
                storage::replace_private(
                    directory,
                    Path::new(PREPARED),
                    &serde_json::to_vec(&tampered).unwrap(),
                )?;
                Ok(prepared)
            })
            .map(|prepared| {
                assert!(enrollment.reconcile().is_err());
                store
                    .with_locked(|directory, _| {
                        storage::replace_private(directory, Path::new(PREPARED), &prepared)
                    })
                    .unwrap();
            })
            .unwrap();
        let recovered = enrollment.reconcile().unwrap();
        assert_eq!(recovered, state);
        assert!(enrollment
            .enroll(
                recovered.revision,
                key.public_key(),
                &owner_key_fingerprint(&key.public_key()).unwrap()
            )
            .is_err());
    }

    #[test]
    fn protected_owner_initialization_requires_exact_provisioned_account_and_recovers_receipt() {
        let (_root, store, vault) = setup();
        let enrollment = OwnerEnrollment {
            installation: &store,
            vault: Box::new(SharedVault(vault.clone())),
            storage: OwnerStorage::NativeVault,
        };
        assert!(enrollment
            .initialize(Path::new("host-identity"), "github:foreign".into())
            .is_err());
        vault.locked.store(true, Ordering::Release);
        assert!(enrollment
            .initialize(Path::new("host-identity"), "github:1234".into())
            .is_err());
        vault.locked.store(false, Ordering::Release);
        assert!(enrollment
            .initialize_with(
                Path::new("host-identity"),
                "github:1234".into(),
                |_, _| Err(refused("fixture.interrupted_reference_publication"))
            )
            .is_err());
        assert!(enrollment.snapshot().is_err());
        let recovered = enrollment.reconcile().unwrap();
        assert!(recovered.keys.is_empty());
        assert!(!recovered.remote_access);
        assert_eq!(enrollment.snapshot().unwrap(), recovered);
        assert!(enrollment
            .initialize(Path::new("host-identity"), "github:1234".into())
            .is_err());

        let (_other_root, other, other_vault) = setup();
        other.revoke().unwrap();
        let stopped = other.snapshot().unwrap();
        other
            .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
            .unwrap();
        let unprovisioned = OwnerEnrollment {
            installation: &other,
            vault: Box::new(SharedVault(other_vault)),
            storage: OwnerStorage::NativeVault,
        };
        assert!(unprovisioned
            .initialize(Path::new("host-identity"), "github:1234".into())
            .is_err());
    }

    #[test]
    fn protected_owner_interrupted_initialization_cannot_switch_credential_storage() {
        for selected in [
            OwnerStorage::NativeVault,
            OwnerStorage::HeadlessProtectedFile,
        ] {
            let (_root, store, vault) = setup();
            let enrollment = match selected {
                OwnerStorage::NativeVault => OwnerEnrollment {
                    installation: &store,
                    vault: Box::new(SharedVault(vault.clone())),
                    storage: selected,
                },
                OwnerStorage::HeadlessProtectedFile => {
                    store.owner_enrollment_with_storage(selected).unwrap()
                }
            };
            assert!(enrollment
                .initialize_with(
                    Path::new("host-identity"),
                    "github:1234".into(),
                    |_, _| Err(refused("fixture.interrupted_reference_publication")),
                )
                .is_err());
            let evidence = || {
                store
                    .with_locked(|directory, _| {
                        Ok((
                            storage::read_private(directory, Path::new(INITIALIZING), 512)?,
                            storage::read_private(directory, Path::new(PREPARED), LIMIT + 512)?,
                        ))
                    })
                    .unwrap()
            };
            let before = evidence();
            let other = match selected {
                OwnerStorage::NativeVault => OwnerStorage::HeadlessProtectedFile,
                OwnerStorage::HeadlessProtectedFile => OwnerStorage::NativeVault,
            };
            assert!(store.owner_enrollment_with_storage(other).is_err());
            let wrong = OwnerEnrollment {
                installation: &store,
                vault: Box::new(SharedVault(vault)),
                storage: other,
            };
            assert!(wrong
                .initialize(Path::new("host-identity"), "github:1234".into())
                .is_err());
            assert!(wrong.reconcile().is_err());
            assert_eq!(evidence(), before);
            assert_eq!(enrollment.reconcile().unwrap().storage, selected);
        }
    }

    #[test]
    fn protected_owner_creation_settings_mask_preserves_revocation_and_permission_removal() {
        let (_root, store, vault) = setup();
        fn configure(store: &Store, allow_creation: bool) {
            store.revoke().unwrap();
            let stopped = store.snapshot().unwrap();
            store
                .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
                .unwrap();
            let mut settings = stopped.settings;
            settings.allow_creation = allow_creation;
            store.configure(settings.clone()).unwrap();
            let ticket = store.request_enable(&settings).unwrap();
            store
                .record_provisioned(&ticket, stopped.binding.unwrap())
                .unwrap();
        }
        configure(&store, true);
        let enrollment = OwnerEnrollment {
            installation: &store,
            vault: Box::new(SharedVault(vault)),
            storage: OwnerStorage::NativeVault,
        };
        let initial = initialize(&enrollment);
        let key = OwnerSigningKey::from_seed(&[61; 32]);
        let state = enroll(&enrollment, &initial, &key);
        let state = enrollment
            .approve_permissions(
                state.revision,
                true,
                Capabilities {
                    attach: true,
                    create: true,
                    creation_allowed: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(
            enrollment
                .current_policy()
                .unwrap()
                .verifier()
                .policy()
                .capabilities
                .creation_allowed
        );
        assert!(state.capabilities.issuance_epoch.is_none());
        let advertised = enrollment
            .current_policy()
            .unwrap()
            .verifier()
            .capabilities();
        advertised.validate().unwrap();
        assert!(!advertised.create && !advertised.ensure && !advertised.creation_allowed);
        assert!(advertised.issuance_epoch.is_none());
        let mut runtime = state.capabilities;
        runtime.issuance_epoch = Some(Epoch::new(7).unwrap());
        assert!(enrollment
            .approve_permissions(state.revision, true, runtime)
            .is_err());
        assert_eq!(enrollment.snapshot().unwrap(), state);
        configure(&store, false);
        let masked = enrollment.snapshot().unwrap();
        assert!(masked.capabilities.creation_allowed);
        assert!(
            !enrollment
                .current_policy()
                .unwrap()
                .verifier()
                .policy()
                .capabilities
                .creation_allowed
        );
        assert!(enrollment
            .approve_permissions(masked.revision, true, state.capabilities)
            .is_err());
        let revoked = enrollment
            .revoke(masked.revision, key.public_key())
            .unwrap();
        let removed = enrollment
            .approve_permissions(revoked.revision, false, Capabilities::default())
            .unwrap();
        assert!(!removed.remote_access && !removed.capabilities.creation_allowed);
        assert!(removed.keys[0].revoked);
    }

    #[test]
    fn protected_owner_compact_anchor_supports_all_64_history_entries_in_2560_byte_vault() {
        let (_root, store, vault) = setup();
        let enrollment = OwnerEnrollment {
            installation: &store,
            vault: Box::new(SharedVault(vault.clone())),
            storage: OwnerStorage::NativeVault,
        };
        let initial = initialize(&enrollment);
        let first = OwnerSigningKey::from_seed(&[1; 32]);
        let mut active = first.public_key();
        let mut state = enroll(&enrollment, &initial, &first);
        for seed in 2..=64 {
            let next = OwnerSigningKey::from_seed(&[seed; 32]).public_key();
            state = enrollment
                .rotate(
                    state.revision,
                    active,
                    next,
                    &owner_key_fingerprint(&next).unwrap(),
                )
                .unwrap();
            active = next;
        }
        assert_eq!(state.keys.len(), 64);
        assert_eq!(state.keys.iter().filter(|key| !key.revoked).count(), 1);
        assert!(record_bytes(&state).unwrap().len() > 2560);
        let state = enrollment.revoke(state.revision, active).unwrap();
        assert!(state.keys.iter().all(|key| key.revoked));
        assert!(enrollment
            .enroll(
                state.revision,
                first.public_key(),
                &owner_key_fingerprint(&first.public_key()).unwrap()
            )
            .is_err());
        assert_eq!(enrollment.reconcile().unwrap(), state);
    }

    #[test]
    fn protected_owner_policy_source_reacquires_selected_storage_and_fresh_revocation_lease() {
        fn assert_source<T: Send + Sync + 'static>() {}
        assert_source::<CurrentOwnerPolicySource>();
        let (_root, store, _vault) = setup();
        let store = Arc::new(store);
        let source =
            CurrentOwnerPolicySource::new(store.clone(), OwnerStorage::HeadlessProtectedFile);
        assert!(matches!(
            source.current_policy(),
            Err(ContractError::Denied)
        ));
        let enrollment = store
            .owner_enrollment_with_storage(OwnerStorage::HeadlessProtectedFile)
            .unwrap();
        let initial = initialize(&enrollment);
        let key = OwnerSigningKey::from_seed(&[81; 32]);
        let state = enroll(&enrollment, &initial, &key);
        let state = enrollment
            .approve_permissions(
                state.revision,
                true,
                Capabilities {
                    inventory: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let guard = source.current_policy().unwrap();
        let epoch = guard.verifier().policy().host_epoch;
        assert!(guard.verifier().policy().remote_access);
        assert!(matches!(source.current_policy(), Err(ContractError::Busy)));
        assert!(store
            .snapshot()
            .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock));
        drop(guard);
        store.revoke().unwrap();
        let guard = source.current_policy().unwrap();
        assert!(!guard.verifier().policy().remote_access);
        assert!(guard.verifier().policy().host_epoch.get() > epoch.get());
        drop(guard);
        assert!(enrollment.revoke(state.revision, key.public_key()).is_err());
        enrollment
            .revoke(enrollment.snapshot().unwrap().revision, key.public_key())
            .unwrap();
        assert!(matches!(
            source.current_policy(),
            Err(ContractError::Denied)
        ));
        let native = CurrentOwnerPolicySource::new(store.clone(), OwnerStorage::NativeVault);
        assert!(matches!(
            native.current_policy(),
            Err(ContractError::Denied)
        ));
        store
            .with_locked(|directory, _| {
                storage::replace_private(directory, Path::new(RECORD), b"corrupt")
            })
            .unwrap();
        assert!(matches!(
            source.current_policy(),
            Err(ContractError::Denied)
        ));
    }

    #[cfg(feature = "host-runtime")]
    #[test]
    fn protected_owner_runtime_trait_retains_lease_and_reloads_revocation() {
        use festerm_tunnel_bridge::host_control::{PolicyGuard, ProtectedPolicySource};

        fn current<T: ProtectedPolicySource>(source: &T) -> Result<T::Guard, ContractError> {
            source.current_policy()
        }

        let (_root, store, _vault) = setup();
        let store = Arc::new(store);
        let source =
            CurrentOwnerPolicySource::new(store.clone(), OwnerStorage::HeadlessProtectedFile);
        assert!(matches!(current(&source), Err(ContractError::Denied)));
        let enrollment = store
            .owner_enrollment_with_storage(OwnerStorage::HeadlessProtectedFile)
            .unwrap();
        let initial = initialize(&enrollment);
        let key = OwnerSigningKey::from_seed(&[82; 32]);
        let state = enroll(&enrollment, &initial, &key);
        enrollment
            .approve_permissions(
                state.revision,
                true,
                Capabilities {
                    inventory: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let guard = current(&source).unwrap();
        let epoch = PolicyGuard::verifier(&guard).policy().host_epoch;
        assert!(PolicyGuard::verifier(&guard).policy().remote_access);
        assert!(matches!(current(&source), Err(ContractError::Busy)));
        assert!(store
            .revoke()
            .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock));
        drop(guard);
        store.revoke().unwrap();
        let guard = current(&source).unwrap();
        assert!(!PolicyGuard::verifier(&guard).policy().remote_access);
        assert!(PolicyGuard::verifier(&guard).policy().host_epoch.get() > epoch.get());
    }

    #[test]
    fn protected_owner_headless_file_selection_is_explicit_bound_and_not_desktop_fallback() {
        let (root, store, _vault) = setup();
        let enrollment = store
            .owner_enrollment_with_storage(OwnerStorage::HeadlessProtectedFile)
            .unwrap();
        let initial = initialize(&enrollment);
        assert_eq!(initial.storage, OwnerStorage::HeadlessProtectedFile);
        assert!(store
            .owner_enrollment_with_storage(OwnerStorage::NativeVault)
            .is_err());
        let key = store
            .with_locked(|directory, installation| {
                let selected =
                    festerm_tunnel_bridge::owner_auth::ExplicitHeadlessFileStore::select(
                        directory,
                        &installation.installation,
                    )?;
                let key = festerm_tunnel_bridge::owner_auth::ClientOwnerKey::generate_headless(
                    &selected,
                    &initial.binding,
                )
                .unwrap();
                let reopened = festerm_tunnel_bridge::owner_auth::ClientOwnerKey::open_headless(
                    &selected,
                    key.reference(),
                    &initial.binding,
                )
                .unwrap();
                assert_eq!(key.public_key(), reopened.public_key());
                assert!(key.remove_native().is_err());
                let reference = key.reference();
                key.remove_headless(&selected).unwrap();
                assert!(matches!(
                    selected.get(&reference),
                    Err(festerm_secret_store::SecretStoreError::Missing)
                ));
                key.remove_headless(&selected).unwrap();
                let foreign = festerm_host_protocol::HostId::new().to_string();
                assert!(
                    festerm_tunnel_bridge::owner_auth::ExplicitHeadlessFileStore::select(
                        directory, &foreign
                    )
                    .is_err()
                );
                Ok(key)
            })
            .unwrap();
        let enrolled = enrollment
            .enroll(
                initial.revision,
                key.public_key(),
                &owner_key_fingerprint(&key.public_key()).unwrap(),
            )
            .unwrap();
        let revoked = enrollment
            .revoke(enrolled.revision, key.public_key())
            .unwrap();
        assert!(revoked.keys[0].revoked);
        let reopened = Store::open(&root.path().join("installation"), Origin::Headless).unwrap();
        assert_eq!(
            reopened
                .owner_enrollment_with_storage(OwnerStorage::HeadlessProtectedFile)
                .unwrap()
                .snapshot()
                .unwrap(),
            revoked
        );
        let desktop = Store::create(
            &root.path().join("desktop"),
            Origin::Desktop,
            Settings {
                friendly_name: "desktop".into(),
                allow_creation: false,
                startup: Startup::Manual,
            },
        )
        .unwrap();
        assert!(desktop
            .owner_enrollment_with_storage(OwnerStorage::HeadlessProtectedFile)
            .is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};
            let anchor = store
                .with_locked(|directory, _| storage::read_private(directory, Path::new(ANCHOR), 36))
                .unwrap();
            let secret_path = root.path().join("installation").join(format!(
                "headless-owner-secret-{}",
                std::str::from_utf8(&anchor).unwrap()
            ));
            assert_eq!(
                std::fs::metadata(&secret_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
            std::fs::remove_file(&secret_path).unwrap();
            symlink(
                root.path().join("installation/owner-enrollment.json"),
                &secret_path,
            )
            .unwrap();
            assert!(enrollment.snapshot().is_err());
            assert!(secret_path
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }

    #[test]
    fn protected_owner_observed_epoch_commit_recovers_only_forward_after_disk_failure() {
        let (_root, store, vault) = setup();
        let enrollment = OwnerEnrollment {
            installation: &store,
            vault: Box::new(SharedVault(vault)),
            storage: OwnerStorage::NativeVault,
        };
        let initial = initialize(&enrollment);
        let key = OwnerSigningKey::from_seed(&[71; 32]);
        let state = enroll(&enrollment, &initial, &key);
        // A prepared-but-not-native-committed proposal must not become consent.
        store
            .with_locked(|directory, _| {
                let mut proposal = state.clone();
                proposal.keys[0].revoked = true;
                proposal.revision = Epoch::new(proposal.revision.get() + 1)?;
                enrollment.prepare(directory, &proposal, record_digest(&state)?)
            })
            .unwrap();
        assert_eq!(enrollment.reconcile().unwrap(), state);
        store.revoke().unwrap();
        store
            .with_locked(|directory, installation| {
                let reference = enrollment.reference(directory, ANCHOR)?;
                let mut advanced = state.clone();
                advanced.installation_epoch = installation.epoch;
                advanced.revision = Epoch::new(advanced.revision.get() + 1)?;
                let result = enrollment.commit_with(
                    directory,
                    &reference,
                    &advanced,
                    record_digest(&state)?,
                    |_, _| Err(refused("fixture.interrupted_disk_publication")),
                );
                assert!(result.is_err());
                Ok(())
            })
            .unwrap();
        assert!(enrollment.snapshot().is_err());
        let recovered = enrollment.reconcile().unwrap();
        assert!(recovered.revision.get() > state.revision.get());
        assert!(enrollment
            .current_policy()
            .unwrap()
            .verifier()
            .challenge()
            .is_err());
        let revoked = enrollment
            .revoke(recovered.revision, key.public_key())
            .unwrap();
        assert!(revoked.keys[0].revoked);
    }
}
