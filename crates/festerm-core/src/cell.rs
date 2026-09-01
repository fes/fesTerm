use std::sync::Arc;

use compact_str::CompactString;

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cell {
    pub(crate) text: CompactString,
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
}

pub(crate) fn blank_cell() -> Cell {
    Cell {
        text: CompactString::const_new(" "),
        width: CellWidth::Single,
        foreground: Color::Default,
        background: Color::Default,
        attributes: Attributes::NONE,
        hyperlink: None,
    }
}
