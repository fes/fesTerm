use egui::{Context, Id, PaintCallback, Painter, Rect, Shape};

use crate::DEFAULT_BACKGROUND;

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
