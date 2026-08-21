//! Bundled terminal font registration.
//!
//! Application chrome typography is a separate concern. These named families
//! are requested only by the terminal renderer, preserving an independent
//! fallback chain and per-session size.

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

const INSTALLATION_MARKER: &str = "fesTerm bundled terminal fonts installed";

const REGULAR_DATA: &str = "JetBrains Mono NL Regular";
const BOLD_DATA: &str = "JetBrains Mono NL Bold";
const ITALIC_DATA: &str = "JetBrains Mono NL Italic";
const BOLD_ITALIC_DATA: &str = "JetBrains Mono NL Bold Italic";

pub(crate) const REGULAR_FAMILY: &str = "fesTerm Terminal Regular";
pub(crate) const BOLD_FAMILY: &str = "fesTerm Terminal Bold";
pub(crate) const ITALIC_FAMILY: &str = "fesTerm Terminal Italic";
pub(crate) const BOLD_ITALIC_FAMILY: &str = "fesTerm Terminal Bold Italic";

fn terminal_font_definitions() -> FontDefinitions {
    let mut definitions = FontDefinitions::default();
    let fallback = definitions
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .expect("egui provides a default monospace fallback family");

    for (name, bytes) in [
        (
            REGULAR_DATA,
            include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMonoNL-Regular.ttf")
                .as_slice(),
        ),
        (
            BOLD_DATA,
            include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMonoNL-Bold.ttf")
                .as_slice(),
        ),
        (
            ITALIC_DATA,
            include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMonoNL-Italic.ttf")
                .as_slice(),
        ),
        (
            BOLD_ITALIC_DATA,
            include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMonoNL-BoldItalic.ttf")
                .as_slice(),
        ),
    ] {
        definitions
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    }

    for (family, primary) in [
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
            .insert(FontFamily::Name(family.into()), chain);
    }

    definitions
}

/// Installs the exact bundled terminal family and its monospace fallbacks.
pub fn install_terminal_fonts(context: &egui::Context) {
    context.set_fonts(terminal_font_definitions());
    context.data_mut(|data| {
        data.insert_temp(egui::Id::new(INSTALLATION_MARKER), true);
    });
}

/// The terminal view's default font size in points, shared with chrome that
/// must visually match the terminal at its un-zoomed size (e.g. pty-styled
/// prompts rendered before a session has a live terminal view yet).
pub const DEFAULT_TERMINAL_FONT_SIZE: f32 = 14.0;

/// Returns the `FontId` for the bundled regular terminal face at the given
/// point size, for chrome (e.g. pty-styled prompts) that must visually match
/// the real terminal view rather than the application's UI font.
pub fn terminal_font(size_points: f32) -> egui::FontId {
    egui::FontId::new(size_points, FontFamily::Name(REGULAR_FAMILY.into()))
}

pub fn terminal_fonts_installed(context: &egui::Context) -> bool {
    context.data(|data| {
        data.get_temp::<bool>(egui::Id::new(INSTALLATION_MARKER))
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_terminal_faces_lead_independent_fallback_chains() {
        let definitions = terminal_font_definitions();
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
