// Relay handshake derived from microsoft/dev-tunnels (MIT), pinned in vendor/dev-tunnels.
//! Narrow modern-russh port of the pinned SDK relay handshake. Unlike the
//! upstream host, this admits a small bounded number of encrypted clients (so a
//! second device can request takeover) and one configured port, with no TCP
//! listeners, port-management API, or unbounded forwarding queues.
use std::{io, sync::Arc, time::Duration};

use russh::{client, Channel, ChannelStream};
use serde::{Deserialize, Serialize};
use tunnels::{
    connections::{
        build_websocket_request, connect_directly, AsyncRWWebSocket, AsyncRWWebSocketOptions,
    },
    contracts::TunnelEndpoint,
};

use crate::protocol::{invalid, Target};

// Neither relay role grants shell, agent, filesystem, or arbitrary forwarding
// channels. Dropping each admission handle rejects it before registration.
macro_rules! reject_unrelated_server_channels {
    () => {
        async fn server_channel_open_session(
            &mut self,
            _: Channel<client::Msg>,
            _: client::ChannelOpenHandle,
            _: &mut client::Session,
        ) -> Result<(), Self::Error> {
            Ok(())
        }
        async fn server_channel_open_agent_forward(
            &mut self,
            _: Channel<client::Msg>,
            _: client::ChannelOpenHandle,
            _: &mut client::Session,
        ) -> Result<(), Self::Error> {
            Ok(())
        }
        async fn server_channel_open_forwarded_streamlocal(
            &mut self,
            _: Channel<client::Msg>,
            _: &str,
            _: client::ChannelOpenHandle,
            _: &mut client::Session,
        ) -> Result<(), Self::Error> {
            Ok(())
        }
        async fn server_channel_open_direct_streamlocal(
            &mut self,
            _: Channel<client::Msg>,
            _: &str,
            _: client::ChannelOpenHandle,
            _: &mut client::Session,
        ) -> Result<(), Self::Error> {
            Ok(())
        }
        async fn server_channel_open_direct_tcpip(
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
        async fn server_channel_open_x11(
            &mut self,
            _: Channel<client::Msg>,
            _: &str,
            _: u32,
            _: client::ChannelOpenHandle,
            _: &mut client::Session,
        ) -> Result<(), Self::Error> {
            Ok(())
        }
    };
}

#[cfg(feature = "relay")]
pub(crate) use reject_unrelated_server_channels;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    #[serde(default)]
    pub file_read: bool,
    #[serde(default)]
    pub path_style: crate::files::PathStyle,
    pub version: u16,
    pub tunnel_id: String,
    pub cluster: String,
    pub port: u16,
    pub target: Target,
    pub endpoint: TunnelEndpoint,
}

pub fn validate_relay_url(value: &str) -> io::Result<()> {
    let url = reqwest::Url::parse(value).map_err(|_| invalid())?;
    let host = url.host_str().ok_or_else(invalid)?;
    if url.scheme() != "wss"
        || !host.ends_with(".rel.tunnels.api.visualstudio.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) async fn websocket(
    url: &str,
    token: &str,
    role: &str,
) -> io::Result<impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> {
    validate_relay_url(url).map_err(|_| stage_error("relay.uri_validation"))?;
    let auth = zeroize::Zeroizing::new(format!("tunnel {token}"));
    let mut request = build_websocket_request(
        url,
        &[
            ("Authorization", &auth),
            ("Sec-WebSocket-Protocol", role),
            ("User-Agent", "fesTerm-experimental-bridge/1"),
        ],
    )
    .map_err(|_| stage_error("relay.websocket_request"))?;
    if let Some(header) = request.headers_mut().get_mut("Authorization") {
        header.set_sensitive(true);
    }
    let websocket = connect_directly(request).await.map_err(|error| {
        use std::error::Error;
        let transient = match &error {
            tunnels::connections::TunnelError::WebSocketError(error) => error
                .source()
                .and_then(|source| source.downcast_ref::<io::Error>())
                .is_some_and(transient_io),
            _ => false,
        };
        relay_stage_error("relay.websocket_upgrade".into(), transient)
    })?;
    Ok(AsyncRWWebSocket::new(AsyncRWWebSocketOptions {
        websocket,
        ping_interval: Duration::from_secs(30),
        ping_timeout: Duration::from_secs(10),
    }))
}

pub(crate) fn stage_error(stage: &'static str) -> io::Error {
    io::Error::other(stage)
}

#[derive(Debug)]
struct TransientRelayFailure(String);

impl std::fmt::Display for TransientRelayFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for TransientRelayFailure {}

/// Only a typed positive transport classification grants recovery, never text.
pub fn is_transient_failure(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<TransientRelayFailure>())
}

pub(crate) fn transient_io(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
            | io::ErrorKind::TimedOut
            | io::ErrorKind::NetworkUnreachable
            | io::ErrorKind::HostUnreachable
    )
}

pub(crate) fn relay_stage_error(stage: String, transient: bool) -> io::Error {
    if transient {
        io::Error::other(TransientRelayFailure(stage))
    } else {
        io::Error::other(stage)
    }
}

#[cfg(feature = "relay")]
pub(crate) fn management_error(error: tunnels::management::HttpError) -> io::Error {
    use tunnels::management::HttpError;
    match error {
        HttpError::ResponseError(response) => relay_stage_error(
            format!(
                "host.endpoint_update.http_{}",
                response.status_code.as_u16()
            ),
            matches!(
                response.status_code.as_u16(),
                408 | 429 | 500 | 502 | 503 | 504
            ),
        ),
        HttpError::ConnectionError(error) if error.is_timeout() => {
            relay_stage_error("host.endpoint_update.timeout".into(), true)
        }
        HttpError::ConnectionError(error) if error.is_decode() => {
            stage_error("host.endpoint_update.decode")
        }
        HttpError::ConnectionError(_) => stage_error("host.endpoint_update.transport"),
        HttpError::AuthorizationError(_) => stage_error("host.endpoint_update.authorization"),
    }
}

pub(crate) fn client_config() -> client::Config {
    client::Config {
        maximum_pending_replies: Some(16),
        window_size: 256 * 1024,
        maximum_packet_size: 32 * 1024,
        channel_buffer_size: 8,
        inactivity_timeout: Some(Duration::from_secs(120)),
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    }
}

#[cfg(feature = "relay")]
pub(crate) fn outer_client_config() -> client::Config {
    client::Config {
        anonymous: true,
        preferred: russh::Preferred::anonymous_none(),
        ..client_config()
    }
}

pub struct ClientConnection {
    pub stream: ChannelStream<client::Msg>,
    pub(crate) _session: Arc<client::Handle<VerifiedHost>>,
    pub(crate) port: u16,
    pub(crate) observation_expiry: Option<tokio::time::Instant>,
    #[cfg(feature = "owner-auth")]
    pub(crate) owner_transport: Option<VerifiedPinnedChannel>,
}

#[cfg(feature = "owner-auth")]
pub(crate) struct VerifiedPinnedChannel {
    binding: russh::ChannelBinding,
    fingerprint: String,
}

#[cfg(feature = "owner-auth")]
impl VerifiedPinnedChannel {
    #[cfg(all(test, feature = "relay"))]
    pub(crate) fn from_fixture_stream(
        stream: &ChannelStream<client::Msg>,
        pinned_key: &str,
    ) -> io::Result<Option<Self>> {
        let fingerprint = host_fingerprint(pinned_key)?;
        Ok(stream.channel_binding().cloned().map(|binding| Self {
            binding,
            fingerprint,
        }))
    }
}

#[cfg(feature = "owner-auth")]
impl ClientConnection {
    pub fn require_owner_channel_binding(&self) -> Result<(), crate::owner_auth::OwnerKeyError> {
        self.owner_channel_binding().map(|_| ())
    }

    pub(crate) fn owner_channel_binding(
        &self,
    ) -> Result<&russh::ChannelBinding, crate::owner_auth::OwnerKeyError> {
        let captured = self
            .owner_transport
            .as_ref()
            .ok_or(crate::owner_auth::OwnerKeyError::ChannelBindingUnavailable)?;
        if self.stream.channel_binding() != Some(&captured.binding) {
            return Err(crate::owner_auth::OwnerKeyError::ChannelBindingUnavailable);
        }
        Ok(&captured.binding)
    }

    pub(crate) fn owner_pinned_fingerprint(
        &self,
    ) -> Result<&str, crate::owner_auth::OwnerKeyError> {
        self.owner_channel_binding()?;
        Ok(&self
            .owner_transport
            .as_ref()
            .ok_or(crate::owner_auth::OwnerKeyError::ChannelBindingUnavailable)?
            .fingerprint)
    }
}

impl tokio::io::AsyncRead for ClientConnection {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for ClientConnection {
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
pub fn host_fingerprint(key: &str) -> io::Result<String> {
    if key.len() > 4096 {
        return Err(invalid());
    }
    let key = russh::keys::parse_public_key_base64(key).map_err(|_| invalid())?;
    Ok(key.fingerprint(russh::keys::HashAlg::Sha256).to_string())
}

pub async fn connect(destination: &Destination, token: &str) -> io::Result<ClientConnection> {
    if destination.version != 1 {
        return Err(invalid());
    }
    connect_endpoint(&destination.endpoint, destination.port, token).await
}

// The stream owns the transport. A retained metadata/probe handle must not
// keep a retired attachment alive after detach or liveness failure.
struct HostProbe(std::sync::Weak<client::Handle<VerifiedHost>>);
impl crate::mobile::HostLiveness for HostProbe {
    fn check(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = io::Result<()>> + Send + '_>> {
        Box::pin(async {
            self.0
                .upgrade()
                .ok_or_else(|| stage_error("host.closed"))?
                .send_ping()
                .await
                .map_err(|_| stage_error("host.liveness"))
        })
    }
}

impl ClientConnection {
    pub(crate) fn observation_binding(&self) -> ForwardingBinding {
        ForwardingBinding {
            session: Arc::downgrade(&self._session),
            port: self.port,
        }
    }
    /// Bind portable observation to this verified transport and its attachment
    /// cancellation. Unknown token expiry refuses observation without affecting
    /// terminal transport.
    pub fn observation_requestor(
        &self,
        target: Target,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> crate::observation_client::ObservationRequestor {
        let (requestor, driver) =
            crate::observation_client::ObservationRequestor::pending(cancellation);
        driver.fence_expiry(self.observation_expiry);
        tokio::spawn(driver.run(self.observation_binding(), target));
        requestor
    }
    /// Shares only the authenticated host handle, without opening another stream.
    pub fn host_liveness(&self) -> Arc<dyn crate::mobile::HostLiveness> {
        Arc::new(HostProbe(Arc::downgrade(&self._session)))
    }

    pub(crate) async fn close(self) -> io::Result<()> {
        self._session
            .disconnect(russh::Disconnect::ByApplication, "complete", "")
            .await
            .map_err(|_| invalid())?;
        if let Ok(session) = Arc::try_unwrap(self._session) {
            let _ = tokio::time::timeout(Duration::from_secs(5), session)
                .await
                .map_err(|_| invalid())?;
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct ForwardingBinding {
    session: std::sync::Weak<client::Handle<VerifiedHost>>,
    port: u16,
}

impl ForwardingBinding {
    pub(crate) fn available(&self) -> bool {
        self.session.strong_count() != 0
    }
    pub(crate) async fn open(&self) -> io::Result<ChannelStream<client::Msg>> {
        // Only the bounded opening job retains a handle, never the provider or stream.
        let session = self.session.upgrade().ok_or_else(invalid)?;
        let stream = session
            .channel_open_direct_tcpip("127.0.0.1", u32::from(self.port), "127.0.0.1", 0)
            .await
            .map_err(|_| invalid())?
            .into_stream();
        drop(session);
        Ok(stream)
    }
}

pub async fn connect_endpoint(
    endpoint: &TunnelEndpoint,
    port: u16,
    token: &str,
) -> io::Result<ClientConnection> {
    if port == 0
        || endpoint.host_public_keys.len() != 1
        || endpoint.host_public_keys[0].is_empty()
        || endpoint.host_public_keys[0].len() > 4096
    {
        return Err(invalid());
    }
    let url = endpoint
        .tunnel_relay_tunnel_endpoint
        .client_relay_uri
        .as_deref()
        .ok_or_else(invalid)?;
    let rw = websocket(url, token, "tunnel-relay-client").await?;
    let mut session = client::connect_stream(
        Arc::new(client_config()),
        rw,
        VerifiedHost(endpoint.host_public_keys[0].clone()),
    )
    .await
    .map_err(|_| invalid())?;
    if !session
        .authenticate_none("tunnel")
        .await
        .map_err(|_| invalid())?
        .success()
    {
        return Err(invalid());
    }
    let stream = session
        .channel_open_direct_tcpip("127.0.0.1", port as u32, "127.0.0.1", 0)
        .await
        .map_err(|_| invalid())?
        .into_stream();
    #[cfg(feature = "owner-auth")]
    let owner_transport = {
        let fingerprint = host_fingerprint(&endpoint.host_public_keys[0])?;
        stream
            .channel_binding()
            .cloned()
            .map(|binding| VerifiedPinnedChannel {
                binding,
                fingerprint,
            })
    };
    Ok(ClientConnection {
        stream,
        _session: Arc::new(session),
        port,
        observation_expiry: crate::observation_client::ObservationDriver::token_expiry(token),
        #[cfg(feature = "owner-auth")]
        owner_transport,
    })
}

pub(crate) struct VerifiedHost(pub(crate) String);
impl client::Handler for VerifiedHost {
    type Error = russh::Error;
    reject_unrelated_server_channels!();
    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(key
            .to_openssh()
            .ok()
            .and_then(|text| text.split_whitespace().nth(1).map(str::to_owned))
            .is_some_and(|key| key == self.0))
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

#[cfg(feature = "relay")]
pub use crate::relay_host::*;

#[cfg(test)]
mod liveness_tests {
    use super::*;
    use crate::mobile::HostLiveness;
    use std::{
        pin::Pin,
        sync::atomic::{AtomicBool, Ordering},
        task::{Context, Poll},
    };
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    // Models a relay that accepts writes while the authenticated host is unreachable.
    struct Gate {
        stream: tokio::io::DuplexStream,
        blackhole: Arc<AtomicBool>,
    }
    impl AsyncRead for Gate {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if self.blackhole.load(Ordering::SeqCst) {
                Poll::Pending
            } else {
                Pin::new(&mut self.stream).poll_read(cx, buf)
            }
        }
    }
    impl AsyncWrite for Gate {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.blackhole.load(Ordering::SeqCst) {
                Poll::Ready(Ok(bytes.len()))
            } else {
                Pin::new(&mut self.stream).poll_write(cx, bytes)
            }
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.stream).poll_flush(cx)
        }
        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.stream).poll_shutdown(cx)
        }
    }
    struct Host;
    impl russh::server::Handler for Host {
        type Error = russh::Error;
        async fn auth_none(&mut self, _: &str) -> Result<russh::server::Auth, Self::Error> {
            Ok(russh::server::Auth::Accept)
        }
    }

    #[tokio::test]
    async fn retained_host_ping_requires_peer_reply_and_rejects_closed_transport() {
        let key = russh::keys::PrivateKey::random(
            &mut russh::keys::key::safe_rng(),
            russh::keys::Algorithm::Ed25519,
        )
        .unwrap();
        let pin = key
            .public_key()
            .to_openssh()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_owned();
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let config = Arc::new(russh::server::Config {
            keys: vec![key],
            ..Default::default()
        });
        let (tx, rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let running = russh::server::run_stream(config, server_io, Host)
                .await
                .unwrap();
            tx.send(running.handle()).unwrap();
            running.await
        });
        let blackhole = Arc::new(AtomicBool::new(false));
        let mut config = client_config();
        config.keepalive_interval = None;
        let mut client = client::connect_stream(
            Arc::new(config),
            Gate {
                stream: client_io,
                blackhole: blackhole.clone(),
            },
            VerifiedHost(pin),
        )
        .await
        .unwrap();
        assert!(client.authenticate_none("tunnel").await.unwrap().success());
        let host = rx.await.unwrap();
        let client = Arc::new(client);
        let probe = HostProbe(Arc::downgrade(&client));
        tokio::time::timeout(Duration::from_secs(2), probe.check())
            .await
            .unwrap()
            .unwrap();
        blackhole.store(true, Ordering::SeqCst);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), probe.check())
                .await
                .is_err(),
            "local enqueue/write is insufficient"
        );
        // Start a new ping and prove that it is awaiting its reply before
        // ending the host. This specifically covers russh's dropped sender bug.
        let pending = probe.check();
        tokio::pin!(pending);
        tokio::select! {
            result = &mut pending => panic!("ping completed without a host response: {result:?}"),
            () = tokio::time::sleep(Duration::from_millis(50)) => {},
        }
        blackhole.store(false, Ordering::SeqCst);
        host.disconnect(russh::Disconnect::ByApplication, "done".into(), "".into())
            .await
            .unwrap();
        // Wake the SSH loop after lifting the synthetic blackhole.
        let _ = client.send_keepalive(false).await;
        let result = tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .unwrap();
        assert!(result.is_err(), "a dropped reply channel is not a pong");
        let _ = server.await.unwrap();
    }
}
