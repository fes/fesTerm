//! Mobile lifecycle admission for one device flow; never retries an issued request.
use std::sync::{Arc, Mutex};

use festerm_tunnel_client::{Error, IdentityRequest, IdentityTransport, ResponseFuture};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Native lifecycle owner. Ordinary background pauses admission; privacy cancellation
/// invalidates captured flows, including a worker that has not started yet.
#[derive(Clone)]
pub struct SignInActivity {
    foreground: watch::Sender<bool>,
    generation: Arc<Mutex<CancellationToken>>,
}

impl Default for SignInActivity {
    fn default() -> Self {
        Self {
            foreground: watch::channel(true).0,
            generation: Arc::new(Mutex::new(CancellationToken::new())),
        }
    }
}

impl SignInActivity {
    pub fn set_foreground(&self, foreground: bool) {
        self.foreground.send_replace(foreground);
    }

    pub fn cancel(&self) {
        let mut generation = crate::lock(&self.generation);
        generation.cancel();
        *generation = CancellationToken::new();
    }

    /// Capture on the initiating UI thread, before spawning its worker.
    #[must_use]
    pub fn capture(&self) -> SignInForeground {
        SignInForeground {
            foreground: self.foreground.subscribe(),
            cancelled: crate::lock(&self.generation).child_token(),
        }
    }
}

/// One flow's captured privacy generation and current foreground state.
pub struct SignInForeground {
    foreground: watch::Receiver<bool>,
    cancelled: CancellationToken,
}

impl SignInForeground {
    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.cancelled.clone()
    }
}

pub(crate) struct ForegroundIdentity<T> {
    pub transport: T,
    pub activity: Option<SignInForeground>,
}

impl<T> ForegroundIdentity<T> {
    pub(crate) fn cancelled(&self, cancel: &CancellationToken) -> bool {
        cancel.is_cancelled()
            || self
                .activity
                .as_ref()
                .is_some_and(|activity| activity.cancelled.is_cancelled())
    }

    pub(crate) fn commit<R>(
        &self,
        cancel: &CancellationToken,
        commit: impl FnOnce() -> R,
    ) -> Result<R, Error> {
        // Check before persistence; persist checks again after store admission.
        // Never hold the generation lock across credential IO.
        if self.cancelled(cancel) {
            return Err(Error::Cancelled);
        }
        let result = commit();
        // An issued store call cannot be undone, but its stale success must not
        // resume a UI operation after privacy/user cancellation.
        if self.cancelled(cancel) {
            return Err(Error::Cancelled);
        }
        Ok(result)
    }
}

impl<T: IdentityTransport> IdentityTransport for ForegroundIdentity<T> {
    fn request<'a>(
        &'a self,
        operation: IdentityRequest<'a>,
        cancel: &'a CancellationToken,
    ) -> ResponseFuture<'a> {
        Box::pin(async move {
            let Some(activity) = &self.activity else {
                return self.transport.request(operation, cancel).await;
            };
            if activity.cancelled.is_cancelled() || cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            // The identity layer's existing expiry select surrounds this wait.
            // An already-issued request is allowed to finish; never replay it.
            if matches!(operation, IdentityRequest::Poll { .. }) {
                let mut foreground = activity.foreground.clone();
                loop {
                    if *foreground.borrow_and_update() {
                        break;
                    }
                    tokio::select! {
                        biased;
                        _ = activity.cancelled.cancelled() => return Err(Error::Cancelled),
                        _ = cancel.cancelled() => return Err(Error::Cancelled),
                        changed = foreground.changed() => {
                            if changed.is_err() { return Err(Error::Cancelled); }
                        }
                    }
                }
            }
            tokio::select! {
                biased;
                _ = activity.cancelled.cancelled() => Err(Error::Cancelled),
                _ = cancel.cancelled() => Err(Error::Cancelled),
                result = self.transport.request(operation, cancel) => result,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_tunnel_client::identity::{self, SERVICE_CLIENT_ID};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use zeroize::Zeroizing;

    #[derive(Clone, Default)]
    struct Script {
        polls: Arc<AtomicUsize>,
        grants: Arc<AtomicUsize>,
    }
    impl IdentityTransport for Script {
        fn request<'a>(
            &'a self,
            operation: IdentityRequest<'a>,
            _: &'a CancellationToken,
        ) -> ResponseFuture<'a> {
            Box::pin(async move {
                let value = match operation {
                    IdentityRequest::Device { .. } => {
                        self.grants.fetch_add(1, Ordering::SeqCst);
                        r#"{"device_code":"PRIVATE_DEVICE","user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","expires_in":10,"interval":5}"#
                    }
                    IdentityRequest::Poll { .. } => {
                        self.polls.fetch_add(1, Ordering::SeqCst);
                        r#"{"access_token":"PRIVATE_TOKEN","token_type":"bearer","scope":""}"#
                    }
                    IdentityRequest::Account { .. } => r#"{"id":7,"login":"fixture"}"#,
                    _ => unreachable!(),
                };
                Ok(Zeroizing::new(value.as_bytes().to_vec()))
            })
        }
    }

    async fn settle() {
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
    }

    fn flow(
        activity: SignInForeground,
        script: Script,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<Result<identity::Identity, Error>> {
        tokio::spawn(async move {
            let transport = ForegroundIdentity {
                transport: script,
                activity: Some(activity),
            };
            identity::sign_in(&transport, SERVICE_CLIENT_ID, Some(7), &cancel, |_| {}).await
        })
    }

    #[tokio::test(start_paused = true)]
    async fn background_poll_waits_and_repeated_resume_uses_same_challenge_once() {
        let activity = SignInActivity::default();
        let script = Script::default();
        let job = flow(activity.capture(), script.clone(), CancellationToken::new());
        settle().await;
        activity.set_foreground(false);
        tokio::time::advance(Duration::from_secs(6)).await;
        settle().await;
        assert_eq!(script.polls.load(Ordering::SeqCst), 0);
        assert!(!job.is_finished());
        activity.set_foreground(true);
        activity.set_foreground(true);
        assert!(job.await.unwrap().is_ok());
        assert_eq!(script.grants.load(Ordering::SeqCst), 1);
        assert_eq!(script.polls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn provider_expiry_remains_live_while_background_paused() {
        let activity = SignInActivity::default();
        let script = Script::default();
        let job = flow(activity.capture(), script.clone(), CancellationToken::new());
        settle().await;
        activity.set_foreground(false);
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(matches!(job.await.unwrap(), Err(Error::Expired)));
        assert_eq!(script.polls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn user_cancel_and_privacy_cancel_refuse_paused_and_not_yet_started_flows() {
        for privacy in [false, true] {
            let activity = SignInActivity::default();
            let script = Script::default();
            let cancel = CancellationToken::new();
            let job = flow(activity.capture(), script.clone(), cancel.clone());
            settle().await;
            activity.set_foreground(false);
            tokio::time::advance(Duration::from_secs(6)).await;
            settle().await;
            if privacy {
                activity.cancel();
            } else {
                cancel.cancel();
            }
            assert!(matches!(job.await.unwrap(), Err(Error::Cancelled)));
            assert_eq!(script.polls.load(Ordering::SeqCst), 0);
        }
        let activity = SignInActivity::default();
        let captured = activity.capture();
        activity.cancel();
        let script = Script::default();
        assert!(matches!(
            flow(captured, script.clone(), CancellationToken::new())
                .await
                .unwrap(),
            Err(Error::Cancelled)
        ));
        assert_eq!(script.grants.load(Ordering::SeqCst), 0);
        // Fresh scope after explicit cancellation remains usable.
        assert!(flow(activity.capture(), script, CancellationToken::new())
            .await
            .unwrap()
            .is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn already_resumed_activity_captured_later_does_not_miss_resume() {
        let activity = SignInActivity::default();
        activity.set_foreground(false);
        activity.set_foreground(true);
        let script = Script::default();
        assert!(
            flow(activity.capture(), script.clone(), CancellationToken::new())
                .await
                .unwrap()
                .is_ok()
        );
        assert_eq!(script.polls.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn privacy_generation_rejects_stale_commit_and_allows_fresh_scope() {
        let activity = SignInActivity::default();
        let transport = ForegroundIdentity {
            transport: (),
            activity: Some(activity.capture()),
        };
        activity.cancel();
        let mut committed = false;
        assert!(matches!(
            transport.commit(&CancellationToken::new(), || committed = true),
            Err(Error::Cancelled)
        ));
        assert!(!committed);
        let transport = ForegroundIdentity {
            transport: (),
            activity: Some(activity.capture()),
        };
        assert!(transport
            .commit(&CancellationToken::new(), || committed = true)
            .is_ok());
        assert!(committed);
    }

    #[tokio::test(start_paused = true)]
    async fn in_flight_poll_is_not_cancelled_or_replayed_on_background() {
        struct Held {
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
            calls: Arc<AtomicUsize>,
        }
        impl IdentityTransport for Held {
            fn request<'a>(
                &'a self,
                _: IdentityRequest<'a>,
                _: &'a CancellationToken,
            ) -> ResponseFuture<'a> {
                Box::pin(async move {
                    self.calls.fetch_add(1, Ordering::SeqCst);
                    self.entered.notify_one();
                    self.release.notified().await;
                    Err(Error::Network)
                })
            }
        }
        let activity = SignInActivity::default();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let transport = ForegroundIdentity {
            transport: Held {
                entered: entered.clone(),
                release: release.clone(),
                calls: calls.clone(),
            },
            activity: Some(activity.capture()),
        };
        let task = tokio::spawn(async move {
            transport
                .request(
                    IdentityRequest::Poll {
                        client_id: SERVICE_CLIENT_ID,
                        device_code: "PRIVATE_DEVICE",
                    },
                    &CancellationToken::new(),
                )
                .await
        });
        entered.notified().await;
        activity.set_foreground(false);
        release.notify_one();
        assert!(matches!(task.await.unwrap(), Err(Error::Network)));
        activity.set_foreground(true);
        settle().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test(start_paused = true)]
    async fn queued_profile_store_admission_rechecks_cancellation_before_secret_write() {
        use festerm_secret_store::{
            MemorySecretStore, SecretBytes, SecretReference, SecretStore, SecretStoreError,
        };
        struct Counted {
            inner: MemorySecretStore,
            writes: AtomicUsize,
        }
        impl SecretStore for Counted {
            fn put(&self, value: &SecretBytes) -> Result<SecretReference, SecretStoreError> {
                self.writes.fetch_add(1, Ordering::SeqCst);
                self.inner.put(value)
            }
            fn get(&self, reference: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
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
        let identity = flow(
            SignInActivity::default().capture(),
            Script::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        let directory = std::env::temp_dir().join(format!(
            "festerm-queued-sign-in-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let secrets = Arc::new(Counted {
            inner: MemorySecretStore::new(),
            writes: AtomicUsize::new(0),
        });
        let store = Arc::new(Mutex::new(
            crate::ProfileStore::open(&directory, secrets.clone()).unwrap(),
        ));
        let activity = SignInActivity::default();
        let transport = ForegroundIdentity {
            transport: (),
            activity: Some(activity.capture()),
        };
        let held = crate::lock(&store); // Represents older native credential IO.
        let worker_store = store.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let cancel = CancellationToken::new();
            transport.commit(&cancel, || {
                entered_tx.send(()).unwrap(); // Precheck passed; store is still held.
                crate::persist(&worker_store, &identity, || transport.cancelled(&cancel))
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        activity.cancel(); // Must return even while the store is unavailable.
        drop(held);
        let result = worker.join().unwrap();
        let writes = secrets.writes.load(Ordering::SeqCst);
        let accounts = crate::lock(&store).accounts().len();
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
        assert_eq!(writes, 0, "cancelled queued flow issued a new native write");
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(accounts, 0);
    }
    #[test]
    fn completed_unconsumed_sign_in_rejects_privacy_or_user_cancel_but_not_background() {
        let activity = SignInActivity::default();
        let captured = activity.capture();
        let flow = crate::SignIn {
            prompt: crate::SignInPrompt {
                user_code: "ABCD-1234".into(),
                verification_url: identity::VERIFICATION_URI.into(),
                expires_in: Duration::from_secs(900),
            },
            state: Arc::new(Mutex::new(crate::SignInState::Done(crate::AccountInfo {
                id: "fixture".into(),
                login: "fixture".into(),
            }))),
            cancel: CancellationToken::new(),
            privacy_cancel: Some(captured.cancellation_token()),
        };
        activity.set_foreground(false);
        assert!(matches!(flow.state(), crate::SignInState::Done(_)));
        activity.cancel();
        assert_eq!(
            flow.state(),
            crate::SignInState::Failed(crate::TunnelError::Cancelled)
        );
        // Legacy/default API flows also refuse a success after explicit Cancel.
        let flow = crate::SignIn {
            privacy_cancel: None,
            cancel: CancellationToken::new(),
            prompt: flow.prompt.clone(),
            state: flow.state.clone(),
        };
        flow.cancel();
        assert_eq!(
            flow.state(),
            crate::SignInState::Failed(crate::TunnelError::Cancelled)
        );
    }
}
