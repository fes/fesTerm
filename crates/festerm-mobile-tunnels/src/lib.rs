//! UI-agnostic Dev Tunnels client backend for the fesTerm iOS app (ADR 0057).
//!
//! Every call blocks the calling thread (the host runs them on workers), is
//! bounded by a timeout and observes a [`CancelToken`]. Calls must not be made
//! from inside a Tokio runtime. Secrets live only in the secret store (iOS
//! Keychain); this crate never logs or returns tokens.
//!
//! Flow: [`TunnelClient::begin_sign_in`] -> [`TunnelClient::list_tunnels`] ->
//! [`TunnelClient::describe_tunnel`] (host fingerprint + remote sessions) ->
//! profile saved by the UI, host approved with [`TunnelClient::approve_host`] ->
//! [`TunnelClient::attach`] -> an [`Attachment`] hosting a
//! [`festerm_session::Session`].
#![forbid(unsafe_code)]

mod error;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use festerm_mobile_profiles::{
    AccountProvider, DevTunnelProfile, Profile, ProfileId, ProfileKind, ProfileStore,
    TrustedTunnelHost, TunnelSource,
};
use festerm_secret_store::SecretBytes;
use festerm_session::{Session, SessionEventNotifier};
use festerm_tunnel_bridge::{
    mobile::{self, BoxStream},
    relay,
};
use festerm_tunnel_client::{
    identity::{self, Identity},
    service::{self, HostBinding, ResolvedTunnel, TunnelSummary},
    Error as ClientError, Https, IdentityRequest, IdentityTransport, ResponseFuture,
};
use tokio_util::sync::CancellationToken;

pub use error::TunnelError;
pub use festerm_tunnel_bridge::mobile::{apply_recovery_sync, Displacer, EndReason, TunnelSession};

const PROMPT_WAIT: Duration = Duration::from_secs(30);
const NETWORK_LIMIT: Duration = Duration::from_secs(45);
const ATTACH_LIMIT: Duration = Duration::from_secs(60);
/// GitHub's device codes live at most this long; the identity layer enforces
/// the actual (`expires_in`) deadline and reports [`TunnelError::Expired`].
pub const DEVICE_CODE_MAX_LIFETIME: Duration = Duration::from_secs(900);

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Tunnel service operations, replaceable in tests.
pub trait TunnelDirectory: Send + Sync {
    fn list<'a>(
        &'a self,
        identity: &'a Identity,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<TunnelSummary>, ClientError>>;
    fn resolve<'a>(
        &'a self,
        identity: &'a Identity,
        name: &'a str,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<ResolvedTunnel, ClientError>>;
}

/// Opens the encrypted byte stream to a resolved bridge, replaceable in tests.
pub trait BridgeConnector: Send + Sync {
    fn connect<'a>(&'a self, resolved: &'a ResolvedTunnel) -> BoxFuture<'a, io::Result<BoxStream>>;
}

struct Microsoft;
impl TunnelDirectory for Microsoft {
    fn list<'a>(
        &'a self,
        identity: &'a Identity,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<TunnelSummary>, ClientError>> {
        Box::pin(service::list(identity, cancel))
    }
    fn resolve<'a>(
        &'a self,
        identity: &'a Identity,
        name: &'a str,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<ResolvedTunnel, ClientError>> {
        Box::pin(service::resolve(identity, name, cancel))
    }
}
impl BridgeConnector for Microsoft {
    fn connect<'a>(&'a self, resolved: &'a ResolvedTunnel) -> BoxFuture<'a, io::Result<BoxStream>> {
        Box::pin(async move {
            let connection = relay::connect_endpoint(
                &resolved.endpoint,
                resolved.binding.port,
                &resolved.connect_token,
            )
            .await?;
            Ok(Box::new(connection) as BoxStream)
        })
    }
}

struct DynIdentity(Arc<dyn IdentityTransport>);
impl IdentityTransport for DynIdentity {
    fn request<'a>(
        &'a self,
        operation: IdentityRequest<'a>,
        cancel: &'a CancellationToken,
    ) -> ResponseFuture<'a> {
        self.0.request(operation, cancel)
    }
}

/// Cloneable cancellation handle for one blocking call or sign-in.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(CancellationToken);
impl CancelToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.cancel();
    }
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

/// A signed-in GitHub account as stored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountInfo {
    /// Stable id profiles refer to.
    pub id: String,
    pub login: String,
}

/// What the sign-in sheet shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignInPrompt {
    pub user_code: String,
    pub verification_url: String,
    /// Upper bound of the code's lifetime; sign-in fails with `Expired` at the
    /// service's real deadline.
    pub expires_in: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignInState {
    Waiting,
    Done(AccountInfo),
    Failed(TunnelError),
}

/// A running device-flow sign-in. Dropping it cancels the flow.
pub struct SignIn {
    prompt: SignInPrompt,
    state: Arc<Mutex<SignInState>>,
    cancel: CancellationToken,
}
impl SignIn {
    #[must_use]
    pub fn prompt(&self) -> &SignInPrompt {
        &self.prompt
    }
    /// Non-blocking; poll from the UI timer.
    #[must_use]
    pub fn state(&self) -> SignInState {
        lock(&self.state).clone()
    }
    /// Blocks until the flow finishes or `timeout` elapses.
    #[must_use]
    pub fn wait(&self, timeout: Duration) -> SignInState {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let state = self.state();
            if state != SignInState::Waiting || std::time::Instant::now() >= deadline {
                return state;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}
impl Drop for SignIn {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunnelInfo {
    /// `id.cluster`, accepted as a profile's tunnel name.
    pub locator: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub active: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostTrust {
    /// Never approved: show the fingerprint ("Verified host - fesTerm bridge").
    New,
    Trusted,
    Changed {
        approved_fingerprint: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionState {
    Unattached,
    /// Held by a client; `by` says whether it is local to the host.
    Attached {
        by: Option<Displacer>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionInfo {
    pub name: String,
    pub state: SessionState,
    /// Whether the host lets a remote client displace the holder.
    pub takeover_allowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunnelDescription {
    pub locator: String,
    pub host: TrustedTunnelHost,
    pub fingerprint: String,
    pub trust: HostTrust,
    pub sessions: Vec<SessionInfo>,
}

/// A live attachment. Hand [`Self::session`] to the terminal view.
pub struct Attachment {
    session: Arc<TunnelSession>,
    /// Set when this attach displaced a holder (`Some(None)`: holder unknown).
    pub took_over: Option<Option<Displacer>>,
    pub session_name: String,
}
impl Attachment {
    #[must_use]
    pub fn session(&self) -> Arc<dyn Session> {
        self.session.clone()
    }
    #[must_use]
    pub fn tunnel_session(&self) -> &Arc<TunnelSession> {
        &self.session
    }
    /// Applies the host's recovered screen and releases live output. Call once.
    pub fn take_recovered_terminal(&self) -> Option<festerm_core::Terminal> {
        self.session.take_recovered_terminal()
    }
    /// Why the attachment ended, once it has (including `Displaced`). The
    /// session is never re-established automatically.
    #[must_use]
    pub fn end_error(&self) -> Option<TunnelError> {
        self.session.end_reason().and_then(TunnelError::from_end)
    }
    /// Detaches without ending the remote shell.
    pub fn detach(&self, timeout: Duration) -> Result<(), TunnelError> {
        self.session
            .shutdown(timeout)
            .map(|_| ())
            .map_err(|_| TunnelError::Timeout)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

pub struct TunnelClient {
    runtime: tokio::runtime::Runtime,
    store: Arc<Mutex<ProfileStore>>,
    transport: Arc<DynIdentity>,
    directory: Arc<dyn TunnelDirectory>,
    connector: Arc<dyn BridgeConnector>,
    client_id: String,
    refresh: tokio::sync::Mutex<()>,
}

impl TunnelClient {
    /// Production client against GitHub and the Dev Tunnels service.
    pub fn new(store: Arc<Mutex<ProfileStore>>) -> Result<Self, TunnelError> {
        let client_id = identity::registered_client_id().ok_or(TunnelError::NotConfigured)?;
        let transport: Arc<dyn IdentityTransport> = Arc::new(Https::new()?);
        Self::with_parts(
            store,
            transport,
            Arc::new(Microsoft),
            Arc::new(Microsoft),
            client_id.to_owned(),
        )
    }

    /// Injects the network seams (tests, previews).
    pub fn with_parts(
        store: Arc<Mutex<ProfileStore>>,
        transport: Arc<dyn IdentityTransport>,
        directory: Arc<dyn TunnelDirectory>,
        connector: Arc<dyn BridgeConnector>,
        client_id: String,
    ) -> Result<Self, TunnelError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("festerm-tunnels")
            .enable_all()
            .build()
            .map_err(|_| TunnelError::Network)?;
        Ok(Self {
            runtime,
            store,
            transport: Arc::new(DynIdentity(transport)),
            directory,
            connector,
            client_id,
            refresh: tokio::sync::Mutex::new(()),
        })
    }

    fn run<T>(
        &self,
        cancel: &CancelToken,
        limit: Duration,
        work: impl Future<Output = Result<T, TunnelError>>,
    ) -> Result<T, TunnelError> {
        self.runtime.block_on(async {
            tokio::select! {
                () = cancel.0.cancelled() => Err(TunnelError::Cancelled),
                result = tokio::time::timeout(limit, work) => result.unwrap_or(Err(TunnelError::Timeout)),
            }
        })
    }

    #[must_use]
    pub fn accounts(&self) -> Vec<AccountInfo> {
        lock(&self.store)
            .accounts()
            .iter()
            .map(|account| AccountInfo {
                id: account.id.clone(),
                login: account.login.clone(),
            })
            .collect()
    }

    /// Removes the account and its stored credential.
    pub fn sign_out(&self, account: &str) -> Result<(), TunnelError> {
        Ok(lock(&self.store).remove_account(account)?)
    }

    /// Starts the GitHub device flow and returns once the code is available.
    /// `expected_account` re-signs an existing account in (it must be the same
    /// GitHub user, bound by numeric id).
    pub fn begin_sign_in(
        &self,
        expected_account: Option<&str>,
        cancel: &CancelToken,
    ) -> Result<SignIn, TunnelError> {
        let expected = match expected_account {
            Some(id) => Some(
                lock(&self.store)
                    .account(id)
                    .map(|account| account.account_id)
                    .ok_or(TunnelError::SignInRequired)?,
            ),
            None => None,
        };
        let flow = cancel.0.child_token();
        let state = Arc::new(Mutex::new(SignInState::Waiting));
        let (early, ready) = std::sync::mpsc::channel::<Result<String, TunnelError>>();
        let (transport, store, client_id) = (
            self.transport.clone(),
            self.store.clone(),
            self.client_id.clone(),
        );
        let (task_state, task_cancel) = (state.clone(), flow.clone());
        self.runtime.spawn(async move {
            let announce = early.clone();
            let result = identity::sign_in(&*transport, &client_id, expected, &task_cancel, |p| {
                let _ = announce.send(Ok(p.user_code.clone()));
            })
            .await;
            let outcome = match result {
                Ok(identity) => persist(&store, &identity),
                Err(error) => Err(TunnelError::from(error)),
            };
            // Reaches the caller only when the flow failed before a code existed.
            if let Err(error) = &outcome {
                let _ = early.send(Err(error.clone()));
            }
            *lock(&task_state) = match outcome {
                Ok(account) => SignInState::Done(account),
                Err(error) => SignInState::Failed(error),
            };
        });
        match ready.recv_timeout(PROMPT_WAIT) {
            Ok(Ok(user_code)) => Ok(SignIn {
                prompt: SignInPrompt {
                    user_code,
                    verification_url: identity::VERIFICATION_URI.to_owned(),
                    expires_in: DEVICE_CODE_MAX_LIFETIME,
                },
                state,
                cancel: flow,
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                flow.cancel();
                Err(TunnelError::Timeout)
            }
        }
    }

    /// Confirms with GitHub that the stored credential still belongs to the
    /// bound account, refreshing it first when it expired.
    pub fn verify_account(
        &self,
        account: &str,
        cancel: &CancelToken,
    ) -> Result<AccountInfo, TunnelError> {
        self.run(cancel, NETWORK_LIMIT, async {
            let identity = self.identity(account, &cancel.0).await?;
            identity
                .confirm_account(&*self.transport, &cancel.0)
                .await?;
            Ok(AccountInfo {
                id: account.to_owned(),
                login: identity.account.login.clone(),
            })
        })
    }

    pub fn list_tunnels(
        &self,
        account: &str,
        cancel: &CancelToken,
    ) -> Result<Vec<TunnelInfo>, TunnelError> {
        self.run(cancel, NETWORK_LIMIT, async {
            let identity = self.identity(account, &cancel.0).await?;
            let tunnels = self.directory.list(&identity, &cancel.0).await?;
            Ok(tunnels
                .into_iter()
                .map(|t| TunnelInfo {
                    locator: t.locator,
                    name: t.name,
                    description: t.description,
                    active: t.active,
                })
                .collect())
        })
    }

    /// Resolves a tunnel, reads its bridge host identity and remote sessions.
    /// `trusted` is the host the profile approved, if any.
    pub fn describe_tunnel(
        &self,
        account: &str,
        tunnel: &str,
        trusted: Option<&TrustedTunnelHost>,
        cancel: &CancelToken,
    ) -> Result<TunnelDescription, TunnelError> {
        self.run(cancel, NETWORK_LIMIT, async {
            let identity = self.identity(account, &cancel.0).await?;
            let resolved = self.directory.resolve(&identity, tunnel, &cancel.0).await?;
            let description = self.describe(&resolved, &cancel.0).await?;
            let host = trusted_host(&resolved.binding);
            let fingerprint = fingerprint(&host)?;
            let trust = match trusted {
                None => HostTrust::New,
                Some(approved) if *approved == host => HostTrust::Trusted,
                Some(approved) => HostTrust::Changed {
                    approved_fingerprint: fingerprint_or_empty(approved),
                },
            };
            Ok(TunnelDescription {
                locator: format!("{}.{}", host.tunnel_id, host.cluster),
                host,
                fingerprint,
                trust,
                sessions: vec![session_info(&description)],
            })
        })
    }

    /// The host pinned by `profile`, for the UI to show or re-approve.
    pub fn trusted_host_for(&self, profile: &ProfileId) -> Option<TrustedTunnelHost> {
        match lock(&self.store).get(profile).map(|p| &p.kind) {
            Some(ProfileKind::DevTunnel(DevTunnelProfile {
                source: TunnelSource::Named { trusted_host, .. },
                ..
            })) => trusted_host.clone(),
            _ => None,
        }
    }

    /// Records the user's approval of `host` for a Named-source profile.
    pub fn approve_host(
        &self,
        profile: &ProfileId,
        host: TrustedTunnelHost,
    ) -> Result<(), TunnelError> {
        let mut store = lock(&self.store);
        let current = store.get(profile).cloned().ok_or_else(missing_profile)?;
        let ProfileKind::DevTunnel(mut dev) = current.kind else {
            return Err(missing_profile());
        };
        let TunnelSource::Named { trusted_host, .. } = &mut dev.source else {
            return Err(TunnelError::Profile(
                "imported destinations are pre-approved".into(),
            ));
        };
        *trusted_host = Some(host);
        Ok(store.update(profile, &current.name, ProfileKind::DevTunnel(dev))?)
    }

    /// The profile's remote session(s) and who holds them.
    pub fn list_sessions(
        &self,
        profile: &ProfileId,
        cancel: &CancelToken,
    ) -> Result<Vec<SessionInfo>, TunnelError> {
        self.run(cancel, NETWORK_LIMIT, async {
            let prepared = self.prepare(profile, &cancel.0).await?;
            Ok(vec![session_info(&prepared.description)])
        })
    }

    /// Attaches to the profile's session. Takeover follows the profile's
    /// `take_over_if_attached`; without it a held session fails with
    /// [`TunnelError::SessionInUse`].
    pub fn attach(
        &self,
        profile: &ProfileId,
        notifier: Arc<dyn SessionEventNotifier>,
        cancel: &CancelToken,
    ) -> Result<Attachment, TunnelError> {
        let attachment = self.run(cancel, ATTACH_LIMIT, async {
            let prepared = self.prepare(profile, &cancel.0).await?;
            let takeover = prepared.profile.take_over_if_attached;
            let held = prepared.description.attached_by;
            if held.is_some() && !takeover {
                return Err(TunnelError::SessionInUse { by: held });
            }
            let stream = self
                .connector
                .connect(&prepared.resolved)
                .await
                .map_err(|_| TunnelError::Network)?;
            let name = prepared.description.target.name.clone();
            let handshake =
                mobile::establish(stream, prepared.description.target, takeover, &cancel.0)
                    .await
                    .map_err(|error| match error {
                        mobile::AttachError::Refused => TunnelError::SessionInUse { by: held },
                        other => other.into(),
                    })?;
            let session = TunnelSession::spawn(self.runtime.handle(), handshake, notifier);
            Ok(Attachment {
                session: Arc::new(session),
                took_over: held.map(Some),
                session_name: name,
            })
        })?;
        let _ = lock(&self.store).mark_used(profile);
        Ok(attachment)
    }

    async fn describe(
        &self,
        resolved: &ResolvedTunnel,
        cancel: &CancellationToken,
    ) -> Result<mobile::Description, TunnelError> {
        let mut stream = tokio::select! {
            () = cancel.cancelled() => return Err(TunnelError::Cancelled),
            stream = self.connector.connect(resolved) => stream.map_err(|_| TunnelError::Network)?,
        };
        tokio::select! {
            () = cancel.cancelled() => Err(TunnelError::Cancelled),
            description = mobile::describe(&mut stream) => description.map_err(|error| {
                if error.kind() == io::ErrorKind::InvalidData { TunnelError::Protocol } else { TunnelError::Network }
            }),
        }
    }

    async fn prepare(
        &self,
        profile: &ProfileId,
        cancel: &CancellationToken,
    ) -> Result<Prepared, TunnelError> {
        let profile: Profile = lock(&self.store)
            .get(profile)
            .cloned()
            .ok_or_else(missing_profile)?;
        let ProfileKind::DevTunnel(dev) = profile.kind else {
            return Err(missing_profile());
        };
        let account = dev.account.clone().ok_or(TunnelError::SignInRequired)?;
        let (name, trusted) = match &dev.source {
            TunnelSource::Named {
                tunnel_name,
                trusted_host,
                ..
            } => (tunnel_name.clone(), trusted_host.clone()),
            TunnelSource::Imported { destination } => (
                format!("{}.{}", destination.tunnel_id, destination.cluster),
                destination
                    .endpoint
                    .host_public_keys
                    .first()
                    .map(|key| TrustedTunnelHost {
                        tunnel_id: destination.tunnel_id.clone(),
                        cluster: destination.cluster.clone(),
                        host_id: destination.endpoint.host_id.clone(),
                        public_key: key.clone(),
                        port: destination.port,
                    }),
            ),
        };
        let identity = self.identity(&account, cancel).await?;
        let resolved = self.directory.resolve(&identity, &name, cancel).await?;
        let host = trusted_host(&resolved.binding);
        match trusted {
            None => {
                return Err(TunnelError::HostNotTrusted {
                    fingerprint: fingerprint(&host)?,
                })
            }
            Some(approved) if approved != host => {
                return Err(TunnelError::HostChanged {
                    approved: fingerprint_or_empty(&approved),
                    presented: fingerprint(&host)?,
                })
            }
            Some(_) => {}
        }
        let description = self.describe(&resolved, cancel).await?;
        if dev
            .session
            .as_deref()
            .is_some_and(|wanted| wanted != description.target.name)
        {
            return Err(TunnelError::SessionNotFound);
        }
        Ok(Prepared {
            profile: dev,
            resolved,
            description,
        })
    }

    /// Loads the account's credential, refreshing and re-storing it when it has
    /// expired. Refresh is serialized so a consumed refresh token is never
    /// reused concurrently.
    async fn identity(
        &self,
        account: &str,
        cancel: &CancellationToken,
    ) -> Result<Identity, TunnelError> {
        let _guard = self.refresh.lock().await;
        let (record, secrets) = {
            let store = lock(&self.store);
            (
                store
                    .account(account)
                    .cloned()
                    .ok_or(TunnelError::SignInRequired)?,
                store.secrets().clone(),
            )
        };
        let reference = record.credential.to_reference();
        let stored = secrets
            .get(&reference)
            .map_err(|_| TunnelError::SecureStorage)?;
        let identity = stored
            .with_bytes(|bytes| Identity::decode_private(bytes, record.account_id, &self.client_id))
            .map_err(|_| TunnelError::SignInRequired)?;
        if !identity.needs_sign_in() {
            return Ok(identity);
        }
        if !identity.can_refresh() {
            return Err(TunnelError::SignInRequired);
        }
        let refreshed = identity
            .refresh(&*self.transport, &self.client_id, cancel)
            .await
            .map_err(|error| match TunnelError::from(error) {
                TunnelError::Denied | TunnelError::Expired => TunnelError::SignInRequired,
                other => other,
            })?;
        let encoded = refreshed.encode_private()?;
        secrets
            .update(&reference, &SecretBytes::copy_from_slice(&encoded))
            .map_err(|_| TunnelError::SecureStorage)?;
        Ok(refreshed)
    }
}

struct Prepared {
    profile: DevTunnelProfile,
    resolved: ResolvedTunnel,
    description: mobile::Description,
}

fn missing_profile() -> TunnelError {
    TunnelError::Profile("not a Dev Tunnel profile".into())
}

fn trusted_host(binding: &HostBinding) -> TrustedTunnelHost {
    TrustedTunnelHost {
        tunnel_id: binding.tunnel_id.clone(),
        cluster: binding.cluster.clone(),
        host_id: binding.host_id.clone(),
        public_key: binding.public_key.clone(),
        port: binding.port,
    }
}

fn fingerprint(host: &TrustedTunnelHost) -> Result<String, TunnelError> {
    relay::host_fingerprint(&host.public_key).map_err(|_| TunnelError::Protocol)
}

fn fingerprint_or_empty(host: &TrustedTunnelHost) -> String {
    fingerprint(host).unwrap_or_default()
}

fn session_info(description: &mobile::Description) -> SessionInfo {
    SessionInfo {
        name: description.target.name.clone(),
        state: match description.attached_by {
            None => SessionState::Unattached,
            by => SessionState::Attached { by },
        },
        takeover_allowed: description.takeover_allowed,
    }
}

fn persist(store: &Mutex<ProfileStore>, identity: &Identity) -> Result<AccountInfo, TunnelError> {
    let encoded = identity.encode_private()?;
    let mut store = lock(store);
    let secret = store.store_secret(&SecretBytes::copy_from_slice(&encoded))?;
    let id = store.upsert_account(
        AccountProvider::Github,
        identity.account.id,
        &identity.account.login,
        secret,
    )?;
    Ok(AccountInfo {
        id,
        login: identity.account.login.clone(),
    })
}
