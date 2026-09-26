use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use egui::{
    epaint::ClippedPrimitive, layers::ShapeIdx, ColorImage, Context, Id, PaintCallback, Painter,
    Rect, Shape, TextureId, ViewportId,
};

/// An owned presentation snapshot. It contains no terminal state or input policy.
pub struct TerminalPaintFrame {
    pub rect: Rect,
    pub pixels_per_point: f32,
    pub primitives: Vec<ClippedPrimitive>,
    pub textures: Vec<(TextureId, Arc<ColorImage>)>,
}

type Factory = dyn Fn(&Context, TerminalPaintFrame) -> Option<PaintCallback> + Send + Sync;

#[derive(Clone)]
struct Hook(Arc<Factory>);

#[derive(Clone, Default)]
struct Images(Arc<Mutex<HashMap<TextureId, Arc<ColorImage>>>>);

fn hook_id() -> Id {
    Id::new("festerm::root-terminal-painter")
}

fn images_id() -> Id {
    Id::new("festerm::terminal-painter-images")
}

/// Installs an optional root-viewport painter before terminal views are created.
///
/// Returning `None` retains every ordinary shape for that frame. Implementations
/// own capability checks and error reporting, and must preserve the supplied
/// clipping, colors, and paint order. Other viewports and translucent painters
/// retain ordinary painting.
pub fn install_root_terminal_painter(
    context: &Context,
    factory: impl Fn(&Context, TerminalPaintFrame) -> Option<PaintCallback> + Send + Sync + 'static,
) {
    context.data_mut(|data| data.insert_temp(hook_id(), Hook(Arc::new(factory))));
}

/// Removes the optional painter, restoring ordinary painting immediately.
pub fn remove_root_terminal_painter(context: &Context) {
    context.data_mut(|data| data.remove::<Hook>(hook_id()));
}

pub(crate) fn installed(context: &Context) -> bool {
    context.data(|data| data.get_temp::<Hook>(hook_id()).is_some())
}

pub(crate) fn record_texture(context: &Context, id: TextureId, image: &Arc<ColorImage>) {
    if let Some(images) = context.data(|data| data.get_temp::<Images>(images_id())) {
        images
            .0
            .lock()
            .expect("native image collector")
            .insert(id, image.clone());
    }
}

pub(crate) struct Batch {
    context: Context,
    hook: Hook,
    images: Images,
    start: ShapeIdx,
    rect: Rect,
}

impl Batch {
    pub(crate) fn begin(painter: &Painter, rect: Rect) -> Option<Self> {
        let context = painter.ctx();
        if context.viewport_id() != ViewportId::ROOT
            || painter.opacity() != 1.0
            || !painter.is_visible()
            || !rect.is_positive()
            || context
                .layer_transform_to_global(painter.layer_id())
                .is_some_and(|transform| transform != egui::emath::TSTransform::IDENTITY)
        {
            return None;
        }
        let hook = context.data(|data| data.get_temp::<Hook>(hook_id()))?;
        let images = Images::default();
        context.data_mut(|data| data.insert_temp(images_id(), images.clone()));
        let start = context.graphics_mut(|graphics| graphics.entry(painter.layer_id()).next_idx());
        Some(Self {
            context: context.clone(),
            hook,
            images,
            start,
            rect: rect.intersect(context.viewport_rect()),
        })
    }

    pub(crate) fn finish(self, painter: &Painter) {
        self.context
            .data_mut(|data| data.remove::<Images>(images_id()));
        let shapes = self.context.graphics(|graphics| {
            graphics
                .get(painter.layer_id())
                .expect("terminal paint list")
                .all_entries()
                .skip(self.start.0)
                .cloned()
                .collect::<Vec<_>>()
        });
        if shapes.is_empty() || !self.rect.is_positive() {
            return;
        }
        let count = shapes.len();
        let pixels_per_point = self.context.pixels_per_point();
        let primitives = self.context.tessellate(shapes, pixels_per_point);
        if primitives.is_empty() {
            return;
        }
        let mut textures = self
            .images
            .0
            .lock()
            .expect("native image collector")
            .clone();
        textures.insert(
            TextureId::Managed(0),
            Arc::new(self.context.fonts(|fonts| fonts.image())),
        );
        let frame = TerminalPaintFrame {
            rect: self.rect,
            pixels_per_point,
            primitives,
            textures: textures.into_iter().collect(),
        };
        if let Some(callback) = (self.hook.0)(&self.context, frame) {
            for index in self.start.0..self.start.0 + count {
                painter.set(ShapeIdx(index), Shape::Noop);
            }
            painter.set(self.start, Shape::Callback(callback));
        }
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        self.context
            .data_mut(|data| data.remove::<Images>(images_id()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Color32;

    #[test]
    fn declined_native_paint_keeps_original_shapes() {
        let context = Context::default();
        install_root_terminal_painter(&context, |_, frame| {
            assert!(!frame.primitives.is_empty());
            assert!(!frame.textures.is_empty());
            None
        });
        let mut output = context.run_ui(Default::default(), |ui| {
            let painter = ui.painter();
            let batch = Batch::begin(painter, ui.max_rect()).unwrap();
            let index = painter.rect_filled(ui.max_rect(), 0.0, Color32::RED);
            batch.finish(painter);
            ui.ctx().graphics(|graphics| {
                let shape = graphics
                    .get(painter.layer_id())
                    .unwrap()
                    .all_entries()
                    .nth(index.0)
                    .unwrap();
                assert!(matches!(&shape.shape, Shape::Rect(rect) if rect.fill == Color32::RED));
            });
        });
        output.textures_delta.clear();
    }

    #[test]
    fn native_paint_replaces_only_its_scope_and_preserves_order() {
        let context = Context::default();
        install_root_terminal_painter(&context, |_, frame| {
            Some(PaintCallback {
                rect: frame.rect,
                callback: Arc::new(()),
            })
        });
        let mut output = context.run_ui(Default::default(), |ui| {
            let painter = ui.painter();
            let rect = ui.max_rect();
            let prefix = painter.rect_filled(rect, 0.0, Color32::GREEN);
            let batch = Batch::begin(painter, rect).unwrap();
            let first = painter.rect_filled(rect, 0.0, Color32::RED);
            let second = painter.rect_filled(rect, 0.0, Color32::BLUE);
            batch.finish(painter);
            let suffix = painter.rect_filled(rect, 0.0, Color32::YELLOW);
            ui.ctx().graphics(|graphics| {
                let shapes = graphics.get(painter.layer_id()).unwrap().all_entries().collect::<Vec<_>>();
                assert!(matches!(&shapes[prefix.0].shape, Shape::Rect(value) if value.fill == Color32::GREEN));
                assert!(matches!(&shapes[first.0].shape, Shape::Callback(_)));
                assert!(matches!(&shapes[second.0].shape, Shape::Noop));
                assert!(matches!(&shapes[suffix.0].shape, Shape::Rect(value) if value.fill == Color32::YELLOW));
            });
        });
        output.textures_delta.clear();
    }

    #[test]
    fn translucent_and_invisible_painters_never_enter_native_capture() {
        let context = Context::default();
        install_root_terminal_painter(&context, |_, _| panic!("unexpected native paint"));
        let rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(50.0, 50.0));
        let mut painter = context.layer_painter(egui::LayerId::background());
        painter.set_opacity(0.5);
        assert!(Batch::begin(&painter, rect).is_none());
        painter.set_opacity(1.0);
        painter.set_invisible();
        assert!(Batch::begin(&painter, rect).is_none());
        remove_root_terminal_painter(&context);
        assert!(!installed(&context));
    }
}
