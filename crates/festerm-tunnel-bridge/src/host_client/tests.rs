use super::*;
use festerm_core::Dimensions;
use festerm_host_protocol::{
    Cursor, Envelope, Epoch, GenerationId, HostId, Lifetime, LogicalSessionId, Refusal,
};
use festerm_tunnel_client::host_control::{read_frame, write_frame};
use tokio::io::DuplexStream;

fn binding() -> Binding {
    Binding::new(HostId::new(), "github:1234".into(), "host-endpoint".into()).unwrap()
}

fn scope() -> Scope {
    Scope {
        request: OperationId::new(),
        generation: 1,
        binding: binding(),
    }
}

fn terminal() -> Terminal {
    Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap()
}

fn summary() -> SessionSummary {
    SessionSummary {
        target: Target {
            logical: LogicalSessionId::new(),
            generation: GenerationId::new(42).unwrap(),
        },
        name: "Existing shared shell".into(),
        lifetime: Lifetime::Persistent {},
        attached: false,
    }
}

async fn reply(peer: &mut DuplexStream, budgets: &Budgets, body: Response) -> Envelope<Request> {
    let request: Envelope<Request> = read_frame(peer, budgets).await.unwrap().decode().unwrap();
    write_frame(
        peer,
        budgets,
        &Envelope::new(request.host.clone(), request.operation.clone(), body),
    )
    .await
    .unwrap();
    request
}

async fn hello(peer: &mut DuplexStream, budgets: &Budgets, inventory: bool) {
    let request = reply(
        peer,
        budgets,
        Response::Capabilities {
            capabilities: Capabilities {
                inventory,
                ..Capabilities::default()
            },
        },
    )
    .await;
    assert!(matches!(request.body, Request::Capabilities {}));
}

fn client(stream: DuplexStream) -> ControlClient<DuplexStream> {
    ControlClient::on_authenticated_stream(
        stream,
        binding(),
        ConnectionPermit::reserve(&Budgets::default()).unwrap(),
    )
}

#[test]
fn selected_host_worker_refuses_foreign_scope_before_vault_or_network_and_releases_budget() {
    use crate::owner_auth::PinnedHost;
    use festerm_host_protocol::PrincipalId;
    use russh::keys::{Algorithm, HashAlg, PrivateKey};
    let key = PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519).unwrap();
    let public = key.public_key().to_openssh().unwrap();
    let public = public.split_whitespace().nth(1).unwrap();
    let approved = OwnerBinding {
        installation: HostId::new().to_string(),
        host: PinnedHost {
            host: HostId::new(),
            host_key_fingerprint: key.public_key().fingerprint(HashAlg::Sha256).to_string(),
        },
        account: "github:1234".into(),
        principal: PrincipalId::new(),
    };
    let destination = Binding::new(
        approved.host.host.clone(),
        approved.account.clone(),
        "owned-endpoint".into(),
    )
    .unwrap();
    let endpoint: TunnelEndpoint = serde_json::from_value(serde_json::json!({
        "connectionMode": "TunnelRelay", "hostId": "owned-relay-host",
        "hostPublicKeys": [public], "clientRelayUri": "wss://example.invalid/client"
    }))
    .unwrap();
    let context = ApprovedContext::from_protected_approval(
        approved,
        SecretReference::generate(),
        endpoint,
        22,
        Zeroizing::new("fixture-token".into()),
        destination,
    )
    .unwrap();
    let budgets = Budgets::default();
    let result = Worker::start(
        context,
        scope(),
        Operation::Inventory,
        &budgets,
        Arc::new(|| {
            panic!("a refused worker cannot notify or execute");
        }),
    );
    assert!(matches!(result, Err(Failure::BindingMismatch)));
    let permits: Vec<_> = (0..festerm_host_protocol::MAX_CLIENTS)
        .map(|_| ConnectionPermit::reserve(&budgets).unwrap())
        .collect();
    assert!(ConnectionPermit::reserve(&budgets).is_err());
    drop(permits);
    assert!(ConnectionPermit::reserve(&budgets).is_ok());
}

#[test]
fn selected_host_worker_account_request_generation_and_lock_fence_late_results() {
    for change in 0..4 {
        let scope = scope();
        let (sender, events) = flume::bounded(2);
        let worker = Worker {
            scope: scope.clone(),
            cancel: CancellationToken::new(),
            events,
        };
        sender
            .try_send(Completion {
                scope: scope.clone(),
                outcome: Outcome::Failed(Failure::MissingApproval),
            })
            .unwrap_or_else(|_| panic!("bounded event slot"));
        let mut current = scope;
        match change {
            0 => current.request = OperationId::new(),
            1 => current.generation += 1,
            2 => current.binding = binding(),
            _ => worker.cancel(),
        }
        assert!(worker.try_receive(&current).is_none());
        assert!(worker.cancel.is_cancelled());
    }
}

#[tokio::test]
async fn selected_host_inventory_preserves_unknowns_and_original_request_without_attachment() {
    let (stream, mut peer) = tokio::io::duplex(8192);
    let operation = OperationId::new();
    let expected = operation.clone();
    let session = summary();
    let unknown = summary().target;
    let expected_session = session.clone();
    let expected_unknown = unknown.clone();
    let server = tokio::spawn(async move {
        let budgets = Budgets::default();
        hello(&mut peer, &budgets, true).await;
        let request = reply(
            &mut peer,
            &budgets,
            Response::Inventory {
                page: InventoryPage {
                    snapshot: OperationId::new(),
                    authorization_epoch: Epoch::new(1).unwrap(),
                    sessions: vec![session],
                    unknown: vec![unknown],
                    next: None,
                },
            },
        )
        .await;
        assert_eq!(request.operation, expected);
        assert_eq!(request.body, Request::List { cursor: None });
    });
    let inventory =
        inventory_authenticated(&mut client(stream), operation, &CancellationToken::new())
            .await
            .unwrap();
    assert_eq!(inventory.sessions, vec![expected_session]);
    assert_eq!(inventory.unknown, vec![expected_unknown]);
    assert!(!inventory.capabilities.ensure && !inventory.capabilities.create);
    server.await.unwrap();
}

#[tokio::test]
async fn selected_host_inventory_collects_current_pages_with_the_original_request_and_unknowns() {
    let (stream, mut peer) = tokio::io::duplex(32768);
    let operation = OperationId::new();
    let expected_operation = operation.clone();
    let mut sessions: Vec<_> = (0..=PAGE_SIZE).map(|_| summary()).collect();
    sessions.sort_by(|a, b| a.target.logical.cmp(&b.target.logical));
    let expected_sessions = sessions.clone();
    let unknown = summary().target;
    let expected_unknown = unknown.clone();
    let server = tokio::spawn(async move {
        let budgets = Budgets::default();
        hello(&mut peer, &budgets, true).await;
        let snapshot = OperationId::new();
        let last = sessions.split_off(PAGE_SIZE);
        for (page, expected_cursor) in [
            (
                InventoryPage {
                    snapshot: snapshot.clone(),
                    authorization_epoch: Epoch::new(1).unwrap(),
                    sessions,
                    unknown: vec![unknown.clone()],
                    next: Some(Cursor {
                        snapshot: snapshot.clone(),
                        offset: 16,
                    }),
                },
                None,
            ),
            (
                InventoryPage {
                    snapshot: snapshot.clone(),
                    authorization_epoch: Epoch::new(1).unwrap(),
                    sessions: last,
                    unknown: vec![unknown],
                    next: None,
                },
                Some(Cursor {
                    snapshot,
                    offset: 16,
                }),
            ),
        ] {
            let request = reply(&mut peer, &budgets, Response::Inventory { page }).await;
            assert_eq!(request.operation, expected_operation);
            assert_eq!(
                request.body,
                Request::List {
                    cursor: expected_cursor
                }
            );
        }
    });
    let inventory =
        inventory_authenticated(&mut client(stream), operation, &CancellationToken::new())
            .await
            .unwrap();
    assert_eq!(inventory.sessions, expected_sessions);
    assert_eq!(inventory.unknown, vec![expected_unknown]);
    server.await.unwrap();
}

#[tokio::test]
async fn selected_host_inventory_refusals_never_become_authoritative_absence() {
    for reason in [
        Refusal::Denied,
        Refusal::Revoked,
        Refusal::Busy,
        Refusal::Offline,
        Refusal::Unknown,
        Refusal::Incompatible,
        Refusal::OutcomeUncertain,
    ] {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            let budgets = Budgets::default();
            hello(&mut peer, &budgets, true).await;
            reply(&mut peer, &budgets, Response::Refused { reason }).await;
        });
        assert_eq!(
            inventory_authenticated(
                &mut client(stream),
                OperationId::new(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err(),
            Error::Refused(reason)
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn selected_host_inventory_rejects_cross_page_epoch_drift() {
    let (stream, mut peer) = tokio::io::duplex(32768);
    let server = tokio::spawn(async move {
        let budgets = Budgets::default();
        hello(&mut peer, &budgets, true).await;
        let snapshot = OperationId::new();
        let mut sessions: Vec<_> = (0..PAGE_SIZE).map(|_| summary()).collect();
        sessions.sort_by(|a, b| a.target.logical.cmp(&b.target.logical));
        reply(
            &mut peer,
            &budgets,
            Response::Inventory {
                page: InventoryPage {
                    snapshot: snapshot.clone(),
                    authorization_epoch: Epoch::new(1).unwrap(),
                    sessions,
                    unknown: vec![],
                    next: Some(Cursor {
                        snapshot: snapshot.clone(),
                        offset: 16,
                    }),
                },
            },
        )
        .await;
        let request = reply(
            &mut peer,
            &budgets,
            Response::Inventory {
                page: InventoryPage {
                    snapshot: snapshot.clone(),
                    authorization_epoch: Epoch::new(2).unwrap(),
                    sessions: vec![],
                    unknown: vec![],
                    next: None,
                },
            },
        )
        .await;
        assert_eq!(
            request.body,
            Request::List {
                cursor: Some(Cursor {
                    snapshot,
                    offset: 16
                }),
            }
        );
    });
    assert_eq!(
        inventory_authenticated(
            &mut client(stream),
            OperationId::new(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err(),
        Error::Contract(ContractError::Stale)
    );
    server.await.unwrap();
}

#[tokio::test]
async fn selected_host_inventory_unsupported_and_cancelled_do_not_start_a_list() {
    let (stream, mut peer) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        hello(&mut peer, &Budgets::default(), false).await;
        use tokio::io::AsyncReadExt;
        assert!(peer.read_u8().await.is_err());
    });
    assert_eq!(
        inventory_authenticated(
            &mut client(stream),
            OperationId::new(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err(),
        Error::Refused(Refusal::Unsupported)
    );
    server.await.unwrap();
    let (stream, _peer) = tokio::io::duplex(8192);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        inventory_authenticated(&mut client(stream), OperationId::new(), &cancel)
            .await
            .unwrap_err(),
        Error::Cancelled
    );
}

#[tokio::test]
async fn selected_host_snapshot_adoption_fences_request_account_generation_and_cancel() {
    for change in 0..4 {
        let original = scope();
        let mut current = original.clone();
        match change {
            0 => current.request = OperationId::new(),
            1 => current.generation += 1,
            2 => current.binding = binding(),
            _ => {}
        }
        let (acknowledge, adopted) = tokio::sync::oneshot::channel();
        let mut presented = terminal();
        presented.ingest(b"original frontend");
        let mut recovered = terminal();
        recovered.ingest(b"recovered frontend");
        let adoption = Adoption {
            scope: original,
            terminal: recovered,
            acknowledge,
        };
        if change == 3 {
            drop(adopted);
        }
        assert_eq!(
            adoption.adopt_into(&current, &mut presented),
            Err(Failure::Control(Error::Contract(ContractError::Stale)))
        );
        assert!(presented
            .text_snapshot()
            .text()
            .contains("original frontend"));
    }
    let current = scope();
    let (acknowledge, adopted) = tokio::sync::oneshot::channel();
    let mut recovered = terminal();
    recovered.ingest(b"actually adopted");
    let adoption = Adoption {
        scope: current.clone(),
        terminal: recovered,
        acknowledge,
    };
    let mut presented = terminal();
    adoption.adopt_into(&current, &mut presented).unwrap();
    assert!(presented
        .text_snapshot()
        .text()
        .contains("actually adopted"));
    adopted.await.unwrap();
}
