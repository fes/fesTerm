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
fn terminal_keyboards_offer_dictation_as_one_non_repeating_action() {
    for kind in [KeyboardKind::HoldSymbols, KeyboardKind::ShellRow] {
        for width in [320.0, 390.0, 768.0] {
            let mut harness = Harness::builder()
                .with_size(egui::vec2(width, 844.0))
                .build_ui_state(
                    move |ui, (keyboard, actions): &mut (TerminalKeyboard, Vec<KeyRowAction>)| {
                        let output = keyboard.show(
                            ui,
                            kind,
                            &KeyRowState {
                                control: false,
                                alt: false,
                                shift: false,
                                panel_open: false,
                                keyboard_visible: true,
                                paste: true,
                            },
                            700.0,
                        );
                        if output.action != KeyRowAction::None {
                            actions.push(output.action);
                        }
                    },
                    (TerminalKeyboard::default(), Vec::new()),
                );
            harness.run();
            let mic = harness.get_by_label("Mic").rect();
            assert!(mic.left() >= 0.0 && mic.right() <= width);
            assert!(mic.width() >= 24.0 && mic.height() >= 24.0);
            for pair in ["Left", "Up", "Down", "Right", "Mic", "Extended keys"].windows(2) {
                let left = harness.get_by_label(pair[0]).rect();
                let right = harness.get_by_label(pair[1]).rect();
                assert!(left.right() < right.left(), "{kind:?}: {pair:?} at {width}");
                assert_eq!(left.top(), right.top());
            }
            for label in ["Left", "Up", "Down", "Right"] {
                harness.get_by_label(label).click();
                harness.run();
            }
            assert_eq!(
                harness.state().1,
                [
                    KeyRowAction::Key(Key::ArrowLeft),
                    KeyRowAction::Key(Key::ArrowUp),
                    KeyRowAction::Key(Key::ArrowDown),
                    KeyRowAction::Key(Key::ArrowRight),
                ]
            );
            harness.state_mut().1.clear();
            let now = harness.ctx.input(|input| input.time);
            harness.event(egui::Event::PointerMoved(mic.center()));
            harness.event(pointer(true, mic.center()));
            harness.step();
            assert_eq!(
                harness.state().0.keys.preview_glyph(),
                Some(&Glyph::Icon(Icon::Microphone))
            );
            harness.input_mut().time = Some(now + REPEAT_DELAY + REPEAT_INTERVAL);
            harness.step();
            assert!(
                harness.state().1.is_empty(),
                "holding Mic must not start capture"
            );
            harness.event(pointer(false, mic.center()));
            harness.run();
            assert_eq!(harness.state().1, [KeyRowAction::Dictate]);
        }
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
    assert_eq!(harness.state().1, [KeyRowAction::Key(Key::Character('%'))]);
    harness.run();
    harness.get_by_label("q").click();
    harness.run();
    assert_eq!(
        harness.state().1,
        [
            KeyRowAction::Key(Key::Character('%')),
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
        Some(&Glyph::text("%"))
    );
    assert_eq!(harness.state().1, [KeyRowAction::Key(Key::Character('%'))]);
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
            KeyRowAction::Key(Key::Character('%')),
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

#[test]
fn terminal_keyboard_puts_globe_beside_symbols_and_extended_keys_in_utility_row() {
    let mut harness = harness();
    harness.run();
    let globe = harness.get_by_label("Switch keyboard").rect();
    let space = harness.get_by_label("Space").rect();
    let symbols = harness.get_by_label("#+=").rect();
    assert_eq!(globe.top(), space.top());
    assert!(globe.right() < symbols.left());
    let extended = harness.get_by_label("Extended keys").rect();
    assert_eq!(extended.top(), harness.get_by_label("Esc").rect().top());
    harness.get_by_label("Switch keyboard").click();
    harness.run();
    harness.get_by_label("Extended keys").click();
    harness.run();
    assert_eq!(
        harness.state().1,
        [KeyRowAction::SwitchKeyboard, KeyRowAction::TogglePanel]
    );
}

#[test]
fn terminal_keyboard_symbol_page_reaches_every_ascii_symbol() {
    let shifted_numbers = "!@#$%^&*()";
    let reachable: String = SYMBOLS.concat() + shifted_numbers + ",.";
    let missing: String = (b'!'..=b'~')
        .map(char::from)
        .filter(|c| c.is_ascii_punctuation() && !reachable.contains(*c))
        .collect();
    assert_eq!(
        missing, "",
        "#+= page must offer every symbol, quotes included"
    );
}

#[test]
fn terminal_keyboard_holding_period_types_question_mark() {
    let mut harness = harness();
    harness.run();
    let period = harness.get_by_label(".").rect().center();
    let now = harness.ctx.input(|input| input.time);
    for (elapsed, events) in [
        (
            0.01,
            vec![egui::Event::PointerMoved(period), pointer(true, period)],
        ),
        (0.8, vec![]),
        (0.9, vec![pointer(false, period)]),
    ] {
        harness.input_mut().time = Some(now + elapsed);
        for event in events {
            harness.event(event);
        }
        harness.step();
    }
    assert_eq!(harness.state().1, [KeyRowAction::Key(Key::Character('?'))]);
}

fn split_harness(
    size: egui::Vec2,
    kind: KeyboardKind,
    split: SplitMode,
    hinge: Option<Hinge>,
) -> Harness<'static, (TerminalKeyboard, Vec<KeyRowAction>)> {
    let keyboard = TerminalKeyboard {
        split,
        hinge,
        ..TerminalKeyboard::default()
    };
    Harness::builder().with_size(size).build_ui_state(
        move |ui, (keyboard, actions): &mut (TerminalKeyboard, Vec<KeyRowAction>)| {
            let output = keyboard.show(
                ui,
                kind,
                &KeyRowState {
                    control: false,
                    alt: false,
                    shift: false,
                    panel_open: false,
                    keyboard_visible: true,
                    paste: true,
                },
                size.y * 0.55,
            );
            if output.action != KeyRowAction::None {
                actions.push(output.action);
            }
        },
        (keyboard, Vec::new()),
    )
}

#[test]
fn split_halves_follow_width_orientation_hinge_and_preference() {
    let area =
        |width: f32| Rect::from_min_size(egui::pos2(7.0, 500.0), egui::vec2(width - 14.0, 300.0));
    let screen =
        |width: f32, height: f32| Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
    // Phones and portrait tablets keep the full keyboard; landscape tablets split.
    for (width, height) in [(390.0, 844.0), (844.0, 390.0), (820.0, 1180.0)] {
        assert_eq!(
            split_halves(area(width), screen(width, height), None, SplitMode::Auto),
            None
        );
    }
    // The 900 pt threshold is the window width, not the inset keyboard area.
    assert!(split_halves(area(900.0), screen(900.0, 700.0), None, SplitMode::Auto).is_some());
    assert_eq!(
        split_halves(area(899.0), screen(899.0, 700.0), None, SplitMode::Auto),
        None
    );
    let [left, right] =
        split_halves(area(1180.0), screen(1180.0, 820.0), None, SplitMode::Auto).unwrap();
    assert_eq!((left.left(), right.right()), (7.0, 1173.0));
    assert_eq!(left.width(), right.width());
    assert!(left.width() <= MAX_HALF && right.left() - left.right() >= MIN_CENTER_GAP);
    assert_eq!(
        split_halves(area(1180.0), screen(1180.0, 820.0), None, SplitMode::Never),
        None
    );
    // Always splits narrower screens while both halves fit, never below that.
    assert!(split_halves(area(600.0), screen(600.0, 900.0), None, SplitMode::Always).is_some());
    assert_eq!(
        split_halves(area(390.0), screen(390.0, 844.0), None, SplitMode::Always),
        None
    );
    // A vertical hinge splits even a portrait-shaped spanned window, and keys
    // stay clear of it on both sides even when it is off-center.
    let hinge = Hinge {
        left: 0.55,
        right: 0.58,
    };
    let [left, right] = split_halves(
        area(1000.0),
        screen(1000.0, 1200.0),
        Some(hinge),
        SplitMode::Auto,
    )
    .unwrap();
    assert!(left.right() <= 550.0 - HINGE_CLEARANCE);
    assert!(right.left() >= 580.0 + HINGE_CLEARANCE);
    assert_eq!(left.width(), right.width());
    assert_eq!(
        split_halves(
            area(1000.0),
            screen(1000.0, 1200.0),
            Some(hinge),
            SplitMode::Never
        ),
        None
    );
    // A hinge too close to an edge for a usable half, or malformed, keeps the
    // full keyboard rather than producing a sliver.
    for hinge in [
        Hinge {
            left: 0.1,
            right: 0.12,
        },
        Hinge {
            left: f32::NAN,
            right: 0.5,
        },
        Hinge {
            left: 0.6,
            right: 0.4,
        },
    ] {
        assert_eq!(
            split_halves(
                area(1000.0),
                screen(1000.0, 1200.0),
                Some(hinge),
                SplitMode::Auto
            ),
            None,
            "{hinge:?}"
        );
    }
}

#[test]
fn split_keyboard_puts_every_key_in_one_half_and_types_from_both() {
    let size = egui::vec2(1180.0, 820.0);
    for kind in [KeyboardKind::HoldSymbols, KeyboardKind::ShellRow] {
        let mut harness = split_harness(size, kind, SplitMode::Auto, None);
        harness.run();
        let centre = size.x / 2.0;
        let half = |label: &str| {
            let rect = harness.get_by_label(label).rect();
            assert!(
                rect.right() < centre - MIN_CENTER_GAP / 2.0 + 0.5
                    || rect.left() > centre + MIN_CENTER_GAP / 2.0 - 0.5,
                "{kind:?}: {label} crosses the centre gap: {rect:?}"
            );
            rect.center().x < centre
        };
        for label in [
            "Esc",
            "Tab",
            "Ctrl",
            "Alt",
            "Shift",
            "Mic",
            "1",
            "5",
            "q",
            "t",
            "a",
            "g",
            "z",
            "v",
            "Typing-row Shift",
            "Switch keyboard",
            "#+=",
            ",",
        ] {
            assert!(half(label), "{kind:?}: {label} belongs on the left half");
        }
        for label in [
            "Left",
            "Up",
            "Down",
            "Right",
            "Extended keys",
            "6",
            "0",
            "y",
            "p",
            "h",
            "l",
            "b",
            "m",
            "Backspace",
            ".",
            "Return",
        ] {
            assert!(!half(label), "{kind:?}: {label} belongs on the right half");
        }
        let spaces = harness
            .get_all_by_label("Space")
            .map(|node| node.rect().width())
            .collect::<Vec<_>>();
        assert_eq!(spaces.len(), 2, "{kind:?}: each half has its own Space");
        assert!((spaces[0] - spaces[1]).abs() < 0.5, "{kind:?}: {spaces:?}");
        // Every character key in both halves shares one width.
        let width = harness.get_by_label("q").rect().width();
        for label in [
            "1", "0", "t", "y", "p", "a", "g", "h", "l", "z", "v", "b", "m",
        ] {
            let other = harness.get_by_label(label).rect().width();
            assert!(
                (other - width).abs() < 0.5,
                "{kind:?}: {label} {other} vs {width}"
            );
        }
        let small = harness.get_by_label("Switch keyboard").rect().width();
        for label in ["#+=", ",", "."] {
            let other = harness.get_by_label(label).rect().width();
            assert!(
                (other - small).abs() < 0.5,
                "{kind:?}: {label} {other} vs {small}"
            );
        }
    }
    // The right middle row ends flush with `p` instead of leaving a hole after `l`.
    let mut harness = split_harness(size, KeyboardKind::HoldSymbols, SplitMode::Auto, None);
    harness.run();
    let quote = harness.get_by_label("'").rect();
    let p_key = harness.get_by_label("p").rect();
    assert!(
        (quote.right() - p_key.right()).abs() < 0.5,
        "{quote:?} {p_key:?}"
    );
    assert!(quote.left() > harness.get_by_label("l").rect().right());
    harness.get_by_label("'").click();
    harness.run();
    assert_eq!(harness.state().1, [KeyRowAction::Key(Key::Character('\''))]);
    let mut harness = split_harness(size, KeyboardKind::HoldSymbols, SplitMode::Auto, None);
    harness.run();
    for label in ["t", "y"] {
        harness.get_by_label(label).click();
        harness.run();
    }
    for space in harness
        .get_all_by_label("Space")
        .map(|node| node.rect().center())
        .collect::<Vec<_>>()
    {
        harness.event(pointer(true, space));
        harness.step();
        harness.event(pointer(false, space));
        harness.run();
    }
    assert_eq!(
        harness.state().1,
        [
            KeyRowAction::Key(Key::Character('t')),
            KeyRowAction::Key(Key::Character('y')),
            KeyRowAction::Key(Key::Character(' ')),
            KeyRowAction::Key(Key::Character(' ')),
        ]
    );
}

#[test]
fn split_keyboard_keeps_keys_off_a_reported_hinge_and_full_mode_overrides_it() {
    let size = egui::vec2(1100.0, 1300.0);
    let hinge = Hinge {
        left: 0.48,
        right: 0.52,
    };
    let mut harness = split_harness(size, KeyboardKind::ShellRow, SplitMode::Auto, Some(hinge));
    harness.run();
    let fold = (size.x * hinge.left - HINGE_CLEARANCE)..=(size.x * hinge.right + HINGE_CLEARANCE);
    for label in [
        "t",
        "y",
        "g",
        "h",
        "v",
        "b",
        "5",
        "6",
        "Mic",
        "Left",
        "Space",
        "Backspace",
    ] {
        for node in harness.get_all_by_label(label) {
            let rect = node.rect();
            assert!(
                rect.right() <= *fold.start() + 0.5 || rect.left() >= *fold.end() - 0.5,
                "{label} overlaps the hinge: {rect:?}"
            );
        }
    }
    let mut harness = split_harness(size, KeyboardKind::ShellRow, SplitMode::Never, Some(hinge));
    harness.run();
    assert_eq!(harness.get_all_by_label("Space").count(), 1);
}

#[test]
fn split_keyboard_keeps_every_key_of_both_layers_once_and_off_the_centre() {
    let size = egui::vec2(1180.0, 820.0);
    let mut labels = vec![
        "Esc",
        "Tab",
        "Ctrl",
        "Alt",
        "Shift",
        "Mic",
        "Left",
        "Up",
        "Down",
        "Right",
        "Extended keys",
        "Hide keyboard",
        "Typing-row Shift",
        "Backspace",
        "Switch keyboard",
        "#+=",
        "ABC",
        "Space",
        "Return",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    for row in [SHELL_ROW, "1234567890,.^"]
        .into_iter()
        .chain(LETTERS)
        .chain(SYMBOLS)
    {
        labels.extend(row.chars().map(String::from));
    }
    labels.sort();
    labels.dedup();
    for kind in [KeyboardKind::HoldSymbols, KeyboardKind::ShellRow] {
        for symbols in [false, true] {
            let counts = |split: SplitMode| {
                let mut harness = split_harness(size, kind, split, None);
                harness.state_mut().0.symbols = symbols;
                harness.run();
                labels
                    .iter()
                    .map(|label| {
                        let rects = harness
                            .query_all_by_label(label)
                            .map(|node| node.rect())
                            .collect::<Vec<_>>();
                        for rect in &rects {
                            let centre = size.x / 2.0;
                            assert!(
                                split == SplitMode::Never
                                    || rect.right() < centre - MIN_CENTER_GAP / 2.0 + 0.5
                                    || rect.left() > centre + MIN_CENTER_GAP / 2.0 - 0.5,
                                "{kind:?} symbols={symbols}: {label} crosses the centre"
                            );
                        }
                        rects.len()
                    })
                    .collect::<Vec<_>>()
            };
            let (full, split) = (counts(SplitMode::Never), counts(SplitMode::Always));
            assert!(full.iter().filter(|&&count| count > 0).count() > 40);
            let extra = if symbols { "^" } else { "'" };
            for ((label, full), split) in labels.iter().zip(full).zip(split) {
                let added = usize::from(label == "Space" || label == extra);
                assert_eq!(
                    split,
                    full + added,
                    "{kind:?} symbols={symbols}: {label} appears {split} times, full board {full}"
                );
            }
        }
    }
}

#[test]
fn split_keyboard_hold_alternates_follow_each_half() {
    let mut harness = split_harness(
        egui::vec2(1180.0, 820.0),
        KeyboardKind::HoldSymbols,
        SplitMode::Always,
        None,
    );
    harness.run();
    for label in ["y", "'", ".", ","] {
        let key = harness.get_by_label(label).rect().center();
        let now = harness.ctx.input(|input| input.time);
        for (elapsed, events) in [
            (
                0.01,
                vec![egui::Event::PointerMoved(key), pointer(true, key)],
            ),
            (0.8, vec![]),
            (0.9, vec![pointer(false, key)]),
        ] {
            harness.input_mut().time = Some(now + elapsed);
            for event in events {
                harness.event(event);
            }
            harness.step();
        }
    }
    assert_eq!(
        harness.state().1,
        [']', '"', '?', '`'].map(|c| KeyRowAction::Key(Key::Character(c)))
    );
}
