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

/// Paints canonical 24-unit icon geometry into any logical-pixel rectangle.
pub fn paint(painter: &Painter, icon: Icon, rect: Rect, color: Color32) {
    let g = Geometry::new(painter, rect, color);
    match icon {
        Icon::AppMark => {
            g.poly(&[(4., 18.), (4., 6.), (11., 6.)]);
            g.line((4., 11.), (9., 11.));
            g.poly(&[(13., 8.), (16., 11.), (13., 14.)]);
            g.line((17.5, 16.), (21., 16.));
        }
        Icon::LocalTerminal | Icon::CommandPalette => {
            g.rect(3., 4., 18., 16., 2.);
            g.poly(&[(7., 9.), (10., 12.), (7., 15.)]);
            g.line((12.5, 15.), (17., 15.));
        }
        Icon::SshRemote => {
            g.rect(3., 4., 12., 12., 2.);
            g.poly(&[(6.5, 8.), (8.5, 10.), (6.5, 12.)]);
            g.line((10., 12.), (12., 12.));
            g.poly(&[(8., 19.), (17., 19.), (20., 16.), (20., 13.)]);
            g.poly(&[(17., 15.), (20., 12.), (22.5, 15.)]);
        }
        Icon::Serial => {
            g.poly(&[(6., 5.), (18., 5.), (20., 14.), (4., 14.), (6., 5.)]);
            for (x, y) in [(8., 9.), (12., 9.), (16., 9.), (10., 12.), (14., 12.)] {
                g.dot(x, y, 0.7);
            }
            g.poly(&[(9., 14.), (9., 16.), (12., 19.), (16., 19.)]);
        }
        Icon::NewSession => {
            g.rect(3., 5., 14., 14., 2.);
            g.poly(&[(6.5, 9.), (9., 11.5), (6.5, 14.)]);
            g.line((10.5, 14.), (13., 14.));
            g.line((19., 8.), (19., 14.));
            g.line((16., 11.), (22., 11.));
        }
        Icon::Settings => {
            for (y, knob) in [(7., 12.), (12., 16.), (17., 9.)] {
                g.line((4., y), (20., y));
                g.circle(knob, y, 2.);
            }
        }
        Icon::SessionInspector => {
            g.rect(3., 4., 18., 16., 2.);
            g.line((15., 4.), (15., 20.));
            g.line((6.5, 9.), (11.5, 9.));
            g.line((6.5, 13.), (11.5, 13.));
            g.line((6.5, 16.), (9.5, 16.));
            g.dot(18., 9., 0.75);
            g.line((18., 12.), (18., 16.));
        }
        Icon::Search => {
            g.circle(10.5, 10.5, 6.5);
            g.line((15.5, 15.5), (20., 20.));
        }
        Icon::Overflow => {
            for x in [6., 12., 18.] {
                g.dot(x, 12., 1.25);
            }
        }
        Icon::Close => {
            g.line((6., 6.), (18., 18.));
            g.line((18., 6.), (6., 18.));
        }
        Icon::Minimize => g.line((5., 17.), (19., 17.)),
        Icon::Maximize => g.rect(5., 5., 14., 14., 1.),
        Icon::Restore => {
            g.poly(&[(8., 8.), (8., 5.), (19., 5.), (19., 16.), (16., 16.)]);
            g.rect(5., 8., 11., 11., 1.);
        }
        Icon::Reconnect => {
            g.poly(&[(20., 7.), (20., 12.), (15., 12.)]);
            g.poly(&[(4., 17.), (4., 12.), (9., 12.)]);
            g.poly(&[(6.1, 8.5), (9., 5.5), (14., 5.), (18.5, 7.), (20., 12.)]);
            g.poly(&[(4., 12.), (5.5, 17.), (10., 19.), (15., 18.5), (17.9, 15.5)]);
        }
        Icon::Disconnect => {
            g.poly(&[
                (9.5, 14.5),
                (7., 17.),
                (4.5, 18.),
                (2., 15.5),
                (2., 12.),
                (5., 9.),
                (9.5, 8.6),
            ]);
            g.poly(&[
                (14.5, 9.5),
                (17., 7.),
                (19.5, 6.),
                (22., 8.5),
                (22., 12.),
                (19., 15.),
                (14.5, 15.4),
            ]);
            g.line((4., 4.), (20., 20.));
        }
        Icon::AuthRequired => {
            g.circle(8., 12., 4.);
            g.line((12., 12.), (21., 12.));
            g.line((17., 12.), (17., 15.));
            g.line((20., 12.), (20., 14.));
        }
        Icon::HostKeyVerification => {
            g.poly(&[
                (12., 3.),
                (5., 6.),
                (5., 11.),
                (7., 16.),
                (12., 21.),
                (17., 16.),
                (19., 11.),
                (19., 6.),
                (12., 3.),
            ]);
            g.circle(10., 11., 2.);
            g.line((12., 11.), (16., 11.));
            g.line((15., 11.), (15., 13.));
        }
        Icon::Warning => {
            g.poly(&[(12., 3.5), (2.7, 21.), (21.3, 21.), (12., 3.5)]);
            g.line((12., 9.), (12., 14.));
            g.dot(12., 17.5, 0.8);
        }
        Icon::Error => {
            g.circle(12., 12., 9.);
            g.line((8.5, 8.5), (15.5, 15.5));
            g.line((15.5, 8.5), (8.5, 15.5));
        }
        Icon::Workspace => {
            g.rect(3., 4., 8., 7., 1.);
            g.rect(13., 4., 8., 7., 1.);
            g.rect(3., 13., 18., 7., 1.);
        }
        Icon::Profile => {
            g.rect(3., 4., 18., 16., 2.);
            g.poly(&[(7., 9.), (9.5, 11.5), (7., 14.)]);
            g.line((11., 14.), (15., 14.));
            g.line((17., 8.), (17., 16.));
        }
        Icon::Copy => {
            g.rect(8., 8., 11., 12., 2.);
            g.poly(&[
                (8., 18.),
                (6., 18.),
                (4., 16.),
                (4., 6.),
                (6., 4.),
                (14., 4.),
                (16., 6.),
                (16., 8.),
            ]);
        }
        Icon::Paste => {
            g.poly(&[
                (9., 5.),
                (6., 5.),
                (4., 7.),
                (4., 19.),
                (6., 21.),
                (18., 21.),
                (20., 19.),
                (20., 7.),
                (18., 5.),
                (15., 5.),
            ]);
            g.rect(9., 3., 6., 4., 1.);
            g.line((8., 12.), (16., 12.));
            g.line((8., 16.), (14., 16.));
        }
        Icon::Clear => {
            g.poly(&[
                (4., 15.),
                (12., 5.),
                (20., 12.),
                (14., 20.),
                (8., 20.),
                (4., 15.),
            ]);
            g.line((9., 11.), (15., 16.));
            g.line((3., 20.), (21., 20.));
        }
        Icon::Diagnostics => {
            g.rect(3., 3., 18., 18., 2.);
            g.poly(&[
                (3., 12.),
                (7., 12.),
                (9., 7.),
                (13., 17.),
                (15., 12.),
                (21., 12.),
            ]);
        }
        Icon::KeyboardShortcuts => {
            g.rect(2.5, 5., 19., 14., 2.);
            for x in [6., 10., 14., 18.] {
                g.dot(x, 9., 0.65);
                g.dot(x, 13., 0.65);
            }
            g.line((8., 16.), (16., 16.));
        }
        Icon::ThemeAppearance => {
            g.circle(12., 12., 4.);
            for ((x1, y1), (x2, y2)) in [
                ((12., 2.), (12., 4.)),
                ((12., 20.), (12., 22.)),
                ((2., 12.), (4., 12.)),
                ((20., 12.), (22., 12.)),
                ((4.9, 4.9), (6.3, 6.3)),
                ((17.7, 17.7), (19.1, 19.1)),
                ((19.1, 4.9), (17.7, 6.3)),
                ((6.3, 17.7), (4.9, 19.1)),
            ] {
                g.line((x1, y1), (x2, y2));
            }
        }
        Icon::TypographyFont => {
            g.poly(&[(5., 19.), (11., 5.), (13., 5.), (19., 19.)]);
            g.line((7.2, 14.), (16.8, 14.));
            g.line((3., 5.), (15., 5.));
        }
        Icon::SecretStorage => {
            g.rect(4., 10., 16., 11., 2.);
            g.poly(&[
                (8., 10.),
                (8., 7.),
                (10., 3.),
                (14., 3.),
                (16., 7.),
                (16., 10.),
            ]);
            g.line((12., 14.), (12., 17.));
        }
        Icon::Back => {
            g.poly(&[(15., 5.), (8., 12.), (15., 19.)]);
        }
        Icon::Edit => {
            g.poly(&[(4., 20.), (4., 16.), (15., 5.), (19., 9.), (8., 20.)]);
            g.line((15., 5.), (19., 9.));
        }
        Icon::Activate => {
            g.poly(&[(9., 5.), (16., 12.), (9., 19.)]);
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
    fn line(&self, a: (f32, f32), b: (f32, f32)) {
        self.painter
            .line_segment([self.point(a.0, a.1), self.point(b.0, b.1)], self.stroke);
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
    fn dot(&self, x: f32, y: f32, radius: f32) {
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
}
