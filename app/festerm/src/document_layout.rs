use std::sync::Arc;

use eframe::egui::{self, text::LayoutJob, Color32, FontId};

/// Egui recreates its font collection on definition, text-option and atlas
/// resets. A tiny ordinary layout acts as a lease on that collection's cache:
/// retaining its Arc prevents address reuse, and eviction conservatively misses.
/// No font bytes, atlas image or hash-only identity is copied into our caches.
#[derive(Default)]
pub(crate) struct LayoutEpoch {
    context: Option<egui::Context>,
    viewport: Option<egui::ViewportId>,
    pixels_per_point: f32,
    dark_mode: bool,
    override_text_color: Option<Color32>,
    witness: Option<Arc<egui::Galley>>,
}

impl LayoutEpoch {
    pub(crate) fn refresh(&mut self, ui: &egui::Ui) -> bool {
        let job = LayoutJob::simple(
            "M".to_owned(),
            FontId::monospace(1.0),
            Color32::TRANSPARENT,
            f32::INFINITY,
        );
        let witness = ui.painter().layout_job(job);
        let viewport = ui.ctx().viewport_id();
        let current = self.context.as_ref() == Some(ui.ctx())
            && self.viewport == Some(viewport)
            && self.pixels_per_point == ui.ctx().pixels_per_point()
            && self.dark_mode == ui.visuals().dark_mode
            && self.override_text_color == ui.visuals().override_text_color
            && self
                .witness
                .as_ref()
                .is_some_and(|previous| Arc::ptr_eq(previous, &witness));
        self.context = Some(ui.ctx().clone());
        self.viewport = Some(viewport);
        self.pixels_per_point = ui.ctx().pixels_per_point();
        self.dark_mode = ui.visuals().dark_mode;
        self.override_text_color = ui.visuals().override_text_color;
        self.witness = Some(witness);
        current
    }
}

#[cfg(test)]
pub(crate) fn exhaust_font_atlas(context: &egui::Context) {
    context.global_style_mut(|style| style.visuals.text_options.max_texture_side = 512);
    let mut output = context.run_ui(Default::default(), |ui| {
        for size in 4..100 {
            let _ = ui.painter().layout_no_wrap(
                (32_u8..127).map(char::from).collect(),
                FontId::monospace(size as f32),
                Color32::WHITE,
            );
            if ui.fonts(|fonts| fonts.font_atlas_fill_ratio()) > 0.8 {
                return;
            }
        }
        panic!("the test must trigger egui's actual begin-pass atlas reset");
    });
    output.textures_delta.clear();
}

/// Account retained allocation capacities, not just input lengths. Shared rows
/// and jobs are deliberately charged again: this is a conservative payload
/// bound, excluding allocator/Arc overhead and ordinary egui/output ownership.
pub(crate) fn galley_payload_bytes(galley: &egui::Galley) -> Option<usize> {
    let mut bytes = std::mem::size_of::<egui::Galley>()
        .checked_add(std::mem::size_of::<LayoutJob>())?
        .checked_add(galley.job.text.capacity())?
        .checked_add(
            galley
                .job
                .sections
                .capacity()
                .checked_mul(std::mem::size_of::<egui::text::LayoutSection>())?,
        )?
        .checked_add(
            galley
                .rows
                .capacity()
                .checked_mul(std::mem::size_of::<egui::epaint::text::PlacedRow>())?,
        )?;
    for section in &galley.job.sections {
        if matches!(section.format.font_id.family, egui::FontFamily::Name(_)) {
            return None;
        }
    }
    for row in &galley.rows {
        bytes = bytes
            .checked_add(std::mem::size_of::<egui::epaint::text::Row>())?
            .checked_add(
                row.glyphs
                    .capacity()
                    .checked_mul(std::mem::size_of::<egui::epaint::text::Glyph>())?,
            )?
            .checked_add(
                row.visuals
                    .mesh
                    .vertices
                    .capacity()
                    .checked_mul(std::mem::size_of::<egui::epaint::Vertex>())?,
            )?
            .checked_add(
                row.visuals
                    .mesh
                    .indices
                    .capacity()
                    .checked_mul(std::mem::size_of::<u32>())?,
            )?;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_layout_epoch_rejects_actual_atlas_reset_and_context_replacement() {
        let context = egui::Context::default();
        context.global_style_mut(|style| style.visuals.text_options.max_texture_side = 512);
        let mut epoch = LayoutEpoch::default();
        let mut frame = |expected| {
            let mut output = context.run_ui(Default::default(), |ui| {
                assert_eq!(epoch.refresh(ui), expected);
            });
            output.textures_delta.clear();
        };
        frame(false);
        frame(true);
        exhaust_font_atlas(&context);
        frame(false);
        // The full atlas was discarded, not just the witness entry.
        assert!(context.fonts(|fonts| fonts.font_atlas_fill_ratio()) < 0.8);
        frame(true);
        let other = egui::Context::default();
        let mut output = other.run_ui(Default::default(), |ui| assert!(!epoch.refresh(ui)));
        output.textures_delta.clear();
    }
}
