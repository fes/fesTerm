use super::*;
use festerm_host_protocol::{
    Creation, EnsureDisposition, Envelope, Epoch, GenerationId, HostId, Lifetime, PrincipalId,
    Recipe, Refusal,
};
use festerm_tunnel_client::host_control::{read_frame, write_frame};
use russh::keys::{Algorithm, HashAlg, PrivateKey};
use tokio::io::{AsyncReadExt, DuplexStream};

pub(super) fn context() -> ApprovedContext {
    let key = PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519).unwrap();
    let public = key.public_key().to_openssh().unwrap();
    let host = HostId::new();
    let binding = Binding::new(host.clone(), "github:1234".into(), "fixture".into()).unwrap();
    ApprovedContext::from_protected_approval(
        OwnerBinding {
            installation: HostId::new().to_string(),
            host: crate::owner_auth::PinnedHost {
                host,
                host_key_fingerprint: key.public_key().fingerprint(HashAlg::Sha256).to_string(),
            },
            account: "github:1234".into(),
            principal: PrincipalId::new(),
        },
        SecretReference::generate(),
        serde_json::from_value(serde_json::json!({
            "connectionMode": "TunnelRelay", "hostId": "fixture",
            "hostPublicKeys": [public.split_whitespace().nth(1).unwrap()],
            "clientRelayUri": "wss://example.invalid/client"
        }))
        .unwrap(),
        22,
        Zeroizing::new("not-a-service-token".into()),
        binding,
    )
    .unwrap()
}

#[test]
fn selected_host_mobile_pre_cancelled_boundary_refuses_before_vault_or_worker() {
    let context = context();
    let scope = Scope {
        request: OperationId::new(),
        generation: 1,
        binding: context.binding().clone(),
    };
    let intent = Arc::new(CreationIntent::new(&context, CreationMode::Ensure, creation()).unwrap());
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        Worker::start_with_cancellation(
            context,
            scope,
            Operation::Create(intent.clone()),
            &Budgets::default(),
            Arc::new(|| {}),
            cancel
        ),
        Err(Failure::Control(Error::Cancelled))
    ));
    assert!(intent.issued_operation().is_none());
    assert!(!intent.is_presented());
}

pub(super) fn creation() -> Creation {
    Creation {
        logical: LogicalSessionId::new(),
        attachment: AttachmentId::new(),
        name: Some("Explicit shell".into()),
        recipe: Recipe::default(),
        lifetime: Lifetime::Persistent {},
    }
}

pub(super) fn caps() -> Capabilities {
    Capabilities {
        create: true,
        ensure: true,
        attach: true,
        detach: true,
        creation_allowed: true,
        issuance_epoch: Some(Epoch::new(1).unwrap()),
        ..Capabilities::default()
    }
}

#[tokio::test]
async fn selected_host_end_refusals_and_lost_reply_retain_original_without_creation_authority() {
    for reason in [
        Some(Refusal::Busy),
        Some(Refusal::Unknown),
        Some(Refusal::Denied),
        Some(Refusal::Offline),
        Some(Refusal::Revoked),
        Some(Refusal::Stale),
        Some(Refusal::OutcomeUncertain),
        None,
    ] {
        let context = context();
        let target = Target {
            logical: LogicalSessionId::new(),
            generation: GenerationId::new(8).unwrap(),
        };
        let intent = Arc::new(EndIntent::new(&context, target.clone()));
        let original = intent.operation().clone();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let capabilities = Capabilities {
            explicit_end: true,
            ..Capabilities::default()
        };
        let server = tokio::spawn(async move {
            handshake(&mut peer, capabilities).await;
            let request = read_frame(&mut peer, &Budgets::default())
                .await
                .unwrap()
                .decode::<Envelope<Request>>()
                .unwrap();
            assert_eq!(request.body, Request::ExplicitEnd { target });
            assert!(
                request.issuance_epoch.is_none(),
                "End is not a creation-issued operation"
            );
            if let Some(reason) = reason {
                respond(&mut peer, &request, Response::Refused { reason }).await;
            }
            request
        });
        let result = end_authenticated(
            &mut client(stream, &context),
            intent.clone(),
            &CancellationToken::new(),
        )
        .await;
        match reason {
            Some(expected) => assert!(
                matches!(result, Err(Failure::Control(Error::Refused(actual))) if actual == expected)
            ),
            None => assert!(matches!(result, Err(Failure::Control(Error::Offline)))),
        }
        assert!(intent.is_issued());
        assert!(!intent.is_resolved());
        assert_eq!(intent.operation(), &original);
        let first = server.await.unwrap();
        let expected = first.clone();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            handshake(
                &mut peer,
                Capabilities {
                    explicit_end: true,
                    ..Capabilities::default()
                },
            )
            .await;
            let retry = read_frame(&mut peer, &Budgets::default())
                .await
                .unwrap()
                .decode::<Envelope<Request>>()
                .unwrap();
            assert_eq!(
                retry, expected,
                "explicit retry cannot change host, operation or exact generation"
            );
            let Request::ExplicitEnd { target } = retry.body.clone() else {
                panic!("no substitution")
            };
            respond(&mut peer, &retry, Response::Ended { target }).await;
        });
        assert!(
            matches!(end_authenticated(&mut client(stream, &context), intent.clone(), &CancellationToken::new()).await.unwrap(),
            Outcome::Ended { target } if &target == intent.target())
        );
        assert!(intent.is_resolved());
        assert_eq!(intent.operation(), &original);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn selected_host_end_requires_capability_and_rejects_replacement_receipt() {
    for supported in [false, true] {
        let context = context();
        let intent = Arc::new(EndIntent::new(
            &context,
            Target {
                logical: LogicalSessionId::new(),
                generation: GenerationId::new(9).unwrap(),
            },
        ));
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            handshake(
                &mut peer,
                Capabilities {
                    explicit_end: supported,
                    ..Capabilities::default()
                },
            )
            .await;
            if supported {
                let request = read_frame(&mut peer, &Budgets::default())
                    .await
                    .unwrap()
                    .decode::<Envelope<Request>>()
                    .unwrap();
                let Request::ExplicitEnd { mut target } = request.body.clone() else {
                    panic!("End")
                };
                target.generation = GenerationId::new(target.generation.get() + 1).unwrap();
                respond(&mut peer, &request, Response::Ended { target }).await;
            } else {
                assert!(
                    peer.read_u8().await.is_err(),
                    "unsupported End must not reach wire"
                );
            }
        });
        let result = end_authenticated(
            &mut client(stream, &context),
            intent.clone(),
            &CancellationToken::new(),
        )
        .await;
        if supported {
            assert!(matches!(
                result,
                Err(Failure::Control(Error::Contract(ContractError::Stale)))
            ));
        } else {
            assert!(matches!(
                result,
                Err(Failure::Control(Error::Refused(Refusal::Unsupported)))
            ));
            assert!(!intent.is_issued());
        }
        assert!(!intent.is_resolved());
        server.await.unwrap();
    }
}

#[test]
fn selected_host_end_context_and_precancel_fence_before_vault_or_worker() {
    let approved = context();
    let intent = Arc::new(EndIntent::new(
        &approved,
        Target {
            logical: LogicalSessionId::new(),
            generation: GenerationId::new(2).unwrap(),
        },
    ));
    let scope = Scope {
        request: OperationId::new(),
        generation: 1,
        binding: approved.binding().clone(),
    };
    let foreign = context();
    assert!(!intent.matches_context(&foreign));
    let mut same_binding_foreign_principal = foreign;
    same_binding_foreign_principal.binding = approved.binding().clone();
    assert!(matches!(
        Worker::start(
            same_binding_foreign_principal,
            scope.clone(),
            Operation::End(intent.clone()),
            &Budgets::default(),
            Arc::new(|| {})
        ),
        Err(Failure::BindingMismatch)
    ));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        Worker::start_with_cancellation(
            approved,
            scope,
            Operation::End(intent.clone()),
            &Budgets::default(),
            Arc::new(|| {}),
            cancel
        ),
        Err(Failure::Control(Error::Cancelled))
    ));
    assert!(!intent.is_issued() && !intent.is_resolved());
}

#[tokio::test]
async fn selected_host_end_single_flight_and_post_send_cancel_keep_original() {
    let context = context();
    let intent = Arc::new(EndIntent::new(
        &context,
        Target {
            logical: LogicalSessionId::new(),
            generation: GenerationId::new(2).unwrap(),
        },
    ));
    let original = intent.operation().clone();
    let (stream, mut peer) = tokio::io::duplex(8192);
    let mut first_client = client(stream, &context);
    let (sent, received) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        handshake(
            &mut peer,
            Capabilities {
                explicit_end: true,
                ..Capabilities::default()
            },
        )
        .await;
        let request = read_frame(&mut peer, &Budgets::default())
            .await
            .unwrap()
            .decode::<Envelope<Request>>()
            .unwrap();
        sent.send(request).unwrap();
        released.await.unwrap();
    });
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let action = intent.clone();
    let first =
        tokio::spawn(async move { end_authenticated(&mut first_client, action, &token).await });
    let request = received.await.unwrap();
    assert_eq!(request.operation, original);
    let (stream, mut second_peer) = tokio::io::duplex(8192);
    assert!(matches!(
        end_authenticated(
            &mut client(stream, &context),
            intent.clone(),
            &CancellationToken::new()
        )
        .await,
        Err(Failure::Control(Error::Contract(ContractError::Busy)))
    ));
    assert!(
        second_peer.read_u8().await.is_err(),
        "single flight emits no second handshake/request"
    );
    cancel.cancel();
    assert!(matches!(
        first.await.unwrap(),
        Err(Failure::Control(Error::Cancelled))
    ));
    assert!(intent.is_issued() && !intent.is_resolved());
    assert_eq!(intent.operation(), &original);
    release.send(()).unwrap();
    server.await.unwrap();
}

pub(super) fn client(
    stream: DuplexStream,
    context: &ApprovedContext,
) -> ControlClient<DuplexStream> {
    ControlClient::on_authenticated_stream(
        stream,
        context.binding.clone(),
        ConnectionPermit::reserve(&Budgets::default()).unwrap(),
    )
}

pub(super) async fn respond(peer: &mut DuplexStream, request: &Envelope<Request>, body: Response) {
    let mut response = Envelope::new(request.host.clone(), request.operation.clone(), body);
    response.issuance_epoch = request.issuance_epoch;
    write_frame(peer, &Budgets::default(), &response)
        .await
        .unwrap();
}

pub(super) async fn handshake(peer: &mut DuplexStream, capabilities: Capabilities) {
    let request = read_frame(peer, &Budgets::default())
        .await
        .unwrap()
        .decode::<Envelope<Request>>()
        .unwrap();
    assert!(matches!(request.body, Request::Capabilities {}));
    respond(peer, &request, Response::Capabilities { capabilities }).await;
}

#[tokio::test]
async fn selected_host_creation_refusals_and_lost_reply_retain_exact_issued_request() {
    for reason in [
        Some(Refusal::Busy),
        Some(Refusal::Unknown),
        Some(Refusal::Denied),
        Some(Refusal::Offline),
        Some(Refusal::Revoked),
        Some(Refusal::OutcomeUncertain),
        None,
    ] {
        for mode in [CreationMode::Create, CreationMode::Ensure] {
            let context = context();
            let intent = Arc::new(CreationIntent::new(&context, mode, creation()).unwrap());
            let (stream, mut peer) = tokio::io::duplex(8192);
            let server = tokio::spawn(async move {
                handshake(&mut peer, caps()).await;
                let request = read_frame(&mut peer, &Budgets::default())
                    .await
                    .unwrap()
                    .decode::<Envelope<Request>>()
                    .unwrap();
                assert!(matches!(
                    (mode, &request.body),
                    (CreationMode::Create, Request::Create { .. })
                        | (CreationMode::Ensure, Request::Ensure { .. })
                ));
                if let Some(reason) = reason {
                    respond(&mut peer, &request, Response::Refused { reason }).await;
                }
                request
            });
            let result = create_authenticated(
                &mut client(stream, &context),
                intent.clone(),
                &CancellationToken::new(),
            )
            .await;
            match reason {
                Some(expected) => assert!(
                    matches!(result, Err(Failure::Control(Error::Refused(actual))) if actual == expected)
                ),
                None => assert!(matches!(result, Err(Failure::Control(Error::Offline)))),
            }
            assert!(!intent.is_resolved());
            let first = server.await.unwrap();
            let issued = intent
                .issued_operation()
                .expect("retain actual captured issuer");
            let expected = first.clone();
            let session = SessionSummary {
                target: Target {
                    logical: intent.creation().logical.clone(),
                    generation: GenerationId::new(42).unwrap(),
                },
                name: "Authoritative result".into(),
                lifetime: Lifetime::Persistent {},
                attached: false,
            };
            let target = session.target.clone();
            let (stream, mut peer) = tokio::io::duplex(8192);
            let server = tokio::spawn(async move {
                let mut fresh = caps();
                fresh.issuance_epoch = Some(Epoch::new(2).unwrap());
                handshake(&mut peer, fresh).await;
                let request = read_frame(&mut peer, &Budgets::default())
                    .await
                    .unwrap()
                    .decode::<Envelope<Request>>()
                    .unwrap();
                assert_eq!(
                    request, expected,
                    "retry cannot regenerate epoch, operation, logical ID, attachment or launch"
                );
                respond(
                    &mut peer,
                    &request,
                    Response::Ensured {
                        session,
                        disposition: EnsureDisposition::Created,
                        lease: None,
                    },
                )
                .await;
            });
            let result = create_authenticated(
                &mut client(stream, &context),
                intent.clone(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
            let Outcome::Created(created) = result else {
                panic!("authoritative exact result required")
            };
            assert_eq!(created.session().target, target);
            assert_eq!(intent.issued_operation(), Some(issued));
            assert!(intent.is_resolved());
            assert!(
                !intent.is_presented(),
                "a wire reply is not frontend delivery"
            );
            server.await.unwrap();
        }
    }
}

#[tokio::test]
async fn selected_host_creation_capability_permission_and_temporary_grant_refuse_without_issuance()
{
    for case in 0..5 {
        let context = context();
        let mut creation = creation();
        let mut capabilities = caps();
        let expected = match case {
            0 => {
                capabilities.create = false;
                Refusal::Unsupported
            }
            1 => {
                capabilities.creation_allowed = false;
                Refusal::Denied
            }
            2 => {
                creation.lifetime = Lifetime::Temporary {
                    reconnect_grace_seconds: 60,
                };
                Refusal::Unsupported
            }
            3 => {
                capabilities.attach = false;
                capabilities.create = false;
                capabilities.ensure = false;
                capabilities.creation_allowed = false;
                Refusal::Unsupported
            }
            _ => {
                capabilities.ensure = false;
                Refusal::Unsupported
            }
        };
        let mode = if case == 4 {
            CreationMode::Ensure
        } else {
            CreationMode::Create
        };
        let intent = Arc::new(CreationIntent::new(&context, mode, creation).unwrap());
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            handshake(&mut peer, capabilities).await;
            let mut remainder = Vec::new();
            peer.read_to_end(&mut remainder).await.unwrap();
            assert!(
                remainder.is_empty(),
                "refusal must not send creation, lookup or terminal input"
            );
        });
        let result = create_authenticated(
            &mut client(stream, &context),
            intent.clone(),
            &CancellationToken::new(),
        )
        .await;
        assert!(
            matches!(result, Err(Failure::Control(Error::Refused(reason))) if reason == expected)
        );
        assert!(intent.issued_operation().is_none());
        assert!(!intent.is_resolved());
        server.await.unwrap();
    }
}

#[test]
fn selected_host_creation_owner_reference_and_binding_fence_before_work() {
    for case in 0..7 {
        let mut context = context();
        let intent =
            Arc::new(CreationIntent::new(&context, CreationMode::Ensure, creation()).unwrap());
        match case {
            0 => context.approved.principal = PrincipalId::new(),
            1 => context.reference = SecretReference::generate(),
            2 => context.approved.installation = HostId::new().to_string(),
            3 => {
                context.binding =
                    Binding::new(HostId::new(), "github:other".into(), "foreign".into()).unwrap()
            }
            4 => context.approved.account = "github:other".into(),
            5 => context.approved.host.host_key_fingerprint.push('x'),
            _ => context.approved.host.host = HostId::new(),
        }
        let scope = Scope {
            request: OperationId::new(),
            generation: 1,
            binding: context.binding.clone(),
        };
        let result = Worker::start(
            context,
            scope,
            Operation::Create(intent.clone()),
            &Budgets::default(),
            Arc::new(|| panic!("refused before native vault/network/thread")),
        );
        assert!(matches!(result, Err(Failure::BindingMismatch)));
        assert!(intent.issued_operation().is_none());
    }
}

#[tokio::test]
async fn selected_host_creation_cancelled_action_never_issues_or_implies_absence() {
    let context = context();
    let intent = Arc::new(CreationIntent::new(&context, CreationMode::Ensure, creation()).unwrap());
    let (stream, mut peer) = tokio::io::duplex(8192);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        create_authenticated(&mut client(stream, &context), intent.clone(), &cancel).await,
        Err(Failure::Control(Error::Cancelled))
    ));
    let mut bytes = Vec::new();
    peer.read_to_end(&mut bytes).await.unwrap();
    assert!(bytes.is_empty());
    assert!(intent.issued_operation().is_none());
    assert!(!intent.is_resolved());
}

#[tokio::test]
async fn selected_host_creation_inflight_duplicate_is_busy_without_second_request() {
    let context = context();
    let intent = Arc::new(CreationIntent::new(&context, CreationMode::Create, creation()).unwrap());
    let (stream, mut peer) = tokio::io::duplex(8192);
    let mut first = client(stream, &context);
    let (seen, arrived) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        handshake(&mut peer, caps()).await;
        let request = read_frame(&mut peer, &Budgets::default())
            .await
            .unwrap()
            .decode::<Envelope<Request>>()
            .unwrap();
        seen.send(()).unwrap();
        let mut bytes = Vec::new();
        peer.read_to_end(&mut bytes).await.unwrap();
        assert!(
            bytes.is_empty(),
            "cancellation cannot send another request or input"
        );
        request
    });
    let cancel = CancellationToken::new();
    let running = cancel.clone();
    let original = intent.clone();
    let first =
        tokio::spawn(async move { create_authenticated(&mut first, original, &running).await });
    arrived.await.unwrap();
    let issued = intent.issued_operation().unwrap();
    let (stream, mut peer) = tokio::io::duplex(8192);
    let result = create_authenticated(
        &mut client(stream, &context),
        intent.clone(),
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(
        result,
        Err(Failure::Control(Error::Contract(ContractError::Busy)))
    ));
    let mut bytes = Vec::new();
    peer.read_to_end(&mut bytes).await.unwrap();
    assert!(bytes.is_empty());
    cancel.cancel();
    assert!(matches!(
        first.await.unwrap(),
        Err(Failure::Control(Error::Cancelled))
    ));
    let request = server.await.unwrap();
    assert_eq!(request.operation, issued.id);
    assert_eq!(intent.issued_operation(), Some(issued));
    assert!(!intent.is_resolved());
    assert!(!intent.is_presented());
}

#[tokio::test]
async fn selected_host_creation_confirmed_reply_without_frontend_admission_retains_action() {
    let context = context();
    let scope = Scope {
        request: OperationId::new(),
        generation: 1,
        binding: context.binding.clone(),
    };
    let intent = Arc::new(CreationIntent::new(&context, CreationMode::Create, creation()).unwrap());
    let (stream, mut peer) = tokio::io::duplex(8192);
    let selection = intent.creation().clone();
    let server = tokio::spawn(async move {
        handshake(&mut peer, caps()).await;
        let request = read_frame(&mut peer, &Budgets::default())
            .await
            .unwrap()
            .decode::<Envelope<Request>>()
            .unwrap();
        respond(
            &mut peer,
            &request,
            Response::Ensured {
                session: SessionSummary {
                    target: Target {
                        logical: selection.logical,
                        generation: GenerationId::new(77).unwrap(),
                    },
                    name: "Confirmed".into(),
                    lifetime: selection.lifetime,
                    attached: false,
                },
                disposition: EnsureDisposition::Created,
                lease: None,
            },
        )
        .await;
    });
    let result = create_authenticated(
        &mut client(stream, &context),
        intent.clone(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    server.await.unwrap();
    let Outcome::Created(created) = result else {
        panic!("authoritative creation required")
    };
    assert!(intent.is_resolved());
    assert!(!intent.is_presented());
    let original = intent.issued_operation();
    let attachment = ApprovedAttachment::from_created(context, scope, *created).unwrap();
    let budgets = Budgets::default();
    let _permits: Vec<_> = (0..4)
        .map(|_| ConnectionPermit::reserve(&budgets).unwrap())
        .collect();
    struct Silent;
    impl festerm_session::SessionEventNotifier for Silent {
        fn notify(&self) {}
    }
    assert!(matches!(
        attachment.start(&budgets, Arc::new(Silent)),
        Err(Failure::Control(Error::Contract(ContractError::Busy)))
    ));
    assert!(
        !intent.is_presented(),
        "a lost/cancelled or uninstalled completion must not retire the original action"
    );
    assert_eq!(intent.issued_operation(), original);
}

#[tokio::test]
async fn selected_host_creation_resume_only_ensure_delegates_absence_to_host_not_client() {
    for refusal in [false, true] {
        let context = context();
        let intent =
            Arc::new(CreationIntent::new(&context, CreationMode::Ensure, creation()).unwrap());
        let selection = intent.creation().clone();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            let mut capabilities = caps();
            capabilities.creation_allowed = false;
            handshake(&mut peer, capabilities).await;
            let request = read_frame(&mut peer, &Budgets::default())
                .await
                .unwrap()
                .decode::<Envelope<Request>>()
                .unwrap();
            assert!(
                matches!(request.body, Request::Ensure { .. }),
                "no client-side lookup/absence/create fallback"
            );
            let body = if refusal {
                Response::Refused {
                    reason: Refusal::Denied,
                }
            } else {
                Response::Ensured {
                    session: SessionSummary {
                        target: Target {
                            logical: selection.logical,
                            generation: GenerationId::new(77).unwrap(),
                        },
                        name: "Existing only".into(),
                        lifetime: selection.lifetime,
                        attached: true,
                    },
                    disposition: EnsureDisposition::Resumed,
                    lease: None,
                }
            };
            respond(&mut peer, &request, body).await;
        });
        let result = create_authenticated(
            &mut client(stream, &context),
            intent.clone(),
            &CancellationToken::new(),
        )
        .await;
        if refusal {
            assert!(matches!(
                result,
                Err(Failure::Control(Error::Refused(Refusal::Denied)))
            ));
            assert!(!intent.is_resolved());
        } else {
            let Outcome::Created(result) = result.unwrap() else {
                panic!("host-confirmed resume required")
            };
            assert_eq!(result.disposition(), EnsureDisposition::Resumed);
            assert_eq!(
                result.session().target.generation,
                GenerationId::new(77).unwrap()
            );
        }
        assert!(intent.issued_operation().is_some());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn selected_host_creation_incompatible_reply_is_uncertain_not_proof_of_no_shell() {
    let context = context();
    let intent = Arc::new(CreationIntent::new(&context, CreationMode::Create, creation()).unwrap());
    let (stream, mut peer) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        handshake(&mut peer, caps()).await;
        let request = read_frame(&mut peer, &Budgets::default())
            .await
            .unwrap()
            .decode::<Envelope<Request>>()
            .unwrap();
        respond(
            &mut peer,
            &request,
            Response::Ensured {
                session: SessionSummary {
                    target: Target {
                        logical: LogicalSessionId::new(),
                        generation: GenerationId::new(77).unwrap(),
                    },
                    name: "Wrong correlated target".into(),
                    lifetime: Lifetime::Persistent {},
                    attached: false,
                },
                disposition: EnsureDisposition::Created,
                lease: None,
            },
        )
        .await;
    });
    let result = create_authenticated(
        &mut client(stream, &context),
        intent.clone(),
        &CancellationToken::new(),
    )
    .await;
    let Err(error @ Failure::Control(Error::Contract(ContractError::Stale))) = result else {
        panic!("foreign generation/logical reply must be refused without an absence claim")
    };
    assert!(error.message().contains("not known"));
    assert!(!error.message().contains("No shell was created"));
    assert!(intent.issued_operation().is_some());
    assert!(!intent.is_resolved());
    assert!(!intent.is_presented());
    server.await.unwrap();
}
