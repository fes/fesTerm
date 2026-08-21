use festerm_core::Cell;

use crate::{
    geometry::{CellPosition, CellRange},
    TerminalSnapshot,
};

/// Local UI selection state. It is deliberately separate from terminal modes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Selection {
    anchor: Option<CellPosition>,
    head: Option<CellPosition>,
    active: bool,
}

impl Selection {
    pub fn begin(&mut self, position: CellPosition) {
        self.anchor = Some(position);
        self.head = Some(position);
        self.active = true;
    }

    pub fn extend(&mut self, position: CellPosition) {
        if self.active {
            self.head = Some(position);
        }
    }

    /// Ends an in-progress selection gesture. A plain click (no drag, so the
    /// released position never differed from the press position) collapses
    /// to no selection at all, rather than leaving a single highlighted
    /// character behind — selection should only ever result from a drag.
    pub fn finish(&mut self) {
        self.active = false;
        if self.anchor == self.head {
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
    let range = selection.range()?;
    let start = normalize_selection_position(snapshot, range.start)?;
    let end = normalize_selection_position(snapshot, range.end)?;
    let range = CellRange::new(start, end);
    let mut copied = String::new();

    for row in range.start.row..=range.end.row {
        if row != range.start.row {
            copied.push('\n');
        }
        let first = if row == range.start.row {
            range.start.column
        } else {
            0
        };
        let last = if row == range.end.row {
            range.end.column
        } else {
            snapshot.dimensions().columns() - 1
        };
        for column in first..=last {
            let cell = snapshot.cell(column, row)?;
            if !cell.is_continuation() {
                copied.push_str(cell.text());
            }
        }
    }
    Some(copied)
}
