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
    occupied_cells: Vec<bool>,
    dirty_rows: Vec<bool>,
    soft_wrapped_rows: Vec<bool>,
    occupied_columns: Vec<usize>,
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
            resized.occupied_cells[new_start..new_start + preserved_columns]
                .copy_from_slice(&self.occupied_cells[old_start..old_start + preserved_columns]);
            resized.soft_wrapped_rows[row] =
                self.soft_wrapped_rows[row] && preserved_columns == self.dimensions.columns();
            resized.occupied_columns[row] = self.occupied_columns[row].min(preserved_columns);
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
                text: String::new(),
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
        self.cells[start..end].fill(cell.clone());
        self.occupied_cells[start..end].fill(!is_structural_blank(&cell));
        self.mark_dirty_range(first_row, last_row);
        // A contiguous fill can split a pair only at either range boundary.
        self.repair_neighborhood(first_row, first_column, &cell);
        self.repair_neighborhood(last_row, last_column, &cell);
        for row in first_row..=last_row {
            self.recompute_occupied(row);
        }
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
        let row_start = row * columns;
        let row_end = row_start + columns;
        let start = row_start + column;
        self.move_cells(start + count..row_end, start);
        self.cells[row_end - count..row_end].fill(cell.clone());
        self.occupied_cells[row_end - count..row_end].fill(!is_structural_blank(&cell));
        self.mark_dirty(row);
        self.repair_neighborhood(row, column, &cell);
        self.repair_neighborhood(row, columns - count, &cell);
        self.recompute_occupied(row);
    }

    pub(crate) fn insert_lines(&mut self, row: usize, bottom: usize, count: usize, cell: Cell) {
        let columns = self.dimensions.columns();
        let count = count.min(bottom - row + 1);
        let source_end = (bottom + 1 - count) * columns;
        self.move_cells(row * columns..source_end, (row + count) * columns);
        let occupied = !is_structural_blank(&cell);
        self.cells[row * columns..(row + count) * columns].fill(cell);
        self.occupied_cells[row * columns..(row + count) * columns].fill(occupied);
        self.move_row_metadata(row..bottom + 1 - count, row + count);
        self.soft_wrapped_rows[row..row + count].fill(false);
        self.occupied_columns[row..row + count].fill(row_extent(occupied, columns));
        self.mark_dirty_range(row, bottom);
    }

    pub(crate) fn delete_lines(&mut self, row: usize, bottom: usize, count: usize, cell: Cell) {
        let columns = self.dimensions.columns();
        let count = count.min(bottom - row + 1);
        self.move_cells(
            (row + count) * columns..(bottom + 1) * columns,
            row * columns,
        );
        let occupied = !is_structural_blank(&cell);
        self.cells[(bottom + 1 - count) * columns..(bottom + 1) * columns].fill(cell);
        self.occupied_cells[(bottom + 1 - count) * columns..(bottom + 1) * columns].fill(occupied);
        self.move_row_metadata(row + count..bottom + 1, row);
        self.soft_wrapped_rows[bottom + 1 - count..bottom + 1].fill(false);
        self.occupied_columns[bottom + 1 - count..bottom + 1].fill(row_extent(occupied, columns));
        self.mark_dirty_range(row, bottom);
    }

    pub(crate) fn scroll_up(
        &mut self,
        top: usize,
        bottom: usize,
        count: usize,
        cell: Cell,
    ) -> Vec<ScreenRow> {
        let columns = self.dimensions.columns();
        let count = count.min(bottom - top + 1);
        let removed = (top..top + count)
            .map(|row| {
                let start = row * columns;
                ScreenRow {
                    cells: self.cells[start..start + self.occupied_columns[row]].to_vec(),
                    soft_wrapped: self.soft_wrapped_rows[row],
                }
            })
            .collect();
        let region_start = top * columns;
        let region_end = (bottom + 1) * columns;
        let shifted_cells = count * columns;
        self.move_cells(region_start + shifted_cells..region_end, region_start);
        let occupied = !is_structural_blank(&cell);
        self.cells[region_end - shifted_cells..region_end].fill(cell);
        self.occupied_cells[region_end - shifted_cells..region_end].fill(occupied);
        self.move_row_metadata(top + count..bottom + 1, top);
        self.soft_wrapped_rows[bottom + 1 - count..bottom + 1].fill(false);
        self.occupied_columns[bottom + 1 - count..bottom + 1].fill(row_extent(occupied, columns));
        self.mark_dirty_range(top, bottom);
        removed
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
        let occupied = !is_structural_blank(&cell);
        self.cells[region_start..region_start + shifted_cells].fill(cell);
        self.occupied_cells[region_start..region_start + shifted_cells].fill(occupied);
        self.move_row_metadata(top..bottom + 1 - count, top + count);
        self.soft_wrapped_rows[top..top + count].fill(false);
        self.occupied_columns[top..top + count].fill(row_extent(occupied, columns));
        self.mark_dirty_range(top, bottom);
    }

    pub(crate) fn mark_soft_wrapped(&mut self, row: usize) {
        self.soft_wrapped_rows[row] = true;
        self.occupied_columns[row] = self.dimensions.columns();
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

    fn move_row_metadata(&mut self, source: std::ops::Range<usize>, destination: usize) {
        let length = source.end - source.start;
        if destination > source.start {
            for offset in (0..length).rev() {
                self.soft_wrapped_rows[destination + offset] =
                    self.soft_wrapped_rows[source.start + offset];
                self.occupied_columns[destination + offset] =
                    self.occupied_columns[source.start + offset];
            }
        } else {
            for offset in 0..length {
                self.soft_wrapped_rows[destination + offset] =
                    self.soft_wrapped_rows[source.start + offset];
                self.occupied_columns[destination + offset] =
                    self.occupied_columns[source.start + offset];
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
            self.occupied_cells[index] = !is_structural_blank(fill);
        }
    }

    fn recompute_occupied(&mut self, row: usize) {
        if self.soft_wrapped_rows[row] {
            self.occupied_columns[row] = self.dimensions.columns();
            return;
        }
        let start = row * self.dimensions.columns();
        self.occupied_columns[row] = self.occupied_cells[start..start + self.dimensions.columns()]
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
