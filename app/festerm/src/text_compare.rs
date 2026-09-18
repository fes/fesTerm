//! Compare: the read-only two-pane view of "what I have" against "what the
//! source now holds" (ADR 0034 §6).
//!
//! Compare exists so a person can decide which whole version to keep. It
//! never merges, never writes, and never edits: it is a way of *looking* at a
//! conflict, and the banner stays pinned above it so the decision is always
//! one click away from the evidence.
//!
//! It is per-view state rather than per-document: one window may be comparing
//! while another goes on showing the text, because looking is not a change to
//! the document.

use eframe::egui::{self, vec2, Align, FontId, Sense, WidgetInfo, WidgetType};
use festerm_document::{DiffSide, LineChange, LineComparison, LineComparisonRow};
use festerm_ui_egui::theme;

use crate::markdown_viewer::{toolbar_button_response, TOOLBAR_BUTTON_GAP, TOOLBAR_BUTTON_HEIGHT};

const COMPARE_TEXT_SIZE: f32 = 13.0;
const ROW_PADDING_X: f32 = 10.0;
const GUTTER_MIN_DIGITS: usize = 2;
const MARKER_GAP: f32 = 6.0;
const PANE_DIVIDER_WIDTH: f32 = 1.0;
const BAR_PADDING_X: i8 = 9;
const BAR_PADDING_Y: i8 = 6;
const LABEL_TEXT_SIZE: f32 = 11.0;
const ROW_EXTRA_HEIGHT: f32 = 6.0;
const BUTTON_RADIUS: f32 = 5.0;

/// One view's comparison. Rebuilt only when either version actually changes,
/// so scrolling and the focused change survive a repaint.
pub(crate) struct ComparePane {
    comparison: LineComparison,
    mine: String,
    source: String,
    remote: bool,
    /// Row indices holding a change, in document order.
    changes: Vec<usize>,
    /// Which entry of `changes` Previous/Next last landed on.
    focused: Option<usize>,
    /// Set when navigation moved, cleared once the row has been scrolled to.
    scroll_to: Option<usize>,
}

impl ComparePane {
    pub(crate) fn new(mine: &str, source: &str, remote: bool) -> Self {
        let comparison = LineComparison::new(mine, source);
        let changes = comparison.change_rows();
        Self {
            comparison,
            mine: mine.to_owned(),
            source: source.to_owned(),
            remote,
            changes,
            focused: None,
            scroll_to: None,
        }
    }

    /// Keeps the comparison true while a sibling view edits the same document
    /// or the source changes again underneath it.
    pub(crate) fn sync(&mut self, mine: &str, source: &str) {
        if self.mine == mine && self.source == source {
            return;
        }
        let focused_row = self
            .focused
            .and_then(|index| self.changes.get(index))
            .copied();
        *self = Self::new(mine, source, self.remote);
        // Keep looking at roughly the change the user was looking at, rather
        // than throwing them back to the top of a document they were part way
        // through reading.
        if let Some(row) = focused_row {
            self.focused = self
                .changes
                .iter()
                .position(|candidate| *candidate >= row)
                .or_else(|| self.changes.len().checked_sub(1));
        }
    }

    #[cfg(test)]
    pub(crate) const fn change_count(&self) -> usize {
        self.comparison.change_count()
    }

    #[cfg(test)]
    pub(crate) const fn focused_change(&self) -> Option<usize> {
        self.focused
    }

    #[cfg(test)]
    pub(crate) fn rows(&self) -> &[LineComparisonRow] {
        self.comparison.rows()
    }

    /// The whole comparison as plain text, one row per line, which is what the
    /// tests assert on: the panes are painted rather than built from widgets.
    #[cfg(test)]
    pub(crate) fn as_text(&self) -> String {
        self.comparison
            .rows()
            .iter()
            .map(|row| match row {
                LineComparisonRow::Collapsed { lines } => collapsed_label(*lines),
                LineComparisonRow::Pair { left, right } => {
                    let side = |line: &Option<festerm_document::DiffLine>| {
                        line.as_ref().map_or_else(String::new, |line| {
                            format!("{}{}", line.change.marker(), line.text)
                        })
                    };
                    format!("{} | {}", side(left), side(right))
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn focus_next(&mut self) {
        if self.changes.is_empty() {
            return;
        }
        let next = match self.focused {
            Some(index) if index + 1 < self.changes.len() => index + 1,
            // Wrapping is what a reader expects of "next" in a bounded list,
            // and stopping dead at the last change only invites a second
            // click that does nothing.
            _ => 0,
        };
        self.focused = Some(next);
        self.scroll_to = Some(self.changes[next]);
    }

    fn focus_previous(&mut self) {
        if self.changes.is_empty() {
            return;
        }
        let previous = match self.focused {
            Some(index) if index > 0 => index - 1,
            _ => self.changes.len() - 1,
        };
        self.focused = Some(previous);
        self.scroll_to = Some(self.changes[previous]);
    }

    /// Renders the whole pane — headings, the two columns, and the footer —
    /// into the height it was given.
    pub(crate) fn show(&mut self, ui: &mut egui::Ui, height: f32) {
        let footer_height = TOOLBAR_BUTTON_HEIGHT + f32::from(BAR_PADDING_Y) * 2.0;
        let heading_height = LABEL_TEXT_SIZE + f32::from(BAR_PADDING_Y) * 2.0 + 4.0;
        let body_height = (height - footer_height - heading_height).max(60.0);

        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            self.show_headings(ui, heading_height);
            hairline(ui);
            ui.allocate_ui(vec2(ui.available_width(), body_height), |ui| {
                ui.set_height(body_height);
                self.show_rows(ui);
            });
            hairline(ui);
            self.show_footer(ui);
        });
    }

    fn show_headings(&self, ui: &mut egui::Ui, height: f32) {
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, theme::SURFACE_PANEL);
        let half = (width - PANE_DIVIDER_WIDTH) / 2.0;
        let font = FontId::proportional(LABEL_TEXT_SIZE);
        for (index, side) in [DiffSide::Mine, DiffSide::Source].into_iter().enumerate() {
            let heading = side.heading(self.remote);
            let pane = egui::Rect::from_min_size(
                egui::pos2(
                    rect.left() + (half + PANE_DIVIDER_WIDTH) * index as f32,
                    rect.top(),
                ),
                vec2(half, height),
            );
            // Allocated rather than only painted, so each heading is a node a
            // screen reader — and a test — can find.
            let response = ui.interact(
                pane,
                ui.id().with(("compare-heading", index)),
                Sense::hover(),
            );
            response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, heading));
            ui.painter().text(
                egui::pos2(pane.left() + ROW_PADDING_X, pane.center().y),
                egui::Align2::LEFT_CENTER,
                heading,
                font.clone(),
                theme::TEXT_SECONDARY,
            );
        }
    }

    fn show_rows(&mut self, ui: &mut egui::Ui) {
        let font = FontId::monospace(COMPARE_TEXT_SIZE);
        let row_height = ui
            .painter()
            .layout_no_wrap("0".to_owned(), font.clone(), theme::TEXT_MUTED)
            .size()
            .y
            + ROW_EXTRA_HEIGHT;
        let gutter = gutter_width(ui, &self.comparison, &font);
        let marker = marker_width(ui, &font);
        let scroll_to = self.scroll_to.take();

        egui::ScrollArea::vertical()
            .id_salt("text-editor-compare")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let width = ui.available_width();
                let half = (width - PANE_DIVIDER_WIDTH) / 2.0;
                for (index, row) in self.comparison.rows().iter().enumerate() {
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(width, row_height), Sense::hover());
                    let accessible = row_accessible_label(row);
                    response
                        .widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &accessible));
                    match row {
                        LineComparisonRow::Collapsed { lines } => {
                            paint_collapsed(ui, rect, half, *lines, &font);
                        }
                        LineComparisonRow::Pair { left, right } => {
                            for (side, line) in [(0.0, left), (1.0, right)] {
                                let pane = egui::Rect::from_min_size(
                                    egui::pos2(
                                        rect.left() + (half + PANE_DIVIDER_WIDTH) * side,
                                        rect.top(),
                                    ),
                                    vec2(half, row_height),
                                );
                                paint_line(ui, pane, line.as_ref(), gutter, marker, &font);
                            }
                        }
                    }
                    paint_divider(ui, rect, half);
                    if scroll_to == Some(index) {
                        ui.scroll_to_rect(rect, Some(Align::Center));
                    }
                }
            });
    }

    fn show_footer(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(theme::SURFACE_PANEL)
            .inner_margin(egui::Margin::symmetric(BAR_PADDING_X, BAR_PADDING_Y))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_height(TOOLBAR_BUTTON_HEIGHT);
                    ui.spacing_mut().item_spacing.x = TOOLBAR_BUTTON_GAP;
                    footer_label(ui, &self.summary(), theme::TEXT_SECONDARY);
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        let navigable = !self.changes.is_empty();
                        ui.add_enabled_ui(navigable, |ui| {
                            if bordered_button(ui, "Next change") {
                                self.focus_next();
                            }
                            if bordered_button(ui, "Previous change") {
                                self.focus_previous();
                            }
                        });
                    });
                });
            });
    }

    /// What the footer says about the comparison, including the honest
    /// admission when a region was too large to align line by line.
    fn summary(&self) -> String {
        let changes = self.comparison.change_count();
        let counted = match changes {
            0 => "No changes".to_owned(),
            1 => "1 change".to_owned(),
            many => format!("{many} changes"),
        };
        if self.comparison.truncated() {
            format!("{counted} · too large to align line by line, shown as one replacement")
        } else {
            counted
        }
    }
}

fn row_accessible_label(row: &LineComparisonRow) -> String {
    match row {
        LineComparisonRow::Collapsed { lines } => collapsed_label(*lines),
        LineComparisonRow::Pair { left, right } => {
            let side = |line: &Option<festerm_document::DiffLine>, name: &str| {
                line.as_ref().map(|line| {
                    let change = match line.change {
                        LineChange::Unchanged => "unchanged",
                        LineChange::Removed => "removed",
                        LineChange::Added => "added",
                    };
                    format!("{name} line {} {change}: {}", line.number, line.text)
                })
            };
            [side(left, "Yours"), side(right, "Source")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ")
        }
    }
}

fn collapsed_label(lines: usize) -> String {
    if lines == 1 {
        "1 unchanged line".to_owned()
    } else {
        format!("{lines} unchanged lines")
    }
}

fn paint_collapsed(ui: &egui::Ui, rect: egui::Rect, half: f32, lines: usize, font: &FontId) {
    ui.painter().rect_filled(rect, 0.0, theme::SURFACE_PANEL);
    let text = format!("· · ·  {}  · · ·", collapsed_label(lines));
    for side in [0.0, 1.0] {
        let centre = rect.left() + (half + PANE_DIVIDER_WIDTH) * side + half / 2.0;
        ui.painter().text(
            egui::pos2(centre, rect.center().y),
            egui::Align2::CENTER_CENTER,
            &text,
            font.clone(),
            theme::TEXT_MUTED,
        );
    }
}

/// One side of one row.
///
/// The line's *text* is always set in the ordinary foreground: tinting a whole
/// line red or green costs contrast on a dark theme and makes colour do work
/// the `-`/`+` marker is already doing. The tint is carried by a quiet band
/// behind the row and by the marker glyph, so the comparison reads the same in
/// monochrome (`docs/gui-design.md`, ADR 0034 §8).
fn paint_line(
    ui: &egui::Ui,
    rect: egui::Rect,
    line: Option<&festerm_document::DiffLine>,
    gutter: f32,
    marker: f32,
    font: &FontId,
) {
    let Some(line) = line else {
        // A side with no line is not a rendering failure, so it is said
        // rather than left as a hole: a flat, quiet band that reads as
        // "nothing here" beside the version that does have a line.
        ui.painter().rect_filled(rect, 0.0, theme::SURFACE_PANEL);
        return;
    };
    let (fill, marker_colour) = match line.change {
        LineChange::Unchanged => (None, theme::TEXT_MUTED),
        LineChange::Removed => (Some(theme::DIFF_REMOVED_FILL), theme::DIFF_REMOVED_TEXT),
        LineChange::Added => (Some(theme::DIFF_ADDED_FILL), theme::DIFF_ADDED_TEXT),
    };
    if let Some(fill) = fill {
        ui.painter().rect_filled(rect, 0.0, fill);
    }
    // Clipped to its own pane: a line longer than half the window must stop
    // at the divider rather than run through the other version.
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    painter.text(
        egui::pos2(rect.left() + gutter, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        line.number.to_string(),
        font.clone(),
        theme::TEXT_MUTED,
    );
    let marker_left = rect.left() + gutter + MARKER_GAP;
    painter.text(
        egui::pos2(marker_left, rect.center().y),
        egui::Align2::LEFT_CENTER,
        line.change.marker(),
        font.clone(),
        marker_colour,
    );
    painter.text(
        egui::pos2(marker_left + marker, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &line.text,
        font.clone(),
        theme::TEXT_SECONDARY,
    );
}

/// The width of the `-`/`+` column, so the text beside it starts in the same
/// place whether or not the line changed.
fn marker_width(ui: &egui::Ui, font: &FontId) -> f32 {
    ui.painter()
        .layout_no_wrap("0 ".to_owned(), font.clone(), theme::TEXT_MUTED)
        .size()
        .x
}

fn paint_divider(ui: &egui::Ui, rect: egui::Rect, half: f32) {
    let x = rect.left() + half + PANE_DIVIDER_WIDTH / 2.0;
    ui.painter().line_segment(
        [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
        egui::Stroke::new(PANE_DIVIDER_WIDTH, theme::BORDER_SUBTLE),
    );
}

/// The width the line-number column needs for the larger of the two versions.
fn gutter_width(ui: &egui::Ui, comparison: &LineComparison, font: &FontId) -> f32 {
    let widest = comparison
        .rows()
        .iter()
        .filter_map(|row| match row {
            LineComparisonRow::Pair { left, right } => [left, right]
                .into_iter()
                .flatten()
                .map(|line| line.number)
                .max(),
            LineComparisonRow::Collapsed { .. } => None,
        })
        .max()
        .unwrap_or(1);
    let digits = widest.to_string().len().max(GUTTER_MIN_DIGITS);
    let digit_width = ui
        .painter()
        .layout_no_wrap("0".repeat(digits), font.clone(), theme::TEXT_MUTED)
        .size()
        .x;
    digit_width + ROW_PADDING_X
}

/// A footer control. The two navigation buttons carry a hairline border
/// because they sit beside a plain count: without one they read as more label
/// rather than as something to press.
fn bordered_button(ui: &mut egui::Ui, label: &str) -> bool {
    let response = toolbar_button_response(ui, None, label, label, false);
    let colour = if ui.is_enabled() {
        theme::BORDER_SUBTLE
    } else {
        theme::BORDER_SUBTLE.gamma_multiply(0.5)
    };
    ui.painter().rect_stroke(
        response.rect,
        BUTTON_RADIUS,
        egui::Stroke::new(1.0, colour),
        egui::StrokeKind::Inside,
    );
    response.clicked()
}

fn footer_label(ui: &mut egui::Ui, text: &str, colour: egui::Color32) {
    let font = FontId::proportional(LABEL_TEXT_SIZE);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, colour);
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    ui.painter().galley(rect.left_top(), galley, colour);
}

fn hairline(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().line_segment(
        [rect.left_center(), rect.right_center()],
        egui::Stroke::new(1.0, theme::BORDER_SUBTLE),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINE: &str = "alpha\nbravo\ncharlie\ndelta\n";
    const SOURCE: &str = "alpha\nBRAVO\ncharlie\nDELTA\n";

    #[test]
    fn compares_the_two_versions() {
        let pane = ComparePane::new(MINE, SOURCE, false);
        // One line rewritten is one change, not one removal plus one
        // addition: the row is what the reader decides about.
        assert_eq!(pane.change_count(), 2);
        let text = pane.as_text();
        assert!(text.contains("-bravo"), "{text}");
        assert!(text.contains("+BRAVO"), "{text}");
        assert!(text.contains("-delta"), "{text}");
        assert!(text.contains("+DELTA"), "{text}");
    }

    #[test]
    fn identical_versions_report_no_changes() {
        let pane = ComparePane::new(MINE, MINE, false);
        assert_eq!(pane.change_count(), 0);
        assert_eq!(pane.summary(), "No changes");
    }

    #[test]
    fn summary_counts_changes() {
        assert_eq!(ComparePane::new(MINE, SOURCE, false).summary(), "2 changes");
        let one = ComparePane::new("alpha\n", "alpha\nbravo\n", false);
        assert_eq!(one.summary(), "1 change");
        assert_eq!(ComparePane::new(MINE, MINE, false).summary(), "No changes");
    }

    #[test]
    fn navigation_walks_the_changes_and_wraps() {
        let mut pane = ComparePane::new(MINE, SOURCE, false);
        assert_eq!(pane.focused_change(), None);
        pane.focus_next();
        assert_eq!(pane.focused_change(), Some(0));
        pane.focus_next();
        assert_eq!(pane.focused_change(), Some(1));
        pane.focus_next();
        assert_eq!(pane.focused_change(), Some(0), "next wraps to the first");
        pane.focus_previous();
        assert_eq!(
            pane.focused_change(),
            Some(1),
            "previous from the first wraps to the last"
        );
    }

    #[test]
    fn navigation_on_an_identical_pair_does_nothing() {
        let mut pane = ComparePane::new(MINE, MINE, false);
        pane.focus_next();
        assert_eq!(pane.focused_change(), None);
        pane.focus_previous();
        assert_eq!(pane.focused_change(), None);
    }

    #[test]
    fn sync_rebuilds_only_when_a_version_changed() {
        let mut pane = ComparePane::new(MINE, SOURCE, false);
        pane.focus_next();
        pane.focus_next();
        let focused = pane.focused_change();
        pane.sync(MINE, SOURCE);
        assert_eq!(
            pane.focused_change(),
            focused,
            "an unchanged pair is left alone"
        );

        pane.sync("alpha\nbravo\ncharlie\ndelta\necho\n", SOURCE);
        assert_eq!(pane.change_count(), 3, "{}", pane.as_text());
    }

    #[test]
    fn headings_name_where_each_version_is() {
        assert_eq!(DiffSide::Source.heading(false), "On disk");
        assert_eq!(DiffSide::Source.heading(true), "On the remote host");
        assert_eq!(DiffSide::Mine.heading(true), "Your version · unsaved");
    }

    #[test]
    fn long_unchanged_runs_are_collapsed() {
        let mine: String = (1..=40).map(|n| format!("line {n}\n")).collect();
        let source = mine.replace("line 1\n", "changed 1\n");
        let pane = ComparePane::new(&mine, &source, false);
        assert!(
            pane.rows()
                .iter()
                .any(|row| matches!(row, LineComparisonRow::Collapsed { .. })),
            "a 40-line file with one change must fold the rest away"
        );
        assert!(pane.as_text().contains("unchanged lines"));
    }
}
