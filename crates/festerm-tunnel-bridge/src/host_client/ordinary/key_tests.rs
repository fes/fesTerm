use super::*;
use festerm_secret_store::MemorySecretStore;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn guard() -> OrdinaryAccountGuard {
    OrdinaryAccountGuard::new(CancellationToken::new(), Arc::new(|| Ok(())))
}
fn binding(account: u64) -> KeyBinding {
    let host = festerm_host_protocol::HostId::new();
    KeyBinding::new(
        account,
        OrdinaryRoute {
            tunnel_id: "fixture".into(),
            cluster: "usw3".into(),
            endpoint_id: format!("{host}-relay"),
            port: 47461,
            host: crate::owner_auth::PinnedHost {
                host,
                host_key_fingerprint: format!("SHA256:{}", "A".repeat(43)),
            },
        },
    )
    .unwrap()
}
struct Vault {
    inner: MemorySecretStore,
    writes: AtomicUsize,
    gets: AtomicUsize,
    cancel_write: AtomicBool,
    cancel_get: AtomicBool,
    cancel: CancellationToken,
}
impl Vault {
    fn new(cancel: CancellationToken) -> Self {
        Self {
            inner: MemorySecretStore::new(),
            writes: AtomicUsize::new(0),
            gets: AtomicUsize::new(0),
            cancel_write: AtomicBool::new(false),
            cancel_get: AtomicBool::new(false),
            cancel,
        }
    }
}
impl SecretStore for Vault {
    fn create_reserved(
        &self,
        r: &SecretReference,
        s: &SecretBytes,
    ) -> Result<(), SecretStoreError> {
        self.inner.create_reserved(r, s)?;
        self.writes.fetch_add(1, Ordering::SeqCst);
        if self.cancel_write.load(Ordering::SeqCst) {
            self.cancel.cancel();
        }
        Ok(())
    }
    fn put(&self, s: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
        self.inner.put(s)
    }
    fn get(&self, r: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        let s = self.inner.get(r);
        if self.cancel_get.load(Ordering::SeqCst) {
            self.cancel.cancel();
        }
        s
    }
    fn update(&self, r: &SecretReference, s: &SecretBytes) -> Result<(), SecretStoreError> {
        self.inner.update(r, s)
    }
    fn delete(&self, r: &SecretReference) -> Result<bool, SecretStoreError> {
        self.inner.delete(r)
    }
}

#[test]
fn ordinary_native_key_reopens_exact_account_route_and_uses_distinct_purpose() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    let index = OrdinaryKeyIndex::in_app_sandbox(root.path()).unwrap();
    let vault = Arc::new(MemorySecretStore::new());
    let selected = binding(42);
    let first = index
        .prepare_with(vault.clone(), &selected, &guard())
        .unwrap();
    let second = index
        .prepare_with(vault.clone(), &selected, &guard())
        .unwrap();
    assert!(first.reference == second.reference);
    assert_eq!(first.key.public_key(), second.key.public_key());
    vault.get(&first.reference).unwrap().with_bytes(|bytes| {
        assert!(bytes.starts_with(KEY_RECORD));
        assert!(!bytes.starts_with(b"fesTerm owner signing key v1\0"));
    });
    let other_account = KeyBinding::new(43, selected.route.clone()).unwrap();
    let other = index
        .prepare_with(vault.clone(), &other_account, &guard())
        .unwrap();
    assert!(first.reference != other.reference);
    // A ready key from another scope cannot be substituted even at a valid ref.
    let wrong = vault.get(&first.reference).unwrap();
    vault.update(&other.reference, &wrong).unwrap();
    assert!(matches!(
        index.prepare_with(vault, &other_account, &guard()),
        Err(KeyError::BindingMismatch)
    ));
    for entry in std::fs::read_dir(root.path().join("host-client-approvals")).unwrap() {
        assert!(entry
            .unwrap()
            .file_name()
            .to_str()
            .unwrap()
            .starts_with("ordinary-v1-"));
    }
}

#[test]
fn ordinary_ready_missing_key_never_generates_replacement() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    let index = OrdinaryKeyIndex::in_app_sandbox(root.path()).unwrap();
    let vault = Arc::new(Vault::new(CancellationToken::new()));
    let selected = binding(42);
    let key = index
        .prepare_with(vault.clone(), &selected, &guard())
        .unwrap();
    vault.delete(&key.reference).unwrap();
    assert!(matches!(
        index.prepare_with(vault.clone(), &selected, &guard()),
        Err(KeyError::Vault(SecretStoreError::Missing))
    ));
    assert_eq!(vault.writes.load(Ordering::SeqCst), 1);
}

#[test]
fn ordinary_native_cancel_before_and_after_write_never_publishes_key_or_replaces_seed() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    let index = OrdinaryKeyIndex::in_app_sandbox(root.path()).unwrap();
    let cancel = CancellationToken::new();
    let vault = Arc::new(Vault::new(cancel.clone()));
    let selected = binding(42);
    let cancelled = OrdinaryAccountGuard::new(cancel.clone(), Arc::new(|| Ok(())));
    cancel.cancel();
    assert!(index
        .prepare_with(vault.clone(), &selected, &cancelled)
        .is_err());
    assert_eq!(vault.gets.load(Ordering::SeqCst), 0);
    assert_eq!(vault.writes.load(Ordering::SeqCst), 0);
    let cancel = CancellationToken::new();
    let vault = Arc::new(Vault::new(cancel.clone()));
    vault.cancel_write.store(true, Ordering::SeqCst);
    let g = OrdinaryAccountGuard::new(cancel, Arc::new(|| Ok(())));
    assert!(index.prepare_with(vault.clone(), &selected, &g).is_err());
    assert_eq!(vault.writes.load(Ordering::SeqCst), 1);
    let directory = index.index.directory(false).unwrap();
    let pending = OrdinaryKeyIndex::read(&directory, &selected).unwrap();
    assert!(!pending.ready);
    let reference = SecretReference::parse(&pending.reference).unwrap();
    let secret = vault.get(&reference).unwrap();
    let seed = decode_key(&secret, &selected).unwrap();
    let expected = OrdinaryDeviceKey::from_seed(&seed).public_key();
    vault.cancel_write.store(false, Ordering::SeqCst);
    let resumed = index
        .prepare_with(vault.clone(), &selected, &guard())
        .unwrap();
    assert!(resumed.reference == reference);
    assert_eq!(resumed.key.public_key(), expected);
    assert_eq!(vault.writes.load(Ordering::SeqCst), 1);
}

#[test]
fn ordinary_native_cancel_during_read_suppresses_ready_snapshot() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    let index = OrdinaryKeyIndex::in_app_sandbox(root.path()).unwrap();
    let cancel = CancellationToken::new();
    let vault = Arc::new(Vault::new(cancel.clone()));
    let selected = binding(42);
    let key = index
        .prepare_with(vault.clone(), &selected, &guard())
        .unwrap();
    vault.cancel_get.store(true, Ordering::SeqCst);
    assert!(index
        .prepare_with(
            vault.clone(),
            &selected,
            &OrdinaryAccountGuard::new(cancel, Arc::new(|| Ok(())))
        )
        .is_err());
    assert_eq!(vault.writes.load(Ordering::SeqCst), 1);
    vault.cancel_get.store(false, Ordering::SeqCst);
    assert!(
        index
            .prepare_with(vault, &selected, &guard())
            .unwrap()
            .reference
            == key.reference
    );
}

#[test]
fn ordinary_request_cancel_during_native_write_preserves_account_and_reserved_seed() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    let index = OrdinaryKeyIndex::in_app_sandbox(root.path()).unwrap();
    let request = CancellationToken::new();
    let account = OrdinaryAccountGuard::new(CancellationToken::new(), Arc::new(|| Ok(())));
    let task_guard = super::super::key_guard_for_request(&account, &request);
    let vault = Arc::new(Vault::new(request.clone()));
    vault.cancel_write.store(true, Ordering::SeqCst);
    let selected = binding(42);
    assert!(index
        .prepare_with(vault.clone(), &selected, &task_guard)
        .is_err());
    assert!(request.is_cancelled());
    assert!(
        account.check().is_ok(),
        "chooser cancellation must not revoke live tabs"
    );
    let directory = index.index.directory(false).unwrap();
    let pending = OrdinaryKeyIndex::read(&directory, &selected).unwrap();
    assert!(!pending.ready);
    let reference = SecretReference::parse(&pending.reference).unwrap();
    vault.cancel_write.store(false, Ordering::SeqCst);
    let next = super::super::key_guard_for_request(&account, &CancellationToken::new());
    let resumed = index.prepare_with(vault.clone(), &selected, &next).unwrap();
    assert!(resumed.reference == reference);
    assert_eq!(vault.writes.load(Ordering::SeqCst), 1);
    account.cancel.cancel();
    assert!(next.check().is_err());
}
