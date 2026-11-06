//! Metadata-only desktop presentation trees. Runtime views remain owned by the app.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::hash::Hash;

pub const MAX_GROUP_PANES: usize = 4;
const RATIO_SCALE: u16 = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneAxis {
    X,
    Y,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaneSide {
    Left,
    Right,
    Above,
    Below,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PanePreset {
    Columns,
    Rows,
    Grid,
}

impl PaneSide {
    pub const fn axis(self) -> PaneAxis {
        match self {
            Self::Left | Self::Right => PaneAxis::X,
            Self::Above | Self::Below => PaneAxis::Y,
        }
    }

    pub const fn first(self) -> bool {
        matches!(self, Self::Left | Self::Above)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PaneTree<V> {
    Leaf {
        pane_id: String,
        view: V,
    },
    Split {
        axis: PaneAxis,
        /// First child's share after the one-physical-pixel divider.
        ratio: u16,
        first: Box<Self>,
        second: Box<Self>,
    },
}

impl<V: Clone + Eq + Hash> PaneTree<V> {
    pub fn views(&self) -> Vec<V> {
        match self {
            Self::Leaf { view, .. } => vec![view.clone()],
            Self::Split { first, second, .. } => {
                let mut views = first.views();
                views.extend(second.views());
                views
            }
        }
    }

    pub fn contains(&self, target: &V) -> bool {
        match self {
            Self::Leaf { view, .. } => view == target,
            Self::Split { first, second, .. } => first.contains(target) || second.contains(target),
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        fn walk<'a, V: Eq + Hash>(
            tree: &'a PaneTree<V>,
            depth: usize,
            views: &mut HashSet<&'a V>,
            panes: &mut HashSet<&'a str>,
        ) -> Result<(), &'static str> {
            if depth >= MAX_GROUP_PANES {
                return Err("Pane tree is too deep");
            }
            match tree {
                PaneTree::Leaf { pane_id, view } => {
                    if pane_id.is_empty()
                        || pane_id.len() > 64
                        || !pane_id
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
                        || !panes.insert(pane_id)
                        || !views.insert(view)
                        || views.len() > MAX_GROUP_PANES
                    {
                        return Err("Invalid, duplicated or excessive pane placement");
                    }
                }
                PaneTree::Split {
                    ratio,
                    first,
                    second,
                    ..
                } => {
                    if !(500..=9_500).contains(ratio) {
                        return Err("Pane ratio is out of bounds");
                    }
                    walk(first, depth + 1, views, panes)?;
                    walk(second, depth + 1, views, panes)?;
                }
            }
            Ok(())
        }
        walk(self, 0, &mut HashSet::new(), &mut HashSet::new())
    }

    pub fn split(&mut self, target: &V, leaf: Self, side: PaneSide) -> bool {
        match self {
            Self::Leaf { view, .. } if view == target => {
                let original = self.clone();
                let (first, second) = if side.first() {
                    (leaf, original)
                } else {
                    (original, leaf)
                };
                *self = Self::Split {
                    axis: side.axis(),
                    ratio: RATIO_SCALE / 2,
                    first: Box::new(first),
                    second: Box::new(second),
                };
                true
            }
            Self::Leaf { .. } => false,
            Self::Split { first, second, .. } => {
                if first.contains(target) {
                    first.split(target, leaf, side)
                } else {
                    second.split(target, leaf, side)
                }
            }
        }
    }

    pub fn remove(self, target: &V) -> Option<Self> {
        match self {
            Self::Leaf { ref view, .. } if view == target => None,
            Self::Leaf { .. } => Some(self),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.remove(target), second.remove(target)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    axis,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (remaining, None) | (None, remaining) => remaining,
            },
        }
    }

    pub fn leaf(&self, target: &V) -> Option<Self> {
        match self {
            Self::Leaf { view, .. } => (view == target).then(|| self.clone()),
            Self::Split { first, second, .. } => first.leaf(target).or_else(|| second.leaf(target)),
        }
    }

    pub fn preset(&self, preset: PanePreset) -> Result<Self, &'static str> {
        let leaves: Vec<_> = self
            .views()
            .iter()
            .map(|view| self.leaf(view).expect("tree view is a leaf"))
            .collect();
        let pair = |axis, first, second| Self::Split {
            axis,
            ratio: 5000,
            first: Box::new(first),
            second: Box::new(second),
        };
        match (preset,leaves.as_slice()) {
            (PanePreset::Columns,[first,second]) => Ok(pair(PaneAxis::X,first.clone(),second.clone())),
            (PanePreset::Rows,[first,second]) => Ok(pair(PaneAxis::Y,first.clone(),second.clone())),
            (PanePreset::Grid,[a,b,c,d]) => Ok(pair(PaneAxis::X,pair(PaneAxis::Y,a.clone(),b.clone()),pair(PaneAxis::Y,c.clone(),d.clone()))),
            _ => Err("Two-column/row presets need two views; the 2×2 preset needs four. Move existing views into the group first."),
        }
    }

    pub fn equalize(&mut self) {
        if let Self::Split {
            ratio,
            first,
            second,
            ..
        } = self
        {
            *ratio = RATIO_SCALE / 2;
            first.equalize();
            second.equalize();
        }
    }

    pub fn map<W>(&self, convert: &impl Fn(&V) -> Option<W>) -> Option<PaneTree<W>> {
        match self {
            Self::Leaf { pane_id, view } => Some(PaneTree::Leaf {
                pane_id: pane_id.clone(),
                view: convert(view)?,
            }),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.map(convert), second.map(convert)) {
                (Some(first), Some(second)) => Some(PaneTree::Split {
                    axis: *axis,
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (remaining, None) | (None, remaining) => remaining,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneGroup<V> {
    pub group_id: String,
    pub label: String,
    pub tree: PaneTree<V>,
    pub focused: V,
}

impl<V: Clone + Eq + Hash> PaneGroup<V> {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.group_id.is_empty()
            || self.group_id.len() > 64
            || !self
                .group_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || self.label.trim().is_empty()
            || self.label.len() > 128
            || self.label.chars().any(char::is_control)
            || !self.tree.contains(&self.focused)
        {
            return Err("Invalid pane group identity, label or focus");
        }
        self.tree.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(view: u32) -> PaneTree<u32> {
        PaneTree::Leaf {
            pane_id: format!("pane-{view}"),
            view,
        }
    }

    fn saved_group() -> PaneGroup<String> {
        PaneGroup {
            group_id: "group-project".into(),
            label: "Project".into(),
            tree: PaneTree::Split {
                axis: PaneAxis::X,
                ratio: 5000,
                first: Box::new(PaneTree::Leaf {
                    pane_id: "pane-one".into(),
                    view: "view-one".into(),
                }),
                second: Box::new(PaneTree::Leaf {
                    pane_id: "pane-two".into(),
                    view: "view-two".into(),
                }),
            },
            focused: "view-two".into(),
        }
    }

    #[test]
    fn pane_workspace_roundtrip_preserves_all_window_trees_and_rejects_foreign_views() {
        use crate::{
            Configuration, Profile, WorkspaceConfiguration, WorkspaceTab, WorkspaceWindow,
        };
        let primary = vec![
            WorkspaceTab::local_session("view-one", "shell").unwrap(),
            WorkspaceTab::local_session("view-two", "shell").unwrap(),
        ];
        let secondary = vec![
            WorkspaceTab::local_session("view-three", "shell").unwrap(),
            WorkspaceTab::local_session("view-four", "shell").unwrap(),
        ];
        let mut second_group = saved_group();
        second_group.group_id = "group-secondary".into();
        second_group.tree = saved_group()
            .tree
            .map(&|view| {
                Some(if view == "view-one" {
                    "view-three".to_owned()
                } else {
                    "view-four".to_owned()
                })
            })
            .unwrap();
        if let PaneTree::Split { first, second, .. } = &mut second_group.tree {
            if let PaneTree::Leaf { pane_id, .. } = first.as_mut() {
                *pane_id = "pane-three".into();
            }
            if let PaneTree::Leaf { pane_id, .. } = second.as_mut() {
                *pane_id = "pane-four".into();
            }
        }
        second_group.focused = "view-three".into();
        let window = WorkspaceWindow::new(secondary, None, None)
            .with_pane_groups(vec![second_group])
            .unwrap();
        let workspace =
            WorkspaceConfiguration::with_windows(primary, Some("view-two".into()), vec![window])
                .unwrap()
                .with_pane_groups(vec![saved_group()])
                .unwrap();
        let profile =
            Profile::local("shell", "controlled-nonexistent-child", Vec::new(), None).unwrap();
        let configuration = Configuration::new_with_workspace(vec![profile], workspace).unwrap();
        let serialized = configuration.to_toml().unwrap();
        assert_eq!(Configuration::parse(&serialized).unwrap(), configuration);
        assert!(!serialized.contains("lifecycle_generation"));
        let mut invalid = saved_group();
        invalid.focused = "view-three".into();
        assert!(configuration
            .workspace()
            .unwrap()
            .clone()
            .with_pane_groups(vec![invalid])
            .is_err());
        let mut invalid = saved_group();
        if let PaneTree::Split { second, .. } = &mut invalid.tree {
            if let PaneTree::Leaf { view, .. } = second.as_mut() {
                *view = "view-three".into();
            }
        }
        invalid.focused = "view-one".into();
        assert!(configuration
            .workspace()
            .unwrap()
            .clone()
            .with_pane_groups(vec![invalid])
            .is_err());
    }

    #[test]
    fn pane_tree_preserves_identity_and_collapses_without_orphans() {
        let mut tree = leaf(1);
        assert!(tree.split(&1, leaf(2), PaneSide::Right));
        assert!(tree.split(&1, leaf(3), PaneSide::Below));
        assert!(tree.split(&2, leaf(4), PaneSide::Below));
        assert_eq!(tree.validate(), Ok(()));
        assert_eq!(tree.views(), vec![1, 3, 2, 4]);
        let tree = tree
            .remove(&1)
            .unwrap()
            .remove(&2)
            .unwrap()
            .remove(&3)
            .unwrap();
        assert_eq!(tree, leaf(4));
        assert!(tree.remove(&4).is_none());
    }

    #[test]
    fn pane_tree_rejects_duplicate_capacity_depth_and_ratio() {
        let mut tree = leaf(1);
        tree.split(&1, leaf(1), PaneSide::Right);
        assert!(tree.validate().is_err());
        let mut tree = leaf(1);
        for id in 2..=5 {
            tree.split(&1, leaf(id), PaneSide::Below);
        }
        assert!(tree.validate().is_err());
        if let PaneTree::Split { ratio, .. } = &mut tree {
            *ratio = 0;
        }
        assert!(tree.validate().is_err());
    }

    #[test]
    fn pane_tree_randomized_edits_keep_unique_reachable_views() {
        let mut random = 0x1234_5678_u32;
        let mut tree = leaf(1);
        let mut next = 2;
        for _ in 0..2_000 {
            random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let views = tree.views();
            let target = views[random as usize % views.len()];
            if random & 1 == 0 && views.len() < MAX_GROUP_PANES {
                tree.split(
                    &target,
                    leaf(next),
                    if random & 2 == 0 {
                        PaneSide::Right
                    } else {
                        PaneSide::Below
                    },
                );
                next += 1;
            } else if views.len() > 1 {
                tree = tree.remove(&target).unwrap();
            }
            tree.validate().unwrap();
            assert_eq!(
                tree.views().len(),
                tree.views().into_iter().collect::<HashSet<_>>().len()
            );
        }
    }
}
