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

fn client_inventory(creation_allowed: bool) -> Inventory {
    Inventory {
        capabilities: Capabilities {
            issuance_epoch: Some(festerm_host_protocol::Epoch::new(1).unwrap()),
            inventory: true,
            profiles: false,
            ensure: true,
            create: true,
            attach: true,
            ensure_profile: false,
            detach: true,
            explicit_end: true,
            temporary: false,
            creation_allowed,
        },
        sessions: Vec::new(),
        profiles: Vec::new(),
        unknown: Vec::new(),
    }
}

fn client_recipe() -> PublicRecipe {
    PublicRecipe::persistent(
        "Local profile",
        HostReference {
            id: HostId::new(),
            account: AccountReference::Github { account_id: 7 },
        },
        Recipe::default(),
    )
    .unwrap()
}

#[test]
fn mobile_host_profile_save_is_offline_permission_independent_and_separates_names() {
    let profiles = crate::profiles::testing::profiles();
    let mut app = app().with_profiles(profiles);
    app.open_hosts();
    let recipe = client_recipe();
    let logical = recipe.logical_session.clone();
    app.hosts.selected = Some(recipe);
    app.hosts.inventory = Some(client_inventory(false));
    app.hosts.begin_profile_edit();
    app.hosts.profile_name = "  Work profile  ".into();
    app.hosts.session_name = "  build shell  ".into();
    app.hosts.create_if_missing = true;
    app.host_action(&egui::Context::default(), Action::SaveProfile)
        .unwrap();
    let saved = app.hosts.selected.as_ref().unwrap();
    assert_eq!(saved.name, "Work profile");
    assert_eq!(saved.session_name(), "build shell");
    assert_eq!(saved.logical_session, logical);
    assert!(matches!(saved.behavior, ConnectionBehavior::ResumeOrCreate));
    assert!(matches!(
        saved.lifetime,
        festerm_host_protocol::Lifetime::Persistent {}
    ));
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
    assert!(app.hosts.originals.is_empty());
    assert!(app
        .profiles
        .as_ref()
        .unwrap()
        .rows(crate::model::Capabilities::all())
        .iter()
        .any(|row| row.name == "Work profile"));
}

#[test]
fn mobile_host_profile_existing_selection_keeps_exact_identity_without_creation_permission() {
    let mut hosts = HostUi {
        selected: Some(client_recipe()),
        ..Default::default()
    };
    let target = host_client::Target {
        logical: LogicalSessionId::new(),
        generation: GenerationId::new(4).unwrap(),
    };
    let mut inventory = client_inventory(false);
    inventory
        .sessions
        .push(festerm_host_protocol::SessionSummary {
            target: target.clone(),
            name: "Remote shell".into(),
            lifetime: festerm_host_protocol::Lifetime::Persistent {},
            attached: false,
        });
    hosts.inventory = Some(inventory);
    hosts.begin_profile_edit();
    hosts.create_if_missing = false;
    hosts.existing_session = Some(0);
    let recipe = hosts.profile_draft().unwrap();
    assert_eq!(recipe.logical_session, target.logical);
    assert_eq!(recipe.session_name(), "Remote shell");
    assert!(
        matches!(recipe.behavior, ConnectionBehavior::AttachOnly { generation } if generation == target.generation)
    );
    assert!(selected_target(Some(&recipe), &target).is_ok());
    assert!(hosts.originals.is_empty());
}

#[test]
fn mobile_host_existing_profile_rename_and_creation_off_round_trip_without_inventory() {
    let profiles = crate::profiles::testing::profiles();
    let mut recipe = client_recipe();
    recipe.session_name = Some("remote shell".into());
    let id = recipe.id.clone();
    let logical = recipe.logical_session.clone();
    profiles
        .store_handle()
        .lock()
        .unwrap()
        .apply_host_recipe_command(MetadataCommand::Save(recipe.clone()))
        .unwrap();
    let mut app = app().with_profiles(profiles);
    assert!(app.select_host_profile(id.as_str(), true));
    app.hosts.profile_name = "Renamed local profile".into();
    app.hosts.create_if_missing = false;
    app.host_action(&egui::Context::default(), Action::SaveProfile)
        .unwrap();
    let store = app.profiles.as_ref().unwrap().store_handle();
    let stored = store.lock().unwrap();
    let saved = stored.host_recipes().get(&id).unwrap();
    assert_eq!(saved.name, "Renamed local profile");
    assert_eq!(saved.session_name(), "remote shell");
    assert_eq!(saved.logical_session, logical);
    assert!(matches!(saved.behavior, ConnectionBehavior::ResumeOnly {}));
    assert_eq!(stored.host_recipes().recipes().len(), 1);
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
}

#[test]
fn mobile_host_profile_rename_with_inventory_preserves_logical_resume_policy() {
    for behavior in [
        ConnectionBehavior::ResumeOnly {},
        ConnectionBehavior::ResumeOrCreate,
    ] {
        let mut recipe = client_recipe();
        recipe.behavior = behavior;
        let logical = recipe.logical_session.clone();
        let mut inventory = client_inventory(false);
        inventory
            .sessions
            .push(festerm_host_protocol::SessionSummary {
                target: host_client::Target {
                    logical: logical.clone(),
                    generation: GenerationId::new(4).unwrap(),
                },
                name: "Remote shell".into(),
                lifetime: festerm_host_protocol::Lifetime::Persistent {},
                attached: false,
            });
        let mut hosts = HostUi {
            selected: Some(recipe),
            inventory: Some(inventory),
            ..Default::default()
        };
        hosts.begin_profile_edit();
        hosts.profile_name = "Renamed profile".into();
        hosts.create_if_missing = false;
        let saved = hosts.profile_draft().unwrap();
        assert_eq!(saved.name, "Renamed profile");
        assert_eq!(saved.logical_session, logical);
        assert!(matches!(saved.behavior, ConnectionBehavior::ResumeOnly {}));
        assert!(selected_target(
            Some(&saved),
            &host_client::Target {
                logical,
                generation: GenerationId::new(5).unwrap(),
            }
        )
        .is_ok());
        assert!(hosts.originals.is_empty());
    }
}

#[test]
#[cfg(unix)]
fn mobile_host_saved_custom_launch_requires_explicit_review_before_creation() {
    for custom in 0..4 {
        let (_root, mut app, _account, _store) = recovery_fixture();
        let recipe = app.hosts.selected.as_mut().unwrap();
        match custom {
            0 => recipe.launch.shell = Some("/bin/sh".into()),
            1 => recipe.launch.args = vec!["-c".into(), "echo reviewed".into()],
            2 => recipe.launch.cwd = Some("/tmp".into()),
            _ => {
                recipe.lifetime = festerm_host_protocol::Lifetime::Temporary {
                    reconnect_grace_seconds: 60,
                };
            }
        }
        assert!(requires_launch_review(recipe));
        app.hosts.launch = true;
        app.hosts.saved_connect = Some(SavedConnect::Originals);
        app.continue_saved_originals(&egui::Context::default())
            .unwrap();
        assert!(app.hosts.saved_connect.is_none());
        assert!(!app.hosts.launch);
        let error = app
            .host_action(&egui::Context::default(), Action::Create)
            .unwrap_err();
        assert!(error.contains("Confirm the exact recipe"));
        assert!(app.hosts.originals.is_empty());
        assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
    }
}

#[test]
fn mobile_host_resume_only_accepts_new_authorized_generation_of_same_logical_never_creates() {
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    let id = festerm_mobile_tunnels::testing::seed_account(&store, 7, "sample", false);
    let account = store
        .lock()
        .unwrap()
        .accounts()
        .iter()
        .find(|account| account.id == id)
        .unwrap()
        .clone();
    let mut recipe = client_recipe();
    recipe.behavior = ConnectionBehavior::ResumeOnly {};
    let matching = host_client::Target {
        logical: recipe.logical_session.clone(),
        generation: GenerationId::new(32).unwrap(),
    };
    assert!(selected_target(Some(&recipe), &matching).is_ok());
    let other = host_client::Target {
        logical: LogicalSessionId::new(),
        ..matching
    };
    assert!(selected_target(Some(&recipe), &other).is_err());
    let mut app = app().with_profiles(profiles);
    app.open_hosts();
    app.hosts.selected = Some(recipe.clone());
    app.hosts.inventory = Some(client_inventory(true));
    app.hosts.saved_connect = Some(SavedConnect::Inventory);
    assert!(app
        .continue_saved_inventory(&egui::Context::default())
        .is_err());
    assert!(app
        .hosts
        .creation_intent(&creation_context(), recipe, account)
        .is_err());
    assert!(app.hosts.originals.is_empty());
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
}

#[test]
#[cfg(unix)]
fn mobile_host_saved_connect_loaded_receipt_continues_without_asserting_host_enrollment() {
    let (_root, mut app, _account, _store) = recovery_fixture();
    app.hosts.locator = "fixture.test".into();
    app.hosts.enrolled = false;
    app.hosts.trust = false;
    app.hosts.launch = true;
    app.hosts.saved_connect = Some(SavedConnect::Receipt);
    let resolved = app.hosts.resolved.take();
    assert!(matches!(
        app.hosts.saved_receipt_loaded().unwrap(),
        Some(Action::Resolve)
    ));
    assert!(app.hosts.saved_connect == Some(SavedConnect::Resolve));
    assert!(!app.hosts.enrolled && !app.hosts.trust);
    assert!(
        app.hosts.context().is_err(),
        "service resolution is still required"
    );
    app.hosts.resolved = resolved;
    assert!(
        app.hosts.context().is_ok(),
        "only a protected owner-proof attempt is enabled"
    );
    app.hosts.saved_connect = None;
    assert!(
        app.hosts.context().is_err(),
        "ordinary UI still requires explicit owner review"
    );
    app.hosts.saved_connect = Some(SavedConnect::Receipt);
    app.hosts.receipt = None;
    assert!(app.hosts.saved_receipt_loaded().is_err());
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
}

#[test]
#[cfg(unix)]
fn mobile_host_saved_connect_never_automatically_retries_retained_original() {
    let (_root, mut app, account, _store) = recovery_fixture();
    let recipe = app.hosts.selected.clone().unwrap();
    let context = app.hosts.context().unwrap();
    app.hosts
        .creation_intent(&context, recipe, account)
        .unwrap();
    app.hosts.saved_connect = Some(SavedConnect::Originals);
    let error = app
        .continue_saved_originals(&egui::Context::default())
        .unwrap_err();
    assert!(error.contains("original action is retained"));
    assert!(app.hosts.saved_connect.is_none());
    assert_eq!(app.hosts.originals.len(), 1);
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
}

#[test]
#[cfg(unix)]
fn mobile_host_selecting_another_host_clears_saved_recipe_and_trust_but_preserves_originals() {
    let (_root, mut app, account, _store) = recovery_fixture();
    let context = app.hosts.context().unwrap();
    let recipe = app.hosts.selected.clone().unwrap();
    app.profiles
        .as_ref()
        .unwrap()
        .store_handle()
        .lock()
        .unwrap()
        .apply_host_recipe_command(MetadataCommand::Save(recipe.clone()))
        .unwrap();
    let original = app
        .hosts
        .creation_intent(&context, recipe, account)
        .unwrap();
    app.hosts.saved = true;
    app.hosts.locator = "first.test".into();
    app.hosts.prerequisite = Zeroizing::new("first host owner binding".into());
    app.hosts.hosts = vec![TunnelSummary {
        locator: "second.test".into(),
        name: Some("Other host".into()),
        description: None,
        active: true,
    }];
    app.host_action(&egui::Context::default(), Action::SelectHost(0))
        .unwrap();
    assert_eq!(app.hosts.locator, "second.test");
    assert!(app.hosts.selected.is_none() && !app.hosts.saved);
    assert!(app.hosts.receipt.is_none() && app.hosts.fingerprint.is_empty());
    assert!(app.hosts.prerequisite.is_empty());
    assert!(!app.hosts.trust && !app.hosts.enrolled && !app.hosts.launch);
    assert!(Arc::ptr_eq(&app.hosts.originals[0].intent, &original));
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
}

#[test]
fn mobile_host_connection_notices_distinguish_authoritative_dispositions() {
    use host_client::EnsureDisposition;
    assert_eq!(
        super::view::creation_notice(EnsureDisposition::Created),
        "Started a new shell."
    );
    assert_eq!(
        super::view::creation_notice(EnsureDisposition::Resumed),
        "Resumed the existing session."
    );
    assert_eq!(
        super::view::creation_notice(EnsureDisposition::Recreated),
        "Previous session ended. Started a new shell."
    );
    let hosts = HostUi {
        inventory: Some(client_inventory(true)),
        ..Default::default()
    };
    assert!(
        hosts.connection_notice.is_none(),
        "inventory emptiness is not a restart signal"
    );
}

#[test]
fn mobile_host_quick_create_requires_name_and_preserves_original_intent_after_failure() {
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    let id = festerm_mobile_tunnels::testing::seed_account(&store, 7, "sample", false);
    let account = store
        .lock()
        .unwrap()
        .accounts()
        .iter()
        .find(|account| account.id == id)
        .unwrap()
        .clone();
    let context = creation_context();
    let mut recipe = client_recipe();
    recipe.host.id = context.binding().host().clone();
    let mut hosts = HostUi {
        selected: Some(recipe),
        quick_new: true,
        inventory: Some(client_inventory(true)),
        quick_name: "   ".into(),
        ..Default::default()
    };
    assert!(hosts.freeze_quick_recipe(&account).is_err());
    assert!(hosts.originals.is_empty());
    hosts.quick_name = "  build  ".into();
    hosts.freeze_quick_recipe(&account).unwrap();
    let recipe = hosts.selected.clone().unwrap();
    let original = hosts
        .creation_intent(&context, recipe, account.clone())
        .unwrap();
    assert_eq!(original.creation().name.as_deref(), Some("build"));
    assert!(matches!(
        original.creation().lifetime,
        festerm_host_protocol::Lifetime::Persistent {}
    ));
    hosts.error = Some("Host unavailable".into());
    hosts.freeze_quick_recipe(&account).unwrap();
    let retry = hosts
        .creation_intent(&context, hosts.selected.clone().unwrap(), account.clone())
        .unwrap();
    assert!(Arc::ptr_eq(&original, &retry));
    assert_eq!(hosts.quick_name, "  build  ");
    hosts.quick_name = "replacement".into();
    assert!(hosts.freeze_quick_recipe(&account).is_err());
    assert_eq!(hosts.originals.len(), 1);
    hosts
        .inventory
        .as_mut()
        .unwrap()
        .capabilities
        .creation_allowed = false;
    assert!(!hosts.quick_creation_allowed());
    assert!(hosts.freeze_quick_recipe(&account).is_err());
}

#[test]
fn mobile_host_settings_sign_out_is_per_account_and_opening_saved_editor_is_offline() {
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    let first = festerm_mobile_tunnels::testing::seed_account(&store, 7, "sample", false);
    let second = festerm_mobile_tunnels::testing::seed_account(&store, 8, "other", false);
    let mut recipe = client_recipe();
    recipe.host_hint = Some(HostHint {
        locator: "fixture.test".into(),
        name: "Build host".into(),
    });
    let id = recipe.id.as_str().to_owned();
    store
        .lock()
        .unwrap()
        .apply_host_recipe_command(MetadataCommand::Save(recipe))
        .unwrap();
    let mut app = app().with_profiles(profiles);
    app.edit_profile(&id);
    assert!(app.hosts.editing && app.hosts.open);
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
    app.apply_shell_actions(
        &egui::Context::default(),
        vec![crate::shell::ShellAction::SignOutTunnelAccount(first)],
    );
    let accounts = app.tunnel_accounts();
    assert_eq!(accounts, vec![(second, "other".into())]);
    assert!(app.hosts.originals.is_empty());
}

#[test]
fn mobile_host_discovered_selection_and_paint_are_offline_and_keep_failed_results() {
    use egui_kittest::kittest::{By, Queryable};
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    let account = festerm_mobile_tunnels::testing::seed_account(&store, 7, "sample", false);
    let mut app = app().with_profiles(profiles);
    app.open_hosts();
    app.hosts.account = account;
    app.hosts.hosts = vec![TunnelSummary {
        locator: "fixture.test".into(),
        name: Some("Build host".into()),
        description: None,
        active: true,
    }];
    app.hosts.discovered = true;
    app.hosts.error = Some("Host timed out; no absence inferred.".into());
    app.host_action(&egui::Context::default(), Action::SelectHost(0))
        .unwrap();
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(390.0, 844.0))
        .build_ui_state(|ui, app: &mut MobileApp| app.show_hosts(ui.ctx()), app);
    harness.run_steps(3);
    harness.get(By::new().label("Find hosts · @sample"));
    harness.get(By::new().label("Collapse Build host"));
    assert_eq!(harness.state().hosts.hosts.len(), 1);
    assert!(harness.state().hosts.error.is_some());
    assert!(harness.state().hosts.pending.is_none());
    assert!(harness.state().hosts.worker.is_none());
    assert!(harness.state().hosts.selected.is_none());
    assert!(harness.state().hosts.receipt.is_none());
}

#[test]
fn mobile_host_header_contains_done_and_title_at_phone_and_tablet_widths() {
    use egui_kittest::kittest::{By, Queryable};
    for width in [390.0, 834.0] {
        for editing in [false, true] {
            let mut app = app();
            app.open_hosts();
            if editing {
                app.hosts.selected = Some(client_recipe());
                app.hosts.begin_profile_edit();
            }
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(width, 844.0))
                .build_ui_state(|ui, app: &mut MobileApp| app.show_hosts(ui.ctx()), app);
            harness.run_steps(3);
            let done = harness.get(By::new().label("Done")).rect();
            let title = harness
                .get(By::new().label(if editing {
                    "Dev Tunnel profile"
                } else {
                    "Find & connect"
                }))
                .rect();
            assert!(done.width() <= 80.0 && done.height() >= 44.0, "{done:?}");
            assert!(
                title.left() >= done.right() && !title.intersects(done),
                "{done:?} {title:?}"
            );
            assert!(
                title.right() <= width - 8.0 && title.height() > 0.0,
                "{title:?}"
            );
            assert!(done.left() >= ((width - 560.0) / 2.0).max(8.0), "{done:?}");
            assert!(title.right() - done.left() <= 560.0, "{done:?} {title:?}");
            assert!(harness.state().hosts.pending.is_none());
        }
    }
}

#[test]
fn mobile_host_disclosure_is_a_painted_triangle_inside_its_touch_target() {
    for expanded in [false, true] {
        let context = egui::Context::default();
        let mut target = None;
        let mut rendered = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(390.0, 844.0),
                )),
                ..Default::default()
            },
            |ui| target = Some(super::view::host_disclosure(ui, "Build host", expanded, true).rect),
        );
        let target = target.unwrap();
        let triangle = rendered
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Path(path) if path.closed && path.points.len() == 3 => {
                    Some(&path.points)
                }
                _ => None,
            })
            .expect("disclosure must be vector geometry, independent of a font glyph");
        assert!(triangle.iter().all(|point| target.contains(*point)));
        assert!(target.height() >= 44.0);
        if expanded {
            assert!(triangle[2].y > triangle[0].y && triangle[0].y == triangle[1].y);
        } else {
            assert!(triangle[2].x > triangle[0].x && triangle[0].x == triangle[1].x);
        }
        rendered.textures_delta.clear();
    }
}

#[test]
fn mobile_host_valid_quick_create_shows_enabled_primary_without_opening_protected_history() {
    use egui_kittest::kittest::{By, Queryable};
    let mut app = app();
    app.open_hosts();
    app.hosts.quick_new = true;
    app.hosts.quick_name = "build".into();
    app.hosts.selected = Some(client_recipe());
    app.hosts.inventory = Some(client_inventory(true));
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(390.0, 844.0))
        .build_ui_state(|ui, app: &mut MobileApp| app.show_hosts(ui.ctx()), app);
    harness.run_steps(3);
    harness.get(
        By::new()
            .label("Create and connect")
            .predicate(|node| !node.is_disabled()),
    );
    assert!(harness.state().hosts.pending.is_none());
    assert!(harness.state().hosts.worker.is_none());
    assert!(harness.state().hosts.original_store.is_none());
    assert!(harness.state().hosts.quick_check.is_none());
}

#[test]
#[cfg(unix)]
fn mobile_host_profile_form_displays_bound_host_and_account_without_retargeting() {
    use egui_kittest::kittest::{By, Queryable};
    let (_root, mut app, _account, _store) = recovery_fixture();
    let recipe = app.hosts.selected.as_mut().unwrap();
    recipe.host_hint = Some(HostHint {
        locator: "fixture.test".into(),
        name: "Build host".into(),
    });
    let host = recipe.host.id.clone();
    app.hosts.begin_profile_edit();
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(390.0, 844.0))
        .build_ui_state(|ui, app: &mut MobileApp| app.show_hosts(ui.ctx()), app);
    harness.run_steps(3);
    harness.get(By::new().label("Host · Build host"));
    harness.get(By::new().label("@mobile"));
    assert_eq!(
        harness.state().hosts.selected.as_ref().unwrap().host.id,
        host
    );
    assert!(harness.state().hosts.pending.is_none() && harness.state().hosts.worker.is_none());
}

#[cfg(unix)]
fn quick_check_fixture() -> (tempfile::TempDir, MobileApp, TunnelAccount) {
    let (root, mut app, account, store) = recovery_fixture();
    app.hosts.quick_new = true;
    app.hosts.quick_name = " build ".into();
    app.hosts.inventory = Some(client_inventory(true));
    app.hosts.freeze_quick_recipe(&account).unwrap();
    app.hosts.original_store = Some(store);
    let receipt = app.hosts.receipt.as_ref().unwrap();
    app.hosts.quick_check = Some(QuickCheck {
        recipe: app.hosts.selected.clone().unwrap(),
        account: account.clone(),
        epoch: app.hosts.epoch,
        generation: app.hosts.generation,
        owner: receipt.binding().clone(),
        reference: receipt.reference(),
    });
    app.hosts.launch = false;
    (root, app, account)
}

#[test]
#[cfg(unix)]
fn mobile_host_explicit_quick_create_history_continuation_keeps_frozen_draft_and_authority() {
    let (_root, mut app, account) = quick_check_fixture();
    let selected = app.hosts.selected.clone();
    assert!(matches!(
        app.hosts.finish_quick_check(&account).unwrap(),
        Action::Create
    ));
    assert_eq!(app.hosts.selected, selected);
    assert_eq!(app.hosts.quick_name, " build ");
    assert!(app.hosts.launch && app.hosts.quick_check.is_none());
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
    for changed in 0..7 {
        let (_root, mut app, account) = quick_check_fixture();
        match changed {
            0 => app.hosts.quick_name = "other shell".into(),
            1 => app.hosts.epoch += 1,
            2 => {
                app.hosts
                    .inventory
                    .as_mut()
                    .unwrap()
                    .capabilities
                    .creation_allowed = false
            }
            3 => app.hosts.selected.as_mut().unwrap().logical_session = LogicalSessionId::new(),
            4 => app.hosts.original_store = None,
            5 => app
                .hosts
                .quick_check
                .as_mut()
                .unwrap()
                .account
                .login
                .push_str("-changed"),
            _ => {
                app.hosts.quick_check.as_mut().unwrap().reference =
                    festerm_secret_store::SecretReference::generate()
            }
        }
        assert!(app.hosts.finish_quick_check(&account).is_err());
        assert!(app.hosts.quick_check.is_none() && !app.hosts.launch);
        assert!(app.hosts.originals.is_empty());
        assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
    }
}

#[test]
#[cfg(unix)]
fn mobile_host_quick_create_history_continuation_requires_review_of_retained_original() {
    let (_root, mut app, account) = quick_check_fixture();
    let context = app.hosts.context().unwrap();
    let recipe = app.hosts.selected.clone().unwrap();
    let original = app
        .hosts
        .creation_intent(&context, recipe, account.clone())
        .unwrap();
    let error = app.hosts.finish_quick_check(&account).err().unwrap();
    assert!(error.contains("original action is retained"));
    assert!(!app.hosts.launch && app.hosts.quick_check.is_none());
    assert_eq!(app.hosts.originals.len(), 1);
    assert!(Arc::ptr_eq(&original, &app.hosts.originals[0].intent));
    assert!(app
        .host_action(&egui::Context::default(), Action::QuickCreate)
        .is_err());
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
}

#[cfg(unix)]
impl MobileApp {
    pub(crate) fn configure_host_gallery(&mut self, state: &str) {
        let (_root, mut fixture, _, _) = recovery_fixture_with_login("sample-dev");
        fixture.hosts.locator = "fixture.test".into();
        fixture.hosts.hosts = vec![
            TunnelSummary {
                locator: "fixture.test".into(),
                name: Some("Build Mac".into()),
                description: None,
                active: true,
            },
            TunnelSummary {
                locator: "second.test".into(),
                name: Some("Linux workstation".into()),
                description: None,
                active: false,
            },
        ];
        fixture.hosts.discovered = true;
        let mut inventory = client_inventory(true);
        for (index, name) in ["build", "notes"].into_iter().enumerate() {
            inventory
                .sessions
                .push(festerm_host_protocol::SessionSummary {
                    target: host_client::Target {
                        logical: LogicalSessionId::new(),
                        generation: GenerationId::new(index as u128 + 1).unwrap(),
                    },
                    name: name.into(),
                    lifetime: festerm_host_protocol::Lifetime::Persistent {},
                    attached: false,
                });
        }
        inventory.capabilities.profiles = true;
        inventory.capabilities.ensure_profile = true;
        inventory
            .profiles
            .push(festerm_host_protocol::ProfileSummary {
                logical: LogicalSessionId::new(),
                revision: festerm_host_protocol::Epoch::new(1).unwrap(),
                name: "Shared build profile".into(),
            });
        fixture.hosts.inventory = Some(inventory);
        fixture.surface = Surface::Tabs;
        fixture.shell.tab = crate::shell::Tab::Profiles;
        fixture.hosts.selected.as_mut().unwrap().behavior = ConnectionBehavior::ChooseSession;
        match state {
            "hosts-before-discovery" => {
                fixture.hosts.clear_review();
                fixture.hosts.hosts.clear();
                fixture.hosts.discovered = false;
            }
            "hosts-found" => {
                fixture.hosts.clear_review();
            }
            "hosts-unavailable" => {
                fixture.hosts.error = Some("Host timed out. Try again.".into());
            }
            "hosts-new" => {
                fixture.hosts.quick_new = true;
                fixture.hosts.quick_name = "release".into();
            }
            "hosts-profile" => {
                fixture.hosts.begin_profile_edit();
                fixture.hosts.profile_name = "Daily development".into();
                fixture.hosts.session_name = "work".into();
                fixture.hosts.create_if_missing = true;
            }
            _ => {}
        }
        *self = fixture;
    }
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
        profiles: Vec::new(),
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

#[test]
fn mobile_host_shared_profile_operation_keeps_exact_revision_and_requires_capabilities() {
    let mut hosts = HostUi::default();
    assert!(hosts.shared_profile_operation(0).is_err());
    let mut inventory = client_inventory(false);
    let logical = LogicalSessionId::new();
    let revision = festerm_host_protocol::Epoch::new(4).unwrap();
    inventory
        .profiles
        .push(festerm_host_protocol::ProfileSummary {
            logical: logical.clone(),
            revision,
            name: "Build shell".into(),
        });
    inventory.capabilities.profiles = true;
    inventory.capabilities.ensure_profile = true;
    hosts.inventory = Some(inventory);
    assert!(matches!(hosts.shared_profile_operation(0).unwrap(),
        host_client::Operation::EnsureProfile { logical: selected, revision: selected_revision, .. }
        if selected == logical && selected_revision == revision));
    assert!(hosts.shared_profile_operation(1).is_err());
    hosts
        .inventory
        .as_mut()
        .unwrap()
        .capabilities
        .ensure_profile = false;
    assert!(hosts.shared_profile_operation(0).is_err());
    assert!(
        !hosts.quick_creation_allowed(),
        "profile permission must not imply generic shell creation"
    );
}

#[test]
fn mobile_host_profile_can_explicitly_change_saved_host_without_network_or_losing_draft() {
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    festerm_mobile_tunnels::testing::seed_account(&store, 7, "sample", false);
    let mut original = client_recipe();
    original.session_name = Some("build".into());
    let mut other = client_recipe();
    other.name = "Other host".into();
    other.host_hint = Some(HostHint {
        locator: "other.test".into(),
        name: "Other host".into(),
    });
    for recipe in [&original, &other] {
        store
            .lock()
            .unwrap()
            .apply_host_recipe_command(MetadataCommand::Save(recipe.clone()))
            .unwrap();
    }
    let mut app = app().with_profiles(profiles);
    assert!(app.select_host_profile(original.id.as_str(), true));
    app.hosts.profile_name = "Edited shortcut".into();
    app.hosts.session_name = "release".into();
    app.hosts.create_if_missing = true;
    let ctx = egui::Context::default();
    app.host_action(&ctx, Action::ChooseProfileHost).unwrap();
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
    app.host_action(&ctx, Action::SelectSavedProfileHost(1))
        .unwrap();
    assert_eq!(app.hosts.profile_name, "Edited shortcut");
    assert_eq!(app.hosts.session_name, "release");
    assert_eq!(app.hosts.selected.as_ref().unwrap().host, other.host);
    assert_ne!(
        app.hosts.selected.as_ref().unwrap().logical_session,
        original.logical_session
    );
    assert!(app.hosts.receipt.is_none() && !app.hosts.enrolled);
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
    app.host_action(&ctx, Action::SaveProfile).unwrap();
    let saved = store
        .lock()
        .unwrap()
        .host_recipes()
        .get(&original.id)
        .unwrap()
        .clone();
    assert_eq!(saved.id, original.id);
    assert_eq!(saved.host, other.host);
    assert_eq!(saved.name, "Edited shortcut");
    assert_eq!(saved.session_name(), "release");
    assert!(matches!(saved.behavior, ConnectionBehavior::ResumeOrCreate));
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
}

#[test]
fn mobile_host_profile_host_selection_cancel_preserves_original_target_and_draft() {
    let profiles = crate::profiles::testing::profiles();
    let original = client_recipe();
    profiles
        .store_handle()
        .lock()
        .unwrap()
        .apply_host_recipe_command(MetadataCommand::Save(original.clone()))
        .unwrap();
    let mut app = app().with_profiles(profiles);
    assert!(app.select_host_profile(original.id.as_str(), true));
    app.hosts.profile_name = "Unsaved name".into();
    app.hosts.session_name = "Unsaved session".into();
    let ctx = egui::Context::default();
    app.host_action(&ctx, Action::ChooseProfileHost).unwrap();
    app.host_action(&ctx, Action::CancelProfileHost).unwrap();
    assert_eq!(app.hosts.selected.as_ref().unwrap().host, original.host);
    assert_eq!(app.hosts.profile_name, "Unsaved name");
    assert_eq!(app.hosts.session_name, "Unsaved session");
    assert!(app.hosts.editing && app.host_recipe_current());
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
}

#[cfg(unix)]
#[test]
fn mobile_host_shared_profiles_are_visible_without_generic_creation_or_render_effects() {
    use egui_kittest::kittest::{By, Queryable};
    let (_root, mut app, _account, _store) = recovery_fixture();
    let mut inventory = client_inventory(false);
    inventory.capabilities.profiles = true;
    inventory.capabilities.ensure_profile = true;
    inventory
        .profiles
        .push(festerm_host_protocol::ProfileSummary {
            logical: LogicalSessionId::new(),
            revision: festerm_host_protocol::Epoch::new(1).unwrap(),
            name: "Shared build".into(),
        });
    app.hosts.inventory = Some(inventory);
    app.hosts.locator = "fixture.test".into();
    app.hosts.selected.as_mut().unwrap().behavior = ConnectionBehavior::ChooseSession;
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(390.0, 844.0))
        .build_ui_state(|ui, app: &mut MobileApp| app.show_hosts(ui.ctx()), app);
    harness.run_steps(3);
    harness.get(By::new().label("Connect · Shared build"));
    assert!(harness.query(By::new().label("+ New session")).is_none());
    assert!(harness.state().hosts.pending.is_none() && harness.state().hosts.worker.is_none());
}

#[test]
fn mobile_host_explicit_session_name_edit_binds_unique_authorized_name_or_new_identity() {
    let original = client_recipe();
    let previous = original.logical_session.clone();
    let mut hosts = HostUi {
        selected: Some(original),
        ..Default::default()
    };
    hosts.session_name = "release".into();
    hosts.reconcile_profile_session_name();
    let next = hosts.selected.as_ref().unwrap().logical_session.clone();
    assert_ne!(next, previous);
    hosts.reconcile_profile_session_name();
    assert_eq!(
        hosts.selected.as_ref().unwrap().logical_session,
        next,
        "rendering/validation must not allocate another identity"
    );
    let listed = LogicalSessionId::new();
    let mut inventory = client_inventory(false);
    inventory
        .sessions
        .push(festerm_host_protocol::SessionSummary {
            target: host_client::Target {
                logical: listed.clone(),
                generation: GenerationId::new(1).unwrap(),
            },
            name: "build".into(),
            lifetime: festerm_host_protocol::Lifetime::Persistent {},
            attached: false,
        });
    hosts.inventory = Some(inventory);
    hosts.session_name = "build".into();
    hosts.reconcile_profile_session_name();
    assert_eq!(hosts.selected.as_ref().unwrap().logical_session, listed);
    assert_eq!(hosts.existing_session, Some(0));
}

#[test]
fn mobile_host_reverting_session_name_restores_saved_identity_and_creation_off_needs_a_target() {
    let recipe = client_recipe();
    let logical = recipe.logical_session.clone();
    let name = recipe.session_name().to_owned();
    let mut hosts = HostUi {
        selected: Some(recipe),
        saved: true,
        ..Default::default()
    };
    hosts.begin_profile_edit();
    hosts.create_if_missing = true;
    hosts.session_name = "another shell".into();
    hosts.reconcile_profile_session_name();
    assert_ne!(hosts.selected.as_ref().unwrap().logical_session, logical);
    hosts.create_if_missing = false;
    assert!(
        hosts.profile_draft().is_err(),
        "a new logical identity is not an existing session"
    );
    hosts.session_name = name;
    hosts.reconcile_profile_session_name();
    assert_eq!(hosts.selected.as_ref().unwrap().logical_session, logical);
    hosts.profile_name = "Renamed shortcut".into();
    let draft = hosts.profile_draft().unwrap();
    assert_eq!(draft.logical_session, logical);
    assert!(matches!(draft.behavior, ConnectionBehavior::ResumeOnly {}));
}

#[test]
fn mobile_host_cancel_host_selection_restores_original_account_and_discards_foreign_results() {
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    let first = festerm_mobile_tunnels::testing::seed_account(&store, 7, "first", false);
    let second = festerm_mobile_tunnels::testing::seed_account(&store, 8, "second", false);
    let original = client_recipe();
    store
        .lock()
        .unwrap()
        .apply_host_recipe_command(MetadataCommand::Save(original.clone()))
        .unwrap();
    let mut app = app().with_profiles(profiles);
    app.select_host_profile(original.id.as_str(), true);
    let ctx = egui::Context::default();
    app.host_action(&ctx, Action::ChooseProfileHost).unwrap();
    app.hosts.account = second;
    app.hosts.discovered = true;
    app.hosts.hosts.push(TunnelSummary {
        locator: "foreign.test".into(),
        name: None,
        description: None,
        active: false,
    });
    app.host_action(&ctx, Action::CancelProfileHost).unwrap();
    assert_eq!(app.hosts.account, first);
    assert!(app.hosts.hosts.is_empty() && !app.hosts.discovered);
    assert_eq!(app.hosts.selected.as_ref().unwrap().host, original.host);
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
}

#[test]
fn mobile_host_legacy_long_implicit_name_stays_editable_without_allocating_a_session() {
    let mut original = client_recipe();
    original.name = "x".repeat(200);
    let logical = original.logical_session.clone();
    let mut hosts = HostUi {
        selected: Some(original),
        saved: true,
        ..Default::default()
    };
    hosts.begin_profile_edit();
    hosts.profile_name = "Short local label".into();
    hosts.create_if_missing = false;
    let draft = hosts.profile_draft().unwrap();
    assert_eq!(draft.logical_session, logical);
    assert_eq!(draft.session_name, None);
    assert!(matches!(draft.behavior, ConnectionBehavior::ResumeOnly {}));
}

#[test]
fn mobile_host_known_identity_reuse_refuses_ambiguous_or_foreign_account_locators() {
    let profiles = crate::profiles::testing::profiles();
    let store = profiles.store_handle();
    let account = festerm_mobile_tunnels::testing::seed_account(&store, 7, "sample", false);
    let mut first = client_recipe();
    first.host_hint = Some(HostHint {
        locator: "same.test".into(),
        name: "First".into(),
    });
    let mut second = client_recipe();
    second.host_hint = first.host_hint.clone();
    for recipe in [&first, &second] {
        store
            .lock()
            .unwrap()
            .apply_host_recipe_command(MetadataCommand::Save(recipe.clone()))
            .unwrap();
    }
    let mut app = app().with_profiles(profiles);
    app.hosts.account = account;
    assert!(app.known_host_recipe("same.test").is_none());
    store
        .lock()
        .unwrap()
        .apply_host_recipe_command(MetadataCommand::Delete(second.id))
        .unwrap();
    assert_eq!(app.known_host_recipe("same.test").unwrap().host, first.host);
    app.hosts.account = "foreign".into();
    assert!(app.known_host_recipe("same.test").is_none());
    assert!(app.hosts.pending.is_none() && app.hosts.worker.is_none());
}
