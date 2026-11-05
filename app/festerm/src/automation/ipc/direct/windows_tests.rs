use super::*;
use festerm_automation_mcp::secure_transport::{
    NativeCleanupFailed, NativeDecision, NativeUnavailable, MAX_FRAME_DURATION,
};
use festerm_automation_mcp_client::{
    AdmissionCleanupFailed, ClientWriteAdmission, DirectClientTransport,
};
use festerm_windows_security::named_pipe::{Pipe, PipeListener};
use rmcp::transport::Transport;
use serde_json::json;
use std::{
    sync::{atomic::Ordering, mpsc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc as async_mpsc, Notify};

const TIMEOUT: Duration = Duration::from_secs(8);

struct Native {
    display: async_mpsc::Sender<String>,
    decision: Arc<Mutex<Option<[u8; 64]>>>,
    cancelled: Arc<AtomicBool>,
    owner: OwnerLiveness,
}

impl TrustedNativeVerification for Native {
    fn show_local_fingerprint(&mut self, fingerprint: &str) -> Result<(), NativeUnavailable> {
        self.display
            .try_send(fingerprint.to_owned())
            .map_err(|_| NativeUnavailable)
    }

    fn poll_decision(&mut self) -> NativeDecision {
        match self.decision.lock().unwrap().take() {
            Some(fingerprint) => NativeDecision::ComparedPeer(fingerprint),
            None => NativeDecision::Pending,
        }
    }

    fn cancel(&mut self) -> Result<(), NativeCleanupFailed> {
        self.cancelled.store(true, Ordering::Release);
        Ok(())
    }
}

impl HostVerification for Native {
    fn owner_liveness(&self) -> OwnerLiveness {
        self.owner.clone()
    }
}

struct Admission {
    open: Arc<AtomicBool>,
    blocked: Arc<Notify>,
    cancelled: Arc<AtomicBool>,
}

impl NativeAdmission for Admission {
    type Guard<'a> = ();

    fn try_admit(&self) -> io::Result<Self::Guard<'_>> {
        if self.open.load(Ordering::Acquire) {
            Ok(())
        } else {
            self.blocked.notify_one();
            Err(io::ErrorKind::WouldBlock.into())
        }
    }

    fn cancel(&mut self) -> io::Result<()> {
        self.cancelled.store(true, Ordering::Release);
        Ok(())
    }
}

struct ClientAdmission;

impl ClientWriteAdmission for ClientAdmission {
    type Guard<'a> = ();

    fn try_admit(&self) -> io::Result<Self::Guard<'_>> {
        Ok(())
    }

    fn cancel(&mut self) -> Result<(), AdmissionCleanupFailed> {
        Ok(())
    }
}

struct NativeFixture {
    native: Native,
    displayed: async_mpsc::Receiver<String>,
    decision: Arc<Mutex<Option<[u8; 64]>>>,
    cancelled: Arc<AtomicBool>,
}

fn native() -> NativeFixture {
    let (display, displayed) = async_mpsc::channel(1);
    let decision = Arc::new(Mutex::new(None));
    let cancelled = Arc::new(AtomicBool::new(false));
    NativeFixture {
        native: Native {
            display,
            decision: decision.clone(),
            cancelled: cancelled.clone(),
            owner: OwnerLiveness::default(),
        },
        displayed,
        decision,
        cancelled,
    }
}

async fn next_event(events: &mpsc::Receiver<Event>, wake: &Notify) -> Event {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let notified = wake.notified();
            match events.try_recv() {
                Ok(event) => return event,
                Err(mpsc::TryRecvError::Empty) => notified.await,
                Err(mpsc::TryRecvError::Disconnected) => panic!("event channel closed"),
            }
        }
    })
    .await
    .expect("direct host did not report an event")
}

#[tokio::test(flavor = "current_thread")]
async fn windows_direct_pipe_exchanges_sdk_frames_and_cuts_queued_output() {
    let name = format!(r"\\.\pipe\festerm-mcp-direct-test-{}", std::process::id());
    let listener = PipeListener::bind_current_user(&name, true).unwrap();
    let client_pipe = Pipe::connect(&name, TIMEOUT, &AtomicBool::new(false)).unwrap();
    client_pipe.verify_server_user().unwrap();
    let host_pipe = listener.accept(&AtomicBool::new(false)).unwrap();

    let NativeFixture {
        native: host_native,
        displayed: mut host_display,
        decision: host_decision,
        cancelled: host_cancelled,
    } = native();
    let NativeFixture {
        native: client_native,
        displayed: mut client_display,
        decision: client_decision,
        cancelled: client_cancelled,
    } = native();
    let host_wake = Wake::default();
    let ui_wake = Arc::new(Notify::new());
    let notify_ui = ui_wake.clone();
    let open = Arc::new(AtomicBool::new(true));
    let blocked = Arc::new(Notify::new());
    let admission_cancelled = Arc::new(AtomicBool::new(false));
    let privacy_closed = Arc::new(AtomicBool::new(false));
    let (event_sender, events) = mpsc::sync_channel(8);
    let (host, mut host_status) = owner::Owner::new(
        host_pipe,
        host_native,
        Admission {
            open: open.clone(),
            blocked: blocked.clone(),
            cancelled: admission_cancelled.clone(),
        },
        PeerContext {
            bootstrap: QualifiedBootstrap::synthetic(),
            key: 17,
            events: event_sender,
            wake_ui: Arc::new(move || notify_ui.notify_one()),
            stop: Arc::new(AtomicBool::new(false)),
            privacy_closed: privacy_closed.clone(),
            wake: host_wake.clone(),
        },
        TIMEOUT,
        MAX_FRAME_DURATION,
    )
    .unwrap();
    let host_task = tokio::spawn(host.run());
    let (wake_sender, wake_receiver) = mpsc::sync_channel(1);
    let (mut client, mut client_status) =
        DirectClientTransport::new(client_pipe, TIMEOUT, MAX_FRAME_DURATION, |wake| {
            wake_sender.send(wake).unwrap();
            (client_native, ClientAdmission)
        })
        .unwrap();
    let client_wake = wake_receiver.recv().unwrap();
    let host_code = tokio::time::timeout(TIMEOUT, host_display.recv())
        .await
        .unwrap()
        .unwrap();
    let client_code = tokio::time::timeout(TIMEOUT, client_display.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(host_code, client_code);
    *host_decision.lock().unwrap() = Some(client_code.as_bytes().try_into().unwrap());
    *client_decision.lock().unwrap() = Some(host_code.as_bytes().try_into().unwrap());
    host_wake.notify();
    client_wake.wake();

    let connection = match next_event(&events, &ui_wake).await {
        Event::Connected(connection) => connection,
        _ => panic!("expected direct connection"),
    };
    let request = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
    tokio::time::timeout(
        TIMEOUT,
        client.send(serde_json::from_value(request).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    match next_event(&events, &ui_wake).await {
        Event::Message { key, bytes } => {
            assert_eq!(key, 17);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["method"],
                "notifications/initialized"
            );
        }
        _ => panic!("expected direct request"),
    }
    connection.finish_message().unwrap();

    connection
        .send(
            br#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#.to_vec(),
            Instant::now() + MAX_FRAME_DURATION,
            &[],
        )
        .unwrap();
    let response = tokio::time::timeout(TIMEOUT, client.receive())
        .await
        .unwrap()
        .expect("direct response missing");
    assert_eq!(
        serde_json::to_value(response).unwrap()["method"],
        "notifications/tools/list_changed"
    );

    open.store(false, Ordering::Release);
    connection
        .send(
            br#"{"jsonrpc":"2.0","method":"notifications/resources/list_changed"}"#.to_vec(),
            Instant::now() + MAX_FRAME_DURATION,
            &[],
        )
        .unwrap();
    tokio::time::timeout(TIMEOUT, blocked.notified())
        .await
        .expect("output did not reach the admission boundary");
    privacy_closed.store(true, Ordering::Release);
    open.store(true, Ordering::Release);
    host_wake.notify();
    assert!(tokio::time::timeout(TIMEOUT, client.receive())
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        tokio::time::timeout(TIMEOUT, host_task)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
    assert_eq!(
        tokio::time::timeout(TIMEOUT, host_status.wait())
            .await
            .unwrap(),
        Ok(())
    );
    let result = tokio::time::timeout(TIMEOUT, client.close()).await.unwrap();
    assert!(result.is_err());
    assert_eq!(
        tokio::time::timeout(TIMEOUT, client_status.wait())
            .await
            .unwrap(),
        result
    );
    assert!(host_cancelled.load(Ordering::Acquire));
    assert!(client_cancelled.load(Ordering::Acquire));
    assert!(admission_cancelled.load(Ordering::Acquire));
}
