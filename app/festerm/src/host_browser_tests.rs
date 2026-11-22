use super::*;
use festerm_config::{
    host_recipes::{HostReference, MetadataCommand, Recipe},
    AccountProvider, Configuration,
};

#[test]
fn renewal_storage_failure_does_not_offer_new_account_authorization() {
    use festerm_tunnel_client::Error;
    assert!(!RequestFailure::renewal(Error::Store).needs_sign_in);
    assert!(!RequestFailure::renewal(Error::Busy).needs_sign_in);
    assert!(!RequestFailure::renewal(Error::Cancelled).needs_sign_in);
    assert!(RequestFailure::renewal(Error::Expired).needs_sign_in);
    assert!(RequestFailure::renewal(Error::Network).needs_sign_in);
}

#[test]
fn host_quick_create_requires_a_bounded_nonblank_name() {
    for invalid in ["", "  ", "bad\nname", "bad\u{0}name"] {
        assert!(!valid_session_name(invalid));
    }
    assert!(!valid_session_name(&"x".repeat(129)));
    assert!(valid_session_name("  release  "));
}

#[test]
fn host_creation_uses_remote_session_name_not_local_profile_name() {
    let (mut app, _) = fixture();
    let browser = app.host_browser.as_mut().unwrap();
    browser.recipe.name = "My shortcut".into();
    browser.recipe.session_name = Some("remote-build".into());
    browser.host_approved = true;
    browser.creation_confirmed = true;
    browser.inventory = Some(Inventory {
        capabilities: Default::default(),
        sessions: Vec::new(),
        profiles: Vec::new(),
        unknown: Vec::new(),
    });
    let capabilities = &mut browser.inventory.as_mut().unwrap().capabilities;
    capabilities.attach = true;
    capabilities.ensure = true;
    capabilities.creation_allowed = true;
    assert_eq!(
        browser.creation_request().unwrap().1.name.as_deref(),
        Some("remote-build")
    );
}

#[test]
fn selected_host_desktop_fingerprint_does_not_enable_host_owner_enrollment() {
    let (_, features) = include_str!("../Cargo.toml")
        .split_once("\nexperimental-dev-tunnels = [")
        .unwrap();
    let (selected, _) = features.split_once(']').unwrap();
    assert!(!selected.contains("festerm-host-setup/owner-enrollment"));
}

fn fixture() -> (FesTermApp, egui::Context) {
    let account = DevTunnelAccount::new(
        "github-user",
        AccountProvider::Github,
        42,
        "user",
        SecretReference::generate(),
    )
    .unwrap();
    let recipe = PublicRecipe::persistent(
        "Selected host",
        HostReference {
            id: uuid::Uuid::new_v4().to_string().try_into().unwrap(),
            account: AccountReference::Github { account_id: 42 },
        },
        Recipe {
            shell: None,
            args: Vec::new(),
            cwd: None,
        },
    )
    .unwrap();
    let (configuration, _) = Configuration::empty().with_account(account).unwrap();
    let (configuration, _) = configuration
        .with_host_recipe_command(MetadataCommand::Save(recipe.clone()))
        .unwrap();
    let context = egui::Context::default();
    let mut app = FesTermApp::for_test_with_configuration(configuration);
    app.state.dispatch(AppCommand::OpenProfiles, &context);
    let tab = app.state.active();
    let request = egui::Id::new("foreground-host-review");
    crate::screens::install_host_recipe_request_for_test(&context, tab, request);
    app.review_host(tab, request, recipe, &context);
    app.host_browser.as_mut().unwrap().ordinary_selected = false;
    app.host_browser.as_mut().unwrap().ordinary_lifetime = None;
    app.host_browser.as_mut().unwrap().feedback = None;
    (app, context)
}

fn assert_unallocated(app: &FesTermApp) {
    if let Some(browser) = &app.host_browser {
        assert!(browser.receiver.is_none());
        assert!(browser.worker.is_none());
        assert!(browser.receipt.is_none());
        assert!(browser.resolved.is_none());
        assert!(browser.inventory.is_none());
    }
    let permits: Vec<_> = (0..4)
        .map(|_| ConnectionPermit::reserve(&app.state.host_client_budgets).unwrap())
        .collect();
    assert!(ConnectionPermit::reserve(&app.state.host_client_budgets).is_err());
    drop(permits);
}

#[test]
fn selected_host_desktop_foreground_review_does_not_read_vault_connect_or_generate_key() {
    let (mut app, context) = fixture();
    let browser = app.host_browser.as_ref().unwrap();
    assert!(browser.current(&app.state, &context));
    assert!(!browser.consent);
    assert!(!browser.host_approved);
    assert_unallocated(&app);
    app.apply_host_review_action(Action::Prepare, &context);
    app.apply_host_review_action(Action::Inventory, &context);
    assert_unallocated(&app);
    let cancel = app.host_browser.as_ref().unwrap().cancel.clone();
    drop(app.host_browser.take());
    assert!(cancel.is_cancelled());
}

#[test]
fn selected_host_desktop_imported_recipe_cannot_become_protected_owner_approval() {
    let (mut app, context) = fixture();
    let browser = app.host_browser.as_mut().unwrap();
    browser.consent = true;
    browser.prerequisite = zeroize::Zeroizing::new(serde_json::to_string(&browser.recipe).unwrap());
    browser.fingerprint = format!("SHA256:{}", "a".repeat(43));
    app.apply_host_review_action(Action::Prepare, &context);
    assert_unallocated(&app);
    assert!(app
        .host_browser
        .as_ref()
        .unwrap()
        .feedback
        .as_ref()
        .unwrap()
        .contains("does not match"));
}

#[test]
fn selected_host_desktop_foreground_review_cancels_on_recipe_editor_or_credential_change() {
    for changed in 0..5 {
        let (mut app, context) = fixture();
        let browser = app.host_browser.as_ref().unwrap();
        let cancel = browser.cancel.clone();
        let tab = browser.tab;
        match changed {
            0 => {
                let account = browser
                    .account
                    .clone()
                    .with_credential(SecretReference::generate());
                let (configuration, _) = app.state.configuration().with_account(account).unwrap();
                app.state.replace_configuration(configuration);
            }
            1 => {
                let mut recipe = browser.recipe.clone();
                recipe.name = "Changed recipe".into();
                let (configuration, _) = app
                    .state
                    .configuration()
                    .with_host_recipe_command(MetadataCommand::Edit(recipe))
                    .unwrap();
                app.state.replace_configuration(configuration);
            }
            4 => {
                let mut recipe = browser.recipe.clone();
                recipe.host.id = festerm_host_protocol::HostId::new();
                recipe.logical_session = host_client::LogicalSessionId::new();
                let (configuration, _) = app
                    .state
                    .configuration()
                    .with_host_recipe_command(MetadataCommand::Edit(recipe))
                    .unwrap();
                app.state.replace_configuration(configuration);
            }
            2 => crate::screens::install_host_recipe_request_for_test(
                &context,
                tab,
                egui::Id::new("replacement-editor"),
            ),
            _ => app.state.active_tab_mut().content = TabContent::Launcher,
        }
        app.poll_host_browser(&context);
        assert!(app.host_browser.is_none());
        assert!(cancel.is_cancelled());
        assert_unallocated(&app);
    }
}

#[test]
fn selected_host_desktop_foreground_timeout_fences_late_worker_without_parent_lockout() {
    let (mut app, context) = fixture();
    let browser = app.host_browser.as_mut().unwrap();
    let cancel = browser.cancel.child_token();
    let (sender, receiver) = mpsc::sync_channel(1);
    browser.receiver = Some(receiver);
    browser.pending_deadline = Some((Instant::now() - Duration::from_secs(1), cancel.clone()));
    app.poll_host_browser(&context);
    let browser = app.host_browser.as_ref().unwrap();
    assert!(cancel.is_cancelled());
    assert!(!browser.cancel.is_cancelled());
    assert!(!browser.busy());
    assert!(sender.try_send(Err("late completion".into())).is_err());
    assert!(browser.feedback.as_ref().unwrap().contains("timed out"));
    assert_unallocated(&app);
}

#[test]
fn native_owner_prompt_budget_keeps_admission_until_worker_drains() {
    for action in [Action::Prepare, Action::Load, Action::Inventory] {
        for complete_before_deadline in [true, false] {
            let (mut app, _) = fixture();
            let started = Instant::now();
            let browser = app.host_browser.as_mut().unwrap();
            let cancel = browser.cancel.child_token();
            let (_ui_sender, receiver) = mpsc::sync_channel(1);
            browser.receiver = Some(receiver);
            browser.pending_deadline =
                Some((started + owner_worker_budget(&action), cancel.clone()));
            let permit = ConnectionPermit::reserve(&app.state.host_client_budgets).unwrap();
            let occupied: Vec<_> = (0..3)
                .map(|_| ConnectionPermit::reserve(&app.state.host_client_budgets).unwrap())
                .collect();
            let (release, blocked) = mpsc::sync_channel(1);
            let (sender, grants) = mpsc::sync_channel(1);
            let worker_cancel = cancel.clone();
            let worker = std::thread::spawn(move || {
                let _permit = permit;
                // Models a synchronous native prompt which cannot be interrupted.
                blocked.recv_timeout(Duration::from_secs(5)).unwrap();
                deliver_owner_completion(&sender, &worker_cancel, "prepared grant")
            });
            let browser = app.host_browser.as_mut().unwrap();
            browser.expire_pending(started + Duration::from_secs(46));
            assert!(browser.busy());
            assert!(!cancel.is_cancelled());
            browser
                .expire_pending(started + owner_worker_budget(&action) - Duration::from_nanos(1));
            assert!(browser.busy());
            if !complete_before_deadline {
                browser.expire_pending(started + owner_worker_budget(&action));
                assert!(cancel.is_cancelled());
                assert!(!browser.cancel.is_cancelled());
                assert!(!browser.busy());
            }
            // UI timeout never frees a permit still held by a native call.
            assert!(ConnectionPermit::reserve(&app.state.host_client_budgets).is_err());
            release.send(()).unwrap();
            assert_eq!(worker.join().unwrap(), complete_before_deadline);
            if complete_before_deadline {
                assert_eq!(grants.recv().unwrap(), "prepared grant");
            } else {
                assert!(grants.try_recv().is_err());
            }
            assert!(ConnectionPermit::reserve(&app.state.host_client_budgets).is_ok());
            drop(occupied);
            let browser = app.host_browser.as_mut().unwrap();
            browser.receiver = None;
            browser.pending_deadline = None;
            assert_unallocated(&app);
        }
    }
}

#[test]
fn selected_host_desktop_foreground_honors_saved_attach_generation_and_refuses_creation() {
    let (mut app, _) = fixture();
    let browser = app.host_browser.as_mut().unwrap();
    let target = Target {
        logical: browser.recipe.logical_session.clone(),
        generation: "7".to_owned().try_into().unwrap(),
    };
    let mut foreign = target.clone();
    foreign.logical = uuid::Uuid::new_v4().to_string().try_into().unwrap();
    browser.recipe.behavior = ConnectionBehavior::ChooseSession;
    assert!(browser.allows_target(&foreign));
    browser.recipe.behavior = ConnectionBehavior::ResumeOrCreate;
    assert!(browser.allows_target(&target));
    assert!(!browser.allows_target(&foreign));
    browser.recipe.behavior = ConnectionBehavior::AttachOnly {
        generation: target.generation,
    };
    assert!(browser.allows_target(&target));
    let mut stale = target.clone();
    stale.generation = "8".to_owned().try_into().unwrap();
    assert!(!browser.allows_target(&stale));
    assert!(!browser.allows_target(&foreign));
    browser.recipe.behavior = ConnectionBehavior::AlwaysCreate;
    assert!(!browser.allows_target(&target));
    assert_unallocated(&app);
}

#[test]
fn selected_host_desktop_creation_requires_separate_consent_and_advertised_creation_permission() {
    let (mut app, context) = fixture();
    app.apply_host_review_action(Action::Create, &context);
    assert_unallocated(&app);
    assert!(app.state.host_creation_intents.is_empty());
    let browser = app.host_browser.as_mut().unwrap();
    browser.inventory = Some(Inventory {
        capabilities: Default::default(),
        sessions: Vec::new(),
        profiles: Vec::new(),
        unknown: Vec::new(),
    });
    assert!(
        browser.creation_request().is_none(),
        "empty inventory is not creation authority"
    );
    let caps = &mut browser.inventory.as_mut().unwrap().capabilities;
    caps.create = true;
    caps.ensure = true;
    caps.attach = true;
    caps.creation_allowed = true;
    browser.host_approved = true;
    assert!(
        browser.creation_request().is_none(),
        "host trust is not shell-launch consent"
    );
    browser.creation_confirmed = true;
    assert!(browser.creation_request().is_some());
    browser
        .inventory
        .as_mut()
        .unwrap()
        .capabilities
        .creation_allowed = false;
    assert!(
        browser.creation_request().is_some(),
        "Ensure delegates authoritative resume-only policy to the host"
    );
    browser.recipe.behavior = ConnectionBehavior::AlwaysCreate;
    assert!(browser.creation_request().is_none());
    assert!(app.state.host_creation_intents.is_empty());
}

#[test]
fn selected_host_desktop_creation_freezes_launch_and_distinguishes_new_logical_from_authoritative_ensure(
) {
    let (mut app, _) = fixture();
    let browser = app.host_browser.as_mut().unwrap();
    browser.host_approved = true;
    browser.creation_confirmed = true;
    browser.inventory = Some(Inventory {
        capabilities: Default::default(),
        sessions: Vec::new(),
        profiles: Vec::new(),
        unknown: Vec::new(),
    });
    let caps = &mut browser.inventory.as_mut().unwrap().capabilities;
    caps.create = true;
    caps.ensure = true;
    caps.attach = true;
    caps.creation_allowed = true;
    browser.recipe.launch = Recipe {
        shell: Some("/owned/test-shell".into()),
        args: vec!["literal argv; not shell expansion".into()],
        cwd: Some("/owned/test-directory".into()),
    };
    let (mode, ensured) = browser.creation_request().unwrap();
    assert_eq!(mode, CreationMode::Ensure);
    assert_eq!(ensured.logical, browser.recipe.logical_session);
    assert_eq!(ensured.recipe, browser.recipe.launch);
    assert_eq!(ensured.lifetime, browser.recipe.lifetime);
    browser.recipe.behavior = ConnectionBehavior::AlwaysCreate;
    let (mode, created) = browser.creation_request().unwrap();
    assert_eq!(mode, CreationMode::Create);
    assert_ne!(created.logical, browser.recipe.logical_session);
    assert_eq!(created.recipe, ensured.recipe);
    browser.recipe.lifetime = Lifetime::Temporary {
        reconnect_grace_seconds: 60,
    };
    assert!(
        browser.creation_request().is_none(),
        "temporary permission is independent"
    );
    browser.inventory.as_mut().unwrap().capabilities.temporary = true;
    assert!(browser.creation_request().is_some());
    browser.recipe.behavior = ConnectionBehavior::AttachOnly {
        generation: "7".to_owned().try_into().unwrap(),
    };
    assert!(browser.creation_request().is_none());
    assert!(app.state.host_creation_intents.is_empty());
}

fn protected_creation_context(recipe: &PublicRecipe) -> ClientContext {
    use festerm_tunnel_bridge::owner_auth::{OwnerBinding, PinnedHost};
    use russh::keys::{Algorithm, HashAlg, PrivateKey};
    let key = PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519).unwrap();
    let public = key.public_key().to_openssh().unwrap();
    let AccountReference::Github { account_id } = recipe.host.account;
    let owner = OwnerBinding {
        installation: uuid::Uuid::new_v4().to_string(),
        host: PinnedHost {
            host: recipe.host.id.clone(),
            host_key_fingerprint: key.public_key().fingerprint(HashAlg::Sha256).to_string(),
        },
        account: format!("github:{account_id}"),
        principal: uuid::Uuid::new_v4().to_string().try_into().unwrap(),
    };
    let binding = Binding::new(
        owner.host.host.clone(),
        owner.account.clone(),
        "owned-fixture".into(),
    )
    .unwrap();
    ApprovedContext::from_protected_approval(
        owner,
        SecretReference::generate(),
        serde_json::from_value(serde_json::json!({
            "connectionMode": "TunnelRelay", "hostId": "owned-fixture",
            "hostPublicKeys": [public.split_whitespace().nth(1).unwrap()],
            "clientRelayUri": "wss://example.invalid/client"
        }))
        .unwrap(),
        22,
        zeroize::Zeroizing::new("not-a-service-token".into()),
        binding,
    )
    .unwrap()
    .into()
}

fn registry_creation(recipe: &PublicRecipe) -> Option<(CreationMode, host_client::Creation)> {
    Some((
        CreationMode::Ensure,
        host_client::Creation {
            logical: recipe.logical_session.clone(),
            attachment: host_client::AttachmentId::new(),
            name: Some(recipe.name.clone()),
            recipe: recipe.launch.clone(),
            lifetime: recipe.lifetime,
        },
    ))
}

#[test]
fn selected_host_desktop_creation_registry_retains_action_across_close_and_refuses_changed_recipe_or_credential(
) {
    let (mut app, _) = fixture();
    let browser = app.host_browser.as_ref().unwrap();
    let recipe = browser.recipe.clone();
    let reference = browser.account_reference.duplicate_for_transport();
    let approved = protected_creation_context(&recipe);
    let first = app
        .state
        .host_creation_intent(&recipe, &reference, &approved, || {
            registry_creation(&recipe)
        })
        .unwrap();
    app.host_browser = None;
    let retry = app
        .state
        .host_creation_intent(&recipe, &reference, &approved, || {
            panic!("retry cannot allocate another action")
        })
        .unwrap();
    assert!(Arc::ptr_eq(&first, &retry));
    let mut changed = recipe.clone();
    changed.launch.args.push("changed launch".into());
    assert!(app
        .state
        .host_creation_intent(&changed, &reference, &approved, || panic!(
            "changed recipe cannot replace an unresolved action"
        ))
        .is_err());
    assert!(app
        .state
        .host_creation_intent(&recipe, &SecretReference::generate(), &approved, || panic!(
            "credential rotation cannot replace an unresolved action"
        ))
        .is_err());
    assert_eq!(app.state.host_creation_intents.len(), 1);
    assert!(!first.is_presented());
    assert!(first.issued_operation().is_none());
}

#[test]
fn selected_host_desktop_creation_registry_is_bounded_without_discarding_original_retry() {
    let (mut app, _) = fixture();
    let browser = app.host_browser.as_ref().unwrap();
    let recipe = browser.recipe.clone();
    let reference = browser.account_reference.duplicate_for_transport();
    let approved = protected_creation_context(&recipe);
    let mut first = None;
    for _ in 0..16 {
        let mut recipe = recipe.clone();
        recipe.id = Default::default();
        let intent = app
            .state
            .host_creation_intent(&recipe, &reference, &approved, || {
                registry_creation(&recipe)
            })
            .unwrap();
        if first.is_none() {
            first = Some((recipe, intent));
        }
    }
    assert_eq!(app.state.host_creation_intents.len(), 16);
    assert!(app
        .state
        .host_creation_intent(&recipe, &reference, &approved, || panic!(
            "full registry must refuse before allocating or contacting a host"
        ))
        .is_err());
    let (recipe, original) = first.unwrap();
    let retry = app
        .state
        .host_creation_intent(&recipe, &reference, &approved, || {
            panic!("capacity cannot prevent retrying the original action")
        })
        .unwrap();
    assert!(Arc::ptr_eq(&original, &retry));
    assert_eq!(app.state.host_creation_intents.len(), 16);
}

#[test]
fn background_host_discovery_always_leaves_one_connection_permit_free() {
    let budgets = host_client::Budgets::default();
    let running: Vec<_> = (0..MAX_PARALLEL_DISCOVERIES)
        .map(|_| reserve_with_headroom(&budgets).expect("headroom"))
        .collect();
    assert!(reserve_with_headroom(&budgets).is_none());
    let interactive =
        ConnectionPermit::reserve(&budgets).expect("interactive connect keeps a slot");
    drop(interactive);
    drop(running);
    assert!(reserve_with_headroom(&budgets).is_some());
}

#[test]
fn discovered_host_reuses_only_unambiguous_same_account_exact_locator_identity() {
    use crate::screens::host_workspace::{remember_host, HostSessions};
    let context = egui::Context::default();
    let configuration = Configuration::empty();
    let reference = HostReference {
        id: festerm_host_protocol::HostId::new(),
        account: AccountReference::Github { account_id: 42 },
    };
    let remembered = HostSessions {
        reference: reference.clone(),
        hint: HostHint {
            name: "Build host".into(),
            locator: "build.use".into(),
        },
        sessions: Vec::new(),
    };
    remember_host(&context, remembered.clone());
    assert_eq!(
        known_host_reference(&context, &configuration, 42, "build.use"),
        Some(reference.clone())
    );
    assert!(known_host_reference(&context, &configuration, 43, "build.use").is_none());
    assert!(known_host_reference(&context, &configuration, 42, "other.use").is_none());
    remember_host(
        &context,
        HostSessions {
            reference: HostReference {
                id: festerm_host_protocol::HostId::new(),
                ..reference
            },
            ..remembered
        },
    );
    assert!(known_host_reference(&context, &configuration, 42, "build.use").is_none());
}

#[test]
fn host_auth_recovery_requires_typed_auth_failure_not_matching_error_text() {
    use festerm_tunnel_client::Error;
    for error in [Error::Expired, Error::AccountChanged] {
        assert!(RequestFailure::from(error).needs_sign_in);
    }
    for error in [
        Error::Denied,
        Error::Network,
        Error::Store,
        Error::Response,
        Error::Cancelled,
        Error::Registration,
        Error::IdentityChanged,
    ] {
        assert!(!RequestFailure::from(error).needs_sign_in);
    }
    assert!(
        !RequestFailure::from("GitHub sign-in expired. Sign in again in Settings.").needs_sign_in
    );
    assert!(!format!("{:?}", RequestFailure::from("PRIVATE_TEXT")).contains("PRIVATE_TEXT"));
}

fn browser_reauthentication_fixture() -> (FesTermApp, egui::Context, Reauthentication) {
    let (mut app, context) = fixture();
    let browser = app.host_browser.as_mut().unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    browser.receiver = Some(receiver);
    assert!(sender.try_send(Err(RequestFailure::expired())).is_ok());
    app.poll_host_browser(&context);
    let browser = app.host_browser.as_ref().unwrap();
    assert!(browser.needs_sign_in && !browser.busy());
    let request = Reauthentication {
        origin: ReauthenticationOrigin::Browser {
            tab: browser.tab,
            request: browser.request,
            generation: browser.generation,
        },
        account: browser.account.clone(),
    };
    (app, context, request)
}

#[test]
fn host_auth_recovery_uses_existing_central_command_for_original_browser_account() {
    let (mut app, context, request) = browser_reauthentication_fixture();
    let debug = format!("{request:?}");
    assert!(!debug.contains(request.account.identifier()));
    assert!(!debug.contains(&request.account.credential_reference().to_persisted_string()));
    let command = app
        .host_reauthentication_command(request, &context)
        .unwrap();
    assert!(
        matches!(command, AppCommand::ReauthenticateDevTunnelAccount { account } if account == "github-user")
    );
    assert!(app.host_browser.is_none());
    assert!(
        app.tunnel_browser.is_none() && app.native_tunnel.is_none(),
        "validation must not start authentication itself"
    );
}

#[test]
fn host_auth_recovery_rejects_stale_browser_generation_accounts_and_tabs_at_dispatch() {
    for changed in 0..7 {
        let (mut app, context, request) = browser_reauthentication_fixture();
        match changed {
            0 => {
                let replacement = request
                    .account
                    .clone()
                    .with_credential(SecretReference::generate());
                let (configuration, _) =
                    app.state.configuration().with_account(replacement).unwrap();
                app.state.replace_configuration(configuration);
            }
            1 => {
                let configuration = app
                    .state
                    .configuration()
                    .without_account(request.account.identifier())
                    .unwrap();
                app.state.replace_configuration(configuration);
            }
            2 => {
                let configuration = app
                    .state
                    .configuration()
                    .without_account(request.account.identifier())
                    .unwrap();
                let replacement = DevTunnelAccount::new(
                    request.account.identifier(),
                    AccountProvider::Github,
                    77,
                    "another-user",
                    SecretReference::generate(),
                )
                .unwrap();
                app.state
                    .replace_configuration(configuration.with_account(replacement).unwrap().0);
            }
            3 => app.host_browser.as_mut().unwrap().generation += 1,
            4 => crate::screens::install_host_recipe_request_for_test(
                &context,
                app.state.active(),
                egui::Id::new("new-editor"),
            ),
            5 => {
                let tab = app.state.active();
                app.state.dispatch(AppCommand::CloseTab(tab), &context);
            }
            _ => app.host_browser.as_mut().unwrap().needs_sign_in = false,
        }
        assert!(
            app.host_reauthentication_command(request, &context)
                .is_none(),
            "changed scope {changed}"
        );
        assert!(app.tunnel_browser.is_none() && app.native_tunnel.is_none());
    }
}

type DiscoveryHarness = egui_kittest::Harness<'static, Option<AppCommand>>;
fn discovery_failure_fixture(failure: RequestFailure) -> (FesTermApp, DiscoveryHarness) {
    use egui_kittest::Harness;
    let (mut app, _) = fixture();
    app.host_browser = None;
    app.state.active_tab_mut().content = TabContent::Launcher;
    let tab = app.state.active();
    let configuration = app.state.configuration().clone();
    let account = configuration.account("github-user").unwrap().clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(460.0, 720.0))
        .build_ui_state(
            move |ui, command: &mut Option<AppCommand>| {
                if let Some(next) = crate::screens::host_workspace::show(ui, tab, &configuration) {
                    *command = Some(next);
                }
            },
            None,
        );
    harness.run_steps(2);
    // The normal open emits one bounded discovery command; this fixture injects
    // its worker result without native credentials or provider requests.
    let Some(AppCommand::FindDevTunnelHosts { request, .. }) = harness.state_mut().take() else {
        panic!("automatic Find hosts")
    };
    crate::screens::host_workspace::report_typed_discovery(
        &harness.ctx,
        tab,
        request,
        &account,
        Err(failure),
    );
    harness.run();
    (app, harness)
}

fn discovery_reauthentication_fixture() -> (FesTermApp, DiscoveryHarness, Reauthentication) {
    use egui_kittest::kittest::Queryable;
    let (app, mut harness) = discovery_failure_fixture(RequestFailure::expired());
    harness.get_by_label("Sign in again").click();
    harness.run_steps(1);
    let Some(AppCommand::ReauthenticateHostAccount(request)) = harness.state_mut().take() else {
        panic!("scoped explicit recovery")
    };
    (app, harness, request)
}

#[test]
fn automatic_host_discovery_ready_worker_is_cancelled_before_adoption_after_account_or_screen_change(
) {
    use egui_kittest::kittest::Queryable;
    for change_account in [false, true] {
        let (mut app, mut harness) = discovery_failure_fixture("Offline".into());
        harness.get_by_label("Refresh hosts").click();
        harness.run_steps(1);
        let Some(AppCommand::FindDevTunnelHosts { request, .. }) = harness.state_mut().take()
        else {
            panic!("explicit retry of the automatic read")
        };
        let tab = app.state.active();
        let account = app
            .state
            .configuration()
            .account("github-user")
            .unwrap()
            .clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .send(Ok(vec![service::TunnelSummary {
                locator: "stale.use".into(),
                name: Some("Stale worker host".into()),
                description: None,
                active: true,
            }]))
            .unwrap();
        let cancel = CancellationToken::new();
        app.host_discovery.push(Discovery {
            tab,
            request,
            account: account.clone(),
            cancel: cancel.clone(),
            receiver,
        });
        if change_account {
            let replacement = account.with_credential(SecretReference::generate());
            let configuration = app
                .state
                .configuration()
                .with_account(replacement)
                .unwrap()
                .0;
            app.state.replace_configuration(configuration);
        } else {
            crate::screens::host_workspace::cancel(&harness.ctx, tab);
        }
        app.poll_host_discovery(&harness.ctx);
        assert!(cancel.is_cancelled());
        assert!(app.host_discovery.is_empty());
        assert!(app.host_discovery_queue.is_empty());
        harness.run_steps(2);
        assert!(harness.query_by_label("Stale worker host").is_none());
        assert!(app.tunnel_browser.is_none() && app.native_tunnel.is_none());
    }
}

#[test]
fn host_auth_recovery_discovery_button_dispatches_original_account_once_without_network() {
    let (mut app, harness, request) = discovery_reauthentication_fixture();
    let stale = request.clone();
    let command = app
        .host_reauthentication_command(request, &harness.ctx)
        .unwrap();
    assert!(
        matches!(command, AppCommand::ReauthenticateDevTunnelAccount { account } if account == "github-user")
    );
    assert!(app
        .host_reauthentication_command(stale, &harness.ctx)
        .is_none());
    assert!(app.tunnel_browser.is_none() && app.native_tunnel.is_none());
}

#[test]
fn host_auth_recovery_discovery_rejects_replaced_removed_switched_cancelled_and_stale_scope() {
    use egui_kittest::kittest::Queryable;
    for changed in 0..7 {
        let (mut app, mut harness, request) = discovery_reauthentication_fixture();
        match changed {
            0 => {
                let replacement = request
                    .account
                    .clone()
                    .with_credential(SecretReference::generate());
                let (configuration, _) =
                    app.state.configuration().with_account(replacement).unwrap();
                app.state.replace_configuration(configuration);
            }
            1 => {
                let configuration = app
                    .state
                    .configuration()
                    .without_account(request.account.identifier())
                    .unwrap();
                app.state.replace_configuration(configuration);
            }
            2 => {
                let configuration = app
                    .state
                    .configuration()
                    .without_account(request.account.identifier())
                    .unwrap();
                let replacement = DevTunnelAccount::new(
                    request.account.identifier(),
                    AccountProvider::Github,
                    77,
                    "another-user",
                    SecretReference::generate(),
                )
                .unwrap();
                app.state
                    .replace_configuration(configuration.with_account(replacement).unwrap().0);
            }
            3 => {
                harness.get_by_label("Refresh hosts").click();
                harness.run_steps(1);
            }
            4 => crate::screens::host_workspace::cancel(&harness.ctx, app.state.active()),
            5 => {
                let tab = app.state.active();
                app.state.dispatch(AppCommand::CloseTab(tab), &harness.ctx);
            }
            _ => app.state.dispatch(AppCommand::OpenProfiles, &harness.ctx),
        }
        assert!(
            app.host_reauthentication_command(request, &harness.ctx)
                .is_none(),
            "changed scope {changed}"
        );
        assert!(app.tunnel_browser.is_none() && app.native_tunnel.is_none());
    }
}

#[test]
fn host_auth_recovery_discovery_nonauth_errors_have_no_sign_in_action() {
    use egui_kittest::kittest::Queryable;
    use festerm_tunnel_client::Error;
    for error in [
        Error::Denied,
        Error::Network,
        Error::Store,
        Error::Response,
        Error::Cancelled,
    ] {
        let (_, harness) = discovery_failure_fixture(RequestFailure::from(error));
        assert!(harness.query_by_label("Sign in again").is_none());
    }
}

fn ordinary_ui_fixture(service: OrdinaryService) -> (FesTermApp, egui::Context) {
    use festerm_secret_store::{MemorySecretStore, SecretBytes};
    let (mut app, context) = fixture();
    let vault: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
    let client_id = identity::registered_client_id().unwrap();
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let mut bytes = b"FESTERM_GITHUB_IDENTITY_V2\0".to_vec();
    bytes.extend(
        serde_json::to_vec(&serde_json::json!({
            "account":{"id":42,"login":"user"}, "client_id":client_id,
            "access_token":"PRIVATE_OWNED_FAKE", "refresh_token":null,
            "expires_at":expires, "refresh_expires_at":null
        }))
        .unwrap(),
    );
    // MemorySecretStore::put uses a per-store deterministic counter, while the
    // native identity lease is shared across tests/processes. Reserve a unique
    // fixture credential so unrelated test vaults never share that lease.
    let reference = SecretReference::generate();
    vault
        .create_reserved(&reference, &SecretBytes::copy_from_slice(&bytes))
        .unwrap();
    let account = DevTunnelAccount::new(
        "github-user",
        AccountProvider::Github,
        42,
        "user",
        reference,
    )
    .unwrap();
    let configuration = app
        .state
        .configuration()
        .with_account(account.clone())
        .unwrap()
        .0;
    app.state.replace_configuration(configuration);
    app.secret_store = Ok(vault);
    let browser = app.host_browser.as_mut().unwrap();
    browser.account_reference = account.credential_reference().duplicate_for_transport();
    browser.account = account;
    browser.locator = "build.fixture".into();
    browser.unbound = true;
    browser.discovered = true;
    browser.recipe.behavior = ConnectionBehavior::ChooseSession;
    browser.ordinary_service = Some(service);
    let mut rendered = context.run_ui(
        egui::RawInput {
            focused: true,
            ..Default::default()
        },
        |_| {},
    );
    rendered.textures_delta.clear();
    assert!(context.input(|input| input.focused));
    (app, context)
}

fn wait_ordinary_ui(app: &mut FesTermApp, context: &egui::Context) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        app.poll_host_browser(context);
        if app
            .host_browser
            .as_ref()
            .is_none_or(|browser| browser.receiver.is_none())
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "owned ordinary UI worker timed out"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn ordinary_desktop_independent_fixture_does_not_share_native_credential_lease() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let fixture_service: OrdinaryService = Arc::new(|_, _, _| {
        Box::pin(async {
            Err(host_client::Failure::Control(
                festerm_tunnel_client::host_control::Error::HostUpdateRequired,
            ))
        })
    });
    let (first, _) = ordinary_ui_fixture(fixture_service);
    let first_account = &first.host_browser.as_ref().unwrap().account;
    let _first_lease = super::super::tunnel_workflow::account_lease(Some(first_account)).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let (mut second, context) = ordinary_ui_fixture(Arc::new(move |_, _, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(host_client::Failure::Control(
                festerm_tunnel_client::host_control::Error::HostUpdateRequired,
            ))
        })
    }));
    second.begin_ordinary_host(&context);
    wait_ordinary_ui(&mut second, &context);
    assert_eq!(
        second.host_browser.as_ref().unwrap().feedback.as_deref(),
        Some(
            host_client::Failure::Control(
                festerm_tunnel_client::host_control::Error::HostUpdateRequired
            )
            .message()
        )
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn ordinary_desktop_expansion_once_requires_update_without_manual_fallback() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let (mut app, context) = ordinary_ui_fixture(Arc::new(move |id, locator, _| {
        assert_eq!(id, 42);
        assert_eq!(locator, "build.fixture");
        observed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(host_client::Failure::Control(
                festerm_tunnel_client::host_control::Error::HostUpdateRequired,
            ))
        })
    }));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    app.begin_ordinary_host(&context);
    wait_ordinary_ui(&mut app, &context);
    for _ in 0..3 {
        app.poll_host_browser(&context);
    }
    let browser = app.host_browser.as_ref().unwrap();
    assert!(browser.ordinary_selected);
    assert!(browser.receipt.is_none() && browser.ordinary.is_none());
    assert!(browser.inventory.is_none() && !browser.host_approved && !browser.consent);
    assert_eq!(
        browser.feedback.as_deref(),
        Some(
            host_client::Failure::Control(
                festerm_tunnel_client::host_control::Error::HostUpdateRequired
            )
            .message()
        )
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(700.0, 900.0))
        .build_ui_state(
            |ui, app: &mut FesTermApp| app.show_host_browser(ui.ctx(), false),
            app,
        );
    harness.run_steps(2);
    use egui_kittest::kittest::Queryable;
    harness.get_by_label("Retry host connection");
    assert!(harness
        .query_by_label("Trust this exact binding and prepare native owner key")
        .is_none());
    assert!(harness.query_by_label("Prepare client key").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

fn ordinary_fixture_context(
    account_id: u64,
) -> (
    festerm_sessiond::private_storage::PrivateTestDirectory,
    host_client::ordinary::OrdinaryContext,
) {
    use festerm_tunnel_bridge::{
        ordinary_auth::{OrdinaryHostInfo, OrdinaryRoute},
        owner_auth::PinnedHost,
    };
    use host_client::ordinary::{OrdinaryAccountGuard, OrdinaryKeyIndex};
    use russh::keys::{Algorithm, HashAlg, PrivateKey};
    let root = festerm_sessiond::private_storage::private_test_directory();
    let ssh = PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519).unwrap();
    let host = festerm_host_protocol::HostId::new();
    let info = OrdinaryHostInfo {
        version: 1,
        installation: festerm_host_protocol::HostId::new(),
        principal: festerm_host_protocol::PrincipalId::new(),
        policy_epoch: festerm_host_protocol::Epoch::new(1).unwrap(),
        route: OrdinaryRoute {
            tunnel_id: "build".into(),
            cluster: "fixture".into(),
            endpoint_id: format!("{host}-relay"),
            port: 47461,
            host: PinnedHost {
                host,
                host_key_fingerprint: ssh.public_key().fingerprint(HashAlg::Sha256).to_string(),
            },
        },
    };
    let token = CancellationToken::new();
    let check = token.clone();
    let guard = OrdinaryAccountGuard::new(
        token,
        Arc::new(move || {
            if check.is_cancelled() {
                Err(festerm_host_protocol::ContractError::Stale)
            } else {
                Ok(())
            }
        }),
    );
    let context = host_client::testing::ordinary_context(
        OrdinaryKeyIndex::in_app_sandbox(root.path()).unwrap(),
        account_id,
        info,
        guard,
    )
    .unwrap();
    (root, context)
}

#[test]
fn ordinary_desktop_inventory_adoption_and_durable_create_follow_global_policy() {
    for enabled in [false, true] {
        let (_root, context) = ordinary_fixture_context(42);
        let binding = context.binding().clone();
        let caps = festerm_host_protocol::Capabilities {
            inventory: true,
            attach: true,
            create: true,
            ensure: true,
            creation_allowed: enabled,
            issuance_epoch: Some(festerm_host_protocol::Epoch::new(1).unwrap()),
            ..Default::default()
        };
        let inventory = Inventory {
            capabilities: caps,
            sessions: Vec::new(),
            profiles: Vec::new(),
            unknown: Vec::new(),
        };
        let (mut app, ctx) = ordinary_ui_fixture(Arc::new(move |_, _, _| {
            let result = (context.clone(), inventory.clone());
            Box::pin(async move { Ok(result) })
        }));
        app.begin_ordinary_host(&ctx);
        wait_ordinary_ui(&mut app, &ctx);
        let browser = app.host_browser.as_mut().unwrap();
        assert_eq!(browser.context().unwrap().binding(), &binding);
        assert_eq!(&browser.recipe.host.id, binding.host());
        assert!(browser.receipt.is_none() && browser.host_approved);
        browser.creation_confirmed = true;
        assert_eq!(browser.allows_creation(), enabled);
        app.apply_host_review_action(Action::Create, &ctx);
        assert_eq!(app.state.host_creation_intents.len(), usize::from(enabled));
        assert_eq!(app.host_browser.as_ref().unwrap().worker.is_some(), enabled);
        if enabled {
            let (_, _, original) = &app.state.host_creation_intents[0];
            assert!(
                original.matches_context(&app.host_browser.as_ref().unwrap().context().unwrap())
            );
            assert_eq!(original.creation().lifetime, Lifetime::Persistent {});
        }
        // Its fixture transport is denied. This validates UI dispatch/context,
        // not a native/server creation or provider grant.
        app.host_browser = None;
    }
}

#[test]
fn ordinary_desktop_privacy_callback_cancels_pending_without_frame_and_blocks_late_adoption() {
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = entered.clone();
    let release = Arc::new(tokio::sync::Notify::new());
    let worker_release = release.clone();
    let (mut app, ctx) = ordinary_ui_fixture(Arc::new(move |_, _, _| {
        observed.store(true, std::sync::atomic::Ordering::Release);
        let release = worker_release.clone();
        Box::pin(async move {
            release.notified().await;
            Err(host_client::Failure::ProviderUnavailable)
        })
    }));
    app.begin_ordinary_host(&ctx);
    let account = app.host_browser.as_ref().unwrap().account.clone();
    let account_token = app.state.ordinary_account_token(&account).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !entered.load(std::sync::atomic::Ordering::Acquire) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    let callback = ordinary_privacy_loss(account_token.clone());
    callback();
    // No UI frame/poll intervened between the platform callback and this fence.
    assert!(account_token.is_cancelled());
    assert!(app.host_browser.as_ref().unwrap().receiver.is_some());
    release.notify_one();
    wait_ordinary_ui(&mut app, &ctx);
    let browser = app.host_browser.as_ref().unwrap();
    assert!(browser.ordinary.is_none() && browser.inventory.is_none());
    assert_eq!(
        browser.feedback.as_deref(),
        Some("Host connection cancelled because account or device privacy changed. Retry explicitly after unlocking.")
    );
    let explicit_fresh = app.state.ordinary_account_token(&account).unwrap();
    assert!(!explicit_fresh.is_cancelled());
    assert!(account_token.is_cancelled());
}

#[test]
fn ordinary_desktop_timeout_cancels_request_without_owner_receipt_guidance() {
    let (mut app, _) = fixture();
    let browser = app.host_browser.as_mut().unwrap();
    browser.ordinary_selected = true;
    let request = browser.cancel.child_token();
    browser.pending_deadline = Some((Instant::now(), request.clone()));
    let (_, receiver) = mpsc::sync_channel(1);
    browser.receiver = Some(receiver);
    browser.expire_pending(Instant::now());
    assert!(request.is_cancelled() && browser.receiver.is_none());
    assert!(!browser.cancel.is_cancelled());
    let message = browser.feedback.as_ref().unwrap();
    assert!(message.contains("Retry this host explicitly"));
    assert!(!message.contains("owner") && !message.contains("receipt"));
}

#[test]
fn ordinary_desktop_privacy_unavailable_refuses_before_bootstrap() {
    let (mut app, ctx) = ordinary_ui_fixture(Arc::new(|_, _, _| {
        panic!("privacy admission must precede bootstrap")
    }));
    app.host_browser.as_mut().unwrap().ordinary_privacy_loss =
        Some(Arc::new(std::sync::atomic::AtomicBool::new(true)));
    app.begin_ordinary_host(&ctx);
    let account_lifetime = app
        .host_browser
        .as_ref()
        .unwrap()
        .ordinary_lifetime
        .clone()
        .unwrap();
    wait_ordinary_ui(&mut app, &ctx);
    assert!(account_lifetime.is_cancelled());
    // Cancellation can occur between the poll's lifetime check and receipt of
    // the worker error. Observe the next frame's privacy-cancellation projection.
    app.poll_host_browser(&ctx);
    let browser = app.host_browser.as_ref().unwrap();
    assert_eq!(
        browser.feedback.as_deref(),
        Some("Host connection cancelled because account or device privacy changed. Retry explicitly after unlocking.")
    );
    assert!(browser.receipt.is_none() && browser.ordinary.is_none() && browser.inventory.is_none());
    assert!(app.state.host_creation_intents.is_empty());
}

#[test]
fn ordinary_desktop_attach_installs_common_session_without_owner_enrollment() {
    let (_root, context) = ordinary_fixture_context(42);
    let target = host_client::Target {
        logical: host_client::LogicalSessionId::new(),
        generation: festerm_host_protocol::GenerationId::new(3).unwrap(),
    };
    let caps = festerm_host_protocol::Capabilities {
        attach: true,
        ..Default::default()
    };
    let inventory = Inventory {
        capabilities: caps,
        sessions: vec![festerm_host_protocol::SessionSummary {
            target: target.clone(),
            name: "Retained shell".into(),
            lifetime: Lifetime::Persistent {},
            attached: false,
        }],
        profiles: Vec::new(),
        unknown: Vec::new(),
    };
    let (mut app, ctx) = ordinary_ui_fixture(Arc::new(move |_, _, _| {
        let result = (context.clone(), inventory.clone());
        Box::pin(async move { Ok(result) })
    }));
    app.begin_ordinary_host(&ctx);
    wait_ordinary_ui(&mut app, &ctx);
    assert!(app.host_browser.as_ref().unwrap().receipt.is_none());
    app.apply_host_review_action(Action::Attach(target), &ctx);
    assert!(app.host_browser.is_none());
    assert!(matches!(
        app.state.active_tab().content,
        TabContent::Session(_)
    ));
    assert!(app.state.host_creation_intents.is_empty());
    app.state.request_background_shutdown();
}

#[test]
fn session_shortcut_binds_exact_host_logical_generation_without_launch_or_creation() {
    let (app, _) = fixture();
    let mut original = app.host_browser.as_ref().unwrap().recipe.clone();
    original.launch.shell = Some("unrelated-command".into());
    original.launch.args = vec!["unrelated-argument".into()];
    original.behavior = ConnectionBehavior::AlwaysCreate;
    let target = Target {
        logical: uuid::Uuid::new_v4().to_string().parse().unwrap(),
        generation: "9".to_owned().try_into().unwrap(),
    };
    let saved = session_shortcut(
        &original,
        &target,
        "remote session",
        Lifetime::Persistent {},
        "local shortcut".into(),
    );
    assert_eq!(saved.host, original.host);
    assert_eq!(saved.logical_session, target.logical);
    assert_eq!(
        saved.behavior,
        ConnectionBehavior::AttachOnly {
            generation: target.generation
        }
    );
    assert_eq!(saved.session_name(), "remote session");
    assert_eq!(saved.name, "local shortcut");
    assert_eq!(
        saved.launch,
        Recipe {
            shell: None,
            args: Vec::new(),
            cwd: None
        }
    );
    assert_ne!(saved.id, original.id);
    let config = Configuration::empty()
        .with_host_recipe_command(MetadataCommand::Save(saved.clone()))
        .unwrap()
        .0;
    assert_eq!(
        saved_session_profile(config.host_recipes(), &original.host, &target),
        Some(&saved)
    );
    let mut other = target.clone();
    other.generation = "10".to_owned().try_into().unwrap();
    assert!(saved_session_profile(config.host_recipes(), &original.host, &other).is_none());
    other = target.clone();
    other.logical = uuid::Uuid::new_v4().to_string().parse().unwrap();
    assert!(saved_session_profile(config.host_recipes(), &original.host, &other).is_none());
    original.host.id = uuid::Uuid::new_v4().to_string().parse().unwrap();
    assert!(saved_session_profile(config.host_recipes(), &original.host, &target).is_none());
}

#[test]
fn optional_created_shortcut_is_original_bound_single_use_and_uses_confirmed_target() {
    let (mut app, _) = fixture();
    let browser = app.host_browser.as_mut().unwrap();
    let scope = browser.scope(&protected_creation_context(&browser.recipe));
    let attachment = host_client::AttachmentId::new();
    browser.pending_created_profile =
        Some((scope.clone(), attachment.clone(), "local name".into()));
    let session = festerm_host_protocol::SessionSummary {
        target: Target {
            logical: uuid::Uuid::new_v4().to_string().parse().unwrap(),
            generation: "12".to_owned().try_into().unwrap(),
        },
        name: "confirmed remote name".into(),
        lifetime: Lifetime::Persistent {},
        attached: false,
    };
    // Failure/Unknown changes feedback, not the issued original's process-local save intent.
    browser.feedback = Some("The original reply is unknown.".into());
    assert!(browser.pending_created_profile.is_some());
    let mut stale_scope = scope.clone();
    stale_scope.generation += 1;
    assert!(browser
        .confirmed_creation_shortcut(&stale_scope, &attachment, &session)
        .is_none());
    assert!(browser.pending_created_profile.is_some());
    let saved = browser
        .confirmed_creation_shortcut(&scope, &attachment, &session)
        .unwrap();
    assert_eq!(saved.logical_session, session.target.logical);
    assert_eq!(
        saved.behavior,
        ConnectionBehavior::AttachOnly {
            generation: session.target.generation
        }
    );
    assert_eq!(saved.name, "local name");
    assert!(browser
        .confirmed_creation_shortcut(&scope, &attachment, &session)
        .is_none());
    browser.pending_created_profile = Some((
        scope.clone(),
        host_client::AttachmentId::new(),
        "foreign original".into(),
    ));
    assert!(browser
        .confirmed_creation_shortcut(&scope, &attachment, &session)
        .is_none());
    assert_eq!(
        app.state.configuration().host_recipes().recipes().len(),
        1,
        "no config write before a validated Created ACK"
    );
}

#[test]
fn host_session_table_deduplicates_logical_ids_keeps_unknown_and_creation_off_inert() {
    use egui_kittest::kittest::{NodeT, Queryable};
    let mut app = FesTermApp::gallery_host_browser("sessions");
    let browser = app.host_browser.as_mut().unwrap();
    browser.workspace = true;
    browser.ordinary_selected = true;
    browser.discovered = true;
    let inventory = browser.inventory.as_mut().unwrap();
    inventory.capabilities.profiles = true;
    inventory.capabilities.ensure_profile = true;
    inventory.capabilities.creation_allowed = false;
    let session = inventory.sessions[0].clone();
    inventory.profiles = vec![
        festerm_host_protocol::ProfileSummary {
            logical: session.target.logical.clone(),
            revision: festerm_host_protocol::Epoch::new(1).unwrap(),
            name: "duplicate advertisement".into(),
        },
        festerm_host_protocol::ProfileSummary {
            logical: uuid::Uuid::new_v4().to_string().parse().unwrap(),
            revision: festerm_host_protocol::Epoch::new(1).unwrap(),
            name: session.name.clone(),
        },
    ];
    inventory.unknown.push(Target {
        logical: inventory.profiles[1].logical.clone(),
        generation: "1".to_owned().try_into().unwrap(),
    });
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1120.0, 900.0))
        .build_ui_state(
            |ui, app: &mut FesTermApp| {
                app.show_workspace_host(ui.ctx(), false);
            },
            app,
        );
    harness.run();
    assert!(harness.query_by_label("duplicate advertisement").is_none());
    harness.get_by_label("Unknown");
    harness.get_by_label("Running");
    harness.get_by_label("GitHub account");
    harness.get_by_label("Refresh sessions");
    assert!(harness.query_by_label("New host shell").is_none());
    let start = harness.get_by_label("Start & Connect");
    assert!(start.accesskit_node().is_disabled());
    assert!(
        harness.get_by_label("Unknown").rect().top() > harness.get_by_label("Running").rect().top(),
        "separate logical sessions occupy separate table rows"
    );
    {
        let browser = harness.state_mut().host_browser.as_mut().unwrap();
        let inventory = browser.inventory.as_mut().unwrap();
        inventory.unknown.clear();
        inventory.capabilities.creation_allowed = true;
        inventory.capabilities.create = true;
    }
    harness.run();
    assert!(!harness
        .get_by_label("Start & Connect")
        .accesskit_node()
        .is_disabled());
    harness.get_by_label("New host shell");
    assert!(harness
        .state()
        .host_browser
        .as_ref()
        .unwrap()
        .worker
        .is_none());
}

#[test]
fn new_host_shell_discards_selected_recipe_launch_and_uses_persistent_host_default() {
    let (mut app, _) = fixture();
    let browser = app.host_browser.as_mut().unwrap();
    browser.recipe.launch = Recipe {
        shell: Some("custom-shell".into()),
        args: vec!["custom-arg".into()],
        cwd: Some("/custom".into()),
    };
    browser.recipe.lifetime = Lifetime::Temporary {
        reconnect_grace_seconds: 60,
    };
    browser.session_name = "  new work  ".into();
    browser.prepare_new_host_shell();
    browser.host_approved = true;
    browser.inventory = Some(Inventory {
        capabilities: festerm_host_protocol::Capabilities {
            create: true,
            creation_allowed: true,
            attach: true,
            ..Default::default()
        },
        sessions: Vec::new(),
        profiles: Vec::new(),
        unknown: Vec::new(),
    });
    let (mode, request) = browser.creation_request().unwrap();
    assert_eq!(mode, CreationMode::Create);
    assert_eq!(request.name.as_deref(), Some("new work"));
    assert_eq!(
        request.recipe,
        Recipe {
            shell: None,
            args: Vec::new(),
            cwd: None
        }
    );
    assert_eq!(request.lifetime, Lifetime::Persistent {});
}

#[test]
fn narrow_host_browser_keeps_new_shell_actions_and_fields_inside_viewport() {
    use egui_kittest::kittest::Queryable;
    let app = FesTermApp::gallery_host_browser("workspace-new-shell-narrow");
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(620.0, 1000.0))
        .build_ui_state(
            |ui, app: &mut FesTermApp| app.show_workspace_host(ui.ctx(), false),
            app,
        );
    harness.run();
    for label in [
        "Find & Connect",
        "Refresh hosts",
        "Save as a local profile",
        "Create & Connect",
        "Cancel",
    ] {
        let rect = harness.get_by_label(label).rect();
        assert!(
            rect.left() >= 0.0
                && rect.right() <= 620.0
                && rect.top() >= 0.0
                && rect.bottom() <= 1000.0,
            "{label}: {rect:?}"
        );
    }
}

#[test]
fn account_switch_keeps_directory_open_retires_old_authority_and_selects_new_account_host() {
    assert_account_switch_keeps_directory(true);
}

#[test]
fn saved_profile_account_switch_retires_profile_request_and_keeps_directory_open() {
    assert_account_switch_keeps_directory(false);
}

fn assert_account_switch_keeps_directory(workspace_entry: bool) {
    use crate::screens::host_workspace::{self, HostSessions};
    use egui_kittest::kittest::Queryable;
    let (mut app, context) = fixture();
    let second = DevTunnelAccount::new(
        "second-github",
        AccountProvider::Github,
        99,
        "second-user",
        SecretReference::generate(),
    )
    .unwrap();
    let configuration = app
        .state
        .configuration()
        .with_account(second.clone())
        .unwrap()
        .0;
    app.state.replace_configuration(configuration);
    app.secret_store = Err(festerm_secret_store::SecretStoreError::LockedOrUnavailable);
    let mut recipe = app.host_browser.take().unwrap().recipe.clone();
    recipe.host_hint = Some(HostHint {
        name: "First account host".into(),
        locator: "first.fixture".into(),
    });
    recipe.behavior = ConnectionBehavior::AttachOnly {
        generation: "7".to_owned().try_into().unwrap(),
    };
    let configuration = app
        .state
        .configuration()
        .with_host_recipe_command(MetadataCommand::Edit(recipe.clone()))
        .unwrap()
        .0;
    app.state.replace_configuration(configuration);
    let saved_metadata = app.state.configuration().host_recipes().clone();
    let tab = app.state.active();
    let request = egui::Id::new("account-switch-entry");
    if workspace_entry {
        host_workspace::install_browser_request_for_test(&context, tab, request);
        app.review_discovered_host(
            tab,
            request,
            host_workspace::HostChoice {
                account: "github-user".into(),
                name: "First account host".into(),
                locator: "first.fixture".into(),
                reference: Some(recipe.host.clone()),
            },
            &context,
        );
    } else {
        crate::screens::install_host_recipe_request_for_test(&context, tab, request);
        app.review_host(tab, request, recipe, &context);
    }
    let browser = app.host_browser.as_mut().expect("normal entry factory");
    assert_eq!(browser.workspace, workspace_entry);
    assert!(browser.current(&app.state, &context));
    browser.host_approved = true;
    browser.inventory = Some(Inventory {
        capabilities: Default::default(),
        sessions: Vec::new(),
        profiles: Vec::new(),
        unknown: Vec::new(),
    });
    let old_request = browser.request;
    let old_cancel = browser.cancel.clone();
    let shared_account_lifetime = CancellationToken::new();
    browser.ordinary_lifetime = Some(shared_account_lifetime.clone());
    let scope = browser.scope(&protected_creation_context(&browser.recipe));
    browser.pending_created_profile = Some((
        scope,
        host_client::AttachmentId::new(),
        "old optional shortcut".into(),
    ));
    let (late, receiver) = mpsc::channel();
    browser.receiver = Some(receiver);
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1120.0, 900.0))
        .build_ui_state(
            |ui, (app, first): &mut (FesTermApp, bool)| {
                if *first {
                    let browser = app.host_browser.as_ref().unwrap();
                    host_workspace::install_browser_request_for_test(
                        ui.ctx(),
                        browser.tab,
                        browser.request,
                    );
                    if !browser.workspace {
                        crate::screens::install_host_recipe_request_for_test(
                            ui.ctx(),
                            browser.tab,
                            browser.request,
                        );
                    }
                    *first = false;
                }
                app.poll_host_browser(ui.ctx());
                app.show_inline_host_browser(ui, false);
                app.show_host_browser(ui.ctx(), false);
            },
            (app, true),
        );
    harness.get_by_role(accesskit::Role::ComboBox).click();
    harness.run_steps(3);
    harness.get_by_label("@second-user").click();
    harness.run_steps(3);
    let browser = harness
        .state()
        .0
        .host_browser
        .as_ref()
        .expect("account switch retains Find & Connect");
    assert!(
        browser.workspace
            && browser.directory_only
            && browser.current(&harness.state().0.state, &harness.ctx)
    );
    if !workspace_entry {
        assert!(!crate::screens::host_recipe_request_is_current(
            &harness.ctx,
            browser.tab,
            old_request
        ));
    }
    assert_eq!(browser.account, second);
    assert_ne!(browser.request, old_request);
    assert!(old_cancel.is_cancelled());
    assert!(
        !shared_account_lifetime.is_cancelled(),
        "other tabs using the account are unaffected"
    );
    assert!(
        browser.receiver.is_none()
            && browser.worker.is_none()
            && browser.inventory.is_none()
            && browser.ordinary.is_none()
            && browser.receipt.is_none()
    );
    assert!(
        !browser.host_approved
            && !browser.auto_connect
            && browser.pending_created_profile.is_none()
    );
    assert!(browser.context().is_err() && !browser.allows_creation());
    assert!(late.send(Err("late old-account result".into())).is_err());
    harness.get_by_label("Choose a host");
    assert!(harness.query_by_label("late old-account result").is_none());
    // Drive the actual Refresh command while admission is held, then deliver
    // its controlled result. No native credential or provider access occurs.
    let mut admission = Vec::new();
    while let Ok(permit) = ConnectionPermit::reserve(&harness.state().0.state.host_client_budgets) {
        admission.push(permit);
    }
    assert!(!admission.is_empty());
    harness.state_mut().0.secret_store =
        Ok(Arc::new(festerm_secret_store::MemorySecretStore::new()));
    harness.get_by_label("Refresh hosts").click();
    harness.run_steps(3);
    let queued = harness
        .state()
        .0
        .host_discovery_queue
        .front()
        .expect("account-scoped discovery waits for admission");
    let tab = queued.tab;
    let discovery_request = queued.request;
    assert_eq!(queued.account, second);
    let directory_request = harness.state().0.host_browser.as_ref().unwrap().request;
    assert_ne!(
        directory_request, discovery_request,
        "chooser lifetime and directory network attempt are distinct"
    );
    host_workspace::report_discovery(
        &harness.ctx,
        tab,
        discovery_request,
        second.identifier(),
        Ok(vec![festerm_tunnel_client::service::TunnelSummary {
            locator: "second.fixture".into(),
            name: Some("Second account build host".into()),
            description: None,
            active: true,
        }]),
    );
    harness.run_steps(3);
    let browser = harness
        .state()
        .0
        .host_browser
        .as_ref()
        .expect("refresh completion retains chooser");
    assert!(browser.directory_only && browser.current(&harness.state().0.state, &harness.ctx));
    assert_eq!(browser.request, directory_request);
    harness.state_mut().0.secret_store =
        Err(festerm_secret_store::SecretStoreError::LockedOrUnavailable);
    let reference = HostReference {
        id: uuid::Uuid::new_v4().to_string().parse().unwrap(),
        account: AccountReference::Github { account_id: 99 },
    };
    host_workspace::remember_host(
        &harness.ctx,
        HostSessions {
            reference: reference.clone(),
            hint: HostHint {
                name: "Second account build host".into(),
                locator: "second.fixture".into(),
            },
            sessions: Vec::new(),
        },
    );
    harness.run_steps(3);
    harness.get_by_label("Second account build host").click();
    harness.run_steps(3);
    let browser = harness.state().0.host_browser.as_ref().unwrap();
    assert!(!browser.directory_only);
    assert_eq!(browser.account, second);
    assert_eq!(browser.recipe.host, reference);
    assert!(browser.inventory.is_none() && browser.ordinary.is_none() && !browser.host_approved);
    assert!(browser.current(&harness.state().0.state, &harness.ctx));
    assert_eq!(
        harness.state().0.state.configuration().host_recipes(),
        &saved_metadata,
        "account/host browsing never retargets saved metadata"
    );
}

#[test]
fn ordinary_desktop_privacy_preparation_error_after_loss_survives_result_discard() {
    for failure in [
        PrivacyPreparationFailure::Unavailable,
        PrivacyPreparationFailure::Timeout,
        PrivacyPreparationFailure::CleanupUnresolved,
    ] {
        let (mut app, ctx) = fixture();
        let (_, ordinary) = ordinary_fixture_context(42);
        let token = CancellationToken::new();
        let diagnostic = PrivacyPreparationDiagnostic {
            generation: 9,
            failure: Arc::new(OnceLock::new()),
            windows_startup: Arc::new(OnceLock::new()),
        };
        let parent_cancel;
        let (send, receiver) = mpsc::sync_channel(1);
        send.send(Ok(ResultEvent::Ordinary(Box::new((
            ordinary,
            Inventory {
                capabilities: Default::default(),
                sessions: Vec::new(),
                profiles: Vec::new(),
                unknown: Vec::new(),
            },
        )))))
        .unwrap();
        {
            let browser = app.host_browser.as_mut().unwrap();
            browser.generation = 9;
            browser.ordinary_selected = true;
            browser.ordinary_lifetime = Some(token.clone());
            browser.privacy_preparation = Some(diagnostic.clone());
            browser.receiver = Some(receiver);
            let child = browser.cancel.child_token();
            browser.pending_deadline = Some((Instant::now() + Duration::from_secs(30), child));
            parent_cancel = browser.cancel.clone();
        }
        ordinary_privacy_loss(token.clone())();
        app.poll_host_browser(&ctx);
        assert!(app.host_browser.as_ref().unwrap().ordinary.is_none());
        // The OS callback fenced admission before installation returned its
        // failure. Its authority result channel has already been discarded.
        record_privacy_preparation_failure(&diagnostic, &parent_cancel, failure, &ctx);
        for _ in 0..2 {
            app.poll_host_browser(&ctx);
            let browser = app.host_browser.as_ref().unwrap();
            assert_eq!(browser.feedback.as_deref(), Some(failure.notice()));
            assert_eq!(browser.privacy_preparation_failure(), Some(failure));
            assert!(
                browser.ordinary.is_none()
                    && browser.inventory.is_none()
                    && browser.receiver.is_none()
            );
            assert!(token.is_cancelled());
        }
        assert!(app.state.host_creation_intents.is_empty());
    }
}

#[test]
fn ordinary_desktop_privacy_preparation_diagnostic_cannot_cross_generation_or_account() {
    let (mut app, ctx) = fixture();
    let old = PrivacyPreparationDiagnostic {
        generation: 7,
        failure: Arc::new(OnceLock::new()),
        windows_startup: Arc::new(OnceLock::new()),
    };
    let old_cancel = app.host_browser.as_ref().unwrap().cancel.clone();
    {
        let browser = app.host_browser.as_mut().unwrap();
        browser.generation = 8;
        browser.privacy_preparation = Some(old.clone());
    }
    record_privacy_preparation_failure(
        &old,
        &old_cancel,
        PrivacyPreparationFailure::Unavailable,
        &ctx,
    );
    assert_eq!(
        app.host_browser
            .as_ref()
            .unwrap()
            .privacy_preparation_failure(),
        None
    );
    let fresh = PrivacyPreparationDiagnostic {
        generation: 8,
        failure: Arc::new(OnceLock::new()),
        windows_startup: Arc::new(OnceLock::new()),
    };
    app.host_browser.as_mut().unwrap().privacy_preparation = Some(fresh);
    let account = app.host_browser.as_ref().unwrap().account.clone();
    let replacement = DevTunnelAccount::new(
        account.identifier(),
        AccountProvider::Github,
        account.account_id(),
        account.login(),
        SecretReference::generate(),
    )
    .unwrap();
    let config = app
        .state
        .configuration()
        .with_account(replacement)
        .unwrap()
        .0;
    app.state.replace_configuration(config);
    app.poll_host_browser(&ctx);
    assert!(app.host_browser.is_none());
    record_privacy_preparation_failure(
        &old,
        &old_cancel,
        PrivacyPreparationFailure::CleanupUnresolved,
        &ctx,
    );
    assert!(app.host_browser.is_none());
}

#[test]
fn ordinary_desktop_user_cancel_does_not_report_privacy_preparation_failure() {
    let (mut app, ctx) = fixture();
    let diagnostic = PrivacyPreparationDiagnostic {
        generation: 0,
        failure: Arc::new(OnceLock::new()),
        windows_startup: Arc::new(OnceLock::new()),
    };
    let browser = app.host_browser.as_mut().unwrap();
    browser.privacy_preparation = Some(diagnostic.clone());
    let cancel = browser.cancel.clone();
    cancel.cancel();
    record_privacy_preparation_failure(
        &diagnostic,
        &cancel,
        PrivacyPreparationFailure::Timeout,
        &ctx,
    );
    assert!(diagnostic.failure.get().is_none());
    assert_eq!(browser.privacy_preparation_failure(), None);
    assert!(browser.feedback.is_none());
}

#[test]
fn ordinary_desktop_retry_clears_preparation_diagnostic_before_new_validation() {
    let (mut app, ctx) = fixture();
    let mut rendered = ctx.run_ui(
        egui::RawInput {
            focused: true,
            ..Default::default()
        },
        |_| {},
    );
    rendered.textures_delta.clear();
    let diagnostic = PrivacyPreparationDiagnostic {
        generation: 0,
        failure: Arc::new(OnceLock::new()),
        windows_startup: Arc::new(OnceLock::new()),
    };
    diagnostic
        .failure
        .set(PrivacyPreparationFailure::Unavailable)
        .unwrap();
    let browser = app.host_browser.as_mut().unwrap();
    browser.privacy_preparation = Some(diagnostic);
    browser.locator = "invalid locator".into();
    app.begin_ordinary_host(&ctx);
    let browser = app.host_browser.as_ref().unwrap();
    assert!(browser.privacy_preparation.is_none());
    assert!(browser
        .feedback
        .as_ref()
        .unwrap()
        .contains("no valid Dev Tunnel locator"));
}

#[test]
fn ordinary_desktop_preparation_failure_remains_in_diagnostics_disclosure() {
    use egui_kittest::{kittest::Queryable, Harness};
    let (mut app, _) = fixture();
    let mut browser = app.host_browser.take().unwrap();
    let diagnostic = PrivacyPreparationDiagnostic {
        generation: browser.generation,
        failure: Arc::new(OnceLock::new()),
        windows_startup: Arc::new(OnceLock::new()),
    };
    diagnostic
        .failure
        .set(PrivacyPreparationFailure::Unavailable)
        .unwrap();
    browser.privacy_preparation = Some(diagnostic);
    let mut harness = Harness::builder().build_ui(move |ui| browser.privacy_diagnostics(ui));
    assert!(harness
        .query_by_label(PrivacyPreparationFailure::Unavailable.notice())
        .is_none());
    harness.get_by_label("Diagnostics").click();
    harness.run();
    harness.get_by_label(PrivacyPreparationFailure::Unavailable.notice());
    harness.get_by_label(
        "Connection admission remains closed. No session result or absence was accepted.",
    );
}

#[test]
fn full_page_host_selection_stays_inline_and_seven_sessions_scroll_in_short_window() {
    use crate::screens::host_workspace;
    use egui_kittest::kittest::Queryable;
    let (mut app, _) = fixture();
    app.host_browser = None;
    app.state.active_tab_mut().content = TabContent::Launcher;
    app.secret_store = Ok(Arc::new(festerm_secret_store::MemorySecretStore::new()));
    // Hold existing admission so the real full-page action cannot contact a provider.
    let mut permits = Vec::new();
    while let Ok(permit) = ConnectionPermit::reserve(&app.state.host_client_budgets) {
        permits.push(permit);
    }
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(752.0, 516.0))
        .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
    harness
        .get_by_label_contains("Dev Tunnel · Experimental")
        .click();
    harness.run_steps(3);
    let directory_heading = harness.get_by_label("Find & Connect").rect();
    // The existing automatic discovery action is processed by the real app router.
    let request = harness
        .state()
        .host_discovery_queue
        .front()
        .expect("queued discovery");
    let tab = request.tab;
    let request = request.request;
    host_workspace::report_discovery(
        &harness.ctx,
        tab,
        request,
        "github-user",
        Ok(vec![festerm_tunnel_client::service::TunnelSummary {
            locator: "first.fixture".into(),
            name: Some("Seven-session host".into()),
            description: None,
            active: true,
        }]),
    );
    harness.run_steps(3);
    harness.state_mut().secret_store =
        Err(festerm_secret_store::SecretStoreError::LockedOrUnavailable);
    harness.get_by_label("Seven-session host").click();
    harness.run_steps(3);
    let browser = harness
        .state()
        .host_browser
        .as_ref()
        .expect("actual host action");
    assert!(browser.workspace && browser.current(&harness.state().state, &harness.ctx));
    assert!(harness.state().workspace_host_is_inline(&harness.ctx));
    assert_eq!(harness.query_all_by_label("Find & Connect").count(), 1);
    let selected_heading = harness.get_by_label("Find & Connect").rect();
    assert!(
        (selected_heading.left() - directory_heading.left()).abs() < 0.5,
        "selected host keeps the full-page left inset: {selected_heading:?}, {directory_heading:?}"
    );
    assert!((selected_heading.height() - directory_heading.height()).abs() < 0.5,
        "selected host keeps the full-page title style: {selected_heading:?}, {directory_heading:?}");
    assert!(harness.query_by_label("Choose a host").is_none());
    // Inject the existing authorized inventory DTO, not a new transport or UI route.
    let mut inventory = FesTermApp::gallery_host_browser("sessions")
        .host_browser
        .take()
        .unwrap()
        .inventory
        .take()
        .unwrap();
    let session = inventory.sessions[0].clone();
    inventory.sessions = (1..=7)
        .map(|index| {
            let mut session = session.clone();
            session.name = format!("Visible session {index}");
            session.target.logical = format!("{index:08x}-1111-4111-8111-111111111111")
                .parse()
                .unwrap();
            session
        })
        .collect();
    let browser = harness.state_mut().host_browser.as_mut().unwrap();
    browser.inventory = Some(inventory);
    browser.host_approved = true;
    browser.feedback = None;
    harness.run_steps(3);
    for index in 1..=7 {
        let label = format!("Visible session {index}");
        harness.get_by_label(&label).scroll_to_me();
        harness.run_steps(3);
        let rect = harness.get_by_label(&label).rect();
        assert!(
            rect.left() >= 0.0
                && rect.right() <= 752.0
                && rect.top() >= 0.0
                && rect.bottom() <= 516.0,
            "{label}: {rect:?}"
        );
    }
    assert_eq!(harness.query_all_by_label("Connect").count(), 7);
    assert!(harness
        .state()
        .host_browser
        .as_ref()
        .unwrap()
        .worker
        .is_none());
    let cancelled = harness
        .state()
        .host_browser
        .as_ref()
        .unwrap()
        .cancel
        .clone();
    harness.state_mut().overlays.about_open = true;
    harness.run_steps(3);
    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert!(!harness.state().overlays.about_open);
    assert!(harness
        .state()
        .host_browser
        .as_ref()
        .is_some_and(|browser| browser.current(&harness.state().state, &harness.ctx)));
    assert!(
        !cancelled.is_cancelled(),
        "About owns its Escape exclusively"
    );
    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert!(harness.state().host_browser.is_none());
    assert!(cancelled.is_cancelled());
    harness.get_by_label("Choose a host");
}

#[test]
fn profile_host_selection_keeps_its_modal_owner() {
    use crate::screens::host_workspace;
    use egui_kittest::kittest::Queryable;
    let app = FesTermApp::gallery_host_browser("workspace-sessions");
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(752.0, 516.0))
        .build_ui_state(
            |ui, (app, first): &mut (FesTermApp, bool)| {
                if *first {
                    let browser = app.host_browser.as_ref().unwrap();
                    host_workspace::install_profile_request_for_test(
                        ui.ctx(),
                        browser.tab,
                        browser.request,
                    );
                    *first = false;
                }
                assert!(!app.show_inline_host_browser(ui, false));
                app.show_host_browser(ui.ctx(), false);
            },
            (app, true),
        );
    harness.run_steps(3);
    assert_eq!(harness.query_all_by_label("Select session").count(), 2);
    assert!(!harness.state().0.workspace_host_is_inline(&harness.ctx));
    assert!(harness
        .state()
        .0
        .host_browser
        .as_ref()
        .unwrap()
        .current(&harness.state().0.state, &harness.ctx));
}

#[test]
fn inline_host_workspace_preserves_pending_automation_settings_until_settings_renders() {
    use crate::screens::host_workspace;
    use egui_kittest::kittest::Queryable;
    let mut app = FesTermApp::gallery_host_browser("workspace-sessions");
    app.open_automation_settings = true;
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(752.0, 516.0))
        .build_ui_state(
            |ui, (app, first): &mut (FesTermApp, bool)| {
                if *first {
                    let browser = app.host_browser.as_ref().unwrap();
                    host_workspace::install_browser_request_for_test(
                        ui.ctx(),
                        browser.tab,
                        browser.request,
                    );
                    *first = false;
                }
                app.ui_content(ui);
            },
            (app, true),
        );
    harness.run_steps(3);
    assert!(harness.state().0.workspace_host_is_inline(&harness.ctx));
    harness.get_by_label("Find & Connect");
    assert!(
        harness.state().0.open_automation_settings,
        "inline rendering does not consume the pending Settings route"
    );
    let context = harness.ctx.clone();
    harness
        .state_mut()
        .0
        .state
        .dispatch(AppCommand::OpenSettings, &context);
    harness.run_steps(3);
    harness.get_by_label("Registered workflows");
    assert!(
        !harness.state().0.open_automation_settings,
        "normal Settings rendering consumes its pending route"
    );
    assert!(harness.query_by_label("Keyboard shortcuts").is_none());
}

#[test]
fn ordinary_desktop_windows_privacy_detail_is_finite_fenced_and_preserves_cleanup_notice() {
    use crate::recording_privacy::{PreparationFailureReport, WindowsPrivacyDiagnostic};
    use egui_kittest::{kittest::Queryable, Harness};
    let (mut app, context) = fixture();
    let mut browser = app.host_browser.take().unwrap();
    let diagnostic = PrivacyPreparationDiagnostic::new(browser.generation);
    let detail = WindowsPrivacyDiagnostic {
        stage: "service_state",
        code: None,
        service_state: Some("stopped"),
        service_not_running: true,
    };
    record_privacy_preparation_report(
        &diagnostic,
        &CancellationToken::new(),
        PreparationFailureReport {
            cleanup_unresolved: true,
            windows_startup: Some(detail),
        },
        &context,
    );
    assert_eq!(
        diagnostic.failure.get(),
        Some(&PrivacyPreparationFailure::CleanupUnresolved)
    );
    browser.privacy_preparation = Some(diagnostic.clone());
    let mut harness = Harness::builder().build_ui(move |ui| browser.privacy_diagnostics(ui));
    assert!(harness.query_by_label(&detail.detail()).is_none());
    harness.get_by_label("Diagnostics").click();
    harness.run();
    harness.get_by_label(&detail.detail());
    harness.get_by_label(PrivacyPreparationFailure::CleanupUnresolved.notice());
    harness.get_by_label("Remote Desktop Services (TermService) is not running. Review Windows privacy-monitor prerequisites before retrying. fesTerm does not start the service or enable Remote Desktop.");
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let ignored = PrivacyPreparationDiagnostic::new(0);
    record_privacy_preparation_report(
        &ignored,
        &cancelled,
        PreparationFailureReport {
            cleanup_unresolved: false,
            windows_startup: Some(detail),
        },
        &context,
    );
    assert!(ignored.failure.get().is_none() && ignored.windows_startup.get().is_none());
}

#[test]
fn ordinary_desktop_windows_privacy_detail_never_relabels_other_native_failures() {
    use crate::recording_privacy::WindowsPrivacyDiagnostic;
    let detail = WindowsPrivacyDiagnostic {
        stage: "wts_registration",
        code: Some(1722),
        service_state: None,
        service_not_running: false,
    };
    assert_eq!(detail.detail(), "Windows privacy startup: stage=wts_registration; OS code=1722; TermService state=not-recorded.");
    assert!(!detail.service_not_running);
    let (mut app, _) = fixture();
    let mut browser = app.host_browser.take().unwrap();
    let diagnostic = PrivacyPreparationDiagnostic::new(browser.generation + 1);
    diagnostic
        .failure
        .set(PrivacyPreparationFailure::Unavailable)
        .unwrap();
    diagnostic.windows_startup.set(detail).unwrap();
    browser.privacy_preparation = Some(diagnostic);
    assert!(browser.privacy_preparation_failure().is_none());
}

#[test]
fn ordinary_desktop_windows_privacy_detail_stays_with_first_failure() {
    use crate::recording_privacy::{PreparationFailureReport, WindowsPrivacyDiagnostic};
    let context = egui::Context::default();
    let cancel = CancellationToken::new();
    let detail = WindowsPrivacyDiagnostic {
        stage: "service_state",
        code: None,
        service_state: Some("stopped"),
        service_not_running: true,
    };
    for first_generic in [false, true] {
        let diagnostic = PrivacyPreparationDiagnostic::new(0);
        if first_generic {
            record_privacy_preparation_failure(
                &diagnostic,
                &cancel,
                PrivacyPreparationFailure::Timeout,
                &context,
            );
        } else {
            record_privacy_preparation_report(
                &diagnostic,
                &cancel,
                PreparationFailureReport::default(),
                &context,
            );
        }
        record_privacy_preparation_report(
            &diagnostic,
            &cancel,
            PreparationFailureReport {
                cleanup_unresolved: true,
                windows_startup: Some(detail),
            },
            &context,
        );
        assert!(
            diagnostic.windows_startup.get().is_none(),
            "late detail cannot join an earlier failure"
        );
        assert_eq!(
            diagnostic.failure.get().copied(),
            Some(if first_generic {
                PrivacyPreparationFailure::Timeout
            } else {
                PrivacyPreparationFailure::Unavailable
            })
        );
    }
}
