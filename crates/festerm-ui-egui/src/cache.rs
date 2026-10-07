use std::sync::Arc;

use compact_str::CompactString;
use festerm_core::{Attributes, Cell, CellWidth, Color, ContentPosition, Dimensions, Terminal};

#[cfg(test)]
use crate::geometry::{dimensions_from_viewport, CellMetrics, ViewSize};
use crate::TerminalSnapshot;

/// A copied cell used by the presentation cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedCell {
    pub(crate) text: CompactString,
    pub(crate) width: CellWidth,
    pub(crate) foreground: Color,
    pub(crate) background: Color,
    pub(crate) attributes: Attributes,
    pub(crate) hyperlink: Option<Arc<str>>,
}

impl RenderedCell {
    pub(crate) fn from_core(cell: &Cell) -> Self {
        Self {
            text: CompactString::new(cell.text()),
            width: cell.width(),
            foreground: cell.foreground(),
            background: cell.background(),
            attributes: cell.attributes(),
            hyperlink: cell.hyperlink_target(),
        }
    }

    pub(crate) fn blank() -> Self {
        Self {
            text: CompactString::const_new(" "),
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
        let dimensions_changed = self.dimensions != Some(dimensions);
        let full_refresh =
            dimensions_changed || self.viewport_offset_rows != snapshot.viewport_offset_rows();
        if full_refresh {
            self.dimensions = Some(dimensions);
            self.viewport_offset_rows = snapshot.viewport_offset_rows();
            if dimensions_changed {
                self.rows = vec![CachedRow::default(); dimensions.rows()];
            }
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
                let cells = &mut self.rows[*row].cells;
                cells.clear();
                cells.extend((0..dimensions.columns()).map(|column| {
                    snapshot
                        .cell(column, *row)
                        .map_or_else(RenderedCell::blank, RenderedCell::from_core)
                }));
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
    fn rendered_cache_dirty_rows_reuse_their_owned_cell_backing() {
        let mut terminal = Terminal::new(Dimensions::new(80, 3).unwrap()).unwrap();
        terminal.ingest(b"\x1b[?25lfirst");
        let mut cache = TerminalRenderCache::default();
        cache.update(TerminalSnapshot::from_terminal(&terminal), &[]);
        let pointers: Vec<_> = cache.rows.iter().map(|row| row.cells.as_ptr()).collect();
        let capacities: Vec<_> = cache.rows.iter().map(|row| row.cells.capacity()).collect();
        for update in 0..128 {
            terminal.ingest(if update % 2 == 0 {
                b"\x1b[2;1Halpha"
            } else {
                b"\x1b[2;1Hbravo"
            });
            let changed = cache.update(TerminalSnapshot::from_terminal(&terminal), &[1]);
            assert_eq!(changed.updated_rows, [1]);
            assert!(!changed.full_refresh);
            for row in 0..3 {
                assert_eq!(cache.rows[row].cells.as_ptr(), pointers[row]);
                assert_eq!(cache.rows[row].cells.capacity(), capacities[row]);
            }
        }
    }

    #[test]
    fn rendered_cache_common_graphemes_and_blanks_have_no_heap_text_payload() {
        let mut terminal = Terminal::new(Dimensions::new(80, 3).unwrap()).unwrap();
        terminal.ingest("ASCII e\u{301} \u{1f642}".as_bytes());
        let mut cache = TerminalRenderCache::default();
        cache.update(TerminalSnapshot::from_terminal(&terminal), &[]);
        for row in &cache.rows {
            assert!(row.cells.iter().all(|cell| !cell.text.is_heap_allocated()));
        }
        assert!(!RenderedCell::blank().text.is_heap_allocated());
        let clone = cache.clone();
        assert_eq!(cache, clone);
        assert!(clone
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .all(|cell| !cell.text.is_heap_allocated()));
    }

    #[test]
    fn rendered_cache_viewport_refresh_reuses_same_dimension_row_backing() {
        let mut terminal = Terminal::new(Dimensions::new(80, 3).unwrap()).unwrap();
        for _ in 0..8 {
            terminal.ingest(b"history\r\n");
        }
        let mut cache = TerminalRenderCache::default();
        cache.update(TerminalSnapshot::from_terminal(&terminal), &[]);
        let outer = cache.rows.as_ptr();
        let pointers: Vec<_> = cache.rows.iter().map(|row| row.cells.as_ptr()).collect();
        for offset in [1, 0, 1, 0] {
            let previous = cache.row_revision(0).unwrap().clone();
            let snapshot = TerminalSnapshot::from_terminal_viewport(&terminal, offset);
            let update = cache.update(snapshot, &[]);
            assert!(update.full_refresh);
            assert_eq!(update.updated_rows, [0, 1, 2]);
            assert_ne!(cache.row_revision(0), Some(&previous));
            assert_eq!(cache.rows.as_ptr(), outer);
            for (row, pointer) in pointers.iter().enumerate() {
                assert_eq!(cache.rows[row].cells.as_ptr(), *pointer);
                for column in 0..80 {
                    assert_eq!(
                        cache.rows[row].cells[column].text(),
                        snapshot.cell(column, row).map_or(" ", Cell::text)
                    );
                }
            }
        }
    }

    #[test]
    fn rendered_cache_dimension_change_retires_exceptional_backing() {
        let mut terminal = Terminal::new(Dimensions::new(512, 16).unwrap()).unwrap();
        let mut cache = TerminalRenderCache::default();
        cache.update(TerminalSnapshot::from_terminal(&terminal), &[]);
        let small = Dimensions::new(8, 3).unwrap();
        terminal.resize(small).unwrap();
        let update = cache.update(TerminalSnapshot::from_terminal(&terminal), &[]);
        assert!(update.full_refresh);
        assert_eq!(cache.rows.len(), 3);
        assert_eq!(cache.rows.capacity(), 3);
        for row in &cache.rows {
            assert_eq!(row.cells.len(), 8);
            assert_eq!(row.cells.capacity(), 8);
        }
    }

    #[test]
    fn rendered_cache_replacing_long_text_does_not_keep_its_heap_buffer() {
        let mut terminal = Terminal::new(Dimensions::new(80, 3).unwrap()).unwrap();
        let long = format!("a{}", "\u{301}".repeat(20));
        terminal.ingest(long.as_bytes());
        let mut cache = TerminalRenderCache::default();
        cache.update(TerminalSnapshot::from_terminal(&terminal), &[]);
        assert_eq!(cache.rows[0].cells[0].text(), long);
        assert!(cache.rows[0].cells[0].text.is_heap_allocated());
        let pointer = cache.rows[0].cells.as_ptr();
        terminal.ingest(b"\x1b[1;1H\x1b[2Kx");
        cache.update(TerminalSnapshot::from_terminal(&terminal), &[0]);
        assert_eq!(cache.rows[0].cells.as_ptr(), pointer);
        assert_eq!(cache.rows[0].cells[0].text(), "x");
        assert!(!cache.rows[0].cells[0].text.is_heap_allocated());
    }

    #[test]
    fn rendered_cache_preserves_unicode_width_style_color_and_hyperlink_values() {
        let mut terminal = Terminal::new(Dimensions::new(80, 3).unwrap()).unwrap();
        terminal.ingest(b"\x1b[1;4;38;2;10;20;30;48;5;4m\x1b]8;;https://example.invalid/\x1b\\");
        terminal.ingest("e\u{301}\u{754c}\u{1f642}".as_bytes());
        terminal.ingest(b"\x1b]8;;\x1b\\\x1b[0m plain");
        let mut cache = TerminalRenderCache::default();
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        cache.update(snapshot, &[]);
        for row in 0..3 {
            for column in 0..80 {
                let rendered = &cache.rows[row].cells[column];
                let core = snapshot.cell(column, row).unwrap();
                assert_eq!(rendered.text(), core.text());
                assert_eq!(rendered.width(), core.width());
                assert_eq!(rendered.foreground(), core.foreground());
                assert_eq!(rendered.background(), core.background());
                assert_eq!(rendered.attributes(), core.attributes());
                assert_eq!(rendered.hyperlink(), core.hyperlink_target().as_deref());
            }
        }
    }

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
