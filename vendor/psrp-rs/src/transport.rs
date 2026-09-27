//! Thin wrapper around [`winrm_rs::Shell`] that ferries PSRP fragments.
//!
//! This module is the **only** place in the crate that imports
//! `winrm_rs::Shell`. Every higher-level component (runspace pool,
//! pipeline) talks to the transport through the [`PsrpTransport`] trait so
//! it can be mocked in tests without standing up a fake SOAP server.

use async_trait::async_trait;
use tracing::{debug, warn};
use winrm_rs::{RESOURCE_URI_PSRP, Shell, SoapError, WinrmClient, WinrmError};

use crate::error::{PsrpError, Result};

/// Abstract transport used by the runspace pool and pipeline.
///
/// `send_fragment` MUST write the pre-encoded fragment bytes as a single
/// `send_input` call. `recv_chunk` MUST transparently retry on
/// `WinrmError::Timeout` (long-polling is expected) but propagate every
/// other error — in particular SOAP faults, which usually mean the
/// server-side shell has died and the pool must be recreated.
#[async_trait]
pub trait PsrpTransport: Send {
    async fn send_fragment(&self, bytes: &[u8]) -> Result<()>;
    async fn recv_chunk(&mut self) -> Result<Vec<u8>>;
    async fn signal_stop(&self) -> Result<()>;
    async fn close_shell(&mut self) -> Result<()>;

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
        let creation_b64 = crate::clixml::encode::base64_encode(creation_fragments);
        let shell = client
            .open_psrp_shell(host, &creation_b64, RESOURCE_URI_PSRP)
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
                Err(WinrmError::Timeout(_)) => {
                    emit_psrp_cancel_diagnostic(
                        "receive-client-timeout",
                        shell,
                        Some(&self.command_id),
                        None,
                        None,
                    );
                    continue;
                }
                Err(WinrmError::Soap(SoapError::Fault {
                    ref code,
                    detail_code,
                    ..
                })) if code.contains("TimedOut") => {
                    // PSRP long-polling: the WinRM server returns a SOAP
                    // fault with code `w:TimedOut` when there's nothing to
                    // read yet. This is the normal long-poll cycle and MUST
                    // be retried (briefing §5 P7). Only fatal SOAP faults
                    // (e.g. shell died, access denied) should propagate.
                    emit_psrp_cancel_diagnostic(
                        "receive-timeout",
                        shell,
                        Some(&self.command_id),
                        Some(code),
                        detail_code,
                    );
                    debug!("PSRP receive operation timed out; retrying");
                    continue;
                }
                Err(WinrmError::Soap(SoapError::Fault {
                    code,
                    detail_code,
                    reason,
                })) => {
                    emit_psrp_cancel_diagnostic(
                        "receive-fault",
                        shell,
                        Some(&self.command_id),
                        Some(&code),
                        detail_code,
                    );
                    warn!("PSRP transport SOAP fault; aborting receive");
                    return Err(PsrpError::Winrm(WinrmError::Soap(SoapError::Fault {
                        code,
                        detail_code,
                        reason,
                    })));
                }
                Err(e) => {
                    emit_psrp_cancel_diagnostic(
                        receive_error_stage(&e),
                        shell,
                        Some(&self.command_id),
                        None,
                        None,
                    );
                    return Err(PsrpError::Winrm(e));
                }
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
        let cmd_id = shell
            .start_command_with_id("", &[&b64], &pipeline_id.hyphenated().to_string())
            .await?;
        debug!(cmd_id, "PSRP pipeline Execute started");
        self.command_id = cmd_id;
        self.has_command = true;
        Ok(())
    }

    async fn signal_stop(&self) -> Result<()> {
        let shell = self.shell()?;
        emit_psrp_cancel_diagnostic("signal-start", shell, Some(&self.command_id), None, None);
        match shell.signal_ctrl_c(&self.command_id).await {
            Ok(()) => {
                emit_psrp_cancel_diagnostic("signal-ok", shell, Some(&self.command_id), None, None);
                Ok(())
            }
            Err(WinrmError::Soap(SoapError::Fault {
                code,
                detail_code,
                reason,
            })) => {
                emit_psrp_cancel_diagnostic(
                    "signal-fault",
                    shell,
                    Some(&self.command_id),
                    Some(&code),
                    detail_code,
                );
                Err(PsrpError::Winrm(WinrmError::Soap(SoapError::Fault {
                    code,
                    detail_code,
                    reason,
                })))
            }
            Err(error) => {
                emit_psrp_cancel_diagnostic(
                    signal_error_stage(&error),
                    shell,
                    Some(&self.command_id),
                    None,
                    None,
                );
                Err(PsrpError::Winrm(error))
            }
        }?;
        Ok(())
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

fn emit_psrp_cancel_diagnostic(
    stage: &'static str,
    shell: &Shell<'_>,
    command_id: Option<&str>,
    soap_code: Option<&str>,
    wsman_detail_code: Option<u32>,
) {
    if std::env::var_os("FESTERM_PSRP_CANCEL_DIAGNOSTICS").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return;
    }
    eprintln!(
        "festerm-psrp-cancel-diagnostic stage={stage} resource_uri={} command_id={} command_id_fingerprint={} soap_code={} wsman_detail_code={}",
        resource_uri_category(shell.resource_uri()),
        command_id_category(command_id),
        command_id_fingerprint(command_id),
        soap_code_category(soap_code),
        wsman_detail_code.map_or_else(|| "none".to_owned(), |code| code.to_string()),
    );
}

fn signal_error_stage(error: &WinrmError) -> &'static str {
    error_stage("signal", error)
}

fn receive_error_stage(error: &WinrmError) -> &'static str {
    error_stage("receive", error)
}

fn error_stage(prefix: &'static str, error: &WinrmError) -> &'static str {
    match error {
        WinrmError::Timeout(_) if prefix == "signal" => "signal-timeout",
        WinrmError::Cancelled if prefix == "signal" => "signal-cancelled",
        WinrmError::Http(_) if prefix == "signal" => "signal-http",
        WinrmError::AuthFailed(_) if prefix == "signal" => "signal-auth",
        WinrmError::Ntlm(_) if prefix == "signal" => "signal-ntlm",
        WinrmError::ResponseTooLarge { .. } if prefix == "signal" => "signal-response-too-large",
        WinrmError::Transfer(_) if prefix == "signal" => "signal-transfer",
        WinrmError::CredSsp(_) if prefix == "signal" => "signal-credssp",
        WinrmError::Soap(_) if prefix == "signal" => "signal-soap",
        WinrmError::Timeout(_) => "receive-client-timeout",
        WinrmError::Cancelled => "receive-cancelled",
        WinrmError::Http(_) => "receive-http",
        WinrmError::AuthFailed(_) => "receive-auth",
        WinrmError::Ntlm(_) => "receive-ntlm",
        WinrmError::ResponseTooLarge { .. } => "receive-response-too-large",
        WinrmError::Transfer(_) => "receive-transfer",
        WinrmError::CredSsp(_) => "receive-credssp",
        WinrmError::Soap(_) => "receive-soap",
    }
}

fn resource_uri_category(uri: &str) -> &'static str {
    if uri == RESOURCE_URI_PSRP {
        "psrp"
    } else if uri.contains("/cmd") {
        "cmd"
    } else {
        "other"
    }
}

fn command_id_category(command_id: Option<&str>) -> &'static str {
    match command_id {
        Some(value) if uuid::Uuid::parse_str(value).is_ok() => "uuid",
        Some("") => "empty",
        Some(_) => "non-uuid",
        None => "none",
    }
}

fn command_id_fingerprint(command_id: Option<&str>) -> String {
    let Some(value) = command_id else {
        return "none".into();
    };
    if uuid::Uuid::parse_str(value).is_err() {
        return "non-uuid".into();
    }
    let suffix_start = value.len().saturating_sub(8);
    format!("uuid-suffix-{}", &value[suffix_start..])
}

fn soap_code_category(code: Option<&str>) -> &'static str {
    let Some(code) = code else {
        return "none";
    };
    if code.contains("TimedOut") {
        "w:TimedOut"
    } else if code.contains("InvalidSelectors") {
        "w:InvalidSelectors"
    } else if code.contains("InvalidResourceURI") {
        "w:InvalidResourceURI"
    } else if code.contains("DestinationUnreachable") {
        "a:DestinationUnreachable"
    } else if code.contains("ActionNotSupported") {
        "a:ActionNotSupported"
    } else if code == "s:Sender" || code == "env:Sender" || code == "soap:Sender" {
        "soap:Sender"
    } else if code == "s:Receiver" || code == "env:Receiver" || code == "soap:Receiver" {
        "soap:Receiver"
    } else {
        "other"
    }
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
    use super::{PsrpError, PsrpTransport, Result, async_trait};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use tokio::sync::Notify;

    /// In-memory transport used by the test suite.
    #[derive(Clone, Default, Debug)]
    pub struct MockTransport {
        pub inbox: Arc<Mutex<VecDeque<Vec<u8>>>>, // bytes to hand out of recv_chunk
        pub outbox: Arc<Mutex<Vec<Vec<u8>>>>,     // bytes captured from send_fragment
        pub stopped: Arc<Mutex<bool>>,
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

        async fn signal_stop(&self) -> Result<()> {
            *self.stopped.lock().unwrap() = true;
            Ok(())
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

        t.signal_stop().await.unwrap();
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
}
