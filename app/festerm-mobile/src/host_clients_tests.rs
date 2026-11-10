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
                target,
                attachment,
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
        let mut byte = [0];
        assert_eq!(
            peer.read(&mut byte).await.unwrap(),
            0,
            "native lock abandons transport; it must not emit input, replay or intentional detach"
        );
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
    use festerm_tunnel_bridge::owner_auth::PinnedHost;
    use russh::keys::{Algorithm, HashAlg, PrivateKey};
    let key = PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519).unwrap();
    let public = key.public_key().to_openssh().unwrap();
    let host = HostId::new();
    ApprovedContext::from_protected_approval(OwnerBinding {
        installation: HostId::new().to_string(),
        host: PinnedHost { host: host.clone(), host_key_fingerprint: key.public_key().fingerprint(HashAlg::Sha256).to_string() },
        account: "github:7".into(), principal: festerm_host_protocol::PrincipalId::new(),
    }, festerm_secret_store::SecretReference::generate(),
    serde_json::from_value(serde_json::json!({"connectionMode":"TunnelRelay","hostId":"fixture",
        "hostPublicKeys":[public.split_whitespace().nth(1).unwrap()],"clientRelayUri":"wss://example.invalid/client"})).unwrap(),
    22, Zeroizing::new("not-a-service-token".into()), Binding::new(host, "github:7".into(), "fixture".into()).unwrap()).unwrap()
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
