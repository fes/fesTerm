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
        session.terminal.ingest(format!("fixture@host-{}:~/project$ cargo test\r\nrunning controlled fixture {}\r\n\x1b[32mtest result: ok\x1b[0m\r\nfixture@host:~/project$ ",index+1,index+1).as_bytes());
    }
    app.state.dispatch(AppCommand::ActivateTab(first), &context);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1600.0, 960.0))
        .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
    harness.ctx.set_theme(egui::ThemePreference::Dark);
    harness.ctx.set_visuals(theme::default_visuals());
    harness.run();
    assert_eq!(harness.state().pane_rects.len(), ids.len());
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
        harness.get_by_label("Move to own tab");
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
