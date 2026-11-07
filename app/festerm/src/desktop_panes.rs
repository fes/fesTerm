use eframe::egui::{self, Rect};
use festerm_config::{PaneAxis, PaneTree};
use std::hash::Hash;

pub(crate) struct DividerGeometry {
    pub(crate) rect: Rect,
    pub(crate) parent: Rect,
    pub(crate) axis: PaneAxis,
    pub(crate) path: Vec<bool>,
}

pub(crate) fn minimum_size<V>(tree: &PaneTree<V>, leaf: egui::Vec2, scale: f32) -> egui::Vec2 {
    match tree {
        PaneTree::Leaf { .. } => leaf,
        PaneTree::Split {
            axis,
            first,
            second,
            ..
        } => {
            let a = minimum_size(first, leaf, scale);
            let b = minimum_size(second, leaf, scale);
            match axis {
                PaneAxis::X => egui::vec2(a.x + b.x + 1.0 / scale, a.y.max(b.y)),
                PaneAxis::Y => egui::vec2(a.x.max(b.x), a.y + b.y + 1.0 / scale),
            }
        }
    }
}

pub(crate) fn fits<V>(
    tree: &PaneTree<V>,
    available: egui::Vec2,
    minimum: egui::Vec2,
    scale: f32,
) -> bool {
    fn walk<V>(tree: &PaneTree<V>, rect: Rect, minimum: egui::Vec2, scale: f32) -> bool {
        match tree {
            PaneTree::Leaf { .. } => rect.width() >= minimum.x && rect.height() >= minimum.y,
            PaneTree::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (a, b, _) = split_rect(rect, *axis, *ratio, scale);
                walk(first, a, minimum, scale) && walk(second, b, minimum, scale)
            }
        }
    }
    walk(
        tree,
        snap_rect(Rect::from_min_size(egui::Pos2::ZERO, available), scale),
        minimum,
        scale,
    )
}

pub(crate) fn geometry<V: Clone + Eq + Hash>(
    tree: &PaneTree<V>,
    rect: Rect,
    pixels_per_point: f32,
) -> Vec<(V, Rect)> {
    fn walk<V: Clone>(tree: &PaneTree<V>, rect: Rect, scale: f32, result: &mut Vec<(V, Rect)>) {
        match tree {
            PaneTree::Leaf { view, .. } => result.push((view.clone(), rect)),
            PaneTree::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (a, b, _) = split_rect(rect, *axis, *ratio, scale);
                walk(first, a, scale, result);
                walk(second, b, scale, result);
            }
        }
    }
    let rect = snap_rect(rect, pixels_per_point);
    let mut result = Vec::new();
    walk(tree, rect, pixels_per_point, &mut result);
    result
}

fn snap_rect(rect: Rect, scale: f32) -> Rect {
    let snap = |value: f32| (value * scale).round() / scale;
    Rect::from_min_max(
        egui::pos2(snap(rect.min.x), snap(rect.min.y)),
        egui::pos2(snap(rect.max.x), snap(rect.max.y)),
    )
}

fn split_rect(rect: Rect, axis: PaneAxis, ratio: u16, scale: f32) -> (Rect, Rect, Rect) {
    let mut first = rect;
    let mut second = rect;
    let mut divider = rect;
    let pixel = 1.0 / scale;
    let (min, length) = match axis {
        PaneAxis::X => (rect.min.x, rect.width()),
        PaneAxis::Y => (rect.min.y, rect.height()),
    };
    let boundary = ((min + (length - pixel) * f32::from(ratio) / 10_000.0) * scale).round() / scale;
    match axis {
        PaneAxis::X => {
            first.max.x = boundary;
            second.min.x = boundary + pixel;
            divider.min.x = boundary;
            divider.max.x = boundary + pixel;
        }
        PaneAxis::Y => {
            first.max.y = boundary;
            second.min.y = boundary + pixel;
            divider.min.y = boundary;
            divider.max.y = boundary + pixel;
        }
    }
    (first, second, divider)
}

pub(crate) fn dividers<V>(tree: &PaneTree<V>, rect: Rect, scale: f32) -> Vec<DividerGeometry> {
    fn walk<V>(
        tree: &PaneTree<V>,
        rect: Rect,
        scale: f32,
        path: Vec<bool>,
        result: &mut Vec<DividerGeometry>,
    ) {
        if let PaneTree::Split {
            axis,
            ratio,
            first,
            second,
        } = tree
        {
            let (a, b, line) = split_rect(rect, *axis, *ratio, scale);
            result.push(DividerGeometry {
                rect: line,
                parent: rect,
                axis: *axis,
                path: path.clone(),
            });
            let mut first_path = path.clone();
            first_path.push(false);
            let mut second_path = path;
            second_path.push(true);
            walk(first, a, scale, first_path, result);
            walk(second, b, scale, second_path, result);
        }
    }
    let mut result = Vec::new();
    walk(tree, snap_rect(rect, scale), scale, Vec::new(), &mut result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pane_dividers_are_one_physical_pixel_at_every_scale() {
        let tree = PaneTree::Split {
            axis: PaneAxis::X,
            ratio: 5000,
            first: Box::new(PaneTree::Leaf {
                pane_id: "left".into(),
                view: 1,
            }),
            second: Box::new(PaneTree::Leaf {
                pane_id: "right".into(),
                view: 2,
            }),
        };
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let rect = Rect::from_min_size(egui::pos2(0.3, 44.15), egui::vec2(1600.0, 960.0));
            let panes = geometry(&tree, rect, scale);
            assert!(((panes[1].1.min.x - panes[0].1.max.x) * scale - 1.0).abs() < 0.001);
            assert!((panes[0].1.max.x * scale).fract().abs() < 0.001);
            let lines = dividers(&tree, rect, scale);
            assert_eq!(lines[0].rect.min.x, panes[0].1.max.x);
            assert_eq!(lines[0].rect.max.x, panes[1].1.min.x);
            assert_eq!(lines[0].rect.min.y, panes[0].1.min.y);
            let leaf = egui::vec2(400.0, 240.0);
            let minimum = minimum_size(&tree, leaf, scale);
            assert!(
                fits(&tree, minimum, leaf, scale),
                "exact one-physical-pixel budget must fit at {scale}x"
            );
            assert!(
                !fits(&tree, minimum - egui::vec2(1.0 / scale, 0.0), leaf, scale),
                "a missing physical pixel must refuse at {scale}x"
            );
        }
    }
}
