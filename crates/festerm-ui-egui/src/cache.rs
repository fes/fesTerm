use std::sync::Arc;

use festerm_core::{Attributes, Cell, CellWidth, Color, Dimensions, Terminal};

use crate::{
    geometry::{dimensions_from_viewport, CellMetrics, ViewSize},
    TerminalSnapshot,
};

/// A copied cell used by the presentation cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedCell {
    pub(crate) text: String,
    pub(crate) width: CellWidth,
    pub(crate) foreground: Color,
    pub(crate) background: Color,
    pub(crate) attributes: Attributes,
    pub(crate) hyperlink: Option<Arc<str>>,
}

impl RenderedCell {
    pub(crate) fn from_core(cell: &Cell) -> Self {
        Self {
            text: cell.text().to_owned(),
            width: cell.width(),
            foreground: cell.foreground(),
            background: cell.background(),
            attributes: cell.attributes(),
            hyperlink: cell.hyperlink_target(),
        }
    }

    pub(crate) fn blank() -> Self {
        Self {
            text: " ".to_owned(),
            width: CellWidth::Single,
            foreground: Color::Default,
            background: Color::Default,
            attributes: Attributes::NONE,
            hyperlink: None,
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
    viewport_offset_rows: usize,
    rows: Vec<CachedRow>,
}

impl TerminalRenderCache {
    pub fn update(
        &mut self,
        snapshot: TerminalSnapshot<'_>,
        dirty_rows: &[usize],
    ) -> RenderCacheUpdate {
        let dimensions = snapshot.dimensions();
        let full_refresh = self.dimensions != Some(dimensions)
            || self.viewport_offset_rows != snapshot.viewport_offset_rows();
        if full_refresh {
            self.dimensions = Some(dimensions);
            self.viewport_offset_rows = snapshot.viewport_offset_rows();
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
                    snapshot
                        .cell(column, *row)
                        .map_or_else(RenderedCell::blank, RenderedCell::from_core)
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

    pub(crate) fn apply_viewport(
        &mut self,
        terminal: &mut Terminal,
        available: ViewSize,
        cell: CellMetrics,
    ) -> ResizeOutcome {
        dimensions_from_viewport(available, cell).map_or(ResizeOutcome::Unchanged, |dimensions| {
            self.apply(terminal, dimensions)
        })
    }
}
