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
    ///
    /// This deliberately compares the grid-relative `anchor`/`head` (each
    /// row `0..rows`, updated fresh from every event's own on-screen
    /// position) rather than `content_anchor`/`content_head` (absolute
    /// scrollback rows). While the pointer hasn't actually moved, heavy PTY
    /// output arriving between press and release advances the absolute row
    /// a stationary on-screen cell maps to - comparing content positions
    /// would then see a "moved" endpoint and wrongly treat an ordinary
    /// click during heavy output as a drag, leaving a stray one-cell
    /// selection behind for a click that never left its starting cell.
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

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_core::{
        ContentPosition, Dimensions, InputEventOutcome, Modifiers, MouseButton, MouseEvent,
        MouseEventKind, Terminal,
    };

    use crate::{
        input::{route_mouse_input, EncodedInputSink},
        TerminalSnapshot,
    };

    #[derive(Default)]
    struct Sink(Vec<Vec<u8>>);

    impl EncodedInputSink for Sink {
        fn record_encoded_input(&mut self, bytes: &[u8]) {
            self.0.push(bytes.to_vec());
        }
    }

    fn terminal(columns: usize, rows: usize) -> Terminal {
        Terminal::new(Dimensions::new(columns, rows).expect("valid test size"))
            .expect("test terminal allocation")
    }

    #[test]
    fn selection_expands_continuations_and_copies_leading_text() {
        let mut terminal = terminal(8, 1);
        terminal.ingest("A界e".as_bytes());
        terminal.take_dirty_rows();
        let mut selection = Selection::default();
        let mut sink = Sink::default();

        let press = route_mouse_input(
            &mut terminal,
            MouseEvent {
                kind: MouseEventKind::Press(MouseButton::Left),
                column: 2,
                row: 0,
                modifiers: Modifiers::NONE,
            },
            &mut selection,
            &mut sink,
        );
        assert_eq!(press.outcome, InputEventOutcome::SelectionAllowed);
        let release = route_mouse_input(
            &mut terminal,
            MouseEvent {
                kind: MouseEventKind::Release(MouseButton::Left),
                column: 3,
                row: 0,
                modifiers: Modifiers::NONE,
            },
            &mut selection,
            &mut sink,
        );
        assert_eq!(release.outcome, InputEventOutcome::SelectionAllowed);
        assert_eq!(
            selection.range(),
            Some(CellRange::new(
                CellPosition { column: 1, row: 0 },
                CellPosition { column: 3, row: 0 }
            ))
        );
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &selection),
            Some("界e".to_owned())
        );
        assert!(sink.0.is_empty());
    }

    #[test]
    fn selection_copy_does_not_insert_newlines_at_soft_wraps() {
        let mut terminal = terminal(4, 2);
        terminal.ingest(b"abcdefgh");
        let mut selection = Selection::default();
        selection.begin(CellPosition { column: 0, row: 0 });
        selection.extend(CellPosition { column: 3, row: 1 });
        selection.finish();

        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &selection),
            Some("abcdefgh".to_owned())
        );
    }

    #[test]
    fn selection_copy_treats_trimmed_history_cells_as_blank_padding() {
        let mut terminal = terminal(8, 2);
        terminal.ingest(b"abc\r\ndef\r\nghi");
        let mut selection = Selection::default();
        selection.begin_at(
            CellPosition { column: 0, row: 0 },
            ContentPosition {
                column: 0,
                absolute_row: 0,
            },
        );
        selection.extend_at(
            CellPosition { column: 2, row: 1 },
            ContentPosition {
                column: 2,
                absolute_row: 1,
            },
        );
        selection.finish();

        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &selection),
            Some("abc     \ndef".to_owned())
        );
    }

    #[test]
    fn evicted_selection_positions_never_alias_new_history_content() {
        let dimensions = Dimensions::new(8, 2).unwrap();
        let mut terminal = Terminal::with_scrollback_limit(dimensions, 1024).unwrap();
        for line in 0..12 {
            terminal.ingest(format!("line-{line:02}\r\n").as_bytes());
        }
        let selected_row = terminal.scrollback_stats().content_row_origin();
        let mut selection = Selection::default();
        selection.begin_at(
            CellPosition { column: 0, row: 0 },
            ContentPosition {
                column: 0,
                absolute_row: selected_row,
            },
        );
        selection.extend_at(
            CellPosition { column: 3, row: 0 },
            ContentPosition {
                column: 3,
                absolute_row: selected_row,
            },
        );
        selection.finish();

        for line in 12..40 {
            terminal.ingest(format!("line-{line:02}\r\n").as_bytes());
        }
        let snapshot = TerminalSnapshot::from_terminal_viewport(
            &terminal,
            terminal.scrollback_stats().physical_rows(),
        );

        assert!(terminal.scrollback_stats().content_row_origin() > selected_row);
        assert_eq!(selection_text(snapshot, &selection), None);
        assert_eq!(selection.range_in_snapshot(snapshot), None);
    }

    #[test]
    fn discarded_scrollback_rows_never_alias_new_screen_content() {
        let dimensions = Dimensions::new(8, 2).unwrap();
        let mut terminal = Terminal::with_scrollback_limit(dimensions, 0).unwrap();
        terminal.ingest(b"old");
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        let mut selection = Selection::default();
        selection.begin_at(
            CellPosition { column: 0, row: 0 },
            snapshot
                .content_position(CellPosition { column: 0, row: 0 })
                .unwrap(),
        );
        selection.extend_at(
            CellPosition { column: 2, row: 0 },
            snapshot
                .content_position(CellPosition { column: 2, row: 0 })
                .unwrap(),
        );
        selection.finish();

        terminal.ingest(b"\r\nnew\r\nnext");
        let snapshot = TerminalSnapshot::from_terminal(&terminal);

        assert!(terminal.scrollback_stats().screen_row_origin() > 0);
        assert_eq!(selection_text(snapshot, &selection), None);
        assert_eq!(selection.range_in_snapshot(snapshot), None);
    }

    #[test]
    fn retention_after_an_oversized_gap_does_not_reuse_discarded_coordinates() {
        let dimensions = Dimensions::new(8, 2).unwrap();
        let mut terminal = Terminal::with_scrollback_limit(dimensions, 200_000).unwrap();
        terminal.ingest(b"kept\r\noversize");
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        let mut selection = Selection::default();
        selection.begin_at(
            CellPosition { column: 0, row: 1 },
            snapshot
                .content_position(CellPosition { column: 0, row: 1 })
                .unwrap(),
        );
        selection.extend_at(
            CellPosition { column: 3, row: 1 },
            snapshot
                .content_position(CellPosition { column: 3, row: 1 })
                .unwrap(),
        );
        selection.finish();

        // A burst large enough to force incremental front-trimming of its
        // own oldest rows (i.e. large enough that even after this line's
        // *own* stale capacity is shrunk to its real size, it is still over
        // budget and must trim), discarding the very rows the selection
        // above was anchored on.
        terminal.ingest(&vec![b'x'; 2500]);
        terminal.ingest(b"\r\nnew-1\r\nnew-2\r\nnew-3\r\nnew-4");
        let snapshot = TerminalSnapshot::from_terminal_viewport(
            &terminal,
            terminal.scrollback_stats().physical_rows(),
        );

        // The selection anchored on the oversized ("oversize" -> huge burst)
        // line must not be silently aliased onto unrelated new content once
        // that line's own oldest rows are discarded for exceeding the byte
        // budget.
        assert_eq!(selection_text(snapshot, &selection), None);
        assert_eq!(selection.range_in_snapshot(snapshot), None);

        // Whatever history remains retained after the oversized gap must
        // still be addressable through its content coordinates - discarding
        // an oversized line must never leave dangling, unresolvable
        // coordinates for content that is still actually present.
        let history_snapshot = TerminalSnapshot::from_terminal(&terminal);
        assert!(
            (0..terminal.scrollback_stats().physical_rows())
                .filter_map(|row| history_snapshot.content_position(CellPosition { column: 0, row }))
                .count()
                > 0,
            "retained history must still be addressable after the oversized gap"
        );
    }

    #[test]
    fn a_plain_click_without_dragging_leaves_no_selection() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"hello");
        let mut selection = Selection::default();
        let mut sink = Sink::default();

        route_mouse_input(
            &mut terminal,
            MouseEvent {
                kind: MouseEventKind::Press(MouseButton::Left),
                column: 2,
                row: 0,
                modifiers: Modifiers::NONE,
            },
            &mut selection,
            &mut sink,
        );
        route_mouse_input(
            &mut terminal,
            MouseEvent {
                kind: MouseEventKind::Release(MouseButton::Left),
                column: 2,
                row: 0,
                modifiers: Modifiers::NONE,
            },
            &mut selection,
            &mut sink,
        );

        assert_eq!(
            selection.range(),
            None,
            "a click that never moved must not leave a single-character selection"
        );
        assert!(!selection.is_active());
    }
}
