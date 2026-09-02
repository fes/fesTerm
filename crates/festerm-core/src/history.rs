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
    cell_owned_bytes: usize,
    /// Count of cells ever removed from the front of this line by
    /// [`trim_oldest_physical_rows_to`](Self::trim_oldest_physical_rows_to).
    /// A stable
    /// cell-offset anchor captured before trimming is expressed in the
    /// line's *original* cell-stream numbering; this lets
    /// [`locate_offset`](Self::locate_offset) detect and reject an offset
    /// that has since been trimmed away, instead of silently resolving it
    /// against different, shifted content.
    trimmed_offset: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LogicalAnchor {
    line_id: u64,
    offset: usize,
    end_boundary: bool,
    trimmed_offset_at_capture: usize,
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

    /// Removes enough oldest physical rows to bring this line to
    /// `target_bytes`, shifting all remaining row boundaries down in one
    /// compaction. Used to amortize trimming of a still-open line that has
    /// grown past the scrollback byte budget.
    fn trim_oldest_physical_rows_to(&mut self, target_bytes: usize) -> usize {
        let mut rows_to_drop = 0;
        let mut cells_to_drop = 0;
        let mut removed_owned_bytes = 0usize;
        while rows_to_drop < self.physical_rows {
            let remaining_cells = self.cells.len().saturating_sub(cells_to_drop);
            let remaining_rows = self.row_ends.len().saturating_sub(rows_to_drop);
            let remaining_owned = self.cell_owned_bytes.saturating_sub(removed_owned_bytes);
            if compact_line_charge(remaining_cells, remaining_rows, remaining_owned) <= target_bytes
                && (rows_to_drop > 0 || self.charged_bytes <= target_bytes)
            {
                break;
            }
            let row_end = self.row_ends[rows_to_drop];
            removed_owned_bytes = removed_owned_bytes.saturating_add(
                self.cells[cells_to_drop..row_end]
                    .iter()
                    .map(cell_owned_charge)
                    .fold(0usize, usize::saturating_add),
            );
            cells_to_drop = row_end;
            rows_to_drop += 1;
        }
        if rows_to_drop == 0 {
            return 0;
        }

        self.cells.drain(0..cells_to_drop);
        self.row_ends.drain(0..rows_to_drop);
        for end in &mut self.row_ends {
            *end -= cells_to_drop;
        }
        self.cells.shrink_to_fit();
        self.row_ends.shrink_to_fit();
        self.physical_rows = self.physical_rows.saturating_sub(rows_to_drop);
        self.trimmed_offset = self.trimmed_offset.saturating_add(cells_to_drop);
        self.cell_owned_bytes = self.cell_owned_bytes.saturating_sub(removed_owned_bytes);
        self.recalculate_charge();
        rows_to_drop
    }

    pub fn physical_row_soft_wrapped(&self, row: usize) -> Option<bool> {
        (row < self.physical_rows).then(|| row + 1 < self.physical_rows || !self.hard_break)
    }

    /// Converts a (physical row, column-within-row) position on this line
    /// into an absolute cell-stream offset, clamping the column to that
    /// row's actual content length. Used to capture a stable cursor anchor
    /// before a reflow changes physical-row boundaries.
    fn anchor_for_row(&self, row: usize, column: usize) -> LogicalAnchor {
        let start = row
            .checked_sub(1)
            .and_then(|previous| self.row_ends.get(previous).copied())
            .unwrap_or(0);
        let end = self.row_ends.get(row).copied().unwrap_or(self.cells.len());
        let row_len = end.saturating_sub(start);
        LogicalAnchor {
            line_id: self.id,
            offset: self.trimmed_offset + start + column.min(row_len),
            end_boundary: column >= row_len,
            trimmed_offset_at_capture: self.trimmed_offset,
        }
    }

    /// Converts an absolute cell-stream offset (in this line's *original*,
    /// untrimmed numbering) back into a (physical row, column-within-row)
    /// position using this line's current physical-row boundaries. Used
    /// after a reflow to relocate a cursor anchor captured via
    /// [`anchor_for_row`](Self::anchor_for_row) at the old
    /// boundaries. Returns `None` if the offset refers to content that has
    /// since been trimmed from the front of this line (see
    /// [`trim_oldest_physical_rows_to`](Self::trim_oldest_physical_rows_to)) - such
    /// an anchor must fail to resolve rather than alias shifted content.
    fn locate_offset(&self, offset: usize, end_boundary: bool) -> Option<(usize, usize)> {
        let offset = offset.checked_sub(self.trimmed_offset)?;
        let offset = offset.min(self.cells.len());
        let row = self
            .row_ends
            .iter()
            .position(|&end| {
                if end_boundary {
                    offset <= end
                } else {
                    offset < end
                }
            })
            .unwrap_or_else(|| self.row_ends.len().saturating_sub(1));
        let start = row
            .checked_sub(1)
            .and_then(|previous| self.row_ends.get(previous).copied())
            .unwrap_or(0);
        Some((row, offset - start))
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
        self.recalculate_charge();
    }

    fn recalculate_charge(&mut self) {
        self.charged_bytes = size_of::<Self>()
            .saturating_add(size_of::<Cell>().saturating_mul(self.cells.capacity()))
            .saturating_add(self.cell_owned_bytes)
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
    oversize_lines: u64,
    active_oversize_line: Option<(u64, usize)>,
    #[cfg(test)]
    trim_compactions: u64,
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
            oversize_lines: 0,
            active_oversize_line: None,
            #[cfg(test)]
            trim_compactions: 0,
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
        self.screen_row_origin = self.screen_row_origin.saturating_add(1);
        let ends_line = !row.soft_wrapped;
        if self.limit_bytes == 0 {
            // Nothing is retained; keep `content_row_origin` tracking
            // `screen_row_origin` so the (empty) retained range stays
            // well-defined for callers.
            self.content_row_origin = self.screen_row_origin;
            if ends_line {
                self.active_oversize_line = None;
            }
            return;
        }

        if self.lines.back().is_none_or(|line| line.hard_break) {
            let continued_oversize = self
                .lines
                .is_empty()
                .then_some(self.active_oversize_line)
                .flatten();
            let (id, trimmed_offset) = continued_oversize.unwrap_or_else(|| {
                self.active_oversize_line = None;
                let id = self.next_id;
                self.next_id = self.next_id.wrapping_add(1);
                (id, 0)
            });
            self.lines.push_back(LogicalLine {
                id,
                cells: Vec::new(),
                row_ends: Vec::new(),
                physical_rows: 0,
                hard_break: false,
                charged_bytes: size_of::<LogicalLine>(),
                cell_owned_bytes: 0,
                trimmed_offset,
            });
            self.charged_bytes = self.charged_bytes.saturating_add(size_of::<LogicalLine>());
        }

        let active_oversize_line = self.active_oversize_line.map(|(id, _)| id);
        let line = self.lines.back_mut().expect("open history line exists");
        let line_id = line.id;
        let prior_charge = line.charged_bytes;
        line.cell_owned_bytes = line.cell_owned_bytes.saturating_add(
            row.cells
                .iter()
                .map(cell_owned_charge)
                .fold(0usize, usize::saturating_add),
        );
        line.cells.extend(row.cells);
        line.row_ends.push(line.cells.len());
        line.physical_rows += 1;
        line.hard_break = ends_line;
        line.recalculate_charge();
        if ends_line
            || (line.charged_bytes > self.limit_bytes && active_oversize_line != Some(line_id))
        {
            // `cells`/`row_ends` may have spare capacity left over from
            // incremental `Vec` growth (e.g. a growth-strategy doubling);
            // since charging is capacity-based, that
            // spare capacity can transiently look like real retained bytes.
            // Shrink once before deciding that a new line genuinely exceeds
            // the whole budget. Once the line is known to be oversized,
            // batched trimming supplies its own allocation headroom.
            line.cells.shrink_to_fit();
            line.row_ends.shrink_to_fit();
            line.recalculate_charge();
        }
        self.charged_bytes = self
            .charged_bytes
            .saturating_sub(prior_charge)
            .saturating_add(line.charged_bytes);

        self.enforce_limit();
        if ends_line {
            self.active_oversize_line = None;
        }
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
        self.active_oversize_line = None;
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
    ) -> Option<LogicalAnchor> {
        for line in &self.lines {
            if absolute_row < line.physical_rows {
                return Some(line.anchor_for_row(absolute_row, column));
            }
            absolute_row -= line.physical_rows;
        }
        None
    }

    /// Resolves a stable logical anchor back into a (column, absolute row)
    /// position using the current (possibly just-reflowed) physical-row
    /// boundaries of the line it names. Returns `None` if the line no
    /// longer exists (for example, evicted during reflow).
    pub(crate) fn resolve_anchor(&self, anchor: LogicalAnchor) -> Option<(usize, usize)> {
        let mut absolute_row = 0;
        for line in &self.lines {
            if line.id == anchor.line_id {
                if anchor.end_boundary
                    && line.trimmed_offset > anchor.trimmed_offset_at_capture
                    && anchor.offset <= line.trimmed_offset
                {
                    return None;
                }
                let (row, column) = line.locate_offset(anchor.offset, anchor.end_boundary)?;
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
                let removed_owned_bytes = line.cells[keep_cell_end..]
                    .iter()
                    .map(cell_owned_charge)
                    .fold(0usize, usize::saturating_add);
                line.cells.truncate(keep_cell_end);
                line.row_ends.truncate(keep_rows);
                // Capacity is charged, so truncating alone would leave the
                // retained prefix billed for the removed live-screen tail.
                line.cells.shrink_to_fit();
                line.row_ends.shrink_to_fit();
                line.physical_rows = keep_rows;
                line.hard_break = false;
                line.cell_owned_bytes = line.cell_owned_bytes.saturating_sub(removed_owned_bytes);
                line.recalculate_charge();
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
            .saturating_add(self.total_physical_rows() as u64);
        rows
    }

    /// Brings retained content back under `limit_bytes`, first by evicting
    /// whole completed lines from the front (oldest first), then - if a
    /// single still-open line by itself exceeds the budget - by
    /// incrementally trimming that line's own oldest physical rows from its
    /// front. This never opens a coordinate gap: every row removed here is
    /// removed from the front of the retained range, so
    /// `content_row_origin + total_physical_rows() == screen_row_origin`
    /// continues to hold for whatever remains.
    fn enforce_limit(&mut self) {
        self.evict_complete_lines();
        if self.charged_bytes <= self.limit_bytes {
            return;
        }

        let Some(line) = self.lines.front_mut() else {
            return;
        };
        debug_assert!(
            !line.hard_break,
            "only a still-open line should remain once complete lines are evicted"
        );
        let line_id = line.id;
        // Compact to half the budget. `Vec` may double cell capacity on the
        // next growth step, so trimming only to 75% can immediately cross the
        // limit again and degenerate back into one front shift per row.
        let target_bytes = self.limit_bytes / 2;
        let mut removed_rows = 0usize;
        while line.charged_bytes > target_bytes && line.physical_rows() > 0 {
            let prior_charge = line.charged_bytes;
            let trimmed_rows = line.trim_oldest_physical_rows_to(target_bytes);
            if trimmed_rows == 0 {
                break;
            }
            #[cfg(test)]
            {
                self.trim_compactions = self.trim_compactions.saturating_add(1);
            }
            removed_rows = removed_rows.saturating_add(trimmed_rows);
            self.charged_bytes = self
                .charged_bytes
                .saturating_sub(prior_charge.saturating_sub(line.charged_bytes));
        }
        self.content_row_origin = self.content_row_origin.saturating_add(removed_rows as u64);
        if removed_rows > 0 {
            if self.active_oversize_line.map(|(id, _)| id) != Some(line_id) {
                self.oversize_lines = self.oversize_lines.saturating_add(1);
            }
            self.active_oversize_line = Some((line_id, line.trimmed_offset));
        }
        if line.physical_rows() == 0 {
            let removed = self.lines.pop_front().expect("front line exists");
            self.charged_bytes = self.charged_bytes.saturating_sub(removed.charged_bytes);
        }
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

    #[cfg(test)]
    pub(crate) const fn trim_compactions(&self) -> u64 {
        self.trim_compactions
    }
}

fn cell_owned_charge(cell: &Cell) -> usize {
    cell.text
        .capacity()
        .saturating_add(cell.hyperlink.as_ref().map_or(0, |target| target.len()))
}

fn compact_line_charge(cell_count: usize, row_count: usize, cell_owned_bytes: usize) -> usize {
    size_of::<LogicalLine>()
        .saturating_add(size_of::<Cell>().saturating_mul(cell_count))
        .saturating_add(cell_owned_bytes)
        .saturating_add(size_of::<usize>().saturating_mul(row_count))
}

#[cfg(test)]
mod tests {
    use super::Scrollback;
    use crate::{cell::blank_cell, screen::ScreenRow};

    #[test]
    fn fully_trimmed_open_line_rejects_stale_anchor_after_recreation() {
        let mut scrollback = Scrollback::new(10_000);
        scrollback.push_rows(vec![ScreenRow {
            cells: vec![blank_cell(); 8],
            soft_wrapped: true,
        }]);
        let anchor = scrollback.line_and_offset_at(0, 0).unwrap();

        scrollback.set_limit_bytes(1);
        assert_eq!(scrollback.total_physical_rows(), 0);

        scrollback.set_limit_bytes(10_000);
        scrollback.push_rows(vec![ScreenRow {
            cells: vec![blank_cell(); 8],
            soft_wrapped: true,
        }]);

        assert_eq!(scrollback.resolve_anchor(anchor), None);
    }

    #[test]
    fn trimmed_end_boundary_does_not_alias_recreated_content() {
        let mut scrollback = Scrollback::new(10_000);
        scrollback.push_rows(vec![ScreenRow {
            cells: vec![blank_cell(); 4],
            soft_wrapped: true,
        }]);
        let anchor = scrollback.line_and_offset_at(0, 7).unwrap();

        scrollback.set_limit_bytes(1);
        scrollback.set_limit_bytes(10_000);
        scrollback.push_rows(vec![ScreenRow {
            cells: vec![blank_cell(); 4],
            soft_wrapped: true,
        }]);

        assert_eq!(scrollback.resolve_anchor(anchor), None);
    }

    #[test]
    fn end_boundary_affinity_stays_on_the_preceding_soft_wrapped_row() {
        let mut scrollback = Scrollback::new(10_000);
        scrollback.push_rows(vec![
            ScreenRow {
                cells: vec![blank_cell(); 4],
                soft_wrapped: true,
            },
            ScreenRow {
                cells: vec![blank_cell(); 2],
                soft_wrapped: true,
            },
        ]);
        let anchor = scrollback.line_and_offset_at(0, 7).unwrap();

        assert_eq!(scrollback.resolve_anchor(anchor), Some((4, 0)));

        scrollback.reflow(3);
        assert_eq!(scrollback.resolve_anchor(anchor), Some((1, 1)));
    }
}
