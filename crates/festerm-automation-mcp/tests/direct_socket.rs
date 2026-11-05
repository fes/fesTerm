#![cfg(unix)]

use std::{
    os::unix::net::UnixStream,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use festerm_automation_mcp::secure_transport::{
    Error, NativeCleanupFailed, NativeDecision, NativeUnavailable, Progress, Role, SecureTransport,
    TrustedNativeVerification, MAX_FRAME_DURATION,
};

struct Native {
    display: SyncSender<String>,
    decision: Receiver<NativeDecision>,
    cancelled: Arc<AtomicBool>,
}

impl TrustedNativeVerification for Native {
    fn show_local_fingerprint(&mut self, fingerprint: &str) -> Result<(), NativeUnavailable> {
        self.display
            .try_send(fingerprint.to_owned())
            .map_err(|_| NativeUnavailable)
    }

    fn poll_decision(&mut self) -> NativeDecision {
        match self.decision.try_recv() {
            Ok(decision) => decision,
            Err(TryRecvError::Empty) => NativeDecision::Pending,
            Err(TryRecvError::Disconnected) => NativeDecision::Denied,
        }
    }

    fn cancel(&mut self) -> Result<(), NativeCleanupFailed> {
        self.cancelled.store(true, Ordering::Release);
        Ok(())
    }
}

fn native() -> (
    Native,
    Receiver<String>,
    SyncSender<NativeDecision>,
    Arc<AtomicBool>,
) {
    let (display_tx, display_rx) = mpsc::sync_channel(1);
    let (decision_tx, decision_rx) = mpsc::sync_channel(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    (
        Native {
            display: display_tx,
            decision: decision_rx,
            cancelled: cancelled.clone(),
        },
        display_rx,
        decision_tx,
        cancelled,
    )
}

fn compared(peer_display: &str) -> NativeDecision {
    NativeDecision::ComparedPeer(peer_display.as_bytes().try_into().unwrap())
}

#[test]
fn direct_socket_requires_both_comparisons_and_closes_on_peer_loss() {
    let (client_socket, app_socket) = UnixStream::pair().unwrap();
    client_socket.set_nonblocking(true).unwrap();
    app_socket.set_nonblocking(true).unwrap();
    let (client_native, client_display, client_decision, client_cancelled) = native();
    let (app_native, app_display, app_decision, app_cancelled) = native();
    let mut client = SecureTransport::new(
        client_socket,
        Role::Client,
        client_native,
        Duration::from_secs(5),
        MAX_FRAME_DURATION,
    )
    .unwrap();
    let mut app = SecureTransport::new(
        app_socket,
        Role::Application,
        app_native,
        Duration::from_secs(5),
        MAX_FRAME_DURATION,
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut client_fingerprint = None;
    let mut app_fingerprint = None;
    while client_fingerprint.is_none() || app_fingerprint.is_none() {
        assert!(Instant::now() < deadline, "socket handshake timed out");
        assert!(!matches!(
            client.poll(|| Ok(())).unwrap(),
            Progress::Frame(_)
        ));
        assert!(!matches!(app.poll(|| Ok(())).unwrap(), Progress::Frame(_)));
        client_fingerprint = client_fingerprint.or_else(|| client_display.try_recv().ok());
        app_fingerprint = app_fingerprint.or_else(|| app_display.try_recv().ok());
        thread::yield_now();
    }
    let client_fingerprint = client_fingerprint.unwrap();
    let app_fingerprint = app_fingerprint.unwrap();
    assert_eq!(client_fingerprint, app_fingerprint);

    client_decision.send(compared(&app_fingerprint)).unwrap();
    for _ in 0..10 {
        assert!(!matches!(
            client.poll(|| Ok(())).unwrap(),
            Progress::Frame(_)
        ));
        assert!(!matches!(app.poll(|| Ok(())).unwrap(), Progress::Frame(_)));
    }
    assert!(!client.is_established());
    assert!(!app.is_established());
    assert_eq!(
        client.prepare_send(b"not yet", Instant::now() + MAX_FRAME_DURATION),
        Err(Error::NotEstablished)
    );

    app_decision.send(compared(&client_fingerprint)).unwrap();
    while !client.is_established() || !app.is_established() {
        assert!(Instant::now() < deadline, "socket confirmation timed out");
        assert!(!matches!(
            client.poll(|| Ok(())).unwrap(),
            Progress::Frame(_)
        ));
        assert!(!matches!(app.poll(|| Ok(())).unwrap(), Progress::Frame(_)));
        thread::yield_now();
    }

    client
        .prepare_send(b"direct request", Instant::now() + MAX_FRAME_DURATION)
        .unwrap();
    let mut sent = false;
    let mut received = None;
    while !sent || received.is_none() {
        assert!(Instant::now() < deadline, "socket request timed out");
        sent |= matches!(client.poll(|| Ok(())).unwrap(), Progress::Sent);
        if let Progress::Frame(frame) = app.poll(|| Ok(())).unwrap() {
            received = Some(frame);
        }
        thread::yield_now();
    }
    assert_eq!(received.as_deref(), Some(b"direct request".as_slice()));

    app.prepare_send(b"direct response", Instant::now() + MAX_FRAME_DURATION)
        .unwrap();
    let mut sent = false;
    let mut received = None;
    while !sent || received.is_none() {
        assert!(Instant::now() < deadline, "socket response timed out");
        sent |= matches!(app.poll(|| Ok(())).unwrap(), Progress::Sent);
        if let Progress::Frame(frame) = client.poll(|| Ok(())).unwrap() {
            received = Some(frame);
        }
        thread::yield_now();
    }
    assert_eq!(received.as_deref(), Some(b"direct response".as_slice()));

    client.close().unwrap();
    assert!(client_cancelled.load(Ordering::Acquire));
    loop {
        assert!(Instant::now() < deadline, "peer loss was not observed");
        match app.poll(|| Ok(())) {
            Err(Error::PeerClosed) => break,
            Ok(Progress::Blocked | Progress::Advanced) => thread::yield_now(),
            other => panic!("unexpected peer-loss result: {other:?}"),
        }
    }
    assert!(app_cancelled.load(Ordering::Acquire));
}
