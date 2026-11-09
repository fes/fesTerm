//! Thin wrapper around [`winrm_rs::Shell`] that ferries PSRP fragments.
//!
//! This module is the **only** place in the crate that imports
//! `winrm_rs::Shell`. Every higher-level component (runspace pool,
//! pipeline) talks to the transport through the [`PsrpTransport`] trait so
//! it can be mocked in tests without standing up a fake SOAP server.

use std::{
    collections::VecDeque,
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use tracing::{debug, warn};
use winrm_rs::{RESOURCE_URI_PSRP, Shell, SoapError, WinrmClient, WinrmError};

use crate::error::{PsrpError, Result};

/// Whether the successful stop response itself acknowledges pipeline termination.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StopAcknowledgement {
    /// A later pipeline-state event must confirm termination.
    #[default]
    AwaitPipelineState,
    /// The transport response confirms the pipeline has stopped.
    Stopped,
}

/// Abstract transport used by the runspace pool and pipeline.
///
/// `send_fragment` MUST write the pre-encoded fragment bytes as a single
/// `send_input` call. `recv_chunk` MUST transparently retry on
/// `WinrmError::Timeout` (long-polling is expected) but propagate every
/// other error — in particular SOAP faults, which usually mean the
/// server-side shell has died and the pool must be recreated.
#[async_trait]
pub trait PsrpTransport: Send + Sync {
    async fn send_fragment(&self, bytes: &[u8]) -> Result<()>;
    async fn recv_chunk(&mut self) -> Result<Vec<u8>>;
    async fn signal_stop(&self, pipeline_id: uuid::Uuid) -> Result<StopAcknowledgement>;
    async fn close_shell(&mut self) -> Result<()>;

    /// Release a completed pipeline's transport resources.
    async fn close_pipeline(&mut self, _pipeline_id: uuid::Uuid) -> Result<()> {
        Ok(())
    }

    /// Send a PSRP fragment that is addressed to a specific pipeline.
    ///
    /// Most transports carry the pipeline id inside the PSRP message itself,
    /// so the default preserves the historical behaviour. Out-of-process
    /// PowerShell SSH remoting also wraps the fragment in an outer XML
    /// envelope whose `PSGuid` must match the target pipeline.
    async fn send_pipeline_fragment(&self, _pipeline_id: uuid::Uuid, bytes: &[u8]) -> Result<()> {
        self.send_fragment(bytes).await
    }

    /// Start a pipeline by executing a WS-Man Command with the first
    /// PSRP fragment as an argument and the pipeline's UUID as the
    /// CommandId. Subsequent Send/Receive use this CommandId.
    /// Default: sends via `send_fragment` (mock transport behavior).
    async fn execute_pipeline(
        &mut self,
        fragment_bytes: &[u8],
        _pipeline_id: uuid::Uuid,
    ) -> Result<()> {
        self.send_fragment(fragment_bytes).await
    }

    /// Disconnect the underlying transport while leaving the server-side
    /// resources alive. Returns an opaque handle that the caller can pass
    /// back to the transport-specific reconnect path. For
    /// [`WinrmPsrpTransport`] this is the WinRM `shell_id`.
    ///
    /// Default implementation returns an error so that transports that
    /// don't support disconnect (e.g. mocks) don't have to implement it.
    async fn disconnect_shell(&mut self) -> Result<String> {
        Err(PsrpError::protocol(
            "this transport does not implement disconnect_shell",
        ))
    }
}

/// PSRP transport over an already-authenticated, already-opened byte stream.
///
/// This is the narrow transport seam needed for PowerShell remoting over an
/// SSH subsystem without making `psrp-rs` own SSH authentication or host-key
/// policy. Callers must open the real `"powershell"` SSH subsystem elsewhere
/// (for fesTerm, through `festerm-ssh`) and pass its bounded stdio stream here.
/// Bytes are written and read exactly as PSRP fragments; there is no PTY,
/// shell text protocol, CLIXML-in-terminal parsing, or WinRM/SOAP wrapping.
/// The stream must use bounded read and write timeouts so a cancelled blocking
/// operation can eventually release it.
#[derive(Debug)]
pub struct BlockingIoPsrpTransport<S> {
    stream: Arc<Mutex<Option<S>>>,
    read_task: tokio::sync::Mutex<Option<BlockingReadTask>>,
    read_chunk_bytes: usize,
    framing: BlockingIoFraming,
    inbound: tokio::sync::Mutex<OutOfProcInbound>,
    ack_timeout: Duration,
}

type BlockingReadTask = tokio::task::JoinHandle<Result<Option<Vec<u8>>>>;

const MAX_READ_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const MAX_OUT_OF_PROC_ENVELOPE_BYTES: usize = 1024 * 1024;
const MAX_OUT_OF_PROC_DATA_BYTES: usize =
    crate::fragment::MAX_FRAGMENT_PAYLOAD + crate::fragment::FRAGMENT_HEADER_LEN;
const MAX_PENDING_OUT_OF_PROC_BYTES: usize = 8 * 1024 * 1024;
const MAX_PENDING_OUT_OF_PROC_PACKETS: usize = 256;
const DEFAULT_OUT_OF_PROC_ACK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
struct OutOfProcInbound {
    buffer: Vec<u8>,
    pending_data: VecDeque<Vec<u8>>,
    pending_bytes: usize,
    in_flight: bool,
    failed: bool,
}

impl OutOfProcInbound {
    fn queue_data(&mut self, data: Vec<u8>) -> Result<()> {
        if self.pending_data.len() >= MAX_PENDING_OUT_OF_PROC_PACKETS
            || self.pending_bytes.saturating_add(data.len()) > MAX_PENDING_OUT_OF_PROC_BYTES
        {
            return Err(PsrpError::protocol("PSRP SSH pending data limit exceeded"));
        }
        self.pending_bytes += data.len();
        self.pending_data.push_back(data);
        Ok(())
    }

    fn take_data(&mut self) -> Option<Vec<u8>> {
        let data = self.pending_data.pop_front()?;
        self.pending_bytes -= data.len();
        Some(data)
    }
}

impl<S> BlockingIoPsrpTransport<S>
where
    S: Read + Write + Send + 'static,
{
    /// Default upper bound for one blocking read operation. Fragment
    /// reassembly above this transport handles arbitrary frame boundaries.
    pub const DEFAULT_READ_CHUNK_BYTES: usize = 16 * 1024;

    /// Wrap an opened PSRP byte stream using the default read chunk bound.
    pub fn new(stream: S) -> Self {
        Self::with_read_chunk_bytes(stream, Self::DEFAULT_READ_CHUNK_BYTES)
    }

    /// Wrap an opened PSRP byte stream with an explicit per-read bound.
    ///
    /// The bound is transport-local. Higher-level command/object budgets still
    /// apply while decoding PSRP/CLIXML payloads.
    pub fn with_read_chunk_bytes(stream: S, read_chunk_bytes: usize) -> Self {
        Self {
            stream: Arc::new(Mutex::new(Some(stream))),
            read_task: tokio::sync::Mutex::new(None),
            read_chunk_bytes: read_chunk_bytes.clamp(1, MAX_READ_CHUNK_BYTES),
            framing: BlockingIoFraming::Raw,
            inbound: tokio::sync::Mutex::new(OutOfProcInbound::default()),
            ack_timeout: DEFAULT_OUT_OF_PROC_ACK_TIMEOUT,
        }
    }

    /// Wrap an opened PowerShell SSH subsystem stream.
    ///
    /// PowerShell's `pwsh -sshs` subsystem uses the out-of-process remoting
    /// XML envelope (`<Data>`, `<Command>`, `<Close>`, `<Signal>`) around
    /// base64-encoded PSRP fragments. This constructor enables that envelope
    /// while preserving the same byte-stream transport bounds.
    pub fn powershell_ssh(stream: S) -> Self {
        Self::with_powershell_ssh_read_chunk_bytes(stream, Self::DEFAULT_READ_CHUNK_BYTES)
    }

    /// Wrap a PowerShell SSH stream with a bound on each protocol acknowledgement.
    pub fn powershell_ssh_with_ack_timeout(stream: S, ack_timeout: Duration) -> Self {
        let mut transport =
            Self::with_powershell_ssh_read_chunk_bytes(stream, Self::DEFAULT_READ_CHUNK_BYTES);
        transport.ack_timeout = ack_timeout;
        transport
    }

    /// Like [`powershell_ssh`](Self::powershell_ssh), with an explicit
    /// transport-local read chunk bound.
    pub fn with_powershell_ssh_read_chunk_bytes(stream: S, read_chunk_bytes: usize) -> Self {
        Self {
            stream: Arc::new(Mutex::new(Some(stream))),
            read_task: tokio::sync::Mutex::new(None),
            read_chunk_bytes: read_chunk_bytes.clamp(1, MAX_READ_CHUNK_BYTES),
            framing: BlockingIoFraming::PowerShellOutOfProc,
            inbound: tokio::sync::Mutex::new(OutOfProcInbound::default()),
            ack_timeout: DEFAULT_OUT_OF_PROC_ACK_TIMEOUT,
        }
    }

    async fn read_chunk(&self) -> Result<Vec<u8>> {
        let mut pending = self.read_task.lock().await;
        loop {
            if pending.is_none() {
                let stream = Arc::clone(&self.stream);
                let limit = self.read_chunk_bytes;
                *pending = Some(tokio::task::spawn_blocking(move || {
                    let mut guard = stream
                        .lock()
                        .map_err(|_| PsrpError::protocol("PSRP SSH stream lock poisoned"))?;
                    let stream = guard
                        .as_mut()
                        .ok_or_else(|| PsrpError::protocol("PSRP SSH stream closed"))?;
                    let mut buffer = vec![0; limit];
                    match stream.read(&mut buffer) {
                        Ok(0) => Err(PsrpError::protocol("PSRP SSH stream closed by peer")),
                        Ok(read) => {
                            buffer.truncate(read);
                            Ok(Some(buffer))
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::TimedOut
                                    | std::io::ErrorKind::WouldBlock
                                    | std::io::ErrorKind::Interrupted
                            ) =>
                        {
                            Ok(None)
                        }
                        Err(error) => Err(PsrpError::protocol(format!(
                            "PSRP SSH stream read: {error}"
                        ))),
                    }
                }));
            }
            // A cancelled waiter leaves the single blocking read available to
            // the next call rather than losing its bytes or spawning another.
            let result = pending
                .as_mut()
                .expect("blocking read task exists")
                .await
                .map_err(|error| {
                    PsrpError::protocol(format!("PSRP SSH read worker failed: {error}"))
                });
            *pending = None;
            match result?? {
                Some(bytes) => return Ok(bytes),
                None => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    }

    async fn next_out_of_proc_packet(
        &self,
        inbound: &mut OutOfProcInbound,
    ) -> Result<OutOfProcPacket> {
        loop {
            if let Some(packet) = take_next_out_of_proc_packet(&mut inbound.buffer)? {
                return Ok(packet);
            }
            let bytes = self.read_chunk().await?;
            inbound.buffer.extend_from_slice(&bytes);
        }
    }

    async fn transact(&self, bytes: Vec<u8>, expected: OutOfProcAck) -> Result<()> {
        let mut inbound = self.inbound.lock().await;
        if inbound.failed || inbound.in_flight {
            return Err(PsrpError::protocol(
                "PSRP SSH acknowledgement state invalidated",
            ));
        }
        // A cancelled transaction must not allow a later operation to reuse a
        // stale acknowledgement or interleave with its abandoned write.
        inbound.in_flight = true;
        let result = tokio::time::timeout(self.ack_timeout, async {
            while !inbound.buffer.is_empty() {
                match take_next_out_of_proc_packet(&mut inbound.buffer)? {
                    Some(OutOfProcPacket::Data(data)) => inbound.queue_data(data)?,
                    Some(OutOfProcPacket::Ack(_)) => {
                        return Err(PsrpError::protocol("stale PSRP SSH acknowledgement"));
                    }
                    None if inbound.buffer.is_empty() => break,
                    None => match self.next_out_of_proc_packet(&mut inbound).await? {
                        OutOfProcPacket::Data(data) => inbound.queue_data(data)?,
                        OutOfProcPacket::Ack(_) => {
                            return Err(PsrpError::protocol("stale PSRP SSH acknowledgement"));
                        }
                    },
                }
            }
            write_out_of_proc_control(&self.stream, bytes).await?;
            loop {
                match self.next_out_of_proc_packet(&mut inbound).await? {
                    OutOfProcPacket::Data(data) => inbound.queue_data(data)?,
                    OutOfProcPacket::Ack(ack) if ack == expected => return Ok(()),
                    OutOfProcPacket::Ack(_) => {
                        return Err(PsrpError::protocol("unexpected PSRP SSH acknowledgement"));
                    }
                }
            }
        })
        .await;
        match result {
            Ok(Ok(())) => {
                inbound.in_flight = false;
                Ok(())
            }
            Ok(Err(error)) => {
                inbound.failed = true;
                Err(error)
            }
            Err(_) => {
                inbound.failed = true;
                Err(PsrpError::AckTimeout)
            }
        }
    }

    async fn send_out_of_proc_data(&self, id: uuid::Uuid, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return self
                .transact(
                    out_of_proc_data(id, bytes),
                    OutOfProcAck::new(OutOfProcAckKind::Data, id),
                )
                .await;
        }
        for chunk in bytes.chunks(MAX_OUT_OF_PROC_DATA_BYTES) {
            self.transact(
                out_of_proc_data(id, chunk),
                OutOfProcAck::new(OutOfProcAckKind::Data, id),
            )
            .await?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockingIoFraming {
    Raw,
    PowerShellOutOfProc,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutOfProcAckKind {
    Command,
    Data,
    Signal,
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OutOfProcAck {
    kind: OutOfProcAckKind,
    id: uuid::Uuid,
}

impl OutOfProcAck {
    fn new(kind: OutOfProcAckKind, id: uuid::Uuid) -> Self {
        Self { kind, id }
    }
}

enum OutOfProcPacket {
    Data(Vec<u8>),
    Ack(OutOfProcAck),
}

#[async_trait]
impl<S> PsrpTransport for BlockingIoPsrpTransport<S>
where
    S: Read + Write + Send + 'static,
{
    async fn send_fragment(&self, bytes: &[u8]) -> Result<()> {
        match self.framing {
            BlockingIoFraming::Raw => write_out_of_proc_control(&self.stream, bytes.to_vec()).await,
            BlockingIoFraming::PowerShellOutOfProc => {
                self.send_out_of_proc_data(uuid::Uuid::nil(), bytes).await
            }
        }
    }

    async fn send_pipeline_fragment(&self, pipeline_id: uuid::Uuid, bytes: &[u8]) -> Result<()> {
        match self.framing {
            BlockingIoFraming::Raw => write_out_of_proc_control(&self.stream, bytes.to_vec()).await,
            BlockingIoFraming::PowerShellOutOfProc => {
                self.send_out_of_proc_data(pipeline_id, bytes).await
            }
        }
    }

    async fn recv_chunk(&mut self) -> Result<Vec<u8>> {
        if self.framing == BlockingIoFraming::Raw {
            return self.read_chunk().await;
        }
        let mut inbound = self.inbound.lock().await;
        if inbound.failed || inbound.in_flight {
            return Err(PsrpError::protocol(
                "PSRP SSH acknowledgement state invalidated",
            ));
        }
        if let Some(data) = inbound.take_data() {
            return Ok(data);
        }
        match self.next_out_of_proc_packet(&mut inbound).await {
            Ok(OutOfProcPacket::Data(data)) => Ok(data),
            Ok(OutOfProcPacket::Ack(_)) => {
                inbound.failed = true;
                Err(PsrpError::protocol("unexpected PSRP SSH acknowledgement"))
            }
            Err(error) => {
                inbound.failed = true;
                Err(error)
            }
        }
    }

    async fn signal_stop(&self, pipeline_id: uuid::Uuid) -> Result<StopAcknowledgement> {
        if self.framing == BlockingIoFraming::PowerShellOutOfProc {
            self.transact(
                out_of_proc_empty("Signal", pipeline_id),
                OutOfProcAck::new(OutOfProcAckKind::Signal, pipeline_id),
            )
            .await?;
        }
        Ok(StopAcknowledgement::AwaitPipelineState)
    }

    async fn close_pipeline(&mut self, pipeline_id: uuid::Uuid) -> Result<()> {
        if self.framing == BlockingIoFraming::PowerShellOutOfProc {
            self.transact(
                out_of_proc_empty("Close", pipeline_id),
                OutOfProcAck::new(OutOfProcAckKind::Close, pipeline_id),
            )
            .await?;
        }
        Ok(())
    }

    async fn close_shell(&mut self) -> Result<()> {
        let close_result = if self.framing == BlockingIoFraming::PowerShellOutOfProc {
            let id = uuid::Uuid::nil();
            self.transact(
                out_of_proc_empty("Close", id),
                OutOfProcAck::new(OutOfProcAckKind::Close, id),
            )
            .await
        } else {
            Ok(())
        };
        let stream = Arc::clone(&self.stream);
        let dispose_result = tokio::task::spawn_blocking(move || {
            let mut guard = stream
                .lock()
                .map_err(|_| PsrpError::protocol("PSRP SSH stream lock poisoned"))?;
            if let Some(mut stream) = guard.take() {
                stream.flush().map_err(|error| {
                    PsrpError::protocol(format!("PSRP SSH stream close flush: {error}"))
                })?;
            }
            Ok(())
        })
        .await
        .map_err(|error| PsrpError::protocol(format!("PSRP SSH close worker failed: {error}")))?;
        close_result.and(dispose_result)
    }

    async fn execute_pipeline(
        &mut self,
        fragment_bytes: &[u8],
        pipeline_id: uuid::Uuid,
    ) -> Result<()> {
        if self.framing == BlockingIoFraming::PowerShellOutOfProc {
            self.transact(
                out_of_proc_empty("Command", pipeline_id),
                OutOfProcAck::new(OutOfProcAckKind::Command, pipeline_id),
            )
            .await?;
            self.send_pipeline_fragment(pipeline_id, fragment_bytes)
                .await
        } else {
            self.send_fragment(fragment_bytes).await
        }
    }
}

fn out_of_proc_data(ps_guid: uuid::Uuid, bytes: &[u8]) -> Vec<u8> {
    format!(
        "<Data Stream='Default' PSGuid='{ps_guid}'>{}</Data>\n",
        crate::clixml::encode::base64_encode(bytes)
    )
    .into_bytes()
}

fn out_of_proc_empty(element: &str, ps_guid: uuid::Uuid) -> Vec<u8> {
    format!("<{element} PSGuid='{ps_guid}' />\n").into_bytes()
}

async fn write_out_of_proc_control<S>(stream: &Arc<Mutex<Option<S>>>, bytes: Vec<u8>) -> Result<()>
where
    S: Write + Send + 'static,
{
    let stream = Arc::clone(stream);
    tokio::task::spawn_blocking(move || {
        let mut guard = stream
            .lock()
            .map_err(|_| PsrpError::protocol("PSRP SSH stream lock poisoned"))?;
        let stream = guard
            .as_mut()
            .ok_or_else(|| PsrpError::protocol("PSRP SSH stream closed"))?;
        stream
            .write_all(&bytes)
            .map_err(|error| PsrpError::protocol(format!("PSRP SSH stream write: {error}")))?;
        stream
            .flush()
            .map_err(|error| PsrpError::protocol(format!("PSRP SSH stream flush: {error}")))?;
        Ok(())
    })
    .await
    .map_err(|error| PsrpError::protocol(format!("PSRP SSH write worker failed: {error}")))?
}

fn take_next_out_of_proc_packet(buffer: &mut Vec<u8>) -> Result<Option<OutOfProcPacket>> {
    let Some(start) = buffer.iter().position(|byte| *byte == b'<') else {
        check_out_of_proc_envelope_size(buffer.len())?;
        buffer.clear();
        return Ok(None);
    };
    if start > 0 {
        buffer.drain(..start);
    }
    let Some(tag_end) = buffer.iter().position(|byte| *byte == b'>') else {
        check_out_of_proc_envelope_size(buffer.len())?;
        return Ok(None);
    };
    check_out_of_proc_envelope_size(tag_end + 1)?;
    let start_tag = std::str::from_utf8(&buffer[..=tag_end])
        .map_err(|error| PsrpError::protocol(format!("PSRP SSH XML tag utf8: {error}")))?;
    if start_tag.starts_with("<Data ") || start_tag.starts_with("<Data>") {
        let closing = b"</Data>";
        let content_start = tag_end + 1;
        let Some(offset) = find_bytes(&buffer[content_start..], closing) else {
            check_out_of_proc_envelope_size(buffer.len())?;
            return Ok(None);
        };
        let close_start = content_start + offset;
        let end = close_start + closing.len();
        check_out_of_proc_envelope_size(end)?;
        let content = std::str::from_utf8(&buffer[content_start..close_start])
            .map_err(|error| PsrpError::protocol(format!("PSRP SSH Data payload utf8: {error}")))?;
        let decoded = decode_out_of_proc_base64(content)
            .ok_or_else(|| PsrpError::protocol("PSRP SSH Data payload is not base64"))?;
        buffer.drain(..end);
        return Ok(Some(OutOfProcPacket::Data(decoded)));
    }
    if start_tag.ends_with("/>") {
        let ack = parse_out_of_proc_ack(start_tag)?;
        buffer.drain(..=tag_end);
        return Ok(Some(OutOfProcPacket::Ack(ack)));
    }
    Err(PsrpError::protocol("unexpected PSRP SSH control envelope"))
}

fn decode_out_of_proc_base64(content: &str) -> Option<Vec<u8>> {
    let mut length = 0usize;
    let mut padding = 0usize;
    let mut last_value = 0u8;
    for byte in content.bytes() {
        match byte {
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            b'=' => {
                padding += 1;
                if padding > 2 {
                    return None;
                }
            }
            _ => {
                if padding != 0 {
                    return None;
                }
                last_value = match byte {
                    b'A'..=b'Z' => byte - b'A',
                    b'a'..=b'z' => byte - b'a' + 26,
                    b'0'..=b'9' => byte - b'0' + 52,
                    b'+' => 62,
                    b'/' => 63,
                    _ => return None,
                };
            }
        }
        length += 1;
    }
    if length % 4 != 0
        || (padding == 1 && last_value & 0b11 != 0)
        || (padding == 2 && last_value & 0b1111 != 0)
    {
        return None;
    }
    crate::clixml::encode::base64_decode(content)
}

fn check_out_of_proc_envelope_size(size: usize) -> Result<()> {
    if size > MAX_OUT_OF_PROC_ENVELOPE_BYTES {
        return Err(PsrpError::protocol("PSRP SSH envelope limit exceeded"));
    }
    Ok(())
}

fn parse_out_of_proc_ack(tag: &str) -> Result<OutOfProcAck> {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_str(tag);
    let element = match reader.read_event() {
        Ok(Event::Empty(element)) => element,
        _ => return Err(PsrpError::protocol("invalid PSRP SSH control envelope")),
    };
    let kind = match element.name().as_ref() {
        "CommandAck" => OutOfProcAckKind::Command,
        "DataAck" => OutOfProcAckKind::Data,
        "SignalAck" => OutOfProcAckKind::Signal,
        "CloseAck" => OutOfProcAckKind::Close,
        _ => return Err(PsrpError::protocol("unexpected PSRP SSH control envelope")),
    };
    let mut id = None;
    for attribute in element.attributes() {
        let attribute =
            attribute.map_err(|_| PsrpError::protocol("invalid PSRP SSH acknowledgement"))?;
        if attribute.key.as_ref() == "PSGuid" {
            let value = attribute.value.as_ref();
            id = Some(
                uuid::Uuid::parse_str(value)
                    .map_err(|_| PsrpError::protocol("invalid PSRP SSH acknowledgement GUID"))?,
            );
        }
    }
    Ok(OutOfProcAck::new(
        kind,
        id.ok_or_else(|| PsrpError::protocol("missing PSRP SSH acknowledgement GUID"))?,
    ))
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Transport backed by a live `winrm_rs::Shell`.
pub struct WinrmPsrpTransport<'c> {
    shell: Option<Shell<'c>>,
    command_id: String,
    done: bool,
    /// True after a pipeline command has been started via Execute.
    /// Before this, Receive/Send use the PSRP-no-commandid path.
    has_command: bool,
}

impl std::fmt::Debug for WinrmPsrpTransport<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WinrmPsrpTransport")
            .field("command_id", &self.command_id)
            .field("has_shell", &self.shell.is_some())
            .field("done", &self.done)
            .finish()
    }
}

impl<'c> WinrmPsrpTransport<'c> {
    /// Open a PSRP shell embedding the opening fragments in `<creationXml>`.
    ///
    /// `creation_fragments` are the raw bytes of the
    /// `SessionCapability + InitRunspacePool` PSRP messages, already
    /// fragment-encoded. They get base64-wrapped and embedded in the
    /// WS-Man Create Shell body.
    pub async fn open(
        client: &'c WinrmClient,
        host: &str,
        creation_fragments: &[u8],
    ) -> Result<Self> {
        Self::open_with_resource_uri(client, host, creation_fragments, RESOURCE_URI_PSRP).await
    }

    /// Open a PSRP shell against an explicit PowerShell session configuration
    /// resource URI.
    ///
    /// Behaves exactly like [`open`](Self::open) but lets the caller select a
    /// non-default endpoint (for example PowerShell 7 or a restricted/JEA
    /// configuration). The chosen `resource_uri` is retained by the returned
    /// [`Shell`] and used for every subsequent Create/Command/Receive/Signal/
    /// Delete operation; there is no fallback to the default configuration.
    pub async fn open_with_resource_uri(
        client: &'c WinrmClient,
        host: &str,
        creation_fragments: &[u8],
        resource_uri: &str,
    ) -> Result<Self> {
        let creation_b64 = crate::clixml::encode::base64_encode(creation_fragments);
        let shell = client
            .open_psrp_shell(host, &creation_b64, resource_uri)
            .await?;
        // PSRP shells do NOT use Execute Command — the shell IS the PS
        // process. Receive/Send operate directly on the shell, using
        // the shell_id as the "command_id" in the WS-Man envelope.
        let command_id = shell.shell_id().to_string();
        debug!(command_id, "PSRP transport started (no command yet)");
        Ok(Self {
            shell: Some(shell),
            command_id,
            done: false,
            has_command: false,
        })
    }

    /// Reconnect to a previously-disconnected PSRP shell.
    pub async fn reconnect(client: &'c WinrmClient, host: &str, shell_id: &str) -> Result<Self> {
        let shell = client
            .reconnect_shell(host, shell_id, RESOURCE_URI_PSRP)
            .await?;
        let command_id = shell.shell_id().to_string();
        debug!(command_id, "PSRP transport reconnected");
        Ok(Self {
            shell: Some(shell),
            command_id,
            done: false,
            has_command: false,
        })
    }

    /// Execute an empty command inside the PSRP shell to obtain a
    /// `command_id` for Send/Receive. Called by the pool after the
    /// opening handshake completes. After this, `send_fragment` and
    /// `recv_chunk` include `CommandId` in their SOAP envelopes.
    pub async fn start_pipeline_command(&mut self) -> Result<()> {
        let shell = self.shell()?;
        let cmd_id = shell.start_command("", &[]).await?;
        debug!(cmd_id, "PSRP pipeline command started");
        self.command_id = cmd_id;
        self.has_command = true;
        Ok(())
    }

    fn shell(&self) -> Result<&Shell<'c>> {
        self.shell
            .as_ref()
            .ok_or_else(|| PsrpError::protocol("transport closed"))
    }
}

#[async_trait]
impl PsrpTransport for WinrmPsrpTransport<'_> {
    async fn send_fragment(&self, bytes: &[u8]) -> Result<()> {
        self.shell()?
            .send_input(&self.command_id, bytes, false)
            .await?;
        Ok(())
    }

    async fn recv_chunk(&mut self) -> Result<Vec<u8>> {
        loop {
            let shell = self.shell()?;
            match shell.receive_next(&self.command_id).await {
                Ok(out) => {
                    if out.done {
                        self.done = true;
                    }
                    if out.stdout.is_empty() && !self.done {
                        continue;
                    }
                    return Ok(out.stdout);
                }
                Err(WinrmError::Timeout(_)) => continue,
                Err(WinrmError::Soap(SoapError::Fault { ref code, .. }))
                    if code.contains("TimedOut") =>
                {
                    // PSRP long-polling: the WinRM server returns a SOAP
                    // fault with code `w:TimedOut` when there's nothing to
                    // read yet. This is the normal long-poll cycle and MUST
                    // be retried (briefing §5 P7). Only fatal SOAP faults
                    // (e.g. shell died, access denied) should propagate.
                    debug!("PSRP receive operation timed out; retrying");
                    continue;
                }
                Err(WinrmError::Soap(SoapError::Fault {
                    code,
                    detail_code,
                    reason,
                })) => {
                    warn!("PSRP transport SOAP fault; aborting receive");
                    return Err(PsrpError::Winrm(WinrmError::Soap(SoapError::Fault {
                        code,
                        detail_code,
                        reason,
                    })));
                }
                Err(e) => return Err(PsrpError::Winrm(e)),
            }
        }
    }

    async fn execute_pipeline(
        &mut self,
        fragment_bytes: &[u8],
        pipeline_id: uuid::Uuid,
    ) -> Result<()> {
        let shell = self.shell()?;
        let b64 = crate::clixml::encode::base64_encode(fragment_bytes);
        // pypsrp sends: command("", arguments=[b64_first_frag], command_id=pipeline_id)
        // The WS-Man Execute carries the first fragment as the sole argument
        // and uses the pipeline UUID as the CommandId.
        let pipeline_command_id = pipeline_command_id(pipeline_id);
        let cmd_id = shell
            .start_command_with_id("", &[&b64], &pipeline_command_id)
            .await?;
        debug!(cmd_id, "PSRP pipeline Execute started");
        self.command_id = cmd_id;
        self.has_command = true;
        Ok(())
    }

    async fn signal_stop(&self, pipeline_id: uuid::Uuid) -> Result<StopAcknowledgement> {
        let pipeline_command_id = pipeline_command_id(pipeline_id);
        self.shell()?.signal_ctrl_c(&pipeline_command_id).await?;
        // Microsoft's OnSignalCompleted and pypsrp.stop treat this response as
        // Stopped; the command can disappear before a subsequent Receive.
        Ok(StopAcknowledgement::Stopped)
    }

    async fn close_shell(&mut self) -> Result<()> {
        if let Some(shell) = self.shell.take() {
            shell.close().await?;
        }
        Ok(())
    }

    async fn disconnect_shell(&mut self) -> Result<String> {
        let shell = self
            .shell
            .take()
            .ok_or_else(|| PsrpError::protocol("transport closed"))?;
        let id = shell.disconnect().await?;
        Ok(id)
    }
}

fn pipeline_command_id(pipeline_id: uuid::Uuid) -> String {
    pipeline_id.hyphenated().to_string().to_uppercase()
}

impl Drop for WinrmPsrpTransport<'_> {
    fn drop(&mut self) {
        if self.shell.is_some() {
            warn!("WinrmPsrpTransport dropped without close — shell leaked server-side");
        }
    }
}

#[cfg(any(test, feature = "__internal"))]
#[doc(hidden)]
pub mod mock {
    use super::{PsrpError, PsrpTransport, Result, StopAcknowledgement, async_trait};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use tokio::sync::Notify;

    /// In-memory transport used by the test suite.
    #[derive(Clone, Default, Debug)]
    pub struct MockTransport {
        pub inbox: Arc<Mutex<VecDeque<Vec<u8>>>>, // bytes to hand out of recv_chunk
        pub outbox: Arc<Mutex<Vec<Vec<u8>>>>,     // bytes captured from send_fragment
        pub pipeline_fragment_ids: Arc<Mutex<Vec<uuid::Uuid>>>,
        pub executed_pipeline_ids: Arc<Mutex<Vec<uuid::Uuid>>>,
        pub stopped: Arc<Mutex<bool>>,
        pub stopped_pipeline_ids: Arc<Mutex<Vec<uuid::Uuid>>>,
        pub closed_pipeline_ids: Arc<Mutex<Vec<uuid::Uuid>>>,
        pub fail_close_pipeline: Arc<Mutex<bool>>,
        pub close_pipeline_blocked: Arc<Mutex<bool>>,
        pub close_pipeline_notify: Arc<Notify>,
        pub stop_acknowledgement: Arc<Mutex<StopAcknowledgement>>,
        pub fail_stop: Arc<Mutex<bool>>,
        pub stop_blocked: Arc<Mutex<bool>>,
        pub stop_notify: Arc<Notify>,
        pub closed: Arc<Mutex<bool>>,
        pub fail_send: Arc<Mutex<bool>>,
        pub fail_recv: Arc<Mutex<Option<PsrpError>>>,
        pub recv_blocked: Arc<Mutex<bool>>,
        pub recv_notify: Arc<Notify>,
    }

    impl MockTransport {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn push_incoming(&self, bytes: Vec<u8>) {
            self.inbox.lock().unwrap().push_back(bytes);
        }

        pub fn sent(&self) -> Vec<Vec<u8>> {
            self.outbox.lock().unwrap().clone()
        }

        pub fn sent_pipeline_fragment_ids(&self) -> Vec<uuid::Uuid> {
            self.pipeline_fragment_ids.lock().unwrap().clone()
        }

        pub fn executed_pipeline_ids(&self) -> Vec<uuid::Uuid> {
            self.executed_pipeline_ids.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl PsrpTransport for MockTransport {
        async fn send_fragment(&self, bytes: &[u8]) -> Result<()> {
            if *self.fail_send.lock().unwrap() {
                return Err(PsrpError::protocol("mock send failure"));
            }
            self.outbox.lock().unwrap().push(bytes.to_vec());
            Ok(())
        }

        async fn send_pipeline_fragment(
            &self,
            pipeline_id: uuid::Uuid,
            bytes: &[u8],
        ) -> Result<()> {
            self.pipeline_fragment_ids.lock().unwrap().push(pipeline_id);
            self.send_fragment(bytes).await
        }

        async fn execute_pipeline(
            &mut self,
            fragment_bytes: &[u8],
            pipeline_id: uuid::Uuid,
        ) -> Result<()> {
            self.executed_pipeline_ids.lock().unwrap().push(pipeline_id);
            self.send_fragment(fragment_bytes).await
        }

        async fn recv_chunk(&mut self) -> Result<Vec<u8>> {
            if let Some(e) = self.fail_recv.lock().unwrap().take() {
                return Err(e);
            }
            if *self.recv_blocked.lock().unwrap() {
                self.recv_notify.notified().await;
                *self.recv_blocked.lock().unwrap() = false;
            }
            let mut inbox = self.inbox.lock().unwrap();
            if let Some(bytes) = inbox.pop_front() {
                Ok(bytes)
            } else {
                Err(PsrpError::protocol("mock inbox empty"))
            }
        }

        async fn signal_stop(&self, pipeline_id: uuid::Uuid) -> Result<StopAcknowledgement> {
            *self.stopped.lock().unwrap() = true;
            self.stopped_pipeline_ids.lock().unwrap().push(pipeline_id);
            if *self.fail_stop.lock().unwrap() {
                return Err(PsrpError::protocol("mock stop failed"));
            }
            let blocked = *self.stop_blocked.lock().unwrap();
            if blocked {
                self.stop_notify.notified().await;
            }
            Ok(*self.stop_acknowledgement.lock().unwrap())
        }

        async fn close_shell(&mut self) -> Result<()> {
            *self.closed.lock().unwrap() = true;
            Ok(())
        }

        async fn close_pipeline(&mut self, pipeline_id: uuid::Uuid) -> Result<()> {
            self.closed_pipeline_ids.lock().unwrap().push(pipeline_id);
            if *self.fail_close_pipeline.lock().unwrap() {
                return Err(PsrpError::protocol("mock pipeline close failure"));
            }
            let blocked = *self.close_pipeline_blocked.lock().unwrap();
            if blocked {
                self.close_pipeline_notify.notified().await;
            }
            Ok(())
        }

        async fn disconnect_shell(&mut self) -> Result<String> {
            *self.closed.lock().unwrap() = true;
            Ok("MOCK-SHELL-ID".into())
        }
    }

    #[tokio::test]
    async fn mock_roundtrip() {
        let mut t = MockTransport::new();
        t.send_fragment(b"hello").await.unwrap();
        assert_eq!(t.sent(), vec![b"hello".to_vec()]);

        t.push_incoming(b"world".to_vec());
        let got = t.recv_chunk().await.unwrap();
        assert_eq!(got, b"world");

        t.signal_stop(uuid::Uuid::new_v4()).await.unwrap();
        t.close_shell().await.unwrap();
        assert!(*t.stopped.lock().unwrap());
        assert!(*t.closed.lock().unwrap());
    }

    #[tokio::test]
    async fn mock_recv_failure() {
        let mut t = MockTransport::new();
        *t.fail_recv.lock().unwrap() = Some(PsrpError::protocol("boom"));
        assert!(t.recv_chunk().await.is_err());
    }

    #[tokio::test]
    async fn mock_recv_can_block_until_notified() {
        let t = MockTransport::new();
        *t.recv_blocked.lock().unwrap() = true;
        t.push_incoming(b"later".to_vec());

        let cloned = t.clone();
        let task = tokio::spawn(async move {
            let mut transport = cloned;
            transport.recv_chunk().await.unwrap()
        });

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        t.recv_notify.notify_waiters();

        assert_eq!(task.await.unwrap(), b"later".to_vec());
    }

    #[tokio::test]
    async fn mock_send_failure() {
        let t = MockTransport::new();
        *t.fail_send.lock().unwrap() = true;
        assert!(t.send_fragment(b"x").await.is_err());
    }

    #[test]
    fn pipeline_command_id_is_uppercase_uuid() {
        let id = uuid::Uuid::parse_str("2bfdf32c-97f5-4a3a-aa8c-b0d968b8ee4a").unwrap();
        assert_eq!(
            super::pipeline_command_id(id),
            "2BFDF32C-97F5-4A3A-AA8C-B0D968B8EE4A"
        );
    }
}

#[cfg(test)]
mod blocking_io_tests {
    use super::{
        BlockingIoPsrpTransport, MAX_OUT_OF_PROC_DATA_BYTES, MAX_PENDING_OUT_OF_PROC_BYTES,
        MAX_PENDING_OUT_OF_PROC_PACKETS, PsrpError, PsrpTransport, StopAcknowledgement,
        out_of_proc_data, out_of_proc_empty,
    };
    use std::{
        collections::VecDeque,
        io::{self, Read, Write},
        sync::{
            Arc, Condvar, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    #[derive(Clone, Debug, Default)]
    struct MemoryStream {
        inbound: Arc<(Mutex<VecDeque<Vec<u8>>>, Condvar)>,
        outbound: Arc<Mutex<Vec<Vec<u8>>>>,
        closed: Arc<Mutex<bool>>,
        read_starts: Arc<AtomicUsize>,
    }

    impl MemoryStream {
        fn push_inbound(&self, bytes: Vec<u8>) {
            let (lock, ready) = &*self.inbound;
            lock.lock().unwrap().push_back(bytes);
            ready.notify_all();
        }

        fn outbound(&self) -> Vec<Vec<u8>> {
            self.outbound.lock().unwrap().clone()
        }

        fn finish_reads(&self) {
            let (lock, ready) = &*self.inbound;
            let _guard = lock.lock().unwrap();
            *self.closed.lock().unwrap() = true;
            ready.notify_all();
        }
    }

    impl Read for MemoryStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.read_starts.fetch_add(1, Ordering::SeqCst);
            let (lock, ready) = &*self.inbound;
            let mut inbound = lock.lock().unwrap();
            loop {
                if let Some(mut bytes) = inbound.pop_front() {
                    let count = buffer.len().min(bytes.len());
                    buffer[..count].copy_from_slice(&bytes[..count]);
                    if count < bytes.len() {
                        bytes.drain(..count);
                        inbound.push_front(bytes);
                    }
                    return Ok(count);
                }
                if *self.closed.lock().unwrap() {
                    return Ok(0);
                }
                let (next, timeout) = ready.wait_timeout(inbound, Duration::from_secs(1)).unwrap();
                inbound = next;
                if timeout.timed_out() && inbound.is_empty() && !*self.closed.lock().unwrap() {
                    return Err(io::Error::from(io::ErrorKind::TimedOut));
                }
            }
        }
    }

    impl Write for MemoryStream {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.outbound.lock().unwrap().push(buffer.to_vec());
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    async fn wait_for_reads(peer: &MemoryStream, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while peer.read_starts.load(Ordering::SeqCst) < count {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("transport did not start the expected read");
    }

    async fn wait_for_writes(peer: &MemoryStream, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while peer.outbound.lock().unwrap().len() < count {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("transport did not write the expected frame");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn blocking_io_transport_writes_raw_fragments_and_reassembles_bounded_reads() {
        let stream = MemoryStream::default();
        let peer = stream.clone();
        let mut transport = BlockingIoPsrpTransport::with_read_chunk_bytes(stream, 4);

        transport.send_fragment(b"fragment").await.unwrap();
        assert_eq!(peer.outbound(), vec![b"fragment".to_vec()]);

        peer.push_inbound(b"abcdef".to_vec());
        assert_eq!(transport.recv_chunk().await.unwrap(), b"abcd".to_vec());
        assert_eq!(transport.recv_chunk().await.unwrap(), b"ef".to_vec());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn blocking_io_transport_closes_without_leaving_a_readable_stream() {
        let stream = MemoryStream::default();
        let mut transport = BlockingIoPsrpTransport::new(stream);
        transport.close_shell().await.unwrap();

        let error = transport.recv_chunk().await.unwrap_err().to_string();
        assert!(error.contains("closed"), "{error}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn powershell_ssh_framing_routes_pipeline_fragments_to_the_pipeline_guid() {
        let stream = MemoryStream::default();
        let peer = stream.clone();
        let transport = BlockingIoPsrpTransport::powershell_ssh(stream);
        let pipeline_id = uuid::Uuid::parse_str("11111111-2222-4333-8444-555555555555").unwrap();

        for id in [uuid::Uuid::nil(), pipeline_id, uuid::Uuid::nil()] {
            peer.push_inbound(out_of_proc_empty("DataAck", id));
        }
        transport.send_fragment(b"runspace").await.unwrap();
        transport
            .send_pipeline_fragment(pipeline_id, b"pipeline-input")
            .await
            .unwrap();
        transport.send_fragment(b"runspace-after").await.unwrap();

        let outbound = peer
            .outbound()
            .into_iter()
            .map(|bytes| String::from_utf8(bytes).unwrap())
            .collect::<Vec<_>>();
        assert!(
            outbound[0].contains("PSGuid='00000000-0000-0000-0000-000000000000'"),
            "{outbound:?}"
        );
        assert!(outbound[0].contains("cnVuc3BhY2U="), "{outbound:?}");
        assert!(
            outbound[1].contains("PSGuid='11111111-2222-4333-8444-555555555555'"),
            "{outbound:?}"
        );
        assert!(outbound[1].contains("cGlwZWxpbmUtaW5wdXQ="), "{outbound:?}");
        assert!(
            outbound[2].contains("PSGuid='00000000-0000-0000-0000-000000000000'"),
            "{outbound:?}"
        );
        assert!(outbound[2].contains("cnVuc3BhY2UtYWZ0ZXI="), "{outbound:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn powershell_ssh_waits_for_command_and_each_fragment_ack() {
        let stream = MemoryStream::default();
        let peer = stream.clone();
        let mut transport = BlockingIoPsrpTransport::powershell_ssh(stream);
        let id = uuid::Uuid::new_v4();
        let task = tokio::spawn(async move {
            transport.execute_pipeline(b"first", id).await?;
            transport.send_pipeline_fragment(id, b"second").await
        });

        wait_for_writes(&peer, 1).await;
        wait_for_reads(&peer, 2).await;
        assert_eq!(peer.outbound(), vec![out_of_proc_empty("Command", id)]);
        peer.push_inbound(out_of_proc_empty("CommandAck", id));

        wait_for_writes(&peer, 2).await;
        assert_eq!(
            peer.outbound(),
            vec![
                out_of_proc_empty("Command", id),
                out_of_proc_data(id, b"first")
            ]
        );
        peer.push_inbound(out_of_proc_empty("DataAck", id));

        wait_for_writes(&peer, 3).await;
        assert_eq!(
            peer.outbound(),
            vec![
                out_of_proc_empty("Command", id),
                out_of_proc_data(id, b"first"),
                out_of_proc_data(id, b"second")
            ]
        );
        peer.push_inbound(out_of_proc_empty("DataAck", id));
        task.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn powershell_ssh_bounds_large_data_packets_and_waits_between_them() {
        let stream = MemoryStream::default();
        let peer = stream.clone();
        let transport = BlockingIoPsrpTransport::powershell_ssh(stream);
        let id = uuid::Uuid::new_v4();
        let bytes = vec![b'x'; MAX_OUT_OF_PROC_DATA_BYTES + 7];
        let first = out_of_proc_data(id, &bytes[..MAX_OUT_OF_PROC_DATA_BYTES]);
        let second = out_of_proc_data(id, &bytes[MAX_OUT_OF_PROC_DATA_BYTES..]);
        let task = tokio::spawn(async move { transport.send_pipeline_fragment(id, &bytes).await });

        wait_for_writes(&peer, 1).await;
        assert_eq!(peer.outbound(), vec![first.clone()]);
        peer.push_inbound(out_of_proc_empty("DataAck", id));
        wait_for_writes(&peer, 2).await;
        assert_eq!(peer.outbound(), vec![first, second]);
        peer.push_inbound(out_of_proc_empty("DataAck", id));
        task.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn powershell_ssh_preserves_data_while_waiting_for_signal_and_close_acks() {
        let stream = MemoryStream::default();
        let peer = stream.clone();
        let mut transport = BlockingIoPsrpTransport::powershell_ssh(stream);
        let id = uuid::Uuid::new_v4();

        let mut initial = out_of_proc_data(id, b"first");
        initial.extend(out_of_proc_data(id, b"second"));
        initial.extend(out_of_proc_empty("DataAck", uuid::Uuid::nil()));
        peer.push_inbound(initial);
        transport.send_fragment(b"request").await.unwrap();
        assert_eq!(transport.recv_chunk().await.unwrap(), b"first");
        assert_eq!(transport.recv_chunk().await.unwrap(), b"second");

        {
            let signal = transport.signal_stop(id);
            tokio::pin!(signal);
            tokio::select! {
                result = &mut signal => panic!("signal completed without its acknowledgement: {result:?}"),
                () = wait_for_writes(&peer, 2) => {}
            }
            assert_eq!(peer.outbound()[1], out_of_proc_empty("Signal", id));
            let mut incoming = out_of_proc_data(id, b"during-signal");
            incoming.extend(out_of_proc_empty("SignalAck", id));
            peer.push_inbound(incoming);
            assert_eq!(
                signal.await.unwrap(),
                StopAcknowledgement::AwaitPipelineState
            );
        }
        assert_eq!(transport.recv_chunk().await.unwrap(), b"during-signal");

        {
            let close = transport.close_pipeline(id);
            tokio::pin!(close);
            tokio::select! {
                result = &mut close => panic!("close completed without its acknowledgement: {result:?}"),
                () = wait_for_writes(&peer, 3) => {}
            }
            assert_eq!(peer.outbound()[2], out_of_proc_empty("Close", id));
            peer.push_inbound(out_of_proc_empty("CloseAck", id));
            close.await.unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn powershell_ssh_rejects_malformed_base64_and_invalidates_stream() {
        for payload in ["QQ==garbage", "QQ=", "QQ===", "Q", "AB==", "AA=A"] {
            let stream = MemoryStream::default();
            let peer = stream.clone();
            let mut transport = BlockingIoPsrpTransport::powershell_ssh(stream);
            let frame = format!("<Data PSGuid='{}'>{payload}</Data>\n", uuid::Uuid::nil());
            let split = frame.len() / 2;
            peer.push_inbound(frame.as_bytes()[..split].to_vec());
            peer.push_inbound(frame.as_bytes()[split..].to_vec());
            assert!(matches!(
                transport.recv_chunk().await,
                Err(PsrpError::Protocol(_))
            ));
            peer.push_inbound(out_of_proc_empty("DataAck", uuid::Uuid::nil()));
            assert!(matches!(
                transport.send_fragment(b"later").await,
                Err(PsrpError::Protocol(_))
            ));
            assert!(peer.outbound().is_empty());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn powershell_ssh_bounds_data_queued_before_acknowledgement() {
        for (payload, limit) in [
            (vec![b'x'], MAX_PENDING_OUT_OF_PROC_PACKETS),
            (
                vec![b'x'; 512 * 1024],
                MAX_PENDING_OUT_OF_PROC_BYTES / (512 * 1024),
            ),
        ] {
            for over_limit in [false, true] {
                let stream = MemoryStream::default();
                let peer = stream.clone();
                let mut transport = BlockingIoPsrpTransport::with_powershell_ssh_read_chunk_bytes(
                    stream,
                    768 * 1024,
                );
                let frame = out_of_proc_data(uuid::Uuid::nil(), &payload);
                for _ in 0..limit + usize::from(over_limit) {
                    peer.push_inbound(frame.clone());
                }
                peer.push_inbound(out_of_proc_empty("DataAck", uuid::Uuid::nil()));
                if over_limit {
                    assert!(matches!(
                        transport.send_fragment(b"first").await,
                        Err(PsrpError::Protocol(_))
                    ));
                    assert!(matches!(
                        transport.send_fragment(b"later").await,
                        Err(PsrpError::Protocol(_))
                    ));
                    assert_eq!(peer.outbound().len(), 1);
                } else {
                    transport.send_fragment(b"first").await.unwrap();
                    for _ in 0..limit {
                        assert_eq!(transport.recv_chunk().await.unwrap(), payload);
                    }
                }
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn powershell_ssh_rejects_mismatched_and_stale_acknowledgements() {
        for acknowledgement in [
            out_of_proc_empty("DataAck", uuid::Uuid::new_v4()),
            out_of_proc_empty("CommandAck", uuid::Uuid::nil()),
            b"<DataAck />\n".to_vec(),
            b"<Unexpected />\n".to_vec(),
            b"<Unexpected>ignored</Unexpected>\n".to_vec(),
        ] {
            let stream = MemoryStream::default();
            let peer = stream.clone();
            let transport = BlockingIoPsrpTransport::powershell_ssh(stream);
            peer.push_inbound(acknowledgement);
            assert!(matches!(
                transport.send_fragment(b"first").await,
                Err(PsrpError::Protocol(_))
            ));
            assert!(matches!(
                transport.send_fragment(b"second").await,
                Err(PsrpError::Protocol(_))
            ));
            assert_eq!(peer.outbound().len(), 1);
        }

        let stream = MemoryStream::default();
        let peer = stream.clone();
        let transport = BlockingIoPsrpTransport::powershell_ssh(stream);
        let mut acknowledgements = out_of_proc_empty("DataAck", uuid::Uuid::nil());
        acknowledgements.extend(out_of_proc_empty("DataAck", uuid::Uuid::nil()));
        peer.push_inbound(acknowledgements);
        transport.send_fragment(b"first").await.unwrap();
        let error = transport.send_fragment(b"second").await.unwrap_err();
        assert!(error.to_string().contains("stale"), "{error}");
        assert_eq!(peer.outbound().len(), 1);

        let stream = MemoryStream::default();
        let peer = stream.clone();
        let transport = BlockingIoPsrpTransport::powershell_ssh(stream);
        let mut first_read = out_of_proc_empty("DataAck", uuid::Uuid::nil());
        first_read.extend_from_slice(b"<DataAck PSG");
        peer.push_inbound(first_read);
        peer.push_inbound(b"uid='00000000-0000-0000-0000-000000000000' />\n".to_vec());
        transport.send_fragment(b"first").await.unwrap();
        let error = transport.send_fragment(b"second").await.unwrap_err();
        assert!(error.to_string().contains("stale"), "{error}");
        assert_eq!(peer.outbound().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn powershell_ssh_ack_timeout_invalidates_and_still_closes_stream() {
        let stream = MemoryStream::default();
        let peer = stream.clone();
        let mut transport = BlockingIoPsrpTransport::powershell_ssh_with_ack_timeout(
            stream,
            Duration::from_millis(30),
        );
        assert!(matches!(
            transport.send_fragment(b"request").await,
            Err(PsrpError::AckTimeout)
        ));
        peer.finish_reads();
        assert!(matches!(
            transport.send_fragment(b"later").await,
            Err(PsrpError::Protocol(_))
        ));
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), transport.close_shell())
                .await
                .unwrap(),
            Err(PsrpError::Protocol(_))
        ));
        assert!(transport.stream.lock().unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn powershell_ssh_close_ack_timeout_reports_error_and_disposes_stream() {
        let stream = MemoryStream::default();
        let peer = stream.clone();
        let mut transport = BlockingIoPsrpTransport::powershell_ssh_with_ack_timeout(
            stream,
            Duration::from_millis(300),
        );
        let release = tokio::spawn(async move {
            wait_for_reads(&peer, 1).await;
            tokio::time::sleep(Duration::from_millis(500)).await;
            peer.finish_reads();
        });
        let result = tokio::time::timeout(Duration::from_secs(5), transport.close_shell())
            .await
            .expect("close did not dispose the stream");
        assert!(matches!(result, Err(PsrpError::AckTimeout)));
        assert!(transport.stream.lock().unwrap().is_none());
        release.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn powershell_ssh_cancelled_transaction_cannot_reuse_late_ack() {
        let stream = MemoryStream::default();
        let peer = stream.clone();
        let mut transport = BlockingIoPsrpTransport::powershell_ssh(stream);
        {
            let send = transport.send_fragment(b"first");
            tokio::pin!(send);
            tokio::select! {
                result = &mut send => panic!("send completed without an acknowledgement: {result:?}"),
                () = wait_for_reads(&peer, 1) => {}
            }
        }
        peer.push_inbound(out_of_proc_empty("DataAck", uuid::Uuid::nil()));
        assert!(matches!(
            transport.send_fragment(b"second").await,
            Err(PsrpError::Protocol(_))
        ));
        assert_eq!(peer.outbound().len(), 1);
        peer.finish_reads();
        assert!(transport.close_shell().await.is_err());
        assert!(transport.stream.lock().unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_blocking_read_is_reused_and_preserves_its_bytes() {
        let stream = MemoryStream::default();
        let peer = stream.clone();
        let mut transport = BlockingIoPsrpTransport::new(stream);
        {
            let read = transport.recv_chunk();
            tokio::pin!(read);
            tokio::select! {
                result = &mut read => panic!("read completed without data: {result:?}"),
                () = wait_for_reads(&peer, 1) => {}
            }
        }
        {
            let read = transport.recv_chunk();
            tokio::pin!(read);
            tokio::select! {
                result = &mut read => panic!("read completed without data: {result:?}"),
                () = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
            assert_eq!(peer.read_starts.load(Ordering::SeqCst), 1);
            peer.push_inbound(b"preserved".to_vec());
            let received = tokio::time::timeout(Duration::from_secs(2), read).await;
            if received.is_err() {
                peer.finish_reads();
            }
            assert_eq!(received.unwrap().unwrap(), b"preserved");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn powershell_ssh_envelope_bound_applies_to_each_coalesced_frame() {
        let stream = MemoryStream::default();
        let peer = stream.clone();
        let mut transport =
            BlockingIoPsrpTransport::with_powershell_ssh_read_chunk_bytes(stream, 2 * 1024 * 1024);
        let first = vec![b'a'; 600_000];
        let second = vec![b'b'; 600_000];
        let mut incoming = out_of_proc_data(uuid::Uuid::nil(), &first);
        incoming.extend(out_of_proc_data(uuid::Uuid::nil(), &second));
        peer.push_inbound(incoming);
        assert_eq!(transport.recv_chunk().await.unwrap(), first);
        assert_eq!(transport.recv_chunk().await.unwrap(), second);

        let oversized = out_of_proc_data(uuid::Uuid::nil(), &vec![b'x'; 800_000]);
        peer.push_inbound(oversized);
        let error = transport.recv_chunk().await.unwrap_err();
        assert!(error.to_string().contains("envelope limit"), "{error}");
    }
}
