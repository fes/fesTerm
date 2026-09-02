use festerm_core::{Cell, ContentPosition, Dimensions};

use crate::{
    geometry::{CellPosition, CellRange},
    TerminalSnapshot,
};

/// Local UI selection state. It is deliberately separate from terminal modes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Selection {
    anchor: Option<CellPosition>,
    head: Option<CellPosition>,
    content_anchor: Option<ContentPosition>,
    content_head: Option<ContentPosition>,
    active: bool,
}

impl Selection {
    pub fn begin(&mut self, position: CellPosition) {
        self.begin_at(
            position,
            ContentPosition {
                column: position.column,
                absolute_row: position.row as u64,
            },
        );
    }

    pub(crate) fn begin_at(&mut self, position: CellPosition, content: ContentPosition) {
        self.anchor = Some(position);
        self.head = Some(position);
        self.content_anchor = Some(content);
        self.content_head = Some(content);
        self.active = true;
    }

    pub fn extend(&mut self, position: CellPosition) {
        self.extend_at(
            position,
            ContentPosition {
                column: position.column,
                absolute_row: position.row as u64,
            },
        );
    }

    pub(crate) fn extend_at(&mut self, position: CellPosition, content: ContentPosition) {
        if self.active {
            self.head = Some(position);
            self.content_head = Some(content);
        }
    }

    /// Ends an in-progress selection gesture. A plain click (no drag, so the
    /// released position never differed from the press position) collapses
    /// to no selection at all, rather than leaving a single highlighted
    /// character behind — selection should only ever result from a drag.
    pub fn finish(&mut self) {
        self.active = false;
        if self.content_anchor == self.content_head {
            self.clear();
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub const fn is_active(&self) -> bool {
        self.active
    }

    pub fn range(&self) -> Option<CellRange> {
        Some(CellRange::new(self.anchor?, self.head?))
    }

    pub(crate) fn content_endpoints(&self) -> Option<(ContentPosition, ContentPosition, bool)> {
        Some((self.content_anchor?, self.content_head?, self.active))
    }

    pub(crate) fn remap_content(
        &mut self,
        anchor: ContentPosition,
        head: ContentPosition,
        active: bool,
    ) {
        self.content_anchor = Some(anchor);
        self.content_head = Some(head);
        self.active = active;
    }

    pub(crate) fn clamp_rectangular(&mut self, dimensions: Dimensions) {
        let clamp = |position: ContentPosition| ContentPosition {
            column: position.column.min(dimensions.columns() - 1),
            absolute_row: position.absolute_row.min((dimensions.rows() - 1) as u64),
        };
        self.content_anchor = self.content_anchor.map(clamp);
        self.content_head = self.content_head.map(clamp);
        if let Some(anchor) = self.anchor.as_mut() {
            anchor.column = anchor.column.min(dimensions.columns() - 1);
            anchor.row = anchor.row.min(dimensions.rows() - 1);
        }
        if let Some(head) = self.head.as_mut() {
            head.column = head.column.min(dimensions.columns() - 1);
            head.row = head.row.min(dimensions.rows() - 1);
        }
    }

    pub(crate) fn range_in_snapshot(&self, snapshot: TerminalSnapshot<'_>) -> Option<CellRange> {
        let (start, end) = content_range(self)?;
        let visible_rows = (0..snapshot.dimensions().rows())
            .filter_map(|row| Some((row, snapshot.content_row_for_viewport_row(row)?)))
            .filter(|(_, content_row)| {
                *content_row >= start.absolute_row && *content_row <= end.absolute_row
            })
            .collect::<Vec<_>>();
        let (first_row, first_content) = *visible_rows.first()?;
        let (last_row, last_content) = *visible_rows.last()?;
        Some(CellRange::new(
            CellPosition {
                column: if start.absolute_row < first_content {
                    0
                } else {
                    start.column.min(snapshot.dimensions().columns() - 1)
                },
                row: first_row,
            },
            CellPosition {
                column: if end.absolute_row > last_content {
                    snapshot.dimensions().columns() - 1
                } else {
                    end.column.min(snapshot.dimensions().columns() - 1)
                },
                row: last_row,
            },
        ))
    }
}

fn content_range(selection: &Selection) -> Option<(ContentPosition, ContentPosition)> {
    let anchor = selection.content_anchor?;
    let head = selection.content_head?;
    if (anchor.absolute_row, anchor.column) <= (head.absolute_row, head.column) {
        Some((anchor, head))
    } else {
        Some((head, anchor))
    }
}

/// Moves a selection endpoint from a continuation to its width-two leading
/// cell, so local selection never copies only half a character.
pub fn normalize_selection_position(
    snapshot: TerminalSnapshot<'_>,
    mut position: CellPosition,
) -> Option<CellPosition> {
    if position.column >= snapshot.dimensions().columns()
        || position.row >= snapshot.dimensions().rows()
    {
        return None;
    }
    while position.column > 0
        && snapshot
            .cell(position.column, position.row)
            .is_some_and(Cell::is_continuation)
    {
        position.column -= 1;
    }
    Some(position)
}

/// Returns selected terminal text without interpreting terminal OSC clipboard
/// sequences. Width-two continuation cells do not add a second character.
pub fn selection_text(snapshot: TerminalSnapshot<'_>, selection: &Selection) -> Option<String> {
    let (start, end) = content_range(selection)?;
    if !snapshot.contains_content_row(start.absolute_row)
        || !snapshot.contains_content_row(end.absolute_row)
    {
        return None;
    }
    let mut copied = String::new();
    let mut row = start.absolute_row;
    let mut previous_row = None;
    loop {
        if let Some(previous) = previous_row {
            if row != previous + 1
                || !snapshot
                    .absolute_row_soft_wrapped(previous)
                    .unwrap_or(false)
            {
                copied.push('\n');
            }
        }
        let first = if row == start.absolute_row {
            start.column
        } else {
            0
        };
        let last = if row == end.absolute_row {
            end.column
        } else {
            snapshot.dimensions().columns() - 1
        };
        for column in first..=last {
            match snapshot.absolute_cell(column, row) {
                Some(cell) if !cell.is_continuation() => copied.push_str(cell.text()),
                Some(_) => {}
                None => copied.push(' '),
            }
        }
        if row == end.absolute_row {
            break;
        }
        previous_row = Some(row);
        row = snapshot.next_content_row(row)?;
        if row > end.absolute_row {
            return None;
        }
    }
    Some(copied)
}
