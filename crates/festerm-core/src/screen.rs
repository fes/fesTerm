use compact_str::CompactString;

use crate::{
    cell::{blank_cell, Cell, CellWidth},
    terminal::TerminalError,
    Dimensions,
};

/// A visible grid and its redraw state.
///
/// Rows are stored in a ring buffer: `top` is the physical row index that
/// currently holds logical row 0. Scrolling the whole screen up or down by
/// `n` rows (by far the hottest path under sustained output - every
/// newline scrolls by one row once the cursor reaches the last row) is
/// therefore an O(n) rotation of `top` plus clearing the rows it reveals,
/// instead of an O(rows*columns) clone of the entire grid on every line.
#[derive(Clone, Debug)]
pub struct Screen {
    dimensions: Dimensions,
    /// Physical row index holding logical row 0.
    top: usize,
    cells: Vec<Cell>,
    occupied_cells: Vec<bool>,
    dirty_rows: Vec<bool>,
    soft_wrapped_rows: Vec<bool>,
    occupied_columns: Vec<usize>,
}

impl Eq for Screen {}

impl PartialEq for Screen {
    /// Compares logical content (as seen through row 0..rows), independent
    /// of the ring buffer's internal `top` rotation.
    fn eq(&self, other: &Self) -> bool {
        if self.dimensions != other.dimensions {
            return false;
        }
        (0..self.dimensions.rows()).all(|row| {
            let columns = self.dimensions.columns();
            let self_start = self.physical_row_start(row);
            let other_start = other.physical_row_start(row);
            self.cells[self_start..self_start + columns]
                == other.cells[other_start..other_start + columns]
                && self.occupied_cells[self_start..self_start + columns]
                    == other.occupied_cells[other_start..other_start + columns]
                && self.dirty_rows[row] == other.dirty_rows[row]
                && self.soft_wrapped_rows[self.physical_row(row)]
                    == other.soft_wrapped_rows[other.physical_row(row)]
                && self.occupied_columns[self.physical_row(row)]
                    == other.occupied_columns[other.physical_row(row)]
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScreenRow {
    pub(crate) cells: Vec<Cell>,
    pub(crate) soft_wrapped: bool,
}

impl Screen {
    pub fn new(dimensions: Dimensions) -> Result<Self, TerminalError> {
        let mut cells = Vec::new();
        cells
            .try_reserve_exact(dimensions.cell_count())
            .map_err(|error| TerminalError::allocation("screen cells", error))?;
        cells.resize(dimensions.cell_count(), blank_cell());
        let occupied_cells = vec![false; dimensions.cell_count()];

        let mut dirty_rows = Vec::new();
        dirty_rows
            .try_reserve_exact(dimensions.rows())
            .map_err(|error| TerminalError::allocation("screen dirty rows", error))?;
        dirty_rows.resize(dimensions.rows(), true);

        let mut soft_wrapped_rows = Vec::new();
        soft_wrapped_rows
            .try_reserve_exact(dimensions.rows())
            .map_err(|error| TerminalError::allocation("screen wrap metadata", error))?;
        soft_wrapped_rows.resize(dimensions.rows(), false);
        let occupied_columns = vec![0; dimensions.rows()];

        Ok(Self {
            dimensions,
            top: 0,
            cells,
            occupied_cells,
            dirty_rows,
            soft_wrapped_rows,
            occupied_columns,
        })
    }

    pub const fn dimensions(&self) -> Dimensions {
        self.dimensions
    }

    /// Maps a logical row (0 = the top visible row) to its physical row
    /// index in the ring buffer.
    fn physical_row(&self, logical_row: usize) -> usize {
        let rows = self.dimensions.rows();
        (self.top + logical_row) % rows
    }

    /// The physical cell-array start offset for a logical row.
    fn physical_row_start(&self, logical_row: usize) -> usize {
        self.physical_row(logical_row) * self.dimensions.columns()
    }

    pub fn cell(&self, column: usize, row: usize) -> Option<Cell> {
        self.cells.get(self.cell_index(column, row)?).cloned()
    }

    /// Borrows a cell without cloning the grid or its cell content.
    ///
    /// This is intended for renderer-facing read-only views. Mutation remains
    /// exclusively owned by the terminal state machine.
    pub fn cell_ref(&self, column: usize, row: usize) -> Option<&Cell> {
        self.cells.get(self.cell_index(column, row)?)
    }

    pub fn row_text(&self, row: usize) -> Option<String> {
        if row >= self.dimensions.rows() {
            return None;
        }

        let start = self.physical_row_start(row);
        let end = start + self.dimensions.columns();
        Some(self.cells[start..end].iter().map(Cell::character).collect())
    }

    pub fn is_row_dirty(&self, row: usize) -> Option<bool> {
        self.dirty_rows.get(row).copied()
    }

    pub fn take_dirty_rows(&mut self) -> Vec<usize> {
        self.dirty_rows
            .iter_mut()
            .enumerate()
            .filter_map(|(row, is_dirty)| {
                if *is_dirty {
                    *is_dirty = false;
                    Some(row)
                } else {
                    None
                }
            })
            .collect()
    }

    /// One past the last row with any occupied content, or `0` if the
    /// screen is entirely blank. Used to trim wholly-blank trailing rows
    /// before folding the screen into a [`Scrollback`](crate::history) for
    /// reflow, so they don't become phantom empty logical lines.
    pub(crate) fn occupied_row_count(&self) -> usize {
        (0..self.dimensions.rows())
            .rposition(|row| self.occupied_columns[self.physical_row(row)] > 0)
            .map_or(0, |row| row + 1)
    }

    /// Extracts every row as content trimmed to its occupied extent (no
    /// trailing padding), paired with whether it soft-wraps into the next
    /// row. Mirrors the extraction `scroll_up` performs when a row leaves
    /// the screen for retained history, so this screen's current content
    /// can be folded into a [`Scrollback`](crate::history) for a unified
    /// reflow across resize.
    pub(crate) fn to_rows(&self) -> Vec<ScreenRow> {
        (0..self.dimensions.rows())
            .map(|row| {
                let start = self.physical_row_start(row);
                let physical_row = self.physical_row(row);
                ScreenRow {
                    cells: self.cells[start..start + self.occupied_columns[physical_row]].to_vec(),
                    soft_wrapped: self.soft_wrapped_rows[physical_row],
                }
            })
            .collect()
    }

    /// Builds a screen of `dimensions` from previously reflowed rows (for
    /// example, the tail of a [`Scrollback::split_off_tail`] call), one row
    /// per entry from the top down. Rows are assumed to already fit within
    /// `dimensions.columns()`; any shorter row is padded with blank cells
    /// and any surplus rows beyond `dimensions.rows()` are ignored. Every
    /// row is marked dirty for the next redraw.
    pub(crate) fn from_rows(
        dimensions: Dimensions,
        rows: Vec<ScreenRow>,
    ) -> Result<Self, TerminalError> {
        let mut screen = Self::new(dimensions)?;
        let columns = dimensions.columns();
        for (row, screen_row) in rows.into_iter().take(dimensions.rows()).enumerate() {
            let start = row * columns;
            let len = screen_row.cells.len().min(columns);
            screen.cells[start..start + len].clone_from_slice(&screen_row.cells[..len]);
            screen.occupied_cells[start..start + len].fill(true);
            screen.soft_wrapped_rows[row] = screen_row.soft_wrapped;
            screen.occupied_columns[row] = len;
        }
        screen.repair_wide_cells();
        screen.mark_all_dirty();
        Ok(screen)
    }

    pub(crate) fn resized(&self, dimensions: Dimensions) -> Result<Self, TerminalError> {
        let mut resized = Self::new(dimensions)?;
        let preserved_rows = self.dimensions.rows().min(dimensions.rows());
        let preserved_columns = self.dimensions.columns().min(dimensions.columns());
        for row in 0..preserved_rows {
            let old_start = self.physical_row_start(row);
            let new_start = row * dimensions.columns();
            let old_physical_row = self.physical_row(row);
            resized.cells[new_start..new_start + preserved_columns]
                .clone_from_slice(&self.cells[old_start..old_start + preserved_columns]);
            resized.occupied_cells[new_start..new_start + preserved_columns]
                .copy_from_slice(&self.occupied_cells[old_start..old_start + preserved_columns]);
            resized.soft_wrapped_rows[row] = self.soft_wrapped_rows[old_physical_row]
                && preserved_columns == self.dimensions.columns();
            resized.occupied_columns[row] =
                self.occupied_columns[old_physical_row].min(preserved_columns);
        }
        resized.repair_wide_cells();
        Ok(resized)
    }

    pub(crate) fn replace_cell(&mut self, column: usize, row: usize, cell: Cell) {
        let index = self
            .cell_index(column, row)
            .expect("terminal cursor must remain within screen dimensions");
        self.cells[index] = cell;
        self.occupied_cells[index] = true;
        self.recompute_occupied(row);
        self.mark_dirty(row);
    }

    pub(crate) fn replace_cluster(&mut self, column: usize, row: usize, cell: Cell) {
        let index = self
            .cell_index(column, row)
            .expect("terminal cursor must remain within screen dimensions");
        let cluster_columns = cell.width.columns();
        debug_assert!(matches!(cluster_columns, 1 | 2));
        let foreground = cell.foreground;
        let background = cell.background;
        let attributes = cell.attributes;
        let hyperlink = cell.hyperlink.clone();
        self.cells[index] = cell;
        self.occupied_cells[index] = true;
        if cluster_columns == 2 {
            let continuation = self
                .cell_index(column + 1, row)
                .expect("wide terminal character must fit in the screen");
            self.cells[continuation] = Cell {
                text: CompactString::const_new(""),
                width: CellWidth::Continuation,
                foreground,
                background,
                attributes,
                hyperlink,
            };
            self.occupied_cells[continuation] = true;
        }
        self.mark_dirty(row);
        let fill = blank_cell();
        self.repair_neighborhood(row, column, &fill);
        self.repair_neighborhood(row, column + cluster_columns, &fill);
        self.recompute_occupied(row);
    }

    pub(crate) fn fill_linear(&mut self, start: usize, end: usize, cell: Cell) {
        if start >= end {
            return;
        }
        let columns = self.dimensions.columns();
        let first_row = start / columns;
        let last_row = (end - 1) / columns;
        let first_column = start % columns;
        let last_column = end - last_row * columns;
        let physical_start = self.logical_linear_to_physical(start);
        let physical_end = physical_start + (end - start);
        self.cells[physical_start..physical_end].fill(cell.clone());
        self.occupied_cells[physical_start..physical_end].fill(!is_structural_blank(&cell));
        self.mark_dirty_range(first_row, last_row);
        // A contiguous fill can split a pair only at either range boundary.
        self.repair_neighborhood(first_row, first_column, &cell);
        self.repair_neighborhood(last_row, last_column, &cell);
        for row in first_row..=last_row {
            self.recompute_occupied(row);
        }
    }

    /// Translates a linear index expressed in logical (row 0 = top visible
    /// row) coordinates into the ring buffer's physical cell-array offset.
    /// Only valid for offsets that stay within a single logical row's span,
    /// since a physical row is always contiguous but successive logical
    /// rows may not be (the ring can wrap between them).
    fn logical_linear_to_physical(&self, linear: usize) -> usize {
        let columns = self.dimensions.columns();
        let row = linear / columns;
        let column = linear % columns;
        self.physical_row_start(row) + column
    }

    pub(crate) fn clear_all(&mut self, cell: Cell) {
        let occupied = if is_structural_blank(&cell) {
            0
        } else {
            self.dimensions.columns()
        };
        self.cells.fill(cell);
        self.occupied_cells.fill(occupied != 0);
        self.soft_wrapped_rows.fill(false);
        self.occupied_columns.fill(occupied);
        self.mark_all_dirty();
        // Clearing collapses any pending rotation: logical row 0 is once
        // again physical row 0, keeping the ring buffer's invariant simple
        // for the very common "clear the whole screen" case.
        self.top = 0;
    }

    pub(crate) fn insert_characters(
        &mut self,
        column: usize,
        row: usize,
        count: usize,
        cell: Cell,
    ) {
        let columns = self.dimensions.columns();
        let count = count.min(columns - column);
        let row_start = self.physical_row_start(row);
        let row_end = row_start + columns;
        let start = row_start + column;
        self.move_cells_within_row(start..row_end - count, start + count);
        self.cells[start..start + count].fill(cell.clone());
        self.occupied_cells[start..start + count].fill(!is_structural_blank(&cell));
        self.mark_dirty(row);
        self.repair_neighborhood(row, column, &cell);
        self.repair_neighborhood(row, column + count, &cell);
        self.repair_neighborhood(row, columns, &cell);
        self.recompute_occupied(row);
    }

    pub(crate) fn delete_characters(
        &mut self,
        column: usize,
        row: usize,
        count: usize,
        cell: Cell,
    ) {
        let columns = self.dimensions.columns();
        let count = count.min(columns - column);
        let row_start = self.physical_row_start(row);
        let row_end = row_start + columns;
        let start = row_start + column;
        self.move_cells_within_row(start + count..row_end, start);
        self.cells[row_end - count..row_end].fill(cell.clone());
        self.occupied_cells[row_end - count..row_end].fill(!is_structural_blank(&cell));
        self.mark_dirty(row);
        self.repair_neighborhood(row, column, &cell);
        self.repair_neighborhood(row, columns - count, &cell);
        self.recompute_occupied(row);
    }

    pub(crate) fn insert_lines(&mut self, row: usize, bottom: usize, count: usize, cell: Cell) {
        let count = count.min(bottom - row + 1);
        for logical in (row..=bottom - count).rev() {
            self.copy_row(logical, logical + count);
        }
        for logical in row..row + count {
            self.fill_row(logical, cell.clone());
        }
        self.mark_dirty_range(row, bottom);
    }

    pub(crate) fn delete_lines(&mut self, row: usize, bottom: usize, count: usize, cell: Cell) {
        let count = count.min(bottom - row + 1);
        for logical in row + count..=bottom {
            self.copy_row(logical, logical - count);
        }
        for logical in bottom + 1 - count..=bottom {
            self.fill_row(logical, cell.clone());
        }
        self.mark_dirty_range(row, bottom);
    }

    pub(crate) fn scroll_up(
        &mut self,
        top: usize,
        bottom: usize,
        count: usize,
        cell: Cell,
    ) -> Vec<ScreenRow> {
        let count = count.min(bottom - top + 1);
        let removed = (top..top + count)
            .map(|row| {
                let start = self.physical_row_start(row);
                ScreenRow {
                    cells: self.cells[start..start + self.occupied_columns[self.physical_row(row)]]
                        .to_vec(),
                    soft_wrapped: self.soft_wrapped_rows[self.physical_row(row)],
                }
            })
            .collect();
        if top == 0 && bottom + 1 == self.dimensions.rows() {
            // The common case: the whole screen scrolls. Rotating the ring
            // by `count` avoids moving any surviving row's cells at all -
            // only the `count` newly revealed rows at the bottom need to be
            // cleared, an O(count) operation instead of O(rows*columns).
            let rows = self.dimensions.rows();
            for offset in 0..count {
                self.fill_row(offset, cell.clone());
            }
            self.top = (self.top + count) % rows;
        } else {
            // A partial scroll region still requires shifting the affected
            // rows physically, since the ring rotation only ever applies to
            // the whole screen. Rows move toward lower indices, so copy
            // forward (destination is never re-read as a source before it's
            // written, since its source index is always still ahead).
            for logical in top + count..=bottom {
                self.copy_row(logical, logical - count);
            }
            for logical in bottom + 1 - count..=bottom {
                self.fill_row(logical, cell.clone());
            }
        }
        self.mark_dirty_range(top, bottom);
        removed
    }

    pub(crate) fn scroll_down(&mut self, top: usize, bottom: usize, count: usize, cell: Cell) {
        let count = count.min(bottom - top + 1);
        for logical in (top..=bottom - count).rev() {
            self.copy_row(logical, logical + count);
        }
        for logical in top..top + count {
            self.fill_row(logical, cell.clone());
        }
        self.mark_dirty_range(top, bottom);
    }

    pub(crate) fn mark_soft_wrapped(&mut self, row: usize) {
        // A width-two glyph can pre-wrap before the final column, so the
        // logical continuation flag and occupied extent are independent.
        let physical_row = self.physical_row(row);
        self.soft_wrapped_rows[physical_row] = true;
    }

    fn cell_index(&self, column: usize, row: usize) -> Option<usize> {
        (column < self.dimensions.columns() && row < self.dimensions.rows())
            .then_some(self.physical_row_start(row) + column)
    }

    /// Copies one logical row's cells and metadata onto another logical
    /// row. Used by the line/region-shifting operations that can't be
    /// expressed as a whole-screen ring rotation (insert/delete lines,
    /// scroll within a bounded region, `scroll_down`).
    fn copy_row(&mut self, source_row: usize, destination_row: usize) {
        let columns = self.dimensions.columns();
        let source_start = self.physical_row_start(source_row);
        let destination_start = self.physical_row_start(destination_row);
        if source_start == destination_start {
            return;
        }
        // A physical row is always contiguous in memory even though logical
        // rows can straddle the ring's wrap point, so a plain slice copy
        // (rather than `move_cells`'s cross-region element loop) suffices.
        let (left, right) = if source_start < destination_start {
            let (left, right) = self.cells.split_at_mut(destination_start);
            (
                &mut left[source_start..source_start + columns],
                &mut right[..columns],
            )
        } else {
            let (left, right) = self.cells.split_at_mut(source_start);
            (
                &mut left[destination_start..destination_start + columns],
                &mut right[..columns],
            )
        };
        let (destination_cells, source_cells) = if source_start < destination_start {
            (right, &*left)
        } else {
            (left, &*right)
        };
        destination_cells.clone_from_slice(source_cells);
        self.occupied_cells
            .copy_within(source_start..source_start + columns, destination_start);
        let source_physical = self.physical_row(source_row);
        let destination_physical = self.physical_row(destination_row);
        self.soft_wrapped_rows[destination_physical] = self.soft_wrapped_rows[source_physical];
        self.occupied_columns[destination_physical] = self.occupied_columns[source_physical];
    }

    /// Overwrites an entire logical row with `cell`, matching the fill
    /// behavior of a full-width `fill_linear` call for that row.
    fn fill_row(&mut self, row: usize, cell: Cell) {
        let columns = self.dimensions.columns();
        let start = self.physical_row_start(row);
        let occupied = !is_structural_blank(&cell);
        self.cells[start..start + columns].fill(cell);
        self.occupied_cells[start..start + columns].fill(occupied);
        let physical_row = self.physical_row(row);
        self.soft_wrapped_rows[physical_row] = false;
        self.occupied_columns[physical_row] = row_extent(occupied, columns);
    }

    fn move_cells_within_row(&mut self, source: std::ops::Range<usize>, destination: usize) {
        let length = source.end - source.start;
        if destination > source.start {
            for offset in (0..length).rev() {
                self.cells[destination + offset] = self.cells[source.start + offset].clone();
                self.occupied_cells[destination + offset] =
                    self.occupied_cells[source.start + offset];
            }
        } else {
            for offset in 0..length {
                self.cells[destination + offset] = self.cells[source.start + offset].clone();
                self.occupied_cells[destination + offset] =
                    self.occupied_cells[source.start + offset];
            }
        }
    }

    fn mark_dirty(&mut self, row: usize) {
        self.dirty_rows[row] = true;
    }

    pub(crate) fn mark_dirty_range(&mut self, first: usize, last: usize) {
        self.dirty_rows[first..=last].fill(true);
    }

    pub(crate) fn mark_all_dirty(&mut self) {
        self.dirty_rows.fill(true);
    }

    fn repair_wide_cells(&mut self) {
        for row in 0..self.dimensions.rows() {
            self.repair_row(row);
        }
    }

    fn repair_row(&mut self, row: usize) {
        let fill = blank_cell();
        for column in 0..self.dimensions.columns() {
            self.repair_cell(row, column, &fill);
        }
        self.recompute_occupied(row);
    }

    fn repair_neighborhood(&mut self, row: usize, boundary: usize, fill: &Cell) {
        let columns = self.dimensions.columns();
        let first = boundary.saturating_sub(1);
        let last = boundary.saturating_add(1).min(columns - 1);
        if first > last {
            return;
        }
        for column in first..=last {
            self.repair_cell(row, column, fill);
        }
    }

    fn repair_cell(&mut self, row: usize, column: usize, fill: &Cell) {
        let columns = self.dimensions.columns();
        let index = self.physical_row_start(row) + column;
        let invalid = match self.cells[index].width {
            CellWidth::Double => {
                column + 1 == columns || self.cells[index + 1].width != CellWidth::Continuation
            }
            CellWidth::Continuation => {
                column == 0 || self.cells[index - 1].width != CellWidth::Double
            }
            CellWidth::Single => false,
        };
        if invalid {
            self.cells[index] = fill.clone();
            self.occupied_cells[index] = !is_structural_blank(fill);
        }
    }

    fn recompute_occupied(&mut self, row: usize) {
        let start = self.physical_row_start(row);
        let physical_row = self.physical_row(row);
        self.occupied_columns[physical_row] = self.occupied_cells
            [start..start + self.dimensions.columns()]
            .iter()
            .rposition(|occupied| *occupied)
            .map_or(0, |column| column + 1);
    }
}

fn is_structural_blank(cell: &Cell) -> bool {
    cell.text == " "
        && cell.width == CellWidth::Single
        && cell.foreground == crate::Color::Default
        && cell.background == crate::Color::Default
        && cell.attributes == crate::Attributes::NONE
        && cell.hyperlink.is_none()
}

const fn row_extent(occupied: bool, columns: usize) -> usize {
    if occupied {
        columns
    } else {
        0
    }
}
