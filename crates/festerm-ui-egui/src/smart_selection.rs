//! Bounded presentation-only inference; the grid remains the source of truth.
use crate::{normalize_selection_position, CellPosition, TerminalSnapshot};

const MAX_CELLS: usize = 32_768;
const MAX_BYTES: usize = 128 * 1024;
const MAX_ROWS: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmartSelectionScope {
    Sentence,
    Paragraph,
    Block,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SmartRow {
    pub row: u64,
    pub first: usize,
    pub last: usize,
    pub text: String,
    pub joined: bool,
    /// Rows broken by TUI painting rather than terminal soft-wrap that belong
    /// to one prose run are rejoined with a single space.
    pub space_join: bool,
    pub soft_wrapped: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SmartSelection {
    pub rows: Vec<SmartRow>,
    pub conservative: bool,
    dimensions: festerm_core::Dimensions,
    screen_generation: u64,
}

impl SmartSelection {
    pub fn text(&self, snapshot: TerminalSnapshot<'_>) -> Option<String> {
        if snapshot.dimensions() != self.dimensions
            || snapshot.screen_generation() != self.screen_generation
        {
            return None;
        }
        let mut text = String::new();
        for (index, row) in self.rows.iter().enumerate() {
            if snapshot.absolute_row_soft_wrapped(row.row) != Some(row.soft_wrapped) {
                return None;
            }
            let mut current = String::new();
            for column in row.first..=row.last {
                match snapshot.absolute_cell(column, row.row) {
                    Some(cell) if !cell.is_continuation() => current.push_str(cell.text()),
                    Some(_) => {}
                    None => current.push(' '),
                }
            }
            // Repaint/eviction must never silently change the text about to be copied.
            if current != row.text {
                return None;
            }
            if index > 0 {
                if row.space_join {
                    text.push(' ');
                } else if !row.joined {
                    text.push('\n');
                }
            }
            text.push_str(&current);
        }
        Some(text)
    }
}

struct Row {
    cells: Vec<(usize, String)>,
    bounded: bool,
    indent: usize,
    /// Column where text starts after a list/bullet marker, if the row has one.
    marker_text: Option<usize>,
    /// Joined to the row above as one prose run across a TUI-painted break.
    prose_join: bool,
}

/// Bullets and ordered-list markers as drawn by terminal UIs such as Copilot CLI.
fn marker_text_column(cells: &[(usize, String)]) -> Option<usize> {
    let first = cells.iter().position(|(_, text)| !text.trim().is_empty())?;
    let token_end = cells[first..]
        .iter()
        .position(|(_, text)| text.trim().is_empty())
        .map_or(cells.len(), |offset| first + offset);
    let token: String = cells[first..token_end]
        .iter()
        .map(|(_, t)| t.as_str())
        .collect();
    let ordered = token.len() > 1
        && matches!(token.chars().last(), Some('.' | ')'))
        && token[..token.len() - 1].chars().all(|c| c.is_ascii_digit());
    let bullet = matches!(
        token.as_str(),
        "●" | "•" | "◦" | "○" | "▪" | "▸" | "▶" | "-" | "*" | "·" | "–"
    );
    if !(ordered || bullet) {
        return None;
    }
    cells[token_end..]
        .iter()
        .find(|(_, text)| !text.trim().is_empty())
        .map(|(column, _)| *column)
}

fn ends_sentence(row: &Row) -> bool {
    row.cells
        .iter()
        .rev()
        .find(|(_, text)| !text.trim().is_empty())
        .is_some_and(|(_, text)| {
            matches!(text.as_str(), "." | "!" | "?" | "。" | "！" | "？" | ":")
        })
}

/// Whether `lower` continues the prose of `upper` across a TUI-painted break.
/// Same-indent rows only count when the run is anchored by a marker row, so
/// indented stack traces and code keep their explicit line breaks.
fn prose_continues(
    snapshot: TerminalSnapshot<'_>,
    upper: &Row,
    upper_y: usize,
    lower: &Row,
) -> bool {
    if upper.marker_text == Some(lower.indent) {
        return true;
    }
    if upper.indent != lower.indent
        || upper.indent == 0
        || upper.marker_text.is_some()
        || lower.marker_text.is_some()
        || ends_sentence(upper)
    {
        return false;
    }
    let mut budget = MAX_CELLS;
    let mut byte_budget = MAX_BYTES;
    let mut y = upper_y;
    for _ in 0..MAX_ROWS {
        let Some(previous) = y.checked_sub(1) else {
            return false;
        };
        y = previous;
        let Some(candidate) = row(snapshot, y, None, &mut budget, &mut byte_budget) else {
            return false;
        };
        if candidate.bounded {
            return false;
        }
        if candidate.marker_text == Some(upper.indent) {
            return true;
        }
        if candidate.indent != upper.indent || candidate.marker_text.is_some() {
            return false;
        }
    }
    false
}

fn border(text: &str) -> bool {
    matches!(
        text,
        "|" | "│"
            | "┃"
            | "║"
            | "┆"
            | "┊"
            | "┌"
            | "┐"
            | "└"
            | "┘"
            | "├"
            | "┤"
            | "─"
            | "━"
            | "═"
            | "┬"
            | "┴"
            | "┼"
            | "+"
    )
}

fn row(
    snapshot: TerminalSnapshot<'_>,
    y: usize,
    x: Option<usize>,
    budget: &mut usize,
    byte_budget: &mut usize,
) -> Option<Row> {
    let width = snapshot.dimensions().columns();
    let mut cells = Vec::new();
    for column in 0..width {
        *budget = budget.checked_sub(1)?;
        let cell = snapshot.cell(column, y);
        let text = cell.map_or(" ", |cell| cell.text());
        if text.len() > *byte_budget {
            *byte_budget = 0;
            return None;
        }
        *byte_budget -= text.len();
        if !cell.is_some_and(|cell| cell.is_continuation()) {
            cells.push((column, text.to_owned()));
        }
    }
    let hit = match x {
        Some(x) => cells.iter().rposition(|(column, _)| *column <= x)?,
        None => cells.iter().position(|(_, text)| !text.trim().is_empty())?,
    };
    if border(&cells[hit].1) || cells[hit].1.trim().is_empty() {
        return None;
    }
    let mut left = 0;
    let mut right = cells.len();
    // Borders and runs of two blank cells are conservative column separators.
    // Initial indentation and trailing padding are not neighboring panes.
    let mut index = 0;
    while index < cells.len() {
        let start = index;
        let separator = if border(&cells[index].1) {
            index += 1;
            true
        } else if cells[index].1.trim().is_empty() {
            while index < cells.len() && cells[index].1.trim().is_empty() {
                index += 1;
            }
            start > 0 && index < cells.len() && index - start >= 2
        } else {
            index += 1;
            false
        };
        if separator {
            if index <= hit {
                left = index;
            } else if start > hit {
                right = start;
                break;
            }
        }
    }
    let bounded = left != 0 || right != cells.len();
    while left < right && cells[left].1.trim().is_empty() {
        left += 1;
    }
    while right > left && cells[right - 1].1.trim().is_empty() {
        right -= 1;
    }
    if left == right {
        return None;
    }
    let indent = cells[left].0;
    let marker_text = marker_text_column(&cells[left..right]);
    if !bounded {
        left = 0;
        if snapshot.absolute_row_soft_wrapped(snapshot.content_row_for_viewport_row(y)?)? {
            let occupied = snapshot
                .absolute_row_occupied_columns(snapshot.content_row_for_viewport_row(y)?)?;
            right = cells.partition_point(|(column, _)| *column < occupied);
        }
    }
    Some(Row {
        indent,
        cells: cells[left..right].to_vec(),
        bounded,
        marker_text,
        prose_join: false,
    })
}

pub(crate) fn infer(
    snapshot: TerminalSnapshot<'_>,
    hit: CellPosition,
    scope: SmartSelectionScope,
) -> Option<SmartSelection> {
    let hit = normalize_selection_position(snapshot, hit)?;
    if snapshot.dimensions().columns() > MAX_CELLS {
        return None;
    }
    let mut budget = MAX_CELLS;
    let mut byte_budget = MAX_BYTES;
    let center = row(
        snapshot,
        hit.row,
        Some(hit.column),
        &mut budget,
        &mut byte_budget,
    )?;
    let conservative = center.bounded;
    let mut first = hit.row;
    let mut last = hit.row;
    let mut examined_rows = 1;
    let mut rows = vec![(hit.row, center)];
    // Pane inference intentionally stops at one visual row: wrap metadata only
    // describes the entire terminal, not individual TUI panes.
    if !rows[0].1.bounded {
        for upwards in [true, false] {
            loop {
                let y = if upwards {
                    first.checked_sub(1)
                } else {
                    (last + 1 < snapshot.dimensions().rows()).then_some(last + 1)
                };
                let Some(y) = y else { break };
                if examined_rows == MAX_ROWS {
                    return None;
                }
                examined_rows += 1;
                let edge = if upwards {
                    &rows[0].1
                } else {
                    &rows[rows.len() - 1].1
                };
                let previous = if upwards { y } else { last };
                let wrapped = snapshot
                    .absolute_row_soft_wrapped(snapshot.content_row_for_viewport_row(previous)?)?;
                if budget < snapshot.dimensions().columns() {
                    return None;
                }
                let Some(candidate) = row(snapshot, y, None, &mut budget, &mut byte_budget) else {
                    if byte_budget == 0 {
                        return None;
                    }
                    break;
                };
                let (upper, upper_y, lower) = if upwards {
                    (&candidate, y, edge)
                } else {
                    (edge, last, &candidate)
                };
                let prose = !wrapped && prose_continues(snapshot, upper, upper_y, lower);
                if candidate.bounded
                    || (!wrapped
                        && !prose
                        && match scope {
                            SmartSelectionScope::Sentence => true,
                            SmartSelectionScope::Paragraph => candidate.indent != edge.indent,
                            SmartSelectionScope::Block => {
                                if upwards {
                                    edge.indent == 0
                                } else {
                                    candidate.indent == 0
                                }
                            }
                        })
                {
                    break;
                }
                let mut candidate = candidate;
                if prose {
                    if upwards {
                        rows[0].1.prose_join = true;
                    } else {
                        candidate.prose_join = true;
                    }
                }
                if upwards {
                    first = y;
                    rows.insert(0, (y, candidate));
                } else {
                    last = y;
                    rows.push((y, candidate));
                }
            }
        }
    }
    let mut mapped = Vec::new();
    let mut clicked = 0;
    let mut text = String::new();
    let mut space_rows = Vec::new();
    for (y, source) in &rows {
        if source.prose_join {
            text.push(' ');
            space_rows.push(*y);
        }
        for (column, value) in &source.cells {
            if source.prose_join && value.trim().is_empty() && mapped_in_row(&mapped, *y) == 0 {
                continue;
            }
            if *y == hit.row && *column == hit.column {
                clicked = text.len();
            }
            let start = text.len();
            text.push_str(value);
            mapped.push((start, text.len(), *y, *column));
        }
    }
    if text.len() > MAX_BYTES {
        return None;
    }
    let (mut begin, mut end) = (0, text.len());
    if scope == SmartSelectionScope::Sentence {
        // Deliberately not a language model: punctuation followed by whitespace.
        for (index, character) in text.char_indices() {
            if matches!(character, '.' | '!' | '?' | '。' | '！' | '？') {
                let after = index + character.len_utf8();
                if after == text.len() || text[after..].starts_with(char::is_whitespace) {
                    if after <= clicked {
                        begin = after;
                    } else {
                        end = after;
                        break;
                    }
                }
            }
        }
        while begin < end && text[begin..end].starts_with(char::is_whitespace) {
            begin += text[begin..].chars().next()?.len_utf8();
        }
    }
    let mut selected = Vec::<SmartRow>::new();
    for (start, finish, y, column) in mapped {
        if finish <= begin || start >= end {
            continue;
        }
        let absolute = snapshot.content_row_for_viewport_row(y)?;
        if selected.last().is_none_or(|row| row.row != absolute) {
            let joined = selected.last().is_some_and(|previous| {
                snapshot.absolute_row_soft_wrapped(previous.row) == Some(true)
            });
            let space_join = !selected.is_empty() && space_rows.contains(&y);
            selected.push(SmartRow {
                row: absolute,
                space_join,
                first: column,
                last: column,
                text: String::new(),
                joined,
                soft_wrapped: snapshot.absolute_row_soft_wrapped(absolute)?,
            });
        }
        let selected = selected.last_mut()?;
        selected.last = column;
        selected.text.push_str(&text[start..finish]);
    }
    (!selected.is_empty()).then_some(SmartSelection {
        rows: selected,
        conservative,
        dimensions: snapshot.dimensions(),
        screen_generation: snapshot.screen_generation(),
    })
}

fn mapped_in_row(mapped: &[(usize, usize, usize, usize)], y: usize) -> usize {
    mapped.iter().filter(|entry| entry.2 == y).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{selection_text, Selection};
    use festerm_core::{Dimensions, Terminal};

    fn make_terminal(columns: usize, rows: usize, text: &str) -> Terminal {
        let mut terminal = Terminal::new(Dimensions::new(columns, rows).unwrap()).unwrap();
        terminal.ingest(text.as_bytes());
        terminal
    }

    fn select(terminal: &Terminal, x: usize, y: usize, scope: SmartSelectionScope) -> Selection {
        let mut selection = Selection::default();
        assert!(selection.infer_smart(
            TerminalSnapshot::from_terminal(terminal),
            CellPosition { column: x, row: y },
            scope
        ));
        selection
    }

    #[test]
    fn smart_selection_wraps_sentences_and_preserves_explicit_breaks() {
        let terminal = make_terminal(12, 5, "First line wraps. Next one.\r\n\r\nSeparate.");
        let sentence = select(&terminal, 3, 1, SmartSelectionScope::Sentence);
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        assert_eq!(
            selection_text(snapshot, &sentence).as_deref(),
            Some("First line wraps.")
        );
        assert_eq!(
            sentence.range(),
            Some(crate::CellRange::new(
                CellPosition { column: 0, row: 0 },
                CellPosition { column: 4, row: 1 },
            ))
        );
        assert_eq!(sentence.smart.as_ref().unwrap().rows[0].first, 0);
        assert_eq!(sentence.smart.as_ref().unwrap().rows[1].last, 4);
        let paragraph = select(&terminal, 3, 1, SmartSelectionScope::Paragraph);
        assert_eq!(
            selection_text(snapshot, &paragraph).as_deref(),
            Some("First line wraps. Next one.")
        );
        let terminal = make_terminal(30, 5, "Explicit line.\r\nSecond line.\r\n\r\nOther");
        let paragraph = select(&terminal, 2, 1, SmartSelectionScope::Paragraph);
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &paragraph).as_deref(),
            Some("Explicit line.\nSecond line.")
        );
    }

    #[test]
    fn smart_selection_never_crosses_borders_or_columns() {
        for (text, x, expected) in [
            ("left prose │ secret neighbor", 3, "left prose"),
            ("left prose  secret neighbor", 17, "secret neighbor"),
            ("│界e\u{301} words│secret│", 2, "界e\u{301} words"),
        ] {
            let terminal = make_terminal(40, 4, &format!("{text}\r\nnext row should stay out"));
            let selection = select(&terminal, x, 0, SmartSelectionScope::Block);
            let snapshot = TerminalSnapshot::from_terminal(&terminal);
            assert_eq!(
                selection_text(snapshot, &selection).as_deref(),
                Some(expected)
            );
            assert_eq!(selection.smart.as_ref().unwrap().rows.len(), 1);
        }
    }

    #[test]
    fn smart_selection_structured_errors_preserve_indent_and_blank_boundary() {
        let terminal = make_terminal(
            40,
            7,
            "Error: failed\r\n  at module.rs:12\r\n  caused by: missing file\r\nunrelated\r\n\r\nother",
        );
        let selection = select(&terminal, 7, 1, SmartSelectionScope::Block);
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &selection).as_deref(),
            Some("Error: failed\n  at module.rs:12\n  caused by: missing file")
        );
        let paragraph = select(&terminal, 7, 1, SmartSelectionScope::Paragraph);
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &paragraph).as_deref(),
            Some("  at module.rs:12\n  caused by: missing file")
        );
    }

    #[test]
    fn smart_selection_joins_hanging_indent_prose_painted_by_a_tui() {
        let terminal = make_terminal(
            40,
            8,
            "● Smart selection spans the\r\n  rows of one answer. It then\r\n  stops at a blank row.\r\n\r\n● Another answer.",
        );
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        let paragraph = select(&terminal, 5, 1, SmartSelectionScope::Paragraph);
        assert_eq!(
            selection_text(snapshot, &paragraph).as_deref(),
            Some("● Smart selection spans the rows of one answer. It then stops at a blank row.")
        );
        let sentence = select(&terminal, 5, 1, SmartSelectionScope::Sentence);
        assert_eq!(
            selection_text(snapshot, &sentence).as_deref(),
            Some("● Smart selection spans the rows of one answer.")
        );
        let code = make_terminal(40, 4, "  let a = 1;\r\n  let b = 2;");
        let block = select(&code, 4, 0, SmartSelectionScope::Block);
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&code), &block).as_deref(),
            Some("  let a = 1;\n  let b = 2;")
        );
    }

    #[test]
    fn smart_selection_unicode_mapping_mutation_and_adjustment() {
        let mut terminal = make_terminal(30, 4, "界e\u{301} test. Next.");
        let mut selection = select(&terminal, 1, 0, SmartSelectionScope::Sentence);
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        assert_eq!(
            selection_text(snapshot, &selection).as_deref(),
            Some("界e\u{301} test.")
        );
        assert_eq!(selection.row_range(snapshot, 0).unwrap().start.column, 0);
        assert_eq!(selection.row_range(snapshot, 0).unwrap().end.column, 8);
        terminal.ingest(b"\rX");
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &selection),
            None
        );
        selection.invalidate_smart(TerminalSnapshot::from_terminal(&terminal));
        assert!(selection.range().is_none());
        selection = select(&terminal, 4, 0, SmartSelectionScope::Sentence);
        selection.begin(CellPosition { column: 3, row: 0 });
        selection.extend(CellPosition { column: 6, row: 0 });
        selection.finish();
        assert!(!selection.is_smart());
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &selection).as_deref(),
            Some(" tes")
        );
        selection.clear();
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &selection),
            None
        );
    }

    #[test]
    fn smart_selection_wide_wrap_excludes_synthetic_padding() {
        let terminal = make_terminal(6, 4, "abcde界e\u{301}.");
        let selection = select(&terminal, 1, 1, SmartSelectionScope::Paragraph);
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        assert_eq!(
            selection_text(snapshot, &selection).as_deref(),
            Some("abcde界e\u{301}.")
        );
        assert_eq!(selection.smart.as_ref().unwrap().rows[0].last, 4);
        assert_eq!(selection.smart.as_ref().unwrap().rows[1].last, 3);
    }

    #[test]
    fn smart_selection_invalidation_tracks_wrap_resize_and_screen_identity() {
        let mut terminal = make_terminal(8, 3, "12345678more");
        let selection = select(&terminal, 1, 0, SmartSelectionScope::Sentence);
        terminal.ingest(b"\x1b[H\n");
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &selection),
            None
        );

        let mut terminal = make_terminal(20, 3, "same text");
        let selection = select(&terminal, 1, 0, SmartSelectionScope::Sentence);
        terminal.ingest(b"\x1b[?1049hsame text");
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &selection),
            None
        );
        terminal.ingest(b"\x1b[?1049l");
        let selection = select(&terminal, 1, 0, SmartSelectionScope::Sentence);
        terminal.ingest(b"\x1b[?1049hsame text\x1b[?1049l");
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &selection),
            None,
            "a hidden alternate enter/exit cannot resurrect primary selection identity"
        );

        let selection = select(&terminal, 1, 0, SmartSelectionScope::Sentence);
        terminal.resize(Dimensions::new(21, 3).unwrap()).unwrap();
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &selection),
            None
        );

        let mut terminal = make_terminal(20, 3, "same text");
        let selection = select(&terminal, 1, 0, SmartSelectionScope::Sentence);
        terminal.reset_to_initial_state();
        terminal.ingest(b"same text");
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &selection),
            None,
            "identical post-reset content belongs to a new buffer lifetime"
        );
    }

    #[test]
    fn smart_selection_rejects_blank_border_and_excessive_context() {
        let terminal = make_terminal(20, 3, "│words│");
        for x in [0, 6, 10] {
            assert!(infer(
                TerminalSnapshot::from_terminal(&terminal),
                CellPosition { column: x, row: 0 },
                SmartSelectionScope::Paragraph
            )
            .is_none());
        }
        let terminal = make_terminal(300, 130, &"x".repeat(300 * 129));
        assert!(infer(
            TerminalSnapshot::from_terminal(&terminal),
            CellPosition { column: 4, row: 70 },
            SmartSelectionScope::Paragraph
        )
        .is_none());
    }
}
