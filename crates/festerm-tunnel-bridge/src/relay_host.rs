//! Host side of the Dev Tunnels relay handshake (desktop/remote host only).
use std::{io, sync::Arc, time::Duration};

use russh::{client, server, Channel, ChannelId, ChannelStream};
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use tunnels::{
    contracts::{TunnelConnectionMode, TunnelEndpoint},
    management::{new_tunnel_management, Authorization, TunnelLocator, TunnelRequestOptions},
};

use crate::{
    host_identity::HostIdentity,
    protocol::invalid,
    relay::{
        management_error, outer_client_config, reject_unrelated_server_channels, relay_stage_error,
        stage_error, transient_io, websocket,
    },
};

/// Concurrent relay clients: enough for one attached device, one taking it over
/// and a read-only discovery, without letting a peer queue unbounded work.
const RELAY_CLIENTS: usize = 4;
/// Forwarded channels per authenticated client: the terminal stream, the
/// observation control stream and at most two observation watches.
const CHANNELS_PER_CLIENT: usize = 4;

pub struct HostConnection {
    pub endpoint: TunnelEndpoint,
    pub connections: mpsc::Receiver<HostStream>,
    cancel: CancellationToken,
    exit: tokio::sync::watch::Receiver<Option<HostExit>>,
    service: Option<tokio::task::JoinHandle<io::Result<()>>>,
    drain_failed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostFailureDisposition {
    Transient,
    Permanent,
}

#[derive(Clone, Copy, Debug)]
struct HostExit {
    category: &'static str,
    disposition: HostFailureDisposition,
}

/// Retain channel and client admission through serving, even after SSH disconnects.
pub struct HostStream {
    stream: ChannelStream<server::Msg>,
    _slot: OwnedSemaphorePermit,
    _client: Arc<OwnedSemaphorePermit>,
    #[cfg(feature = "owner-auth")]
    owner_host: crate::owner_auth::PinnedHost,
}
#[cfg(feature = "owner-auth")]
impl HostStream {
    pub fn require_owner_channel_binding(&self) -> Result<(), crate::owner_auth::OwnerKeyError> {
        self.owner_channel_binding().map(|_| ())
    }
    pub(crate) fn owner_channel_binding(
        &self,
    ) -> Result<&russh::ChannelBinding, crate::owner_auth::OwnerKeyError> {
        self.stream
            .channel_binding()
            .ok_or(crate::owner_auth::OwnerKeyError::ChannelBindingUnavailable)
    }
    pub(crate) fn owner_host(&self) -> &crate::owner_auth::PinnedHost {
        &self.owner_host
    }
}
impl tokio::io::AsyncRead for HostStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl tokio::io::AsyncWrite for HostStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
impl HostConnection {
    pub fn failure_category(&self) -> Option<&'static str> {
        self.exit.borrow().map(|exit| exit.category)
    }

    pub fn failure_disposition(&self) -> Option<HostFailureDisposition> {
        self.exit.borrow().map(|exit| exit.disposition)
    }

    /// Retire the receiver and await the relay supervisor and every client.
    pub async fn close(&mut self) -> io::Result<()> {
        if self.drain_failed {
            return Err(stage_error("host.relay_drain_failed"));
        }
        self.connections.close();
        self.cancel.cancel();
        while self.connections.try_recv().is_ok() {}
        let result = if let Some(service) = self.service.as_mut() {
            service
                .await
                .map_err(|_| stage_error("host.relay_drain_failed"))
                .and_then(|result| result)
        } else {
            Ok(())
        };
        self.service = None;
        self.drain_failed = result.is_err();
        result
    }
}
impl Drop for HostConnection {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub async fn host(
    tunnel_id: &str,
    cluster: &str,
    port: u16,
    token: &str,
) -> io::Result<HostConnection> {
    host_with_identity(tunnel_id, cluster, port, token, HostIdentity::ephemeral()?).await
}

/// Reuses a locally trusted signing identity and relay host identifier.
/// Relay-issued connection URIs remain service-owned and may change.
pub async fn host_with_identity(
    tunnel_id: &str,
    cluster: &str,
    port: u16,
    token: &str,
    identity: HostIdentity,
) -> io::Result<HostConnection> {
    if port == 0
        || ![tunnel_id, cluster].iter().all(|s| {
            !s.is_empty()
                && s.len() <= 100
                && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
    {
        return Err(stage_error("host.arguments"));
    }
    let host_id = identity.host_id();
    #[cfg(feature = "owner-auth")]
    let owner_host = crate::owner_auth::PinnedHost {
        host: host_id.clone().try_into()?,
        host_key_fingerprint: identity.fingerprint()?,
    };
    let key = identity.into_key();
    let public = key
        .public_key()
        .to_openssh()
        .map_err(|_| stage_error("host.key_encoding"))?;
    let request = TunnelEndpoint {
        id: Some(format!("{host_id}-relay")),
        host_id,
        connection_mode: TunnelConnectionMode::TunnelRelay,
        host_public_keys: vec![public
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| stage_error("host.key_encoding"))?
            .to_owned()],
        port_uri_format: None,
        tunnel_uri: None,
        port_ssh_command_format: None,
        tunnel_ssh_command: None,
        ssh_gateway_public_key: None,
        local_network_tunnel_endpoint: Default::default(),
        tunnel_relay_tunnel_endpoint: Default::default(),
    };
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| stage_error("host.http_client"))?;
    let mut builder = new_tunnel_management("fesTerm-experimental-bridge/1");
    builder.client(http);
    let management: tunnels::management::TunnelManagementClient = builder.into();
    let endpoint = management
        .update_tunnel_relay_endpoints(
            &TunnelLocator::ID {
                cluster: cluster.into(),
                id: tunnel_id.into(),
            },
            &request,
            &TunnelRequestOptions {
                authorization: Some(Authorization::Tunnel(token.into())),
                ..Default::default()
            },
        )
        .await
        .map_err(management_error)?;
    if endpoint.host_public_keys != request.host_public_keys
        || endpoint.host_id != request.host_id
        || endpoint.id != request.id
        || !matches!(endpoint.connection_mode, TunnelConnectionMode::TunnelRelay)
    {
        return Err(stage_error("host.endpoint_key_mismatch"));
    }
    let url = endpoint
        .tunnel_relay_tunnel_endpoint
        .host_relay_uri
        .as_deref()
        .ok_or_else(|| stage_error("host.endpoint_uri_missing"))?;
    let rw = websocket(url, token, "tunnel-relay-host").await?;
    let (outer_tx, mut outer_rx) = mpsc::channel(RELAY_CLIENTS);
    let slots = Arc::new(Semaphore::new(RELAY_CLIENTS));
    let session = client::connect_stream(
        Arc::new(outer_client_config()),
        rw,
        Outer {
            tx: outer_tx,
            slots,
            permit: None,
        },
    )
    .await
    .map_err(|error| relay_stage_error("host.outer_ssh".into(), transient_outer_error(&error)))?;
    let cancel = CancellationToken::new();
    let stopped = cancel.clone();
    let (tx, connections) = mpsc::channel(RELAY_CLIENTS * CHANNELS_PER_CLIENT);
    let (exit_sender, exit) = tokio::sync::watch::channel(None);
    let service = tokio::spawn(async move {
        let server_config = Arc::new(server::Config {
            keys: vec![key],
            window_size: 256 * 1024,
            maximum_packet_size: 32 * 1024,
            channel_buffer_size: 8,
            event_buffer_size: 8,
            inactivity_timeout: Some(Duration::from_secs(120)),
            ..Default::default()
        });
        let service = async {
            let mut clients = tokio::task::JoinSet::new();
            let mut drain_failed = false;
            loop {
                let incoming = tokio::select! {
                    _ = stopped.cancelled() => break,
                    value = outer_rx.recv() => match value { Some(value) => value, None => break },
                };
                let (channel, permit) = incoming;
                let permit = Arc::new(permit);
                while let Some(result) = clients.try_join_next() {
                    if !matches!(result, Ok(Ok(()))) {
                        drain_failed = true;
                    }
                }
                if drain_failed {
                    stopped.cancel();
                    break;
                }
                let (authed, adopted) = oneshot::channel();
                let handler = Port {
                    port,
                    tx: tx.clone(),
                    slots: Arc::new(Semaphore::new(CHANNELS_PER_CLIENT)),
                    client: permit.clone(),
                    authed: Some(authed),
                    #[cfg(feature = "owner-auth")]
                    owner_host: owner_host.clone(),
                };
                let server_config = server_config.clone();
                let stopped = stopped.clone();
                // Clients are serviced concurrently so a second device can reach
                // the session daemon and take the session over; the daemon, not
                // the relay, arbitrates the attachment. Each client holds its
                // permit until it finishes, bounding the per-client tasks.
                clients.spawn(async move {
                    let mut running = tokio::select! {
                        biased;
                        _ = stopped.cancelled() => return Ok(()),
                        result = server::run_stream(server_config, channel, handler) =>
                            match result { Ok(running) => running, Err(_) => return Ok(()) },
                    };
                    let mut finished = false;
                    tokio::select! {
                        _ = stopped.cancelled() => {}
                        _ = async {
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_secs(30)) => return,
                                _ = &mut running => { finished = true; return; }
                                result = adopted => { if result.is_err() { return; } }
                            }
                            if running.handle().forward_tcpip("127.0.0.1".into(), port as u32).await.is_err() {
                                return;
                            }
                            let _ = (&mut running).await;
                            finished = true;
                        } => {}
                    }
                    if !finished {
                        let _ = tokio::time::timeout(
                            Duration::from_secs(2),
                            running.handle().disconnect(russh::Disconnect::ByApplication, "bridge closed".into(), "en".into()),
                        ).await;
                        let _ = tokio::time::timeout(Duration::from_secs(2), &mut running)
                            .await
                            .map_err(|_| stage_error("host.relay_drain_failed"))?;
                    }
                    drop(permit);
                    Ok::<_, io::Error>(())
                });
            }
            stopped.cancel();
            while let Some(result) = clients.join_next().await {
                if !matches!(result, Ok(Ok(()))) {
                    drain_failed = true;
                }
            }
            if drain_failed {
                Err(stage_error("host.relay_drain_failed"))
            } else {
                Ok(())
            }
        };
        supervise_outer(session, service, stopped.clone(), exit_sender).await
    });
    Ok(HostConnection {
        endpoint,
        connections,
        cancel,
        exit,
        service: Some(service),
        drain_failed: false,
    })
}

fn transient_outer_error(error: &russh::Error) -> bool {
    match error {
        russh::Error::HUP
        | russh::Error::ConnectionTimeout
        | russh::Error::KeepaliveTimeout
        | russh::Error::InactivityTimeout => true,
        russh::Error::IO(error) => transient_io(error),
        _ => false,
    }
}

fn outer_exit(result: Result<(), russh::Error>) -> HostExit {
    let disposition = if result.as_ref().is_err_and(transient_outer_error) {
        HostFailureDisposition::Transient
    } else {
        HostFailureDisposition::Permanent
    };
    HostExit {
        category: outer_exit_category(result),
        disposition,
    }
}

fn outer_exit_category(result: Result<(), russh::Error>) -> &'static str {
    use russh::Error;
    match result {
        Ok(()) => "host.outer_closed",
        Err(Error::Disconnect | Error::HUP) => "host.outer_peer_disconnected",
        Err(Error::Inconsistent) => "host.outer_protocol_inconsistent",
        Err(Error::PacketAuth) => "host.outer_packet_auth",
        Err(Error::NotAuthenticated | Error::UnsupportedAuthMethod) => "host.outer_auth_state",
        Err(Error::NoCommonAlgo { .. }) => "host.outer_algorithm_negotiation",
        Err(Error::UnknownAlgo) => "host.outer_unknown_algorithm",
        Err(Error::UnknownKey | Error::WrongServerSig) => "host.outer_key_verification",
        Err(Error::Kex | Error::KexInit) => "host.outer_key_exchange",
        Err(Error::PacketSize(_)) => "host.outer_packet_size",
        Err(Error::WrongChannel | Error::ChannelOpenFailure(_)) => "host.outer_channel_state",
        Err(Error::SendError | Error::RecvError) => "host.outer_channel",
        Err(Error::Pending) => "host.outer_pending_limit",
        Err(Error::DecryptionError) => "host.outer_decryption",
        Err(Error::Keys(_)) => "host.outer_key_decode",
        Err(Error::SshKey(_)) => "host.outer_ssh_key_decode",
        Err(Error::SshEncoding(_)) => "host.outer_encoding",
        Err(Error::Utf8(_)) => "host.outer_utf8",
        Err(Error::Join(_)) => "host.outer_task",
        Err(Error::Elapsed(_)) => "host.outer_deadline",
        Err(Error::InvalidConfig(_)) => "host.outer_configuration",
        Err(Error::StrictKeyExchangeViolation { .. }) => "host.outer_strict_exchange",
        Err(Error::NoAuthMethod | Error::Version) => "host.outer_protocol",
        Err(Error::RequestDenied) => "host.outer_request_denied",
        Err(Error::IO(_)) => "host.outer_transport",
        Err(Error::ConnectionTimeout | Error::KeepaliveTimeout | Error::InactivityTimeout) => {
            "host.outer_timeout"
        }
        Err(_) => "host.outer_other",
    }
}

async fn supervise_outer(
    mut session: client::Handle<Outer>,
    service: impl std::future::Future<Output = io::Result<()>>,
    stopped: CancellationToken,
    exit: tokio::sync::watch::Sender<Option<HostExit>>,
) -> io::Result<()> {
    tokio::pin!(service);
    let mut service_result = None;
    let (finished, service_finished) = tokio::select! {
        _ = stopped.cancelled() => (false, false),
        result = &mut session => {
            exit.send_replace(Some(outer_exit(result)));
            (true, false)
        },
        result = &mut service => {
            service_result = Some(result);
            match tokio::time::timeout(Duration::from_secs(2), &mut session).await {
                Ok(result) => {
                    exit.send_replace(Some(outer_exit(result)));
                    (true, true)
                }
                Err(_) => {
                    if !stopped.is_cancelled() {
                        exit.send_replace(Some(HostExit {
                            category: "host.outer_channel_closed",
                            disposition: HostFailureDisposition::Permanent,
                        }));
                    }
                    (false, true)
                }
            }
        }
    };
    stopped.cancel();
    let service_result = if service_finished {
        service_result.expect("completed relay service")
    } else {
        service.await
    };
    if !finished {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            session.disconnect(russh::Disconnect::ByApplication, "bridge closed", "en"),
        )
        .await;
        let _ = tokio::time::timeout(Duration::from_secs(2), &mut session)
            .await
            .map_err(|_| stage_error("host.relay_drain_failed"))?;
    }
    service_result
}

type OuterStream = (ChannelStream<client::Msg>, OwnedSemaphorePermit);
struct Outer {
    tx: mpsc::Sender<OuterStream>,
    slots: Arc<Semaphore>,
    permit: Option<OwnedSemaphorePermit>,
}
impl client::Handler for Outer {
    type Error = russh::Error;
    reject_unrelated_server_channels!();
    async fn should_accept_unknown_server_channel(&mut self, _: ChannelId, kind: &str) -> bool {
        if kind != "client-ssh-session-stream" {
            return false;
        }
        self.permit = self.slots.clone().try_acquire_owned().ok();
        self.permit.is_some()
    }
    async fn server_channel_open_unknown(
        &mut self,
        channel: Channel<client::Msg>,
        reply: client::ChannelOpenHandle,
        _: &mut client::Session,
    ) -> Result<(), Self::Error> {
        if let Some(permit) = self.permit.take() {
            reply.accept().await;
            let _ = self.tx.try_send((channel.into_stream(), permit));
        }
        Ok(())
    }
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        _: Channel<client::Msg>,
        _: &str,
        _: u32,
        _: &str,
        _: u32,
        _: client::ChannelOpenHandle,
        _: &mut client::Session,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

struct Port {
    port: u16,
    tx: mpsc::Sender<HostStream>,
    slots: Arc<Semaphore>,
    client: Arc<OwnedSemaphorePermit>,
    authed: Option<oneshot::Sender<()>>,
    #[cfg(feature = "owner-auth")]
    owner_host: crate::owner_auth::PinnedHost,
}
impl server::Handler for Port {
    type Error = russh::Error;
    async fn auth_none(&mut self, _: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }

    async fn auth_succeeded(&mut self, _: &mut server::Session) -> Result<(), Self::Error> {
        if let Some(sender) = self.authed.take() {
            let _ = sender.send(());
        }
        Ok(())
    }
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<server::Msg>,
        host: &str,
        port: u32,
        _: &str,
        _: u32,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if port != self.port as u32 || host != "127.0.0.1" {
            return Ok(());
        }
        let Ok(slot) = self.slots.clone().try_acquire_owned() else {
            return Ok(());
        };
        // Reserve before accepting so an accepted channel is never silently dropped.
        let Ok(queued) = self.tx.try_reserve() else {
            return Ok(());
        };
        reply.accept().await;
        queued.send(HostStream {
            stream: channel.into_stream(),
            _slot: slot,
            _client: self.client.clone(),
            #[cfg(feature = "owner-auth")]
            owner_host: self.owner_host.clone(),
        });
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::relay::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub(crate) fn closed_test_listener() -> HostConnection {
        let (tx, connections) = mpsc::channel(1);
        drop(tx);
        let (_exit, exit) = tokio::sync::watch::channel(None);
        HostConnection {
            endpoint: TunnelEndpoint::default(),
            connections,
            cancel: CancellationToken::new(),
            exit,
            service: None,
            drain_failed: false,
        }
    }

    #[test]
    fn host_relay_recovery_classifies_positive_transport_without_text_or_denial_retry() {
        use tunnels::management::{HttpError, ResponseError};
        for status in [
            400, 401, 403, 404, 408, 409, 412, 429, 500, 501, 502, 503, 504,
        ] {
            let error = management_error(HttpError::ResponseError(ResponseError {
                url: reqwest::Url::parse("https://example.invalid/PRIVATE").unwrap(),
                status_code: reqwest::StatusCode::from_u16(status).unwrap(),
                data: Some("PRIVATE".into()),
                request_id: Some("PRIVATE".into()),
            }));
            assert_eq!(
                is_transient_failure(&error),
                matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
            );
            assert!(!format!("{error:?}").contains("PRIVATE"));
        }
        for error in [
            russh::Error::HUP,
            russh::Error::KeepaliveTimeout,
            russh::Error::IO(io::ErrorKind::ConnectionReset.into()),
        ] {
            assert_eq!(
                outer_exit(Err(error)).disposition,
                HostFailureDisposition::Transient
            );
        }
        for result in [
            Ok(()),
            Err(russh::Error::Disconnect),
            Err(russh::Error::RequestDenied),
            Err(russh::Error::PacketAuth),
            Err(russh::Error::WrongServerSig),
            Err(russh::Error::IO(io::Error::other("host.outer_timeout"))),
            Err(russh::Error::IO(io::ErrorKind::PermissionDenied.into())),
        ] {
            assert_eq!(
                outer_exit(result).disposition,
                HostFailureDisposition::Permanent
            );
        }
        assert!(!is_transient_failure(&io::Error::other(
            "host.endpoint_update.http_503"
        )));
    }

    #[tokio::test(start_paused = true)]
    async fn host_relay_recovery_close_awaits_owned_service_even_after_waiter_cancellation() {
        let mut listener = closed_test_listener();
        let stopped = listener.cancel.clone();
        let (finish, finished) = oneshot::channel::<()>();
        listener.service = Some(tokio::spawn(async move {
            stopped.cancelled().await;
            finished.await.unwrap();
            Ok(())
        }));
        {
            let closing = listener.close();
            tokio::pin!(closing);
            assert!(tokio::time::timeout(Duration::from_secs(3), &mut closing)
                .await
                .is_err());
        }
        assert!(listener.cancel.is_cancelled());
        assert!(!listener.service.as_ref().unwrap().is_finished());
        finish.send(()).unwrap();
        listener.close().await.unwrap();
        assert!(listener.service.is_none());
        listener.close().await.unwrap();
    }

    #[tokio::test]
    async fn host_relay_recovery_drain_failure_never_becomes_success_on_second_close() {
        let mut listener = closed_test_listener();
        let service = tokio::spawn(std::future::pending::<io::Result<()>>());
        service.abort();
        listener.service = Some(service);
        assert!(listener.close().await.is_err());
        assert!(listener.close().await.is_err());
        assert_eq!(listener.failure_disposition(), None);
    }

    #[tokio::test]
    async fn anonymous_wire_peer_requires_none_mac_no_newkeys_and_reports_lifetime() {
        async fn read_packet(io: &mut tokio::io::DuplexStream) -> Vec<u8> {
            let length = io.read_u32().await.unwrap() as usize;
            assert!((5..=4096).contains(&length));
            let mut packet = vec![0; length];
            io.read_exact(&mut packet).await.unwrap();
            let padding = packet[0] as usize;
            assert!(padding >= 4 && padding < length - 1);
            packet[1..length - padding].to_vec()
        }
        async fn write_packet(io: &mut tokio::io::DuplexStream, payload: &[u8]) {
            let padding = 4 + (8 - (5 + payload.len() + 4) % 8) % 8;
            let length = u32::try_from(1 + payload.len() + padding).unwrap();
            io.write_u32(length).await.unwrap();
            io.write_u8(u8::try_from(padding).unwrap()).await.unwrap();
            io.write_all(payload).await.unwrap();
            io.write_all(&vec![0; padding]).await.unwrap();
        }
        fn string(out: &mut Vec<u8>, value: &[u8]) {
            out.extend_from_slice(&u32::try_from(value.len()).unwrap().to_be_bytes());
            out.extend_from_slice(value);
        }
        fn take_string<'a>(input: &mut &'a [u8]) -> &'a [u8] {
            let length = u32::from_be_bytes(input[..4].try_into().unwrap()) as usize;
            let result = &input[4..4 + length];
            *input = &input[4 + length..];
            result
        }

        let (client_io, mut peer_io) = tokio::io::duplex(16 * 1024);
        let (ready, alive) = oneshot::channel();
        let (stop, stopped) = oneshot::channel();
        let peer = tokio::spawn(async move {
            peer_io
                .write_all(b"SSH-2.0-festerm-test-relay\r\n")
                .await
                .unwrap();
            let mut identified = false;
            for _ in 0..255 {
                if peer_io.read_u8().await.unwrap() == b'\n' {
                    identified = true;
                    break;
                }
            }
            assert!(identified);
            let offered = read_packet(&mut peer_io).await;
            assert_eq!(offered[0], 20);
            let mut names = &offered[17..];
            for index in 0..10 {
                let value = take_string(&mut names);
                if index == 4 || index == 5 {
                    assert!(value
                        .split(|byte| *byte == b',')
                        .any(|name| name == b"none"));
                }
            }
            let mut kex = vec![20];
            kex.extend_from_slice(&[0; 16]);
            for name in [
                b"none".as_slice(),
                b"none",
                b"none",
                b"none",
                b"none",
                b"none",
                b"none",
                b"none",
                b"",
                b"",
            ] {
                string(&mut kex, name);
            }
            kex.extend_from_slice(&[0; 5]);
            write_packet(&mut peer_io, &kex).await;
            let mut keepalive = vec![80];
            string(&mut keepalive, b"keepalive@openssh.com");
            keepalive.push(1);
            write_packet(&mut peer_io, &keepalive).await;
            // A real service rejects NEWKEYS here: no keys were exchanged.
            assert_eq!(read_packet(&mut peer_io).await, [81]);
            ready.send(()).unwrap();
            stopped.await.unwrap();
            let mut disconnect = vec![1];
            disconnect.extend_from_slice(&11_u32.to_be_bytes());
            string(&mut disconnect, b"PRIVATE_PEER_DESCRIPTION_NOT_FOR_LOGS");
            string(&mut disconnect, b"en");
            write_packet(&mut peer_io, &disconnect).await;
        });
        let (tx, _rx) = mpsc::channel(1);
        let session = tokio::time::timeout(
            Duration::from_secs(3),
            client::connect_stream(
                Arc::new(outer_client_config()),
                client_io,
                Outer {
                    tx,
                    slots: Arc::new(Semaphore::new(1)),
                    permit: None,
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let (exit, mut status) = tokio::sync::watch::channel(None);
        let stopped = CancellationToken::new();
        let serving = {
            let stopped = stopped.clone();
            async move {
                stopped.cancelled().await;
                Ok(())
            }
        };
        let supervisor = tokio::spawn(supervise_outer(session, serving, stopped, exit));
        tokio::time::timeout(Duration::from_secs(3), alive)
            .await
            .unwrap()
            .unwrap();
        assert!(status.borrow().is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(30), status.changed())
                .await
                .is_err()
        );
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), status.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            status.borrow().map(|exit| exit.category),
            Some("host.outer_closed")
        );
        assert!(!format!("{:?}", *status.borrow()).contains("PRIVATE_PEER"));
        supervisor.await.unwrap().unwrap();
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn anonymous_outer_transport_negotiates_none_without_weakening_inner_defaults() {
        struct Relay;
        impl server::Handler for Relay {
            type Error = russh::Error;
        }
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let config = Arc::new(server::Config {
            preferred: russh::Preferred::anonymous_none(),
            ..Default::default()
        });
        let task = tokio::spawn(async move {
            server::run_stream(config, server_io, Relay)
                .await
                .unwrap()
                .await
        });
        let (tx, _rx) = mpsc::channel(1);
        let handle = tokio::time::timeout(
            Duration::from_secs(3),
            client::connect_stream(
                Arc::new(outer_client_config()),
                client_io,
                Outer {
                    tx,
                    slots: Arc::new(Semaphore::new(1)),
                    permit: None,
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        drop(handle);
        task.abort();
        let _ = task.await;
        let inner = client_config();
        assert!(!inner.anonymous);
        assert!(!inner.preferred.kex.contains(&russh::kex::NONE));
        assert!(!inner.preferred.cipher.contains(&russh::cipher::NONE));
        assert!(!inner.preferred.key.is_empty());
    }

    #[tokio::test]
    async fn anonymous_none_is_not_an_encrypted_client_fallback() {
        struct Relay;
        impl server::Handler for Relay {
            type Error = russh::Error;
        }
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let task = tokio::spawn(async move {
            server::run_stream(
                Arc::new(server::Config {
                    preferred: russh::Preferred::anonymous_none(),
                    ..Default::default()
                }),
                server_io,
                Relay,
            )
            .await
        });
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            client::connect_stream(
                Arc::new(client_config()),
                client_io,
                VerifiedHost("no-accept-any-key-fallback".into()),
            ),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        task.abort();
        let _ = task.await;
    }

    #[test]
    fn endpoint_stage_diagnostics_never_format_request_or_response_material() {
        use tunnels::management::{HttpError, ResponseError};
        let marker = "DO_NOT_LOG_PRIVATE_TOKEN";
        let error = management_error(HttpError::ResponseError(ResponseError {
            url: reqwest::Url::parse(&format!("https://example.invalid/{marker}")).unwrap(),
            status_code: reqwest::StatusCode::FORBIDDEN,
            data: Some(marker.into()),
            request_id: Some(marker.into()),
        }));
        assert_eq!(error.to_string(), "host.endpoint_update.http_403");
        assert!(!format!("{error:?}").contains(marker));
        let error = management_error(HttpError::AuthorizationError(marker.into()));
        assert_eq!(error.to_string(), "host.endpoint_update.authorization");
        assert!(!format!("{error:?}").contains(marker));
        let request_error = reqwest::Client::new().get(marker).build().unwrap_err();
        let error = management_error(HttpError::ConnectionError(request_error));
        assert_eq!(error.to_string(), "host.endpoint_update.transport");
        assert!(!format!("{error:?}").contains(marker));
        assert_eq!(
            outer_exit_category(Err(russh::Error::InvalidConfig(marker.into()))),
            "host.outer_configuration"
        );
        assert_eq!(
            outer_exit_category(Err(russh::Error::IO(io::Error::other(marker)))),
            "host.outer_transport"
        );
    }

    fn key() -> russh::keys::PrivateKey {
        russh::keys::PrivateKey::random(
            &mut russh::keys::key::safe_rng(),
            russh::keys::Algorithm::Ed25519,
        )
        .unwrap()
    }
    fn pin(key: &russh::keys::PrivateKey) -> String {
        key.public_key()
            .to_openssh()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_owned()
    }

    #[test]
    fn relay_urls_refuse_arbitrary_authorities_and_plaintext() {
        for uri in [
            "ws://foo.rel.tunnels.api.visualstudio.com/connect",
            "wss://evil.test/connect",
            "wss://foo.rel.tunnels.api.visualstudio.com.evil.test/connect",
            "wss://user:secret@foo.rel.tunnels.api.visualstudio.com/connect",
            "wss://foo.rel.tunnels.api.visualstudio.com:444/connect",
        ] {
            assert!(validate_relay_url(uri).is_err());
        }
        assert!(validate_relay_url("wss://usw3.rel.tunnels.api.visualstudio.com/connect").is_ok());
    }

    #[tokio::test]
    async fn encrypted_transport_pins_host_and_exports_only_one_preconfigured_port() {
        let key = key();
        let expected = pin(&key);
        #[cfg(feature = "owner-auth")]
        let owner_host = crate::owner_auth::PinnedHost {
            host: Default::default(),
            host_key_fingerprint: key
                .public_key()
                .fingerprint(russh::keys::HashAlg::Sha256)
                .to_string(),
        };
        let config = Arc::new(server::Config {
            keys: vec![key],
            channel_buffer_size: 8,
            ..Default::default()
        });
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (tx, mut rx) = mpsc::channel(CHANNELS_PER_CLIENT);
        let clients = Arc::new(Semaphore::new(1));
        let client_lease = Arc::new(clients.clone().try_acquire_owned().unwrap());
        let server = tokio::spawn(async move {
            server::run_stream(
                config,
                server_io,
                Port {
                    port: 47461,
                    tx,
                    slots: Arc::new(Semaphore::new(CHANNELS_PER_CLIENT)),
                    client: client_lease,
                    authed: None,
                    #[cfg(feature = "owner-auth")]
                    owner_host,
                },
            )
            .await
            .unwrap()
            .await
        });
        let mut client = client::connect_stream(
            Arc::new(client_config()),
            client_io,
            VerifiedHost(expected.clone()),
        )
        .await
        .unwrap();
        assert!(client.authenticate_none("tunnel").await.unwrap().success());
        assert!(client
            .channel_open_direct_tcpip("127.0.0.1", 22, "127.0.0.1", 0)
            .await
            .is_err());
        let mut channel = client
            .channel_open_direct_tcpip("127.0.0.1", 47461, "127.0.0.1", 0)
            .await
            .unwrap()
            .into_stream();
        let mut accepted = rx.recv().await.unwrap();
        channel.write_all(b"controlled").await.unwrap();
        let mut bytes = [0; 10];
        accepted.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"controlled");
        // Observation control and watch streams open on the same authenticated
        // client through the observation binding, bounded per client.
        let connection = ClientConnection {
            #[cfg(feature = "owner-auth")]
            owner_transport: VerifiedPinnedChannel::from_fixture_stream(&channel, &expected)
                .unwrap(),
            stream: channel,
            _session: Arc::new(client),
            port: 47461,
            observation_expiry: None,
        };
        #[cfg(feature = "owner-auth")]
        {
            assert_eq!(connection.require_owner_channel_binding(), Ok(()));
            assert_eq!(accepted.require_owner_channel_binding(), Ok(()));
            assert_eq!(
                connection.owner_channel_binding().unwrap(),
                accepted.owner_channel_binding().unwrap()
            );
        }
        let binding = connection.observation_binding();
        let mut extra = Vec::new();
        for _ in 1..CHANNELS_PER_CLIENT {
            let mut stream = binding.open().await.unwrap();
            let mut sibling = rx.recv().await.unwrap();
            #[cfg(feature = "owner-auth")]
            {
                assert_ne!(
                    connection.owner_channel_binding().unwrap(),
                    stream.channel_binding().unwrap()
                );
                assert_eq!(
                    stream.channel_binding().unwrap(),
                    sibling.owner_channel_binding().unwrap()
                );
                if extra.is_empty() {
                    use crate::owner_auth::*;
                    let vault = festerm_secret_store::MemorySecretStore::new();
                    let approved = OwnerBinding {
                        installation: festerm_host_protocol::HostId::new().to_string(),
                        host: accepted.owner_host().clone(),
                        account: "github:channel-fixture".into(),
                        principal: Default::default(),
                    };
                    let key = fixture_key(&vault, &approved);
                    let verifier = OwnerVerifier::new(
                        OwnerPolicy {
                            host: approved.host.clone(),
                            principal: approved.principal.clone(),
                            host_epoch: festerm_host_protocol::Epoch::new(1).unwrap(),
                            grant_epoch: festerm_host_protocol::Epoch::new(1).unwrap(),
                            remote_access: true,
                            capabilities: Default::default(),
                        },
                        &[key.public_key()],
                    )
                    .unwrap();
                    let pending = accepted.challenge_owner(&verifier).unwrap();
                    let proof = connection
                        .prove_owner_fixture(&vault, &key, &approved, pending.challenge())
                        .unwrap();
                    assert!(sibling.verify_owner(pending, &proof, &verifier).is_err());
                    let pending = sibling.challenge_owner(&verifier).unwrap();
                    assert!(connection
                        .prove_owner_fixture(&vault, &key, &approved, pending.challenge())
                        .is_err());
                }
            }
            stream.write_all(b"o").await.unwrap();
            let mut byte = [0; 1];
            sibling.read_exact(&mut byte).await.unwrap();
            extra.push((stream, sibling));
        }
        assert!(binding.open().await.is_err());
        for _ in 0..=CHANNELS_PER_CLIENT {
            drop(extra.pop().unwrap());
            let mut stream = binding.open().await.unwrap();
            let mut accepted = rx.recv().await.unwrap();
            stream.write_all(b"r").await.unwrap();
            let mut byte = [0; 1];
            accepted.read_exact(&mut byte).await.unwrap();
            assert_eq!(byte, *b"r");
            extra.push((stream, accepted));
            assert!(binding.open().await.is_err());
        }
        drop(extra);
        connection.close().await.unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .is_ok());
        assert_eq!(clients.available_permits(), 0);
        drop(accepted);
        assert_eq!(clients.available_permits(), 1);
    }

    #[tokio::test]
    async fn encrypted_transport_refuses_changed_host_key_before_authentication() {
        let host_key = key();
        #[cfg(feature = "owner-auth")]
        let owner_host = crate::owner_auth::PinnedHost {
            host: Default::default(),
            host_key_fingerprint: host_key
                .public_key()
                .fingerprint(russh::keys::HashAlg::Sha256)
                .to_string(),
        };
        let config = Arc::new(server::Config {
            keys: vec![host_key],
            ..Default::default()
        });
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (tx, _rx) = mpsc::channel(1);
        let server = tokio::spawn(async move {
            let _ = server::run_stream(
                config,
                server_io,
                Port {
                    port: 47461,
                    tx,
                    slots: Arc::new(Semaphore::new(CHANNELS_PER_CLIENT)),
                    client: Arc::new(Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap()),
                    authed: None,
                    #[cfg(feature = "owner-auth")]
                    owner_host,
                },
            )
            .await;
        });
        assert!(client::connect_stream(
            Arc::new(client_config()),
            client_io,
            VerifiedHost(pin(&key()))
        )
        .await
        .is_err());
        server.abort();
    }

    #[cfg(feature = "owner-auth")]
    pub(crate) async fn owner_pair(
        key: russh::keys::PrivateKey,
        host: crate::owner_auth::PinnedHost,
    ) -> (ClientConnection, HostStream, tokio::task::JoinHandle<()>) {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        owner_pair_on(key, host, client_io, server_io).await
    }

    #[cfg(feature = "host-routing")]
    pub(crate) async fn owner_network_pair(
        key: russh::keys::PrivateKey,
        host: crate::owner_auth::PinnedHost,
    ) -> (ClientConnection, HostStream, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (client_io, server_io) =
            tokio::join!(tokio::net::TcpStream::connect(address), listener.accept());
        owner_pair_on(key, host, client_io.unwrap(), server_io.unwrap().0).await
    }

    #[cfg(feature = "owner-auth")]
    async fn owner_pair_on<
        C: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    >(
        key: russh::keys::PrivateKey,
        host: crate::owner_auth::PinnedHost,
        client_io: C,
        server_io: S,
    ) -> (ClientConnection, HostStream, tokio::task::JoinHandle<()>) {
        let expected = pin(&key);
        let config = Arc::new(server::Config {
            keys: vec![key],
            ..Default::default()
        });
        let (tx, mut rx) = mpsc::channel(CHANNELS_PER_CLIENT);
        let server = tokio::spawn(async move {
            let running = server::run_stream(
                config,
                server_io,
                Port {
                    port: 47461,
                    tx,
                    slots: Arc::new(Semaphore::new(CHANNELS_PER_CLIENT)),
                    client: Arc::new(Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap()),
                    authed: None,
                    owner_host: host,
                },
            )
            .await
            .unwrap();
            let _ = running.await;
        });
        let mut session = client::connect_stream(
            Arc::new(client_config()),
            client_io,
            VerifiedHost(expected.clone()),
        )
        .await
        .unwrap();
        assert!(session.authenticate_none("tunnel").await.unwrap().success());
        let stream = session
            .channel_open_direct_tcpip("127.0.0.1", 47461, "127.0.0.1", 0)
            .await
            .unwrap()
            .into_stream();
        let connection = ClientConnection {
            owner_transport: VerifiedPinnedChannel::from_fixture_stream(&stream, &expected)
                .unwrap(),
            stream,
            _session: Arc::new(session),
            port: 47461,
            observation_expiry: None,
        };
        (connection, rx.recv().await.unwrap(), server)
    }

    #[cfg(feature = "owner-auth")]
    #[tokio::test]
    async fn host_owner_authentication_failure_retains_stream_for_bounded_denial() {
        use crate::owner_auth::*;
        use festerm_host_protocol::{ContractError, Epoch};
        use festerm_secret_store::MemorySecretStore;

        for expired in [false, true] {
            let ssh_key = key();
            let host = PinnedHost {
                host: Default::default(),
                host_key_fingerprint: ssh_key
                    .public_key()
                    .fingerprint(russh::keys::HashAlg::Sha256)
                    .to_string(),
            };
            let (mut client, accepted, server) = owner_pair(ssh_key, host.clone()).await;
            let vault = MemorySecretStore::new();
            let approved = OwnerBinding {
                installation: festerm_host_protocol::HostId::new().to_string(),
                host,
                account: "github:failure-fixture".into(),
                principal: Default::default(),
            };
            let key = fixture_key(&vault, &approved);
            let verifier = OwnerVerifier::new(
                OwnerPolicy {
                    host: approved.host.clone(),
                    principal: approved.principal.clone(),
                    host_epoch: Epoch::new(1).unwrap(),
                    grant_epoch: Epoch::new(1).unwrap(),
                    remote_access: true,
                    capabilities: Default::default(),
                },
                &[key.public_key()],
            )
            .unwrap();
            let signing_key = if expired {
                key
            } else {
                fixture_key(&vault, &approved)
            };
            let mut pending = accepted.challenge_owner(&verifier).unwrap();
            let proof = client
                .prove_owner_fixture(&vault, &signing_key, &approved, pending.challenge())
                .unwrap();
            if expired {
                pending.expire_fixture();
            }
            let (mut rejected, reason) = accepted
                .authenticate_owner(pending, &proof, &verifier)
                .err()
                .unwrap()
                .into_parts();
            assert_eq!(
                reason,
                if expired {
                    ContractError::Stale
                } else {
                    ContractError::Denied
                }
            );
            let reply = reason.to_string();
            tokio::time::timeout(Duration::from_secs(1), async {
                rejected.write_all(reply.as_bytes()).await.unwrap();
                rejected.shutdown().await.unwrap();
                let mut received = vec![0; reply.len()];
                client.stream.read_exact(&mut received).await.unwrap();
                assert_eq!(received, reply.as_bytes());
            })
            .await
            .unwrap();
            drop(rejected);
            client.close().await.unwrap();
            server.await.unwrap();
        }
    }

    #[cfg(feature = "owner-auth")]
    #[tokio::test]
    async fn host_owner_real_ssh_channel_binding_rejects_relay_replay_and_stale_authority() {
        use crate::owner_auth::*;
        use festerm_host_protocol::Epoch;
        use festerm_secret_store::MemorySecretStore;

        let ssh_key = key();
        let host = PinnedHost {
            host: Default::default(),
            host_key_fingerprint: ssh_key
                .public_key()
                .fingerprint(russh::keys::HashAlg::Sha256)
                .to_string(),
        };
        let (mut client, accepted, server) = owner_pair(ssh_key.clone(), host.clone()).await;
        let (mut other, other_host, other_server) = owner_pair(ssh_key, host.clone()).await;
        assert_ne!(
            client.owner_channel_binding().unwrap(),
            other.owner_channel_binding().unwrap()
        );
        assert_eq!(
            client.owner_channel_binding().unwrap(),
            accepted.owner_channel_binding().unwrap()
        );
        let vault = MemorySecretStore::new();
        let approved = OwnerBinding {
            installation: festerm_host_protocol::HostId::new().to_string(),
            host,
            account: "github:fixture-owner".into(),
            principal: Default::default(),
        };
        let owner_key = fixture_key(&vault, &approved);
        let policy = OwnerPolicy {
            host: approved.host.clone(),
            principal: approved.principal.clone(),
            host_epoch: Epoch::new(1).unwrap(),
            grant_epoch: Epoch::new(1).unwrap(),
            remote_access: true,
            capabilities: Default::default(),
        };
        let verifier = OwnerVerifier::new(policy.clone(), &[owner_key.public_key()]).unwrap();
        let proof_for = |pending: &PendingChannelOwnerChallenge| {
            client
                .prove_owner_fixture(&vault, &owner_key, &approved, pending.challenge())
                .unwrap()
        };
        let pending = accepted.challenge_owner(&verifier).unwrap();
        let proof = proof_for(&pending);
        assert!(other
            .prove_owner_fixture(&vault, &owner_key, &approved, pending.challenge())
            .is_err());
        assert!(other_host.verify_owner(pending, &proof, &verifier).is_err());
        let pending = accepted.challenge_owner(&verifier).unwrap();
        assert!(accepted.verify_owner(pending, &proof, &verifier).is_err());
        let pending = accepted.challenge_owner(&verifier).unwrap();
        let unbound =
            client.prove_unbound_fixture(&vault, &owner_key, &approved, pending.challenge());
        assert!(accepted.verify_owner(pending, &unbound, &verifier).is_err());

        for field in 0..5 {
            let pending = accepted.challenge_owner(&verifier).unwrap();
            let mut foreign = approved.clone();
            match field {
                0 => foreign.account = "github:foreign".into(),
                1 => foreign.installation = festerm_host_protocol::HostId::new().to_string(),
                2 => foreign.host.host = Default::default(),
                3 => foreign.host.host_key_fingerprint = format!("SHA256:{}", "a".repeat(43)),
                _ => foreign.principal = Default::default(),
            }
            assert!(client
                .prove_owner_fixture(&vault, &owner_key, &foreign, pending.challenge())
                .is_err());
        }
        let foreign_key = fixture_key(&vault, &approved);
        let pending = accepted.challenge_owner(&verifier).unwrap();
        let proof = client
            .prove_owner_fixture(&vault, &foreign_key, &approved, pending.challenge())
            .unwrap();
        assert!(accepted.verify_owner(pending, &proof, &verifier).is_err());
        let mut pending = accepted.challenge_owner(&verifier).unwrap();
        let proof = proof_for(&pending);
        pending.expire_fixture();
        assert!(accepted.verify_owner(pending, &proof, &verifier).is_err());
        let pending = accepted.challenge_owner(&verifier).unwrap();
        let proof = proof_for(&pending);
        let mut revoked_policy = policy;
        revoked_policy.grant_epoch = Epoch::new(2).unwrap();
        let revoked = OwnerVerifier::new(revoked_policy, &[foreign_key.public_key()]).unwrap();
        assert!(accepted.verify_owner(pending, &proof, &revoked).is_err());

        let pending = accepted.challenge_owner(&verifier).unwrap();
        let proof = proof_for(&pending);
        let (mut accepted, _owner, _guard) = accepted
            .authenticate_owner(pending, &proof, &verifier)
            .unwrap()
            .into_parts();
        client.stream.write_all(b"owner").await.unwrap();
        let mut bytes = [0; 5];
        accepted.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"owner");
        std::mem::swap(&mut client.stream, &mut other.stream);
        assert!(client.require_owner_channel_binding().is_err());
        assert!(other.require_owner_channel_binding().is_err());
        std::mem::swap(&mut client.stream, &mut other.stream);
        client.close().await.unwrap();
        other.close().await.unwrap();
        server.await.unwrap();
        other_server.await.unwrap();
    }
}
