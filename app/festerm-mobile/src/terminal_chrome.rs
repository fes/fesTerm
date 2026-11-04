//! Terminal screen chrome: header, status line, session chips, key supplement
//! row, extended-keys panel and reconnect banner (mock-ups 01-04 and 16).

use super::key_press::{paint_glyph, Glyph, KeyPress, Repeat};
use super::model::{ReconnectBanner, SurfaceKind, SurfaceState, SurfaceSummary};
use super::terminal_keyboard::KeyboardKind;
use super::theme::{color, radius, space, type_scale};
use super::widgets::{
    bottom_sheet, button, count_chip, labelled, long_press_or_click, paint_fit, plus_chip,
    segmented, session_chip, sheet_title, ButtonKind, Dot,
};
use eframe::egui::{self, Align2, CornerRadius, Margin, Rect, Response, Sense, Stroke, Ui};
use festerm_core::{Key, Modifiers};
use festerm_ui_egui::icon::{self, Icon};

pub struct HeaderOutput {
    pub back: bool,
    pub more: Response,
    pub chip: ChipAction,
}

/// What sits between the back chevron and the overflow button.
pub enum HeaderCenter<'a> {
    /// Workspace heading (files, markdown).
    Title { title: &'a str, subtitle: &'a str },
    /// Terminal: scrolling session chips, "All N" and "+".
    Chips(&'a [SurfaceSummary]),
}

/// Back chevron, title or session chips, and the overflow button.
pub fn header(ui: &mut Ui, center: HeaderCenter<'_>) -> HeaderOutput {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 56.0), Sense::hover());
    let back = ui.interact(
        Rect::from_min_size(rect.min, egui::vec2(44.0, 56.0)),
        ui.id().with("header-back"),
        Sense::CLICK,
    );
    icon::paint(
        ui.painter(),
        Icon::Back,
        Rect::from_center_size(
            egui::pos2(rect.left() + 20.0, rect.center().y),
            egui::vec2(22.0, 22.0),
        ),
        color::ACCENT_TEXT,
    );
    let more_rect = Rect::from_center_size(
        egui::pos2(rect.right() - 24.0, rect.center().y),
        egui::vec2(44.0, 44.0),
    );
    let more = ui.interact(more_rect, ui.id().with("header-more"), Sense::CLICK);
    icon::paint(
        ui.painter(),
        Icon::Overflow,
        Rect::from_center_size(more_rect.center(), egui::vec2(22.0, 22.0)),
        color::TEXT,
    );
    let middle = Rect::from_x_y_ranges(
        (rect.left() + 44.0)..=(more_rect.left() - 2.0),
        rect.y_range(),
    );
    let mut chip = ChipAction::None;
    match center {
        HeaderCenter::Title { title, subtitle } => {
            let text_left = middle.left() + 4.0;
            let width = (middle.right() - text_left).max(40.0);
            paint_fit(
                ui.painter(),
                egui::pos2(text_left, rect.center().y - 9.0),
                Align2::LEFT_CENTER,
                title,
                type_scale::font(type_scale::NAV_TITLE),
                color::TEXT,
                width,
            );
            paint_fit(
                ui.painter(),
                egui::pos2(text_left, rect.center().y + 12.0),
                Align2::LEFT_CENTER,
                subtitle,
                type_scale::font(type_scale::CAPTION),
                color::TEXT_MUTED,
                width,
            );
        }
        HeaderCenter::Chips(surfaces) => {
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(middle)
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
                |ui| chip = chip_strip(ui, surfaces),
            );
        }
    }
    ui.painter().hline(
        rect.x_range(),
        rect.bottom(),
        Stroke::new(1.0, color::DIVIDER),
    );
    HeaderOutput {
        back: labelled(back, "Back", true).clicked(),
        more: labelled(more, "More", true),
        chip,
    }
}

/// Content width of the chip strip and key row on wide screens; the bars still
/// span the window, but controls stop growing into thumb-unfriendly slabs.
const CHROME_MAX_WIDTH: f32 = 720.0;
const KEYS_PANEL_MAX_WIDTH: f32 = 640.0;

/// Frame margin that centers content no wider than `max` inside `available`.
fn capped_margin(available: f32, max: f32, horizontal: i8, vertical: i8) -> Margin {
    let extra = ((available - max) / 2.0 - f32::from(horizontal)).max(0.0);
    let side = (f32::from(horizontal) + extra).min(120.0) as i8;
    Margin {
        left: side,
        right: side,
        top: vertical,
        bottom: vertical,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChipAction {
    None,
    Select(u64),
    /// Long-pressed: ask before closing this session.
    Close(u64),
    All,
    New,
}

fn chip_dot(surface: &SurfaceSummary) -> Dot {
    match (surface.kind, surface.state) {
        (SurfaceKind::Local, _) => Dot::Idle,
        (_, SurfaceState::Connected) => Dot::Connected,
        (_, SurfaceState::Connecting) => Dot::Warning,
        (_, SurfaceState::Disconnected) => Dot::Danger,
    }
}

/// Header chip strip, laid out right to left: "+" and "All N" stay pinned while
/// the open sessions scroll sideways in the remaining width.
fn chip_strip(ui: &mut Ui, surfaces: &[SurfaceSummary]) -> ChipAction {
    let mut action = ChipAction::None;
    ui.spacing_mut().item_spacing.x = 6.0;
    if plus_chip(ui, "New").clicked() {
        action = ChipAction::New;
    }
    if count_chip(ui, "All", surfaces.len()).clicked() {
        action = ChipAction::All;
    }
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
        let current_key = ui.id().with("current-chip");
        let shown = ui
            .data(|data| data.get_temp::<Option<u64>>(current_key))
            .flatten();
        let current = surfaces
            .iter()
            .find(|surface| surface.current)
            .map(|s| s.id);
        egui::ScrollArea::horizontal()
            .id_salt("session-chips")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for surface in surfaces {
                        let response = session_chip(
                            ui,
                            &surface.title,
                            chip_dot(surface),
                            surface.unread,
                            surface.current,
                        );
                        if surface.current && shown != current {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                        match long_press_or_click(ui, &response) {
                            (true, _) => action = ChipAction::Close(surface.id),
                            (_, true) => action = ChipAction::Select(surface.id),
                            _ => {}
                        }
                    }
                });
            });
        ui.data_mut(|data| data.insert_temp(current_key, current));
    });
    action
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyRowAction {
    None,
    Escape,
    Tab,
    ToggleControl,
    ToggleAlt,
    ToggleShift,
    Paste,
    Dictate,
    TogglePanel,
    ToggleKeyboard,
    SwitchKeyboard,
    Key(Key),
    SelectKeyboard(KeyboardKind),
}

pub struct KeyRowState {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub panel_open: bool,
    pub keyboard_visible: bool,
    pub paste: bool,
}

pub struct KeyRowOutput {
    pub action: KeyRowAction,
    pub control_rect: Rect,
    pub click: bool,
}

impl Default for KeyRowOutput {
    fn default() -> Self {
        Self {
            action: KeyRowAction::None,
            control_rect: Rect::NOTHING,
            click: false,
        }
    }
}

fn key_button(
    ui: &mut Ui,
    width: f32,
    height: f32,
    label: &str,
    active: bool,
    paint: impl FnOnce(&egui::Painter, Rect, egui::Color32),
) -> Response {
    key_button_filled(
        ui,
        width,
        height,
        label,
        active,
        color::TERMINAL_KEY_MUTED,
        paint,
    )
}

/// Keys match the app-owned keyboards: utility keys use the muted fill,
/// character-like keys the raised one, and a latched key the selected blue.
fn key_button_filled(
    ui: &mut Ui,
    width: f32,
    height: f32,
    label: &str,
    active: bool,
    fill: egui::Color32,
    paint: impl FnOnce(&egui::Painter, Rect, egui::Color32),
) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), Sense::CLICK);
    let painter = ui.painter();
    let (fill, ink) = if active {
        (
            color::TERMINAL_KEY_SELECTED,
            color::TERMINAL_KEY_SELECTED_INK,
        )
    } else {
        (fill, color::TEXT)
    };
    painter.rect_filled(rect, CornerRadius::same(radius::KEY), fill);
    paint(painter, rect, ink);
    labelled(response, label, true)
}

fn glyph_key(
    ui: &mut Ui,
    width: f32,
    height: f32,
    label: &str,
    glyph: &Glyph,
    active: bool,
) -> Response {
    key_button(ui, width, height, label, active, |painter, rect, tint| {
        paint_glyph(painter, glyph, rect.center(), 1.0, tint);
    })
}

fn text_key(ui: &mut Ui, width: f32, label: &str, active: bool) -> Response {
    key_button(
        ui,
        width,
        space::KEY_HEIGHT,
        label,
        active,
        |painter, rect, tint| {
            paint_fit(
                painter,
                rect.center(),
                Align2::CENTER_CENTER,
                label,
                type_scale::font(type_scale::BODY - 1.0),
                tint,
                rect.width() - 4.0,
            );
        },
    )
}

/// Esc Tab Ctrl Alt Shift, optional paste, extended keys, keyboard switch and
/// keyboard toggle.
pub fn key_row(ui: &mut Ui, state: &KeyRowState, keys: &mut KeyPress) -> KeyRowOutput {
    let mut action = KeyRowAction::None;
    let mut control_rect = Rect::NOTHING;
    let margin = capped_margin(ui.available_width(), CHROME_MAX_WIDTH, 8, 6);
    egui::Frame::new()
        .fill(color::TERMINAL_CHROME)
        .inner_margin(margin)
        .show(ui, |ui| {
            keys.latch(state.control || state.alt || state.shift);
            keys.begin(ui, false);
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.horizontal(|ui| {
                let count = 8 + usize::from(state.paste);
                let gap = ui.spacing().item_spacing.x;
                let width =
                    ((ui.available_width() - gap * (count as f32 - 1.0)) / count as f32).max(24.0);
                for (label, key) in [("Esc", Key::Escape), ("Tab", Key::Tab)] {
                    let response = text_key(ui, width, label, false);
                    if keys
                        .key(ui, &response, Glyph::text(label), Repeat::Auto)
                        .is_some()
                    {
                        action = KeyRowAction::Key(key);
                    }
                }
                for (label, toggle, active) in [
                    ("Ctrl", KeyRowAction::ToggleControl, state.control),
                    ("Alt", KeyRowAction::ToggleAlt, state.alt),
                    ("Shift", KeyRowAction::ToggleShift, state.shift),
                ] {
                    let response = text_key(ui, width, label, active);
                    if label == "Ctrl" {
                        control_rect = response.rect;
                    }
                    if keys.toggle(ui, &response, Glyph::text(label)) {
                        action = toggle;
                    }
                }
                let height = space::KEY_HEIGHT;
                let mut command = |ui: &mut Ui, label, glyph: Glyph, active, chosen| {
                    let response = glyph_key(ui, width, height, label, &glyph, active);
                    if keys.toggle(ui, &response, glyph) {
                        action = chosen;
                    }
                };
                if state.paste {
                    command(
                        ui,
                        "Paste",
                        Glyph::Icon(Icon::Paste),
                        false,
                        KeyRowAction::Paste,
                    );
                }
                command(
                    ui,
                    "Extended keys",
                    Glyph::Grid,
                    state.panel_open,
                    KeyRowAction::TogglePanel,
                );
                command(
                    ui,
                    "Switch keyboard",
                    Glyph::Icon(Icon::RemoteGlobe),
                    false,
                    KeyRowAction::SwitchKeyboard,
                );
                command(
                    ui,
                    if state.keyboard_visible {
                        "Hide keyboard"
                    } else {
                        "Show keyboard"
                    },
                    Glyph::Icon(Icon::KeyboardShortcuts),
                    false,
                    KeyRowAction::ToggleKeyboard,
                );
            });
        });
    KeyRowOutput {
        action,
        control_rect,
        click: false,
    }
}

/// Keyboard choices shown inline in the More menu: a nested submenu opens on
/// hover, which touch screens never produce reliably.
pub(super) fn keyboard_menu(ui: &mut Ui, kind: KeyboardKind) -> KeyRowAction {
    let mut action = KeyRowAction::None;
    ui.separator();
    ui.label(
        egui::RichText::new("Keyboard")
            .size(type_scale::CAPTION)
            .color(color::TEXT_MUTED),
    );
    for option in KeyboardKind::ALL {
        let response = ui.selectable_label(kind == option, option.label());
        if labelled(response, option.label(), true).clicked() {
            action = KeyRowAction::SelectKeyboard(option);
            ui.close();
        }
    }
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "The globe key switches keyboards. For voice, use the Built-in keyboard's microphone.",
            )
            .size(type_scale::MICRO)
            .color(color::TEXT_FAINT),
        )
        .wrap(),
    );
    ui.separator();
    action
}

pub(super) fn keyboard_visibility_button(
    ui: &mut Ui,
    width: f32,
    height: f32,
    visible: bool,
) -> Response {
    key_button(
        ui,
        width,
        height,
        if visible {
            "Hide keyboard"
        } else {
            "Show keyboard"
        },
        false,
        |painter, rect, tint| {
            icon::paint(
                painter,
                Icon::KeyboardShortcuts,
                Rect::from_center_size(rect.center(), egui::vec2(20.0, 20.0)),
                tint,
            );
        },
    )
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KeysPanel {
    pub open: bool,
    pub tab: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KeysPanelOutput {
    pub key: Option<(Key, Modifiers)>,
    pub close: bool,
}

#[derive(Clone, Copy)]
struct PanelKey {
    arrow: bool,
    label: &'static str,
    key: Key,
    modifiers: Modifiers,
}

impl PanelKey {
    fn glyph(&self) -> Glyph {
        if self.arrow {
            Glyph::Arrow(self.key)
        } else {
            Glyph::text(self.label)
        }
    }

    /// Insert toggles a mode, so it acts once; everything else, including
    /// Return, repeats like a hardware key.
    fn repeat(&self) -> Repeat {
        if self.key == Key::Insert {
            Repeat::Release
        } else {
            Repeat::Auto
        }
    }
}

const fn plain(label: &'static str, key: Key) -> PanelKey {
    PanelKey {
        arrow: false,
        label,
        key,
        modifiers: Modifiers::NONE,
    }
}

const fn arrow(label: &'static str, key: Key) -> PanelKey {
    PanelKey {
        arrow: true,
        label,
        key,
        modifiers: Modifiers::NONE,
    }
}

const fn symbol(label: &'static str, character: char) -> PanelKey {
    plain(label, Key::Character(character))
}

const NAVIGATION: [PanelKey; 12] = [
    plain("Home", Key::Home),
    arrow("Up", Key::ArrowUp),
    plain("End", Key::End),
    plain("PgUp", Key::PageUp),
    arrow("Left", Key::ArrowLeft),
    arrow("Down", Key::ArrowDown),
    arrow("Right", Key::ArrowRight),
    plain("PgDn", Key::PageDown),
    plain("Insert", Key::Insert),
    plain("Delete", Key::Delete),
    PanelKey {
        arrow: false,
        label: "Shift+Tab",
        key: Key::Tab,
        modifiers: Modifiers::SHIFT,
    },
    plain("Return", Key::Enter),
];

const FUNCTION_KEYS: [PanelKey; 12] = [
    plain("F1", Key::Function(1)),
    plain("F2", Key::Function(2)),
    plain("F3", Key::Function(3)),
    plain("F4", Key::Function(4)),
    plain("F5", Key::Function(5)),
    plain("F6", Key::Function(6)),
    plain("F7", Key::Function(7)),
    plain("F8", Key::Function(8)),
    plain("F9", Key::Function(9)),
    plain("F10", Key::Function(10)),
    plain("F11", Key::Function(11)),
    plain("F12", Key::Function(12)),
];

const SYMBOLS: [PanelKey; 12] = [
    symbol("~", '~'),
    symbol("|", '|'),
    symbol("\\", '\\'),
    symbol("`", '`'),
    symbol("{", '{'),
    symbol("}", '}'),
    symbol("[", '['),
    symbol("]", ']'),
    symbol("<", '<'),
    symbol(">", '>'),
    symbol("-", '-'),
    symbol("_", '_'),
];

/// Extended terminal keys (mock-up 03). Modifier latches stay with the caller.
pub fn keys_panel(ui: &mut Ui, panel: &mut KeysPanel, keys: &mut KeyPress) -> KeysPanelOutput {
    let mut output = KeysPanelOutput::default();
    egui::Frame::new()
        .fill(color::TERMINAL_CHROME)
        .stroke(Stroke::new(1.0, color::TERMINAL_BORDER))
        .inner_margin(capped_margin(
            ui.available_width(),
            KEYS_PANEL_MAX_WIDTH,
            12,
            8,
        ))
        .show(ui, |ui| {
            keys.begin(ui, false);
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            let (title, _) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), Sense::hover());
            paint_fit(
                ui.painter(),
                egui::pos2(title.left() + 2.0, title.center().y),
                Align2::LEFT_CENTER,
                "Terminal keys",
                type_scale::font(type_scale::CAPTION + 0.5),
                color::TEXT,
                title.width() * 0.6,
            );
            let close = ui.interact(
                Rect::from_center_size(
                    egui::pos2(title.right() - 30.0, title.center().y),
                    egui::vec2(72.0, 36.0),
                ),
                ui.id().with("keys-panel-close"),
                Sense::CLICK,
            );
            paint_fit(
                ui.painter(),
                egui::pos2(title.right() - 24.0, title.center().y),
                Align2::RIGHT_CENTER,
                "ABC",
                type_scale::font(type_scale::CAPTION),
                color::ACCENT_TEXT,
                40.0,
            );
            icon::paint(
                ui.painter(),
                Icon::KeyboardShortcuts,
                Rect::from_center_size(
                    egui::pos2(title.right() - 10.0, title.center().y),
                    egui::vec2(16.0, 16.0),
                ),
                color::ACCENT_TEXT,
            );
            output.close = labelled(close, "Show keyboard", true).clicked();
            segmented(
                ui,
                &[(None, "Navigation"), (None, "F-keys"), (None, "Symbols")],
                &mut panel.tab,
                false,
            );
            let entries: &[PanelKey] = match panel.tab {
                0 => &NAVIGATION,
                1 => &FUNCTION_KEYS,
                _ => &SYMBOLS,
            };
            let gap = ui.spacing().item_spacing.x;
            let width = (ui.available_width() - gap * 3.0) / 4.0;
            for row in entries.chunks(4) {
                ui.horizontal(|ui| {
                    for entry in row {
                        let glyph = entry.glyph();
                        let response = key_button_filled(
                            ui,
                            width,
                            space::KEY_HEIGHT,
                            entry.label,
                            false,
                            color::TERMINAL_KEY,
                            |painter, rect, tint| {
                                paint_glyph(painter, &glyph, rect.center(), 1.0, tint);
                            },
                        );
                        if keys.key(ui, &response, glyph, entry.repeat()).is_some() {
                            output.key = Some((entry.key, entry.modifiers));
                        }
                    }
                });
            }
            ui.label(
                egui::RichText::new("Ctrl / Alt / Shift apply to the next key")
                    .size(type_scale::MICRO)
                    .color(color::TEXT_FAINT),
            );
        });
    output
}

/// Reconnect banner (mock-up 16). Returns true when the action is chosen.
pub fn reconnect_banner(ui: &mut Ui, banner: &ReconnectBanner, compact: bool) -> bool {
    let mut chosen = false;
    egui::Frame::new()
        .fill(color::SURFACE)
        .stroke(Stroke::new(1.0, color::BORDER))
        .corner_radius(CornerRadius::same(radius::CARD))
        .inner_margin(Margin::same(if compact { 8 } else { 14 }))
        .outer_margin(Margin::symmetric(12, 6))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            if !compact {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), Sense::hover());
                    icon::paint(ui.painter(), Icon::Disconnect, rect, color::TEXT_MUTED);
                    ui.label(
                        egui::RichText::new(
                            banner
                                .custom
                                .as_ref()
                                .map_or("Connection interrupted", |text| text.title.as_str()),
                        )
                            .size(type_scale::BODY)
                            .strong(),
                    );
                });
                let explanation = match (&banner.custom, &banner.reattach) {
                    (Some(text), _) => text.body.clone(),
                    (None, Some(session)) => format!(
                        "The connection dropped; your {session} session keeps running.\nReconnecting reattaches it."
                    ),
                    (None, None) => {
                        "Your previous shell cannot be resumed.\nReconnecting starts a new shell."
                            .to_owned()
                    }
                };
                ui.label(
                    egui::RichText::new(explanation)
                        .size(type_scale::CAPTION + 0.5)
                        .color(color::TEXT_MUTED),
                );
            }
            if let Some(detail) = &banner.detail {
                ui.label(
                    egui::RichText::new(detail)
                        .size(type_scale::CAPTION)
                        .color(color::WARNING),
                );
            }
            let label = if let Some(text) = &banner.custom {
                text.action
            } else if banner.reattach.is_some() {
                "Reconnect · reattach"
            } else {
                "Reconnect · new shell"
            };
            chosen = button(ui, ButtonKind::Primary, label, true).clicked();
            if !compact {
                ui.label(
                    egui::RichText::new(&banner.status)
                        .size(type_scale::MICRO)
                        .color(color::TEXT_FAINT),
                );
            }
        });
    chosen
}

/// Confirmation raised by long-pressing a session chip. Returns `Some(true)`
/// to close, `Some(false)` when cancelled or dismissed.
pub fn close_session_sheet(ctx: &egui::Context, bounds: Rect, title: &str) -> Option<bool> {
    let mut choice = None;
    let outcome = bottom_sheet(ctx, "close-session", bounds, |ui| {
        if sheet_title(ui, &format!("Close {title}?"), None) {
            choice = Some(false);
        }
        ui.label(
            egui::RichText::new("This closes the tab and ends its connection.")
                .size(type_scale::BODY)
                .color(color::TEXT_MUTED),
        );
        ui.add_space(space::SM);
        if button(ui, ButtonKind::Destructive, "Close session", true).clicked() {
            choice = Some(true);
        }
        if button(ui, ButtonKind::Secondary, "Cancel", true).clicked() {
            choice = Some(false);
        }
    });
    if outcome.dismissed && choice.is_none() {
        choice = Some(false);
    }
    choice
}

/// Replaces the key row when the transport is gone but history stays usable.
pub fn read_only_note(ui: &mut Ui) {
    egui::Frame::new()
        .fill(color::TERMINAL_CHROME)
        .inner_margin(Margin::symmetric(8, 14))
        .show(ui, |ui| {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), Sense::hover());
            let text = paint_fit(
                ui.painter(),
                rect.center() + egui::vec2(10.0, 0.0),
                Align2::CENTER_CENTER,
                "Scroll and copy remain available",
                type_scale::font(type_scale::CAPTION),
                color::TEXT_MUTED,
                rect.width() - 30.0,
            );
            ui.painter().circle_stroke(
                egui::pos2(text.left() - 12.0, rect.center().y),
                6.0,
                Stroke::new(1.2, color::TEXT_MUTED),
            );
            ui.painter().text(
                egui::pos2(text.left() - 12.0, rect.center().y),
                Align2::CENTER_CENTER,
                "i",
                type_scale::font(9.0),
                color::TEXT_MUTED,
            );
        });
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    #[test]
    fn terminal_keys_follow_the_requested_navigation_rows() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                |ui, state: &mut (KeysPanel, KeysPanelOutput)| {
                    let output = keys_panel(ui, &mut state.0, &mut KeyPress::default());
                    if output.key.is_some() {
                        state.1 = output;
                    }
                },
                (KeysPanel { open: true, tab: 0 }, KeysPanelOutput::default()),
            );
        harness.run();
        let rows = [
            ["Home", "Up", "End", "PgUp"],
            ["Left", "Down", "Right", "PgDn"],
            ["Insert", "Delete", "Shift+Tab", "Return"],
        ];
        let mut previous_row = None;
        for row in rows {
            let first = harness.get_by_label(row[0]).rect();
            if let Some(previous) = previous_row {
                assert!(first.top() > previous);
            }
            previous_row = Some(first.bottom());
            let mut previous_right = None;
            for label in row {
                let rect = harness.get_by_label(label).rect();
                assert_eq!(rect.top(), first.top());
                if let Some(right) = previous_right {
                    assert!(rect.left() > right);
                }
                previous_right = Some(rect.right());
            }
        }
        harness.get_by_label("Up").click();
        harness.run();
        assert_eq!(harness.state().1.key, Some((Key::ArrowUp, Modifiers::NONE)));
    }

    #[test]
    fn default_extra_keys_include_shift_without_overflowing_phone_widths() {
        for width in [320.0, 390.0, 844.0] {
            let mut harness = Harness::builder()
                .with_size(egui::vec2(width, 300.0))
                .build_ui_state(
                    |ui, action: &mut KeyRowAction| {
                        let output = key_row(
                            ui,
                            &KeyRowState {
                                control: false,
                                alt: false,
                                shift: false,
                                panel_open: false,
                                keyboard_visible: true,
                                paste: true,
                            },
                            &mut KeyPress::default(),
                        );
                        if output.action != KeyRowAction::None {
                            *action = output.action;
                        }
                    },
                    KeyRowAction::None,
                );
            harness.run();
            let alt = harness.get_by_label("Alt").rect();
            let shift = harness.get_by_label("Shift").rect();
            let paste = harness.get_by_label("Paste").rect();
            assert!(alt.right() < shift.left() && shift.right() < paste.left());
            assert!(harness.get_by_label("Hide keyboard").rect().right() <= width);
            harness.get_by_label("Shift").click();
            harness.run();
            assert_eq!(*harness.state(), KeyRowAction::ToggleShift);
        }
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
    fn accessory_and_extended_keys_preview_and_repeat_like_hardware_keys() {
        use crate::key_press::{REPEAT_DELAY, REPEAT_INTERVAL};
        type State = (KeysPanel, KeyPress, Vec<KeyRowAction>, bool);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                |ui, (panel, keys, actions, control): &mut State| {
                    if let Some((key, _)) = keys_panel(ui, panel, keys).key {
                        actions.push(KeyRowAction::Key(key));
                    }
                    let state = KeyRowState {
                        control: *control,
                        alt: false,
                        shift: false,
                        panel_open: true,
                        keyboard_visible: true,
                        paste: true,
                    };
                    let output = key_row(ui, &state, keys);
                    if output.action != KeyRowAction::None {
                        actions.push(output.action);
                    }
                },
                (
                    KeysPanel { open: true, tab: 1 },
                    KeyPress::default(),
                    Vec::new(),
                    false,
                ),
            );
        harness.run();
        for (label, glyph) in [
            ("Shift", Glyph::text("Shift")),
            ("Switch keyboard", Glyph::Icon(Icon::RemoteGlobe)),
            ("F5", Glyph::text("F5")),
        ] {
            let center = harness.get_by_label(label).rect().center();
            harness.event(egui::Event::PointerMoved(center));
            harness.event(pointer(true, center));
            harness.step();
            assert_eq!(harness.state().1.preview_glyph(), Some(&glyph), "{label}");
            harness.event(pointer(false, center));
            harness.run();
        }
        assert_eq!(
            harness.state().2,
            [
                KeyRowAction::ToggleShift,
                KeyRowAction::SwitchKeyboard,
                KeyRowAction::Key(Key::Function(5)),
            ]
        );
        harness.state_mut().2.clear();
        let tab = harness.get_by_label("Tab").rect().center();
        let now = harness.ctx.input(|input| input.time);
        harness.event(egui::Event::PointerMoved(tab));
        harness.input_mut().time = Some(now + 0.01);
        harness.event(pointer(true, tab));
        harness.step();
        harness.input_mut().time = Some(now + 0.01 + REPEAT_DELAY + REPEAT_INTERVAL);
        harness.step();
        harness.event(pointer(false, tab));
        harness.step();
        assert_eq!(harness.state().2, [KeyRowAction::Key(Key::Tab); 2]);
        harness.state_mut().2.clear();
        harness.run();
        harness.state_mut().3 = true;
        let now = harness.ctx.input(|input| input.time);
        harness.input_mut().time = Some(now + 0.01);
        harness.event(pointer(true, tab));
        harness.step();
        harness.input_mut().time = Some(now + 2.0);
        harness.step();
        harness.event(pointer(false, tab));
        harness.step();
        assert_eq!(
            harness.state().2,
            [KeyRowAction::Key(Key::Tab)],
            "a press under a one-shot modifier never repeats without it"
        );
        for key in NAVIGATION {
            let expected = if key.key == Key::Insert {
                Repeat::Release
            } else {
                Repeat::Auto
            };
            assert_eq!(key.repeat(), expected, "{}", key.label);
        }
    }
}
