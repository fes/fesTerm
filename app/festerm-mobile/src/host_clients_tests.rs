use super::*;
use crate::{Lifecycle, SuspensionHandle};
use festerm_core::{Dimensions, Terminal};
use festerm_host_protocol::{Capabilities, Envelope, GenerationId, HostId, Request, Response};
use festerm_session::{Session, SessionLifecycle};
use std::{cell::Cell, rc::Rc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn app() -> MobileApp {
    MobileApp::new(Rc::new(Cell::new(Lifecycle {
        active: true,
        ..Lifecycle::default()
    })))
}

#[test]
fn mobile_host_open_is_offline_and_lock_callback_cancels_without_another_frame() {
    let mut app = app();
    app.open_hosts();
    assert!(app.hosts.open);
    let context = egui::Context::default();
    let mut rendered = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(390.0, 844.0),
            )),
            ..Default::default()
        },
        |ui| app.show_hosts(ui.ctx()),
    );
    assert!(!rendered.shapes.is_empty());
    rendered.textures_delta.clear();
    assert!(app.hosts.pending.is_none());
    assert!(app.hosts.worker.is_none());
    assert!(app.hosts.receipt.is_none());
    assert!(!app.hosts.trust && !app.hosts.enrolled && !app.hosts.launch);
    app.host_action(&egui::Context::default(), Action::NewReview)
        .unwrap();
    assert!(app.hosts.selected.is_none() && app.hosts.pending.is_none());
    let epoch = app.sink.live.host.epoch();
    let token = app.sink.live.host.token(epoch);
    let callback = app.sink.live.content_lock_callback();
    callback();
    assert!(token.is_cancelled());
    assert!(!app.host_foreground());
    assert!(app.sink.live.host.token(epoch).is_cancelled());
}

#[test]
fn mobile_host_suspend_fences_pending_work_and_never_arms_automatic_creation() {
    let mut app = app();
    app.open_hosts();
    let token = app.sink.live.host.token(app.hosts.epoch);
    app.sink.live.suspend();
    assert!(token.is_cancelled());
    assert!(!app.host_foreground());
    assert!(app.hosts.originals.is_empty());
    assert!(app
        .host_action(&egui::Context::default(), Action::Create)
        .is_err());
    assert!(app.hosts.worker.is_none());
}

#[test]
fn mobile_host_private_generation_exhaustion_refuses_new_work() {
    let privacy = Privacy::default();
    privacy.epoch.store(u64::MAX, Ordering::Release);
    privacy.invalidate();
    assert_eq!(privacy.epoch(), u64::MAX);
    assert!(privacy.token(u64::MAX).is_cancelled());
}

#[test]
fn mobile_host_private_generation_concurrent_invalidation_preserves_cancellation_and_saturates() {
    for initial in [7, u64::MAX - 4] {
        let privacy = Arc::new(Privacy::default());
        privacy.epoch.store(initial, Ordering::Release);
        let token = privacy.token(initial);
        assert!(!token.is_cancelled());
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let privacy = privacy.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let mut previous = privacy.epoch();
                    for _ in 0..64 {
                        privacy.invalidate();
                        let current = privacy.epoch();
                        assert!(current > previous || current == u64::MAX);
                        previous = current;
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let expected = initial.saturating_add(8 * 64);
        assert_eq!(privacy.epoch(), expected);
        assert!(token.is_cancelled());
        assert!(privacy.token(initial).is_cancelled());
        assert_eq!(privacy.token(expected).is_cancelled(), expected == u64::MAX);
    }
}

#[test]
fn mobile_host_save_and_reviewed_import_are_metadata_only_not_execution_approval() {
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    let mut app = app().with_profiles(profiles);
    app.open_hosts();
    let recipe = PublicRecipe::persistent(
        "Imported mobile",
        HostReference {
            id: HostId::new(),
            account: AccountReference::Github { account_id: 7 },
        },
        Recipe::default(),
    )
    .unwrap();
    app.hosts.selected = Some(recipe.clone());
    app.host_action(&egui::Context::default(), Action::Save)
        .unwrap();
    assert_eq!(
        store.lock().unwrap().host_recipes().get(&recipe.id),
        Some(&recipe)
    );
    let mut imported = recipe;
    imported.id = festerm_mobile_profiles::host_recipes::RecipeId::new();
    app.hosts.import = imported.export().unwrap();
    assert!(app
        .host_action(&egui::Context::default(), Action::Import)
        .is_err());
    app.hosts.import_reviewed = true;
    app.host_action(&egui::Context::default(), Action::Import)
        .unwrap();
    assert_eq!(
        store.lock().unwrap().host_recipes().get(&imported.id),
        Some(&imported)
    );
    assert!(
        app.hosts.receipt.is_none()
            && app.hosts.resolved.is_none()
            && app.hosts.inventory.is_none()
    );
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
    assert!(!app.hosts.trust && !app.hosts.enrolled && !app.hosts.launch);
    assert!(app.hosts.originals.is_empty());
    app.hosts.account = festerm_mobile_tunnels::testing::seed_account(&store, 7, "mobile", false);
    app.hosts.trust = true;
    app.hosts.enrolled = true;
    app.hosts.launch = true;
    assert!(app
        .host_action(&egui::Context::default(), Action::Resolve)
        .is_err());
    assert!(app
        .host_action(&egui::Context::default(), Action::Create)
        .is_err());
    assert!(
        app.hosts.pending.is_none() && app.hosts.worker.is_none() && app.hosts.originals.is_empty(),
        "even forged UI flags plus public metadata cannot substitute for a protected receipt"
    );
}

async fn frame(peer: &mut tokio::io::DuplexStream, kind: u8, bytes: &[u8]) {
    peer.write_u8(kind).await.unwrap();
    peer.write_u32(bytes.len() as u32).await.unwrap();
    peer.write_all(bytes).await.unwrap();
    peer.flush().await.unwrap();
}

async fn request(peer: &mut tokio::io::DuplexStream) -> Envelope<Request> {
    let _kind = peer.read_u8().await.unwrap();
    let count = peer.read_u32().await.unwrap();
    assert!(count <= 128 * 1024);
    let mut bytes = vec![0; count as usize];
    peer.read_exact(&mut bytes).await.unwrap();
    festerm_host_protocol::decode(&bytes).unwrap()
}

async fn response(peer: &mut tokio::io::DuplexStream, request: &Envelope<Request>, body: Response) {
    let bytes = festerm_host_protocol::encode(&Envelope::new(
        request.host.clone(),
        request.operation.clone(),
        body,
    ))
    .unwrap();
    frame(
        peer,
        festerm_tunnel_bridge::host_terminal::HOST_REPLY,
        &bytes,
    )
    .await;
}

#[tokio::test]
async fn mobile_host_installed_frontend_adopts_before_running_and_lock_never_detaches_or_replays() {
    installed_host_round_trip(false, false).await;
}

#[tokio::test]
async fn mobile_host_close_retires_correlated_attachment_without_end_or_late_frontend_mutation() {
    installed_host_round_trip(true, false).await;
}

#[tokio::test]
async fn mobile_host_disconnect_uses_correlated_retirement_not_end_or_transport_abandonment() {
    installed_host_round_trip(true, true).await;
}

async fn installed_host_round_trip(intentional_close: bool, disconnect: bool) {
    let context = egui::Context::default();
    let host = HostId::new();
    let scope = Scope {
        request: OperationId::new(),
        generation: 7,
        binding: Binding::new(host, "github:7".into(), "fixture-endpoint".into()).unwrap(),
    };
    let target = host_client::Target {
        logical: LogicalSessionId::new(),
        generation: GenerationId::new(9).unwrap(),
    };
    let (stream, mut peer) = tokio::io::duplex(128 * 1024);
    let permit = ConnectionPermit::reserve(&Budgets::default()).unwrap();
    let session = host_client::testing::session_from_stream(
        stream,
        permit,
        scope,
        target,
        Arc::new(live_ssh::Repaint(context.clone())),
    )
    .unwrap();
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    let account = festerm_mobile_tunnels::testing::seed_account(&store, 7, "mobile", false);
    let mut app = app().with_profiles(profiles);
    app.open_hosts();
    app.hosts.account = account.clone();
    app.hosts.approval_account = store
        .lock()
        .unwrap()
        .accounts()
        .iter()
        .find(|current| current.id == account)
        .cloned();
    app.install_host_session(&context, "Owned mobile HOST".into(), session.clone());
    let host_tab = app.active_tab.unwrap();
    assert!(app.active_is_host());
    assert!(
        !app.active_is_local(),
        "an actual HOST tab is never an offline fixture"
    );
    assert!(app.chrome().subtitle.contains("HOST"));
    assert!(app
        .tab_surfaces()
        .iter()
        .any(|tab| tab.id == host_tab && tab.kind == crate::model::SurfaceKind::DevTunnel));
    assert!(!app
        .sink
        .live
        .connection
        .borrow()
        .as_ref()
        .unwrap()
        .input_enabled());
    let (snapshot_sent, received_snapshot) = tokio::sync::oneshot::channel();
    let (activation_allowed, allow_activation) = tokio::sync::oneshot::channel();
    let (detach_allowed, allow_detach) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let hello = request(&mut peer).await;
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
        let attach = request(&mut peer).await;
        let Request::Attach {
            target, attachment, ..
        } = &attach.body
        else {
            panic!("exact attach")
        };
        let target = target.clone();
        let attachment = attachment.clone();
        let mut terminal = Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap();
        terminal.ingest(b"OWNED-MOBILE-SNAPSHOT");
        let snapshot =
            festerm_tunnel_bridge::protocol::encode_snapshot(&terminal.recovery_clone()).unwrap();
        frame(
            &mut peer,
            festerm_tunnel_bridge::protocol::SNAPSHOT,
            &snapshot,
        )
        .await;
        snapshot_sent.send(()).unwrap();
        assert_eq!(
            peer.read_u8().await.unwrap(),
            festerm_tunnel_bridge::protocol::ADOPTED
        );
        assert_eq!(peer.read_u32().await.unwrap(), 0);
        allow_activation.await.unwrap();
        response(
            &mut peer,
            &attach,
            Response::Attached {
                target: target.clone(),
                attachment: attachment.clone(),
                lease: None,
            },
        )
        .await;
        frame(
            &mut peer,
            festerm_tunnel_bridge::protocol::OUTPUT,
            b"\r\nOWNED-MOBILE-OUTPUT",
        )
        .await;
        if intentional_close {
            let detach = request(&mut peer).await;
            assert!(
                matches!(&detach.body, Request::Detach {
                target: actual, attachment: actual_attachment, lease: None, lease_epoch: None,
                reason: festerm_host_protocol::DetachKind::Intentional,
            } if actual == &target && actual_attachment == &attachment),
                "close emits only exact attachment detach, never ExplicitEnd/input/replay"
            );
            allow_detach.await.unwrap();
            response(&mut peer, &detach, Response::Detached { target }).await;
        } else {
            let mut byte = [0];
            assert_eq!(
                peer.read(&mut byte).await.unwrap(),
                0,
                "native lock abandons transport; it must not emit input, replay or intentional detach"
            );
        }
    });
    received_snapshot.await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !session.has_pending_snapshot() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(session.lifecycle(), SessionLifecycle::Starting);
    assert!(!app
        .sink
        .live
        .connection
        .borrow()
        .as_ref()
        .unwrap()
        .input_enabled());
    app.sink
        .live
        .connection
        .borrow_mut()
        .as_mut()
        .unwrap()
        .poll(&mut app.terminal, &context);
    assert!(app
        .terminal
        .text_snapshot()
        .text()
        .contains("OWNED-MOBILE-SNAPSHOT"));
    assert!(
        !app.sink
            .live
            .connection
            .borrow()
            .as_ref()
            .unwrap()
            .input_enabled(),
        "snapshot acknowledgment alone is not activation"
    );
    activation_allowed.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !app
            .terminal
            .text_snapshot()
            .text()
            .contains("OWNED-MOBILE-OUTPUT")
        {
            app.sink
                .live
                .connection
                .borrow_mut()
                .as_mut()
                .unwrap()
                .poll(&mut app.terminal, &context);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(app
        .sink
        .live
        .connection
        .borrow()
        .as_ref()
        .unwrap()
        .input_enabled());
    assert!(!app
        .terminal
        .text_snapshot()
        .text()
        .contains("FOREIGN-MOBILE"));
    assert_eq!(
        app.chrome().line.state,
        crate::model::SurfaceState::Connected
    );
    if intentional_close {
        if disconnect {
            app.apply_shell_actions(
                &context,
                vec![crate::shell::ShellAction::DisconnectSurface(host_tab)],
            );
            assert!(app.tabs.iter().any(|tab| tab.id == host_tab));
            assert!(!app
                .sink
                .live
                .connection
                .borrow()
                .as_ref()
                .unwrap()
                .input_enabled());
        } else {
            app.close_tab(host_tab);
            assert!(!app.tabs.iter().any(|tab| tab.id == host_tab));
        }
        assert_eq!(app.hosts.retirements.len(), 1);
        assert!(app.hosts.ends.is_empty());
        app.reset_fixture();
        let replacement = app.active_tab;
        app.terminal.ingest(b"UNRELATED-REPLACEMENT-FRONTEND");
        detach_allowed.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !app.hosts.retirements.is_empty() {
                app.poll_hosts(&context);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(session.lifecycle(), SessionLifecycle::Stopped);
        assert_eq!(app.active_tab, replacement);
        assert!(app
            .terminal
            .text_snapshot()
            .text()
            .contains("UNRELATED-REPLACEMENT-FRONTEND"));
        assert!(!app
            .terminal
            .text_snapshot()
            .text()
            .contains("OWNED-MOBILE-OUTPUT"));
        assert!(app
            .notice
            .as_ref()
            .unwrap()
            .contains("correlated attachment detach confirmed"));
        assert!(
            app.hosts.ends.is_empty(),
            "retirement cannot mint End authority"
        );
        server.await.unwrap();
        return;
    }
    app.sink.live.content_lock_callback()();
    assert!(
        !app.sink
            .live
            .connection
            .borrow()
            .as_ref()
            .unwrap()
            .input_enabled(),
        "no extra frame is needed to gate input"
    );
    app.sink
        .live
        .connection
        .borrow_mut()
        .as_mut()
        .unwrap()
        .send(b"must-not-replay");
    app.sink
        .live
        .connection
        .borrow_mut()
        .as_mut()
        .unwrap()
        .poll(&mut app.terminal, &context);
    assert_eq!(app.reconnect_banner().custom.unwrap().action, "Review host");
    app.reconnect_active(&context).unwrap();
    assert!(app.hosts.open && app.hosts.pending.is_none() && app.hosts.worker.is_none());
    assert_eq!(
        app.active_tab,
        Some(host_tab),
        "review cannot replace the original tab with a new shell"
    );
    app.reset_fixture();
    assert_ne!(
        app.active_tab,
        Some(host_tab),
        "resetting the offline demo must not mutate the HOST frontend"
    );
    assert!(app
        .tabs
        .iter()
        .find(|tab| tab.id == host_tab)
        .unwrap()
        .parked
        .as_ref()
        .unwrap()
        .0
        .text_snapshot()
        .text()
        .contains("OWNED-MOBILE-OUTPUT"));
    drop(app);
    drop(session);
    server.await.unwrap();
}

#[test]
fn mobile_host_native_privacy_handle_is_shared_with_suspension_owner() {
    let suspension = SuspensionHandle::default();
    let token = suspension.host.token(0);
    suspension.suspend();
    assert!(token.is_cancelled());
}

fn creation_context() -> ApprovedContext {
    creation_context_for(HostId::new())
}

fn creation_context_for(host: HostId) -> ApprovedContext {
    creation_context_at(host, "fixture")
}

fn creation_context_at(host: HostId, endpoint: &str) -> ApprovedContext {
    use festerm_tunnel_bridge::owner_auth::PinnedHost;
    use russh::keys::{Algorithm, HashAlg, PrivateKey};
    let key = PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519).unwrap();
    let public = key.public_key().to_openssh().unwrap();
    ApprovedContext::from_protected_approval(OwnerBinding {
        installation: HostId::new().to_string(),
        host: PinnedHost { host: host.clone(), host_key_fingerprint: key.public_key().fingerprint(HashAlg::Sha256).to_string() },
        account: "github:7".into(), principal: festerm_host_protocol::PrincipalId::new(),
    }, festerm_secret_store::SecretReference::generate(),
    serde_json::from_value(serde_json::json!({"connectionMode":"TunnelRelay","hostId":"fixture",
        "hostPublicKeys":[public.split_whitespace().nth(1).unwrap()],"clientRelayUri":"wss://example.invalid/client"})).unwrap(),
    22, Zeroizing::new("not-a-service-token".into()), Binding::new(host, "github:7".into(), endpoint.into()).unwrap()).unwrap()
}

#[cfg(unix)]
fn recovery_fixture() -> (
    tempfile::TempDir,
    MobileApp,
    TunnelAccount,
    Arc<OriginalStore>,
) {
    recovery_fixture_with_login("mobile")
}

#[cfg(unix)]
fn recovery_fixture_with_login(
    login: &str,
) -> (
    tempfile::TempDir,
    MobileApp,
    TunnelAccount,
    Arc<OriginalStore>,
) {
    let parent = std::env::current_dir().unwrap().join("target");
    std::fs::create_dir_all(&parent).unwrap();
    let root = tempfile::Builder::new()
        .prefix("recovery-fence-")
        .tempdir_in(parent)
        .unwrap();
    let profiles = crate::profiles::testing::profiles();
    let profile_store = profiles.store_handle();
    let id = festerm_mobile_tunnels::testing::seed_account(&profile_store, 7, login, false);
    let account = profile_store
        .lock()
        .unwrap()
        .accounts()
        .iter()
        .find(|account| account.id == id)
        .unwrap()
        .clone();
    let context = creation_context_at(HostId::new(), "fixture.test");
    let (receipt, resolved, originals) =
        host_client::originals::testing::memory_fixture(root.path(), context).unwrap();
    let mut app = app().with_profiles(profiles);
    app.open_hosts();
    app.hosts.account = id;
    app.hosts.fingerprint = receipt.binding().host.host_key_fingerprint.clone();
    app.hosts.receipt = Some(receipt);
    app.hosts.resolved = Some(resolved);
    app.hosts.approval_account = Some(account.clone());
    app.hosts.trust = true;
    app.hosts.enrolled = true;
    app.hosts.selected = Some(
        PublicRecipe::persistent(
            "Recovery fence",
            HostReference {
                id: app.hosts.context().unwrap().binding().host().clone(),
                account: AccountReference::Github { account_id: 7 },
            },
            Recipe::default(),
        )
        .unwrap(),
    );
    app.hosts.inventory = Some(Inventory {
        capabilities: Capabilities {
            create: true,
            ensure: true,
            explicit_end: true,
            creation_allowed: true,
            ..Capabilities::default()
        },
        sessions: Vec::new(),
        unknown: Vec::new(),
    });
    assert!(app.host_foreground() && app.host_recipe_current());
    assert_eq!(app.host_account().as_ref(), Some(&account));
    app.hosts.context().unwrap();
    (root, app, account, originals)
}

#[cfg(unix)]
async fn checkpoint_end(
    app: &MobileApp,
    store: Arc<OriginalStore>,
    account: TunnelAccount,
) -> Arc<EndIntent> {
    use festerm_host_protocol::Refusal;
    use festerm_tunnel_client::host_control::{read_frame, write_frame};
    let context = app.hosts.context().unwrap();
    let intent = Arc::new(EndIntent::new(
        &context,
        host_client::Target {
            logical: LogicalSessionId::new(),
            generation: GenerationId::new(9).unwrap(),
        },
    ));
    intent
        .protect_original(
            store,
            OriginalReview {
                account,
                recipe: None,
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    let (stream, mut peer) = tokio::io::duplex(8192);
    let operation = intent.operation().clone();
    let server = tokio::spawn(async move {
        let budgets = Budgets::default();
        let hello: Envelope<Request> = read_frame(&mut peer, &budgets)
            .await
            .unwrap()
            .decode()
            .unwrap();
        write_frame(
            &mut peer,
            &budgets,
            &Envelope::new(
                hello.host,
                hello.operation,
                Response::Capabilities {
                    capabilities: Capabilities {
                        explicit_end: true,
                        ..Capabilities::default()
                    },
                },
            ),
        )
        .await
        .unwrap();
        let end: Envelope<Request> = read_frame(&mut peer, &budgets)
            .await
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!(end.operation, operation);
        assert!(matches!(end.body, Request::ExplicitEnd { .. }));
        write_frame(
            &mut peer,
            &budgets,
            &Envelope::new(
                end.host,
                end.operation,
                Response::Refused {
                    reason: Refusal::OutcomeUncertain,
                },
            ),
        )
        .await
        .unwrap();
    });
    let result = host_client::testing::end_from_stream(
        stream,
        ConnectionPermit::reserve(&Budgets::default()).unwrap(),
        context.binding().clone(),
        intent.clone(),
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(
        result,
        Err(host_client::Failure::Control(
            festerm_tunnel_client::host_control::Error::Refused(Refusal::OutcomeUncertain)
        ))
    ));
    server.await.unwrap();
    assert!(intent.is_issued() && !intent.is_resolved());
    intent
}

#[cfg(unix)]
async fn finish_local_host_job(app: &mut MobileApp, context: &egui::Context) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.hosts.pending.is_some() {
            app.poll_hosts(context);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(app.hosts.worker.is_none());
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn mobile_host_forget_is_default_no_and_only_ack_discards_selected_history() {
    let (_root, mut app, account, store) = recovery_fixture();
    let first = checkpoint_end(&app, store.clone(), account.clone()).await;
    let second = checkpoint_end(&app, store.clone(), account.clone()).await;
    let context = app.hosts.context().unwrap();
    app.hosts
        .retain_end(&context, account.clone(), first.clone())
        .unwrap();
    app.hosts
        .retain_end(&context, account, second.clone())
        .unwrap();
    app.hosts.original_store = Some(store.clone());
    let ui = egui::Context::default();
    app.host_action(&ui, Action::ReviewForgetEnd(0)).unwrap();
    finish_local_host_job(&mut app, &ui).await;
    assert!(!app.hosts.forget_review.as_ref().unwrap().confirmed);
    assert!(app.host_action(&ui, Action::ConfirmForget).is_err());
    assert!(app.hosts.pending.is_none());
    app.host_action(&ui, Action::CancelForget).unwrap();
    assert!(app.hosts.forget_review.is_none());
    assert_eq!(
        host_client::originals::testing::reopen(&store, &context)
            .unwrap()
            .1
            .len(),
        2
    );
    app.host_action(&ui, Action::ReviewForgetEnd(0)).unwrap();
    finish_local_host_job(&mut app, &ui).await;
    app.hosts.forget_review.as_mut().unwrap().confirmed = true;
    app.host_action(&ui, Action::ConfirmForget).unwrap();
    assert_eq!(app.hosts.ends.len(), 2);
    assert!(app.hosts.pending.is_some() && app.hosts.worker.is_none());
    finish_local_host_job(&mut app, &ui).await;
    assert_eq!(app.hosts.ends.len(), 1);
    assert_eq!(app.hosts.ends[0].intent.operation(), second.operation());
    assert!(!first.is_resolved() && !second.is_resolved());
    assert!(!app.hosts.launch && app.hosts.end_review.is_none());
    assert_eq!(
        host_client::originals::testing::reopen(&store, &context)
            .unwrap()
            .1
            .len(),
        1
    );
    assert!(app
        .hosts
        .error
        .as_ref()
        .unwrap()
        .contains("Remote session/action unchanged"));
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn mobile_host_forget_releases_only_confirmed_local_capacity_without_creation() {
    let (_root, mut app, account, store) = recovery_fixture();
    let context = app.hosts.context().unwrap();
    let recipe = || {
        PublicRecipe::persistent(
            "Retained",
            HostReference {
                id: context.binding().host().clone(),
                account: AccountReference::Github { account_id: 7 },
            },
            Recipe::default(),
        )
        .unwrap()
    };
    for _ in 0..16 {
        app.hosts
            .creation_intent(&context, recipe(), account.clone())
            .unwrap();
    }
    assert!(app
        .hosts
        .creation_intent(&context, recipe(), account.clone())
        .is_err());
    let surviving = app.hosts.originals[1].intent.creation().attachment.clone();
    app.hosts.original_store = Some(store.clone());
    let ui = egui::Context::default();
    app.host_action(&ui, Action::ReviewForgetCreation(0))
        .unwrap();
    finish_local_host_job(&mut app, &ui).await;
    assert_eq!(app.hosts.originals.len(), 16);
    app.hosts.forget_review.as_mut().unwrap().confirmed = true;
    app.host_action(&ui, Action::ConfirmForget).unwrap();
    assert_eq!(app.hosts.originals.len(), 16);
    finish_local_host_job(&mut app, &ui).await;
    assert_eq!(app.hosts.originals.len(), 15);
    assert_eq!(
        app.hosts.originals[0].intent.creation().attachment,
        surviving
    );
    assert!(app
        .hosts
        .error
        .as_ref()
        .unwrap()
        .contains("no native checkpoint remained"));
    assert!(!app.hosts.launch && app.hosts.worker.is_none());
    assert!(host_client::originals::testing::reopen(&store, &context)
        .unwrap()
        .1
        .is_empty());
    app.hosts
        .creation_intent(&context, recipe(), account)
        .unwrap();
    assert_eq!(app.hosts.originals.len(), 16);
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn mobile_host_forget_rejects_stale_confirmation_selection_lock_and_busy_work() {
    for changed in 0..9 {
        let (_root, mut app, account, store) = recovery_fixture();
        let intent = checkpoint_end(&app, store.clone(), account.clone()).await;
        let context = app.hosts.context().unwrap();
        app.hosts
            .retain_end(&context, account.clone(), intent.clone())
            .unwrap();
        app.hosts.original_store = Some(store.clone());
        let ui = egui::Context::default();
        let record = store
            .review_forget(
                &context,
                OriginalId::End(intent.operation().clone()),
                &CancellationToken::new(),
            )
            .unwrap();
        app.hosts
            .accept_forget_review(record, &account, app.hosts.generation)
            .unwrap();
        app.hosts.forget_review.as_mut().unwrap().confirmed = true;
        match changed {
            0 => app.hosts.forget_review.as_mut().unwrap().confirmed = false,
            1 => app
                .hosts
                .forget_review
                .as_mut()
                .unwrap()
                .account
                .login
                .push_str("-changed"),
            2 => app.hosts.forget_review.as_mut().unwrap().epoch += 1,
            3 => app.hosts.generation += 1,
            4 => app.hosts.ends.clear(),
            5 => app.hosts.receipt = None,
            6 => {
                app.hosts.pending = Some(Pending {
                    account: account.clone(),
                    recipe: app.hosts.selected.clone(),
                    epoch: app.hosts.epoch,
                    token: app.sink.live.host.token(app.hosts.epoch),
                    job: Job::spawn(&ui, |_| Err("held local work".into())),
                });
                assert!(app.host_action(&ui, Action::ReviewForgetEnd(0)).is_err());
            }
            7 => app.sink.live.content_lock_callback()(),
            _ => app.host_action(&ui, Action::CancelForget).unwrap(),
        }
        assert!(app.host_action(&ui, Action::ConfirmForget).is_err());
        assert!(app.hosts.worker.is_none());
        assert!(!intent.is_resolved());
        assert_eq!(
            host_client::originals::testing::reopen(&store, &context)
                .unwrap()
                .1
                .len(),
            1
        );
        app.hosts.cancel();
    }
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn mobile_host_forget_stale_ack_or_failure_preserves_memory_without_remote_inference() {
    let (_root, mut app, account, store) = recovery_fixture();
    let intent = checkpoint_end(&app, store.clone(), account.clone()).await;
    let context = app.hosts.context().unwrap();
    app.hosts
        .retain_end(&context, account.clone(), intent.clone())
        .unwrap();
    app.hosts.original_store = Some(store.clone());
    let generation = app.hosts.generation;
    let cancel = CancellationToken::new();
    let forgotten = store
        .review_forget(
            &context,
            OriginalId::End(intent.operation().clone()),
            &cancel,
        )
        .unwrap()
        .forget(&context, &cancel)
        .unwrap();
    app.hosts.generation += 1;
    assert!(app
        .hosts
        .accept_forgotten(forgotten, &account, generation)
        .is_err());
    assert_eq!(app.hosts.ends.len(), 1);
    assert!(!intent.is_resolved());
    let ui = egui::Context::default();
    app.hosts.pending = Some(Pending {
        account,
        recipe: app.hosts.selected.clone(),
        epoch: app.hosts.epoch,
        token: app.sink.live.host.token(app.hosts.epoch),
        job: Job::spawn(&ui, |_| Err("local storage completion unconfirmed".into())),
    });
    finish_local_host_job(&mut app, &ui).await;
    assert_eq!(app.hosts.ends.len(), 1);
    assert_eq!(
        app.hosts.error.as_deref(),
        Some("local storage completion unconfirmed")
    );
    assert!(app.hosts.worker.is_none() && !app.hosts.launch);
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn mobile_host_recovery_preserves_original_after_same_account_metadata_changes() {
    for changed in ["profile", "login", "credential", "all"] {
        let (_root, mut app, account, store) = recovery_fixture();
        let original = checkpoint_end(&app, store.clone(), account.clone()).await;
        let mut current = account.clone();
        if matches!(changed, "profile" | "all") {
            current.id = "recreated-local-profile".into();
        }
        if matches!(changed, "login" | "all") {
            current.login = "renamed-mobile".into();
        }
        if matches!(changed, "credential" | "all") {
            current.credential = festerm_mobile_profiles::SecretRef::from_reference(
                &festerm_secret_store::SecretReference::generate(),
            );
        }
        assert_ne!(current, account);
        let context = app.hosts.context().unwrap();
        let (reopened, batch) = host_client::originals::testing::reopen(&store, &context).unwrap();
        let protected_review = batch[0].review.clone();
        app.hosts.launch = true;
        app.hosts
            .accept_recovered(reopened, batch, &current)
            .unwrap();
        assert_eq!(app.hosts.ends.len(), 1);
        let restored = &app.hosts.ends[0];
        assert_eq!(restored.account, current);
        assert_eq!(restored.intent.operation(), original.operation());
        assert_eq!(restored.intent.target(), original.target());
        assert!(restored.intent.is_issued() && !restored.intent.is_resolved());
        assert!(restored.intent.matches_context(&context));
        assert!(app.hosts.original_store.is_some() && !app.hosts.launch);
        assert!(app.hosts.originals.is_empty());
        assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
        let (_, retained) = host_client::originals::testing::reopen(&store, &context).unwrap();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].review, protected_review);
        assert!(matches!(&retained[0].intent, RecoveredIntent::End(end)
            if end.operation() == original.operation() && end.target() == original.target()));
    }
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn mobile_host_recovery_fence_foreign_account_batch_is_atomic() {
    let (_root, mut app, account, store) = recovery_fixture();
    let original = checkpoint_end(&app, store.clone(), account.clone()).await;
    let mut foreign = account.clone();
    foreign.account_id = 8;
    let foreign_original = checkpoint_end(&app, store.clone(), foreign).await;
    let context = app.hosts.context().unwrap();
    let (reopened, mut batch) = host_client::originals::testing::reopen(&store, &context).unwrap();
    assert_eq!(batch.len(), 2);
    batch.sort_by_key(|item| OriginalReview::decode(&item.review).unwrap().account != account);
    app.hosts.launch = true;
    let error = app
        .hosts
        .accept_recovered(reopened, batch, &account)
        .unwrap_err();
    assert_eq!(
        error,
        "The original authenticated account differs; original retained, no replacement."
    );
    assert!(app.hosts.originals.is_empty() && app.hosts.ends.is_empty());
    assert!(app.hosts.original_store.is_none() && app.hosts.launch);
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
    let (_, retained) = host_client::originals::testing::reopen(&store, &context).unwrap();
    assert_eq!(retained.len(), 2);
    assert!(retained.iter().all(|item| matches!(&item.intent, RecoveredIntent::End(end)
        if end.operation() == original.operation() || end.operation() == foreign_original.operation())));
    let (reopened, retained) = host_client::originals::testing::reopen(&store, &context).unwrap();
    let valid: Vec<_> = retained
        .into_iter()
        .filter(|item| OriginalReview::decode(&item.review).unwrap().account == account)
        .collect();
    assert_eq!(valid.len(), 1);
    app.hosts
        .accept_recovered(reopened, valid, &account)
        .unwrap();
    assert_eq!(app.hosts.ends.len(), 1);
    assert_eq!(app.hosts.ends[0].intent.operation(), original.operation());
    assert!(app.hosts.original_store.is_some());
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn mobile_host_recovery_fence_late_result_after_account_or_context_switch_never_installs() {
    for changed_account in [false, true] {
        for empty in [false, true] {
            let (_old_root, mut old_app, old_account, old_store) = recovery_fixture();
            if !empty {
                checkpoint_end(&old_app, old_store.clone(), old_account.clone()).await;
            }
            let old_context = old_app.hosts.context().unwrap();
            let (valid_store, valid_batch) =
                host_client::originals::testing::reopen(&old_store, &old_context).unwrap();
            old_app
                .hosts
                .accept_recovered(valid_store, valid_batch, &old_account)
                .unwrap();
            assert!(old_app.hosts.original_store.is_some());
            let (store, batch) =
                host_client::originals::testing::reopen(&old_store, &old_context).unwrap();
            assert_eq!(batch.is_empty(), empty);
            let (_new_root, mut app, account, _) =
                recovery_fixture_with_login(if changed_account {
                    "new-mobile"
                } else {
                    "mobile"
                });
            let old_recipe = old_app.hosts.selected.clone();
            if changed_account {
                app.hosts.receipt = old_app.hosts.receipt.take();
                app.hosts.resolved = old_app.hosts.resolved.take();
                app.hosts.fingerprint = old_app.hosts.fingerprint.clone();
                app.hosts.selected = old_recipe.clone();
                assert_ne!(account, old_account);
                assert_eq!(
                    app.hosts.context().unwrap().binding(),
                    old_context.binding()
                );
            } else {
                app.profiles = old_app.profiles.take();
                app.hosts.account = old_account.id.clone();
                app.hosts.approval_account = Some(old_account.clone());
                assert_eq!(app.host_account().as_ref(), Some(&old_account));
                assert_ne!(
                    app.hosts.context().unwrap().binding(),
                    old_context.binding()
                );
            }
            assert!(app.host_foreground() && app.host_recipe_current());
            app.hosts.context().unwrap();
            let token = app.sink.live.host.token(app.hosts.epoch);
            let (ready, received) = std::sync::mpsc::channel();
            app.hosts.pending = Some(Pending {
                account: old_account,
                recipe: old_recipe,
                epoch: app.hosts.epoch,
                token: token.clone(),
                job: Job::spawn(&egui::Context::default(), move |_| {
                    ready.send(()).unwrap();
                    Ok(ResultEvent::Originals(store, batch))
                }),
            });
            received.recv_timeout(Duration::from_secs(5)).unwrap();
            app.poll_hosts(&egui::Context::default());
            assert_eq!(
                app.hosts.error.as_deref(),
                Some("The original account credential changed; no result was installed.")
            );
            assert!(token.is_cancelled());
            assert!(app.hosts.original_store.is_none());
            assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
            assert!(app.hosts.originals.is_empty() && app.hosts.ends.is_empty());
            assert!(!app.hosts.launch && app.hosts.end_review.is_none());
        }
    }
}

#[test]
#[cfg(unix)]
fn mobile_host_recovery_fence_create_and_end_before_explicit_reopen_issue_nothing() {
    let (_root, mut app, account, store) = recovery_fixture();
    let context = app.hosts.context().unwrap();
    assert!(host_client::originals::testing::reopen(&store, &context)
        .unwrap()
        .1
        .is_empty());
    assert!(app.hosts.original_store.is_none());
    app.hosts.launch = true;
    let ctx = egui::Context::default();
    assert_eq!(
        app.host_action(&ctx, Action::Create).unwrap_err(),
        "Explicitly reopen protected originals before any Create or End; no operation issued."
    );
    assert!(app.hosts.originals.is_empty());
    let intent = Arc::new(EndIntent::new(
        &context,
        host_client::Target {
            logical: LogicalSessionId::new(),
            generation: GenerationId::new(9).unwrap(),
        },
    ));
    let operation = intent.operation().clone();
    app.review_end(account.clone(), intent.clone());
    app.hosts.end_review.as_mut().unwrap().confirmed = true;
    assert!(app
        .hosts
        .end_review
        .as_ref()
        .unwrap()
        .authorize(&account, &context, app.hosts.epoch, app.hosts.generation)
        .is_ok());
    assert_eq!(
        app.host_action(&ctx, Action::ConfirmEnd).unwrap_err(),
        "Explicitly reopen protected originals before any Create or End; no operation issued."
    );
    assert_eq!(intent.operation(), &operation);
    assert!(!intent.is_issued() && !intent.is_resolved());
    assert!(app.hosts.originals.is_empty() && app.hosts.ends.is_empty());
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
    assert!(host_client::originals::testing::reopen(&store, &context)
        .unwrap()
        .1
        .is_empty());
}

#[test]
fn mobile_host_original_review_is_strict_typed_and_missing_or_ambiguous_selection_never_mints() {
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    let id = festerm_mobile_tunnels::testing::seed_account(&store, 7, "mobile", false);
    let account = store
        .lock()
        .unwrap()
        .accounts()
        .iter()
        .find(|value| value.id == id)
        .unwrap()
        .clone();
    let context = creation_context();
    let recipe = PublicRecipe::persistent(
        "Cold restart",
        HostReference {
            id: context.binding().host().clone(),
            account: AccountReference::Github { account_id: 7 },
        },
        Recipe::default(),
    )
    .unwrap();
    let encoded = OriginalReview {
        account: account.clone(),
        recipe: Some(recipe.clone()),
    }
    .encode()
    .unwrap();
    let decoded = OriginalReview::decode(&encoded).unwrap();
    assert_eq!(decoded.account, account);
    assert_eq!(decoded.recipe.as_ref(), Some(&recipe));
    for invalid in [
        "{}",
        "{\"account\":null,\"recipe\":null}",
        "{\"account\":{},\"recipe\":null}",
        "{",
    ] {
        assert!(OriginalReview::decode(invalid).is_err());
    }
    let mut extra: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    extra["authority"] = true.into();
    assert!(OriginalReview::decode(&extra.to_string()).is_err());
    let mut hosts = HostUi::default();
    let first = hosts
        .creation_intent(&context, recipe.clone(), account.clone())
        .unwrap();
    let mut another = first.creation().clone();
    another.attachment = AttachmentId::new();
    let second = Arc::new(CreationIntent::new(&context, CreationMode::Ensure, another).unwrap());
    hosts.originals.push(Original {
        recipe: recipe.clone(),
        account: account.clone(),
        intent: second.clone(),
    });
    assert!(hosts
        .creation_intent(&context, recipe.clone(), account.clone())
        .is_err());
    hosts.selected_original = Some(second.creation().attachment.to_string());
    assert!(Arc::ptr_eq(
        &hosts
            .creation_intent(&context, recipe.clone(), account.clone())
            .unwrap(),
        &second
    ));
    hosts.selected_original = Some(AttachmentId::new().to_string());
    assert!(hosts.creation_intent(&context, recipe, account).is_err());
    assert_eq!(hosts.originals.len(), 2);
    assert!(first.issued_operation().is_none() && second.issued_operation().is_none());
}

#[tokio::test]
async fn mobile_host_end_registry_does_not_exhaust_after_seventeen_exact_ended_replies() {
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    let id = festerm_mobile_tunnels::testing::seed_account(&store, 7, "mobile", false);
    let account = store
        .lock()
        .unwrap()
        .accounts()
        .iter()
        .find(|account| account.id == id)
        .unwrap()
        .clone();
    let context = creation_context();
    let mut hosts = HostUi::default();
    for generation in 1..=17 {
        let target = host_client::Target {
            logical: LogicalSessionId::new(),
            generation: GenerationId::new(generation).unwrap(),
        };
        let intent = hosts
            .end_intent(&context, target.clone(), &account)
            .unwrap();
        hosts
            .retain_end(&context, account.clone(), intent.clone())
            .unwrap();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let expected = intent.operation().clone();
        let server = tokio::spawn(async move {
            use festerm_tunnel_client::host_control::{read_frame, write_frame};
            let budgets = Budgets::default();
            let hello: Envelope<Request> = read_frame(&mut peer, &budgets)
                .await
                .unwrap()
                .decode()
                .unwrap();
            write_frame(
                &mut peer,
                &budgets,
                &Envelope::new(
                    hello.host,
                    hello.operation,
                    Response::Capabilities {
                        capabilities: Capabilities {
                            explicit_end: true,
                            ..Capabilities::default()
                        },
                    },
                ),
            )
            .await
            .unwrap();
            let end: Envelope<Request> = read_frame(&mut peer, &budgets)
                .await
                .unwrap()
                .decode()
                .unwrap();
            assert_eq!(end.operation, expected);
            assert!(
                matches!(&end.body, Request::ExplicitEnd { target: actual } if actual == &target)
            );
            assert!(end.issuance_epoch.is_none());
            write_frame(
                &mut peer,
                &budgets,
                &Envelope::new(end.host, end.operation, Response::Ended { target }),
            )
            .await
            .unwrap();
        });
        assert!(matches!(
            host_client::testing::end_from_stream(
                stream,
                ConnectionPermit::reserve(&Budgets::default()).unwrap(),
                context.binding().clone(),
                intent.clone(),
                &CancellationToken::new(),
            )
            .await
            .unwrap(),
            Outcome::Ended { .. }
        ));
        server.await.unwrap();
        assert!(intent.is_resolved());
        // A dropped UI completion must not turn confirmed originals into a
        // lifetime cap. The next explicit admission reconciles this receipt.
    }
}

#[test]
fn mobile_host_end_registry_never_replaces_original_after_account_or_protected_authority_change() {
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    let id = festerm_mobile_tunnels::testing::seed_account(&store, 7, "mobile", false);
    let account = store
        .lock()
        .unwrap()
        .accounts()
        .iter()
        .find(|account| account.id == id)
        .unwrap()
        .clone();
    let context = creation_context();
    let target = host_client::Target {
        logical: LogicalSessionId::new(),
        generation: GenerationId::new(9).unwrap(),
    };
    let mut hosts = HostUi::default();
    let original = hosts
        .end_intent(&context, target.clone(), &account)
        .unwrap();
    let operation = original.operation().clone();
    hosts.ends.push(OriginalEnd {
        host: context.binding().host().clone(),
        account: account.clone(),
        intent: original.clone(),
    });
    hosts.clear_approval();
    assert!(Arc::ptr_eq(
        &original,
        &hosts
            .end_intent(&context, target.clone(), &account)
            .unwrap()
    ));
    let mut changed_account = account.clone();
    changed_account.login.push_str("-changed");
    assert!(hosts
        .end_intent(&context, target.clone(), &changed_account)
        .is_err());
    let changed_authority = creation_context_for(context.binding().host().clone());
    assert_eq!(changed_authority.binding(), context.binding());
    assert!(
        hosts
            .end_intent(&changed_authority, target.clone(), &account)
            .is_err(),
        "same public host/endpoint is not the original protected principal/reference"
    );
    let mut replacement = target.clone();
    replacement.generation = GenerationId::new(10).unwrap();
    let new_review = hosts
        .end_intent(&context, replacement.clone(), &account)
        .unwrap();
    assert_eq!(new_review.target(), &replacement);
    assert_ne!(new_review.operation(), &operation);
    assert_eq!(original.operation(), &operation);
    assert_eq!(original.target(), &target);
    assert!(
        !original.is_issued() && !new_review.is_issued(),
        "review is not execution"
    );
}

#[test]
fn mobile_host_end_confirmation_fences_account_native_epoch_generation_and_protected_owner() {
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    let id = festerm_mobile_tunnels::testing::seed_account(&store, 7, "mobile", false);
    let account = store
        .lock()
        .unwrap()
        .accounts()
        .iter()
        .find(|account| account.id == id)
        .unwrap()
        .clone();
    let context = creation_context();
    let intent = Arc::new(EndIntent::new(
        &context,
        host_client::Target {
            logical: LogicalSessionId::new(),
            generation: GenerationId::new(9).unwrap(),
        },
    ));
    let mut review = EndReview {
        account: account.clone(),
        intent: intent.clone(),
        epoch: 3,
        generation: 7,
        confirmed: false,
        source: None,
    };
    assert!(review.authorize(&account, &context, 3, 7).is_err());
    review.confirmed = true;
    assert!(Arc::ptr_eq(
        &review.authorize(&account, &context, 3, 7).unwrap(),
        &intent
    ));
    let mut changed = account.clone();
    changed.account_id += 1;
    assert!(review.authorize(&changed, &context, 3, 7).is_err());
    assert!(review.authorize(&account, &context, 4, 7).is_err());
    assert!(review.authorize(&account, &context, 3, 8).is_err());
    assert!(review
        .authorize(&account, &creation_context(), 3, 7)
        .is_err());
    assert!(!intent.is_issued() && !intent.is_resolved());
}

#[test]
fn mobile_host_end_cancel_close_lock_and_metadata_never_issue_and_keep_original() {
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    let id = festerm_mobile_tunnels::testing::seed_account(&store, 7, "mobile", false);
    let account = store
        .lock()
        .unwrap()
        .accounts()
        .iter()
        .find(|account| account.id == id)
        .unwrap()
        .clone();
    let context = creation_context();
    let intent = Arc::new(EndIntent::new(
        &context,
        host_client::Target {
            logical: LogicalSessionId::new(),
            generation: GenerationId::new(9).unwrap(),
        },
    ));
    let operation = intent.operation().clone();
    let mut app = app().with_profiles(profiles);
    app.open_hosts();
    app.hosts.account = id;
    app.hosts.ends.push(OriginalEnd {
        host: context.binding().host().clone(),
        account: account.clone(),
        intent: intent.clone(),
    });
    app.review_end(account.clone(), intent.clone());
    app.hosts.end_review.as_mut().unwrap().confirmed = true;
    app.host_action(&egui::Context::default(), Action::CancelEnd)
        .unwrap();
    assert!(app.hosts.end_review.is_none());
    app.review_end(account, intent.clone());
    app.host_action(&egui::Context::default(), Action::Close)
        .unwrap();
    app.open_hosts();
    assert!(app.hosts.end_review.is_none());
    app.hosts.trust = true;
    app.hosts.enrolled = true;
    assert!(app
        .host_action(&egui::Context::default(), Action::ConfirmEnd)
        .is_err());
    assert!(
        app.host_action(&egui::Context::default(), Action::ReviewOriginalEnd(0))
            .is_err(),
        "public flags/account cannot replace the original protected receipt"
    );
    app.sink.live.content_lock_callback()();
    assert!(app
        .host_action(&egui::Context::default(), Action::ConfirmEnd)
        .is_err());
    assert!(app.hosts.worker.is_none());
    assert_eq!(app.hosts.ends.len(), 1);
    assert_eq!(app.hosts.ends[0].intent.operation(), &operation);
    assert!(!intent.is_issued() && !intent.is_resolved());
}

#[test]
fn mobile_host_session_choice_never_retargets_attach_only_resume_or_always_create() {
    let mut recipe = PublicRecipe::persistent(
        "Mobile choice",
        HostReference {
            id: HostId::new(),
            account: AccountReference::Github { account_id: 7 },
        },
        Recipe::default(),
    )
    .unwrap();
    let target = host_client::Target {
        logical: recipe.logical_session.clone(),
        generation: GenerationId::new(8).unwrap(),
    };
    assert!(selected_target(Some(&recipe), &target).is_ok());
    let mut foreign = target.clone();
    foreign.logical = LogicalSessionId::new();
    assert!(selected_target(Some(&recipe), &foreign).is_err());
    recipe.behavior = ConnectionBehavior::AlwaysCreate;
    assert!(selected_target(Some(&recipe), &target).is_err());
    recipe.behavior = ConnectionBehavior::ChooseSession;
    assert!(selected_target(Some(&recipe), &foreign).is_ok());
    recipe.behavior = ConnectionBehavior::AttachOnly {
        generation: target.generation,
    };
    assert!(selected_target(Some(&recipe), &target).is_ok());
    let mut replacement = target.clone();
    replacement.generation = GenerationId::new(9).unwrap();
    assert!(selected_target(Some(&recipe), &replacement).is_err());
    assert!(selected_target(Some(&recipe), &foreign).is_err());
}

#[test]
fn mobile_host_original_registry_survives_close_refuses_changed_binding_and_is_process_local() {
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    let id = festerm_mobile_tunnels::testing::seed_account(&store, 7, "mobile", false);
    let account = store
        .lock()
        .unwrap()
        .accounts()
        .iter()
        .find(|account| account.id == id)
        .unwrap()
        .clone();
    let context = creation_context();
    let mut recipe = PublicRecipe::persistent(
        "Mobile ensure",
        HostReference {
            id: context.binding().host().clone(),
            account: AccountReference::Github { account_id: 7 },
        },
        Recipe::default(),
    )
    .unwrap();
    recipe.behavior = ConnectionBehavior::ResumeOrCreate;
    let mut hosts = HostUi::default();
    let original = hosts
        .creation_intent(&context, recipe.clone(), account.clone())
        .unwrap();
    assert_eq!(original.creation().logical, recipe.logical_session);
    hosts.clear_approval();
    let retry = hosts
        .creation_intent(&context, recipe.clone(), account.clone())
        .unwrap();
    assert!(Arc::ptr_eq(&original, &retry));
    assert!(
        original.issued_operation().is_none(),
        "local intent does not issue or authorize anything"
    );
    let mut changed_recipe = recipe.clone();
    changed_recipe.launch.args.push("changed".into());
    assert!(hosts
        .creation_intent(&context, changed_recipe, account.clone())
        .is_err());
    let mut changed_account = account.clone();
    changed_account.credential = festerm_mobile_profiles::SecretRef::from_reference(
        &festerm_secret_store::SecretReference::generate(),
    );
    assert!(hosts
        .creation_intent(&context, recipe.clone(), changed_account)
        .is_err());
    for index in 1..16 {
        let mut next = recipe.clone();
        next.id = festerm_mobile_profiles::host_recipes::RecipeId::new();
        next.name = format!("mobile-{index}");
        hosts
            .creation_intent(&context, next, account.clone())
            .unwrap();
    }
    let mut overflow = recipe.clone();
    overflow.id = festerm_mobile_profiles::host_recipes::RecipeId::new();
    assert!(hosts
        .creation_intent(&context, overflow, account.clone())
        .is_err());
    assert!(Arc::ptr_eq(
        &original,
        &hosts
            .creation_intent(&context, recipe.clone(), account.clone())
            .unwrap()
    ));
    assert_eq!(hosts.originals.len(), 16);
    assert!(
        HostUi::default().originals.is_empty(),
        "restart does not invent retry authority"
    );
    let mut app = app().with_profiles(profiles);
    app.hosts = hosts;
    app.hosts.trust = true;
    app.hosts.enrolled = true;
    app.hosts.launch = true;
    let ui = egui::Context::default();
    app.host_action(&ui, Action::ReviewOriginal(0)).unwrap();
    assert_eq!(app.hosts.selected.as_ref(), Some(&recipe));
    assert_eq!(app.hosts.account, account.id);
    assert!(!app.hosts.trust && !app.hosts.enrolled && !app.hosts.launch);
    assert!(
        app.hosts.receipt.is_none() && app.hosts.pending.is_none() && app.hosts.worker.is_none()
    );
    assert!(Arc::ptr_eq(
        &original,
        &app.hosts
            .creation_intent(&context, recipe, account)
            .unwrap()
    ));
    assert!(
        original.issued_operation().is_none(),
        "review is local metadata, not operation issuance"
    );
    assert!(app.host_action(&ui, Action::ReviewOriginal(16)).is_err());
}

#[test]
fn mobile_host_changed_account_or_recipe_discards_completed_work_without_a_replacement() {
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    let id = festerm_mobile_tunnels::testing::seed_account(&store, 7, "mobile", false);
    let account = store
        .lock()
        .unwrap()
        .accounts()
        .iter()
        .find(|account| account.id == id)
        .unwrap()
        .clone();
    let mut app = app().with_profiles(profiles);
    app.open_hosts();
    app.hosts.account = id;
    let context = egui::Context::default();
    let mut stale_account = account;
    stale_account.login = "changed-original".into();
    let token = app.sink.live.host.token(app.hosts.epoch);
    app.hosts.pending = Some(Pending {
        account: stale_account.clone(),
        recipe: None,
        epoch: app.hosts.epoch,
        token: token.clone(),
        job: Job::spawn(&context, |_| Ok(ResultEvent::Hosts(Vec::new()))),
    });
    app.poll_hosts(&context);
    assert!(token.is_cancelled());
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
    assert!(app
        .hosts
        .error
        .as_ref()
        .unwrap()
        .contains("credential changed"));
    app.hosts.approval_account = Some(stale_account);
    app.poll_hosts(&context);
    assert!(
        app.hosts.approval_account.is_none(),
        "idle approval also pins the complete account credential"
    );
    let recipe = PublicRecipe::persistent(
        "Mobile",
        HostReference {
            id: HostId::new(),
            account: AccountReference::Github { account_id: 7 },
        },
        Recipe::default(),
    )
    .unwrap();
    app.hosts.selected = Some(recipe.clone());
    app.host_action(&context, Action::Save).unwrap();
    let mut changed = recipe;
    changed.name = "Changed mobile".into();
    store
        .lock()
        .unwrap()
        .apply_host_recipe_command(MetadataCommand::Edit(changed))
        .unwrap();
    assert!(!app.host_recipe_current());
    app.poll_hosts(&context);
    assert!(app.hosts.receipt.is_none() && app.hosts.inventory.is_none());
    assert!(app.hosts.originals.is_empty());
}
