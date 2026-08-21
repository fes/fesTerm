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

use egui::{
    emath::TSTransform, vec2, Align, Color32, DragAndDrop, Id, Key, LayerId, Layout, Order, Popup,
    RichText, ScrollArea, Sense, Stroke, TextEdit, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType,
};

use crate::{icon, icon::Icon, theme};

/// Chip footprint bounds (`docs/gui-design.md` "Active and inactive tabs"):
/// chips clip long secondary text with an ellipsis rather than growing
/// without bound, and never shrink below a minimum that still fits the
/// status dot, a short label, and (when shown) the close control.
const CHIP_MIN_WIDTH: f32 = 132.0;
const CHIP_MAX_WIDTH: f32 = 220.0;
/// Fixed two-line chip height (primary line + secondary line), independent
/// of whether a chip currently has secondary text. Trimmed from an earlier
/// `38.0`: pixel-measurement of a live screenshot showed the two text
/// lines' own ink plus font leading only need about 30 logical px, so the
/// remaining ~8px was pure top/bottom breathing room (`ui.add_space` calls
/// in `paint_chip`) rather than text-adjacent minimum spacing - trimming
/// just that padding (not the shared `CHROME_TOP_INSET`/`CHROME_SIDE_INSET`,
/// which also sizes the terminal viewport's border) shrinks the whole top
/// chrome band without crowding either text line or the 22px window-control
/// icons that share this row.
const CHIP_HEIGHT: f32 = 34.0;

// This chrome band defines its own small, fixed color palette rather than
// pulling from `ui.visuals()` (`docs/gui-design.md`): egui's derived
// widget-interaction colors (e.g. `strong_text_color()`, which maps to a
// pressed-button style, or `weak_text_color()`, which can end up
// unreadably dark against a near-black fill) are tuned for generic
// buttons/labels, not this row's specific dark-chip design, and produced
// exactly this readability bug in practice.
/// Cool light grey used for the active chip's outline and its close control, so
/// the two read as the same visual "active" affordance rather than the
/// close control looking dimmer/disabled by comparison.
const CHIP_ACTIVE_OUTLINE: Color32 = theme::BORDER_ACTIVE;
/// Subtle blue-grey outline for inactive chips and the Launcher control.
const CHIP_INACTIVE_OUTLINE: Color32 = theme::BORDER_SUBTLE;
/// Always-legible text colors for chip content.
const CHIP_PRIMARY_TEXT: Color32 = theme::TEXT_PRIMARY;
const CHIP_SECONDARY_TEXT: Color32 = theme::TEXT_SECONDARY;
/// Default and hovered colors for the row's painter-drawn icon controls
/// (new-tab, search, panel toggle, overflow menu).
const CHROME_ICON_COLOR: Color32 = theme::TEXT_SECONDARY;
const CHROME_ICON_COLOR_HOVERED: Color32 = theme::TEXT_PRIMARY;
/// Close-button hover color, distinct from the chip outline/icon palette to
/// keep its "destructive" affordance recognizable.
const CHROME_CLOSE_HOVER: Color32 = theme::STATUS_ERROR;

/// Background fill for the top chrome band. It deliberately matches the
/// terminal well so there is no extra title-band border; chips and controls
/// provide the hierarchy within one continuous surface.
const CHROME_BACKGROUND: Color32 = theme::SURFACE_CHROME;
/// Fill for an inactive chip and the "new tab" control, raised just enough
/// from the continuous chrome/terminal well to remain an independent object.
const CHIP_INACTIVE_FILL: Color32 = theme::SURFACE_TAB_INACTIVE;
/// Fill for the *active* chip: the lightest blue-graphite surface in the
/// chrome hierarchy. It stays lighter than both the inactive chip and chrome
/// band so fill and outline independently communicate selection.
const CHIP_ACTIVE_FILL: Color32 = theme::SURFACE_TAB_ACTIVE;

/// Horizontal inset from the window's left/right edges to the first/last
/// chrome element. The terminal content below shares the same side inset.
pub(crate) const CHROME_SIDE_INSET: f32 = 8.0;
/// Vertical inset from the window's top edge to the chip row itself.
///
const CHROME_TOP_INSET: f32 = 8.0;

/// The native macOS traffic lights occupy the left side of the transparent
/// titlebar. Keep the first application control clear of that hit-test area.
const MACOS_TRAFFIC_LIGHTS_RESERVED_WIDTH: f32 = 76.0;

/// Vertical distance, in points, from the window's top edge to the center
/// of the first chip row.
///
/// macOS uses this (`festerm_macos_window::offset_traffic_lights`) to keep
/// the native traffic lights vertically centered against the chip row,
/// re-applied every frame rather than assumed once from AppKit's default
/// titlebar placement at window creation — a placement that can drift
/// across macOS versions and would go stale the moment chip height itself
/// becomes runtime-configurable (for example, a future narrower
/// single-line chip preference). Keeping this a plain function of the
/// current row geometry, called every frame, means that once such a
/// preference exists, the traffic lights follow it automatically with no
/// further wiring.
pub const fn chrome_band_center_from_top() -> f32 {
    CHROME_TOP_INSET + CHIP_HEIGHT / 2.0
}

/// Footprint reserved for the trailing icon controls. On macOS the native
/// traffic lights replace the custom window buttons, leaving only overflow,
/// panel-toggle, and search controls in this block.
/// The chip row's own available width is capped to leave this much room
/// free (`show`), rather than letting the chip row claim the full row width
/// and only discovering afterward that the icons no longer fit: on a narrow
/// window that made the icons overlap the chips instead of
/// wrapping/scrolling around them.
const TRAILING_CONTROLS_RESERVED_WIDTH: f32 = (if cfg!(target_os = "macos") { 3.0 } else { 6.0 })
    * 22.0
    + (if cfg!(target_os = "macos") { 3.0 } else { 6.0 }) * 8.0
    + CHROME_SIDE_INSET;

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
    pub const fn color(self) -> Color32 {
        match self {
            Self::Connected => theme::STATUS_RUNNING,
            Self::Starting => theme::STATUS_STARTING,
            Self::Reconnecting => theme::STATUS_RECONNECTING,
            Self::Disconnected => theme::STATUS_DISCONNECTED,
            Self::AuthRequired => theme::STATUS_ATTENTION,
            Self::Failed => theme::STATUS_ERROR,
            Self::Exited => theme::STATUS_EXITED,
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
    /// Whether this chip's identity may be renamed in place
    /// (`docs/gui-design.md` "Identity precedence"): session chips back a
    /// real, storable label, while singleton application surfaces such as
    /// Launcher and Settings do not.
    pub renamable: bool,
}

/// A user gesture translated from the chip row. The application layer maps
/// this to an `AppCommand`; this crate does not dispatch or interpret it.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum ChromeAction {
    Activate(ChipId),
    Close(ChipId),
    NewTab,
    OpenSettings,
    ToggleInspector,
    /// Emitted by the search-icon control; mirrors the `Ctrl+Shift+P`
    /// shortcut precedent (`app.rs::handle_shortcuts`) by asking the caller
    /// to toggle its transient, chrome-external command-palette overlay.
    TogglePalette,
    /// Emitted from the overflow menu's "Toggle chip layout" entry.
    ToggleChipLayout,
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
    /// Moves one chip exactly one place without activating it. These are
    /// semantic context-menu gestures; the application owns tab-order policy.
    MoveLeft(ChipId),
    MoveRight(ChipId),
    RenameStarted {
        restore_focus: Option<Id>,
    },
    RenameFinished,
    /// Emitted when a rename edit is committed with a non-empty trimmed
    /// name. Only emitted for chips with `renamable: true`.
    Rename {
        id: ChipId,
        name: String,
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
    inspector_available: bool,
    layout: ChipLayout,
) -> Vec<ChromeAction> {
    let mut actions = Vec::new();
    // Paint the full inset-inclusive row first. This uses the same surface as
    // the terminal below, making the frameless chrome and terminal one visual
    // well instead of stacking a separate colored title band above content.
    let band_rect = egui::Rect::from_min_size(
        ui.cursor().min,
        vec2(ui.available_width(), CHROME_TOP_INSET + CHIP_HEIGHT),
    );
    ui.painter().rect_filled(band_rect, 0.0, CHROME_BACKGROUND);
    ui.add_space(CHROME_TOP_INSET);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        // Clamp this row's own height to the chip's compact content height
        // (rather than letting it inherit the full remaining panel height,
        // as a fresh `ui.horizontal` otherwise would): both the chip row's
        // `Align::Min` sub-layout and the icon block's `Align::Center`
        // sub-layout below need to agree on the *same* row height for
        // their vertical centering to land on the same line - previously
        // only the chip row was height-constrained, so the icon block's
        // `Align::Center` centered within the whole remaining panel height
        // instead, landing visibly off from the chips' own vertical
        // center.
        ui.set_min_height(CHIP_HEIGHT);
        ui.set_max_height(CHIP_HEIGHT);
        ui.add_space(CHROME_SIDE_INSET);
        if cfg!(target_os = "macos") {
            ui.add_space(MACOS_TRAFFIC_LIGHTS_RESERVED_WIDTH);
        }
        // Background drag-to-move region for this frameless window
        // (`docs/gui-design.md` "native min/max/close window buttons
        // directly in the same band as the chips"): a fixed-height band
        // matching the row's own compact content height (not
        // `ui.max_rect()`, which - before any content has been laid out -
        // still spans the full remaining panel height, and would swallow
        // pointer events meant for the terminal view painted below this
        // row by the caller). Registered *before* the chips/icons below so
        // their own click handling still takes priority over this
        // catch-all background sense wherever they visually sit on top of
        // it (mirrors egui's own `custom_window_frame` example).
        let row_band = egui::Rect::from_min_size(
            ui.cursor().min,
            vec2(ui.available_width(), CHIP_HEIGHT + 6.0),
        );
        let drag_response = ui.interact(
            row_band,
            Id::new("chrome_row_drag_region"),
            Sense::click_and_drag(),
        );
        if drag_response.drag_started_by(egui::PointerButton::Primary) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if drag_response.double_clicked() {
            let maximized = ui.input(|input| input.viewport().maximized.unwrap_or(false));
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        }
        // Scope the chip row to its own top-aligned sub-layout, rather than
        // changing this whole row's (or `ui.horizontal_top`'s) cross-axis
        // alignment: both alternatives hand the *entire* remaining panel
        // height down to the trailing icon controls' own `Align::Center`
        // sub-layout below (instead of just this row's compact content
        // height), centering the icons somewhere in the middle of the whole
        // window and leaving no room for the terminal view painted after
        // this function returns. Keeping the outer row's own layout as
        // plain `Align::Center` (proven safe:
        // `chrome_row_stays_a_compact_band_even_with_a_tall_available_area`)
        // while only this narrower sub-block opts into `Align::Min` fixes
        // the chip-row alignment without that regression.
        ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
            // Cap the chip row's own width so it leaves room for the
            // trailing icon controls painted as this row's next sibling,
            // rather than claiming the full remaining row width and only
            // discovering afterward that the icons don't fit: on a narrow
            // window that made the last chip(s) render underneath the
            // icons instead of wrapping/scrolling to stay clear of them
            // (`chip_row_never_overlaps_the_trailing_icon_controls_on_a_narrow_window`).
            let max_chip_row_width =
                (ui.available_width() - TRAILING_CONTROLS_RESERVED_WIDTH).max(0.0);
            ui.set_max_width(max_chip_row_width);
            let chip_row = |ui: &mut Ui| {
                for (index, chip) in chips.iter().enumerate() {
                    show_chip(
                        ui,
                        chip,
                        chip.id == active,
                        index > 0,
                        index + 1 < chips.len(),
                        &mut actions,
                    );
                }
                paint_new_chip_button(ui, &mut actions);
                // A small strip past the last chip lets a drag be released
                // (or live-shuffled to) the end of the row.
                end_of_row_drop_target(ui, &mut actions);
            };
            match layout {
                ChipLayout::Wrap => {
                    ui.horizontal_wrapped(chip_row);
                }
                ChipLayout::SingleRowScroll => {
                    ScrollArea::horizontal()
                        .id_salt("chip_row_scroll")
                        .max_width(max_chip_row_width)
                        .show(ui, |ui| ui.horizontal(chip_row));
                }
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(CHROME_SIDE_INSET);
            if !cfg!(target_os = "macos") {
                paint_close_icon(ui);
                let maximized = ui.input(|input| input.viewport().maximized.unwrap_or(false));
                paint_maximize_icon(ui, maximized);
                paint_minimize_icon(ui);
            }
            paint_overflow_menu(ui, &mut actions);
            if inspector_available && paint_panel_icon(ui, inspector_open) {
                actions.push(ChromeAction::ToggleInspector);
            }
            if paint_search_icon(ui) {
                actions.push(ChromeAction::TogglePalette);
            }
        });
    });
    // Egui input events are global to the frame. The terminal view is painted
    // after this band and must not encode mouse gestures already claimed by
    // chrome controls or the window-drag region.
    ui.input_mut(|input| {
        input.events.retain(|event| {
            !matches!(
                event,
                egui::Event::PointerButton { pos, .. } | egui::Event::PointerMoved(pos)
                    if band_rect.contains(*pos)
            )
        });
    });
    actions
}

/// Compact "add chip" control placed right after the last chip, painted
/// with the same chip-style rounded outline as an inactive chip (mockup:
/// the `+` control reads as a small chip in its own right, not a bare
/// icon floating in the row) - the sole way to open a new Launcher tab
/// from the chrome row (`AGENTS.md`: no duplicate widget-specific copies
/// of the same operation) - an earlier full "+ Launcher" chip-style
/// button duplicated this control and was removed as redundant.
fn paint_new_chip_button(ui: &mut Ui, actions: &mut Vec<ChromeAction>) {
    let size = CHIP_HEIGHT;
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
    ui.painter().rect_filled(rect, 6.0, fill);
    ui.painter().rect_stroke(
        rect,
        6.0,
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

/// Painter-drawn window-minimize icon (a single horizontal line), replacing
/// the native title-bar minimize button that native decorations would
/// otherwise have provided (`docs/gui-design.md` "native min/max/close
/// window buttons directly in the same band as the chips"). Calls
/// `ViewportCommand::Minimized` directly rather than going through
/// `ChromeAction`/`AppCommand`, since this is an OS-window-level action with
/// no application-state implications.
fn paint_minimize_icon(ui: &mut Ui) {
    let size = 22.0;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Minimize"));
    let color = if response.hovered() {
        CHROME_ICON_COLOR_HOVERED
    } else {
        CHROME_ICON_COLOR
    };
    icon::paint(ui.painter(), Icon::Minimize, rect.shrink(3.0), color);
    let response = response.on_hover_text("Minimize");
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
fn paint_maximize_icon(ui: &mut Ui, maximized: bool) {
    let size = 22.0;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let label = if maximized { "Restore" } else { "Maximize" };
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    let color = if response.hovered() {
        CHROME_ICON_COLOR_HOVERED
    } else {
        CHROME_ICON_COLOR
    };
    icon::paint(
        ui.painter(),
        if maximized {
            Icon::Restore
        } else {
            Icon::Maximize
        },
        rect.shrink(3.0),
        color,
    );
    let response = response.on_hover_text(label);
    if response.clicked() {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }
}

/// Painter-drawn window-close icon (an X), colored with the same
/// destructive-hover red as a chip's own close control
/// (`CHROME_CLOSE_HOVER`).
fn paint_close_icon(ui: &mut Ui) {
    let size = 22.0;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Close"));
    let color = if response.hovered() {
        CHROME_CLOSE_HOVER
    } else {
        CHROME_ICON_COLOR
    };
    icon::paint(ui.painter(), Icon::Close, rect.shrink(3.0), color);
    let response = response.on_hover_text("Close");
    if response.clicked() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// Painter-drawn magnifying-glass icon toggling the command palette
/// (mirrors the platform shortcut precedent in
/// `app.rs::handle_shortcuts`, which also just toggles the palette directly
/// rather than going through `AppCommand`). Returns whether it was clicked
/// this frame.
fn paint_search_icon(ui: &mut Ui) -> bool {
    let shortcut = if cfg!(target_os = "macos") {
        "Cmd+Shift+P"
    } else {
        "Ctrl+Shift+P"
    };
    let accessible_label = format!("Command palette ({shortcut})");
    let size = 22.0;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    response
        .widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, accessible_label.clone()));
    let color = if response.hovered() {
        CHROME_ICON_COLOR_HOVERED
    } else {
        CHROME_ICON_COLOR
    };
    icon::paint(ui.painter(), Icon::CommandPalette, rect.shrink(3.0), color);
    response.on_hover_text(accessible_label).clicked()
}

/// Painter-drawn side-panel icon toggling the session inspector. Returns
/// whether it was clicked this frame; `open` paints it in an active state so
/// the control's own affordance (not a text label) communicates whether the
/// inspector is currently shown.
fn paint_panel_icon(ui: &mut Ui, open: bool) -> bool {
    let size = 22.0;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    response
        .widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Toggle session inspector"));
    let color = if open || response.hovered() {
        CHROME_ICON_COLOR_HOVERED
    } else {
        CHROME_ICON_COLOR
    };
    icon::paint(
        ui.painter(),
        Icon::SessionInspector,
        rect.shrink(3.0),
        color,
    );
    response.on_hover_text("Toggle session inspector").clicked()
}

/// Painter-drawn "more" (vertical ellipsis) icon opening a small popup menu
/// holding the deliberately few actions that don't warrant their own
/// always-visible control (`docs/gui-design.md` "Application chrome and
/// session context": "remain reachable from the command palette and compact
/// overflow menu").
fn paint_overflow_menu(ui: &mut Ui, actions: &mut Vec<ChromeAction>) {
    let size = 22.0;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "More actions"));
    let color = if response.hovered() {
        CHROME_ICON_COLOR_HOVERED
    } else {
        CHROME_ICON_COLOR
    };
    icon::paint(ui.painter(), Icon::Overflow, rect.shrink(3.0), color);
    let response = response.on_hover_text("More actions");

    Popup::menu(&response).show(|ui| {
        if ui.button("Open Settings").clicked() {
            actions.push(ChromeAction::OpenSettings);
            ui.close();
        }
        if ui.button("Toggle chip layout").clicked() {
            actions.push(ChromeAction::ToggleChipLayout);
            ui.close();
        }
    });
}

/// An invisible strip past the last chip that accepts a live-dragged chip,
/// so a chip can be moved to the very end of the row (not just before
/// another chip).
fn end_of_row_drop_target(ui: &mut Ui, actions: &mut Vec<ChromeAction>) {
    let ctx = ui.ctx().clone();
    let id = Id::new("chrome_chip_row_end");
    let (_, rect) = ui.allocate_space(vec2(24.0, 1.0));
    let response = ui.interact(rect, id, Sense::hover());
    if let Some(dragged) = DragAndDrop::payload::<ChipId>(&ctx) {
        if response.contains_pointer() {
            actions.push(ChromeAction::Reorder {
                moved: *dragged,
                before: None,
            });
        }
    }
}

/// Per-frame identity for a chip's interactive footprint, drag state, and
/// rename-edit buffer, keyed by the chip's stable `ChipId`.
fn chip_widget_id(id: ChipId) -> Id {
    Id::new("chrome_chip").with(id.0)
}

/// Ephemeral, UI-only rename buffer key: whether `chip_id` currently has an
/// in-progress rename edit, and its current (uncommitted) text. Not part of
/// `ChipViewModel` because this module is pure presentation
/// (`docs/gui-design.md`); the caller only ever sees a committed
/// `ChromeAction::Rename`.
fn rename_buffer_id(id: ChipId) -> Id {
    Id::new("chrome_chip_rename").with(id.0)
}

fn show_chip(
    ui: &mut Ui,
    chip: &ChipViewModel,
    active: bool,
    can_move_left: bool,
    can_move_right: bool,
    actions: &mut Vec<ChromeAction>,
) {
    let chip_id = chip_widget_id(chip.id);
    let ctx = ui.ctx().clone();
    let cached_size = ctx
        .data_mut(|d| d.get_temp::<Vec2>(chip_id))
        .unwrap_or_else(|| vec2(CHIP_MIN_WIDTH, CHIP_HEIGHT));

    if ctx.is_being_dragged(chip_id) {
        // Currently being dragged: keep the payload alive, reserve the
        // chip's last-known footprint in the row (so drop targets don't
        // collapse out from under the pointer), and paint the real chip
        // floating at the pointer position, exactly as
        // `Ui::dnd_drag_source` does natively for its wrapped content.
        DragAndDrop::set_payload(&ctx, chip.id);

        let (_, ghost_rect) = ui.allocate_space(cached_size);
        ui.painter().rect_stroke(
            ghost_rect,
            4.0,
            Stroke::new(1.0, CHIP_INACTIVE_OUTLINE),
            egui::StrokeKind::Inside,
        );

        let layer_id = LayerId::new(Order::Tooltip, chip_id);
        let mut floating_ui =
            ui.new_child(UiBuilder::new().max_rect(ghost_rect).layer_id(layer_id));
        let content_response = paint_chip(
            &mut floating_ui,
            chip,
            active,
            false,
            active,
            chip_id,
            actions,
        );

        if let Some(pointer_pos) = ctx.pointer_interact_pos() {
            let delta = pointer_pos - content_response.rect.center();
            ctx.transform_layer_shapes(layer_id, TSTransform::from_translation(delta));
        }
        return;
    }

    let (_, bg_rect) = ui.allocate_space(cached_size);
    // Interacting the whole chip's background footprint *before* its inner
    // content is added registers it first in this frame's widget order.
    // egui resolves overlapping widgets by giving a later-registered widget
    // priority within their shared area (confirmed via
    // `egui::hit_test::thin_resize_handle_next_to_label`), so the close
    // button and rename field placed afterward, inside the same rect, still
    // reliably receive their own clicks while the rest of the chip acts as
    // a click-to-activate, press-and-hold-to-reorder surface with no
    // separate drag-handle affordance needed.
    let bg_response = ui.interact(bg_rect, chip_id, Sense::click_and_drag());
    bg_response.widget_info(|| {
        WidgetInfo::labeled(WidgetType::Other, true, format!("{} chip", chip.primary))
    });

    // The close control is a deliberately scarce affordance
    // (`docs/gui-design.md`): only the active chip ever shows it, matching
    // the mockup where inactive chips carry a plain dark-grey outline and
    // no close affordance at all, even on hover.
    let show_close = active;
    let hovered = bg_response.hovered();
    let mut content_ui = ui.new_child(UiBuilder::new().max_rect(bg_rect));
    let content_response = paint_chip(
        &mut content_ui,
        chip,
        active,
        hovered,
        show_close,
        chip_id,
        actions,
    );
    let clamped_size = vec2(
        content_response
            .rect
            .size()
            .x
            .clamp(CHIP_MIN_WIDTH, CHIP_MAX_WIDTH),
        CHIP_HEIGHT,
    );
    ctx.data_mut(|d| d.insert_temp(chip_id, clamped_size));

    if bg_response.clicked() {
        actions.push(ChromeAction::Activate(chip.id));
    }

    // Use raw pointer geometry for the secondary click so the menu covers the
    // complete chip, including label/status/close child widgets, without
    // placing a final invisible response above those controls and stealing
    // their ordinary primary-click behavior. Opening this menu deliberately
    // does not activate the target chip.
    let secondary_clicked = ui.input_mut(|input| {
        let mut released = false;
        input.events.retain(|event| {
            let egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Secondary,
                pressed,
                ..
            } = event
            else {
                return true;
            };
            if !bg_rect.contains(*pos) {
                return true;
            }
            released |= !pressed;
            false
        });
        released
    });
    let menu_restore_focus_id = chip_id.with("context_menu_restore_focus");
    if secondary_clicked {
        if let Some(focused) = ui.memory(|memory| memory.focused()) {
            ui.data_mut(|data| data.insert_temp(menu_restore_focus_id, focused));
        } else {
            ui.data_mut(|data| data.remove::<Id>(menu_restore_focus_id));
        }
    }
    Popup::context_menu(&bg_response)
        .open_memory(secondary_clicked.then_some(egui::SetOpenCommand::Bool(true)))
        .show(|ui| {
            style_context_menu(ui);
            if chip.renamable && ui.button("Rename session").clicked() {
                ui.data_mut(|data| {
                    data.insert_temp(rename_buffer_id(chip.id), chip.primary.clone())
                });
                actions.push(ChromeAction::RenameStarted {
                    restore_focus: ui.data(|data| data.get_temp(menu_restore_focus_id)),
                });
                ui.close();
            }
            if can_move_left && ui.button("Move left").clicked() {
                actions.push(ChromeAction::MoveLeft(chip.id));
                ui.close();
            }
            if can_move_right && ui.button("Move right").clicked() {
                actions.push(ChromeAction::MoveRight(chip.id));
                ui.close();
            }
            if chip.closable {
                if chip.renamable || can_move_left || can_move_right {
                    ui.separator();
                }
                let label = if chip.renamable {
                    "Close session"
                } else {
                    "Close"
                };
                if ui
                    .button(RichText::new(label).color(theme::STATUS_ERROR))
                    .clicked()
                {
                    actions.push(ChromeAction::Close(chip.id));
                    ui.close();
                }
            }
        });

    // Live reorder: while another chip is being dragged, settle the row's
    // order continuously as the pointer passes anywhere over this chip's
    // full footprint, rather than only on release. This uses raw pointer
    // geometry (not `bg_response.contains_pointer()`), because that would
    // be false wherever an inner widget (the label, status dot, or close
    // button) covers the same pixel, making the target's own visible label
    // an undroppable dead zone.
    if let Some(dragged) = DragAndDrop::payload::<ChipId>(&ctx) {
        if let Some(pointer_pos) = ctx.pointer_interact_pos() {
            if *dragged != chip.id && bg_rect.contains(pointer_pos) {
                actions.push(ChromeAction::Reorder {
                    moved: *dragged,
                    before: Some(chip.id),
                });
            }
        }
    }
}

fn style_context_menu(ui: &mut Ui) {
    ui.set_min_width(176.0);
    ui.spacing_mut().interact_size.y = 30.0;
    ui.spacing_mut().item_spacing.y = 2.0;
}

/// Paints one chip's content (status dot, label/rename field, secondary
/// text, close button) into `ui`, which the caller has already bounded to
/// the chip's footprint. Returns the response covering that content.
///
/// Renders as two lines: the primary line (status dot, stable identity, and
/// the close control, when `show_close`) and, indented beneath it, a
/// smaller/muted secondary line carrying transient terminal-provided
/// metadata (`docs/gui-design.md` "Identity precedence"). Both lines
/// truncate rather than growing the chip past `CHIP_MAX_WIDTH`.
fn paint_chip(
    ui: &mut Ui,
    chip: &ChipViewModel,
    active: bool,
    hovered: bool,
    show_close: bool,
    chip_id: Id,
    actions: &mut Vec<ChromeAction>,
) -> egui::Response {
    let corner_radius = 6;
    // The active chip's fill is the lightest surface in the row (measured
    // against the mockup: selected-chip fill lum ~170, the panel's overall
    // brightest element), not a darker merge with the terminal content -
    // see `CHIP_ACTIVE_FILL`'s doc comment for the earlier, inverted
    // assumption this replaces. Hovering an *inactive* chip previews that
    // same lighter fill (without touching its outline) rather than
    // brightening the outline - matching `paint_new_chip_button`'s hover
    // treatment for a consistent hover language across all chip-shaped
    // controls in the row.
    let fill = if active || hovered {
        CHIP_ACTIVE_FILL
    } else {
        CHIP_INACTIVE_FILL
    };
    let stroke = if active {
        Stroke::new(1.5, CHIP_ACTIVE_OUTLINE)
    } else {
        Stroke::new(1.0, CHIP_INACTIVE_OUTLINE)
    };

    let outer_rect = ui.max_rect();
    ui.painter().rect_filled(outer_rect, corner_radius, fill);
    ui.painter()
        .rect_stroke(outer_rect, corner_radius, stroke, egui::StrokeKind::Inside);

    // The close control is positioned from the chip's own outer rect
    // (evenly inset from the top and right edges) rather than flowing
    // through the primary line's layout: this keeps its position fixed
    // regardless of label length and avoids the label being pulled
    // towards the right edge, which a right-to-left sub-layout previously
    // caused for short labels.
    const CLOSE_SIZE: f32 = 16.0;
    const CLOSE_INSET: f32 = 8.0;
    let close_rect = if chip.closable && show_close {
        let rect = egui::Rect::from_min_size(
            outer_rect.right_top() + vec2(-CLOSE_INSET - CLOSE_SIZE, CLOSE_INSET),
            vec2(CLOSE_SIZE, CLOSE_SIZE),
        );
        let mut close_ui = ui.new_child(UiBuilder::new().max_rect(rect));
        paint_close_button(&mut close_ui, chip.id, actions);
        Some(rect)
    } else {
        None
    };

    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        // The primary line is optically one pixel high. Moving that pixel
        // from the inter-row gap to the top inset shifts only the status dot
        // and title; the secondary line keeps its previous y-position.
        ui.spacing_mut().item_spacing.y = 0.0;
        // Rows would otherwise reserve `interact_size.y` (a button-sized
        // minimum, ~24px) even for a single line of small text, inflating
        // the gap between the primary and secondary lines well past what
        // the text itself needs.
        ui.spacing_mut().interact_size.y = 0.0;
        ui.style_mut().visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        // `Ui::vertical` centers its children horizontally by default; the
        // mockup left-aligns the chip's content (with a small reserved
        // strip for the status dot), so lay the two lines out top-down
        // with `Align::Min` instead.
        ui.with_layout(Layout::top_down(Align::Min), |ui| {
            // Claim the chip's full footprint up front: otherwise a chip
            // with no secondary text (e.g. a Launcher tab) lays out a
            // shorter content block that then gets vertically *centered*
            // within the row by the parent chip-row layout, instead of
            // sitting flush at the top like every other chip.
            ui.set_min_size(outer_rect.size());
            ui.add_space(3.0);
            // Reserve the close button's width (if shown) so both the
            // primary label and the secondary line truncate before
            // reaching under it, without otherwise affecting either
            // line's left-aligned position. Shared across both lines so a
            // long secondary line (e.g. "Awaiting SSH password") can't run
            // under the button the way the primary label used to before
            // this was hoisted out of the primary-only closure below.
            let reserved = close_rect.map_or(0.0, |rect| outer_rect.right() - rect.left());
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                if !matches!(chip.status, ChipStatus::Neutral) {
                    paint_status_dot(ui, chip.status);
                }

                let rename_id = rename_buffer_id(chip.id);
                let editing: Option<String> = ui.data(|d| d.get_temp(rename_id));

                ui.scope(|ui| {
                    let max_width = (ui.available_width() - reserved).max(0.0);
                    ui.set_max_width(max_width);
                    paint_chip_primary(ui, chip, rename_id, editing, actions);
                });
            });
            if let Some(secondary) = &chip.secondary {
                ui.horizontal(|ui| {
                    // Indent under the primary line's label (status dot
                    // width + spacing), not the chip's own left padding.
                    ui.add_space(if matches!(chip.status, ChipStatus::Neutral) {
                        8.0
                    } else {
                        22.0
                    });
                    ui.scope(|ui| {
                        let max_width = (ui.available_width() - reserved).max(0.0);
                        ui.set_max_width(max_width);
                        ui.add(
                            egui::Label::new(
                                RichText::new(secondary).color(CHIP_SECONDARY_TEXT).small(),
                            )
                            // Chip text is identity/navigation chrome, not
                            // selectable document content: without this, egui's
                            // default `selectable_labels` style makes the whole
                            // chip show a text (I-beam) hover cursor instead of
                            // the plain arrow a clickable chip should have.
                            .selectable(false)
                            .truncate(),
                        );
                    });
                });
            }
            ui.add_space(4.0);
        });
    });

    ui.interact(outer_rect, chip_id.with("content"), Sense::hover())
}

/// Paints the primary-line label or its in-progress rename `TextEdit`,
/// filling the remaining horizontal space in `ui` (truncating rather than
/// growing the chip). Split out of [`paint_chip`] so the close button (when
/// shown) can be laid out first, right-to-left, without the label pushing
/// it out of the chip.
fn paint_chip_primary(
    ui: &mut Ui,
    chip: &ChipViewModel,
    rename_id: Id,
    editing: Option<String>,
    actions: &mut Vec<ChromeAction>,
) {
    if let Some(mut buffer) = editing {
        let response = ui.add(TextEdit::singleline(&mut buffer).desired_width(f32::INFINITY));
        let cancel = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Escape));
        let confirm = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Enter));
        let commit = !cancel && (confirm || response.lost_focus());
        // Re-request focus only if we're staying in edit mode; doing this
        // unconditionally would immediately re-grab focus after
        // Enter/Escape surrendered it, masking the commit/cancel.
        if !response.has_focus() && !cancel && !commit {
            response.request_focus();
        }
        if cancel {
            ui.data_mut(|d| d.remove::<String>(rename_id));
            actions.push(ChromeAction::RenameFinished);
        } else if commit {
            let trimmed = buffer.trim();
            if !trimmed.is_empty() {
                actions.push(ChromeAction::Rename {
                    id: chip.id,
                    name: trimmed.to_owned(),
                });
            }
            ui.data_mut(|d| d.remove::<String>(rename_id));
            actions.push(ChromeAction::RenameFinished);
        } else {
            ui.data_mut(|d| d.insert_temp(rename_id, buffer));
        }
    } else {
        let label = RichText::new(&chip.primary).color(CHIP_PRIMARY_TEXT);
        let label_response = ui.add(
            egui::Label::new(label)
                .sense(Sense::click())
                // See the secondary-line label above: this is clickable
                // navigation chrome, not selectable text, so the hover
                // cursor should read as a plain arrow, not an I-beam.
                .selectable(false)
                .truncate(),
        );
        if label_response.clicked() {
            actions.push(ChromeAction::Activate(chip.id));
        }
        if chip.renamable && label_response.double_clicked() {
            ui.data_mut(|d| d.insert_temp(rename_id, chip.primary.clone()));
            actions.push(ChromeAction::RenameStarted {
                restore_focus: None,
            });
        }
    }
}

/// Compact, non-color-exclusive connection-state dot, painted directly
/// rather than relying on a glyph the active font may not have coverage for
/// (the previous `\u{25cf}` rendered as tofu/an empty box on this machine).
fn paint_status_dot(ui: &mut Ui, status: ChipStatus) {
    let diameter = 8.0;
    // Allocate at the primary label's own line height (rather than just
    // the dot's diameter) so this row's cross-axis `Align::Center`
    // computes the same center line for both the dot and the label text,
    // instead of centering the dot within a shorter box that happens to
    // sit slightly off from the text's own optical center.
    let text_height = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, response) = ui.allocate_exact_size(vec2(diameter, text_height), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), diameter / 2.0, status.color());
    response.on_hover_text(status.accessible_label());
}

/// Painter-drawn close control (replacing the previous `\u{2715}` glyph,
/// which likewise rendered as tofu): two crossed lines inside a small
/// clickable square, with an explicit accessible label so screen readers
/// and headless-test queries don't depend on the (absent) visual glyph.
fn paint_close_button(ui: &mut Ui, id: ChipId, actions: &mut Vec<ChromeAction>) {
    let size = 16.0;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Close"));

    let color = if response.hovered() {
        CHROME_CLOSE_HOVER
    } else {
        // Matches the active chip's own outline color (the close control
        // only ever appears on the active chip), so the two read as the
        // same "active" affordance rather than the close control looking
        // dimmer/disabled by comparison.
        CHIP_ACTIVE_OUTLINE
    };
    let inset = rect.shrink(4.0);
    ui.painter().line_segment(
        [inset.left_top(), inset.right_bottom()],
        Stroke::new(1.5, color),
    );
    ui.painter().line_segment(
        [inset.right_top(), inset.left_bottom()],
        Stroke::new(1.5, color),
    );

    let response = response.on_hover_text("Close");
    if response.clicked() {
        actions.push(ChromeAction::Close(id));
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
            renamable: true,
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
            // A small step_dt keeps synthetic per-frame time increments (see
            // `Harness::step`, which advances the simulated clock once per
            // queued input event) from accidentally exceeding egui's
            // real-time-based double-click window in tests that simulate a
            // double click across several separate events/frames.
            .with_step_dt(0.01)
            .build_ui_state(
                |ui, state: &mut ChromeHarnessState| {
                    let actions = show(ui, &state.chips, state.active, false, true, state.layout);
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
    fn chip_context_menu_targets_inactive_chip_without_activating_it() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one"), chip(2, "two"), chip(3, "three")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });
        harness.run();

        harness.get_by_label("two chip").click_secondary();
        harness.run();
        assert!(harness.query_by_label("Rename session").is_some());
        assert!(harness.query_by_label("Move left").is_some());
        assert!(harness.query_by_label("Move right").is_some());
        assert!(harness.query_by_label("Close session").is_some());
        assert!(!harness
            .state()
            .observed
            .contains(&ChromeAction::Activate(ChipId(2))));

        harness.get_by_label("Move right").click();
        harness.run();
        assert!(harness
            .state()
            .observed
            .contains(&ChromeAction::MoveRight(ChipId(2))));
        assert!(!harness
            .state()
            .observed
            .contains(&ChromeAction::Activate(ChipId(2))));
    }

    #[test]
    fn chip_context_menu_omits_moves_at_edges() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one"), chip(2, "two")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });
        harness.run();

        harness.get_by_label("one chip").click_secondary();
        harness.run();
        assert!(harness.query_by_label("Move left").is_none());
        assert!(harness.query_by_label("Move right").is_some());
    }

    #[test]
    fn chip_context_menu_escape_closes_without_emitting_an_action() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one"), chip(2, "two")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });
        harness.run();
        harness.get_by_label("two chip").click_secondary();
        harness.run();
        assert!(harness.query_by_label("Rename session").is_some());

        harness.key_press(Key::Escape);
        harness.run();

        assert!(harness.query_by_label("Rename session").is_none());
        assert!(harness.state().observed.is_empty());
    }

    #[test]
    fn chip_context_menu_item_is_keyboard_activatable() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one"), chip(2, "two")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });
        harness.run();
        harness.get_by_label("two chip").click_secondary();
        harness.run();
        harness.get_by_label("Move left").focus();
        harness.run();

        harness.key_press(Key::Enter);
        harness.run();

        assert!(harness
            .state()
            .observed
            .contains(&ChromeAction::MoveLeft(ChipId(2))));
    }

    #[test]
    fn chrome_row_stays_a_compact_band_even_with_a_tall_available_area() {
        // Regression test: the chrome row's top-level layout previously
        // switched from plain `ui.horizontal` (`Align::Center`) to an
        // `Align::Min` layout (via `with_layout`/`horizontal_top`) directly
        // on the incoming `ui`, to top-align the "+ Launcher" control with
        // the chip row. That fixed their alignment but, empirically, an
        // `Align::Min` top-level layout here hands the *entire* remaining
        // panel height down through to the trailing icon controls' nested
        // `Align::Center` sub-layout (instead of just this row's compact
        // content height) — landing the icons roughly mid-window instead of
        // near the top, and (in the real app, where `ui.separator()` and
        // the terminal view are added to the same `ui` right after
        // `chrome::show` returns) leaving no vertical room for them. Only
        // plain `Align::Center` at the very top level avoids this; the
        // launcher/chip alignment fix is instead scoped to a narrower
        // `Align::Min` sub-layout around just those two elements.
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 900.0))
            .with_step_dt(0.01)
            .build_ui_state(
                |ui, state: &mut ChromeHarnessState| {
                    let actions = show(ui, &state.chips, state.active, false, true, state.layout);
                    state.observed.extend(actions);
                    // Mirrors `app.rs`'s `ui_content`, which adds
                    // `ui.separator()` immediately after `chrome::show`.
                    ui.separator();
                },
                ChromeHarnessState {
                    chips: vec![chip(1, "one"), chip(2, "two")],
                    active: ChipId(1),
                    layout: ChipLayout::Wrap,
                    observed: Vec::new(),
                },
            );
        harness.run();

        let chip_rect = harness.get_by_label("one chip").rect();
        assert!(
            chip_rect.top() < 20.0,
            "expected the chip row to sit at the top of a tall panel, got {chip_rect:?}"
        );

        let search_rect = harness.get_by_label_contains("Command palette").rect();
        assert!(
            search_rect.bottom() < 100.0,
            "expected the trailing icon controls to stay within the compact chip-row band \
             rather than centering within the full panel height, got {search_rect:?}"
        );
    }

    #[test]
    fn chip_row_never_overlaps_the_trailing_icon_controls_on_a_narrow_window() {
        // Regression test: on a narrow window, the wrapped chip row
        // previously computed its wrap width from the *full* remaining row
        // width (unaware the trailing icon controls still needed to be
        // painted as its sibling), so its bounding box could extend under -
        // rather than stop clear of - the search/panel/overflow icons.
        // `ChipLayout::SingleRowScroll` is not covered here: a horizontally
        // scrolled chip can legitimately report an off-screen logical rect
        // past the visible viewport (that's the scrolling mechanism doing
        // its job, not an overlap bug), so per-chip rect assertions don't
        // apply there the same way.
        let mut harness = Harness::builder()
            .with_size(egui::vec2(360.0, 300.0))
            .with_step_dt(0.01)
            .build_ui_state(
                |ui, state: &mut ChromeHarnessState| {
                    let actions = show(ui, &state.chips, state.active, false, true, state.layout);
                    state.observed.extend(actions);
                },
                ChromeHarnessState {
                    chips: vec![
                        chip(1, "one"),
                        chip(2, "two"),
                        chip(3, "three"),
                        chip(4, "four"),
                    ],
                    active: ChipId(1),
                    layout: ChipLayout::Wrap,
                    observed: Vec::new(),
                },
            );
        harness.run();

        let search_rect = harness.get_by_label_contains("Command palette").rect();
        for label in ["one chip", "two chip", "three chip", "four chip"] {
            let chip_rect = harness.get_by_label(label).rect();
            assert!(
                chip_rect.right() <= search_rect.left() + 1.0,
                "expected {label}'s rect ({chip_rect:?}) to stay clear of the search icon \
                 ({search_rect:?})"
            );
        }
    }

    #[test]
    fn clicking_a_chip_close_button_emits_a_close_action() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one"), chip(2, "two")],
            // Only the active chip ever shows a close button
            // (`docs/gui-design.md`): inactive chips have no close
            // affordance, even on hover.
            active: ChipId(2),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });
        harness.run();

        // The chip's own background footprint is always present, so its
        // rect can be queried directly. The close button is laid out flush
        // with the chip's top-right corner (`paint_chip`/
        // `paint_chip_primary`'s right-to-left first line).
        let bg_rect = harness.get_by_label("two chip").rect();
        let target = bg_rect.right_top() + egui::vec2(-11.0, 11.0);
        harness.event(egui::Event::PointerMoved(target));
        harness.event(egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        harness.event(egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        harness.run();

        assert!(harness
            .state()
            .observed
            .contains(&ChromeAction::Close(ChipId(2))));
    }

    #[test]
    fn chip_secondary_line_stays_clear_of_the_close_button() {
        // Regression test: the secondary line (e.g. "Awaiting SSH
        // password") used to lay out at the chip's full width, unaware
        // that the primary line above it was reserving space for the
        // close button, so a long secondary string could visually run
        // under/behind the close button instead of truncating before it.
        let mut harness = harness(ChromeHarnessState {
            chips: vec![ChipViewModel {
                secondary: Some("Awaiting SSH password authentication".to_owned()),
                ..chip(1, "one")
            }],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });
        harness.run();

        let close_rect = harness.get_by_label("Close").rect();
        let secondary_rect = harness
            .get_by_label_contains("Awaiting SSH password")
            .rect();
        assert!(
            secondary_rect.right() <= close_rect.left() + 1.0,
            "expected the secondary line ({secondary_rect:?}) to stay clear of the close \
             button ({close_rect:?})"
        );
    }

    #[test]
    fn inline_new_chip_button_emits_a_new_tab_action() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });

        harness.get_by_label("New tab").click();
        harness.run();

        assert!(harness.state().observed.contains(&ChromeAction::NewTab));
    }

    #[test]
    fn search_icon_emits_a_toggle_palette_action() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });

        harness.get_by_label_contains("Command palette").click();
        harness.run();

        assert!(harness
            .state()
            .observed
            .contains(&ChromeAction::TogglePalette));
    }

    #[test]
    fn overflow_menu_settings_entry_emits_an_open_settings_action() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });

        harness.get_by_label("More actions").click();
        harness.run();
        harness.get_by_label("Open Settings").click();
        harness.run();

        assert!(harness
            .state()
            .observed
            .contains(&ChromeAction::OpenSettings));
    }

    #[test]
    fn overflow_menu_chip_layout_entry_emits_a_toggle_chip_layout_action() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });

        harness.get_by_label("More actions").click();
        harness.run();
        harness.get_by_label("Toggle chip layout").click();
        harness.run();

        assert!(harness
            .state()
            .observed
            .contains(&ChromeAction::ToggleChipLayout));
    }

    #[test]
    fn inspector_toggle_emits_a_toggle_inspector_action() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });

        harness.get_by_label_contains("inspector").click();
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

        // Whole-chip press-and-hold drag: no separate handle. Press just
        // inside the chip's left padding strip, clear of the status dot,
        // label, and close button sub-widgets, so the press unambiguously
        // starts a drag on the chip's own background rather than
        // activating or closing it.
        let one_rect = harness.get_by_label("one chip").rect();
        let from = one_rect.left_center() + egui::vec2(3.0, 0.0);
        let to_rect = harness.get_by_label("three chip").rect();
        let to = to_rect.left_center() + egui::vec2(3.0, 0.0);

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

    #[test]
    fn double_clicking_a_renamable_chip_and_committing_emits_a_rename_action() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });
        harness.run();

        // Two clicks queued before a single `run()` land within one
        // frame's input batch, which egui's own double-click-timing check
        // (based on the frame's shared `now`) treats as a double click.
        let label_rect = harness.get_by_label("one").rect();
        let center = label_rect.center();
        let click = |harness: &Harness<'_, ChromeHarnessState>| {
            harness.event(egui::Event::PointerMoved(center));
            harness.event(egui::Event::PointerButton {
                pos: center,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            });
            harness.event(egui::Event::PointerButton {
                pos: center,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            });
        };
        click(&harness);
        harness.run();
        click(&harness);
        harness.run();

        harness.event(egui::Event::Text("renamed".to_owned()));
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        // Losing focus (Enter doesn't itself blur a single-line TextEdit in
        // every backend) is what actually commits; force it in case Enter
        // alone didn't yield `lost_focus()` this run.
        harness.run();

        assert!(
            harness.state().observed.iter().any(|action| matches!(
                action,
                ChromeAction::Rename { id: ChipId(1), name } if !name.is_empty()
            )),
            "observed actions: {:?}",
            harness.state().observed
        );
        assert!(harness
            .state()
            .observed
            .iter()
            .any(|action| matches!(action, ChromeAction::RenameStarted { .. })));
        assert!(harness
            .state()
            .observed
            .contains(&ChromeAction::RenameFinished));
    }

    #[test]
    fn non_renamable_chip_double_click_does_not_enter_edit_mode() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![ChipViewModel {
                id: ChipId(1),
                primary: "Launcher".to_owned(),
                secondary: None,
                status: ChipStatus::Neutral,
                closable: false,
                renamable: false,
            }],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });
        harness.run();

        let label = harness.get_by_label("Launcher");
        label.click();
        label.click();
        harness.run();

        // No rename should ever be observed for a non-renamable chip.
        assert!(harness
            .state()
            .observed
            .iter()
            .all(|action| !matches!(action, ChromeAction::Rename { .. })));
    }
}
