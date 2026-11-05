#![cfg(unix)]

use std::{
    collections::VecDeque,
    io,
    os::unix::net::UnixStream as StdUnixStream,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self as std_mpsc, Receiver, SyncSender, TryRecvError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use festerm_automation_mcp::{
    secure_transport::{
        NativeCleanupFailed, NativeDecision, NativeUnavailable, Progress, Role, SecureTransport,
        TrustedNativeVerification, MAX_FRAME_DURATION,
    },
    BackendResult, Event, InputDisposition, Machine, Operation, Refusal,
};
use festerm_automation_mcp_client::{
    AdmissionCleanupFailed, BoundaryWake, ClientWriteAdmission, ConnectorStatus,
    DirectClientTransport, Error,
};
use rmcp::{
    model::{CallToolRequestParams, ClientConfig, ClientJsonRpcMessage, ProtocolVersion},
    transport::Transport,
    ServiceExt,
};
use serde_json::json;
use tokio::{net::UnixStream, sync::mpsc, time::timeout};

const TEST_TIMEOUT: Duration = Duration::from_secs(8);

struct Native {
    display: mpsc::Sender<String>,
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

struct Admission(Arc<AtomicBool>);

impl ClientWriteAdmission for Admission {
    type Guard<'a> = ();

    fn try_admit(&self) -> io::Result<Self::Guard<'_>> {
        if self.0.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }

    fn cancel(&mut self) -> Result<(), AdmissionCleanupFailed> {
        Ok(())
    }
}

fn native() -> (
    Native,
    mpsc::Receiver<String>,
    SyncSender<NativeDecision>,
    Arc<AtomicBool>,
) {
    let (display_tx, display_rx) = mpsc::channel(1);
    let (decision_tx, decision_rx) = std_mpsc::sync_channel(1);
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

struct Harness {
    client: Option<DirectClientTransport>,
    status: ConnectorStatus,
    host: Option<SecureTransport<StdUnixStream, Native>>,
    client_display: mpsc::Receiver<String>,
    host_display: mpsc::Receiver<String>,
    client_decision: SyncSender<NativeDecision>,
    host_decision: SyncSender<NativeDecision>,
    wake: BoundaryWake,
    client_cancelled: Arc<AtomicBool>,
    host_cancelled: Arc<AtomicBool>,
    admission_open: Arc<AtomicBool>,
}

impl Harness {
    fn new(send_buffer: Option<usize>) -> Self {
        Self::with_admission(send_buffer, true)
    }

    fn with_admission(send_buffer: Option<usize>, admission_open: bool) -> Self {
        let (client_socket, host_socket) = UnixStream::pair().unwrap();
        if let Some(size) = send_buffer {
            socket2::SockRef::from(&client_socket)
                .set_send_buffer_size(size)
                .unwrap();
        }
        let host_socket = host_socket.into_std().unwrap();
        let (client_native, client_display, client_decision, client_cancelled) = native();
        let (host_native, host_display, host_decision, host_cancelled) = native();
        let (wake_tx, wake_rx) = std_mpsc::sync_channel(1);
        let admission_open = Arc::new(AtomicBool::new(admission_open));
        let admission = admission_open.clone();
        let (client, status) = DirectClientTransport::new(
            client_socket,
            TEST_TIMEOUT,
            MAX_FRAME_DURATION,
            move |wake| {
                wake_tx.try_send(wake).unwrap();
                (client_native, Admission(admission))
            },
        )
        .unwrap();
        let host = SecureTransport::new(
            host_socket,
            Role::Application,
            host_native,
            TEST_TIMEOUT,
            MAX_FRAME_DURATION,
        )
        .unwrap();
        Self {
            client: Some(client),
            status,
            host: Some(host),
            client_display,
            host_display,
            client_decision,
            host_decision,
            wake: wake_rx.recv().unwrap(),
            client_cancelled,
            host_cancelled,
            admission_open,
        }
    }

    async fn fingerprints(&mut self) -> (String, String) {
        let client = timeout(TEST_TIMEOUT, self.client_display.recv())
            .await
            .unwrap()
            .unwrap();
        let host = timeout(TEST_TIMEOUT, self.host_display.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(client, host);
        (client, host)
    }

    fn approve_client(&self, host_fingerprint: &str) {
        self.client_decision
            .try_send(NativeDecision::ComparedPeer(
                host_fingerprint.as_bytes().try_into().unwrap(),
            ))
            .unwrap();
        self.wake.wake();
    }

    fn approve_host(&self, client_fingerprint: &str) {
        self.host_decision
            .try_send(NativeDecision::ComparedPeer(
                client_fingerprint.as_bytes().try_into().unwrap(),
            ))
            .unwrap();
    }
}

fn serve_machine(
    mut host: SecureTransport<StdUnixStream, Native>,
    inputs: Arc<AtomicUsize>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let deadline = Instant::now() + TEST_TIMEOUT;
        let mut machine = Machine::with_direct_input_catalog();
        let mut responses = VecDeque::<Vec<u8>>::new();
        loop {
            assert!(Instant::now() < deadline, "synthetic host timed out");
            if host.is_established() && !host.has_pending_output() {
                if let Some(response) = responses.pop_front() {
                    host.prepare_send(&response, Instant::now() + MAX_FRAME_DURATION)
                        .unwrap();
                }
            }
            match host.poll(|| Ok(())) {
                Ok(Progress::Frame(frame)) => {
                    let response = match machine.process_bytes(&frame) {
                        Event::Reply(response) => Some(response),
                        Event::Backend(request) => {
                            let result = match request.operation {
                                Operation::ControllerCapabilities => {
                                    BackendResult::ControllerCapabilities {
                                        enrollment_available: true,
                                        metadata_available: true,
                                        input_available: true,
                                    }
                                }
                                Operation::TerminalSendText { .. } => {
                                    inputs.fetch_add(1, Ordering::AcqRel);
                                    BackendResult::InputDisposition(
                                        InputDisposition::OutcomeUnknown,
                                    )
                                }
                                _ => BackendResult::Refused(Refusal::Unavailable),
                            };
                            machine.complete(&request.key, result)
                        }
                        Event::Cancelled(_) | Event::NoReply => None,
                    };
                    if let Some(response) = response {
                        assert!(responses.len() < 4);
                        responses.push_back(response.into_bytes());
                    }
                }
                Ok(_) => {}
                Err(festerm_automation_mcp::secure_transport::Error::PeerClosed) => break,
                Err(error) => panic!("synthetic host failed: {error}"),
            }
            thread::sleep(Duration::from_millis(1));
        }
        machine.close();
    })
}

fn notification() -> ClientJsonRpcMessage {
    serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }))
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn official_sdk_uses_confirmed_encrypted_socket_without_replay() {
    let mut harness = Harness::new(None);
    let inputs = Arc::new(AtomicUsize::new(0));
    let host = serve_machine(harness.host.take().unwrap(), inputs.clone());
    let transport = harness.client.take().unwrap();
    let mut sdk = tokio::spawn(async move {
        let mut config = ClientConfig::default();
        config.protocol_version = ProtocolVersion::V_2025_11_25;
        config.serve(transport).await
    });

    let (client_fingerprint, host_fingerprint) = harness.fingerprints().await;
    harness.approve_client(&host_fingerprint);
    assert!(timeout(Duration::from_millis(50), &mut sdk).await.is_err());
    harness.approve_host(&client_fingerprint);
    let client = timeout(TEST_TIMEOUT, sdk).await.unwrap().unwrap().unwrap();
    assert_eq!(
        client.peer_info().unwrap().protocol_version,
        ProtocolVersion::V_2025_11_25
    );
    let tools = client.list_all_tools().await.unwrap();
    assert!(tools
        .iter()
        .any(|tool| tool.name == "festerm_terminal_send_text"));
    let capabilities = client
        .call_tool(
            CallToolRequestParams::new("festerm_controller_capabilities")
                .with_arguments(serde_json::Map::new()),
        )
        .await
        .unwrap()
        .structured_content
        .unwrap();
    assert_eq!(capabilities["data"]["input_available"], true);

    let unknown = client
        .call_tool(
            CallToolRequestParams::new("festerm_terminal_send_text").with_arguments(
                json!({"enrollment_ref": "a".repeat(32), "text": "one intentional action"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(unknown.is_error, Some(true));
    assert_eq!(
        unknown.structured_content.unwrap()["data"]["disposition"],
        "outcome_unknown"
    );
    assert_eq!(inputs.load(Ordering::Acquire), 1);
    client.cancel().await.unwrap();
    assert_eq!(
        timeout(TEST_TIMEOUT, harness.status.wait()).await.unwrap(),
        Ok(())
    );
    host.join().unwrap();
    assert!(!harness.status.cleanup_failed());
    assert!(harness.client_cancelled.load(Ordering::Acquire));
    assert!(harness.host_cancelled.load(Ordering::Acquire));
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_a_queued_send_closes_without_sending_an_application_frame() {
    let mut harness = Harness::new(None);
    let mut host = harness.host.take().unwrap();
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            assert!(Instant::now() < deadline, "peer close timed out");
            match host.poll(|| Ok(())) {
                Ok(Progress::Frame(_)) => panic!("cancelled frame reached the host"),
                Ok(_) => thread::sleep(Duration::from_millis(1)),
                Err(_) => break,
            }
        }
    });
    let mut transport = harness.client.take().unwrap();
    let mut send = Box::pin(transport.send(notification()));
    assert!(timeout(Duration::from_millis(50), &mut send).await.is_err());
    drop(send);
    assert_eq!(
        timeout(TEST_TIMEOUT, harness.status.wait()).await.unwrap(),
        Err(Error::SendCancelled)
    );
    worker.join().unwrap();
    assert!(harness.client_cancelled.load(Ordering::Acquire));
    assert!(harness.host_cancelled.load(Ordering::Acquire));
    assert!(!harness.status.cleanup_failed());
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_receive_keeps_the_next_frame_and_malformed_input_is_observable() {
    let mut harness = Harness::new(None);
    let mut host = harness.host.take().unwrap();
    let (send_tx, send_rx) = std_mpsc::sync_channel::<Vec<u8>>(2);
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            assert!(Instant::now() < deadline, "synthetic host timed out");
            if host.is_established() && !host.has_pending_output() {
                if let Ok(frame) = send_rx.try_recv() {
                    host.prepare_send(&frame, Instant::now() + MAX_FRAME_DURATION)
                        .unwrap();
                }
            }
            match host.poll(|| Ok(())) {
                Ok(_) => thread::sleep(Duration::from_millis(1)),
                Err(festerm_automation_mcp::secure_transport::Error::PeerClosed) => break,
                Err(error) => panic!("synthetic host failed: {error}"),
            }
        }
    });
    let mut transport = harness.client.take().unwrap();
    let (client_fingerprint, host_fingerprint) = harness.fingerprints().await;
    harness.approve_client(&host_fingerprint);
    harness.approve_host(&client_fingerprint);
    assert!(timeout(Duration::from_millis(50), transport.receive())
        .await
        .is_err());
    send_tx
        .try_send(br#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#.to_vec())
        .unwrap();
    let message = timeout(TEST_TIMEOUT, transport.receive())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(message).unwrap()["method"],
        "notifications/tools/list_changed"
    );
    send_tx.try_send(b"not-json".to_vec()).unwrap();
    assert!(timeout(TEST_TIMEOUT, transport.receive())
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        timeout(TEST_TIMEOUT, harness.status.wait()).await.unwrap(),
        Err(Error::InvalidMessage)
    );
    worker.join().unwrap();
    assert!(!harness.status.cleanup_failed());
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_after_a_partial_write_closes_and_never_finishes_the_frame() {
    let mut harness = Harness::new(Some(1024));
    let mut host = harness.host.take().unwrap();
    let (partial_tx, partial_rx) = tokio::sync::oneshot::channel();
    let (continue_tx, continue_rx) = std_mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + TEST_TIMEOUT;
        let mut partial_tx = Some(partial_tx);
        loop {
            assert!(Instant::now() < deadline, "partial frame timed out");
            let established = host.is_established();
            match host.poll(|| Ok(())) {
                Ok(Progress::Frame(_)) => panic!("cancelled frame completed"),
                Ok(Progress::Advanced) if established && partial_tx.is_some() => {
                    partial_tx.take().unwrap().send(()).unwrap();
                    continue_rx.recv_timeout(TEST_TIMEOUT).unwrap();
                }
                Ok(_) => thread::sleep(Duration::from_millis(1)),
                Err(festerm_automation_mcp::secure_transport::Error::PeerClosed) => break,
                Err(error) => panic!("synthetic host failed: {error}"),
            }
        }
    });
    let mut transport = harness.client.take().unwrap();
    let (client_fingerprint, host_fingerprint) = harness.fingerprints().await;
    harness.approve_client(&host_fingerprint);
    harness.approve_host(&client_fingerprint);
    let request: ClientJsonRpcMessage = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 41,
        "method": "tools/call",
        "params": {
            "name": "festerm_terminal_send_text",
            "arguments": {"enrollment_ref": "a".repeat(32), "text": "x".repeat(60_000)}
        }
    }))
    .unwrap();
    let mut send = Box::pin(transport.send(request));
    tokio::select! {
        result = &mut send => panic!("frame unexpectedly completed: {result:?}"),
        result = partial_rx => result.unwrap(),
    }
    assert!(timeout(Duration::from_millis(50), &mut send).await.is_err());
    drop(send);
    assert_eq!(
        timeout(TEST_TIMEOUT, harness.status.wait()).await.unwrap(),
        Err(Error::SendCancelled)
    );
    continue_tx.try_send(()).unwrap();
    worker.join().unwrap();
    assert!(!harness.status.cleanup_failed());
}

#[tokio::test(flavor = "current_thread")]
async fn temporary_admission_contention_wakes_and_idle_disconnect_closes() {
    let mut harness = Harness::with_admission(None, false);
    let mut host = harness.host.take().unwrap();
    let (established_tx, established_rx) = tokio::sync::oneshot::channel();
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + TEST_TIMEOUT;
        let mut established_tx = Some(established_tx);
        loop {
            assert!(Instant::now() < deadline, "host did not close");
            match host.poll(|| Ok(())) {
                Ok(Progress::Established) => {
                    established_tx.take().unwrap().send(()).unwrap();
                }
                Ok(Progress::Frame(_)) => panic!("unexpected application frame"),
                Ok(_) => thread::sleep(Duration::from_millis(1)),
                Err(festerm_automation_mcp::secure_transport::Error::PeerClosed) => break,
                Err(error) => panic!("synthetic host failed: {error}"),
            }
        }
    });
    assert!(
        timeout(Duration::from_millis(50), harness.client_display.recv())
            .await
            .is_err()
    );
    assert!(harness.status.result().is_none());
    harness.admission_open.store(true, Ordering::Release);
    harness.wake.wake();
    let (client_fingerprint, host_fingerprint) = harness.fingerprints().await;
    harness.approve_client(&host_fingerprint);
    harness.approve_host(&client_fingerprint);
    timeout(TEST_TIMEOUT, established_rx)
        .await
        .unwrap()
        .unwrap();
    harness.status.disconnect();
    assert_eq!(
        timeout(TEST_TIMEOUT, harness.status.wait()).await.unwrap(),
        Ok(())
    );
    worker.join().unwrap();
    assert!(harness.client_cancelled.load(Ordering::Acquire));
    assert!(harness.host_cancelled.load(Ordering::Acquire));
}

#[tokio::test(flavor = "current_thread")]
async fn bounded_queue_backpressure_closes_pending_sends() {
    let mut harness = Harness::new(None);
    let mut client = harness.client.take().unwrap();
    let mut sends = Vec::new();
    for _ in 0..5 {
        sends.push(tokio::spawn(client.send(notification())));
        tokio::task::yield_now().await;
    }
    let result = timeout(TEST_TIMEOUT, harness.status.wait()).await.unwrap();
    assert_eq!(result, Err(Error::Backpressure));
    for send in sends {
        assert!(timeout(TEST_TIMEOUT, send).await.unwrap().unwrap().is_err());
    }
    harness.host.as_mut().unwrap().close().unwrap();
    assert!(!harness.status.cleanup_failed());
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_outbound_message_closes_before_transmission() {
    let mut harness = Harness::new(None);
    let mut client = harness.client.take().unwrap();
    let request: ClientJsonRpcMessage = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": {
            "name": "festerm_terminal_send_text",
            "arguments": {"enrollment_ref": "a".repeat(32), "text": "x".repeat(70_000)}
        }
    }))
    .unwrap();
    assert_eq!(client.send(request).await, Err(Error::FrameTooLarge));
    assert_eq!(
        timeout(TEST_TIMEOUT, harness.status.wait()).await.unwrap(),
        Err(Error::FrameTooLarge)
    );
    harness.host.as_mut().unwrap().close().unwrap();
    assert!(!harness.status.cleanup_failed());
}

#[tokio::test(flavor = "current_thread")]
async fn deeply_nested_outbound_message_is_refused_before_transmission() {
    let mut harness = Harness::new(None);
    let mut client = harness.client.take().unwrap();
    let mut nested = json!("value");
    for _ in 0..32 {
        nested = json!({"child": nested});
    }
    let request: ClientJsonRpcMessage = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": "tools/call",
        "params": {"name": "example", "arguments": {"value": nested}}
    }))
    .unwrap();
    assert_eq!(client.send(request).await, Err(Error::InvalidMessage));
    assert_eq!(
        timeout(TEST_TIMEOUT, harness.status.wait()).await.unwrap(),
        Err(Error::InvalidMessage)
    );
    harness.host.as_mut().unwrap().close().unwrap();
}
