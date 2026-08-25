use crate::modes::{MouseTrackingMode, TerminalModes};

/// A named non-text terminal key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Character(char),
    /// A Ctrl-chord over `character`, encoded as the C0/C1 control byte
    /// xterm and other terminal emulators send for it (e.g. Ctrl+B sends
    /// `0x02`). `character` is the base key as if Ctrl were not held (its
    /// case does not matter: Ctrl+B and Ctrl+Shift+B send the same byte).
    /// A UI only emits this for a `character` this crate can actually map;
    /// see [`control_byte`].
    Control(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowRight,
    ArrowLeft,
    Keypad(KeypadKey),
}

/// A keypad key whose encoding depends on DECKPAM/DECKPNM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeypadKey {
    Digit(u8),
    Decimal,
    Enter,
    Add,
    Subtract,
    Multiply,
    Divide,
    Equal,
    Separator,
}

/// A focus change reported by `DECSET ?1004`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusEvent {
    In,
    Out,
}

/// Mouse buttons recognized by xterm-style mouse reporting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// Mouse-wheel direction recognized by xterm-style mouse reporting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MouseWheel {
    Up,
    Down,
}

/// Pointer activity received at the UI boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MouseEventKind {
    Press(MouseButton),
    Release(MouseButton),
    /// `button` is the held button, if any, while the pointer moved.
    Move {
        button: Option<MouseButton>,
    },
    Wheel(MouseWheel),
}

/// Keyboard modifiers encoded in xterm mouse reports.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Modifiers {
    bits: u8,
}

impl Modifiers {
    pub const NONE: Self = Self { bits: 0 };
    pub const SHIFT: Self = Self { bits: 1 << 0 };
    pub const ALT: Self = Self { bits: 1 << 1 };
    pub const CONTROL: Self = Self { bits: 1 << 2 };

    pub const fn contains(self, other: Self) -> bool {
        self.bits & other.bits == other.bits
    }

    pub const fn with(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }

    const fn mouse_bits(self) -> u8 {
        (if self.contains(Self::SHIFT) { 4 } else { 0 })
            | (if self.contains(Self::ALT) { 8 } else { 0 })
            | (if self.contains(Self::CONTROL) { 16 } else { 0 })
    }
}

/// A mouse event in zero-based terminal-cell coordinates.
///
/// `column` and `row` name the cell under the pointer. SGR reports add one to
/// both values because its wire format is one based.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MouseEvent {
    pub kind: MouseEventKind,
    pub column: usize,
    pub row: usize,
    pub modifiers: Modifiers,
}

/// Typed UI input accepted by the terminal core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputEvent {
    Key(Key),
    Paste(String),
    Focus(FocusEvent),
    Mouse(MouseEvent),
}

/// Observable result of handling a typed input event.
///
/// A UI may start local selection only after `SelectionAllowed`. An enabled
/// terminal mouse mode claims every mouse event, including one that its
/// tracking level does not report, so `SelectionClaimed` means no local
/// selection even though no bytes were queued.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputEventOutcome {
    Encoded { bytes: usize },
    SelectionAllowed,
    SelectionClaimed,
    QueueOverflow,
    Rejected,
}

pub(crate) fn encode_key(key: Key, modes: TerminalModes) -> Option<Vec<u8>> {
    let bytes = match key {
        Key::Character(character) => character.to_string().into_bytes(),
        Key::Control(character) => vec![control_byte(character)?],
        Key::Enter => b"\r".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Backspace => vec![0x7f],
        Key::Escape => vec![0x1b],
        Key::ArrowUp => cursor_key_bytes(b'A', modes.application_cursor()),
        Key::ArrowDown => cursor_key_bytes(b'B', modes.application_cursor()),
        Key::ArrowRight => cursor_key_bytes(b'C', modes.application_cursor()),
        Key::ArrowLeft => cursor_key_bytes(b'D', modes.application_cursor()),
        Key::Keypad(key) => keypad_key_bytes(key, modes.application_keypad())?,
    };
    Some(bytes)
}

/// Maps a Ctrl-chord's base `character` to the C0/C1 control byte xterm
/// sends for it: for a letter, its position in the alphabet (`Ctrl+A` is
/// `0x01` through `Ctrl+Z` is `0x1a`); a few punctuation keys conventionally
/// paired with Ctrl for the remaining low control codes; and space for NUL.
/// Returns `None` for any `character` without an established control-byte
/// mapping, which callers treat as "send nothing" rather than guessing.
pub(crate) fn control_byte(character: char) -> Option<u8> {
    match character {
        'a'..='z' => Some(character.to_ascii_uppercase() as u8 - b'A' + 1),
        'A'..='Z' => Some(character as u8 - b'A' + 1),
        ' ' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        _ => None,
    }
}

fn cursor_key_bytes(final_byte: u8, application: bool) -> Vec<u8> {
    if application {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

fn keypad_key_bytes(key: KeypadKey, application: bool) -> Option<Vec<u8>> {
    let normal = match key {
        KeypadKey::Digit(digit @ 0..=9) => vec![b'0' + digit],
        KeypadKey::Digit(_) => return None,
        KeypadKey::Decimal => b".".to_vec(),
        KeypadKey::Enter => b"\r".to_vec(),
        KeypadKey::Add => b"+".to_vec(),
        KeypadKey::Subtract => b"-".to_vec(),
        KeypadKey::Multiply => b"*".to_vec(),
        KeypadKey::Divide => b"/".to_vec(),
        KeypadKey::Equal => b"=".to_vec(),
        KeypadKey::Separator => b",".to_vec(),
    };
    if !application {
        return Some(normal);
    }

    let final_byte = match key {
        KeypadKey::Digit(0) => b'p',
        KeypadKey::Digit(1) => b'q',
        KeypadKey::Digit(2) => b'r',
        KeypadKey::Digit(3) => b's',
        KeypadKey::Digit(4) => b't',
        KeypadKey::Digit(5) => b'u',
        KeypadKey::Digit(6) => b'v',
        KeypadKey::Digit(7) => b'w',
        KeypadKey::Digit(8) => b'x',
        KeypadKey::Digit(9) => b'y',
        KeypadKey::Digit(_) => return None,
        KeypadKey::Decimal => b'n',
        KeypadKey::Enter => b'M',
        KeypadKey::Add => b'k',
        KeypadKey::Subtract => b'm',
        KeypadKey::Multiply => b'j',
        KeypadKey::Divide => b'o',
        KeypadKey::Equal => b'X',
        KeypadKey::Separator => b'l',
    };
    Some(vec![0x1b, b'O', final_byte])
}

pub(crate) fn encode_paste(text: String, modes: TerminalModes) -> Option<Vec<u8>> {
    if !modes.bracketed_paste() {
        return Some(text.into_bytes());
    }

    const START: &[u8] = b"\x1b[200~";
    const END: &[u8] = b"\x1b[201~";
    let capacity = paste_encoded_length(&text, modes)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(capacity).ok()?;
    bytes.extend_from_slice(START);
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(END);
    Some(bytes)
}

pub(crate) fn paste_encoded_length(text: &str, modes: TerminalModes) -> Option<usize> {
    if modes.bracketed_paste() {
        text.len()
            .checked_add(b"\x1b[200~".len())?
            .checked_add(b"\x1b[201~".len())
    } else {
        Some(text.len())
    }
}

pub(crate) fn mouse_event_is_reported(kind: MouseEventKind, tracking: MouseTrackingMode) -> bool {
    match kind {
        MouseEventKind::Press(_) | MouseEventKind::Wheel(_) => true,
        MouseEventKind::Release(_) => tracking != MouseTrackingMode::X10,
        MouseEventKind::Move {
            button: Some(_), ..
        } => matches!(
            tracking,
            MouseTrackingMode::ButtonMotion | MouseTrackingMode::AnyMotion
        ),
        MouseEventKind::Move { button: None, .. } => tracking == MouseTrackingMode::AnyMotion,
    }
}

fn mouse_button_code(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

fn mouse_code(event: MouseEvent, sgr: bool) -> (u8, u8) {
    let modifiers = event.modifiers.mouse_bits();
    match event.kind {
        MouseEventKind::Press(button) => (mouse_button_code(button) | modifiers, b'M'),
        MouseEventKind::Release(button) => (
            (if sgr { mouse_button_code(button) } else { 3 }) | modifiers,
            if sgr { b'm' } else { b'M' },
        ),
        MouseEventKind::Move { button } => {
            (button.map_or(3, mouse_button_code) | modifiers | 32, b'M')
        }
        MouseEventKind::Wheel(MouseWheel::Up) => (64 | modifiers, b'M'),
        MouseEventKind::Wheel(MouseWheel::Down) => (65 | modifiers, b'M'),
    }
}

pub(crate) fn encode_sgr_mouse(event: MouseEvent) -> Option<Vec<u8>> {
    let column = event.column.checked_add(1)?;
    let row = event.row.checked_add(1)?;
    let (code, final_byte) = mouse_code(event, true);
    Some(format!("\x1b[<{code};{column};{row}{}", final_byte as char).into_bytes())
}

pub(crate) fn encode_legacy_mouse(event: MouseEvent) -> Option<Vec<u8>> {
    const MAX_LEGACY_COORDINATE: usize = 222;
    if event.column > MAX_LEGACY_COORDINATE || event.row > MAX_LEGACY_COORDINATE {
        return None;
    }
    let (code, _) = mouse_code(event, false);
    Some(vec![
        0x1b,
        b'[',
        b'M',
        code + 32,
        event.column as u8 + 33,
        event.row as u8 + 33,
    ])
}
