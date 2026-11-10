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
            Self::Challenge { challenge } => challenge.validate(),
            Self::Proof { proof } => proof.validate(),
            Self::Accepted {} | Self::Denied {} => Ok(()),
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
    loop {
        if cancel.is_cancelled() {
            return Err(Box::new((state, Error::Cancelled)));
        }
        match source.current_policy() {
            Ok(current) => return Ok(work(state, current.verifier())),
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
                self.authenticate(stream, permit.clone(), deadline, shutdown.clone()),
            ) =>
                result.map_err(|_| Error::Deadline)??,
        };
        let (mut stream, owner, guard) = authenticated.into_parts();
        // Role is selected only on this proven encrypted channel. A single
        // byte avoids a peer-controlled pre-dispatch queue/allocation.
        let role = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Err(Error::Cancelled),
            result = tokio::time::timeout(AUTH_DEADLINE, stream.read_u8()) =>
                result.map_err(|_| Error::Deadline)??,
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
