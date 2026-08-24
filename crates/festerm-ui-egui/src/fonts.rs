//! Bundled terminal font registration.
//!
//! Application chrome typography is a separate concern. Terminal cell
//! geometry is measured from the selected primary face; fallback and shaping
//! may paint only inside the cell spans allocated by the terminal core.

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

const INSTALLATION_MARKER: &str = "fesTerm bundled terminal font installation";
const GENERATION_MARKER: &str = "fesTerm bundled terminal font generation";

const REGULAR_DATA: &str = "fesTerm Selected Terminal Regular";
const BOLD_DATA: &str = "fesTerm Selected Terminal Bold";
const ITALIC_DATA: &str = "fesTerm Selected Terminal Italic";
const BOLD_ITALIC_DATA: &str = "fesTerm Selected Terminal Bold Italic";
const LIGATURE_REGULAR_DATA: &str = "fesTerm Selected Ligature Terminal Regular";
const LIGATURE_BOLD_DATA: &str = "fesTerm Selected Ligature Terminal Bold";
const LIGATURE_ITALIC_DATA: &str = "fesTerm Selected Ligature Terminal Italic";
const LIGATURE_BOLD_ITALIC_DATA: &str = "fesTerm Selected Ligature Terminal Bold Italic";

pub(crate) const REGULAR_FAMILY: &str = "fesTerm Terminal Regular";
pub(crate) const BOLD_FAMILY: &str = "fesTerm Terminal Bold";
pub(crate) const ITALIC_FAMILY: &str = "fesTerm Terminal Italic";
pub(crate) const BOLD_ITALIC_FAMILY: &str = "fesTerm Terminal Bold Italic";
pub(crate) const LIGATURE_REGULAR_FAMILY: &str = "fesTerm Ligature Terminal Regular";
pub(crate) const LIGATURE_BOLD_FAMILY: &str = "fesTerm Ligature Terminal Bold";
pub(crate) const LIGATURE_ITALIC_FAMILY: &str = "fesTerm Ligature Terminal Italic";
pub(crate) const LIGATURE_BOLD_ITALIC_FAMILY: &str = "fesTerm Ligature Terminal Bold Italic";

/// A bundled primary terminal family. These identifiers are independent of
/// platform font discovery and remain stable across operating systems.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum TerminalFontFamily {
    #[default]
    JetBrainsMono,
    IosevkaTerm,
    JuliaMono,
    MapleMono,
}

/// Monotonic identity for one egui font-atlas installation.
///
/// Cached galleys include this value so a family change can never reuse
/// layouts produced against a previous atlas, including an A -> B -> A
/// sequence.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct TerminalFontGeneration(u64);

/// The complete terminal text policy consumed by each terminal view.
///
/// Fallback ordering belongs to the installed family definitions. Ligatures
/// are kept separate from family choice so future OpenType controls do not
/// require changing persisted font identities.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct TerminalFontSet {
    family: TerminalFontFamily,
    ligatures: bool,
    generation: TerminalFontGeneration,
}

impl TerminalFontSet {
    pub const fn new(
        family: TerminalFontFamily,
        ligatures: bool,
        generation: TerminalFontGeneration,
    ) -> Self {
        Self {
            family,
            ligatures,
            generation,
        }
    }

    pub const fn family(self) -> TerminalFontFamily {
        self.family
    }

    pub const fn ligatures(self) -> bool {
        self.ligatures
    }

    pub const fn generation(self) -> TerminalFontGeneration {
        self.generation
    }
}

#[derive(Clone, Copy)]
struct FontAssets {
    regular: &'static [u8],
    bold: &'static [u8],
    italic: &'static [u8],
    bold_italic: &'static [u8],
    ligature_faces: Option<FontFaces>,
}

#[derive(Clone, Copy)]
struct FontFaces {
    regular: &'static [u8],
    bold: &'static [u8],
    italic: &'static [u8],
    bold_italic: &'static [u8],
}

fn font_assets(family: TerminalFontFamily) -> FontAssets {
    match family {
        TerminalFontFamily::JetBrainsMono => FontAssets {
            regular: include_bytes!(
                "../../../assets/fonts/jetbrains-mono/JetBrainsMonoNL-Regular.ttf"
            ),
            bold: include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMonoNL-Bold.ttf"),
            italic: include_bytes!(
                "../../../assets/fonts/jetbrains-mono/JetBrainsMonoNL-Italic.ttf"
            ),
            bold_italic: include_bytes!(
                "../../../assets/fonts/jetbrains-mono/JetBrainsMonoNL-BoldItalic.ttf"
            ),
            ligature_faces: Some(FontFaces {
                regular: include_bytes!(
                    "../../../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf"
                ),
                bold: include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Bold.ttf"),
                italic: include_bytes!(
                    "../../../assets/fonts/jetbrains-mono/JetBrainsMono-Italic.ttf"
                ),
                bold_italic: include_bytes!(
                    "../../../assets/fonts/jetbrains-mono/JetBrainsMono-BoldItalic.ttf"
                ),
            }),
        },
        TerminalFontFamily::IosevkaTerm => FontAssets {
            regular: include_bytes!(
                "../../../assets/fonts/iosevka-term/FesTermIosevka-Regular.ttf"
            ),
            bold: include_bytes!("../../../assets/fonts/iosevka-term/FesTermIosevka-Bold.ttf"),
            italic: include_bytes!("../../../assets/fonts/iosevka-term/FesTermIosevka-Italic.ttf"),
            bold_italic: include_bytes!(
                "../../../assets/fonts/iosevka-term/FesTermIosevka-BoldItalic.ttf"
            ),
            ligature_faces: None,
        },
        TerminalFontFamily::JuliaMono => FontAssets {
            regular: include_bytes!("../../../assets/fonts/julia-mono/JuliaMono-Regular.ttf"),
            bold: include_bytes!("../../../assets/fonts/julia-mono/JuliaMono-Bold.ttf"),
            italic: include_bytes!("../../../assets/fonts/julia-mono/JuliaMono-RegularItalic.ttf"),
            bold_italic: include_bytes!(
                "../../../assets/fonts/julia-mono/JuliaMono-BoldItalic.ttf"
            ),
            ligature_faces: None,
        },
        TerminalFontFamily::MapleMono => FontAssets {
            regular: include_bytes!("../../../assets/fonts/maple-mono/MapleMono-Regular.ttf"),
            bold: include_bytes!("../../../assets/fonts/maple-mono/MapleMono-Bold.ttf"),
            italic: include_bytes!("../../../assets/fonts/maple-mono/MapleMono-Italic.ttf"),
            bold_italic: include_bytes!(
                "../../../assets/fonts/maple-mono/MapleMono-BoldItalic.ttf"
            ),
            ligature_faces: None,
        },
    }
}

fn terminal_font_definitions(family: TerminalFontFamily) -> FontDefinitions {
    let mut definitions = FontDefinitions::default();
    let fallback = definitions
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .expect("egui provides a default monospace fallback family");
    let assets = font_assets(family);

    for (name, bytes) in [
        (REGULAR_DATA, assets.regular),
        (BOLD_DATA, assets.bold),
        (ITALIC_DATA, assets.italic),
        (BOLD_ITALIC_DATA, assets.bold_italic),
    ] {
        definitions
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    }

    if let Some(ligature_faces) = assets.ligature_faces {
        for (name, bytes) in [
            (LIGATURE_REGULAR_DATA, ligature_faces.regular),
            (LIGATURE_BOLD_DATA, ligature_faces.bold),
            (LIGATURE_ITALIC_DATA, ligature_faces.italic),
            (LIGATURE_BOLD_ITALIC_DATA, ligature_faces.bold_italic),
        ] {
            definitions
                .font_data
                .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
        }
    }

    for (font_family, primary) in [
        (REGULAR_FAMILY, REGULAR_DATA),
        (BOLD_FAMILY, BOLD_DATA),
        (ITALIC_FAMILY, ITALIC_DATA),
        (BOLD_ITALIC_FAMILY, BOLD_ITALIC_DATA),
    ] {
        let mut chain = Vec::with_capacity(fallback.len() + 1);
        chain.push(primary.to_owned());
        chain.extend(fallback.iter().cloned());
        definitions
            .families
            .insert(FontFamily::Name(font_family.into()), chain);
    }

    let ligature_names = if assets.ligature_faces.is_some() {
        [
            LIGATURE_REGULAR_DATA,
            LIGATURE_BOLD_DATA,
            LIGATURE_ITALIC_DATA,
            LIGATURE_BOLD_ITALIC_DATA,
        ]
    } else {
        [REGULAR_DATA, BOLD_DATA, ITALIC_DATA, BOLD_ITALIC_DATA]
    };
    for (font_family, primary) in [
        (LIGATURE_REGULAR_FAMILY, ligature_names[0]),
        (LIGATURE_BOLD_FAMILY, ligature_names[1]),
        (LIGATURE_ITALIC_FAMILY, ligature_names[2]),
        (LIGATURE_BOLD_ITALIC_FAMILY, ligature_names[3]),
    ] {
        let mut chain = Vec::with_capacity(fallback.len() + 1);
        chain.push(primary.to_owned());
        chain.extend(fallback.iter().cloned());
        definitions
            .families
            .insert(FontFamily::Name(font_family.into()), chain);
    }

    definitions
}

/// Installs one exact bundled primary family and its monospace fallbacks.
pub fn install_terminal_font_family(
    context: &egui::Context,
    family: TerminalFontFamily,
) -> TerminalFontGeneration {
    context.set_fonts(terminal_font_definitions(family));
    context.data_mut(|data| {
        let generation = data
            .get_temp::<u64>(egui::Id::new(GENERATION_MARKER))
            .unwrap_or(0)
            .saturating_add(1);
        data.insert_temp(egui::Id::new(GENERATION_MARKER), generation);
        data.insert_temp(egui::Id::new(INSTALLATION_MARKER), family);
        TerminalFontGeneration(generation)
    })
}

/// Installs the default family for direct UI-crate consumers.
pub fn install_terminal_fonts(context: &egui::Context) -> TerminalFontGeneration {
    install_terminal_font_family(context, TerminalFontFamily::default())
}

/// The terminal view's default font size in points.
pub const DEFAULT_TERMINAL_FONT_SIZE: f32 = 14.0;

/// Returns the regular terminal `FontId` at the given point size.
pub fn terminal_font(size_points: f32) -> egui::FontId {
    egui::FontId::new(size_points, FontFamily::Name(REGULAR_FAMILY.into()))
}

pub fn terminal_fonts_installed(context: &egui::Context) -> bool {
    context.data(|data| {
        data.get_temp::<TerminalFontFamily>(egui::Id::new(INSTALLATION_MARKER))
            .is_some()
    })
}

pub(crate) fn terminal_font_family_installed(
    context: &egui::Context,
    family: TerminalFontFamily,
) -> bool {
    context.data(|data| {
        data.get_temp::<TerminalFontFamily>(egui::Id::new(INSTALLATION_MARKER)) == Some(family)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_terminal_faces_lead_independent_fallback_chains() {
        for selected in [
            TerminalFontFamily::JetBrainsMono,
            TerminalFontFamily::IosevkaTerm,
            TerminalFontFamily::JuliaMono,
            TerminalFontFamily::MapleMono,
        ] {
            let definitions = terminal_font_definitions(selected);
            for (family, primary) in [
                (REGULAR_FAMILY, REGULAR_DATA),
                (BOLD_FAMILY, BOLD_DATA),
                (ITALIC_FAMILY, ITALIC_DATA),
                (BOLD_ITALIC_FAMILY, BOLD_ITALIC_DATA),
            ] {
                let chain = definitions
                    .families
                    .get(&FontFamily::Name(family.into()))
                    .expect("terminal face has a named family");
                assert_eq!(chain.first().map(String::as_str), Some(primary));
                assert!(definitions.font_data.contains_key(primary));
                assert!(chain.len() > 1, "missing glyphs must retain fallback");
            }
        }
    }

    #[test]
    fn installations_receive_monotonic_generations() {
        let context = egui::Context::default();
        let first = install_terminal_font_family(&context, TerminalFontFamily::JetBrainsMono);
        let second = install_terminal_font_family(&context, TerminalFontFamily::JuliaMono);

        assert_ne!(first, second);
        assert!(terminal_fonts_installed(&context));
    }
}
