use std::sync::Arc;

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

/// Foreground opacity used to render the SGR faint attribute.
pub const FAINT_OPACITY: f32 = 0.6;

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
    // Runtime graphemes are bounded; schemas 2 and 3 still encode this as usize.
    #[serde(with = "legacy_text_capacity")]
    pub(crate) text_capacity_bytes: u16,
    pub(crate) width: CellWidth,
    pub(crate) foreground: Color,
    pub(crate) background: Color,
    pub(crate) attributes: Attributes,
    pub(crate) hyperlink: Option<Arc<str>>,
}

// Recovery schemas and scrollback accounting charge the original cell layout.
#[allow(dead_code)]
struct LegacyCellLayout {
    text: CompactString,
    text_capacity_bytes: usize,
    width: CellWidth,
    foreground: Color,
    background: Color,
    attributes: Attributes,
    hyperlink: Option<Arc<str>>,
}

pub(crate) const HISTORY_CELL_CHARGE: usize = std::mem::size_of::<LegacyCellLayout>();

#[cfg(target_pointer_width = "64")]
const _: () = assert!(
    HISTORY_CELL_CHARGE == 64,
    "64-bit history snapshots require the original 64-byte cell charge"
);
const _: () = assert!(
    std::mem::size_of::<Cell>() <= HISTORY_CELL_CHARGE,
    "cell storage must not exceed its historical history charge"
);
const _: () = assert!(
    MAX_CELL_TEXT_CAPACITY_BYTES <= u16::MAX as usize,
    "cell text capacity must fit in the compact field"
);

mod legacy_text_capacity {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S>(capacity: &u16, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        usize::from(*capacity).serialize(serializer)
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<u16, D::Error>
    where
        D: Deserializer<'de>,
    {
        let capacity = usize::deserialize(deserializer)?;
        u16::try_from(capacity).map_err(serde::de::Error::custom)
    }
}

impl Cell {
    pub(crate) fn text_capacity(text: &CompactString) -> u16 {
        u16::try_from(text.capacity()).expect("cell text capacity must fit in u16")
    }

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

    pub(crate) fn recording_text_is_redacted(&self, scheme: &crate::ColorScheme) -> bool {
        crate::screen::recording_text_is_hidden(
            self.foreground,
            self.background,
            self.attributes,
            scheme,
        )
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
        self.text_capacity_bytes = Self::text_capacity(&self.text);
    }

    pub(crate) fn text_owned_charge(&self) -> usize {
        usize::from(self.text_capacity_bytes)
    }

    pub(crate) fn validate_recovery_state(&self) -> Result<(), String> {
        if self.text.len() > crate::unicode::MAX_GRAPHEME_BYTES {
            return Err(format!(
                "cell stores {} text bytes above the {}-byte grapheme limit",
                self.text.len(),
                crate::unicode::MAX_GRAPHEME_BYTES
            ));
        }
        let text_capacity = usize::from(self.text_capacity_bytes);
        if text_capacity < self.text.len() {
            return Err(format!(
                "cell records {} text capacity bytes for {} bytes of text",
                self.text_capacity_bytes,
                self.text.len()
            ));
        }
        if text_capacity > MAX_CELL_TEXT_CAPACITY_BYTES {
            return Err(format!(
                "cell records {} text capacity bytes above the {}-byte recovery limit",
                self.text_capacity_bytes, MAX_CELL_TEXT_CAPACITY_BYTES
            ));
        }
        Ok(())
    }

    pub(crate) fn restore_recovery_allocation(&mut self) {
        let text_capacity = usize::from(self.text_capacity_bytes);
        if text_capacity > self.text.capacity() {
            self.text
                .reserve(text_capacity.saturating_sub(self.text.len()));
        }
        if self.text.capacity() > text_capacity {
            self.text.shrink_to(text_capacity);
        }
    }
}

pub(crate) fn blank_cell() -> Cell {
    let text = CompactString::const_new(" ");
    let text_capacity_bytes = Cell::text_capacity(&text);
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
