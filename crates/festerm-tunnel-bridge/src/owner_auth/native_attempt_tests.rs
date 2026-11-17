use super::*;
use crate::relay_host::tests::owner_network_pair;
use festerm_secret_store::{MemorySecretStore, SecretBytes, SecretReference, SecretStoreError};
use russh::keys::{Algorithm, HashAlg, PrivateKey};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Vault {
    inner: MemorySecretStore,
    reads: AtomicUsize,
}
impl SecretStore for Vault {
    fn put(&self, value: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
        self.inner.put(value)
    }
    fn get(&self, reference: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.get(reference)
    }
    fn update(
        &self,
        reference: &SecretReference,
        value: &SecretBytes,
    ) -> Result<(), SecretStoreError> {
        self.inner.update(reference, value)
    }
    fn delete(&self, reference: &SecretReference) -> Result<bool, SecretStoreError> {
        self.inner.delete(reference)
    }
}

fn setup() -> (PrivateKey, OwnerBinding, Vault, ClientOwnerKey, OwnerPolicy) {
    let ssh = PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519).unwrap();
    let host = PinnedHost {
        host: Default::default(),
        host_key_fingerprint: ssh.public_key().fingerprint(HashAlg::Sha256).to_string(),
    };
    let approved = OwnerBinding {
        installation: festerm_host_protocol::HostId::new().to_string(),
        host: host.clone(),
        account: "github:native-attempt".into(),
        principal: Default::default(),
    };
    let vault = Vault::default();
    let key = fixture_key(&vault, &approved);
    let policy = OwnerPolicy {
        host,
        principal: approved.principal.clone(),
        host_epoch: festerm_host_protocol::Epoch::new(1).unwrap(),
        grant_epoch: festerm_host_protocol::Epoch::new(1).unwrap(),
        remote_access: true,
        capabilities: Default::default(),
    };
    (ssh, approved, vault, key, policy)
}

fn attempt(
    vault: &Vault,
    key: &ClientOwnerKey,
    approved: &OwnerBinding,
    cancel: CancellationToken,
) -> NativeOwnerAttempt {
    ClientOwnerKey::prepare_attempt_using(vault, key.reference(), approved, cancel).unwrap()
}

#[tokio::test]
async fn native_attempt_reads_once_and_local_deletion_only_affects_later_attempts() {
    let (ssh, approved, vault, key, policy) = setup();
    let verifier =
        OwnerVerifier::from_protected_permissions(policy.clone(), &[key.public_key()]).unwrap();
    let (client, host, connection) = owner_network_pair(ssh, approved.host.clone()).await;
    let snapshot = attempt(&vault, &key, &approved, CancellationToken::new());
    assert_eq!(vault.reads.load(Ordering::SeqCst), 1);
    vault.delete(&key.reference()).unwrap();
    let pending = host.challenge_owner(&verifier).unwrap();
    let proof = client
        .prove_native_attempt(snapshot, pending.challenge())
        .unwrap();
    assert_eq!(
        vault.reads.load(Ordering::SeqCst),
        1,
        "proof cannot reopen the vault"
    );
    host.verify_owner(pending, &proof, &verifier).unwrap();
    assert!(matches!(
        ClientOwnerKey::prepare_attempt_using(
            &vault,
            key.reference(),
            &approved,
            CancellationToken::new()
        ),
        Err(OwnerKeyError::Vault(SecretStoreError::Missing))
    ));
    connection.abort();
}

#[tokio::test]
async fn native_attempt_retains_exact_loaded_key_and_fresh_server_revocation() {
    let (ssh, approved, vault, key, policy) = setup();
    let verifier =
        OwnerVerifier::from_protected_permissions(policy.clone(), &[key.public_key()]).unwrap();
    let (client, host, connection) = owner_network_pair(ssh, approved.host.clone()).await;
    let snapshot = attempt(&vault, &key, &approved, CancellationToken::new());
    let revocation_snapshot = attempt(&vault, &key, &approved, CancellationToken::new());
    // Replacing local bytes does not retarget an already extracted attempt.
    let other = fixture_key(&vault, &approved);
    let bytes = vault.inner.get(&other.reference()).unwrap();
    vault.update(&key.reference(), &bytes).unwrap();
    let pending = host.challenge_owner(&verifier).unwrap();
    let proof = client
        .prove_native_attempt(snapshot, pending.challenge())
        .unwrap();
    host.verify_owner(pending, &proof, &verifier).unwrap();
    let replacement = attempt(&vault, &key, &approved, CancellationToken::new());
    let pending = host.challenge_owner(&verifier).unwrap();
    let proof = client
        .prove_native_attempt(replacement, pending.challenge())
        .unwrap();
    assert!(
        host.verify_owner(pending, &proof, &verifier).is_err(),
        "unenrolled replacement is refused"
    );
    let pending = host.challenge_owner(&verifier).unwrap();
    let proof = client
        .prove_native_attempt(revocation_snapshot, pending.challenge())
        .unwrap();
    let mut revoked = policy;
    revoked.remote_access = false;
    let current = OwnerVerifier::from_protected_permissions(revoked, &[key.public_key()]).unwrap();
    assert!(host.verify_owner(pending, &proof, &current).is_err());
    connection.abort();
}

#[tokio::test]
async fn native_attempt_rejects_foreign_context_cancel_and_expired_queue() {
    let (ssh, approved, vault, key, policy) = setup();
    let verifier = OwnerVerifier::from_protected_permissions(policy, &[key.public_key()]).unwrap();
    let (client, host, connection) = owner_network_pair(ssh.clone(), approved.host.clone()).await;
    let (foreign_client, _foreign_host, other_connection) =
        owner_network_pair(ssh, approved.host.clone()).await;
    for kind in 0..8 {
        let cancel = CancellationToken::new();
        let mut snapshot = attempt(&vault, &key, &approved, cancel.clone());
        let pending = host.challenge_owner(&verifier).unwrap();
        let mut challenge = pending.challenge().clone();
        match kind {
            0 => cancel.cancel(),
            1 => snapshot.guard.expires = Instant::now(),
            2 => challenge.challenge.host.host = Default::default(),
            3 => challenge.challenge.principal = Default::default(),
            4 => snapshot.approved.host.host_key_fingerprint = "SHA256:foreign".into(),
            5 => snapshot.public_key = [0; 32],
            6 => {
                assert!(foreign_client
                    .prove_native_attempt(snapshot, &challenge)
                    .is_err());
                continue;
            }
            _ => challenge.challenge.protocol = "foreign-domain".into(),
        }
        assert!(client.prove_native_attempt(snapshot, &challenge).is_err());
    }
    // A proof built while live cannot be published after cancel/expiry.
    for cancel_after in [true, false] {
        let cancel = CancellationToken::new();
        let snapshot = attempt(&vault, &key, &approved, cancel.clone());
        let mut guard = snapshot.publication_guard();
        let pending = host.challenge_owner(&verifier).unwrap();
        client
            .prove_native_attempt(snapshot, pending.challenge())
            .unwrap();
        if cancel_after {
            cancel.cancel();
        } else {
            guard.expires = Instant::now();
        }
        assert!(guard.check().is_err());
    }
    connection.abort();
    other_connection.abort();
}

#[test]
fn native_attempt_rejects_changed_record_binding_and_missing_references() {
    let (_, approved, vault, key, _) = setup();
    for kind in 0..4 {
        let mut foreign = approved.clone();
        match kind {
            0 => foreign.installation = festerm_host_protocol::HostId::new().to_string(),
            1 => foreign.account = "github:foreign".into(),
            2 => foreign.host.host = Default::default(),
            _ => foreign.principal = Default::default(),
        }
        assert!(matches!(
            ClientOwnerKey::prepare_attempt_using(
                &vault,
                key.reference(),
                &foreign,
                CancellationToken::new()
            ),
            Err(OwnerKeyError::InvalidBinding)
        ));
    }
    assert!(matches!(
        ClientOwnerKey::prepare_attempt_using(
            &vault,
            SecretReference::generate(),
            &approved,
            CancellationToken::new()
        ),
        Err(OwnerKeyError::Vault(SecretStoreError::Missing))
    ));
}
