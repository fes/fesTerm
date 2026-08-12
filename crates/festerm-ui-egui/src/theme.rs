//! Default fesTerm application palette.
//!
//! These roles style application surfaces and the terminal's default
//! foreground/background only. ANSI, indexed, and explicit RGB terminal
//! colors remain protocol data and are resolved independently by the renderer.

use egui::{Color32, Stroke, Visuals};

pub const SURFACE_WINDOW: Color32 = Color32::from_rgb(0x0e, 0x13, 0x19);
pub const SURFACE_TERMINAL: Color32 = Color32::from_rgb(0x11, 0x16, 0x1e);
/// Chrome and terminal intentionally share one continuous window well. Chips,
/// controls, and content establish hierarchy without a separate title-band
/// color or separator.
pub const SURFACE_CHROME: Color32 = SURFACE_TERMINAL;
pub const SURFACE_TAB_INACTIVE: Color32 = Color32::from_rgb(0x1a, 0x22, 0x2c);
pub const SURFACE_TAB_ACTIVE: Color32 = Color32::from_rgb(0x29, 0x33, 0x3e);
pub const SURFACE_OVERLAY: Color32 = Color32::from_rgb(0x26, 0x31, 0x3d);
pub const SURFACE_SELECTION: Color32 = Color32::from_rgb(0x28, 0x51, 0x6b);

pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xe8, 0xed, 0xf2);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xa7, 0xb2, 0xbd);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x78, 0x85, 0x92);

pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(0x35, 0x41, 0x4e);
pub const BORDER_ACTIVE: Color32 = Color32::from_rgb(0x91, 0xa7, 0xb8);
pub const ACCENT_PRIMARY: Color32 = Color32::from_rgb(0x42, 0xbf, 0xd0);

pub const STATUS_RUNNING: Color32 = Color32::from_rgb(0x4f, 0xc1, 0x7d);
pub const STATUS_STARTING: Color32 = Color32::from_rgb(0xd2, 0xa9, 0x4b);
pub const STATUS_RECONNECTING: Color32 = ACCENT_PRIMARY;
pub const STATUS_DISCONNECTED: Color32 = Color32::from_rgb(0x7c, 0x87, 0x94);
pub const STATUS_ATTENTION: Color32 = Color32::from_rgb(0xb5, 0x8a, 0xd4);
pub const STATUS_ERROR: Color32 = Color32::from_rgb(0xd9, 0x68, 0x68);
pub const STATUS_EXITED: Color32 = Color32::from_rgb(0x65, 0x72, 0x80);

/// Maps the semantic palette onto egui's built-in widget vocabulary.
pub fn default_visuals() -> Visuals {
    let mut visuals = Visuals::dark();

    visuals.override_text_color = None;
    visuals.weak_text_color = Some(TEXT_MUTED);
    visuals.hyperlink_color = ACCENT_PRIMARY;
    visuals.faint_bg_color = SURFACE_TAB_INACTIVE;
    visuals.extreme_bg_color = SURFACE_TERMINAL;
    visuals.text_edit_bg_color = Some(SURFACE_TAB_INACTIVE);
    visuals.code_bg_color = SURFACE_TAB_INACTIVE;
    visuals.warn_fg_color = STATUS_STARTING;
    visuals.error_fg_color = STATUS_ERROR;
    visuals.window_fill = SURFACE_OVERLAY;
    visuals.window_stroke = Stroke::new(1.0, BORDER_SUBTLE);
    visuals.panel_fill = SURFACE_WINDOW;
    visuals.selection.bg_fill = SURFACE_SELECTION;
    visuals.selection.stroke = Stroke::new(1.0, TEXT_PRIMARY);

    style_widget(
        &mut visuals.widgets.noninteractive,
        SURFACE_WINDOW,
        SURFACE_WINDOW,
        BORDER_SUBTLE,
        TEXT_PRIMARY,
    );
    style_widget(
        &mut visuals.widgets.inactive,
        SURFACE_TAB_INACTIVE,
        SURFACE_TAB_INACTIVE,
        BORDER_SUBTLE,
        TEXT_SECONDARY,
    );
    style_widget(
        &mut visuals.widgets.hovered,
        SURFACE_TAB_ACTIVE,
        SURFACE_TAB_ACTIVE,
        BORDER_ACTIVE,
        TEXT_PRIMARY,
    );
    style_widget(
        &mut visuals.widgets.active,
        SURFACE_SELECTION,
        SURFACE_SELECTION,
        ACCENT_PRIMARY,
        TEXT_PRIMARY,
    );
    style_widget(
        &mut visuals.widgets.open,
        SURFACE_TAB_ACTIVE,
        SURFACE_TAB_ACTIVE,
        BORDER_ACTIVE,
        TEXT_PRIMARY,
    );

    visuals
}

fn style_widget(
    widget: &mut egui::style::WidgetVisuals,
    background: Color32,
    weak_background: Color32,
    border: Color32,
    foreground: Color32,
) {
    widget.bg_fill = background;
    widget.weak_bg_fill = weak_background;
    widget.bg_stroke = Stroke::new(1.0, border);
    widget.fg_stroke = Stroke::new(1.0, foreground);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relative_luminance(color: Color32) -> f32 {
        fn linear(channel: u8) -> f32 {
            let channel = f32::from(channel) / 255.0;
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
    }

    fn contrast_ratio(first: Color32, second: Color32) -> f32 {
        let first = relative_luminance(first);
        let second = relative_luminance(second);
        let (lighter, darker) = if first > second {
            (first, second)
        } else {
            (second, first)
        };
        (lighter + 0.05) / (darker + 0.05)
    }

    #[test]
    fn default_visuals_map_the_semantic_roles() {
        let visuals = default_visuals();
        assert!(visuals.dark_mode);
        assert_eq!(visuals.panel_fill, SURFACE_WINDOW);
        assert_eq!(visuals.window_fill, SURFACE_OVERLAY);
        assert_eq!(visuals.hyperlink_color, ACCENT_PRIMARY);
        assert_eq!(visuals.selection.bg_fill, SURFACE_SELECTION);
        assert_eq!(visuals.widgets.inactive.weak_bg_fill, SURFACE_TAB_INACTIVE);
        assert_eq!(visuals.widgets.hovered.weak_bg_fill, SURFACE_TAB_ACTIVE);
    }

    #[test]
    fn essential_text_roles_keep_normal_text_contrast() {
        for surface in [SURFACE_WINDOW, SURFACE_TERMINAL, SURFACE_CHROME] {
            assert!(contrast_ratio(TEXT_PRIMARY, surface) >= 4.5);
            assert!(contrast_ratio(TEXT_SECONDARY, surface) >= 4.5);
        }
    }

    #[test]
    fn active_tab_fill_stays_quiet_but_distinct() {
        assert!(relative_luminance(SURFACE_TAB_ACTIVE) > relative_luminance(SURFACE_TAB_INACTIVE));
        assert!(contrast_ratio(SURFACE_TAB_ACTIVE, SURFACE_CHROME) < 1.5);
        assert!(contrast_ratio(SURFACE_TAB_ACTIVE, SURFACE_TAB_INACTIVE) >= 1.2);
    }
}
