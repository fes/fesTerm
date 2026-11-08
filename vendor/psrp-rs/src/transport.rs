//! Thin wrapper around [`winrm_rs::Shell`] that ferries PSRP fragments.
//!
//! This module is the **only** place in the crate that imports
//! `winrm_rs::Shell`. Every higher-level component (runspace pool,
//! pipeline) talks to the transport through the [`PsrpTransport`] trait so
//! it can be mocked in tests without standing up a fake SOAP server.

use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
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
#[derive(Debug)]
pub struct BlockingIoPsrpTransport<S> {
    stream: Arc<Mutex<Option<S>>>,
    read_chunk_bytes: usize,
    framing: BlockingIoFraming,
    inbound_buffer: Vec<u8>,
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
            read_chunk_bytes: read_chunk_bytes.max(1),
            framing: BlockingIoFraming::Raw,
            inbound_buffer: Vec::new(),
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

    /// Like [`powershell_ssh`](Self::powershell_ssh), with an explicit
    /// transport-local read chunk bound.
    pub fn with_powershell_ssh_read_chunk_bytes(stream: S, read_chunk_bytes: usize) -> Self {
        Self {
            stream: Arc::new(Mutex::new(Some(stream))),
            read_chunk_bytes: read_chunk_bytes.max(1),
            framing: BlockingIoFraming::PowerShellOutOfProc,
            inbound_buffer: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockingIoFraming {
    Raw,
    PowerShellOutOfProc,
}

#[async_trait]
impl<S> PsrpTransport for BlockingIoPsrpTransport<S>
where
    S: Read + Write + Send + 'static,
{
    async fn send_fragment(&self, bytes: &[u8]) -> Result<()> {
        let bytes = match self.framing {
            BlockingIoFraming::Raw => bytes.to_vec(),
            BlockingIoFraming::PowerShellOutOfProc => out_of_proc_data(uuid::Uuid::nil(), bytes),
        };
        write_out_of_proc_control(&self.stream, bytes).await
    }

    async fn send_pipeline_fragment(&self, pipeline_id: uuid::Uuid, bytes: &[u8]) -> Result<()> {
        let bytes = match self.framing {
            BlockingIoFraming::Raw => bytes.to_vec(),
            BlockingIoFraming::PowerShellOutOfProc => out_of_proc_data(pipeline_id, bytes),
        };
        write_out_of_proc_control(&self.stream, bytes).await
    }

    async fn recv_chunk(&mut self) -> Result<Vec<u8>> {
        loop {
            if self.framing == BlockingIoFraming::PowerShellOutOfProc {
                if let Some(bytes) = take_next_out_of_proc_data(&mut self.inbound_buffer)? {
                    return Ok(bytes);
                }
            }
            let stream = Arc::clone(&self.stream);
            let limit = self.read_chunk_bytes;
            let bytes = match tokio::task::spawn_blocking(move || {
                let mut buffer = vec![0; limit];
                let mut guard = stream
                    .lock()
                    .map_err(|_| PsrpError::protocol("PSRP SSH stream lock poisoned"))?;
                let stream = guard
                    .as_mut()
                    .ok_or_else(|| PsrpError::protocol("PSRP SSH stream closed"))?;
                match stream.read(&mut buffer) {
                    Ok(0) => Err(PsrpError::protocol("PSRP SSH stream closed by peer")),
                    Ok(read) => {
                        buffer.truncate(read);
                        Ok(buffer)
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::TimedOut
                                | std::io::ErrorKind::WouldBlock
                                | std::io::ErrorKind::Interrupted
                        ) =>
                    {
                        Ok(Vec::new())
                    }
                    Err(error) => Err(PsrpError::protocol(format!(
                        "PSRP SSH stream read: {error}"
                    ))),
                }
            })
            .await
            .map_err(|error| PsrpError::protocol(format!("PSRP SSH read worker failed: {error}")))?
            {
                Ok(bytes) if bytes.is_empty() => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    continue;
                }
                result => result?,
            };
            if self.framing == BlockingIoFraming::Raw {
                return Ok(bytes);
            }
            self.inbound_buffer.extend_from_slice(&bytes);
        }
    }

    async fn signal_stop(&self, pipeline_id: uuid::Uuid) -> Result<StopAcknowledgement> {
        if self.framing == BlockingIoFraming::PowerShellOutOfProc {
            write_out_of_proc_control(&self.stream, out_of_proc_empty("Signal", pipeline_id))
                .await?;
        }
        Ok(StopAcknowledgement::AwaitPipelineState)
    }

    async fn close_shell(&mut self) -> Result<()> {
        if self.framing == BlockingIoFraming::PowerShellOutOfProc {
            let _ = write_out_of_proc_control(
                &self.stream,
                out_of_proc_empty("Close", uuid::Uuid::nil()),
            )
            .await;
        }
        let stream = Arc::clone(&self.stream);
        tokio::task::spawn_blocking(move || {
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
        .map_err(|error| PsrpError::protocol(format!("PSRP SSH close worker failed: {error}")))?
    }

    async fn execute_pipeline(
        &mut self,
        fragment_bytes: &[u8],
        pipeline_id: uuid::Uuid,
    ) -> Result<()> {
        if self.framing == BlockingIoFraming::PowerShellOutOfProc {
            write_out_of_proc_control(&self.stream, out_of_proc_empty("Command", pipeline_id))
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

fn take_next_out_of_proc_data(buffer: &mut Vec<u8>) -> Result<Option<Vec<u8>>> {
    loop {
        let Some(start) = buffer.iter().position(|byte| *byte == b'<') else {
            buffer.clear();
            return Ok(None);
        };
        if start > 0 {
            buffer.drain(..start);
        }
        let Some(tag_end) = buffer.iter().position(|byte| *byte == b'>') else {
            return Ok(None);
        };
        let start_tag = std::str::from_utf8(&buffer[..=tag_end])
            .map_err(|error| PsrpError::protocol(format!("PSRP SSH XML tag utf8: {error}")))?;
        if start_tag.starts_with("<Data ") || start_tag.starts_with("<Data>") {
            let closing = b"</Data>";
            let Some(close_start) = find_bytes(buffer, closing) else {
                return Ok(None);
            };
            let content =
                std::str::from_utf8(&buffer[tag_end + 1..close_start]).map_err(|error| {
                    PsrpError::protocol(format!("PSRP SSH Data payload utf8: {error}"))
                })?;
            let end = close_start + closing.len();
            let decoded = crate::clixml::encode::base64_decode(content.trim())
                .ok_or_else(|| PsrpError::protocol("PSRP SSH Data payload is not base64"))?;
            buffer.drain(..end);
            return Ok(Some(decoded));
        }
        if start_tag.ends_with("/>") {
            buffer.drain(..=tag_end);
            continue;
        }
        let name_end = start_tag[1..]
            .find(|ch: char| ch.is_ascii_whitespace() || ch == '>')
            .map(|idx| idx + 1)
            .unwrap_or(start_tag.len() - 1);
        let name = &start_tag[1..name_end];
        let closing = format!("</{name}>");
        let Some(close_start) = find_bytes(buffer, closing.as_bytes()) else {
            return Ok(None);
        };
        buffer.drain(..close_start + closing.len());
    }
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
        pub stop_acknowledgement: Arc<Mutex<StopAcknowledgement>>,
        pub fail_stop: Arc<Mutex<bool>>,
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
            Ok(*self.stop_acknowledgement.lock().unwrap())
        }

        async fn close_shell(&mut self) -> Result<()> {
            *self.closed.lock().unwrap() = true;
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
    use super::{BlockingIoPsrpTransport, PsrpTransport};
    use std::{
        collections::VecDeque,
        io::{self, Read, Write},
        sync::{Arc, Condvar, Mutex},
        time::Duration,
    };

    #[derive(Clone, Debug, Default)]
    struct MemoryStream {
        inbound: Arc<(Mutex<VecDeque<Vec<u8>>>, Condvar)>,
        outbound: Arc<Mutex<Vec<Vec<u8>>>>,
        closed: Arc<Mutex<bool>>,
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
    }

    impl Read for MemoryStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
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
                inbound = ready
                    .wait_timeout(inbound, Duration::from_secs(1))
                    .unwrap()
                    .0;
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
}
