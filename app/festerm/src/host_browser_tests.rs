use super::*;
use festerm_config::{
    host_recipes::{HostReference, MetadataCommand, Recipe},
    AccountProvider, Configuration,
};

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

fn protected_creation_context(recipe: &PublicRecipe) -> ApprovedContext {
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
    use egui_kittest::{kittest::Queryable, Harness};
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
    harness.run();
    assert!(
        harness.state().is_none(),
        "opening the page must remain offline"
    );
    harness.get_by_label("Find hosts · @user").click();
    harness.run_steps(1);
    let Some(AppCommand::FindDevTunnelHosts { request, .. }) = harness.state_mut().take() else {
        panic!("explicit Find hosts")
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
                harness.get_by_label("Find hosts · @user").click();
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
