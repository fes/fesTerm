use std::sync::Arc;

use egui::{emath::TSTransform, epaint::ClippedShape, Painter, Rect, Shape};

use super::{FontSettings, GridLayout};
use crate::{cache::RowRevision, geometry::CellRange};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_ROWS: usize = 1024;

#[derive(Clone, PartialEq)]
pub(super) struct Key {
    revision: RowRevision,
    layout: GridLayout,
    clip: Rect,
    viewport: Rect,
    pixels_per_point: f32,
    fonts: FontSettings,
    selection: Option<CellRange>,
    shaped: bool,
    font_revision: egui::epaint::FontImageRevision,
    tessellation: egui::epaint::TessellationOptions,
}

impl Key {
    pub(super) fn new(
        painter: &Painter,
        revision: &RowRevision,
        layout: GridLayout,
        fonts: &FontSettings,
        selection: Option<CellRange>,
        shaped: bool,
    ) -> Option<Self> {
        if !painter.is_visible()
            || painter.opacity() != 1.0
            || !painter.clip_rect().is_positive()
            || !painter.pixels_per_point().is_finite()
            || painter.pixels_per_point() <= 0.0
            || painter
                .ctx()
                .layer_transform_to_global(painter.layer_id())
                .is_some_and(|transform| transform != TSTransform::IDENTITY)
        {
            return None;
        }
        let tessellation = painter.ctx().tessellation_options(|options| *options);
        if tessellation.debug_paint_clip_rects
            || tessellation.debug_paint_text_rects
            || tessellation.debug_ignore_clip_rects
        {
            return None;
        }
        Some(Self {
            revision: revision.clone(),
            layout,
            clip: painter.clip_rect(),
            viewport: painter.ctx().viewport_rect(),
            pixels_per_point: painter.pixels_per_point(),
            fonts: fonts.clone(),
            selection,
            shaped,
            font_revision: painter.ctx().fonts_mut(|fonts| fonts.image_revision()),
            tessellation,
        })
    }

    fn finish(&mut self, painter: &Painter) {
        self.font_revision = painter.ctx().fonts_mut(|fonts| fonts.image_revision());
    }
}

struct Entry {
    key: Key,
    shapes: Vec<ClippedShape>,
    bytes: usize,
}

pub(super) struct Rows {
    entries: Vec<Option<Entry>>,
    bytes: usize,
    budget: usize,
    previous_revisions: Vec<RowRevision>,
    retain_frame: bool,
    pub(super) reused: usize,
    pub(super) rebuilt: usize,
}

impl Default for Rows {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            bytes: 0,
            budget: MAX_BYTES,
            previous_revisions: Vec::new(),
            retain_frame: true,
            reused: 0,
            rebuilt: 0,
        }
    }
}

impl Rows {
    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.previous_revisions.clear();
        self.bytes = 0;
    }

    pub(super) fn enabled(&self) -> bool {
        self.budget > 0 && !self.entries.is_empty() && self.retain_frame
    }

    pub(super) fn diagnostics(&self) -> (usize, usize, usize) {
        (self.reused, self.rebuilt, self.bytes)
    }

    pub(super) fn begin(&mut self, rows: usize, full_redraw: bool) {
        self.retain_frame = true;
        self.reused = 0;
        self.rebuilt = 0;
        if full_redraw || self.entries.len() != rows {
            self.clear();
        }
        if rows <= MAX_ROWS && self.entries.len() != rows {
            self.entries.resize_with(rows, || None);
        }
    }

    pub(super) fn observe(&mut self, cache: &crate::cache::TerminalRenderCache) {
        if self.budget == 0 || self.entries.is_empty() {
            return;
        }
        self.retain_frame = self.previous_revisions.len() != self.entries.len()
            || self
                .previous_revisions
                .iter()
                .enumerate()
                .any(|(row, old)| {
                    cache
                        .row_revision(row)
                        .is_some_and(|current| current == old)
                });
        self.previous_revisions.clear();
        self.previous_revisions
            .extend((0..self.entries.len()).filter_map(|row| cache.row_revision(row).cloned()));
    }

    pub(super) fn replay(&mut self, painter: &Painter, row: usize, key: &Key) -> bool {
        let Some(entry) = self.entries.get(row).and_then(Option::as_ref) else {
            return false;
        };
        if entry.key != *key {
            return false;
        }
        painter.ctx().graphics_mut(|graphics| {
            let list = graphics.entry(painter.layer_id());
            for shape in &entry.shapes {
                list.add(shape.clip_rect, shape.shape.clone());
            }
        });
        self.reused += 1;
        true
    }

    pub(super) fn remove(&mut self, row: usize) {
        if let Some(entry) = self.entries.get_mut(row).and_then(Option::take) {
            self.bytes -= entry.bytes;
        }
    }

    pub(super) fn capture(
        &mut self,
        painter: &Painter,
        row: usize,
        mut key: Key,
        start: egui::layers::ShapeIdx,
    ) {
        self.remove(row);
        let Some(slot) = self.entries.get_mut(row) else {
            return;
        };
        key.finish(painter);
        let remaining = self.budget.saturating_sub(self.bytes);
        let shapes = painter.ctx().graphics(|graphics| {
            graphics.get(painter.layer_id()).map(|list| {
                let bytes = list
                    .all_entries()
                    .skip(start.0)
                    .try_fold(0usize, |total, shape| {
                        total.checked_add(retained_bytes(&shape.shape)?)
                    })?;
                (bytes <= remaining)
                    .then(|| (list.all_entries().skip(start.0).cloned().collect(), bytes))
            })
        });
        self.rebuilt += 1;
        if let Some((shapes, bytes)) = shapes.flatten() {
            *slot = Some(Entry { key, shapes, bytes });
            self.bytes += bytes;
        }
    }

    #[cfg(test)]
    pub(super) fn disable(&mut self) {
        self.set_budget(0);
    }

    #[cfg(test)]
    pub(super) fn set_budget(&mut self, bytes: usize) {
        self.clear();
        self.budget = bytes.min(MAX_BYTES);
    }
}

fn retained_bytes(shape: &Shape) -> Option<usize> {
    let extra = match shape {
        Shape::Mesh(mesh) if mesh.texture_id == egui::TextureId::default() => {
            std::mem::size_of_val(mesh.as_ref())
                .checked_add(2 * std::mem::size_of::<usize>())?
                .checked_add(
                    mesh.vertices
                        .capacity()
                        .checked_mul(std::mem::size_of::<egui::epaint::Vertex>())?,
                )?
                .checked_add(
                    mesh.indices
                        .capacity()
                        .checked_mul(std::mem::size_of::<u32>())?,
                )?
        }
        Shape::Text(text) => {
            let galley = &text.galley;
            let mut bytes = std::mem::size_of_val(galley.as_ref())
                .checked_add(std::mem::size_of_val(galley.job.as_ref()))?
                .checked_add(4 * std::mem::size_of::<usize>())?
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
            for row in &galley.rows {
                if row.visuals.mesh.texture_id != egui::TextureId::default() {
                    return None;
                }
                bytes = bytes
                    .checked_add(std::mem::size_of_val(row.row.as_ref()))?
                    .checked_add(2 * std::mem::size_of::<usize>())?
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
            bytes
        }
        Shape::Rect(_) | Shape::LineSegment { .. } | Shape::Noop => 0,
        _ => return None,
    };
    std::mem::size_of::<ClippedShape>().checked_add(extra)
}

pub(super) fn backgrounds(
    painter: &Painter,
    cells: &[crate::cache::RenderedCell],
    columns: std::ops::Range<usize>,
    row: usize,
    layout: GridLayout,
    selection: Option<CellRange>,
) {
    let options = painter.ctx().tessellation_options(|options| *options);
    // Rectangles use WHITE_UV, so this mesh never captures font-atlas coordinates.
    let mut tessellator =
        egui::epaint::Tessellator::new(painter.pixels_per_point(), options, [1, 1], Vec::new());
    tessellator.set_clip_rect(painter.clip_rect());
    let mut mesh = egui::epaint::Mesh::default();
    for column in columns {
        let cell = &cells[column];
        if cell.width == festerm_core::CellWidth::Continuation {
            continue;
        }
        let position = crate::geometry::CellPosition { column, row };
        let columns = super::rendered_cell_columns(cell, layout.dimensions, column);
        let selected = super::rendered_cell_is_selected(selection, position, columns);
        if super::cell_needs_background_paint(cell, selected) {
            let rect = super::grid_cell_rect(layout, position, columns);
            let color = if selected {
                crate::SELECTION_BACKGROUND
            } else {
                super::cell_colors(cell).1
            };
            tessellator.tessellate_rect(
                &egui::epaint::RectShape::filled(rect, 0.0, color),
                &mut mesh,
            );
        }
    }
    if !mesh.is_empty() {
        painter.add(Shape::Mesh(Arc::new(mesh)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_row_budget_rejects_overflow_and_releases_shapes() {
        let context = egui::Context::default();
        let mut rows = Rows::default();
        rows.begin(2, false);
        let rect_bytes = retained_bytes(&Shape::rect_filled(
            Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(20.0, 20.0)),
            0.0,
            egui::Color32::BLUE,
        ))
        .expect("supported rectangle");
        rows.budget = rect_bytes;
        let mut output = context.run_ui(Default::default(), |ui| {
            let painter = ui.painter();
            let key = Key::new(
                painter,
                &RowRevision::default(),
                super::super::tests::grid_layout(4, 2),
                &FontSettings::default(),
                None,
                true,
            )
            .expect("ordinary painter");
            let start =
                context.graphics_mut(|graphics| graphics.entry(painter.layer_id()).next_idx());
            painter.rect_filled(painter.clip_rect(), 0.0, egui::Color32::BLUE);
            rows.capture(painter, 0, key.clone(), start);
            assert_eq!(rows.bytes, rect_bytes);
            let start =
                context.graphics_mut(|graphics| graphics.entry(painter.layer_id()).next_idx());
            painter.rect_filled(painter.clip_rect(), 0.0, egui::Color32::RED);
            rows.capture(painter, 1, key, start);
            assert!(rows.entries[0].is_some());
            assert!(rows.entries[1].is_none());
            assert_eq!(rows.bytes, rect_bytes);
        });
        output.textures_delta.clear();
        rows.begin(MAX_ROWS + 1, false);
        assert!(!rows.enabled());
        assert_eq!(rows.bytes, 0);
        rows.begin(2, false);
        assert!(rows.enabled());
        rows.clear();
        assert_eq!(rows.bytes, 0);
        assert!(rows.entries.is_empty());
    }

    #[test]
    fn retained_row_payload_rejects_foreign_textures_and_counts_mesh_capacity() {
        let mut mesh = egui::epaint::Mesh::default();
        mesh.vertices.reserve(100);
        mesh.indices.reserve(300);
        let expected = std::mem::size_of::<ClippedShape>()
            + std::mem::size_of::<egui::epaint::Mesh>()
            + 2 * std::mem::size_of::<usize>()
            + mesh.vertices.capacity() * std::mem::size_of::<egui::epaint::Vertex>()
            + mesh.indices.capacity() * std::mem::size_of::<u32>();
        let mut mesh = Arc::new(mesh);
        assert_eq!(retained_bytes(&Shape::Mesh(mesh.clone())), Some(expected));
        Arc::get_mut(&mut mesh).expect("unique mesh").texture_id = egui::TextureId::Managed(1);
        assert_eq!(retained_bytes(&Shape::Mesh(mesh)), None);
        let mesh = egui::epaint::Mesh::with_texture(egui::TextureId::User(0));
        assert_eq!(retained_bytes(&Shape::Mesh(Arc::new(mesh))), None);
    }

    #[test]
    fn retained_row_clear_releases_owned_meshes() {
        let context = egui::Context::default();
        let mut rows = Rows::default();
        rows.begin(1, false);
        let mut mesh = egui::epaint::Mesh::default();
        mesh.add_colored_rect(
            Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(20.0, 20.0)),
            egui::Color32::BLUE,
        );
        let mesh = Arc::new(mesh);
        let weak = Arc::downgrade(&mesh);
        let mut output = context.run_ui(Default::default(), |ui| {
            let painter = ui.painter();
            let key = Key::new(
                painter,
                &RowRevision::default(),
                super::super::tests::grid_layout(4, 1),
                &FontSettings::default(),
                None,
                true,
            )
            .expect("ordinary painter");
            let start =
                context.graphics_mut(|graphics| graphics.entry(painter.layer_id()).next_idx());
            painter.add(Shape::Mesh(mesh.clone()));
            rows.capture(painter, 0, key, start);
        });
        output.textures_delta.clear();
        output.shapes.clear();
        drop(mesh);
        assert!(weak.upgrade().is_some());
        rows.clear();
        assert!(weak.upgrade().is_none());
    }
}
