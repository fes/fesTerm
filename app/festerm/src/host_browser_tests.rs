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
