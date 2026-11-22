//! Offline coverage of the real selected-account preparation and native-store boundary.
use super::*;
use festerm_secret_store::{MemorySecretStore, SecretReference, SecretStore, SecretStoreError};
use std::sync::atomic::{AtomicUsize, Ordering};
use testing::{seed_account, FixedDirectory, StreamConnector, CLIENT_ID};

#[derive(Default)]
struct Store {
    inner: MemorySecretStore,
    reads: AtomicUsize,
    fail_read: AtomicUsize,
    writes: AtomicUsize,
    fail: AtomicUsize,
    lose: AtomicUsize,
    fail_after: AtomicUsize,
    cancel_after_write: Mutex<Option<(usize, CancellationToken)>>,
}
impl SecretStore for Store {
    fn put(&self, bytes: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
        self.inner.put(bytes)
    }
    fn get(&self, key: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
        let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.fail_read.load(Ordering::SeqCst) {
            return Err(SecretStoreError::Unsupported);
        }
        self.inner.get(key)
    }
    fn delete(&self, key: &SecretReference) -> Result<bool, SecretStoreError> {
        self.inner.delete(key)
    }
    fn update(&self, key: &SecretReference, bytes: &SecretBytes) -> Result<(), SecretStoreError> {
        let n = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.fail.load(Ordering::SeqCst) {
            return Err(SecretStoreError::Unsupported);
        }
        if n == self.lose.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.inner.update(key, bytes)?;
        if let Some((at, cancel)) = &*lock(&self.cancel_after_write) {
            if n == *at {
                cancel.cancel();
            }
        }
        if n == self.fail_after.load(Ordering::SeqCst) {
            return Err(SecretStoreError::Unsupported);
        }
        Ok(())
    }
}
struct Http {
    store: Arc<Store>,
    reference: SecretReference,
    refreshes: AtomicUsize,
    fail: bool,
    wrong: bool,
    on_account: Option<Arc<dyn Fn() + Send + Sync>>,
}
impl IdentityTransport for Http {
    fn request<'a>(
        &'a self,
        request: IdentityRequest<'a>,
        cancel: &'a CancellationToken,
    ) -> ResponseFuture<'a> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(ClientError::Cancelled);
            }
            let bytes = match request {
                IdentityRequest::Refresh { .. } => {
                    self.refreshes.fetch_add(1, Ordering::SeqCst);
                    let current = self.store.inner.get(&self.reference).unwrap();
                    let identity = current
                        .with_bytes(|bytes| Identity::decode_private(bytes, 42, CLIENT_ID))
                        .unwrap();
                    assert!(identity.needs_sign_in());
                    assert!(
                        !identity.can_refresh(),
                        "exchange must follow a verified one-use fence"
                    );
                    if self.fail {
                        return Err(ClientError::Network);
                    }
                    br#"{"access_token":"NEW_ACCESS","refresh_token":"NEW_REFRESH","token_type":"bearer","scope":"","expires_in":3600,"refresh_token_expires_in":7200}"#.to_vec()
                }
                IdentityRequest::Account { .. } => {
                    if let Some(action) = &self.on_account {
                        action();
                    }
                    if self.wrong {
                        br#"{"id":99,"login":"other"}"#.to_vec()
                    } else {
                        br#"{"id":42,"login":"owner"}"#.to_vec()
                    }
                }
                _ => panic!("renewal must not start a device sign-in"),
            };
            Ok(bytes.into())
        })
    }
}
fn fixture(
    fail: bool,
    wrong: bool,
    on_account: Option<Arc<dyn Fn() + Send + Sync>>,
) -> (Arc<TunnelClient>, Arc<Store>, Arc<Http>, TunnelAccount) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/tmp")
        .join(format!(
            "identity-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
    std::fs::create_dir_all(&root).unwrap();
    let secrets = Arc::new(Store::default());
    let profiles = Arc::new(Mutex::new(
        ProfileStore::open(&root, secrets.clone()).unwrap(),
    ));
    let id = seed_account(&profiles, 42, "owner", true);
    let record = lock(&profiles).account(&id).unwrap().clone();
    let http = Arc::new(Http {
        store: secrets.clone(),
        reference: record.credential.to_reference(),
        refreshes: AtomicUsize::new(0),
        fail,
        wrong,
        on_account,
    });
    let client = TunnelClient::with_parts(
        profiles,
        http.clone(),
        Arc::new(FixedDirectory::new()),
        Arc::new(StreamConnector(|| -> BoxStream {
            panic!("identity work must not connect a relay")
        })),
        CLIENT_ID.into(),
    )
    .unwrap();
    (Arc::new(client), secrets, http, record)
}
fn load(store: &Store, record: &TunnelAccount) -> Identity {
    store
        .inner
        .get(&record.credential.to_reference())
        .unwrap()
        .with_bytes(|bytes| Identity::decode_private(bytes, record.account_id, CLIENT_ID))
        .unwrap()
}

#[test]
fn selected_identity_fences_exchange_and_reuses_fresh_pair_for_siblings_and_legacy() {
    let (client, store, http, record) = fixture(false, false, None);
    let sibling = seed_account(&client.store, 99, "sibling", false);
    let sibling_record = lock(&client.store).account(&sibling).unwrap().clone();
    let sibling_bytes = store
        .inner
        .get(&sibling_record.credential.to_reference())
        .unwrap();
    let barrier = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            client.prepare_account_identity(&record, &CancellationToken::new(), &|| Ok(()))
        });
        let b = scope.spawn(|| {
            barrier.wait();
            client.prepare_account_identity(&record, &CancellationToken::new(), &|| Ok(()))
        });
        let c = scope.spawn(|| {
            barrier.wait();
            client.list_tunnels(&record.id, &CancelToken::new())
        });
        barrier.wait();
        assert_eq!(c.join().unwrap().unwrap().len(), 1);
        assert!(!a.join().unwrap().unwrap().needs_sign_in());
        assert!(!b.join().unwrap().unwrap().needs_sign_in());
    });
    assert_eq!(http.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(store.writes.load(Ordering::SeqCst), 2);
    assert!(!load(&store, &record).needs_sign_in());
    client
        .list_tunnels(&record.id, &CancelToken::new())
        .unwrap();
    assert_eq!(http.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(lock(&client.store).account(&record.id), Some(&record));
    assert!(store
        .inner
        .get(&sibling_record.credential.to_reference())
        .unwrap()
        .with_bytes(|after| sibling_bytes.with_bytes(|before| before == after)));
}

#[test]
fn selected_identity_failed_or_false_ack_fence_sends_zero_exchange() {
    for mode in 0..3 {
        let (client, store, http, record) = fixture(false, false, None);
        match mode {
            0 => store.fail.store(1, Ordering::SeqCst),
            1 => store.lose.store(1, Ordering::SeqCst),
            _ => store.fail_after.store(1, Ordering::SeqCst),
        }
        assert_eq!(
            client
                .prepare_account_identity(&record, &CancellationToken::new(), &|| Ok(()))
                .unwrap_err(),
            TunnelError::SecureStorage
        );
        assert_eq!(http.refreshes.load(Ordering::SeqCst), 0);
        if mode == 2 {
            assert!(!load(&store, &record).can_refresh());
        }
    }
}

#[test]
fn selected_identity_uncertain_exchange_or_publication_cannot_replay_consumed_pair() {
    for mode in 0..5 {
        let (client, store, http, record) = fixture(mode == 0, mode == 1, None);
        match mode {
            2 => store.fail.store(2, Ordering::SeqCst),
            3 => store.lose.store(2, Ordering::SeqCst),
            4 => store.fail_after.store(2, Ordering::SeqCst),
            _ => {}
        }
        let failure = client
            .prepare_account_identity(&record, &CancellationToken::new(), &|| Ok(()))
            .unwrap_err();
        assert_eq!(
            failure,
            match mode {
                0 => TunnelError::Network,
                1 => TunnelError::AccountChanged,
                _ => TunnelError::SecureStorage,
            }
        );
        assert_eq!(http.refreshes.load(Ordering::SeqCst), 1);
        let reopened =
            client.prepare_account_identity(&record, &CancellationToken::new(), &|| Ok(()));
        if mode == 4 {
            assert!(!reopened.unwrap().needs_sign_in());
        }
        // Committed replacement may be verified afresh.
        else {
            assert_eq!(reopened.unwrap_err(), TunnelError::SignInRequired);
            assert!(!load(&store, &record).can_refresh());
        }
        assert_eq!(http.refreshes.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn selected_identity_privacy_and_snapshot_refusal_precede_vault_access() {
    let (client, store, http, record) = fixture(false, false, None);
    store.reads.store(0, Ordering::SeqCst);
    assert_eq!(
        client
            .prepare_account_identity(&record, &CancellationToken::new(), &|| Err(
                TunnelError::Cancelled
            ))
            .unwrap_err(),
        TunnelError::Cancelled
    );
    let mut stale = record.clone();
    stale.login.push_str("-changed");
    assert_eq!(
        client
            .prepare_account_identity(&stale, &CancellationToken::new(), &|| Ok(()))
            .unwrap_err(),
        TunnelError::AccountChanged
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        client
            .prepare_account_identity(&record, &cancel, &|| Err(TunnelError::AccountChanged))
            .unwrap_err(),
        TunnelError::Cancelled
    );
    assert_eq!(store.reads.load(Ordering::SeqCst), 0);
    assert_eq!(store.writes.load(Ordering::SeqCst), 0);
    assert_eq!(http.refreshes.load(Ordering::SeqCst), 0);
}

#[test]
fn selected_identity_native_and_decode_failures_do_not_claim_expiry() {
    let (client, store, http, record) = fixture(false, false, None);
    store
        .inner
        .update(
            &record.credential.to_reference(),
            &SecretBytes::copy_from_slice(b"invalid"),
        )
        .unwrap();
    assert_eq!(
        client
            .prepare_account_identity(&record, &CancellationToken::new(), &|| Ok(()))
            .unwrap_err(),
        TunnelError::Protocol
    );
    store
        .inner
        .delete(&record.credential.to_reference())
        .unwrap();
    assert_eq!(
        client
            .prepare_account_identity(&record, &CancellationToken::new(), &|| Ok(()))
            .unwrap_err(),
        TunnelError::SecureStorage
    );
    assert_eq!(http.refreshes.load(Ordering::SeqCst), 0);
}

#[test]
fn selected_identity_cancellation_after_exchange_keeps_fence_and_never_publishes() {
    let cancel = CancellationToken::new();
    let interrupted = cancel.clone();
    let (client, store, http, record) =
        fixture(false, false, Some(Arc::new(move || interrupted.cancel())));
    assert_eq!(
        client
            .prepare_account_identity(&record, &cancel, &|| Ok(()))
            .unwrap_err(),
        TunnelError::Cancelled
    );
    assert!(!load(&store, &record).can_refresh());
    assert_eq!(store.writes.load(Ordering::SeqCst), 1);
    assert_eq!(http.refreshes.load(Ordering::SeqCst), 1);
}

#[test]
fn selected_identity_cancelled_after_fence_never_exchanges_or_rearms() {
    let (client, store, http, record) = fixture(false, false, None);
    let cancel = CancellationToken::new();
    *lock(&store.cancel_after_write) = Some((1, cancel.clone()));
    assert_eq!(
        client
            .prepare_account_identity(&record, &cancel, &|| Ok(()))
            .unwrap_err(),
        TunnelError::Cancelled
    );
    assert_eq!(http.refreshes.load(Ordering::SeqCst), 0);
    assert_eq!(store.writes.load(Ordering::SeqCst), 1);
    assert!(!load(&store, &record).can_refresh());
    assert_eq!(
        client
            .prepare_account_identity(&record, &CancellationToken::new(), &|| Ok(()))
            .unwrap_err(),
        TunnelError::SignInRequired
    );
    assert_eq!(http.refreshes.load(Ordering::SeqCst), 0);
}

#[test]
fn selected_identity_account_replacement_after_exchange_refuses_old_snapshot_publication() {
    let changed = Arc::new(Mutex::new(None::<Arc<Mutex<ProfileStore>>>));
    let slot = changed.clone();
    let (client, store, http, record) = fixture(
        false,
        false,
        Some(Arc::new(move || {
            seed_account(lock(&slot).as_ref().unwrap(), 42, "owner", false);
        })),
    );
    *lock(&changed) = Some(client.store.clone());
    assert_eq!(
        client
            .prepare_account_identity(&record, &CancellationToken::new(), &|| Ok(()))
            .unwrap_err(),
        TunnelError::AccountChanged
    );
    assert_eq!(http.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(store.writes.load(Ordering::SeqCst), 1);
    let replacement = lock(&client.store).account(&record.id).unwrap().clone();
    assert_ne!(replacement.credential, record.credential);
    assert!(!load(&store, &replacement).needs_sign_in());
}

#[test]
fn selected_identity_native_read_and_both_publication_readbacks_fail_closed() {
    for failed_read in [1, 3, 5] {
        let (client, store, http, record) = fixture(false, false, None);
        store.reads.store(0, Ordering::SeqCst);
        store.fail_read.store(failed_read, Ordering::SeqCst);
        assert_eq!(
            client
                .prepare_account_identity(&record, &CancellationToken::new(), &|| Ok(()))
                .unwrap_err(),
            TunnelError::SecureStorage
        );
        assert_eq!(
            http.refreshes.load(Ordering::SeqCst),
            usize::from(failed_read == 5)
        );
        if failed_read == 3 {
            assert!(!load(&store, &record).can_refresh());
        }
        if failed_read == 5 {
            assert!(!load(&store, &record).needs_sign_in());
        }
    }
}

#[test]
fn selected_identity_changed_native_pair_after_exchange_is_not_overwritten() {
    type Replacement = Option<(Arc<Store>, TunnelAccount)>;
    let changed = Arc::new(Mutex::new(None::<(Arc<Store>, TunnelAccount)>));
    let slot: Arc<Mutex<Replacement>> = changed.clone();
    let (client, store, http, record) = fixture(
        false,
        false,
        Some(Arc::new(move || {
            let replacement = lock(&slot);
            let (store, record) = replacement.as_ref().unwrap();
            let mut bytes = b"FESTERM_GITHUB_IDENTITY_V2\0".to_vec();
            bytes.extend_from_slice(format!(r#"{{"account":{{"id":42,"login":"owner"}},"client_id":"{CLIENT_ID}","access_token":"OTHER_ACCESS","refresh_token":"OTHER_REFRESH","expires_at":null,"refresh_expires_at":null}}"#).as_bytes());
            store
                .inner
                .update(
                    &record.credential.to_reference(),
                    &SecretBytes::copy_from_slice(&bytes),
                )
                .unwrap();
        })),
    );
    *lock(&changed) = Some((store.clone(), record.clone()));
    assert_eq!(
        client
            .prepare_account_identity(&record, &CancellationToken::new(), &|| Ok(()))
            .unwrap_err(),
        TunnelError::SecureStorage
    );
    assert_eq!(http.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(store.writes.load(Ordering::SeqCst), 1);
    assert!(!load(&store, &record).needs_sign_in());
}
