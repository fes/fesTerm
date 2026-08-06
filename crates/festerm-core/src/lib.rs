//! GUI-independent terminal state primitives.
//!
//! This first core slice intentionally supports only printable ASCII and basic
//! C0 controls. ANSI/VT escape sequences are introduced in a later milestone.

use std::fmt;

/// The largest screen that the core will allocate.
pub const MAX_CELL_COUNT: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Dimensions {
    columns: usize,
    rows: usize,
    cell_count: usize,
}

impl Dimensions {
    pub fn new(columns: usize, rows: usize) -> Result<Self, DimensionsError> {
        if columns < 2 {
            return Err(DimensionsError::TooFewColumns { columns });
        }
        if rows == 0 {
            return Err(DimensionsError::ZeroRows);
        }

        let cell_count = columns
            .checked_mul(rows)
            .ok_or(DimensionsError::CellCountOverflow { columns, rows })?;
        if cell_count > MAX_CELL_COUNT {
            return Err(DimensionsError::TooManyCells {
                cell_count,
                maximum: MAX_CELL_COUNT,
            });
        }

        Ok(Self {
            columns,
            rows,
            cell_count,
        })
    }

    pub const fn columns(self) -> usize {
        self.columns
    }

    pub const fn rows(self) -> usize {
        self.rows
    }

    pub const fn cell_count(self) -> usize {
        self.cell_count
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DimensionsError {
    TooFewColumns { columns: usize },
    ZeroRows,
    CellCountOverflow { columns: usize, rows: usize },
    TooManyCells { cell_count: usize, maximum: usize },
}

impl fmt::Display for DimensionsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewColumns { columns } => {
                write!(
                    formatter,
                    "terminal requires at least 2 columns, received {columns}"
                )
            }
            Self::ZeroRows => formatter.write_str("terminal requires at least 1 row, received 0"),
            Self::CellCountOverflow { columns, rows } => write!(
                formatter,
                "terminal dimensions {columns} columns by {rows} rows overflow the cell count"
            ),
            Self::TooManyCells {
                cell_count,
                maximum,
            } => write!(
                formatter,
                "terminal dimensions require {cell_count} cells, exceeding the maximum of {maximum}"
            ),
        }
    }
}

impl std::error::Error for DimensionsError {}

/// A color value. Rendering and color application are intentionally deferred.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb {
        red: u8,
        green: u8,
        blue: u8,
    },
}

/// Cell attributes reserved for later ANSI/VT support.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Attributes {
    bits: u16,
}

impl Attributes {
    pub const NONE: Self = Self { bits: 0 };

    pub const fn bits(self) -> u16 {
        self.bits
    }
}

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
    foreground: Color,
    background: Color,
    attributes: Attributes,
}

impl Cell {
    pub const fn character(self) -> char {
        self.character
    }

    pub const fn foreground(self) -> Color {
        self.foreground
    }

    pub const fn background(self) -> Color {
        self.background
    }

    pub const fn attributes(self) -> Attributes {
        self.attributes
    }
}

/// The primary screen's cells and redraw state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Screen {
    dimensions: Dimensions,
    cells: Vec<Cell>,
    dirty_rows: Vec<bool>,
}

impl Screen {
    pub fn new(dimensions: Dimensions) -> Result<Self, TerminalError> {
        let mut cells = Vec::new();
        cells
            .try_reserve_exact(dimensions.cell_count())
            .map_err(|error| TerminalError::allocation("screen cells", error))?;
        cells.resize(dimensions.cell_count(), blank_cell());

        let mut dirty_rows = Vec::new();
        dirty_rows
            .try_reserve_exact(dimensions.rows())
            .map_err(|error| TerminalError::allocation("screen dirty rows", error))?;
        dirty_rows.resize(dimensions.rows(), true);

        Ok(Self {
            dimensions,
            cells,
            dirty_rows,
        })
    }

    pub const fn dimensions(&self) -> Dimensions {
        self.dimensions
    }

    pub fn cell(&self, column: usize, row: usize) -> Option<Cell> {
        self.cells.get(self.cell_index(column, row)?).copied()
    }

    pub fn row_text(&self, row: usize) -> Option<String> {
        if row >= self.dimensions.rows() {
            return None;
        }

        let start = row * self.dimensions.columns();
        let end = start + self.dimensions.columns();
        Some(
            self.cells[start..end]
                .iter()
                .map(|cell| cell.character)
                .collect(),
        )
    }

    pub fn is_row_dirty(&self, row: usize) -> Option<bool> {
        self.dirty_rows.get(row).copied()
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

    fn replace_cell(&mut self, column: usize, row: usize, cell: Cell) {
        let index = self
            .cell_index(column, row)
            .expect("terminal cursor must remain within screen dimensions");
        self.cells[index] = cell;
        self.mark_dirty(row);
    }

    fn scroll_up(&mut self) {
        self.cells.rotate_left(self.dimensions.columns());
        let start = (self.dimensions.rows() - 1) * self.dimensions.columns();
        self.cells[start..].fill(blank_cell());
        self.dirty_rows.fill(true);
    }

    fn cell_index(&self, column: usize, row: usize) -> Option<usize> {
        (column < self.dimensions.columns() && row < self.dimensions.rows())
            .then_some(row * self.dimensions.columns() + column)
    }

    fn mark_dirty(&mut self, row: usize) {
        self.dirty_rows[row] = true;
    }
}

/// Terminal modes with stable defaults for the initial core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalModes {
    auto_wrap: bool,
}

impl TerminalModes {
    pub const fn auto_wrap(self) -> bool {
        self.auto_wrap
    }
}

impl Default for TerminalModes {
    fn default() -> Self {
        Self { auto_wrap: true }
    }
}

/// A parser operation emitted from a single input byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalOp {
    Print(char),
    CarriageReturn,
    LineFeed,
    Backspace,
    Tab,
    Ignored,
}

/// The byte parser. It deliberately recognizes only M1 ASCII and C0 input.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Parser;

impl Parser {
    pub const fn new() -> Self {
        Self
    }

    pub const fn advance(&mut self, byte: u8) -> TerminalOp {
        match byte {
            b'\r' => TerminalOp::CarriageReturn,
            b'\n' => TerminalOp::LineFeed,
            b'\x08' => TerminalOp::Backspace,
            b'\t' => TerminalOp::Tab,
            b' '..=b'~' => TerminalOp::Print(byte as char),
            _ => TerminalOp::Ignored,
        }
    }
}

#[derive(Debug)]
pub struct TerminalError {
    message: String,
}

impl TerminalError {
    fn allocation(resource: &str, error: std::collections::TryReserveError) -> Self {
        Self {
            message: format!("unable to allocate {resource}: {error}"),
        }
    }
}

impl fmt::Display for TerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TerminalError {}

/// GUI-independent terminal state with one primary screen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Terminal {
    parser: Parser,
    primary_screen: Screen,
    cursor: Cursor,
    modes: TerminalModes,
    pending_wrap: bool,
    reply_queue: Vec<u8>,
    input_queue: Vec<u8>,
}

impl Terminal {
    pub fn new(dimensions: Dimensions) -> Result<Self, TerminalError> {
        Ok(Self {
            parser: Parser::new(),
            primary_screen: Screen::new(dimensions)?,
            cursor: Cursor { column: 0, row: 0 },
            modes: TerminalModes::default(),
            pending_wrap: false,
            reply_queue: Vec::new(),
            input_queue: Vec::new(),
        })
    }

    pub const fn dimensions(&self) -> Dimensions {
        self.primary_screen.dimensions()
    }

    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    pub const fn modes(&self) -> TerminalModes {
        self.modes
    }

    pub const fn screen(&self) -> &Screen {
        &self.primary_screen
    }

    pub fn cell(&self, column: usize, row: usize) -> Option<Cell> {
        self.primary_screen.cell(column, row)
    }

    pub fn row_text(&self, row: usize) -> Option<String> {
        self.primary_screen.row_text(row)
    }

    pub fn is_row_dirty(&self, row: usize) -> Option<bool> {
        self.primary_screen.is_row_dirty(row)
    }

    pub fn ingest(&mut self, bytes: &[u8]) {
        for byte in bytes {
            let operation = self.parser.advance(*byte);
            self.apply(operation);
        }
    }

    pub fn take_dirty_rows(&mut self) -> Vec<usize> {
        self.primary_screen.take_dirty_rows()
    }

    /// Queues bytes produced by future input encoders for the session transport.
    pub fn queue_input(&mut self, bytes: &[u8]) {
        self.input_queue.extend_from_slice(bytes);
    }

    pub fn queued_input(&self) -> &[u8] {
        &self.input_queue
    }

    pub fn drain_input(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.input_queue)
    }

    /// Queues bytes produced by future terminal protocol replies.
    pub fn queue_reply(&mut self, bytes: &[u8]) {
        self.reply_queue.extend_from_slice(bytes);
    }

    pub fn queued_replies(&self) -> &[u8] {
        &self.reply_queue
    }

    pub fn drain_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.reply_queue)
    }

    pub fn apply(&mut self, operation: TerminalOp) {
        match operation {
            TerminalOp::Print(character) => self.print(character),
            TerminalOp::CarriageReturn => {
                self.cursor.column = 0;
                self.pending_wrap = false;
            }
            TerminalOp::LineFeed => {
                self.line_feed();
                self.pending_wrap = false;
            }
            TerminalOp::Backspace => {
                self.cursor.column = self.cursor.column.saturating_sub(1);
                self.pending_wrap = false;
            }
            TerminalOp::Tab => {
                self.tab();
                self.pending_wrap = false;
            }
            TerminalOp::Ignored => {}
        }
    }

    fn print(&mut self, character: char) {
        if self.pending_wrap && self.modes.auto_wrap() {
            self.cursor.column = 0;
            self.line_feed();
            self.pending_wrap = false;
        }

        self.primary_screen.replace_cell(
            self.cursor.column,
            self.cursor.row,
            Cell {
                character,
                ..blank_cell()
            },
        );

        if self.cursor.column + 1 == self.dimensions().columns() {
            self.pending_wrap = true;
        } else {
            self.cursor.column += 1;
        }
    }

    fn line_feed(&mut self) {
        if self.cursor.row + 1 == self.dimensions().rows() {
            self.primary_screen.scroll_up();
        } else {
            self.cursor.row += 1;
            self.primary_screen.mark_dirty(self.cursor.row);
        }
    }

    fn tab(&mut self) {
        let next_tab_stop = ((self.cursor.column / 8) + 1) * 8;
        self.cursor.column = next_tab_stop.min(self.dimensions().columns() - 1);
    }
}

const fn blank_cell() -> Cell {
    Cell {
        character: ' ',
        foreground: Color::Default,
        background: Color::Default,
        attributes: Attributes::NONE,
    }
}

#[cfg(test)]
mod tests {
    use super::{Dimensions, Parser, Terminal, TerminalOp, MAX_CELL_COUNT};

    fn terminal(columns: usize, rows: usize) -> Terminal {
        Terminal::new(Dimensions::new(columns, rows).unwrap()).unwrap()
    }

    #[test]
    fn validates_dimensions_before_allocating() {
        assert!(Dimensions::new(0, 1).is_err());
        assert!(Dimensions::new(1, 1).is_err());
        assert!(Dimensions::new(2, 0).is_err());
        assert!(Dimensions::new(usize::MAX, 2).is_err());
        assert!(Dimensions::new(2, (MAX_CELL_COUNT / 2) + 1).is_err());
        assert!(Terminal::new(Dimensions::new(2, 1).unwrap()).is_ok());
    }

    #[test]
    fn parser_emits_milestone_one_operations() {
        let mut parser = Parser::new();
        assert_eq!(parser.advance(b'A'), TerminalOp::Print('A'));
        assert_eq!(parser.advance(b'\r'), TerminalOp::CarriageReturn);
        assert_eq!(parser.advance(0x9b), TerminalOp::Ignored);
    }

    #[test]
    fn writes_printable_text_and_tracks_cursor_and_dirty_rows() {
        let mut terminal = terminal(5, 2);
        assert_eq!(terminal.take_dirty_rows(), vec![0, 1]);
        assert_eq!(terminal.is_row_dirty(0), Some(false));

        terminal.ingest(b"hi");

        assert_eq!(terminal.row_text(0).as_deref(), Some("hi   "));
        assert_eq!(terminal.cursor().column(), 2);
        assert_eq!(terminal.cursor().row(), 0);
        assert_eq!(terminal.take_dirty_rows(), vec![0]);
    }

    #[test]
    fn retains_the_final_bottom_right_byte_until_next_printable_byte() {
        let mut terminal = terminal(3, 2);
        terminal.ingest(b"abcdef");

        assert_eq!(terminal.row_text(0).as_deref(), Some("abc"));
        assert_eq!(terminal.row_text(1).as_deref(), Some("def"));
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (2, 1)
        );

        terminal.ingest(b"g");
        assert_eq!(terminal.row_text(0).as_deref(), Some("def"));
        assert_eq!(terminal.row_text(1).as_deref(), Some("g  "));
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (1, 1)
        );
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

    #[test]
    fn output_queues_preserve_and_drain_exact_bytes() {
        let mut terminal = terminal(2, 1);
        terminal.queue_input(&[0x80, b'A']);
        terminal.queue_reply(&[0x9b, b'R']);

        assert_eq!(terminal.queued_input(), &[0x80, b'A']);
        assert_eq!(terminal.queued_replies(), &[0x9b, b'R']);
        assert_eq!(terminal.drain_input(), vec![0x80, b'A']);
        assert_eq!(terminal.drain_replies(), vec![0x9b, b'R']);
        assert!(terminal.queued_input().is_empty());
        assert!(terminal.queued_replies().is_empty());
    }
}
