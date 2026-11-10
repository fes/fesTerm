//! Explicit host-local enrollment, separate from portable setup and bearer access.
//! Native vault state is the rollback fence; disk publication follows it. A crash
//! between the two refuses admission and requires local reconciliation.
use crate::{
    lifecycle::{Phase, Snapshot, Store},
    protected_state,
};
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use festerm_host_protocol::{Capabilities, Epoch, PrincipalId, Validate};
use festerm_secret_store::{native_store, SecretBytes, SecretReference, SecretStore};
use festerm_sessiond::private_storage as storage;
use festerm_tunnel_bridge::host_identity::HostIdentity;
use festerm_tunnel_bridge::owner_auth::{
    validate_owner_public_key, OwnerBinding, OwnerPolicy, OwnerVerifier, PinnedHost, MAX_OWNER_KEYS,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, io, path::Path};

const RECORD: &str = "owner-enrollment.json";
const ANCHOR: &str = "owner-enrollment-vault-ref";
const LIMIT: usize = 16 * 1024;
const MAX_HISTORICAL_KEYS: usize = 64;

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
        self.capabilities.validate()?;
        if self.version != 1
            || self.binding.installation != installation.installation
            || self.installation_epoch > installation.epoch
            || self.keys.len() > MAX_HISTORICAL_KEYS
            || self.keys.iter().filter(|key| !key.revoked).count() > MAX_OWNER_KEYS
            || self.capabilities.creation_allowed && !installation.settings.allow_creation
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
            capabilities: self.capabilities,
        };
        OwnerVerifier::new(policy, &keys).map_err(Into::into)
    }
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
}

/// Scoped protected policy guard for the backend's atomic admission callback.
/// Drop before native startup waits or network I/O; reacquire for final effects.
pub struct CurrentOwnerPolicy {
    _lease: protected_state::Lease,
    verifier: OwnerVerifier,
}

impl CurrentOwnerPolicy {
    pub fn verifier(&self) -> &OwnerVerifier {
        &self.verifier
    }
}

impl Store {
    pub fn owner_enrollment(&self) -> io::Result<OwnerEnrollment<'_>> {
        Ok(OwnerEnrollment {
            installation: self,
            vault: native_store().map_err(vault_error)?,
        })
    }
}

impl OwnerEnrollment<'_> {
    /// Explicit local approval of an exact installation, pinned host and account.
    /// This generates an owner principal locally; account strings confer no role.
    /// No remote access, creation permission or key is granted by initialization.
    pub fn initialize(
        &self,
        identity_file: &Path,
        account: String,
    ) -> io::Result<EnrollmentSnapshot> {
        self.installation.with_locked(|directory, installation| {
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
            let record = EnrollmentSnapshot {
                version: 1,
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
            let bytes = serde_json::to_vec(&record).map_err(|_| refused("owner.invalid_policy"))?;
            let reference = self
                .vault
                .put(&SecretBytes::copy_from_slice(&bytes))
                .map_err(vault_error)?;
            // A partial initialization stays visible and fails closed.
            protected_state::publish_new(
                directory,
                Path::new(ANCHOR),
                reference.to_persisted_string().as_bytes(),
            )?;
            protected_state::publish_new(directory, Path::new(RECORD), &bytes)?;
            Ok(record)
        })
    }

    fn read(
        &self,
        directory: &File,
        installation: &Snapshot,
    ) -> io::Result<(SecretReference, EnrollmentSnapshot)> {
        let anchor = storage::read_private(directory, Path::new(ANCHOR), 36)?;
        let reference = SecretReference::parse(
            std::str::from_utf8(&anchor).map_err(|_| refused("owner.invalid_anchor"))?,
        )
        .map_err(vault_error)?;
        let bytes = storage::read_private(directory, Path::new(RECORD), LIMIT)?;
        let protected = self.vault.get(&reference).map_err(vault_error)?;
        if !protected.with_bytes(|expected| expected == bytes.as_slice()) {
            return Err(refused(
                "owner.policy_rollback_or_interrupted_commit_local_reconciliation_required",
            ));
        }
        let mut record: EnrollmentSnapshot =
            serde_json::from_slice(&bytes).map_err(|_| refused("owner.invalid_policy"))?;
        record.validate(installation)?;
        let identity = HostIdentity::load_from_private_directory(directory, &record.identity_file)?;
        if identity.host_id() != record.binding.host.host.as_str()
            || identity.fingerprint()? != record.binding.host.host_key_fingerprint
        {
            return Err(refused(
                "owner.host_identity_changed_reconfirmation_required",
            ));
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
            let bytes = serde_json::to_vec(&record).map_err(|_| refused("owner.invalid_policy"))?;
            self.vault
                .update(&reference, &SecretBytes::copy_from_slice(&bytes))
                .map_err(vault_error)?;
            storage::replace_private(directory, Path::new(RECORD), &bytes)?;
        }
        Ok((reference, record))
    }

    pub fn snapshot(&self) -> io::Result<EnrollmentSnapshot> {
        self.installation.with_locked(|directory, installation| {
            self.read(directory, installation).map(|(_, record)| record)
        })
    }

    /// Compare-and-swap every consent operation; stale foreground results cannot
    /// re-add an old key or overwrite later revocation. Vault first, then disk.
    fn change(
        &self,
        revision: Epoch,
        update: impl FnOnce(&mut EnrollmentSnapshot) -> io::Result<()>,
    ) -> io::Result<EnrollmentSnapshot> {
        self.installation.with_locked(|directory, installation| {
            let (reference, mut record) = self.read(directory, installation)?;
            if record.revision != revision {
                return Err(refused("owner.stale_operation"));
            }
            update(&mut record)?;
            record.revision = Epoch::new(
                record
                    .revision
                    .get()
                    .checked_add(1)
                    .ok_or_else(|| refused("owner.epoch_exhausted"))?,
            )?;
            record.validate(installation)?;
            let bytes = serde_json::to_vec(&record).map_err(|_| refused("owner.invalid_policy"))?;
            if bytes.len() > LIMIT {
                return Err(refused("owner.policy_limit"));
            }
            self.vault
                .update(&reference, &SecretBytes::copy_from_slice(&bytes))
                .map_err(vault_error)?;
            storage::replace_private(directory, Path::new(RECORD), &bytes)?;
            Ok(record)
        })
    }

    pub fn enroll(
        &self,
        revision: Epoch,
        public_key: [u8; 32],
        confirmed_fingerprint: &str,
    ) -> io::Result<EnrollmentSnapshot> {
        self.change(revision, |record| {
            add_key(record, public_key, confirmed_fingerprint)
        })
    }

    pub fn revoke(&self, revision: Epoch, public_key: [u8; 32]) -> io::Result<EnrollmentSnapshot> {
        self.change(revision, |record| revoke_key(record, public_key))
    }

    pub fn rotate(
        &self,
        revision: Epoch,
        old: [u8; 32],
        new: [u8; 32],
        confirmed_fingerprint: &str,
    ) -> io::Result<EnrollmentSnapshot> {
        self.change(revision, |record| {
            revoke_key(record, old)?;
            add_key(record, new, confirmed_fingerprint)
        })
    }

    /// Separate protected local permission consent. Importing a key never calls
    /// this. Creation additionally requires the existing host settings grant.
    pub fn approve_permissions(
        &self,
        revision: Epoch,
        remote_access: bool,
        capabilities: Capabilities,
    ) -> io::Result<EnrollmentSnapshot> {
        self.change(revision, |record| {
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
mod tests {
    use super::*;
    use crate::lifecycle::{Origin, Settings, Startup};
    use festerm_host_protocol::{Envelope, OperationId, Request};
    use festerm_secret_store::{MemorySecretStore, SecretStoreError};
    use festerm_tunnel_bridge::owner_auth::OwnerSigningKey;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    struct Vault {
        memory: MemorySecretStore,
        locked: AtomicBool,
    }
    struct SharedVault(Arc<Vault>);
    impl SecretStore for SharedVault {
        fn put(&self, value: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
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
            if self.0.locked.load(Ordering::Acquire) {
                return Err(SecretStoreError::LockedOrUnavailable);
            }
            self.0.memory.update(reference, value)
        }
        fn delete(&self, reference: &SecretReference) -> Result<bool, SecretStoreError> {
            self.0.memory.delete(reference)
        }
    }
    fn setup() -> (tempfile::TempDir, Store, Arc<Vault>) {
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let store = Store::create(
            &root.path().join("installation"),
            Origin::Headless,
            Settings {
                friendly_name: "owner fixture".into(),
                allow_creation: false,
                startup: Startup::Manual,
            },
        )
        .unwrap();
        HostIdentity::load_or_create(&root.path().join("installation/host-identity")).unwrap();
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
    fn protected_owner_enrollment_is_explicit_persistent_and_not_creation_consent() {
        let (root, store, vault) = setup();
        let enrollment = OwnerEnrollment {
            installation: &store,
            vault: Box::new(SharedVault(vault.clone())),
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
        };
        let initial = initialize(&enrollment);
        let ticket = store
            .request_enable(&store.snapshot().unwrap().settings)
            .unwrap();
        store
            .record_provisioned(
                &ticket,
                crate::lifecycle::Binding {
                    host_id: initial.binding.host.host.to_string(),
                    account: initial.binding.account.clone(),
                    qualified_tunnel: "fixture.invalid/exact".into(),
                    cloud_incarnation: "synthetic-incarnation".into(),
                    host_key_fingerprint: initial.binding.host.host_key_fingerprint.clone(),
                },
            )
            .unwrap();
        let initial = enrollment.snapshot().unwrap();
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
        };
        let initial = initialize(&enrollment);
        let old = store
            .with_locked(|directory, _| {
                storage::read_private(directory, Path::new("host-lifecycle.json"), 32 * 1024)
            })
            .unwrap();
        store
            .request_enable(&store.snapshot().unwrap().settings)
            .unwrap();
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
            };
            let initial = initialize(&enrollment);
            if change_account {
                let ticket = store
                    .request_enable(&store.snapshot().unwrap().settings)
                    .unwrap();
                store
                    .record_provisioned(
                        &ticket,
                        crate::lifecycle::Binding {
                            host_id: initial.binding.host.host.to_string(),
                            account: "github:foreign".into(),
                            qualified_tunnel: "fixture.invalid/exact".into(),
                            cloud_incarnation: "synthetic-incarnation".into(),
                            host_key_fingerprint: initial.binding.host.host_key_fingerprint,
                        },
                    )
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
                state.keys[0].revoked = true;
                state.revision = Epoch::new(state.revision.get() + 1)?;
                vault
                    .memory
                    .update(
                        &reference,
                        &SecretBytes::copy_from_slice(&serde_json::to_vec(&state).unwrap()),
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
    }
}
