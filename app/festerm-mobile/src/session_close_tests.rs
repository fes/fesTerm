//! Closing sessions from the header chips (long press, then confirm) and from
//! the sessions list (swipe left, then tap Close).

use super::*;
use egui_kittest::{kittest::Queryable, Harness};

fn pointer(pressed: bool, pos: egui::Pos2) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

fn app_with_background_tab() -> (Rc<RefCell<MobileApp>>, u64) {
    let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
        active: true,
        ..Default::default()
    })));
    let background = app.push_tab("Build box".to_owned(), None);
    (Rc::new(RefCell::new(app)), background)
}

fn harness(app: &Rc<RefCell<MobileApp>>) -> Harness<'static, ()> {
    let displayed = app.clone();
    Harness::builder()
        .with_size(egui::vec2(390.0, 844.0))
        .build_ui_state(
            move |ui, _: &mut ()| displayed.borrow_mut().show_with_occlusion(ui, 0.35),
            (),
        )
}

fn widest(harness: &Harness<'_, ()>, label: &str) -> egui::Rect {
    harness
        .get_all_by_label(label)
        .map(|node| node.rect())
        .max_by(|a, b| a.width().total_cmp(&b.width()))
        .expect(label)
}

#[test]
fn long_pressing_a_session_chip_asks_before_closing_it() {
    let (app, background) = app_with_background_tab();
    let active = app.borrow().active_tab;
    let mut harness = harness(&app);
    harness.run();

    let chip = harness.get_by_label("Build box").rect().center();
    let now = harness.ctx.input(|input| input.time);
    for (elapsed, events) in [
        (
            0.01,
            vec![egui::Event::PointerMoved(chip), pointer(true, chip)],
        ),
        (0.3, vec![]),
        (0.6, vec![]),
        (0.7, vec![pointer(false, chip)]),
    ] {
        harness.input_mut().time = Some(now + elapsed);
        for event in events {
            harness.event(event);
        }
        harness.step();
    }
    harness.run();
    assert_eq!(app.borrow().confirm_close, Some(background));
    assert_eq!(
        app.borrow().active_tab,
        active,
        "the release ending a long press is not a tap"
    );

    harness.get_by_label("Cancel").click();
    harness.run();
    assert_eq!(app.borrow().confirm_close, None);
    assert_eq!(app.borrow().tabs.len(), 2);

    app.borrow_mut().confirm_close = Some(background);
    harness.run();
    harness.get_by_label("Close session").click();
    harness.run();
    assert!(app.borrow().tabs.iter().all(|tab| tab.id != background));
    assert_eq!(app.borrow().active_tab, active);
}

#[test]
fn swiping_a_session_row_left_reveals_close() {
    let (app, background) = app_with_background_tab();
    app.borrow_mut().shell.sessions_sheet = true;
    let mut harness = harness(&app);
    harness.run();

    let row = widest(&harness, "Build box");
    let start = row.center();
    harness.event(egui::Event::PointerMoved(start));
    harness.event(pointer(true, start));
    harness.step();
    for dx in [-20.0, -60.0, -100.0] {
        harness.event(egui::Event::PointerMoved(start + egui::vec2(dx, 1.0)));
        harness.step();
    }
    harness.event(pointer(false, start + egui::vec2(-100.0, 1.0)));
    harness.run();
    assert_eq!(app.borrow().tabs.len(), 2, "swiping only reveals Close");
    assert!(app.borrow().shell.sessions_sheet);

    let close = harness
        .get_all_by_label("Close")
        .map(|node| node.rect())
        .find(|rect| rect.right() >= row.right() - 1.0 && rect.top() == row.top())
        .expect("revealed Close button");
    assert!((close.width() - widgets::SWIPE_ACTION_WIDTH).abs() < 1.0);
    harness.event(egui::Event::PointerMoved(close.center()));
    harness.event(pointer(true, close.center()));
    harness.event(pointer(false, close.center()));
    harness.run();
    assert!(app.borrow().tabs.iter().all(|tab| tab.id != background));
}
