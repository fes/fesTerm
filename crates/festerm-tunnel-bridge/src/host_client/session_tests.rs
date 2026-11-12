use super::*;
use festerm_core::Dimensions;
use festerm_host_protocol::{GenerationId, HostId, LogicalSessionId};
use tokio::io::AsyncWriteExt;

struct Notify(tokio::sync::Notify);
impl SessionEventNotifier for Notify {
    fn notify(&self) {
        self.0.notify_one();
    }
}

fn scope() -> Scope {
    Scope {
        request: OperationId::new(),
        generation: 1,
        binding: Binding::new(
            HostId::new(),
            "owned-account".into(),
            "owned-endpoint".into(),
        )
        .unwrap(),
    }
}

async fn frame(peer: &mut tokio::io::DuplexStream, kind: u8, bytes: &[u8]) {
    peer.write_u8(kind).await.unwrap();
    peer.write_u32(bytes.len() as u32).await.unwrap();
    peer.write_all(bytes).await.unwrap();
    peer.flush().await.unwrap();
}

async fn response(peer: &mut tokio::io::DuplexStream, request: &Envelope<Request>, body: Response) {
    let bytes = festerm_host_protocol::encode(&Envelope::new(
        request.host.clone(),
        request.operation.clone(),
        body,
    ))
    .unwrap();
    frame(peer, HOST_REPLY, &bytes).await;
}

async fn event(session: &HostSession, notifier: &Notify) -> SessionEvent {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match session.try_recv_event() {
                Ok(event) => return event,
                Err(SessionTryReceiveError::Empty) => notifier.0.notified().await,
                Err(SessionTryReceiveError::Closed) => panic!("missing terminal event"),
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn selected_host_session_presents_only_after_frontend_adoption_and_confirmed_activation() {
    presentation_round_trip(true).await;
}

#[tokio::test]
async fn selected_host_session_active_app_lock_closes_transport_without_detach_or_replay() {
    presentation_round_trip(false).await;
}

#[test]
fn selected_host_mobile_cancel_records_closed_state_after_shared_token_cancel() {
    let cancel = CancellationToken::new();
    let (_sender, events) = flume::bounded(2);
    let session = HostSession::from_worker(
        Worker {
            scope: scope(),
            cancel: cancel.clone(),
            events,
        },
        Arc::new(Notify(tokio::sync::Notify::new())),
    );
    cancel.cancel();
    session.cancel();
    assert!(
        matches!(session.lifecycle(), SessionLifecycle::Disconnected(_)),
        "native privacy may cancel the admission token before its registered Session"
    );
    assert!(session.try_send_input(b"never-replay").is_err());
    assert!(matches!(
        session.try_recv_event(),
        Ok(SessionEvent::Lifecycle(SessionLifecycle::Disconnected(_)))
    ));
}

async fn presentation_round_trip(intentional_detach: bool) {
    let (stream, mut peer) = tokio::io::duplex(8192);
    let scope = scope();
    let target = Target {
        logical: LogicalSessionId::new(),
        generation: GenerationId::new(7).unwrap(),
    };
    let notifier = Arc::new(Notify(tokio::sync::Notify::new()));
    let (sender, events) = flume::bounded(2);
    let cancel = CancellationToken::new();
    let session = HostSession::from_worker(
        Worker {
            scope: scope.clone(),
            cancel: cancel.clone(),
            events,
        },
        notifier.clone(),
    );
    let server = tokio::spawn(async move {
        let permit = ConnectionPermit::reserve(&Budgets::default()).unwrap();
        let hello = host_terminal::read_frame(&mut peer, &permit, true)
            .await
            .unwrap();
        let hello: Envelope<Request> = festerm_host_protocol::decode(hello.payload()).unwrap();
        response(
            &mut peer,
            &hello,
            Response::Capabilities {
                capabilities: Capabilities {
                    attach: true,
                    detach: true,
                    ..Capabilities::default()
                },
            },
        )
        .await;
        let request = host_terminal::read_frame(&mut peer, &permit, true)
            .await
            .unwrap();
        let request: Envelope<Request> = festerm_host_protocol::decode(request.payload()).unwrap();
        let Request::Attach {
            target, attachment, ..
        } = &request.body
        else {
            panic!("attach")
        };
        let mut terminal = Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap();
        terminal.ingest(b"approved recovery");
        let bytes = crate::protocol::encode_snapshot_bounded(
            &terminal.recovery_clone(),
            host_terminal::HOST_SNAPSHOT_LIMIT,
        )
        .unwrap();
        frame(&mut peer, crate::protocol::SNAPSHOT, &bytes).await;
        let adopted = host_terminal::read_frame(&mut peer, &permit, true)
            .await
            .unwrap();
        assert_eq!(adopted.kind(), crate::protocol::ADOPTED);
        response(
            &mut peer,
            &request,
            Response::Attached {
                target: target.clone(),
                attachment: attachment.clone(),
                lease: None,
            },
        )
        .await;
        let input = host_terminal::read_frame(&mut peer, &permit, true)
            .await
            .unwrap();
        assert_eq!(input.payload(), b"once");
        frame(&mut peer, OUTPUT, b"actual output").await;
        if !intentional_detach {
            assert!(host_terminal::read_frame(&mut peer, &permit, true)
                .await
                .is_err());
            return;
        }
        let detach = host_terminal::read_frame(&mut peer, &permit, true)
            .await
            .unwrap();
        let detach: Envelope<Request> = festerm_host_protocol::decode(detach.payload()).unwrap();
        let Request::Detach { target, .. } = &detach.body else {
            panic!("detach")
        };
        response(
            &mut peer,
            &detach,
            Response::Detached {
                target: target.clone(),
            },
        )
        .await;
    });
    let notify = notifier.clone();
    let attach = tokio::spawn(async move {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || notify.notify());
        let outcome = attach_authenticated(
            stream,
            ConnectionPermit::reserve(&Budgets::default()).unwrap(),
            &scope,
            target,
            &cancel,
            &sender,
            &wake,
        )
        .await
        .unwrap();
        sender
            .send_async(Completion { scope, outcome })
            .await
            .unwrap_or_else(|_| panic!("completion"));
        wake();
    });
    assert!(session.try_send_input(b"before adoption").is_err());
    tokio::time::timeout(Duration::from_secs(2), notifier.0.notified())
        .await
        .unwrap();
    let mut presented = Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap();
    assert!(session.adopt_into(&mut presented).unwrap());
    assert!(presented
        .text_snapshot()
        .text()
        .contains("approved recovery"));
    assert_eq!(session.lifecycle(), SessionLifecycle::Starting);
    tokio::time::timeout(Duration::from_secs(2), notifier.0.notified())
        .await
        .unwrap();
    assert_eq!(session.lifecycle(), SessionLifecycle::Running);
    assert!(!session.adopt_into(&mut presented).unwrap());
    assert!(session.terminal_replies_owned_by_backend());
    session.try_send_input(b"once").unwrap();
    assert_eq!(
        event(&session, &notifier).await,
        SessionEvent::Lifecycle(SessionLifecycle::Running)
    );
    let SessionEvent::Output(bytes) = event(&session, &notifier).await else {
        panic!("ordered terminal output");
    };
    assert_eq!(bytes, b"actual output");
    presented.ingest(&bytes);
    assert!(presented
        .text_snapshot()
        .text()
        .contains("approved recoveryactual output"));
    if intentional_detach {
        session.try_shutdown().unwrap();
        assert_eq!(session.lifecycle(), SessionLifecycle::Stopping);
        assert!(session.try_send_input(b"not replayed").is_err());
        assert_eq!(
            event(&session, &notifier).await,
            SessionEvent::Lifecycle(SessionLifecycle::Stopping)
        );
        assert_eq!(
            event(&session, &notifier).await,
            SessionEvent::Lifecycle(SessionLifecycle::Stopped)
        );
    } else {
        session.cancel();
        assert!(session.try_send_input(b"not replayed").is_err());
        assert!(matches!(
            event(&session, &notifier).await,
            SessionEvent::Lifecycle(SessionLifecycle::Disconnected(_)),
        ));
        assert!(session.try_shutdown().is_err());
    }
    server.await.unwrap();
    attach.await.unwrap();
}

#[test]
fn selected_host_session_app_lock_cancels_without_fabricated_detach_or_input() {
    let (sender, events) = flume::bounded(2);
    let cancel = CancellationToken::new();
    let session = HostSession::from_worker(
        Worker {
            scope: scope(),
            cancel: cancel.clone(),
            events,
        },
        festerm_session::noop_session_event_notifier(),
    );
    session.cancel();
    assert!(cancel.is_cancelled());
    assert!(matches!(
        session.lifecycle(),
        SessionLifecycle::Disconnected(_)
    ));
    assert!(session.try_send_input(b"stale").is_err());
    assert!(session.try_shutdown().is_err());
    drop(sender);
}
