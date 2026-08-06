//! GUI-independent ANSI/VT terminal state primitives.
//!
//! The parser accepts printable ASCII and C0 controls plus 7-bit ESC/CSI
//! syntax. Raw C1 bytes are deliberately not controls: treating them as such
//! would make UTF-8 continuation bytes ambiguous.

use std::fmt;

/// The largest screen that the core will allocate.
pub const MAX_CELL_COUNT: usize = 4 * 1024 * 1024;
/// The maximum number of CSI parameters retained by the parser.
pub const MAX_CSI_PARAMETERS: usize = 32;
/// The maximum number of CSI intermediate bytes retained by the parser.
pub const MAX_CSI_INTERMEDIATES: usize = 2;
/// The maximum unsupported string-protocol payload discarded before recovery.
pub const MAX_STRING_BYTES: usize = 4096;
/// The maximum number of bytes retained by either session transport queue.
///
/// A queued write is accepted atomically: if the entire write does not fit,
/// none of its bytes are retained and the caller receives an overflow result.
pub const TRANSPORT_QUEUE_HIGH_WATERMARK: usize = 64 * 1024;

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

/// The observable outcome of adding bytes to a session transport queue.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueuePushResult {
    accepted: usize,
    overflowed: bool,
}

impl QueuePushResult {
    /// The number of bytes accepted, either the complete write or zero.
    pub const fn accepted(self) -> usize {
        self.accepted
    }

    /// Whether the write was rejected because the queue high watermark was met.
    pub const fn overflowed(self) -> bool {
        self.overflowed
    }
}

/// A color value used by a cell.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Color {
    #[default]
    Default,
    /// ANSI palette entries use 0 through 15; SGR indexed colors may use all
    /// values through 255.
    Indexed(u8),
    Rgb {
        red: u8,
        green: u8,
        blue: u8,
    },
}

/// Bitflags for the standard SGR text attributes supported by M2.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Attributes {
    bits: u16,
}

impl Attributes {
    pub const NONE: Self = Self { bits: 0 };
    pub const BOLD: Self = Self { bits: 1 << 0 };
    pub const FAINT: Self = Self { bits: 1 << 1 };
    pub const ITALIC: Self = Self { bits: 1 << 2 };
    pub const UNDERLINE: Self = Self { bits: 1 << 3 };
    pub const DOUBLE_UNDERLINE: Self = Self { bits: 1 << 4 };
    pub const SLOW_BLINK: Self = Self { bits: 1 << 5 };
    pub const RAPID_BLINK: Self = Self { bits: 1 << 6 };
    pub const INVERSE: Self = Self { bits: 1 << 7 };
    pub const CONCEALED: Self = Self { bits: 1 << 8 };
    pub const STRIKETHROUGH: Self = Self { bits: 1 << 9 };

    pub const fn bits(self) -> u16 {
        self.bits
    }

    pub const fn from_bits(bits: u16) -> Self {
        Self { bits }
    }

    pub const fn contains(self, other: Self) -> bool {
        self.bits & other.bits == other.bits
    }

    const fn with(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }

    const fn without(self, other: Self) -> Self {
        Self {
            bits: self.bits & !other.bits,
        }
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

/// A visible grid and its redraw state.
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

    fn resized(&self, dimensions: Dimensions) -> Result<Self, TerminalError> {
        let mut resized = Self::new(dimensions)?;
        let preserved_rows = self.dimensions.rows().min(dimensions.rows());
        let preserved_columns = self.dimensions.columns().min(dimensions.columns());
        for row in 0..preserved_rows {
            let old_start = row * self.dimensions.columns();
            let new_start = row * dimensions.columns();
            resized.cells[new_start..new_start + preserved_columns]
                .copy_from_slice(&self.cells[old_start..old_start + preserved_columns]);
        }
        Ok(resized)
    }

    fn replace_cell(&mut self, column: usize, row: usize, cell: Cell) {
        let index = self
            .cell_index(column, row)
            .expect("terminal cursor must remain within screen dimensions");
        self.cells[index] = cell;
        self.mark_dirty(row);
    }

    fn fill_linear(&mut self, start: usize, end: usize, cell: Cell) {
        if start >= end {
            return;
        }
        self.cells[start..end].fill(cell);
        let columns = self.dimensions.columns();
        for row in start / columns..=(end - 1) / columns {
            self.mark_dirty(row);
        }
    }

    fn clear_all(&mut self, cell: Cell) {
        self.cells.fill(cell);
        self.mark_all_dirty();
    }

    fn insert_characters(&mut self, column: usize, row: usize, count: usize, cell: Cell) {
        let columns = self.dimensions.columns();
        let count = count.min(columns - column);
        let row_start = row * columns;
        let row_end = row_start + columns;
        let start = row_start + column;
        self.cells
            .copy_within(start..row_end - count, start + count);
        self.cells[start..start + count].fill(cell);
        self.mark_dirty(row);
    }

    fn delete_characters(&mut self, column: usize, row: usize, count: usize, cell: Cell) {
        let columns = self.dimensions.columns();
        let count = count.min(columns - column);
        let row_start = row * columns;
        let row_end = row_start + columns;
        let start = row_start + column;
        self.cells.copy_within(start + count..row_end, start);
        self.cells[row_end - count..row_end].fill(cell);
        self.mark_dirty(row);
    }

    fn insert_lines(&mut self, row: usize, bottom: usize, count: usize, cell: Cell) {
        let columns = self.dimensions.columns();
        let count = count.min(bottom - row + 1);
        let source_end = (bottom + 1 - count) * columns;
        self.cells
            .copy_within(row * columns..source_end, (row + count) * columns);
        self.cells[row * columns..(row + count) * columns].fill(cell);
        self.mark_dirty_range(row, bottom);
    }

    fn delete_lines(&mut self, row: usize, bottom: usize, count: usize, cell: Cell) {
        let columns = self.dimensions.columns();
        let count = count.min(bottom - row + 1);
        self.cells.copy_within(
            (row + count) * columns..(bottom + 1) * columns,
            row * columns,
        );
        self.cells[(bottom + 1 - count) * columns..(bottom + 1) * columns].fill(cell);
        self.mark_dirty_range(row, bottom);
    }

    fn scroll_up(&mut self, top: usize, bottom: usize, count: usize, cell: Cell) {
        let columns = self.dimensions.columns();
        let count = count.min(bottom - top + 1);
        let region_start = top * columns;
        let region_end = (bottom + 1) * columns;
        let shifted_cells = count * columns;
        self.cells
            .copy_within(region_start + shifted_cells..region_end, region_start);
        self.cells[region_end - shifted_cells..region_end].fill(cell);
        self.mark_dirty_range(top, bottom);
    }

    fn scroll_down(&mut self, top: usize, bottom: usize, count: usize, cell: Cell) {
        let columns = self.dimensions.columns();
        let count = count.min(bottom - top + 1);
        let region_start = top * columns;
        let region_end = (bottom + 1) * columns;
        let shifted_cells = count * columns;
        self.cells.copy_within(
            region_start..region_end - shifted_cells,
            region_start + shifted_cells,
        );
        self.cells[region_start..region_start + shifted_cells].fill(cell);
        self.mark_dirty_range(top, bottom);
    }

    fn cell_index(&self, column: usize, row: usize) -> Option<usize> {
        (column < self.dimensions.columns() && row < self.dimensions.rows())
            .then_some(row * self.dimensions.columns() + column)
    }

    fn mark_dirty(&mut self, row: usize) {
        self.dirty_rows[row] = true;
    }

    fn mark_dirty_range(&mut self, first: usize, last: usize) {
        self.dirty_rows[first..=last].fill(true);
    }

    fn mark_all_dirty(&mut self) {
        self.dirty_rows.fill(true);
    }
}

/// Terminal modes implemented by this milestone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalModes {
    auto_wrap: bool,
    origin_mode: bool,
    alternate_screen: bool,
    cursor_visible: bool,
}

impl TerminalModes {
    pub const fn auto_wrap(self) -> bool {
        self.auto_wrap
    }

    pub const fn origin_mode(self) -> bool {
        self.origin_mode
    }

    pub const fn alternate_screen(self) -> bool {
        self.alternate_screen
    }

    pub const fn cursor_visible(self) -> bool {
        self.cursor_visible
    }
}

impl Default for TerminalModes {
    fn default() -> Self {
        Self {
            auto_wrap: true,
            origin_mode: false,
            alternate_screen: false,
            cursor_visible: true,
        }
    }
}

/// The separator preceding a retained CSI parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParameterSeparator {
    Start,
    Semicolon,
    Colon,
}

/// Bounded CSI parameters, including their semicolon/colon structure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CsiParameters {
    values: [u16; MAX_CSI_PARAMETERS],
    separators: [ParameterSeparator; MAX_CSI_PARAMETERS],
    length: usize,
}

impl Default for CsiParameters {
    fn default() -> Self {
        Self {
            values: [0; MAX_CSI_PARAMETERS],
            separators: [ParameterSeparator::Start; MAX_CSI_PARAMETERS],
            length: 0,
        }
    }
}

impl CsiParameters {
    pub const fn len(self) -> usize {
        self.length
    }

    pub const fn is_empty(self) -> bool {
        self.length == 0
    }

    pub const fn value(self, index: usize) -> Option<u16> {
        if index < self.length {
            Some(self.values[index])
        } else {
            None
        }
    }

    pub const fn separator(self, index: usize) -> Option<ParameterSeparator> {
        if index < self.length {
            Some(self.separators[index])
        } else {
            None
        }
    }

    pub fn has_colon(self) -> bool {
        self.separators[..self.length].contains(&ParameterSeparator::Colon)
    }

    fn begin(&mut self, separator: ParameterSeparator) -> bool {
        if self.length == MAX_CSI_PARAMETERS {
            return false;
        }
        self.separators[self.length] = separator;
        self.values[self.length] = 0;
        self.length += 1;
        true
    }

    fn append_separator(&mut self, separator: ParameterSeparator) -> bool {
        if self.length == 0 && !self.begin(ParameterSeparator::Start) {
            return false;
        }
        self.begin(separator)
    }

    fn append_digit(&mut self, digit: u8) -> bool {
        if self.length == 0 && !self.begin(ParameterSeparator::Start) {
            return false;
        }
        let index = self.length - 1;
        let digit = u16::from(digit);
        let Some(value) = self.values[index]
            .checked_mul(10)
            .and_then(|value| value.checked_add(digit))
        else {
            return false;
        };
        self.values[index] = value;
        true
    }
}

/// A typed operation emitted by [`Parser`] and applied by [`Terminal`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalOp {
    Print(char),
    CarriageReturn,
    LineFeed,
    Backspace,
    Tab,
    Index,
    NextLine,
    ReverseIndex,
    SaveDec,
    RestoreDec,
    CursorUp(CsiParameters),
    CursorDown(CsiParameters),
    CursorForward(CsiParameters),
    CursorBack(CsiParameters),
    CursorNextLine(CsiParameters),
    CursorPreviousLine(CsiParameters),
    CursorHorizontalAbsolute(CsiParameters),
    CursorPosition(CsiParameters),
    VerticalPositionAbsolute(CsiParameters),
    EraseDisplay(CsiParameters),
    EraseLine(CsiParameters),
    EraseCharacters(CsiParameters),
    InsertCharacters(CsiParameters),
    DeleteCharacters(CsiParameters),
    InsertLines(CsiParameters),
    DeleteLines(CsiParameters),
    ScrollUp(CsiParameters),
    ScrollDown(CsiParameters),
    SetScrollRegion(CsiParameters),
    SaveAnsi,
    RestoreAnsi,
    SetGraphicsRendition(CsiParameters),
    SetModes {
        private: bool,
        enabled: bool,
        parameters: CsiParameters,
    },
    DeviceStatus(CsiParameters),
    Ignored,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StringKind {
    Osc,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParserState {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    String { kind: StringKind, bytes: usize },
    StringEscape { kind: StringKind, bytes: usize },
}

/// A bounded state-machine parser for M2 ESC and CSI input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Parser {
    state: ParserState,
    parameters: CsiParameters,
    parameter_digits: u8,
    private: bool,
    intermediates: [u8; MAX_CSI_INTERMEDIATES],
    intermediate_length: usize,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub const fn new() -> Self {
        Self {
            state: ParserState::Ground,
            parameters: CsiParameters {
                values: [0; MAX_CSI_PARAMETERS],
                separators: [ParameterSeparator::Start; MAX_CSI_PARAMETERS],
                length: 0,
            },
            parameter_digits: 0,
            private: false,
            intermediates: [0; MAX_CSI_INTERMEDIATES],
            intermediate_length: 0,
        }
    }

    /// Advances one byte and returns at most one fully typed operation.
    pub fn advance(&mut self, byte: u8) -> TerminalOp {
        if matches!(byte, 0x18 | 0x1a) {
            self.state = ParserState::Ground;
            self.clear_csi();
            return TerminalOp::Ignored;
        }
        if matches!(
            self.state,
            ParserState::String { .. } | ParserState::StringEscape { .. }
        ) {
            if let Some(operation) = c0_operation(byte) {
                return operation;
            }
            return self.advance_string(byte);
        }

        if byte == 0x1b {
            self.state = ParserState::Escape;
            self.clear_csi();
            return TerminalOp::Ignored;
        }
        if let Some(operation) = c0_operation(byte) {
            return operation;
        }

        match self.state {
            ParserState::Ground => match byte {
                b' '..=b'~' => TerminalOp::Print(byte as char),
                _ => TerminalOp::Ignored,
            },
            ParserState::Escape => self.advance_escape(byte),
            ParserState::EscapeIntermediate => {
                self.state = ParserState::Ground;
                TerminalOp::Ignored
            }
            ParserState::CsiEntry => self.advance_csi_entry(byte),
            ParserState::CsiParam => self.advance_csi_param(byte),
            ParserState::CsiIntermediate => self.advance_csi_intermediate(byte),
            ParserState::CsiIgnore => {
                if (0x40..=0x7e).contains(&byte) {
                    self.state = ParserState::Ground;
                    self.clear_csi();
                }
                TerminalOp::Ignored
            }
            ParserState::String { .. } | ParserState::StringEscape { .. } => {
                unreachable!("string states return before parser dispatch")
            }
        }
    }

    fn advance_escape(&mut self, byte: u8) -> TerminalOp {
        self.state = match byte {
            b'[' => {
                self.clear_csi();
                ParserState::CsiEntry
            }
            b']' => ParserState::String {
                kind: StringKind::Osc,
                bytes: 0,
            },
            b'P' | b'X' | b'^' | b'_' => ParserState::String {
                kind: StringKind::Other,
                bytes: 0,
            },
            0x20..=0x2f => ParserState::EscapeIntermediate,
            _ => ParserState::Ground,
        };
        match byte {
            b'D' => TerminalOp::Index,
            b'E' => TerminalOp::NextLine,
            b'M' => TerminalOp::ReverseIndex,
            b'7' => TerminalOp::SaveDec,
            b'8' => TerminalOp::RestoreDec,
            _ => TerminalOp::Ignored,
        }
    }

    fn advance_csi_entry(&mut self, byte: u8) -> TerminalOp {
        match byte {
            b'?' if !self.private && self.parameters.is_empty() => {
                self.private = true;
                TerminalOp::Ignored
            }
            b'0'..=b'9' => self.append_csi_digit(byte),
            b';' => self.append_csi_separator(ParameterSeparator::Semicolon),
            b':' => self.append_csi_separator(ParameterSeparator::Colon),
            0x20..=0x2f => self.append_csi_intermediate(byte),
            0x40..=0x7e => self.complete_csi(byte),
            _ => self.ignore_csi(),
        }
    }

    fn advance_csi_param(&mut self, byte: u8) -> TerminalOp {
        match byte {
            b'0'..=b'9' => self.append_csi_digit(byte),
            b';' => self.append_csi_separator(ParameterSeparator::Semicolon),
            b':' => self.append_csi_separator(ParameterSeparator::Colon),
            0x20..=0x2f => self.append_csi_intermediate(byte),
            0x40..=0x7e => self.complete_csi(byte),
            _ => self.ignore_csi(),
        }
    }

    fn advance_csi_intermediate(&mut self, byte: u8) -> TerminalOp {
        match byte {
            0x20..=0x2f => self.append_csi_intermediate(byte),
            0x40..=0x7e => self.complete_csi(byte),
            _ => self.ignore_csi(),
        }
    }

    fn advance_string(&mut self, byte: u8) -> TerminalOp {
        match self.state {
            ParserState::String { kind, bytes } => {
                if kind == StringKind::Osc && byte == 0x07 {
                    self.state = ParserState::Ground;
                } else if byte == 0x1b {
                    self.state = ParserState::StringEscape { kind, bytes };
                } else {
                    self.advance_string_payload(kind, bytes, 1);
                }
            }
            ParserState::StringEscape { kind, bytes } => {
                if byte == b'\\' {
                    self.state = ParserState::Ground;
                } else if byte == 0x1b {
                    self.advance_string_payload(kind, bytes, 1);
                    if let ParserState::String { bytes, .. } = self.state {
                        self.state = ParserState::StringEscape { kind, bytes };
                    }
                } else {
                    self.advance_string_payload(kind, bytes, 2);
                }
            }
            _ => unreachable!("only string states call advance_string"),
        }
        TerminalOp::Ignored
    }

    fn advance_string_payload(&mut self, kind: StringKind, bytes: usize, additional: usize) {
        let Some(bytes) = bytes.checked_add(additional) else {
            self.state = ParserState::Ground;
            return;
        };
        self.state = if bytes >= MAX_STRING_BYTES {
            ParserState::Ground
        } else {
            ParserState::String { kind, bytes }
        };
    }

    fn append_csi_digit(&mut self, byte: u8) -> TerminalOp {
        self.parameter_digits = self.parameter_digits.saturating_add(1);
        if self.parameter_digits > 5 || !self.parameters.append_digit(byte - b'0') {
            return self.ignore_csi();
        }
        self.state = ParserState::CsiParam;
        TerminalOp::Ignored
    }

    fn append_csi_separator(&mut self, separator: ParameterSeparator) -> TerminalOp {
        if !self.parameters.append_separator(separator) {
            return self.ignore_csi();
        }
        self.parameter_digits = 0;
        self.state = ParserState::CsiParam;
        TerminalOp::Ignored
    }

    fn append_csi_intermediate(&mut self, byte: u8) -> TerminalOp {
        if self.intermediate_length == MAX_CSI_INTERMEDIATES {
            return self.ignore_csi();
        }
        self.intermediates[self.intermediate_length] = byte;
        self.intermediate_length += 1;
        self.state = ParserState::CsiIntermediate;
        TerminalOp::Ignored
    }

    fn complete_csi(&mut self, final_byte: u8) -> TerminalOp {
        let parameters = self.parameters;
        let private = self.private;
        let has_intermediates = self.intermediate_length != 0;
        self.state = ParserState::Ground;
        self.clear_csi();

        if has_intermediates {
            return TerminalOp::Ignored;
        }
        if private {
            return match final_byte {
                b'h' => TerminalOp::SetModes {
                    private: true,
                    enabled: true,
                    parameters,
                },
                b'l' => TerminalOp::SetModes {
                    private: true,
                    enabled: false,
                    parameters,
                },
                _ => TerminalOp::Ignored,
            };
        }
        if parameters.has_colon() && final_byte != b'm' {
            return TerminalOp::Ignored;
        }

        match final_byte {
            b'A' => TerminalOp::CursorUp(parameters),
            b'B' => TerminalOp::CursorDown(parameters),
            b'C' => TerminalOp::CursorForward(parameters),
            b'D' => TerminalOp::CursorBack(parameters),
            b'E' => TerminalOp::CursorNextLine(parameters),
            b'F' => TerminalOp::CursorPreviousLine(parameters),
            b'G' | b'`' => TerminalOp::CursorHorizontalAbsolute(parameters),
            b'H' | b'f' => TerminalOp::CursorPosition(parameters),
            b'J' => TerminalOp::EraseDisplay(parameters),
            b'K' => TerminalOp::EraseLine(parameters),
            b'L' => TerminalOp::InsertLines(parameters),
            b'M' => TerminalOp::DeleteLines(parameters),
            b'P' => TerminalOp::DeleteCharacters(parameters),
            b'S' => TerminalOp::ScrollUp(parameters),
            b'T' => TerminalOp::ScrollDown(parameters),
            b'X' => TerminalOp::EraseCharacters(parameters),
            b'@' => TerminalOp::InsertCharacters(parameters),
            b'd' => TerminalOp::VerticalPositionAbsolute(parameters),
            b'm' => TerminalOp::SetGraphicsRendition(parameters),
            b'n' => TerminalOp::DeviceStatus(parameters),
            b'r' => TerminalOp::SetScrollRegion(parameters),
            b's' if parameters.is_empty() => TerminalOp::SaveAnsi,
            b'u' if parameters.is_empty() => TerminalOp::RestoreAnsi,
            b'h' => TerminalOp::SetModes {
                private: false,
                enabled: true,
                parameters,
            },
            b'l' => TerminalOp::SetModes {
                private: false,
                enabled: false,
                parameters,
            },
            _ => TerminalOp::Ignored,
        }
    }

    fn ignore_csi(&mut self) -> TerminalOp {
        self.state = ParserState::CsiIgnore;
        self.clear_csi();
        TerminalOp::Ignored
    }

    fn clear_csi(&mut self) {
        self.parameters = CsiParameters::default();
        self.parameter_digits = 0;
        self.private = false;
        self.intermediate_length = 0;
    }
}

fn c0_operation(byte: u8) -> Option<TerminalOp> {
    match byte {
        b'\r' => Some(TerminalOp::CarriageReturn),
        b'\n' => Some(TerminalOp::LineFeed),
        b'\x08' => Some(TerminalOp::Backspace),
        b'\t' => Some(TerminalOp::Tab),
        _ => None,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveScreen {
    Primary,
    Alternate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BufferState {
    screen: Screen,
    cursor: Cursor,
    scroll_top: usize,
    scroll_bottom: usize,
    pending_wrap: bool,
    dec_saved: Option<SavedDecState>,
    ansi_saved: Option<SavedAnsiCursor>,
}

impl BufferState {
    fn new(dimensions: Dimensions) -> Result<Self, TerminalError> {
        Ok(Self {
            screen: Screen::new(dimensions)?,
            cursor: Cursor { column: 0, row: 0 },
            scroll_top: 0,
            scroll_bottom: dimensions.rows() - 1,
            pending_wrap: false,
            dec_saved: None,
            ansi_saved: None,
        })
    }

    fn resized(&self, dimensions: Dimensions) -> Result<Self, TerminalError> {
        let mut resized = Self {
            screen: self.screen.resized(dimensions)?,
            cursor: Cursor {
                column: self.cursor.column.min(dimensions.columns() - 1),
                row: self.cursor.row.min(dimensions.rows() - 1),
            },
            scroll_top: self.scroll_top.min(dimensions.rows() - 1),
            scroll_bottom: self.scroll_bottom.min(dimensions.rows() - 1),
            pending_wrap: self.pending_wrap,
            dec_saved: self.dec_saved,
            ansi_saved: self.ansi_saved,
        };
        if resized.scroll_top >= resized.scroll_bottom && dimensions.rows() > 1 {
            resized.scroll_top = 0;
            resized.scroll_bottom = dimensions.rows() - 1;
        }
        resized.pending_wrap &= resized.cursor.column + 1 == dimensions.columns();
        resized.clamp_saved_states(dimensions);
        Ok(resized)
    }

    fn reset(&mut self) {
        self.screen.clear_all(blank_cell());
        self.cursor = Cursor { column: 0, row: 0 };
        self.scroll_top = 0;
        self.scroll_bottom = self.screen.dimensions().rows() - 1;
        self.pending_wrap = false;
        self.dec_saved = None;
        self.ansi_saved = None;
    }

    fn clamp_saved_states(&mut self, dimensions: Dimensions) {
        if let Some(saved) = &mut self.dec_saved {
            saved.cursor.column = saved.cursor.column.min(dimensions.columns() - 1);
            saved.cursor.row = saved.cursor.row.min(dimensions.rows() - 1);
            if saved.origin_mode {
                saved.cursor.row = saved.cursor.row.clamp(self.scroll_top, self.scroll_bottom);
            }
            saved.pending_wrap &= saved.cursor.column + 1 == dimensions.columns();
        }
        if let Some(saved) = &mut self.ansi_saved {
            saved.cursor.column = saved.cursor.column.min(dimensions.columns() - 1);
            saved.cursor.row = saved.cursor.row.min(dimensions.rows() - 1);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SavedDecState {
    cursor: Cursor,
    pending_wrap: bool,
    attributes: Attributes,
    foreground: Color,
    background: Color,
    origin_mode: bool,
    auto_wrap: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SavedAnsiCursor {
    cursor: Cursor,
}

/// GUI-independent M2 terminal state. The terminal owns one logical writer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Terminal {
    parser: Parser,
    primary: BufferState,
    alternate: Option<BufferState>,
    active_screen: ActiveScreen,
    modes: TerminalModes,
    current_attributes: Attributes,
    current_foreground: Color,
    current_background: Color,
    reply_queue: Vec<u8>,
    input_queue: Vec<u8>,
    reply_queue_overflowed: bool,
    input_queue_overflowed: bool,
}

impl Terminal {
    pub fn new(dimensions: Dimensions) -> Result<Self, TerminalError> {
        Ok(Self {
            parser: Parser::new(),
            primary: BufferState::new(dimensions)?,
            alternate: None,
            active_screen: ActiveScreen::Primary,
            modes: TerminalModes::default(),
            current_attributes: Attributes::NONE,
            current_foreground: Color::Default,
            current_background: Color::Default,
            reply_queue: Vec::new(),
            input_queue: Vec::new(),
            reply_queue_overflowed: false,
            input_queue_overflowed: false,
        })
    }

    pub const fn dimensions(&self) -> Dimensions {
        self.primary.screen.dimensions()
    }

    pub const fn cursor(&self) -> Cursor {
        match self.active_screen {
            ActiveScreen::Primary => self.primary.cursor,
            ActiveScreen::Alternate => match &self.alternate {
                Some(alternate) => alternate.cursor,
                None => self.primary.cursor,
            },
        }
    }

    pub const fn modes(&self) -> TerminalModes {
        self.modes
    }

    pub const fn attributes(&self) -> Attributes {
        self.current_attributes
    }

    pub const fn foreground(&self) -> Color {
        self.current_foreground
    }

    pub const fn background(&self) -> Color {
        self.current_background
    }

    pub fn screen(&self) -> &Screen {
        &self.active_buffer().screen
    }

    pub fn primary_screen(&self) -> &Screen {
        &self.primary.screen
    }

    pub fn alternate_screen(&self) -> Option<&Screen> {
        self.alternate.as_ref().map(|alternate| &alternate.screen)
    }

    pub fn cell(&self, column: usize, row: usize) -> Option<Cell> {
        self.screen().cell(column, row)
    }

    pub fn row_text(&self, row: usize) -> Option<String> {
        self.screen().row_text(row)
    }

    pub fn is_row_dirty(&self, row: usize) -> Option<bool> {
        self.screen().is_row_dirty(row)
    }

    pub fn ingest(&mut self, bytes: &[u8]) {
        for byte in bytes {
            let operation = self.parser.advance(*byte);
            self.apply(operation);
        }
    }

    /// Resizes grids without reflow, retaining the upper-left intersection.
    pub fn resize(&mut self, dimensions: Dimensions) -> Result<(), TerminalError> {
        let primary = self.primary.resized(dimensions)?;
        let alternate = match &self.alternate {
            Some(alternate) => Some(alternate.resized(dimensions)?),
            None => None,
        };

        self.primary = primary;
        self.alternate = alternate;
        Ok(())
    }

    pub fn take_dirty_rows(&mut self) -> Vec<usize> {
        self.active_buffer_mut().screen.take_dirty_rows()
    }

    /// Queues an atomic input write for the session transport.
    ///
    /// The write is rejected when it would exceed
    /// [`TRANSPORT_QUEUE_HIGH_WATERMARK`]. Call
    /// [`Self::take_input_queue_overflowed`] to observe automatic or prior
    /// rejected writes.
    pub fn queue_input(&mut self, bytes: &[u8]) -> QueuePushResult {
        let result = queue_transport_bytes(&mut self.input_queue, bytes);
        self.input_queue_overflowed |= result.overflowed();
        result
    }

    pub fn queued_input(&self) -> &[u8] {
        &self.input_queue
    }

    pub fn drain_input(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.input_queue)
    }

    /// Reports and clears whether an input write overflowed since the last call.
    pub fn take_input_queue_overflowed(&mut self) -> bool {
        std::mem::take(&mut self.input_queue_overflowed)
    }

    /// Queues an atomic terminal-protocol reply for the session transport.
    ///
    /// The write is rejected when it would exceed
    /// [`TRANSPORT_QUEUE_HIGH_WATERMARK`]. Call
    /// [`Self::take_reply_queue_overflowed`] to observe rejected automatic
    /// replies, including DSR responses.
    pub fn queue_reply(&mut self, bytes: &[u8]) -> QueuePushResult {
        let result = queue_transport_bytes(&mut self.reply_queue, bytes);
        self.reply_queue_overflowed |= result.overflowed();
        result
    }

    pub fn queued_replies(&self) -> &[u8] {
        &self.reply_queue
    }

    pub fn drain_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.reply_queue)
    }

    /// Reports and clears whether a reply write overflowed since the last call.
    pub fn take_reply_queue_overflowed(&mut self) -> bool {
        std::mem::take(&mut self.reply_queue_overflowed)
    }

    pub fn apply(&mut self, operation: TerminalOp) {
        match operation {
            TerminalOp::Print(character) => self.print(character),
            TerminalOp::CarriageReturn => {
                self.active_buffer_mut().cursor.column = 0;
                self.clear_pending_wrap();
            }
            TerminalOp::LineFeed | TerminalOp::Index => {
                self.index();
                self.clear_pending_wrap();
            }
            TerminalOp::Backspace => {
                let buffer = self.active_buffer_mut();
                buffer.cursor.column = buffer.cursor.column.saturating_sub(1);
                buffer.pending_wrap = false;
            }
            TerminalOp::Tab => {
                self.tab();
                self.clear_pending_wrap();
            }
            TerminalOp::NextLine => {
                self.active_buffer_mut().cursor.column = 0;
                self.index();
                self.clear_pending_wrap();
            }
            TerminalOp::ReverseIndex => {
                self.reverse_index();
                self.clear_pending_wrap();
            }
            TerminalOp::SaveDec => self.save_dec(),
            TerminalOp::RestoreDec => self.restore_dec(),
            TerminalOp::CursorUp(parameters) => self.move_vertical(parameters, false),
            TerminalOp::CursorDown(parameters) => self.move_vertical(parameters, true),
            TerminalOp::CursorForward(parameters) => self.move_horizontal(parameters, true),
            TerminalOp::CursorBack(parameters) => self.move_horizontal(parameters, false),
            TerminalOp::CursorNextLine(parameters) => {
                self.move_vertical(parameters, true);
                self.active_buffer_mut().cursor.column = 0;
            }
            TerminalOp::CursorPreviousLine(parameters) => {
                self.move_vertical(parameters, false);
                self.active_buffer_mut().cursor.column = 0;
            }
            TerminalOp::CursorHorizontalAbsolute(parameters) => {
                self.cursor_horizontal_absolute(parameters)
            }
            TerminalOp::CursorPosition(parameters) => self.cursor_position(parameters),
            TerminalOp::VerticalPositionAbsolute(parameters) => {
                self.vertical_position_absolute(parameters)
            }
            TerminalOp::EraseDisplay(parameters) => self.erase_display(parameters),
            TerminalOp::EraseLine(parameters) => self.erase_line(parameters),
            TerminalOp::EraseCharacters(parameters) => self.erase_characters(parameters),
            TerminalOp::InsertCharacters(parameters) => self.insert_characters(parameters),
            TerminalOp::DeleteCharacters(parameters) => self.delete_characters(parameters),
            TerminalOp::InsertLines(parameters) => self.insert_lines(parameters),
            TerminalOp::DeleteLines(parameters) => self.delete_lines(parameters),
            TerminalOp::ScrollUp(parameters) => self.scroll_up(parameters),
            TerminalOp::ScrollDown(parameters) => self.scroll_down(parameters),
            TerminalOp::SetScrollRegion(parameters) => self.set_scroll_region(parameters),
            TerminalOp::SaveAnsi => self.save_ansi(),
            TerminalOp::RestoreAnsi => self.restore_ansi(),
            TerminalOp::SetGraphicsRendition(parameters) => self.set_graphics_rendition(parameters),
            TerminalOp::SetModes {
                private,
                enabled,
                parameters,
            } => self.set_modes(private, enabled, parameters),
            TerminalOp::DeviceStatus(parameters) => self.device_status(parameters),
            TerminalOp::Ignored => {}
        }
    }

    fn active_buffer(&self) -> &BufferState {
        match self.active_screen {
            ActiveScreen::Primary => &self.primary,
            ActiveScreen::Alternate => self
                .alternate
                .as_ref()
                .expect("alternate state exists while active"),
        }
    }

    fn active_buffer_mut(&mut self) -> &mut BufferState {
        match self.active_screen {
            ActiveScreen::Primary => &mut self.primary,
            ActiveScreen::Alternate => self
                .alternate
                .as_mut()
                .expect("alternate state exists while active"),
        }
    }

    fn erase_cell(&self) -> Cell {
        Cell {
            character: ' ',
            foreground: self.current_foreground,
            background: self.current_background,
            attributes: self.current_attributes,
        }
    }

    fn print(&mut self, character: char) {
        if !character.is_ascii() || !(' '..='~').contains(&character) {
            return;
        }
        if self.active_buffer().pending_wrap && self.modes.auto_wrap {
            let buffer = self.active_buffer_mut();
            buffer.cursor.column = 0;
            buffer.pending_wrap = false;
            self.index();
        }

        let cell = Cell {
            character,
            foreground: self.current_foreground,
            background: self.current_background,
            attributes: self.current_attributes,
        };
        let columns = self.dimensions().columns();
        let auto_wrap = self.modes.auto_wrap;
        let buffer = self.active_buffer_mut();
        buffer
            .screen
            .replace_cell(buffer.cursor.column, buffer.cursor.row, cell);
        if buffer.cursor.column + 1 == columns {
            buffer.pending_wrap = auto_wrap;
        } else {
            buffer.cursor.column += 1;
        }
    }

    fn index(&mut self) {
        let fill = self.erase_cell();
        let dimensions = self.dimensions();
        let buffer = self.active_buffer_mut();
        if buffer.cursor.row == buffer.scroll_bottom {
            buffer
                .screen
                .scroll_up(buffer.scroll_top, buffer.scroll_bottom, 1, fill);
        } else if buffer.cursor.row + 1 < dimensions.rows() {
            buffer.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        let fill = self.erase_cell();
        let buffer = self.active_buffer_mut();
        if buffer.cursor.row == buffer.scroll_top {
            buffer
                .screen
                .scroll_down(buffer.scroll_top, buffer.scroll_bottom, 1, fill);
        } else {
            buffer.cursor.row = buffer.cursor.row.saturating_sub(1);
        }
    }

    fn tab(&mut self) {
        let columns = self.dimensions().columns();
        let buffer = self.active_buffer_mut();
        let next_tab_stop = ((buffer.cursor.column / 8) + 1) * 8;
        buffer.cursor.column = next_tab_stop.min(columns - 1);
    }

    fn clear_pending_wrap(&mut self) {
        self.active_buffer_mut().pending_wrap = false;
    }

    fn parameter_or(parameters: CsiParameters, index: usize, default: usize) -> usize {
        match parameters.value(index) {
            Some(0) | None => default,
            Some(value) => usize::from(value),
        }
    }

    fn raw_parameter(parameters: CsiParameters, index: usize, default: usize) -> usize {
        parameters.value(index).map_or(default, usize::from)
    }

    fn vertical_bounds(&self) -> (usize, usize) {
        if self.modes.origin_mode {
            let buffer = self.active_buffer();
            (buffer.scroll_top, buffer.scroll_bottom)
        } else {
            (0, self.dimensions().rows() - 1)
        }
    }

    fn move_vertical(&mut self, parameters: CsiParameters, down: bool) {
        let count = Self::parameter_or(parameters, 0, 1);
        let (top, bottom) = self.vertical_bounds();
        let buffer = self.active_buffer_mut();
        buffer.cursor.row = if down {
            buffer.cursor.row.saturating_add(count).min(bottom)
        } else {
            buffer.cursor.row.saturating_sub(count).max(top)
        };
        buffer.pending_wrap = false;
    }

    fn move_horizontal(&mut self, parameters: CsiParameters, forward: bool) {
        let count = Self::parameter_or(parameters, 0, 1);
        let columns = self.dimensions().columns();
        let buffer = self.active_buffer_mut();
        buffer.cursor.column = if forward {
            buffer.cursor.column.saturating_add(count).min(columns - 1)
        } else {
            buffer.cursor.column.saturating_sub(count)
        };
        buffer.pending_wrap = false;
    }

    fn cursor_horizontal_absolute(&mut self, parameters: CsiParameters) {
        let column = Self::parameter_or(parameters, 0, 1) - 1;
        let columns = self.dimensions().columns();
        let buffer = self.active_buffer_mut();
        buffer.cursor.column = column.min(columns - 1);
        buffer.pending_wrap = false;
    }

    fn cursor_position(&mut self, parameters: CsiParameters) {
        let requested_row = Self::parameter_or(parameters, 0, 1) - 1;
        let requested_column = Self::parameter_or(parameters, 1, 1) - 1;
        let columns = self.dimensions().columns();
        let (top, bottom) = self.vertical_bounds();
        let row = if self.modes.origin_mode {
            top.saturating_add(requested_row).min(bottom)
        } else {
            requested_row.min(bottom)
        };
        let buffer = self.active_buffer_mut();
        buffer.cursor.row = row;
        buffer.cursor.column = requested_column.min(columns - 1);
        buffer.pending_wrap = false;
    }

    fn vertical_position_absolute(&mut self, parameters: CsiParameters) {
        let requested_row = Self::parameter_or(parameters, 0, 1) - 1;
        let (top, bottom) = self.vertical_bounds();
        let row = if self.modes.origin_mode {
            top.saturating_add(requested_row).min(bottom)
        } else {
            requested_row.min(bottom)
        };
        let buffer = self.active_buffer_mut();
        buffer.cursor.row = row;
        buffer.pending_wrap = false;
    }

    fn erase_display(&mut self, parameters: CsiParameters) {
        let mode = Self::raw_parameter(parameters, 0, 0);
        let columns = self.dimensions().columns();
        let rows = self.dimensions().rows();
        let cell = self.erase_cell();
        let cursor = self.cursor();
        let screen = &mut self.active_buffer_mut().screen;
        match mode {
            0 => screen.fill_linear(cursor.row * columns + cursor.column, columns * rows, cell),
            1 => screen.fill_linear(0, cursor.row * columns + cursor.column + 1, cell),
            2 => screen.clear_all(cell),
            _ => {}
        }
    }

    fn erase_line(&mut self, parameters: CsiParameters) {
        let mode = Self::raw_parameter(parameters, 0, 0);
        let columns = self.dimensions().columns();
        let cell = self.erase_cell();
        let cursor = self.cursor();
        let start = cursor.row * columns;
        let screen = &mut self.active_buffer_mut().screen;
        match mode {
            0 => screen.fill_linear(start + cursor.column, start + columns, cell),
            1 => screen.fill_linear(start, start + cursor.column + 1, cell),
            2 => screen.fill_linear(start, start + columns, cell),
            _ => {}
        }
    }

    fn erase_characters(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let columns = self.dimensions().columns();
        let cell = self.erase_cell();
        let cursor = self.cursor();
        let start = cursor.row * columns + cursor.column;
        let end = start.saturating_add(count).min((cursor.row + 1) * columns);
        self.active_buffer_mut()
            .screen
            .fill_linear(start, end, cell);
    }

    fn insert_characters(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let cell = self.erase_cell();
        let cursor = self.cursor();
        self.active_buffer_mut()
            .screen
            .insert_characters(cursor.column, cursor.row, count, cell);
    }

    fn delete_characters(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let cell = self.erase_cell();
        let cursor = self.cursor();
        self.active_buffer_mut()
            .screen
            .delete_characters(cursor.column, cursor.row, count, cell);
    }

    fn insert_lines(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let cell = self.erase_cell();
        let buffer = self.active_buffer_mut();
        if (buffer.scroll_top..=buffer.scroll_bottom).contains(&buffer.cursor.row) {
            buffer
                .screen
                .insert_lines(buffer.cursor.row, buffer.scroll_bottom, count, cell);
        }
    }

    fn delete_lines(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let cell = self.erase_cell();
        let buffer = self.active_buffer_mut();
        if (buffer.scroll_top..=buffer.scroll_bottom).contains(&buffer.cursor.row) {
            buffer
                .screen
                .delete_lines(buffer.cursor.row, buffer.scroll_bottom, count, cell);
        }
    }

    fn scroll_up(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let cell = self.erase_cell();
        let buffer = self.active_buffer_mut();
        buffer
            .screen
            .scroll_up(buffer.scroll_top, buffer.scroll_bottom, count, cell);
    }

    fn scroll_down(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let cell = self.erase_cell();
        let buffer = self.active_buffer_mut();
        buffer
            .screen
            .scroll_down(buffer.scroll_top, buffer.scroll_bottom, count, cell);
    }

    fn set_scroll_region(&mut self, parameters: CsiParameters) {
        let rows = self.dimensions().rows();
        let top = Self::parameter_or(parameters, 0, 1) - 1;
        let bottom = Self::parameter_or(parameters, 1, rows) - 1;
        if top >= bottom || bottom >= rows {
            return;
        }
        let origin_mode = self.modes.origin_mode;
        let buffer = self.active_buffer_mut();
        buffer.scroll_top = top;
        buffer.scroll_bottom = bottom;
        buffer.cursor.column = 0;
        buffer.cursor.row = if origin_mode { top } else { 0 };
        buffer.pending_wrap = false;
    }

    fn save_dec(&mut self) {
        let attributes = self.current_attributes;
        let foreground = self.current_foreground;
        let background = self.current_background;
        let origin_mode = self.modes.origin_mode;
        let auto_wrap = self.modes.auto_wrap;
        let buffer = self.active_buffer_mut();
        buffer.dec_saved = Some(SavedDecState {
            cursor: buffer.cursor,
            pending_wrap: buffer.pending_wrap,
            attributes,
            foreground,
            background,
            origin_mode,
            auto_wrap,
        });
    }

    fn restore_dec(&mut self) {
        let Some(saved) = self.active_buffer().dec_saved else {
            return;
        };
        let dimensions = self.dimensions();
        let buffer = self.active_buffer_mut();
        buffer.cursor = Cursor {
            column: saved.cursor.column.min(dimensions.columns() - 1),
            row: saved.cursor.row.min(dimensions.rows() - 1),
        };
        if saved.origin_mode {
            buffer.cursor.row = buffer
                .cursor
                .row
                .clamp(buffer.scroll_top, buffer.scroll_bottom);
        }
        buffer.pending_wrap =
            saved.pending_wrap && buffer.cursor.column + 1 == dimensions.columns();
        self.current_attributes = saved.attributes;
        self.current_foreground = saved.foreground;
        self.current_background = saved.background;
        self.modes.origin_mode = saved.origin_mode;
        self.modes.auto_wrap = saved.auto_wrap;
    }

    fn save_ansi(&mut self) {
        let cursor = self.cursor();
        self.active_buffer_mut().ansi_saved = Some(SavedAnsiCursor { cursor });
    }

    fn restore_ansi(&mut self) {
        let Some(saved) = self.active_buffer().ansi_saved else {
            return;
        };
        let dimensions = self.dimensions();
        let buffer = self.active_buffer_mut();
        buffer.cursor = Cursor {
            column: saved.cursor.column.min(dimensions.columns() - 1),
            row: saved.cursor.row.min(dimensions.rows() - 1),
        };
        buffer.pending_wrap = false;
    }

    fn set_graphics_rendition(&mut self, parameters: CsiParameters) {
        if parameters.is_empty() {
            self.reset_graphics_rendition();
            return;
        }

        let mut index = 0;
        while index < parameters.len() {
            let Some(code) = parameters.value(index) else {
                break;
            };
            match code {
                0 => self.reset_graphics_rendition(),
                1 => self.current_attributes = self.current_attributes.with(Attributes::BOLD),
                2 => self.current_attributes = self.current_attributes.with(Attributes::FAINT),
                3 => self.current_attributes = self.current_attributes.with(Attributes::ITALIC),
                4 => {
                    self.current_attributes = self
                        .current_attributes
                        .without(Attributes::DOUBLE_UNDERLINE)
                        .with(Attributes::UNDERLINE)
                }
                5 => self.current_attributes = self.current_attributes.with(Attributes::SLOW_BLINK),
                6 => {
                    self.current_attributes = self.current_attributes.with(Attributes::RAPID_BLINK)
                }
                7 => self.current_attributes = self.current_attributes.with(Attributes::INVERSE),
                8 => self.current_attributes = self.current_attributes.with(Attributes::CONCEALED),
                9 => {
                    self.current_attributes =
                        self.current_attributes.with(Attributes::STRIKETHROUGH)
                }
                21 => {
                    self.current_attributes = self
                        .current_attributes
                        .without(Attributes::UNDERLINE)
                        .with(Attributes::DOUBLE_UNDERLINE)
                }
                22 => {
                    self.current_attributes = self
                        .current_attributes
                        .without(Attributes::BOLD)
                        .without(Attributes::FAINT)
                }
                23 => self.current_attributes = self.current_attributes.without(Attributes::ITALIC),
                24 => {
                    self.current_attributes = self
                        .current_attributes
                        .without(Attributes::UNDERLINE)
                        .without(Attributes::DOUBLE_UNDERLINE)
                }
                25 => {
                    self.current_attributes = self
                        .current_attributes
                        .without(Attributes::SLOW_BLINK)
                        .without(Attributes::RAPID_BLINK)
                }
                27 => {
                    self.current_attributes = self.current_attributes.without(Attributes::INVERSE)
                }
                28 => {
                    self.current_attributes = self.current_attributes.without(Attributes::CONCEALED)
                }
                29 => {
                    self.current_attributes =
                        self.current_attributes.without(Attributes::STRIKETHROUGH)
                }
                30..=37 => self.current_foreground = Color::Indexed((code - 30) as u8),
                39 => self.current_foreground = Color::Default,
                40..=47 => self.current_background = Color::Indexed((code - 40) as u8),
                49 => self.current_background = Color::Default,
                90..=97 => self.current_foreground = Color::Indexed((code - 90 + 8) as u8),
                100..=107 => self.current_background = Color::Indexed((code - 100 + 8) as u8),
                38 | 48 => {
                    let foreground = code == 38;
                    index = self.set_extended_color(parameters, index, foreground);
                }
                _ => {}
            }
            index += 1;
        }
    }

    fn set_extended_color(
        &mut self,
        parameters: CsiParameters,
        index: usize,
        foreground: bool,
    ) -> usize {
        let Some(mode) = parameters.value(index + 1) else {
            return index;
        };
        let separator = parameters.separator(index + 1);
        let color = match (separator, mode) {
            (Some(ParameterSeparator::Semicolon), 5)
                if parameters.separator(index + 2) == Some(ParameterSeparator::Semicolon) =>
            {
                parameters
                    .value(index + 2)
                    .and_then(|value| u8::try_from(value).ok())
                    .map(Color::Indexed)
            }
            (Some(ParameterSeparator::Semicolon), 2)
                if parameters.separator(index + 2) == Some(ParameterSeparator::Semicolon)
                    && parameters.separator(index + 3) == Some(ParameterSeparator::Semicolon)
                    && parameters.separator(index + 4) == Some(ParameterSeparator::Semicolon) =>
            {
                match (
                    parameters.value(index + 2),
                    parameters.value(index + 3),
                    parameters.value(index + 4),
                ) {
                    (Some(red), Some(green), Some(blue)) => {
                        match (u8::try_from(red), u8::try_from(green), u8::try_from(blue)) {
                            (Ok(red), Ok(green), Ok(blue)) => Some(Color::Rgb { red, green, blue }),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            // Canonical colon true-color syntax reserves an empty color-space
            // subparameter: `38:2::red:green:blue`.
            (Some(ParameterSeparator::Colon), 5)
                if parameters.separator(index + 2) == Some(ParameterSeparator::Colon) =>
            {
                parameters
                    .value(index + 2)
                    .and_then(|value| u8::try_from(value).ok())
                    .map(Color::Indexed)
            }
            (Some(ParameterSeparator::Colon), 2)
                if parameters.separator(index + 2) == Some(ParameterSeparator::Colon)
                    && parameters.separator(index + 3) == Some(ParameterSeparator::Colon)
                    && parameters.separator(index + 4) == Some(ParameterSeparator::Colon)
                    && parameters.separator(index + 5) == Some(ParameterSeparator::Colon) =>
            {
                match (
                    parameters.value(index + 3),
                    parameters.value(index + 4),
                    parameters.value(index + 5),
                ) {
                    (Some(red), Some(green), Some(blue)) => {
                        match (u8::try_from(red), u8::try_from(green), u8::try_from(blue)) {
                            (Ok(red), Ok(green), Ok(blue)) => Some(Color::Rgb { red, green, blue }),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        };

        if let Some(color) = color {
            if foreground {
                self.current_foreground = color;
            } else {
                self.current_background = color;
            }
        }
        match (separator, mode) {
            (Some(ParameterSeparator::Semicolon), 5) => index + 2,
            (Some(ParameterSeparator::Semicolon), 2) => index + 4,
            (Some(ParameterSeparator::Colon), 5) => index + 2,
            (Some(ParameterSeparator::Colon), 2) => index + 5,
            _ => index,
        }
    }

    fn reset_graphics_rendition(&mut self) {
        self.current_attributes = Attributes::NONE;
        self.current_foreground = Color::Default;
        self.current_background = Color::Default;
    }

    fn set_modes(&mut self, private: bool, enabled: bool, parameters: CsiParameters) {
        if !private {
            return;
        }
        for index in 0..parameters.len() {
            let Some(mode) = parameters.value(index) else {
                continue;
            };
            match mode {
                6 => {
                    self.modes.origin_mode = enabled;
                    self.home_cursor();
                }
                7 => {
                    self.modes.auto_wrap = enabled;
                    if !enabled {
                        self.clear_pending_wrap();
                    }
                }
                25 => self.modes.cursor_visible = enabled,
                47 => {
                    if enabled {
                        self.enter_alternate(false);
                    } else {
                        self.leave_alternate(false);
                    }
                }
                1047 => {
                    if enabled {
                        self.enter_alternate(true);
                    } else {
                        self.leave_alternate(true);
                    }
                }
                1048 => {
                    if enabled {
                        self.save_dec();
                    } else {
                        self.restore_dec();
                    }
                }
                1049 => {
                    if enabled {
                        self.save_dec();
                        self.enter_alternate(true);
                    } else {
                        self.leave_alternate(true);
                        self.restore_dec();
                    }
                }
                _ => {}
            }
        }
    }

    fn enter_alternate(&mut self, clear: bool) {
        if self.active_screen == ActiveScreen::Alternate {
            return;
        }
        if self.alternate.is_none() {
            let Ok(alternate) = BufferState::new(self.dimensions()) else {
                return;
            };
            self.alternate = Some(alternate);
        }
        if clear {
            let cell = self.erase_cell();
            let rows = self.dimensions().rows();
            let alternate = self
                .alternate
                .as_mut()
                .expect("alternate buffer was just allocated");
            alternate.screen.clear_all(cell);
            alternate.cursor = Cursor { column: 0, row: 0 };
            alternate.scroll_top = 0;
            alternate.scroll_bottom = rows - 1;
            alternate.pending_wrap = false;
        }
        self.active_screen = ActiveScreen::Alternate;
        self.modes.alternate_screen = true;
        self.active_buffer_mut().screen.mark_all_dirty();
    }

    fn leave_alternate(&mut self, reset: bool) {
        if self.active_screen == ActiveScreen::Alternate {
            self.active_screen = ActiveScreen::Primary;
            self.primary.screen.mark_all_dirty();
        }
        if reset {
            if let Some(alternate) = &mut self.alternate {
                alternate.reset();
            }
        }
        self.modes.alternate_screen = false;
    }

    fn home_cursor(&mut self) {
        let origin_mode = self.modes.origin_mode;
        let buffer = self.active_buffer_mut();
        buffer.cursor.column = 0;
        buffer.cursor.row = if origin_mode { buffer.scroll_top } else { 0 };
        buffer.pending_wrap = false;
    }

    fn device_status(&mut self, parameters: CsiParameters) {
        match parameters.value(0) {
            Some(5) => {
                self.queue_reply(b"\x1b[0n");
            }
            Some(6) => {
                let cursor = self.cursor();
                let row = if self.modes.origin_mode {
                    cursor
                        .row
                        .saturating_sub(self.active_buffer().scroll_top)
                        .saturating_add(1)
                } else {
                    cursor.row.saturating_add(1)
                };
                let reply = format!("\x1b[{row};{}R", cursor.column.saturating_add(1));
                self.queue_reply(reply.as_bytes());
            }
            _ => {}
        }
    }
}

fn queue_transport_bytes(queue: &mut Vec<u8>, bytes: &[u8]) -> QueuePushResult {
    let Some(remaining) = TRANSPORT_QUEUE_HIGH_WATERMARK.checked_sub(queue.len()) else {
        return QueuePushResult {
            accepted: 0,
            overflowed: true,
        };
    };
    if bytes.len() > remaining || queue.try_reserve_exact(bytes.len()).is_err() {
        return QueuePushResult {
            accepted: 0,
            overflowed: true,
        };
    }
    queue.extend_from_slice(bytes);
    QueuePushResult {
        accepted: bytes.len(),
        overflowed: false,
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
    use super::{
        Attributes, Color, Dimensions, Parser, QueuePushResult, Terminal, TerminalOp,
        MAX_CELL_COUNT, MAX_CSI_PARAMETERS, MAX_STRING_BYTES, TRANSPORT_QUEUE_HIGH_WATERMARK,
    };

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
    fn parser_emits_typed_operations_and_ignores_raw_c1() {
        let mut parser = Parser::new();
        assert_eq!(parser.advance(b'A'), TerminalOp::Print('A'));
        assert_eq!(parser.advance(b'\r'), TerminalOp::CarriageReturn);
        assert_eq!(parser.advance(0x9b), TerminalOp::Ignored);
        assert_eq!(parser.advance(0x1b), TerminalOp::Ignored);
        assert_eq!(parser.advance(b'['), TerminalOp::Ignored);
        assert_eq!(parser.advance(b'2'), TerminalOp::Ignored);
        assert_eq!(
            parser.advance(b'J'),
            TerminalOp::EraseDisplay({
                let mut parameters = super::CsiParameters::default();
                assert!(parameters.append_digit(2));
                parameters
            })
        );
    }

    #[test]
    fn malformed_and_over_limit_sequences_recover_without_printing_payload() {
        let mut terminal = terminal(8, 1);
        let oversized_parameters = format!("\x1b[{}mOK", "1;".repeat(MAX_CSI_PARAMETERS + 1));
        terminal.ingest(oversized_parameters.as_bytes());
        terminal.ingest(b"\x1b[123456mX");
        terminal.ingest(b"\x1b[!\"#AB");
        terminal.ingest(&[0x1b, b']']);
        terminal.ingest(&vec![b'x'; MAX_STRING_BYTES]);
        terminal.ingest(b"Y");

        assert_eq!(terminal.row_text(0).as_deref(), Some("OKXBY   "));
        assert_eq!(terminal.attributes(), Attributes::NONE);
    }

    #[test]
    fn preserves_right_margin_until_the_next_printable_byte() {
        let mut terminal = terminal(3, 2);
        terminal.ingest(b"abcdef");
        assert_eq!(terminal.row_text(0).as_deref(), Some("abc"));
        assert_eq!(terminal.row_text(1).as_deref(), Some("def"));
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (2, 1)
        );

        terminal.ingest(b"\x1b[31mg");
        assert_eq!(terminal.row_text(0).as_deref(), Some("def"));
        assert_eq!(terminal.row_text(1).as_deref(), Some("g  "));
        assert_eq!(terminal.cell(0, 1).unwrap().foreground(), Color::Indexed(1));
    }

    #[test]
    fn applies_movement_erasure_and_editing_within_the_active_region() {
        let mut terminal = terminal(5, 4);
        terminal.ingest(b"11111\r\n22222\r\n33333\r\n44444");
        terminal.ingest(b"\x1b[2;4r\x1b[2;1H\x1b[2L\x1b[1M");
        terminal.ingest(b"\x1b[2;2H\x1b[2@A\x1b[1P\x1b[2X");

        assert_eq!(terminal.row_text(0).as_deref(), Some("11111"));
        assert_eq!(terminal.row_text(1).as_deref(), Some(" A   "));
        assert_eq!(terminal.row_text(2).as_deref(), Some("22222"));
        assert_eq!(terminal.row_text(3).as_deref(), Some("     "));
    }

    #[test]
    fn origin_mode_and_scroll_region_bound_cursor_addressing_and_scrolling() {
        let mut terminal = terminal(4, 4);
        terminal.ingest(b"top \r\none \r\ntwo \r\nbot ");
        terminal.ingest(b"\x1b[2;3r\x1b[?6h\x1b[1;1HABCDZWXYQ");
        assert_eq!(terminal.row_text(0).as_deref(), Some("top "));
        assert_eq!(terminal.row_text(1).as_deref(), Some("ZWXY"));
        assert_eq!(terminal.row_text(2).as_deref(), Some("Q   "));
        assert_eq!(terminal.row_text(3).as_deref(), Some("bot "));
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (1, 2)
        );
    }

    #[test]
    fn saves_and_restores_dec_and_ansi_state() {
        let mut terminal = terminal(6, 2);
        terminal.ingest(b"\x1b[31;1mAB\x1b7\x1b[2;6H\x1b[0mZ\x1b8C");
        assert_eq!(terminal.cell(2, 0).unwrap().foreground(), Color::Indexed(1));
        assert!(terminal
            .cell(2, 0)
            .unwrap()
            .attributes()
            .contains(Attributes::BOLD));
        terminal.ingest(b"\x1b[s\x1b[2;1H\x1b[uD");
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (4, 0)
        );
    }

    #[test]
    fn alternate_modes_preserve_primary_and_1049_restores_saved_cursor() {
        let mut terminal = terminal(5, 2);
        terminal.ingest(b"main\x1b[?47halt\x1b[?47l");
        assert_eq!(terminal.row_text(0).as_deref(), Some("main "));
        terminal.ingest(b"\x1b[?1049halt\x1b[?1049lX");
        assert_eq!(terminal.row_text(0).as_deref(), Some("mainX"));
        assert!(!terminal.modes().alternate_screen());
    }

    #[test]
    fn dec_restore_clamps_an_origin_cursor_to_current_margins_before_cpr() {
        let mut terminal = terminal(5, 4);
        terminal.ingest(b"\x1b[2;3r\x1b[?6h\x1b7\x1b[?6l\x1b[3;4r\x1b8\x1b[6n");

        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (0, 2)
        );
        assert!(terminal.modes().origin_mode());
        assert_eq!(terminal.drain_replies(), b"\x1b[1;1R");
    }

    #[test]
    fn dec_and_ansi_saved_cursors_are_scoped_to_the_active_screen() {
        let mut terminal = terminal(5, 2);
        terminal.ingest(
            b"\x1b[1;2H\x1b7\x1b[s\x1b[?47h\
              \x1b[2;3H\x1b7\x1b[s\x1b[1;1H\x1b8A\
              \x1b[?47l\x1b8P",
        );

        assert_eq!(
            terminal.primary_screen().row_text(0).as_deref(),
            Some(" P   ")
        );
        assert_eq!(
            terminal.alternate_screen().unwrap().row_text(1).as_deref(),
            Some("  A  ")
        );
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (2, 0)
        );

        terminal.ingest(b"\x1b[?47h\x1b8");
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (2, 1)
        );
        terminal.ingest(b"\x1b[u");
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (2, 1)
        );
    }

    #[test]
    fn applies_standard_indexed_and_true_color_sgr() {
        let mut terminal = terminal(5, 1);
        terminal.ingest(b"\x1b[1;4;31;48;5;200mA\x1b[38;2;1;2;3;49mB\x1b[0mC");
        let first = terminal.cell(0, 0).unwrap();
        assert_eq!(first.foreground(), Color::Indexed(1));
        assert_eq!(first.background(), Color::Indexed(200));
        assert!(first.attributes().contains(Attributes::BOLD));
        assert!(first.attributes().contains(Attributes::UNDERLINE));
        assert_eq!(
            terminal.cell(1, 0).unwrap().foreground(),
            Color::Rgb {
                red: 1,
                green: 2,
                blue: 3
            }
        );
        assert_eq!(terminal.cell(2, 0).unwrap().foreground(), Color::Default);
    }

    #[test]
    fn reports_basic_device_status_without_advertising_identity() {
        let mut terminal = terminal(5, 3);
        terminal.ingest(b"\x1b[2;3H\x1b[5n\x1b[6n\x1b[c");
        assert_eq!(terminal.drain_replies(), b"\x1b[0n\x1b[2;3R");
    }

    #[test]
    fn resize_preserves_upper_left_cells_and_clamps_state() {
        let mut terminal = terminal(5, 3);
        terminal.ingest(b"abcdefghi\x1b[2;3r\x1b[?6h\x1b[3;5H");
        terminal.resize(Dimensions::new(3, 2).unwrap()).unwrap();
        assert_eq!(terminal.row_text(0).as_deref(), Some("abc"));
        assert_eq!(terminal.row_text(1).as_deref(), Some("fgh"));
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (2, 1)
        );
        assert_eq!(terminal.take_dirty_rows(), vec![0, 1]);
    }

    #[test]
    fn transport_queues_are_bounded_observable_and_preserve_accepted_order() {
        let mut terminal = terminal(2, 1);
        assert_eq!(
            terminal.queue_input(&[0x80, b'A']),
            QueuePushResult {
                accepted: 2,
                overflowed: false
            }
        );
        let input_fill = vec![b'i'; TRANSPORT_QUEUE_HIGH_WATERMARK - 2];
        assert_eq!(
            terminal.queue_input(&input_fill).accepted(),
            input_fill.len()
        );
        assert_eq!(
            terminal.queue_input(b"overflow"),
            QueuePushResult {
                accepted: 0,
                overflowed: true
            }
        );

        assert_eq!(&terminal.queued_input()[..2], &[0x80, b'A']);
        assert_eq!(
            terminal.queued_input().len(),
            TRANSPORT_QUEUE_HIGH_WATERMARK
        );
        assert!(terminal.take_input_queue_overflowed());
        assert!(!terminal.take_input_queue_overflowed());
        let drained_input = terminal.drain_input();
        assert_eq!(&drained_input[..2], &[0x80, b'A']);
        assert!(terminal.queued_input().is_empty());

        let reply_fill = vec![b'r'; TRANSPORT_QUEUE_HIGH_WATERMARK - 3];
        assert_eq!(
            terminal.queue_reply(&reply_fill).accepted(),
            reply_fill.len()
        );
        terminal.ingest(b"\x1b[5n");
        assert_eq!(terminal.queued_replies(), reply_fill);
        assert!(terminal.take_reply_queue_overflowed());
        assert!(!terminal.take_reply_queue_overflowed());
        assert_eq!(terminal.drain_replies(), reply_fill);
    }
}
