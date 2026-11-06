use std::sync::Arc;
use std::time::Duration;

use russh::{client, server};
use tokio::sync::oneshot;

struct TestClient {
    host_key: russh::keys::PublicKey,
}

impl client::Handler for TestClient {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(key == &self.host_key)
    }
}

struct TestServer {
    reverse_result: Option<oneshot::Sender<Option<u32>>>,
}

impl server::Handler for TestServer {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }

    async fn auth_succeeded(&mut self, session: &mut server::Session) -> Result<(), Self::Error> {
        session.tcpip_forward("127.0.0.1", 2222, None)?;
        session.keepalive_request()?;
        session.tcpip_forward("127.0.0.1", 2222, self.reverse_result.take())?;
        Ok(())
    }

    async fn tcpip_forward(
        &mut self,
        _address: &str,
        port: &mut u32,
        _session: &mut server::Session,
    ) -> Result<bool, Self::Error> {
        if *port == 0 {
            *port = 4242;
        }
        Ok(true)
    }
}

#[test]
fn mixed_global_requests_correlate_over_a_real_duplex_connection_in_both_directions() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            let key = russh::keys::PrivateKey::random(
                &mut russh::keys::key::safe_rng(),
                russh::keys::Algorithm::Ed25519,
            )
            .unwrap();
            let expected_key = key.public_key().clone();
            let server_config = Arc::new(server::Config {
                keys: vec![key],
                inactivity_timeout: None,
                ..Default::default()
            });
            let (client_stream, server_stream) = tokio::io::duplex(128 * 1024);
            let (reverse_sender, reverse_result) = oneshot::channel();
            let server_task = tokio::spawn(server::run_stream(
                server_config,
                server_stream,
                TestServer {
                    reverse_result: Some(reverse_sender),
                },
            ));
            let mut client = client::connect_stream(
                Arc::new(client::Config::default()),
                client_stream,
                TestClient {
                    host_key: expected_key,
                },
            )
            .await
            .unwrap();
            let server = server_task.await.unwrap().unwrap();
            assert!(client.authenticate_none("fixture").await.unwrap().success());

            // The client rejects forwarding requests but accepts keepalives.
            // A stray failure for the first no-reply request would consume
            // the keepalive's slot and turn the next success into a false
            // forwarding success.
            assert_eq!(reverse_result.await.unwrap(), None);

            client.send_keepalive(false).await.unwrap();
            client.no_more_sessions(false).await.unwrap();
            client.send_keepalive(true).await.unwrap();
            assert_eq!(client.tcpip_forward("127.0.0.1", 0).await.unwrap(), 4242);
            client.send_ping().await.unwrap();

            client
                .disconnect(russh::Disconnect::ByApplication, "fixture complete", "")
                .await
                .unwrap();
            let _ = client.await;
            let _ = server.await;
        })
        .await
        .expect("global request fixture timed out");
    });
}
