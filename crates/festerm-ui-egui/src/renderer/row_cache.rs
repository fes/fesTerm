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
    pub(super) background_meshes: bool,
    pub(super) reused: usize,
    pub(super) rebuilt: usize,
}

impl Default for Rows {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            bytes: 0,
            budget: MAX_BYTES,
            background_meshes: true,
            reused: 0,
            rebuilt: 0,
        }
    }
}

impl Rows {
    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    pub(super) fn enabled(&self) -> bool {
        self.budget > 0 && !self.entries.is_empty()
    }

    pub(super) fn diagnostics(&self) -> (usize, usize, usize) {
        (self.reused, self.rebuilt, self.bytes)
    }

    pub(super) fn begin(&mut self, rows: usize, full_redraw: bool) {
        self.reused = 0;
        self.rebuilt = 0;
        if full_redraw || self.entries.len() != rows {
            self.clear();
        }
        if rows <= MAX_ROWS && self.entries.len() != rows {
            self.entries.resize_with(rows, || None);
        }
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
        self.clear();
        self.budget = 0;
    }
}

fn retained_bytes(shape: &Shape) -> Option<usize> {
    let extra = match shape {
        Shape::Mesh(mesh) => mesh
            .vertices
            .capacity()
            .checked_mul(std::mem::size_of::<egui::epaint::Vertex>())?
            .checked_add(
                mesh.indices
                    .capacity()
                    .checked_mul(std::mem::size_of::<u32>())?,
            )?,
        Shape::Text(text) => {
            let galley = &text.galley;
            let mut bytes = std::mem::size_of_val(galley.as_ref())
                .checked_add(std::mem::size_of_val(galley.job.as_ref()))?
                .checked_add(galley.job.text.capacity())?
                .checked_add(
                    galley
                        .job
                        .sections
                        .capacity()
                        .checked_mul(std::mem::size_of::<egui::text::LayoutSection>())?,
                )?;
            for row in &galley.rows {
                bytes = bytes
                    .checked_add(std::mem::size_of_val(row))?
                    .checked_add(std::mem::size_of_val(row.row.as_ref()))?
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
    for (column, cell) in cells.iter().enumerate() {
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
