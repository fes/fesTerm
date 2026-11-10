//! Bounded HOST RPC on an already authenticated, host-key-pinned channel.
//!
//! These APIs provide framing/correlation, not authentication. The composition
//! boundary must supply the *same* channel used for enrolled owner proof.
//! Never retry transport failures automatically or reinterpret them as absence.

use festerm_host_protocol::{
    decode, encode, BudgetPermit, Budgets, Capabilities, ClientBudget, ContractError, Envelope,
    HostId, OperationId, Request, Resource, Response, Validate, CONTROL_DEADLINE_SECONDS,
    CONTROL_FRAME_BYTES,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{fmt, io, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Contract(ContractError),
    Closed,
    Deadline,
    Cancelled,
    Offline,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Contract(_) => "invalid or refused host control message",
            Self::Closed => "host control connection closed",
            Self::Deadline => "host control deadline exceeded",
            Self::Cancelled => "host control cancelled",
            Self::Offline => "host control transport unavailable",
        })
    }
}

impl std::error::Error for Error {}

impl From<ContractError> for Error {
    fn from(value: ContractError) -> Self {
        Self::Contract(value)
    }
}

impl From<io::Error> for Error {
    fn from(_: io::Error) -> Self {
        Self::Offline
    }
}

/// A control allocation retains its aggregate byte charge until decoding and
/// use finish. Socket admission must separately precede worker allocation.
pub struct Frame {
    bytes: Vec<u8>,
    _permit: BudgetPermit,
}

impl Frame {
    pub fn decode<T: DeserializeOwned + Validate>(&self) -> Result<T, Error> {
        Ok(decode(&self.bytes)?)
    }
}

/// Reject the length before allocating or reading a peer-controlled body.
pub async fn read_frame(
    stream: &mut (impl AsyncRead + Unpin),
    budgets: &Budgets,
) -> Result<Frame, Error> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > CONTROL_FRAME_BYTES {
        return Err(ContractError::Limit.into());
    }
    let permit = budgets.reserve(Resource::ControlBytes, length)?;
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    Ok(Frame {
        bytes,
        _permit: permit,
    })
}

pub async fn write_frame<T: Serialize + Validate>(
    stream: &mut (impl AsyncWrite + Unpin),
    budgets: &Budgets,
    value: &T,
) -> Result<(), Error> {
    // Reserve the worst-case allocation before bounded serialization.
    let _permit = budgets.reserve(Resource::ControlBytes, CONTROL_FRAME_BYTES)?;
    let bytes = encode(value)?;
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

/// Immutable account/endpoint identity. A friendly name is never a binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Binding {
    host: HostId,
    account: String,
    endpoint: String,
}

impl Binding {
    pub fn new(host: HostId, account: String, endpoint: String) -> Result<Self, Error> {
        if [&account, &endpoint].iter().any(|value| {
            value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control)
        }) {
            return Err(ContractError::Malformed.into());
        }
        Ok(Self {
            host,
            account,
            endpoint,
        })
    }

    pub fn host(&self) -> &HostId {
        &self.host
    }
    pub fn account(&self) -> &str {
        &self.account
    }
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

/// Sequential control connection: one outstanding operation, no background
/// queue, no global token, no terminal input, and no weaker-protocol fallback.
pub struct ControlClient<S> {
    stream: S,
    binding: Binding,
    permit: ConnectionPermit,
    capabilities: Option<Capabilities>,
    usable: bool,
}

/// Reserve before opening an endpoint/socket or allocating its worker.
/// A separate terminal connection consumes a separate aggregate client slot.
pub struct ConnectionPermit {
    client: ClientBudget,
    budgets: Budgets,
}

impl ConnectionPermit {
    pub fn reserve(budgets: &Budgets) -> Result<Self, ContractError> {
        Ok(Self {
            client: budgets.admit_client()?,
            budgets: budgets.clone(),
        })
    }

    pub fn budgets(&self) -> &Budgets {
        &self.budgets
    }

    pub fn reserve_terminal_bytes(
        &self,
        bytes: usize,
    ) -> Result<festerm_host_protocol::ClientBytesPermit, ContractError> {
        self.client.reserve_terminal_bytes(bytes)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> ControlClient<S> {
    /// Low-level composition API, NOT host trust or owner authentication.
    /// Only call after enrolled proof on this exact pinned stream.
    pub fn on_authenticated_stream(stream: S, binding: Binding, permit: ConnectionPermit) -> Self {
        Self {
            stream,
            binding,
            permit,
            capabilities: None,
            usable: true,
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> ControlClient<S> {
    pub fn binding(&self) -> &Binding {
        &self.binding
    }

    pub async fn handshake(&mut self, cancel: &CancellationToken) -> Result<Capabilities, Error> {
        let response = self
            .request(OperationId::new(), Request::Capabilities {}, cancel)
            .await?;
        match response.body {
            Response::Capabilities { capabilities } => {
                self.capabilities = Some(capabilities);
                Ok(capabilities)
            }
            _ => {
                self.usable = false;
                Err(ContractError::Denied.into())
            }
        }
    }

    pub async fn request(
        &mut self,
        operation: OperationId,
        body: Request,
        cancel: &CancellationToken,
    ) -> Result<Envelope<Response>, Error> {
        if !self.usable {
            return Err(Error::Closed);
        }
        if self.capabilities.is_none() && !matches!(body, Request::Capabilities {}) {
            return Err(ContractError::Denied.into());
        }
        let request = Envelope::new(self.binding.host.clone(), operation, body);
        request.validate()?;
        let _operation = self.permit.budgets.reserve(Resource::Control, 1)?;
        // Poison before the first await: dropping/cancelling this future can
        // leave a partial frame or a late reply on the stream.
        self.usable = false;
        let exchange = async {
            write_frame(&mut self.stream, &self.permit.budgets, &request).await?;
            let reply: Envelope<Response> = read_frame(&mut self.stream, &self.permit.budgets)
                .await?
                .decode()?;
            reply.validate_for(&request)?;
            Ok(reply)
        };
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(Duration::from_secs(u64::from(CONTROL_DEADLINE_SECONDS)), exchange) =>
                result.unwrap_or(Err(Error::Deadline)),
        };
        if result.is_ok() {
            self.usable = true;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_host_protocol::{LogicalSessionId, LookupResult, Refusal};
    use tokio::io::duplex;

    fn permit() -> ConnectionPermit {
        ConnectionPermit::reserve(&Budgets::default()).unwrap()
    }

    fn binding() -> Binding {
        Binding::new(
            HostId::new(),
            "owned-account".into(),
            "owned-endpoint".into(),
        )
        .unwrap()
    }

    async fn reply(stream: &mut (impl AsyncRead + AsyncWrite + Unpin), body: Response) {
        let budgets = Budgets::default();
        let request: Envelope<Request> = read_frame(stream, &budgets)
            .await
            .unwrap()
            .decode()
            .unwrap();
        write_frame(
            stream,
            &budgets,
            &Envelope::new(request.host, request.operation, body),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn framed_handshake_and_denial_never_become_absence() {
        let (client, mut server) = duplex(1024);
        let server = tokio::spawn(async move {
            reply(
                &mut server,
                Response::Capabilities {
                    capabilities: Capabilities::default(),
                },
            )
            .await;
            reply(
                &mut server,
                Response::Lookup {
                    result: LookupResult::Denied {},
                },
            )
            .await;
            reply(
                &mut server,
                Response::Refused {
                    reason: Refusal::OutcomeUncertain,
                },
            )
            .await;
        });
        let mut client = ControlClient::on_authenticated_stream(client, binding(), permit());
        let cancel = CancellationToken::new();
        assert_eq!(
            client.handshake(&cancel).await.unwrap(),
            Capabilities::default()
        );
        let response = client
            .request(
                OperationId::new(),
                Request::Lookup {
                    logical: LogicalSessionId::new(),
                },
                &cancel,
            )
            .await
            .unwrap();
        assert!(matches!(
            response.body,
            Response::Lookup {
                result: LookupResult::Denied {}
            }
        ));
        let response = client
            .request(
                OperationId::new(),
                Request::Lookup {
                    logical: LogicalSessionId::new(),
                },
                &cancel,
            )
            .await
            .unwrap();
        assert!(matches!(
            response.body,
            Response::Refused {
                reason: Refusal::OutcomeUncertain
            }
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn wrong_operation_or_host_permanently_retires_stream() {
        for wrong_host in [false, true] {
            let (client, mut server) = duplex(1024);
            let server = tokio::spawn(async move {
                let budgets = Budgets::default();
                let request: Envelope<Request> = read_frame(&mut server, &budgets)
                    .await
                    .unwrap()
                    .decode()
                    .unwrap();
                let reply = Envelope::new(
                    if wrong_host {
                        HostId::new()
                    } else {
                        request.host
                    },
                    if wrong_host {
                        request.operation
                    } else {
                        OperationId::new()
                    },
                    Response::Capabilities {
                        capabilities: Capabilities::default(),
                    },
                );
                write_frame(&mut server, &budgets, &reply).await.unwrap();
            });
            let mut client = ControlClient::on_authenticated_stream(client, binding(), permit());
            assert_eq!(
                client
                    .handshake(&CancellationToken::new())
                    .await
                    .unwrap_err(),
                Error::Contract(ContractError::Stale)
            );
            assert_eq!(
                client
                    .handshake(&CancellationToken::new())
                    .await
                    .unwrap_err(),
                Error::Closed
            );
            server.await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_and_cancellation_fence_late_replies() {
        let (stream, _peer) = duplex(1024);
        let mut client = ControlClient::on_authenticated_stream(stream, binding(), permit());
        assert_eq!(
            client
                .handshake(&CancellationToken::new())
                .await
                .unwrap_err(),
            Error::Deadline
        );
        assert_eq!(
            client
                .handshake(&CancellationToken::new())
                .await
                .unwrap_err(),
            Error::Closed
        );
        let (stream, _peer) = duplex(1024);
        let mut client = ControlClient::on_authenticated_stream(stream, binding(), permit());
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            client.handshake(&cancel).await.unwrap_err(),
            Error::Cancelled
        );
        assert_eq!(
            client
                .handshake(&CancellationToken::new())
                .await
                .unwrap_err(),
            Error::Closed
        );
    }

    #[tokio::test]
    async fn dropped_exchange_cannot_adopt_a_late_reply() {
        let (stream, mut peer) = duplex(1024);
        let mut client = ControlClient::on_authenticated_stream(stream, binding(), permit());
        let cancel = CancellationToken::new();
        {
            let exchange = client.handshake(&cancel);
            tokio::pin!(exchange);
            tokio::select! {
                result = &mut exchange => panic!("unexpected completion: {result:?}"),
                result = peer.read_u32() => { assert!(result.unwrap() > 0); }
            }
        }
        assert_eq!(client.handshake(&cancel).await.unwrap_err(), Error::Closed);
    }

    #[tokio::test]
    async fn frame_lengths_refuse_without_reading_body_and_release_budget() {
        let budgets = Budgets::default();
        for length in [0, (CONTROL_FRAME_BYTES + 1) as u32, u32::MAX] {
            let (mut client, mut server) = duplex(4);
            client.write_u32(length).await.unwrap();
            assert!(matches!(
                read_frame(&mut server, &budgets).await,
                Err(Error::Contract(ContractError::Limit))
            ));
        }
        let all = budgets
            .reserve(
                Resource::ControlBytes,
                festerm_host_protocol::MAX_CONTROL_BYTES,
            )
            .unwrap();
        let (mut client, mut server) = duplex(4);
        client.write_u32(1).await.unwrap();
        assert!(matches!(
            read_frame(&mut server, &budgets).await,
            Err(Error::Contract(ContractError::Busy))
        ));
        drop(all);
        assert!(budgets
            .reserve(
                Resource::ControlBytes,
                festerm_host_protocol::MAX_CONTROL_BYTES
            )
            .is_ok());
    }

    #[tokio::test]
    async fn legacy_protocol_and_wrong_reply_shape_have_no_fallback() {
        let (stream, mut peer) = duplex(1024);
        let task = tokio::spawn(async move {
            let budgets = Budgets::default();
            let _: Envelope<Request> = read_frame(&mut peer, &budgets)
                .await
                .unwrap()
                .decode()
                .unwrap();
            let legacy = br#"{"protocol":"festerm-bridge-v1","version":4,"target":{}}"#;
            peer.write_u32(legacy.len() as u32).await.unwrap();
            peer.write_all(legacy).await.unwrap();
        });
        let mut client = ControlClient::on_authenticated_stream(stream, binding(), permit());
        assert_eq!(
            client
                .handshake(&CancellationToken::new())
                .await
                .unwrap_err(),
            Error::Contract(ContractError::UnsupportedVersion)
        );
        task.await.unwrap();
        let (stream, mut peer) = duplex(1024);
        let task = tokio::spawn(async move {
            reply(
                &mut peer,
                Response::Lookup {
                    result: LookupResult::Offline {},
                },
            )
            .await;
        });
        let mut client = ControlClient::on_authenticated_stream(stream, binding(), permit());
        assert!(client.handshake(&CancellationToken::new()).await.is_err());
        task.await.unwrap();
    }
}
