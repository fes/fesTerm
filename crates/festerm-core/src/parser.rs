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
            b']' => self.start_string(StringKind::Osc),
            b'P' | b'X' | b'^' | b'_' => self.start_string(StringKind::Other),
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
