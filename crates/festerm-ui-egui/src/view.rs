use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use egui::{Align2, Popup, Rect, Sense, Stroke, Ui};
use festerm_core::{ContentPosition, InputEventOutcome, MouseTrackingMode, Terminal};

use crate::{
    cache::{ResizeOutcome, ResizeTracker, TerminalRenderCache},
    geometry::{cell_from_point, dimensions_from_viewport, viewport_layout, CellMetrics, ViewSize},
    input::{
        route_egui_events, EncodedInputSink, InputAdapterState, InputSinkDiagnostics,
        InputSuppression, KeyboardOwnership, TerminalPointerState, TERMINAL_RESIZE_DEBOUNCE,
    },
    renderer::{
        measure_input_to_paint_submission, paint_grid, FontSettings, GlyphCache, GridLayout,
        GridPaint,
    },
    selection::selection_text,
    selection::Selection,
    TerminalSnapshot, DEFAULT_BACKGROUND, DEFAULT_FOREGROUND,
};

use crate::fonts::DEFAULT_TERMINAL_FONT_SIZE;
const MIN_TERMINAL_FONT_SIZE: f32 = 8.0;
const MAX_TERMINAL_FONT_SIZE: f32 = 32.0;
const TERMINAL_ZOOM_STEP: f32 = 1.0;

/// Application-owned terminal capabilities that affect local viewport
/// commands without exposing a session backend to the presentation crate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalViewOptions {
    /// The current session can accept a paste through its ordered input path.
    pub paste_available: bool,
    /// Application-owned foreground UI permits terminal keyboard and pointer
    /// input this frame. Confirmation dialogs set this false so input cannot
    /// leak through their backdrop into a shell or full-screen TUI. This is
    /// a full blackout: history navigation, selection, and the context menu
    /// are also inert while it is false.
    pub terminal_input_enabled: bool,
    /// The session backing this view can still accept typed keystrokes
    /// (i.e. it has not exited, failed, stopped, or disconnected). Unlike
    /// `terminal_input_enabled`, this only suppresses keystrokes/paste
    /// delivered to the shell; scrollback navigation, selection, and Copy
    /// remain available so a read-only/dead session's history stays
    /// inspectable.
    pub keyboard_input_enabled: bool,
    /// Clipboard text is returned to the application policy layer instead of
    /// being encoded immediately. This lets the composition root apply paste
    /// confirmation without exposing session identity to this crate.
    pub defer_paste_to_application: bool,
    /// Scales how many scrollback rows one trackpad/wheel scroll step
    /// moves, on top of the fixed pixel-to-row mapping this view otherwise
    /// uses. `1.0` preserves fesTerm's original behavior; this crate stays
    /// unaware of `festerm-config`'s `ScrollSpeedPreference` clickstop names
    /// and only sees the resulting multiplier (feature request #67).
    pub scroll_speed_multiplier: f32,
}

impl Default for TerminalViewOptions {
    fn default() -> Self {
        Self {
            paste_available: true,
            terminal_input_enabled: true,
            keyboard_input_enabled: true,
            defer_paste_to_application: false,
            scroll_speed_multiplier: 1.0,
        }
    }
}

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
    /// Color emoji glyphs successfully submitted through the renderer-owned
    /// texture path during the latest frame.
    pub color_emoji_paints: usize,
    /// Color emoji paints that reused an existing text-and-size texture.
    pub color_emoji_cache_hits: usize,
    /// Color emoji paints that populated a new text-and-size texture.
    pub color_emoji_cache_misses: usize,
    /// Color rasterization attempts made after both positive and negative
    /// cache lookup missed.
    pub color_emoji_rasterization_attempts: usize,
    /// Rasterization attempts that fell back to monochrome text.
    pub color_emoji_rasterization_failures: usize,
    /// Failed text-and-size keys reused from the bounded negative cache.
    pub color_emoji_negative_cache_hits: usize,
    pub last_input_outcome: Option<InputEventOutcome>,
    pub input_queue_depth: usize,
    pub input_sink: Option<InputSinkDiagnostics>,
}

/// The initial `egui` terminal renderer and input adapter.
///
/// It renders one cached layout per leading display cell by default. An
/// explicit font policy may instead shape compatible ASCII cell runs while
/// immutable grid geometry continues to own interaction and clipping.
#[derive(Default)]
pub struct TerminalView {
    pub(crate) fonts: FontSettings,
    force_cell_run_shaping: bool,
    pub(crate) cache: TerminalRenderCache,
    pub(crate) glyphs: GlyphCache,
    pub(crate) selection: Selection,
    pub(crate) resize: ResizeTracker,
    pub(crate) diagnostics: FrameDiagnostics,
    pub(crate) keyboard: KeyboardOwnership,
    pub(crate) pointer: TerminalPointerState,
    /// Whether this view has already claimed keyboard focus once. A freshly
    /// started session should grab focus immediately so typing works without
    /// first clicking into the terminal, but only on its first frame -
    /// afterwards the user is free to click elsewhere (e.g. the launcher, a
    /// rename field) without this view stealing focus back every frame.
    has_requested_initial_focus: bool,
    /// Explicit OSC 8 target captured under the pointer when the local menu
    /// opens. It remains stable while the pointer moves through the popup.
    context_link: Option<Arc<str>>,
    secondary_gesture: SecondaryGestureOwnership,
    history: HistoryViewport,
    scrollbar_dragging: bool,
    pending_paste_requests: VecDeque<String>,
    /// Fractional scroll rows left over from the last wheel event after
    /// applying `scroll_speed_multiplier`, carried into the next event so a
    /// slow clickstop (e.g. "Very slow", well under `1.0`) actually slows
    /// scrolling down rather than being silently rounded back up to at
    /// least one row per event. Reset whenever the wheel direction flips so
    /// a reversed scroll doesn't inherit a stale carry from the opposite
    /// direction.
    scroll_fraction_carry: f32,
    /// Sign of the most recent wheel delta.y this carry was accumulated
    /// under (`1.0`, `-1.0`, or `0.0` before the first event), used to
    /// detect a direction reversal above.
    scroll_fraction_sign: f32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct HistoryViewport {
    offset_rows: usize,
    observed_history_rows: usize,
    unseen_output: bool,
}

impl HistoryViewport {
    fn sync(&mut self, terminal: &Terminal) {
        let rows = terminal.scrollback_stats().physical_rows();
        if self.offset_rows > 0 && rows > self.observed_history_rows {
            self.offset_rows = self
                .offset_rows
                .saturating_add(rows - self.observed_history_rows);
            self.unseen_output = true;
        }
        self.offset_rows = self.offset_rows.min(rows);
        self.observed_history_rows = rows;
    }

    fn reflowed(
        &mut self,
        previous_rows: usize,
        new_rows: usize,
        mapped_top_content_row: Option<usize>,
    ) {
        if self.offset_rows > 0 && previous_rows > 0 {
            self.offset_rows = if let Some(content_row) = mapped_top_content_row {
                new_rows.saturating_sub(content_row.min(new_rows))
            } else {
                let ratio = f64::from(u32::try_from(self.offset_rows).unwrap_or(u32::MAX))
                    / f64::from(u32::try_from(previous_rows).unwrap_or(u32::MAX).max(1));
                let rescaled = (ratio * new_rows as f64).round();
                if rescaled.is_finite() && rescaled >= 0.0 {
                    (rescaled as usize).min(new_rows)
                } else {
                    new_rows
                }
            };
        } else {
            self.offset_rows = self.offset_rows.min(new_rows);
        }
        self.observed_history_rows = new_rows;
    }

    fn scroll_up(&mut self, rows: usize) {
        self.offset_rows = self.offset_rows.saturating_add(rows);
    }

    fn scroll_down(&mut self, rows: usize) {
        self.offset_rows = self.offset_rows.saturating_sub(rows);
        if self.offset_rows == 0 {
            self.unseen_output = false;
        }
    }

    fn latest(&mut self) {
        self.offset_rows = 0;
        self.unseen_output = false;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScrollbarGeometry {
    hit_track: Rect,
    visual_track: Rect,
    thumb: Rect,
}

/// Scales `raw_rows` (the pixel/line/page-derived row count from a single
/// wheel event) by `multiplier`, carrying any fractional row leftover into
/// `carry` for the next call instead of rounding every event up to at least
/// one row. Without this, a clickstop like "Very slow" (well under `1.0`)
/// would be indistinguishable from `Normal` on devices that emit many small
/// wheel events per swipe (e.g. a trackpad), since each individual event's
/// scaled row count would otherwise get floored back up to `1`.
fn scaled_scroll_rows(raw_rows: usize, multiplier: f32, carry: &mut f32) -> usize {
    let scaled = (raw_rows as f32) * multiplier.max(0.0) + *carry;
    let rows = scaled.floor().max(0.0) as usize;
    *carry = scaled - scaled.floor();
    rows
}

fn scrollbar_geometry(
    viewport: Rect,
    visible_rows: usize,
    history_rows: usize,
    offset_rows: usize,
) -> Option<ScrollbarGeometry> {
    if history_rows == 0 || visible_rows == 0 || viewport.height() <= 0.0 {
        return None;
    }
    let total_rows = history_rows.saturating_add(visible_rows);
    let hit_track = Rect::from_min_max(
        egui::pos2(viewport.right() - 6.0, viewport.top()),
        viewport.right_bottom(),
    );
    let visual_track = Rect::from_min_max(
        egui::pos2(viewport.right() - 3.0, viewport.top()),
        viewport.right_bottom(),
    );
    let thumb_height = (viewport.height() * visible_rows as f32 / total_rows as f32)
        .max(24.0)
        .min(viewport.height());
    let travel = (viewport.height() - thumb_height).max(0.0);
    let top_content_row = history_rows.saturating_sub(offset_rows.min(history_rows));
    let progress = top_content_row as f32 / history_rows as f32;
    let thumb_top = viewport.top() + travel * progress;
    Some(ScrollbarGeometry {
        hit_track,
        visual_track,
        thumb: Rect::from_min_size(
            egui::pos2(visual_track.left(), thumb_top),
            egui::vec2(visual_track.width(), thumb_height),
        ),
    })
}

fn jump_to_latest_rect(viewport: Rect) -> Rect {
    Rect::from_min_size(
        egui::pos2(viewport.right() - 116.0, viewport.bottom() - 34.0),
        egui::vec2(108.0, 26.0),
    )
}

#[derive(Clone, Copy, Debug, Default)]
enum SecondaryGestureOwnership {
    #[default]
    None,
    Local(egui::Pos2),
    Terminal,
}

impl TerminalView {
    /// Current per-session terminal font size in logical points.
    pub const fn font_size_points(&self) -> f32 {
        self.fonts.size_points
    }

    /// Increases only this session's terminal presentation size.
    pub fn zoom_in(&mut self) -> bool {
        self.set_font_size_points(self.fonts.size_points + TERMINAL_ZOOM_STEP)
    }

    /// Decreases only this session's terminal presentation size.
    pub fn zoom_out(&mut self) -> bool {
        self.set_font_size_points(self.fonts.size_points - TERMINAL_ZOOM_STEP)
    }

    /// Restores only this session's terminal presentation size.
    pub fn reset_zoom(&mut self) -> bool {
        self.set_font_size_points(DEFAULT_TERMINAL_FONT_SIZE)
    }

    /// Text currently highlighted by this session's selection, if any. Used
    /// by the command palette's "Copy" entry so it can copy the same text a
    /// keyboard/OS copy shortcut would (`route_egui_events`'s
    /// `egui::Event::Copy` handling), without duplicating the selection
    /// logic.
    pub fn selected_text(&self, terminal: &Terminal) -> Option<String> {
        selection_text(
            TerminalSnapshot::from_terminal_viewport(terminal, self.history.offset_rows),
            &self.selection,
        )
    }

    fn set_font_size_points(&mut self, requested: f32) -> bool {
        let size = requested.clamp(MIN_TERMINAL_FONT_SIZE, MAX_TERMINAL_FONT_SIZE);
        if (size - self.fonts.size_points).abs() < f32::EPSILON {
            return false;
        }
        self.fonts.size_points = size;
        true
    }

    /// Applies the application-wide primary-family and shaping policy while
    /// preserving this session's independent zoom level.
    pub fn set_font_set(&mut self, font_set: crate::TerminalFontSet) {
        if self.fonts.font_set() == font_set {
            return;
        }
        self.fonts.set_font_set(font_set);
        self.glyphs.clear();
    }

    pub const fn color_emoji_enabled(&self) -> bool {
        self.fonts.font_set().color_emoji()
    }

    #[cfg(all(test, any(target_os = "windows", target_os = "linux")))]
    pub(crate) fn enable_cell_run_shaping_for_test(&mut self) {
        self.force_cell_run_shaping = true;
    }

    pub fn diagnostics(&self) -> &FrameDiagnostics {
        &self.diagnostics
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub const fn history_offset_rows(&self) -> usize {
        self.history.offset_rows
    }

    pub const fn follows_latest_output(&self) -> bool {
        self.history.offset_rows == 0
    }

    /// Marks this view as needing keyboard focus again on its next frame.
    /// Chip/tab activation switches which session's `show`/`show_in_ui` is
    /// called each frame (inactive tabs render nothing at all), so the
    /// one-shot "claim focus on first frame" behavior only fires once ever
    /// per session, not once per activation - clicking a chip to switch to
    /// an already-rendered-before session left keyboard focus stranded on
    /// the chrome row until the user clicked inside the terminal
    /// themselves. Callers should invoke this whenever a session's tab
    /// becomes the active tab (`docs/gui-design.md`).
    pub fn request_focus_on_next_frame(&mut self) {
        self.has_requested_initial_focus = false;
    }

    /// Takes every clipboard event deferred since the prior frame. Multiple
    /// events are significant to paste safety and must not be collapsed.
    pub fn take_paste_requests(&mut self) -> Vec<String> {
        self.pending_paste_requests.drain(..).collect()
    }

    /// Scrolls history so a terminal-search match becomes visible, used by
    /// the application-owned find bar (`docs/gui-design.md`
    /// "Terminal-content search"). `row` is a "document row": a retained
    /// scrollback row (oldest-first, `row < terminal.scrollback_stats()
    /// .physical_rows()`) or, past that, a live visible-screen row. A live
    /// row simply returns the viewport to the latest position; while the
    /// alternate screen is active, `row`'s scrollback component is ignored,
    /// matching `TerminalSnapshot`'s own forced-zero scrollback offset in
    /// that mode.
    pub fn reveal_document_row(&mut self, terminal: &Terminal, row: usize) {
        self.history.sync(terminal);
        let history_rows = if terminal.modes().alternate_screen() {
            0
        } else {
            terminal.scrollback_stats().physical_rows()
        };
        if row < history_rows {
            self.history.offset_rows = history_rows - row;
            self.history.unseen_output = false;
        } else {
            self.history.latest();
        }
    }

    /// Shows the terminal, filling all available space in `ui`. Detailed
    /// per-frame diagnostics are not rendered inline (`docs/gui-design.md`
    /// "Bottom status bar"); callers that want to surface them can read
    /// [`TerminalView::diagnostics`] or format [`TerminalView::diagnostics_summary`]
    /// into their own chrome (e.g. the application status bar).
    pub fn show(&mut self, ui: &mut Ui, terminal: &mut Terminal, sink: &mut impl EncodedInputSink) {
        self.show_with_options(ui, terminal, sink, TerminalViewOptions::default());
    }

    pub fn show_with_options(
        &mut self,
        ui: &mut Ui,
        terminal: &mut Terminal,
        sink: &mut impl EncodedInputSink,
        options: TerminalViewOptions,
    ) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(DEFAULT_BACKGROUND)
                    // Share the chrome row's own inset
                    // (`crate::chrome::CHROME_SIDE_INSET`, equal on every
                    // side) so the terminal viewport reserves the same
                    // handful of border pixels on all four edges that
                    // Windows Terminal/Terminus/the mockup do, instead of
                    // running flush to the window edges on top/bottom while
                    // only the sides were inset.
                    .inner_margin(egui::Margin::symmetric(
                        crate::chrome::CHROME_SIDE_INSET as i8,
                        crate::chrome::CHROME_SIDE_INSET as i8,
                    )),
            )
            .show(ui, |ui| {
                self.show_in_ui_with_options(ui, terminal, sink, options);
            });
    }

    /// Shows the cell grid inside an existing `egui` UI.
    pub fn show_in_ui(
        &mut self,
        ui: &mut Ui,
        terminal: &mut Terminal,
        sink: &mut impl EncodedInputSink,
    ) {
        self.show_in_ui_with_options(ui, terminal, sink, TerminalViewOptions::default());
    }

    pub fn show_in_ui_with_options(
        &mut self,
        ui: &mut Ui,
        terminal: &mut Terminal,
        sink: &mut impl EncodedInputSink,
        options: TerminalViewOptions,
    ) {
        if !crate::fonts::terminal_font_family_installed(ui.ctx(), self.fonts.font_set().family()) {
            let generation =
                crate::install_terminal_font_family(ui.ctx(), self.fonts.font_set().family());
            self.set_font_set(
                crate::TerminalFontSet::new(
                    self.fonts.font_set().family(),
                    self.fonts.font_set().ligatures(),
                    generation,
                )
                .with_color_emoji(self.fonts.font_set().color_emoji()),
            );
            // The named families become available after egui rebuilds its
            // atlas at the pass boundary. Direct TerminalView users can
            // bypass the application composition root, so yield safely.
            ui.ctx().request_repaint();
            return;
        }
        let frame_started = Instant::now();
        let glyph = ui.painter().layout_no_wrap(
            "M".to_owned(),
            self.fonts.regular_font_id(),
            DEFAULT_FOREGROUND,
        );
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
        let stats_before_resize = terminal.scrollback_stats();
        let history_rows_before_resize = stats_before_resize.physical_rows();
        let alternate_screen = terminal.modes().alternate_screen();
        let old_first_content_row = stats_before_resize.content_row_origin().saturating_add(
            history_rows_before_resize.saturating_sub(self.history.offset_rows) as u64,
        );
        let mut positions = Vec::new();
        let top_position_index = (!alternate_screen && self.history.offset_rows > 0).then(|| {
            positions.push(ContentPosition {
                column: 0,
                absolute_row: old_first_content_row,
            });
            positions.len() - 1
        });
        let selection_position_indices = (!alternate_screen)
            .then(|| self.selection.content_endpoints())
            .flatten()
            .map(|(anchor, head, active)| {
                let anchor_index = positions.len();
                positions.push(anchor);
                let head_index = positions.len();
                positions.push(head);
                (anchor_index, head_index, active)
            });
        let (resize_outcome, mapped_positions) = self
            .resize
            .apply_viewport_with_content_positions(terminal, viewport, metrics, &positions);
        if matches!(resize_outcome, ResizeOutcome::Resized(_)) {
            self.pointer = TerminalPointerState::default();
            let mapped_top_content_row = top_position_index
                .and_then(|index| mapped_positions.get(index).copied().flatten())
                .and_then(|position| {
                    let origin = terminal.scrollback_stats().content_row_origin();
                    position
                        .absolute_row
                        .checked_sub(origin)
                        .and_then(|row| usize::try_from(row).ok())
                });
            self.history.reflowed(
                history_rows_before_resize,
                terminal.scrollback_stats().physical_rows(),
                mapped_top_content_row,
            );
            if alternate_screen {
                self.selection.clamp_rectangular(terminal.dimensions());
            } else if let Some((anchor_index, head_index, active)) = selection_position_indices {
                let mapped = mapped_positions
                    .get(anchor_index)
                    .copied()
                    .flatten()
                    .zip(mapped_positions.get(head_index).copied().flatten());
                if let Some((anchor, head)) = mapped {
                    self.selection.remap_content(anchor, head, active);
                } else {
                    self.selection.clear();
                }
            }
            sink.record_terminal_resize(terminal.dimensions());
            // Guarantees the sink's debounced resize (see
            // `TERMINAL_RESIZE_DEBOUNCE`) actually gets flushed even if the
            // window then sits idle and nothing else would otherwise
            // schedule a later frame.
            ui.ctx().request_repaint_after(TERMINAL_RESIZE_DEBOUNCE);
        }

        let (viewport_rect, response) = ui.allocate_exact_size(available, Sense::click_and_drag());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Terminal viewport")
        });
        ui.painter()
            .rect_filled(viewport_rect, 0.0, DEFAULT_BACKGROUND);
        let vp_layout =
            viewport_layout(viewport_rect.min, viewport, metrics, terminal.dimensions());
        self.diagnostics.grid_rect = Some(vp_layout.grid);
        if options.terminal_input_enabled
            && (response.clicked() || !self.has_requested_initial_focus)
        {
            response.request_focus();
            self.has_requested_initial_focus = true;
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
        let mouse_reporting = !matches!(terminal.modes().mouse_tracking(), MouseTrackingMode::None);

        if !options.terminal_input_enabled {
            // The application owns a foreground modal. Preserve only focus
            // state and clipboard-delivery events (the latter invalidates an
            // already captured paste); history, selection, mouse reporting,
            // and keyboard routing must all remain inert behind the backdrop.
            ui.input_mut(|input| {
                input.events.retain(|event| {
                    matches!(event, egui::Event::WindowFocused(_) | egui::Event::Paste(_))
                });
            });
        }

        self.history.sync(terminal);
        if !terminal.modes().alternate_screen() {
            let page_rows = terminal.dimensions().rows().saturating_sub(1).max(1);
            let terminal_focused = response.has_focus();
            let terminal_hovered = response.hovered();
            let over_scrollbar = scrollbar_geometry(
                vp_layout.viewport,
                terminal.dimensions().rows(),
                terminal.scrollback_stats().physical_rows(),
                self.history.offset_rows,
            )
            .is_some_and(|geometry| {
                ui.input(|input| {
                    input
                        .pointer
                        .hover_pos()
                        .is_some_and(|position| geometry.hit_track.contains(position))
                })
            });
            let mut history_changed = false;
            ui.input_mut(|input| {
                input.events.retain(|event| match event {
                    egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                        ..
                    } if terminal_hovered
                        && (!mouse_reporting || modifiers.shift || over_scrollbar) =>
                    {
                        let raw_rows = match unit {
                            egui::MouseWheelUnit::Point => {
                                ((delta.y.abs() / metrics.height).ceil() as usize).max(1)
                            }
                            egui::MouseWheelUnit::Line => delta.y.abs().ceil() as usize,
                            egui::MouseWheelUnit::Page => {
                                (delta.y.abs().ceil() as usize).saturating_mul(page_rows)
                            }
                        };
                        // Accumulate the fractional row this event didn't
                        // quite earn (e.g. a 0.1x "Very slow" clickstop)
                        // into the next same-direction event instead of
                        // rounding every event up to at least one row,
                        // which would otherwise make slow clickstops
                        // indistinguishable from Normal on a trackpad's
                        // rapid small wheel events. A direction reversal
                        // drops any stale carry from the opposite way; a
                        // horizontal-only event (delta.y == 0.0) leaves the
                        // carry and its recorded direction untouched.
                        if delta.y != 0.0 {
                            let sign = delta.y.signum();
                            if sign != self.scroll_fraction_sign {
                                self.scroll_fraction_carry = 0.0;
                                self.scroll_fraction_sign = sign;
                            }
                        }
                        let rows = scaled_scroll_rows(
                            raw_rows,
                            options.scroll_speed_multiplier,
                            &mut self.scroll_fraction_carry,
                        );
                        if rows > 0 {
                            if delta.y > 0.0 {
                                self.history.scroll_up(rows);
                            } else if delta.y < 0.0 {
                                self.history.scroll_down(rows);
                            }
                            history_changed = true;
                        }
                        false
                    }
                    egui::Event::Key {
                        key: egui::Key::PageUp,
                        pressed: true,
                        modifiers,
                        ..
                    } if terminal_focused && modifiers.shift => {
                        self.history.scroll_up(page_rows);
                        history_changed = true;
                        false
                    }
                    egui::Event::Key {
                        key: egui::Key::PageDown,
                        pressed: true,
                        modifiers,
                        ..
                    } if terminal_focused && modifiers.shift => {
                        self.history.scroll_down(page_rows);
                        history_changed = true;
                        false
                    }
                    egui::Event::Key {
                        key: egui::Key::End,
                        pressed: true,
                        modifiers,
                        ..
                    } if terminal_focused && modifiers.ctrl => {
                        self.history.latest();
                        history_changed = true;
                        false
                    }
                    _ => true,
                });
            });
            if history_changed {
                self.history.sync(terminal);
                self.selection.clear();
                self.pointer = TerminalPointerState::default();
            }
        }

        let history_rows = terminal.scrollback_stats().physical_rows();
        let scrollbar = scrollbar_geometry(
            vp_layout.viewport,
            terminal.dimensions().rows(),
            history_rows,
            self.history.offset_rows,
        );
        let scrollbar_response = scrollbar.map(|geometry| {
            let response = ui.interact(
                geometry.hit_track,
                response.id.with("history_scrollbar"),
                Sense::click_and_drag(),
            );
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Slider,
                    true,
                    "Terminal history scrollbar",
                )
            });
            if response.drag_started()
                && response
                    .interact_pointer_pos()
                    .is_some_and(|position| geometry.thumb.expand(3.0).contains(position))
            {
                self.scrollbar_dragging = true;
            }
            if self.scrollbar_dragging && response.dragged() {
                if let Some(position) = response.interact_pointer_pos() {
                    let travel = geometry.visual_track.height() - geometry.thumb.height();
                    let progress = if travel <= 0.0 {
                        1.0
                    } else {
                        ((position.y - geometry.visual_track.top() - geometry.thumb.height() / 2.0)
                            / travel)
                            .clamp(0.0, 1.0)
                    };
                    let top_content_row = (progress * history_rows as f32).round() as usize;
                    self.history.offset_rows = history_rows.saturating_sub(top_content_row);
                    self.history.unseen_output &= self.history.offset_rows > 0;
                }
            } else if response.clicked() {
                if let Some(position) = response.interact_pointer_pos() {
                    let page = terminal.dimensions().rows().saturating_sub(1).max(1);
                    if position.y < geometry.thumb.top() {
                        self.history.scroll_up(page);
                    } else if position.y > geometry.thumb.bottom() {
                        self.history.scroll_down(page);
                    }
                    self.history.sync(terminal);
                }
            }
            if response.drag_stopped() {
                self.scrollbar_dragging = false;
            }
            response
        });
        let jump_rect = (self.history.offset_rows > 0 && self.history.unseen_output)
            .then(|| jump_to_latest_rect(vp_layout.viewport));
        let jump_response = jump_rect.map(|rect| {
            let response = ui.interact(rect, response.id.with("jump_to_latest"), Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Jump to latest")
            });
            if response.clicked() {
                self.history.latest();
                self.selection.clear();
                self.pointer = TerminalPointerState::default();
            }
            response
        });
        if jump_response.as_ref().is_some_and(egui::Response::clicked) {
            response.request_focus();
        }

        let scrollbar_hovered = scrollbar_response
            .as_ref()
            .is_some_and(egui::Response::hovered);
        let reserved_pointer_rects = [scrollbar.map(|geometry| geometry.hit_track), jump_rect];
        ui.input_mut(|input| {
            input.events.retain(|event| {
                !matches!(
                    event,
                    egui::Event::PointerButton { pos, .. } | egui::Event::PointerMoved(pos)
                        if reserved_pointer_rects
                            .iter()
                            .flatten()
                            .any(|rect| rect.contains(*pos))
                )
            });
        });

        // A TUI with mouse tracking owns ordinary right-click. Shift is the
        // stable local override; without mouse tracking, right-click is local
        // by default. Remove both press and release before terminal routing so
        // opening the menu can never emit half of a mouse protocol gesture.
        let mut local_context_release = None;
        ui.input_mut(|input| {
            input.events.retain(|event| {
                let egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers,
                } = event
                else {
                    return true;
                };
                if *pressed {
                    if !viewport_rect.contains(*pos) {
                        self.secondary_gesture = SecondaryGestureOwnership::None;
                        return true;
                    }
                    if !mouse_reporting || modifiers.shift {
                        self.secondary_gesture = SecondaryGestureOwnership::Local(*pos);
                        return false;
                    }
                    self.secondary_gesture = SecondaryGestureOwnership::Terminal;
                    return true;
                }
                match std::mem::take(&mut self.secondary_gesture) {
                    SecondaryGestureOwnership::Local(press_position) => {
                        local_context_release = Some(press_position);
                        false
                    }
                    SecondaryGestureOwnership::Terminal | SecondaryGestureOwnership::None => true,
                }
            });
        });

        if let Some(position) = local_context_release {
            let snapshot =
                TerminalSnapshot::from_terminal_viewport(terminal, self.history.offset_rows);
            self.context_link =
                cell_from_point(layout.rect.min, layout.dimensions, layout.metrics, position)
                    .and_then(|cell| snapshot.cell(cell.column, cell.row))
                    .and_then(|cell| cell.hyperlink_target());
        }
        let selected_text = selection_text(
            TerminalSnapshot::from_terminal_viewport(terminal, self.history.offset_rows),
            &self.selection,
        )
        .filter(|text| !text.is_empty());
        let menu_has_items = options.terminal_input_enabled
            && (self.context_link.is_some() || selected_text.is_some() || options.paste_available);
        let set_open = local_context_release.map(|_| egui::SetOpenCommand::Bool(menu_has_items));
        let menu = Popup::context_menu(&response)
            .open_memory(set_open)
            .show(|ui| {
                style_context_menu(ui);
                if let Some(text) = selected_text.clone() {
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(text);
                        ui.close();
                    }
                }
                if options.paste_available && ui.button("Paste").clicked() {
                    response.request_focus();
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                    ui.close();
                }
                // Find in terminal belongs immediately above this separator
                // once search exists. Do not render an inert placeholder.
                if let Some(link) = self.context_link.clone() {
                    if selected_text.is_some() || options.paste_available {
                        ui.separator();
                    }
                    if ui.button("Open link").clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(link.as_ref()));
                        ui.close();
                    }
                    if ui.button("Copy link").clicked() {
                        ui.ctx().copy_text(link.to_string());
                        ui.close();
                    }
                }
            });
        let context_menu_open = menu.is_some();
        if options.defer_paste_to_application {
            // `response.has_focus()`/`clicked()` each read the egui context
            // themselves (via `Context::input`/`Context::memory`), so they
            // must be evaluated *before* entering `input_mut` below: egui's
            // context lock is not reentrant, and calling them from inside
            // the `input_mut` closure deadlocks the UI thread the moment a
            // paste is delivered (e.g. Cmd+V).
            let should_defer =
                response.has_focus() || response.clicked() || !options.terminal_input_enabled;
            ui.input_mut(|input| {
                input.events.retain(|event| {
                    if let egui::Event::Paste(text) = event {
                        if should_defer {
                            self.pending_paste_requests.push_back(text.clone());
                            return false;
                        }
                    }
                    true
                });
            });
        }
        let reports = route_egui_events(
            ui,
            &response,
            layout,
            terminal,
            InputAdapterState {
                selection: &mut self.selection,
                keyboard: &mut self.keyboard,
                pointer: &mut self.pointer,
                viewport_offset_rows: self.history.offset_rows,
            },
            sink,
            InputSuppression {
                blackout: context_menu_open || !options.terminal_input_enabled,
                keystrokes: !options.keyboard_input_enabled,
            },
        );
        for report in reports.routes {
            self.diagnostics.last_input_outcome = Some(report.outcome);
            self.diagnostics.input_queue_depth = report.queue_depth;
        }

        let dirty_rows = terminal.take_dirty_rows();
        let snapshot = TerminalSnapshot::from_terminal_viewport(terminal, self.history.offset_rows);
        let update = self.cache.update(snapshot, &dirty_rows);
        self.diagnostics.dirty_rows = update.updated_rows.len();
        let (paint_stats, input_to_paint_submission) =
            measure_input_to_paint_submission(reports.input_observed, || {
                paint_grid(
                    ui.painter().with_clip_rect(vp_layout.viewport),
                    GridPaint {
                        layout,
                        snapshot,
                        cache: &self.cache,
                        selection: &self.selection,
                        fonts: &self.fonts,
                        shape_cell_runs: self.force_cell_run_shaping
                            || self.fonts.font_set().ligatures(),
                        focused: response.has_focus(),
                    },
                    &mut self.glyphs,
                )
            });
        self.diagnostics.color_emoji_paints = paint_stats.color_emoji_paints;
        self.diagnostics.color_emoji_cache_hits = paint_stats.color_emoji_cache_hits;
        self.diagnostics.color_emoji_cache_misses = paint_stats.color_emoji_cache_misses;
        self.diagnostics.color_emoji_rasterization_attempts =
            paint_stats.color_emoji_rasterization_attempts;
        self.diagnostics.color_emoji_rasterization_failures =
            paint_stats.color_emoji_rasterization_failures;
        self.diagnostics.color_emoji_negative_cache_hits =
            paint_stats.color_emoji_negative_cache_hits;
        if let Some(geometry) = scrollbar {
            let visible = self.history.offset_rows > 0
                || scrollbar_hovered
                || self.selection.is_active()
                || self.scrollbar_dragging;
            if visible {
                let painter = ui.painter().with_clip_rect(vp_layout.viewport);
                painter.rect_filled(
                    geometry.visual_track,
                    1.5,
                    crate::theme::SURFACE_TAB_INACTIVE.gamma_multiply(0.65),
                );
                painter.rect_filled(
                    scrollbar_geometry(
                        vp_layout.viewport,
                        terminal.dimensions().rows(),
                        history_rows,
                        self.history.offset_rows,
                    )
                    .expect("history rows still exist")
                    .thumb,
                    1.5,
                    if scrollbar_hovered || self.scrollbar_dragging {
                        crate::theme::BORDER_ACTIVE
                    } else {
                        crate::theme::TEXT_MUTED
                    },
                );
            }
        }
        if let (Some(rect), Some(response)) = (jump_rect, jump_response) {
            let visuals = if response.hovered() {
                ui.visuals().widgets.hovered
            } else {
                ui.visuals().widgets.inactive
            };
            let painter = ui.painter().with_clip_rect(vp_layout.viewport);
            painter.rect(
                rect,
                5.0,
                visuals.weak_bg_fill,
                Stroke::new(1.0, visuals.bg_stroke.color),
                egui::StrokeKind::Inside,
            );
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Jump to latest",
                egui::FontId::proportional(12.0),
                visuals.fg_stroke.color,
            );
        }
        self.diagnostics.input_to_paint_submission = input_to_paint_submission;
        self.diagnostics.input_sink = sink.input_diagnostics();
        self.diagnostics.frame_time = Some(frame_started.elapsed());
    }

    /// Grid dimensions of the most recently rendered frame, formatted as
    /// e.g. `"80×24"`, for display in the application status bar
    /// (`docs/gui-design.md` "Bottom status bar"). `None` before the first
    /// frame has been shown.
    pub fn dimensions_label(&self) -> Option<String> {
        self.diagnostics
            .calculated_dimensions
            .map(|size| format!("{}×{}", size.columns(), size.rows()))
    }

    /// Formats detailed per-frame diagnostics (frame time, dirty rows, emoji
    /// cache work, input queue depth, input-sink counters) as a single line,
    /// for optional display in the application status bar. `session_diagnostics` is a
    /// caller-supplied prefix describing session-level state (e.g. shell
    /// process status) that isn't tracked by this view.
    pub fn diagnostics_summary(&self, session_diagnostics: &str) -> String {
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
        format!(
            "{session_diagnostics}; frame {frame}; size {dimensions}; dirty rows {}; \
             emoji paints {}; cache hits {}; misses {}; raster attempts {}; failures {}; negative hits {}; \
             input {:?}; queue {}; input→paint submission \
             {input_to_paint_submission} (not presentation); {sink}",
            self.diagnostics.dirty_rows,
            self.diagnostics.color_emoji_paints,
            self.diagnostics.color_emoji_cache_hits,
            self.diagnostics.color_emoji_cache_misses,
            self.diagnostics.color_emoji_rasterization_attempts,
            self.diagnostics.color_emoji_rasterization_failures,
            self.diagnostics.color_emoji_negative_cache_hits,
            self.diagnostics.last_input_outcome,
            self.diagnostics.input_queue_depth,
        )
    }
}

fn style_context_menu(ui: &mut Ui) {
    ui.set_min_width(176.0);
    ui.spacing_mut().interact_size.y = 30.0;
    ui.spacing_mut().item_spacing.y = 2.0;
}

#[cfg(test)]
mod history_overlay_tests {
    use super::*;

    #[test]
    fn scrollbar_thumb_maps_oldest_and_latest_without_consuming_grid_width() {
        let viewport = Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(400.0, 200.0));
        let latest = scrollbar_geometry(viewport, 20, 80, 0).unwrap();
        let oldest = scrollbar_geometry(viewport, 20, 80, 80).unwrap();

        assert_eq!(latest.hit_track.right(), viewport.right());
        assert_eq!(latest.hit_track.width(), 6.0);
        assert_eq!(latest.visual_track.width(), 3.0);
        assert_eq!(latest.thumb.bottom(), viewport.bottom());
        assert_eq!(oldest.thumb.top(), viewport.top());
        assert_eq!(latest.thumb.height(), 40.0);
    }

    #[test]
    fn scrollbar_thumb_has_a_practical_minimum() {
        let viewport = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 100.0));
        let geometry = scrollbar_geometry(viewport, 10, 10_000, 5_000).unwrap();
        assert_eq!(geometry.thumb.height(), 24.0);
        assert!(scrollbar_geometry(viewport, 10, 0, 0).is_none());
    }

    #[test]
    fn scaled_scroll_rows_preserves_a_slow_clickstop_instead_of_flooring_every_event_to_one() {
        // Regression test: "Very slow" (0.1x) previously had no effect at
        // all on trackpad-style scrolling, because each wheel event's
        // single-event row count was rounded back up to a minimum of one
        // row regardless of the multiplier. With a fractional carry, ten
        // consecutive one-row events at 0.1x should move a total of one
        // row, not ten.
        let mut carry = 0.0f32;
        let mut total_rows = 0usize;
        for _ in 0..10 {
            total_rows += scaled_scroll_rows(1, 0.1, &mut carry);
        }
        assert_eq!(total_rows, 1);

        // The next nine events should still produce nothing (0.1 * 10 == 1
        // exactly lands the carry back at 0.0, restarting the cycle).
        for _ in 0..9 {
            assert_eq!(scaled_scroll_rows(1, 0.1, &mut carry), 0);
        }
        assert_eq!(scaled_scroll_rows(1, 0.1, &mut carry), 1);
    }

    #[test]
    fn scaled_scroll_rows_at_normal_multiplier_moves_every_event_immediately() {
        let mut carry = 0.0f32;
        assert_eq!(scaled_scroll_rows(3, 1.0, &mut carry), 3);
        assert_eq!(carry, 0.0);
        assert_eq!(scaled_scroll_rows(1, 1.0, &mut carry), 1);
    }

    #[test]
    fn scaled_scroll_rows_at_fast_multiplier_still_moves_every_event() {
        let mut carry = 0.0f32;
        assert_eq!(scaled_scroll_rows(1, 1.75, &mut carry), 1);
        assert!((carry - 0.75).abs() < 1e-6);
        assert_eq!(scaled_scroll_rows(1, 1.75, &mut carry), 2);
        assert!((carry - 0.5).abs() < 1e-6);
    }

    #[test]
    fn deferred_paste_requests_preserve_multiplicity_for_application_policy() {
        let mut view = TerminalView::default();
        view.pending_paste_requests
            .push_back("first\nsecond".into());
        view.pending_paste_requests.push_back("later".into());

        assert_eq!(
            view.take_paste_requests(),
            vec!["first\nsecond".to_owned(), "later".to_owned()]
        );
        assert!(view.take_paste_requests().is_empty());
    }

    #[test]
    fn reflowed_rescales_anchored_offset_and_clamps_to_new_history_size() {
        // #51: an anchored viewport offset must stay roughly the same
        // relative place in retained history after a resize reflows
        // physical rows, rather than jumping to an unrelated raw count.
        let mut history = HistoryViewport {
            offset_rows: 40,
            observed_history_rows: 100,
            unseen_output: false,
        };

        // Reflow to a wider terminal that halves the physical row count:
        // the anchored offset should rescale proportionally (40/100 -> ~20).
        history.reflowed(100, 50, None);
        assert_eq!(history.offset_rows, 20);
        assert_eq!(history.observed_history_rows, 50);

        // Reflow that shrinks retained history below the rescaled offset
        // must clamp rather than leave a stale, out-of-range position.
        history.reflowed(50, 5, None);
        assert!(history.offset_rows <= 5);
    }

    #[test]
    fn reflowed_uses_the_mapped_top_logical_position_when_available() {
        let mut history = HistoryViewport {
            offset_rows: 40,
            observed_history_rows: 100,
            unseen_output: false,
        };

        history.reflowed(100, 50, Some(12));

        assert_eq!(history.offset_rows, 38);
        assert_eq!(history.observed_history_rows, 50);
    }

    #[test]
    fn reflowed_leaves_following_viewport_untouched() {
        // A viewport that is not anchored (offset 0, i.e. "following
        // latest") must remain following after reflow instead of being
        // pulled into history.
        let mut history = HistoryViewport::default();
        history.reflowed(100, 40, None);
        assert_eq!(history.offset_rows, 0);
        assert_eq!(history.observed_history_rows, 40);
    }

    #[test]
    fn sync_clamps_anchored_offset_when_eviction_shrinks_retained_history() {
        // #51: when eviction removes the rows an anchored offset pointed
        // into, the viewport must clamp to the nearest retained position
        // (deterministic fallback) instead of tracking a now-invalid index.
        let dimensions = festerm_core::Dimensions::new(4, 2).expect("4x2 is a valid terminal size");
        let mut terminal = Terminal::with_scrollback_limit(dimensions, 1024)
            .expect("small scrollback limit is valid");
        for line in 1..=8 {
            terminal.ingest(format!("line{line}\r\n").as_bytes());
        }
        let rows_before_more_output = terminal.scrollback_stats().physical_rows();

        let mut history = HistoryViewport {
            offset_rows: rows_before_more_output,
            observed_history_rows: rows_before_more_output,
            unseen_output: false,
        };

        // Push enough additional output to force eviction of older rows.
        for line in 9..=64 {
            terminal.ingest(format!("line{line}\r\n").as_bytes());
        }
        assert!(
            terminal.scrollback_stats().evicted_lines() > 0,
            "the tiny scrollback limit must have forced eviction"
        );

        history.sync(&terminal);

        let rows_after = terminal.scrollback_stats().physical_rows();
        assert!(
            history.offset_rows <= rows_after,
            "an anchored offset must clamp to retained history after eviction"
        );
    }

    #[test]
    fn selected_text_returns_none_without_an_active_selection() {
        let dimensions = festerm_core::Dimensions::new(8, 1).expect("8x1 is a valid terminal size");
        let terminal = Terminal::new(dimensions).expect("valid terminal");
        let view = TerminalView::default();

        assert_eq!(view.selected_text(&terminal), None);
    }

    #[test]
    fn selected_text_returns_the_highlighted_text() {
        let dimensions = festerm_core::Dimensions::new(8, 1).expect("8x1 is a valid terminal size");
        let mut terminal = Terminal::new(dimensions).expect("valid terminal");
        terminal.ingest(b"selected");
        let mut view = TerminalView::default();
        view.selection
            .begin(crate::geometry::CellPosition { column: 0, row: 0 });
        view.selection
            .extend(crate::geometry::CellPosition { column: 3, row: 0 });
        view.selection.finish();

        assert_eq!(view.selected_text(&terminal), Some("sele".to_owned()));
    }
}
