#![cfg(feature = "relay")]

#[tokio::test]
async fn relay_tls_startup_with_both_crypto_providers_refuses_closed_peer_without_panicking() {
    let _ = rustls::crypto::ring::default_provider();
    let _ = rustls::crypto::aws_lc_rs::default_provider();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        drop(stream);
    });
    let request =
        tunnels::connections::build_websocket_request(&format!("wss://{address}/"), &[]).unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tunnels::connections::connect_directly(request),
    )
    .await
    .unwrap();
    peer.await.unwrap();
    assert!(result.is_err());
}
