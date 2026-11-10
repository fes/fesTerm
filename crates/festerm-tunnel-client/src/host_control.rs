//! Bounded HOST RPC on an already authenticated, host-key-pinned channel.
//!
//! These APIs provide framing/correlation, not authentication. The composition
//! boundary must supply the *same* channel used for enrolled owner proof.
//! Never retry transport failures automatically or reinterpret them as absence.

use festerm_host_protocol::{
    decode, encode, BudgetPermit, Budgets, Capabilities, ClientBudget, ContractError, Envelope,
    HostId, IssuedOperation, OperationId, Request, Resource, Response, Validate,
    CONTROL_DEADLINE_SECONDS, CONTROL_FRAME_BYTES,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{fmt, io, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Contract(ContractError),
    Refused(festerm_host_protocol::Refusal),
    Closed,
    Deadline,
    Cancelled,
    Offline,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Contract(_) => "invalid or refused host control message",
            Self::Refused(_) => "host operation refused",
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
    let first = stream.read_u8().await?;
    read_frame_started(stream, budgets, first).await
}

/// Finish a frame after separately admitting its first byte (idle deadline).
pub async fn read_frame_started(
    stream: &mut (impl AsyncRead + Unpin),
    budgets: &Budgets,
    first: u8,
) -> Result<Frame, Error> {
    let mut prefix = [first, 0, 0, 0];
    stream.read_exact(&mut prefix[1..]).await?;
    let length = u32::from_be_bytes(prefix) as usize;
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

/// Original creation scope plus immutable destination. Reconnection to the same
/// binding can reconcile it; account/endpoint changes cannot silently reuse it.
#[derive(Clone, Debug)]
pub struct IssuedControlOperation {
    binding: Binding,
    operation: IssuedOperation,
}

impl IssuedControlOperation {
    pub fn operation(&self) -> &IssuedOperation {
        &self.operation
    }
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

    pub fn issue_operation(&self) -> Result<IssuedControlOperation, Error> {
        if !self.usable {
            return Err(Error::Closed);
        }
        let epoch = self
            .capabilities
            .filter(|caps| caps.ensure || caps.create)
            .and_then(|caps| caps.issuance_epoch)
            .ok_or(Error::Refused(festerm_host_protocol::Refusal::Unsupported))?;
        Ok(IssuedControlOperation {
            binding: self.binding.clone(),
            operation: IssuedOperation::new(epoch),
        })
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
            Response::Refused { reason } => {
                self.usable = false;
                Err(Error::Refused(reason))
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
        self.exchange(
            Envelope::new(self.binding.host.clone(), operation, body),
            cancel,
        )
        .await
    }

    pub async fn request_issued(
        &mut self,
        operation: &IssuedControlOperation,
        body: Request,
        cancel: &CancellationToken,
    ) -> Result<Envelope<Response>, Error> {
        if operation.binding != self.binding {
            return Err(ContractError::Stale.into());
        }
        self.exchange(
            Envelope::issued(self.binding.host.clone(), &operation.operation, body),
            cancel,
        )
        .await
    }

    async fn exchange(
        &mut self,
        request: Envelope<Request>,
        cancel: &CancellationToken,
    ) -> Result<Envelope<Response>, Error> {
        if !self.usable {
            return Err(Error::Closed);
        }
        if self.capabilities.is_none() && !matches!(request.body, Request::Capabilities {}) {
            return Err(ContractError::Denied.into());
        }
        request.validate()?;
        if self
            .capabilities
            .is_some_and(|caps| !caps.supports(&request.body))
        {
            let mut response = Envelope::new(
                request.host,
                request.operation,
                Response::Refused {
                    reason: festerm_host_protocol::Refusal::Unsupported,
                },
            );
            response.issuance_epoch = request.issuance_epoch;
            return Ok(response);
        }
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
    use festerm_host_protocol::{
        AttachmentId, Creation, Epoch, Lifetime, LogicalSessionId, LookupResult, Recipe, Refusal,
    };
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
        let mut response = Envelope::new(request.host, request.operation, body);
        response.issuance_epoch = request.issuance_epoch;
        write_frame(stream, &budgets, &response).await.unwrap();
    }

    fn creation() -> Request {
        Request::Ensure {
            creation: Creation {
                logical: LogicalSessionId::new(),
                attachment: AttachmentId::new(),
                name: None,
                recipe: Recipe::default(),
                lifetime: Lifetime::Persistent {},
            },
        }
    }

    fn scoped_capabilities(epoch: u64) -> Capabilities {
        Capabilities {
            ensure: true,
            attach: true,
            creation_allowed: true,
            issuance_epoch: Some(Epoch::new(epoch).unwrap()),
            ..Capabilities::default()
        }
    }

    #[tokio::test]
    async fn issued_retry_retains_scope_across_reconnect_and_local_refusal() {
        let destination = binding();
        let cancel = CancellationToken::new();
        let (stream, mut peer) = duplex(1024);
        let task = tokio::spawn(async move {
            reply(
                &mut peer,
                Response::Capabilities {
                    capabilities: scoped_capabilities(1),
                },
            )
            .await;
        });
        let mut first =
            ControlClient::on_authenticated_stream(stream, destination.clone(), permit());
        first.handshake(&cancel).await.unwrap();
        let original = first.issue_operation().unwrap();
        drop(first);
        task.await.unwrap();

        let body = creation();
        let expected = original.operation().clone();
        let (stream, mut peer) = duplex(1024);
        let task = tokio::spawn(async move {
            reply(
                &mut peer,
                Response::Capabilities {
                    capabilities: scoped_capabilities(2),
                },
            )
            .await;
            let budgets = Budgets::default();
            let request: Envelope<Request> = read_frame(&mut peer, &budgets)
                .await
                .unwrap()
                .decode()
                .unwrap();
            assert_eq!(request.issued_operation().unwrap(), expected);
            let response = Envelope::issued(
                request.host,
                &expected,
                Response::Refused {
                    reason: Refusal::OutcomeUncertain,
                },
            );
            write_frame(&mut peer, &budgets, &response).await.unwrap();
        });
        let mut second =
            ControlClient::on_authenticated_stream(stream, destination.clone(), permit());
        second.handshake(&cancel).await.unwrap();
        assert_eq!(
            second.issue_operation().unwrap().operation().epoch,
            Epoch::new(2).unwrap()
        );
        let response = second
            .request_issued(&original, body.clone(), &cancel)
            .await
            .unwrap();
        assert_eq!(response.issuance_epoch, Some(original.operation().epoch));
        assert_eq!(
            response.body,
            Response::Refused {
                reason: Refusal::OutcomeUncertain
            }
        );
        drop(second);
        task.await.unwrap();

        let (stream, mut peer) = duplex(1024);
        let task = tokio::spawn(async move {
            reply(
                &mut peer,
                Response::Capabilities {
                    capabilities: Capabilities::default(),
                },
            )
            .await;
            assert!(
                tokio::time::timeout(Duration::from_millis(20), peer.read_u8())
                    .await
                    .is_err()
            );
        });
        let mut third = ControlClient::on_authenticated_stream(stream, destination, permit());
        third.handshake(&cancel).await.unwrap();
        assert!(matches!(
            third.issue_operation(),
            Err(Error::Refused(Refusal::Unsupported))
        ));
        let response = third
            .request_issued(&original, body, &cancel)
            .await
            .unwrap();
        assert_eq!(response.issuance_epoch, Some(original.operation().epoch));
        assert_eq!(
            response.body,
            Response::Refused {
                reason: Refusal::Unsupported
            }
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn issued_operation_cannot_cross_account_endpoint_or_host() {
        let destination = binding();
        let cancel = CancellationToken::new();
        let (stream, mut peer) = duplex(1024);
        let task = tokio::spawn(async move {
            reply(
                &mut peer,
                Response::Capabilities {
                    capabilities: scoped_capabilities(1),
                },
            )
            .await;
        });
        let mut original_client =
            ControlClient::on_authenticated_stream(stream, destination.clone(), permit());
        original_client.handshake(&cancel).await.unwrap();
        let original = original_client.issue_operation().unwrap();
        task.await.unwrap();
        for changed in [
            Binding::new(
                destination.host.clone(),
                "other-account".into(),
                destination.endpoint.clone(),
            )
            .unwrap(),
            Binding::new(
                destination.host.clone(),
                destination.account.clone(),
                "other-endpoint".into(),
            )
            .unwrap(),
            Binding::new(
                HostId::new(),
                destination.account.clone(),
                destination.endpoint.clone(),
            )
            .unwrap(),
        ] {
            let (stream, mut peer) = duplex(1024);
            let task = tokio::spawn(async move {
                reply(
                    &mut peer,
                    Response::Capabilities {
                        capabilities: scoped_capabilities(2),
                    },
                )
                .await;
                assert!(
                    tokio::time::timeout(Duration::from_millis(20), peer.read_u8())
                        .await
                        .is_err()
                );
            });
            let mut client = ControlClient::on_authenticated_stream(stream, changed, permit());
            client.handshake(&cancel).await.unwrap();
            assert_eq!(
                client.request_issued(&original, creation(), &cancel).await,
                Err(Error::Contract(ContractError::Stale))
            );
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn missing_or_forbidden_scope_is_local_and_wrong_echo_retires_stream() {
        let (stream, mut peer) = duplex(1024);
        let task = tokio::spawn(async move {
            reply(
                &mut peer,
                Response::Capabilities {
                    capabilities: scoped_capabilities(1),
                },
            )
            .await;
            let budgets = Budgets::default();
            let request: Envelope<Request> = read_frame(&mut peer, &budgets)
                .await
                .unwrap()
                .decode()
                .unwrap();
            assert_eq!(request.issuance_epoch, Some(Epoch::new(1).unwrap()));
            let mut response = Envelope::new(
                request.host,
                request.operation,
                Response::Refused {
                    reason: Refusal::Unknown,
                },
            );
            response.issuance_epoch = Some(Epoch::new(2).unwrap());
            write_frame(&mut peer, &budgets, &response).await.unwrap();
        });
        let mut client = ControlClient::on_authenticated_stream(stream, binding(), permit());
        let cancel = CancellationToken::new();
        client.handshake(&cancel).await.unwrap();
        let operation = client.issue_operation().unwrap();
        assert_eq!(
            client
                .request(OperationId::new(), creation(), &cancel)
                .await,
            Err(Error::Contract(ContractError::Malformed))
        );
        assert_eq!(
            client
                .request_issued(
                    &operation,
                    Request::Lookup {
                        logical: LogicalSessionId::new()
                    },
                    &cancel
                )
                .await,
            Err(Error::Contract(ContractError::Malformed))
        );
        assert_eq!(
            client.request_issued(&operation, creation(), &cancel).await,
            Err(Error::Contract(ContractError::Stale))
        );
        assert_eq!(client.handshake(&cancel).await, Err(Error::Closed));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn framed_handshake_and_denial_never_become_absence() {
        let (client, mut server) = duplex(1024);
        let server = tokio::spawn(async move {
            reply(
                &mut server,
                Response::Capabilities {
                    capabilities: Capabilities {
                        inventory: true,
                        ..Capabilities::default()
                    },
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
            Capabilities {
                inventory: true,
                ..Capabilities::default()
            }
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
    async fn handshake_preserves_typed_refusals() {
        for reason in [
            Refusal::Busy,
            Refusal::Offline,
            Refusal::Unknown,
            Refusal::Incompatible,
            Refusal::Revoked,
            Refusal::OutcomeUncertain,
        ] {
            let (stream, mut peer) = duplex(1024);
            let server = tokio::spawn(async move {
                reply(&mut peer, Response::Refused { reason }).await;
            });
            let mut client = ControlClient::on_authenticated_stream(stream, binding(), permit());
            assert_eq!(
                client.handshake(&CancellationToken::new()).await,
                Err(Error::Refused(reason))
            );
            assert_eq!(
                client.handshake(&CancellationToken::new()).await,
                Err(Error::Closed)
            );
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn unadvertised_operation_is_correlated_unsupported_without_transport_work() {
        let (stream, mut peer) = duplex(1024);
        let server = tokio::spawn(async move {
            reply(
                &mut peer,
                Response::Capabilities {
                    capabilities: Capabilities::default(),
                },
            )
            .await;
            assert!(
                tokio::time::timeout(Duration::from_millis(20), peer.read_u8())
                    .await
                    .is_err()
            );
        });
        let mut client = ControlClient::on_authenticated_stream(stream, binding(), permit());
        let cancel = CancellationToken::new();
        client.handshake(&cancel).await.unwrap();
        let operation = OperationId::new();
        let response = client
            .request(
                operation.clone(),
                Request::Lookup {
                    logical: LogicalSessionId::new(),
                },
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(response.operation, operation);
        assert_eq!(
            response.body,
            Response::Refused {
                reason: Refusal::Unsupported
            }
        );
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
