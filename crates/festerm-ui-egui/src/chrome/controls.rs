//! Toolbar chip-add/scroll controls and the trailing window-control icons
//! (minimize/maximize/close/search/overflow) painted into the chrome row.
//! Split out of `chrome.rs`; see that module's docs for the presentation
//! contract these controls live under.

use egui::{vec2, Color32, Popup, Sense, Stroke, Ui, WidgetInfo, WidgetType};

use crate::icon::{self, Icon};

use super::{
    ChromeAction, CHIP_ACTIVE_FILL, CHIP_CORNER_RADIUS, CHIP_INACTIVE_FILL, CHIP_INACTIVE_OUTLINE,
    CHIP_SCROLL_CONTROL_WIDTH, CHROME_CLOSE_HOVER, CHROME_CONTROL_SIZE, CHROME_ICON_COLOR,
    CHROME_ICON_COLOR_HOVERED,
};

/// Compact "add chip" control placed right after the last chip, painted
/// with the same chip-style rounded outline as an inactive chip (mockup:
/// the `+` control reads as a small chip in its own right, not a bare
/// icon floating in the row) - the sole way to open a new Launcher tab
/// from the chrome row (`AGENTS.md`: no duplicate widget-specific copies
/// of the same operation) - an earlier full "+ Launcher" chip-style
/// button duplicated this control and was removed as redundant.
pub(super) fn paint_new_chip_button(
    ui: &mut Ui,
    chip_row_height: f32,
    actions: &mut Vec<ChromeAction>,
) {
    let size = chip_row_height;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "New tab"));
    let hovered = response.hovered();
    // Hover feedback is communicated by matching the active chip's lighter
    // fill, not by brightening the outline - the outline stays the fixed
    // `CHIP_INACTIVE_OUTLINE` regardless of hover state.
    let fill = if hovered {
        CHIP_ACTIVE_FILL
    } else {
        CHIP_INACTIVE_FILL
    };
    ui.painter()
        .rect_filled(rect, CHIP_CORNER_RADIUS as f32, fill);
    ui.painter().rect_stroke(
        rect,
        CHIP_CORNER_RADIUS as f32,
        Stroke::new(1.0, CHIP_INACTIVE_OUTLINE),
        egui::StrokeKind::Inside,
    );
    let color = if hovered {
        CHROME_ICON_COLOR_HOVERED
    } else {
        CHROME_ICON_COLOR
    };
    icon::paint(ui.painter(), Icon::NewSession, rect.shrink(7.0), color);
    let response = response.on_hover_text("New tab");
    if response.clicked() {
        actions.push(ChromeAction::NewTab);
    }
}

pub(super) fn paint_chip_scroll_control(ui: &mut Ui, right: bool, height: f32) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(vec2(CHIP_SCROLL_CONTROL_WIDTH, height), Sense::click());
    let label = if right {
        "Scroll chips right"
    } else {
        "Scroll chips left"
    };
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    let color = if response.hovered() {
        CHROME_ICON_COLOR_HOVERED
    } else {
        CHROME_ICON_COLOR
    };
    let center = rect.center();
    let direction = if right { 1.0 } else { -1.0 };
    ui.painter().line_segment(
        [
            egui::pos2(center.x - 3.0 * direction, center.y - 5.0),
            egui::pos2(center.x + 2.0 * direction, center.y),
        ],
        Stroke::new(1.5, color),
    );
    ui.painter().line_segment(
        [
            egui::pos2(center.x + 2.0 * direction, center.y),
            egui::pos2(center.x - 3.0 * direction, center.y + 5.0),
        ],
        Stroke::new(1.5, color),
    );
    response.on_hover_text(label).clicked()
}

/// Shared allocate+label+hover-color+icon-paint+tooltip scaffold for the
/// fixed-size toolbar icon buttons in the trailing chrome band. Each caller
/// still owns its own click handling (and, for the overflow menu, the popup
/// that follows), since those differ per control.
fn paint_toolbar_icon_button(
    ui: &mut Ui,
    label: impl Into<String>,
    icon: Icon,
    hovered_color: Color32,
) -> egui::Response {
    let label = label.into();
    let size = CHROME_CONTROL_SIZE;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label.clone()));
    let color = if response.hovered() {
        hovered_color
    } else {
        CHROME_ICON_COLOR
    };
    icon::paint(ui.painter(), icon, rect.shrink(3.0), color);
    response.on_hover_text(label)
}

/// Painter-drawn window-minimize icon (a single horizontal line), replacing
/// the native title-bar minimize button that native decorations would
/// otherwise have provided (`docs/gui-design.md` "native min/max/close
/// window buttons directly in the same band as the chips"). Calls
/// `ViewportCommand::Minimized` directly rather than going through
/// `ChromeAction`/`AppCommand`, since this is an OS-window-level action with
/// no application-state implications.
pub(super) fn paint_minimize_icon(ui: &mut Ui) {
    let response =
        paint_toolbar_icon_button(ui, "Minimize", Icon::Minimize, CHROME_ICON_COLOR_HOVERED);
    if response.clicked() {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }
}

/// Painter-drawn window-maximize/restore icon: a single square when the
/// window is not currently maximized, or two overlapping squares (the
/// conventional "restore" glyph) when it is. `maximized` reflects the
/// viewport's real current state (`ui.input(|i| i.viewport().maximized)`)
/// so the icon's own shape communicates state, not a text label.
pub(super) fn paint_maximize_icon(ui: &mut Ui, maximized: bool) {
    let label = if maximized { "Restore" } else { "Maximize" };
    let icon = if maximized {
        Icon::Restore
    } else {
        Icon::Maximize
    };
    let response = paint_toolbar_icon_button(ui, label, icon, CHROME_ICON_COLOR_HOVERED);
    if response.clicked() {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }
}

/// Painter-drawn window-close icon (an X), colored with the same
/// destructive-hover red as a chip's own close control
/// (`CHROME_CLOSE_HOVER`).
pub(super) fn paint_close_icon(ui: &mut Ui) {
    // Distinct from a chip's own close control's "Close" label
    // (`paint_close_button`): both can be present in the same frame on
    // non-macOS platforms (custom titlebar), and an ambiguous shared label
    // made `harness.get_by_label("Close")` match either one nondeterministically
    // in headless tests (`chip_secondary_line_stays_clear_of_the_close_button`
    // failed only on Linux CI, where this icon - skipped on macOS in favor of
    // native traffic lights - is also painted).
    let response = paint_toolbar_icon_button(ui, "Close window", Icon::Close, CHROME_CLOSE_HOVER);
    if response.clicked() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// Painter-drawn magnifying-glass icon toggling the command palette
/// (mirrors the platform shortcut precedent in
/// `app.rs::handle_shortcuts`, which also just toggles the palette directly
/// rather than going through `AppCommand`). Returns whether it was clicked
/// this frame.
pub(super) fn paint_search_icon(ui: &mut Ui) -> bool {
    let default_shortcut = if cfg!(target_os = "macos") {
        "\u{2318}+Shift+P"
    } else {
        "Ctrl+Shift+P"
    };
    let shortcut = ui
        .data(|data| data.get_temp::<String>(egui::Id::new("command-palette-shortcut-label")))
        .unwrap_or_else(|| default_shortcut.to_owned());
    let accessible_label = format!("Command palette ({shortcut})");
    paint_toolbar_icon_button(
        ui,
        accessible_label,
        Icon::CommandPalette,
        CHROME_ICON_COLOR_HOVERED,
    )
    .clicked()
}

/// Painter-drawn "more" (vertical ellipsis) icon opening a small popup menu
/// holding the deliberately few actions that don't warrant their own
/// always-visible control (`docs/gui-design.md` "Application chrome and
/// session context": "remain reachable from the command palette and compact
/// overflow menu").
pub(super) fn paint_overflow_menu(
    ui: &mut Ui,
    include_palette: bool,
    include_inspector: bool,
    actions: &mut Vec<ChromeAction>,
) {
    let response = paint_toolbar_icon_button(
        ui,
        "More actions",
        Icon::Overflow,
        CHROME_ICON_COLOR_HOVERED,
    );

    Popup::menu(&response).show(|ui| {
        if ui.button("Open File…").clicked() {
            actions.push(ChromeAction::OpenMarkdownFile);
            ui.close();
        }
        if ui.button("Open Profiles").clicked() {
            actions.push(ChromeAction::OpenProfiles);
            ui.close();
        }
        if ui.button("Open Settings").clicked() {
            actions.push(ChromeAction::OpenSettings);
            ui.close();
        }
        if include_inspector && ui.button("Session inspector").clicked() {
            actions.push(ChromeAction::ToggleInspector);
            ui.close();
        }
        if ui.button("About fesTerm").clicked() {
            actions.push(ChromeAction::OpenAbout);
            ui.close();
        }
        if include_palette {
            ui.separator();
        }
        if include_palette && ui.button("Command palette").clicked() {
            actions.push(ChromeAction::TogglePalette);
            ui.close();
        }
    });
}
pub(super) fn style_context_menu(ui: &mut Ui) {
    ui.set_min_width(176.0);
    ui.spacing_mut().interact_size.y = 30.0;
    ui.spacing_mut().item_spacing.y = 2.0;
}
