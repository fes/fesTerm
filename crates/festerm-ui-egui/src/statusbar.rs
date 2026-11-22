//! Bottom application status bar.
//!
//! `docs/gui-design.md` ("Bottom status bar"): a quiet, always-dark footer
//! that shows sourced dimensions/locality on the left and transport state on
//! the right. It is purely presentation —
//! this crate owns no session, tab, or terminal state, and every string it
//! paints is supplied by the caller.
//!
//! Stable identity stays in the active chip. Client time/date and fabricated
//! shell, encoding, line-ending, or command-timing fields are absent.

use egui::{vec2, Align, Color32, Layout, RichText, Ui, UiBuilder};

use crate::chrome::ChipStatus;
use crate::theme;

/// Fixed height for the bottom status bar, kept compact
/// (`docs/gui-design.md` "Quiet by default").
const STATUS_BAR_HEIGHT: f32 = 24.0;
const STATUS_BAR_TEXT: Color32 = theme::TEXT_SECONDARY;
const STATUS_BAR_TEXT_DIM: Color32 = theme::TEXT_MUTED;
const STATUS_BAR_BORDER: Color32 = theme::BORDER_SUBTLE;
/// Left inset before the first sourced segment - deliberately more
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
    /// The editor's vi state as a word — `NORMAL`, `INSERT`, `VISUAL`,
    /// `REPLACE`, `COMMAND`, `SEARCH` — shown at the leading edge of the bar.
    ///
    /// It is a word rather than a cursor shape or a colour because a shape is
    /// invisible to a screen reader, unreliable under a high-contrast theme,
    /// and ambiguous the moment the caret scrolls off screen (ADR 0034 §10).
    pub mode: Option<&'a str>,
    /// Leading context for surfaces that aren't a terminal session and so
    /// have no grid/locality to report — currently the SFTP file manager,
    /// which uses it for `"SFTP"` ahead of its `system` endpoints. Mirrors
    /// the mockup's `SFTP · Local + ops@prod-03.example` status line
    /// (`docs/images/gui-mockups/sftp-workflow.html`, `.fsftp-statusline`).
    /// Rendered first, in the same weight as `dimensions`.
    pub context: Option<&'a str>,
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
    /// The active session's same sanitized secondary display value normally
    /// shown on its chip, relocated here only while the **Show session
    /// details in chips** preference is off (`docs/gui-design.md` "Show
    /// session details in chips"). Sits after dimensions/locality and before
    /// the right-aligned state; never shown alongside chip secondary text.
    pub detail: Option<&'a str>,
    /// The durable session the active terminal is attached to, as
    /// `provider · session name` (e.g. `"tmux · deploy-watch"`), shown only
    /// while the **Show durable session name in status bar** preference is
    /// on (`docs/gui-design.md` "Bottom status bar").
    ///
    /// This is stable identity rather than the terminal-provided title, and
    /// it is independent of `detail`: the two answer different questions
    /// ("which durable session owns this shell?" versus "what is this tab
    /// showing right now?") and may appear together.
    pub durable_session: Option<&'a str>,
    /// Optional active SSH port-forward count for the focused session.
    pub port_forwards: Option<&'a str>,
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
        let has_dot = !matches!(content.status, ChipStatus::Neutral);
        let label_width = ui
            .painter()
            .layout_no_wrap(
                content.status_label.to_owned(),
                egui::TextStyle::Small.resolve(ui.style()),
                STATUS_BAR_TEXT,
            )
            .size()
            .x;
        let has_state = has_dot || !content.status_label.is_empty();
        let state_width = if has_state {
            8.0 + label_width
                + if has_dot { 8.0 } else { 0.0 }
                + if has_dot && !content.status_label.is_empty() {
                    8.0
                } else {
                    0.0
                }
        } else {
            0.0
        };
        let state_left = (content_rect.right() - state_width).max(content_rect.left());
        let state_rect =
            egui::Rect::from_min_max(egui::pos2(state_left, content_rect.top()), content_rect.max);
        let left_start = (content_rect.left() + STATUS_BAR_LEFT_INSET).min(state_left);
        let left_end = (state_left - if has_state { 8.0 } else { 0.0 }).max(left_start);
        let left_rect = egui::Rect::from_min_max(
            egui::pos2(left_start, content_rect.top()),
            egui::pos2(left_end, content_rect.bottom()),
        );
        let mut left = ui.new_child(
            UiBuilder::new()
                .max_rect(left_rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        left.set_clip_rect(ui.clip_rect().intersect(left_rect));
        left.spacing_mut().item_spacing.x = 8.0;
        let fields = [
            content.mode.map(|v| {
                (
                    v,
                    RichText::new(v)
                        .small()
                        .strong()
                        .color(theme::ACCENT_PRIMARY),
                )
            }),
            content
                .context
                .map(|v| (v, RichText::new(v).small().color(STATUS_BAR_TEXT))),
            content
                .dimensions
                .map(|v| (v, RichText::new(v).small().color(STATUS_BAR_TEXT))),
            content
                .system
                .map(|v| (v, RichText::new(v).small().color(STATUS_BAR_TEXT_DIM))),
            content
                .detail
                .map(|v| (v, RichText::new(v).small().color(STATUS_BAR_TEXT_DIM))),
            content
                .durable_session
                .map(|v| (v, RichText::new(v).small().color(STATUS_BAR_TEXT_DIM))),
            content
                .port_forwards
                .map(|v| (v, RichText::new(v).small().color(STATUS_BAR_TEXT_DIM))),
        ];
        for (value, text) in fields.into_iter().flatten() {
            if left.available_width() <= 0.0 {
                break;
            }
            left.add(egui::Label::new(text).truncate())
                .on_hover_text(value);
        }
        // Reserve the factual state independently of variable-length left text.
        // Neutral still permits a plain label, while omitting the transport dot.
        let mut right = ui.new_child(
            UiBuilder::new()
                .max_rect(state_rect)
                .layout(Layout::right_to_left(Align::Center)),
        );
        right.set_clip_rect(ui.clip_rect().intersect(state_rect));
        right.spacing_mut().item_spacing.x = 8.0;
        right.add_space(8.0);
        if has_dot {
            let text_height = right.text_style_height(&egui::TextStyle::Small);
            let (rect, _) = right.allocate_exact_size(vec2(8.0, text_height), egui::Sense::hover());
            right
                .painter()
                .circle_filled(rect.center(), 3.5, content.status.color());
        }
        if !content.status_label.is_empty() {
            right
                .add(
                    egui::Label::new(
                        RichText::new(content.status_label)
                            .small()
                            .color(STATUS_BAR_TEXT),
                    )
                    .truncate(),
                )
                .on_hover_text(content.status_label);
        }
    });
}

#[cfg(test)]
mod tests {
    use egui_kittest::{kittest::Queryable, Harness};

    use super::*;

    #[test]
    fn status_bar_reserves_connected_state_before_narrow_long_details() {
        let path = "Remote · /home/evidence/a-very-long-project-directory/another-long-directory/working-tree";
        let mut harness = Harness::builder()
            .with_size(egui::vec2(387.0, 60.0))
            .build_ui(|ui| {
                show(
                    ui,
                    StatusBarContent {
                        mode: None,
                        context: None,
                        dimensions: Some("44×34"),
                        system: Some(path),
                        status: ChipStatus::Connected,
                        status_label: "Connected",
                        detail: Some("copilot — a-long-running-task-title"),
                        durable_session: Some("festerm-sessiond · shared-development-shell"),
                        port_forwards: Some("3 active forwards"),
                    },
                )
            });
        harness.run();
        let state = harness.get_by_label("Connected").rect();
        let grid = harness.get_by_label("44×34").rect();
        let detail = harness.get_by_label(path).rect();
        assert!(state.right() <= 387.0 && state.left() >= 0.0, "{state:?}");
        assert!(grid.right() + 8.0 <= state.left(), "{grid:?} {state:?}");
        assert!(detail.right() + 8.0 <= state.left(), "{detail:?} {state:?}");
        let painted = harness
            .output()
            .shapes
            .iter()
            .find(|shape| {
                matches!(
                    &shape.shape, egui::Shape::Text(text) if text.galley.job.text == path
                )
            })
            .expect("long source remains a real text label");
        assert!(painted.clip_rect.right() + 8.0 <= state.left());
        let dot = harness
            .output()
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Circle(circle) if (circle.radius - 3.5).abs() < f32::EPSILON => {
                    Some(circle)
                }
                _ => None,
            })
            .expect("Connected still has its factual state dot");
        assert!(state.right() + 8.0 <= dot.center.x - dot.radius);
        assert!(dot.center.x + dot.radius <= 387.0);
        if let egui::Shape::Text(text) = &painted.shape {
            assert_eq!(text.galley.rows.len(), 1);
            assert!(text.galley.elided, "long details need visible truncation");
        }
    }

    #[test]
    fn status_bar_wide_labels_keep_full_text_without_elision() {
        let labels = [
            "INSERT",
            "SSH",
            "80×24",
            "Remote · Linux",
            "cargo test",
            "tmux · deploy",
            "2 active forwards",
            "Connected",
        ];
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1200.0, 60.0))
            .build_ui(|ui| {
                show(
                    ui,
                    StatusBarContent {
                        mode: Some(labels[0]),
                        context: Some(labels[1]),
                        dimensions: Some(labels[2]),
                        system: Some(labels[3]),
                        detail: Some(labels[4]),
                        durable_session: Some(labels[5]),
                        port_forwards: Some(labels[6]),
                        status: ChipStatus::Connected,
                        status_label: labels[7],
                    },
                )
            });
        harness.run();
        let state = harness.get_by_label("Connected").rect();
        for label in labels {
            let rect = harness.get_by_label(label).rect();
            if label != "Connected" {
                assert!(rect.right() + 8.0 <= state.left());
            }
            let text = harness
                .output()
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == label => Some(text),
                    _ => None,
                })
                .expect("each wide label is painted");
            assert_eq!(text.galley.rows.len(), 1);
            assert!(
                !text.galley.elided,
                "{label} must fit at the ordinary wide size"
            );
        }
    }

    #[test]
    fn status_bar_renders_left_and_right_text() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(400.0, 60.0))
            .build_ui(|ui| {
                show(
                    ui,
                    StatusBarContent {
                        mode: None,
                        context: None,
                        dimensions: Some("80×24"),
                        system: Some("Local · Windows"),
                        status: ChipStatus::Connected,
                        status_label: "Running",
                        detail: None,
                        durable_session: None,
                        port_forwards: None,
                    },
                );
            });
        harness.run();
        assert!(harness.query_by_label("80×24").is_some());
        assert!(harness.query_by_label("Local · Windows").is_some());
        assert!(harness.query_by_label("Running").is_some());
    }

    #[test]
    fn status_bar_omits_status_label_and_dot_when_neutral() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(400.0, 60.0))
            .build_ui(|ui| {
                show(
                    ui,
                    StatusBarContent {
                        mode: None,
                        context: None,
                        dimensions: None,
                        system: None,
                        status: ChipStatus::Neutral,
                        status_label: "",
                        detail: None,
                        durable_session: None,
                        port_forwards: None,
                    },
                );
            });
        harness.run();
        assert!(harness.query_by_label("Running").is_none());
        assert!(harness.query_by_label("80×24").is_none());
    }

    #[test]
    fn status_bar_shows_a_label_without_a_dot_when_neutral() {
        // The Markdown viewer has a state worth naming (`Read only`) but no
        // transport to report, so the label and the dot are independent.
        let mut harness = Harness::builder()
            .with_size(egui::vec2(400.0, 60.0))
            .build_ui(|ui| {
                show(
                    ui,
                    StatusBarContent {
                        mode: None,
                        context: Some("Local Markdown"),
                        dimensions: None,
                        system: Some("UTF-8"),
                        status: ChipStatus::Neutral,
                        status_label: "Read only",
                        detail: None,
                        durable_session: None,
                        port_forwards: None,
                    },
                );
            });
        harness.run();
        assert!(harness.query_by_label("Read only").is_some());
        assert!(harness.query_by_label("Local Markdown").is_some());
    }

    #[test]
    fn status_bar_shows_the_relocated_detail_between_system_and_status() {
        // `docs/gui-design.md` "Show session details in chips": while chips
        // are compact, the active session's detail relocates here, sitting
        // after dimensions/locality and before the right-aligned state.
        let mut harness = Harness::builder()
            .with_size(egui::vec2(400.0, 60.0))
            .build_ui(|ui| {
                show(
                    ui,
                    StatusBarContent {
                        mode: None,
                        context: None,
                        dimensions: Some("80×24"),
                        system: Some("Local · macOS"),
                        status: ChipStatus::Connected,
                        status_label: "Running",
                        detail: Some("cargo test — fesTerm"),
                        durable_session: None,
                        port_forwards: None,
                    },
                );
            });
        harness.run();
        assert!(harness.query_by_label("cargo test — fesTerm").is_some());
    }

    #[test]
    fn status_bar_shows_an_active_port_forward_count_when_present() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(400.0, 60.0))
            .build_ui(|ui| {
                show(
                    ui,
                    StatusBarContent {
                        mode: None,
                        context: None,
                        dimensions: Some("80×24"),
                        system: Some("Remote"),
                        status: ChipStatus::Connected,
                        status_label: "Connected",
                        detail: None,
                        durable_session: None,
                        port_forwards: Some("2 active forwards"),
                    },
                );
            });
        harness.run();
        assert!(harness.query_by_label("2 active forwards").is_some());
    }

    #[test]
    fn status_bar_shows_durable_session_identity_alongside_the_relocated_detail() {
        // Feature request #168: identity and detail answer different
        // questions, so unlike the chip/status-bar detail relationship these
        // two are not alternatives and may appear together.
        let mut harness = Harness::builder()
            .with_size(egui::vec2(500.0, 60.0))
            .build_ui(|ui| {
                show(
                    ui,
                    StatusBarContent {
                        mode: None,
                        context: None,
                        dimensions: Some("80×24"),
                        system: Some("Local · macOS"),
                        status: ChipStatus::Connected,
                        status_label: "Running",
                        detail: Some("cargo test — fesTerm"),
                        durable_session: Some("tmux · deploy-watch"),
                        port_forwards: None,
                    },
                );
            });
        harness.run();

        let durable = harness.get_by_label("tmux · deploy-watch").rect();
        let detail = harness.get_by_label("cargo test — fesTerm").rect();
        assert!(
            detail.right() <= durable.left(),
            "durable identity belongs with the left-hand session facts, after the \
             relocated detail: detail ended at {} and identity started at {}",
            detail.right(),
            durable.left()
        );
    }
}
