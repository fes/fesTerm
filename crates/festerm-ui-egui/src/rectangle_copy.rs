use std::{fmt, mem::size_of};

use festerm_core::{CellWidth, ContentPosition, Dimensions};

use crate::{
    selection::{collect_bounded_text, TextWork},
    CellPosition, RenderedCell, SelectedTextError, TerminalContextText, TerminalRenderCache,
    TerminalSnapshot, TERMINAL_CONTEXT_MAX_BYTES, TERMINAL_CONTEXT_MAX_ROWS,
};

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
    primary_drag: Option<PrimaryDrag>,
    trim_trailing_spaces: bool,
    pub(crate) secondary_pressed: bool,
    pub(crate) held_buttons: u8,
    rows: Vec<FrozenRow>,
}

struct FrozenRow {
    absolute_row: u64,
    soft_wrapped: bool,
    occupied_columns: usize,
}

struct PrimaryDrag {
    anchor: CellPosition,
    moved: bool,
}

impl RectangleCopy {
    pub(crate) fn capture(
        snapshot: TerminalSnapshot<'_>,
        trim_trailing_spaces: bool,
        budget: usize,
    ) -> Result<Self, RectangleCopyError> {
        let dimensions = snapshot.dimensions();
        let mut bytes = TerminalRenderCache::frozen_storage_bytes(dimensions)
            .and_then(|bytes| bytes.checked_add(size_of::<Self>()))
            .and_then(|bytes| {
                bytes.checked_add(dimensions.rows().checked_mul(size_of::<FrozenRow>())?)
            })
            .ok_or(RectangleCopyError::SnapshotTooLarge)?;
        if bytes > budget {
            return Err(RectangleCopyError::SnapshotTooLarge);
        }
        for row in 0..dimensions.rows() {
            for column in 0..dimensions.columns() {
                if let Some(cell) = snapshot.cell(column, row) {
                    bytes = bytes
                        .checked_add(
                            RenderedCell::text_heap_bytes(cell.text())
                                .ok_or(RectangleCopyError::SnapshotTooLarge)?,
                        )
                        .and_then(|bytes| {
                            // Charge each link independently, including Arc counters/alignment,
                            // even though the cache normally shares the core's allocation.
                            let link_bytes = cell.hyperlink_target().map_or(Some(0), |link| {
                                link.len().checked_add(3 * size_of::<usize>() - 1)
                            })?;
                            bytes.checked_add(link_bytes)
                        })
                        .ok_or(RectangleCopyError::SnapshotTooLarge)?;
                    if bytes > budget {
                        return Err(RectangleCopyError::SnapshotTooLarge);
                    }
                }
            }
        }
        let mut rows = Vec::with_capacity(dimensions.rows());
        rows.extend((0..dimensions.rows()).map(|row| {
            let absolute_row = snapshot
                .content_row_for_viewport_row(row)
                .expect("visible row exists");
            FrozenRow {
                absolute_row,
                soft_wrapped: snapshot
                    .absolute_row_soft_wrapped(absolute_row)
                    .expect("visible row retains wrap metadata"),
                occupied_columns: snapshot
                    .absolute_row_occupied_columns(absolute_row)
                    .expect("visible row retains occupied-column metadata"),
            }
        }));
        Ok(Self {
            dimensions,
            corners: [None; 2],
            next_corner: 0,
            primary_drag: None,
            trim_trailing_spaces,
            secondary_pressed: false,
            held_buttons: 0,
            rows,
        })
    }

    fn click(&mut self, position: CellPosition) {
        debug_assert!(
            position.column < self.dimensions.columns() && position.row < self.dimensions.rows()
        );
        self.corners[self.next_corner] = Some(position);
        self.next_corner ^= 1;
    }

    pub(crate) fn primary_press(&mut self, position: CellPosition) {
        self.click(position);
        self.primary_drag = Some(PrimaryDrag {
            anchor: position,
            moved: false,
        });
    }

    pub(crate) fn primary_move(&mut self, position: CellPosition) -> bool {
        let Some(drag) = self.primary_drag.as_mut() else {
            return false;
        };
        if !drag.moved && position == drag.anchor {
            return false;
        }
        debug_assert!(
            position.column < self.dimensions.columns() && position.row < self.dimensions.rows()
        );
        let corners = [Some(drag.anchor), Some(position)];
        let changed = self.corners != corners;
        self.corners = corners;
        self.next_corner = 0;
        drag.moved = true;
        changed
    }

    pub(crate) fn primary_release(&mut self) {
        self.primary_drag = None;
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
            [None, _] => {
                "Rectangle copy (frozen): click a corner or drag a rectangle; Escape cancels."
            }
            [Some(_), None] => {
                "Rectangle copy (frozen): click opposite corner or drag; Escape cancels."
            }
            [Some(_), Some(_)] => {
                "Rectangle copy (frozen): Enter copies; right-click opens Copy/path/link menu; Escape cancels."
            }
        }
    }

    pub(crate) fn selected_text(&self, cache: &TerminalRenderCache) -> Option<String> {
        self.selected_text_impl(cache, usize::MAX, false)
            .expect("rectangle selected text fits in a String")
    }

    pub(crate) fn selected_text_with_limit(
        &self,
        cache: &TerminalRenderCache,
        max_bytes: usize,
    ) -> Result<Option<String>, SelectedTextError> {
        self.selected_text_impl(cache, max_bytes, true)
    }

    fn selected_text_impl(
        &self,
        cache: &TerminalRenderCache,
        max_bytes: usize,
        limited_work: bool,
    ) -> Result<Option<String>, SelectedTextError> {
        if self.corners[1].is_none() {
            return Ok(None);
        }
        let Some((first, last)) = self.bounds() else {
            return Ok(None);
        };
        self.text_in_bounds_bounded(cache, first, last, max_bytes, limited_work)
    }

    pub(crate) fn context_copy_text(&self, cache: &TerminalRenderCache) -> String {
        let (first, last) = self.context_bounds();
        self.text_in_bounds(cache, first, last)
    }

    fn context_bounds(&self) -> (CellPosition, CellPosition) {
        self.bounds().unwrap_or((
            CellPosition { column: 0, row: 0 },
            CellPosition {
                column: self.dimensions.columns() - 1,
                row: self.dimensions.rows() - 1,
            },
        ))
    }

    pub(crate) fn contains(&self, position: CellPosition) -> bool {
        let (first, last) = self.context_bounds();
        position.column >= first.column
            && position.column <= last.column
            && position.row >= first.row
            && position.row <= last.row
    }

    pub(crate) fn content_position(&self, position: CellPosition) -> ContentPosition {
        ContentPosition {
            column: position.column,
            absolute_row: self.rows[position.row].absolute_row,
        }
    }

    pub(crate) fn context_text(
        &self,
        cache: &TerminalRenderCache,
        position: CellPosition,
    ) -> Option<TerminalContextText> {
        if !self.contains(position) {
            return None;
        }
        let (mut first, mut last) = self.context_bounds();
        let rectangle = self.bounds().is_some();
        if !rectangle {
            first.row = position.row;
            last.row = position.row;
            while first.row > 0 && self.rows[first.row - 1].soft_wrapped {
                first.row -= 1;
                if last.row - first.row + 1 > TERMINAL_CONTEXT_MAX_ROWS {
                    return None;
                }
            }
            while last.row + 1 < self.dimensions.rows() && self.rows[last.row].soft_wrapped {
                last.row += 1;
                if last.row - first.row + 1 > TERMINAL_CONTEXT_MAX_ROWS {
                    return None;
                }
            }
        }
        if last.row - first.row + 1 > TERMINAL_CONTEXT_MAX_ROWS {
            return None;
        }
        let mut text = String::new();
        let mut clicked_range = None;
        for (offset, frozen_row) in self.rows[first.row..=last.row].iter().enumerate() {
            let row = first.row + offset;
            let cells = cache.row(row)?;
            let start = first.column.saturating_sub(usize::from(
                cells[first.column].width() == CellWidth::Continuation,
            ));
            let end = if rectangle {
                last.column + 1
            } else {
                frozen_row.occupied_columns
            };
            for (column, cell) in cells.iter().enumerate().take(end).skip(start) {
                if cell.width() == CellWidth::Continuation {
                    continue;
                }
                let byte_start = text.len();
                if byte_start + cell.text().len() > TERMINAL_CONTEXT_MAX_BYTES {
                    return None;
                }
                text.push_str(cell.text());
                if row == position.row
                    && position.column >= column
                    && position.column < column + cell.width().columns().max(1)
                {
                    clicked_range = Some(byte_start..text.len());
                }
            }
        }
        Some(TerminalContextText {
            text,
            clicked_range: clicked_range?,
        })
    }

    fn text_in_bounds(
        &self,
        cache: &TerminalRenderCache,
        first: CellPosition,
        last: CellPosition,
    ) -> String {
        self.text_in_bounds_bounded(cache, first, last, usize::MAX, false)
            .expect("rectangle text fits in a String")
            .expect("rectangle bounds contain text")
    }

    fn text_in_bounds_bounded(
        &self,
        cache: &TerminalRenderCache,
        first: CellPosition,
        last: CellPosition,
        max_bytes: usize,
        limited_work: bool,
    ) -> Result<Option<String>, SelectedTextError> {
        collect_bounded_text(max_bytes, TextWork::new(limited_work), |append, work| {
            for row in first.row..=last.row {
                work.charge(1)?;
                if row != first.row {
                    append("\n")?;
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
                let cells = &cells[start..=last.column];
                // Determine trimmed bounds without first copying all padding.
                let end = if self.trim_trailing_spaces {
                    let mut end = 0;
                    for (index, cell) in cells.iter().enumerate().rev() {
                        work.charge(1)?;
                        if cell.width() != CellWidth::Continuation {
                            work.charge(cell.text().len())?;
                            if !cell.text().trim_end_matches(' ').is_empty() {
                                end = index + 1;
                                break;
                            }
                        }
                    }
                    end
                } else {
                    cells.len()
                };
                for (index, cell) in cells[..end].iter().enumerate() {
                    work.charge(1)?;
                    if cell.width() != CellWidth::Continuation {
                        let text = if self.trim_trailing_spaces && index + 1 == end {
                            work.charge(cell.text().len())?;
                            cell.text().trim_end_matches(' ')
                        } else {
                            cell.text()
                        };
                        append(text)?;
                    }
                }
            }
            Ok(Some(()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_core::Terminal;

    #[test]
    fn rectangle_context_capture_wraps_at_selected_edges_and_keeps_copy_rows_separate() {
        let (mut terminal, cache, mut copy) = fixture(
            24,
            3,
            b"left https://examp zz\r\nleft le.com/a     zz",
            true,
        );
        copy.click(CellPosition { column: 5, row: 0 });
        copy.click(CellPosition { column: 17, row: 1 });
        terminal.ingest(b"\x1b[2J\x1b[Hhttps://wrong.example/");
        let capture = copy
            .context_text(&cache, CellPosition { column: 7, row: 1 })
            .unwrap();
        assert_eq!(capture.text, "https://example.com/a     ");
        assert_eq!(&capture.text[capture.clicked_range], ".");
        assert_eq!(
            copy.selected_text(&cache).as_deref(),
            Some("https://examp\nle.com/a")
        );
        assert!(copy
            .context_text(&cache, CellPosition { column: 4, row: 0 })
            .is_none());
        assert!(copy
            .context_text(&cache, CellPosition { column: 5, row: 2 })
            .is_none());
    }

    #[test]
    fn rectangle_context_capture_without_corners_uses_frozen_terminal_soft_wraps() {
        let (terminal, cache, copy) = fixture(8, 3, b"abcdefghijk\r\nseparate", true);
        let capture = copy
            .context_text(&cache, CellPosition { column: 1, row: 1 })
            .unwrap();
        assert_eq!(capture.text, "abcdefghijk");
        assert_eq!(&capture.text[capture.clicked_range], "j");
        assert_eq!(copy.context_copy_text(&cache), "abcdefgh\nijk\nseparate");
        assert_eq!(terminal.screen().row_soft_wrapped(0), Some(true));
    }

    #[test]
    fn rectangle_context_capture_nonzero_start_preserves_absolute_row_index() {
        let (_, cache, mut copy) = fixture(8, 4, b"ignored\r\nabcdefghijk\r\nseparate", true);
        let capture = copy
            .context_text(&cache, CellPosition { column: 1, row: 2 })
            .unwrap();
        assert_eq!(capture.text, "abcdefghijk");
        assert_eq!(&capture.text[capture.clicked_range], "j");
        copy.click(CellPosition { column: 1, row: 2 });
        copy.click(CellPosition { column: 2, row: 2 });
        let capture = copy
            .context_text(&cache, CellPosition { column: 1, row: 2 })
            .unwrap();
        assert_eq!(capture.text, "jk");
        assert_eq!(&capture.text[capture.clicked_range], "j");
    }

    #[test]
    fn rectangle_context_capture_preserves_intersecting_unicode_graphemes() {
        let (_, cache, mut copy) = fixture(8, 2, "\u{754c}e\u{301} text".as_bytes(), true);
        copy.click(CellPosition { column: 1, row: 0 });
        copy.click(CellPosition { column: 2, row: 0 });
        let capture = copy
            .context_text(&cache, CellPosition { column: 1, row: 0 })
            .unwrap();
        assert_eq!(capture.text, "\u{754c}e\u{301}");
        assert_eq!(&capture.text[capture.clicked_range], "\u{754c}");
    }

    #[test]
    fn rectangle_context_capture_limits_rows_and_bytes_without_limiting_copy() {
        let (_, cache, mut copy) = fixture(2, TERMINAL_CONTEXT_MAX_ROWS, b"hi", true);
        copy.click(CellPosition { column: 0, row: 0 });
        copy.click(CellPosition {
            column: 1,
            row: TERMINAL_CONTEXT_MAX_ROWS - 1,
        });
        assert!(copy
            .context_text(&cache, CellPosition { column: 0, row: 0 })
            .is_some());
        let (_, cache, mut copy) = fixture(2, TERMINAL_CONTEXT_MAX_ROWS + 1, b"hi", true);
        copy.click(CellPosition { column: 0, row: 0 });
        copy.click(CellPosition {
            column: 1,
            row: TERMINAL_CONTEXT_MAX_ROWS,
        });
        assert!(copy
            .context_text(&cache, CellPosition { column: 0, row: 0 })
            .is_none());
        assert!(copy.selected_text(&cache).is_some());
        let text = "x".repeat(TERMINAL_CONTEXT_MAX_BYTES);
        let (_, cache, copy) = fixture(text.len(), 1, text.as_bytes(), true);
        assert_eq!(
            copy.context_text(&cache, CellPosition { column: 0, row: 0 })
                .unwrap()
                .text,
            text
        );
        let text = "x".repeat(TERMINAL_CONTEXT_MAX_BYTES + 1);
        let (_, cache, copy) = fixture(text.len(), 1, text.as_bytes(), true);
        assert!(copy
            .context_text(&cache, CellPosition { column: 0, row: 0 })
            .is_none());
        assert_eq!(copy.context_copy_text(&cache), text);
    }

    #[test]
    fn rectangle_context_capture_menu_copy_uses_single_corner() {
        let (_, cache, mut copy) = fixture(8, 2, b"first\r\nsecond", true);
        copy.click(CellPosition { column: 1, row: 1 });
        assert!(copy.selected_text(&cache).is_none());
        assert_eq!(copy.context_copy_text(&cache), "e");
        assert!(copy
            .context_text(&cache, CellPosition { column: 0, row: 1 })
            .is_none());
    }

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
    fn bounded_rectangle_copy_counts_utf8_newlines_and_uses_frozen_text() {
        let (mut terminal, cache, mut copy) = fixture(8, 2, "A界e\u{301}\r\nB🙂Z".as_bytes(), true);
        assert_eq!(copy.selected_text_with_limit(&cache, 0), Ok(None));
        copy.click(CellPosition { column: 2, row: 0 });
        assert_eq!(copy.selected_text_with_limit(&cache, 0), Ok(None));
        copy.click(CellPosition { column: 3, row: 1 });
        terminal.ingest(b"\x1b[2J\x1b[Hchanged");
        terminal.resize(Dimensions::new(4, 4).unwrap()).unwrap();
        let expected = "界e\u{301}\n🙂Z";
        for limit in 0..=expected.len() + 1 {
            let result = copy.selected_text_with_limit(&cache, limit);
            if limit < expected.len() {
                assert_eq!(result, Err(SelectedTextError::MaxBytesExceeded));
            } else {
                assert_eq!(result.as_ref().unwrap().as_deref(), Some(expected));
            }
        }
        assert_eq!(copy.selected_text(&cache).as_deref(), Some(expected));
    }

    #[test]
    fn bounded_rectangle_copy_excludes_trimmed_padding_before_applying_limit() {
        for trim in [false, true] {
            let (_, cache, mut copy) = fixture(4096, 3, b"a\r\n\r\nb", trim);
            copy.click(CellPosition { column: 0, row: 0 });
            copy.click(CellPosition {
                column: 4095,
                row: 2,
            });
            let expected = if trim {
                "a\n\nb".to_owned()
            } else {
                format!(
                    "a{}\n{}\nb{}",
                    " ".repeat(4095),
                    " ".repeat(4096),
                    " ".repeat(4095)
                )
            };
            assert_eq!(
                copy.selected_text_with_limit(&cache, expected.len()),
                Ok(Some(expected.clone()))
            );
            assert_eq!(
                copy.selected_text_with_limit(&cache, expected.len() - 1),
                Err(SelectedTextError::MaxBytesExceeded)
            );
            assert_eq!(copy.selected_text(&cache), Some(expected));
        }
        let (_, cache, mut copy) = fixture(8, 1, b"", true);
        copy.click(CellPosition { column: 0, row: 0 });
        copy.click(CellPosition { column: 7, row: 0 });
        assert_eq!(
            copy.selected_text_with_limit(&cache, 0),
            Ok(Some(String::new()))
        );
    }

    #[test]
    fn bounded_rectangle_copy_refuses_oversized_trimmed_whitespace_traversal() {
        let (_, cache, mut copy) = fixture(1024, 40, b"", true);
        copy.click(CellPosition { column: 0, row: 0 });
        copy.click(CellPosition {
            column: 1023,
            row: 39,
        });
        let expected = "\n".repeat(39);
        assert_eq!(
            copy.selected_text_with_limit(&cache, expected.len()),
            Err(SelectedTextError::WorkLimitExceeded)
        );
        assert_eq!(
            copy.selected_text_with_limit(&cache, usize::MAX),
            Err(SelectedTextError::WorkLimitExceeded)
        );
        assert_eq!(copy.selected_text(&cache), Some(expected));
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
    fn rectangle_copy_primary_clicks_preserve_alternating_corners() {
        let (_, cache, mut copy) = fixture(8, 3, b"abcdefgh\r\n12345678\r\nABCDEFGH", true);
        let first = CellPosition { column: 5, row: 2 };
        copy.primary_press(first);
        assert!(!copy.primary_move(first));
        copy.primary_release();
        assert_eq!(copy.selected_text(&cache), None);
        copy.primary_press(CellPosition { column: 2, row: 0 });
        copy.primary_release();
        assert_eq!(
            copy.selected_text(&cache).as_deref(),
            Some("cdef\n3456\nCDEF")
        );
        copy.primary_press(CellPosition { column: 4, row: 1 });
        copy.primary_release();
        assert_eq!(copy.selected_text(&cache).as_deref(), Some("cde\n345"));
    }

    #[test]
    fn rectangle_copy_drag_replaces_both_corners_and_restores_click_alternation() {
        for existing_corners in 0..=2 {
            let (_, cache, mut copy) = fixture(8, 3, b"abcdefgh\r\n12345678\r\nABCDEFGH", true);
            for _ in 0..existing_corners {
                copy.click(CellPosition { column: 7, row: 0 });
            }
            copy.primary_press(CellPosition { column: 5, row: 2 });
            assert!(copy.primary_move(CellPosition { column: 2, row: 0 }));
            assert_eq!(
                copy.selected_text(&cache).as_deref(),
                Some("cdef\n3456\nCDEF")
            );
            assert!(copy.primary_move(CellPosition { column: 3, row: 1 }));
            copy.primary_release();
            assert_eq!(copy.selected_text(&cache).as_deref(), Some("456\nDEF"));
            copy.primary_press(CellPosition { column: 0, row: 0 });
            copy.primary_release();
            assert_eq!(copy.selected_text(&cache).as_deref(), Some("abcd\n1234"));
            copy.primary_press(CellPosition { column: 0, row: 0 });
            copy.primary_release();
            assert_eq!(copy.selected_text(&cache).as_deref(), Some("a"));
        }
    }

    #[test]
    fn rectangle_copy_drag_can_return_to_anchor_and_ignores_unowned_motion() {
        let (_, cache, mut copy) = fixture(8, 2, b"abcdefgh\r\n12345678", true);
        let anchor = CellPosition { column: 1, row: 1 };
        let other = CellPosition { column: 4, row: 0 };
        assert!(!copy.primary_move(other));
        assert!(copy.bounds().is_none());
        copy.primary_press(anchor);
        assert!(copy.primary_move(other));
        assert!(copy.primary_move(anchor));
        assert_eq!(copy.selected_text(&cache).as_deref(), Some("2"));
        copy.primary_release();
        assert!(!copy.primary_move(other));
        assert_eq!(copy.selected_text(&cache).as_deref(), Some("2"));
        copy.primary_press(other);
        let bounds = copy.bounds();
        copy.primary_release();
        assert!(!copy.primary_move(anchor));
        assert_eq!(copy.bounds(), bounds);
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
    fn rectangle_copy_admission_counts_heap_backed_combining_grapheme_capacity() {
        let dimensions = Dimensions::new(450, 450).unwrap();
        let mut terminal = Terminal::new(dimensions).unwrap();
        let grapheme = format!("a{}", "\u{301}".repeat(12));
        assert_eq!(grapheme.len(), 25);
        terminal.ingest(grapheme.repeat(dimensions.cell_count()).as_bytes());
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        let owned = RenderedCell::from_core(snapshot.cell(0, 0).unwrap());
        assert!(owned.text.is_heap_allocated());
        let actual_cells =
            dimensions.cell_count() * (size_of::<RenderedCell>() + owned.text.capacity());
        assert!(actual_cells > RECTANGLE_COPY_MAX_BYTES);
        assert!(matches!(
            RectangleCopy::capture(snapshot, true, RECTANGLE_COPY_MAX_BYTES),
            Err(RectangleCopyError::SnapshotTooLarge)
        ));
        let mut terminal = Terminal::new(Dimensions::new(2, 1).unwrap()).unwrap();
        terminal.ingest(grapheme.as_bytes());
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        let bytes = TerminalRenderCache::frozen_storage_bytes(snapshot.dimensions()).unwrap()
            + size_of::<RectangleCopy>()
            + size_of::<FrozenRow>()
            + RenderedCell::text_heap_bytes(&grapheme).unwrap();
        assert!(RectangleCopy::capture(snapshot, true, bytes).is_ok());
        assert!(RectangleCopy::capture(snapshot, true, bytes + 1).is_ok());
        assert!(matches!(
            RectangleCopy::capture(snapshot, true, bytes - 1),
            Err(RectangleCopyError::SnapshotTooLarge)
        ));
    }

    #[test]
    fn rectangle_copy_admission_checks_exact_cell_text_and_link_budgets() {
        let mut terminal = Terminal::new(Dimensions::new(2, 1).unwrap()).unwrap();
        terminal.ingest(b"\x1b]8;;https://example.invalid/path\x1b\\A\x1b]8;;\x1b\\B");
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        let bytes = TerminalRenderCache::frozen_storage_bytes(snapshot.dimensions()).unwrap()
            + size_of::<RectangleCopy>()
            + size_of::<FrozenRow>()
            + "https://example.invalid/path".len()
            + 3 * size_of::<usize>()
            - 1;
        assert!(RectangleCopy::capture(snapshot, true, bytes).is_ok());
        assert!(RectangleCopy::capture(snapshot, true, bytes + 1).is_ok());
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
