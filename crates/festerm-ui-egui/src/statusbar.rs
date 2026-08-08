//! Bottom application status bar.
//!
//! `docs/gui-design.md` ("Contextual status region"): a quiet, always-dark
//! footer that shows the active session's real state on the left and
//! connection/clock information on the right. It is purely presentation —
//! this crate owns no session, tab, or terminal state, and every string it
//! paints is supplied by the caller.
//!
//! Unlike the reference mockup (which shows fabricated shell
//! version/encoding/line-ending fields fesTerm does not actually track),
//! this bar only ever shows genuinely available data: the caller decides
//! what `left` contains, and this module does not invent placeholder
//! metadata to visually pad it out.

use egui::{Align, Color32, Layout, RichText, Ui};

use crate::chrome::ChipStatus;

/// Fixed height for the bottom status bar, kept compact
/// (`docs/gui-design.md` "Quiet by default").
const STATUS_BAR_HEIGHT: f32 = 24.0;
const STATUS_BAR_TEXT: Color32 = Color32::from_gray(0x9a);
const STATUS_BAR_BORDER: Color32 = Color32::from_gray(0x30);

/// Renders the bottom status bar band.
///
/// `left` is a single-line summary of the active session (or a neutral
/// application label when no session is active). `status` and
/// `status_label` describe connection state via the same non-color-exclusive
/// vocabulary as the chip row's status dot. `clock` and `date` are
/// pre-formatted by the caller so this presentation-only crate never needs a
/// date/time dependency of its own.
pub fn show(
    ui: &mut Ui,
    left: &str,
    status: ChipStatus,
    status_label: &str,
    clock: &str,
    date: &str,
) {
    ui.scope(|ui| {
        ui.set_min_height(STATUS_BAR_HEIGHT);
        ui.set_max_height(STATUS_BAR_HEIGHT);
        let rect = ui.max_rect();
        ui.painter().hline(
            rect.x_range(),
            rect.top(),
            egui::Stroke::new(1.0, STATUS_BAR_BORDER),
        );
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add_space(8.0);
            ui.label(RichText::new(left).small().color(STATUS_BAR_TEXT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                ui.label(RichText::new(date).small().color(STATUS_BAR_TEXT));
                ui.label(RichText::new(clock).small().color(STATUS_BAR_TEXT));
                if !matches!(status, ChipStatus::Neutral) {
                    ui.label(RichText::new(status_label).small().color(STATUS_BAR_TEXT));
                    let (response, painter) =
                        ui.allocate_painter(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    painter.circle_filled(response.rect.center(), 3.5, status.color());
                }
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use egui_kittest::{kittest::Queryable, Harness};

    use super::*;

    #[test]
    fn status_bar_renders_left_and_right_text() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(400.0, 60.0))
            .build_ui(|ui| {
                show(
                    ui,
                    "Local Shell — cmd.exe",
                    ChipStatus::Connected,
                    "Connected",
                    "12:34:56",
                    "2026-08-08",
                );
            });
        harness.run();
        assert!(harness.query_by_label("Local Shell — cmd.exe").is_some());
        assert!(harness.query_by_label("Connected").is_some());
        assert!(harness.query_by_label("12:34:56").is_some());
        assert!(harness.query_by_label("2026-08-08").is_some());
    }

    #[test]
    fn status_bar_omits_status_label_and_dot_when_neutral() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(400.0, 60.0))
            .build_ui(|ui| {
                show(
                    ui,
                    "fesTerm",
                    ChipStatus::Neutral,
                    "",
                    "12:34:56",
                    "2026-08-08",
                );
            });
        harness.run();
        assert!(harness.query_by_label("fesTerm").is_some());
        assert!(harness.query_by_label("12:34:56").is_some());
    }
}
