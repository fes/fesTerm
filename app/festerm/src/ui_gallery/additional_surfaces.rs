use super::*;

pub(super) fn scenarios() -> Vec<Scenario> {
    let mut scenes = Vec::new();
    macro_rules! scene {
        ($id:literal, $title:literal, $route:literal, $capture:expr) => {
            scenes.push(Scenario {
                id: $id,
                section: "workflow-surfaces",
                title: $title,
                caption: $route,
                capture: $capture,
            });
        };
    }
    scene!(
        "profiles-new-menu",
        "Profiles: New Profile menu",
        "Profiles > New Profile. Exit: choose a transport or Escape.",
        || profile(ProfileScene::NewMenu)
    );
    scene!(
        "profiles-local-editor",
        "Profiles: Local profile and persistence",
        "Profiles > New Profile > Local. Exit: Save/Cancel; capture never launches a shell.",
        || profile(ProfileScene::Local)
    );
    scene!(
        "profiles-row-menu",
        "Profiles: profile actions",
        "Profiles > saved SSH row > More actions. Exit: choose an action or Escape.",
        || profile(ProfileScene::RowMenu)
    );
    scene!("profiles-delete-confirmation", "Profiles: delete confirmation", "Profiles > saved SSH row > More actions > Delete. The current surface exposes only Delete, not a local Cancel. Reopen Profiles through the global action to abandon it; capture never deletes.", || profile(ProfileScene::Delete));
    scene!("profiles-duplicate-editor", "Profiles: duplicate draft", "Profiles > saved SSH row > More actions > Duplicate. Exit: Save new profile or Cancel; credentials are not copied.", || profile(ProfileScene::Duplicate));
    scene!(
        "profiles-search-empty",
        "Profiles: search without results",
        "Profiles > Search profiles > unmatched query. Exit: clear the query.",
        || profile(ProfileScene::NoMatch)
    );
    scene!("workspace-ssh-restored", "Workspace: SSH requires fresh authentication", "Restore metadata-only SSH workspace tab. Exit: enter fresh credentials and Connect, or Back/Cancel. No transport is restored.", || restored(false));
    scene!("workspace-sftp-restored", "Workspace: SFTP requires fresh authentication", "Restore metadata-only terminal SFTP workspace tab. Exit: fresh authentication or Cancel. No prior trust or secret is restored.", || restored(true));
    scene!(
        "markdown-viewer-menu",
        "Markdown: viewer overflow",
        "Markdown viewer > Viewer menu. Exit: Escape, Reload or Close viewer.",
        || markdown_menu(false)
    );
    scene!(
        "markdown-path-menu",
        "Markdown: path context menu",
        "Markdown viewer > right-click origin path. Exit: Escape; copy requires deliberate action.",
        || markdown_menu(true)
    );
    scene!("settings-all-cards", "Settings: complete card stack", "More actions > Settings. Tall headless viewport exposes the full implemented card stack; scroll in a normal window. Exit: select another chip or close Settings.", settings);
    scene!("chrome-rename-session", "Chrome: inline session rename", "Right-click a renamable chip > Rename session (also available by double-click). Isolated production chrome, not native focus-integration evidence. Exit: Enter commits the local alias; Escape cancels. Capture commits nothing.", chrome_rename);
    scene!("chrome-single-row-overflow", "Chrome: single scrolling row", "Settings > session-chip layout > Single scrolling row > open enough tabs to overflow. Exit: scroll horizontally or restore wrapping.", || {
        let mut harness = chip_gallery_harness(egui::vec2(640.0, 160.0), true, None, ChipLayout::SingleRowScroll);
        finish(&mut harness)
    });
    scene!("about-update-checking", "About: checking for updates", "About > Check for updates > check pending. Exit: response or close About; inert synthetic status.", || about_update(crate::updates::UpdateStatus::Checking));
    scene!(
        "about-update-current",
        "About: already current",
        "About > Check for updates > no newer version. Exit: close About.",
        || about_update(crate::updates::UpdateStatus::Current)
    );
    scene!("about-update-available", "About: update available", "About > successful update check > review release notes. Exit: close or deliberate Download; no download is started.", || about_update(crate::updates::UpdateStatus::Available(update_summary())));
    scene!("about-update-refresh-download", "About: refreshing download metadata", "About > Download > refresh signed update metadata. Exit: result/refusal or close; no network worker exists.", || about_update(crate::updates::UpdateStatus::Refreshing(update_summary(), crate::updates::RefreshPurpose::Download)));
    scene!("about-update-refresh-install", "About: refreshing install metadata", "About > Install > revalidate update metadata. Exit: result/refusal or close; no installer exists.", || about_update(crate::updates::UpdateStatus::Refreshing(update_summary(), crate::updates::RefreshPurpose::Install)));
    scene!("about-update-downloading", "About: downloading update", "About > Download > download pending. Exit: completion/failure; closing About does not imply cancellation. Inert status only.", || about_update(crate::updates::UpdateStatus::Downloading(update_summary())));
    scene!("about-update-installing", "About: installing update", "About > deliberate Install > installation pending. Exit: completion/failure. Inert presentation, no installer or live process.", || about_update(crate::updates::UpdateStatus::Installing(update_summary())));
    scene!("about-update-check-failed", "About: failed check with retry", "About > update check fails validation. Exit: Retry check or close; no unverified download is installed.", || about_update(crate::updates::UpdateStatus::Failed { message: "Synthetic update verification failed. Nothing was installed.", retry_check: true }));
    scene!("about-update-install-failed", "About: failed update operation", "About > update operation fails without a safe check retry. Exit: close and inspect the explicit error.", || about_update(crate::updates::UpdateStatus::Failed { message: "Synthetic update operation failed. Existing installation was retained.", retry_check: false }));
    scene!(
        "palette-no-match",
        "Palette: no results",
        "Open Command palette > enter an unmatched query. Exit: clear the query or Escape.",
        || palette(false)
    );
    scene!("palette-category-popup", "Palette: narrow category choices", "Narrow Command palette > All categories. Wide layout uses category chips instead. Exit: choose a category or Escape; Escape does not also dismiss the palette.", || palette(true));
    scene!(
        "markdown-find",
        "Markdown: Find matches",
        "Read-only Markdown viewer > Find > query. Exit: Close Find/Escape.",
        || markdown_find(false)
    );
    scene!(
        "markdown-find-empty",
        "Markdown: Find without matches",
        "Markdown viewer > Find > unmatched query. Exit: change query or Close Find.",
        || markdown_find(true)
    );
    scene!("markdown-reload-refusal", "Markdown: remote reload unavailable", "Remote Markdown viewer > Reload. The embedded production footer exposes the refusal/stale state normally owned by the app status bar. Exit: close and reopen from SFTP to refresh; no new transport is guessed.", || render_markdown(900.0, 820.0, |tab| {
        tab.reload();
        tab.set_status_bar_visible(false);
        assert_eq!(tab.status_bar_label(), "Reloading isn't supported for this file yet");
    }, |_| {}));
    scene!(
        "markdown-copy-rich-menu",
        "Markdown: rich selection copy menu",
        "Markdown Preview > select a heading > right-click. Exit: Escape; capture never copies.",
        || markdown_copy(false)
    );
    scene!("markdown-copy-source-menu", "Markdown: source-aware selection menu", "Markdown Preview > select a heading > Shift + right-click. Exit: Escape; Markdown source is an explicit separate choice.", || markdown_copy(true));
    scene!("inspector-local-diagnostics", "Inspector: local diagnostics", "Local session > Session inspector > Diagnostics. Exit: collapse diagnostics or close inspector.", || inspector_variant(0));
    scene!("inspector-serial-diagnostics", "Inspector: serial line details", "Serial session > Session inspector > Diagnostics. Exit: close; all device identities are synthetic.", || inspector_variant(1));
    scene!("inspector-failed-diagnostics", "Inspector: disconnected recovery and trace", "Disconnected SSH > Session inspector > Diagnostics. Exit: explicit Reconnect or Close; trace is synthetic routing metadata.", || inspector_variant(2));
    scenes.extend(markdown_surface_scenes().into_iter().map(|scene| Scenario {
        id: scene.id,
        section: "workflow-surfaces",
        title: scene.title,
        caption: scene.caption,
        capture: scene.capture,
    }));
    scenes
}

#[derive(Clone, Copy)]
enum ProfileScene {
    NewMenu,
    Local,
    RowMenu,
    Delete,
    Duplicate,
    NoMatch,
}

fn profile(scene: ProfileScene) -> image::RgbaImage {
    let configuration = synthetic_configuration();
    let tab = AppState::for_test().active();
    let mut harness = gallery_harness(
        egui::vec2(1000.0, 1300.0),
        move |ui, ()| {
            let _ = screens::show_profiles(
                ui,
                tab,
                &configuration,
                None,
                None::<NewProfileKind>,
                festerm_config::PersistenceProviderKind::FestermSessiond,
            );
        },
        (),
    );
    harness.run();
    match scene {
        ProfileScene::NewMenu | ProfileScene::Local => {
            harness.get_by_label("New Profile").click();
            harness.run();
            if matches!(scene, ProfileScene::Local) {
                harness
                    .get_by_role_and_label(accesskit::Role::Button, "Local")
                    .click();
                harness.run();
                assert!(harness.query_by_label("New Local Profile").is_some());
            } else {
                assert!(harness
                    .query_by_role_and_label(accesskit::Role::Button, "Local")
                    .is_some());
            }
        }
        ProfileScene::NoMatch => {
            harness.get_by_label("Search profiles…").focus();
            harness.event(egui::Event::Text("no-synthetic-profile-matches".into()));
            harness.run();
            assert!(harness
                .query_by_label("No profiles match this search.")
                .is_some());
        }
        _ => {
            harness
                .get_by_label("More actions for Staging web-1")
                .click();
            harness.run();
            let action = match scene {
                ProfileScene::Delete => Some("Delete"),
                ProfileScene::Duplicate => Some("Duplicate"),
                _ => None,
            };
            if let Some(action) = action {
                harness
                    .get_by_role_and_label(accesskit::Role::Button, action)
                    .click();
                harness.run();
            } else {
                assert!(harness.query_by_label("Open SFTP").is_some());
            }
        }
    }
    finish(&mut harness)
}

fn restored(sftp: bool) -> image::RgbaImage {
    let profile = Profile::ssh(
        "Restored build server",
        "build.example.net",
        22,
        "builder",
        "xterm-256color",
        100,
        30,
    )
    .unwrap();
    let Profile::Ssh(profile) = profile else {
        unreachable!()
    };
    let tab = AppState::for_test().active();
    let mut harness = gallery_harness(
        egui::vec2(900.0, 1000.0),
        move |ui, ()| {
            let command = if sftp {
                screens::show_sftp_authentication_required(ui, tab, &profile, false)
            } else {
                screens::show_ssh_authentication_required(ui, tab, &profile, false)
            };
            assert!(command.is_none(), "no connection during capture");
        },
        (),
    );
    finish(&mut harness)
}

fn markdown_menu(path: bool) -> image::RgbaImage {
    render_markdown(
        900.0,
        820.0,
        |_| {},
        |harness| {
            if path {
                let pos = harness
                    .get_by_label("~/projects/nimbus-relay/NOTES.md")
                    .rect()
                    .center();
                right_click(harness, pos);
            } else {
                harness.get_by_label("Viewer menu").click();
                harness.run();
            }
            assert!(harness.query_by_label("Copy path").is_some());
        },
    )
}

fn right_click<State>(harness: &mut Harness<'_, State>, pos: egui::Pos2) {
    harness.event(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos,
            pressed,
            button: egui::PointerButton::Secondary,
            modifiers: egui::Modifiers::NONE,
        });
    }
    harness.run();
}

fn settings() -> image::RgbaImage {
    let model = synthetic_settings_view_model();
    let mut harness = gallery_harness(
        egui::vec2(1000.0, 8000.0),
        move |ui, ()| {
            let _ = screens::show_settings(ui, model.clone());
        },
        (),
    );
    harness.run();
    assert!(
        harness.get_by_label("KEYBOARD BINDINGS").rect().bottom() < 7900.0,
        "last settings card must be in the captured viewport"
    );
    finish(&mut harness)
}

fn palette(category: bool) -> image::RgbaImage {
    let mut harness = palette_gallery_harness(category);
    if category {
        harness.get_by_role(accesskit::Role::ComboBox).click();
    } else {
        harness
            .get_by_role(accesskit::Role::TextInput)
            .type_text("no-synthetic-command-matches");
    }

    harness.run();
    finish(&mut harness)
}

fn update_summary() -> crate::updates::UpdateSummary {
    crate::updates::UpdateSummary {
            version: "0.18.0".into(),
            notes: Some("Synthetic review release.\n\n- Reliability improvements.\n- No update is downloaded by this fixture.".into()),
        }
}

fn about_update(status: crate::updates::UpdateStatus) -> image::RgbaImage {
    let (mut app, _, _) = crate::app::FesTermApp::for_test_with_fake_ssh_session([]);
    app.open_about_for_gallery(
        false,
        crate::updates::UpdateController::status_for_gallery(status),
        &egui::Context::default(),
    );
    let mut icon_installed = false;
    let mut harness = gallery_harness(
        egui::vec2(1000.0, 900.0),
        move |ui, app: &mut crate::app::FesTermApp| {
            if !icon_installed {
                app.install_about_icon_for_gallery(ui.ctx());
                icon_installed = true;
            }
            app.ui_content(ui);
        },
        app,
    );
    harness.run_steps(4);
    harness.get_by_label("About fesTerm");
    harness.remove_cursor();
    harness.step();
    harness.render().expect("render actual About update state")
}

fn chrome_rename() -> image::RgbaImage {
    let mut harness = chip_gallery_harness(egui::vec2(1000.0, 160.0), true, None, ChipLayout::Wrap);
    harness.run();
    let pos = harness.get_by_label("Staging web-1").rect().center();
    right_click(&mut harness, pos);
    harness.get_by_label("Rename session").click();
    harness.run_steps(2);
    harness.get_by_role(accesskit::Role::TextInput);
    finish(&mut harness)
}
fn markdown_find(empty: bool) -> image::RgbaImage {
    render_markdown(
        900.0,
        820.0,
        |tab| {
            tab.open_find();
            let count =
                tab.set_find_query_for_test(if empty { "unmatched-fixture" } else { "relay" });
            assert_eq!(count == 0, empty);
        },
        |_| {},
    )
}

fn markdown_copy(source: bool) -> image::RgbaImage {
    render_markdown(
        900.0,
        820.0,
        |_| {},
        |harness| {
            fn heading_rect(shape: &egui::Shape) -> Option<egui::Rect> {
                match shape {
                    egui::Shape::Text(text)
                        if text.galley.job.text == "Nimbus Relay — Project Notes"
                            && text.galley.size().y > 22.0 =>
                    {
                        Some(text.visual_bounding_rect())
                    }
                    egui::Shape::Vec(shapes) => shapes.iter().find_map(heading_rect),
                    _ => None,
                }
            }
            let rect = harness
                .output()
                .shapes
                .iter()
                .find_map(|shape| heading_rect(&shape.shape))
                .expect("rendered Markdown title geometry");
            let from = egui::pos2(rect.left() + 2.0, rect.center().y);
            let to = egui::pos2(rect.right() - 2.0, rect.center().y);
            harness.event(egui::Event::PointerMoved(from));
            harness.event(egui::Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run();
            harness.event(egui::Event::PointerMoved(to));
            harness.run();
            harness.event(egui::Event::PointerButton {
                pos: to,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run();
            harness.event(egui::Event::ModifiersChanged(egui::Modifiers {
                shift: source,
                ..egui::Modifiers::NONE
            }));
            right_click(harness, to);
            assert!(harness.query_by_label("Copy rich text").is_some());
            if source {
                assert!(harness.query_by_label("Copy Markdown source").is_some());
            }
        },
    )
}

fn inspector_variant(kind: u8) -> image::RgbaImage {
    let (transport, label) = match kind {
        1 => (
            TransportFacts::Serial {
                device: "/dev/tty.fixture-review",
                baud_rate: 115200,
                data_bits: "8",
                parity: "None",
                stop_bits: "1",
                flow_control: "None",
            },
            "Serial session",
        ),
        2 => (
            TransportFacts::Ssh {
                username: "builder",
                host: "build.example.net",
                port: 22,
            },
            "SSH session",
        ),
        _ => (TransportFacts::Local, "Local shell"),
    };
    render_inspector_over_terminal(
        980.0, 1000.0, 100, 28, SSH_TRANSCRIPT,
        InspectorContent {
            subject_id: 50 + u64::from(kind), identity: "Review fixture",
            type_label: label, state: if kind == 2 { "Disconnected" } else { "Running" },
            state_message: (kind == 2).then_some("Synthetic connection lost. Reconnect starts a new connection without replaying pending input."),
            state_color: festerm_ui_egui::theme::STATUS_RUNNING,
            grid: Some("100 × 28"), terminal_title: None, profile: None, transport,
            trust_fingerprint: (kind == 2).then_some("SHA256:synthetic-review"),
            diagnostics: "queue_depth=0 last_outcome=Delivered",
            input_recording: kind == 2, input_report: "fixture event: key=ArrowLeft outcome=Delivered bytes=3",
            reconnect_available: kind == 2, open_sftp_available: false,
            persistent_session: None,
        }, true,
    )
}
