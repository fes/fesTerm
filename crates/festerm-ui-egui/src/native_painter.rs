use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use egui::{
    epaint::ClippedPrimitive, layers::ShapeIdx, ColorImage, Context, Id, PaintCallback, Painter,
    Rect, Shape, TextureId, ViewportId,
};

/// An owned presentation snapshot. It contains no terminal state or input policy.
pub struct TerminalPaintFrame {
    pub rect: Rect,
    pub pixels_per_point: f32,
    /// Bypass retained-region reuse for this presentation, even if identical.
    pub full_redraw: bool,
    pub primitives: Vec<ClippedPrimitive>,
    pub textures: Vec<(TextureId, Arc<ColorImage>)>,
    pub font_atlas_capture: FontAtlasCapture,
}

/// Content-free capture diagnostics, separate from native callback timings.
#[derive(Clone, Copy, Debug, Default)]
pub struct FontAtlasCapture {
    pub elapsed: Duration,
    pub atlas_bytes: usize,
    pub cloned_bytes: usize,
    pub reused: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct NativePainterOptions {
    /// Capture wall time only when renderer profiling is enabled.
    pub capture_font_atlas_timings: bool,
    /// May reduce the 64 MiB retention limit; zero disables snapshot retention.
    pub font_atlas_cache_budget_bytes: usize,
}

impl Default for NativePainterOptions {
    fn default() -> Self {
        Self {
            capture_font_atlas_timings: false,
            font_atlas_cache_budget_bytes: MAX_CACHED_FONT_BYTES,
        }
    }
}

type Factory = dyn Fn(&Context, TerminalPaintFrame) -> Option<PaintCallback> + Send + Sync;
type AtlasAdmission = dyn Fn(&Context, [usize; 2]) -> bool + Send + Sync;

#[derive(Clone)]
struct Hook {
    factory: Arc<Factory>,
    options: NativePainterOptions,
    atlas_admission: Option<Arc<AtlasAdmission>>,
}

#[derive(Clone)]
struct FontAtlasSnapshot {
    revision: egui::epaint::FontImageRevision,
    image: Arc<ColorImage>,
}

const MAX_CACHED_FONT_BYTES: usize = 64 * 1024 * 1024;

fn font_atlas_id() -> Id {
    Id::new("festerm::native-font-atlas-snapshot")
}

fn font_atlas_snapshot_with_budget(
    context: &Context,
    budget_bytes: usize,
) -> (Arc<ColorImage>, bool) {
    let previous = context.data(|data| data.get_temp::<FontAtlasSnapshot>(font_atlas_id()));
    let (snapshot, reused) = context.fonts_mut(|fonts| {
        let revision = fonts.image_revision();
        if let Some(previous) = previous.filter(|previous| {
            previous.revision == revision
                && previous.image.pixels.len() * std::mem::size_of::<egui::Color32>()
                    <= budget_bytes.min(MAX_CACHED_FONT_BYTES)
        }) {
            return (previous, true);
        }
        (
            FontAtlasSnapshot {
                revision,
                image: Arc::new(fonts.image()),
            },
            false,
        )
    });
    if !reused {
        context.data_mut(|data| {
            data.remove::<FontAtlasSnapshot>(font_atlas_id());
            let bytes = snapshot.image.pixels.len() * std::mem::size_of::<egui::Color32>();
            #[cfg(test)]
            {
                let id = Id::new("festerm::test-atlas-copied-bytes");
                let copied = data.get_temp::<usize>(id).unwrap_or_default();
                data.insert_temp(id, copied + bytes);
            }
            if bytes <= budget_bytes.min(MAX_CACHED_FONT_BYTES) {
                data.insert_temp(font_atlas_id(), snapshot.clone());
            }
        });
    }
    (snapshot.image, reused)
}

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
    install_root_terminal_painter_with_options(context, NativePainterOptions::default(), factory);
}

/// Installs the same painter with optional content-free capture timing.
pub fn install_root_terminal_painter_with_options(
    context: &Context,
    options: NativePainterOptions,
    factory: impl Fn(&Context, TerminalPaintFrame) -> Option<PaintCallback> + Send + Sync + 'static,
) {
    install_root_terminal_painter_hook(context, options, None, factory);
}

/// Installs backend-owned atlas admission before copying pixels.
///
/// The callback receives post-tessellation dimensions, owns refusal reporting,
/// and returns false to preserve ordinary shapes without capturing the atlas.
pub fn install_root_terminal_painter_with_admission(
    context: &Context,
    options: NativePainterOptions,
    admission: impl Fn(&Context, [usize; 2]) -> bool + Send + Sync + 'static,
    factory: impl Fn(&Context, TerminalPaintFrame) -> Option<PaintCallback> + Send + Sync + 'static,
) {
    install_root_terminal_painter_hook(context, options, Some(Arc::new(admission)), factory);
}

fn install_root_terminal_painter_hook(
    context: &Context,
    options: NativePainterOptions,
    atlas_admission: Option<Arc<AtlasAdmission>>,
    factory: impl Fn(&Context, TerminalPaintFrame) -> Option<PaintCallback> + Send + Sync + 'static,
) {
    context.data_mut(|data| {
        data.remove::<FontAtlasSnapshot>(font_atlas_id());
        data.insert_temp(
            hook_id(),
            Hook {
                factory: Arc::new(factory),
                options,
                atlas_admission,
            },
        );
    });
}

/// Removes the optional painter, restoring ordinary painting immediately.
pub fn remove_root_terminal_painter(context: &Context) {
    context.data_mut(|data| {
        data.remove::<Hook>(hook_id());
        data.remove::<FontAtlasSnapshot>(font_atlas_id());
    });
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
    full_redraw: bool,
}

impl Batch {
    fn is_current(&self) -> bool {
        self.context.data(|data| {
            data.get_temp::<Hook>(hook_id())
                .is_some_and(|hook| Arc::ptr_eq(&hook.factory, &self.hook.factory))
        })
    }

    pub(crate) fn begin(painter: &Painter, rect: Rect, full_redraw: bool) -> Option<Self> {
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
            full_redraw,
        })
    }

    pub(crate) fn finish(self, painter: &Painter) {
        self.context
            .data_mut(|data| data.remove::<Images>(images_id()));
        if !self.is_current() {
            return;
        }
        let shapes = self.context.graphics(|graphics| {
            graphics
                .get(painter.layer_id())
                .expect("terminal paint list")
                .all_entries()
                .skip(self.start.0)
                .cloned()
                .collect::<Vec<_>>()
        });
        if (shapes.is_empty() && !self.full_redraw) || !self.rect.is_positive() {
            return;
        }
        let count = shapes.len();
        let pixels_per_point = self.context.pixels_per_point();
        let primitives = self.context.tessellate(shapes, pixels_per_point);
        if primitives.is_empty() && !self.full_redraw {
            return;
        }
        if let Some(admission) = &self.hook.atlas_admission {
            let dimensions = self.context.fonts(|fonts| fonts.font_image_size());
            let admitted = admission(&self.context, dimensions);
            if !self.is_current() {
                return;
            }
            if !admitted {
                self.context
                    .data_mut(|data| data.remove::<FontAtlasSnapshot>(font_atlas_id()));
                return;
            }
        }
        let mut textures = self
            .images
            .0
            .lock()
            .expect("native image collector")
            .clone();
        let capture_started = self
            .hook
            .options
            .capture_font_atlas_timings
            .then(Instant::now);
        let (font_atlas, reused) = font_atlas_snapshot_with_budget(
            &self.context,
            self.hook.options.font_atlas_cache_budget_bytes,
        );
        let atlas_bytes = font_atlas.pixels.len() * std::mem::size_of::<egui::Color32>();
        let font_atlas_capture = FontAtlasCapture {
            elapsed: capture_started
                .map(|started| started.elapsed())
                .unwrap_or_default(),
            atlas_bytes,
            cloned_bytes: if reused { 0 } else { atlas_bytes },
            reused,
        };
        textures.insert(TextureId::Managed(0), font_atlas);
        let frame = TerminalPaintFrame {
            rect: self.rect,
            pixels_per_point,
            full_redraw: self.full_redraw,
            primitives,
            textures: textures.into_iter().collect(),
            font_atlas_capture,
        };
        if let Some(callback) = (self.hook.factory)(&self.context, frame) {
            if count == 0 {
                painter.add(Shape::Callback(callback));
                return;
            }
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

    fn font_atlas_snapshot(context: &Context) -> (Arc<ColorImage>, bool) {
        font_atlas_snapshot_with_budget(context, MAX_CACHED_FONT_BYTES)
    }

    #[test]
    fn native_atlas_admission_refuses_before_any_pixel_snapshot_or_factory_capture() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let context = Context::default();
        let admission_calls = Arc::new(AtomicUsize::new(0));
        let factory_calls = Arc::new(AtomicUsize::new(0));
        let cloned_bytes = Arc::new(AtomicUsize::new(0));
        let admitted = admission_calls.clone();
        let captured = factory_calls.clone();
        let copied = cloned_bytes.clone();
        install_root_terminal_painter_with_admission(
            &context,
            NativePainterOptions::default(),
            move |_, _| {
                admitted.fetch_add(1, Ordering::Relaxed);
                false
            },
            move |_, frame| {
                captured.fetch_add(1, Ordering::Relaxed);
                copied.fetch_add(frame.font_atlas_capture.cloned_bytes, Ordering::Relaxed);
                None
            },
        );
        let mut output = context.run_ui(Default::default(), |ui| {
            let painter = ui.painter();
            let batch = Batch::begin(painter, ui.max_rect(), false).unwrap();
            painter.rect_filled(ui.max_rect(), 0.0, Color32::RED);
            batch.finish(painter);
        });
        output.textures_delta.clear();
        assert_eq!(
            context.data(|data| data
                .get_temp::<usize>(Id::new("festerm::test-atlas-copied-bytes"))
                .unwrap_or_default()),
            0,
            "refused atlas must not make even a discarded pixel copy"
        );
        assert_eq!(
            factory_calls.load(Ordering::Relaxed),
            0,
            "refused atlas copied {} bytes before factory rejection",
            cloned_bytes.load(Ordering::Relaxed)
        );
        assert_eq!(admission_calls.load(Ordering::Relaxed), 1);
        assert_eq!(cloned_bytes.load(Ordering::Relaxed), 0);
        assert!(context.data(|data| data
            .get_temp::<FontAtlasSnapshot>(font_atlas_id())
            .is_none()));
        assert!(output
            .shapes
            .iter()
            .any(|shape| matches!(&shape.shape, Shape::Rect(rect) if rect.fill == Color32::RED)));
    }

    #[test]
    fn native_atlas_admission_uses_post_tessellation_dimensions_and_preserves_font_deltas() {
        let context = Context::default();
        let dimensions = Arc::new(Mutex::new(None));
        let observed = dimensions.clone();
        install_root_terminal_painter_with_admission(
            &context,
            NativePainterOptions {
                font_atlas_cache_budget_bytes: 0,
                ..Default::default()
            },
            move |context, size| {
                assert_eq!(size, context.fonts(|fonts| fonts.font_image_size()));
                *observed.lock().unwrap() = Some(size);
                true
            },
            |context, frame| {
                let image = frame
                    .textures
                    .iter()
                    .find(|(id, _)| *id == TextureId::Managed(0))
                    .unwrap()
                    .1
                    .as_ref();
                assert_eq!(image, &context.fonts(|fonts| fonts.image()));
                assert!(!frame.font_atlas_capture.reused);
                assert_eq!(
                    frame.font_atlas_capture.cloned_bytes,
                    image.pixels.len() * 4
                );
                assert_eq!(
                    context.data(|data| data
                        .get_temp::<usize>(Id::new("festerm::test-atlas-copied-bytes"))
                        .unwrap()),
                    frame.font_atlas_capture.cloned_bytes
                );
                None
            },
        );
        let mut output = context.run_ui(Default::default(), |ui| {
            let painter = ui.painter();
            let batch = Batch::begin(painter, ui.max_rect(), false).unwrap();
            painter.text(
                egui::Pos2::ZERO,
                egui::Align2::LEFT_TOP,
                "same-frame glyph growth",
                egui::FontId::monospace(37.0),
                Color32::WHITE,
            );
            batch.finish(painter);
            assert_eq!(
                *dimensions.lock().unwrap(),
                Some(ui.ctx().fonts(|fonts| fonts.font_image_size()))
            );
            assert!(ui.ctx().data(|data| data
                .get_temp::<FontAtlasSnapshot>(font_atlas_id())
                .is_none()));
        });
        assert!(output
            .textures_delta
            .set
            .iter()
            .any(|(id, _)| *id == TextureId::Managed(0)));
        output.textures_delta.clear();
    }

    #[test]
    fn retired_admission_cannot_capture_or_clear_a_replacement_painter_snapshot() {
        for replace in [false, true] {
            for admit in [false, true] {
                let context = Context::default();
                let replacement = Arc::new(Mutex::new(None));
                let observed = replacement.clone();
                install_root_terminal_painter_with_admission(
                    &context,
                    NativePainterOptions::default(),
                    move |context, _| {
                        remove_root_terminal_painter(context);
                        if replace {
                            install_root_terminal_painter(context, |_, _| {
                                panic!("replacement factory must start with its own batch")
                            });
                            let (image, _) = font_atlas_snapshot(context);
                            *observed.lock().unwrap() = Some(Arc::downgrade(&image));
                        }
                        admit
                    },
                    |_, _| panic!("retired capture factory called"),
                );
                let mut output = context.run_ui(Default::default(), |ui| {
                    let painter = ui.painter();
                    let batch = Batch::begin(painter, ui.max_rect(), false).unwrap();
                    painter.rect_filled(ui.max_rect(), 0.0, Color32::RED);
                    batch.finish(painter);
                });
                output.textures_delta.clear();
                let snapshot =
                    context.data(|data| data.get_temp::<FontAtlasSnapshot>(font_atlas_id()));
                assert_eq!(snapshot.is_some(), replace);
                if replace {
                    let replacement = replacement.lock().unwrap();
                    let image = replacement.as_ref().unwrap().upgrade().unwrap();
                    assert!(Arc::ptr_eq(&snapshot.unwrap().image, &image));
                }
                assert!(output.shapes.iter().any(
                    |shape| matches!(&shape.shape, Shape::Rect(rect) if rect.fill == Color32::RED)
                ));
            }
        }
    }

    #[test]
    fn native_atlas_admission_recovers_after_refusal_and_releases_old_snapshot() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let context = Context::default();
        let permitted = Arc::new(AtomicBool::new(true));
        let admission = permitted.clone();
        let captures = Arc::new(Mutex::new(Vec::new()));
        let observed = captures.clone();
        install_root_terminal_painter_with_admission(
            &context,
            NativePainterOptions::default(),
            move |_, _| admission.load(Ordering::Relaxed),
            move |_, frame| {
                let image = &frame
                    .textures
                    .iter()
                    .find(|(id, _)| *id == TextureId::Managed(0))
                    .unwrap()
                    .1;
                observed
                    .lock()
                    .unwrap()
                    .push((Arc::downgrade(image), frame.font_atlas_capture));
                None
            },
        );
        let mut output = context.run_ui(Default::default(), |ui| {
            let painter = ui.painter();
            for accepted in [true, false, false, true, true] {
                permitted.store(accepted, Ordering::Relaxed);
                let batch = Batch::begin(painter, ui.max_rect(), false).unwrap();
                painter.rect_filled(ui.max_rect(), 0.0, Color32::RED);
                batch.finish(painter);
                let captures = captures.lock().unwrap();
                if !accepted {
                    assert_eq!(captures.len(), 1);
                    assert!(captures[0].0.upgrade().is_none());
                }
            }
        });
        output.textures_delta.clear();
        let captures = captures.lock().unwrap();
        assert_eq!(captures.len(), 3);
        assert!(!captures[0].1.reused);
        assert!(!captures[1].1.reused);
        assert!(captures[2].1.reused);
        assert_eq!(captures[2].1.cloned_bytes, 0);
        assert!(captures[0].0.upgrade().is_none());
        assert!(Arc::ptr_eq(
            &captures[1].0.upgrade().unwrap(),
            &captures[2].0.upgrade().unwrap()
        ));
    }

    #[test]
    fn retired_capture_cannot_repopulate_a_removed_or_replaced_painter_cache() {
        for replace in [false, true] {
            let context = Context::default();
            install_root_terminal_painter(&context, |_, _| panic!("retired painter called"));
            let mut output = context.run_ui(Default::default(), |ui| {
                let painter = ui.painter();
                let batch = Batch::begin(painter, ui.max_rect(), false).unwrap();
                painter.rect_filled(ui.max_rect(), 0.0, Color32::RED);
                remove_root_terminal_painter(ui.ctx());
                if replace {
                    install_root_terminal_painter(ui.ctx(), |_, _| {
                        panic!("replacement must start with its own batch")
                    });
                }
                batch.finish(painter);
                assert!(ui.ctx().data(|data| data
                    .get_temp::<FontAtlasSnapshot>(font_atlas_id())
                    .is_none()));
            });
            assert!(output.shapes.iter().any(
                |shape| matches!(&shape.shape, Shape::Rect(rect) if rect.fill == Color32::RED)
            ));
            output.textures_delta.clear();
        }
    }

    #[test]
    fn font_snapshot_budget_bounds_retention_without_changing_pixels() {
        let context = Context::default();
        let mut output = context.run_ui(Default::default(), |ui| {
            let (image, _) = font_atlas_snapshot_with_budget(ui.ctx(), 0);
            let bytes = image.pixels.len() * std::mem::size_of::<egui::Color32>();
            assert!(bytes > 0);
            assert!(ui.ctx().data(|data| data
                .get_temp::<FontAtlasSnapshot>(font_atlas_id())
                .is_none()));
            let (other, reused) = font_atlas_snapshot_with_budget(ui.ctx(), bytes - 1);
            assert!(!reused);
            assert!(!Arc::ptr_eq(&image, &other));
            assert_eq!(image.as_ref(), other.as_ref());
            let (_, reused) = font_atlas_snapshot_with_budget(ui.ctx(), bytes);
            assert!(!reused);
            let (cached, reused) = font_atlas_snapshot_with_budget(ui.ctx(), bytes);
            assert!(reused);
            assert_eq!(cached.as_ref(), image.as_ref());
            let (_, reused) = font_atlas_snapshot_with_budget(ui.ctx(), bytes - 1);
            assert!(!reused);
            assert!(ui.ctx().data(|data| data
                .get_temp::<FontAtlasSnapshot>(font_atlas_id())
                .is_none()));
        });
        output.textures_delta.clear();
    }

    #[test]
    fn font_snapshot_reuses_unchanged_pixels_and_refreshes_same_frame_glyphs() {
        let context = Context::default();
        let mut output = context.run_ui(Default::default(), |ui| {
            let (original, reused) = font_atlas_snapshot(ui.ctx());
            assert!(!reused);
            let frozen = original.as_ref().clone();
            let (unchanged, reused) = font_atlas_snapshot(ui.ctx());
            assert!(reused);
            assert!(Arc::ptr_eq(&original, &unchanged));
            let _ = ui.painter().layout_no_wrap(
                "New glyphs at a new size".to_owned(),
                egui::FontId::monospace(37.0),
                Color32::WHITE,
            );
            let (updated, reused) = font_atlas_snapshot(ui.ctx());
            assert!(!reused);
            assert!(!Arc::ptr_eq(&original, &updated));
            assert_ne!(original.as_ref(), updated.as_ref());
            assert_eq!(original.as_ref(), &frozen);
            assert_eq!(updated.as_ref(), &ui.ctx().fonts(|fonts| fonts.image()));
            let (unchanged, reused) = font_atlas_snapshot(ui.ctx());
            assert!(reused);
            assert!(Arc::ptr_eq(&updated, &unchanged));
        });
        output.textures_delta.clear();
    }

    #[test]
    fn font_snapshot_preserves_deltas_and_releases_on_painter_teardown() {
        let context = Context::default();
        install_root_terminal_painter(&context, |_, _| None);
        let mut weak = None;
        let mut output = context.run_ui(Default::default(), |ui| {
            let (image, _) = font_atlas_snapshot(ui.ctx());
            weak = Some(Arc::downgrade(&image));
        });
        assert!(!output.textures_delta.set.is_empty());
        output.textures_delta.clear();
        let mut weak = weak.unwrap();
        assert!(weak.upgrade().is_some());
        remove_root_terminal_painter(&context);
        assert!(weak.upgrade().is_none());
        install_root_terminal_painter(&context, |_, _| None);
        let mut output = context.run_ui(Default::default(), |ui| {
            let (image, reused) = font_atlas_snapshot(ui.ctx());
            assert!(!reused);
            weak = Arc::downgrade(&image);
        });
        output.textures_delta.clear();
        install_root_terminal_painter(&context, |_, _| None);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn font_snapshot_refreshes_for_dpi_zoom_and_font_definition_changes() {
        let context = Context::default();
        let mut snapshots = Vec::new();
        for stage in 0..4 {
            match stage {
                1 => context.set_pixels_per_point(2.0),
                2 => context.set_zoom_factor(1.5),
                3 => {
                    let mut fonts = egui::FontDefinitions::default();
                    for names in fonts.families.values_mut() {
                        names.reverse();
                    }
                    context.set_fonts(fonts);
                }
                _ => {}
            }
            let mut output = context.run_ui(Default::default(), |ui| {
                let _ = ui.painter().layout_no_wrap(
                    "Atlas scale".to_owned(),
                    egui::FontId::monospace(17.0),
                    Color32::WHITE,
                );
                let (image, reused) = font_atlas_snapshot(ui.ctx());
                assert!(!reused, "stage {stage}");
                assert_eq!(image.as_ref(), &ui.ctx().fonts(|fonts| fonts.image()));
                if let Some(previous) = snapshots.last() {
                    assert!(!Arc::ptr_eq(previous, &image), "stage {stage}");
                }
                snapshots.push(image);
            });
            output.textures_delta.clear();
        }
    }

    #[test]
    fn font_capture_reports_reuse_without_default_timing() {
        let context = Context::default();
        let captures = Arc::new(Mutex::new(Vec::new()));
        let observed = captures.clone();
        install_root_terminal_painter(&context, move |_, frame| {
            observed.lock().unwrap().push(frame.font_atlas_capture);
            None
        });
        let mut output = context.run_ui(Default::default(), |ui| {
            for _ in 0..2 {
                let painter = ui.painter();
                let batch = Batch::begin(painter, ui.max_rect(), false).unwrap();
                painter.rect_filled(ui.max_rect(), 0.0, Color32::WHITE);
                batch.finish(painter);
            }
        });
        output.textures_delta.clear();
        let captures = captures.lock().unwrap();
        assert_eq!(captures.len(), 2);
        assert!(!captures[0].reused);
        assert_eq!(captures[0].cloned_bytes, captures[0].atlas_bytes);
        assert!(captures[1].reused);
        assert_eq!(captures[1].cloned_bytes, 0);
        assert_eq!(captures[1].atlas_bytes, captures[0].atlas_bytes);
        assert!(captures.iter().all(|capture| capture.elapsed.is_zero()));
    }

    #[test]
    fn empty_full_redraw_reaches_the_native_hook_without_inventing_pixels() {
        let context = Context::default();
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = called.clone();
        install_root_terminal_painter(&context, move |_, frame| {
            assert!(frame.full_redraw);
            assert!(frame.primitives.is_empty());
            observed.store(true, std::sync::atomic::Ordering::Relaxed);
            None
        });
        let mut output = context.run_ui(Default::default(), |ui| {
            let painter = ui.painter();
            Batch::begin(painter, ui.max_rect(), true)
                .unwrap()
                .finish(painter);
        });
        output.textures_delta.clear();
        assert!(called.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn empty_full_redraw_can_insert_the_native_callback() {
        let context = Context::default();
        install_root_terminal_painter(&context, |_, frame| {
            assert!(frame.full_redraw);
            assert!(frame.primitives.is_empty());
            Some(PaintCallback {
                rect: frame.rect,
                callback: Arc::new(()),
            })
        });
        let mut output = context.run_ui(Default::default(), |ui| {
            let painter = ui.painter();
            Batch::begin(painter, ui.max_rect(), true)
                .unwrap()
                .finish(painter);
        });
        assert!(matches!(
            output.shapes.last().map(|shape| &shape.shape),
            Some(Shape::Callback(_))
        ));
        output.textures_delta.clear();
    }

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
            let batch = Batch::begin(painter, ui.max_rect(), false).unwrap();
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
            let batch = Batch::begin(painter, rect, false).unwrap();
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
        assert!(Batch::begin(&painter, rect, false).is_none());
        painter.set_opacity(1.0);
        painter.set_invisible();
        assert!(Batch::begin(&painter, rect, false).is_none());
        remove_root_terminal_painter(&context);
        assert!(!installed(&context));
    }
}
