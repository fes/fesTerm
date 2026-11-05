use festerm_automation_mcp::Refusal;
use festerm_core::Terminal;
use serde_json::{json, Value};
use std::io::{self, Write};

const METADATA_RESERVE: usize = 512;
const MAX_VISITED_CELLS: usize = 65_536;

struct Text {
    value: String,
    remaining: usize,
}

impl Text {
    fn new(max_bytes: usize) -> Result<Self, Refusal> {
        let remaining = max_bytes
            .checked_sub(METADATA_RESERVE)
            .filter(|remaining| *remaining > 0)
            .ok_or(Refusal::ResponseTooLarge)?;
        Ok(Self {
            value: String::new(),
            remaining,
        })
    }

    fn push(&mut self, character: char) -> bool {
        let cost = match character {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            '\0'..='\u{1f}' => 6,
            _ => character.len_utf8(),
        };
        if cost > self.remaining {
            return false;
        }
        self.remaining -= cost;
        self.value.push(character);
        true
    }
}

pub(super) fn bounded(value: Value, max_bytes: usize) -> Result<Value, Refusal> {
    struct Counter {
        remaining: usize,
        exceeded: bool,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let Some(remaining) = self.remaining.checked_sub(bytes.len()) else {
                self.exceeded = true;
                return Err(io::Error::other("encoded observation exceeds its limit"));
            };
            self.remaining = remaining;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter {
        remaining: max_bytes,
        exceeded: false,
    };
    if serde_json::to_writer(&mut counter, &value).is_err() {
        return Err(if counter.exceeded {
            Refusal::ResponseTooLarge
        } else {
            Refusal::InvalidBackendResult
        });
    }
    Ok(value)
}

pub(super) fn screen(
    terminal: &Terminal,
    revision: u64,
    max_bytes: usize,
) -> Result<Value, Refusal> {
    let dimensions = terminal.screen().dimensions();
    let mut text = Text::new(max_bytes)?;
    let mut truncated = false;
    let mut visited = 0;
    'rows: for row in 0..dimensions.rows() {
        if row > 0 && !text.push('\n') {
            truncated = true;
            break;
        }
        for column in 0..dimensions.columns() {
            visited += 1;
            if visited > MAX_VISITED_CELLS {
                truncated = true;
                break 'rows;
            }
            if let Some(cell) = terminal.cell_ref(column, row) {
                if !cell.is_continuation() {
                    for character in cell.text().chars() {
                        if !text.push(character) {
                            truncated = true;
                            break 'rows;
                        }
                    }
                }
            }
        }
    }
    bounded(
        json!({
            "revision": revision,
            "screen_generation": terminal.screen_generation(),
            "columns": dimensions.columns(),
            "rows": dimensions.rows(),
            "cursor": {"column": terminal.cursor().column(), "row": terminal.cursor().row()},
            "alternate_screen": terminal.modes().alternate_screen(),
            "text": text.value,
            "truncated": truncated,
            "untrusted": true,
        }),
        max_bytes,
    )
}

#[derive(Clone, Copy, Default)]
struct Cursor {
    revision: u64,
    line: usize,
    cell: usize,
    byte: usize,
}

impl Cursor {
    fn parse(value: &str) -> Option<Self> {
        let mut fields = value.split(':');
        let revision = fields.next()?.parse().ok()?;
        let line = fields.next()?.parse().ok()?;
        let cell = fields.next()?.parse().ok()?;
        let byte = fields.next()?.parse().ok()?;
        let cursor = Self {
            revision,
            line,
            cell,
            byte,
        };
        (fields.next().is_none() && cursor.encode() == value).then_some(cursor)
    }

    fn encode(self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.revision, self.line, self.cell, self.byte
        )
    }
}

pub(super) fn valid_cursor(value: &str) -> bool {
    Cursor::parse(value).is_some()
}

pub(super) fn history(
    terminal: &Terminal,
    revision: u64,
    cursor: Option<&str>,
    max_bytes: usize,
) -> Result<Value, Refusal> {
    let mut position = match cursor {
        Some(cursor) => Cursor::parse(cursor).ok_or(Refusal::InvalidArguments)?,
        None => Cursor {
            revision,
            ..Cursor::default()
        },
    };
    if position.revision != revision {
        return bounded(
            json!({
                "revision": revision, "text": "", "gap": true,
                "next_cursor": null, "truncated": false, "untrusted": true,
            }),
            max_bytes,
        );
    }
    let mut text = Text::new(max_bytes)?;
    let count = terminal.scrollback_lines().len();
    if position.line > count {
        return Err(Refusal::InvalidArguments);
    }
    let initial = position.encode();
    let mut visited = 0;
    let mut truncated = false;
    'lines: for line in terminal.scrollback_lines().skip(position.line) {
        if position.cell > line.cells().len() {
            return Err(Refusal::InvalidArguments);
        }
        for cell in &line.cells()[position.cell..] {
            visited += 1;
            if visited > MAX_VISITED_CELLS {
                truncated = true;
                break 'lines;
            }
            let value = cell.text();
            if position.byte > value.len() || !value.is_char_boundary(position.byte) {
                return Err(Refusal::InvalidArguments);
            }
            if !cell.is_continuation() {
                for character in value[position.byte..].chars() {
                    if !text.push(character) {
                        truncated = true;
                        break 'lines;
                    }
                    position.byte += character.len_utf8();
                }
            }
            position.cell += 1;
            position.byte = 0;
        }
        if position.byte != 0 {
            return Err(Refusal::InvalidArguments);
        }
        if line.has_hard_break() && !text.push('\n') {
            truncated = true;
            break;
        }
        position.line += 1;
        position.cell = 0;
    }
    if position.line == count && (position.cell != 0 || position.byte != 0) {
        return Err(Refusal::InvalidArguments);
    }
    let next = truncated.then(|| position.encode());
    if next.as_ref() == Some(&initial) {
        return Err(Refusal::ResponseTooLarge);
    }
    bounded(
        json!({
            "revision": revision, "text": text.value, "gap": false,
            "next_cursor": next, "truncated": truncated, "untrusted": true,
            "retained_primary_history_only": true,
            "evicted_lines": terminal.scrollback_stats().evicted_lines(),
            "oversize_lines": terminal.scrollback_stats().oversize_lines(),
        }),
        max_bytes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_core::Dimensions;

    fn terminal(columns: usize, rows: usize) -> Terminal {
        Terminal::new(Dimensions::new(columns, rows).unwrap()).unwrap()
    }

    #[test]
    fn screen_observation_counts_json_escapes_and_utf8_before_copying() {
        let mut terminal = terminal(1024, 2);
        terminal.ingest("é\"\\\r\n".repeat(300).as_bytes());
        let snapshot = screen(&terminal, 7, 640).unwrap();
        assert!(serde_json::to_vec(&snapshot).unwrap().len() <= 640);
        assert_eq!(snapshot["revision"], 7);
        assert_eq!(snapshot["truncated"], true);
        assert!(screen(&terminal, 7, 4).is_err());
    }

    #[test]
    fn history_pagination_has_no_repeated_or_lost_text_and_reports_change_gaps() {
        let mut terminal = terminal(4, 2);
        terminal.ingest("é\"\\\r\n".repeat(100).as_bytes());
        let mut cursor = None;
        let mut text = String::new();
        loop {
            let page = history(&terminal, 3, cursor.as_deref(), 576).unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() <= 576);
            text.push_str(page["text"].as_str().unwrap());
            cursor = page["next_cursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        let full = history(&terminal, 3, None, 32_768).unwrap();
        assert_eq!(text, full["text"].as_str().unwrap());
        let gap = history(&terminal, 4, Some("3:0:0:0"), 576).unwrap();
        assert_eq!(gap["gap"], true);
        assert_eq!(gap["text"], "");
        assert!(history(&terminal, 4, Some("04:0:0:0"), 576).is_err());
        assert!(history(&terminal, 4, Some("4:999999:0:0"), 576).is_err());
    }
}
