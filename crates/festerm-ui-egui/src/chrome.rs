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

use egui::{Align, Color32, Frame, Layout, RichText, Stroke, Ui};

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
    const fn accessible_label(self) -> &'static str {
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
}

/// Renders the top-of-window chrome band: New Tab, the independent session
/// chips, and the session-inspector toggle. Returns every gesture observed
/// this frame; callers apply at most the ones they recognize.
pub fn show(
    ui: &mut Ui,
    chips: &[ChipViewModel],
    active: ChipId,
    inspector_open: bool,
) -> Vec<ChromeAction> {
    let mut actions = Vec::new();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        if ui
            .button("+ Launcher")
            .on_hover_text("Open a new launcher tab")
            .clicked()
        {
            actions.push(ChromeAction::NewTab);
        }
        for chip in chips {
            show_chip(ui, chip, chip.id == active, &mut actions);
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
    Frame::group(ui.style())
        .fill(fill)
        .stroke(stroke)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
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
}

#[cfg(test)]
mod tests {
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
}
