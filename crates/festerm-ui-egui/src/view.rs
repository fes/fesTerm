use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use egui::{Align2, Popup, Rect, Sense, Stroke, Ui};
use festerm_core::{InputEventOutcome, MouseTrackingMode, Terminal};

use crate::{
    cache::{ResizeOutcome, ResizeTracker, TerminalRenderCache},
    fonts::terminal_fonts_installed,
    geometry::{cell_from_point, dimensions_from_viewport, viewport_layout, CellMetrics, ViewSize},
    input::{
        route_egui_events, EncodedInputSink, InputAdapterState, InputSinkDiagnostics,
        KeyboardOwnership, TerminalPointerState,
    },
    renderer::{
        measure_input_to_paint_submission, paint_grid, FontSettings, GlyphCache, GridLayout,
        GridPaint,
    },
    selection::selection_text,
    selection::Selection,
    TerminalSnapshot, DEFAULT_BACKGROUND, DEFAULT_FOREGROUND,
};

const DEFAULT_TERMINAL_FONT_SIZE: f32 = 14.0;
const MIN_TERMINAL_FONT_SIZE: f32 = 8.0;
const MAX_TERMINAL_FONT_SIZE: f32 = 32.0;
const TERMINAL_ZOOM_STEP: f32 = 1.0;

/// Application-owned terminal capabilities that affect local viewport
/// commands without exposing a session backend to the presentation crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalViewOptions {
    /// The current session can accept a paste through its ordered input path.
    pub paste_available: bool,
    /// Application-owned foreground UI permits terminal keyboard and pointer
    /// input this frame. Confirmation dialogs set this false so input cannot
    /// leak through their backdrop into a shell or full-screen TUI.
    pub terminal_input_enabled: bool,
    /// Clipboard text is returned to the application policy layer instead of
    /// being encoded immediately. This lets the composition root apply paste
    /// confirmation without exposing session identity to this crate.
    pub defer_paste_to_application: bool,
}

impl Default for TerminalViewOptions {
    fn default() -> Self {
        Self {
            paste_available: true,
            terminal_input_enabled: true,
            defer_paste_to_application: false,
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

    /// Rescales an anchored offset proportionally after a resize reflows
    /// retained scrollback, so a history position stays roughly the same
    /// relative place in the (now differently sized) retained history
    /// instead of jumping to an unrelated raw row count. `previous_rows` and
    /// `new_rows` are the physical-row totals observed immediately before
    /// and after the resize that triggered reflow.
    fn reflowed(&mut self, previous_rows: usize, new_rows: usize) {
        if self.offset_rows > 0 && previous_rows > 0 {
            let ratio = f64::from(u32::try_from(self.offset_rows).unwrap_or(u32::MAX))
                / f64::from(u32::try_from(previous_rows).unwrap_or(u32::MAX).max(1));
            let rescaled = (ratio * new_rows as f64).round();
            self.offset_rows = if rescaled.is_finite() && rescaled >= 0.0 {
                (rescaled as usize).min(new_rows)
            } else {
                new_rows
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

    fn set_font_size_points(&mut self, requested: f32) -> bool {
        let size = requested.clamp(MIN_TERMINAL_FONT_SIZE, MAX_TERMINAL_FONT_SIZE);
        if (size - self.fonts.size_points).abs() < f32::EPSILON {
            return false;
        }
        self.fonts.size_points = size;
        true
    }

    #[cfg(all(test, any(target_os = "windows", target_os = "linux")))]
    pub(crate) fn enable_cell_run_shaping_for_test(&mut self) {
        self.cell_run_shaping = true;
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
        if !terminal_fonts_installed(ui.ctx()) {
            crate::install_terminal_fonts(ui.ctx());
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
        let history_rows_before_resize = terminal.scrollback_stats().physical_rows();
        if matches!(
            self.resize.apply_viewport(terminal, viewport, metrics),
            ResizeOutcome::Resized(_)
        ) {
            self.selection.clear();
            self.pointer = TerminalPointerState::default();
            self.history.reflowed(
                history_rows_before_resize,
                terminal.scrollback_stats().physical_rows(),
            );
            sink.record_terminal_resize(terminal.dimensions());
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
                        let rows = match unit {
                            egui::MouseWheelUnit::Point => {
                                ((delta.y.abs() / metrics.height).ceil() as usize).max(1)
                            }
                            egui::MouseWheelUnit::Line => delta.y.abs().ceil() as usize,
                            egui::MouseWheelUnit::Page => {
                                (delta.y.abs().ceil() as usize).saturating_mul(page_rows)
                            }
                        };
                        if delta.y > 0.0 {
                            self.history.scroll_up(rows);
                        } else if delta.y < 0.0 {
                            self.history.scroll_down(rows);
                        }
                        history_changed = delta.y != 0.0;
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
            ui.input_mut(|input| {
                input.events.retain(|event| {
                    if let egui::Event::Paste(text) = event {
                        if response.has_focus()
                            || response.clicked()
                            || !options.terminal_input_enabled
                        {
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
            },
            sink,
            context_menu_open || !options.terminal_input_enabled,
        );
        for report in reports.routes {
            self.diagnostics.last_input_outcome = Some(report.outcome);
            self.diagnostics.input_queue_depth = report.queue_depth;
        }

        let dirty_rows = terminal.take_dirty_rows();
        let snapshot = TerminalSnapshot::from_terminal_viewport(terminal, self.history.offset_rows);
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

    /// Formats detailed per-frame diagnostics (frame time, dirty rows, input
    /// queue depth, input-sink counters) as a single line, for optional
    /// display in the application status bar. `session_diagnostics` is a
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
             input {:?}; queue {}; input→paint submission \
             {input_to_paint_submission} (not presentation); {sink}",
            self.diagnostics.dirty_rows,
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
}
