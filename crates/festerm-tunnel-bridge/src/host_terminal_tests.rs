use super::*;
use festerm_core::Dimensions;
use festerm_host_protocol::{
    Budgets, Capabilities, ContractError, GenerationId, HostId, LogicalSessionId, Refusal,
};
use tokio::io::duplex;

fn binding() -> Binding {
    Binding::new(
        HostId::new(),
        "owned-account".into(),
        "owned-endpoint".into(),
    )
    .unwrap()
}
fn target() -> HostTarget {
    HostTarget {
        logical: LogicalSessionId::new(),
        generation: GenerationId::new(9).unwrap(),
    }
}
fn permit() -> ConnectionPermit {
    ConnectionPermit::reserve(&Budgets::default()).unwrap()
}

async fn handshake(
    peer: &mut tokio::io::DuplexStream,
    permit: &ConnectionPermit,
) -> Envelope<Request> {
    let hello = read_frame(peer, permit, true).await.unwrap();
    assert_eq!(hello.kind, HOST_REQUEST);
    let hello: Envelope<Request> = festerm_host_protocol::decode(&hello.bytes).unwrap();
    assert!(matches!(hello.body, Request::Capabilities {}));
    write_contract(
        peer,
        permit,
        HOST_REPLY,
        &Envelope::new(
            hello.host,
            hello.operation,
            Response::Capabilities {
                capabilities: Capabilities {
                    attach: true,
                    detach: true,
                    ..Capabilities::default()
                },
            },
        ),
    )
    .await
    .unwrap();
    let request = read_frame(peer, permit, true).await.unwrap();
    let request: Envelope<Request> = festerm_host_protocol::decode(&request.bytes).unwrap();
    assert!(matches!(request.body, Request::Attach { .. }));
    let mut terminal = Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap();
    terminal.ingest(b"owned recovery");
    let bytes = encode_snapshot_bounded(&terminal.recovery_clone(), HOST_SNAPSHOT_LIMIT).unwrap();
    write(peer, SNAPSHOT, &bytes).await.unwrap();
    request
}

fn attached(request: &Envelope<Request>) -> Envelope<Response> {
    let Request::Attach {
        target, attachment, ..
    } = &request.body
    else {
        unreachable!()
    };
    Envelope::new(
        request.host.clone(),
        request.operation.clone(),
        Response::Attached {
            target: target.clone(),
            attachment: attachment.clone(),
            lease: None,
        },
    )
}

#[tokio::test]
async fn frontend_adoption_waits_for_confirmed_attachment_then_full_duplex_never_replays_input() {
    let (stream, mut peer) = duplex(4096);
    let server = tokio::spawn(async move {
        let permit = permit();
        let request = handshake(&mut peer, &permit).await;
        let adopted = read_frame(&mut peer, &permit, true).await.unwrap();
        assert_eq!(adopted.kind, ADOPTED);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), peer.read_u8())
                .await
                .is_err()
        );
        write_contract(&mut peer, &permit, HOST_REPLY, &attached(&request))
            .await
            .unwrap();
        let input = read_frame(&mut peer, &permit, true).await.unwrap();
        assert_eq!(
            (input.kind, input.bytes.as_slice()),
            (INPUT, b"one input".as_slice())
        );
        write(&mut peer, OUTPUT, b"one output").await.unwrap();
        let request = read_frame(&mut peer, &permit, true).await.unwrap();
        let request: Envelope<Request> = festerm_host_protocol::decode(&request.bytes).unwrap();
        let Request::Detach { target, .. } = request.body else {
            panic!("exact detach")
        };
        write_contract(
            &mut peer,
            &permit,
            HOST_REPLY,
            &Envelope::new(
                request.host,
                request.operation,
                Response::Detached { target },
            ),
        )
        .await
        .unwrap();
    });
    let cancel = CancellationToken::new();
    let mut pending = establish(
        stream,
        permit(),
        binding(),
        OperationId::new(),
        target(),
        AttachmentId::new(),
        false,
        &cancel,
    )
    .await
    .unwrap();
    assert!(pending
        .take_snapshot()
        .unwrap()
        .row_text(0)
        .unwrap()
        .contains("owned recovery"));
    let mut terminal = pending.adopt(&cancel).await.unwrap().spawn().unwrap();
    terminal.input(b"one input").unwrap();
    let frame = terminal.next_event().await.unwrap();
    assert_eq!(
        (frame.kind(), frame.payload()),
        (OUTPUT, b"one output".as_slice())
    );
    drop(frame);
    let operation = terminal.request_detach().unwrap();
    assert_eq!(terminal.input(b"not replayed"), Err(Error::Closed));
    let frame = terminal.next_event().await.unwrap();
    let response: Envelope<Response> = festerm_host_protocol::decode(frame.payload()).unwrap();
    assert_eq!(response.operation, operation);
    assert!(matches!(response.body, Response::Detached { .. }));
    server.await.unwrap();
}

#[tokio::test]
async fn attachment_confirmation_refuses_foreign_generation_and_typed_uncertainty() {
    for uncertain in [false, true] {
        let (stream, mut peer) = duplex(4096);
        let server = tokio::spawn(async move {
            let permit = permit();
            let request = handshake(&mut peer, &permit).await;
            assert_eq!(
                read_frame(&mut peer, &permit, true).await.unwrap().kind,
                ADOPTED
            );
            let mut response = attached(&request);
            if uncertain {
                response.body = Response::Refused {
                    reason: Refusal::OutcomeUncertain,
                };
            } else if let Response::Attached { target, .. } = &mut response.body {
                target.generation = GenerationId::new(10).unwrap();
            }
            write_contract(&mut peer, &permit, HOST_REPLY, &response)
                .await
                .unwrap();
        });
        let cancel = CancellationToken::new();
        let mut pending = establish(
            stream,
            permit(),
            binding(),
            OperationId::new(),
            target(),
            AttachmentId::new(),
            false,
            &cancel,
        )
        .await
        .unwrap();
        pending.take_snapshot().unwrap();
        let result = pending.adopt(&cancel).await;
        assert!(matches!(
            (uncertain, result),
            (true, Err(Error::Refused(Refusal::OutcomeUncertain)))
                | (false, Err(Error::Contract(ContractError::Stale)))
        ));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn terminal_rejects_legacy_file_frames_and_oversized_lengths_before_body_allocation() {
    for (kind, length, client) in [
        (HELLO, 1, true),
        (FILE_READ, 1, true),
        (HOST_REPLY, 1, true),
        (SNAPSHOT, (HOST_SNAPSHOT_LIMIT + 1) as u32, false),
        (INPUT, u32::MAX, true),
        (ADOPTED, 1, true),
        (RESIZE, 0, true),
    ] {
        let budgets = Budgets::default();
        let permit = ConnectionPermit::reserve(&budgets).unwrap();
        let (mut output, mut input) = duplex(5);
        output.write_u8(kind).await.unwrap();
        output.write_u32(length).await.unwrap();
        assert!(read_frame(&mut input, &permit, client).await.is_err());
        assert!(budgets
            .reserve(
                Resource::TerminalBytes,
                festerm_host_protocol::MAX_QUEUED_BYTES
            )
            .is_ok());
    }
    assert!(validate_length(HOST_REQUEST, 10).is_err());
    assert!(validate_length(HOST_REPLY, 10).is_err());
}

#[tokio::test]
async fn idle_terminal_reads_allow_silence_but_partial_frames_expire_and_release_bytes() {
    let budgets = Budgets::default();
    let permit = ConnectionPermit::reserve(&budgets).unwrap();
    let (mut output, mut input) = duplex(5);
    tokio::time::pause();
    let reader = tokio::spawn(async move { read_frame(&mut input, &permit, false).await });
    tokio::time::advance(Duration::from_secs(30)).await;
    assert!(!reader.is_finished());
    output.write_u8(OUTPUT).await.unwrap();
    output.write_u32(32).await.unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(DEADLINE + Duration::from_secs(1)).await;
    assert!(matches!(reader.await.unwrap(), Err(Error::Deadline)));
    assert!(budgets
        .reserve(
            Resource::TerminalBytes,
            festerm_host_protocol::MAX_QUEUED_BYTES
        )
        .is_ok());
}

#[tokio::test]
async fn queued_terminal_frames_retain_per_client_and_aggregate_byte_charges() {
    let budgets = Budgets::default();
    let permit = ConnectionPermit::reserve(&budgets).unwrap();
    let mut held = Vec::new();
    for _ in 0..MAX_BYTES_PER_CLIENT / CHUNK_LIMIT {
        let (mut output, mut input) = duplex(CHUNK_LIMIT + 5);
        output.write_u8(OUTPUT).await.unwrap();
        output.write_u32(CHUNK_LIMIT as u32).await.unwrap();
        output.write_all(&vec![b'x'; CHUNK_LIMIT]).await.unwrap();
        held.push(read_frame(&mut input, &permit, false).await.unwrap());
    }
    assert!(matches!(
        permit.reserve_terminal_bytes(1),
        Err(ContractError::Busy)
    ));
    let (mut output, mut input) = duplex(5);
    output.write_u8(OUTPUT).await.unwrap();
    output.write_u32(1).await.unwrap();
    assert!(matches!(
        read_frame(&mut input, &permit, false).await,
        Err(Error::Contract(ContractError::Busy))
    ));
    drop(held);
    assert!(permit.reserve_terminal_bytes(MAX_BYTES_PER_CLIENT).is_ok());
}

#[cfg(feature = "relay")]
#[tokio::test]
async fn abandoned_fair_turn_cannot_block_other_terminals() {
    use std::{future::Future, task::Poll};
    let scheduler = OutputScheduler::default();
    let cancel = CancellationToken::new();
    let first = ConnectionId::new();
    let second = ConnectionId::new();
    let mut abandoned = Box::pin(scheduler.turn(&first, &cancel));
    std::future::poll_fn(|cx| {
        assert!(abandoned.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let next = scheduler.turn(&second, &cancel);
    tokio::pin!(next);
    std::future::poll_fn(|cx| {
        assert!(next.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(abandoned);
    tokio::time::timeout(Duration::from_secs(1), next)
        .await
        .unwrap()
        .unwrap();
    assert!(scheduler.0.ready.lock().unwrap().is_empty());
}

#[cfg(all(feature = "relay", unix))]
mod native_routes {
    use super::*;
    use crate::host_control::tests::{native_routes::NativeFixture, verified, Policy};
    use festerm_host_protocol::{Creation, Lifetime};
    use festerm_tunnel_client::host_control::ControlClient;
    use std::time::Instant;

    async fn create(
        fixture: &NativeFixture,
        control: &mut ControlClient<tokio::io::DuplexStream>,
        creation: Creation,
    ) -> HostTarget {
        let operation = control.issue_operation().unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let response = control
                .request_issued(
                    &operation,
                    Request::Ensure {
                        creation: creation.clone(),
                    },
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            match response.body {
                Response::Ensured { session, .. } => return session.target,
                Response::Refused {
                    reason: Refusal::Busy | Refusal::Unknown | Refusal::OutcomeUncertain,
                } if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await
                }
                _ => panic!("owned ensure {}: {response:?}", fixture.root.display()),
            }
        }
    }

    async fn pending(
        fixture: &NativeFixture,
        source: Arc<Policy>,
        target: HostTarget,
        takeover: bool,
        budgets: &Budgets,
        scheduler: &OutputScheduler,
    ) -> (
        Result<PendingTerminal<tokio::io::DuplexStream>, Error>,
        tokio::task::JoinHandle<Result<(), Error>>,
    ) {
        pending_using(
            fixture,
            source,
            target,
            AttachmentId::new(),
            takeover,
            budgets,
            scheduler,
        )
        .await
    }

    async fn pending_using(
        fixture: &NativeFixture,
        source: Arc<Policy>,
        target: HostTarget,
        attachment: AttachmentId,
        takeover: bool,
        budgets: &Budgets,
        scheduler: &OutputScheduler,
    ) -> (
        Result<PendingTerminal<tokio::io::DuplexStream>, Error>,
        tokio::task::JoinHandle<Result<(), Error>>,
    ) {
        let request = Envelope::new(
            fixture.policy.host.host.clone(),
            OperationId::new(),
            Request::Attach {
                target,
                attachment,
                takeover,
            },
        );
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let (pending, server) =
                pending_once(fixture, source.clone(), &request, budgets, scheduler).await;
            if matches!(pending, Err(Error::Refused(Refusal::Busy))) && Instant::now() < deadline {
                assert!(server.await.unwrap().is_err());
                tokio::time::sleep(Duration::from_millis(25)).await;
            } else {
                return (pending, server);
            }
        }
    }

    async fn pending_once(
        fixture: &NativeFixture,
        source: Arc<Policy>,
        request: &Envelope<Request>,
        budgets: &Budgets,
        scheduler: &OutputScheduler,
    ) -> (
        Result<PendingTerminal<tokio::io::DuplexStream>, Error>,
        tokio::task::JoinHandle<Result<(), Error>>,
    ) {
        let Request::Attach {
            target,
            attachment,
            takeover,
        } = &request.body
        else {
            panic!("owned attachment request")
        };
        let (owner, guard, _) = verified(&fixture.policy);
        let (stream, peer) = duplex(4096);
        let server = tokio::spawn(native::serve_terminal(
            peer,
            ConnectionPermit::reserve(budgets).unwrap(),
            owner,
            guard,
            Arc::new(native::NativeTerminal::new(fixture.control(), source)),
            scheduler.clone(),
            CancellationToken::new(),
        ));
        let binding = Binding::new(
            fixture.policy.host.host.clone(),
            "owned-account".into(),
            "owned-endpoint".into(),
        )
        .unwrap();
        let pending = establish(
            stream,
            permit(),
            binding,
            request.operation.clone(),
            target.clone(),
            attachment.clone(),
            *takeover,
            &CancellationToken::new(),
        )
        .await;
        (pending, server)
    }

    async fn live(
        fixture: &NativeFixture,
        source: Arc<Policy>,
        target: HostTarget,
        takeover: bool,
        budgets: &Budgets,
        scheduler: &OutputScheduler,
    ) -> (
        TerminalHandle,
        tokio::task::JoinHandle<Result<(), Error>>,
        Terminal,
    ) {
        let (pending, server) =
            pending(fixture, source, target, takeover, budgets, scheduler).await;
        let mut pending = pending.unwrap();
        let snapshot = pending.take_snapshot().unwrap();
        let terminal = match pending.adopt(&CancellationToken::new()).await {
            Ok(terminal) => terminal.spawn().unwrap(),
            Err(error) => {
                let result = tokio::time::timeout(Duration::from_secs(2), server).await;
                panic!("owned adoption {error:?}; server={result:?}");
            }
        };
        (terminal, server, snapshot)
    }

    async fn until(terminal: &mut TerminalHandle, expected: &[u8]) -> Vec<u8> {
        let mut observed = Vec::new();
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let frame = terminal.next_event().await.unwrap();
                if frame.kind == OUTPUT {
                    observed.extend_from_slice(&frame.bytes);
                    assert!(observed.len() < MAX_BYTES_PER_CLIENT);
                    if observed
                        .windows(expected.len())
                        .any(|bytes| bytes == expected)
                    {
                        return;
                    }
                }
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "expected {:?}, received {:?}",
            String::from_utf8_lossy(expected),
            String::from_utf8_lossy(&observed)
        );
        observed
    }

    async fn detached(terminal: &mut TerminalHandle) {
        let operation = terminal.request_detach().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let frame = terminal.next_event().await.unwrap();
                if frame.kind == HOST_REPLY {
                    let response: Envelope<Response> =
                        festerm_host_protocol::decode(&frame.bytes).unwrap();
                    assert_eq!(response.operation, operation);
                    assert!(matches!(response.body, Response::Detached { .. }));
                    return;
                }
            }
        })
        .await
        .unwrap();
    }

    async fn fresh_inventory(control: &mut ControlClient<tokio::io::DuplexStream>) -> Response {
        let operation = OperationId::new();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let response = control
                    .request(
                        operation.clone(),
                        Request::List { cursor: None },
                        &CancellationToken::new(),
                    )
                    .await
                    .unwrap();
                match response.body {
                    Response::Inventory { .. } => return response.body,
                    Response::Refused {
                        reason: Refusal::Busy,
                    } => tokio::time::sleep(Duration::from_millis(25)).await,
                    _ => panic!("inventory: {:?}", response.body),
                }
            }
        })
        .await
        .expect("control must progress independently of terminal traffic")
    }

    #[tokio::test]
    #[ignore = "requires explicitly built owned native helper and fixture child"]
    async fn native_two_adopted_terminals_route_exact_bytes_without_inventory_frontends() {
        let mut fixture = NativeFixture::new(true);
        fixture.policy.capabilities.detach = true;
        let (backend, source) = fixture.backend();
        let budgets = Budgets::default();
        let scheduler = OutputScheduler::default();
        let (mut control, control_server) =
            fixture.connect(backend, source.clone(), &budgets).await;
        let mut a = fixture.creation();
        a.recipe.args = vec![
            "emit:ready-A".into(),
            "read-line".into(),
            "echo:A".into(),
            "spin".into(),
        ];
        let mut b = fixture.creation();
        b.recipe.args = vec![
            "emit:ready-B".into(),
            "read-line".into(),
            "echo:B".into(),
            "spin".into(),
        ];
        let a = create(&fixture, &mut control, a).await;
        let b = create(&fixture, &mut control, b).await;
        let inventory = control
            .request(
                OperationId::new(),
                Request::List { cursor: None },
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let Response::Inventory { page } = inventory.body else {
            panic!("inventory")
        };
        assert!(page.sessions.iter().all(|session| !session.attached));
        let (mut a_client, a_server, a_snapshot) =
            live(&fixture, source.clone(), a, false, &budgets, &scheduler).await;
        let (mut b_client, b_server, b_snapshot) =
            live(&fixture, source, b, false, &budgets, &scheduler).await;
        assert!(a_snapshot.row_text(0).unwrap().contains("ready-A"));
        assert!(b_snapshot.row_text(0).unwrap().contains("ready-B"));
        a_client.input(b"alpha\n").unwrap();
        b_client.input(b"beta\n").unwrap();
        let a_output = until(&mut a_client, b"A:alpha").await;
        let b_output = until(&mut b_client, b"B:beta").await;
        assert!(!a_output.windows(4).any(|bytes| bytes == b"beta"));
        assert!(!b_output.windows(5).any(|bytes| bytes == b"alpha"));
        assert!(matches!(
            fresh_inventory(&mut control).await,
            Response::Inventory { .. }
        ));
        detached(&mut a_client).await;
        detached(&mut b_client).await;
        assert!(a_server.await.unwrap().is_ok());
        assert!(b_server.await.unwrap().is_ok());
        assert_eq!(
            festerm_sessiond::list_running_sessions_in(&fixture.root)
                .unwrap()
                .len(),
            2
        );
        drop(control);
        assert!(control_server.await.unwrap().is_err());
    }

    #[tokio::test]
    #[ignore = "requires explicitly built owned native helper and fixture child"]
    async fn native_takeover_waits_for_adoption_and_stale_generation_cannot_attach() {
        let mut fixture = NativeFixture::new(true);
        fixture.policy.capabilities.detach = true;
        let (backend, source) = fixture.backend();
        let budgets = Budgets::default();
        let scheduler = OutputScheduler::default();
        let (mut control, control_server) =
            fixture.connect(backend, source.clone(), &budgets).await;
        let mut creation = fixture.creation();
        creation.recipe.args = vec![
            "emit:ready".into(),
            "read-line".into(),
            "echo:active".into(),
            "read-line".into(),
            "echo:before".into(),
            "read-line".into(),
            "echo:after".into(),
            "spin".into(),
        ];
        let target = create(&fixture, &mut control, creation).await;
        let mut stale = target.clone();
        stale.generation = GenerationId::new(target.generation.get() + 1).unwrap();
        let (refused, refused_server) =
            pending(&fixture, source.clone(), stale, false, &budgets, &scheduler).await;
        assert!(matches!(refused, Err(Error::Refused(Refusal::Stale))));
        assert!(refused_server.await.unwrap().is_err());
        let (mut old, old_server, _) = live(
            &fixture,
            source.clone(),
            target.clone(),
            false,
            &budgets,
            &scheduler,
        )
        .await;
        old.input(b"old-active\n").unwrap();
        until(&mut old, b"active:old-active").await;
        let lease_name = format!("host-daemon-{}.json", target.generation.get());
        let old_lease = fixture.checkpoint(&lease_name, 4096);
        let (new, new_server) = pending(
            &fixture,
            source.clone(),
            target.clone(),
            true,
            &budgets,
            &scheduler,
        )
        .await;
        let mut new = new.unwrap();
        new.take_snapshot().unwrap();
        let pending_lease = fixture.checkpoint(&lease_name, 4096);
        assert_eq!(pending_lease["writer_epoch"], old_lease["writer_epoch"]);
        assert_eq!(
            pending_lease["last_attachment"],
            old_lease["last_attachment"]
        );
        assert!(!pending_lease["pending"].is_null());
        let before_refusal = fixture.checkpoint("host-sessions.json", 4 * 1024 * 1024);
        let request = Envelope::new(
            fixture.policy.host.host.clone(),
            OperationId::new(),
            Request::Attach {
                target,
                attachment: AttachmentId::new(),
                takeover: false,
            },
        );
        let (refused, refused_server) =
            pending_once(&fixture, source, &request, &budgets, &scheduler).await;
        assert!(
            matches!(refused, Err(Error::Refused(Refusal::Busy))),
            "refusal: {:?}",
            refused.as_ref().err()
        );
        assert!(refused_server.await.unwrap().is_err());
        assert_eq!(fixture.checkpoint(&lease_name, 4096), pending_lease);
        assert_eq!(
            fixture.checkpoint("host-sessions.json", 4 * 1024 * 1024),
            before_refusal
        );
        old.input(b"old-before\n").unwrap();
        until(&mut old, b"before:old-before").await;
        let mut new = new
            .adopt(&CancellationToken::new())
            .await
            .unwrap()
            .spawn()
            .unwrap();
        let committed_lease = fixture.checkpoint(&lease_name, 4096);
        assert!(committed_lease["pending"].is_null());
        assert_eq!(
            committed_lease["writer_epoch"].as_u64().unwrap(),
            old_lease["writer_epoch"].as_u64().unwrap() + 1
        );
        assert_eq!(
            committed_lease["last_attachment"],
            pending_lease["pending"]["ticket"]
        );
        assert_eq!(committed_lease["native_pid"], old_lease["native_pid"]);
        assert_eq!(committed_lease["generation"], old_lease["generation"]);
        assert_eq!(committed_lease["grant"], old_lease["grant"]);
        let _ = old.input(b"must-not-replay");
        new.input(b"new-after\n").unwrap();
        let output = until(&mut new, b"after:new-after").await;
        assert!(!output.windows(15).any(|bytes| bytes == b"must-not-replay"));
        detached(&mut new).await;
        assert!(new_server.await.unwrap().is_ok());
        drop(old);
        let _ = old_server.await.unwrap();
        drop(control);
        assert!(control_server.await.unwrap().is_err());
    }

    #[tokio::test]
    #[ignore = "requires explicitly built owned native helper and fixture child"]
    async fn native_active_writer_refuses_non_takeover_before_pending_takeover_and_retains_adoption_fences(
    ) {
        let mut fixture = NativeFixture::new(true);
        fixture.policy.capabilities.detach = true;
        let (backend, source) = fixture.backend();
        let budgets = Budgets::default();
        let scheduler = OutputScheduler::default();
        let (mut control, control_server) =
            fixture.connect(backend, source.clone(), &budgets).await;
        let mut creation = fixture.creation();
        creation.recipe.args = vec![
            "emit:ready".into(),
            "read-line".into(),
            "echo:before".into(),
            "read-line".into(),
            "echo:after".into(),
            "spin".into(),
        ];
        let target = create(&fixture, &mut control, creation).await;
        let mut stale = target.clone();
        stale.generation = GenerationId::new(target.generation.get() + 1).unwrap();
        let (refused, refused_server) =
            pending(&fixture, source.clone(), stale, false, &budgets, &scheduler).await;
        assert!(matches!(refused, Err(Error::Refused(Refusal::Stale))));
        assert!(refused_server.await.unwrap().is_err());
        let (mut old, old_server, _) = live(
            &fixture,
            source.clone(),
            target.clone(),
            false,
            &budgets,
            &scheduler,
        )
        .await;
        let lease_name = format!("host-daemon-{}.json", target.generation.get());
        let before_lease = fixture.checkpoint(&lease_name, 4096);
        assert!(before_lease["pending"].is_null());
        let before_record = fixture.checkpoint("host-sessions.json", 4 * 1024 * 1024);
        let request = Envelope::new(
            fixture.policy.host.host.clone(),
            OperationId::new(),
            Request::Attach {
                target: target.clone(),
                attachment: AttachmentId::new(),
                takeover: false,
            },
        );
        let (refused, refused_server) =
            pending_once(&fixture, source.clone(), &request, &budgets, &scheduler).await;
        assert!(
            matches!(refused, Err(Error::Refused(Refusal::Busy))),
            "refusal: {:?}",
            refused.as_ref().err()
        );
        assert!(refused_server.await.unwrap().is_err());
        assert_eq!(fixture.checkpoint(&lease_name, 4096), before_lease);
        assert_eq!(
            fixture.checkpoint("host-sessions.json", 4 * 1024 * 1024),
            before_record
        );
        let (new, new_server) = pending(&fixture, source, target, true, &budgets, &scheduler).await;
        let mut new = new.unwrap();
        new.take_snapshot().unwrap();
        old.input(b"old-before\n").unwrap();
        until(&mut old, b"before:old-before").await;
        let mut new = new
            .adopt(&CancellationToken::new())
            .await
            .unwrap()
            .spawn()
            .unwrap();
        let _ = old.input(b"must-not-replay");
        new.input(b"new-after\n").unwrap();
        let output = until(&mut new, b"after:new-after").await;
        assert!(!output.windows(15).any(|bytes| bytes == b"must-not-replay"));
        detached(&mut new).await;
        assert!(new_server.await.unwrap().is_ok());
        drop(old);
        let _ = old_server.await.unwrap();
        drop(control);
        assert!(control_server.await.unwrap().is_err());
    }

    #[tokio::test]
    #[ignore = "requires explicitly built owned native helper and fixture child"]
    async fn native_temporary_detach_and_pending_cancellation_keep_lease_meanings_distinct() {
        let mut fixture = NativeFixture::new(true);
        fixture.policy.capabilities.temporary = true;
        fixture.policy.capabilities.detach = true;
        let (backend, source) = fixture.backend();
        let budgets = Budgets::default();
        let scheduler = OutputScheduler::default();
        let (mut control, control_server) =
            fixture.connect(backend, source.clone(), &budgets).await;
        let mut creation = fixture.creation();
        creation.lifetime = Lifetime::Temporary {
            reconnect_grace_seconds: 60,
        };
        let initial_attachment = creation.attachment.clone();
        let target = create(&fixture, &mut control, creation).await;
        let (unadopted, pending_server) = pending_using(
            &fixture,
            source.clone(),
            target.clone(),
            initial_attachment.clone(),
            false,
            &budgets,
            &scheduler,
        )
        .await;
        let held_policy = source.0.clone().lock_owned().await;
        drop(unadopted.unwrap());
        assert!(pending_server.await.unwrap().is_err());
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(held_policy);
        assert_eq!(
            festerm_sessiond::list_running_sessions_in(&fixture.root)
                .unwrap()
                .len(),
            1
        );
        let (old, old_server) = pending_using(
            &fixture,
            source.clone(),
            target.clone(),
            initial_attachment,
            false,
            &budgets,
            &scheduler,
        )
        .await;
        let mut old = old.unwrap();
        old.take_snapshot().unwrap();
        let old = old
            .adopt(&CancellationToken::new())
            .await
            .unwrap()
            .spawn()
            .unwrap();
        let old_lease = old.binding().lease.clone().unwrap();
        let (cancelled, cancelled_server) = pending(
            &fixture,
            source.clone(),
            target.clone(),
            true,
            &budgets,
            &scheduler,
        )
        .await;
        drop(cancelled.unwrap());
        assert!(cancelled_server.await.unwrap().is_err());
        tokio::time::sleep(Duration::from_millis(200)).await;
        let (mut terminal, server, _) =
            live(&fixture, source, target, true, &budgets, &scheduler).await;
        let actual = terminal.binding().lease.as_ref().unwrap();
        assert_eq!(actual.epoch, old_lease.epoch.next().unwrap());
        assert_ne!(actual.attachment, old_lease.attachment);
        drop(old);
        let _ = old_server.await.unwrap();
        assert_eq!(
            festerm_sessiond::list_running_sessions_in(&fixture.root)
                .unwrap()
                .len(),
            1
        );
        detached(&mut terminal).await;
        assert!(server.await.unwrap().is_ok());
        tokio::time::timeout(Duration::from_secs(5), async {
            while !festerm_sessiond::list_running_sessions_in(&fixture.root)
                .unwrap()
                .is_empty()
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        drop(control);
        assert!(control_server.await.unwrap().is_err());
    }

    #[tokio::test]
    #[ignore = "requires explicitly built owned native helper and fixture child"]
    async fn native_revocation_tears_down_slow_output_without_stopping_persistent_shell() {
        let fixture = NativeFixture::new(true);
        let (backend, source) = fixture.backend();
        let budgets = Budgets::default();
        let scheduler = OutputScheduler::default();
        let (mut control, control_server) =
            fixture.connect(backend, source.clone(), &budgets).await;
        let mut creation = fixture.creation();
        creation.recipe.args = vec![
            "read-line".into(),
            "emit-repeat:100000:output-flood".into(),
            "spin".into(),
        ];
        let target = create(&fixture, &mut control, creation).await;
        let (terminal, server, _) = live(
            &fixture,
            source.clone(),
            target,
            false,
            &budgets,
            &scheduler,
        )
        .await;
        terminal.input(b"start\n").unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !server.is_finished(),
            "slow output must remain live before revocation"
        );
        assert!(matches!(
            fresh_inventory(&mut control).await,
            Response::Inventory { .. }
        ));
        let mut policy = fixture.policy.clone();
        policy.remote_access = false;
        policy.grant_epoch = policy.grant_epoch.next().unwrap();
        source
            .0
            .try_lock()
            .unwrap()
            .replace(
                policy,
                &[crate::owner_auth::OwnerSigningKey::from_seed(&[17; 32]).public_key()],
            )
            .unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        assert_eq!(
            festerm_sessiond::list_running_sessions_in(&fixture.root)
                .unwrap()
                .len(),
            1
        );
        drop((terminal, control));
        assert!(control_server.await.unwrap().is_err());
    }
}
