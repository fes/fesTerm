use crate::{
    cell::{blank_cell, Cell, CellWidth},
    terminal::TerminalError,
    Dimensions,
};

/// A visible grid and its redraw state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Screen {
    dimensions: Dimensions,
    cells: Vec<Cell>,
    dirty_rows: Vec<bool>,
}

impl Screen {
    pub fn new(dimensions: Dimensions) -> Result<Self, TerminalError> {
        let mut cells = Vec::new();
        cells
            .try_reserve_exact(dimensions.cell_count())
            .map_err(|error| TerminalError::allocation("screen cells", error))?;
        cells.resize(dimensions.cell_count(), blank_cell());

        let mut dirty_rows = Vec::new();
        dirty_rows
            .try_reserve_exact(dimensions.rows())
            .map_err(|error| TerminalError::allocation("screen dirty rows", error))?;
        dirty_rows.resize(dimensions.rows(), true);

        Ok(Self {
            dimensions,
            cells,
            dirty_rows,
        })
    }

    pub const fn dimensions(&self) -> Dimensions {
        self.dimensions
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

        let start = row * self.dimensions.columns();
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

    pub(crate) fn resized(&self, dimensions: Dimensions) -> Result<Self, TerminalError> {
        let mut resized = Self::new(dimensions)?;
        let preserved_rows = self.dimensions.rows().min(dimensions.rows());
        let preserved_columns = self.dimensions.columns().min(dimensions.columns());
        for row in 0..preserved_rows {
            let old_start = row * self.dimensions.columns();
            let new_start = row * dimensions.columns();
            resized.cells[new_start..new_start + preserved_columns]
                .clone_from_slice(&self.cells[old_start..old_start + preserved_columns]);
        }
        resized.repair_wide_cells();
        Ok(resized)
    }

    pub(crate) fn replace_cell(&mut self, column: usize, row: usize, cell: Cell) {
        let index = self
            .cell_index(column, row)
            .expect("terminal cursor must remain within screen dimensions");
        self.cells[index] = cell;
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
        if cluster_columns == 2 {
            let continuation = self
                .cell_index(column + 1, row)
                .expect("wide terminal character must fit in the screen");
            self.cells[continuation] = Cell {
                text: String::new(),
                width: CellWidth::Continuation,
                foreground,
                background,
                attributes,
                hyperlink,
            };
        }
        self.mark_dirty(row);
        let fill = blank_cell();
        self.repair_neighborhood(row, column, &fill);
        self.repair_neighborhood(row, column + cluster_columns, &fill);
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
        self.cells[start..end].fill(cell.clone());
        self.mark_dirty_range(first_row, last_row);
        // A contiguous fill can split a pair only at either range boundary.
        self.repair_neighborhood(first_row, first_column, &cell);
        self.repair_neighborhood(last_row, last_column, &cell);
    }

    pub(crate) fn clear_all(&mut self, cell: Cell) {
        self.cells.fill(cell);
        self.mark_all_dirty();
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
        let row_start = row * columns;
        let row_end = row_start + columns;
        let start = row_start + column;
        self.move_cells(start..row_end - count, start + count);
        self.cells[start..start + count].fill(cell.clone());
        self.mark_dirty(row);
        self.repair_neighborhood(row, column, &cell);
        self.repair_neighborhood(row, column + count, &cell);
        self.repair_neighborhood(row, columns, &cell);
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
        let row_start = row * columns;
        let row_end = row_start + columns;
        let start = row_start + column;
        self.move_cells(start + count..row_end, start);
        self.cells[row_end - count..row_end].fill(cell.clone());
        self.mark_dirty(row);
        self.repair_neighborhood(row, column, &cell);
        self.repair_neighborhood(row, columns - count, &cell);
    }

    pub(crate) fn insert_lines(&mut self, row: usize, bottom: usize, count: usize, cell: Cell) {
        let columns = self.dimensions.columns();
        let count = count.min(bottom - row + 1);
        let source_end = (bottom + 1 - count) * columns;
        self.move_cells(row * columns..source_end, (row + count) * columns);
        self.cells[row * columns..(row + count) * columns].fill(cell);
        self.mark_dirty_range(row, bottom);
    }

    pub(crate) fn delete_lines(&mut self, row: usize, bottom: usize, count: usize, cell: Cell) {
        let columns = self.dimensions.columns();
        let count = count.min(bottom - row + 1);
        self.move_cells(
            (row + count) * columns..(bottom + 1) * columns,
            row * columns,
        );
        self.cells[(bottom + 1 - count) * columns..(bottom + 1) * columns].fill(cell);
        self.mark_dirty_range(row, bottom);
    }

    pub(crate) fn scroll_up(&mut self, top: usize, bottom: usize, count: usize, cell: Cell) {
        let columns = self.dimensions.columns();
        let count = count.min(bottom - top + 1);
        let region_start = top * columns;
        let region_end = (bottom + 1) * columns;
        let shifted_cells = count * columns;
        self.move_cells(region_start + shifted_cells..region_end, region_start);
        self.cells[region_end - shifted_cells..region_end].fill(cell);
        self.mark_dirty_range(top, bottom);
    }

    pub(crate) fn scroll_down(&mut self, top: usize, bottom: usize, count: usize, cell: Cell) {
        let columns = self.dimensions.columns();
        let count = count.min(bottom - top + 1);
        let region_start = top * columns;
        let region_end = (bottom + 1) * columns;
        let shifted_cells = count * columns;
        self.move_cells(
            region_start..region_end - shifted_cells,
            region_start + shifted_cells,
        );
        self.cells[region_start..region_start + shifted_cells].fill(cell);
        self.mark_dirty_range(top, bottom);
    }

    fn cell_index(&self, column: usize, row: usize) -> Option<usize> {
        (column < self.dimensions.columns() && row < self.dimensions.rows())
            .then_some(row * self.dimensions.columns() + column)
    }

    fn move_cells(&mut self, source: std::ops::Range<usize>, destination: usize) {
        let length = source.end - source.start;
        if destination > source.start {
            for offset in (0..length).rev() {
                self.cells[destination + offset] = self.cells[source.start + offset].clone();
            }
        } else {
            for offset in 0..length {
                self.cells[destination + offset] = self.cells[source.start + offset].clone();
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
        let index = row * columns + column;
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
        }
    }
}
