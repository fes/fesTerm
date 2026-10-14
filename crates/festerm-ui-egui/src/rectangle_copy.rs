use std::{fmt, mem::size_of};

use festerm_core::{CellWidth, Dimensions};

use crate::{CellPosition, RenderedCell, TerminalRenderCache, TerminalSnapshot};

pub(crate) const RECTANGLE_COPY_MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RectangleCopyError {
    SnapshotTooLarge,
    PointerGestureInProgress,
}

impl fmt::Display for RectangleCopyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SnapshotTooLarge => {
                "Visible terminal content exceeds the 16 MiB rectangle-copy limit."
            }
            Self::PointerGestureInProgress => {
                "Release the mouse buttons before starting rectangle copy."
            }
        })
    }
}

impl std::error::Error for RectangleCopyError {}

pub(crate) struct RectangleCopy {
    pub(crate) dimensions: Dimensions,
    corners: [Option<CellPosition>; 2],
    next_corner: usize,
    trim_trailing_spaces: bool,
    pub(crate) secondary_pressed: bool,
    pub(crate) held_buttons: u8,
}

impl RectangleCopy {
    pub(crate) fn capture(
        snapshot: TerminalSnapshot<'_>,
        trim_trailing_spaces: bool,
        budget: usize,
    ) -> Result<Self, RectangleCopyError> {
        let dimensions = snapshot.dimensions();
        let mut bytes = dimensions
            .cell_count()
            .checked_mul(size_of::<RenderedCell>())
            .and_then(|bytes| bytes.checked_add(dimensions.rows().checked_mul(64)?))
            .ok_or(RectangleCopyError::SnapshotTooLarge)?;
        if bytes > budget {
            return Err(RectangleCopyError::SnapshotTooLarge);
        }
        for row in 0..dimensions.rows() {
            for column in 0..dimensions.columns() {
                if let Some(cell) = snapshot.cell(column, row) {
                    bytes = bytes
                        .checked_add(cell.text().len())
                        .and_then(|bytes| {
                            bytes.checked_add(cell.hyperlink_target().map_or(0, |link| link.len()))
                        })
                        .ok_or(RectangleCopyError::SnapshotTooLarge)?;
                    if bytes > budget {
                        return Err(RectangleCopyError::SnapshotTooLarge);
                    }
                }
            }
        }
        Ok(Self {
            dimensions,
            corners: [None; 2],
            next_corner: 0,
            trim_trailing_spaces,
            secondary_pressed: false,
            held_buttons: 0,
        })
    }

    pub(crate) fn click(&mut self, position: CellPosition) {
        debug_assert!(
            position.column < self.dimensions.columns() && position.row < self.dimensions.rows()
        );
        self.corners[self.next_corner] = Some(position);
        self.next_corner ^= 1;
    }

    pub(crate) fn bounds(&self) -> Option<(CellPosition, CellPosition)> {
        let first = self.corners[0]?;
        let second = self.corners[1].unwrap_or(first);
        Some((
            CellPosition {
                column: first.column.min(second.column),
                row: first.row.min(second.row),
            },
            CellPosition {
                column: first.column.max(second.column),
                row: first.row.max(second.row),
            },
        ))
    }

    pub(crate) fn status(&self) -> &'static str {
        match self.corners {
            [None, _] => "Rectangle copy (frozen): click the first corner; Escape cancels.",
            [Some(_), None] => {
                "Rectangle copy (frozen): click the opposite corner; Escape cancels."
            }
            [Some(_), Some(_)] => {
                "Rectangle copy (frozen): Enter/right-click copies; Escape cancels."
            }
        }
    }

    pub(crate) fn selected_text(&self, cache: &TerminalRenderCache) -> Option<String> {
        self.corners[1]?;
        let (first, last) = self.bounds()?;
        let mut copied = String::new();
        for row in first.row..=last.row {
            if row != first.row {
                copied.push('\n');
            }
            let cells = cache
                .row(row)
                .expect("frozen copy retains every admitted visible row");
            let start =
                if first.column > 0 && cells[first.column].width() == CellWidth::Continuation {
                    first.column - 1
                } else {
                    first.column
                };
            for cell in &cells[start..=last.column] {
                if cell.width() != CellWidth::Continuation {
                    copied.push_str(cell.text());
                }
            }
            if self.trim_trailing_spaces {
                while copied.ends_with(' ') {
                    copied.pop();
                }
            }
        }
        Some(copied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_core::Terminal;

    fn fixture(
        columns: usize,
        rows: usize,
        bytes: &[u8],
        trim: bool,
    ) -> (Terminal, TerminalRenderCache, RectangleCopy) {
        let mut terminal = Terminal::new(Dimensions::new(columns, rows).unwrap()).unwrap();
        terminal.ingest(bytes);
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        let copy = RectangleCopy::capture(snapshot, trim, RECTANGLE_COPY_MAX_BYTES).unwrap();
        let mut cache = TerminalRenderCache::default();
        cache.update(snapshot, &[]);
        (terminal, cache, copy)
    }

    #[test]
    fn rectangle_copy_alternates_corners_and_requires_two_clicks() {
        let (_, cache, mut copy) = fixture(8, 3, b"abcdefgh\r\n12345678\r\nABCDEFGH", true);
        copy.click(CellPosition { column: 5, row: 2 });
        assert_eq!(copy.selected_text(&cache), None);
        copy.click(CellPosition { column: 2, row: 0 });
        assert_eq!(
            copy.selected_text(&cache).as_deref(),
            Some("cdef\n3456\nCDEF")
        );
        copy.click(CellPosition { column: 4, row: 1 });
        assert_eq!(copy.selected_text(&cache).as_deref(), Some("cde\n345"));
        copy.click(CellPosition { column: 4, row: 1 });
        assert_eq!(copy.selected_text(&cache).as_deref(), Some("5"));
    }

    #[test]
    fn rectangle_copy_trims_only_ascii_row_ends_and_preserves_blank_rows() {
        let (_, cache, mut copy) = fixture(8, 3, "A \u{a0}  \r\n\r\n B  ".as_bytes(), true);
        copy.click(CellPosition { column: 0, row: 0 });
        copy.click(CellPosition { column: 5, row: 2 });
        assert_eq!(
            copy.selected_text(&cache).as_deref(),
            Some("A \u{a0}\n\n B")
        );
        copy.trim_trailing_spaces = false;
        assert_eq!(
            copy.selected_text(&cache).as_deref(),
            Some("A \u{a0}   \n      \n B    ")
        );
    }

    #[test]
    fn rectangle_copy_keeps_screen_rows_separate_and_excludes_hyperlink_targets() {
        let (_, cache, mut copy) = fixture(
            4,
            2,
            b"\x1b]8;;https://example.invalid/private\x1b\\abcdefgh\x1b]8;;\x1b\\",
            true,
        );
        copy.click(CellPosition { column: 1, row: 0 });
        copy.click(CellPosition { column: 2, row: 1 });
        assert_eq!(copy.selected_text(&cache).as_deref(), Some("bc\nfg"));
    }

    #[test]
    fn rectangle_copy_keeps_intersecting_wide_and_combining_graphemes_whole() {
        let (_, cache, mut copy) =
            fixture(8, 2, "A\u{754c}e\u{301}\r\nB\u{1f642}Z".as_bytes(), true);
        copy.click(CellPosition { column: 2, row: 0 });
        copy.click(CellPosition { column: 3, row: 1 });
        assert_eq!(
            copy.selected_text(&cache).as_deref(),
            Some("\u{754c}e\u{301}\n\u{1f642}Z")
        );
        copy.click(CellPosition { column: 1, row: 0 });
        copy.click(CellPosition { column: 1, row: 1 });
        assert_eq!(
            copy.selected_text(&cache).as_deref(),
            Some("\u{754c}\n\u{1f642}")
        );
    }

    #[test]
    fn rectangle_copy_reads_frozen_rows_after_live_output_resize_and_screen_switch() {
        let (mut terminal, cache, mut copy) = fixture(8, 2, b"original\r\nsnapshot", true);
        copy.click(CellPosition { column: 0, row: 0 });
        copy.click(CellPosition { column: 7, row: 1 });
        terminal.ingest(b"\x1b[2J\x1b[Hchanged\x1b[?1049hnew screen");
        terminal.resize(Dimensions::new(4, 4).unwrap()).unwrap();
        assert_eq!(
            copy.selected_text(&cache).as_deref(),
            Some("original\nsnapshot")
        );
    }

    #[test]
    fn rectangle_copy_admission_checks_exact_cell_text_and_link_budgets() {
        let mut terminal = Terminal::new(Dimensions::new(2, 1).unwrap()).unwrap();
        terminal.ingest(b"\x1b]8;;https://example.invalid/path\x1b\\A\x1b]8;;\x1b\\B");
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        let bytes = 2 * size_of::<RenderedCell>() + 64 + 2 + "https://example.invalid/path".len();
        assert!(RectangleCopy::capture(snapshot, true, bytes).is_ok());
        assert!(matches!(
            RectangleCopy::capture(snapshot, true, bytes - 1),
            Err(RectangleCopyError::SnapshotTooLarge)
        ));
        assert!(matches!(
            RectangleCopy::capture(snapshot, true, 0),
            Err(RectangleCopyError::SnapshotTooLarge)
        ));
    }
}
