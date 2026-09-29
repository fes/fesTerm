use egui::{Context, Id, PaintCallback, Painter, Rect, Shape};

use crate::DEFAULT_BACKGROUND;
use std::sync::Arc;

type PanelFillPainter = dyn Fn(&egui::Ui, Rect, egui::Color32) -> Option<Shape> + Send + Sync;

#[derive(Clone)]
struct PanelFillCallback(Arc<PanelFillPainter>);

/// Installs an optional graphics-only painter for flat application panel fills.
/// Returning `None` retains ordinary egui geometry and painter behavior.
pub fn install_panel_fill_callback(
    context: &Context,
    callback: impl Fn(&egui::Ui, Rect, egui::Color32) -> Option<Shape> + Send + Sync + 'static,
) {
    context.data_mut(|data| {
        data.insert_temp(
            Id::new("festerm::panel-fill"),
            PanelFillCallback(Arc::new(callback)),
        );
    });
}

pub(crate) fn paint_panel_fill(ui: &egui::Ui, rect: Rect, fill: egui::Color32) {
    let callback = ui
        .ctx()
        .data(|data| data.get_temp::<PanelFillCallback>(Id::new("festerm::panel-fill")));
    let shape = callback
        .and_then(|callback| callback.0(ui, rect, fill))
        .unwrap_or_else(|| Shape::rect_filled(rect, 0.0, fill));
    ui.painter().add(shape);
}

/// Installs a native painter for the opaque default terminal background.
///
/// The callback must fill its rectangle with `theme::SURFACE_TERMINAL` and
/// honor egui's clipping. The UI retains its ordinary path without this hook
/// or when its painter is translucent.
pub fn install_terminal_background_callback(context: &Context, callback: PaintCallback) {
    context.data_mut(|data| data.insert_temp(callback_id(), callback));
}

fn callback_id() -> Id {
    Id::new("festerm::terminal-background")
}

pub(crate) fn background_shape(painter: &Painter, rect: Rect) -> Shape {
    if painter.opacity() == 1.0 && painter.is_visible() {
        if let Some(mut callback) = painter
            .ctx()
            .data(|data| data.get_temp::<PaintCallback>(callback_id()))
        {
            callback.rect = rect;
            return Shape::Callback(callback);
        }
    }
    Shape::rect_filled(rect, 0.0, DEFAULT_BACKGROUND)
}

pub(crate) fn paint_background(painter: &Painter, rect: Rect) {
    painter.add(background_shape(painter, rect));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn panel_fill_callback_preserves_declined_geometry_and_color() {
        let context = Context::default();
        let rect = Rect::from_min_size(egui::pos2(3.0, 5.0), egui::vec2(17.0, 19.0));
        let fill = egui::Color32::from_rgb(30, 60, 90);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for installed in [false, true] {
            if installed {
                let calls = Arc::clone(&calls);
                install_panel_fill_callback(&context, move |_, actual_rect, actual_fill| {
                    assert_eq!(actual_rect, rect);
                    assert_eq!(actual_fill, fill);
                    calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    None
                });
            }
            let mut output = context.run_ui(Default::default(), |ui| {
                paint_panel_fill(ui, rect, fill);
            });
            output.textures_delta.clear();
            assert!(output.shapes.iter().any(|shape| {
                matches!(&shape.shape, Shape::Rect(shape) if shape.rect == rect && shape.fill == fill)
            }));
        }
        assert!(calls.load(std::sync::atomic::Ordering::Relaxed) > 0);
    }

    #[test]
    fn native_background_hook_preserves_rectangles_and_translucent_fallback() {
        let context = Context::default();
        let rect = Rect::from_min_size(egui::pos2(3.0, 5.0), egui::vec2(17.0, 19.0));
        let mut painter = context.layer_painter(egui::LayerId::background());
        assert!(matches!(background_shape(&painter, rect), Shape::Rect(_)));
        install_terminal_background_callback(
            &context,
            PaintCallback {
                rect: Rect::NOTHING,
                callback: Arc::new(()),
            },
        );
        let Shape::Callback(callback) = background_shape(&painter, rect) else {
            panic!("native background callback missing");
        };
        assert_eq!(callback.rect, rect);
        painter.set_opacity(0.5);
        assert!(matches!(background_shape(&painter, rect), Shape::Rect(_)));
        painter.set_opacity(1.0);
        painter.set_invisible();
        assert!(matches!(background_shape(&painter, rect), Shape::Rect(_)));
    }
}
