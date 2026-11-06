use super::*;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use festerm_config::PaneSide;

fn grouped_app() -> (
    FesTermApp,
    TabId,
    TabId,
    crate::session_controller::fake::FakeSshSession,
    crate::session_controller::fake::FakeSshSession,
) {
    let (mut app, first, first_transport) = FesTermApp::for_test_with_fake_ssh_session([]);
    let context = egui::Context::default();
    app.state.dispatch(AppCommand::OpenLauncher, &context);
    let second_transport = crate::session_controller::fake::FakeSshSession::new([]);
    let second = app.state.replace_active_with_test_ssh_session(
        second_transport.clone(),
        "second",
        "second.example.test",
        22,
    );
    app.state
        .pane_geometry(egui::vec2(1600.0, 960.0), egui::vec2(520.0, 240.0));
    app.state.dispatch(
        AppCommand::CombinePanes {
            source: second,
            target: first,
            side: PaneSide::Right,
            expected_revision: app.state.pane_revision(),
        },
        &context,
    );
    (app, first, second, first_transport, second_transport)
}

#[test]
fn panes_disabled_hover_still_supports_explicit_rearrangement_without_resizing() {
    let (mut app, first, second, first_transport, second_transport) = grouped_app();
    let context = egui::Context::default();
    app.state.dispatch(AppCommand::TogglePaneHandles, &context);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1600.0, 960.0))
        .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
    harness.run();
    let before = harness.state().pane_rects.clone();
    let grids = [first, second].map(|view| {
        harness
            .state()
            .state
            .session_tab(view)
            .unwrap()
            .view
            .diagnostics()
            .grid_rect
    });
    let context = harness.ctx.clone();
    harness.state_mut().begin_pane_rearrange(&context);
    harness.run();
    assert_eq!(harness.state().pane_rects, before);
    for (view, grid) in [first, second].into_iter().zip(grids) {
        assert_eq!(
            harness
                .state()
                .state
                .session_tab(view)
                .unwrap()
                .view
                .diagnostics()
                .grid_rect,
            grid
        );
        let name = harness
            .state()
            .state
            .session_tab(view)
            .unwrap()
            .label
            .clone();
        harness.get_by_label(&format!("Move {name} pane"));
    }
    harness.input_mut().events.extend([
        egui::Event::Text("do-not-send".into()),
        egui::Event::Paste("do-not-paste".into()),
        egui::Event::Ime(egui::ImeEvent::Commit("do-not-commit".into())),
    ]);
    harness.step();
    assert!(first_transport.sent().is_empty());
    assert!(second_transport.sent().is_empty());
    harness.get_by_label("Done rearranging").click();
    harness.run();
    assert!(!harness.state().pane_controls.blocks_input());
    assert_eq!(harness.state().pane_rects, before);
}

#[test]
fn panes_first_handle_press_drains_prefix_and_quarantines_suffix() {
    for paste in [false, true] {
        let (app, first, second, first_transport, second_transport) = grouped_app();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1600.0, 960.0))
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        let rect = harness
            .state()
            .pane_rects
            .iter()
            .find(|(view, _)| *view == first)
            .unwrap()
            .1;
        let pos = egui::pos2(rect.center().x, rect.top() + 16.0);
        if paste {
            let (modifiers, key) = crate::keyboard::chord(if cfg!(target_os = "macos") {
                "Primary+V"
            } else {
                "Ctrl+Shift+V"
            })
            .unwrap();
            harness.input_mut().events.extend([
                egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
                egui::Event::Paste("prefix".into()),
            ]);
        } else {
            harness
                .input_mut()
                .events
                .push(egui::Event::Text("prefix".into()));
        }
        harness.input_mut().events.extend([
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::Text("suffix-must-not-send".into()),
        ]);
        harness.step();
        harness.run();
        assert_eq!(harness.state().state.active(), second);
        assert_eq!(second_transport.sent().concat(), b"prefix", "paste={paste}");
        assert!(first_transport.sent().is_empty());
        assert!(harness.state().pane_controls.blocks_input());
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
        let name = harness
            .state()
            .state
            .session_tab(first)
            .unwrap()
            .label
            .clone();
        harness.get_by_label(&format!("Moving: {name}"));
    }
}

#[test]
fn panes_deferred_complete_handle_click_preserves_release_and_menu_identity() {
    for paste in [false, true] {
        for menu in [false, true] {
            let (app, first, second, first_transport, second_transport) = grouped_app();
            let mut harness = Harness::builder()
                .with_size(egui::vec2(1600.0, 960.0))
                .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
            harness.run();
            let rect = harness
                .state()
                .pane_rects
                .iter()
                .find(|(view, _)| *view == first)
                .unwrap()
                .1;
            let pos = egui::pos2(
                rect.center().x + if menu { 94.0 } else { 0.0 },
                rect.top() + 16.0,
            );
            if paste {
                let (modifiers, key) = crate::keyboard::chord(if cfg!(target_os = "macos") {
                    "Primary+V"
                } else {
                    "Ctrl+Shift+V"
                })
                .unwrap();
                harness.input_mut().events.extend([
                    egui::Event::Key {
                        key,
                        physical_key: Some(key),
                        pressed: true,
                        repeat: false,
                        modifiers,
                    },
                    egui::Event::Paste("prefix".into()),
                ]);
            } else {
                harness
                    .input_mut()
                    .events
                    .push(egui::Event::Text("prefix".into()));
            }
            harness.input_mut().events.extend([
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::Text("suffix".into()),
            ]);
            harness.run();
            assert_eq!(harness.state().state.active(), second);
            assert_eq!(second_transport.sent().concat(), b"prefix");
            assert!(first_transport.sent().is_empty());
            if menu {
                harness.get_by_label("Detach to own tab");
            } else {
                let name = harness
                    .state()
                    .state
                    .session_tab(first)
                    .unwrap()
                    .label
                    .clone();
                harness.get_by_label(&format!("Moving: {name}"));
            }
            harness.input_mut().events.push(egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: Some(egui::Key::Escape),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run();
            assert!(!harness.state().pane_controls.blocks_input());
        }
    }
}

#[test]
fn panes_escape_cancels_native_payload_before_every_trailing_drop_action() {
    for stale in [false, true] {
        for destination in ["Left", "Swap", "Detach", "Close…"] {
            let (app, first, _, first_transport, second_transport) = grouped_app();
            let mut harness = Harness::builder()
                .with_size(egui::vec2(1600.0, 960.0))
                .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
            harness.run();
            let mut before = harness.state().state.pane_group(first).unwrap().clone();
            let mut revision = harness.state().state.pane_revision();
            let rect = harness
                .state()
                .pane_rects
                .iter()
                .find(|(view, _)| *view == first)
                .unwrap()
                .1;
            let pos = egui::pos2(rect.center().x, rect.top() + 16.0);
            harness.input_mut().events.extend([
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            harness.step();
            harness
                .input_mut()
                .events
                .push(egui::Event::PointerMoved(pos + egui::vec2(0.0, 150.0)));
            harness.run();
            let target = harness.get_by_label(destination).rect().center();
            if stale {
                let context = harness.ctx.clone();
                harness
                    .state_mut()
                    .state
                    .dispatch(AppCommand::EqualizePanes(first), &context);
                revision = harness.state().state.pane_revision();
                before = harness.state().state.pane_group(first).unwrap().clone();
                harness
                    .input_mut()
                    .events
                    .push(egui::Event::Text("stale-gesture-input".into()));
                harness.step();
                assert!(harness.state().pane_controls.blocks_input());
                harness.state_mut().overlays.transient_notice = None;
            } else {
                harness.input_mut().events.push(egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: Some(egui::Key::Escape),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            harness.run();
            harness.input_mut().events.extend([
                egui::Event::PointerMoved(target),
                egui::Event::PointerButton {
                    pos: target,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            harness.run();
            assert_eq!(
                harness.state().state.pane_group(first),
                Some(&before),
                "{destination}"
            );
            assert_eq!(harness.state().state.pane_revision(), revision);
            assert!(harness.state().overlays.pending_close.is_none());
            assert!(!harness.state().pane_controls.blocks_input());
            assert!(first_transport.sent().is_empty());
            assert!(second_transport.sent().is_empty());
        }
    }
}

#[test]
fn panes_native_external_chip_swap_and_owned_strip_detach_keep_live_input_owner() {
    let (mut app, first, second, first_transport, second_transport) = grouped_app();
    let context = egui::Context::default();
    app.state.dispatch(AppCommand::OpenLauncher, &context);
    let third_transport = crate::session_controller::fake::FakeSshSession::new([]);
    let third = app.state.replace_active_with_test_ssh_session(
        third_transport.clone(),
        "third",
        "third.example.test",
        22,
    );
    let generation = app
        .state
        .session_tab(third)
        .unwrap()
        .controller
        .lifecycle_generation();
    app.state.dispatch(AppCommand::ActivateTab(first), &context);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1600.0, 960.0))
        .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
    harness.run();
    let position = harness
        .state()
        .pane_rects
        .iter()
        .find(|(view, _)| *view == first)
        .unwrap()
        .1
        .center();
    egui::DragAndDrop::set_payload(&harness.ctx, ChipId(third.chip_id()));
    harness.input_mut().events.extend([
        egui::Event::PointerMoved(position),
        egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
    ]);
    harness.run();
    let name = harness
        .state()
        .state
        .session_tab(third)
        .unwrap()
        .label
        .clone();
    harness.get_by_label(&format!("Moving: {name}"));
    let swap = harness.get_by_label("Swap").rect().center();
    harness.input_mut().events.extend([
        egui::Event::PointerMoved(swap),
        egui::Event::PointerButton {
            pos: swap,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::Text("release-must-not-send".into()),
    ]);
    harness.run();
    assert_eq!(harness.state().state.active(), third);
    assert!(harness.state().state.pane_group(first).is_none());
    assert!(harness
        .state()
        .state
        .pane_group(third)
        .unwrap()
        .tree
        .views()
        .contains(&second));
    assert_eq!(
        harness
            .state()
            .state
            .session_tab(third)
            .unwrap()
            .controller
            .lifecycle_generation(),
        generation
    );
    harness
        .input_mut()
        .events
        .push(egui::Event::Text("post-swap".into()));
    harness.run();
    assert_eq!(third_transport.sent().concat(), b"post-swap");
    assert!(first_transport.sent().is_empty());
    assert!(second_transport.sent().is_empty());
    let rect = harness
        .state()
        .pane_rects
        .iter()
        .find(|(view, _)| *view == third)
        .unwrap()
        .1;
    let pos = egui::pos2(rect.center().x, rect.top() + 16.0);
    harness.input_mut().events.extend([
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
    ]);
    harness.step();
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos + egui::vec2(0.0, 150.0)));
    harness.run();
    let strip = chrome::pane_tab_strip_target(&harness.ctx).unwrap();
    assert!(strip.width() >= 100.0);
    let detach = strip.center();
    harness.input_mut().events.extend([
        egui::Event::PointerMoved(detach),
        egui::Event::PointerButton {
            pos: detach,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        },
    ]);
    harness.run();
    assert!(harness.state().state.pane_group(third).is_none());
    assert!(harness.state().state.pane_group(second).is_none());
    assert_eq!(
        harness
            .state()
            .state
            .session_tab(third)
            .unwrap()
            .controller
            .lifecycle_generation(),
        generation
    );
    assert_eq!(third_transport.sent().concat(), b"post-swap");
    assert!(first_transport.sent().is_empty());
    assert!(second_transport.sent().is_empty());
}

#[test]
fn panes_hover_reveal_does_not_capture_the_whole_top_strip() {
    let (app, first, _, first_transport, second_transport) = grouped_app();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1600.0, 960.0))
        .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
    harness.run();
    let rect = harness
        .state()
        .pane_rects
        .iter()
        .find(|(view, _)| *view == first)
        .unwrap()
        .1;
    let pos = rect.left_top() + egui::vec2(12.0, 14.0);
    harness.input_mut().events.extend([
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::Text("ordinary-input".into()),
    ]);
    harness.run();
    assert_eq!(harness.state().state.active(), first);
    assert_eq!(first_transport.sent().concat(), b"ordinary-input");
    assert!(second_transport.sent().is_empty());
    assert!(!harness.state().pane_controls.blocks_input());
}

#[test]
fn panes_native_close_drop_keeps_exact_source_confirmation_after_release() {
    let (mut app, first, second, first_transport, second_transport) = grouped_app();
    if !app.state.confirm_session_close() {
        app.state.dispatch(
            AppCommand::ToggleConfirmSessionClose,
            &egui::Context::default(),
        );
    }
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1600.0, 960.0))
        .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
    harness.run();
    let before = harness.state().state.pane_group(first).unwrap().clone();
    let rect = harness
        .state()
        .pane_rects
        .iter()
        .find(|(view, _)| *view == first)
        .unwrap()
        .1;
    let pos = egui::pos2(rect.center().x, rect.top() + 16.0);
    harness.input_mut().events.extend([
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
    ]);
    harness.step();
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos + egui::vec2(0.0, 150.0)));
    harness.run();
    let close = harness.get_by_label("Close…").rect().center();
    harness.input_mut().events.extend([
        egui::Event::PointerMoved(close),
        egui::Event::PointerButton {
            pos: close,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        },
    ]);
    harness.run();
    assert_eq!(
        harness.state().overlays.pending_close.as_ref().unwrap().tab,
        first
    );
    assert_eq!(harness.state().state.active(), second);
    assert_eq!(harness.state().state.pane_group(first), Some(&before));
    harness.get_by_label("Cancel").click();
    harness.run();
    assert!(harness.state().overlays.pending_close.is_none());
    assert_eq!(harness.state().state.pane_group(first), Some(&before));
    assert!(first_transport.sent().is_empty());
    assert!(second_transport.sent().is_empty());
}

#[test]
fn panes_rearrangement_blocks_focused_document_edits_and_dirty_close_uses_shared_guard() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pane.txt");
    std::fs::write(&path, "seed").unwrap();
    let (mut app, first, _, first_transport, second_transport) = grouped_app();
    let context = egui::Context::default();
    app.state
        .dispatch(AppCommand::OpenTextEditor { path }, &context);
    let editor = app.state.active();
    app.state.dispatch(
        AppCommand::CombinePanes {
            source: editor,
            target: first,
            side: PaneSide::Below,
            expected_revision: app.state.pane_revision(),
        },
        &context,
    );
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1800.0, 1100.0))
        .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
    harness.run();
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .focus();
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .type_text("before");
    harness.run();
    let document = harness.state().state.active_document().unwrap();
    let text = harness
        .state()
        .state
        .documents()
        .borrow()
        .get(document)
        .unwrap()
        .text()
        .text()
        .to_owned();
    assert!(text.contains("before"));
    let context = harness.ctx.clone();
    harness.state_mut().begin_pane_rearrange(&context);
    harness.run();
    harness.input_mut().events.extend([
        egui::Event::Text("must-not-edit".into()),
        egui::Event::Paste("must-not-paste".into()),
    ]);
    harness.run();
    assert_eq!(
        harness
            .state()
            .state
            .documents()
            .borrow()
            .get(document)
            .unwrap()
            .text()
            .text(),
        text
    );
    harness.get_by_label("Move pane.txt pane").click();
    harness.run();
    harness.get_by_label("Close…").click();
    harness.run();
    assert!(harness.state().overlays.pending_document_close.is_some());
    assert!(harness
        .state()
        .state
        .tabs()
        .iter()
        .any(|tab| tab.id == editor));
    assert!(first_transport.sent().is_empty());
    assert!(second_transport.sent().is_empty());
}

#[test]
fn panes_rearrangement_escape_layers_and_stale_movement_are_mutation_free() {
    let (app, first, _, _, _) = grouped_app();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1600.0, 960.0))
        .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
    harness.run();
    let context = harness.ctx.clone();
    harness.state_mut().begin_pane_rearrange(&context);
    harness.run();
    let name = harness
        .state()
        .state
        .session_tab(first)
        .unwrap()
        .label
        .clone();
    harness.get_by_label(&format!("Move {name} pane")).click();
    harness.run();
    let revision = harness.state().state.pane_revision();
    harness
        .state_mut()
        .state
        .dispatch(AppCommand::EqualizePanes(first), &context);
    assert_ne!(harness.state().state.pane_revision(), revision);
    let stable = harness.state().state.pane_group(first).unwrap().clone();
    harness.step();
    assert!(harness
        .state()
        .overlays
        .transient_notice
        .as_ref()
        .is_some_and(|(notice, _)| notice.contains("layout changed")));
    harness.state_mut().overlays.transient_notice = None;
    harness.run();
    assert_eq!(harness.state().state.pane_group(first), Some(&stable));
    harness.get_by_label(&format!("Move {name} pane")).click();
    harness.run();
    for (index, blocked) in [true, false].into_iter().enumerate() {
        harness.input_mut().events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
        assert_eq!(
            harness.state().pane_controls.blocks_input(),
            blocked,
            "escape layer {index}"
        );
        assert_eq!(harness.state().state.pane_group(first), Some(&stable));
    }
}

#[test]
fn panes_preserve_terminal_padding_and_resize_only_the_inset_content() {
    let (mut app, first, second, _, _) = grouped_app();
    let context = egui::Context::default();
    app.state
        .dispatch(AppCommand::SeparatePaneGroup(second), &context);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1600.0, 960.0))
        .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
    harness.run();
    let pane = harness.state().pane_rects[0].1;
    let grid = harness
        .state()
        .state
        .session_tab(second)
        .unwrap()
        .view
        .diagnostics()
        .grid_rect
        .unwrap();
    let padding = grid.min - pane.min;
    assert!(padding.x > 0.0 && padding.y > 0.0);

    for side in [PaneSide::Right, PaneSide::Below] {
        let context = harness.ctx.clone();
        let revision = harness.state().state.pane_revision();
        harness.state_mut().state.dispatch(
            AppCommand::CombinePanes {
                source: first,
                target: second,
                side,
                expected_revision: revision,
            },
            &context,
        );
        harness.run();
        assert_eq!(harness.state().pane_rects.len(), 2);
        for (view, pane) in &harness.state().pane_rects {
            let session = harness.state().state.session_tab(*view).unwrap();
            let diagnostics = session.view.diagnostics();
            let grid = diagnostics.grid_rect.unwrap();
            assert_eq!(
                grid.min - pane.min,
                padding,
                "focused and passive panes must retain the single-view inset"
            );
            let dimensions = session.terminal.dimensions();
            assert_eq!(diagnostics.calculated_dimensions, Some(dimensions));
            let cell =
                grid.size() / egui::vec2(dimensions.columns() as f32, dimensions.rows() as f32);
            let remaining = pane.size() - padding * 2.0 - grid.size();
            assert!(remaining.x >= -0.01 && remaining.x < cell.x + 0.01);
            assert!(remaining.y >= -0.01 && remaining.y < cell.y + 0.01);
        }
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::SeparatePaneGroup(second), &context);
        harness.run();
    }
}

#[test]
fn panes_pinned_details_and_layout_dialog_block_terminal_input() {
    let (app, _, second, first_transport, second_transport) = grouped_app();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1600.0, 960.0))
        .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
    harness.run();
    harness
        .get_by_label("Pane details, 2 panes, 0 unread")
        .click();
    harness.run();
    assert!(harness.state().pane_details_open);
    harness
        .input_mut()
        .events
        .push(egui::Event::Text("must-not-send".into()));
    harness.step();
    assert!(first_transport.sent().is_empty());
    assert!(second_transport.sent().is_empty());
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: Some(egui::Key::Escape),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.state_mut().pane_layout_menu = Some(second);
    harness.run();
    harness
        .input_mut()
        .events
        .push(egui::Event::Text("also-must-not-send".into()));
    harness.step();
    assert!(first_transport.sent().is_empty());
    assert!(second_transport.sent().is_empty());
}

#[test]
fn panes_retire_ime_composition_before_focus_handoff_and_reject_delayed_commit() {
    for cleared_preedit in [false, true] {
        let (app, first, _, first_transport, second_transport) = grouped_app();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1600.0, 960.0))
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        harness
            .input_mut()
            .events
            .push(egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "private-composition".into(),
                active_range_chars: None,
            }));
        harness.step();
        if cleared_preedit {
            harness
                .input_mut()
                .events
                .push(egui::Event::Ime(egui::ImeEvent::Preedit {
                    text: String::new(),
                    active_range_chars: None,
                }));
            harness.step();
        }
        let position = harness
            .state()
            .pane_rects
            .iter()
            .find(|(view, _)| *view == first)
            .unwrap()
            .1
            .center();
        harness.input_mut().events.extend([
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        harness.step();
        assert_eq!(harness.state().state.active(), first);
        harness.input_mut().events.extend([
            egui::Event::Ime(egui::ImeEvent::Preedit {
                text: String::new(),
                active_range_chars: None,
            }),
            egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "stale-preedit".into(),
                active_range_chars: None,
            }),
            egui::Event::Ime(egui::ImeEvent::Commit("private-composition".into())),
        ]);
        harness.step();
        assert!(first_transport.sent().is_empty());
        assert!(second_transport.sent().is_empty());
        harness.input_mut().events.extend([
            egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "fresh".into(),
                active_range_chars: None,
            }),
            egui::Event::Ime(egui::ImeEvent::Commit("fresh".into())),
        ]);
        harness.step();
        assert_eq!(first_transport.sent().concat(), b"fresh");
        assert!(second_transport.sent().is_empty());
    }
}

#[test]
fn panes_retired_ime_without_old_commit_requires_reentry_but_not_ordinary_input() {
    let (app, first, _, first_transport, second_transport) = grouped_app();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1600.0, 960.0))
        .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
    harness.run();
    harness
        .input_mut()
        .events
        .push(egui::Event::Ime(egui::ImeEvent::Preedit {
            text: "abandoned".into(),
            active_range_chars: None,
        }));
    harness.step();
    let position = harness
        .state()
        .pane_rects
        .iter()
        .find(|(view, _)| *view == first)
        .unwrap()
        .1
        .center();
    harness.input_mut().events.extend([
        egui::Event::PointerMoved(position),
        egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
    ]);
    harness.step();
    harness
        .input_mut()
        .events
        .push(egui::Event::Text("ordinary".into()));
    harness.step();
    assert_eq!(first_transport.sent().concat(), b"ordinary");
    for (text, expected) in [
        ("must-reenter", b"ordinary".as_slice()),
        ("fresh", b"ordinaryfresh".as_slice()),
    ] {
        harness.input_mut().events.extend([
            egui::Event::Ime(egui::ImeEvent::Preedit {
                text: text.into(),
                active_range_chars: None,
            }),
            egui::Event::Ime(egui::ImeEvent::Commit(text.into())),
        ]);
        harness.step();
        assert_eq!(first_transport.sent().concat(), expected);
    }
    assert!(second_transport.sent().is_empty());
    assert!(harness
        .state()
        .overlays
        .transient_notice
        .as_ref()
        .is_some_and(|(notice, _)| notice.contains("Re-enter")));
}

#[test]
fn panes_ime_and_deferred_input_are_isolated_between_viewports() {
    let context = egui::Context::default();
    let owner = crate::keyboard::CompositionOwner {
        surface: 1,
        epoch: 0,
        generation: Some(1),
    };
    let root = egui::RawInput {
        events: vec![egui::Event::Ime(egui::ImeEvent::Preedit {
            text: "root-private".into(),
            active_range_chars: None,
        })],
        ..Default::default()
    };
    let mut output = context.run_ui(root, |ui| {
        let context = ui.ctx();
        assert!(crate::keyboard::composition_owns_keys(context, owner));
        let id = FesTermApp::ordered_keyboard_events_id(context);
        context
            .data_mut(|data| data.insert_temp(id, vec![egui::Event::Text("root-private".into())]));
    });
    output.textures_delta.clear();
    let viewport = egui::ViewportId::from_hash_of("second-window");
    let mut second = egui::RawInput {
        viewport_id: viewport,
        ..Default::default()
    };
    second.viewports.insert(viewport, Default::default());
    let mut output = context.run_ui(second, |ui| {
        let context = ui.ctx();
        assert!(!crate::keyboard::composition_owns_keys(
            context,
            crate::keyboard::CompositionOwner {
                surface: 2,
                ..owner
            }
        ));
        let id = FesTermApp::ordered_keyboard_events_id(context);
        assert!(context
            .data(|data| data.get_temp::<Vec<egui::Event>>(id))
            .is_none());
    });
    output.textures_delta.clear();
    let mut output = context.run_ui(egui::RawInput::default(), |ui| {
        let context = ui.ctx();
        assert!(crate::keyboard::composition_active(context, owner));
        let id = FesTermApp::ordered_keyboard_events_id(context);
        assert_eq!(
            context
                .data(|data| data.get_temp::<Vec<egui::Event>>(id))
                .unwrap(),
            vec![egui::Event::Text("root-private".into())]
        );
    });
    output.textures_delta.clear();
}

#[test]
fn panes_retire_ime_when_the_same_view_changes_session_generation() {
    let (app, _, second, first_transport, second_transport) = grouped_app();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1600.0, 960.0))
        .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
    harness.run();
    harness
        .input_mut()
        .events
        .push(egui::Event::Ime(egui::ImeEvent::Preedit {
            text: "old-generation".into(),
            active_range_chars: None,
        }));
    harness.step();
    harness
        .state_mut()
        .state
        .session_tab_mut(second)
        .unwrap()
        .controller
        .advance_lifecycle_generation();
    harness
        .input_mut()
        .events
        .push(egui::Event::Ime(egui::ImeEvent::Commit(
            "old-generation".into(),
        )));
    harness.step();
    assert!(first_transport.sent().is_empty());
    assert!(second_transport.sent().is_empty());
}

#[test]
fn panes_later_click_cannot_retarget_earlier_text_or_native_paste() {
    for paste in [false, true] {
        let (app, first, second, first_transport, second_transport) = grouped_app();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1600.0, 960.0))
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        let position = harness
            .state()
            .pane_rects
            .iter()
            .find(|(view, _)| *view == first)
            .unwrap()
            .1
            .center();
        if paste {
            let (modifiers, key) = crate::keyboard::chord(if cfg!(target_os = "macos") {
                "Primary+V"
            } else {
                "Ctrl+Shift+V"
            })
            .unwrap();
            harness.input_mut().events.extend([
                egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
                egui::Event::Paste("earlier-secret".into()),
            ]);
        } else {
            harness
                .input_mut()
                .events
                .push(egui::Event::Text("earlier-secret".into()));
        }
        harness.input_mut().events.extend([
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::Text("later-first".into()),
        ]);
        harness.step();
        if !paste {
            assert_eq!(
                harness.state().state.active(),
                second,
                "earlier widget input must drain before the focus transition"
            );
        }
        harness.run();
        assert_eq!(harness.state().state.active(), first);
        assert_eq!(
            second_transport.sent().concat(),
            b"earlier-secret",
            "paste={paste}"
        );
        assert_eq!(
            first_transport.sent().concat(),
            b"later-first",
            "paste={paste}"
        );
    }
}

#[test]
fn panes_pointer_focus_binds_same_frame_keyboard_to_only_the_clicked_surface() {
    let (app, first, _, first_transport, second_transport) = grouped_app();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1600.0, 960.0))
        .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
    harness.run();
    let position = harness
        .state()
        .pane_rects
        .iter()
        .find(|(view, _)| *view == first)
        .unwrap()
        .1
        .center();
    harness.input_mut().events.extend([
        egui::Event::PointerMoved(position),
        egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::Text("clicked-first".into()),
    ]);
    harness.step();
    assert_eq!(harness.state().state.active(), first);
    assert_eq!(first_transport.sent().concat(), b"clicked-first");
    assert!(second_transport.sent().is_empty());
}

#[test]
fn panes_render_both_live_terminals_and_route_input_only_to_focused_view() {
    let (mut app, first, first_transport) = FesTermApp::for_test_with_fake_ssh_session([]);
    let context = egui::Context::default();
    app.state.dispatch(AppCommand::OpenLauncher, &context);
    let second_transport = crate::session_controller::fake::FakeSshSession::new([]);
    let second = app.state.replace_active_with_test_ssh_session(
        second_transport.clone(),
        "second",
        "second.example.test",
        22,
    );
    let generations = [first, second].map(|id| {
        app.state
            .session_tab(id)
            .unwrap()
            .controller
            .lifecycle_generation()
    });
    app.state
        .pane_geometry(egui::vec2(1600.0, 960.0), egui::vec2(520.0, 240.0));
    app.state.dispatch(
        AppCommand::CombinePanes {
            source: second,
            target: first,
            side: PaneSide::Right,
            expected_revision: app.state.pane_revision(),
        },
        &context,
    );
    assert!(app.state.take_pane_error().is_none());
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1600.0, 960.0))
        .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
    harness.run();
    assert_eq!(harness.state().pane_rects.len(), 2);
    let first_grid = harness
        .state()
        .state
        .session_tab(first)
        .unwrap()
        .view
        .diagnostics()
        .grid_rect
        .unwrap();
    let second_grid = harness
        .state()
        .state
        .session_tab(second)
        .unwrap()
        .view
        .diagnostics()
        .grid_rect
        .unwrap();
    assert!(first_grid.max.x <= second_grid.min.x);
    harness
        .input_mut()
        .events
        .push(egui::Event::Text("second-input".into()));
    harness.step();
    assert_eq!(second_transport.sent().concat(), b"second-input");
    assert!(first_transport.sent().is_empty());
    let context = harness.ctx.clone();
    harness
        .state_mut()
        .state
        .dispatch(AppCommand::ActivateTab(first), &context);
    harness.run();
    harness
        .input_mut()
        .events
        .push(egui::Event::Text("first-input".into()));
    harness.step();
    assert_eq!(first_transport.sent().concat(), b"first-input");
    assert_eq!(second_transport.sent().concat(), b"second-input");
    assert_eq!(
        [first, second].map(|id| harness
            .state()
            .state
            .session_tab(id)
            .unwrap()
            .controller
            .lifecycle_generation()),
        generations
    );
    harness
        .state_mut()
        .state
        .dispatch(AppCommand::SeparatePaneGroup(first), &context);
    harness.run();
    assert_eq!(harness.state().state.tabs().len(), 2);
    assert!(harness.state().state.pane_group(first).is_none());
    assert_eq!(
        [first, second].map(|id| harness
            .state()
            .state
            .session_tab(id)
            .unwrap()
            .controller
            .lifecycle_generation()),
        generations
    );
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum PaneGalleryScene {
    Single,
    Equal,
    Unequal,
    Three,
    Layout,
    HoverEqual,
    HoverUnequal,
    HoverThree,
    Four,
    Details,
    Handles,
    Rearrange,
    Moving,
    HandleMenu,
}

pub(crate) fn pane_gallery_frame(details: bool) -> image::RgbaImage {
    pane_gallery_scene_frame(if details {
        PaneGalleryScene::Details
    } else {
        PaneGalleryScene::Four
    })
}

pub(crate) fn pane_gallery_scene_frame(scene: PaneGalleryScene) -> image::RgbaImage {
    let (mut app, first, second, _, _) = grouped_app();
    let context = egui::Context::default();
    let mut ids = vec![first, second];
    if scene == PaneGalleryScene::Single {
        app.state.dispatch(AppCommand::CloseTab(second), &context);
        ids.pop();
    }
    let extra = match scene {
        PaneGalleryScene::Three | PaneGalleryScene::HoverThree => 1,
        PaneGalleryScene::Four | PaneGalleryScene::Details | PaneGalleryScene::Layout => 2,
        _ => 0,
    };
    for (target, name) in [(first, "build"), (second, "logs")].into_iter().take(extra) {
        app.state.dispatch(AppCommand::OpenLauncher, &context);
        let view = app.state.replace_active_with_test_ssh_session(
            crate::session_controller::fake::FakeSshSession::new([]),
            name,
            "fixture.example.test",
            22,
        );
        app.state.dispatch(
            AppCommand::CombinePanes {
                source: view,
                target,
                side: PaneSide::Below,
                expected_revision: app.state.pane_revision(),
            },
            &context,
        );
        assert!(app.state.take_pane_error().is_none());
        ids.push(view);
    }
    if matches!(
        scene,
        PaneGalleryScene::Unequal | PaneGalleryScene::HoverUnequal
    ) {
        app.state.dispatch(
            AppCommand::ResizePaneSplit {
                view: first,
                path: Vec::new(),
                ratio: 6667,
                expected_revision: app.state.pane_revision(),
            },
            &context,
        );
        assert!(app.state.take_pane_error().is_none());
    }
    for (index, view) in ids.iter().enumerate() {
        let session = app.state.session_tab_mut(*view).unwrap();
        session.label = ["Shell", "Tests", "Build", "Logs"][index].into();
        session.terminal.ingest(format!("fixture@host-{}:~/project$ cargo test\r\n{}: running controlled fixture {}\r\n\x1b[32mtest result: ok\x1b[0m\r\nfixture@host:~/project$ ",index+1,session.label,index+1).as_bytes());
    }
    app.state.dispatch(AppCommand::ActivateTab(first), &context);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1600.0, 960.0))
        .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
    harness.ctx.set_theme(egui::ThemePreference::Dark);
    harness.ctx.set_visuals(theme::default_visuals());
    harness.run();
    assert_eq!(harness.state().pane_rects.len(), ids.len());
    if matches!(
        scene,
        PaneGalleryScene::Handles
            | PaneGalleryScene::Rearrange
            | PaneGalleryScene::Moving
            | PaneGalleryScene::HandleMenu
    ) {
        let rect = harness
            .state()
            .pane_rects
            .iter()
            .find(|(view, _)| *view == first)
            .unwrap()
            .1;
        if matches!(
            scene,
            PaneGalleryScene::Rearrange | PaneGalleryScene::Moving
        ) {
            let context = harness.ctx.clone();
            harness.state_mut().begin_pane_rearrange(&context);
        }
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(egui::pos2(
                rect.center().x,
                rect.top() + 16.0,
            )));
        harness.run();
        harness.get_by_label("Move Shell pane");
        if scene == PaneGalleryScene::Moving {
            harness.get_by_label("Move Shell pane").click();
            harness.run();
            harness.get_by_label("Moving: Shell");
        }
        if scene == PaneGalleryScene::HandleMenu {
            harness.get_by_label("Actions for Shell").click();
            harness.run();
            harness.get_by_label("Detach to own tab");
        }
        return harness.render().unwrap();
    }
    if matches!(scene, PaneGalleryScene::Four | PaneGalleryScene::Details) {
        let image = harness.render().unwrap();
        let focused_background = image.get_pixel(400, 200);
        for (x, y) in [(1200, 200), (400, 700), (1200, 700)] {
            assert_eq!(
                image.get_pixel(x, y),
                focused_background,
                "passive panes must not be dimmed"
            );
        }
    }
    if scene == PaneGalleryScene::Layout {
        harness.state_mut().pane_layout_menu = Some(first);
        harness.run();
        harness.get_by_label("Rearrange panes");
        return harness.render().unwrap();
    }
    let hover = matches!(
        scene,
        PaneGalleryScene::HoverEqual
            | PaneGalleryScene::HoverUnequal
            | PaneGalleryScene::HoverThree
    );
    if scene != PaneGalleryScene::Details && !hover {
        return harness.render().unwrap();
    }
    let unread = if ids.len() > 2 { ids[2] } else { second };
    harness
        .state_mut()
        .state
        .session_tab_mut(unread)
        .unwrap()
        .has_new_output_since_active = true;
    harness.run();
    let (chips, _) = harness.state().chip_view_models();
    let group = chips.iter().find(|chip| !chip.panes.is_empty()).unwrap();
    let content = harness
        .state()
        .pane_rects
        .iter()
        .fold(egui::Rect::NOTHING, |bounds, (_, rect)| bounds.union(*rect));
    for member in &group.panes {
        let view = TabId::from_chip_id(member.id.0);
        let rect = harness
            .state()
            .pane_rects
            .iter()
            .find(|(id, _)| *id == view)
            .unwrap()
            .1;
        assert_eq!(
            member.label,
            harness.state().state.session_tab(view).unwrap().label
        );
        assert_eq!(
            member.unread,
            view == unread,
            "unread binds view identity, not list row"
        );
        assert_eq!(
            member.bounds.center().x > 500.0,
            rect.center().x > content.center().x
        );
        assert_eq!(
            member.bounds.center().y > 500.0,
            rect.center().y > content.center().y
        );
    }
    if hover {
        harness
            .ctx
            .global_style_mut(|style| style.interaction.tooltip_delay = 0.0);
        let group = harness.state().state.pane_group(first).unwrap();
        harness
            .get_by_label(&format!(
                "{} group, {} panes, 1 unread",
                group.label,
                ids.len()
            ))
            .hover();
        harness.run_steps(5);
        harness.get_by_label("1: Shell · SSH · Connected · focused");
        assert!(
            !harness.state().pane_details_open,
            "capture a hover, not a pinned popup"
        );
        return harness.render().unwrap();
    }
    harness
        .get_by_label("Pane details, 4 panes, 1 unread")
        .click();
    harness.run();
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(egui::pos2(700.0, 300.0)));
    harness.run();
    let image = harness.render().unwrap();
    assert!(
        image.enumerate_pixels().any(|(x, y, pixel)| {
            x < 150
                && y < 44
                && pixel[0] > 180
                && pixel[1] > 120
                && pixel[1] < pixel[0]
                && pixel[2] < 120
        }),
        "the unread quadrant retains the themed amber accent as well as its shape"
    );
    image
}

#[test]
fn panes_passive_surfaces_preserve_full_opacity() {
    let _ = pane_gallery_frame(false);
}

#[test]
fn panes_unread_details_preserve_member_shape_and_accent() {
    let _ = pane_gallery_frame(true);
}

#[test]
fn panes_group_close_refuses_stale_lifecycle_without_closing_any_member() {
    let (mut app, first, _) = FesTermApp::for_test_with_fake_ssh_session([]);
    let context = egui::Context::default();
    app.state.dispatch(AppCommand::NewTextDocument, &context);
    let second = app.state.active();
    app.state
        .pane_geometry(egui::vec2(1600.0, 960.0), egui::vec2(520.0, 240.0));
    app.state.dispatch(
        AppCommand::CombinePanes {
            source: second,
            target: first,
            side: PaneSide::Right,
            expected_revision: app.state.pane_revision(),
        },
        &context,
    );
    let plan = app.state.pane_close_plan(first).unwrap();
    app.state
        .session_tab_mut(first)
        .unwrap()
        .controller
        .advance_lifecycle_generation();
    app.state.dispatch(
        AppCommand::ClosePaneGroup {
            plan,
            discard: Vec::new(),
        },
        &context,
    );
    assert!(app.state.take_pane_error().is_some());
    assert_eq!(app.state.tabs().len(), 2);
    assert_eq!(app.state.pane_members(first), vec![first, second]);
}

#[test]
fn panes_cross_window_transfer_refuses_without_mutating_sources() {
    let (mut app, first, _) = FesTermApp::for_test_with_fake_ssh_session([]);
    let context = egui::Context::default();
    app.state.dispatch(AppCommand::OpenLauncher, &context);
    let second = app.state.replace_active_with_test_ssh_session(
        crate::session_controller::fake::FakeSshSession::new([]),
        "second",
        "second.example.test",
        22,
    );
    app.state
        .pane_geometry(egui::vec2(1600.0, 960.0), egui::vec2(520.0, 240.0));
    app.state.dispatch(
        AppCommand::CombinePanes {
            source: second,
            target: first,
            side: PaneSide::Right,
            expected_revision: app.state.pane_revision(),
        },
        &context,
    );
    let members = app.state.pane_members(first);
    assert!(app.state.detach_tab(first).is_none());
    assert_eq!(app.state.pane_members(first), members);
    assert_eq!(app.state.tabs().len(), 2);
    assert!(app.state.take_pane_error().is_some());
    app.state
        .dispatch(AppCommand::SeparatePaneGroup(first), &context);
    assert!(app.state.detach_tab(first).is_some());
    assert_eq!(app.state.tabs().len(), 1);
}
