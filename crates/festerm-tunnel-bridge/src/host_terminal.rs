//! Separate, generation-bound HOST terminal streams. No listener is installed.
//! Existing terminal snapshots and binary frames remain the terminal protocol;
//! HOST request/reply frames never grant file, observation or MCP capabilities.
use crate::protocol::*;
use festerm_core::Terminal;
#[cfg(feature = "relay")]
use festerm_host_protocol::ConnectionId;
use festerm_host_protocol::{
    AttachmentId, BudgetPermit, ClientBytesPermit, Envelope, OperationId, Request, Resource,
    Response, Target as HostTarget, TemporaryLease, CONTROL_FRAME_BYTES, MAX_BYTES_PER_CLIENT,
};
use festerm_tunnel_client::host_control::{Binding, ConnectionPermit, Error};
#[cfg(feature = "relay")]
use std::{collections::VecDeque, sync::Mutex};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

mod client;
#[cfg(feature = "relay")]
pub(crate) mod native;
pub use client::TerminalHandle;
#[cfg(test)]
#[path = "host_terminal_tests.rs"]
mod tests;

pub const HOST_REQUEST: u8 = 60;
pub const HOST_REPLY: u8 = 61;
pub const HOST_SNAPSHOT_LIMIT: usize = MAX_BYTES_PER_CLIENT;
const DEADLINE: Duration = Duration::from_secs(15);

/// One ready chunk per terminal in FIFO rotation. Admission is bounded by the
/// aggregate attachment permits. Scheduling never retains a lock or turn across
/// network I/O: a slow socket can occupy only its own bounded in-flight chunk.
#[derive(Clone, Default)]
#[cfg(feature = "relay")]
pub(crate) struct OutputScheduler(Arc<Scheduling>);
#[derive(Default)]
#[cfg(feature = "relay")]
struct Scheduling {
    ready: Mutex<VecDeque<ConnectionId>>,
    wake: tokio::sync::Notify,
}

#[cfg(feature = "relay")]
impl OutputScheduler {
    async fn turn(
        &self,
        attachment: &ConnectionId,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        struct Retire<'a>(&'a OutputScheduler, &'a ConnectionId);
        impl Drop for Retire<'_> {
            fn drop(&mut self) {
                self.0.retire(self.1);
            }
        }
        {
            let mut ready = self.0.ready.lock().map_err(|_| Error::Closed)?;
            if ready.len() >= festerm_host_protocol::MAX_ATTACHMENTS {
                return Err(festerm_host_protocol::ContractError::Busy.into());
            }
            ready.push_back(attachment.clone());
        }
        let _retire = Retire(self, attachment);
        tokio::task::yield_now().await;
        loop {
            let notified = self.0.wake.notified();
            {
                let mut ready = self.0.ready.lock().map_err(|_| Error::Closed)?;
                if ready.front() == Some(attachment) {
                    ready.pop_front();
                    self.0.wake.notify_waiters();
                    return Ok(());
                }
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => { self.retire(attachment); return Err(Error::Cancelled); }
                _ = notified => {}
            }
        }
    }

    fn retire(&self, attachment: &ConnectionId) {
        if let Ok(mut ready) = self.0.ready.lock() {
            ready.retain(|id| id != attachment);
        }
        self.0.wake.notify_waiters();
    }
}

pub struct Frame {
    kind: u8,
    bytes: Vec<u8>,
    _terminal: Option<ClientBytesPermit>,
    _control: Option<BudgetPermit>,
}
impl Frame {
    pub fn kind(&self) -> u8 {
        self.kind
    }
    pub fn payload(&self) -> &[u8] {
        &self.bytes
    }
}

fn maximum(kind: u8, client: bool) -> Result<usize, Error> {
    let allowed = if client {
        matches!(kind, HOST_REQUEST | ADOPTED | INPUT | RESIZE | SYNC)
    } else {
        matches!(
            kind,
            HOST_REPLY | SNAPSHOT | OUTPUT | RESIZED | SYNC | EXITED | DISPLACED
        )
    };
    if !allowed {
        return Err(festerm_host_protocol::ContractError::Malformed.into());
    }
    Ok(match kind {
        HOST_REQUEST | HOST_REPLY => CONTROL_FRAME_BYTES,
        SNAPSHOT => HOST_SNAPSHOT_LIMIT,
        INPUT | OUTPUT | SYNC => CHUNK_LIMIT,
        RESIZE | RESIZED => 8,
        DISPLACED => 1,
        _ => 0,
    })
}

pub async fn read_frame(
    stream: &mut (impl AsyncRead + Unpin),
    permit: &ConnectionPermit,
    from_client: bool,
) -> Result<Frame, Error> {
    let kind = stream.read_u8().await?;
    tokio::time::timeout(DEADLINE, read_body(stream, permit, from_client, kind))
        .await
        .map_err(|_| Error::Deadline)?
}

async fn read_body(
    stream: &mut (impl AsyncRead + Unpin),
    permit: &ConnectionPermit,
    from_client: bool,
    kind: u8,
) -> Result<Frame, Error> {
    let length = stream.read_u32().await? as usize;
    validate(kind, length, from_client)?;
    let control = matches!(kind, HOST_REQUEST | HOST_REPLY);
    let terminal = if length > 0 && !control {
        Some(permit.reserve_terminal_bytes(length)?)
    } else {
        None
    };
    let control = if length > 0 && control {
        Some(permit.budgets().reserve(Resource::ControlBytes, length)?)
    } else {
        None
    };
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    match kind {
        RESIZE | RESIZED => {
            crate::wire::decode_size(&bytes)?;
        }
        DISPLACED if Displacer::from_code(bytes[0]).is_none() => {
            return Err(festerm_host_protocol::ContractError::Malformed.into())
        }
        SYNC => {
            let sync: Sync = serde_json::from_slice(&bytes)
                .map_err(|_| festerm_host_protocol::ContractError::Malformed)?;
            match sync {
                Sync::Scrollback(limit) if limit > HOST_SNAPSHOT_LIMIT => {
                    return Err(festerm_host_protocol::ContractError::Limit.into())
                }
                Sync::Bytes(bytes) if bytes.len() > CHUNK_LIMIT => {
                    return Err(festerm_host_protocol::ContractError::Limit.into())
                }
                _ => {}
            }
        }
        _ => {}
    }
    Ok(Frame {
        kind,
        bytes,
        _terminal: terminal,
        _control: control,
    })
}

fn validate(kind: u8, length: usize, client: bool) -> Result<(), Error> {
    if length > maximum(kind, client)? {
        return Err(festerm_host_protocol::ContractError::Limit.into());
    }
    if matches!(kind, RESIZE | RESIZED) && length != 8
        || kind == DISPLACED && length != 1
        || matches!(kind, SNAPSHOT | INPUT | OUTPUT | SYNC) && length == 0
    {
        return Err(festerm_host_protocol::ContractError::Malformed.into());
    }
    if matches!(kind, HOST_REQUEST | HOST_REPLY) {
        if length == 0 {
            return Err(festerm_host_protocol::ContractError::Malformed.into());
        }
    } else {
        validate_length(kind, length)?;
    }
    Ok(())
}

async fn write(
    stream: &mut (impl AsyncWrite + Unpin),
    kind: u8,
    bytes: &[u8],
) -> Result<(), Error> {
    validate(
        kind,
        bytes.len(),
        !matches!(
            kind,
            HOST_REPLY | SNAPSHOT | OUTPUT | RESIZED | EXITED | DISPLACED
        ),
    )?;
    let work = async {
        stream.write_u8(kind).await?;
        stream.write_u32(bytes.len() as u32).await?;
        stream.write_all(bytes).await?;
        stream.flush().await
    };
    tokio::time::timeout(DEADLINE, work)
        .await
        .map_err(|_| Error::Deadline)??;
    Ok(())
}

pub(crate) async fn write_contract<T: serde::Serialize + festerm_host_protocol::Validate>(
    stream: &mut (impl AsyncWrite + Unpin),
    permit: &ConnectionPermit,
    kind: u8,
    value: &T,
) -> Result<(), Error> {
    let _charge = permit
        .budgets()
        .reserve(Resource::ControlBytes, CONTROL_FRAME_BYTES)?;
    let bytes = festerm_host_protocol::encode(value)?;
    write(stream, kind, &bytes).await
}

/// Recovery received, but no frontend adoption or Attached success yet.
pub struct PendingTerminal<S> {
    stream: S,
    permit: ConnectionPermit,
    binding: Binding,
    request: Envelope<Request>,
    snapshot: Option<Terminal>,
    capabilities: festerm_host_protocol::Capabilities,
}

#[allow(clippy::too_many_arguments)]
pub async fn establish<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    permit: ConnectionPermit,
    binding: Binding,
    operation: OperationId,
    target: HostTarget,
    attachment: AttachmentId,
    takeover: bool,
    cancel: &CancellationToken,
) -> Result<PendingTerminal<S>, Error> {
    let request = Envelope::new(
        binding.host().clone(),
        operation,
        Request::Attach {
            target,
            attachment,
            takeover,
        },
    );
    let work = async {
        let hello = Envelope::new(
            binding.host().clone(),
            OperationId::new(),
            Request::Capabilities {},
        );
        write_contract(&mut stream, &permit, HOST_REQUEST, &hello).await?;
        let frame = read_frame(&mut stream, &permit, false).await?;
        if frame.kind != HOST_REPLY {
            return Err(festerm_host_protocol::ContractError::Malformed.into());
        }
        let response: Envelope<Response> = festerm_host_protocol::decode(&frame.bytes)?;
        response.validate_for(&hello)?;
        let capabilities = match response.body {
            Response::Capabilities { capabilities } if capabilities.attach => capabilities,
            Response::Refused { reason } => return Err(Error::Refused(reason)),
            _ => return Err(Error::Refused(festerm_host_protocol::Refusal::Unsupported)),
        };
        drop(frame);
        write_contract(&mut stream, &permit, HOST_REQUEST, &request).await?;
        let frame = read_frame(&mut stream, &permit, false).await?;
        if frame.kind == HOST_REPLY {
            let reply: Envelope<Response> = festerm_host_protocol::decode(&frame.bytes)?;
            reply.validate_for(&request)?;
            return Err(match reply.body {
                Response::Refused { reason } => Error::Refused(reason),
                _ => festerm_host_protocol::ContractError::Malformed.into(),
            });
        }
        if frame.kind != SNAPSHOT {
            return Err(festerm_host_protocol::ContractError::Malformed.into());
        }
        let snapshot = decode_snapshot(&frame.bytes)?;
        Ok(PendingTerminal {
            stream,
            permit,
            binding,
            request,
            snapshot: Some(snapshot),
            capabilities,
        })
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Error::Cancelled),
        result = tokio::time::timeout(DEADLINE, work) => result.map_err(|_| Error::Deadline)?,
    }
}

#[derive(Clone, Debug)]
pub struct LiveBinding {
    pub host: Binding,
    pub target: HostTarget,
    pub attachment: AttachmentId,
    pub lease: Option<TemporaryLease>,
    pub capabilities: festerm_host_protocol::Capabilities,
}

pub struct LiveTerminal<S> {
    stream: S,
    permit: Arc<ConnectionPermit>,
    binding: LiveBinding,
    usable: bool,
}

impl<S: AsyncRead + AsyncWrite + Unpin> PendingTerminal<S> {
    pub fn take_snapshot(&mut self) -> Option<Terminal> {
        self.snapshot.take()
    }

    /// Only call after the frontend has actually adopted `take_snapshot()`.
    pub async fn adopt(mut self, cancel: &CancellationToken) -> Result<LiveTerminal<S>, Error> {
        if self.snapshot.is_some() {
            return Err(festerm_host_protocol::ContractError::Denied.into());
        }
        let work = async {
            write(&mut self.stream, ADOPTED, &[]).await?;
            let frame = read_frame(&mut self.stream, &self.permit, false).await?;
            if frame.kind != HOST_REPLY {
                return Err(festerm_host_protocol::ContractError::Malformed.into());
            }
            let reply: Envelope<Response> = festerm_host_protocol::decode(&frame.bytes)?;
            reply.validate_for(&self.request)?;
            let (target, attachment, lease) = match reply.body {
                Response::Attached {
                    target,
                    attachment,
                    lease,
                } => (target, attachment, lease),
                Response::Refused { reason } => return Err(Error::Refused(reason)),
                _ => return Err(festerm_host_protocol::ContractError::Malformed.into()),
            };
            Ok(LiveTerminal {
                stream: self.stream,
                permit: Arc::new(self.permit),
                binding: LiveBinding {
                    host: self.binding,
                    target,
                    attachment,
                    lease,
                    capabilities: self.capabilities,
                },
                usable: true,
            })
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(DEADLINE, work) => result.map_err(|_| Error::Deadline)?,
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> LiveTerminal<S> {
    pub fn binding(&self) -> &LiveBinding {
        &self.binding
    }

    pub async fn receive(&mut self, cancel: &CancellationToken) -> Result<Frame, Error> {
        if !self.usable {
            return Err(Error::Closed);
        }
        self.usable = false;
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = read_frame(&mut self.stream, &self.permit, false) => result,
        };
        if let Ok(frame) = &result {
            if !matches!(frame.kind, OUTPUT | RESIZED | SYNC | EXITED | DISPLACED) {
                return Err(festerm_host_protocol::ContractError::Malformed.into());
            }

            self.usable = !matches!(frame.kind, EXITED | DISPLACED);
        }
        result
    }

    pub async fn input(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if !self.usable {
            return Err(Error::Closed);
        }
        if bytes.is_empty() || bytes.len() > CHUNK_LIMIT {
            return Err(festerm_host_protocol::ContractError::Limit.into());
        }
        let _bytes = self.permit.reserve_terminal_bytes(bytes.len())?;
        self.usable = false;
        write(&mut self.stream, INPUT, bytes).await?;
        self.usable = true;
        Ok(())
    }

    pub async fn resize(&mut self, size: festerm_session::TerminalSize) -> Result<(), Error> {
        if !self.usable {
            return Err(Error::Closed);
        }
        let _bytes = self.permit.reserve_terminal_bytes(8)?;
        self.usable = false;
        write(&mut self.stream, RESIZE, &crate::wire::encode_size(size)).await?;
        self.usable = true;
        Ok(())
    }

    pub async fn detach(mut self, cancel: &CancellationToken) -> Result<(), Error> {
        if !self.usable {
            return Err(Error::Closed);
        }
        if !self.binding.capabilities.detach {
            return Err(Error::Refused(festerm_host_protocol::Refusal::Unsupported));
        }
        let lease = self.binding.lease.as_ref();
        let request = Envelope::new(
            self.binding.host.host().clone(),
            OperationId::new(),
            Request::Detach {
                target: self.binding.target.clone(),
                attachment: self.binding.attachment.clone(),
                lease: lease.map(|lease| lease.lease.clone()),
                lease_epoch: lease.map(|lease| lease.epoch),
                reason: festerm_host_protocol::DetachKind::Intentional,
            },
        );
        let work = async {
            write_contract(&mut self.stream, &self.permit, HOST_REQUEST, &request).await?;
            loop {
                let frame = read_frame(&mut self.stream, &self.permit, false).await?;
                if frame.kind == HOST_REPLY {
                    let reply: Envelope<Response> = festerm_host_protocol::decode(&frame.bytes)?;
                    reply.validate_for(&request)?;
                    return match reply.body {
                        Response::Detached { .. } => Ok(()),
                        Response::Refused { reason } => Err(Error::Refused(reason)),
                        _ => Err(festerm_host_protocol::ContractError::Malformed.into()),
                    };
                }
                if !matches!(frame.kind, OUTPUT | RESIZED | SYNC) {
                    return Err(Error::Closed);
                }
            }
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(DEADLINE, work) => result.map_err(|_| Error::Deadline)?,
        }
    }
}
