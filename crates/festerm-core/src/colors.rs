//! Concrete colors the terminal can report to a program that asks.
//!
//! The core does not paint anything, but it is the only component allowed to
//! answer protocol requests, and `OSC 4/10/11/12` are requests *about* color.
//! Answering them truthfully means the core has to know which RGB values the
//! embedder actually puts on screen, so the embedder hands it a
//! [`ColorScheme`] and the core reports from that.
//!
//! Indices 16 through 255 are not part of any theme: the 6x6x6 cube and the
//! 24-step gray ramp are defined by the protocol itself, so they are computed
//! here rather than stored. `festerm-ui-egui` resolves through this same code
//! precisely so a reported color and a painted color cannot drift apart.

use crate::cell::Color;

/// A fully resolved color, with no palette indirection left in it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Rgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Rgb {
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    /// Formats the color as xterm's `rgb:` reply specification.
    ///
    /// Each component is reported at 16-bit precision, which the protocol
    /// expects; an 8-bit value is doubled into its 16-bit equivalent so that
    /// `0xff` reports as `ffff` rather than `00ff`, keeping white white.
    pub fn to_xparsecolor(self) -> String {
        let scale = |component: u8| u16::from(component) * 0x101;
        format!(
            "rgb:{:04x}/{:04x}/{:04x}",
            scale(self.red),
            scale(self.green),
            scale(self.blue)
        )
    }
}

/// The colors an embedder paints, in the form the core needs to report them.
///
/// The defaults mirror `festerm-ui-egui`'s terminal theme so that a core-only
/// test, a headless conformance shim, and the application all answer the same
/// way. The application still injects its own values through
/// [`crate::Terminal::set_color_scheme`], so a future theme change stays
/// correct without editing this file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ColorScheme {
    foreground: Rgb,
    background: Rgb,
    cursor: Rgb,
    ansi: [Rgb; 16],
}

impl ColorScheme {
    /// The 16 ANSI entries `festerm-ui-egui` paints.
    pub const DEFAULT_ANSI: [Rgb; 16] = [
        Rgb::new(0, 0, 0),
        Rgb::new(205, 49, 49),
        Rgb::new(13, 188, 121),
        Rgb::new(229, 229, 16),
        Rgb::new(36, 114, 200),
        Rgb::new(188, 63, 188),
        Rgb::new(17, 168, 205),
        Rgb::new(229, 229, 229),
        Rgb::new(102, 102, 102),
        Rgb::new(241, 76, 76),
        Rgb::new(35, 209, 139),
        Rgb::new(245, 245, 67),
        Rgb::new(59, 142, 234),
        Rgb::new(214, 112, 214),
        Rgb::new(41, 184, 219),
        Rgb::new(255, 255, 255),
    ];

    /// `festerm-ui-egui`'s `TEXT_PRIMARY`, its default terminal foreground.
    pub const DEFAULT_FOREGROUND: Rgb = Rgb::new(0xe8, 0xed, 0xf2);
    /// `festerm-ui-egui`'s `SURFACE_TERMINAL`, its default terminal surface.
    pub const DEFAULT_BACKGROUND: Rgb = Rgb::new(0x11, 0x16, 0x1e);

    pub const fn new(foreground: Rgb, background: Rgb, cursor: Rgb, ansi: [Rgb; 16]) -> Self {
        Self {
            foreground,
            background,
            cursor,
            ansi,
        }
    }

    pub const fn foreground(&self) -> Rgb {
        self.foreground
    }

    pub const fn background(&self) -> Rgb {
        self.background
    }

    pub const fn cursor(&self) -> Rgb {
        self.cursor
    }

    /// The RGB value of one palette entry.
    ///
    /// Entries 0 through 15 come from the scheme. Everything above is
    /// protocol-defined: 16 through 231 are a 6x6x6 cube over the levels
    /// xterm uses, and 232 through 255 are a 24-step gray ramp.
    pub fn palette(&self, index: u8) -> Rgb {
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
                let value = index - 16;
                Rgb::new(
                    LEVELS[(value / 36) as usize],
                    LEVELS[((value / 6) % 6) as usize],
                    LEVELS[(value % 6) as usize],
                )
            }
            _ => {
                let level = 8 + (index - 232) * 10;
                Rgb::new(level, level, level)
            }
        }
    }

    /// Resolves a cell color, using the scheme's default for [`Color::Default`].
    pub fn resolve(&self, color: Color, default: Rgb) -> Rgb {
        match color {
            Color::Default => default,
            Color::Indexed(index) => self.palette(index),
            Color::Rgb { red, green, blue } => Rgb::new(red, green, blue),
        }
    }
}

impl Default for ColorScheme {
    fn default() -> Self {
        Self::new(
            Self::DEFAULT_FOREGROUND,
            Self::DEFAULT_BACKGROUND,
            // The renderer draws the cursor in the default foreground color,
            // so that is the honest answer to `OSC 12`.
            Self::DEFAULT_FOREGROUND,
            Self::DEFAULT_ANSI,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eight_bit_components_scale_to_their_sixteen_bit_equivalents() {
        assert_eq!(Rgb::new(255, 0, 128).to_xparsecolor(), "rgb:ffff/0000/8080");
        assert_eq!(
            ColorScheme::DEFAULT_BACKGROUND.to_xparsecolor(),
            "rgb:1111/1616/1e1e"
        );
    }

    #[test]
    fn palette_covers_the_ansi_entries_the_cube_and_the_gray_ramp() {
        let scheme = ColorScheme::default();
        assert_eq!(scheme.palette(1), Rgb::new(205, 49, 49));
        // 196 is the pure red corner of the 6x6x6 cube.
        assert_eq!(scheme.palette(196), Rgb::new(255, 0, 0));
        assert_eq!(scheme.palette(232), Rgb::new(8, 8, 8));
        assert_eq!(scheme.palette(255), Rgb::new(238, 238, 238));
    }

    #[test]
    fn resolving_falls_back_to_the_supplied_default_only_for_default() {
        let scheme = ColorScheme::default();
        let fallback = Rgb::new(1, 2, 3);
        assert_eq!(scheme.resolve(Color::Default, fallback), fallback);
        assert_eq!(
            scheme.resolve(
                Color::Rgb {
                    red: 9,
                    green: 8,
                    blue: 7
                },
                fallback
            ),
            Rgb::new(9, 8, 7)
        );
        assert_eq!(scheme.resolve(Color::Indexed(2), fallback), scheme.ansi[2]);
    }
}
