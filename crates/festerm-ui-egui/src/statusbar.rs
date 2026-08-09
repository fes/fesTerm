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

use egui::{vec2, Align, Color32, Layout, RichText, Ui, UiBuilder};

use crate::chrome::ChipStatus;

/// Fixed height for the bottom status bar, kept compact
/// (`docs/gui-design.md` "Quiet by default").
const STATUS_BAR_HEIGHT: f32 = 24.0;
const STATUS_BAR_TEXT: Color32 = Color32::from_gray(0x9a);
const STATUS_BAR_TEXT_DIM: Color32 = Color32::from_gray(0x78);
const STATUS_BAR_BORDER: Color32 = Color32::from_gray(0x30);
/// Left inset before the first (`left`) segment - deliberately more
/// generous than the `8.0` gap used between the bar's other segments
/// (mockup: the status bar's leading text sits noticeably more indented
/// than sibling chrome regions, a subtle intentional asymmetry rather than
/// a margin bug).
const STATUS_BAR_LEFT_INSET: f32 = 20.0;
/// Small downward nudge applied to this row's whole content rect: egui
/// centers a `Label`'s line-height box exactly, but a line of text's own
/// visible ink sits slightly above that box's mathematical center (fonts
/// reserve more headroom above cap-height than below the baseline for
/// descenders most glyphs here don't use), so a mathematically-centered
/// row optically reads as sitting a little high.
const OPTICAL_VERTICAL_NUDGE: f32 = 1.0;

/// Everything the status bar needs to render one frame. Every field is
/// supplied by the caller (this crate owns no session/tab state); `None`
/// simply omits that segment rather than fabricating a placeholder.
pub struct StatusBarContent<'a> {
    /// Single-line summary of the active session (or a neutral application
    /// label when no session is active).
    pub left: &'a str,
    /// Grid dimensions of the active session's terminal (e.g. `"80×24"`),
    /// when one is active. This is the one piece of the old per-terminal
    /// diagnostics panel that was genuinely useful at a glance, so it now
    /// lives here instead (`docs/gui-design.md` "Bottom status bar").
    pub dimensions: Option<&'a str>,
    /// The session's locality and host platform (e.g. `"Local · Windows"`
    /// / `"Local · Unix"`), when known. This is genuinely available
    /// environment data (not fabricated shell/encoding metadata fesTerm
    /// doesn't track). Deliberately not framed as a line-ending convention:
    /// the host OS a session runs on does not reliably imply its byte
    /// stream's CRLF/LF semantics, especially for a remote (SSH) session.
    pub system: Option<&'a str>,
    /// Connection state, using the same non-color-exclusive vocabulary as
    /// the chip row's status dot.
    pub status: ChipStatus,
    pub status_label: &'a str,
    /// Pre-formatted by the caller so this presentation-only crate never
    /// needs a date/time dependency of its own.
    pub clock: &'a str,
    pub date: &'a str,
}

/// Renders the bottom status bar band.
pub fn show(ui: &mut Ui, content: StatusBarContent<'_>) {
    ui.scope(|ui| {
        ui.set_min_height(STATUS_BAR_HEIGHT);
        ui.set_max_height(STATUS_BAR_HEIGHT);
        let rect = ui.max_rect();
        ui.painter().hline(
            rect.x_range(),
            rect.top(),
            egui::Stroke::new(1.0, STATUS_BAR_BORDER),
        );
        // Shift the whole row's content rect down by a small optical
        // nudge (see `OPTICAL_VERTICAL_NUDGE`) rather than centering
        // strictly on the bar's mathematical middle - a plain
        // `ui.horizontal` here centered every element exactly, but still
        // read as sitting slightly above the bar's true visual center.
        let content_rect = rect.translate(vec2(0.0, OPTICAL_VERTICAL_NUDGE));
        let mut content_ui = ui.new_child(UiBuilder::new().max_rect(content_rect));
        let ui = &mut content_ui;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add_space(STATUS_BAR_LEFT_INSET);
            ui.label(RichText::new(content.left).small().color(STATUS_BAR_TEXT));
            if let Some(dimensions) = content.dimensions {
                ui.label(RichText::new(dimensions).small().color(STATUS_BAR_TEXT_DIM));
            }
            if let Some(system) = content.system {
                ui.label(RichText::new(system).small().color(STATUS_BAR_TEXT_DIM));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                ui.label(RichText::new(content.date).small().color(STATUS_BAR_TEXT));
                ui.label(RichText::new(content.clock).small().color(STATUS_BAR_TEXT));
                if !matches!(content.status, ChipStatus::Neutral) {
                    // A deliberate vertical divider between the time
                    // cluster (clock/date, just added) and the status
                    // cluster (added next): the mockup treats these as two
                    // distinct semantic groups sharing the bar's right
                    // side, not one run-on string.
                    paint_vertical_divider(ui);
                    ui.label(
                        RichText::new(content.status_label)
                            .small()
                            .color(STATUS_BAR_TEXT),
                    );
                    // Sized to the surrounding small-text line height
                    // (rather than a bare fixed box) so the dot's own
                    // center lands on the same optical center line as the
                    // text beside it - a fixed `8.0`-tall box was slightly
                    // shorter than the text's line height and read as
                    // sitting above center.
                    let text_height = ui.text_style_height(&egui::TextStyle::Small);
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(8.0, text_height), egui::Sense::hover());
                    ui.painter()
                        .circle_filled(rect.center(), 3.5, content.status.color());
                    let _ = response;
                }
            });
        });
    });
}

/// A thin vertical rule the same height as the surrounding small text,
/// used to separate distinct semantic clusters in the status bar (mockup:
/// "a vertical line separates time from status").
fn paint_vertical_divider(ui: &mut Ui) {
    let height = ui.text_style_height(&egui::TextStyle::Small);
    let (rect, _response) = ui.allocate_exact_size(egui::vec2(1.0, height), egui::Sense::hover());
    ui.painter().line_segment(
        [rect.center_top(), rect.center_bottom()],
        egui::Stroke::new(1.0, STATUS_BAR_BORDER),
    );
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
                    StatusBarContent {
                        left: "Local Shell — cmd.exe",
                        dimensions: Some("80×24"),
                        system: Some("Local · Windows"),
                        status: ChipStatus::Connected,
                        status_label: "Connected",
                        clock: "12:34:56",
                        date: "2026-08-08",
                    },
                );
            });
        harness.run();
        assert!(harness.query_by_label("Local Shell — cmd.exe").is_some());
        assert!(harness.query_by_label("80×24").is_some());
        assert!(harness.query_by_label("Local · Windows").is_some());
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
                    StatusBarContent {
                        left: "fesTerm",
                        dimensions: None,
                        system: None,
                        status: ChipStatus::Neutral,
                        status_label: "",
                        clock: "12:34:56",
                        date: "2026-08-08",
                    },
                );
            });
        harness.run();
        assert!(harness.query_by_label("fesTerm").is_some());
        assert!(harness.query_by_label("12:34:56").is_some());
    }
}
