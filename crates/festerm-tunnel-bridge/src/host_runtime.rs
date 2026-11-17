//! Explicit composition on actual inner SSH channels. This does not install,
//! provision or enable a host, and never accepts a raw TCP/duplex transport.
#[cfg(feature = "relay")]
use crate::{
    host_control::{self, native::NativeControl, PolicyGuard, ProtectedPolicySource},
    host_terminal::{native::NativeTerminal, OutputScheduler},
    owner_auth::{ExplicitHeadlessFileStore, OwnerAuthenticatedStream},
    relay::{HostConnection, HostStream},
};
use crate::{
    owner_auth::{
        ChannelOwnerChallenge, ChannelOwnerProof, ClientOwnerKey, OwnerBinding, OwnerKeyError,
    },
    relay::ClientConnection,
};
#[cfg(feature = "relay")]
use festerm_host_protocol::{Budgets, MAX_CLIENTS};
use festerm_host_protocol::{ContractError, Validate, AUTH_DEADLINE_SECONDS};
#[cfg(feature = "relay")]
use festerm_sessiond::host_control::SessionControl;
use festerm_tunnel_client::host_control::{read_frame, write_frame, ConnectionPermit, Error};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use tokio::io::AsyncWriteExt;
#[cfg(feature = "relay")]
use tokio::{io::AsyncReadExt, task::JoinSet};
use tokio_util::sync::CancellationToken;

const AUTH_DEADLINE: Duration = Duration::from_secs(AUTH_DEADLINE_SECONDS as u64);
#[cfg(feature = "relay")]
const AUTH_REFUSAL_RESERVE: Duration = Duration::from_millis(100);

/// Each role proves its own actual signed original SSH session/channel.
/// A control proof never authorizes a sibling terminal channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamRole {
    Control,
    Terminal,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case", deny_unknown_fields)]
enum Authentication {
    OrdinaryChallengeV1 {
        challenge: crate::ordinary_auth::OrdinaryChallenge,
    },
    OrdinaryProofV1 {
        proof: crate::ordinary_auth::OrdinaryProof,
    },
    OrdinaryRefreshRequiredV1 {},
    Challenge {
        challenge: ChannelOwnerChallenge,
    },
    Proof {
        proof: ChannelOwnerProof,
    },
    Accepted {},
    Denied {},
    Refused {
        reason: festerm_host_protocol::Refusal,
    },
}

/// Native client composition loads its key before connecting. This wire helper
/// never opens a vault and never falls back to enrolled-owner authentication.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn authenticate_ordinary_client_using_guard(
    mut stream: ClientConnection,
    permit: ConnectionPermit,
    expected: crate::ordinary_auth::OrdinaryRoute,
    role: StreamRole,
    cancel: CancellationToken,
    prove: impl FnOnce(
            &ClientConnection,
            &crate::ordinary_auth::OrdinaryChallenge,
        ) -> Result<crate::ordinary_auth::OrdinaryProof, ContractError>
        + Send
        + 'static,
    may_publish: impl Fn() -> Result<(), ContractError> + Send + 'static,
) -> Result<
    (
        ClientConnection,
        ConnectionPermit,
        crate::ordinary_auth::OrdinaryHostInfo,
    ),
    Error,
> {
    let permit = Arc::new(permit);
    let work = async {
        let frame = read_frame(&mut stream, permit.budgets()).await?;
        let challenge = match frame.decode()? {
            Authentication::OrdinaryChallengeV1 { challenge } => challenge,
            Authentication::Challenge { .. } => return Err(Error::HostUpdateRequired),
            Authentication::OrdinaryRefreshRequiredV1 {} => {
                return Err(Error::HostPolicyRefreshRequired)
            }
            Authentication::Refused { reason } => return Err(Error::Refused(reason)),
            _ => return Err(ContractError::Denied.into()),
        };
        drop(frame);
        expected.validate()?;
        if challenge.host_info.route != expected
            || stream
                .owner_pinned_fingerprint()
                .map_err(|_| ContractError::Denied)?
                != expected.host.host_key_fingerprint
            || *stream
                .owner_channel_binding()
                .map_err(|_| ContractError::Denied)?
                .as_bytes()
                != crate::owner_auth::unhex::<32>(&challenge.channel_binding)?
        {
            return Err(ContractError::Denied.into());
        }
        may_publish()?;
        let info = challenge.host_info.clone();
        let charge = permit.clone();
        let (mut stream, proof) = tokio::task::spawn_blocking(move || {
            let _charge = charge;
            let proof = prove(&stream, &challenge)?;
            Ok::<_, ContractError>((stream, proof))
        })
        .await
        .map_err(|_| Error::Closed)??;
        if proof.role != role {
            return Err(ContractError::Denied.into());
        }
        may_publish()?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        write_frame(
            &mut stream,
            permit.budgets(),
            &Authentication::OrdinaryProofV1 { proof },
        )
        .await?;
        let frame = read_frame(&mut stream, permit.budgets()).await?;
        match frame.decode()? {
            Authentication::Accepted {} => {}
            Authentication::OrdinaryRefreshRequiredV1 {} => {
                return Err(Error::HostPolicyRefreshRequired)
            }
            Authentication::Refused { reason } => return Err(Error::Refused(reason)),
            _ => return Err(ContractError::Denied.into()),
        }
        drop(frame);
        may_publish()?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok::<_, Error>((stream, info))
    };
    let (stream, info) = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(Error::Cancelled),
        result = tokio::time::timeout(AUTH_DEADLINE, work) => result.map_err(|_| Error::Deadline)??,
    };
    Ok((
        stream,
        Arc::try_unwrap(permit).map_err(|_| Error::Closed)?,
        info,
    ))
}

/// Native callers must supply already foreground-approved owner metadata and
/// an existing key. This never enrolls, accepts a changed pin or creates a key.
pub async fn authenticate_client(
    stream: ClientConnection,
    permit: ConnectionPermit,
    key: ClientOwnerKey,
    approved: OwnerBinding,
    role: StreamRole,
    cancel: CancellationToken,
) -> Result<(ClientConnection, ConnectionPermit), Error> {
    authenticate_client_using(stream, permit, role, cancel, move |stream, challenge| {
        stream.prove_owner(&key, &approved, challenge)
    })
    .await
}

/// The selected-host client loaded this single-use native snapshot before
/// opening the relay. No native vault access occurs inside the wire deadline.
#[cfg(feature = "host-routing")]
pub(crate) async fn authenticate_native_client(
    stream: ClientConnection,
    permit: ConnectionPermit,
    attempt: crate::owner_auth::NativeOwnerAttempt,
    role: StreamRole,
    cancel: CancellationToken,
) -> Result<(ClientConnection, ConnectionPermit), Error> {
    let guard = attempt.publication_guard();
    authenticate_client_using_guard(
        stream,
        permit,
        role,
        cancel,
        move |stream, challenge| stream.prove_native_attempt(attempt, challenge),
        move || guard.check(),
    )
    .await
}

/// Only an explicitly selected headless credential store; never a vault-error
/// fallback. The host setup composition must also verify headless origin.
#[cfg(feature = "relay")]
#[allow(clippy::too_many_arguments)]
pub async fn authenticate_headless_client(
    stream: ClientConnection,
    permit: ConnectionPermit,
    selected: ExplicitHeadlessFileStore,
    key: ClientOwnerKey,
    approved: OwnerBinding,
    role: StreamRole,
    cancel: CancellationToken,
) -> Result<(ClientConnection, ConnectionPermit), Error> {
    authenticate_client_using(stream, permit, role, cancel, move |stream, challenge| {
        stream.prove_owner_headless(&selected, &key, &approved, challenge)
    })
    .await
}

async fn authenticate_client_using(
    stream: ClientConnection,
    permit: ConnectionPermit,
    role: StreamRole,
    cancel: CancellationToken,
    prove: impl FnOnce(
            &ClientConnection,
            &ChannelOwnerChallenge,
        ) -> Result<ChannelOwnerProof, OwnerKeyError>
        + Send
        + 'static,
) -> Result<(ClientConnection, ConnectionPermit), Error> {
    authenticate_client_using_guard(stream, permit, role, cancel, prove, || Ok(())).await
}

async fn authenticate_client_using_guard(
    mut stream: ClientConnection,
    permit: ConnectionPermit,
    role: StreamRole,
    cancel: CancellationToken,
    prove: impl FnOnce(
            &ClientConnection,
            &ChannelOwnerChallenge,
        ) -> Result<ChannelOwnerProof, OwnerKeyError>
        + Send
        + 'static,
    may_publish: impl Fn() -> Result<(), OwnerKeyError>,
) -> Result<(ClientConnection, ConnectionPermit), Error> {
    let permit = Arc::new(permit);
    let work = async {
        let frame = read_frame(&mut stream, permit.budgets()).await?;
        let challenge = match frame.decode()? {
            Authentication::Challenge { challenge } => challenge,
            Authentication::Refused { reason } => return Err(Error::Refused(reason)),
            _ => return Err(ContractError::Denied.into()),
        };
        drop(frame);
        let charge = permit.clone();
        let (mut stream, proof) = tokio::task::spawn_blocking(move || {
            let _charge = charge;
            let proof = prove(&stream, &challenge).map_err(|_| ContractError::Denied)?;
            Ok::<_, ContractError>((stream, proof))
        })
        .await
        .map_err(|_| Error::Closed)??;
        // A completed blocking proof may have waited in the executor queue.
        // Check its original authority immediately before proof publication.
        may_publish().map_err(|_| ContractError::Denied)?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        write_frame(
            &mut stream,
            permit.budgets(),
            &Authentication::Proof { proof },
        )
        .await?;
        let frame = read_frame(&mut stream, permit.budgets()).await?;
        match frame.decode()? {
            Authentication::Accepted {} => {}
            Authentication::Refused { reason } => return Err(Error::Refused(reason)),
            _ => return Err(ContractError::Denied.into()),
        }
        drop(frame);
        stream
            .write_u8(match role {
                StreamRole::Control => 0,
                StreamRole::Terminal => 1,
            })
            .await?;
        stream.flush().await?;
        Ok::<_, Error>(stream)
    };
    let stream = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(Error::Cancelled),
        result = tokio::time::timeout(AUTH_DEADLINE, work) =>
            result.map_err(|_| Error::Deadline)??,
    };
    Ok((stream, Arc::try_unwrap(permit).map_err(|_| Error::Closed)?))
}

impl Validate for Authentication {
    fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::OrdinaryChallengeV1 { challenge } => challenge.validate(),
            Self::OrdinaryProofV1 { proof } => proof.validate(),
            Self::Challenge { challenge } => challenge.validate(),
            Self::Proof { proof } => proof.validate(),
            Self::Accepted {} | Self::Denied {} | Self::OrdinaryRefreshRequiredV1 {} => Ok(()),
            Self::Refused {
                reason: festerm_host_protocol::Refusal::Busy,
            } => Ok(()),
            Self::Refused { .. } => Err(ContractError::Malformed),
        }
    }
}

#[cfg(feature = "relay")]
fn with_authentication_policy<T, R>(
    source: &impl ProtectedPolicySource,
    state: T,
    deadline: tokio::time::Instant,
    cancel: &CancellationToken,
    work: impl FnOnce(T, &crate::owner_auth::OwnerVerifier) -> R,
) -> Result<R, Box<(T, Error)>> {
    with_policy_view(
        source,
        state,
        host_control::AuthorityKind::EnrolledOwner,
        deadline,
        cancel,
        |state, view| match view.owner() {
            Ok(verifier) => Ok(work(state, verifier)),
            Err(error) => Err(Box::new((state, error.into()))),
        },
    )
}

#[cfg(feature = "relay")]
fn with_policy_view<T, R>(
    source: &impl ProtectedPolicySource,
    state: T,
    kind: host_control::AuthorityKind,
    deadline: tokio::time::Instant,
    cancel: &CancellationToken,
    work: impl FnOnce(T, host_control::PolicyView<'_>) -> Result<R, Box<(T, Error)>>,
) -> Result<R, Box<(T, Error)>> {
    loop {
        if cancel.is_cancelled() {
            return Err(Box::new((state, Error::Cancelled)));
        }
        match source.current_policy_for(kind) {
            Ok(current) => return work(state, current.view()),
            Err(ContractError::Busy) => {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining <= AUTH_REFUSAL_RESERVE {
                    return Err(Box::new((
                        state,
                        Error::Refused(festerm_host_protocol::Refusal::Busy),
                    )));
                }
                // Leave wire-publication time inside the original authentication
                // deadline. No policy lease or verifier survives a Busy wait.
                std::thread::sleep(Duration::from_millis(10).min(remaining - AUTH_REFUSAL_RESERVE));
            }
            Err(error) => return Err(Box::new((state, error.into()))),
        }
    }
}

#[cfg(feature = "relay")]
async fn ordinary_policy_phase<A: ProtectedPolicySource, R: Send + 'static>(
    source: Arc<A>,
    permit: Arc<ConnectionPermit>,
    stream: HostStream,
    deadline: tokio::time::Instant,
    cancel: CancellationToken,
    work: impl FnOnce(&HostStream, &crate::ordinary_auth::OrdinaryPolicy) -> Result<R, Error>
        + Send
        + 'static,
) -> Result<(HostStream, R), Error> {
    let charge = permit.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _charge = charge;
        with_policy_view(
            source.as_ref(),
            stream,
            host_control::AuthorityKind::Ordinary,
            deadline,
            &cancel,
            |stream, view| {
                let host_control::PolicyView::Ordinary(policy) = view else {
                    return Err(Box::new((stream, ContractError::Denied.into())));
                };
                match work(&stream, policy) {
                    Ok(result) => Ok((stream, result)),
                    Err(error) => Err(Box::new((stream, error))),
                }
            },
        )
    })
    .await
    .map_err(|_| Error::Closed)?;
    match result {
        Ok(value) => Ok(value),
        Err(failure) => {
            let (mut stream, error) = *failure;
            let response = if matches!(error, Error::Refused(festerm_host_protocol::Refusal::Busy))
            {
                Authentication::Refused {
                    reason: festerm_host_protocol::Refusal::Busy,
                }
            } else if matches!(error, Error::Contract(ContractError::Stale)) {
                Authentication::OrdinaryRefreshRequiredV1 {}
            } else {
                Authentication::Denied {}
            };
            let _ = write_frame(&mut stream, permit.budgets(), &response).await;
            Err(error)
        }
    }
}

#[cfg(all(test, feature = "relay"))]
mod tests;

/// One shared aggregate budget and FIFO output scheduler per host runtime.
/// Production policy sources must reload protected enrollment under its owned
/// lease; no cached verifier or client metadata is current authority.
#[cfg(feature = "relay")]
pub struct HostRuntime<A: ProtectedPolicySource> {
    policy: Arc<A>,
    control: Arc<NativeControl<A>>,
    terminal: Arc<NativeTerminal<A>>,
    budgets: Budgets,
    output: OutputScheduler,
}

#[cfg(feature = "relay")]
impl<A: ProtectedPolicySource> HostRuntime<A> {
    pub fn new(sessions: SessionControl, policy: Arc<A>) -> Self {
        let sessions = Arc::new(sessions);
        Self {
            control: Arc::new(NativeControl::with_terminal_routes(
                sessions.clone(),
                policy.clone(),
            )),
            terminal: Arc::new(NativeTerminal::new(sessions, policy.clone())),
            policy,
            budgets: Budgets::default(),
            output: OutputScheduler::default(),
        }
    }

    /// Consume an existing authenticated relay listener. No fallback to legacy
    /// or unauthenticated localhost is attempted. Legacy callers are unchanged.
    pub async fn serve(
        self: Arc<Self>,
        listener: &mut HostConnection,
        shutdown: CancellationToken,
    ) -> Result<(), Error> {
        let mut workers = JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    workers.abort_all();
                    while workers.join_next().await.is_some() {}
                    return Err(Error::Cancelled);
                }
                _ = workers.join_next(), if !workers.is_empty() => {}
                incoming = listener.connections.recv(), if workers.len() < MAX_CLIENTS => {
                    let Some(stream) = incoming else {
                        workers.abort_all();
                        while workers.join_next().await.is_some() {}
                        return Err(Error::Closed);
                    };
                    let permit = match ConnectionPermit::reserve(&self.budgets) {
                        Ok(permit) => permit,
                        Err(_) => { drop(stream); continue; }
                    };
                    let runtime = self.clone();
                    let cancel = shutdown.clone();
                    workers.spawn(async move {
                        runtime.serve_reserved(stream, permit, cancel).await
                    });
                }
            }
        }
    }

    /// Reserve before any proof, policy I/O, frame allocation or worker. Only
    /// HostStream can enter; authorities and streams cannot be supplied apart.
    pub async fn serve_stream(
        &self,
        stream: HostStream,
        shutdown: CancellationToken,
    ) -> Result<(), Error> {
        let permit = ConnectionPermit::reserve(&self.budgets)?;
        self.serve_reserved(stream, permit, shutdown).await
    }

    async fn serve_reserved(
        &self,
        stream: HostStream,
        permit: ConnectionPermit,
        shutdown: CancellationToken,
    ) -> Result<(), Error> {
        // Blocking protected-policy work owns a permit even if its async waiter
        // times out. A cancelled client cannot accumulate uncharged vault work.
        let permit = Arc::new(permit);
        let deadline = tokio::time::Instant::now() + AUTH_DEADLINE;
        let authenticated = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Err(Error::Cancelled),
            result = tokio::time::timeout_at(
                deadline,
                self.authenticate_any(stream, permit.clone(), deadline, shutdown.clone()),
            ) =>
                result.map_err(|_| Error::Deadline)??,
        };
        let (mut stream, owner, guard, ordinary_role) = authenticated;
        // Role is selected only on this proven encrypted channel. A single
        // byte avoids a peer-controlled pre-dispatch queue/allocation.
        let role = if let Some(role) = ordinary_role {
            match role {
                StreamRole::Control => 0,
                StreamRole::Terminal => 1,
            }
        } else {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => return Err(Error::Cancelled),
                result = tokio::time::timeout(AUTH_DEADLINE, stream.read_u8()) => result.map_err(|_|Error::Deadline)??,
            }
        };
        let permit = Arc::try_unwrap(permit).map_err(|_| Error::Closed)?;
        match role {
            0 => {
                host_control::serve_control(
                    stream,
                    permit,
                    owner,
                    guard,
                    self.policy.clone(),
                    self.control.clone(),
                    shutdown,
                )
                .await
            }
            1 => {
                crate::host_terminal::native::serve_terminal(
                    stream,
                    permit,
                    owner,
                    guard,
                    self.terminal.clone(),
                    self.output.clone(),
                    shutdown,
                )
                .await
            }
            _ => Err(ContractError::Malformed.into()),
        }
    }

    async fn authenticate_any(
        &self,
        stream: HostStream,
        permit: Arc<ConnectionPermit>,
        deadline: tokio::time::Instant,
        cancel: CancellationToken,
    ) -> Result<
        (
            HostStream,
            host_control::ConnectionAuthority,
            crate::owner_auth::ConnectionGuard,
            Option<StreamRole>,
        ),
        Error,
    > {
        if stream.requested_authorization() != crate::relay_host::RequestedAuthorization::OrdinaryV1
        {
            let (stream, owner, guard) = self
                .authenticate(stream, permit, deadline, cancel)
                .await?
                .into_parts();
            return Ok((stream, owner.into(), guard, None));
        }
        self.authenticate_ordinary(stream, permit, deadline, cancel)
            .await
    }

    async fn authenticate_ordinary(
        &self,
        stream: HostStream,
        permit: Arc<ConnectionPermit>,
        deadline: tokio::time::Instant,
        cancel: CancellationToken,
    ) -> Result<
        (
            HostStream,
            host_control::ConnectionAuthority,
            crate::owner_auth::ConnectionGuard,
            Option<StreamRole>,
        ),
        Error,
    > {
        use crate::ordinary_auth::{OrdinaryCapability, PendingOrdinaryChallenge};
        // A route is supplied only by the production provider relay. Encryption
        // without that admission is insufficient, including local test streams.
        let route = stream.ordinary_route().cloned();
        let (mut stream, route) = ordinary_policy_phase(
            self.policy.clone(),
            permit.clone(),
            stream,
            deadline,
            cancel.clone(),
            move |stream, policy| {
                stream
                    .require_owner_channel_binding()
                    .map_err(|_| ContractError::Denied)?;
                let route = route.ok_or(ContractError::Denied)?;
                policy.validate()?;
                if policy.host_info.route != route {
                    return Err(ContractError::Denied.into());
                }
                Ok(route)
            },
        )
        .await?;
        let observed = tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + Duration::from_secs(10)),
            self.policy.observe_ordinary(&route, &cancel),
        )
        .await;
        if !matches!(observed, Ok(Ok(()))) {
            let response = if matches!(observed, Ok(Err(ContractError::Stale))) {
                Authentication::OrdinaryRefreshRequiredV1 {}
            } else {
                Authentication::Denied {}
            };
            let _ = write_frame(&mut stream, permit.budgets(), &response).await;
            return Err(match observed {
                Err(_) => Error::Deadline,
                Ok(Err(error)) => error.into(),
                _ => unreachable!(),
            });
        }
        let (mut stream, pending) = ordinary_policy_phase(
            self.policy.clone(),
            permit.clone(),
            stream,
            deadline,
            cancel.clone(),
            move |stream, policy| {
                policy.validate()?;
                if policy.host_info.route != route {
                    return Err(ContractError::Denied.into());
                }
                Ok(PendingOrdinaryChallenge::new(
                    policy.host_info.clone(),
                    stream
                        .owner_channel_binding()
                        .map_err(|_| ContractError::Denied)?,
                    deadline.into_std(),
                )?)
            },
        )
        .await?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Deadline);
        }
        write_frame(
            &mut stream,
            permit.budgets(),
            &Authentication::OrdinaryChallengeV1 {
                challenge: pending.challenge().clone(),
            },
        )
        .await?;
        let frame = read_frame(&mut stream, permit.budgets()).await?;
        let Authentication::OrdinaryProofV1 { proof } = frame.decode()? else {
            return Err(ContractError::Denied.into());
        };
        drop(frame);
        let (mut stream, (capability, guard, role)) = ordinary_policy_phase(
            self.policy.clone(),
            permit.clone(),
            stream,
            deadline,
            cancel.clone(),
            move |stream, policy| {
                policy.validate()?;
                let role = proof.role;
                let verified = pending.verify(
                    &proof,
                    &policy.host_info,
                    stream
                        .owner_channel_binding()
                        .map_err(|_| ContractError::Denied)?,
                    &policy.denied_keys,
                )?;
                let (capability, guard) = OrdinaryCapability::mint(verified, policy)?;
                Ok((capability, guard, role))
            },
        )
        .await?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Deadline);
        }
        write_frame(&mut stream, permit.budgets(), &Authentication::Accepted {}).await?;
        Ok((
            stream,
            host_control::ConnectionAuthority::Ordinary(capability),
            guard,
            Some(role),
        ))
    }

    async fn authenticate(
        &self,
        stream: HostStream,
        permit: Arc<ConnectionPermit>,
        deadline: tokio::time::Instant,
        cancel: CancellationToken,
    ) -> Result<OwnerAuthenticatedStream, Error> {
        let source = self.policy.clone();
        let charge = permit.clone();
        let policy_cancel = cancel.clone();
        let challenge = tokio::task::spawn_blocking(move || {
            let _charge = charge;
            with_authentication_policy(
                source.as_ref(),
                stream,
                deadline,
                &policy_cancel,
                |stream, verifier| {
                    stream
                        .challenge_owner(verifier)
                        .map(|pending| (stream, pending))
                },
            )
        })
        .await
        .map_err(|_| Error::Closed)?;
        let (mut stream, pending) = match challenge {
            Ok(result) => result?,
            Err(failure) => {
                let (stream, error) = *failure;
                return Self::refuse_authentication(stream, &permit, deadline, error).await;
            }
        };
        write_frame(
            &mut stream,
            permit.budgets(),
            &Authentication::Challenge {
                challenge: pending.challenge().clone(),
            },
        )
        .await?;
        let frame = read_frame(&mut stream, permit.budgets()).await?;
        let Authentication::Proof { proof } = frame.decode()? else {
            return Err(ContractError::Malformed.into());
        };
        drop(frame);
        let source = self.policy.clone();
        let charge = permit.clone();
        let verified = tokio::task::spawn_blocking(move || {
            let _charge = charge;
            with_authentication_policy(
                source.as_ref(),
                (stream, pending, proof),
                deadline,
                &cancel,
                |(stream, pending, proof), verifier| {
                    stream.authenticate_owner(pending, &proof, verifier)
                },
            )
        })
        .await
        .map_err(|_| Error::Closed)?;
        let verified = match verified {
            Ok(result) => result,
            Err(failure) => {
                let ((stream, _, _), error) = *failure;
                return Self::refuse_authentication(stream, &permit, deadline, error).await;
            }
        };
        match verified {
            Ok(mut authenticated) => {
                // Success is sent before either dispatcher reads its first frame.
                write_frame(
                    authenticated.stream_mut(),
                    permit.budgets(),
                    &Authentication::Accepted {},
                )
                .await?;
                Ok(authenticated)
            }
            Err(failure) => {
                let (mut stream, _) = failure.into_parts();
                let _ =
                    write_frame(&mut stream, permit.budgets(), &Authentication::Denied {}).await;
                Err(ContractError::Denied.into())
            }
        }
    }

    async fn refuse_authentication(
        mut stream: HostStream,
        permit: &ConnectionPermit,
        deadline: tokio::time::Instant,
        error: Error,
    ) -> Result<OwnerAuthenticatedStream, Error> {
        if !matches!(error, Error::Cancelled) {
            let refusal = if matches!(error, Error::Refused(festerm_host_protocol::Refusal::Busy)) {
                Authentication::Refused {
                    reason: festerm_host_protocol::Refusal::Busy,
                }
            } else if matches!(error, Error::Contract(ContractError::Stale)) {
                Authentication::OrdinaryRefreshRequiredV1 {}
            } else {
                Authentication::Denied {}
            };
            let _ = tokio::time::timeout_at(
                deadline,
                write_frame(&mut stream, permit.budgets(), &refusal),
            )
            .await;
        }
        Err(error)
    }
}
