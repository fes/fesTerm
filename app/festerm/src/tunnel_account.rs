//! Renewal under the caller's existing per-account lease and cancellation fence.
use festerm_config::DevTunnelAccount;
use festerm_secret_store::{SecretBytes, SecretStore};
use festerm_tunnel_client::{identity::Identity, Error, IdentityTransport};
use tokio_util::sync::CancellationToken;

pub(super) fn publish(
    account: &DevTunnelAccount,
    store: &dyn SecretStore,
    bytes: &[u8],
    check: &impl Fn() -> Result<(), Error>,
) -> Result<(), Error> {
    check()?;
    store
        .update(
            account.credential_reference(),
            &SecretBytes::copy_from_slice(bytes),
        )
        .map_err(|_| Error::Store)?;
    let saved = store
        .get(account.credential_reference())
        .map_err(|_| Error::Store)?;
    if !saved.with_bytes(|saved| saved == bytes) {
        return Err(Error::Store);
    }
    check()
}

/// The caller retains its account lease until this returns. Before a possibly
/// consuming exchange, persist the existing private record without refresh
/// authority and verify it. An uncertain exchange/publication cannot replay the
/// old pair after reopening. No new device sign-in or account fallback occurs.
pub(super) async fn renew(
    identity: Identity,
    account: &DevTunnelAccount,
    store: &dyn SecretStore,
    client: &str,
    transport: &impl IdentityTransport,
    cancel: &CancellationToken,
    check: &impl Fn() -> Result<(), Error>,
) -> Result<Identity, Error> {
    check()?;
    if identity.account.id != account.account_id() {
        return Err(Error::AccountChanged);
    }
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    if !identity.needs_sign_in() {
        return Ok(identity);
    }
    publish(account, store, &identity.encode_refresh_attempt()?, check)?;
    let refreshed = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        identity.refresh(transport, client, cancel),
    )
    .await
    .map_err(|_| Error::Expired)??;
    check()?;
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    publish(account, store, &refreshed.encode_private()?, check)?;
    Ok(refreshed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_config::AccountProvider;
    use festerm_secret_store::{MemorySecretStore, SecretReference, SecretStoreError};
    use festerm_tunnel_client::{IdentityRequest, ResponseFuture};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Store {
        inner: MemorySecretStore,
        writes: AtomicUsize,
        fail_write: usize,
        lose_write: usize,
    }
    impl SecretStore for Store {
        fn put(&self, bytes: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
            self.inner.put(bytes)
        }
        fn get(&self, key: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
            self.inner.get(key)
        }
        fn delete(&self, key: &SecretReference) -> Result<bool, SecretStoreError> {
            self.inner.delete(key)
        }
        fn update(
            &self,
            key: &SecretReference,
            bytes: &SecretBytes,
        ) -> Result<(), SecretStoreError> {
            let n = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
            if n == self.fail_write {
                return Err(SecretStoreError::Unsupported);
            }
            if n == self.lose_write {
                return Ok(());
            }
            self.inner.update(key, bytes)
        }
    }
    struct Transport {
        calls: AtomicUsize,
        fail: bool,
        wrong_account: bool,
    }
    impl IdentityTransport for Transport {
        fn request<'a>(
            &'a self,
            request: IdentityRequest<'a>,
            _: &'a CancellationToken,
        ) -> ResponseFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if self.fail {
                    return Err(Error::Network);
                }
                Ok(match request {
                    IdentityRequest::Refresh { .. } => br#"{"access_token":"NEW_ACCESS","refresh_token":"NEW_REFRESH","token_type":"bearer","scope":"","expires_in":3600,"refresh_token_expires_in":7200}"#.to_vec(),
                    IdentityRequest::Account { .. } if self.wrong_account => br#"{"id":43,"login":"other"}"#.to_vec(),
                    IdentityRequest::Account { .. } => br#"{"id":42,"login":"owner"}"#.to_vec(),
                    _ => panic!("renewal must never start device sign-in"),
                }.into())
            })
        }
    }
    fn fixture(fail_write: usize, lose_write: usize) -> (Store, DevTunnelAccount) {
        let store = Store {
            inner: MemorySecretStore::new(),
            writes: AtomicUsize::new(0),
            fail_write,
            lose_write,
        };
        let mut bytes = b"FESTERM_GITHUB_IDENTITY_V2\0".to_vec();
        bytes.extend_from_slice(br#"{"account":{"id":42,"login":"owner"},"client_id":"ownedApp","access_token":"OLD_ACCESS","refresh_token":"OLD_REFRESH","expires_at":0,"refresh_expires_at":null}"#);
        let key = store.put(&SecretBytes::copy_from_slice(&bytes)).unwrap();
        let account =
            DevTunnelAccount::new("owner", AccountProvider::Github, 42, "owner", key).unwrap();
        (store, account)
    }
    fn load(store: &Store, account: &DevTunnelAccount) -> Identity {
        store
            .get(account.credential_reference())
            .unwrap()
            .with_bytes(|bytes| Identity::decode_private(bytes, 42, "ownedApp"))
            .unwrap()
    }
    fn transport(fail: bool, wrong_account: bool) -> Transport {
        Transport {
            calls: AtomicUsize::new(0),
            fail,
            wrong_account,
        }
    }

    #[tokio::test]
    async fn expired_identity_renews_and_publishes_exact_same_account() {
        let (store, account) = fixture(0, 0);
        let http = transport(false, false);
        let result = renew(
            load(&store, &account),
            &account,
            &store,
            "ownedApp",
            &http,
            &CancellationToken::new(),
            &|| Ok(()),
        )
        .await
        .unwrap();
        assert!(!result.needs_sign_in());
        assert_eq!(result.account.id, account.account_id());
        assert!(!load(&store, &account).needs_sign_in());
        assert_eq!(store.writes.load(Ordering::SeqCst), 2);
        assert_eq!(http.calls.load(Ordering::SeqCst), 2);
        let unused = transport(true, false);
        let current = renew(
            load(&store, &account),
            &account,
            &store,
            "ownedApp",
            &unused,
            &CancellationToken::new(),
            &|| Ok(()),
        )
        .await
        .unwrap();
        assert!(!current.needs_sign_in());
        assert_eq!(unused.calls.load(Ordering::SeqCst), 0);
        assert_eq!(store.writes.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn failed_or_unverified_fence_never_exchanges() {
        for (fail, lost) in [(1, 0), (0, 1)] {
            let (store, account) = fixture(fail, lost);
            let http = transport(false, false);
            assert_eq!(
                renew(
                    load(&store, &account),
                    &account,
                    &store,
                    "ownedApp",
                    &http,
                    &CancellationToken::new(),
                    &|| Ok(())
                )
                .await
                .unwrap_err(),
                Error::Store
            );
            assert_eq!(http.calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn uncertain_exchange_or_publication_cannot_replay_after_reopen() {
        for (network, wrong, fail, lost, expected) in [
            (true, false, 0, 0, Error::Network),
            (false, true, 0, 0, Error::AccountChanged),
            (false, false, 2, 0, Error::Store),
            (false, false, 0, 2, Error::Store),
        ] {
            let (store, account) = fixture(fail, lost);
            let http = transport(network, wrong);
            assert_eq!(
                renew(
                    load(&store, &account),
                    &account,
                    &store,
                    "ownedApp",
                    &http,
                    &CancellationToken::new(),
                    &|| Ok(())
                )
                .await
                .unwrap_err(),
                expected
            );
            let calls = http.calls.load(Ordering::SeqCst);
            assert!(!load(&store, &account).can_refresh());
            assert_eq!(
                renew(
                    load(&store, &account),
                    &account,
                    &store,
                    "ownedApp",
                    &http,
                    &CancellationToken::new(),
                    &|| Ok(())
                )
                .await
                .unwrap_err(),
                Error::Expired
            );
            assert_eq!(http.calls.load(Ordering::SeqCst), calls);
        }
    }

    #[tokio::test]
    async fn cancellation_and_changed_guard_never_mutate_or_exchange() {
        for cancelled in [true, false] {
            let (store, account) = fixture(0, 0);
            let http = transport(false, false);
            let cancel = CancellationToken::new();
            if cancelled {
                cancel.cancel();
            }
            assert_eq!(
                renew(
                    load(&store, &account),
                    &account,
                    &store,
                    "ownedApp",
                    &http,
                    &cancel,
                    &|| if cancelled {
                        Ok(())
                    } else {
                        Err(Error::AccountChanged)
                    }
                )
                .await
                .unwrap_err(),
                if cancelled {
                    Error::Cancelled
                } else {
                    Error::AccountChanged
                }
            );
            assert_eq!(store.writes.load(Ordering::SeqCst), 0);
            assert_eq!(http.calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn guard_loss_after_exchange_keeps_attempt_fenced_without_publication() {
        let (store, account) = fixture(0, 0);
        let http = transport(false, false);
        let checks = std::cell::Cell::new(0);
        let check = || {
            checks.set(checks.get() + 1);
            if checks.get() >= 4 {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        };
        assert_eq!(
            renew(
                load(&store, &account),
                &account,
                &store,
                "ownedApp",
                &http,
                &CancellationToken::new(),
                &check
            )
            .await
            .unwrap_err(),
            Error::Cancelled
        );
        assert_eq!(http.calls.load(Ordering::SeqCst), 2);
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
        assert!(!load(&store, &account).can_refresh());
    }
}
