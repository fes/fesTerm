use icu_properties::{props::GraphemeExtend, CodePointSetData};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Utf8Advance {
    Pending,
    Character(char),
    Invalid,
}

/// A deliberately small, strict UTF-8 decoder that retains at most four
/// bytes across [`crate::Terminal::ingest`] calls.
#[derive(Clone, Debug, Eq, PartialEq)]
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
}

pub(crate) fn is_combining_character(character: char) -> bool {
    character == '\u{200d}' || CodePointSetData::new::<GraphemeExtend>().contains(character)
}
