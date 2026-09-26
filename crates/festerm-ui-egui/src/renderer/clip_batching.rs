use egui::{
    emath::{GuiRounding, TSTransform},
    Pos2, Rect,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GridTextClipOverride {
    Auto,
    #[cfg_attr(not(test), expect(dead_code, reason = "test-only baseline clip path"))]
    AlwaysCell,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GridTextClipPainter {
    SharedParent,
    ClippedCell,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct GridGlyphClipPolicy {
    enabled: bool,
    parent_clip_rect: Rect,
    viewport_rect: Rect,
    pixels_per_point: f32,
    round_text_to_pixels: bool,
}

impl GridGlyphClipPolicy {
    pub(super) fn new(painter: &egui::Painter, clip_override: GridTextClipOverride) -> Self {
        let context = painter.ctx();
        let (round_text_to_pixels, debug_clip_rects, debug_text_rects, debug_ignore_clip_rects) =
            context.tessellation_options(|options| {
                (
                    options.round_text_to_pixels,
                    options.debug_paint_clip_rects,
                    options.debug_paint_text_rects,
                    options.debug_ignore_clip_rects,
                )
            });
        let has_non_identity_transform = context
            .layer_transform_to_global(painter.layer_id())
            .is_some_and(|transform| transform != TSTransform::IDENTITY);
        let enabled = match clip_override {
            GridTextClipOverride::Auto => {
                painter.is_visible()
                    && painter.clip_rect().is_positive()
                    && painter.opacity() == 1.0
                    && painter.ctx().pixels_per_point().is_finite()
                    && painter.ctx().pixels_per_point() > 0.0
                    && !has_non_identity_transform
                    && !debug_clip_rects
                    && !debug_text_rects
                    && !debug_ignore_clip_rects
            }
            GridTextClipOverride::AlwaysCell => false,
        };
        Self {
            enabled,
            parent_clip_rect: painter.clip_rect(),
            viewport_rect: context.viewport_rect(),
            pixels_per_point: context.pixels_per_point(),
            round_text_to_pixels,
        }
    }

    pub(super) fn galley_painter(
        self,
        cell_rect: Rect,
        text_position: Pos2,
        galley: &egui::Galley,
    ) -> GridTextClipPainter {
        let final_bounds = self.final_galley_bounds(text_position, galley);
        self.mesh_bounds_painter(cell_rect, final_bounds)
    }

    fn final_galley_bounds(self, text_position: Pos2, galley: &egui::Galley) -> Rect {
        let galley_origin = if self.round_text_to_pixels {
            text_position.round_to_pixels(self.pixels_per_point)
        } else {
            text_position
        };
        galley.mesh_bounds.translate(galley_origin.to_vec2())
    }

    fn mesh_bounds_painter(self, cell_rect: Rect, mesh_bounds: Rect) -> GridTextClipPainter {
        if !self.enabled {
            return GridTextClipPainter::ClippedCell;
        }
        if !mesh_bounds.is_positive() {
            return GridTextClipPainter::SharedParent;
        }
        let cell_clip_rect = cell_rect.intersect(self.parent_clip_rect);
        if !cell_clip_rect.is_positive() {
            return GridTextClipPainter::ClippedCell;
        }
        if !rect_contains_rect(cell_rect, mesh_bounds) {
            return GridTextClipPainter::ClippedCell;
        }
        if !self.backend_scissor_contains(mesh_bounds, cell_clip_rect) {
            return GridTextClipPainter::ClippedCell;
        }
        GridTextClipPainter::SharedParent
    }

    fn backend_scissor_contains(self, mesh_bounds: Rect, clip_rect: Rect) -> bool {
        let viewport_pixels = rounded_pixel_rect(self.viewport_rect, self.pixels_per_point);
        let clip_pixels =
            rounded_pixel_rect(clip_rect, self.pixels_per_point).clamp(viewport_pixels);
        let mesh_pixels = conservative_mesh_pixel_rect(mesh_bounds, self.pixels_per_point);
        mesh_pixels.min_x >= clip_pixels.min_x
            && mesh_pixels.min_y >= clip_pixels.min_y
            && mesh_pixels.max_x <= clip_pixels.max_x
            && mesh_pixels.max_y <= clip_pixels.max_y
    }
}

fn rect_contains_rect(container: Rect, candidate: Rect) -> bool {
    candidate.min.x >= container.min.x
        && candidate.min.y >= container.min.y
        && candidate.max.x <= container.max.x
        && candidate.max.y <= container.max.y
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PixelRect {
    min_x: i32,
    min_y: i32,
    max_x: i32,
    max_y: i32,
}

impl PixelRect {
    fn clamp(self, bounds: Self) -> Self {
        let min_x = self.min_x.clamp(bounds.min_x, bounds.max_x);
        let min_y = self.min_y.clamp(bounds.min_y, bounds.max_y);
        let max_x = self.max_x.clamp(min_x, bounds.max_x);
        let max_y = self.max_y.clamp(min_y, bounds.max_y);
        Self {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }
}

fn rounded_pixel_rect(rect: Rect, pixels_per_point: f32) -> PixelRect {
    PixelRect {
        min_x: (rect.min.x * pixels_per_point).round() as i32,
        min_y: (rect.min.y * pixels_per_point).round() as i32,
        max_x: (rect.max.x * pixels_per_point).round() as i32,
        max_y: (rect.max.y * pixels_per_point).round() as i32,
    }
}

fn conservative_mesh_pixel_rect(rect: Rect, pixels_per_point: f32) -> PixelRect {
    PixelRect {
        min_x: (rect.min.x * pixels_per_point).floor() as i32,
        min_y: (rect.min.y * pixels_per_point).floor() as i32,
        max_x: (rect.max.x * pixels_per_point).ceil() as i32,
        max_y: (rect.max.y * pixels_per_point).ceil() as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2, Context, LayerId};

    #[test]
    fn sharing_stays_disabled_for_transforms_and_debug_overlays() {
        let context = Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(200.0, 200.0));
        let mut transformed_policy = None;
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |context| {
                let painter = context.layer_painter(LayerId::background());
                context.set_transform_layer(
                    painter.layer_id(),
                    TSTransform::from_translation(vec2(1.0, 0.0)),
                );
                transformed_policy = Some(GridGlyphClipPolicy::new(
                    &painter,
                    GridTextClipOverride::Auto,
                ));
            },
        );
        output.textures_delta.clear();
        let transformed_policy = transformed_policy.expect("policy captured");
        assert_eq!(
            transformed_policy.mesh_bounds_painter(
                Rect::from_min_size(pos2(10.0, 10.0), vec2(8.0, 16.0)),
                Rect::from_min_size(pos2(11.0, 12.0), vec2(4.0, 8.0)),
            ),
            GridTextClipPainter::ClippedCell
        );

        for (debug_clip_rects, debug_text_rects, debug_ignore_clip_rects) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let debug_context = Context::default();
            let mut policy = None;
            let mut output = debug_context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |context| {
                    context.tessellation_options_mut(|options| {
                        options.debug_paint_clip_rects = debug_clip_rects;
                        options.debug_paint_text_rects = debug_text_rects;
                        options.debug_ignore_clip_rects = debug_ignore_clip_rects;
                    });
                    let painter = context.layer_painter(LayerId::background());
                    policy = Some(GridGlyphClipPolicy::new(
                        &painter,
                        GridTextClipOverride::Auto,
                    ));
                },
            );
            output.textures_delta.clear();
            let policy = policy.expect("debug policy captured");
            assert_eq!(
                policy.mesh_bounds_painter(
                    Rect::from_min_size(pos2(10.0, 10.0), vec2(8.0, 16.0)),
                    Rect::from_min_size(pos2(11.0, 12.0), vec2(4.0, 8.0)),
                ),
                GridTextClipPainter::ClippedCell
            );
        }
    }

    #[test]
    fn sharing_requires_float_and_backend_pixel_containment() {
        let policy = GridGlyphClipPolicy {
            enabled: true,
            parent_clip_rect: Rect::from_min_size(Pos2::ZERO, vec2(40.0, 40.0)),
            viewport_rect: Rect::from_min_size(Pos2::ZERO, vec2(40.0, 40.0)),
            pixels_per_point: 1.5,
            round_text_to_pixels: true,
        };
        let cell = Rect::from_min_size(pos2(5.25, 7.125), vec2(8.0, 16.0));

        assert_eq!(
            policy
                .mesh_bounds_painter(cell, Rect::from_min_max(pos2(6.0, 8.0), pos2(12.25, 20.5)),),
            GridTextClipPainter::SharedParent
        );
        assert_eq!(
            policy
                .mesh_bounds_painter(cell, Rect::from_min_max(pos2(4.9, 8.0), pos2(12.25, 20.5)),),
            GridTextClipPainter::ClippedCell
        );
        assert_eq!(
            policy
                .mesh_bounds_painter(cell, Rect::from_min_max(pos2(6.0, 8.0), pos2(13.34, 20.5)),),
            GridTextClipPainter::ClippedCell
        );
    }

    #[test]
    fn parent_viewport_clipping_remains_a_hard_boundary() {
        let policy = GridGlyphClipPolicy {
            enabled: true,
            parent_clip_rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(16.0, 16.0)),
            viewport_rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(16.0, 16.0)),
            pixels_per_point: 2.0,
            round_text_to_pixels: true,
        };
        let cell = Rect::from_min_size(pos2(12.0, 2.0), vec2(8.0, 12.0));

        assert_eq!(
            policy
                .mesh_bounds_painter(cell, Rect::from_min_max(pos2(12.5, 4.0), pos2(15.75, 12.0)),),
            GridTextClipPainter::SharedParent
        );
        assert_eq!(
            policy
                .mesh_bounds_painter(cell, Rect::from_min_max(pos2(12.5, 4.0), pos2(16.2, 12.0)),),
            GridTextClipPainter::ClippedCell
        );
    }
}
