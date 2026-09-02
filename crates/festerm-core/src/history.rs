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
    content_row_origin: u64,
    screen_row_origin: u64,
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
    pub const fn content_row_origin(self) -> u64 {
        self.content_row_origin
    }

    /// Monotonic content coordinate of the first live-screen row.
    pub const fn screen_row_origin(self) -> u64 {
        self.screen_row_origin
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

    pub fn physical_row_soft_wrapped(&self, row: usize) -> Option<bool> {
        (row < self.physical_rows).then(|| row + 1 < self.physical_rows || !self.hard_break)
    }

    /// Converts a (physical row, column-within-row) position on this line
    /// into an absolute cell-stream offset, clamping the column to that
    /// row's actual content length. Used to capture a stable cursor anchor
    /// before a reflow changes physical-row boundaries.
    fn cell_offset_for_row(&self, row: usize, column: usize) -> usize {
        let start = row
            .checked_sub(1)
            .and_then(|previous| self.row_ends.get(previous).copied())
            .unwrap_or(0);
        let end = self.row_ends.get(row).copied().unwrap_or(self.cells.len());
        start + column.min(end.saturating_sub(start))
    }

    /// Converts an absolute cell-stream offset back into a (physical row,
    /// column-within-row) position using this line's current physical-row
    /// boundaries. Used after a reflow to relocate a cursor anchor captured
    /// via [`cell_offset_for_row`](Self::cell_offset_for_row) at the old
    /// boundaries.
    fn locate_offset(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.cells.len());
        let row = self
            .row_ends
            .iter()
            .position(|&end| offset < end)
            .unwrap_or_else(|| self.row_ends.len().saturating_sub(1));
        let start = row
            .checked_sub(1)
            .and_then(|previous| self.row_ends.get(previous).copied())
            .unwrap_or(0);
        (row, offset - start)
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
    content_row_origin: u64,
    screen_row_origin: u64,
    discarded_gap_rows: u64,
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
            content_row_origin: 0,
            screen_row_origin: 0,
            discarded_gap_rows: 0,
            oversize_lines: 0,
            dropping_oversize_line: false,
        }
    }

    pub(crate) fn set_limit_bytes(&mut self, limit_bytes: usize) {
        self.limit_bytes = limit_bytes;
        self.enforce_limit();
    }

    pub(crate) fn push_rows(&mut self, rows: Vec<ScreenRow>) {
        for row in rows {
            self.push_row(row);
        }
    }

    fn push_row(&mut self, row: ScreenRow) {
        let row_coordinate = self.screen_row_origin;
        self.screen_row_origin = self.screen_row_origin.saturating_add(1);
        let ends_line = !row.soft_wrapped;
        if self.limit_bytes == 0 || self.dropping_oversize_line {
            self.discarded_gap_rows = self.discarded_gap_rows.saturating_add(1);
            if ends_line {
                self.dropping_oversize_line = false;
            }
            return;
        }

        // Retained rows are represented as one contiguous coordinate range.
        // If rows were discarded (zero limit or an oversized logical line),
        // retaining again must not place new content into those stale
        // coordinates. Drop the older pre-gap history and begin a fresh
        // retained range at this row's actual monotonic coordinate.
        if self.discarded_gap_rows > 0 {
            self.evicted_lines = self.evicted_lines.saturating_add(self.lines.len() as u64);
            self.lines.clear();
            self.charged_bytes = 0;
            self.content_row_origin = row_coordinate;
            self.discarded_gap_rows = 0;
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
            self.discarded_gap_rows = self
                .discarded_gap_rows
                .saturating_add(removed.physical_rows as u64);
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
            self.content_row_origin = self
                .content_row_origin
                .saturating_add(removed.physical_rows as u64);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.content_row_origin = self.screen_row_origin;
        self.lines.clear();
        self.charged_bytes = 0;
        self.discarded_gap_rows = 0;
        self.dropping_oversize_line = false;
    }

    /// Rewraps every retained logical line's physical-row boundaries at the
    /// new primary width. Logical-line identity, cell content, ordering, and
    /// hard-break endings are unchanged; only physical row splits and the
    /// accounted row-index storage move. Narrower rows can require more
    /// row-index capacity, so the caller re-applies the bound after splitting
    /// the live-screen tail back out.
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

    /// Total physical rows across every retained logical line.
    pub(crate) fn total_physical_rows(&self) -> usize {
        self.lines.iter().map(|line| line.physical_rows).sum()
    }

    /// Captures a stable logical anchor (line identity plus cell-stream
    /// offset) for a cursor sitting at `absolute_row` (0-indexed from the
    /// oldest retained row) and `column` within it. Returns `None` if
    /// `absolute_row` is out of range.
    pub(crate) fn line_and_offset_at(
        &self,
        mut absolute_row: usize,
        column: usize,
    ) -> Option<(u64, usize)> {
        for line in &self.lines {
            if absolute_row < line.physical_rows {
                return Some((line.id, line.cell_offset_for_row(absolute_row, column)));
            }
            absolute_row -= line.physical_rows;
        }
        None
    }

    /// Resolves a stable logical anchor back into a (column, absolute row)
    /// position using the current (possibly just-reflowed) physical-row
    /// boundaries of the line it names. Returns `None` if the line no
    /// longer exists (for example, evicted during reflow).
    pub(crate) fn resolve_anchor(&self, line_id: u64, offset: usize) -> Option<(usize, usize)> {
        let mut absolute_row = 0;
        for line in &self.lines {
            if line.id == line_id {
                let (row, column) = line.locate_offset(offset);
                return Some((column, absolute_row + row));
            }
            absolute_row += line.physical_rows;
        }
        None
    }

    /// Removes and returns up to the trailing `rows` physical rows of
    /// retained content as [`ScreenRow`]s, splitting a logical line if the
    /// boundary falls inside one. The remaining lines (with the split
    /// line's kept prefix, now open rather than hard-broken) stay as the
    /// updated retained scrollback. Returns fewer than `rows` entries if
    /// there isn't enough retained content.
    ///
    /// Used when reconstructing the visible screen from unified logical
    /// content during resize; this can only reduce accounted bytes, so it
    /// never triggers eviction.
    pub(crate) fn split_off_tail(&mut self, rows: usize) -> Vec<ScreenRow> {
        let mut remaining = rows;
        let mut segments: Vec<Vec<ScreenRow>> = Vec::new();
        while remaining > 0 {
            let Some(mut line) = self.lines.pop_back() else {
                break;
            };
            self.charged_bytes = self.charged_bytes.saturating_sub(line.charged_bytes);
            if line.physical_rows <= remaining {
                remaining -= line.physical_rows;
                let physical_rows = line.physical_rows;
                let hard_break = line.hard_break;
                let segment = (0..physical_rows)
                    .map(|row| {
                        let cells = line.physical_row(row).expect("row exists").to_vec();
                        let soft_wrapped = row + 1 != physical_rows || !hard_break;
                        ScreenRow {
                            cells,
                            soft_wrapped,
                        }
                    })
                    .collect();
                segments.push(segment);
            } else {
                let keep_rows = line.physical_rows - remaining;
                let segment = (keep_rows..line.physical_rows)
                    .map(|row| {
                        let cells = line.physical_row(row).expect("row exists").to_vec();
                        let soft_wrapped = row + 1 != line.physical_rows || !line.hard_break;
                        ScreenRow {
                            cells,
                            soft_wrapped,
                        }
                    })
                    .collect();
                segments.push(segment);

                let keep_cell_end = line.row_ends[keep_rows - 1];
                line.cells.truncate(keep_cell_end);
                line.row_ends.truncate(keep_rows);
                // Capacity is charged, so truncating alone would leave the
                // retained prefix billed for the removed live-screen tail.
                line.cells.shrink_to_fit();
                line.row_ends.shrink_to_fit();
                line.physical_rows = keep_rows;
                line.hard_break = false;
                line.charged_bytes = size_of::<LogicalLine>()
                    .saturating_add(charged_cells(&line.cells, line.cells.capacity()))
                    .saturating_add(size_of::<usize>().saturating_mul(line.row_ends.capacity()));
                self.charged_bytes = self.charged_bytes.saturating_add(line.charged_bytes);
                self.lines.push_back(line);
                remaining = 0;
            }
        }
        segments.reverse();
        let rows = segments.into_iter().flatten().collect();
        self.enforce_limit();
        self.screen_row_origin = self
            .content_row_origin
            .saturating_add(self.total_physical_rows() as u64)
            .saturating_add(self.discarded_gap_rows);
        rows
    }

    fn enforce_limit(&mut self) {
        self.evict_complete_lines();
        if self.charged_bytes <= self.limit_bytes {
            return;
        }

        let removed = self
            .lines
            .pop_front()
            .expect("over-budget scrollback contains an open line");
        debug_assert!(!removed.hard_break);
        self.charged_bytes = self.charged_bytes.saturating_sub(removed.charged_bytes);
        self.oversize_lines = self.oversize_lines.saturating_add(1);
        self.content_row_origin = self
            .content_row_origin
            .saturating_add(removed.physical_rows as u64);
        self.dropping_oversize_line = true;
    }

    pub(crate) fn stats(&self) -> ScrollbackStats {
        ScrollbackStats {
            limit_bytes: self.limit_bytes,
            charged_bytes: self.charged_bytes,
            logical_lines: self.lines.len(),
            physical_rows: self.lines.iter().map(|line| line.physical_rows).sum(),
            content_row_origin: self.content_row_origin,
            screen_row_origin: self.screen_row_origin,
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
