use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(crate) const MAX_GRAPHEME_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum Utf8Advance {
    Pending,
    Character(char),
    Invalid,
}

/// A deliberately small, strict UTF-8 decoder that retains at most four
/// bytes across [`crate::Terminal::ingest`] calls.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Utf8Decoder {
    bytes: [u8; 4],
    length: usize,
    expected: usize,
}

impl Utf8Decoder {
    pub(crate) const fn new() -> Self {
        Self {
            bytes: [0; 4],
            length: 0,
            expected: 0,
        }
    }

    pub(crate) const fn pending(&self) -> bool {
        self.expected != 0
    }

    /// Starts a UTF-8 sequence. `false` means the byte cannot begin one.
    pub(crate) fn start(&mut self, byte: u8) -> bool {
        self.expected = match byte {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return false,
        };
        self.bytes[0] = byte;
        self.length = 1;
        true
    }

    pub(crate) fn advance(&mut self, byte: u8) -> Utf8Advance {
        debug_assert!(self.pending());
        let is_second_byte = self.length == 1;
        let valid_second_byte = match self.bytes[0] {
            0xe0 => (0xa0..=0xbf).contains(&byte),
            0xed => (0x80..=0x9f).contains(&byte),
            0xf0 => (0x90..=0xbf).contains(&byte),
            0xf4 => (0x80..=0x8f).contains(&byte),
            _ => (0x80..=0xbf).contains(&byte),
        };
        if !(0x80..=0xbf).contains(&byte) || (is_second_byte && !valid_second_byte) {
            self.reset();
            return Utf8Advance::Invalid;
        }

        self.bytes[self.length] = byte;
        self.length += 1;
        if self.length < self.expected {
            return Utf8Advance::Pending;
        }

        let character = std::str::from_utf8(&self.bytes[..self.expected])
            .ok()
            .and_then(|text| text.chars().next());
        self.reset();
        character.map_or(Utf8Advance::Invalid, Utf8Advance::Character)
    }

    fn reset(&mut self) {
        self.length = 0;
        self.expected = 0;
    }

    pub(crate) fn validate_recovery_state(&self) -> Result<(), String> {
        if self.expected > self.bytes.len() {
            return Err(format!(
                "utf-8 decoder expects {} bytes, exceeding the {}-byte maximum",
                self.expected,
                self.bytes.len()
            ));
        }
        if self.expected == 0 {
            if self.length != 0 {
                return Err(format!(
                    "utf-8 decoder stores {0} pending bytes without an active sequence",
                    self.length
                ));
            }
            return Ok(());
        }
        if self.length == 0 || self.length > self.expected {
            return Err(format!(
                "utf-8 decoder stores {} bytes for a {}-byte sequence",
                self.length, self.expected
            ));
        }
        let valid_lead = match self.expected {
            2 => matches!(self.bytes[0], 0xc2..=0xdf),
            3 => matches!(self.bytes[0], 0xe0..=0xef),
            4 => matches!(self.bytes[0], 0xf0..=0xf4),
            _ => false,
        };
        if !valid_lead {
            return Err("utf-8 decoder stores an invalid leading byte".to_owned());
        }
        let mut decoder = Self::new();
        if !decoder.start(self.bytes[0]) {
            return Err("utf-8 decoder could not restart its stored leading byte".to_owned());
        }
        if decoder.expected != self.expected {
            return Err(format!(
                "utf-8 decoder expects {} bytes but its leading byte implies {}",
                self.expected, decoder.expected
            ));
        }
        for byte in &self.bytes[1..self.length] {
            match decoder.advance(*byte) {
                Utf8Advance::Pending => {}
                Utf8Advance::Character(_) | Utf8Advance::Invalid => {
                    return Err(
                        "utf-8 decoder stores bytes that do not represent a pending sequence"
                            .to_owned(),
                    )
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn extends_grapheme(cluster: &str, character: char) -> bool {
    if character.is_ascii() {
        return false;
    }
    let mut candidate = String::with_capacity(cluster.len() + character.len_utf8());
    candidate.push_str(cluster);
    candidate.push(character);
    candidate.graphemes(true).count() == 1
}

pub(crate) fn grapheme_width(cluster: &str) -> usize {
    UnicodeWidthStr::width(cluster).min(2)
}
