use std::borrow::Cow;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use russh::{client, server, Channel, ChannelId, ChannelMsg};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio::sync::{mpsc, oneshot};

const WINDOW: u32 = 64;
const FIRST: &[u8] = b"first";
const SECOND: &[u8] = b"0123456789ABCDEF0123456789ABCDEF";
const OTHER: &[u8] = b"other";
const CUT_DESCRIPTION: &str = "pressure fixture cut";
const WAIT: Duration = Duration::from_secs(10);

struct WriteWitness {
    armed: Arc<AtomicBool>,
    flushed: Option<oneshot::Sender<()>>,
    shutdown: Option<oneshot::Sender<bool>>,
    unflushed: bool,
}

struct TrackedStream {
    inner: DuplexStream,
    witness: Option<WriteWitness>,
    dropped: Arc<AtomicBool>,
}

impl Drop for TrackedStream {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl AsyncRead for TrackedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for TrackedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let (Poll::Ready(Ok(bytes)), Some(witness)) = (&result, self.witness.as_mut()) {
            witness.unflushed |= *bytes > 0;
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_flush(cx);
        let flushed = match (&result, self.witness.as_mut()) {
            (Poll::Ready(Ok(())), Some(witness)) => {
                witness.unflushed = false;
                if witness.armed.swap(false, Ordering::SeqCst) {
                    witness.flushed.take()
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(flushed) = flushed {
            let _ = flushed.send(());
        }
        result
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_shutdown(cx);
        let shutdown = match (&result, self.witness.as_mut()) {
            (Poll::Ready(Ok(())), Some(witness)) => witness
                .shutdown
                .take()
                .map(|shutdown| (shutdown, !witness.unflushed)),
            _ => None,
        };
        if let Some((shutdown, all_flushed)) = shutdown {
            let _ = shutdown.send(all_flushed);
        }
        result
    }
}

struct PressureClient {
    host_key: russh::keys::PublicKey,
    data: mpsc::Sender<(ChannelId, Vec<u8>)>,
    second_decoded: Option<oneshot::Sender<(ChannelId, u32)>>,
    disconnected: Option<oneshot::Sender<client::RemoteDisconnectInfo>>,
}

impl client::Handler for PressureClient {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(key == &self.host_key)
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        self.data
            .try_send((channel, data.to_vec()))
            .map_err(|_| russh::Error::SendError)
    }

    fn adjust_window(&mut self, channel: ChannelId, window: u32) -> u32 {
        if let Some(decoded) = self.second_decoded.take() {
            let _ = decoded.send((channel, window));
        }
        window
    }

    async fn disconnected(
        &mut self,
        reason: client::DisconnectReason<Self::Error>,
    ) -> Result<(), Self::Error> {
        match reason {
            client::DisconnectReason::ReceivedDisconnect(info) => {
                if let Some(disconnected) = self.disconnected.take() {
                    disconnected
                        .send(info)
                        .map_err(|_| russh::Error::SendError)?;
                }
                Ok(())
            }
            client::DisconnectReason::Error(error) => Err(error),
        }
    }
}

struct PressureServer {
    channels: Vec<Channel<server::Msg>>,
    channel_ids: Option<oneshot::Sender<(ChannelId, ChannelId)>>,
}

struct PressureSignals {
    flush_armed: Arc<AtomicBool>,
    flushed: oneshot::Receiver<()>,
    shutdown: oneshot::Receiver<bool>,
    park_armed: Arc<AtomicBool>,
    parked: oneshot::Receiver<()>,
}

impl server::Handler for PressureServer {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if self.channels.len() == 2 {
            return Err(russh::Error::Inconsistent);
        }
        self.channels.push(channel);
        if self.channels.len() == 2 {
            self.channel_ids
                .take()
                .ok_or(russh::Error::Inconsistent)?
                .send((self.channels[0].id(), self.channels[1].id()))
                .map_err(|_| russh::Error::SendError)?;
        }
        reply.accept().await;
        Ok(())
    }

    async fn tcpip_forward(
        &mut self,
        _address: &str,
        port: &mut u32,
        session: &mut server::Session,
    ) -> Result<bool, Self::Error> {
        let Some(first) = self.channels.first() else {
            return Err(russh::Error::Inconsistent);
        };
        let first = first.id();
        match *port {
            2222 => {
                session.data(first, FIRST)?;
                Ok(true)
            }
            3333 => {
                session.data(first, SECOND)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

async fn within<F: Future>(name: &str, future: F) -> F::Output {
    tokio::time::timeout(WAIT, future)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {name}"))
}

async fn poll_once<F: Future + ?Sized>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}

fn expect_data(message: Option<ChannelMsg>, expected: &[u8]) {
    match message {
        Some(ChannelMsg::Data { data }) => assert_eq!(data.as_ref(), expected),
        other => panic!("expected channel data {expected:?}, got {other:?}"),
    }
}

async fn exercise(
    client_stream: TrackedStream,
    server_stream: TrackedStream,
    signals: PressureSignals,
    saturated: bool,
) {
    let PressureSignals {
        flush_armed,
        flushed,
        shutdown,
        park_armed,
        parked,
    } = signals;
    let key = russh::keys::PrivateKey::random(
        &mut russh::keys::key::safe_rng(),
        russh::keys::Algorithm::Ed25519,
    )
    .unwrap();
    let host_key = key.public_key().clone();
    let server_config = Arc::new(server::Config {
        keys: vec![key],
        inactivity_timeout: None,
        ..Default::default()
    });
    let (channel_ids_tx, channel_ids_rx) = oneshot::channel();
    let server_task = tokio::spawn(server::run_stream(
        server_config,
        server_stream,
        PressureServer {
            channels: Vec::new(),
            channel_ids: Some(channel_ids_tx),
        },
    ));
    let (data_tx, mut data_rx) = mpsc::channel(4);
    let (decoded_tx, decoded_rx) = oneshot::channel();
    let (disconnected_tx, disconnected_rx) = oneshot::channel();
    let mut config = client::Config {
        channel_buffer_size: if saturated { 1 } else { 2 },
        window_size: WINDOW,
        ..Default::default()
    };
    config.preferred.cipher = Cow::Borrowed(&[russh::cipher::CHACHA20_POLY1305]);
    config.preferred.compression = Cow::Borrowed(&[russh::compression::NONE]);
    let mut client = within(
        "client handshake",
        client::connect_stream(
            Arc::new(config),
            client_stream,
            PressureClient {
                host_key,
                data: data_tx,
                second_decoded: Some(decoded_tx),
                disconnected: Some(disconnected_tx),
            },
        ),
    )
    .await
    .unwrap();
    let server = within("server handshake", server_task)
        .await
        .unwrap()
        .unwrap();
    assert!(
        within("authentication", client.authenticate_none("fixture"))
            .await
            .unwrap()
            .success()
    );

    let mut first = within("first channel open", client.channel_open_session())
        .await
        .unwrap();
    let mut other = within("other channel open", client.channel_open_session())
        .await
        .unwrap();
    let first_id = first.id();
    let other_id = other.id();
    assert_ne!(first_id, other_id);
    let (first_server_id, other_server_id) = within("server channel identities", channel_ids_rx)
        .await
        .unwrap();
    assert_ne!(first_server_id, other_server_id);

    assert_eq!(
        within(
            "first server response",
            client.tcpip_forward("127.0.0.1", 2222)
        )
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        within("first delivery callback", data_rx.recv()).await,
        Some((first_id, FIRST.to_vec()))
    );
    let mut decoded = Box::pin(decoded_rx);
    assert!(poll_once(decoded.as_mut()).await.is_pending());

    // The second crosses the window threshold, so adjust_window observes it
    // after decryption and before delivery to the channel receiver.
    let mut response = Box::pin(client.tcpip_forward("127.0.0.1", 3333));
    let mut early_response = None;
    let (decoded_id, decoded_window) = within("second packet decoded", async {
        tokio::select! {
            result = response.as_mut() => {
                assert!(!saturated, "global reply escaped pressure: {result:?}");
                early_response = Some(result);
                decoded.as_mut().await.unwrap()
            }
            result = decoded.as_mut() => result.unwrap(),
        }
    })
    .await;
    assert_eq!((decoded_id, decoded_window), (first_id, WINDOW));

    // With capacity one the first packet is still in the receiver, so the
    // client cannot deliver SECOND or process the subsequent global response.
    if saturated {
        assert!(poll_once(response.as_mut()).await.is_pending());
    }
    flush_armed.store(true, Ordering::SeqCst);
    let server_handle = server.handle();
    within(
        "unrelated server output queued",
        server_handle.data(other_server_id, OTHER),
    )
    .await
    .unwrap();
    within(
        "server disconnect queued",
        server_handle.disconnect(
            russh::Disconnect::ByApplication,
            CUT_DESCRIPTION.to_owned(),
            String::new(),
        ),
    )
    .await
    .unwrap();
    within("post-saturation server flush", flushed)
        .await
        .unwrap();
    // The server flushes its disconnect before shutting down its write half;
    // the wrapper also checks that no accepted write was left unflushed.
    assert!(
        within("server write-half shutdown", shutdown)
            .await
            .unwrap(),
        "server shutdown left unflushed writes"
    );

    // The locked Tokio current-thread scheduler calls the park hook after its
    // runnable and deferred queues are empty. Duplex writes wake readers
    // directly, so this gives already-runnable client work a chance to run.
    park_armed.store(true, Ordering::SeqCst);
    within("client scheduler quiescence", parked).await.unwrap();

    let mut other_wait = Box::pin(other.wait());
    let mut disconnected = Box::pin(disconnected_rx);
    if saturated {
        assert!(poll_once(other_wait.as_mut()).await.is_pending());
        assert!(poll_once(response.as_mut()).await.is_pending());
        assert!(poll_once(disconnected.as_mut()).await.is_pending());

        expect_data(within("first retained output", first.wait()).await, FIRST);
        expect_data(within("second retained output", first.wait()).await, SECOND);
    }
    expect_data(
        within("other channel output", other_wait.as_mut()).await,
        OTHER,
    );
    drop(other_wait);
    let response_result = match early_response {
        Some(result) => result,
        None => within("global response", response.as_mut()).await,
    };
    assert_eq!(response_result.unwrap(), 0);
    drop(response);
    let disconnect = within("peer disconnect after drain", disconnected.as_mut())
        .await
        .unwrap();
    assert!(matches!(
        disconnect.reason_code,
        russh::Disconnect::ByApplication
    ));
    assert_eq!(disconnect.message, CUT_DESCRIPTION);
    assert!(disconnect.lang_tag.is_empty());
    drop(disconnected);
    if !saturated {
        expect_data(within("first retained output", first.wait()).await, FIRST);
        expect_data(within("second retained output", first.wait()).await, SECOND);
    }
    assert_eq!(
        within("second delivery callback", data_rx.recv()).await,
        Some((first_id, SECOND.to_vec()))
    );
    assert_eq!(
        within("other delivery callback", data_rx.recv()).await,
        Some((other_id, OTHER.to_vec()))
    );

    within("client task retirement", client).await.unwrap();
    within("server task retirement", server).await.unwrap();
    assert!(within("first channel retirement", first.wait())
        .await
        .is_none());
    assert!(within("other channel retirement", other.wait())
        .await
        .is_none());
    assert!(within("callback retirement", data_rx.recv())
        .await
        .is_none());
}

fn run_fixture(saturated: bool) {
    let park_armed = Arc::new(AtomicBool::new(false));
    let armed_for_hook = park_armed.clone();
    let (parked_tx, parked_rx) = oneshot::channel();
    let parked_tx = Mutex::new(Some(parked_tx));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .on_thread_park(move || {
            if armed_for_hook.swap(false, Ordering::SeqCst) {
                if let Some(parked) = parked_tx.lock().unwrap().take() {
                    let _ = parked.send(());
                }
            }
        })
        .build()
        .unwrap();
    let (client, server) = tokio::io::duplex(128 * 1024);
    let client_dropped = Arc::new(AtomicBool::new(false));
    let server_dropped = Arc::new(AtomicBool::new(false));
    let flush_armed = Arc::new(AtomicBool::new(false));
    let (flushed_tx, flushed_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let client = TrackedStream {
        inner: client,
        witness: None,
        dropped: client_dropped.clone(),
    };
    let server = TrackedStream {
        inner: server,
        witness: Some(WriteWitness {
            armed: flush_armed.clone(),
            flushed: Some(flushed_tx),
            shutdown: Some(shutdown_tx),
            unflushed: false,
        }),
        dropped: server_dropped.clone(),
    };
    // Session handles detach their private tasks on drop. A dedicated runtime
    // retires them even if the fixture panics, and the flags verify socket drops.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(60),
                exercise(
                    client,
                    server,
                    PressureSignals {
                        flush_armed,
                        flushed: flushed_rx,
                        shutdown: shutdown_rx,
                        park_armed,
                        parked: parked_rx,
                    },
                    saturated,
                ),
            )
            .await
            .expect("pressure fixture timed out");
        });
    }));
    runtime.shutdown_timeout(Duration::from_secs(2));
    assert!(
        client_dropped.load(Ordering::SeqCst),
        "client stream leaked"
    );
    assert!(
        server_dropped.load(Ordering::SeqCst),
        "server stream leaked"
    );
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[test]
fn saturated_inbound_channel_blocks_unrelated_data_reply_and_peer_cut_until_drained() {
    run_fixture(true);
}

#[test]
fn unsaturated_inbound_channel_allows_unrelated_data_reply_and_peer_cut_before_drain() {
    run_fixture(false);
}
