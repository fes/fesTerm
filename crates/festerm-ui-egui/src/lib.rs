//! `egui` presentation for the GUI-independent terminal core.
//!
//! This crate owns points, fonts, glyph layout, selection presentation, and
//! native-window input translation. Terminal protocol state and input encoding
//! remain in `festerm-core`.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use egui::{
    text::{LayoutJob, TextFormat},
    Color32, FontFamily, FontId, Label, Pos2, Rect, Response, Sense, Stroke, TextStyle, Ui, Vec2,
};
use festerm_core::{
    Attributes, Cell, CellWidth, Color, Cursor, CursorStyle, Dimensions, FocusEvent, InputEvent,
    InputEventOutcome, Key, Modifiers, MouseButton, MouseEvent, MouseEventKind, MouseWheel, Screen,
    Terminal, TerminalModes, MAX_CELL_COUNT,
};

const DEFAULT_FOREGROUND: Color32 = Color32::from_rgb(220, 220, 220);
const DEFAULT_BACKGROUND: Color32 = Color32::from_rgb(24, 24, 24);
const SELECTION_BACKGROUND: Color32 = Color32::from_rgb(52, 91, 135);
const GLYPH_CACHE_CAPACITY: usize = 4_096;

fn grid_view_size(available: Vec2, reserved_footer_height: f32) -> ViewSize {
    ViewSize {
        width: available.x,
        height: (available.y - reserved_footer_height).max(0.0),
    }
}

/// A read-only, renderer-facing view of the currently visible terminal grid.
///
/// It borrows core state and therefore cannot outlive or mutate the terminal.
/// The renderer copies only rows announced as dirty into its presentation
/// cache; it does not clone a complete core grid per GUI frame.
#[derive(Clone, Copy)]
pub struct TerminalSnapshot<'a> {
    screen: &'a Screen,
    cursor: Cursor,
    cursor_style: CursorStyle,
    modes: TerminalModes,
}

impl<'a> TerminalSnapshot<'a> {
    pub fn from_terminal(terminal: &'a Terminal) -> Self {
        Self {
            screen: terminal.screen(),
            cursor: terminal.cursor(),
            cursor_style: terminal.cursor_style(),
            modes: terminal.modes(),
        }
    }

    pub const fn dimensions(self) -> Dimensions {
        self.screen.dimensions()
    }

    pub const fn cursor(self) -> Cursor {
        self.cursor
    }

    pub const fn cursor_style(self) -> CursorStyle {
        self.cursor_style
    }

    pub const fn modes(self) -> TerminalModes {
        self.modes
    }

    /// Returns a borrowed core cell, preserving width-two/continuation roles.
    pub fn cell(self, column: usize, row: usize) -> Option<&'a Cell> {
        self.screen.cell_ref(column, row)
    }
}

/// The measured point-space size of one terminal cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
}

impl CellMetrics {
    pub fn new(width: f32, height: f32) -> Option<Self> {
        (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
            .then_some(Self { width, height })
    }

    fn size_for(self, dimensions: Dimensions) -> Vec2 {
        Vec2::new(
            self.width * dimensions.columns() as f32,
            self.height * dimensions.rows() as f32,
        )
    }
}

/// A toolkit-independent width and height expressed in GUI points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewSize {
    pub width: f32,
    pub height: f32,
}

/// Converts available point-space extent to a valid core terminal size.
///
/// The core requires at least two columns and one row. Oversized views are
/// capped before `Dimensions::new`, so normal window resizing cannot request
/// invalid or excessive terminal allocations.
pub fn dimensions_from_points(available: ViewSize, cell: CellMetrics) -> Option<Dimensions> {
    if !available.width.is_finite()
        || !available.height.is_finite()
        || available.width < 0.0
        || available.height < 0.0
    {
        return None;
    }

    let rows = ((available.height / cell.height).floor() as usize).clamp(1, MAX_CELL_COUNT / 2);
    let columns = ((available.width / cell.width).floor() as usize)
        .max(2)
        .min(MAX_CELL_COUNT / rows);

    Dimensions::new(columns, rows).ok()
}

/// A zero-based cell coordinate.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CellPosition {
    pub column: usize,
    pub row: usize,
}

/// Maps a point into a visible cell, returning `None` outside the grid.
pub fn cell_from_point(
    grid_origin: Pos2,
    dimensions: Dimensions,
    cell: CellMetrics,
    point: Pos2,
) -> Option<CellPosition> {
    if !point.x.is_finite() || !point.y.is_finite() {
        return None;
    }

    let column = ((point.x - grid_origin.x) / cell.width).floor();
    let row = ((point.y - grid_origin.y) / cell.height).floor();
    if column < 0.0
        || row < 0.0
        || column >= dimensions.columns() as f32
        || row >= dimensions.rows() as f32
    {
        return None;
    }
    Some(CellPosition {
        column: column as usize,
        row: row as usize,
    })
}

fn clamped_cell_from_point(
    grid_origin: Pos2,
    dimensions: Dimensions,
    cell: CellMetrics,
    point: Pos2,
) -> Option<CellPosition> {
    if !point.x.is_finite() || !point.y.is_finite() {
        return None;
    }

    let column = ((point.x - grid_origin.x) / cell.width)
        .floor()
        .clamp(0.0, dimensions.columns().saturating_sub(1) as f32);
    let row = ((point.y - grid_origin.y) / cell.height)
        .floor()
        .clamp(0.0, dimensions.rows().saturating_sub(1) as f32);
    Some(CellPosition {
        column: column as usize,
        row: row as usize,
    })
}

/// An inclusive, row-major range of display cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CellRange {
    pub start: CellPosition,
    pub end: CellPosition,
}

impl CellRange {
    pub fn new(first: CellPosition, second: CellPosition) -> Self {
        if (first.row, first.column) <= (second.row, second.column) {
            Self {
                start: first,
                end: second,
            }
        } else {
            Self {
                start: second,
                end: first,
            }
        }
    }

    pub fn contains(self, position: CellPosition) -> bool {
        (position.row, position.column) >= (self.start.row, self.start.column)
            && (position.row, position.column) <= (self.end.row, self.end.column)
    }
}

/// Local UI selection state. It is deliberately separate from terminal modes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Selection {
    anchor: Option<CellPosition>,
    head: Option<CellPosition>,
    active: bool,
}

impl Selection {
    pub fn begin(&mut self, position: CellPosition) {
        self.anchor = Some(position);
        self.head = Some(position);
        self.active = true;
    }

    pub fn extend(&mut self, position: CellPosition) {
        if self.active {
            self.head = Some(position);
        }
    }

    pub fn finish(&mut self) {
        self.active = false;
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub const fn is_active(&self) -> bool {
        self.active
    }

    pub fn range(&self) -> Option<CellRange> {
        Some(CellRange::new(self.anchor?, self.head?))
    }
}

/// Moves a selection endpoint from a continuation to its width-two leading
/// cell, so local selection never copies only half a character.
pub fn normalize_selection_position(
    snapshot: TerminalSnapshot<'_>,
    mut position: CellPosition,
) -> Option<CellPosition> {
    if position.column >= snapshot.dimensions().columns()
        || position.row >= snapshot.dimensions().rows()
    {
        return None;
    }
    while position.column > 0
        && snapshot
            .cell(position.column, position.row)
            .is_some_and(Cell::is_continuation)
    {
        position.column -= 1;
    }
    Some(position)
}

/// Returns selected terminal text without interpreting terminal OSC clipboard
/// sequences. Width-two continuation cells do not add a second character.
pub fn selection_text(snapshot: TerminalSnapshot<'_>, selection: &Selection) -> Option<String> {
    let range = selection.range()?;
    let start = normalize_selection_position(snapshot, range.start)?;
    let end = normalize_selection_position(snapshot, range.end)?;
    let range = CellRange::new(start, end);
    let mut copied = String::new();

    for row in range.start.row..=range.end.row {
        if row != range.start.row {
            copied.push('\n');
        }
        let first = if row == range.start.row {
            range.start.column
        } else {
            0
        };
        let last = if row == range.end.row {
            range.end.column
        } else {
            snapshot.dimensions().columns() - 1
        };
        for column in first..=last {
            let cell = snapshot.cell(column, row)?;
            if !cell.is_continuation() {
                copied.push_str(cell.text());
            }
        }
    }
    Some(copied)
}

/// A copied cell used by the presentation cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedCell {
    text: String,
    width: CellWidth,
    foreground: Color,
    background: Color,
    attributes: Attributes,
    hyperlink: Option<Arc<str>>,
}

impl RenderedCell {
    fn from_core(cell: &Cell) -> Self {
        Self {
            text: cell.text().to_owned(),
            width: cell.width(),
            foreground: cell.foreground(),
            background: cell.background(),
            attributes: cell.attributes(),
            hyperlink: cell.hyperlink_target(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub const fn width(&self) -> CellWidth {
        self.width
    }

    pub const fn foreground(&self) -> Color {
        self.foreground
    }

    pub const fn background(&self) -> Color {
        self.background
    }

    pub const fn attributes(&self) -> Attributes {
        self.attributes
    }

    /// Returns a passive OSC 8 target for future explicit link activation.
    ///
    /// Rendering and selection never open a target automatically.
    pub fn hyperlink(&self) -> Option<&str> {
        self.hyperlink.as_deref()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct CachedRow {
    cells: Vec<RenderedCell>,
}

/// A changed-row presentation update.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderCacheUpdate {
    pub updated_rows: Vec<usize>,
    pub full_refresh: bool,
}

/// A row-cache for a terminal renderer.
///
/// The cache owns presentation copies only for rows reported dirty by the
/// core. Initial creation and a terminal-size change populate every visible
/// row, which is required for correctness.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TerminalRenderCache {
    dimensions: Option<Dimensions>,
    rows: Vec<CachedRow>,
}

impl TerminalRenderCache {
    pub fn update(
        &mut self,
        snapshot: TerminalSnapshot<'_>,
        dirty_rows: &[usize],
    ) -> RenderCacheUpdate {
        let dimensions = snapshot.dimensions();
        let full_refresh = self.dimensions != Some(dimensions);
        if full_refresh {
            self.dimensions = Some(dimensions);
            self.rows = vec![CachedRow::default(); dimensions.rows()];
        }

        let rows: Vec<usize> = if full_refresh {
            (0..dimensions.rows()).collect()
        } else {
            dirty_rows
                .iter()
                .copied()
                .filter(|row| *row < dimensions.rows())
                .collect()
        };
        for row in &rows {
            self.rows[*row].cells = (0..dimensions.columns())
                .map(|column| {
                    RenderedCell::from_core(
                        snapshot
                            .cell(column, *row)
                            .expect("terminal dimensions and screen must agree"),
                    )
                })
                .collect();
        }

        RenderCacheUpdate {
            updated_rows: rows,
            full_refresh,
        }
    }

    pub const fn dimensions(&self) -> Option<Dimensions> {
        self.dimensions
    }

    pub fn row(&self, row: usize) -> Option<&[RenderedCell]> {
        self.rows.get(row).map(|row| row.cells.as_slice())
    }
}

/// Applies a requested terminal size only when it has changed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResizeTracker {
    last_requested: Option<Dimensions>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResizeOutcome {
    Unchanged,
    Resized(Dimensions),
    Rejected,
}

impl ResizeTracker {
    pub fn apply(&mut self, terminal: &mut Terminal, dimensions: Dimensions) -> ResizeOutcome {
        if self.last_requested == Some(dimensions) && terminal.dimensions() == dimensions {
            return ResizeOutcome::Unchanged;
        }
        match terminal.resize(dimensions) {
            Ok(()) => {
                self.last_requested = Some(dimensions);
                ResizeOutcome::Resized(dimensions)
            }
            Err(_) => ResizeOutcome::Rejected,
        }
    }
}

/// The observable result of routing an event through the core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputRoute {
    pub outcome: InputEventOutcome,
    /// Input bytes waiting before this view drains them to its application sink.
    pub queue_depth: usize,
    pub delivered_bytes: usize,
}

/// Content-free input metadata exposed by an application-owned sink.
///
/// Counters saturate in implementations so a no-session demo can remain
/// observable without retaining user input or terminal protocol bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InputSinkDiagnostics {
    pub event_count: u64,
    pub byte_count: u64,
    pub last_outcome: Option<InputEventOutcome>,
    pub last_queue_depth: usize,
}

/// Application-owned destination for bytes encoded by the core.
pub trait EncodedInputSink {
    /// Delivers transient encoded bytes. Implementations must not retain them
    /// unless they are an active session transport.
    fn record_encoded_input(&mut self, bytes: &[u8]);

    /// Observes content-free routing metadata after every routed core event.
    fn observe_input_route(&mut self, _route: InputRoute) {}

    /// Receives a resize after the application-owned terminal core accepts it.
    ///
    /// The UI does not know about PTYs or sessions. An application can forward
    /// this cell-space size to its active backend without giving the backend
    /// access to the terminal core.
    fn record_terminal_resize(&mut self, _dimensions: Dimensions) {}

    /// Returns sink-owned, content-free diagnostics when available.
    fn input_diagnostics(&self) -> Option<InputSinkDiagnostics> {
        None
    }
}

/// Routes one typed UI event to the mode-aware core encoder and drains encoded
/// bytes into the application-owned sink. It intentionally performs no session
/// I/O itself.
pub fn route_input(
    terminal: &mut Terminal,
    event: InputEvent,
    sink: &mut impl EncodedInputSink,
) -> InputRoute {
    let outcome = terminal.handle_input(event);
    let queue_depth = terminal.queued_input().len();
    let bytes = terminal.drain_input();
    let delivered_bytes = bytes.len();
    if !bytes.is_empty() {
        sink.record_encoded_input(&bytes);
    }
    let route = InputRoute {
        outcome,
        queue_depth,
        delivered_bytes,
    };
    sink.observe_input_route(route);
    route
}

/// Routes pointer input while enforcing the core's selection-versus-terminal
/// mouse policy.
pub fn route_mouse_input(
    terminal: &mut Terminal,
    event: MouseEvent,
    selection: &mut Selection,
    sink: &mut impl EncodedInputSink,
) -> InputRoute {
    let position = CellPosition {
        column: event.column,
        row: event.row,
    };
    let route = route_input(terminal, InputEvent::Mouse(event), sink);
    match route.outcome {
        InputEventOutcome::SelectionAllowed => {
            let position =
                normalize_selection_position(TerminalSnapshot::from_terminal(terminal), position);
            match (event.kind, position) {
                (MouseEventKind::Press(MouseButton::Left), Some(position)) => {
                    selection.begin(position);
                }
                (MouseEventKind::Move { .. }, Some(position)) => selection.extend(position),
                (MouseEventKind::Release(MouseButton::Left), Some(position)) => {
                    selection.extend(position);
                    selection.finish();
                }
                (MouseEventKind::Release(MouseButton::Left), None) => selection.finish(),
                _ => {}
            }
        }
        InputEventOutcome::SelectionClaimed | InputEventOutcome::Encoded { .. } => {
            selection.clear();
        }
        InputEventOutcome::QueueOverflow | InputEventOutcome::Rejected => {}
    }
    route
}

/// Font configuration for the initial cell renderer.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSettings {
    pub size_points: f32,
}

impl Default for FontSettings {
    fn default() -> Self {
        Self { size_points: 14.0 }
    }
}

impl FontSettings {
    fn font_id(&self) -> FontId {
        FontId::new(self.size_points, FontFamily::Monospace)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct GlyphKey {
    text: String,
    foreground: Color32,
    attributes: u16,
    font_size_bits: u32,
    layout_width_bits: u32,
}

/// Cache laid-out cell glyphs. `egui` owns the underlying font atlas; this
/// cache avoids rebuilding a one-cell layout job for unchanged text styling.
#[derive(Default)]
struct GlyphCache {
    layouts: HashMap<GlyphKey, std::sync::Arc<egui::Galley>>,
}

impl GlyphCache {
    fn layout(
        &mut self,
        painter: &egui::Painter,
        cell: &RenderedCell,
        foreground: Color32,
        font: &FontSettings,
        layout_width: f32,
    ) -> std::sync::Arc<egui::Galley> {
        let key = GlyphKey {
            text: cell.text.clone(),
            foreground,
            attributes: cell.attributes.bits(),
            font_size_bits: font.size_points.to_bits(),
            layout_width_bits: layout_width.to_bits(),
        };
        if let Some(layout) = self.layouts.get(&key) {
            return layout.clone();
        }
        if self.layouts.len() >= GLYPH_CACHE_CAPACITY {
            self.layouts.clear();
        }

        let mut job = LayoutJob::default();
        job.wrap.max_width = layout_width;
        job.break_on_newline = false;
        job.append(
            &cell.text,
            0.0,
            TextFormat {
                font_id: font.font_id(),
                color: foreground,
                italics: cell.attributes.contains(Attributes::ITALIC),
                ..Default::default()
            },
        );
        let layout = painter.layout_job(job);
        self.layouts.insert(key, layout.clone());
        layout
    }
}

/// Diagnostics captured by the UI path without recording terminal content.
#[derive(Clone, Debug, Default)]
pub struct FrameDiagnostics {
    pub frame_time: Option<Duration>,
    /// Time from observed input routing through submitting grid paint shapes to
    /// egui. This does not measure GPU presentation or pixels on screen.
    pub input_to_paint_submission: Option<Duration>,
    pub calculated_dimensions: Option<Dimensions>,
    pub dirty_rows: usize,
    pub last_input_outcome: Option<InputEventOutcome>,
    pub input_queue_depth: usize,
    pub input_sink: Option<InputSinkDiagnostics>,
}

/// Tracks whether the terminal previously owned keyboard input, independent of
/// egui's response state for the current frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct KeyboardOwnership {
    terminal_owned: bool,
}

impl KeyboardOwnership {
    fn focus_in_if_needed(&mut self, terminal_has_keyboard_focus: bool) -> Option<FocusEvent> {
        if terminal_has_keyboard_focus && !self.terminal_owned {
            self.terminal_owned = true;
            Some(FocusEvent::In)
        } else {
            None
        }
    }

    fn focus_out_if_owned(&mut self) -> Option<FocusEvent> {
        if self.terminal_owned {
            self.terminal_owned = false;
            Some(FocusEvent::Out)
        } else {
            None
        }
    }
}

/// Pointer state maintained in event order across frames.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct TerminalPointerState {
    pressed: [bool; 3],
    captured: [bool; 3],
    last_position: Option<Pos2>,
    modifiers: Modifiers,
}

impl TerminalPointerState {
    fn button_index(button: MouseButton) -> usize {
        match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        }
    }

    fn press(&mut self, button: MouseButton, position: Pos2, capture: bool, modifiers: Modifiers) {
        let index = Self::button_index(button);
        self.pressed[index] = true;
        self.captured[index] = capture;
        self.last_position = Some(position);
        self.modifiers = modifiers;
    }

    fn release(&mut self, button: MouseButton, position: Pos2, modifiers: Modifiers) {
        let index = Self::button_index(button);
        self.pressed[index] = false;
        self.captured[index] = false;
        self.last_position = Some(position);
        self.modifiers = modifiers;
    }

    fn moved(&mut self, position: Pos2) {
        self.last_position = Some(position);
    }

    fn captured(&self) -> bool {
        self.captured
            .iter()
            .zip(self.pressed)
            .any(|(captured, pressed)| *captured && pressed)
    }

    fn button_captured(&self, button: MouseButton) -> bool {
        let index = Self::button_index(button);
        self.pressed[index] && self.captured[index]
    }

    fn held_button(&self) -> Option<MouseButton> {
        [MouseButton::Left, MouseButton::Middle, MouseButton::Right]
            .into_iter()
            .find(|button| self.pressed[Self::button_index(*button)])
    }
}

/// Measures through execution of `submit`, which is the point at which grid
/// shapes have been handed to egui rather than presented by the OS.
fn measure_input_to_paint_submission<T>(
    input_observed: Option<Instant>,
    submit: impl FnOnce() -> T,
) -> (T, Option<Duration>) {
    let submitted = submit();
    (submitted, input_observed.map(|started| started.elapsed()))
}

/// The initial `egui` terminal renderer and input adapter.
///
/// It renders one cached layout per leading display cell. This deliberately
/// preserves the one-cell mapping and does **not** claim ligature shaping;
/// ligature-capable run shaping remains Milestone 6 work.
#[derive(Default)]
pub struct TerminalView {
    fonts: FontSettings,
    cache: TerminalRenderCache,
    glyphs: GlyphCache,
    selection: Selection,
    resize: ResizeTracker,
    diagnostics: FrameDiagnostics,
    show_diagnostics: bool,
    keyboard: KeyboardOwnership,
    pointer: TerminalPointerState,
}

impl TerminalView {
    pub fn diagnostics(&self) -> &FrameDiagnostics {
        &self.diagnostics
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// Shows the terminal with the compatibility no-session status text.
    pub fn show(
        &mut self,
        context: &egui::Context,
        terminal: &mut Terminal,
        sink: &mut impl EncodedInputSink,
    ) {
        self.show_with_status(
            context,
            terminal,
            sink,
            "No session attached: encoded input is not sent or retained.",
            "No session diagnostics are available.",
        );
    }

    /// Shows the terminal and application-provided compact and detailed status.
    pub fn show_with_status(
        &mut self,
        context: &egui::Context,
        terminal: &mut Terminal,
        sink: &mut impl EncodedInputSink,
        session_status: &str,
        session_diagnostics: &str,
    ) {
        egui::CentralPanel::default().show(context, |ui| {
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
        });
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
        let calculated = dimensions_from_points(
            ViewSize {
                width: available.x,
                height: available.y,
            },
            metrics,
        );
        self.diagnostics.calculated_dimensions = calculated;
        if let Some(dimensions) = calculated {
            if matches!(
                self.resize.apply(terminal, dimensions),
                ResizeOutcome::Resized(_)
            ) {
                self.selection.clear();
                self.pointer = TerminalPointerState::default();
                sink.record_terminal_resize(dimensions);
            }
        }

        let dimensions = terminal.dimensions();
        let (grid_rect, response) =
            ui.allocate_exact_size(metrics.size_for(dimensions), Sense::click_and_drag());
        if response.clicked() {
            response.request_focus();
        }

        let layout = GridLayout {
            rect: grid_rect,
            dimensions,
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
                    ui.painter().with_clip_rect(grid_rect),
                    GridPaint {
                        layout,
                        snapshot,
                        cache: &self.cache,
                        selection: &self.selection,
                        fonts: &self.fonts,
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

#[derive(Clone, Copy)]
struct GridLayout {
    rect: Rect,
    dimensions: Dimensions,
    metrics: CellMetrics,
}

struct InputAdapterState<'a> {
    selection: &'a mut Selection,
    keyboard: &'a mut KeyboardOwnership,
    pointer: &'a mut TerminalPointerState,
}

fn route_egui_events(
    ui: &Ui,
    response: &Response,
    layout: GridLayout,
    terminal: &mut Terminal,
    input: InputAdapterState<'_>,
    sink: &mut impl EncodedInputSink,
) -> InputRoutingReports {
    let InputAdapterState {
        selection,
        keyboard,
        pointer,
    } = input;
    let keyboard_focused = response.has_focus() || response.clicked();
    let events = ui.input(|input| input.events.clone());
    let mut reports = InputRoutingReports::default();
    let mut focus_out_routed = false;

    for event in events {
        match event {
            egui::Event::Copy if keyboard_focused => {
                if let Some(text) =
                    selection_text(TerminalSnapshot::from_terminal(terminal), selection)
                {
                    ui.ctx().copy_text(text);
                }
            }
            egui::Event::Paste(text) if keyboard_focused => {
                record_terminal_input(
                    &mut reports,
                    selection,
                    Instant::now(),
                    route_input(terminal, InputEvent::Paste(text), sink),
                );
            }
            egui::Event::Text(text) if keyboard_focused => {
                for character in text.chars() {
                    record_terminal_input(
                        &mut reports,
                        selection,
                        Instant::now(),
                        route_input(terminal, InputEvent::Key(Key::Character(character)), sink),
                    );
                }
            }
            egui::Event::Key {
                key, pressed: true, ..
            } if keyboard_focused => {
                if let Some(key) = translate_key(key) {
                    record_terminal_input(
                        &mut reports,
                        selection,
                        Instant::now(),
                        route_input(terminal, InputEvent::Key(key), sink),
                    );
                }
            }
            egui::Event::WindowFocused(focused) => {
                let focus = if focused {
                    keyboard.focus_in_if_needed(keyboard_focused)
                } else {
                    keyboard.focus_out_if_owned()
                };
                focus_out_routed |= !focused && focus.is_some();
                if let Some(focus) = focus {
                    reports.record(
                        Instant::now(),
                        route_input(terminal, InputEvent::Focus(focus), sink),
                    );
                }
            }
            egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers,
            } => {
                if let Some(button) = translate_pointer_button(button) {
                    let observed = Instant::now();
                    if let Some(route) = route_pointer_event(
                        PointerInputEvent::Button {
                            position: pos,
                            button,
                            pressed,
                            modifiers: translate_modifiers(modifiers),
                        },
                        layout,
                        terminal,
                        selection,
                        pointer,
                        sink,
                    ) {
                        reports.record(observed, route);
                    }
                }
            }
            egui::Event::PointerMoved(position) => {
                let observed = Instant::now();
                if let Some(route) = route_pointer_event(
                    PointerInputEvent::Moved { position },
                    layout,
                    terminal,
                    selection,
                    pointer,
                    sink,
                ) {
                    reports.record(observed, route);
                }
            }
            egui::Event::MouseWheel {
                delta, modifiers, ..
            } => {
                let observed = Instant::now();
                if let Some(route) = route_pointer_event(
                    PointerInputEvent::Wheel {
                        delta_y: delta.y,
                        modifiers: translate_modifiers(modifiers),
                    },
                    layout,
                    terminal,
                    selection,
                    pointer,
                    sink,
                ) {
                    reports.record(observed, route);
                }
            }
            egui::Event::PointerGone => pointer.last_position = None,
            _ => {}
        }
    }

    if response.lost_focus() && !focus_out_routed {
        if let Some(focus) = keyboard.focus_out_if_owned() {
            reports.record(
                Instant::now(),
                route_input(terminal, InputEvent::Focus(focus), sink),
            );
        }
    } else if keyboard_focused {
        if let Some(focus) = keyboard.focus_in_if_needed(true) {
            reports.record(
                Instant::now(),
                route_input(terminal, InputEvent::Focus(focus), sink),
            );
        }
    }

    reports
}

fn record_terminal_input(
    reports: &mut InputRoutingReports,
    selection: &mut Selection,
    observed: Instant,
    route: InputRoute,
) {
    if matches!(route.outcome, InputEventOutcome::Encoded { .. }) {
        selection.clear();
    }
    reports.record(observed, route);
}

#[derive(Default)]
struct InputRoutingReports {
    routes: Vec<InputRoute>,
    input_observed: Option<Instant>,
}

impl InputRoutingReports {
    fn record(&mut self, observed: Instant, route: InputRoute) {
        self.input_observed.get_or_insert(observed);
        self.routes.push(route);
    }
}

#[derive(Clone, Copy, Debug)]
enum PointerInputEvent {
    Button {
        position: Pos2,
        button: MouseButton,
        pressed: bool,
        modifiers: Modifiers,
    },
    Moved {
        position: Pos2,
    },
    Wheel {
        delta_y: f32,
        modifiers: Modifiers,
    },
}

/// Routes ordered pointer events with event-time button/capture state.
///
/// A press beginning in the grid captures its button until release. Captured
/// movement and release outside the grid clamp to its nearest visible cell;
/// non-captured pointer input outside the grid is ignored.
fn route_pointer_event(
    event: PointerInputEvent,
    layout: GridLayout,
    terminal: &mut Terminal,
    selection: &mut Selection,
    pointer: &mut TerminalPointerState,
    sink: &mut impl EncodedInputSink,
) -> Option<InputRoute> {
    match event {
        PointerInputEvent::Button {
            position,
            button,
            pressed: true,
            modifiers,
        } => {
            let cell =
                cell_from_point(layout.rect.min, layout.dimensions, layout.metrics, position);
            pointer.press(button, position, cell.is_some(), modifiers);
            cell.map(|position| {
                route_mouse_input(
                    terminal,
                    MouseEvent {
                        kind: MouseEventKind::Press(button),
                        column: position.column,
                        row: position.row,
                        modifiers,
                    },
                    selection,
                    sink,
                )
            })
        }
        PointerInputEvent::Button {
            position,
            button,
            pressed: false,
            modifiers,
        } => {
            let cell =
                cell_from_point(layout.rect.min, layout.dimensions, layout.metrics, position)
                    .or_else(|| {
                        (pointer.button_captured(button) || selection.is_active()).then(|| {
                            clamped_cell_from_point(
                                layout.rect.min,
                                layout.dimensions,
                                layout.metrics,
                                position,
                            )
                        })?
                    });
            let route = cell.map(|position| {
                route_mouse_input(
                    terminal,
                    MouseEvent {
                        kind: MouseEventKind::Release(button),
                        column: position.column,
                        row: position.row,
                        modifiers,
                    },
                    selection,
                    sink,
                )
            });
            pointer.release(button, position, modifiers);
            route
        }
        PointerInputEvent::Moved { position } => {
            pointer.moved(position);
            cell_from_point(layout.rect.min, layout.dimensions, layout.metrics, position)
                .or_else(|| {
                    (pointer.captured() || selection.is_active()).then(|| {
                        clamped_cell_from_point(
                            layout.rect.min,
                            layout.dimensions,
                            layout.metrics,
                            position,
                        )
                    })?
                })
                .map(|position| {
                    route_mouse_input(
                        terminal,
                        MouseEvent {
                            kind: MouseEventKind::Move {
                                button: pointer.held_button(),
                            },
                            column: position.column,
                            row: position.row,
                            modifiers: pointer.modifiers,
                        },
                        selection,
                        sink,
                    )
                })
        }
        PointerInputEvent::Wheel { delta_y, modifiers } if delta_y != 0.0 => {
            pointer.last_position.and_then(|position| {
                cell_from_point(layout.rect.min, layout.dimensions, layout.metrics, position).map(
                    |position| {
                        route_mouse_input(
                            terminal,
                            MouseEvent {
                                kind: MouseEventKind::Wheel(if delta_y > 0.0 {
                                    MouseWheel::Up
                                } else {
                                    MouseWheel::Down
                                }),
                                column: position.column,
                                row: position.row,
                                modifiers,
                            },
                            selection,
                            sink,
                        )
                    },
                )
            })
        }
        PointerInputEvent::Wheel { .. } => None,
    }
}

fn translate_key(key: egui::Key) -> Option<Key> {
    match key {
        egui::Key::Enter => Some(Key::Enter),
        egui::Key::Tab => Some(Key::Tab),
        egui::Key::Backspace => Some(Key::Backspace),
        egui::Key::Escape => Some(Key::Escape),
        egui::Key::ArrowUp => Some(Key::ArrowUp),
        egui::Key::ArrowDown => Some(Key::ArrowDown),
        egui::Key::ArrowLeft => Some(Key::ArrowLeft),
        egui::Key::ArrowRight => Some(Key::ArrowRight),
        _ => None,
    }
}

fn translate_pointer_button(button: egui::PointerButton) -> Option<MouseButton> {
    match button {
        egui::PointerButton::Primary => Some(MouseButton::Left),
        egui::PointerButton::Middle => Some(MouseButton::Middle),
        egui::PointerButton::Secondary => Some(MouseButton::Right),
        egui::PointerButton::Extra1 | egui::PointerButton::Extra2 => None,
    }
}

fn translate_modifiers(modifiers: egui::Modifiers) -> Modifiers {
    let mut translated = Modifiers::NONE;
    if modifiers.shift {
        translated = translated.with(Modifiers::SHIFT);
    }
    if modifiers.alt {
        translated = translated.with(Modifiers::ALT);
    }
    if modifiers.ctrl || modifiers.mac_cmd {
        translated = translated.with(Modifiers::CONTROL);
    }
    translated
}

struct GridPaint<'a> {
    layout: GridLayout,
    snapshot: TerminalSnapshot<'a>,
    cache: &'a TerminalRenderCache,
    selection: &'a Selection,
    fonts: &'a FontSettings,
    focused: bool,
}

fn grid_cell_rect(layout: GridLayout, position: CellPosition, columns: usize) -> Rect {
    Rect::from_min_size(
        Pos2::new(
            layout.rect.left() + position.column as f32 * layout.metrics.width,
            layout.rect.top() + position.row as f32 * layout.metrics.height,
        ),
        Vec2::new(layout.metrics.width * columns as f32, layout.metrics.height),
    )
}

fn rendered_cell_columns(cell: &RenderedCell, dimensions: Dimensions, column: usize) -> usize {
    cell.width
        .columns()
        .max(1)
        .min(dimensions.columns().saturating_sub(column))
}

fn rendered_cell_is_selected(
    selection: Option<CellRange>,
    position: CellPosition,
    columns: usize,
) -> bool {
    selection.is_some_and(|range| {
        (0..columns).any(|offset| {
            range.contains(CellPosition {
                column: position.column + offset,
                row: position.row,
            })
        })
    })
}

fn cell_needs_background_paint(cell: &RenderedCell, selected: bool) -> bool {
    selected || cell.background != Color::Default || cell.attributes.contains(Attributes::INVERSE)
}

fn paint_grid(painter: egui::Painter, paint: GridPaint<'_>, glyphs: &mut GlyphCache) {
    let Some(dimensions) = paint.cache.dimensions() else {
        return;
    };
    let selection_range = paint.selection.range();
    painter.rect_filled(paint.layout.rect, 0.0, DEFAULT_BACKGROUND);
    for row in 0..dimensions.rows() {
        let Some(cells) = paint.cache.row(row) else {
            continue;
        };
        for (column, cell) in cells.iter().enumerate() {
            if cell.width == CellWidth::Continuation {
                continue;
            }
            let position = CellPosition { column, row };
            let columns = rendered_cell_columns(cell, dimensions, column);
            let rect = grid_cell_rect(paint.layout, position, columns);
            let (foreground, background) = cell_colors(cell);
            let selected = rendered_cell_is_selected(selection_range, position, columns);
            if cell_needs_background_paint(cell, selected) {
                painter.rect_filled(
                    rect,
                    0.0,
                    if selected {
                        SELECTION_BACKGROUND
                    } else {
                        background
                    },
                );
            }
            if cell.text.is_empty() {
                continue;
            }
            let galley = glyphs.layout(&painter, cell, foreground, paint.fonts, rect.width());
            let text_position = Pos2::new(
                rect.left(),
                rect.top() + ((paint.layout.metrics.height - galley.size().y) / 2.0).max(0.0),
            );
            painter.galley(text_position, galley, foreground);
            let double_underline = cell.attributes.contains(Attributes::DOUBLE_UNDERLINE);
            if cell.attributes.contains(Attributes::UNDERLINE) || double_underline {
                let underline_y = rect.bottom() - if double_underline { 3.0 } else { 2.0 };
                painter.line_segment(
                    [
                        Pos2::new(rect.left(), underline_y),
                        Pos2::new(rect.right(), underline_y),
                    ],
                    Stroke::new(1.0_f32, foreground),
                );
                if double_underline {
                    let underline_y = rect.bottom() - 1.0;
                    painter.line_segment(
                        [
                            Pos2::new(rect.left(), underline_y),
                            Pos2::new(rect.right(), underline_y),
                        ],
                        Stroke::new(1.0_f32, foreground),
                    );
                }
            }
            if cell.attributes.contains(Attributes::STRIKETHROUGH) {
                let strikethrough_y = rect.center().y;
                painter.line_segment(
                    [
                        Pos2::new(rect.left(), strikethrough_y),
                        Pos2::new(rect.right(), strikethrough_y),
                    ],
                    Stroke::new(1.0_f32, foreground),
                );
            }
        }
    }

    if paint.snapshot.modes().cursor_visible() {
        let cursor = paint.snapshot.cursor();
        if cursor.column() < dimensions.columns() && cursor.row() < dimensions.rows() {
            let cell_rect = Rect::from_min_size(
                Pos2::new(
                    paint.layout.rect.left() + cursor.column() as f32 * paint.layout.metrics.width,
                    paint.layout.rect.top() + cursor.row() as f32 * paint.layout.metrics.height,
                ),
                Vec2::new(paint.layout.metrics.width, paint.layout.metrics.height),
            );
            let color = if paint.focused {
                DEFAULT_FOREGROUND
            } else {
                DEFAULT_FOREGROUND.gamma_multiply(0.5)
            };
            paint_cursor(painter, cell_rect, paint.snapshot.cursor_style(), color);
        }
    }
}

fn paint_cursor(painter: egui::Painter, cell: Rect, style: CursorStyle, color: Color32) {
    match style {
        CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock => {
            painter.rect_stroke(cell.shrink(0.5), 0.0, Stroke::new(1.0_f32, color));
        }
        CursorStyle::BlinkingUnderline | CursorStyle::SteadyUnderline => {
            painter.line_segment(
                [
                    Pos2::new(cell.left(), cell.bottom() - 1.0),
                    Pos2::new(cell.right(), cell.bottom() - 1.0),
                ],
                Stroke::new(1.0_f32, color),
            );
        }
        CursorStyle::BlinkingBar | CursorStyle::SteadyBar => {
            painter.line_segment(
                [
                    Pos2::new(cell.left() + 0.5, cell.top()),
                    Pos2::new(cell.left() + 0.5, cell.bottom()),
                ],
                Stroke::new(1.0_f32, color),
            );
        }
    }
}

fn cell_colors(cell: &RenderedCell) -> (Color32, Color32) {
    let mut foreground = resolve_color(cell.foreground, DEFAULT_FOREGROUND);
    let mut background = resolve_color(cell.background, DEFAULT_BACKGROUND);
    if cell.attributes.contains(Attributes::INVERSE) {
        std::mem::swap(&mut foreground, &mut background);
    }
    if cell.attributes.contains(Attributes::CONCEALED) {
        foreground = background;
    }
    if cell.attributes.contains(Attributes::FAINT) {
        foreground = foreground.gamma_multiply(0.6);
    }
    // egui's bundled monospace font has no independent bold face. A modest
    // brightness adjustment is the available, geometry-preserving fallback.
    if cell.attributes.contains(Attributes::BOLD) {
        foreground = foreground.gamma_multiply(1.15);
    }
    (foreground, background)
}

/// Resolves terminal colors using the xterm-style ANSI/256-color palette.
pub fn resolve_color(color: Color, default: Color32) -> Color32 {
    match color {
        Color::Default => default,
        Color::Rgb { red, green, blue } => Color32::from_rgb(red, green, blue),
        Color::Indexed(index) if index < 16 => ansi_color(index),
        Color::Indexed(index @ 16..=231) => {
            let value = index - 16;
            let levels = [0, 95, 135, 175, 215, 255];
            Color32::from_rgb(
                levels[(value / 36) as usize],
                levels[((value / 6) % 6) as usize],
                levels[(value % 6) as usize],
            )
        }
        Color::Indexed(index) => {
            let level = 8 + (index - 232) * 10;
            Color32::from_gray(level)
        }
    }
}

fn ansi_color(index: u8) -> Color32 {
    const COLORS: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (255, 255, 255),
    ];
    let (red, green, blue) = COLORS[index as usize];
    Color32::from_rgb(red, green, blue)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use festerm_test_support::load_fixture;

    use super::*;

    #[derive(Default)]
    struct Sink(Vec<Vec<u8>>);

    impl EncodedInputSink for Sink {
        fn record_encoded_input(&mut self, bytes: &[u8]) {
            self.0.push(bytes.to_vec());
        }
    }

    fn terminal(columns: usize, rows: usize) -> Terminal {
        Terminal::new(Dimensions::new(columns, rows).expect("valid test size"))
            .expect("test terminal allocation")
    }

    fn grid_layout(columns: usize, rows: usize) -> GridLayout {
        GridLayout {
            rect: Rect::from_min_size(
                Pos2::new(5.0, 7.0),
                Vec2::new(columns as f32 * 10.0, rows as f32 * 20.0),
            ),
            dimensions: Dimensions::new(columns, rows).expect("valid test size"),
            metrics: CellMetrics::new(10.0, 20.0).expect("valid test cell metrics"),
        }
    }

    #[test]
    fn point_dimensions_are_bounded_and_valid() {
        let cell = CellMetrics::new(8.0, 16.0).unwrap();
        assert_eq!(
            dimensions_from_points(
                ViewSize {
                    width: 1.0,
                    height: 1.0
                },
                cell
            ),
            Some(Dimensions::new(2, 1).unwrap())
        );
        let maximum = dimensions_from_points(
            ViewSize {
                width: f32::MAX,
                height: f32::MAX,
            },
            cell,
        )
        .unwrap();
        assert!(maximum.cell_count() <= MAX_CELL_COUNT);
        assert_eq!(
            dimensions_from_points(
                ViewSize {
                    width: f32::NAN,
                    height: 5.0
                },
                cell
            ),
            None
        );
    }

    #[test]
    fn grid_view_reserves_the_diagnostics_footer() {
        assert_eq!(
            grid_view_size(Vec2::new(800.0, 600.0), 18.0),
            ViewSize {
                width: 800.0,
                height: 582.0,
            }
        );
        assert_eq!(
            grid_view_size(Vec2::new(800.0, 12.0), 18.0),
            ViewSize {
                width: 800.0,
                height: 0.0,
            }
        );
    }

    #[test]
    fn point_mapping_uses_zero_based_cell_coordinates() {
        let dimensions = Dimensions::new(4, 2).unwrap();
        let cell = CellMetrics::new(10.0, 20.0).unwrap();
        assert_eq!(
            cell_from_point(Pos2::new(5.0, 7.0), dimensions, cell, Pos2::new(34.9, 46.9)),
            Some(CellPosition { column: 2, row: 1 })
        );
        assert_eq!(
            cell_from_point(Pos2::new(5.0, 7.0), dimensions, cell, Pos2::new(45.0, 7.0)),
            None
        );
    }

    #[test]
    fn selection_expands_continuations_and_copies_leading_text() {
        let mut terminal = terminal(8, 1);
        terminal.ingest("A界e".as_bytes());
        terminal.take_dirty_rows();
        let mut selection = Selection::default();
        let mut sink = Sink::default();

        let press = route_mouse_input(
            &mut terminal,
            MouseEvent {
                kind: MouseEventKind::Press(MouseButton::Left),
                column: 2,
                row: 0,
                modifiers: Modifiers::NONE,
            },
            &mut selection,
            &mut sink,
        );
        assert_eq!(press.outcome, InputEventOutcome::SelectionAllowed);
        let release = route_mouse_input(
            &mut terminal,
            MouseEvent {
                kind: MouseEventKind::Release(MouseButton::Left),
                column: 3,
                row: 0,
                modifiers: Modifiers::NONE,
            },
            &mut selection,
            &mut sink,
        );
        assert_eq!(release.outcome, InputEventOutcome::SelectionAllowed);
        assert_eq!(
            selection.range(),
            Some(CellRange::new(
                CellPosition { column: 1, row: 0 },
                CellPosition { column: 3, row: 0 }
            ))
        );
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &selection),
            Some("界e".to_owned())
        );
        assert!(sink.0.is_empty());
    }

    #[test]
    fn wide_cells_use_one_two_column_paint_and_selection_span() {
        let mut terminal = terminal(4, 1);
        terminal.ingest(b"\x1b[4;38;2;1;2;3;48;5;196m\xe7\x95\x8c");
        let dirty_rows = terminal.take_dirty_rows();
        let mut cache = TerminalRenderCache::default();
        cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);
        let cells = cache.row(0).expect("cached row");
        let leading = &cells[0];
        let continuation = &cells[1];

        assert_eq!(leading.width(), CellWidth::Double);
        assert_eq!(continuation.width(), CellWidth::Continuation);
        assert_eq!(leading.attributes(), continuation.attributes());
        assert_eq!(leading.foreground(), continuation.foreground());
        assert_eq!(leading.background(), continuation.background());

        let layout = grid_layout(4, 1);
        let rect = grid_cell_rect(layout, CellPosition { column: 0, row: 0 }, 2);
        assert_eq!(rect.min, Pos2::new(5.0, 7.0));
        assert_eq!(rect.size(), Vec2::new(20.0, 20.0));
        assert!(rendered_cell_is_selected(
            Some(CellRange::new(
                CellPosition { column: 1, row: 0 },
                CellPosition { column: 1, row: 0 },
            )),
            CellPosition { column: 0, row: 0 },
            rendered_cell_columns(leading, layout.dimensions, 0),
        ));
    }

    #[test]
    fn application_mouse_claim_prevents_local_selection() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"\x1b[?1000h");
        let mut selection = Selection::default();
        let mut sink = Sink::default();
        let route = route_mouse_input(
            &mut terminal,
            MouseEvent {
                kind: MouseEventKind::Press(MouseButton::Left),
                column: 1,
                row: 0,
                modifiers: Modifiers::NONE,
            },
            &mut selection,
            &mut sink,
        );

        assert_eq!(route.outcome, InputEventOutcome::Encoded { bytes: 6 });
        assert_eq!(selection.range(), None);
        assert_eq!(sink.0, vec![b"\x1b[M \"!".to_vec()]);
    }

    #[test]
    fn focus_out_routes_once_after_prior_terminal_keyboard_ownership() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"\x1b[?1004h");
        let mut keyboard = KeyboardOwnership {
            terminal_owned: true,
        };
        let mut sink = Sink::default();

        let first = keyboard
            .focus_out_if_owned()
            .map(|focus| route_input(&mut terminal, InputEvent::Focus(focus), &mut sink));
        let second = keyboard
            .focus_out_if_owned()
            .map(|focus| route_input(&mut terminal, InputEvent::Focus(focus), &mut sink));

        assert_eq!(
            first.map(|route| route.outcome),
            Some(InputEventOutcome::Encoded { bytes: 3 })
        );
        assert_eq!(second, None);
        assert_eq!(sink.0, vec![b"\x1b[O".to_vec()]);
    }

    #[test]
    fn ordered_drag_uses_button_state_before_same_frame_release() {
        let mut terminal = terminal(4, 2);
        terminal.ingest(b"\x1b[?1002h\x1b[?1006h");
        let mut selection = Selection::default();
        let mut pointer = TerminalPointerState::default();
        let mut sink = Sink::default();
        let layout = grid_layout(4, 2);

        for event in [
            PointerInputEvent::Button {
                position: Pos2::new(6.0, 8.0),
                button: MouseButton::Left,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            PointerInputEvent::Moved {
                position: Pos2::new(26.0, 8.0),
            },
            PointerInputEvent::Button {
                position: Pos2::new(26.0, 8.0),
                button: MouseButton::Left,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ] {
            assert!(route_pointer_event(
                event,
                layout,
                &mut terminal,
                &mut selection,
                &mut pointer,
                &mut sink,
            )
            .is_some());
        }

        assert_eq!(
            sink.0,
            vec![
                b"\x1b[<0;1;1M".to_vec(),
                b"\x1b[<32;3;1M".to_vec(),
                b"\x1b[<0;3;1m".to_vec(),
            ]
        );
    }

    #[test]
    fn application_mouse_release_outside_grid_is_captured_and_clamped() {
        let mut terminal = terminal(4, 2);
        terminal.ingest(b"\x1b[?1000h\x1b[?1006h");
        let mut selection = Selection::default();
        let mut pointer = TerminalPointerState::default();
        let mut sink = Sink::default();
        let layout = grid_layout(4, 2);

        assert!(route_pointer_event(
            PointerInputEvent::Button {
                position: Pos2::new(6.0, 8.0),
                button: MouseButton::Left,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            layout,
            &mut terminal,
            &mut selection,
            &mut pointer,
            &mut sink,
        )
        .is_some());
        assert!(route_pointer_event(
            PointerInputEvent::Button {
                position: Pos2::new(-50.0, 300.0),
                button: MouseButton::Left,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
            layout,
            &mut terminal,
            &mut selection,
            &mut pointer,
            &mut sink,
        )
        .is_some());

        assert_eq!(
            sink.0,
            vec![b"\x1b[<0;1;1M".to_vec(), b"\x1b[<0;1;2m".to_vec()]
        );
        assert_eq!(selection.range(), None);
    }

    #[test]
    fn local_selection_capture_clamps_an_outside_release() {
        let mut terminal = terminal(4, 2);
        terminal.ingest(b"ABCD");
        let mut selection = Selection::default();
        let mut pointer = TerminalPointerState::default();
        let mut sink = Sink::default();
        let layout = grid_layout(4, 2);

        for event in [
            PointerInputEvent::Button {
                position: Pos2::new(16.0, 8.0),
                button: MouseButton::Left,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            PointerInputEvent::Button {
                position: Pos2::new(200.0, 300.0),
                button: MouseButton::Left,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ] {
            assert_eq!(
                route_pointer_event(
                    event,
                    layout,
                    &mut terminal,
                    &mut selection,
                    &mut pointer,
                    &mut sink,
                )
                .map(|route| route.outcome),
                Some(InputEventOutcome::SelectionAllowed)
            );
        }

        assert!(!selection.is_active());
        assert_eq!(
            selection.range(),
            Some(CellRange::new(
                CellPosition { column: 1, row: 0 },
                CellPosition { column: 3, row: 1 },
            ))
        );
    }

    #[test]
    fn routing_uses_core_keyboard_modes_and_drains_to_sink() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"\x1b[?1h");
        let mut sink = Sink::default();

        let route = route_input(&mut terminal, InputEvent::Key(Key::ArrowUp), &mut sink);

        assert_eq!(route.outcome, InputEventOutcome::Encoded { bytes: 3 });
        assert_eq!(route.queue_depth, 3);
        assert_eq!(sink.0, vec![b"\x1bOA".to_vec()]);
        assert!(terminal.queued_input().is_empty());
    }

    #[test]
    fn accepted_typed_input_clears_local_selection() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"selected");
        let mut selection = Selection::default();
        selection.begin(CellPosition { column: 0, row: 0 });
        selection.extend(CellPosition { column: 3, row: 0 });
        selection.finish();
        let mut sink = Sink::default();
        let mut reports = InputRoutingReports::default();

        record_terminal_input(
            &mut reports,
            &mut selection,
            Instant::now(),
            route_input(
                &mut terminal,
                InputEvent::Key(Key::Character('x')),
                &mut sink,
            ),
        );

        assert_eq!(selection.range(), None);
        assert_eq!(sink.0, vec![b"x".to_vec()]);
    }

    #[test]
    fn routing_uses_core_paste_and_focus_modes() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"\x1b[?2004h\x1b[?1004h");
        let mut sink = Sink::default();

        assert_eq!(
            route_input(
                &mut terminal,
                InputEvent::Paste("paste".to_owned()),
                &mut sink
            )
            .outcome,
            InputEventOutcome::Encoded { bytes: 17 }
        );
        assert_eq!(
            route_input(&mut terminal, InputEvent::Focus(FocusEvent::In), &mut sink).outcome,
            InputEventOutcome::Encoded { bytes: 3 }
        );
        assert_eq!(
            sink.0,
            vec![b"\x1b[200~paste\x1b[201~".to_vec(), b"\x1b[I".to_vec()]
        );
    }

    #[test]
    fn input_latency_finishes_after_paint_submission_work() {
        let observed = Instant::now();
        let (_, elapsed) = measure_input_to_paint_submission(Some(observed), || {
            std::thread::sleep(Duration::from_millis(2));
        });

        assert!(
            elapsed.is_some_and(|duration| duration >= Duration::from_millis(2)),
            "the measurement must include the submitted grid paint work"
        );
        assert_eq!(measure_input_to_paint_submission::<()>(None, || ()).1, None);
    }

    #[test]
    fn cache_updates_dirty_rows_without_full_grid_copies() {
        let mut terminal = terminal(4, 2);
        terminal.take_dirty_rows();
        let mut cache = TerminalRenderCache::default();
        terminal.ingest(b"A");
        let dirty_rows = terminal.take_dirty_rows();

        let update = cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

        assert!(update.full_refresh);
        assert_eq!(update.updated_rows, vec![0, 1]);
        terminal.ingest(b"B");
        let dirty_rows = terminal.take_dirty_rows();
        let update = cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);
        assert!(!update.full_refresh);
        assert_eq!(update.updated_rows, vec![0]);
        assert_eq!(cache.row(0).unwrap()[1].text(), "B");
    }

    #[test]
    fn recorded_fixture_state_preserves_renderer_cell_structure() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/m3/unicode-cells.fixture");
        let fixture = load_fixture(&path).expect("fixture parses");
        let mut terminal = Terminal::new(fixture.dimensions).unwrap();
        terminal.ingest(&fixture.input);
        let dirty_rows = terminal.take_dirty_rows();
        let mut cache = TerminalRenderCache::default();

        cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

        let row = cache.row(0).unwrap();
        assert_eq!(row[1].text(), "界");
        assert_eq!(row[1].width(), CellWidth::Double);
        assert_eq!(row[2].width(), CellWidth::Continuation);
        assert_eq!(row[3].text(), "e\u{301}");
    }

    #[test]
    fn cache_preserves_passive_hyperlink_metadata() {
        let mut terminal = terminal(4, 1);
        terminal.ingest(b"\x1b]8;;https://example.com\x1b\\go\x1b]8;;\x1b\\");
        let dirty_rows = terminal.take_dirty_rows();
        let mut cache = TerminalRenderCache::default();

        cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

        assert_eq!(
            cache.row(0).unwrap()[0].hyperlink(),
            Some("https://example.com")
        );
        assert_eq!(cache.row(0).unwrap()[2].hyperlink(), None);
    }

    #[test]
    fn renderer_resolves_terminal_colors_and_basic_attributes() {
        assert_eq!(
            resolve_color(Color::Indexed(196), DEFAULT_BACKGROUND),
            Color32::from_rgb(255, 0, 0)
        );
        assert_eq!(
            resolve_color(
                Color::Rgb {
                    red: 1,
                    green: 2,
                    blue: 3
                },
                DEFAULT_BACKGROUND
            ),
            Color32::from_rgb(1, 2, 3)
        );

        let mut terminal = terminal(4, 1);
        terminal.ingest(b"\x1b[1;3;4;7;8;9;38;2;1;2;3;48;5;196mX");
        let dirty_rows = terminal.take_dirty_rows();
        let mut cache = TerminalRenderCache::default();
        cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);
        let cell = &cache.row(0).unwrap()[0];

        assert!(cell.attributes.contains(Attributes::BOLD));
        assert!(cell.attributes.contains(Attributes::ITALIC));
        assert!(cell.attributes.contains(Attributes::UNDERLINE));
        assert!(cell.attributes.contains(Attributes::INVERSE));
        assert!(cell.attributes.contains(Attributes::CONCEALED));
        assert!(cell.attributes.contains(Attributes::STRIKETHROUGH));
        let (foreground, background) = cell_colors(cell);
        assert_eq!(
            foreground, background,
            "conceal uses the effective background"
        );
        assert_eq!(background, Color32::from_rgb(1, 2, 3));
    }

    #[test]
    fn default_cells_share_the_grid_background_without_individual_paints() {
        let default = RenderedCell {
            text: String::new(),
            width: CellWidth::Single,
            foreground: Color::Default,
            background: Color::Default,
            attributes: Attributes::NONE,
            hyperlink: None,
        };
        let colored = RenderedCell {
            background: Color::Indexed(4),
            ..default.clone()
        };
        let inverse = RenderedCell {
            attributes: Attributes::INVERSE,
            ..default.clone()
        };

        assert!(!cell_needs_background_paint(&default, false));
        assert!(cell_needs_background_paint(&default, true));
        assert!(cell_needs_background_paint(&colored, false));
        assert!(cell_needs_background_paint(&inverse, false));
    }

    #[test]
    fn sustained_output_keeps_cache_input_and_resize_paths_usable() {
        let started = Instant::now();
        let mut terminal = terminal(120, 40);
        let mut cache = TerminalRenderCache::default();
        let initial_dirty_rows = terminal.take_dirty_rows();
        cache.update(
            TerminalSnapshot::from_terminal(&terminal),
            &initial_dirty_rows,
        );

        for _ in 0..1_000 {
            terminal.ingest(
                b"representative output line exercises terminal scrolling and dirty rows\r\n",
            );
            let dirty_rows = terminal.take_dirty_rows();
            cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);
        }

        let mut sink = Sink::default();
        assert_eq!(
            route_input(&mut terminal, InputEvent::Key(Key::ArrowDown), &mut sink).outcome,
            InputEventOutcome::Encoded { bytes: 3 }
        );
        let mut resize = ResizeTracker::default();
        assert_eq!(
            resize.apply(&mut terminal, Dimensions::new(100, 30).unwrap()),
            ResizeOutcome::Resized(Dimensions::new(100, 30).unwrap())
        );
        let dirty_rows = terminal.take_dirty_rows();
        let update = cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

        assert!(update.full_refresh);
        assert_eq!(cache.dimensions(), Some(Dimensions::new(100, 30).unwrap()));
        assert_eq!(sink.0, vec![b"\x1b[B".to_vec()]);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "representative output path became unexpectedly slow"
        );
    }

    #[test]
    fn repeated_resize_refreshes_cached_banner_and_prompt_cells() {
        let mut terminal = terminal(12, 4);
        terminal.ingest(b"Windows cmd\r\nCopyright\r\nC:\\Users\\fes>");
        let mut cache = TerminalRenderCache::default();
        let initial_dirty_rows = terminal.take_dirty_rows();
        cache.update(
            TerminalSnapshot::from_terminal(&terminal),
            &initial_dirty_rows,
        );
        let mut resize = ResizeTracker::default();

        for dimensions in [
            Dimensions::new(11, 4).unwrap(),
            Dimensions::new(12, 3).unwrap(),
            Dimensions::new(11, 3).unwrap(),
            Dimensions::new(12, 3).unwrap(),
            Dimensions::new(11, 4).unwrap(),
        ] {
            assert_eq!(
                resize.apply(&mut terminal, dimensions),
                ResizeOutcome::Resized(dimensions)
            );
            let dirty_rows = terminal.take_dirty_rows();
            let update = cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

            assert!(update.full_refresh);
            assert_eq!(cache.row(0).unwrap()[0].text(), "W");
            assert_eq!(cache.row(1).unwrap()[0].text(), "C");
            assert_eq!(cache.row(2).unwrap()[0].text(), "C");
        }
    }
}
