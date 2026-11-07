//! Bounded presentation-only inference; the grid remains the source of truth.
use crate::{
    normalize_selection_position,
    selection::{collect_bounded_text, TextWork},
    CellPosition, SelectedTextError, TerminalSnapshot,
};

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
    /// Blank rows inside a block record that precede this row.
    pub blank_before: usize,
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
        self.text_impl(snapshot, usize::MAX, false)
            .expect("smart selected text fits in a String")
    }

    pub fn text_with_limit(
        &self,
        snapshot: TerminalSnapshot<'_>,
        max_bytes: usize,
    ) -> Result<Option<String>, SelectedTextError> {
        self.text_impl(snapshot, max_bytes, true)
    }

    fn text_impl(
        &self,
        snapshot: TerminalSnapshot<'_>,
        max_bytes: usize,
        limited_work: bool,
    ) -> Result<Option<String>, SelectedTextError> {
        if snapshot.dimensions() != self.dimensions
            || snapshot.screen_generation() != self.screen_generation
        {
            return Ok(None);
        }
        let mut work = TextWork::new(limited_work);
        for row in &self.rows {
            work.charge(1)?;
            if snapshot.absolute_row_soft_wrapped(row.row) != Some(row.soft_wrapped) {
                return Ok(None);
            }
            let mut matched = 0;
            for column in row.first..=row.last {
                work.charge(1)?;
                let fragment = match snapshot.absolute_cell(column, row.row) {
                    Some(cell) if !cell.is_continuation() => cell.text(),
                    Some(_) => continue,
                    None => " ",
                };
                // Compare borrowed fragments before any allocation, even if a repaint
                // replaced one cell with a much larger combining grapheme.
                if fragment.len() > row.text.len() - matched {
                    return Ok(None);
                }
                work.charge(fragment.len())?;
                if !row.text[matched..].starts_with(fragment) {
                    return Ok(None);
                }
                matched += fragment.len();
            }
            if matched != row.text.len() {
                return Ok(None);
            }
        }
        collect_bounded_text(max_bytes, work, |append, work| {
            for (index, row) in self.rows.iter().enumerate() {
                work.charge(1 + row.blank_before)?;
                if index > 0 {
                    if row.space_join {
                        append(" ")?;
                    } else if !row.joined {
                        append("\n")?;
                    }
                    for _ in 0..row.blank_before {
                        append("\n")?;
                    }
                }
                append(&row.text)?;
            }
            Ok(Some(()))
        })
    }
}

struct Row {
    cells: Vec<(usize, String)>,
    /// Cells painted on a background other than the row's text, such as
    /// Copilot CLI inline code; their punctuation never ends a sentence.
    code: Vec<bool>,
    bounded: bool,
    indent: usize,
    /// Column where text starts after a list/bullet marker, if the row has one.
    marker_text: Option<usize>,
    /// Joined to the row above as one prose run across a TUI-painted break.
    prose_join: bool,
    blank_before: usize,
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
    gaps: bool,
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
        let Some(candidate) = row(snapshot, y, None, gaps, &mut budget, &mut byte_budget) else {
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

/// A lone glyph in the last column after blank padding is a TUI scrollbar
/// (Copilot CLI paints `┃`); nothing lies beyond it, so it splits no panes.
fn scrollbar(text: &str) -> bool {
    border(text)
        || matches!(
            text,
            "█" | "▐" | "▕" | "▌" | "░" | "▒" | "▓" | "╎" | "╏" | "┇" | "┋"
        )
}

/// A marker row that owns the hit as one block record: Copilot CLI messages
/// start with `●` and keep their body, blank rows included, indented deeper.
/// Only borders split a record; a double space inside its prose is not a pane.
fn record_anchor(
    snapshot: TerminalSnapshot<'_>,
    hit_y: usize,
    hit: &Row,
) -> Option<(usize, usize)> {
    let mut budget = MAX_CELLS;
    let mut byte_budget = MAX_BYTES;
    let columns = snapshot.dimensions().columns();
    let mut anchor = hit.marker_text.map(|_| (hit_y, hit.indent));
    let mut indent = hit.indent;
    let mut y = hit_y;
    for _ in 0..MAX_ROWS {
        let Some(above) = y.checked_sub(1) else { break };
        y = above;
        if budget < columns {
            break;
        }
        let Some(candidate) = row(snapshot, y, None, false, &mut budget, &mut byte_budget) else {
            if byte_budget == 0 {
                break;
            }
            continue;
        };
        if candidate.bounded {
            break;
        }
        if candidate.indent < indent {
            if candidate.marker_text.is_none() {
                break;
            }
            // Keep climbing: a nested bullet belongs to the outermost message.
            indent = candidate.indent;
            anchor = Some((y, indent));
        }
    }
    anchor
}

fn row(
    snapshot: TerminalSnapshot<'_>,
    y: usize,
    x: Option<usize>,
    gaps: bool,
    budget: &mut usize,
    byte_budget: &mut usize,
) -> Option<Row> {
    let width = snapshot.dimensions().columns();
    let mut cells = Vec::new();
    let mut backgrounds = Vec::new();
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
            backgrounds.push(cell.map(|cell| cell.background()));
        }
    }
    let hit = match x {
        Some(x) => cells.iter().rposition(|(column, _)| *column <= x)?,
        None => cells.iter().position(|(_, text)| !text.trim().is_empty())?,
    };
    if border(&cells[hit].1) || cells[hit].1.trim().is_empty() {
        return None;
    }
    let mut edge = cells.len();
    if edge >= 2 && scrollbar(&cells[edge - 1].1) && cells[edge - 2].1.trim().is_empty() {
        edge -= 1;
    }
    if hit >= edge {
        return None;
    }
    let mut left = 0;
    let mut right = edge;
    // Borders and runs of two blank cells are conservative column separators.
    // Initial indentation and trailing padding are not neighboring panes.
    let mut index = 0;
    while index < edge {
        let start = index;
        let separator = if border(&cells[index].1) {
            index += 1;
            true
        } else if cells[index].1.trim().is_empty() {
            while index < edge && cells[index].1.trim().is_empty() {
                index += 1;
            }
            // Inline code painted on a background (Copilot CLI) pads a normal
            // space with a styled one; a pane gap is uniformly painted.
            let gap = backgrounds[start..index]
                .windows(2)
                .any(|pair| pair[0] == pair[1]);
            gaps && start > 0 && index < edge && gap
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
    let bounded = left != 0 || right != edge;
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
    let base = backgrounds[left];
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
        code: backgrounds[left..right]
            .iter()
            .map(|background| *background != base)
            .collect(),
        cells: cells[left..right].to_vec(),
        bounded,
        marker_text,
        prose_join: false,
        blank_before: 0,
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
    let mut center = row(
        snapshot,
        hit.row,
        Some(hit.column),
        true,
        &mut budget,
        &mut byte_budget,
    )?;
    let mut anchor = None;
    if !center.bounded {
        anchor = record_anchor(snapshot, hit.row, &center);
    } else if let Some(open) = row(
        snapshot,
        hit.row,
        Some(hit.column),
        false,
        &mut budget,
        &mut byte_budget,
    )
    .filter(|open| !open.bounded)
    {
        // A gap-split row inside a marker-led message is prose, not panes.
        anchor = record_anchor(snapshot, hit.row, &open).filter(|(y, _)| *y != hit.row);
        if anchor.is_some() {
            center = open;
        }
    }
    let gaps = anchor.is_none();
    let conservative = center.bounded;
    let mut first = hit.row;
    let mut last = hit.row;
    let mut examined_rows = 1;
    let mut rows = vec![(hit.row, center)];
    // Pane inference intentionally stops at one visual row: wrap metadata only
    // describes the entire terminal, not individual TUI panes.
    if !rows[0].1.bounded {
        let record = anchor.filter(|_| scope == SmartSelectionScope::Block);
        for upwards in [true, false] {
            let mut cursor = if upwards { first } else { last };
            let mut pending = 0;
            loop {
                let y = if upwards {
                    cursor.checked_sub(1)
                } else {
                    (cursor + 1 < snapshot.dimensions().rows()).then_some(cursor + 1)
                };
                let Some(y) = y else { break };
                if upwards && record.is_some_and(|(anchor, _)| y < anchor) {
                    break;
                }
                if examined_rows == MAX_ROWS {
                    return None;
                }
                examined_rows += 1;
                let edge = if upwards {
                    &rows[0].1
                } else {
                    &rows[rows.len() - 1].1
                };
                let wrapped = pending == 0
                    && snapshot.absolute_row_soft_wrapped(
                        snapshot.content_row_for_viewport_row(if upwards { y } else { cursor })?,
                    )?;
                if budget < snapshot.dimensions().columns() {
                    return None;
                }
                let Some(mut candidate) =
                    row(snapshot, y, None, gaps, &mut budget, &mut byte_budget)
                else {
                    if byte_budget == 0 {
                        return None;
                    }
                    if record.is_some() && !wrapped {
                        pending += 1;
                        cursor = y;
                        continue;
                    }
                    break;
                };
                let (upper, upper_y, lower) = if upwards {
                    (&candidate, y, edge)
                } else {
                    (edge, cursor, &candidate)
                };
                let prose = !wrapped
                    && pending == 0
                    && prose_continues(snapshot, upper, upper_y, lower, gaps);
                if candidate.bounded
                    || (!wrapped
                        && !prose
                        && match scope {
                            SmartSelectionScope::Sentence => true,
                            SmartSelectionScope::Paragraph => candidate.indent != edge.indent,
                            SmartSelectionScope::Block => match record {
                                Some((_, indent)) => !upwards && candidate.indent <= indent,
                                None if upwards => edge.indent == 0,
                                None => candidate.indent == 0,
                            },
                        })
                {
                    break;
                }
                if prose {
                    if upwards {
                        rows[0].1.prose_join = true;
                    } else {
                        candidate.prose_join = true;
                    }
                }
                if upwards {
                    rows[0].1.blank_before = pending;
                    first = y;
                    rows.insert(0, (y, candidate));
                } else {
                    candidate.blank_before = pending;
                    last = y;
                    rows.push((y, candidate));
                }
                pending = 0;
                cursor = y;
            }
        }
    }
    let mut mapped = Vec::new();
    let mut clicked = 0;
    let mut text = String::new();
    let mut space_rows = Vec::new();
    let mut code = Vec::new();
    // A leading list marker is layout, not content; later rows lose the same
    // indentation so nested structure stays relative to the selected text.
    let dedent = rows[0].1.marker_text.unwrap_or(0);
    for (index, (y, source)) in rows.iter().enumerate() {
        if source.prose_join {
            text.push(' ');
            space_rows.push(*y);
        }
        for ((column, value), styled) in source.cells.iter().zip(&source.code) {
            let leading = mapped_in_row(&mapped, *y) == 0;
            if (index == 0 && *column < dedent)
                || (leading && value.trim().is_empty() && (source.prose_join || *column < dedent))
            {
                continue;
            }
            if *y == hit.row && *column == hit.column {
                clicked = text.len();
            }
            let start = text.len();
            text.push_str(value);
            if *styled {
                code.push(start..text.len());
            }
            mapped.push((start, text.len(), *y, *column));
        }
    }
    if text.len() > MAX_BYTES {
        return None;
    }
    let (mut begin, mut end) = (0, text.len());
    if scope == SmartSelectionScope::Sentence {
        // Deliberately not a language model: punctuation followed by whitespace,
        // outside inline code such as an illustrative `1.` list marker.
        for (index, character) in text.char_indices() {
            if matches!(character, '.' | '!' | '?' | '。' | '！' | '？')
                && !code.iter().any(|range| range.contains(&index))
            {
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
            let blank_before = if selected.is_empty() {
                0
            } else {
                rows.iter()
                    .find(|(row, _)| *row == y)
                    .map_or(0, |(_, row)| row.blank_before)
            };
            selected.push(SmartRow {
                row: absolute,
                space_join,
                blank_before,
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
    use crate::{selection_text, selection_text_with_limit, Selection};
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
    fn bounded_smart_selection_counts_utf8_joins_and_rejects_repainted_large_cells() {
        for (columns, text, expected) in [
            (12, "界e\u{301}. Next.", "界e\u{301}. Next."),
            (
                12,
                "First line wraps. Next one.",
                "First line wraps. Next one.",
            ),
            (
                30,
                "Explicit line.\r\nSecond line.",
                "Explicit line.\nSecond line.",
            ),
            (
                30,
                "• First part\r\n  second part",
                "First part second part",
            ),
        ] {
            let terminal = make_terminal(columns, 5, text);
            let selection = select(&terminal, 2, 0, SmartSelectionScope::Paragraph);
            let snapshot = TerminalSnapshot::from_terminal(&terminal);
            assert_eq!(
                selection_text(snapshot, &selection).as_deref(),
                Some(expected)
            );
            for limit in 0..=expected.len() + 1 {
                let result = selection_text_with_limit(snapshot, &selection, limit);
                if limit < expected.len() {
                    assert_eq!(result, Err(SelectedTextError::MaxBytesExceeded));
                } else {
                    assert_eq!(result.as_ref().unwrap().as_deref(), Some(expected));
                }
            }
        }

        let mut terminal = make_terminal(20, 2, "Original text.");
        let selection = select(&terminal, 2, 0, SmartSelectionScope::Sentence);
        terminal.ingest(format!("\x1b[HO{}", "\u{301}".repeat(120)).as_bytes());
        assert_eq!(
            selection_text_with_limit(TerminalSnapshot::from_terminal(&terminal), &selection, 1),
            Ok(None)
        );
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
            Some("Smart selection spans the rows of one answer. It then stops at a blank row.")
        );
        let sentence = select(&terminal, 5, 1, SmartSelectionScope::Sentence);
        assert_eq!(
            selection_text(snapshot, &sentence).as_deref(),
            Some("Smart selection spans the rows of one answer.")
        );
        let code = make_terminal(40, 4, "  let a = 1;\r\n  let b = 2;");
        let block = select(&code, 4, 0, SmartSelectionScope::Block);
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&code), &block).as_deref(),
            Some("  let a = 1;\n  let b = 2;")
        );
    }

    /// Copilot CLI layout: one-column margin, `●` messages, hanging-indent
    /// bullets, and a `┃` scrollbar thumb in the last column.
    fn copilot_terminal() -> Terminal {
        let lines = [
            " ❯ previous prompt",
            "",
            " ● I don't think Tree-sitter is the right tool here. By the",
            "   time the output reaches us, it has been rewrapped.",
            "",
            "   What I would do:",
            "",
            "   • Use style signals. Colors mark the",
            "     message boundaries.",
            "   • Build a test set first. Capture failures. The design doc already",
            "     requires this set before any model is considered. Every rule change gets measured against it.",
            "   • Then fix actual failures, not guesses.",
            "",
            "   Limitation: copied text reads like \"leading  ● ,\".",
            "",
            " ● Next message.",
            "",
            " ~/src [main]",
        ];
        let text: Vec<String> = lines.iter().map(|line| format!("{line:<119}┃")).collect();
        make_terminal(120, 20, &text.join("\r\n"))
    }

    #[test]
    fn smart_selection_spans_copilot_inline_code_and_wrapped_bullets() {
        let code = |text: &str| format!("\x1b[48;5;236m {text} \x1b[49m");
        let lines = [
            format!(
                " • Bullets: Paragraph and Block selections now start after a leading {}, {} or {} marker, and",
                code("●"),
                code("•"),
                code("1.")
            ),
            "   the rows below lose the same indentation.".to_owned(),
            format!(" • Block: clicking anywhere in a {} message selects it.", code("●")),
        ];
        let text: Vec<String> = lines
            .iter()
            .map(|line| format!("{line}\x1b[K\x1b[120G┃"))
            .collect();
        let terminal = make_terminal(120, 5, &text.join("\r\n"));
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        let expected =
            "Bullets: Paragraph and Block selections now start after a leading  ● ,  •  \
                        or  1.  marker, and the rows below lose the same indentation.";
        for scope in [SmartSelectionScope::Paragraph, SmartSelectionScope::Block] {
            let selection = select(&terminal, 10, 0, scope);
            assert!(!selection.smart.as_ref().unwrap().conservative);
            assert_eq!(
                selection_text(snapshot, &selection).as_deref(),
                Some(expected)
            );
        }
        for (x, y) in [(10, 0), (10, 1)] {
            let sentence = select(&terminal, x, y, SmartSelectionScope::Sentence);
            assert_eq!(
                selection_text(snapshot, &sentence).as_deref(),
                Some(expected)
            );
        }
        let neighbor = make_terminal(40, 2, "left  \x1b[48;5;236m  \x1b[49m right");
        let selection = select(&neighbor, 1, 0, SmartSelectionScope::Paragraph);
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&neighbor), &selection).as_deref(),
            Some("left")
        );
    }

    #[test]
    fn smart_selection_reads_copilot_messages_past_its_scrollbar() {
        let terminal = copilot_terminal();
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        let paragraph = select(&terminal, 20, 9, SmartSelectionScope::Paragraph);
        assert!(!paragraph.smart.as_ref().unwrap().conservative);
        assert_eq!(
            selection_text(snapshot, &paragraph).as_deref(),
            Some(
                "Build a test set first. Capture failures. The design doc already requires this set \
                 before any model is considered. Every rule change gets measured against it."
            )
        );
        let message = "I don't think Tree-sitter is the right tool here. By the time the output \
                       reaches us, it has been rewrapped.\n\nWhat I would do:\n\n• Use style \
                       signals. Colors mark the message boundaries.\n• Build a test set first. \
                       Capture failures. The design doc already requires this set before any model \
                       is considered. Every rule change gets measured against it.\n• Then fix \
                       actual failures, not guesses.\n\nLimitation: copied text reads like \
                       \"leading  ● ,\".";
        for (x, y) in [(10, 3), (1, 2), (6, 5), (20, 10), (36, 13)] {
            let block = select(&terminal, x, y, SmartSelectionScope::Block);
            assert_eq!(selection_text(snapshot, &block).as_deref(), Some(message));
        }
        let paragraph = select(&terminal, 36, 13, SmartSelectionScope::Paragraph);
        assert_eq!(
            selection_text(snapshot, &paragraph).as_deref(),
            Some("   Limitation: copied text reads like \"leading  ● ,\".")
        );
        let block = select(&terminal, 5, 15, SmartSelectionScope::Block);
        assert_eq!(
            selection_text(snapshot, &block).as_deref(),
            Some("Next message.")
        );
        let prompt = select(&terminal, 3, 17, SmartSelectionScope::Block);
        assert_eq!(
            selection_text(snapshot, &prompt).as_deref(),
            Some(" ~/src [main]")
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
