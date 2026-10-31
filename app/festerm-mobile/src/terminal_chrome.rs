//! Terminal screen chrome: header, status line, session chips, key supplement
//! row, extended-keys panel and reconnect banner (mock-ups 01-04 and 16).

use super::model::{ConnectionLine, ReconnectBanner, SurfaceState, SurfaceSummary};
use super::theme::{color, radius, space, type_scale};
use super::widgets::{
    button, chip, count_chip, labelled, paint_fit, plus_chip, segmented, status_dot, ButtonKind,
    Dot,
};
use eframe::egui::{
    self, Align2, CornerRadius, Margin, Rect, Response, Sense, Stroke, StrokeKind, Ui,
};
use festerm_core::{Key, Modifiers};
use festerm_ui_egui::icon::{self, Icon};

pub struct HeaderOutput {
    pub back: bool,
    pub files: bool,
    pub more: Response,
}

/// Back chevron, title/subtitle and files/more buttons.
pub fn header(ui: &mut Ui, title: &str, subtitle: &str, show_files: bool) -> HeaderOutput {
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
    let mut files = false;
    let mut text_right = rect.right() - 56.0;
    if show_files {
        let files_rect = Rect::from_center_size(
            egui::pos2(rect.right() - 68.0, rect.center().y),
            egui::vec2(44.0, 44.0),
        );
        let response = ui.interact(files_rect, ui.id().with("header-files"), Sense::CLICK);
        icon::paint(
            ui.painter(),
            Icon::FileTransfer,
            Rect::from_center_size(files_rect.center(), egui::vec2(22.0, 22.0)),
            color::TEXT,
        );
        files = labelled(response, "Files", true).clicked();
        text_right = rect.right() - 100.0;
    }
    let text_left = rect.left() + 48.0;
    let width = (text_right - text_left).max(40.0);
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
    HeaderOutput {
        back: labelled(back, "Back", true).clicked(),
        files,
        more: labelled(more, "More", true),
    }
}

pub fn status_line(ui: &mut Ui, line: &ConnectionLine) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), Sense::hover());
    let painter = ui.painter();
    painter.hline(rect.x_range(), rect.top(), Stroke::new(1.0, color::DIVIDER));
    let dot = match line.state {
        SurfaceState::Connected => Dot::Connected,
        SurfaceState::Connecting => Dot::Warning,
        SurfaceState::Disconnected => Dot::Idle,
    };
    status_dot(
        painter,
        egui::pos2(rect.left() + 17.0, rect.center().y),
        dot,
    );
    let label = paint_fit(
        painter,
        egui::pos2(rect.left() + 27.0, rect.center().y),
        Align2::LEFT_CENTER,
        &line.label,
        type_scale::font(type_scale::MICRO + 0.5),
        color::TEXT_MUTED,
        rect.width() * 0.6,
    );
    if let Some(context) = &line.context {
        paint_fit(
            painter,
            egui::pos2(rect.right() - 14.0, rect.center().y),
            Align2::RIGHT_CENTER,
            context,
            type_scale::font(type_scale::MICRO + 0.5),
            color::TEXT_FAINT,
            (rect.width() - label.width() - 50.0).max(30.0),
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChipAction {
    None,
    Select(u64),
    All,
    New,
}

/// Session chip strip: open surfaces, "All N" and "+".
pub fn chip_strip(ui: &mut Ui, surfaces: &[SurfaceSummary]) -> ChipAction {
    let mut action = ChipAction::None;
    let full_width = ui.available_width();
    egui::Frame::new()
        .fill(color::BAR)
        .inner_margin(Margin::symmetric(8, 8))
        .show(ui, |ui| {
            ui.set_min_width(full_width - 16.0);
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.horizontal(|ui| {
                let trailing = 58.0 + 36.0 + 40.0;
                let width = (ui.available_width() - trailing).max(60.0);
                egui::ScrollArea::horizontal()
                    .id_salt("session-chips")
                    .max_width(width)
                    .auto_shrink([true, true])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for surface in surfaces {
                                let response = chip(
                                    ui,
                                    Some(surface.kind.glyph()),
                                    &surface.title,
                                    surface.current,
                                );
                                if response.clicked() {
                                    action = ChipAction::Select(surface.id);
                                }
                            }
                        });
                    });
                if count_chip(ui, "All", surfaces.len()).clicked() {
                    action = ChipAction::All;
                }
                if plus_chip(ui, "New").clicked() {
                    action = ChipAction::New;
                }
            });
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
    Paste,
    TogglePanel,
    ToggleKeyboard,
}

pub struct KeyRowState {
    pub control: bool,
    pub alt: bool,
    pub panel_open: bool,
    pub keyboard_visible: bool,
    pub paste: bool,
}

pub struct KeyRowOutput {
    pub action: KeyRowAction,
    pub control_rect: Rect,
}

fn paint_grid_icon(painter: &egui::Painter, rect: Rect, tint: egui::Color32) {
    let cell = rect.width() * 0.42;
    for (dx, dy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
        let min = rect.min + egui::vec2(dx * (rect.width() - cell), dy * (rect.height() - cell));
        painter.rect_stroke(
            Rect::from_min_size(min, egui::vec2(cell, cell)),
            CornerRadius::same(2),
            Stroke::new(1.6, tint),
            StrokeKind::Inside,
        );
    }
}

fn key_button(
    ui: &mut Ui,
    width: f32,
    label: &str,
    active: bool,
    paint: impl FnOnce(&egui::Painter, Rect, egui::Color32),
) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, space::KEY_HEIGHT), Sense::CLICK);
    let painter = ui.painter();
    let corner = CornerRadius::same(radius::CONTROL);
    painter.rect_filled(
        rect,
        corner,
        if active {
            color::KEY_ACTIVE
        } else {
            color::KEY
        },
    );
    if active {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(1.0, color::ACCENT_TEXT),
            StrokeKind::Inside,
        );
    }
    paint(painter, rect, color::TEXT);
    labelled(response, label, true)
}

fn text_key(ui: &mut Ui, width: f32, label: &str, active: bool) -> Response {
    key_button(ui, width, label, active, |painter, rect, tint| {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            type_scale::font(type_scale::BODY - 1.0),
            tint,
        );
    })
}

/// Esc Tab Ctrl Alt, optional paste, extended-keys toggle, keyboard toggle.
pub fn key_row(ui: &mut Ui, state: &KeyRowState) -> KeyRowOutput {
    let mut action = KeyRowAction::None;
    let mut control_rect = Rect::NOTHING;
    egui::Frame::new()
        .fill(color::BAR)
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.horizontal(|ui| {
                let count = 6 + usize::from(state.paste);
                let gap = ui.spacing().item_spacing.x;
                let width =
                    ((ui.available_width() - gap * (count as f32 - 1.0)) / count as f32).max(32.0);
                if text_key(ui, width, "Esc", false).clicked() {
                    action = KeyRowAction::Escape;
                }
                if text_key(ui, width, "Tab", false).clicked() {
                    action = KeyRowAction::Tab;
                }
                let control = text_key(ui, width, "Ctrl", state.control);
                control_rect = control.rect;
                if control.clicked() {
                    action = KeyRowAction::ToggleControl;
                }
                if text_key(ui, width, "Alt", state.alt).clicked() {
                    action = KeyRowAction::ToggleAlt;
                }
                if state.paste
                    && key_button(ui, width, "Paste", false, |painter, rect, tint| {
                        icon::paint(
                            painter,
                            Icon::Paste,
                            Rect::from_center_size(rect.center(), egui::vec2(20.0, 20.0)),
                            tint,
                        );
                    })
                    .clicked()
                {
                    action = KeyRowAction::Paste;
                }
                if key_button(
                    ui,
                    width,
                    "Extended keys",
                    state.panel_open,
                    |painter, rect, tint| {
                        paint_grid_icon(
                            painter,
                            Rect::from_center_size(rect.center(), egui::vec2(18.0, 18.0)),
                            tint,
                        );
                    },
                )
                .clicked()
                {
                    action = KeyRowAction::TogglePanel;
                }
                if key_button(
                    ui,
                    width,
                    if state.keyboard_visible {
                        "Hide keyboard"
                    } else {
                        "Show keyboard"
                    },
                    false,
                    |painter, rect, tint| {
                        icon::paint(
                            painter,
                            Icon::KeyboardShortcuts,
                            Rect::from_center_size(rect.center(), egui::vec2(22.0, 22.0)),
                            tint,
                        );
                    },
                )
                .clicked()
                {
                    action = KeyRowAction::ToggleKeyboard;
                }
            });
        });
    KeyRowOutput {
        action,
        control_rect,
    }
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
enum Glyph {
    Text(&'static str),
    Arrow(Key),
}

#[derive(Clone, Copy)]
struct PanelKey {
    glyph: Glyph,
    label: &'static str,
    key: Key,
    modifiers: Modifiers,
}

const fn plain(label: &'static str, key: Key) -> PanelKey {
    PanelKey {
        glyph: Glyph::Text(label),
        label,
        key,
        modifiers: Modifiers::NONE,
    }
}

const fn arrow(label: &'static str, key: Key) -> PanelKey {
    PanelKey {
        glyph: Glyph::Arrow(key),
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
    plain("End", Key::End),
    plain("PgUp", Key::PageUp),
    plain("PgDn", Key::PageDown),
    arrow("Left", Key::ArrowLeft),
    arrow("Up", Key::ArrowUp),
    arrow("Down", Key::ArrowDown),
    arrow("Right", Key::ArrowRight),
    plain("Insert", Key::Insert),
    plain("Delete", Key::Delete),
    PanelKey {
        glyph: Glyph::Text("Shift+Tab"),
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

fn paint_arrow(painter: &egui::Painter, center: egui::Pos2, key: Key, tint: egui::Color32) {
    let direction = match key {
        Key::ArrowLeft => egui::vec2(-1.0, 0.0),
        Key::ArrowRight => egui::vec2(1.0, 0.0),
        Key::ArrowUp => egui::vec2(0.0, -1.0),
        _ => egui::vec2(0.0, 1.0),
    };
    let side = egui::vec2(-direction.y, direction.x);
    let stroke = Stroke::new(1.6, tint);
    let tail = center - direction * 7.0;
    let head = center + direction * 7.0;
    painter.line_segment([tail, head], stroke);
    painter.line_segment([head, head - direction * 5.0 + side * 4.5], stroke);
    painter.line_segment([head, head - direction * 5.0 - side * 4.5], stroke);
}

/// Extended terminal keys (mock-up 03). Ctrl/Alt latches stay with the caller.
pub fn keys_panel(ui: &mut Ui, panel: &mut KeysPanel) -> KeysPanelOutput {
    let mut output = KeysPanelOutput::default();
    egui::Frame::new()
        .fill(color::SURFACE)
        .stroke(Stroke::new(1.0, color::DIVIDER))
        .inner_margin(Margin {
            left: 12,
            right: 12,
            top: 8,
            bottom: 8,
        })
        .show(ui, |ui| {
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
            let keys: &[PanelKey] = match panel.tab {
                0 => &NAVIGATION,
                1 => &FUNCTION_KEYS,
                _ => &SYMBOLS,
            };
            let gap = ui.spacing().item_spacing.x;
            let width = (ui.available_width() - gap * 3.0) / 4.0;
            for row in keys.chunks(4) {
                ui.horizontal(|ui| {
                    for entry in row {
                        let response =
                            key_button(ui, width, entry.label, false, |painter, rect, tint| {
                                match entry.glyph {
                                    Glyph::Text(text) => {
                                        painter.text(
                                            rect.center(),
                                            Align2::CENTER_CENTER,
                                            text,
                                            type_scale::font(type_scale::BODY - 1.0),
                                            tint,
                                        );
                                    }
                                    Glyph::Arrow(key) => {
                                        paint_arrow(painter, rect.center(), key, tint)
                                    }
                                }
                            });
                        if response.clicked() {
                            output.key = Some((entry.key, entry.modifiers));
                        }
                    }
                });
            }
            ui.label(
                egui::RichText::new("Ctrl / Alt apply to the next key")
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
                        egui::RichText::new("Connection interrupted")
                            .size(type_scale::BODY)
                            .strong(),
                    );
                });
                ui.label(
                    egui::RichText::new(
                        "Your previous shell cannot be resumed.\nReconnecting starts a new shell.",
                    )
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
            chosen = button(ui, ButtonKind::Primary, "Reconnect · new shell", true).clicked();
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

/// Replaces the key row when the transport is gone but history stays usable.
pub fn read_only_note(ui: &mut Ui) {
    egui::Frame::new()
        .fill(color::BAR)
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
