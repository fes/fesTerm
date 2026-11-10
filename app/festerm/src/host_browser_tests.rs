use super::*;
use festerm_config::{
    host_recipes::{HostReference, MetadataCommand, Recipe},
    AccountProvider, Configuration,
};

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
    for changed in 0..4 {
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
