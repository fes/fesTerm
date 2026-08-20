use std::{collections::VecDeque, mem::size_of};

use crate::{cell::CellWidth, screen::ScreenRow, Cell};

/// Default retained primary-screen payload budget: 64 MiB per terminal.
pub const DEFAULT_SCROLLBACK_LIMIT_BYTES: usize = 64 * 1024 * 1024;

/// Content-free measurements for bounded primary-screen history.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScrollbackStats {
    limit_bytes: usize,
    charged_bytes: usize,
    logical_lines: usize,
    physical_rows: usize,
    evicted_lines: u64,
    oversize_lines: u64,
}

impl ScrollbackStats {
    pub const fn limit_bytes(self) -> usize {
        self.limit_bytes
    }
    pub const fn charged_bytes(self) -> usize {
        self.charged_bytes
    }
    pub const fn logical_lines(self) -> usize {
        self.logical_lines
    }
    pub const fn physical_rows(self) -> usize {
        self.physical_rows
    }
    pub const fn evicted_lines(self) -> u64 {
        self.evicted_lines
    }
    pub const fn oversize_lines(self) -> u64 {
        self.oversize_lines
    }
}

/// One retained logical line. Cell content remains terminal-owned and in memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalLine {
    id: u64,
    cells: Vec<Cell>,
    row_ends: Vec<usize>,
    physical_rows: usize,
    hard_break: bool,
    charged_bytes: usize,
}

impl LogicalLine {
    pub const fn id(&self) -> u64 {
        self.id
    }
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }
    pub const fn physical_rows(&self) -> usize {
        self.physical_rows
    }
    pub fn physical_row(&self, row: usize) -> Option<&[Cell]> {
        let end = *self.row_ends.get(row)?;
        let start = row
            .checked_sub(1)
            .and_then(|previous| self.row_ends.get(previous).copied())
            .unwrap_or(0);
        Some(&self.cells[start..end])
    }
    pub const fn has_hard_break(&self) -> bool {
        self.hard_break
    }

    /// Rebuilds physical-row boundaries for this logical line at `columns`,
    /// without changing its cell content, identity, or hard-break ending.
    ///
    /// A leading [`CellWidth::Double`] cell and its trailing
    /// [`CellWidth::Continuation`] cell are treated as one atomic two-column
    /// unit that never splits across a row boundary, mirroring the live
    /// auto-wrap print path.
    fn reflow(&mut self, columns: usize) {
        let columns = columns.max(1);
        let cell_count = self.cells.len();
        let mut row_ends = Vec::new();
        if cell_count == 0 {
            row_ends.push(0);
        } else {
            let mut index = 0;
            let mut used_columns = 0;
            while index < cell_count {
                let unit_cells = if self.cells[index].width() == CellWidth::Double
                    && self
                        .cells
                        .get(index + 1)
                        .is_some_and(|cell| cell.width() == CellWidth::Continuation)
                {
                    2
                } else {
                    1
                };
                let unit_columns = unit_cells;
                if used_columns > 0 && used_columns + unit_columns > columns {
                    row_ends.push(index);
                    used_columns = 0;
                }
                index += unit_cells;
                used_columns += unit_columns;
            }
            row_ends.push(cell_count);
        }
        self.physical_rows = row_ends.len();
        self.row_ends = row_ends;
        self.charged_bytes = size_of::<Self>()
            .saturating_add(charged_cells(&self.cells, self.cells.capacity()))
            .saturating_add(size_of::<usize>().saturating_mul(self.row_ends.capacity()));
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Scrollback {
    limit_bytes: usize,
    charged_bytes: usize,
    lines: VecDeque<LogicalLine>,
    next_id: u64,
    evicted_lines: u64,
    oversize_lines: u64,
    dropping_oversize_line: bool,
}

impl Scrollback {
    pub(crate) fn new(limit_bytes: usize) -> Self {
        Self {
            limit_bytes,
            charged_bytes: 0,
            lines: VecDeque::new(),
            next_id: 0,
            evicted_lines: 0,
            oversize_lines: 0,
            dropping_oversize_line: false,
        }
    }

    pub(crate) fn push_rows(&mut self, rows: Vec<ScreenRow>) {
        for row in rows {
            self.push_row(row);
        }
    }

    fn push_row(&mut self, row: ScreenRow) {
        let ends_line = !row.soft_wrapped;
        if self.limit_bytes == 0 || self.dropping_oversize_line {
            if ends_line {
                self.dropping_oversize_line = false;
            }
            return;
        }

        if self.lines.back().is_none_or(|line| line.hard_break) {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1);
            self.lines.push_back(LogicalLine {
                id,
                cells: Vec::new(),
                row_ends: Vec::new(),
                physical_rows: 0,
                hard_break: false,
                charged_bytes: size_of::<LogicalLine>(),
            });
            self.charged_bytes = self.charged_bytes.saturating_add(size_of::<LogicalLine>());
        }

        let line = self.lines.back_mut().expect("open history line exists");
        let prior_charge = line.charged_bytes;
        line.cells.extend(row.cells);
        line.row_ends.push(line.cells.len());
        line.physical_rows += 1;
        line.hard_break = ends_line;
        line.charged_bytes = size_of::<LogicalLine>()
            .saturating_add(charged_cells(&line.cells, line.cells.capacity()))
            .saturating_add(size_of::<usize>().saturating_mul(line.row_ends.capacity()));
        self.charged_bytes = self
            .charged_bytes
            .saturating_add(line.charged_bytes.saturating_sub(prior_charge));

        if line.charged_bytes > self.limit_bytes {
            let removed = self.lines.pop_back().expect("oversize line exists");
            self.charged_bytes = self.charged_bytes.saturating_sub(removed.charged_bytes);
            self.oversize_lines = self.oversize_lines.saturating_add(1);
            self.dropping_oversize_line = !ends_line;
        }
        self.evict_complete_lines();
    }

    fn evict_complete_lines(&mut self) {
        while self.charged_bytes > self.limit_bytes {
            if self.lines.front().is_some_and(|line| !line.hard_break) {
                break;
            }
            let Some(removed) = self.lines.pop_front() else {
                break;
            };
            self.charged_bytes = self.charged_bytes.saturating_sub(removed.charged_bytes);
            self.evicted_lines = self.evicted_lines.saturating_add(1);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.lines.clear();
        self.charged_bytes = 0;
        self.dropping_oversize_line = false;
    }

    /// Rewraps every retained logical line's physical-row boundaries at the
    /// new primary width. Logical-line identity, cell content, ordering, and
    /// hard-break endings are unchanged; only physical row splits and the
    /// accounted row-index storage move. This never evicts lines: reflow
    /// does not add cell content, so charged bytes cannot exceed the
    /// existing budget as a result of calling it.
    pub(crate) fn reflow(&mut self, columns: usize) {
        for line in &mut self.lines {
            let prior_charge = line.charged_bytes;
            line.reflow(columns);
            self.charged_bytes = self
                .charged_bytes
                .saturating_sub(prior_charge)
                .saturating_add(line.charged_bytes);
        }
    }

    pub(crate) fn lines(&self) -> impl ExactSizeIterator<Item = &LogicalLine> {
        self.lines.iter()
    }

    pub(crate) fn stats(&self) -> ScrollbackStats {
        ScrollbackStats {
            limit_bytes: self.limit_bytes,
            charged_bytes: self.charged_bytes,
            logical_lines: self.lines.len(),
            physical_rows: self.lines.iter().map(|line| line.physical_rows).sum(),
            evicted_lines: self.evicted_lines,
            oversize_lines: self.oversize_lines,
        }
    }
}

fn charged_cells(cells: &[Cell], capacity: usize) -> usize {
    size_of::<Cell>().saturating_mul(capacity).saturating_add(
        cells
            .iter()
            .map(|cell| {
                cell.text
                    .capacity()
                    .saturating_add(cell.hyperlink.as_ref().map_or(0, |target| target.len()))
            })
            .fold(0usize, usize::saturating_add),
    )
}
