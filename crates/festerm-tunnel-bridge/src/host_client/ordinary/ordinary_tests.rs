use super::*;
use crate::host_client::{
    creation_tests::{caps, creation, handshake, respond},
    originals::{OriginalStore, RecoveredIntent},
};
use festerm_host_protocol::{Envelope, Epoch, Lifetime, Refusal, Request, Response};
use festerm_sessiond::private_storage::{private_test_directory, PrivateTestDirectory};
use festerm_tunnel_client::host_control::read_frame;

fn fixture() -> (PrivateTestDirectory, OrdinaryContext) {
    let root = private_test_directory();
    let index = OrdinaryKeyIndex::in_app_sandbox(root.path()).unwrap();
    let host = festerm_host_protocol::HostId::new();
    let info = OrdinaryHostInfo {
        version: 1,
        route: OrdinaryRoute {
            tunnel_id: "fixture".into(),
            cluster: "usw3".into(),
            endpoint_id: format!("{host}-relay"),
            port: 47461,
            host: crate::owner_auth::PinnedHost {
                host,
                host_key_fingerprint: format!("SHA256:{}", "A".repeat(43)),
            },
        },
        installation: festerm_host_protocol::HostId::new(),
        principal: festerm_host_protocol::PrincipalId::new(),
        policy_epoch: Epoch::new(1).unwrap(),
    };
    let context = testing_context(
        index,
        42,
        info,
        OrdinaryAccountGuard::new(CancellationToken::new(), Arc::new(|| Ok(()))),
    )
    .unwrap();
    (root, context)
}
fn changed_info(context: &OrdinaryContext, info: OrdinaryHostInfo) -> OrdinaryContext {
    let key = &context.0.key;
    OrdinaryContext(Arc::new(Inner {
        info,
        binding: context.0.binding.clone(),
        endpoint: context.0.endpoint.clone(),
        token: context.0.token.clone(),
        key: NativeKey {
            key: key.key.clone(),
            reference: key.reference.duplicate_for_transport(),
            index: key.index.clone(),
            vault: key.vault.clone(),
        },
        guard: context.0.guard.clone(),
        fixture_only: true,
    }))
}
fn client(
    stream: tokio::io::DuplexStream,
    context: &OrdinaryContext,
) -> ControlClient<tokio::io::DuplexStream> {
    ControlClient::on_authenticated_stream(
        stream,
        context.binding().clone(),
        ConnectionPermit::reserve(&Budgets::default()).unwrap(),
    )
}
async fn request(peer: &mut tokio::io::DuplexStream) -> Envelope<Request> {
    read_frame(peer, &Budgets::default())
        .await
        .unwrap()
        .decode()
        .unwrap()
}

#[tokio::test]
async fn ordinary_durable_original_preserves_exact_issuance_across_policy_epoch_change() {
    let (root, context) = fixture();
    let (store, originals) = OriginalStore::recover(root.path(), &context).unwrap();
    assert!(originals.is_empty());
    let intent = Arc::new(CreationIntent::new(&context, CreationMode::Create, creation()).unwrap());
    intent
        .protect_original(store.clone(), "normal durable creation".into())
        .unwrap();
    let (stream, mut peer) = tokio::io::duplex(8192);
    let first = tokio::spawn(async move {
        handshake(&mut peer, caps()).await;
        request(&mut peer).await
    });
    assert!(matches!(
        create_authenticated(
            &mut client(stream, &context),
            intent.clone(),
            &CancellationToken::new()
        )
        .await,
        Err(Failure::Control(Error::Offline | Error::Io(_)))
    ));
    let first = first.await.unwrap();
    let issued = intent.issued_operation().unwrap();
    assert!(intent.is_ordinary());
    assert!(matches!(first.body, Request::Create { .. }));
    let mut info = context.host_info().clone();
    info.policy_epoch = Epoch::new(9).unwrap();
    let current = changed_info(&context, info);
    let (reopened, mut originals) = OriginalStore::recover(root.path(), &current).unwrap();
    assert_eq!(originals.len(), 1);
    let RecoveredIntent::Creation(restored) = originals.remove(0).intent else {
        panic!("durable creation")
    };
    assert_eq!(restored.issued_operation().unwrap(), issued);
    assert!(restored.matches_context(&current));
    restored
        .protect_original(reopened, "normal durable creation".into())
        .unwrap();
    let (stream, mut peer) = tokio::io::duplex(8192);
    let expected = first.clone();
    let server = tokio::spawn(async move {
        let mut capabilities = caps();
        capabilities.issuance_epoch = Some(Epoch::new(9).unwrap());
        handshake(&mut peer, capabilities).await;
        let retry = request(&mut peer).await;
        assert_eq!(
            retry, expected,
            "new policy must not mint another UUID/epoch/body"
        );
        respond(
            &mut peer,
            &retry,
            Response::Refused {
                reason: Refusal::OutcomeUncertain,
            },
        )
        .await;
    });
    assert!(matches!(
        create_authenticated(
            &mut client(stream, &current),
            restored,
            &CancellationToken::new()
        )
        .await,
        Err(Failure::Control(Error::Refused(Refusal::OutcomeUncertain)))
    ));
    server.await.unwrap();
    assert_eq!(
        OriginalStore::recover(root.path(), &current)
            .unwrap()
            .1
            .len(),
        1
    );
    let mut retarget = current.host_info().clone();
    retarget.principal = festerm_host_protocol::PrincipalId::new();
    assert!(OriginalStore::recover(root.path(), &changed_info(&current, retarget)).is_err());
}

#[test]
fn ordinary_context_never_becomes_admin_and_refuses_temporary_creation() {
    let (_root, context) = fixture();
    let common: ClientContext = context.clone().into();
    assert!(common.as_enrolled_owner().is_none());
    let durable = creation();
    assert!(CreationIntent::new(&common, CreationMode::Create, durable.clone()).is_ok());
    let mut temporary = durable;
    temporary.lifetime = Lifetime::Temporary {
        reconnect_grace_seconds: 30,
    };
    assert!(CreationIntent::new(&common, CreationMode::Create, temporary).is_err());
    assert!(Failure::Control(Error::HostUpdateRequired)
        .message()
        .contains("Update fesTerm on this host, then try again."));
    assert!(!Failure::Control(Error::Refused(Refusal::Denied))
        .message()
        .contains("Verify explicit owner enrollment"));
}

#[test]
fn ordinary_cancelled_account_refuses_ready_results_and_new_workers() {
    let (_root, context) = fixture();
    let scope = Scope {
        request: OperationId::new(),
        generation: 1,
        binding: context.binding().clone(),
    };
    let (sender, events) = flume::bounded(2);
    let cancel = CancellationToken::new();
    let worker = Worker {
        end_intent: Some(Arc::new(ending::EndAttempt::new(Arc::new(EndIntent::new(
            &context,
            Target {
                logical: LogicalSessionId::new(),
                generation: festerm_host_protocol::GenerationId::new(7).unwrap(),
            },
        ))))),
        scope: scope.clone(),
        cancel: cancel.clone(),
        ordinary_guard: Some(context.account_guard()),
        events,
    };
    sender
        .send(Completion {
            scope: scope.clone(),
            outcome: Outcome::Failed(Failure::ProviderUnavailable),
        })
        .unwrap_or_else(|_| panic!("bounded ready event"));
    assert!(worker
        .end_failure_diagnostic(&scope, Failure::OriginalStorage)
        .is_some());
    context.0.guard.cancel.cancel();
    assert!(worker
        .end_failure_diagnostic(&scope, Failure::OriginalStorage)
        .is_none());
    assert!(worker.try_receive(&scope).is_none());
    assert!(cancel.is_cancelled());
    assert!(Worker::start(
        context,
        scope,
        Operation::Inventory,
        &Budgets::default(),
        Arc::new(|| {})
    )
    .is_err());
}

#[tokio::test]
async fn ordinary_account_cancel_drains_live_worker_but_chooser_cancel_does_not_cancel_account() {
    let (_root, context) = fixture();
    let budgets = Budgets::default();
    let permit = ConnectionPermit::reserve(&budgets).unwrap();
    let scope = Scope {
        request: OperationId::new(),
        generation: 1,
        binding: context.binding().clone(),
    };
    let started = Arc::new(tokio::sync::Notify::new());
    let work_started = started.clone();
    let (done, drained) = tokio::sync::oneshot::channel();
    struct Drain(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for Drain {
        fn drop(&mut self) {
            if let Some(done) = self.0.take() {
                let _ = done.send(());
            }
        }
    }
    let request_cancel = CancellationToken::new();
    let worker = Worker::spawn_with_guard(
        scope,
        Arc::new(|| {}),
        request_cancel.clone(),
        Some(context.account_guard()),
        move |_, cancel, _, _| async move {
            let _permit = permit;
            let _drain = Drain(Some(done));
            work_started.notify_one();
            cancel.cancelled().await;
            Err(Error::Cancelled.into())
        },
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    // A separate chooser's cancellation does not invalidate this captured account.
    let chooser = CancellationToken::new();
    chooser.cancel();
    assert!(context.0.guard.check().is_ok());
    assert!(!worker.is_cancelled());
    context.0.guard.cancel.cancel();
    assert!(worker.is_cancelled());
    tokio::time::timeout(Duration::from_secs(2), drained)
        .await
        .unwrap()
        .unwrap();
    // select! drops the losing task before its cancellation handler runs, so
    // Drain proves task retirement but cannot acknowledge request cancellation.
    tokio::time::timeout(Duration::from_secs(2), request_cancel.cancelled())
        .await
        .unwrap();
    assert!(request_cancel.is_cancelled());
    let charges: Vec<_> = (0..4)
        .map(|_| ConnectionPermit::reserve(&budgets).unwrap())
        .collect();
    drop(charges);
}

#[tokio::test]
async fn ordinary_test_context_cannot_open_real_transport_or_grant_remote_authority() {
    let (_root, context) = fixture();
    let permit = ConnectionPermit::reserve(&Budgets::default()).unwrap();
    assert!(matches!(
        context
            .connect(permit, StreamRole::Control, &CancellationToken::new())
            .await,
        Err(Failure::Control(Error::Contract(ContractError::Denied)))
    ));
}

#[test]
fn reconnect_changed_challenge_never_calls_device_signer() {
    let (_root, context) = fixture();
    for change in ["installation", "principal", "policy", "route", "ssh-pin"] {
        let expected = context.host_info();
        let mut actual = expected.clone();
        match change {
            "installation" => actual.installation = festerm_host_protocol::HostId::new(),
            "principal" => actual.principal = festerm_host_protocol::PrincipalId::new(),
            "policy" => actual.policy_epoch = Epoch::new(2).unwrap(),
            "route" => actual.route.endpoint_id.push('x'),
            "ssh-pin" => actual.route.host.host_key_fingerprint.push('x'),
            _ => unreachable!(),
        }
        let calls = std::cell::Cell::new(0);
        assert!(after_existing_host_check(Some(expected), &actual, || {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .is_err());
        assert_eq!(calls.get(), 0, "{change}");
        assert!(after_existing_host_check(None, &actual, || {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .is_ok());
        assert_eq!(
            calls.get(),
            1,
            "normal discovery retains its initial proof behavior"
        );
    }
    assert!(
        after_existing_host_check(Some(context.host_info()), context.host_info(), || Ok(()))
            .is_ok()
    );
}

#[tokio::test]
async fn ordinary_end_preserves_original_after_lost_reply_and_requires_same_issuer() {
    let (root, context) = fixture();
    let target = Target {
        logical: festerm_host_protocol::LogicalSessionId::new(),
        generation: festerm_host_protocol::GenerationId::new(7).unwrap(),
    };
    let (store, _) = OriginalStore::recover(root.path(), &context).unwrap();
    let intent = Arc::new(EndIntent::new(&context, target.clone()));
    intent
        .protect_original(store, "Terminate the selected shell".into())
        .unwrap();
    let operation = intent.operation().clone();
    let (stream, mut peer) = tokio::io::duplex(8192);
    let expected_target = target.clone();
    let server = tokio::spawn(async move {
        handshake(
            &mut peer,
            festerm_host_protocol::Capabilities {
                explicit_end: true,
                ..Default::default()
            },
        )
        .await;
        let envelope = request(&mut peer).await;
        assert_eq!(
            envelope.body,
            Request::ExplicitEnd {
                target: expected_target
            }
        );
        envelope
    });
    assert!(end_authenticated(
        &mut client(stream, &context),
        intent.clone(),
        &CancellationToken::new()
    )
    .await
    .is_err());
    let first = server.await.unwrap();
    assert!(intent.is_issued() && !intent.is_resolved());
    let (_, mut originals) = OriginalStore::recover(root.path(), &context).unwrap();
    let RecoveredIntent::End(restored) = originals.remove(0).intent else {
        panic!("End original")
    };
    assert_eq!(restored.operation(), &operation);
    assert_eq!(restored.target(), &target);
    let mut replaced = context.host_info().clone();
    replaced.principal = festerm_host_protocol::PrincipalId::new();
    assert!(!restored.matches_context(&changed_info(&context, replaced)));
    let (stream, mut peer) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        handshake(
            &mut peer,
            festerm_host_protocol::Capabilities {
                explicit_end: true,
                ..Default::default()
            },
        )
        .await;
        let retry = request(&mut peer).await;
        assert_eq!(
            retry, first,
            "only an explicit retry retains the original operation and target"
        );
        respond(&mut peer, &retry, Response::Ended { target }).await;
    });
    assert!(matches!(
        end_authenticated(
            &mut client(stream, &context),
            restored,
            &CancellationToken::new()
        )
        .await,
        Ok(Outcome::Ended { .. })
    ));
    server.await.unwrap();
    assert!(OriginalStore::recover(root.path(), &context)
        .unwrap()
        .1
        .is_empty());
}

#[tokio::test]
async fn ordinary_end_default_off_sends_no_end_and_cancelled_account_cannot_start() {
    let (root, context) = fixture();
    let target = Target {
        logical: festerm_host_protocol::LogicalSessionId::new(),
        generation: festerm_host_protocol::GenerationId::new(2).unwrap(),
    };
    let intent = Arc::new(EndIntent::new(&context, target));
    let (store, _) = OriginalStore::recover(root.path(), &context).unwrap();
    intent
        .protect_original(store, "confirmed review".into())
        .unwrap();
    let (stream, mut peer) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        handshake(&mut peer, caps()).await;
        let received = tokio::time::timeout(
            std::time::Duration::from_millis(25),
            read_frame(&mut peer, &Budgets::default()),
        )
        .await;
        assert!(
            !matches!(received, Ok(Ok(_))),
            "no End frame without advertised consent"
        );
    });
    assert!(end_authenticated(
        &mut client(stream, &context),
        intent.clone(),
        &CancellationToken::new()
    )
    .await
    .is_err());
    assert!(!intent.is_issued());
    server.await.unwrap();
    context.0.guard.cancel.cancel();
    assert!(context.account_guard().check().is_err());
    assert!(!intent.is_issued());
}
