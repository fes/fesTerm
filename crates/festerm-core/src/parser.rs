use std::sync::Arc;

use crate::{MAX_CSI_INTERMEDIATES, MAX_CSI_PARAMETERS, MAX_STRING_BYTES};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OscAction {
    SetTitle(String),
    SetHyperlink(Option<Arc<str>>),
}

fn parse_osc(payload: Vec<u8>) -> Option<OscAction> {
    let mut parts = payload.splitn(2, |byte| *byte == b';');
    let command = parts.next()?;
    let data = parts.next()?;
    match command {
        b"0" | b"2" => std::str::from_utf8(data)
            .ok()
            .map(sanitize_title)
            .map(OscAction::SetTitle),
        b"8" => parse_osc8(data).map(OscAction::SetHyperlink),
        _ => None,
    }
}

fn sanitize_title(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect()
}

fn parse_osc8(data: &[u8]) -> Option<Option<Arc<str>>> {
    let mut parts = data.splitn(2, |byte| *byte == b';');
    let _parameters = parts.next()?;
    let uri = parts.next()?;
    if uri.is_empty() {
        return Some(None);
    }
    let uri = std::str::from_utf8(uri).ok()?;
    if uri.len() > 2_048 || uri.chars().any(char::is_control) {
        return None;
    }
    let scheme = uri.split_once(':')?.0;
    matches!(scheme, "http" | "https" | "mailto").then(|| Some(Arc::from(uri)))
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

    pub(crate) fn append_separator(&mut self, separator: ParameterSeparator) -> bool {
        if self.length == 0 && !self.begin(ParameterSeparator::Start) {
            return false;
        }
        self.begin(separator)
    }

    pub(crate) fn append_digit(&mut self, digit: u8) -> bool {
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

/// A typed operation emitted by [`Parser`] and applied by [`crate::Terminal`].
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
    SetTabStop,
    SetApplicationKeypad(bool),
    SetCursorStyle(CsiParameters),
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
    DeviceAttributes {
        secondary: bool,
    },
    ClearTabStops(CsiParameters),
    Ignored,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StringKind {
    Osc,
    Other,
}

/// Which `G` graphic character set slot a `ESC (` / `ESC )` designation
/// targets (ISO 2022; VT100 only implements `G0`/`G1`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CharsetSlot {
    G0,
    G1,
}

/// A designatable graphic character set. `Other` covers every VT100
/// designation this parser does not special-case (e.g. UK `A`), which all
/// behave like `Ascii` for printable 7-bit bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
enum Charset {
    #[default]
    Ascii,
    DecSpecialGraphics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParserState {
    Ground,
    Escape,
    EscapeIntermediate,
    /// Immediately after `ESC (` or `ESC )`: the next byte designates the
    /// named slot's charset.
    CharsetDesignate(CharsetSlot),
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    String {
        kind: StringKind,
        bytes: usize,
    },
    StringEscape {
        kind: StringKind,
        bytes: usize,
    },
}

/// A bounded state-machine parser for ESC and CSI input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Parser {
    state: ParserState,
    parameters: CsiParameters,
    parameter_digits: u8,
    private: bool,
    secondary: bool,
    intermediates: [u8; MAX_CSI_INTERMEDIATES],
    intermediate_length: usize,
    string_payload: Vec<u8>,
    osc_action: Option<OscAction>,
    /// The charset designated into `G0` (via `ESC (`); active unless
    /// shifted out with `SO` (0x0E).
    g0_charset: Charset,
    /// The charset designated into `G1` (via `ESC )`); active only after
    /// `SO` (0x0E), until the next `SI` (0x0F).
    g1_charset: Charset,
    /// Whether `SO` has shifted the active charset from `G0` to `G1`.
    shifted_to_g1: bool,
}

/// Translates a 7-bit printable byte through the DEC Special Graphics
/// (VT100 line-drawing) charset, as designated by `ESC ( 0` / `ESC ) 0`.
/// Bytes outside `0x60..=0x7e` are not remapped by this charset and pass
/// through unchanged; this is the same mapping xterm and other emulators
/// use, which is what tmux (and ncurses' `smacs`/`rmacs`) draw its window
/// borders and status-bar dividers with.
const fn dec_special_graphics(byte: u8) -> char {
    match byte {
        0x60 => '\u{25c6}', // ◆
        0x61 => '\u{2592}', // ▒
        0x62 => '\u{2409}', // ␉ HT
        0x63 => '\u{240c}', // ␌ FF
        0x64 => '\u{240d}', // ␍ CR
        0x65 => '\u{240a}', // ␊ LF
        0x66 => '\u{00b0}', // °
        0x67 => '\u{00b1}', // ±
        0x68 => '\u{2424}', // ␤ NL
        0x69 => '\u{240b}', // ␋ VT
        0x6a => '\u{2518}', // ┘
        0x6b => '\u{2510}', // ┐
        0x6c => '\u{250c}', // ┌
        0x6d => '\u{2514}', // └
        0x6e => '\u{253c}', // ┼
        0x6f => '\u{23ba}', // ⎺ scan line 1
        0x70 => '\u{23bb}', // ⎻ scan line 3
        0x71 => '\u{2500}', // ─ scan line 5
        0x72 => '\u{23bc}', // ⎼ scan line 7
        0x73 => '\u{23bd}', // ⎽ scan line 9
        0x74 => '\u{251c}', // ├
        0x75 => '\u{2524}', // ┤
        0x76 => '\u{2534}', // ┴
        0x77 => '\u{252c}', // ┬
        0x78 => '\u{2502}', // │
        0x79 => '\u{2264}', // ≤
        0x7a => '\u{2265}', // ≥
        0x7b => '\u{03c0}', // π
        0x7c => '\u{2260}', // ≠
        0x7d => '\u{00a3}', // £
        0x7e => '\u{00b7}', // ·
        _ => byte as char,
    }
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
            secondary: false,
            intermediates: [0; MAX_CSI_INTERMEDIATES],
            intermediate_length: 0,
            string_payload: Vec::new(),
            osc_action: None,
            g0_charset: Charset::Ascii,
            g1_charset: Charset::Ascii,
            shifted_to_g1: false,
        }
    }

    pub(crate) fn is_ground(&self) -> bool {
        self.state == ParserState::Ground
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
        // SO (Shift Out) / SI (Shift In) switch the active charset between
        // the G1 and G0 slots most recently designated by `ESC )` / `ESC (`;
        // this is how tmux (and ncurses' smacs/rmacs) turn VT100 line
        // drawing on and off around window borders and dividers.
        match byte {
            0x0e => {
                self.shifted_to_g1 = true;
                return TerminalOp::Ignored;
            }
            0x0f => {
                self.shifted_to_g1 = false;
                return TerminalOp::Ignored;
            }
            _ => {}
        }
        if let Some(operation) = c0_operation(byte) {
            return operation;
        }

        match self.state {
            ParserState::Ground => match byte {
                b' '..=b'~' => TerminalOp::Print(self.translate_printable(byte)),
                _ => TerminalOp::Ignored,
            },
            ParserState::Escape => self.advance_escape(byte),
            ParserState::EscapeIntermediate => {
                self.state = ParserState::Ground;
                TerminalOp::Ignored
            }
            ParserState::CharsetDesignate(slot) => {
                self.state = ParserState::Ground;
                self.designate_charset(slot, byte);
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

    /// Maps a printable 7-bit byte through the currently shifted-in
    /// charset (`G1` after `SO`, otherwise `G0`).
    fn translate_printable(&self, byte: u8) -> char {
        let charset = if self.shifted_to_g1 {
            self.g1_charset
        } else {
            self.g0_charset
        };
        match charset {
            Charset::Ascii => byte as char,
            Charset::DecSpecialGraphics => dec_special_graphics(byte),
        }
    }

    /// Designates `byte` as the charset for `slot` (the byte following
    /// `ESC (` / `ESC )`). Only DEC Special Graphics (`0`) is distinguished
    /// from plain ASCII; every other VT100 designation (`A`, `B`, `1`, `2`,
    /// ...) prints its bytes unchanged, matching this parser's existing
    /// 7-bit-printable behavior.
    fn designate_charset(&mut self, slot: CharsetSlot, byte: u8) {
        let charset = match byte {
            b'0' => Charset::DecSpecialGraphics,
            _ => Charset::Ascii,
        };
        match slot {
            CharsetSlot::G0 => self.g0_charset = charset,
            CharsetSlot::G1 => self.g1_charset = charset,
        }
    }

    fn advance_escape(&mut self, byte: u8) -> TerminalOp {
        self.state = match byte {
            b'[' => {
                self.clear_csi();
                ParserState::CsiEntry
            }
            b']' => self.start_string(StringKind::Osc),
            b'P' | b'X' | b'^' | b'_' => self.start_string(StringKind::Other),
            b'(' => ParserState::CharsetDesignate(CharsetSlot::G0),
            b')' => ParserState::CharsetDesignate(CharsetSlot::G1),
            0x20..=0x2f => ParserState::EscapeIntermediate,
            _ => ParserState::Ground,
        };
        match byte {
            b'D' => TerminalOp::Index,
            b'E' => TerminalOp::NextLine,
            b'M' => TerminalOp::ReverseIndex,
            b'7' => TerminalOp::SaveDec,
            b'8' => TerminalOp::RestoreDec,
            b'H' => TerminalOp::SetTabStop,
            b'=' => TerminalOp::SetApplicationKeypad(true),
            b'>' => TerminalOp::SetApplicationKeypad(false),
            _ => TerminalOp::Ignored,
        }
    }

    fn advance_csi_entry(&mut self, byte: u8) -> TerminalOp {
        match byte {
            b'?' if !self.private && !self.secondary && self.parameters.is_empty() => {
                self.private = true;
                TerminalOp::Ignored
            }
            b'>' if !self.private && !self.secondary && self.parameters.is_empty() => {
                self.secondary = true;
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
                    return self.finish_string(kind);
                } else if byte == 0x1b {
                    self.state = ParserState::StringEscape { kind, bytes };
                } else {
                    self.advance_string_payload(kind, bytes, &[byte]);
                }
            }
            ParserState::StringEscape { kind, bytes } => {
                if byte == b'\\' {
                    self.state = ParserState::Ground;
                    return self.finish_string(kind);
                } else if byte == 0x1b {
                    self.advance_string_payload(kind, bytes, &[0x1b]);
                    if let ParserState::String { bytes, .. } = self.state {
                        self.state = ParserState::StringEscape { kind, bytes };
                    }
                } else {
                    self.advance_string_payload(kind, bytes, &[0x1b, byte]);
                }
            }
            _ => unreachable!("only string states call advance_string"),
        }
        TerminalOp::Ignored
    }

    fn start_string(&mut self, kind: StringKind) -> ParserState {
        self.string_payload.clear();
        ParserState::String { kind, bytes: 0 }
    }

    fn advance_string_payload(&mut self, kind: StringKind, bytes: usize, payload: &[u8]) {
        let Some(bytes) = bytes.checked_add(payload.len()) else {
            self.state = ParserState::Ground;
            return;
        };
        if kind == StringKind::Osc {
            self.string_payload.extend_from_slice(payload);
        }
        self.state = if bytes >= MAX_STRING_BYTES {
            ParserState::Ground
        } else {
            ParserState::String { kind, bytes }
        };
    }

    fn finish_string(&mut self, kind: StringKind) -> TerminalOp {
        if kind != StringKind::Osc {
            self.string_payload.clear();
            return TerminalOp::Ignored;
        }
        self.osc_action = parse_osc(std::mem::take(&mut self.string_payload));
        TerminalOp::Ignored
    }

    pub(crate) fn take_osc_action(&mut self) -> Option<OscAction> {
        self.osc_action.take()
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
        let secondary = self.secondary;
        let intermediates = self.intermediates;
        let intermediate_length = self.intermediate_length;
        self.state = ParserState::Ground;
        self.clear_csi();

        if intermediate_length != 0 {
            return match (private, intermediate_length, intermediates[0], final_byte) {
                (false, 1, b' ', b'q') if !parameters.has_colon() => {
                    TerminalOp::SetCursorStyle(parameters)
                }
                _ => TerminalOp::Ignored,
            };
        }
        if parameters.has_colon() && final_byte != b'm' {
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
        if secondary {
            return matches!(final_byte, b'c')
                .then_some(TerminalOp::DeviceAttributes { secondary: true })
                .unwrap_or(TerminalOp::Ignored);
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
            b'c' => TerminalOp::DeviceAttributes { secondary: false },
            b'g' => TerminalOp::ClearTabStops(parameters),
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
        self.secondary = false;
        self.intermediate_length = 0;
    }
}

pub(crate) fn c0_operation(byte: u8) -> Option<TerminalOp> {
    match byte {
        b'\r' => Some(TerminalOp::CarriageReturn),
        b'\n' => Some(TerminalOp::LineFeed),
        b'\x08' => Some(TerminalOp::Backspace),
        b'\t' => Some(TerminalOp::Tab),
        _ => None,
    }
}
