// Relay handshake derived from microsoft/dev-tunnels (MIT), pinned in vendor/dev-tunnels.
//! Narrow modern-russh port of the pinned SDK relay handshake. Unlike the
//! upstream host, this admits one encrypted client and one configured port,
//! with no TCP listeners, port-management API, or unbounded forwarding queues.
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
    let websocket = connect_directly(request)
        .await
        .map_err(|_| stage_error("relay.websocket_upgrade"))?;
    Ok(AsyncRWWebSocket::new(AsyncRWWebSocketOptions {
        websocket,
        ping_interval: Duration::from_secs(30),
        ping_timeout: Duration::from_secs(10),
    }))
}

pub(crate) fn stage_error(stage: &'static str) -> io::Error {
    io::Error::other(stage)
}

#[cfg(feature = "relay")]
pub(crate) fn management_error(error: tunnels::management::HttpError) -> io::Error {
    use tunnels::management::HttpError;
    match error {
        HttpError::ResponseError(response) => io::Error::other(format!(
            "host.endpoint_update.http_{}",
            response.status_code.as_u16()
        )),
        HttpError::ConnectionError(error) if error.is_timeout() => {
            stage_error("host.endpoint_update.timeout")
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
    pub(crate) _session: client::Handle<VerifiedHost>,
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

impl ClientConnection {
    pub(crate) async fn close(self) -> io::Result<()> {
        self._session
            .disconnect(russh::Disconnect::ByApplication, "complete", "")
            .await
            .map_err(|_| invalid())?;
        tokio::time::timeout(Duration::from_secs(5), self._session)
            .await
            .map(|_| ())
            .map_err(|_| invalid())
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
    Ok(ClientConnection {
        stream,
        _session: session,
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
