//! Mobile design tokens derived from the owner-approved iOS mock-ups.
//!
//! Tokens are scoped to this host. Desktop visuals and terminal protocol
//! colors stay in the shared crates. Mock-ups communicate aesthetics, so the
//! values are sampled palette and proportion, not a pixel contract.

use eframe::egui::{
    self, epaint::Shadow, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle,
};

pub mod color {
    use super::Color32;

    pub const BG: Color32 = Color32::from_rgb(15, 20, 28);
    pub const TERMINAL_BG: Color32 = Color32::from_rgb(14, 18, 26);
    pub const SURFACE: Color32 = Color32::from_rgb(27, 37, 47);
    pub const SURFACE_RAISED: Color32 = Color32::from_rgb(38, 51, 63);
    pub const FIELD: Color32 = Color32::from_rgb(19, 29, 39);
    pub const SHEET: Color32 = Color32::from_rgb(27, 39, 51);
    pub const BAR: Color32 = Color32::from_rgb(21, 29, 38);
    pub const BORDER: Color32 = Color32::from_rgb(38, 58, 77);
    pub const DIVIDER: Color32 = Color32::from_rgb(34, 47, 60);
    pub const SELECTED: Color32 = Color32::from_rgb(38, 60, 80);
    pub const ICON_TILE: Color32 = Color32::from_rgb(32, 46, 60);
    pub const KEY: Color32 = Color32::from_rgb(39, 51, 63);
    pub const KEY_ACTIVE: Color32 = Color32::from_rgb(35, 74, 108);
    pub const PRIMARY: Color32 = Color32::from_rgb(28, 126, 245);
    pub const PRIMARY_PRESSED: Color32 = Color32::from_rgb(22, 100, 205);
    pub const ON_PRIMARY: Color32 = Color32::WHITE;
    pub const ACCENT_TEXT: Color32 = Color32::from_rgb(100, 167, 248);
    pub const TEXT: Color32 = Color32::from_rgb(230, 237, 243);
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(140, 156, 172);
    pub const TEXT_FAINT: Color32 = Color32::from_rgb(98, 114, 130);
    pub const SUCCESS: Color32 = Color32::from_rgb(62, 207, 142);
    pub const WARNING: Color32 = Color32::from_rgb(233, 183, 128);
    pub const WARNING_FILL: Color32 = Color32::from_rgb(52, 42, 35);
    pub const DANGER: Color32 = Color32::from_rgb(240, 98, 98);
    pub const SCRIM: Color32 = Color32::from_black_alpha(150);
}

/// Corner radii in points.
pub mod radius {
    pub const CONTROL: u8 = 10;
    pub const FIELD: u8 = 12;
    pub const BUTTON: u8 = 12;
    pub const CARD: u8 = 14;
    pub const CHIP: u8 = 12;
    pub const SHEET: u8 = 22;
}

/// Spacing scale in points.
pub mod space {
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 24.0;
    /// Minimum touch target.
    pub const TOUCH: f32 = 44.0;
    pub const BUTTON_HEIGHT: f32 = 48.0;
    pub const FIELD_HEIGHT: f32 = 48.0;
    pub const TAB_BAR_HEIGHT: f32 = 56.0;
    pub const CHIP_HEIGHT: f32 = 40.0;
    pub const KEY_HEIGHT: f32 = 40.0;
}

/// Type scale in points.
pub mod type_scale {
    use super::{FontFamily, FontId};

    pub const LARGE_TITLE: f32 = 26.0;
    pub const TITLE: f32 = 20.0;
    pub const NAV_TITLE: f32 = 17.0;
    pub const BODY: f32 = 15.0;
    pub const CAPTION: f32 = 12.5;
    pub const MICRO: f32 = 11.0;
    pub const TERMINAL: f32 = 14.0;

    pub fn font(size: f32) -> FontId {
        FontId::new(size, FontFamily::Proportional)
    }

    pub fn mono(size: f32) -> FontId {
        FontId::new(size, FontFamily::Monospace)
    }
}

pub fn card_radius() -> CornerRadius {
    CornerRadius::same(radius::CARD)
}

pub fn sheet_shadow() -> Shadow {
    Shadow {
        offset: [0, -6],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(140),
    }
}

pub fn card_margin() -> Margin {
    Margin::same(space::MD as i8)
}

/// Applies the mobile tokens to `style`, preserving the capture harness's
/// nonblinking cursor preference.
pub fn apply(style: &egui::Style) -> egui::Style {
    let mut style = style.clone();
    let mut visuals = egui::Visuals::dark();
    let border = Stroke::new(1.0, color::BORDER);
    visuals.override_text_color = Some(color::TEXT);
    visuals.panel_fill = color::BG;
    visuals.window_fill = color::SHEET;
    visuals.extreme_bg_color = color::FIELD;
    visuals.faint_bg_color = color::SURFACE;
    visuals.window_stroke = border;
    visuals.window_corner_radius = CornerRadius::same(radius::CARD);
    visuals.menu_corner_radius = CornerRadius::same(radius::CONTROL);
    visuals.popup_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    visuals.widgets.noninteractive.bg_stroke = border;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, color::TEXT_MUTED);
    for widget in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(radius::CONTROL);
        widget.bg_stroke = border;
        widget.fg_stroke = Stroke::new(1.0, color::TEXT);
    }
    visuals.widgets.inactive.bg_fill = color::KEY;
    visuals.widgets.inactive.weak_bg_fill = color::KEY;
    visuals.widgets.hovered.bg_fill = color::SURFACE_RAISED;
    visuals.widgets.hovered.weak_bg_fill = color::SURFACE_RAISED;
    visuals.widgets.active.bg_fill = color::KEY_ACTIVE;
    visuals.widgets.active.weak_bg_fill = color::KEY_ACTIVE;
    visuals.widgets.open.bg_fill = color::KEY_ACTIVE;
    visuals.widgets.open.weak_bg_fill = color::KEY_ACTIVE;
    visuals.selection.bg_fill = color::KEY_ACTIVE;
    visuals.selection.stroke = Stroke::new(1.0, color::TEXT);
    visuals.hyperlink_color = color::ACCENT_TEXT;
    visuals.text_cursor.blink = style.visuals.text_cursor.blink;
    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(space::SM, space::SM);
    style.spacing.button_padding = egui::vec2(space::MD, 8.0);
    style.spacing.interact_size.y = space::TOUCH;
    style.text_styles = [
        (TextStyle::Heading, type_scale::font(type_scale::TITLE)),
        (TextStyle::Body, type_scale::font(type_scale::BODY)),
        (TextStyle::Button, type_scale::font(type_scale::BODY)),
        (TextStyle::Small, type_scale::font(type_scale::CAPTION)),
        (TextStyle::Monospace, type_scale::mono(type_scale::TERMINAL)),
    ]
    .into();
    style
}
