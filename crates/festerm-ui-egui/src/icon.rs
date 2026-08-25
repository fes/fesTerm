//! Semantic first-party icon presentation.
//!
//! The canonical editable geometry remains under `assets/icons/source`. This
//! module is the Rust-facing, path-private presentation boundary promised by
//! `docs/icon-system.md`: callers name product concepts and supply semantic
//! color; they never depend on an asset filename or hard-coded asset color.

use egui::{pos2, Color32, Painter, Pos2, Rect, Stroke, StrokeKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Icon {
    AppMark,
    LocalTerminal,
    SshRemote,
    Serial,
    NewSession,
    Settings,
    SessionInspector,
    Search,
    CommandPalette,
    Overflow,
    Close,
    Minimize,
    Maximize,
    Restore,
    Reconnect,
    Disconnect,
    AuthRequired,
    HostKeyVerification,
    Warning,
    Error,
    Workspace,
    Profile,
    Copy,
    Paste,
    Clear,
    Diagnostics,
    KeyboardShortcuts,
    ThemeAppearance,
    TypographyFont,
    SecretStorage,
    Back,
    Edit,
    Activate,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Primitive {
    Polyline(&'static [(f32, f32)]),
    Rectangle {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: f32,
    },
    Circle {
        x: f32,
        y: f32,
        radius: f32,
    },
    FilledCircle {
        x: f32,
        y: f32,
        radius: f32,
    },
}

include!("icon_geometry.rs");

/// Paints canonical 24-unit icon geometry into any logical-pixel rectangle.
pub fn paint(painter: &Painter, icon: Icon, rect: Rect, color: Color32) {
    let g = Geometry::new(painter, rect, color);
    for primitive in icon_geometry(icon) {
        match *primitive {
            Primitive::Polyline(points) => g.poly(points),
            Primitive::Rectangle {
                x,
                y,
                width,
                height,
                radius,
            } => g.rect(x, y, width, height, radius),
            Primitive::Circle { x, y, radius } => g.circle(x, y, radius),
            Primitive::FilledCircle { x, y, radius } => {
                g.filled_circle(x, y, radius);
            }
        }
    }
}

struct Geometry<'a> {
    painter: &'a Painter,
    rect: Rect,
    stroke: Stroke,
}

impl<'a> Geometry<'a> {
    fn new(painter: &'a Painter, rect: Rect, color: Color32) -> Self {
        let scale = rect.width().min(rect.height()) / 24.0;
        Self {
            painter,
            rect,
            stroke: Stroke::new(1.75 * scale, color),
        }
    }
    fn point(&self, x: f32, y: f32) -> Pos2 {
        let side = self.rect.width().min(self.rect.height());
        let origin = self.rect.center() - egui::vec2(side, side) / 2.0;
        pos2(origin.x + x / 24.0 * side, origin.y + y / 24.0 * side)
    }
    fn poly(&self, points: &[(f32, f32)]) {
        self.painter.line(
            points.iter().map(|p| self.point(p.0, p.1)).collect(),
            self.stroke,
        );
    }
    fn rect(&self, x: f32, y: f32, w: f32, h: f32, radius: f32) {
        self.painter.rect_stroke(
            Rect::from_min_max(self.point(x, y), self.point(x + w, y + h)),
            radius * self.rect.width().min(self.rect.height()) / 24.0,
            self.stroke,
            StrokeKind::Inside,
        );
    }
    fn circle(&self, x: f32, y: f32, radius: f32) {
        self.painter.circle_stroke(
            self.point(x, y),
            radius * self.rect.width().min(self.rect.height()) / 24.0,
            self.stroke,
        );
    }
    fn filled_circle(&self, x: f32, y: f32, radius: f32) {
        self.painter.circle_filled(
            self.point(x, y),
            radius * self.rect.width().min(self.rect.height()) / 24.0,
            self.stroke.color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_inventory_has_one_variant_per_canonical_source() {
        let icons = [
            Icon::AppMark,
            Icon::LocalTerminal,
            Icon::SshRemote,
            Icon::Serial,
            Icon::NewSession,
            Icon::Settings,
            Icon::SessionInspector,
            Icon::Search,
            Icon::CommandPalette,
            Icon::Overflow,
            Icon::Close,
            Icon::Minimize,
            Icon::Maximize,
            Icon::Restore,
            Icon::Reconnect,
            Icon::Disconnect,
            Icon::AuthRequired,
            Icon::HostKeyVerification,
            Icon::Warning,
            Icon::Error,
            Icon::Workspace,
            Icon::Profile,
            Icon::Copy,
            Icon::Paste,
            Icon::Clear,
            Icon::Diagnostics,
            Icon::KeyboardShortcuts,
            Icon::ThemeAppearance,
            Icon::TypographyFont,
            Icon::SecretStorage,
            Icon::Back,
            Icon::Edit,
            Icon::Activate,
        ];
        assert_eq!(icons.len(), 33);
        let sources =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons/source");
        assert_eq!(std::fs::read_dir(sources).unwrap().count(), icons.len());
    }

    #[test]
    fn edit_geometry_preserves_the_closed_pencil_edge() {
        let Primitive::Polyline(points) = icon_geometry(Icon::Edit)[0] else {
            panic!("edit pencil body must be a polyline");
        };

        assert_eq!(points.first(), Some(&(4.0, 20.0)));
        assert_eq!(points.last(), Some(&(4.0, 20.0)));
        assert!(points
            .windows(2)
            .any(|edge| edge == [(8.0, 20.0), (4.0, 20.0)]));
    }
}
