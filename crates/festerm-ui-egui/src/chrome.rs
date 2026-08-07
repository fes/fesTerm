//! Independent session-chip application chrome.
//!
//! This module implements the chip-row presentation contract in
//! `docs/gui-design.md` ("Tab Model", "Application chrome and session
//! context"): every session is an independent lozenge with visible space
//! around it, a neutral surface, a compact non-color-exclusive status
//! indicator, and a stable primary identity that transient terminal titles
//! never replace.
//!
//! This module is pure presentation. It owns no session, tab, or terminal
//! state and performs no protocol or backend work; callers translate the
//! returned [`ChromeAction`] values into application commands per
//! `docs/application-command-model.md`.

use egui::{Align, Color32, Frame, Id, Layout, RichText, ScrollArea, Stroke, Ui};

/// Opaque, content-free chip identity correlated by the application layer to
/// its own stable tab identifier. It carries no terminal content.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ChipId(pub u64);

/// Compact, non-color-exclusive connection-state vocabulary
/// (`docs/gui-design.md` "Connection states").
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChipStatus {
    Connected,
    Starting,
    Reconnecting,
    Disconnected,
    AuthRequired,
    Failed,
    Exited,
    /// Non-session application surfaces (Launcher, Settings) carry no
    /// connection state and show no status dot.
    Neutral,
}

impl ChipStatus {
    /// Semantic `status.*` role color (`docs/gui-design.md` "Semantic color
    /// roles"). These are placeholder concrete values until a theme system
    /// exists; the accessible label, not color alone, carries the meaning.
    const fn color(self) -> Color32 {
        match self {
            Self::Connected => Color32::from_rgb(0x3d, 0xc9, 0x6b),
            Self::Starting => Color32::from_rgb(0xe0, 0xb3, 0x3a),
            Self::Reconnecting => Color32::from_rgb(0x4f, 0xa8, 0xe0),
            Self::Disconnected => Color32::from_rgb(0x8a, 0x8f, 0x98),
            Self::AuthRequired => Color32::from_rgb(0xa1, 0x7b, 0xd1),
            Self::Failed => Color32::from_rgb(0xd9, 0x53, 0x4f),
            Self::Exited => Color32::from_rgb(0x6e, 0x76, 0x81),
            Self::Neutral => Color32::TRANSPARENT,
        }
    }

    /// Accessible, human-readable state name shown via hover/tooltip text so
    /// state is never encoded through color alone.
    pub const fn accessible_label(self) -> &'static str {
        match self {
            Self::Connected => "Connected",
            Self::Starting => "Starting",
            Self::Reconnecting => "Reconnecting",
            Self::Disconnected => "Disconnected",
            Self::AuthRequired => "Authentication required",
            Self::Failed => "Failed",
            Self::Exited => "Exited",
            Self::Neutral => "",
        }
    }
}

/// One chip's presentation state for one frame.
///
/// `primary` is the stable identity (`docs/gui-design.md` "Identity
/// precedence"); `secondary` is optional transient terminal-provided metadata
/// that must never replace it.
pub struct ChipViewModel {
    pub id: ChipId,
    pub primary: String,
    pub secondary: Option<String>,
    pub status: ChipStatus,
    pub closable: bool,
}

/// A user gesture translated from the chip row. The application layer maps
/// this to an `AppCommand`; this crate does not dispatch or interpret it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ChromeAction {
    Activate(ChipId),
    Close(ChipId),
    NewTab,
    OpenSettings,
    ToggleInspector,
    /// Emitted by drag-and-drop: `moved` should be relocated to sit
    /// immediately before `before` (or at the end of the row if `None`).
    /// Reordering only changes chip position; it must preserve the moved
    /// chip's identity, session, and active/inactive state
    /// (`docs/gui-design.md` "Drag-and-drop reorders independent session
    /// objects and should preserve their identity and state.").
    Reorder {
        moved: ChipId,
        before: Option<ChipId>,
    },
}

/// User-configurable chip layout mode
/// (`docs/gui-design.md` "Tab overflow and wrapping": "Wrapping must remain
/// user-configurable because some users will prefer a single scrolling
/// row.").
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChipLayout {
    /// Many chips wrap onto additional rows.
    Wrap,
    /// Chips stay on a single row; overflow scrolls horizontally instead of
    /// wrapping.
    SingleRowScroll,
}

/// Renders the top-of-window chrome band: New Tab, the independent session
/// chips, and the session-inspector toggle. Returns every gesture observed
/// this frame; callers apply at most the ones they recognize.
pub fn show(
    ui: &mut Ui,
    chips: &[ChipViewModel],
    active: ChipId,
    inspector_open: bool,
    layout: ChipLayout,
) -> Vec<ChromeAction> {
    let mut actions = Vec::new();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        if ui
            .button("+ Launcher")
            .on_hover_text("Open a new launcher tab")
            .clicked()
        {
            actions.push(ChromeAction::NewTab);
        }
        let chip_row = |ui: &mut Ui| {
            for chip in chips {
                show_chip(ui, chip, chip.id == active, &mut actions);
            }
            // A drop zone past the last chip lets a drag be released at the
            // end of the row.
            let (_, payload) = ui.dnd_drop_zone::<ChipId, _>(Frame::NONE, |ui| {
                ui.allocate_exact_size(egui::vec2(24.0, 1.0), egui::Sense::hover());
            });
            if let Some(moved) = payload {
                actions.push(ChromeAction::Reorder {
                    moved: *moved,
                    before: None,
                });
            }
        };
        match layout {
            ChipLayout::Wrap => {
                ui.horizontal_wrapped(chip_row);
            }
            ChipLayout::SingleRowScroll => {
                ScrollArea::horizontal()
                    .id_salt("chip_row_scroll")
                    .show(ui, |ui| ui.horizontal(chip_row));
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .selectable_label(inspector_open, "\u{24d8} Inspector")
                .on_hover_text("Toggle session inspector")
                .clicked()
            {
                actions.push(ChromeAction::ToggleInspector);
            }
            // Stand-in for the deliberately small future overflow menu
            // (`docs/gui-design.md` "Application chrome and session
            // context"): Settings is the only entry today.
            if ui.button("\u{22ef}").on_hover_text("Settings").clicked() {
                actions.push(ChromeAction::OpenSettings);
            }
        });
    });
    actions
}

fn show_chip(ui: &mut Ui, chip: &ChipViewModel, active: bool, actions: &mut Vec<ChromeAction>) {
    // `Frame::group` gives every chip its own bordered lozenge with visible
    // surrounding space, rather than a connected browser-style tab strip.
    let fill = if active {
        ui.visuals().selection.bg_fill
    } else {
        ui.visuals().widgets.inactive.weak_bg_fill
    };
    let stroke = if active {
        Stroke::new(1.5, ui.visuals().selection.stroke.color)
    } else {
        ui.visuals().widgets.noninteractive.bg_stroke
    };
    let frame = Frame::group(ui.style()).fill(fill).stroke(stroke);
    let drag_id = Id::new("chrome_chip_handle").with(chip.id.0);

    // The whole chip is a drop zone (so releasing a dragged chip anywhere
    // over it reorders the row), but only a small dedicated grip handle is a
    // drag source. Making the entire chip draggable would let a drag gesture
    // that starts on the label or close button consume the click before the
    // inner widget sees it, which would break ordinary activation and close
    // clicks (`docs/gui-design.md` "Tab close controls should avoid
    // accidental activation or closure.").
    let (drop_response, payload) = ui.dnd_drop_zone::<ChipId, _>(frame, |ui| {
        ui.horizontal(|ui| {
            ui.dnd_drag_source(drag_id, chip.id, |ui| {
                ui.label(RichText::new("\u{28ff}").weak())
                    .on_hover_text("Drag to reorder");
            });
            if !matches!(chip.status, ChipStatus::Neutral) {
                ui.label(RichText::new("\u{25cf}").color(chip.status.color()))
                    .on_hover_text(chip.status.accessible_label());
            }
            let label = if active {
                RichText::new(&chip.primary).strong()
            } else {
                RichText::new(&chip.primary)
            };
            if ui.selectable_label(active, label).clicked() {
                actions.push(ChromeAction::Activate(chip.id));
            }
            if let Some(secondary) = &chip.secondary {
                ui.label(RichText::new(secondary).weak().small());
            }
            if chip.closable && ui.small_button("\u{2715}").on_hover_text("Close").clicked() {
                actions.push(ChromeAction::Close(chip.id));
            }
        });
    });
    let _ = drop_response;
    if let Some(moved) = payload {
        if *moved != chip.id {
            actions.push(ChromeAction::Reorder {
                moved: *moved,
                before: Some(chip.id),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::{kittest::Queryable, Harness};

    use super::*;

    #[test]
    fn accessible_labels_never_rely_on_color_alone() {
        for status in [
            ChipStatus::Connected,
            ChipStatus::Starting,
            ChipStatus::Reconnecting,
            ChipStatus::Disconnected,
            ChipStatus::AuthRequired,
            ChipStatus::Failed,
            ChipStatus::Exited,
        ] {
            assert!(!status.accessible_label().is_empty());
        }
        assert!(ChipStatus::Neutral.accessible_label().is_empty());
    }

    fn chip(id: u64, primary: &str) -> ChipViewModel {
        ChipViewModel {
            id: ChipId(id),
            primary: primary.to_owned(),
            secondary: None,
            status: ChipStatus::Connected,
            closable: true,
        }
    }

    /// Harness state for headless UI-level coverage of `show()`: the chip
    /// row plus every `ChromeAction` observed across frames, so a test can
    /// drive real pointer/keyboard events and assert on the resulting
    /// gestures rather than reimplementing chrome logic.
    struct ChromeHarnessState {
        chips: Vec<ChipViewModel>,
        active: ChipId,
        layout: ChipLayout,
        observed: Vec<ChromeAction>,
    }

    fn harness(state: ChromeHarnessState) -> Harness<'static, ChromeHarnessState> {
        Harness::builder()
            .with_size(egui::vec2(900.0, 200.0))
            .build_ui_state(
                |ui, state: &mut ChromeHarnessState| {
                    let actions = show(ui, &state.chips, state.active, false, state.layout);
                    state.observed.extend(actions);
                },
                state,
            )
    }

    #[test]
    fn clicking_a_chip_label_emits_an_activate_action() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one"), chip(2, "two")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });

        harness.get_by_label("two").click();
        harness.run();

        assert!(harness
            .state()
            .observed
            .contains(&ChromeAction::Activate(ChipId(2))));
    }

    #[test]
    fn clicking_a_chip_close_button_emits_a_close_action() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one"), chip(2, "two")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });

        harness.get_all_by_label("\u{2715}").nth(1).unwrap().click();
        harness.run();

        assert!(harness
            .state()
            .observed
            .contains(&ChromeAction::Close(ChipId(2))));
    }

    #[test]
    fn new_launcher_button_emits_a_new_tab_action() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });

        harness.get_by_label_contains("Launcher").click();
        harness.run();

        assert!(harness.state().observed.contains(&ChromeAction::NewTab));
    }

    #[test]
    fn inspector_toggle_emits_a_toggle_inspector_action() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });

        harness.get_by_label_contains("Inspector").click();
        harness.run();

        assert!(harness
            .state()
            .observed
            .contains(&ChromeAction::ToggleInspector));
    }

    #[test]
    fn dragging_one_chip_onto_another_emits_a_reorder_action() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one"), chip(2, "two"), chip(3, "three")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });
        harness.run();

        let handles: Vec<_> = harness
            .get_all_by_label("\u{28ff}")
            .map(|node| node.rect().center())
            .collect();
        let from = handles[0]; // "one"'s drag handle
        let to = harness.get_by_label("three").rect().center();

        // Drag "one" onto "three": press, move past the drag threshold over
        // several frames, then release on the target chip.
        harness.drag_at(from);
        harness.run();
        let steps = 8;
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            harness.hover_at(from + (to - from) * t);
            harness.run();
        }
        harness.drop_at(to);
        harness.run();

        assert!(
            harness.state().observed.iter().any(|action| matches!(
                action,
                ChromeAction::Reorder {
                    moved: ChipId(1),
                    before: Some(ChipId(3)),
                }
            )),
            "observed actions: {:?}",
            harness.state().observed
        );
    }
}
