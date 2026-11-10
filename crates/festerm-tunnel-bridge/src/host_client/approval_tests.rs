use super::*;
use crate::owner_auth::PinnedHost;
use festerm_host_protocol::PrincipalId;
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

fn review(binding: &OwnerBinding) -> ReviewedOwner {
    ReviewedOwner::confirm(
        &serde_json::to_string(binding).unwrap(),
        &binding.host.host,
        &binding.account,
        &binding.host.host_key_fingerprint,
    )
    .unwrap()
}

fn fixture() -> (
    storage::PrivateTestDirectory,
    ReceiptIndex,
    MemorySecretStore,
) {
    let parent = std::env::current_dir().unwrap().join("target");
    std::fs::create_dir_all(&parent).unwrap();
    let root = storage::private_test_directory_in(&parent);
    let index = ReceiptIndex {
        root: root.path().join("client-receipts"),
    };
    (root, index, MemorySecretStore::new())
}

#[cfg(all(unix, not(feature = "relay")))]
struct CountingVault {
    inner: MemorySecretStore,
    creates: std::sync::atomic::AtomicUsize,
}

#[cfg(all(unix, not(feature = "relay")))]
impl SecretStore for CountingVault {
    fn create_reserved(
        &self,
        reference: &SecretReference,
        secret: &festerm_secret_store::SecretBytes,
    ) -> Result<(), SecretStoreError> {
        self.creates
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.create_reserved(reference, secret)
    }

    fn put(
        &self,
        _: &festerm_secret_store::SecretBytes,
    ) -> Result<SecretReference, SecretStoreError> {
        panic!("receipt preparation must use its reserved reference")
    }

    fn get(
        &self,
        reference: &SecretReference,
    ) -> Result<festerm_secret_store::SecretBytes, SecretStoreError> {
        self.inner.get(reference)
    }

    fn update(
        &self,
        reference: &SecretReference,
        secret: &festerm_secret_store::SecretBytes,
    ) -> Result<(), SecretStoreError> {
        self.inner.update(reference, secret)
    }

    fn delete(&self, reference: &SecretReference) -> Result<bool, SecretStoreError> {
        self.inner.delete(reference)
    }
}

#[cfg(all(unix, not(feature = "relay")))]
#[test]
fn selected_host_portable_receipt_abandoned_staging_recovers_exactly_one_key() {
    use std::{io::Write, sync::atomic::Ordering};
    let (_root, index, inner) = fixture();
    let vault = CountingVault {
        inner,
        creates: 0.into(),
    };
    let binding = binding();
    let directory = index.directory(true).unwrap();
    let staging = format!(".stage-{}", festerm_host_protocol::OperationId::new());
    let staging = Path::new(&staging);
    let mut interrupted =
        festerm_unix_security::open_private_file(&directory, staging, true).unwrap();
    interrupted.write_all(b"{\"v").unwrap();
    interrupted.sync_all().unwrap();
    drop(interrupted);
    assert!(matches!(
        index.load_with(&vault, &binding.host.host, &binding.account),
        Err(Error::Missing)
    ));
    assert_eq!(vault.creates.load(Ordering::SeqCst), 0);
    let receipt = index.prepare_with(&vault, review(&binding)).unwrap();
    let name = ReceiptIndex::name(&binding.host.host, &binding.account, "json").unwrap();
    let original = storage::read_private(&directory, &name, MAX_RECEIPT_BYTES).unwrap();
    assert_eq!(
        storage::publish_new(&directory, &name, b"replacement")
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        storage::read_private(&directory, &name, MAX_RECEIPT_BYTES).unwrap(),
        original
    );
    let reopened = index.prepare_with(&vault, review(&binding)).unwrap();
    assert_eq!(reopened.public_key(), receipt.public_key());
    assert_eq!(
        reopened.reference().to_persisted_string(),
        receipt.reference().to_persisted_string()
    );
    assert_eq!(
        index
            .load_with(&vault, &binding.host.host, &binding.account)
            .unwrap()
            .public_key(),
        receipt.public_key()
    );
    assert_eq!(vault.creates.load(Ordering::SeqCst), 1);
    assert_eq!(
        storage::read_private(&directory, staging, 32).unwrap(),
        b"{\"v"
    );
}

#[cfg(all(unix, not(feature = "relay")))]
#[test]
fn selected_host_portable_receipt_malformed_final_remains_storage_without_key_creation() {
    let (_root, index, inner) = fixture();
    let vault = CountingVault {
        inner,
        creates: 0.into(),
    };
    let binding = binding();
    let directory = index.directory(true).unwrap();
    let name = ReceiptIndex::name(&binding.host.host, &binding.account, "json").unwrap();
    storage::write_new_private(&directory, &name, b"{\"v").unwrap();
    assert!(matches!(
        index.load_with(&vault, &binding.host.host, &binding.account),
        Err(Error::Storage)
    ));
    assert!(matches!(
        index.prepare_with(&vault, review(&binding)),
        Err(Error::Storage)
    ));
    assert_eq!(vault.creates.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        storage::read_private(&directory, &name, 32).unwrap(),
        b"{\"v"
    );
}

#[test]
fn selected_host_receipt_review_requires_exact_account_host_and_independent_pin() {
    let binding = binding();
    let json = serde_json::to_string(&binding).unwrap();
    for (host, account, pin) in [
        (
            HostId::new(),
            binding.account.as_str(),
            binding.host.host_key_fingerprint.as_str(),
        ),
        (
            binding.host.host.clone(),
            "github:9876",
            binding.host.host_key_fingerprint.as_str(),
        ),
        (
            binding.host.host.clone(),
            binding.account.as_str(),
            "SHA256:wrong",
        ),
    ] {
        assert!(matches!(
            ReviewedOwner::confirm(&json, &host, account, pin),
            Err(Error::BindingMismatch)
        ));
    }

    assert!(matches!(
        ReviewedOwner::confirm(
            &"x".repeat(MAX_RECEIPT_BYTES + 1),
            &binding.host.host,
            &binding.account,
            ""
        ),
        Err(Error::BindingMismatch)
    ));
}

#[cfg(unix)]
#[test]
fn selected_host_mobile_receipt_uses_native_app_root_but_refuses_linked_index() {
    let (root, _, vault) = fixture();
    let index = ReceiptIndex::in_app_sandbox(root.path()).unwrap();
    let binding = binding();
    let original = index.prepare_with(&vault, review(&binding)).unwrap();
    assert_eq!(
        index
            .load_with(&vault, &binding.host.host, &binding.account)
            .unwrap()
            .public_key(),
        original.public_key()
    );
    let retained = root.path().join("retained-native-index");
    std::fs::rename(&index.root, &retained).unwrap();
    std::os::unix::fs::symlink(&retained, &index.root).unwrap();
    assert!(matches!(
        index.load_with(&vault, &binding.host.host, &binding.account),
        Err(Error::Storage)
    ));
    assert!(matches!(
        index.prepare_with(&vault, review(&binding)),
        Err(Error::Storage)
    ));
    assert!(std::fs::symlink_metadata(&index.root).unwrap().is_symlink());
}

#[test]
fn selected_host_receipt_prepare_and_reopen_use_only_exact_protected_index() {
    let (_root, index, vault) = fixture();
    let binding = binding();
    assert!(matches!(
        index.load_with(&vault, &binding.host.host, &binding.account),
        Err(Error::Missing)
    ));
    let receipt = index.prepare_with(&vault, review(&binding)).unwrap();
    let reference = receipt.reference().to_persisted_string();
    let second = index.prepare_with(&vault, review(&binding)).unwrap();
    let loaded = index
        .load_with(&vault, &binding.host.host, &binding.account)
        .unwrap();
    assert_eq!(loaded.binding(), &binding);
    assert_eq!(loaded.reference().to_persisted_string(), reference);
    assert_eq!(second.public_key(), receipt.public_key());
    assert_eq!(loaded.public_key(), receipt.public_key());
    let mut changed = binding.clone();
    changed.principal = PrincipalId::new();
    assert!(matches!(
        index.prepare_with(&vault, review(&changed)),
        Err(Error::BindingMismatch)
    ));
    assert!(matches!(
        index.load_with(&vault, &binding.host.host, "github:9876"),
        Err(Error::Missing)
    ));
}

#[test]
fn selected_host_receipt_pending_or_missing_ready_key_never_regenerates_during_load() {
    let (_root, index, vault) = fixture();
    let binding = binding();
    let directory = index.directory(true).unwrap();
    let reference = SecretReference::generate();
    let record = Record {
        version: 1,
        binding: binding.clone(),
        reference: reference.to_persisted_string(),
        ready: false,
        originals: None,
    };
    let name = ReceiptIndex::name(&binding.host.host, &binding.account, "json").unwrap();
    storage::write_new_private(&directory, &name, &serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(matches!(
        index.load_with(&vault, &binding.host.host, &binding.account),
        Err(Error::Incomplete)
    ));
    assert!(matches!(
        vault.get(&reference),
        Err(SecretStoreError::Missing)
    ));
    let receipt = index.prepare_with(&vault, review(&binding)).unwrap();
    assert_eq!(receipt.reference().to_persisted_string(), record.reference);
    vault.delete(&receipt.reference()).unwrap();
    assert!(matches!(
        index.prepare_with(&vault, review(&binding)),
        Err(Error::OwnerKey(OwnerKeyError::Vault(
            SecretStoreError::Missing
        )))
    ));
    assert!(matches!(
        vault.get(&reference),
        Err(SecretStoreError::Missing)
    ));
}

#[test]
fn selected_host_receipt_pending_existing_key_recovers_same_reference_and_busy_is_not_absence() {
    let (_root, index, vault) = fixture();
    let binding = binding();
    let receipt = index.prepare_with(&vault, review(&binding)).unwrap();
    let directory = index.directory(false).unwrap();
    let mut record = ReceiptIndex::read(&directory, &binding.host.host, &binding.account).unwrap();
    record.ready = false;
    let name = ReceiptIndex::name(&binding.host.host, &binding.account, "json").unwrap();
    storage::replace_private(&directory, &name, &serde_json::to_vec(&record).unwrap()).unwrap();
    let resumed = index.prepare_with(&vault, review(&binding)).unwrap();
    assert_eq!(resumed.public_key(), receipt.public_key());
    assert_eq!(resumed.reference().to_persisted_string(), record.reference);
    let _lease = ReceiptIndex::lease(&directory, &binding.host.host, &binding.account).unwrap();
    assert!(matches!(
        index.load_with(&vault, &binding.host.host, &binding.account),
        Err(Error::Busy)
    ));
    assert!(matches!(
        index.prepare_with(&vault, review(&binding)),
        Err(Error::Busy)
    ));
}

#[test]
fn selected_host_receipt_rejects_changed_protected_binding_without_key_replacement() {
    let (_root, index, vault) = fixture();
    let binding = binding();
    let original = index.prepare_with(&vault, review(&binding)).unwrap();
    let directory = index.directory(false).unwrap();
    let mut record = ReceiptIndex::read(&directory, &binding.host.host, &binding.account).unwrap();
    record.binding.principal = PrincipalId::new();
    let name = ReceiptIndex::name(&binding.host.host, &binding.account, "json").unwrap();
    storage::replace_private(&directory, &name, &serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(matches!(
        index.load_with(&vault, &binding.host.host, &binding.account),
        Err(Error::OwnerKey(OwnerKeyError::InvalidBinding))
    ));
    assert!(matches!(
        index.prepare_with(&vault, review(&record.binding)),
        Err(Error::OwnerKey(OwnerKeyError::InvalidBinding))
    ));
    assert_eq!(
        ClientOwnerKey::open(&vault, original.reference(), &binding)
            .unwrap()
            .public_key(),
        original.public_key()
    );
}

#[cfg(unix)]
#[test]
fn selected_host_receipt_symlink_is_refused_without_following_or_replacing_it() {
    let (_root, index, vault) = fixture();
    let binding = binding();
    index.prepare_with(&vault, review(&binding)).unwrap();
    let name = ReceiptIndex::name(&binding.host.host, &binding.account, "json").unwrap();
    let original = index.root.join("retained-original.json");
    let linked = index.root.join(name);
    std::fs::rename(&linked, &original).unwrap();
    std::os::unix::fs::symlink(&original, &linked).unwrap();
    assert!(matches!(
        index.load_with(&vault, &binding.host.host, &binding.account),
        Err(Error::Storage)
    ));
    assert!(matches!(
        index.prepare_with(&vault, review(&binding)),
        Err(Error::Storage)
    ));
    assert!(std::fs::symlink_metadata(linked).unwrap().is_symlink());
    assert!(original.is_file());
}

#[cfg(unix)]
#[test]
fn selected_host_receipt_hardlink_and_nonprivate_metadata_refuse_without_repair() {
    use std::os::unix::fs::PermissionsExt;
    let (_root, index, vault) = fixture();
    let binding = binding();
    let original = index.prepare_with(&vault, review(&binding)).unwrap();
    let name = ReceiptIndex::name(&binding.host.host, &binding.account, "json").unwrap();
    let record = index.root.join(name);
    let retained = index.root.join("retained-hardlink");
    std::fs::hard_link(&record, &retained).unwrap();
    assert!(matches!(
        index.load_with(&vault, &binding.host.host, &binding.account),
        Err(Error::Storage)
    ));
    assert!(matches!(
        index.prepare_with(&vault, review(&binding)),
        Err(Error::Storage)
    ));
    std::fs::remove_file(retained).unwrap();
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        index.load_with(&vault, &binding.host.host, &binding.account),
        Err(Error::Storage)
    ));
    assert!(matches!(
        index.prepare_with(&vault, review(&binding)),
        Err(Error::Storage)
    ));
    assert_eq!(
        std::fs::metadata(record).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert_eq!(
        ClientOwnerKey::open(&vault, original.reference(), &binding)
            .unwrap()
            .public_key(),
        original.public_key()
    );
}
