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

use egui::viewport::ResizeDirection;
use egui::{
    emath::TSTransform, vec2, Align, Align2, Color32, CursorIcon, DragAndDrop, Id, Key, LayerId,
    Layout, Order, PointerButton, Popup, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui,
    UiBuilder, WidgetInfo, WidgetType,
};

use crate::{icon, icon::Icon, theme};

/// Chip footprint bounds (`docs/gui-design.md` "Single-row allocation
/// contract"). The focused chip keeps its ordinary minimum because it owns
/// the stable identity and Close control. Inactive chips may compact much
/// further before the row starts scrolling.
const CHIP_FOCUSED_MIN_WIDTH: f32 = 132.0;
const CHIP_INACTIVE_MIN_WIDTH: f32 = 72.0;
const CHIP_MAX_WIDTH: f32 = 220.0;
/// Fixed two-line chip height (primary line + secondary line), used row-wide
/// whenever "Show session details in chips" is on - independent of whether
/// any one chip currently has secondary text (see `paint_chip`'s doc
/// comment: a Launcher chip with no secondary line still claims this same
/// height so every chip in the row lines up evenly). Trimmed from an
/// earlier `38.0`: pixel-measurement of a live screenshot showed the two
/// text lines' own ink plus font leading only need about 30 logical px, so
/// the remaining ~8px was pure top/bottom breathing room (`ui.add_space`
/// calls in `paint_chip`) rather than text-adjacent minimum spacing -
/// trimming just that padding (not the shared
/// `CHROME_TOP_INSET`/`CHROME_SIDE_INSET`, which also sizes the terminal
/// viewport's border) shrinks the whole top chrome band without crowding
/// either text line or the 22px window-control icons that share this row.
const CHIP_HEIGHT_FULL: f32 = 34.0;
/// Single-line chip height used row-wide once "Show session details in
/// chips" is off (`docs/gui-design.md` "compact single-line chip"): the
/// secondary line never paints in that mode (`show`'s `suppressed_chips`),
/// so the row-wide chip height - and the whole top chrome band above the
/// terminal, via `CHROME_TOP_INSET + chip_height(..)` - shrinks to match
/// instead of leaving an empty second line's worth of dead vertical space.
/// Sized the same way `CHIP_HEIGHT_FULL` was: primary line's own ink/leading
/// plus the same top/bottom breathing room `paint_chip` reserves for a
/// single line, without the second line's height or the inter-line gap.
const CHIP_HEIGHT_COMPACT: f32 = 28.0;

/// The chip row's height for the current "Show session details in chips"
/// preference: [`CHIP_HEIGHT_FULL`] (two lines) when chips show their
/// secondary text, or [`CHIP_HEIGHT_COMPACT`] (one line) once that detail
/// relocates to the status bar instead.
const fn chip_height(show_session_details: bool) -> f32 {
    if show_session_details {
        CHIP_HEIGHT_FULL
    } else {
        CHIP_HEIGHT_COMPACT
    }
}

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
/// Quick-switch number overlay color (feature request #69): distinct from
/// every `ChipStatus` color so it never reads as a new connection state.
const CHIP_QUICK_SWITCH_NUMBER: Color32 = theme::TEXT_PRIMARY;

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
/// becomes runtime-configurable. Takes the current "Show session details in
/// chips" preference so it follows the row's actual height
/// (`CHIP_HEIGHT_FULL` vs `CHIP_HEIGHT_COMPACT`) with no further wiring.
pub const fn chrome_band_center_from_top(show_session_details: bool) -> f32 {
    CHROME_TOP_INSET + chip_height(show_session_details) / 2.0
}

const CHROME_CONTROL_SIZE: f32 = 22.0;
const CHIP_SCROLL_CONTROL_WIDTH: f32 = 20.0;

/// Space consumed by the controls at the trailing edge of the chrome row.
/// Search and Inspector are optional at narrow widths; Overflow and native
/// window controls remain visible.
const fn trailing_controls_reserved_width(show_search: bool, show_inspector: bool) -> f32 {
    let platform_controls = if cfg!(target_os = "macos") { 0 } else { 3 };
    let control_count = platform_controls + 1 + show_search as usize + show_inspector as usize;
    control_count as f32 * (CHROME_CONTROL_SIZE + 8.0) + CHROME_SIDE_INSET
}

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
    /// This chip's 1-based quick-switch position (`Cmd+1`..`Cmd+9` /
    /// `Ctrl+1`..`Ctrl+9`), or `None` if it's beyond the first
    /// `MAX_QUICK_SWITCH_TABS` chips and has no shortcut. Used to paint the
    /// quick-switch number overlay (feature request #69) when the caller
    /// reports the modifier is currently held.
    pub quick_switch_number: Option<u8>,
    /// Whether this chip's status dot should slow-pulse because its session
    /// has produced output since the tab was last active (feature request
    /// #68). Always `false` for `Neutral` chips (no dot to pulse) and for
    /// the currently active chip; the caller (`FesTermApp::chip_view_models`)
    /// is responsible for gating this on the "pulse on new output" setting
    /// before constructing this value, so this crate does not need its own
    /// copy of that preference.
    pub pulse_new_output: bool,
}

/// A user gesture translated from the chip row. The application layer maps
/// this to an `AppCommand`; this crate does not dispatch or interpret it.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum ChromeAction {
    Activate(ChipId),
    Close(ChipId),
    NewTab,
    OpenSettings,
    OpenProfiles,
    ToggleInspector,
    /// Emitted by the search-icon control; mirrors the `Ctrl+Shift+P`
    /// shortcut precedent (`app.rs::handle_shortcuts`) by asking the caller
    /// to toggle its transient, chrome-external command-palette overlay.
    TogglePalette,
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
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    chips: &[ChipViewModel],
    active: ChipId,
    inspector_open: bool,
    inspector_available: bool,
    layout: ChipLayout,
    show_session_details: bool,
    quick_switch_overlay_active: bool,
) -> Vec<ChromeAction> {
    // Compact chips (`docs/gui-design.md` "Show session details in chips"):
    // when the preference is off, every chip is a single-line chip - the
    // secondary detail line never paints or affects chip width - and the
    // active session's same detail relocates to the status bar instead
    // (handled by the caller, which still has the original `chips` values).
    let suppressed_chips;
    let chips: &[ChipViewModel] = if show_session_details {
        chips
    } else {
        suppressed_chips = chips
            .iter()
            .map(|chip| ChipViewModel {
                id: chip.id,
                primary: chip.primary.clone(),
                secondary: None,
                status: chip.status,
                closable: chip.closable,
                renamable: chip.renamable,
                quick_switch_number: chip.quick_switch_number,
                pulse_new_output: chip.pulse_new_output,
            })
            .collect::<Vec<_>>();
        &suppressed_chips
    };
    let chip_row_height = chip_height(show_session_details);
    let active_chip_changed_id = Id::new("chrome_chip_row_last_active");
    let previous_active = ui.data_mut(|data| data.get_temp::<ChipId>(active_chip_changed_id));
    let active_just_changed = previous_active != Some(active);
    ui.data_mut(|data| data.insert_temp(active_chip_changed_id, active));
    let mut actions = Vec::new();
    // Paint the full inset-inclusive row first. This uses the same surface as
    // the terminal below, making the frameless chrome and terminal one visual
    // well instead of stacking a separate colored title band above content.
    let band_rect = egui::Rect::from_min_size(
        ui.cursor().min,
        vec2(ui.available_width(), CHROME_TOP_INSET + chip_row_height),
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
        ui.set_min_height(chip_row_height);
        ui.set_max_height(chip_row_height);
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
            vec2(ui.available_width(), chip_row_height + 6.0),
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
        let natural_widths: Vec<f32> = chips
            .iter()
            .map(|chip| natural_chip_width(ui, chip, chip.id == active))
            .collect();
        let active_index = chips.iter().position(|chip| chip.id == active);
        let mut show_search = true;
        let mut show_inspector = inspector_available;
        let available_row_width = ui.available_width();
        let fixed_new_session_width = if matches!(layout, ChipLayout::SingleRowScroll) {
            chip_row_height + 8.0
        } else {
            0.0
        };
        let mut max_chip_row_width = (available_row_width
            - trailing_controls_reserved_width(show_search, show_inspector)
            - fixed_new_session_width)
            .max(0.0);
        let mut allocation = allocate_single_row_widths(
            &natural_widths,
            active_index,
            max_chip_row_width,
            ui.spacing().item_spacing.x,
        );

        // Optional global controls collapse into Overflow only after inactive
        // chips have reached minimum width, and before the strip may scroll.
        if matches!(layout, ChipLayout::SingleRowScroll) && allocation.scrolling {
            show_search = false;
            max_chip_row_width = (available_row_width
                - trailing_controls_reserved_width(show_search, show_inspector)
                - fixed_new_session_width)
                .max(0.0);
            allocation = allocate_single_row_widths(
                &natural_widths,
                active_index,
                max_chip_row_width,
                ui.spacing().item_spacing.x,
            );
        }
        if matches!(layout, ChipLayout::SingleRowScroll) && allocation.scrolling && show_inspector {
            show_inspector = false;
            max_chip_row_width = (available_row_width
                - trailing_controls_reserved_width(show_search, show_inspector)
                - fixed_new_session_width)
                .max(0.0);
            allocation = allocate_single_row_widths(
                &natural_widths,
                active_index,
                max_chip_row_width,
                ui.spacing().item_spacing.x,
            );
        }
        let scroll_width_id = Id::new("chrome_chip_row_last_scroll_width");
        let previous_scroll_width = ui.data_mut(|data| data.get_temp::<f32>(scroll_width_id));
        let scroll_width_changed =
            previous_scroll_width.is_none_or(|width| (width - max_chip_row_width).abs() > 0.5);
        ui.data_mut(|data| data.insert_temp(scroll_width_id, max_chip_row_width));
        let reveal_active = active_just_changed || (allocation.scrolling && scroll_width_changed);

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
            ui.set_max_width(max_chip_row_width);
            let mut chip_row =
                |ui: &mut Ui, forced_widths: Option<&[f32]>, include_new_session: bool| {
                    for (index, chip) in chips.iter().enumerate() {
                        let is_active = chip.id == active;
                        show_chip(
                            ui,
                            chip,
                            ChipPresentation {
                                active: is_active,
                                can_move_left: index > 0,
                                can_move_right: index + 1 < chips.len(),
                                forced_width: forced_widths.map(|widths| widths[index]),
                                row_height: chip_row_height,
                                reveal: is_active && reveal_active,
                                quick_switch_overlay_active,
                            },
                            &mut actions,
                        );
                    }
                    if include_new_session {
                        paint_new_chip_button(ui, chip_row_height, &mut actions);
                    }
                    // A small strip past the last chip lets a drag be released
                    // (or live-shuffled to) the end of the row.
                    end_of_row_drop_target(ui, &mut actions);
                };
            match layout {
                ChipLayout::Wrap => {
                    ui.horizontal_wrapped(|ui| chip_row(ui, None, true));
                }
                ChipLayout::SingleRowScroll => {
                    if allocation.scrolling {
                        ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let scroll_left = paint_chip_scroll_control(ui, false, chip_row_height);
                            let viewport_width =
                                (max_chip_row_width - 2.0 * CHIP_SCROLL_CONTROL_WIDTH).max(0.0);
                            let output = ScrollArea::horizontal()
                                .id_salt("chip_row_scroll")
                                .max_width(viewport_width)
                                .max_height(chip_row_height)
                                .min_scrolled_height(chip_row_height)
                                .content_margin(0.0)
                                .scroll_bar_visibility(
                                    egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                                )
                                .show(ui, |ui| {
                                    ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                                        chip_row(ui, Some(&allocation.widths), false)
                                    })
                                });
                            let scroll_right = paint_chip_scroll_control(ui, true, chip_row_height);
                            update_chip_scroll_state(ui, output, scroll_left, scroll_right);
                        });
                    } else {
                        ui.horizontal(|ui| chip_row(ui, Some(&allocation.widths), true));
                    }
                }
            }
        });
        if matches!(layout, ChipLayout::SingleRowScroll) && allocation.scrolling {
            paint_new_chip_button(ui, chip_row_height, &mut actions);
        }
        // Anchor this block to the same fixed rect used to paint `band_rect`
        // (rather than inheriting whatever height this outer `ui` happens to
        // report at this point) so its `Align::Center` vertical center lands
        // on exactly the chips' own center line regardless of anything the
        // chip row/scroll area above did to this `ui`'s reported cursor or
        // remaining height while painting - the earlier
        // `set_min_height`/`set_max_height` calls constrain layout of the
        // *chip row*, but this statement runs after that content and a
        // platform's text/scroll-area metrics could otherwise leave this
        // `ui`'s own remaining rect subtly taller than `chip_row_height`.
        let trailing_controls_rect = egui::Rect::from_min_size(
            band_rect.min + vec2(0.0, CHROME_TOP_INSET),
            vec2(band_rect.width(), chip_row_height),
        );
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(trailing_controls_rect)
                .layout(Layout::right_to_left(Align::Center)),
            |ui| {
                ui.add_space(CHROME_SIDE_INSET);
                if !cfg!(target_os = "macos") {
                    paint_close_icon(ui);
                    let maximized = ui.input(|input| input.viewport().maximized.unwrap_or(false));
                    paint_maximize_icon(ui, maximized);
                    paint_minimize_icon(ui);
                }
                paint_overflow_menu(
                    ui,
                    !show_search,
                    inspector_available && !show_inspector,
                    &mut actions,
                );
                if show_inspector && paint_panel_icon(ui, inspector_open) {
                    actions.push(ChromeAction::ToggleInspector);
                }
                if show_search && paint_search_icon(ui) {
                    actions.push(ChromeAction::TogglePalette);
                }
            },
        );
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

/// Thin, invisible hit zones along the window's edges/corners that forward
/// drag starts to the platform window via [`egui::ViewportCommand::BeginResize`].
///
/// Disabling native OS decorations (`with_decorations(false)` on non-macOS
/// platforms, `app/festerm/src/main.rs`) removes the OS-provided resize
/// borders along with the native title bar; this restores that interaction.
/// The actual resizing is still performed by the windowing system - this
/// only detects the pointer gesture and forwards it, the same way
/// `ViewportCommand::Minimized`/`Maximized`/`Close` are sent directly for
/// the painter-drawn window controls above rather than routed through
/// `ChromeAction`/the application command model (an OS-window-level action
/// with no application-state implications).
///
/// No-op while the window is fullscreen or maximized (there is no edge to
/// grab), and only meaningful where native decorations are off, so callers
/// should skip it on macOS.
pub fn handle_resize_border(ui: &Ui) {
    let fullscreen = ui.ctx().input(|i| i.viewport().fullscreen).unwrap_or(false);
    let maximized = ui.ctx().input(|i| i.viewport().maximized).unwrap_or(false);
    if fullscreen || maximized {
        return;
    }

    let rect = ui.max_rect();
    const RESIZE_MARGIN: f32 = 6.0;
    const CORNER_SIZE: f32 = 16.0;

    // Corners get larger square hit zones so diagonal resizing is easy to
    // grab; edges get thin strips spanning between the corners.
    let regions = [
        (
            Rect::from_min_max(
                rect.left_top(),
                rect.left_top() + vec2(CORNER_SIZE, CORNER_SIZE),
            ),
            ResizeDirection::NorthWest,
            CursorIcon::ResizeNorthWest,
            "chrome_resize_nw",
        ),
        (
            Rect::from_min_max(
                rect.right_top() - vec2(CORNER_SIZE, 0.0),
                rect.right_top() + vec2(0.0, CORNER_SIZE),
            ),
            ResizeDirection::NorthEast,
            CursorIcon::ResizeNorthEast,
            "chrome_resize_ne",
        ),
        (
            Rect::from_min_max(
                rect.left_bottom() - vec2(0.0, CORNER_SIZE),
                rect.left_bottom() + vec2(CORNER_SIZE, 0.0),
            ),
            ResizeDirection::SouthWest,
            CursorIcon::ResizeSouthWest,
            "chrome_resize_sw",
        ),
        (
            Rect::from_min_max(
                rect.right_bottom() - vec2(CORNER_SIZE, CORNER_SIZE),
                rect.right_bottom(),
            ),
            ResizeDirection::SouthEast,
            CursorIcon::ResizeSouthEast,
            "chrome_resize_se",
        ),
        (
            Rect::from_min_max(
                rect.left_top() + vec2(CORNER_SIZE, 0.0),
                rect.right_top() + vec2(-CORNER_SIZE, RESIZE_MARGIN),
            ),
            ResizeDirection::North,
            CursorIcon::ResizeNorth,
            "chrome_resize_n",
        ),
        (
            Rect::from_min_max(
                rect.left_bottom() + vec2(CORNER_SIZE, -RESIZE_MARGIN),
                rect.right_bottom() + vec2(-CORNER_SIZE, 0.0),
            ),
            ResizeDirection::South,
            CursorIcon::ResizeSouth,
            "chrome_resize_s",
        ),
        (
            Rect::from_min_max(
                rect.left_top() + vec2(0.0, CORNER_SIZE),
                rect.left_bottom() + vec2(RESIZE_MARGIN, -CORNER_SIZE),
            ),
            ResizeDirection::West,
            CursorIcon::ResizeWest,
            "chrome_resize_w",
        ),
        (
            Rect::from_min_max(
                rect.right_top() + vec2(-RESIZE_MARGIN, CORNER_SIZE),
                rect.right_bottom() + vec2(0.0, -CORNER_SIZE),
            ),
            ResizeDirection::East,
            CursorIcon::ResizeEast,
            "chrome_resize_e",
        ),
    ];

    for (region_rect, direction, cursor_icon, id) in regions {
        let response = ui.interact(region_rect, ui.id().with(id), Sense::click_and_drag());
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(cursor_icon);
        }
        if response.drag_started_by(PointerButton::Primary) {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
        }
    }
}

/// Compact "add chip" control placed right after the last chip, painted
/// with the same chip-style rounded outline as an inactive chip (mockup:
/// the `+` control reads as a small chip in its own right, not a bare
/// icon floating in the row) - the sole way to open a new Launcher tab
/// from the chrome row (`AGENTS.md`: no duplicate widget-specific copies
/// of the same operation) - an earlier full "+ Launcher" chip-style
/// button duplicated this control and was removed as redundant.
fn paint_new_chip_button(ui: &mut Ui, chip_row_height: f32, actions: &mut Vec<ChromeAction>) {
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

fn paint_chip_scroll_control(ui: &mut Ui, right: bool, height: f32) -> bool {
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

fn update_chip_scroll_state<R>(
    ui: &Ui,
    output: egui::scroll_area::ScrollAreaOutput<R>,
    scroll_left: bool,
    scroll_right: bool,
) {
    let max_offset = (output.content_size.x - output.inner_rect.width()).max(0.0);
    let mut state = output.state;
    let page = (output.inner_rect.width() * 0.8).max(CHIP_INACTIVE_MIN_WIDTH);
    let mut delta = if scroll_left {
        -page
    } else if scroll_right {
        page
    } else {
        0.0
    };

    if DragAndDrop::has_any_payload(ui.ctx()) {
        if let Some(pointer) = ui.ctx().pointer_interact_pos() {
            const EDGE_ZONE: f32 = 24.0;
            const EDGE_STEP: f32 = 10.0;
            if output.inner_rect.contains(pointer) {
                if pointer.x <= output.inner_rect.left() + EDGE_ZONE {
                    delta = -EDGE_STEP;
                } else if pointer.x >= output.inner_rect.right() - EDGE_ZONE {
                    delta = EDGE_STEP;
                }
            }
        }
    }

    if delta != 0.0 {
        state.offset.x = (state.offset.x + delta).clamp(0.0, max_offset);
        state.store(ui.ctx(), output.id);
        ui.ctx().request_repaint();
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
    // Distinct from a chip's own close control's "Close" label
    // (`paint_close_button`): both can be present in the same frame on
    // non-macOS platforms (custom titlebar), and an ambiguous shared label
    // made `harness.get_by_label("Close")` match either one nondeterministically
    // in headless tests (`chip_secondary_line_stays_clear_of_the_close_button`
    // failed only on Linux CI, where this icon - skipped on macOS in favor of
    // native traffic lights - is also painted).
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Close window"));
    let color = if response.hovered() {
        CHROME_CLOSE_HOVER
    } else {
        CHROME_ICON_COLOR
    };
    icon::paint(ui.painter(), Icon::Close, rect.shrink(3.0), color);
    let response = response.on_hover_text("Close window");
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
        "\u{2318}+Shift+P"
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
fn paint_overflow_menu(
    ui: &mut Ui,
    include_palette: bool,
    include_inspector: bool,
    actions: &mut Vec<ChromeAction>,
) {
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
        if include_palette && ui.button("Command palette").clicked() {
            actions.push(ChromeAction::TogglePalette);
            ui.close();
        }
        if include_inspector && ui.button("Session inspector").clicked() {
            actions.push(ChromeAction::ToggleInspector);
            ui.close();
        }
        if include_palette || include_inspector {
            ui.separator();
        }
        if ui.button("Open Settings").clicked() {
            actions.push(ChromeAction::OpenSettings);
            ui.close();
        }
        if ui.button("Open Profiles").clicked() {
            actions.push(ChromeAction::OpenProfiles);
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

/// The drop target remains at the logical end of the scrollable chip strip.
/// New Session is reserved and painted outside that strip so it cannot scroll
/// away with inactive chips.
const fn chip_row_end_target_width() -> f32 {
    24.0
}

#[derive(Debug, PartialEq)]
struct SingleRowAllocation {
    widths: Vec<f32>,
    scrolling: bool,
}

/// Allocates the focused chip at its natural width when the scroll viewport
/// can contain it, then water-fills the remaining budget across inactive
/// chips. Once every inactive chip reaches its minimum, those compact widths
/// are retained inside the scroll area.
fn allocate_single_row_widths(
    natural_widths: &[f32],
    active_index: Option<usize>,
    available_width: f32,
    item_spacing: f32,
) -> SingleRowAllocation {
    let count = natural_widths.len();
    if count == 0 {
        return SingleRowAllocation {
            widths: Vec::new(),
            scrolling: false,
        };
    }
    let spacing_total = item_spacing * count as f32;
    let available_for_chips =
        (available_width - chip_row_end_target_width() - spacing_total).max(0.0);
    let natural_total: f32 = natural_widths.iter().sum();
    if natural_total <= available_for_chips {
        return SingleRowAllocation {
            widths: natural_widths.to_vec(),
            scrolling: false,
        };
    }

    let focused_width = active_index.and_then(|index| natural_widths.get(index).copied());
    let inactive_count = count - usize::from(focused_width.is_some());
    let inactive_budget = (available_for_chips - focused_width.unwrap_or(0.0)).max(0.0);
    let inactive_minimum_total = CHIP_INACTIVE_MIN_WIDTH * inactive_count as f32;
    let scrolling = inactive_minimum_total > inactive_budget;
    if scrolling {
        let focused_max_width = (available_width - 2.0 * CHIP_SCROLL_CONTROL_WIDTH).max(0.0);
        let widths = natural_widths
            .iter()
            .enumerate()
            .map(|(index, &width)| {
                if Some(index) == active_index {
                    width.min(focused_max_width)
                } else {
                    CHIP_INACTIVE_MIN_WIDTH
                }
            })
            .collect();
        return SingleRowAllocation {
            widths,
            scrolling: true,
        };
    }

    let mut low = CHIP_INACTIVE_MIN_WIDTH;
    let mut high = natural_widths
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != active_index)
        .map(|(_, &width)| width)
        .fold(CHIP_INACTIVE_MIN_WIDTH, f32::max);
    for _ in 0..32 {
        let cap = (low + high) / 2.0;
        let used: f32 = natural_widths
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != active_index)
            .map(|(_, &width)| width.min(cap).max(CHIP_INACTIVE_MIN_WIDTH))
            .sum();
        if used <= inactive_budget {
            low = cap;
        } else {
            high = cap;
        }
    }
    let widths = natural_widths
        .iter()
        .enumerate()
        .map(|(index, &width)| {
            if Some(index) == active_index {
                width
            } else {
                width.min(low).max(CHIP_INACTIVE_MIN_WIDTH)
            }
        })
        .collect();
    SingleRowAllocation {
        widths,
        scrolling: false,
    }
}

/// Ephemeral, UI-only rename buffer key: whether `chip_id` currently has an
/// in-progress rename edit, and its current (uncommitted) text. Not part of
/// `ChipViewModel` because this module is pure presentation
/// (`docs/gui-design.md`); the caller only ever sees a committed
/// `ChromeAction::Rename`.
fn rename_buffer_id(id: ChipId) -> Id {
    Id::new("chrome_chip_rename").with(id.0)
}

/// Measures a chip's true content-driven width directly from its text,
/// independent of whatever rect it's actually painted into.
///
/// This exists because `paint_chip` used to be measured *after* painting it
/// into a rect the caller already chose (`content_response.rect`, which is
/// just `ui.max_rect()` - the imposed rect, echoed straight back, not the
/// label's actual desired size). That made a chip's cached "natural" width
/// a no-op feedback loop: whatever width it was given is exactly the width
/// it reported back, so a chip could never discover it wanted to be wider
/// than whatever it happened to start at - including its very first frame,
/// which defaulted to the minimum before a real width was ever cached.
/// In practice every chip appeared permanently stuck at that minimum
/// and the Chrome-style "shrink before scroll" row always had nothing to
/// shrink from. Measuring the text directly (mirroring `paint_chip`'s own
/// insets/spacing) sidesteps the chicken-and-egg problem entirely.
fn natural_chip_width(ui: &Ui, chip: &ChipViewModel, show_close: bool) -> f32 {
    let ctx = ui.ctx();
    let style = ui.style();
    let body_font = style
        .text_styles
        .get(&egui::TextStyle::Body)
        .cloned()
        .unwrap_or_else(egui::FontId::default);
    let small_font = style
        .text_styles
        .get(&egui::TextStyle::Small)
        .cloned()
        .unwrap_or_else(|| egui::FontId::proportional(10.0));

    const LEFT_INSET: f32 = 8.0;
    const RIGHT_PADDING: f32 = 8.0;
    const DOT_DIAMETER: f32 = 8.0;
    const DOT_LABEL_SPACING: f32 = 6.0;
    // `CLOSE_INSET` + `CLOSE_SIZE` from `paint_chip`'s close-button layout.
    const CLOSE_RESERVED: f32 = 24.0;

    let primary_width = ctx.fonts_mut(|f| {
        f.layout_no_wrap(chip.primary.clone(), body_font, Color32::WHITE)
            .size()
            .x
    });
    let mut primary_line = LEFT_INSET + primary_width;
    if !matches!(chip.status, ChipStatus::Neutral) {
        primary_line += DOT_DIAMETER + DOT_LABEL_SPACING;
    }

    let mut width: f32 = primary_line;
    if let Some(secondary) = &chip.secondary {
        let secondary_width = ctx.fonts_mut(|f| {
            f.layout_no_wrap(secondary.clone(), small_font, Color32::WHITE)
                .size()
                .x
        });
        let indent = if matches!(chip.status, ChipStatus::Neutral) {
            8.0
        } else {
            22.0
        };
        width = width.max(indent + secondary_width);
    }

    width += RIGHT_PADDING;
    if show_close {
        width += CLOSE_RESERVED;
    }

    let minimum = if show_close {
        CHIP_FOCUSED_MIN_WIDTH
    } else {
        CHIP_INACTIVE_MIN_WIDTH
    };
    width.clamp(minimum, CHIP_MAX_WIDTH)
}

struct ChipPresentation {
    active: bool,
    can_move_left: bool,
    can_move_right: bool,
    forced_width: Option<f32>,
    row_height: f32,
    reveal: bool,
    quick_switch_overlay_active: bool,
}

fn show_chip(
    ui: &mut Ui,
    chip: &ChipViewModel,
    presentation: ChipPresentation,
    actions: &mut Vec<ChromeAction>,
) {
    let ChipPresentation {
        active,
        can_move_left,
        can_move_right,
        forced_width,
        row_height,
        reveal,
        quick_switch_overlay_active,
    } = presentation;
    let chip_id = chip_widget_id(chip.id);
    let ctx = ui.ctx().clone();
    // The chip's true content-driven width, measured directly from its text
    // (see `natural_chip_width`'s doc comment for why this can't be
    // discovered by measuring the *painted* chip's response rect instead).
    let natural_size = vec2(natural_chip_width(ui, chip, active), row_height);
    // Inactive chips may be allocated below their natural size; their text
    // truncates to the width selected by the row allocator.
    let bg_size = vec2(forced_width.unwrap_or(natural_size.x), row_height);

    if ctx.is_being_dragged(chip_id) {
        // Currently being dragged: keep the payload alive, reserve the
        // chip's last-known footprint in the row (so drop targets don't
        // collapse out from under the pointer), and paint the real chip
        // floating at the pointer position, exactly as
        // `Ui::dnd_drag_source` does natively for its wrapped content.
        DragAndDrop::set_payload(&ctx, chip.id);

        let (_, ghost_rect) = ui.allocate_space(natural_size);
        ui.painter().rect_stroke(
            ghost_rect,
            4.0,
            Stroke::new(1.0, CHIP_INACTIVE_OUTLINE),
            egui::StrokeKind::Inside,
        );

        let layer_id = LayerId::new(Order::Tooltip, chip_id);
        let mut floating_ui =
            ui.new_child(UiBuilder::new().max_rect(ghost_rect).layer_id(layer_id));
        let chip_painter = floating_ui.painter().clone();
        let content_response = paint_chip(
            &chip_painter,
            &mut floating_ui,
            chip,
            ChipPaintState {
                active,
                hovered: false,
                show_close: active,
                chip_id,
                outer_rect: ghost_rect,
                quick_switch_overlay_active,
            },
            actions,
        );

        if let Some(pointer_pos) = ctx.pointer_interact_pos() {
            let delta = pointer_pos - content_response.rect.center();
            ctx.transform_layer_shapes(layer_id, TSTransform::from_translation(delta));
        }
        return;
    }

    let (_, bg_rect) = ui.allocate_space(bg_size);
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
    // Bring a freshly-activated chip into view (`reveal`, set by the
    // caller only on the frame its `active` id changed): with
    // `ChipLayout::SingleRowScroll`, once there's no more room to shrink
    // chips further, the row falls back to a horizontally scrolling
    // `ScrollArea` that otherwise starts - and stays - at its initial
    // offset, leaving a newly created (or newly clicked) chip past the
    // fold completely invisible until the user manually scrolled to find
    // it. A no-op outside any scrolling ancestor (`ChipLayout::Wrap`, or
    // a `SingleRowScroll` row that still fits without scrolling).
    if reveal {
        bg_response.scroll_to_me(Some(Align::Center));
    }

    // The close control is a deliberately scarce affordance
    // (`docs/gui-design.md`): only the active chip ever shows it, matching
    // the mockup where inactive chips carry a plain dark-grey outline and
    // no close affordance at all, even on hover.
    let show_close = active;
    let hovered = bg_response.hovered();
    let chip_painter = ui.painter().clone();
    let mut content_ui = ui.new_child(UiBuilder::new().max_rect(bg_rect));
    paint_chip(
        &chip_painter,
        &mut content_ui,
        chip,
        ChipPaintState {
            active,
            hovered,
            show_close,
            chip_id,
            outer_rect: bg_rect,
            quick_switch_overlay_active,
        },
        actions,
    );

    if bg_response.clicked() {
        actions.push(ChromeAction::Activate(chip.id));
    }

    // Double-clicking anywhere on the chip (including the title label,
    // which senses only hover so it doesn't compete with `bg_response` for
    // click/drag priority - see `paint_chip_primary`) starts a rename,
    // unless a rename is already in progress.
    let rename_id = rename_buffer_id(chip.id);
    let already_editing = ui.data(|d| d.get_temp::<String>(rename_id)).is_some();
    if chip.renamable && !already_editing && bg_response.double_clicked() {
        ui.data_mut(|d| d.insert_temp(rename_id, chip.primary.clone()));
        actions.push(ChromeAction::RenameStarted {
            restore_focus: None,
        });
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
struct ChipPaintState {
    active: bool,
    hovered: bool,
    show_close: bool,
    chip_id: Id,
    outer_rect: egui::Rect,
    quick_switch_overlay_active: bool,
}

fn paint_chip(
    painter: &egui::Painter,
    ui: &mut Ui,
    chip: &ChipViewModel,
    state: ChipPaintState,
    actions: &mut Vec<ChromeAction>,
) -> egui::Response {
    let ChipPaintState {
        active,
        hovered,
        show_close,
        chip_id,
        outer_rect,
        quick_switch_overlay_active,
    } = state;
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

    painter.rect_filled(outer_rect, corner_radius, fill);
    painter.rect_stroke(outer_rect, corner_radius, stroke, egui::StrokeKind::Inside);

    // The close control is positioned from the chip's own outer rect
    // (evenly inset from the right edge, vertically centered) rather than
    // flowing through the primary line's layout: this keeps its position
    // fixed regardless of label length and avoids the label being pulled
    // towards the right edge, which a right-to-left sub-layout previously
    // caused for short labels. Centering vertically (rather than a fixed
    // inset from the top) is what lets it track `outer_rect`'s own height
    // as chips shrink in compact mode (`CHIP_HEIGHT_COMPACT`) instead of
    // staying pinned to where the top-inset would have placed it for the
    // taller two-line chip height.
    const CLOSE_SIZE: f32 = 16.0;
    const CLOSE_INSET: f32 = 8.0;
    let close_rect = if chip.closable && show_close {
        let rect = egui::Rect::from_min_size(
            egui::pos2(
                outer_rect.right() - CLOSE_INSET - CLOSE_SIZE,
                outer_rect.center().y - CLOSE_SIZE / 2.0,
            ),
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
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.spacing_mut().interact_size.y = 0.0;
        ui.style_mut().visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        let reserved = close_rect.map_or(0.0, |rect| outer_rect.right() - rect.left());
        if let Some(secondary) = &chip.secondary {
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.set_min_size(outer_rect.size());
                ui.add_space(3.0);
                ui.horizontal(|ui| {
                    paint_chip_primary_contents(
                        ui,
                        chip,
                        reserved,
                        actions,
                        quick_switch_overlay_active,
                    );
                });
                ui.horizontal(|ui| {
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
                ui.add_space(4.0);
            });
        } else {
            let line_height = ui.text_style_height(&egui::TextStyle::Body);
            let line_rect = egui::Rect::from_center_size(
                outer_rect.center(),
                vec2(outer_rect.width(), line_height),
            );
            let mut line_ui = ui.new_child(
                UiBuilder::new()
                    .max_rect(line_rect)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            paint_chip_primary_contents(
                &mut line_ui,
                chip,
                reserved,
                actions,
                quick_switch_overlay_active,
            );
        }
    });

    ui.interact(outer_rect, chip_id.with("content"), Sense::hover())
}

fn paint_chip_primary_contents(
    ui: &mut Ui,
    chip: &ChipViewModel,
    reserved: f32,
    actions: &mut Vec<ChromeAction>,
    quick_switch_overlay_active: bool,
) {
    ui.add_space(8.0);
    // Feature request #69: while the quick-switch modifier is held and the
    // preference is on, an eligible chip's quick-switch number temporarily
    // takes the place of its usual status presentation - the status dot
    // for session chips, or a reserved slot for `Neutral` chips (Launcher/
    // Settings/etc.) that otherwise paint no dot at all.
    let show_number = quick_switch_overlay_active && chip.quick_switch_number.is_some();
    if show_number {
        paint_quick_switch_number(ui, chip.quick_switch_number.expect("checked above"));
    } else if !matches!(chip.status, ChipStatus::Neutral) {
        paint_status_dot(ui, chip.status, chip.pulse_new_output);
    }

    let rename_id = rename_buffer_id(chip.id);
    let editing: Option<String> = ui.data(|d| d.get_temp(rename_id));
    ui.scope(|ui| {
        let max_width = (ui.available_width() - reserved).max(0.0);
        ui.set_max_width(max_width);
        paint_chip_primary(ui, chip, rename_id, editing, actions);
    });
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
        // Deliberately `Sense::hover()` only (no click/drag), matching the
        // secondary line below: the chip's own `bg_response` (covering the
        // whole chip footprint, including this label's pixels) is the sole
        // widget that senses click-and-drag here. Giving this label its own
        // `Sense::click()` used to let it win the *click* half of egui's
        // hit-test tie-break (it's registered on top of `bg_response`),
        // which could leave a press-and-drag started on the title text
        // attributed to whichever drag-sensing widget the hit-test fell
        // back to next - sometimes the row's own native-window-drag region
        // instead of the chip's reorder drag - rather than reliably
        // reordering the chip the same way starting a drag on the
        // secondary line already did. Activation and rename-start are
        // instead driven entirely by `bg_response` in `show_chip`.
        ui.add(
            egui::Label::new(label)
                // See the secondary-line label above: this is clickable
                // navigation chrome, not selectable text, so the hover
                // cursor should read as a plain arrow, not an I-beam.
                .selectable(false)
                .truncate(),
        );
    }
}

/// Compact, non-color-exclusive connection-state dot, painted directly
/// rather than relying on a glyph the active font may not have coverage for
/// (the previous `\u{25cf}` rendered as tofu/an empty box on this machine).
fn paint_status_dot(ui: &mut Ui, status: ChipStatus, pulse: bool) {
    let diameter = 8.0;
    // Allocate at the primary label's own line height (rather than just
    // the dot's diameter) so this row's cross-axis `Align::Center`
    // computes the same center line for both the dot and the label text,
    // instead of centering the dot within a shorter box that happens to
    // sit slightly off from the text's own optical center.
    let text_height = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, response) = ui.allocate_exact_size(vec2(diameter, text_height), Sense::hover());
    let color = if pulse {
        // Feature request #68: a slow (~2.4s period), smooth fade between
        // full and low opacity - deliberately slower and gentler than the
        // fixed-solid connection-state dot so it reads as an ambient "new
        // output" cue rather than an alarm, and never changes the dot's
        // hue, which stays reserved for connection-state semantics.
        let phase = (ui.input(|i| i.time) * std::f64::consts::TAU / 2.4).sin();
        let alpha = (0.35 + 0.65 * (phase * 0.5 + 0.5)) as f32;
        ui.ctx().request_repaint();
        status.color().gamma_multiply(alpha)
    } else {
        status.color()
    };
    ui.painter()
        .circle_filled(rect.center(), diameter / 2.0, color);
    response.on_hover_text(status.accessible_label());
}

/// Overlay painted in the status-dot's slot (or, for `Neutral` chips that
/// have no dot slot at all, a same-sized reserved slot) while the
/// quick-switch modifier is held and the preference is on (feature request
/// #69): the chip's 1-based `Cmd+N`/`Ctrl+N` quick-switch digit, in an
/// accent color distinct from any status color so it never reads as a new
/// connection state.
fn paint_quick_switch_number(ui: &mut Ui, number: u8) {
    let diameter = 8.0;
    let text_height = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, response) = ui.allocate_exact_size(vec2(diameter, text_height), Sense::hover());
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        number.to_string(),
        egui::FontId::new(10.0, egui::FontFamily::Monospace),
        CHIP_QUICK_SWITCH_NUMBER,
    );
    let label = format!("Quick switch: {number}");
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, label.clone()));
    response.on_hover_text(label);
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
            quick_switch_number: None,
            pulse_new_output: false,
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
                    let actions = show(
                        ui,
                        &state.chips,
                        state.active,
                        false,
                        true,
                        state.layout,
                        true,
                        false,
                    );
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
                    let actions = show(
                        ui,
                        &state.chips,
                        state.active,
                        false,
                        true,
                        state.layout,
                        true,
                        false,
                    );
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
                    let actions = show(
                        ui,
                        &state.chips,
                        state.active,
                        false,
                        true,
                        state.layout,
                        true,
                        false,
                    );
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
    fn single_row_layout_shrinks_chips_before_falling_back_to_a_scrollbar() {
        let chips: Vec<ChipViewModel> = (1..=4)
            .map(|id| {
                chip(
                    id,
                    &format!("workspace-session-number-{id}-long-descriptive-name"),
                )
            })
            .collect();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 200.0))
            .with_step_dt(0.01)
            .build_ui_state(
                |ui, state: &mut ChromeHarnessState| {
                    let actions = show(
                        ui,
                        &state.chips,
                        state.active,
                        false,
                        true,
                        state.layout,
                        true,
                        false,
                    );
                    state.observed.extend(actions);
                },
                ChromeHarnessState {
                    chips,
                    active: ChipId(1),
                    layout: ChipLayout::SingleRowScroll,
                    observed: Vec::new(),
                },
            );
        harness.run();

        let focused_rect = harness
            .get_by_label("workspace-session-number-1-long-descriptive-name chip")
            .rect();
        assert!(
            (focused_rect.width() - CHIP_MAX_WIDTH).abs() < 0.01,
            "focused chip must retain its normal width, got {focused_rect:?}"
        );
        assert!(harness.query_by_label("Scroll chips left").is_none());
        assert!(harness.query_by_label("Scroll chips right").is_none());

        let window_right = 900.0;
        for id in 2..=4 {
            let label = format!("workspace-session-number-{id}-long-descriptive-name chip");
            let chip_rect = harness.get_by_label(&label).rect();
            assert!(
                chip_rect.right() <= window_right,
                "expected {label}'s rect ({chip_rect:?}) to stay within the \
                 {window_right}-wide window instead of overflowing into a \
                 scrolled-away area"
            );
            assert!(
                (CHIP_INACTIVE_MIN_WIDTH..CHIP_MAX_WIDTH).contains(&chip_rect.width()),
                "expected inactive {label} to absorb the shortage without \
                 reaching overflow, got {chip_rect:?}"
            );
        }
    }

    #[test]
    fn non_scrolling_single_row_places_new_session_next_to_last_chip() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(752.0, 200.0))
            .build_ui(|ui| {
                show(
                    ui,
                    &[chip(1, "one"), chip(2, "two"), chip(3, "three")],
                    ChipId(3),
                    false,
                    true,
                    ChipLayout::SingleRowScroll,
                    true,
                    false,
                );
            });
        harness.run();

        assert!(harness.query_by_label("Scroll chips left").is_none());
        let last_chip = harness.get_by_label("three chip").rect();
        let new_session = harness.get_by_label("New tab").rect();
        assert_eq!(new_session.left() - last_chip.right(), 8.0);
    }

    #[test]
    fn activating_an_offscreen_chip_scrolls_it_into_view_in_single_row_layout() {
        let chips: Vec<ChipViewModel> = (1..=12)
            .map(|id| chip(id, &format!("session-{id}-with-a-long-descriptive-name")))
            .collect();
        let window_width = 360.0;
        let mut harness = Harness::builder()
            .with_size(egui::vec2(window_width, 200.0))
            .with_step_dt(0.01)
            .build_ui_state(
                |ui, state: &mut ChromeHarnessState| {
                    let actions = show(
                        ui,
                        &state.chips,
                        state.active,
                        false,
                        true,
                        state.layout,
                        true,
                        false,
                    );
                    state.observed.extend(actions);
                },
                ChromeHarnessState {
                    chips,
                    active: ChipId(1),
                    layout: ChipLayout::SingleRowScroll,
                    observed: Vec::new(),
                },
            );
        harness.run();

        // Sanity check this test actually exercises the scroll fallback:
        // the last chip must start out beyond the window, unreachable
        // without scrolling.
        let last_label = "session-12-with-a-long-descriptive-name chip";
        let before = harness.get_by_label(last_label).rect();
        assert!(
            before.left() >= window_width,
            "expected the last chip to start out past the fold ({before:?}) so this test \
             actually exercises the scroll-into-view behavior"
        );
        assert!(harness.query_by_label("Scroll chips left").is_some());
        assert!(harness.query_by_label("Scroll chips right").is_some());
        let first_label = "session-1-with-a-long-descriptive-name chip";
        let first_before = harness.get_by_label(first_label).rect();
        harness.get_by_label("Scroll chips right").click();
        harness.run();
        harness.run();
        let first_after = harness.get_by_label(first_label).rect();
        assert!(
            first_after.left() < first_before.left(),
            "right scroll affordance must move chip content left, got \
             before={first_before:?}, after={first_after:?}"
        );

        harness.state_mut().active = ChipId(12);
        // `scroll_to_me` schedules an animated scroll rather than jumping
        // instantly; a handful of frames lets it settle.
        for _ in 0..30 {
            harness.run();
        }

        let after = harness.get_by_label(last_label).rect();
        let visible_left = harness.get_by_label("Scroll chips left").rect().right();
        let visible_right = harness.get_by_label("Scroll chips right").rect().left();
        assert!(
            after.left() >= visible_left - 1.0 && after.right() <= visible_right + 1.0,
            "expected activating {last_label} to scroll it back into the \
             visible chip viewport {visible_left}..{visible_right}, got {after:?}"
        );
        let expected_active_width = CHIP_MAX_WIDTH.min(visible_right - visible_left);
        assert!(
            (after.width() - expected_active_width).abs() < 0.01,
            "newly focused chip must expand to the largest fully visible \
             width, got {after:?}"
        );
        let old_focused = harness
            .get_by_label("session-1-with-a-long-descriptive-name chip")
            .rect();
        assert!(
            (old_focused.width() - CHIP_INACTIVE_MIN_WIDTH).abs() < 0.01,
            "scroll fallback must retain the old chip's compacted inactive \
             width, got {old_focused:?}"
        );
    }

    #[test]
    fn scrolling_compact_chips_keep_the_non_scrolling_row_baseline() {
        let chips: Vec<ChipViewModel> = (1..=8)
            .map(|id| chip(id, &format!("session-{id}-with-a-long-descriptive-name")))
            .collect();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(360.0, 200.0))
            .build_ui_state(
                |ui, state: &mut ChromeHarnessState| {
                    let actions = show(
                        ui,
                        &state.chips,
                        state.active,
                        false,
                        true,
                        state.layout,
                        false,
                        false,
                    );
                    state.observed.extend(actions);
                },
                ChromeHarnessState {
                    chips,
                    active: ChipId(1),
                    layout: ChipLayout::SingleRowScroll,
                    observed: Vec::new(),
                },
            );
        harness.run();

        assert!(harness.query_by_label("Scroll chips left").is_some());
        let chip_rect = harness
            .get_by_label("session-1-with-a-long-descriptive-name chip")
            .rect();
        let scroll_control_rect = harness.get_by_label("Scroll chips left").rect();
        assert_eq!(chip_rect.height(), CHIP_HEIGHT_COMPACT);
        assert_eq!(chip_rect.top(), scroll_control_rect.top());
        assert_eq!(chip_rect.bottom(), scroll_control_rect.bottom());
    }

    #[test]
    fn inactive_chips_can_use_the_approved_narrow_natural_width() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one"), chip(2, "two")],
            active: ChipId(1),
            layout: ChipLayout::SingleRowScroll,
            observed: Vec::new(),
        });
        harness.run();

        let focused = harness.get_by_label("one chip").rect();
        let inactive = harness.get_by_label("two chip").rect();
        assert_eq!(focused.width(), CHIP_FOCUSED_MIN_WIDTH);
        assert_eq!(inactive.width(), CHIP_INACTIVE_MIN_WIDTH);
    }

    #[test]
    fn narrow_single_row_collapses_optional_controls_into_overflow() {
        let chips = (1..=6)
            .map(|id| chip(id, &format!("long-session-name-{id}")))
            .collect();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(360.0, 200.0))
            .with_step_dt(0.01)
            .build_ui_state(
                |ui, state: &mut ChromeHarnessState| {
                    let actions = show(
                        ui,
                        &state.chips,
                        state.active,
                        false,
                        true,
                        state.layout,
                        true,
                        false,
                    );
                    state.observed.extend(actions);
                },
                ChromeHarnessState {
                    chips,
                    active: ChipId(1),
                    layout: ChipLayout::SingleRowScroll,
                    observed: Vec::new(),
                },
            );
        harness.run();

        assert!(harness
            .query_by_label_contains("Command palette (")
            .is_none());
        assert!(harness.query_by_label("Toggle session inspector").is_none());
        let new_session = harness.get_by_label("New tab").rect();
        let overflow = harness.get_by_label("More actions").rect();
        assert!(
            new_session.left() >= 0.0 && new_session.right() <= overflow.left(),
            "New Session must remain fixed and visible before Overflow, got \
             new={new_session:?}, overflow={overflow:?}"
        );
        harness.get_by_label("More actions").click();
        harness.run();
        assert!(harness.query_by_label("Command palette").is_some());
        assert!(harness.query_by_label("Session inspector").is_some());
    }

    #[test]
    fn a_single_wide_chip_renders_at_its_natural_width_not_stuck_at_the_minimum() {
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(
                1,
                "a-very-long-session-label-that-should-need-more-than-min-width",
            )],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });
        harness.run();

        let chip_rect = harness
            .get_by_label("a-very-long-session-label-that-should-need-more-than-min-width chip")
            .rect();
        assert!(
            chip_rect.width() > CHIP_FOCUSED_MIN_WIDTH,
            "expected the chip's natural width ({chip_rect:?}) to exceed \
             its focused minimum ({CHIP_FOCUSED_MIN_WIDTH})"
        );
    }

    fn allocation_for_chip_budget(
        natural: &[f32],
        active_index: usize,
        chip_budget: f32,
    ) -> SingleRowAllocation {
        let spacing = 8.0;
        let available = chip_budget + chip_row_end_target_width() + spacing * natural.len() as f32;
        allocate_single_row_widths(natural, Some(active_index), available, spacing)
    }

    #[test]
    fn single_row_keeps_natural_widths_when_they_fit() {
        let natural = vec![184.0, 136.0, 136.0, 80.0];
        let allocation = allocation_for_chip_budget(&natural, 0, natural.iter().sum());
        assert_eq!(allocation.widths, natural);
        assert!(!allocation.scrolling);
    }

    #[test]
    fn focused_chip_priority_is_identical_at_both_vertical_densities() {
        fn widths(show_session_details: bool) -> Vec<f32> {
            let chips: Vec<_> = (1..=4)
                .map(|id| {
                    chip(
                        id,
                        &format!("workspace-session-number-{id}-long-descriptive-name"),
                    )
                })
                .collect();
            let mut harness = Harness::builder()
                .with_size(egui::vec2(900.0, 200.0))
                .build_ui(move |ui| {
                    show(
                        ui,
                        &chips,
                        ChipId(1),
                        false,
                        true,
                        ChipLayout::SingleRowScroll,
                        show_session_details,
                        false,
                    );
                });
            harness.run();
            (1..=4)
                .map(|id| {
                    harness
                        .get_by_label(&format!(
                            "workspace-session-number-{id}-long-descriptive-name chip"
                        ))
                        .rect()
                        .width()
                })
                .collect()
        }

        for measured in [widths(true), widths(false)] {
            assert!((measured[0] - CHIP_MAX_WIDTH).abs() < 0.01);
            assert!(measured[1..].iter().all(|width| *width < CHIP_MAX_WIDTH));
        }
    }

    #[test]
    fn single_row_compacts_only_inactive_chips_with_water_filling() {
        let natural = vec![184.0, 136.0, 136.0, 80.0, 80.0];
        let allocation = allocation_for_chip_budget(&natural, 0, 184.0 + 84.0 + 84.0 + 80.0 + 80.0);

        assert!(!allocation.scrolling);
        assert_eq!(allocation.widths[0], 184.0);
        assert!((allocation.widths[1] - 84.0).abs() < 0.01);
        assert!((allocation.widths[2] - 84.0).abs() < 0.01);
        assert_eq!(allocation.widths[3], 80.0);
        assert_eq!(allocation.widths[4], 80.0);
    }

    #[test]
    fn single_row_scrolls_only_after_every_inactive_chip_reaches_minimum() {
        let natural = vec![184.0, 136.0, 136.0, 80.0, 80.0];
        let threshold = 184.0 + CHIP_INACTIVE_MIN_WIDTH * 4.0;

        let at_threshold = allocation_for_chip_budget(&natural, 0, threshold);
        assert!(!at_threshold.scrolling);
        assert_eq!(at_threshold.widths[0], 184.0);
        assert!(at_threshold.widths[1..]
            .iter()
            .all(|width| (*width - CHIP_INACTIVE_MIN_WIDTH).abs() < 0.01));

        let below_threshold = allocation_for_chip_budget(&natural, 0, threshold - 1.0);
        assert!(below_threshold.scrolling);
        assert_eq!(below_threshold.widths[0], 184.0);
        assert!(below_threshold.widths[1..]
            .iter()
            .all(|width| (*width - CHIP_INACTIVE_MIN_WIDTH).abs() < 0.01));
    }

    #[test]
    fn changing_focus_protects_the_new_focused_chip_not_the_old_one() {
        let natural = vec![184.0, 160.0, 136.0, 136.0];
        let budget = 184.0 + 100.0 * 3.0;
        let first = allocation_for_chip_budget(&natural, 0, budget);
        let second = allocation_for_chip_budget(&natural, 1, budget);

        assert_eq!(first.widths[0], 184.0);
        assert!(first.widths[1] < 160.0);
        assert_eq!(second.widths[1], 160.0);
        assert!(second.widths[0] < 184.0);
    }

    #[test]
    fn removing_chips_allows_remaining_chips_to_grow_back_to_natural_width() {
        let crowded = vec![184.0, 160.0, 150.0, 140.0];
        let budget = 184.0 + 100.0 * 3.0;
        let before = allocation_for_chip_budget(&crowded, 0, budget);
        assert!(before.widths[1] < crowded[1]);

        let fewer = vec![184.0, 160.0];
        let after = allocation_for_chip_budget(&fewer, 0, budget);
        assert_eq!(after.widths, fewer);
        assert!(!after.scrolling);
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
    fn hiding_session_details_suppresses_every_chip_secondary_line() {
        // Regression test for `docs/gui-design.md` "Show session details in
        // chips": turning the preference off must hide the secondary detail
        // line on every chip (not just the active one), even though the
        // caller's own `ChipViewModel`s still carry `secondary` values.
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 200.0))
            .build_ui(|ui| {
                show(
                    ui,
                    &[
                        ChipViewModel {
                            secondary: Some("Launcher".to_owned()),
                            ..chip(1, "one")
                        },
                        ChipViewModel {
                            secondary: Some("Local · macOS".to_owned()),
                            ..chip(2, "two")
                        },
                    ],
                    ChipId(1),
                    false,
                    true,
                    ChipLayout::Wrap,
                    false,
                    false,
                );
            });
        harness.run();

        assert!(harness.query_by_label("Launcher").is_none());
        assert!(harness.query_by_label("Local · macOS").is_none());
        assert!(harness.query_by_label("one").is_some());
        assert!(harness.query_by_label("two").is_some());
    }

    #[test]
    fn showing_session_details_still_paints_every_chip_secondary_line() {
        // Companion to the test above: the default (on) preference must
        // keep painting secondary text exactly as before this preference
        // existed.
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 200.0))
            .build_ui(|ui| {
                show(
                    ui,
                    &[ChipViewModel {
                        secondary: Some("Launcher".to_owned()),
                        ..chip(1, "one")
                    }],
                    ChipId(1),
                    false,
                    true,
                    ChipLayout::Wrap,
                    true,
                    false,
                );
            });
        harness.run();

        assert!(harness.query_by_label("Launcher").is_some());
    }

    #[test]
    fn hiding_session_details_shrinks_chip_height_and_the_chrome_band() {
        // Regression test: turning "Show session details in chips" off is
        // meant to yield a narrower top chrome band (a single-line chip
        // row), not just hide the secondary text while chips keep their
        // old two-line footprint.
        fn chip_height_with(show_session_details: bool) -> f32 {
            let mut harness = Harness::builder()
                .with_size(egui::vec2(900.0, 200.0))
                .build_ui(|ui| {
                    show(
                        ui,
                        &[chip(1, "one")],
                        ChipId(1),
                        false,
                        true,
                        ChipLayout::Wrap,
                        show_session_details,
                        false,
                    );
                });
            harness.run();
            harness.get_by_label("one chip").rect().height()
        }

        let full = chip_height_with(true);
        let compact = chip_height_with(false);
        assert!(
            compact < full,
            "compact chip height ({compact}) must be shorter than the \
             two-line height ({full})"
        );
        assert_eq!(full, CHIP_HEIGHT_FULL);
        assert_eq!(compact, CHIP_HEIGHT_COMPACT);
        assert!(
            chrome_band_center_from_top(false) < chrome_band_center_from_top(true),
            "the chrome band's own vertical center must follow the shorter \
             compact chip row too"
        );
    }

    #[test]
    fn compact_chip_primary_line_is_vertically_centered() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 200.0))
            .build_ui(|ui| {
                show(
                    ui,
                    &[chip(1, "one")],
                    ChipId(1),
                    false,
                    true,
                    ChipLayout::SingleRowScroll,
                    false,
                    false,
                );
            });
        harness.run();

        let chip_rect = harness.get_by_label("one chip").rect();
        let label_rect = harness.get_by_label("one").rect();
        assert!(
            (label_rect.center().y - chip_rect.center().y).abs() <= 1.0,
            "compact title must be vertically centered, got label={label_rect:?}, \
             chip={chip_rect:?}"
        );
    }

    #[test]
    fn close_button_stays_vertically_centered_in_both_full_and_compact_chip_heights() {
        // Regression test: the close ("X") button used to be positioned
        // with a fixed inset from the chip's *top* edge, so it stayed
        // pinned near the top of the taller two-line chip height even
        // after the chip shrank to `CHIP_HEIGHT_COMPACT` in single-line
        // mode - visibly off-center rather than tracking the shorter box.
        fn close_and_chip_rects(show_session_details: bool) -> (egui::Rect, egui::Rect) {
            let mut harness = Harness::builder()
                .with_size(egui::vec2(900.0, 200.0))
                .build_ui(|ui| {
                    show(
                        ui,
                        &[chip(1, "one")],
                        ChipId(1),
                        false,
                        true,
                        ChipLayout::Wrap,
                        show_session_details,
                        false,
                    );
                });
            harness.run();
            (
                harness.get_by_label("Close").rect(),
                harness.get_by_label("one chip").rect(),
            )
        }

        for show_session_details in [true, false] {
            let (close_rect, chip_rect) = close_and_chip_rects(show_session_details);
            let close_center = close_rect.center().y;
            let chip_center = chip_rect.center().y;
            assert!(
                (close_center - chip_center).abs() <= 1.0,
                "close button center ({close_center}) must track the chip's \
                 own vertical center ({chip_center}) when \
                 show_session_details={show_session_details}"
            );
        }
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
    fn dragging_from_the_title_label_reorders_instead_of_moving_the_window() {
        // Regression test: pressing on the chip's *title* text used to be
        // able to fall through the label's own `Sense::click()` widget to
        // the chrome row's native-window-drag region instead of the chip's
        // own reorder-drag, unlike starting the same drag from the
        // secondary line or the chip's padding, which always worked
        // (`dragging_one_chip_onto_another_emits_a_reorder_action`). The
        // title label now senses only hover, so `bg_response` uncontested
        // owns both click and drag over its pixels too.
        let mut harness = harness(ChromeHarnessState {
            chips: vec![chip(1, "one"), chip(2, "two"), chip(3, "three")],
            active: ChipId(1),
            layout: ChipLayout::Wrap,
            observed: Vec::new(),
        });
        harness.run();

        let from = harness.get_by_label("one").rect().center();
        let to_rect = harness.get_by_label("three chip").rect();
        let to = to_rect.left_center() + egui::vec2(3.0, 0.0);

        harness.drag_at(from);
        harness.run();
        let mut start_drag_sent = harness.output().viewport_output.values().any(|vp| {
            vp.commands
                .iter()
                .any(|cmd| matches!(cmd, egui::ViewportCommand::StartDrag))
        });
        let steps = 8;
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            harness.hover_at(from + (to - from) * t);
            harness.run();
            start_drag_sent |= harness.output().viewport_output.values().any(|vp| {
                vp.commands
                    .iter()
                    .any(|cmd| matches!(cmd, egui::ViewportCommand::StartDrag))
            });
        }
        harness.drop_at(to);
        harness.run();

        assert!(
            !start_drag_sent,
            "dragging from the title label should never move the native window"
        );
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
                quick_switch_number: None,
                pulse_new_output: false,
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

    #[test]
    fn quick_switch_overlay_shows_number_only_while_active_and_hides_the_status_dot() {
        // Feature request #69: while the overlay is active, an eligible
        // chip's quick-switch number takes the status dot's slot; while
        // inactive, the ordinary status dot (identified by its accessible
        // hover label) is present instead and no number is shown.
        fn eligible_chip() -> ChipViewModel {
            let mut chip = chip(1, "one");
            chip.quick_switch_number = Some(1);
            chip
        }

        let mut inactive_harness = Harness::builder()
            .with_size(egui::vec2(900.0, 200.0))
            .build_ui(|ui| {
                show(
                    ui,
                    &[eligible_chip()],
                    ChipId(1),
                    false,
                    true,
                    ChipLayout::SingleRowScroll,
                    false,
                    false,
                );
            });
        inactive_harness.run();
        assert!(inactive_harness.query_by_label("Quick switch: 1").is_none());
        inactive_harness.get_by_label("one");

        let mut active_harness = Harness::builder()
            .with_size(egui::vec2(900.0, 200.0))
            .build_ui(|ui| {
                show(
                    ui,
                    &[eligible_chip()],
                    ChipId(1),
                    false,
                    true,
                    ChipLayout::SingleRowScroll,
                    false,
                    true,
                );
            });
        active_harness.run();
        active_harness.get_by_label("Quick switch: 1");
    }

    #[test]
    fn quick_switch_overlay_reserves_a_number_slot_on_neutral_chips_with_no_status_dot() {
        // `Neutral` chips (Launcher/Settings/etc.) never paint a status dot
        // at all, so the overlay must still be able to show their number.
        let neutral_chip = ChipViewModel {
            id: ChipId(1),
            primary: "Launcher".to_owned(),
            secondary: None,
            status: ChipStatus::Neutral,
            closable: false,
            renamable: false,
            quick_switch_number: Some(1),
            pulse_new_output: false,
        };

        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 200.0))
            .build_ui(move |ui| {
                let chip = ChipViewModel {
                    id: neutral_chip.id,
                    primary: neutral_chip.primary.clone(),
                    secondary: neutral_chip.secondary.clone(),
                    status: neutral_chip.status,
                    closable: neutral_chip.closable,
                    renamable: neutral_chip.renamable,
                    quick_switch_number: neutral_chip.quick_switch_number,
                    pulse_new_output: neutral_chip.pulse_new_output,
                };
                show(
                    ui,
                    &[chip],
                    ChipId(1),
                    false,
                    true,
                    ChipLayout::SingleRowScroll,
                    false,
                    true,
                );
            });
        harness.run();
        harness.get_by_label("Quick switch: 1");
    }

    #[test]
    fn pulsing_status_dot_renders_without_altering_the_chip_label_or_accessible_hover_text() {
        // Feature request #68: the pulse is a pure animation cue layered on
        // the existing status dot, so a pulsing chip must still expose the
        // exact same primary label and hover/accessible status text as a
        // non-pulsing one - the flag must never change `ChipStatus` or its
        // accessible label.
        fn pulsing_chip() -> ChipViewModel {
            let mut chip = chip(1, "one");
            chip.pulse_new_output = true;
            chip
        }

        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 200.0))
            .build_ui(|ui| {
                show(
                    ui,
                    &[pulsing_chip()],
                    ChipId(1),
                    false,
                    true,
                    ChipLayout::SingleRowScroll,
                    false,
                    false,
                );
            });
        harness.run_steps(2);

        harness.get_by_label("one");
    }
}
