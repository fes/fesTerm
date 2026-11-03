use super::*;
use crate::key_press::{Glyph, REPEAT_DELAY, REPEAT_INTERVAL};
use egui_kittest::{kittest::Queryable, Harness};

fn harness() -> Harness<'static, (TerminalKeyboard, Vec<KeyRowAction>)> {
    Harness::builder()
        .with_size(egui::vec2(390.0, 844.0))
        .build_ui_state(
            |ui, (keyboard, actions): &mut (TerminalKeyboard, Vec<KeyRowAction>)| {
                let available_height = ui.available_height();
                let output = keyboard.show(
                    ui,
                    KeyboardKind::HoldSymbols,
                    &KeyRowState {
                        control: false,
                        alt: false,
                        shift: false,
                        panel_open: false,
                        keyboard_visible: true,
                        paste: true,
                    },
                    available_height,
                );
                if output.action != KeyRowAction::None {
                    actions.push(output.action);
                }
            },
            (TerminalKeyboard::default(), Vec::new()),
        )
}

fn pointer(pressed: bool, pos: egui::Pos2) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

#[test]
fn terminal_keyboard_hold_emits_one_alternate_and_never_its_base_on_release() {
    let mut harness = harness();
    harness.run();
    let q = harness.get_by_label("q").rect().center();
    let now = harness.ctx.input(|input| input.time);
    for (elapsed, events) in [
        (0.01, vec![egui::Event::PointerMoved(q), pointer(true, q)]),
        (0.42, vec![]),
        (0.8, vec![]),
        (0.9, vec![pointer(false, q)]),
    ] {
        harness.input_mut().time = Some(now + elapsed);
        for event in events {
            harness.event(event);
        }
        harness.step();
    }
    assert_eq!(harness.state().1, [KeyRowAction::Key(Key::Character('/'))]);
    harness.run();
    harness.get_by_label("q").click();
    harness.run();
    assert_eq!(
        harness.state().1,
        [
            KeyRowAction::Key(Key::Character('/')),
            KeyRowAction::Key(Key::Character('q')),
        ]
    );
}

#[test]
fn terminal_keyboard_previews_track_taps_and_alternates_without_early_input() {
    let mut harness = harness();
    harness.run();
    let q = harness.get_by_label("q").rect().center();
    harness.event(egui::Event::PointerMoved(q));
    harness.event(pointer(true, q));
    harness.step();
    assert_eq!(
        harness.state().0.keys.preview_glyph(),
        Some(&Glyph::text("q"))
    );
    assert!(harness.state().1.is_empty());
    let now = harness.ctx.input(|input| input.time);
    harness.input_mut().time = Some(now + 0.42);
    harness.step();
    assert_eq!(
        harness.state().0.keys.preview_glyph(),
        Some(&Glyph::text("/"))
    );
    assert_eq!(harness.state().1, [KeyRowAction::Key(Key::Character('/'))]);
    harness.event(pointer(false, q));
    harness.step();
    assert!(harness.state().0.keys.preview_glyph().is_none());
    assert_eq!(harness.state().1.len(), 1);

    harness.run();
    harness.state_mut().0.symbols = true;
    harness.run();
    let bracket = harness.get_by_label("[").rect().center();
    harness.event(egui::Event::PointerMoved(bracket));
    harness.event(pointer(true, bracket));
    harness.step();
    assert_eq!(
        harness.state().0.keys.preview_glyph(),
        Some(&Glyph::text("["))
    );
    assert_eq!(
        harness.state().1.len(),
        2,
        "keys without a hold alternate type on press"
    );
    let now = harness.ctx.input(|input| input.time);
    for (elapsed, total) in [
        (REPEAT_DELAY - 0.05, 2),
        (REPEAT_DELAY, 3),
        (REPEAT_DELAY + REPEAT_INTERVAL / 2.0, 3),
        (REPEAT_DELAY + REPEAT_INTERVAL, 4),
    ] {
        harness.input_mut().time = Some(now + elapsed);
        harness.step();
        assert_eq!(harness.state().1.len(), total, "at {elapsed}s");
    }
    harness.event(pointer(false, bracket));
    harness.step();
    assert!(harness.state().0.keys.preview_glyph().is_none());
    assert_eq!(
        harness.state().1,
        [
            KeyRowAction::Key(Key::Character('/')),
            KeyRowAction::Key(Key::Character('[')),
            KeyRowAction::Key(Key::Character('[')),
            KeyRowAction::Key(Key::Character('[')),
        ]
    );
}

#[test]
fn terminal_keyboard_previews_utility_keys_and_repeats_held_return() {
    let mut harness = harness();
    harness.run();
    for (label, glyph) in [
        ("Ctrl", Glyph::text("Ctrl")),
        ("Up", Glyph::Arrow(Key::ArrowUp)),
        ("Backspace", Glyph::Backspace),
        (
            "Switch keyboard",
            Glyph::Icon(festerm_ui_egui::icon::Icon::RemoteGlobe),
        ),
    ] {
        let center = harness.get_by_label(label).rect().center();
        harness.event(egui::Event::PointerMoved(center));
        harness.event(pointer(true, center));
        harness.step();
        assert_eq!(
            harness.state().0.keys.preview_glyph(),
            Some(&glyph),
            "{label}"
        );
        harness.event(pointer(false, center));
        harness.run();
    }
    harness.state_mut().1.clear();
    let enter = harness.get_by_label("Return").rect().center();
    let now = harness.ctx.input(|input| input.time);
    harness.event(egui::Event::PointerMoved(enter));
    harness.input_mut().time = Some(now + 0.01);
    harness.event(pointer(true, enter));
    harness.step();
    assert_eq!(
        harness.state().1,
        [KeyRowAction::Key(Key::Enter)],
        "Return emits on press"
    );
    harness.input_mut().time = Some(now + 0.01 + REPEAT_DELAY - 0.05);
    harness.step();
    assert_eq!(harness.state().1.len(), 1, "no repeat before the delay");
    harness.input_mut().time = Some(now + 0.01 + REPEAT_DELAY + REPEAT_INTERVAL);
    harness.step();
    harness.event(pointer(false, enter));
    harness.step();
    assert_eq!(harness.state().1, [KeyRowAction::Key(Key::Enter); 2]);
}

#[test]
fn terminal_keyboard_hold_and_repeat_cancel_without_a_late_key_or_catchup_burst() {
    for cancellation in ["leave", "touch-cancel", "blur", "resize", "switch"] {
        let mut harness = harness();
        harness.run();
        let q = harness.get_by_label("q").rect().center();
        let now = harness.ctx.input(|input| input.time);
        harness.input_mut().time = Some(now + 0.01);
        harness.event(egui::Event::PointerMoved(q));
        harness.event(pointer(true, q));
        harness.step();
        harness.input_mut().time = Some(now + 0.2);
        match cancellation {
            "leave" => harness.event(egui::Event::PointerMoved(q + egui::vec2(15.0, 0.0))),
            "touch-cancel" => harness.event(egui::Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(1),
                phase: egui::TouchPhase::Cancel,
                pos: q,
                force: None,
            }),
            "blur" => harness.input_mut().focused = false,
            "resize" => {
                harness.set_size(egui::vec2(320.0, 568.0));
            }
            _ => harness.state_mut().0.cancel(),
        }
        harness.step();
        assert!(
            harness.state().0.keys.preview_glyph().is_none(),
            "{cancellation}"
        );
        harness.input_mut().time = Some(now + 1.0);
        harness.step();
        harness.event(pointer(false, q));
        harness.step();
        assert!(harness.state().1.is_empty(), "{cancellation}");
    }

    let mut harness = harness();
    harness.run();
    let backspace = harness.get_by_label("Backspace").rect().center();
    let now = harness.ctx.input(|input| input.time);
    harness.event(egui::Event::PointerMoved(backspace));
    harness.event(pointer(true, backspace));
    harness.step();
    assert_eq!(harness.state().1.len(), 1);
    harness.input_mut().time = Some(now + 10.0);
    harness.step();
    assert_eq!(
        harness.state().1.len(),
        2,
        "one repeat, not a delayed burst; held={:?}, blocked={}, down={:?}",
        harness
            .state()
            .0
            .keys
            .press
            .as_ref()
            .map(|p| (p.id, p.next, p.emitted)),
        harness.state().0.keys.blocked,
        harness
            .ctx
            .input(|input| (input.pointer.any_down(), input.pointer.interact_pos())),
    );
    harness.event(pointer(false, backspace));
    harness.step();
    harness.run();
    assert_eq!(harness.state().1, [KeyRowAction::Key(Key::Backspace); 2]);
}
