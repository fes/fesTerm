use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use egui::{
    text::{LayoutJob, TextFormat},
    Color32, FontFamily, FontId, Pos2, Rect, Stroke, StrokeKind, Vec2,
};
use festerm_core::{Attributes, Color, CursorStyle, Dimensions};

use crate::{
    cache::{RenderedCell, TerminalRenderCache},
    geometry::{CellGeometry, CellPosition, CellRange},
    selection::Selection,
    TerminalSnapshot, DEFAULT_BACKGROUND, DEFAULT_FOREGROUND, GLYPH_CACHE_CAPACITY,
    SELECTION_BACKGROUND,
};

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
    pub(crate) fn font_id(&self) -> FontId {
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
pub(crate) struct GlyphCache {
    layouts: HashMap<GlyphKey, Arc<egui::Galley>>,
}

impl GlyphCache {
    pub(crate) fn layout(
        &mut self,
        painter: &egui::Painter,
        text: &str,
        attributes: Attributes,
        foreground: Color32,
        font: &FontSettings,
        layout_width: f32,
    ) -> Arc<egui::Galley> {
        let key = GlyphKey {
            text: text.to_owned(),
            foreground,
            attributes: attributes.bits(),
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
            text,
            0.0,
            TextFormat {
                font_id: font.font_id(),
                color: foreground,
                italics: attributes.contains(Attributes::ITALIC),
                ..Default::default()
            },
        );
        let layout = painter.layout_job(job);
        self.layouts.insert(key, layout.clone());
        layout
    }
}

#[derive(Clone, Copy)]
pub(crate) struct GridLayout {
    pub(crate) rect: Rect,
    pub(crate) dimensions: Dimensions,
    pub(crate) metrics: crate::geometry::CellMetrics,
}

impl GridLayout {
    pub(crate) fn cell_geometry(self) -> CellGeometry {
        CellGeometry::new(self.rect.min, self.dimensions, self.metrics)
    }
}

pub(crate) struct GridPaint<'a> {
    pub(crate) layout: GridLayout,
    pub(crate) snapshot: TerminalSnapshot<'a>,
    pub(crate) cache: &'a TerminalRenderCache,
    pub(crate) selection: &'a Selection,
    pub(crate) fonts: &'a FontSettings,
    pub(crate) cell_run_shaping: bool,
    pub(crate) focused: bool,
}

pub(crate) fn grid_cell_rect(layout: GridLayout, position: CellPosition, columns: usize) -> Rect {
    layout
        .cell_geometry()
        .cell_rect(position, columns)
        .expect("renderer requests an in-bounds leading-cell span")
}

pub(crate) fn rendered_cell_columns(
    cell: &RenderedCell,
    dimensions: Dimensions,
    column: usize,
) -> usize {
    cell.width
        .columns()
        .max(1)
        .min(dimensions.columns().saturating_sub(column))
}

pub(crate) fn rendered_cell_is_selected(
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

pub(crate) fn cell_needs_background_paint(cell: &RenderedCell, selected: bool) -> bool {
    selected || cell.background != Color::Default || cell.attributes.contains(Attributes::INVERSE)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GlyphRun {
    position: CellPosition,
    columns: usize,
    text: String,
    foreground: Color32,
    attributes: Attributes,
    selected: bool,
    has_hyperlink: bool,
    single_width_only: bool,
}

impl GlyphRun {
    #[cfg(test)]
    pub(crate) const fn position(&self) -> CellPosition {
        self.position
    }

    #[cfg(test)]
    pub(crate) const fn columns(&self) -> usize {
        self.columns
    }

    #[cfg(test)]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    fn can_extend(&self, cell: &RenderedCell, foreground: Color32, selected: bool) -> bool {
        !self.selected
            && !selected
            && !self.has_hyperlink
            && self.single_width_only
            && cell.width == festerm_core::CellWidth::Single
            && self.foreground == foreground
            && self.attributes == cell.attributes
            && cell.hyperlink.is_none()
    }
}

/// Produces shaping runs without changing terminal-cell ownership.
///
/// Every run starts at a leading terminal cell and owns an explicit count of
/// physical columns. Wide cells, selections, style changes, and hyperlinks
/// create hard boundaries. The renderer may shape a run as one visual glyph
/// sequence, but cursor, selection, and hit testing keep using `CellGeometry`.
pub(crate) fn glyph_runs(
    cells: &[RenderedCell],
    row: usize,
    dimensions: Dimensions,
    selection: Option<CellRange>,
) -> Vec<GlyphRun> {
    let mut runs = Vec::new();
    for (column, cell) in cells.iter().enumerate() {
        if cell.width == festerm_core::CellWidth::Continuation {
            continue;
        }
        let position = CellPosition { column, row };
        let columns = rendered_cell_columns(cell, dimensions, column);
        let (foreground, _) = cell_colors(cell);
        let selected = rendered_cell_is_selected(selection, position, columns);
        let can_extend = runs.last().is_some_and(|run: &GlyphRun| {
            run.position.row == row
                && run.position.column + run.columns == column
                && run.can_extend(cell, foreground, selected)
        });
        if can_extend {
            let run = runs.last_mut().expect("run existence was checked");
            run.columns += columns;
            run.text.push_str(&cell.text);
        } else {
            runs.push(GlyphRun {
                position,
                columns,
                text: cell.text.clone(),
                foreground,
                attributes: cell.attributes,
                selected,
                has_hyperlink: cell.hyperlink.is_some(),
                single_width_only: cell.width == festerm_core::CellWidth::Single,
            });
        }
    }
    runs
}

pub(crate) fn paint_grid(painter: egui::Painter, paint: GridPaint<'_>, glyphs: &mut GlyphCache) {
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
            if cell.width == festerm_core::CellWidth::Continuation {
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
            if !paint.cell_run_shaping && !cell.text.is_empty() {
                let galley = glyphs.layout(
                    &painter,
                    &cell.text,
                    cell.attributes,
                    foreground,
                    paint.fonts,
                    rect.width(),
                );
                let text_position = Pos2::new(
                    rect.left(),
                    rect.top() + ((paint.layout.metrics.height - galley.size().y) / 2.0).max(0.0),
                );
                painter.galley(text_position, galley, foreground);
            }
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
        if paint.cell_run_shaping {
            for run in glyph_runs(cells, row, dimensions, selection_range) {
                if run.text.is_empty() {
                    continue;
                }
                let rect = grid_cell_rect(paint.layout, run.position, run.columns);
                let run_painter = painter.with_clip_rect(rect);
                let galley = glyphs.layout(
                    &run_painter,
                    &run.text,
                    run.attributes,
                    run.foreground,
                    paint.fonts,
                    rect.width(),
                );
                let text_position = Pos2::new(
                    rect.left(),
                    rect.top() + ((paint.layout.metrics.height - galley.size().y) / 2.0).max(0.0),
                );
                run_painter.galley(text_position, galley, run.foreground);
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
            // Until the running program explicitly requests a cursor shape
            // via DECSCUSR, render a vertical bar rather than the
            // spec-mandated blinking-block reset state: a full hollow box
            // reads as "unfocused" even when it isn't, and a bar is the
            // more typical default cursor appearance for a fresh session.
            // `cursor_style()` itself is untouched and still reports the
            // spec-accurate value to anything that queries it.
            let style = if paint.snapshot.cursor_style_requested_by_program() {
                paint.snapshot.cursor_style()
            } else {
                CursorStyle::SteadyBar
            };
            let focused_block = paint.focused
                && matches!(style, CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock);
            paint_cursor(painter.clone(), cell_rect, style, color, paint.focused);
            if focused_block {
                // A filled block would otherwise fully hide the character
                // underneath; redraw it inverted (background-colored) on
                // top, matching every other terminal emulator's filled
                // block-cursor convention.
                if let Some(cell) = paint.cache.row(cursor.row()).and_then(|row| {
                    row.get(cursor.column())
                        .filter(|cell| !cell.text.is_empty())
                }) {
                    let galley = glyphs.layout(
                        &painter,
                        &cell.text,
                        cell.attributes,
                        DEFAULT_BACKGROUND,
                        paint.fonts,
                        cell_rect.width(),
                    );
                    let text_position = Pos2::new(
                        cell_rect.left(),
                        cell_rect.top()
                            + ((paint.layout.metrics.height - galley.size().y) / 2.0).max(0.0),
                    );
                    painter.galley(text_position, galley, DEFAULT_BACKGROUND);
                }
            }
        }
    }
}

fn paint_cursor(
    painter: egui::Painter,
    cell: Rect,
    style: CursorStyle,
    color: Color32,
    focused: bool,
) {
    match style {
        CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock => {
            // Filled when focused (the conventional "this pane has the
            // keyboard" look shared by other terminal emulators), hollow
            // when not, so shape alone communicates focus state rather
            // than always drawing a hollow box regardless of focus.
            if focused {
                painter.rect_filled(cell, 0.0, color);
            } else {
                painter.rect_stroke(
                    cell.shrink(0.5),
                    0.0,
                    Stroke::new(1.0_f32, color),
                    StrokeKind::Inside,
                );
            }
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

pub(crate) fn cell_colors(cell: &RenderedCell) -> (Color32, Color32) {
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

/// Measures through execution of `submit`, which is the point at which grid
/// shapes have been handed to egui rather than presented by the OS.
pub(crate) fn measure_input_to_paint_submission<T>(
    input_observed: Option<Instant>,
    submit: impl FnOnce() -> T,
) -> (T, Option<Duration>) {
    let submitted = submit();
    (submitted, input_observed.map(|started| started.elapsed()))
}
