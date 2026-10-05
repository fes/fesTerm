use std::sync::Arc;

use festerm_core::{Attributes, Cell, CellWidth, Color, ContentPosition, Dimensions, Terminal};

#[cfg(test)]
use crate::geometry::{dimensions_from_viewport, CellMetrics, ViewSize};
use crate::TerminalSnapshot;

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

#[derive(Clone, Debug, Default)]
struct CachedRow {
    cells: Vec<RenderedCell>,
    revision: RowRevision,
}

impl PartialEq for CachedRow {
    fn eq(&self, other: &Self) -> bool {
        self.cells == other.cells
    }
}

impl Eq for CachedRow {}

#[derive(Clone, Debug)]
pub(crate) struct RowRevision(Arc<()>);

impl Default for RowRevision {
    fn default() -> Self {
        Self(Arc::new(()))
    }
}

impl PartialEq for RowRevision {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for RowRevision {}

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
        if !rows.is_empty() {
            // Revisions are compared at the same row position, never across rows.
            let revision = RowRevision::default();
            for row in &rows {
                self.rows[*row].cells = (0..dimensions.columns())
                    .map(|column| {
                        snapshot
                            .cell(column, *row)
                            .map_or_else(RenderedCell::blank, RenderedCell::from_core)
                    })
                    .collect();
                self.rows[*row].revision = revision.clone();
            }
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

    pub(crate) fn row_revision(&self, row: usize) -> Option<&RowRevision> {
        self.rows.get(row).map(|row| &row.revision)
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
    pub(crate) fn request(&mut self, dimensions: Dimensions) -> bool {
        let changed = self.last_requested != Some(dimensions);
        self.last_requested = Some(dimensions);
        changed
    }

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

    #[cfg(test)]
    pub(crate) fn apply_viewport_with_content_positions(
        &mut self,
        terminal: &mut Terminal,
        available: ViewSize,
        cell: CellMetrics,
        positions: &[ContentPosition],
    ) -> (ResizeOutcome, Vec<Option<ContentPosition>>) {
        let Some(dimensions) = dimensions_from_viewport(available, cell) else {
            return (ResizeOutcome::Unchanged, Vec::new());
        };
        self.apply_dimensions_with_content_positions(terminal, dimensions, positions)
    }

    pub(crate) fn apply_dimensions_with_content_positions(
        &mut self,
        terminal: &mut Terminal,
        dimensions: Dimensions,
        positions: &[ContentPosition],
    ) -> (ResizeOutcome, Vec<Option<ContentPosition>>) {
        if self.last_requested == Some(dimensions) && terminal.dimensions() == dimensions {
            return (
                ResizeOutcome::Unchanged,
                positions.iter().copied().map(Some).collect(),
            );
        }
        match terminal.resize_with_content_positions(dimensions, positions) {
            Ok(positions) => {
                self.last_requested = Some(dimensions);
                (ResizeOutcome::Resized(dimensions), positions)
            }
            Err(_) => (ResizeOutcome::Rejected, Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_revisions_change_only_for_rebuilt_rows_without_changing_value_equality() {
        let mut terminal = Terminal::new(Dimensions::new(8, 3).expect("valid dimensions"))
            .expect("test allocation");
        terminal.ingest(b"\x1b[?25lone");
        let mut cache = TerminalRenderCache::default();
        cache.update(TerminalSnapshot::from_terminal(&terminal), &[]);
        let mut equal = cache.clone();
        equal.update(TerminalSnapshot::from_terminal(&terminal), &[0, 1, 2]);
        assert_eq!(cache, equal);
        for row in 0..3 {
            assert_ne!(cache.row_revision(row), equal.row_revision(row));
        }
        let revisions: Vec<_> = (0..3)
            .map(|row| cache.row_revision(row).expect("cached row").clone())
            .collect();
        terminal.ingest(b"\x1b[2;1Htwo");
        cache.update(TerminalSnapshot::from_terminal(&terminal), &[1]);
        assert_eq!(cache.row_revision(0), Some(&revisions[0]));
        assert_ne!(cache.row_revision(1), Some(&revisions[1]));
        assert_eq!(cache.row_revision(2), Some(&revisions[2]));
    }

    #[test]
    fn row_revisions_share_one_token_per_nonempty_update() {
        let mut terminal = Terminal::new(Dimensions::new(8, 3).expect("valid dimensions"))
            .expect("test allocation");
        terminal.ingest(b"\x1b[?25lone\x1b[2;1Htwo\x1b[3;1Hthree");
        let mut cache = TerminalRenderCache::default();
        cache.update(TerminalSnapshot::from_terminal(&terminal), &[]);
        let initial = cache.clone();
        assert_eq!(cache.row_revision(0), cache.row_revision(1));
        assert_eq!(cache.row_revision(0), cache.row_revision(2));

        let mut independent = TerminalRenderCache::default();
        independent.update(TerminalSnapshot::from_terminal(&terminal), &[]);
        assert_eq!(cache, independent);
        for row in 0..3 {
            assert_ne!(cache.row_revision(row), independent.row_revision(row));
        }

        terminal.ingest(b"\x1b[1;1HONE\x1b[3;1HTHREE");
        let update = cache.update(TerminalSnapshot::from_terminal(&terminal), &[0, 2, 0, 99]);
        assert_eq!(update.updated_rows, [0, 2, 0]);
        assert!(!update.full_refresh);
        assert_eq!(cache.row_revision(0), cache.row_revision(2));
        assert_ne!(cache.row_revision(0), initial.row_revision(0));
        assert_eq!(cache.row_revision(1), initial.row_revision(1));
        assert_ne!(cache.row_revision(0), cache.row_revision(1));
        assert_eq!(initial.row(0).expect("initial row")[0].text(), "o");
        assert_eq!(cache.row(0).expect("updated row")[0].text(), "O");

        let changed = cache.clone();
        for dirty in [&[][..], &[99][..]] {
            let update = cache.update(TerminalSnapshot::from_terminal(&terminal), dirty);
            assert!(update.updated_rows.is_empty());
            assert!(!update.full_refresh);
            for row in 0..3 {
                assert_eq!(cache.row_revision(row), changed.row_revision(row));
            }
        }

        terminal.ingest(b"\x1b[2;1HTWO");
        cache.update(TerminalSnapshot::from_terminal(&terminal), &[1]);
        assert_ne!(cache.row_revision(1), changed.row_revision(1));
        for row in [0, 2] {
            assert_eq!(cache.row_revision(row), changed.row_revision(row));
            assert_ne!(cache.row_revision(row), cache.row_revision(1));
        }
    }
}
