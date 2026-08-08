use egui::{Pos2, Rect, Vec2};
use festerm_core::{Dimensions, MAX_CELL_COUNT};

pub(crate) fn dimensions_from_viewport(
    available: ViewSize,
    cell: CellMetrics,
) -> Option<Dimensions> {
    if available.width < cell.width * 2.0 || available.height < cell.height {
        return None;
    }
    dimensions_from_points(available, cell)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ViewportLayout {
    pub(crate) viewport: Rect,
    pub(crate) grid: Rect,
    pub(crate) dimensions: Dimensions,
}

pub(crate) fn viewport_layout(
    origin: Pos2,
    available: ViewSize,
    metrics: CellMetrics,
    dimensions: Dimensions,
) -> ViewportLayout {
    let viewport = Rect::from_min_size(origin, Vec2::new(available.width, available.height));
    ViewportLayout {
        viewport,
        grid: Rect::from_min_size(origin, metrics.size_for(dimensions)),
        dimensions,
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

    pub(crate) fn size_for(self, dimensions: Dimensions) -> Vec2 {
        Vec2::new(
            self.width * dimensions.columns() as f32,
            self.height * dimensions.rows() as f32,
        )
    }
}

/// The immutable point-space mapping for one rendered terminal grid.
///
/// Terminal cells, not shaped glyph advances, own cursor, selection, and hit
/// testing coordinates. A future ligature or fallback renderer may paint a
/// glyph run across this geometry, but cannot redefine it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CellGeometry {
    origin: Pos2,
    dimensions: Dimensions,
    metrics: CellMetrics,
}

impl CellGeometry {
    pub(crate) const fn new(origin: Pos2, dimensions: Dimensions, metrics: CellMetrics) -> Self {
        Self {
            origin,
            dimensions,
            metrics,
        }
    }

    /// Returns the exact rectangle owned by a leading cell span.
    ///
    /// A width-two leading cell owns two adjacent physical columns. Its
    /// continuation owns no independent rectangle; cursor and hit testing
    /// continue to use one physical column at a time.
    pub(crate) fn cell_rect(self, position: CellPosition, columns: usize) -> Option<Rect> {
        if columns == 0
            || position.row >= self.dimensions.rows()
            || position.column >= self.dimensions.columns()
            || columns > self.dimensions.columns().saturating_sub(position.column)
        {
            return None;
        }
        Some(Rect::from_min_size(
            Pos2::new(
                self.origin.x + position.column as f32 * self.metrics.width,
                self.origin.y + position.row as f32 * self.metrics.height,
            ),
            Vec2::new(self.metrics.width * columns as f32, self.metrics.height),
        ))
    }

    pub(crate) fn hit_test(self, point: Pos2) -> Option<CellPosition> {
        cell_from_point(self.origin, self.dimensions, self.metrics, point)
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

pub(crate) fn clamped_cell_from_point(
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
