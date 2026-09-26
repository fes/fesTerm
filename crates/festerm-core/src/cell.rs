use std::sync::Arc;

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

const MAX_CELL_TEXT_CAPACITY_BYTES: usize = crate::unicode::MAX_GRAPHEME_BYTES * 2;

/// A color value used by a cell.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
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
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
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
    /// Not a rendition. The cell is protected from erasure, by DECSCA or by
    /// SPA/EPA. It lives here because it travels with the cell exactly as a
    /// rendition does, but `SGR 0` must not clear it.
    pub const PROTECTED: Self = Self { bits: 1 << 10 };

    pub const fn bits(self) -> u16 {
        self.bits
    }

    pub const fn from_bits(bits: u16) -> Self {
        Self { bits }
    }

    pub const fn contains(self, other: Self) -> bool {
        self.bits & other.bits == other.bits
    }

    pub(crate) const fn with(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }

    pub(crate) const fn without(self, other: Self) -> Self {
        Self {
            bits: self.bits & !other.bits,
        }
    }
}

/// The display-cell role occupied by a [`Cell`].
///
/// A double-width character owns a leading `Double` cell and the following
/// `Continuation` cell. Continuations never carry text.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CellWidth {
    Single,
    Double,
    Continuation,
}

impl CellWidth {
    pub const fn columns(self) -> usize {
        match self {
            Self::Single => 1,
            Self::Double => 2,
            Self::Continuation => 0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Cell {
    pub(crate) text: CompactString,
    pub(crate) text_capacity_bytes: usize,
    pub(crate) width: CellWidth,
    pub(crate) foreground: Color,
    pub(crate) background: Color,
    pub(crate) attributes: Attributes,
    pub(crate) hyperlink: Option<Arc<str>>,
}

impl Cell {
    /// Returns the leading character, or a space for a continuation.
    pub fn character(&self) -> char {
        self.text.chars().next().unwrap_or(' ')
    }

    /// Returns the leading character and any attached combining marks.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub const fn width(&self) -> CellWidth {
        self.width
    }

    pub const fn is_continuation(&self) -> bool {
        matches!(self.width, CellWidth::Continuation)
    }

    pub const fn foreground(&self) -> Color {
        self.foreground
    }

    pub const fn background(&self) -> Color {
        self.background
    }

    pub const fn attributes(&self) -> Attributes {
        self.attributes
    }

    /// Returns the trusted-for-display OSC 8 target attached to this cell.
    ///
    /// The core never opens links; the presentation layer must require an
    /// explicit user action before using this value.
    pub fn hyperlink(&self) -> Option<&str> {
        self.hyperlink.as_deref()
    }

    /// Clones the shared OSC 8 target without duplicating its URI bytes.
    pub fn hyperlink_target(&self) -> Option<Arc<str>> {
        self.hyperlink.clone()
    }

    pub(crate) fn refresh_text_capacity_charge(&mut self) {
        self.text_capacity_bytes = self.text.capacity();
    }

    pub(crate) fn text_owned_charge(&self) -> usize {
        self.text_capacity_bytes
    }

    pub(crate) fn validate_recovery_state(&self) -> Result<(), String> {
        if self.text.len() > crate::unicode::MAX_GRAPHEME_BYTES {
            return Err(format!(
                "cell stores {} text bytes above the {}-byte grapheme limit",
                self.text.len(),
                crate::unicode::MAX_GRAPHEME_BYTES
            ));
        }
        if self.text_capacity_bytes < self.text.len() {
            return Err(format!(
                "cell records {} text capacity bytes for {} bytes of text",
                self.text_capacity_bytes,
                self.text.len()
            ));
        }
        if self.text_capacity_bytes > MAX_CELL_TEXT_CAPACITY_BYTES {
            return Err(format!(
                "cell records {} text capacity bytes above the {}-byte recovery limit",
                self.text_capacity_bytes, MAX_CELL_TEXT_CAPACITY_BYTES
            ));
        }
        Ok(())
    }

    pub(crate) fn restore_recovery_allocation(&mut self) {
        if self.text_capacity_bytes > self.text.capacity() {
            self.text
                .reserve(self.text_capacity_bytes.saturating_sub(self.text.len()));
        }
        if self.text.capacity() > self.text_capacity_bytes {
            self.text.shrink_to(self.text_capacity_bytes);
        }
    }
}

pub(crate) fn blank_cell() -> Cell {
    let text = CompactString::const_new(" ");
    let text_capacity_bytes = text.capacity();
    Cell {
        text,
        text_capacity_bytes,
        width: CellWidth::Single,
        foreground: Color::Default,
        background: Color::Default,
        attributes: Attributes::NONE,
        hyperlink: None,
    }
}
