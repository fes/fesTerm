// Relay handshake derived from microsoft/dev-tunnels (MIT), pinned in vendor/dev-tunnels.
//! Narrow modern-russh port of the pinned SDK relay handshake. Unlike the
//! upstream host, this admits one encrypted client and one configured port,
//! with no TCP listeners, port-management API, or unbounded forwarding queues.
use std::{io, sync::Arc, time::Duration};

use russh::{client, server, Channel, ChannelId, ChannelStream};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use tunnels::{
    connections::{
        build_websocket_request, connect_directly, AsyncRWWebSocket, AsyncRWWebSocketOptions,
    },
    contracts::{TunnelConnectionMode, TunnelEndpoint},
    management::{new_tunnel_management, Authorization, TunnelLocator, TunnelRequestOptions},
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

async fn websocket(
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

fn stage_error(stage: &'static str) -> io::Error {
    io::Error::other(stage)
}

fn management_error(error: tunnels::management::HttpError) -> io::Error {
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

fn client_config() -> client::Config {
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

fn outer_client_config() -> client::Config {
    client::Config {
        anonymous: true,
        preferred: russh::Preferred::anonymous_none(),
        ..client_config()
    }
}

pub struct ClientConnection {
    pub stream: ChannelStream<client::Msg>,
    _session: client::Handle<VerifiedHost>,
}

pub async fn connect(destination: &Destination, token: &str) -> io::Result<ClientConnection> {
    if destination.version != 1
        || destination.port == 0
        || destination.endpoint.host_public_keys.len() != 1
        || destination.endpoint.host_public_keys[0].len() > 4096
    {
        return Err(invalid());
    }
    let url = destination
        .endpoint
        .tunnel_relay_tunnel_endpoint
        .client_relay_uri
        .as_deref()
        .ok_or_else(invalid)?;
    let rw = websocket(url, token, "tunnel-relay-client").await?;
    let mut session = client::connect_stream(
        Arc::new(client_config()),
        rw,
        VerifiedHost(destination.endpoint.host_public_keys[0].clone()),
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
        .channel_open_direct_tcpip("127.0.0.1", destination.port as u32, "127.0.0.1", 0)
        .await
        .map_err(|_| invalid())?
        .into_stream();
    Ok(ClientConnection {
        stream,
        _session: session,
    })
}

struct VerifiedHost(String);
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

pub struct HostConnection {
    pub endpoint: TunnelEndpoint,
    pub connections: mpsc::Receiver<ChannelStream<server::Msg>>,
    cancel: CancellationToken,
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
    if port == 0
        || ![tunnel_id, cluster].iter().all(|s| {
            !s.is_empty()
                && s.len() <= 100
                && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
    {
        return Err(stage_error("host.arguments"));
    }
    let key = russh::keys::PrivateKey::random(
        &mut russh::keys::key::safe_rng(),
        russh::keys::Algorithm::Ed25519,
    )
    .map_err(|_| stage_error("host.key_generation"))?;
    let public = key
        .public_key()
        .to_openssh()
        .map_err(|_| stage_error("host.key_encoding"))?;
    let host_id = uuid::Uuid::new_v4().to_string();
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
    if endpoint.host_public_keys != request.host_public_keys {
        return Err(stage_error("host.endpoint_key_mismatch"));
    }
    let url = endpoint
        .tunnel_relay_tunnel_endpoint
        .host_relay_uri
        .as_deref()
        .ok_or_else(|| stage_error("host.endpoint_uri_missing"))?;
    let rw = websocket(url, token, "tunnel-relay-host").await?;
    let (outer_tx, mut outer_rx) = mpsc::channel(1);
    let slots = Arc::new(Semaphore::new(1));
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
    .map_err(|_| stage_error("host.outer_ssh"))?;
    let cancel = CancellationToken::new();
    let stopped = cancel.clone();
    let (tx, connections) = mpsc::channel(1);
    tokio::spawn(async move {
        let server_config = Arc::new(server::Config {
            keys: vec![key],
            window_size: 256 * 1024,
            maximum_packet_size: 32 * 1024,
            channel_buffer_size: 8,
            event_buffer_size: 8,
            inactivity_timeout: Some(Duration::from_secs(120)),
            ..Default::default()
        });
        loop {
            let incoming = tokio::select! {
                _ = stopped.cancelled() => break,
                value = outer_rx.recv() => match value { Some(value) => value, None => break },
            };
            let (channel, permit) = incoming;
            let (authed, adopted) = oneshot::channel();
            let handler = Port {
                port,
                tx: tx.clone(),
                opened: false,
                authed: Some(authed),
            };
            // Only one encrypted client is serviced; keeping its permit until
            // completion also prevents an upstream per-client task backlog.
            let result = async {
                let mut running = server::run_stream(server_config.clone(), channel, handler)
                    .await
                    .map_err(|_| invalid())?;
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(30)) => return Err(invalid()),
                    _ = &mut running => return Ok(()),
                    result = adopted => { result.map_err(|_| invalid())?; }
                }
                running
                    .handle()
                    .forward_tcpip("127.0.0.1".into(), port as u32)
                    .await
                    .map_err(|_| invalid())?;
                tokio::select! {
                    _ = stopped.cancelled() => {}
                    _ = &mut running => {}
                }
                Ok::<_, io::Error>(())
            };
            tokio::select! { _ = stopped.cancelled() => break, _ = result => {} }
            drop(permit);
        }
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            session.disconnect(russh::Disconnect::ByApplication, "bridge closed", "en"),
        )
        .await;
    });
    Ok(HostConnection {
        endpoint,
        connections,
        cancel,
    })
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
    tx: mpsc::Sender<ChannelStream<server::Msg>>,
    opened: bool,
    authed: Option<oneshot::Sender<()>>,
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
        if self.opened || port != self.port as u32 || host != "127.0.0.1" {
            return Ok(());
        }
        self.opened = true;
        reply.accept().await;
        let _ = self.tx.try_send(channel.into_stream());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
        let config = Arc::new(server::Config {
            keys: vec![key],
            channel_buffer_size: 8,
            ..Default::default()
        });
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (tx, mut rx) = mpsc::channel(1);
        let server = tokio::spawn(async move {
            server::run_stream(
                config,
                server_io,
                Port {
                    port: 47461,
                    tx,
                    opened: false,
                    authed: None,
                },
            )
            .await
            .unwrap()
            .await
        });
        let mut client =
            client::connect_stream(Arc::new(client_config()), client_io, VerifiedHost(expected))
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
        assert!(client
            .channel_open_direct_tcpip("127.0.0.1", 47461, "127.0.0.1", 0)
            .await
            .is_err());
        client
            .disconnect(russh::Disconnect::ByApplication, "test complete", "en")
            .await
            .unwrap();
        drop(accepted);
        drop(channel);
        server.abort();
    }

    #[tokio::test]
    async fn encrypted_transport_refuses_changed_host_key_before_authentication() {
        let config = Arc::new(server::Config {
            keys: vec![key()],
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
                    opened: false,
                    authed: None,
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
}
