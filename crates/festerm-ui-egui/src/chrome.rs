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

/// Chip footprint bounds (`docs/gui-design.md` "Active and inactive tabs"):
/// chips clip long secondary text with an ellipsis rather than growing
/// without bound, and never shrink below a minimum that still fits the
/// status dot, a short label, and (when shown) the close control.
const CHIP_MIN_WIDTH: f32 = 132.0;
const CHIP_MAX_WIDTH: f32 = 220.0;
/// Fixed two-line chip height (primary line + secondary line), independent
/// of whether a chip currently has secondary text.
const CHIP_HEIGHT: f32 = 38.0;

// This chrome band defines its own small, fixed color palette rather than
// pulling from `ui.visuals()` (`docs/gui-design.md`): egui's derived
// widget-interaction colors (e.g. `strong_text_color()`, which maps to a
// pressed-button style, or `weak_text_color()`, which can end up
// unreadably dark against a near-black fill) are tuned for generic
// buttons/labels, not this row's specific dark-chip design, and produced
// exactly this readability bug in practice.
/// Light grey used for the active chip's outline and its close control, so
/// the two read as the same visual "active" affordance rather than the
/// close control looking dimmer/disabled by comparison.
const CHIP_ACTIVE_OUTLINE: Color32 = Color32::from_gray(0xc8);
/// Darker grey outline for inactive chips and the Launcher control.
const CHIP_INACTIVE_OUTLINE: Color32 = Color32::from_gray(0x48);
/// Always-legible text colors for chip content.
const CHIP_PRIMARY_TEXT: Color32 = Color32::from_gray(0xe6);
const CHIP_SECONDARY_TEXT: Color32 = Color32::from_gray(0x9a);
/// Default and hovered colors for the row's painter-drawn icon controls
/// (new-tab, search, panel toggle, overflow menu).
const CHROME_ICON_COLOR: Color32 = Color32::from_gray(0x9a);
const CHROME_ICON_COLOR_HOVERED: Color32 = Color32::from_gray(0xf0);
/// Close-button hover color, distinct from the chip outline/icon palette to
/// keep its "destructive" affordance recognizable.
const CHROME_CLOSE_HOVER: Color32 = Color32::from_rgb(0xe0, 0x5c, 0x5c);

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
    layout: ChipLayout,
) -> Vec<ChromeAction> {
    let mut actions = Vec::new();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
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
            let chip_row = |ui: &mut Ui| {
                for chip in chips {
                    show_chip(ui, chip, chip.id == active, &mut actions);
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
                        .show(ui, |ui| ui.horizontal(chip_row));
                }
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            paint_overflow_menu(ui, &mut actions);
            if paint_panel_icon(ui, inspector_open) {
                actions.push(ChromeAction::ToggleInspector);
            }
            if paint_search_icon(ui) {
                actions.push(ChromeAction::TogglePalette);
            }
        });
    });
    actions
}

/// Compact icon-only "add chip" control placed right after the last chip.
/// This is the sole way to open a new Launcher tab from the chrome row
/// (`AGENTS.md`: no duplicate widget-specific copies of the same
/// operation) - an earlier full "+ Launcher" chip-style button duplicated
/// this control and was removed as redundant.
fn paint_new_chip_button(ui: &mut Ui, actions: &mut Vec<ChromeAction>) {
    let size = 22.0;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "New tab"));
    let color = if response.hovered() {
        CHROME_ICON_COLOR_HOVERED
    } else {
        CHROME_ICON_COLOR
    };
    let center = rect.center();
    let half = size * 0.28;
    ui.painter().line_segment(
        [center - vec2(half, 0.0), center + vec2(half, 0.0)],
        Stroke::new(1.5, color),
    );
    ui.painter().line_segment(
        [center - vec2(0.0, half), center + vec2(0.0, half)],
        Stroke::new(1.5, color),
    );
    let response = response.on_hover_text("New tab");
    if response.clicked() {
        actions.push(ChromeAction::NewTab);
    }
}

/// Painter-drawn magnifying-glass icon toggling the command palette
/// (mirrors the `Ctrl+Shift+P` shortcut precedent in
/// `app.rs::handle_shortcuts`, which also just toggles the palette directly
/// rather than going through `AppCommand`). Returns whether it was clicked
/// this frame.
fn paint_search_icon(ui: &mut Ui) -> bool {
    let size = 22.0;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Search (Ctrl+Shift+P)"));
    let color = if response.hovered() {
        CHROME_ICON_COLOR_HOVERED
    } else {
        CHROME_ICON_COLOR
    };
    let center = rect.center() - vec2(1.0, 1.0);
    let radius = size * 0.22;
    ui.painter()
        .circle_stroke(center, radius, Stroke::new(1.5, color));
    let handle_start = center + vec2(radius * 0.7, radius * 0.7);
    let handle_end = handle_start + vec2(size * 0.22, size * 0.22);
    ui.painter()
        .line_segment([handle_start, handle_end], Stroke::new(1.5, color));
    response.on_hover_text("Search (Ctrl+Shift+P)").clicked()
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
    let outer = rect.shrink(3.0);
    ui.painter().rect_stroke(
        outer,
        2.0,
        Stroke::new(1.5, color),
        egui::StrokeKind::Inside,
    );
    let divider_x = outer.right() - outer.width() * 0.35;
    ui.painter().line_segment(
        [
            egui::pos2(divider_x, outer.top()),
            egui::pos2(divider_x, outer.bottom()),
        ],
        Stroke::new(1.5, color),
    );
    if open {
        let panel_rect =
            egui::Rect::from_min_max(egui::pos2(divider_x, outer.top()), outer.right_bottom());
        ui.painter()
            .rect_filled(panel_rect, 0.0, color.gamma_multiply(0.35));
    }
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
    let center = rect.center();
    for offset in [-6.0, 0.0, 6.0] {
        ui.painter()
            .circle_filled(center + vec2(0.0, offset), 1.6, color);
    }
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

fn show_chip(ui: &mut Ui, chip: &ChipViewModel, active: bool, actions: &mut Vec<ChromeAction>) {
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
        let content_response = paint_chip(&mut floating_ui, chip, active, active, chip_id, actions);

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
    let mut content_ui = ui.new_child(UiBuilder::new().max_rect(bg_rect));
    let content_response = paint_chip(&mut content_ui, chip, active, show_close, chip_id, actions);
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
    show_close: bool,
    chip_id: Id,
    actions: &mut Vec<ChromeAction>,
) -> egui::Response {
    let corner_radius = 6;
    // Both states share the same neutral fill, matching the rest of the
    // application's near-black background (`docs/gui-design.md`): the
    // active chip is signified by its outline and close control, not by a
    // distinct accent fill.
    let fill = crate::DEFAULT_BACKGROUND;
    let stroke = if active {
        Stroke::new(1.5, CHIP_ACTIVE_OUTLINE)
    } else {
        Stroke::new(1.0, Color32::from_gray(0x48))
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
        ui.spacing_mut().item_spacing.y = 1.0;
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
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                if !matches!(chip.status, ChipStatus::Neutral) {
                    paint_status_dot(ui, chip.status);
                }

                let rename_id = rename_buffer_id(chip.id);
                let editing: Option<String> = ui.data(|d| d.get_temp(rename_id));

                // Reserve the close button's width (if shown) so the
                // label truncates before reaching under it, without
                // otherwise affecting the label's left-aligned position.
                let reserved = close_rect.map_or(0.0, |rect| outer_rect.right() - rect.left());
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
                    ui.add(
                        egui::Label::new(
                            RichText::new(secondary).color(CHIP_SECONDARY_TEXT).small(),
                        )
                        .truncate(),
                    );
                });
            }
            ui.add_space(6.0);
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
        let cancel = ui.input(|i| i.key_pressed(Key::Escape));
        let commit = !cancel && response.lost_focus();
        // Re-request focus only if we're staying in edit mode; doing this
        // unconditionally would immediately re-grab focus after
        // Enter/Escape surrendered it, masking the commit/cancel.
        if !response.has_focus() && !cancel && !commit {
            response.request_focus();
        }
        if cancel {
            ui.data_mut(|d| d.remove::<String>(rename_id));
        } else if commit {
            let trimmed = buffer.trim();
            if !trimmed.is_empty() {
                actions.push(ChromeAction::Rename {
                    id: chip.id,
                    name: trimmed.to_owned(),
                });
            }
            ui.data_mut(|d| d.remove::<String>(rename_id));
        } else {
            ui.data_mut(|d| d.insert_temp(rename_id, buffer));
        }
    } else {
        let label = RichText::new(&chip.primary).color(CHIP_PRIMARY_TEXT);
        let label_response = ui.add(egui::Label::new(label).sense(Sense::click()).truncate());
        if label_response.clicked() {
            actions.push(ChromeAction::Activate(chip.id));
        }
        if chip.renamable && label_response.double_clicked() {
            ui.data_mut(|d| d.insert_temp(rename_id, chip.primary.clone()));
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
                    let actions = show(ui, &state.chips, state.active, false, state.layout);
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

        let search_rect = harness.get_by_label("Search (Ctrl+Shift+P)").rect();
        assert!(
            search_rect.bottom() < 100.0,
            "expected the trailing icon controls to stay within the compact chip-row band \
             rather than centering within the full panel height, got {search_rect:?}"
        );
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

        harness.get_by_label_contains("Search").click();
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
