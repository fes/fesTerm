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
    /// The same three dots stacked vertically, for the per-row overflow
    /// control in a table whose rows are shorter than they are wide.
    OverflowVertical,
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
    MarkdownDocument,
    Outline,
    RenderedView,
    SourceView,
    ExternalLink,
    /// Chevron pointing up: "previous match" in the Markdown Find card.
    PreviousMatch,
    /// Chevron pointing down: "next match" in the Markdown Find card.
    NextMatch,
    /// Chevron pointing down, after the words on a button that opens a menu
    /// rather than acting on its own. Distinct from `NextMatch`, which is a
    /// navigation control that happens to share the shape.
    Disclosure,
    /// Re-read the current content in place. Distinct from `Reconnect`,
    /// which re-establishes a transport.
    Refresh,
    /// Navigate to the containing directory.
    ParentDirectory,
    /// Navigate to the account's home directory.
    HomeDirectory,
    /// The saved-profile collection's identity (a bookmark), distinct from
    /// `Profile`, which stands for one individual saved definition.
    SavedProfiles,
    /// The collection of locally running sessions available to reattach.
    RunningSessions,
    /// A file-transfer (SFTP) session or destination.
    FileTransfer,
    /// The remote/network badge composited over a session icon. `SshRemote`
    /// already includes this mark; painting `RemoteGlobe` over it in a second
    /// color is how a surface renders the badge as an accent without the
    /// asset layer owning two-tone art.
    RemoteGlobe,
    /// Proceed into the flow a launch card represents.
    Proceed,
    /// Create a new saved profile.
    NewProfile,
    /// Change the ordering of a list.
    SortOrder,
    /// Attach an already-running session to a tab.
    Reattach,
    /// A disclosure group whose contents are currently shown.
    SectionExpanded,
    /// A disclosure group whose contents are currently hidden.
    SectionCollapsed,
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
    let g = Geometry::new(painter, rect, color, icon_stroke_width(icon));
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
    fn new(painter: &'a Painter, rect: Rect, color: Color32, source_stroke_width: f32) -> Self {
        let scale = rect.width().min(rect.height()) / 24.0;
        Self {
            painter,
            rect,
            stroke: Stroke::new(source_stroke_width * scale, color),
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
            Icon::MarkdownDocument,
            Icon::Outline,
            Icon::RenderedView,
            Icon::SourceView,
            Icon::ExternalLink,
            Icon::PreviousMatch,
            Icon::NextMatch,
            Icon::Disclosure,
            Icon::Refresh,
            Icon::ParentDirectory,
            Icon::HomeDirectory,
            Icon::FileTransfer,
            Icon::NewProfile,
            Icon::OverflowVertical,
            Icon::Proceed,
            Icon::Reattach,
            Icon::RemoteGlobe,
            Icon::RunningSessions,
            Icon::SavedProfiles,
            Icon::SectionCollapsed,
            Icon::SectionExpanded,
            Icon::SortOrder,
        ];
        assert_eq!(icons.len(), 55);
        let sources =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons/source");
        assert_eq!(std::fs::read_dir(sources).unwrap().count(), icons.len());
    }

    #[test]
    fn source_strokes_scale_at_card_and_profile_row_sizes() {
        let ctx = egui::Context::default();
        let painter = ctx.layer_painter(egui::LayerId::background());
        for size in [16.0, 20.0, 28.0, 42.0, 50.0] {
            let rect = Rect::from_min_size(Pos2::ZERO, egui::Vec2::splat(size));
            for (icon, source_width) in [
                (Icon::SshRemote, 1.0),
                (Icon::RemoteGlobe, 1.0),
                (Icon::LocalTerminal, 1.25),
                (Icon::Serial, 1.25),
                (Icon::Search, 1.75),
            ] {
                let geometry =
                    Geometry::new(&painter, rect, Color32::WHITE, icon_stroke_width(icon));
                assert!((geometry.stroke.width - source_width * size / 24.0).abs() < 0.0001);
            }
        }
    }

    #[test]
    fn remote_globe_overlay_matches_the_complete_ssh_mark() {
        let globe = icon_geometry(Icon::RemoteGlobe);
        assert!(icon_geometry(Icon::SshRemote).ends_with(globe));
        assert_eq!(
            icon_stroke_width(Icon::SshRemote),
            icon_stroke_width(Icon::RemoteGlobe)
        );
        // Two parallels and one elliptical meridian, not an equatorial cross.
        assert_eq!(globe.len(), 4);
        let Primitive::Circle { radius, .. } = globe[0] else {
            panic!("the globe must have a circular outline");
        };
        assert_eq!(radius, 4.5);
        let Primitive::Polyline(meridian) = globe[3] else {
            panic!("the globe must have an elliptical meridian");
        };
        let min_x = meridian.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
        let max_x = meridian
            .iter()
            .map(|p| p.0)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!((max_x - min_x - 4.0).abs() < 0.01);
    }

    #[test]
    fn serial_mark_has_two_staggered_rows_of_nine_filled_pins() {
        let pins: Vec<_> = icon_geometry(Icon::Serial)
            .iter()
            .filter_map(|primitive| match primitive {
                Primitive::FilledCircle { x, y, radius } => Some((*x, *y, *radius)),
                _ => None,
            })
            .collect();
        assert_eq!(pins.len(), 9);
        assert_eq!(pins.iter().filter(|p| p.1 == 10.3).count(), 5);
        assert_eq!(pins.iter().filter(|p| p.1 == 13.7).count(), 4);
        assert!(pins.iter().all(|p| p.2 == 0.85));
        assert!(pins[5].0 > pins[0].0 && pins[8].0 < pins[4].0);
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
