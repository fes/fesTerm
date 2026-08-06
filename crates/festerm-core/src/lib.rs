//! GUI-independent terminal state primitives.
//!
//! This first core slice intentionally supports only printable ASCII and basic
//! C0 controls. ANSI/VT escape sequences are introduced in a later milestone.

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Dimensions {
    columns: usize,
    rows: usize,
}

impl Dimensions {
    pub fn new(columns: usize, rows: usize) -> Result<Self, DimensionsError> {
        if columns == 0 || rows == 0 {
            return Err(DimensionsError { columns, rows });
        }

        Ok(Self { columns, rows })
    }

    pub const fn columns(self) -> usize {
        self.columns
    }

    pub const fn rows(self) -> usize {
        self.rows
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DimensionsError {
    columns: usize,
    rows: usize,
}

impl fmt::Display for DimensionsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "terminal dimensions must be non-zero, received {} columns by {} rows",
            self.columns, self.rows
        )
    }
}

impl std::error::Error for DimensionsError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cursor {
    column: usize,
    row: usize,
}

impl Cursor {
    pub const fn column(self) -> usize {
        self.column
    }

    pub const fn row(self) -> usize {
        self.row
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cell {
    character: char,
}

impl Cell {
    pub const fn character(self) -> char {
        self.character
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Terminal {
    dimensions: Dimensions,
    cells: Vec<Cell>,
    cursor: Cursor,
    dirty_rows: Vec<bool>,
}

impl Terminal {
    pub fn new(dimensions: Dimensions) -> Self {
        let cell_count = dimensions.columns * dimensions.rows;

        Self {
            dimensions,
            cells: vec![blank_cell(); cell_count],
            cursor: Cursor { column: 0, row: 0 },
            dirty_rows: vec![true; dimensions.rows],
        }
    }

    pub const fn dimensions(&self) -> Dimensions {
        self.dimensions
    }

    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    pub fn cell(&self, column: usize, row: usize) -> Option<Cell> {
        self.cells.get(self.cell_index(column, row)?).copied()
    }

    pub fn row_text(&self, row: usize) -> Option<String> {
        if row >= self.dimensions.rows {
            return None;
        }

        let start = row * self.dimensions.columns;
        let end = start + self.dimensions.columns;
        Some(
            self.cells[start..end]
                .iter()
                .map(|cell| cell.character)
                .collect(),
        )
    }

    pub fn ingest(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.apply(TerminalOp::from_byte(*byte));
        }
    }

    pub fn take_dirty_rows(&mut self) -> Vec<usize> {
        self.dirty_rows
            .iter_mut()
            .enumerate()
            .filter_map(|(row, is_dirty)| {
                if *is_dirty {
                    *is_dirty = false;
                    Some(row)
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn apply(&mut self, operation: TerminalOp) {
        match operation {
            TerminalOp::Print(character) => self.print(character),
            TerminalOp::CarriageReturn => self.cursor.column = 0,
            TerminalOp::LineFeed => self.line_feed(),
            TerminalOp::Backspace => self.cursor.column = self.cursor.column.saturating_sub(1),
            TerminalOp::Tab => self.tab(),
            TerminalOp::Ignored => {}
        }
    }

    fn print(&mut self, character: char) {
        let index = self
            .cell_index(self.cursor.column, self.cursor.row)
            .expect("cursor remains within terminal dimensions");
        self.cells[index] = Cell { character };
        self.mark_dirty(self.cursor.row);

        if self.cursor.column + 1 == self.dimensions.columns {
            self.cursor.column = 0;
            self.line_feed();
        } else {
            self.cursor.column += 1;
        }
    }

    fn line_feed(&mut self) {
        if self.cursor.row + 1 == self.dimensions.rows {
            self.cells.rotate_left(self.dimensions.columns);
            let start = (self.dimensions.rows - 1) * self.dimensions.columns;
            self.cells[start..].fill(blank_cell());
            self.dirty_rows.fill(true);
        } else {
            self.cursor.row += 1;
            self.mark_dirty(self.cursor.row);
        }
    }

    fn tab(&mut self) {
        let next_tab_stop = ((self.cursor.column / 8) + 1) * 8;
        self.cursor.column = next_tab_stop.min(self.dimensions.columns - 1);
    }

    fn cell_index(&self, column: usize, row: usize) -> Option<usize> {
        (column < self.dimensions.columns && row < self.dimensions.rows)
            .then_some(row * self.dimensions.columns + column)
    }

    fn mark_dirty(&mut self, row: usize) {
        self.dirty_rows[row] = true;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalOp {
    Print(char),
    CarriageReturn,
    LineFeed,
    Backspace,
    Tab,
    Ignored,
}

impl TerminalOp {
    pub const fn from_byte(byte: u8) -> Self {
        match byte {
            b'\r' => Self::CarriageReturn,
            b'\n' => Self::LineFeed,
            b'\x08' => Self::Backspace,
            b'\t' => Self::Tab,
            b' '..=b'~' => Self::Print(byte as char),
            _ => Self::Ignored,
        }
    }
}

const fn blank_cell() -> Cell {
    Cell { character: ' ' }
}

#[cfg(test)]
mod tests {
    use super::{Dimensions, Terminal};

    fn terminal(columns: usize, rows: usize) -> Terminal {
        Terminal::new(Dimensions::new(columns, rows).unwrap())
    }

    #[test]
    fn rejects_zero_dimensions() {
        assert!(Dimensions::new(0, 1).is_err());
        assert!(Dimensions::new(1, 0).is_err());
    }

    #[test]
    fn writes_printable_text_and_tracks_cursor() {
        let mut terminal = terminal(5, 2);
        terminal.take_dirty_rows();

        terminal.ingest(b"hi");

        assert_eq!(terminal.row_text(0).as_deref(), Some("hi   "));
        assert_eq!(terminal.cursor().column(), 2);
        assert_eq!(terminal.cursor().row(), 0);
        assert_eq!(terminal.take_dirty_rows(), vec![0]);
    }

    #[test]
    fn scrolls_when_output_reaches_bottom() {
        let mut terminal = terminal(3, 2);

        terminal.ingest(b"abcdefg");

        assert_eq!(terminal.row_text(0).as_deref(), Some("def"));
        assert_eq!(terminal.row_text(1).as_deref(), Some("g  "));
        assert_eq!(terminal.cursor().column(), 1);
        assert_eq!(terminal.cursor().row(), 1);
    }

    #[test]
    fn supports_basic_controls() {
        let mut terminal = terminal(10, 2);
        terminal.ingest(b"abc\rZ\n\x08!\tQ");

        assert_eq!(terminal.row_text(0).as_deref(), Some("Zbc       "));
        assert_eq!(terminal.row_text(1).as_deref(), Some("!       Q "));
        assert_eq!(terminal.cursor().column(), 9);
        assert_eq!(terminal.cursor().row(), 1);
    }
}
