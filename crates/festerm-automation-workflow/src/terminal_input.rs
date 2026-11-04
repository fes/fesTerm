//! Inert, bounded encoding of explicit worker terminal-input requests.
//!
//! Prepared bytes are not authority, target binding, a lease, or a transport
//! reservation. At admission the application must recheck the captured modes,
//! target generation, broker grant, deadline, and all input/privacy fences.

use festerm_automation::MAX_INPUT_BYTES;
use festerm_core::{
    InputEvent, InputPreparationError, Key, KeypadKey, Modifiers, PreparedInput, Terminal,
    MAX_PREPARED_INPUT_EVENTS,
};

const MAX_KEY_TOKEN_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// The token does not match the declared key grammar.
    InvalidKey,
    /// The core refused encoding, including unsupported chords or locked keys.
    Input(InputPreparationError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidKey => formatter.write_str("invalid terminal key"),
            Self::Input(reason) => write!(formatter, "terminal input refused: {reason:?}"),
        }
    }
}

impl std::error::Error for Error {}

/// Prepare the exact text with the terminal's existing paste semantics.
/// No submit key is added; supplied newlines or control characters can still
/// cause execution and therefore require the same control grant as any input.
pub fn prepare_send_text(terminal: &Terminal, text: &str) -> Result<PreparedInput, Error> {
    if text.is_empty() {
        return Err(Error::Input(InputPreparationError::Empty));
    }
    if text.len() > MAX_INPUT_BYTES as usize {
        return Err(Error::Input(InputPreparationError::TooLarge));
    }
    let mut owned = String::new();
    owned
        .try_reserve_exact(text.len())
        .map_err(|_| Error::Input(InputPreparationError::Allocation))?;
    owned.push_str(text);
    terminal
        .prepare_input(&[InputEvent::Paste(owned)], MAX_INPUT_BYTES as usize)
        .map_err(Error::Input)
}

/// Prepare a complete key batch without changing the terminal input queue.
///
/// Each token has zero or more distinct lowercase `ctrl:`, `alt:`, or
/// `shift:` modifiers, followed by one exact key name. A single ASCII letter,
/// digit, `.`, `_`, or `-` is a character. Other keys use the names documented
/// in the automation design. Character values are already Shift-translated,
/// as required by the terminal core; the modifier does not change their case.
pub fn prepare_send_keys(terminal: &Terminal, keys: &[String]) -> Result<PreparedInput, Error> {
    if keys.is_empty() {
        return Err(Error::Input(InputPreparationError::Empty));
    }
    if keys.len() > MAX_PREPARED_INPUT_EVENTS {
        return Err(Error::Input(InputPreparationError::TooManyEvents));
    }
    let mut events = Vec::new();
    events
        .try_reserve_exact(keys.len())
        .map_err(|_| Error::Input(InputPreparationError::Allocation))?;
    for token in keys {
        events.push(parse_key(token)?);
    }
    terminal
        .prepare_input(&events, MAX_INPUT_BYTES as usize)
        .map_err(Error::Input)
}

fn parse_key(token: &str) -> Result<InputEvent, Error> {
    if token.is_empty() || token.len() > MAX_KEY_TOKEN_BYTES || !token.is_ascii() {
        return Err(Error::InvalidKey);
    }
    let mut modifiers = Modifiers::NONE;
    let mut parts = token.split(':').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            let key = named_key(part).ok_or(Error::InvalidKey)?;
            return Ok(InputEvent::ModifiedKey { key, modifiers });
        }
        let modifier = match part {
            "ctrl" => Modifiers::CONTROL,
            "alt" => Modifiers::ALT,
            "shift" => Modifiers::SHIFT,
            _ => return Err(Error::InvalidKey),
        };
        if modifiers.contains(modifier) {
            return Err(Error::InvalidKey);
        }
        modifiers = modifiers.with(modifier);
    }
    Err(Error::InvalidKey)
}

fn named_key(name: &str) -> Option<Key> {
    let key = match name {
        "Enter" => Key::Enter,
        "Tab" => Key::Tab,
        "Backspace" => Key::Backspace,
        "Escape" => Key::Escape,
        "ArrowUp" => Key::ArrowUp,
        "ArrowDown" => Key::ArrowDown,
        "ArrowRight" => Key::ArrowRight,
        "ArrowLeft" => Key::ArrowLeft,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "Home" => Key::Home,
        "End" => Key::End,
        "Insert" => Key::Insert,
        "Delete" => Key::Delete,
        "Space" => Key::Character(' '),
        "Plus" => Key::Character('+'),
        "Equals" => Key::Character('='),
        "Slash" => Key::Character('/'),
        "Backslash" => Key::Character('\\'),
        "BracketLeft" => Key::Character('['),
        "BracketRight" => Key::Character(']'),
        "Comma" => Key::Character(','),
        "Semicolon" => Key::Character(';'),
        "Quote" => Key::Character('\''),
        "Backtick" => Key::Character('`'),
        "F1" => Key::Function(1),
        "F2" => Key::Function(2),
        "F3" => Key::Function(3),
        "F4" => Key::Function(4),
        "F5" => Key::Function(5),
        "F6" => Key::Function(6),
        "F7" => Key::Function(7),
        "F8" => Key::Function(8),
        "F9" => Key::Function(9),
        "F10" => Key::Function(10),
        "F11" => Key::Function(11),
        "F12" => Key::Function(12),
        _ => {
            if let Some(keypad) = name.strip_prefix("Keypad") {
                let key = match keypad {
                    "0" => KeypadKey::Digit(0),
                    "1" => KeypadKey::Digit(1),
                    "2" => KeypadKey::Digit(2),
                    "3" => KeypadKey::Digit(3),
                    "4" => KeypadKey::Digit(4),
                    "5" => KeypadKey::Digit(5),
                    "6" => KeypadKey::Digit(6),
                    "7" => KeypadKey::Digit(7),
                    "8" => KeypadKey::Digit(8),
                    "9" => KeypadKey::Digit(9),
                    "Decimal" => KeypadKey::Decimal,
                    "Enter" => KeypadKey::Enter,
                    "Add" => KeypadKey::Add,
                    "Subtract" => KeypadKey::Subtract,
                    "Multiply" => KeypadKey::Multiply,
                    "Divide" => KeypadKey::Divide,
                    "Equal" => KeypadKey::Equal,
                    "Separator" => KeypadKey::Separator,
                    _ => return None,
                };
                Key::Keypad(key)
            } else {
                let [byte] = name.as_bytes() else {
                    return None;
                };
                if !byte.is_ascii_alphanumeric() && !matches!(byte, b'.' | b'_' | b'-') {
                    return None;
                }
                Key::Character(char::from(*byte))
            }
        }
    };
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_automation_js::protocol::{HostRequest, Target};
    use festerm_core::Dimensions;

    fn terminal() -> Terminal {
        Terminal::new(Dimensions::new(20, 4).unwrap()).unwrap()
    }

    fn keys(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn terminal_requests_prepare_exact_mode_aware_bytes_without_input() {
        let mut terminal = terminal();
        terminal.ingest(b"\x1b[?1h\x1b=");
        let keys = keys(&["ctrl:c", "alt:ArrowLeft", "shift:Tab", "F5", "Keypad1"]);
        assert!(HostRequest::SendKeys {
            target: Target {
                id: "target".to_owned(),
                generation: 1,
            },
            keys: keys.clone(),
        }
        .validate()
        .is_ok());
        let prepared = prepare_send_keys(&terminal, &keys).unwrap();
        assert_eq!(prepared.bytes(), b"\x03\x1b[1;3D\x1b[Z\x1b[15~\x1bOq");
        assert_eq!(prepared.modes(), terminal.modes());
        assert!(terminal.drain_input().is_empty());

        assert_eq!(
            prepare_send_text(&terminal, "hello\r\nworld\n")
                .unwrap()
                .bytes(),
            b"hello\rworld\r"
        );
        terminal.ingest(b"\x1b[?2004h");
        let pasted = prepare_send_text(&terminal, "hello\n").unwrap();
        assert_eq!(pasted.bytes(), b"\x1b[200~hello\n\x1b[201~");
        assert_eq!(pasted.modes(), terminal.modes());
        terminal.ingest(b"\x1b[?2004l");
        assert_ne!(pasted.modes(), terminal.modes());
        assert!(terminal.drain_input().is_empty());
    }

    #[test]
    fn invalid_or_unsupported_keys_refuse_the_whole_batch() {
        let mut terminal = terminal();
        for invalid in [
            "ctrl:ctrl:c",
            "Ctrl:c",
            "ctrl::c",
            "ctrl:c:alt",
            "F01",
            "F13",
            "Keypad10",
            "ctrl:unknown",
            "ctrl:+",
            "é",
        ] {
            assert_eq!(
                prepare_send_keys(&terminal, &keys(&["Enter", invalid])).err(),
                Some(Error::InvalidKey),
                "{invalid}"
            );
        }
        for unsupported in [
            "ctrl:Enter",
            "shift:Escape",
            "ctrl:1",
            "ctrl:.",
            "ctrl:_",
            "ctrl:-",
            "ctrl:Plus",
        ] {
            assert_eq!(
                prepare_send_keys(&terminal, &keys(&["Enter", unsupported])).err(),
                Some(Error::Input(InputPreparationError::Rejected)),
                "{unsupported}"
            );
        }
        terminal.ingest(b"\x1b[2h");
        assert_eq!(
            prepare_send_keys(&terminal, &keys(&["Enter"])).err(),
            Some(Error::Input(InputPreparationError::Rejected))
        );
        assert!(terminal.drain_input().is_empty());
    }

    #[test]
    fn terminal_request_limits_and_character_semantics_are_explicit() {
        let mut terminal = terminal();
        assert_eq!(
            prepare_send_keys(&terminal, &[]).err(),
            Some(Error::Input(InputPreparationError::Empty))
        );
        assert_eq!(
            prepare_send_keys(&terminal, &vec!["Enter".to_owned(); 65]).err(),
            Some(Error::Input(InputPreparationError::TooManyEvents))
        );
        assert_eq!(
            prepare_send_keys(&terminal, &keys(&[&"a".repeat(65)])).err(),
            Some(Error::InvalidKey)
        );
        assert_eq!(
            prepare_send_text(&terminal, &"x".repeat(MAX_INPUT_BYTES as usize + 1)).err(),
            Some(Error::Input(InputPreparationError::TooLarge))
        );
        assert_eq!(
            prepare_send_text(&terminal, "").err(),
            Some(Error::Input(InputPreparationError::Empty))
        );
        terminal.ingest(b"\x1b[?2004h");
        assert_eq!(
            prepare_send_text(&terminal, &"x".repeat(MAX_INPUT_BYTES as usize)).err(),
            Some(Error::Input(InputPreparationError::TooLarge))
        );
        assert_eq!(
            prepare_send_keys(
                &terminal,
                &keys(&["shift:A", "shift:a", "ctrl:BracketLeft"]),
            )
            .unwrap()
            .bytes(),
            b"Aa\x1b"
        );
    }
}
