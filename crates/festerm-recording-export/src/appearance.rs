use festerm_recording::{RecordingFont, RecordingTheme};
use festerm_ui_egui::{
    RecordingFrameSize, RecordingPalette, RecordingRenderer, TerminalFontFamily,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ExportError;

#[derive(Deserialize)]
struct Catalog {
    pinned_version: String,
    color_release: Option<String>,
    files: Vec<CatalogFile>,
}

#[derive(Deserialize)]
struct CatalogFile {
    path: String,
    sha256: String,
}

#[derive(Serialize)]
struct AppearanceIdentity {
    version: u8,
    renderer_version: &'static str,
    font: RecordingFont,
    font_version: String,
    /// Regular, bold, italic and bold-italic face hashes, in that order.
    font_faces_sha256: [String; 4],
    emoji_font_version: String,
    color_emoji_release: String,
    emoji_faces_sha256: [String; 2],
    theme: RecordingTheme,
    font_size_points: f32,
    width: usize,
    height: usize,
    recovered_prefix: bool,
}

pub(crate) struct PreparedAppearance {
    font: RecordingFont,
    theme: RecordingTheme,
    version: String,
    hashes: [String; 4],
    faces: Option<[Vec<u8>; 4]>,
}

impl PreparedAppearance {
    pub(crate) fn new(
        font: RecordingFont,
        theme: RecordingTheme,
        faces: Option<[Vec<u8>; 4]>,
    ) -> Result<Self, ExportError> {
        let json = match font {
            RecordingFont::JetBrainsMono | RecordingFont::JetBrainsMonoLigatures => {
                include_str!("../../../assets/fonts/jetbrains-mono/manifest.json")
            }
            RecordingFont::IosevkaTerm => {
                include_str!("../../../assets/fonts/iosevka-term/manifest.json")
            }
            RecordingFont::JuliaMono => {
                include_str!("../../../assets/fonts/julia-mono/manifest.json")
            }
            RecordingFont::MapleMono => {
                include_str!("../../../assets/fonts/maple-mono/manifest.json")
            }
        };
        let catalog: Catalog = serde_json::from_str(json).map_err(|error| {
            ExportError::new(format!("invalid recording font catalog: {error}"))
        })?;
        let hashes: [String; 4] = catalog
            .files
            .into_iter()
            .filter(|file| match font {
                RecordingFont::JetBrainsMono => {
                    file.path.contains("/JetBrainsMonoNL-") && file.path.ends_with(".ttf")
                }
                RecordingFont::JetBrainsMonoLigatures => {
                    file.path.contains("/JetBrainsMono-") && file.path.ends_with(".ttf")
                }
                _ => file.path.ends_with(".ttf"),
            })
            .map(|file| file.sha256)
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| ExportError::new("recording font catalog does not have four faces"))?;
        if catalog.pinned_version.is_empty()
            || hashes
                .iter()
                .any(|hash| hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(ExportError::new("recording font catalog is invalid"));
        }
        match (&faces, font.bundled()) {
            (None, true) => {}
            (Some(faces), false)
                if faces.iter().zip(&hashes).all(|(bytes, expected)| {
                    bytes.len() <= 32 * 1024 * 1024
                        && format!("{:x}", Sha256::digest(bytes)) == *expected
                }) => {}
            _ => {
                return Err(ExportError::new(format!(
                    "the selected recording font {} is unavailable or failed verification; choose another font",
                    font.label()
                )))
            }
        }
        Ok(Self {
            font,
            theme,
            version: catalog.pinned_version,
            hashes,
            faces,
        })
    }

    pub(crate) fn into_renderer(
        self,
        size: RecordingFrameSize,
        font_size: f32,
        recovered_prefix: bool,
    ) -> Result<(RecordingRenderer, Vec<u8>), ExportError> {
        let (family, ligatures) = match self.font {
            RecordingFont::JetBrainsMono => (TerminalFontFamily::JetBrainsMono, false),
            RecordingFont::JetBrainsMonoLigatures => (TerminalFontFamily::JetBrainsMono, true),
            RecordingFont::IosevkaTerm => (TerminalFontFamily::IosevkaTerm, false),
            RecordingFont::JuliaMono => (TerminalFontFamily::JuliaMono, false),
            RecordingFont::MapleMono => (TerminalFontFamily::MapleMono, false),
        };
        let palette = match self.theme {
            RecordingTheme::FesTermDark => RecordingPalette::Dark,
            RecordingTheme::FesTermLight => RecordingPalette::Light,
        };
        let renderer = RecordingRenderer::with_appearance(
            size, font_size, family, ligatures, self.faces, palette,
        )
        .map_err(|error| {
            ExportError::new(format!("could not create recording renderer: {error}"))
        })?;
        let emoji: Catalog = serde_json::from_str(include_str!(
            "../../../assets/fonts/noto-emoji/manifest.json"
        ))
        .map_err(|error| ExportError::new(format!("invalid recording emoji catalog: {error}")))?;
        let emoji_hashes: [String; 2] = emoji
            .files
            .into_iter()
            .filter(|file| file.path.ends_with(".ttf"))
            .map(|file| file.sha256)
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| ExportError::new("recording emoji catalog does not have two faces"))?;
        if emoji.pinned_version.is_empty()
            || emoji_hashes
                .iter()
                .any(|hash| hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(ExportError::new("recording emoji catalog is invalid"));
        }
        let metadata = AppearanceIdentity {
            version: 1,
            renderer_version: env!("CARGO_PKG_VERSION"),
            font: self.font,
            font_version: self.version,
            font_faces_sha256: self.hashes,
            emoji_font_version: emoji.pinned_version,
            color_emoji_release: emoji
                .color_release
                .filter(|version| !version.is_empty())
                .ok_or_else(|| ExportError::new("recording color emoji version is unavailable"))?,
            emoji_faces_sha256: emoji_hashes,
            theme: self.theme,
            font_size_points: font_size,
            width: size.width(),
            height: size.height(),
            recovered_prefix,
        };
        let bytes = serde_json::to_vec_pretty(&metadata).map_err(|error| {
            ExportError::new(format!("could not describe recording appearance: {error}"))
        })?;
        Ok((renderer, bytes))
    }
}
