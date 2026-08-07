use std::time::{Duration, Instant};

use egui::{Label, Sense, TextStyle, Ui, Vec2};
use festerm_core::{InputEventOutcome, Terminal};

use crate::{
    cache::{ResizeOutcome, ResizeTracker, TerminalRenderCache},
    geometry::{dimensions_from_viewport, grid_view_size, viewport_layout, CellMetrics, ViewSize},
    input::{
        route_egui_events, EncodedInputSink, InputAdapterState, InputSinkDiagnostics,
        KeyboardOwnership, TerminalPointerState,
    },
    renderer::{
        measure_input_to_paint_submission, paint_grid, FontSettings, GlyphCache, GridLayout,
        GridPaint,
    },
    selection::Selection,
    TerminalSnapshot, DEFAULT_BACKGROUND, DEFAULT_FOREGROUND,
};

/// Diagnostics captured by the UI path without recording terminal content.
#[derive(Clone, Debug, Default)]
pub struct FrameDiagnostics {
    pub frame_time: Option<Duration>,
    /// Time from observed input routing through submitting grid paint shapes to
    /// egui. This does not measure GPU presentation or pixels on screen.
    pub input_to_paint_submission: Option<Duration>,
    pub calculated_dimensions: Option<festerm_core::Dimensions>,
    pub grid_rect: Option<egui::Rect>,
    pub dirty_rows: usize,
    pub last_input_outcome: Option<InputEventOutcome>,
    pub input_queue_depth: usize,
    pub input_sink: Option<InputSinkDiagnostics>,
}

/// The initial `egui` terminal renderer and input adapter.
///
/// It renders one cached layout per leading display cell. This deliberately
/// preserves the one-cell mapping and does **not** claim ligature shaping;
/// ligature-capable run shaping remains Milestone 6 work.
#[derive(Default)]
pub struct TerminalView {
    pub(crate) fonts: FontSettings,
    cell_run_shaping: bool,
    pub(crate) cache: TerminalRenderCache,
    pub(crate) glyphs: GlyphCache,
    pub(crate) selection: Selection,
    pub(crate) resize: ResizeTracker,
    pub(crate) diagnostics: FrameDiagnostics,
    pub(crate) show_diagnostics: bool,
    pub(crate) keyboard: KeyboardOwnership,
    pub(crate) pointer: TerminalPointerState,
}

impl TerminalView {
    #[cfg(test)]
    pub(crate) fn enable_cell_run_shaping_for_test(&mut self) {
        self.cell_run_shaping = true;
    }

    pub fn diagnostics(&self) -> &FrameDiagnostics {
        &self.diagnostics
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// Shows the terminal with the compatibility no-session status text.
    pub fn show(&mut self, ui: &mut Ui, terminal: &mut Terminal, sink: &mut impl EncodedInputSink) {
        self.show_with_status(
            ui,
            terminal,
            sink,
            "No session attached: encoded input is not sent or retained.",
            "No session diagnostics are available.",
        );
    }

    /// Shows the terminal and application-provided compact and detailed status.
    pub fn show_with_status(
        &mut self,
        ui: &mut Ui,
        terminal: &mut Terminal,
        sink: &mut impl EncodedInputSink,
        session_status: &str,
        session_diagnostics: &str,
    ) {
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(DEFAULT_BACKGROUND))
            .show(ui, |ui| {
                self.show_in_panel(ui, terminal, sink, session_status, session_diagnostics);
            });
    }

    fn show_in_panel(
        &mut self,
        ui: &mut Ui,
        terminal: &mut Terminal,
        sink: &mut impl EncodedInputSink,
        session_status: &str,
        session_diagnostics: &str,
    ) {
        ui.horizontal(|ui| {
            ui.heading("fesTerm");
            if ui
                .selectable_label(self.show_diagnostics, "Diagnostics")
                .clicked()
            {
                self.show_diagnostics = !self.show_diagnostics;
            }
            ui.add_sized(
                [
                    ui.available_width(),
                    ui.text_style_height(&TextStyle::Small),
                ],
                Label::new(session_status).truncate(),
            );
        });
        ui.separator();
        let footer_height = if self.show_diagnostics {
            ui.text_style_height(&TextStyle::Small)
        } else {
            0.0
        };
        let grid_size = grid_view_size(ui.available_size_before_wrap(), footer_height);
        ui.allocate_ui(Vec2::new(grid_size.width, grid_size.height), |ui| {
            self.show_in_ui(ui, terminal, sink);
        });
        if self.show_diagnostics {
            self.show_diagnostics(ui, session_diagnostics);
        }
    }

    /// Shows the cell grid inside an existing `egui` UI.
    pub fn show_in_ui(
        &mut self,
        ui: &mut Ui,
        terminal: &mut Terminal,
        sink: &mut impl EncodedInputSink,
    ) {
        let frame_started = Instant::now();
        let glyph =
            ui.painter()
                .layout_no_wrap("M".to_owned(), self.fonts.font_id(), DEFAULT_FOREGROUND);
        let Some(metrics) = CellMetrics::new(glyph.size().x, glyph.size().y) else {
            return;
        };
        let available = ui.available_size_before_wrap();
        let viewport = ViewSize {
            width: available.x,
            height: available.y,
        };
        let calculated = dimensions_from_viewport(viewport, metrics);
        self.diagnostics.calculated_dimensions = calculated;
        if matches!(
            self.resize.apply_viewport(terminal, viewport, metrics),
            ResizeOutcome::Resized(_)
        ) {
            self.selection.clear();
            self.pointer = TerminalPointerState::default();
            sink.record_terminal_resize(terminal.dimensions());
        }

        let (viewport_rect, response) = ui.allocate_exact_size(available, Sense::click_and_drag());
        ui.painter()
            .rect_filled(viewport_rect, 0.0, DEFAULT_BACKGROUND);
        let vp_layout =
            viewport_layout(viewport_rect.min, viewport, metrics, terminal.dimensions());
        self.diagnostics.grid_rect = Some(vp_layout.grid);
        if response.clicked() {
            response.request_focus();
        }
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            );
        });
        let layout = GridLayout {
            rect: vp_layout.grid,
            dimensions: vp_layout.dimensions,
            metrics,
        };
        let reports = route_egui_events(
            ui,
            &response,
            layout,
            terminal,
            InputAdapterState {
                selection: &mut self.selection,
                keyboard: &mut self.keyboard,
                pointer: &mut self.pointer,
            },
            sink,
        );
        for report in reports.routes {
            self.diagnostics.last_input_outcome = Some(report.outcome);
            self.diagnostics.input_queue_depth = report.queue_depth;
        }

        let dirty_rows = terminal.take_dirty_rows();
        let snapshot = TerminalSnapshot::from_terminal(terminal);
        let update = self.cache.update(snapshot, &dirty_rows);
        self.diagnostics.dirty_rows = update.updated_rows.len();
        let (_, input_to_paint_submission) =
            measure_input_to_paint_submission(reports.input_observed, || {
                paint_grid(
                    ui.painter().with_clip_rect(vp_layout.viewport),
                    GridPaint {
                        layout,
                        snapshot,
                        cache: &self.cache,
                        selection: &self.selection,
                        fonts: &self.fonts,
                        cell_run_shaping: self.cell_run_shaping,
                        focused: response.has_focus(),
                    },
                    &mut self.glyphs,
                );
            });
        self.diagnostics.input_to_paint_submission = input_to_paint_submission;
        self.diagnostics.input_sink = sink.input_diagnostics();
        self.diagnostics.frame_time = Some(frame_started.elapsed());
    }

    fn show_diagnostics(&self, ui: &mut Ui, session_diagnostics: &str) {
        let dimensions = self
            .diagnostics
            .calculated_dimensions
            .map(|size| format!("{}x{}", size.columns(), size.rows()))
            .unwrap_or_else(|| "unavailable".to_owned());
        let frame = self
            .diagnostics
            .frame_time
            .map(|duration| format!("{:.2} ms", duration.as_secs_f64() * 1_000.0))
            .unwrap_or_else(|| "n/a".to_owned());
        let input_to_paint_submission = self
            .diagnostics
            .input_to_paint_submission
            .map(|duration| format!("{:.2} ms", duration.as_secs_f64() * 1_000.0))
            .unwrap_or_else(|| "n/a".to_owned());
        let sink = self.diagnostics.input_sink.map_or_else(
            || "input sink diagnostics unavailable".to_owned(),
            |diagnostics| {
                format!(
                    "sink events {}; bytes {}; last {:?}; queue {}",
                    diagnostics.event_count,
                    diagnostics.byte_count,
                    diagnostics.last_outcome,
                    diagnostics.last_queue_depth,
                )
            },
        );
        ui.add(
            Label::new(format!(
                "{session_diagnostics}; frame {frame}; size {dimensions}; dirty rows {}; \
             input {:?}; queue {}; input→paint submission \
             {input_to_paint_submission} (not presentation); {sink}",
                self.diagnostics.dirty_rows,
                self.diagnostics.last_input_outcome,
                self.diagnostics.input_queue_depth,
            ))
            .truncate(),
        );
    }
}
