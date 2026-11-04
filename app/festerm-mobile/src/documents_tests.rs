use super::*;
use crate::{Lifecycle, MobileApp, Surface};
use egui_kittest::{kittest::Queryable, Harness};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

const INITIAL: &[u8] = b"\xef\xbb\xbf# Runbook\r\none two\r\n";

fn opening(host: &DocumentHost) -> Opening {
    Opening {
        file: DocumentFile,
        existing: host
            .registry
            .views()
            .map(|(view, id)| (id, host.registry.read_ticket(view).unwrap()))
            .collect(),
    }
}

fn snapshot(bytes: &[u8], version: u8, writable: bool) -> DocumentFileSnapshot {
    DocumentFileSnapshot {
        path: "/authorized/Runbook.md".into(),
        bytes: bytes.to_vec(),
        version: [version; 32],
        writable,
    }
}

fn app(writable: bool) -> MobileApp {
    let mut app = MobileApp::new(Rc::new(Cell::new(Lifecycle {
        active: true,
        ..Default::default()
    })));
    app.open_local_tab();
    app.surface = Surface::Terminal;
    let opening = opening(&app.documents);
    app.documents
        .attach(opening, snapshot(INITIAL, 1, writable), &mut app.next_tab)
        .unwrap();
    let tab = app.documents.views[0].tab;
    app.activate_tab(tab);
    app
}

fn harness(app: MobileApp) -> Harness<'static, MobileApp> {
    Harness::builder()
        .with_size(egui::vec2(390.0, 844.0))
        .build_ui_state(
            |ui, app: &mut MobileApp| app.show_with_occlusion(ui, 0.0),
            app,
        )
}

fn focus(harness: &mut Harness<'_, MobileApp>, view: ViewId, at: usize) {
    let id = body_id(view);
    let mut state = egui::TextEdit::load_state(&harness.ctx, id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::one(
            egui::text::CCursor::new(at),
        )));
    state.store(&harness.ctx, id);
    harness.ctx.memory_mut(|memory| memory.request_focus(id));
    harness.run();
}

#[test]
fn document_surface_native_multiline_edits_never_reach_the_terminal() {
    let pending = Rc::new(RefCell::new(Vec::new()));
    let incoming = pending.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(390.0, 844.0))
        .build_ui_state(
            move |ui, app: &mut MobileApp| {
                crate::apply_field_input(ui.ctx(), std::mem::take(&mut *incoming.borrow_mut()));
                app.show_with_occlusion(ui, 0.0);
            },
            app(true),
        );
    harness.run();
    let view = harness.state().documents.active.unwrap();
    let original = harness
        .state()
        .documents
        .registry
        .view(view)
        .unwrap()
        .text()
        .text()
        .to_owned();
    focus(&mut harness, view, 0);
    *pending.borrow_mut() = vec![
        festerm_ios_window::FieldInput::Select(0..0),
        festerm_ios_window::FieldInput::Insert("local".into()),
        festerm_ios_window::FieldInput::Insert("\n".into()),
        festerm_ios_window::FieldInput::Insert("text".into()),
    ];
    harness.run();
    let entry = harness.state().documents.registry.view(view).unwrap();
    assert_eq!(entry.text().text(), format!("local\ntext{original}"));
    assert!(entry.dirty());
    assert_eq!(harness.state().active_tab, None, "terminal remains parked");
    assert_eq!(
        harness.state().sink.diagnostics,
        festerm_ui_egui::InputSinkDiagnostics::default()
    );
    assert!(!harness.state().terminal_keyboard_input);
    let undo = harness.get_by_label("Undo");
    undo.click();
    harness.run();
    assert_eq!(
        harness
            .state()
            .documents
            .registry
            .view(view)
            .unwrap()
            .text()
            .to_bytes(),
        INITIAL
    );
    harness.get_by_label("Redo").click();
    harness.run();
    assert!(harness
        .state()
        .documents
        .registry
        .view(view)
        .unwrap()
        .text()
        .text()
        .starts_with("local\ntext"));
}

#[test]
fn document_views_share_history_but_keep_independent_widget_selection() {
    let mut app = app(true);
    let first = app.documents.active.unwrap();
    app.documents.duplicate(first, &mut app.next_tab);
    let second = app.documents.active.unwrap();
    let mut harness = harness(app);
    harness.run();
    focus(&mut harness, second, 2);
    harness.event(egui::Event::Text("X".into()));
    harness.run();
    let id = harness
        .state()
        .documents
        .registry
        .document_for(first)
        .unwrap();
    assert_eq!(
        harness
            .state()
            .documents
            .registry
            .document_for(second)
            .unwrap(),
        id
    );
    assert!(harness
        .state()
        .documents
        .surfaces(true)
        .iter()
        .all(|surface| surface.title.ends_with(" *")));
    harness.state_mut().documents.active = Some(first);
    harness.run();
    focus(&mut harness, first, 0);
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    harness.run();
    assert_eq!(
        harness
            .state()
            .documents
            .registry
            .view(second)
            .unwrap()
            .text()
            .to_bytes(),
        INITIAL
    );
    let second_cursor = egui::TextEdit::load_state(&harness.ctx, body_id(second))
        .unwrap()
        .cursor
        .char_range()
        .unwrap();
    assert_eq!(second_cursor.primary.index.0, 3);
    let state = egui::TextEdit::load_state(&harness.ctx, body_id(first)).unwrap();
    assert!(!state.undoer().has_undo(&(
        egui::text::CCursorRange::default(),
        "widget copy".to_owned()
    )));
}

#[test]
fn read_only_document_surface_cannot_type_or_undo() {
    let mut harness = harness(app(false));
    harness.run();
    let view = harness.state().documents.active.unwrap();
    focus(&mut harness, view, 0);
    harness.event(egui::Event::Text("must not edit".into()));
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    harness.run();
    assert_eq!(
        harness
            .state()
            .documents
            .registry
            .view(view)
            .unwrap()
            .text()
            .to_bytes(),
        INITIAL
    );
    assert!(!harness
        .state()
        .documents
        .registry
        .view(view)
        .unwrap()
        .writable());
}

#[test]
fn document_hardware_tab_indents_locally_without_changing_focus() {
    let mut harness = harness(app(true));
    harness.run();
    let view = harness.state().documents.active.unwrap();
    focus(&mut harness, view, 0);
    harness.key_press(egui::Key::Tab);
    harness.event(egui::Event::Text("indented".into()));
    harness.run();
    assert!(harness
        .state()
        .documents
        .registry
        .view(view)
        .unwrap()
        .text()
        .text()
        .starts_with("\tindented"));
    assert_eq!(
        harness.ctx.memory(|memory| memory.focused()),
        Some(body_id(view))
    );
    assert_eq!(harness.state().sink.diagnostics.byte_count, 0);
}

#[test]
fn document_last_dirty_close_prompts_without_background_keyboard_edits() {
    let mut app = app(true);
    let view = app.documents.active.unwrap();
    let target = app.documents.registry.target(view, 0..0).unwrap();
    app.documents
        .registry
        .replace(&target, &target, "unsaved ")
        .unwrap();
    app.documents.duplicate(view, &mut app.next_tab);
    let other = app.documents.active.unwrap();
    let first_tab = app.documents.views[0].tab;
    let last_tab = app.documents.views[1].tab;
    let mut harness = harness(app);
    harness.run();
    harness.state_mut().close_tab(first_tab);
    harness.run();
    assert!(harness.state().documents.registry.view(view).is_err());
    assert!(harness.state().documents.close.is_none());
    focus(&mut harness, other, 0);
    harness.state_mut().close_tab(last_tab);
    harness.run();
    let before = harness
        .state()
        .documents
        .registry
        .view(other)
        .unwrap()
        .text()
        .text()
        .to_owned();
    harness.event(egui::Event::Text("not behind a sheet".into()));
    harness.run();
    assert_eq!(
        harness
            .state()
            .documents
            .registry
            .view(other)
            .unwrap()
            .text()
            .text(),
        before
    );
    harness.get_by_label("Keep editing").click();
    harness.run();
    assert!(harness.state().documents.registry.view(other).is_ok());
    harness.state_mut().close_tab(last_tab);
    harness.run();
    harness.get_by_label("Discard changes").click();
    harness.run();
    assert!(harness.state().documents.views.is_empty());
    assert!(egui::TextEdit::load_state(&harness.ctx, body_id(other)).is_none());
    assert_eq!(
        harness.state().tabs.len(),
        1,
        "closing a document never closes the terminal"
    );
}

#[test]
fn fresh_file_authority_preserves_dirty_text_and_observes_conflict() {
    let mut app = app(true);
    let view = app.documents.active.unwrap();
    let target = app.documents.registry.target(view, 0..0).unwrap();
    app.documents
        .registry
        .replace(&target, &target, "my edits ")
        .unwrap();
    let id = app.documents.registry.document_for(view).unwrap();
    app.documents.unavailable(id);
    assert!(!app.documents.registry.view(view).unwrap().writable());
    let captured = opening(&app.documents);
    app.documents
        .attach(
            captured,
            snapshot(b"outside replacement\n", 2, true),
            &mut app.next_tab,
        )
        .unwrap();
    let entry = app.documents.registry.view(view).unwrap();
    assert!(entry.text().text().starts_with("my edits "));
    assert!(entry.conflict().is_some());
    assert!(entry.writable());
    assert_eq!(app.documents.sources.len(), 1);
    assert!(app
        .documents
        .registry
        .replace(&target, &target, "stale")
        .is_err());
}

#[test]
fn late_open_and_failed_native_save_keep_canonical_content() {
    let mut app = app(true);
    let view = app.documents.active.unwrap();
    let captured = opening(&app.documents);
    let target = app.documents.registry.target(view, 0..0).unwrap();
    app.documents
        .registry
        .replace(&target, &target, "newer ")
        .unwrap();
    app.documents
        .attach(captured, snapshot(INITIAL, 1, true), &mut app.next_tab)
        .unwrap();
    assert!(app
        .documents
        .registry
        .view(view)
        .unwrap()
        .text()
        .text()
        .starts_with("newer "));
    assert!(app
        .documents
        .notice
        .as_deref()
        .unwrap()
        .contains("changed during the read"));
    app.documents.save(view);
    let entry = app.documents.registry.view(view).unwrap();
    assert!(entry.dirty());
    assert!(!entry.saving());
    assert_eq!(
        app.documents.notice.as_deref(),
        Some(DocumentFileError::Unsupported.message())
    );
}

#[test]
fn document_launcher_keeps_existing_global_navigation() {
    let mut harness = harness(MobileApp::new(Rc::new(Cell::new(Lifecycle {
        active: true,
        ..Default::default()
    }))));
    harness.run();
    harness.get_by_label("Documents").click();
    harness.run();
    assert_eq!(harness.state().surface, Surface::Documents);
    harness.get_by_label("Browse in Files").click();
    harness.run();
    assert_eq!(
        harness.state().documents.notice.as_deref(),
        Some(DocumentFileError::Unsupported.message())
    );
    harness.get_by_label("Back").click();
    harness.run();
    assert_eq!(harness.state().surface, Surface::Tabs);
    harness.get_by_label("Settings");
    harness.get_by_label("Sessions");
}

fn changed_app() -> MobileApp {
    let mut app = app(true);
    let view = app.documents.active.unwrap();
    let target = app.documents.registry.target(view, 0..0).unwrap();
    app.documents
        .registry
        .replace(&target, &target, "my edits\n")
        .unwrap();
    let id = app.documents.registry.document_for(view).unwrap();
    let read = app.documents.registry.read_ticket(view).unwrap();
    app.documents.sources.get_mut(&id).unwrap().read = Some(read);
    app.documents
        .source_event(
            id,
            DocumentFileEvent::Loaded(snapshot(b"outside replacement\n", 2, true)),
        )
        .unwrap();
    app
}

#[test]
fn stale_refresh_requeues_without_losing_edits_or_restoring_obsolete_access() {
    let mut app = app(true);
    let view = app.documents.active.unwrap();
    let id = app.documents.registry.document_for(view).unwrap();
    let read = app.documents.registry.read_ticket(view).unwrap();
    app.documents.sources.get_mut(&id).unwrap().read = Some(read);
    let target = app.documents.registry.target(view, 0..0).unwrap();
    app.documents
        .registry
        .replace(&target, &target, "typing during read\n")
        .unwrap();
    app.documents
        .source_event(
            id,
            DocumentFileEvent::Loaded(snapshot(b"outside\n", 2, false)),
        )
        .unwrap();
    assert!(app.documents.notice.is_none());
    assert!(app.documents.refresh.contains(&id));
    assert!(!app.documents.registry.view(view).unwrap().writable());
    assert!(app
        .documents
        .registry
        .view(view)
        .unwrap()
        .text()
        .text()
        .starts_with("typing during read"));
    let fresh = app.documents.registry.read_ticket(view).unwrap();
    app.documents.sources.get_mut(&id).unwrap().read = Some(fresh);
    app.documents
        .source_event(
            id,
            DocumentFileEvent::Loaded(snapshot(b"outside\n", 2, false)),
        )
        .unwrap();
    assert_eq!(
        app.documents
            .registry
            .view(view)
            .unwrap()
            .conflict()
            .unwrap()
            .bytes(),
        b"outside\n"
    );
    let pending = app.documents.registry.read_ticket(view).unwrap();
    app.documents.sources.get_mut(&id).unwrap().read = Some(pending);
    app.documents.invalidate_sources();
    app.documents.refresh.clear();
    app.documents
        .source_event(
            id,
            DocumentFileEvent::Loaded(snapshot(b"outside\n", 2, true)),
        )
        .unwrap();
    assert!(app.documents.refresh.contains(&id));
    assert!(!app.documents.registry.view(view).unwrap().available());
    assert!(!app.documents.registry.view(view).unwrap().writable());
}

#[test]
fn conflict_keep_requires_review_and_leaves_save_explicit_and_version_bound() {
    let mut harness = harness(changed_app());
    harness.run();
    let view = harness.state().documents.active.unwrap();
    harness.get_by_label("Compare / resolve").click();
    harness.run();
    harness.event(egui::Event::Text("not behind review".into()));
    harness.run();
    assert!(harness
        .state()
        .documents
        .registry
        .view(view)
        .unwrap()
        .text()
        .text()
        .starts_with("my edits\n#"));
    harness.get_by_label("Keep my edits").click();
    harness.run();
    let entry = harness.state().documents.registry.view(view).unwrap();
    assert!(entry.conflict().unwrap().kept_edits());
    assert!(entry.dirty());
    assert!(!entry.saving());
    let (ticket, bytes) = harness
        .state_mut()
        .documents
        .registry
        .prepare_save(view, false)
        .unwrap();
    assert_eq!(ticket.expected, SourceVersion([2; 32]));
    assert!(bytes
        .windows(b"my edits".len())
        .any(|bytes| bytes == b"my edits"));
}

#[test]
fn conflict_reload_discards_only_after_explicit_review_and_refuses_stale_decisions() {
    let mut harness = harness(changed_app());
    harness.run();
    let view = harness.state().documents.active.unwrap();
    harness.get_by_label("Compare / resolve").click();
    harness.run();
    let captured = harness.state().documents.compare.clone().unwrap();
    let ticket = harness
        .state()
        .documents
        .registry
        .read_ticket(view)
        .unwrap();
    harness
        .state_mut()
        .documents
        .registry
        .observe(
            &ticket,
            SourceSnapshot {
                bytes: b"newer external\n".to_vec(),
                version: SourceVersion([3; 32]),
            },
        )
        .unwrap();
    assert_eq!(
        harness.state_mut().documents.resolve(&captured, false),
        Err(DocumentError::Stale)
    );
    assert_eq!(
        harness.state_mut().documents.resolve(&captured, true),
        Err(DocumentError::Stale)
    );
    harness.run();
    assert!(harness.state().documents.compare.is_none());
    harness.run();
    harness.get_by_label("Compare / resolve").click();
    harness.run();
    harness.get_by_label("Reload and discard edits").click();
    harness.run();
    let entry = harness.state().documents.registry.view(view).unwrap();
    assert_eq!(entry.text().to_bytes(), b"newer external\n");
    assert!(!entry.dirty());
    assert!(!harness.state_mut().documents.registry.undo(view).unwrap());
}

#[test]
fn document_chip_close_from_terminal_uses_dirty_guard_and_blocks_background_input() {
    let mut app = changed_app();
    let view = app.documents.active.unwrap();
    let terminal = app.tabs[0].id;
    app.activate_tab(terminal);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(844.0, 844.0))
        .build_ui_state(
            |ui, app: &mut MobileApp| app.show_with_occlusion(ui, 0.0),
            app,
        );
    harness.run();
    let chip = harness.get_by_label("Runbook.md !").rect().center();
    let now = harness.ctx.input(|input| input.time);
    for (elapsed, pressed) in [
        (0.01, Some(true)),
        (0.3, None),
        (0.6, None),
        (0.7, Some(false)),
    ] {
        harness.input_mut().time = Some(now + elapsed);
        harness.event(egui::Event::PointerMoved(chip));
        if let Some(pressed) = pressed {
            harness.event(egui::Event::PointerButton {
                pos: chip,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        harness.step();
    }
    harness.run();
    assert!(harness.state().confirm_close.is_none());
    assert_eq!(harness.state().documents.close, Some(view));
    let before = harness.state().sink.diagnostics.byte_count;
    harness.event(egui::Event::Text("not behind close".into()));
    harness.key_press(egui::Key::Enter);
    harness.run();
    assert_eq!(harness.state().sink.diagnostics.byte_count, before);
    harness.get_by_label("Discard changes").click();
    harness.run();
    assert!(harness.state().documents.registry.view(view).is_err());
    assert_eq!(harness.state().active_tab, Some(terminal));
}

#[test]
fn short_document_view_moves_touch_sized_actions_into_more() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(844.0, 390.0))
        .build_ui_state(
            |ui, app: &mut MobileApp| app.show_with_occlusion(ui, 0.35),
            app(true),
        );
    harness.run();
    assert!(harness.query_by_label("Save").is_none());
    harness.get_by_label("More").click();
    harness.run();
    assert!(
        egui::Popup::is_any_open(&harness.ctx),
        "document More popup must open"
    );
    harness.run();
    for label in ["Save", "Undo", "Redo"] {
        assert!(harness.get_by_label(label).rect().height() >= space::TOUCH);
    }
}

#[test]
fn conflict_actions_stay_above_keyboard_occlusion_at_short_and_narrow_sizes() {
    for (width, height, occlusion) in [
        (844.0, 390.0, 0.35),
        (390.0, 844.0, 0.35),
        (320.0, 568.0, 0.35),
    ] {
        let mut app = changed_app();
        let view = app.documents.active.unwrap();
        app.documents.compare = Some(app.documents.registry.target(view, 0..0).unwrap());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(width, height))
            .build_ui_state(
                move |ui, app: &mut MobileApp| app.show_with_occlusion(ui, occlusion),
                app,
            );
        harness.run();
        let bottom = height * (1.0 - occlusion);
        for label in ["Keep my edits", "Reload and discard edits", "Cancel"] {
            let rect = harness.get_by_label(label).rect();
            assert!(
                rect.top() >= 0.0 && rect.bottom() <= bottom,
                "{label} {rect:?} must clear keyboard boundary {bottom}"
            );
            assert!(rect.height() >= space::TOUCH);
        }
    }
}

fn wait_preview(harness: &mut Harness<'_, MobileApp>, view: ViewId) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        harness.step();
        let revision = harness
            .state()
            .documents
            .registry
            .view(view)
            .unwrap()
            .text()
            .revision();
        if harness
            .state()
            .documents
            .views
            .iter()
            .any(|entry| entry.id == view && entry.preview_revision == Some(revision))
        {
            harness.run();
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "preview must become current"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn markdown_edit_preview_keeps_source_selection_and_shared_history() {
    let mut harness = harness(app(true));
    harness.run();
    let view = harness.state().documents.active.unwrap();
    focus(&mut harness, view, 2);
    harness.event(egui::Event::Text("X".into()));
    harness.run();
    let before = harness
        .state()
        .documents
        .registry
        .view(view)
        .unwrap()
        .text()
        .to_bytes();
    let selection = egui::TextEdit::load_state(&harness.ctx, body_id(view))
        .unwrap()
        .cursor
        .char_range();
    harness.get_by_label("Preview").click();
    wait_preview(&mut harness, view);
    assert_eq!(
        harness
            .state()
            .documents
            .registry
            .view(view)
            .unwrap()
            .text()
            .to_bytes(),
        before
    );
    assert!(!harness.state().terminal_keyboard_input);
    assert!(harness.output().platform_output.ime.is_none());
    harness.get_by_label("Edit").click();
    harness.run();
    assert!(harness.ctx.memory(|memory| memory.has_focus(body_id(view))));
    assert!(harness.output().platform_output.ime.is_some());
    assert_eq!(
        egui::TextEdit::load_state(&harness.ctx, body_id(view))
            .unwrap()
            .cursor
            .char_range(),
        selection
    );
    harness.get_by_label("Undo").click();
    harness.run();
    assert_eq!(
        harness
            .state()
            .documents
            .registry
            .view(view)
            .unwrap()
            .text()
            .to_bytes(),
        INITIAL
    );
}

#[test]
fn markdown_contents_is_view_local_and_never_mutates_or_sends_terminal_input() {
    let mut harness = harness(app(true));
    harness.run();
    let view = harness.state().documents.active.unwrap();
    harness.get_by_label("Preview").click();
    wait_preview(&mut harness, view);
    harness.get_by_label("Contents").click();
    harness.run();
    assert_eq!(
        harness.state().documents.contents.map(|(owner, _)| owner),
        Some(view)
    );
    let before = harness
        .state()
        .documents
        .registry
        .view(view)
        .unwrap()
        .text()
        .to_bytes();
    harness.event(egui::Event::Text("not into source or terminal".into()));
    harness.run();
    assert_eq!(
        harness
            .state()
            .documents
            .registry
            .view(view)
            .unwrap()
            .text()
            .to_bytes(),
        before
    );
    assert_eq!(harness.state().sink.diagnostics.byte_count, 0);
    let ticket = harness
        .state()
        .documents
        .registry
        .read_ticket(view)
        .unwrap();
    harness
        .state_mut()
        .documents
        .registry
        .observe(
            &ticket,
            SourceSnapshot {
                bytes: b"# External\n".to_vec(),
                version: SourceVersion([2; 32]),
            },
        )
        .unwrap();
    harness.step();
    assert!(harness.state().documents.contents.is_none());
    wait_preview(&mut harness, view);
    assert!(!harness
        .state()
        .documents
        .registry
        .view(view)
        .unwrap()
        .dirty());
}

#[test]
fn markdown_wide_preview_uses_contents_rail_and_dismisses_old_sheet() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1194.0, 834.0))
        .build_ui_state(
            |ui, app: &mut MobileApp| app.show_with_occlusion(ui, 0.0),
            app(true),
        );
    harness.run();
    let view = harness.state().documents.active.unwrap();
    harness.get_by_label("Preview").click();
    wait_preview(&mut harness, view);
    assert!(harness.query_by_label("Contents").is_none());
    assert!(harness.query_by_label("CONTENTS").is_some());
    let revision = harness
        .state()
        .documents
        .registry
        .view(view)
        .unwrap()
        .text()
        .revision();
    harness.state_mut().documents.contents = Some((view, revision));
    harness.run();
    assert!(harness.state().documents.contents.is_none());
    assert!(harness.query_by_label("Contents").is_none());
    assert!(harness.query_by_label("CONTENTS").is_some());
}

#[test]
fn markdown_contents_scrolls_to_an_offscreen_heading_without_editing() {
    let mut app = app(true);
    let view = app.documents.active.unwrap();
    let text = format!(
        "# Start\n\n{}## Destination\n\nFinal section.\n",
        "A paragraph before the destination.\n\n".repeat(80)
    );
    let ticket = app.documents.registry.read_ticket(view).unwrap();
    app.documents
        .registry
        .observe(
            &ticket,
            SourceSnapshot {
                bytes: text.as_bytes().to_vec(),
                version: SourceVersion([2; 32]),
            },
        )
        .unwrap();
    let mut harness = harness(app);
    harness.run();
    harness.get_by_label("Preview").click();
    wait_preview(&mut harness, view);
    assert!(harness.get_by_label("Destination").rect().top() > 844.0);
    harness.get_by_label("Contents").click();
    harness.run();
    harness
        .get_all_by_label("Destination")
        .last()
        .unwrap()
        .click();
    harness.run_steps(60);
    harness.run();
    assert!(harness.state().documents.contents.is_none());
    let destination = harness.get_by_label("Destination").rect();
    assert!(
        destination.top() >= 0.0 && destination.bottom() <= 844.0,
        "heading must enter the visible preview: {destination:?}"
    );
    let entry = harness.state().documents.registry.view(view).unwrap();
    assert_eq!(entry.text().text(), text);
    assert!(!entry.dirty());
    assert_eq!(harness.state().sink.diagnostics.byte_count, 0);
}

#[test]
fn markdown_contents_navigation_and_view_switch_dismiss_the_captured_sheet() {
    let mut harness = harness(app(true));
    harness.run();
    let view = harness.state().documents.active.unwrap();
    harness.get_by_label("Preview").click();
    wait_preview(&mut harness, view);
    harness.get_by_label("Contents").click();
    harness.run();
    let heading = harness.get_all_by_label("Runbook").last().unwrap();
    assert!(heading.rect().height() >= space::TOUCH);
    heading.click();
    harness.run();
    assert!(harness.state().documents.contents.is_none());
    {
        let app = harness.state_mut();
        app.documents.duplicate(view, &mut app.next_tab);
    }
    let sibling = harness.state().documents.active.unwrap();
    let original_tab = harness
        .state()
        .documents
        .views
        .iter()
        .find(|entry| entry.id == view)
        .unwrap()
        .tab;
    let sibling_tab = harness
        .state()
        .documents
        .views
        .iter()
        .find(|entry| entry.id == sibling)
        .unwrap()
        .tab;
    harness.state_mut().activate_tab(original_tab);
    harness.run();
    harness.get_by_label("Contents").click();
    harness.run();
    assert!(harness.state().documents.contents.is_some());
    harness.state_mut().activate_tab(sibling_tab);
    harness.run();
    assert!(harness.state().documents.contents.is_none());
    assert!(
        !harness
            .state()
            .documents
            .views
            .iter()
            .find(|entry| entry.id == sibling)
            .unwrap()
            .previewing
    );
    assert_eq!(
        harness.state().documents.registry.document_for(view),
        harness.state().documents.registry.document_for(sibling)
    );
    assert_eq!(
        harness
            .state()
            .documents
            .registry
            .view(sibling)
            .unwrap()
            .text()
            .to_bytes(),
        INITIAL
    );
    assert_eq!(harness.state().sink.diagnostics.byte_count, 0);
}

#[test]
#[ignore = "explicit actual-widget capture requires headless wgpu and a fresh output directory"]
fn capture_document_ui() {
    use sha2::{Digest, Sha256};
    use std::{fs, path::PathBuf, process::Command};
    let output =
        PathBuf::from(std::env::var_os("FESTERM_DOCUMENT_UI_OUT").expect("fresh capture output"));
    fs::create_dir(&output).expect("capture output must not already exist");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let git = |args: &[&str]| {
        let result = Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(result.status.success());
        String::from_utf8(result.stdout).unwrap().trim().to_owned()
    };
    let mut images = Vec::new();
    for (family, width, height, occlusion) in [
        ("phone", 390.0, 844.0, 0.0),
        ("phone-keyboard", 390.0, 844.0, 0.35),
        ("landscape", 844.0, 390.0, 0.35),
        ("tablet", 1194.0, 834.0, 0.0),
    ] {
        for state in [
            "launcher",
            "source",
            "dirty",
            "read-only",
            "conflict",
            "preview",
            "contents",
        ] {
            let mut app = app(state != "read-only");
            let view = app.documents.active.unwrap();
            let ticket = app.documents.registry.read_ticket(view).unwrap();
            app.documents.registry.observe(&ticket, SourceSnapshot {
                bytes: b"# Operations runbook\n\n## Before you start\n\nConfirm the host and the maintenance window.\nKeep credentials out of this document.\n\n## Checks\n\n- Review the deployment notes\n- Verify the current version\n- Keep a terminal available for diagnostics\n\n```sh\nprintf 'review before running\\n'\n```\n\n## Recovery\n\nStop and compare external changes before saving.\n".to_vec(),
                version: SourceVersion([2; 32]),
            }).unwrap();
            if state == "launcher" {
                app.documents.active = None;
            }
            if state == "dirty" {
                let target = app.documents.registry.target(view, 0..0).unwrap();
                app.documents
                    .registry
                    .replace(&target, &target, "Updated notes\n\n")
                    .unwrap();
                app.documents.duplicate(view, &mut app.next_tab);
            }
            if state == "conflict" {
                let target = app.documents.registry.target(view, 0..0).unwrap();
                app.documents
                    .registry
                    .replace(&target, &target, "My pending notes\n\n")
                    .unwrap();
                let ticket = app.documents.registry.read_ticket(view).unwrap();
                app.documents.registry.observe(&ticket, SourceSnapshot {
                    bytes: b"# Operations runbook\n\nThe file was updated outside fesTerm.\nReview this version before deciding.\n".to_vec(),
                    version: SourceVersion([3; 32]),
                }).unwrap();
                app.documents.compare = Some(app.documents.registry.target(view, 0..0).unwrap());
            }
            if state == "preview" || state == "contents" {
                app.documents
                    .views
                    .iter_mut()
                    .find(|entry| entry.id == view)
                    .unwrap()
                    .previewing = true;
            }
            let visible_occlusion = if state == "launcher" || state == "read-only" {
                0.0
            } else {
                occlusion
            };
            let mut harness = Harness::builder()
                .with_size(egui::vec2(width, height))
                .build_ui_state(
                    move |ui, app: &mut MobileApp| app.show_with_occlusion(ui, visible_occlusion),
                    app,
                );
            harness.ctx.set_theme(egui::ThemePreference::Dark);
            harness
                .ctx
                .all_styles_mut(|style| style.visuals.text_cursor.blink = false);
            if state == "preview" || state == "contents" {
                wait_preview(&mut harness, view);
                if state == "contents" {
                    let revision = harness
                        .state()
                        .documents
                        .registry
                        .view(view)
                        .unwrap()
                        .text()
                        .revision();
                    harness.state_mut().documents.contents = Some((view, revision));
                    harness.run();
                }
            } else {
                harness.run();
            }
            if visible_occlusion > 0.0 && matches!(state, "source" | "dirty") {
                let view = harness.state().documents.active.unwrap();
                focus(&mut harness, view, 0);
            }
            harness.remove_cursor();
            harness.run();
            let image = harness.render().expect("actual document widgets render");
            let name = format!("{family}-{state}.png");
            let path = output.join(&name);
            image.save(&path).unwrap();
            let hash: String = Sha256::digest(fs::read(&path).unwrap())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            images.push(serde_json::json!({
                "image": name, "sha256": hash, "logical_size": [width, height],
                "simulated_keyboard_occlusion_fraction": visible_occlusion,
            }));
        }
    }
    fs::write(output.join("manifest.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "source_commit": git(&["rev-parse", "HEAD"]),
        "source_dirty": !git(&["status", "--porcelain", "--untracked-files=normal"]).is_empty(),
        "capture_method": "actual MobileApp widgets through egui_kittest/headless wgpu",
        "fixture": "repository-owned source bytes and refusing non-iOS native stub; no Files grant or native I/O",
        "native_ios": false, "native_keyboard_rendered": false,
        "native_validation": "not-run", "images": images,
    })).unwrap()).unwrap();
}
