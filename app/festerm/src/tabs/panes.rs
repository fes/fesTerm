use super::{AppCommand, AppState, TabId};
use eframe::egui;
use festerm_config::{PaneGroup, PaneSide, PaneTree, MAX_GROUP_PANES};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneClosePlan {
    pub(crate) anchor: TabId,
    pub(crate) group_id: String,
    pub(crate) tree: PaneTree<TabId>,
    pub(crate) label: String,
    pub(crate) members: Vec<(TabId, Option<u64>)>,
    pub(crate) documents: Vec<PaneCloseDocument>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneCloseDocument {
    pub(crate) id: festerm_document::DocumentId,
    pub(crate) revision: u64,
    pub(crate) views: usize,
    pub(crate) title: String,
    pub(crate) origin: String,
}

impl AppState {
    pub(crate) fn pane_close_plan(&self, view: TabId) -> Option<PaneClosePlan> {
        let group = self.pane_group(view)?;
        let members: Vec<_> = group
            .tree
            .views()
            .into_iter()
            .map(|id| {
                (
                    id,
                    self.session_tab(id)
                        .map(|session| session.controller.lifecycle_generation()),
                )
            })
            .collect();
        let registry = self.documents.borrow();
        let mut documents = Vec::new();
        for (id, _) in &members {
            let tab = self.tabs.iter().find(|tab| tab.id == *id)?;
            let Some(document) = super::view_document(&tab.content) else {
                continue;
            };
            if documents
                .iter()
                .any(|entry: &PaneCloseDocument| entry.id == document)
            {
                continue;
            }
            let open = registry.get(document)?;
            let closing_views = self
                .tabs
                .iter()
                .filter(|tab| {
                    members.iter().any(|(id, _)| *id == tab.id)
                        && super::view_document(&tab.content) == Some(document)
                })
                .count();
            if open.text().is_dirty() && closing_views >= open.views() {
                documents.push(PaneCloseDocument {
                    id: document,
                    revision: open.text().revision(),
                    views: open.views(),
                    title: open.origin().file_name().to_owned(),
                    origin: open.origin().qualified_label(),
                });
            }
        }
        Some(PaneClosePlan {
            anchor: view,
            group_id: group.group_id.clone(),
            tree: group.tree.clone(),
            label: group.label.clone(),
            members,
            documents,
        })
    }

    pub(crate) fn saved_pane_groups(
        &self,
        saved: &[festerm_config::WorkspaceTab],
    ) -> Vec<PaneGroup<String>> {
        self.pane_groups
            .iter()
            .filter_map(|group| {
                let convert = |view: &TabId| {
                    let id = &self.tabs.iter().find(|t| t.id == *view)?.durable_id;
                    saved
                        .iter()
                        .any(|tab| tab.identifier() == id)
                        .then(|| id.clone())
                };
                let tree = group.tree.map(&convert)?;
                if tree.views().len() < 2 {
                    return None;
                }
                let focused = convert(&group.focused)
                    .filter(|view| tree.contains(view))
                    .unwrap_or_else(|| tree.views()[0].clone());
                Some(PaneGroup {
                    group_id: group.group_id.clone(),
                    label: group.label.clone(),
                    tree,
                    focused,
                })
            })
            .collect()
    }

    pub(crate) fn restore_pane_groups(&mut self, saved: &[PaneGroup<String>]) {
        self.pane_groups = saved
            .iter()
            .map(|group| {
                let convert = |id: &String| {
                    self.tabs
                        .iter()
                        .find(|tab| &tab.durable_id == id)
                        .map(|tab| tab.id)
                };
                let tree = group
                    .tree
                    .map(&convert)
                    .expect("validated workspace group references");
                let focused = convert(&group.focused).expect("validated workspace group focus");
                PaneGroup {
                    group_id: group.group_id.clone(),
                    label: group.label.clone(),
                    tree,
                    focused,
                }
            })
            .collect();
        for group in &self.pane_groups {
            for view in group.tree.views() {
                if let Some(PaneTree::Leaf { pane_id, .. }) = group.tree.leaf(&view) {
                    if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == view) {
                        tab.pane_id = pane_id;
                    }
                }
            }
        }
        self.reconcile_pane_chips();
        self.focus_pane(self.active);
    }
    pub(crate) fn surface_tab_mut(&mut self, view: TabId) -> Option<&mut super::Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == view)
    }
    pub(crate) fn pane_group(&self, view: TabId) -> Option<&PaneGroup<TabId>> {
        self.pane_groups
            .iter()
            .find(|group| group.tree.contains(&view))
    }

    pub(crate) fn pane_revision(&self) -> u64 {
        self.pane_revision
    }

    pub(crate) fn pane_geometry(&mut self, available: egui::Vec2, minimum: egui::Vec2) {
        self.pane_available = available;
        self.pane_minimum = minimum;
    }

    pub(crate) fn pane_display_scale(&mut self, scale: f32) {
        self.pane_scale = scale;
    }

    pub(crate) fn take_pane_error(&mut self) -> Option<String> {
        self.pane_error.take()
    }

    pub(crate) fn pane_zoom(&self) -> Option<TabId> {
        self.pane_zoom
    }

    pub(crate) fn presentation_tabs(&self) -> Vec<TabId> {
        self.tabs
            .iter()
            .filter_map(|tab| {
                self.pane_group(tab.id).map_or(Some(tab.id), |group| {
                    (group.tree.views().first() == Some(&tab.id)).then_some(tab.id)
                })
            })
            .collect()
    }

    pub(crate) fn pane_members(&self, view: TabId) -> Vec<TabId> {
        self.pane_group(view)
            .map_or_else(|| vec![view], |group| group.tree.views())
    }

    pub(crate) fn composable(&self, view: TabId) -> bool {
        self.tabs
            .iter()
            .any(|tab| tab.id == view && tab.content.movable_across_windows())
    }

    fn reconcile_pane_chips(&mut self) {
        self.pane_chip_ids
            .retain(|id, _| self.pane_groups.iter().any(|group| &group.group_id == id));
        for group in &self.pane_groups {
            self.pane_chip_ids
                .entry(group.group_id.clone())
                .or_insert_with(TabId::next);
        }
    }

    pub(crate) fn presentation_chip(&self, view: TabId) -> TabId {
        self.pane_group(view)
            .and_then(|group| self.pane_chip_ids.get(&group.group_id))
            .copied()
            .unwrap_or(view)
    }

    pub(crate) fn pane_view_for_chip(&self, chip: u64) -> Option<TabId> {
        self.pane_groups
            .iter()
            .find(|group| {
                self.pane_chip_ids
                    .get(&group.group_id)
                    .is_some_and(|id| id.chip_id() == chip)
            })
            .map(|group| group.focused)
    }

    fn pane_changed(&mut self) {
        self.reconcile_pane_chips();
        self.pane_revision = self.pane_revision.wrapping_add(1);
        self.pane_zoom = None;
        self.workspace_dirty = true;
        if let Some(revision) = &self.presentation_workspace {
            revision.changed();
        }
        self.input_ownership_epoch = self.input_ownership_epoch.wrapping_add(1);
    }

    pub(super) fn remove_pane(&mut self, view: TabId) {
        let Some(index) = self.pane_groups.iter().position(|g| g.tree.contains(&view)) else {
            return;
        };
        let mut group = self.pane_groups.remove(index);
        if let Some(tree) = group.tree.remove(&view) {
            let members = tree.views();
            if members.len() > 1 {
                group.tree = tree;
                if group.focused == view {
                    group.focused = members[0];
                }
                self.pane_groups.insert(index, group);
            }
        }
        self.pane_changed();
    }

    pub(super) fn focus_pane(&mut self, view: TabId) {
        for group in &mut self.pane_groups {
            if group.tree.contains(&view) {
                group.focused = view;
            }
        }
        if let Some(zoomed) = self.pane_zoom {
            self.pane_zoom = self
                .pane_group(view)
                .filter(|group| group.tree.contains(&zoomed))
                .map(|_| view);
        }
    }

    pub(crate) fn grouped_view_for_chip(&self, view: TabId) -> TabId {
        self.pane_group(view).map_or(view, |group| group.focused)
    }

    pub(crate) fn planned_pane_split(
        &self,
        source: TabId,
        target: TabId,
        side: PaneSide,
    ) -> Result<PaneGroup<TabId>, String> {
        if source == target || !self.composable(source) || !self.composable(target) {
            return Err("Choose two different session or document views.".into());
        }
        let leaf = self
            .pane_group(source)
            .and_then(|g| g.tree.leaf(&source))
            .unwrap_or_else(|| PaneTree::Leaf {
                pane_id: self
                    .tabs
                    .iter()
                    .find(|tab| tab.id == source)
                    .expect("owned source")
                    .pane_id
                    .clone(),
                view: source,
            });
        let mut destination = self
            .pane_group(target)
            .cloned()
            .unwrap_or_else(|| PaneGroup {
                group_id: uuid::Uuid::new_v4().to_string(),
                label: "Project".into(),
                tree: PaneTree::Leaf {
                    pane_id: self
                        .tabs
                        .iter()
                        .find(|tab| tab.id == target)
                        .expect("owned target")
                        .pane_id
                        .clone(),
                    view: target,
                },
                focused: target,
            });
        let same_group = destination.tree.contains(&source);
        if !same_group && destination.tree.views().len() >= MAX_GROUP_PANES {
            return Err(
                "A group can contain at most four panes. Move a pane to its own tab first.".into(),
            );
        }
        if same_group {
            destination.tree = destination
                .tree
                .remove(&source)
                .ok_or("Source no longer exists")?;
        }
        if !destination.tree.split(&target, leaf, side) {
            return Err("The destination pane no longer exists.".into());
        }
        destination.focused = source;
        destination.validate().map_err(str::to_owned)?;
        let minimum = crate::desktop_panes::minimum_size(
            &destination.tree,
            self.pane_minimum,
            self.pane_scale,
        );
        if self.pane_available.x < minimum.x
            || self.pane_available.y < minimum.y
            || !crate::desktop_panes::fits(
                &destination.tree,
                self.pane_available,
                self.pane_minimum,
                self.pane_scale,
            )
        {
            return Err("Not enough space for this split (at least 52 columns × 12 rows per terminal). Enlarge the window or use a new tab.".into());
        }
        Ok(destination)
    }

    fn combine_pane(&mut self, source: TabId, target: TabId, side: PaneSide) -> Result<(), String> {
        let destination = self.planned_pane_split(source, target, side)?;
        let same_group = self
            .pane_group(source)
            .is_some_and(|group| group.group_id == destination.group_id);
        let mut groups = self.pane_groups.clone();
        groups.retain(|group| group.group_id != destination.group_id);
        if !same_group {
            if let Some(index) = groups.iter().position(|g| g.tree.contains(&source)) {
                let mut previous = groups.remove(index);
                if let Some(tree) = previous.tree.remove(&source) {
                    if tree.views().len() > 1 {
                        if previous.focused == source {
                            previous.focused = tree.views()[0];
                        }
                        previous.tree = tree;
                        groups.insert(index, previous);
                    }
                }
            }
        }
        groups.push(destination);
        let members = groups
            .last()
            .expect("destination group was appended")
            .tree
            .views();
        let destination_position = self
            .tabs
            .iter()
            .position(|tab| tab.id == target)
            .expect("destination is an owned view");
        let insertion = self.tabs[..destination_position]
            .iter()
            .filter(|tab| !members.contains(&tab.id))
            .count();
        let mut moved = Vec::new();
        for member in &members {
            let index = self
                .tabs
                .iter()
                .position(|tab| tab.id == *member)
                .expect("validated group contains owned views");
            moved.push(self.tabs.remove(index));
        }
        self.tabs.splice(insertion..insertion, moved);
        self.pane_groups = groups;
        self.pane_changed();
        self.set_active(source);
        Ok(())
    }

    pub(super) fn dispatch_pane_command(&mut self, command: AppCommand) {
        let result = match command {
            AppCommand::FocusPaneDirection(side) => {
                if let Some(group) = self.pane_group(self.active) {
                    let rectangles = crate::desktop_panes::geometry(
                        &group.tree,
                        egui::Rect::from_min_size(egui::Pos2::ZERO, self.pane_available),
                        1.0,
                    );
                    let center = rectangles
                        .iter()
                        .find(|(view, _)| *view == self.active)
                        .expect("focused member has geometry")
                        .1
                        .center();
                    let neighbor = rectangles
                        .iter()
                        .filter_map(|(view, rect)| {
                            if *view == self.active {
                                return None;
                            }
                            let delta = rect.center() - center;
                            let (along, across) = match side {
                                PaneSide::Left => (-delta.x, delta.y.abs()),
                                PaneSide::Right => (delta.x, delta.y.abs()),
                                PaneSide::Above => (-delta.y, delta.x.abs()),
                                PaneSide::Below => (delta.y, delta.x.abs()),
                            };
                            (along > 0.0).then_some((*view, along + across * 2.0))
                        })
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .map(|(view, _)| view);
                    if let Some(view) = neighbor {
                        self.set_active(view);
                    }
                }
                Ok(())
            }
            AppCommand::ResizeFocusedPane { axis, increase } => {
                fn nearest(
                    tree: &PaneTree<TabId>,
                    view: TabId,
                    axis: festerm_config::PaneAxis,
                    path: Vec<bool>,
                ) -> Option<(Vec<bool>, u16, bool)> {
                    let PaneTree::Split {
                        axis: split_axis,
                        ratio,
                        first,
                        second,
                    } = tree
                    else {
                        return None;
                    };
                    let in_second = second.contains(&view);
                    let child = if in_second { second } else { first };
                    let mut child_path = path.clone();
                    child_path.push(in_second);
                    nearest(child, view, axis, child_path)
                        .or_else(|| (*split_axis == axis).then_some((path, *ratio, in_second)))
                }
                let target = self
                    .pane_group(self.active)
                    .and_then(|group| nearest(&group.tree, self.active, axis, Vec::new()));
                if let Some((path, ratio, in_second)) = target {
                    let grow_first = increase != in_second;
                    let ratio = if grow_first {
                        ratio.saturating_add(500).min(9500)
                    } else {
                        ratio.saturating_sub(500).max(500)
                    };
                    return self.dispatch_pane_command(AppCommand::ResizePaneSplit {
                        view: self.active,
                        path,
                        ratio,
                        expected_revision: self.pane_revision,
                    });
                }
                Err("There is no divider on that axis for the focused pane.".into())
            }
            AppCommand::ClosePaneGroup { plan, discard } => {
                if self.pane_close_plan(plan.anchor).as_ref() != Some(&plan) {
                    Err("The group, session or document changed. Review Close group again.".into())
                } else if plan
                    .documents
                    .iter()
                    .any(|document| !discard.contains(&document.id))
                {
                    Err(
                        "Save the unsaved documents or explicitly approve each discard first."
                            .into(),
                    )
                } else {
                    // Preflight is complete before the first irreversible
                    // close. Existing close owns each surface's shutdown.
                    for (id, _) in plan.members {
                        self.close(id);
                    }
                    Ok(())
                }
            }
            AppCommand::SetPanePreset { view, preset } => {
                if let Some(index) = self.pane_groups.iter().position(|g| g.tree.contains(&view)) {
                    match self.pane_groups[index].tree.preset(preset) {
                        Ok(tree) if crate::desktop_panes::fits(&tree,self.pane_available,self.pane_minimum,self.pane_scale) => {
                            self.pane_groups[index].tree = tree;
                            self.pane_changed();
                            Ok(())
                        }
                        Ok(_) => Err("Not enough space for this preset. Enlarge the window or use separate tabs.".into()),
                        Err(error) => Err(error.into()),
                    }
                } else {
                    Err("Move existing views into a group first.".into())
                }
            }
            AppCommand::SwapPanes {
                first,
                second,
                expected_revision,
            } => {
                if expected_revision != self.pane_revision {
                    Err("The layout changed. Select the panes again.".into())
                } else if let Some(index) = self
                    .pane_groups
                    .iter()
                    .position(|g| g.tree.contains(&first) && g.tree.contains(&second))
                {
                    let mut group = self.pane_groups[index].clone();
                    let a = group.tree.leaf(&first).expect("source exists");
                    let b = group.tree.leaf(&second).expect("destination exists");
                    // A temporary view-only marker is unnecessary: replace
                    // both leaves in one traversal of the original tree.
                    fn swap(
                        tree: &mut PaneTree<TabId>,
                        first: TabId,
                        second: TabId,
                        a: &PaneTree<TabId>,
                        b: &PaneTree<TabId>,
                    ) {
                        match tree {
                            PaneTree::Leaf { view, .. } if *view == first => *tree = b.clone(),
                            PaneTree::Leaf { view, .. } if *view == second => *tree = a.clone(),
                            PaneTree::Split {
                                first: left,
                                second: right,
                                ..
                            } => {
                                swap(left, first, second, a, b);
                                swap(right, first, second, a, b);
                            }
                            _ => {}
                        }
                    }
                    swap(&mut group.tree, first, second, &a, &b);
                    if let Err(error) = group.validate() {
                        Err(error.into())
                    } else {
                        self.pane_groups[index] = group;
                        self.pane_changed();
                        Ok(())
                    }
                } else {
                    Err("Select two members of the same pane group.".into())
                }
            }
            AppCommand::CombinePanes {
                source,
                target,
                side,
                expected_revision,
            } => {
                if expected_revision != self.pane_revision {
                    Err("The layout changed. Select the destination again.".into())
                } else {
                    self.combine_pane(source, target, side)
                }
            }
            AppCommand::SeparatePane(view) => {
                if !self.composable(view) {
                    Err("The view no longer exists.".into())
                } else {
                    self.remove_pane(view);
                    self.set_active(view);
                    Ok(())
                }
            }
            AppCommand::SeparatePaneGroup(view) => {
                if let Some(index) = self.pane_groups.iter().position(|g| g.tree.contains(&view)) {
                    let group = self.pane_groups.remove(index);
                    let members = group.tree.views();
                    let position = self
                        .tabs
                        .iter()
                        .position(|t| t.id == members[0])
                        .unwrap_or(0);
                    let mut moved = Vec::new();
                    for member in members {
                        if let Some(index) = self.tabs.iter().position(|t| t.id == member) {
                            moved.push(self.tabs.remove(index));
                        }
                    }
                    let position = position.min(self.tabs.len());
                    self.tabs.splice(position..position, moved);
                    self.pane_changed();
                    self.set_active(group.focused);
                    Ok(())
                } else {
                    Err("The group no longer exists.".into())
                }
            }
            AppCommand::EqualizePanes(view) => {
                if let Some(group) = self.pane_groups.iter_mut().find(|g| g.tree.contains(&view)) {
                    let mut tree = group.tree.clone();
                    tree.equalize();
                    if crate::desktop_panes::fits(
                        &tree,
                        self.pane_available,
                        self.pane_minimum,
                        self.pane_scale,
                    ) {
                        group.tree = tree;
                        self.pane_changed();
                        Ok(())
                    } else {
                        Err("Equalizing would violate the minimum pane size. Enlarge the window first.".into())
                    }
                } else {
                    Err("The group no longer exists.".into())
                }
            }
            AppCommand::ZoomPane(view) => {
                if self.pane_group(view).is_some() {
                    self.pane_zoom = if self.pane_zoom == Some(view) {
                        None
                    } else {
                        Some(view)
                    };
                    self.set_active(view);
                    Ok(())
                } else {
                    Err("Select a grouped pane first.".into())
                }
            }
            AppCommand::FocusNextPane { reverse } => {
                let members = self.pane_members(self.active);
                let index = members.iter().position(|v| *v == self.active).unwrap_or(0);
                let next = if reverse {
                    (index + members.len() - 1) % members.len()
                } else {
                    (index + 1) % members.len()
                };
                self.set_active(members[next]);
                self.workspace_dirty = true;
                Ok(())
            }
            AppCommand::RenamePaneGroup { view, label } => {
                if let Some(group) = self.pane_groups.iter_mut().find(|g| g.tree.contains(&view)) {
                    let mut proposed = group.clone();
                    proposed.label = label.trim().to_owned();
                    if let Err(error) = proposed.validate() {
                        Err(error.into())
                    } else {
                        *group = proposed;
                        self.pane_changed();
                        Ok(())
                    }
                } else {
                    Err("The group no longer exists.".into())
                }
            }
            AppCommand::ResizePaneSplit {
                view,
                path,
                ratio,
                expected_revision,
            } => {
                if self.pane_revision != expected_revision {
                    Err("The layout changed during resize.".into())
                } else if let Some(index) =
                    self.pane_groups.iter().position(|g| g.tree.contains(&view))
                {
                    let mut proposed = self.pane_groups[index].clone();
                    let mut node = &mut proposed.tree;
                    let mut valid = path.len() < MAX_GROUP_PANES;
                    for second in path {
                        if let PaneTree::Split {
                            first,
                            second: last,
                            ..
                        } = node
                        {
                            node = if second { last } else { first };
                        } else {
                            valid = false;
                            break;
                        }
                    }
                    if valid {
                        if let PaneTree::Split { ratio: current, .. } = node {
                            *current = ratio;
                        } else {
                            valid = false;
                        }
                    }
                    if !valid
                        || proposed.validate().is_err()
                        || !crate::desktop_panes::fits(
                            &proposed.tree,
                            self.pane_available,
                            self.pane_minimum,
                            self.pane_scale,
                        )
                    {
                        Err("The divider cannot move beyond the minimum pane size.".into())
                    } else {
                        self.pane_groups[index] = proposed;
                        self.pane_changed();
                        Ok(())
                    }
                } else {
                    Err("The group no longer exists.".into())
                }
            }
            _ => unreachable!("only pane commands enter this reducer"),
        };
        if let Err(error) = result {
            self.pane_error = Some(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_config::Configuration;

    fn state() -> (AppState, egui::Context, Vec<TabId>) {
        let context = egui::Context::default();
        let mut state = AppState::with_launcher(Configuration::empty());
        let mut ids = Vec::new();
        for _ in 0..5 {
            state.dispatch(AppCommand::NewTextDocument, &context);
            ids.push(state.active());
        }
        state.pane_geometry(egui::vec2(8000.0, 1400.0), egui::vec2(520.0, 240.0));
        (state, context, ids)
    }

    #[test]
    fn panes_equalize_refuses_without_changing_a_minimum_constrained_tree() {
        let (mut state, context, ids) = state();
        for (source, target) in [(ids[1], ids[0]), (ids[2], ids[1])] {
            state.dispatch(
                AppCommand::CombinePanes {
                    source,
                    target,
                    side: PaneSide::Right,
                    expected_revision: state.pane_revision(),
                },
                &context,
            );
        }
        state.dispatch(
            AppCommand::ResizePaneSplit {
                view: ids[0],
                path: Vec::new(),
                ratio: 3333,
                expected_revision: state.pane_revision(),
            },
            &context,
        );
        state.pane_geometry(egui::vec2(1562.0, 240.0), egui::vec2(520.0, 240.0));
        let before = state.pane_group(ids[0]).unwrap().tree.clone();
        assert!(crate::desktop_panes::fits(
            &before,
            state.pane_available,
            state.pane_minimum,
            state.pane_scale
        ));
        state.dispatch(AppCommand::EqualizePanes(ids[0]), &context);
        assert!(state.take_pane_error().is_some());
        assert_eq!(state.pane_group(ids[0]).unwrap().tree, before);
    }

    #[test]
    fn panes_zoom_and_close_preflight_do_not_leak_between_groups() {
        let (mut state, context, ids) = state();
        for (source, target) in [(ids[1], ids[0]), (ids[3], ids[2])] {
            state.dispatch(
                AppCommand::CombinePanes {
                    source,
                    target,
                    side: PaneSide::Right,
                    expected_revision: state.pane_revision(),
                },
                &context,
            );
        }
        state.dispatch(AppCommand::ActivateTab(ids[0]), &context);
        state.dispatch(AppCommand::ZoomPane(ids[0]), &context);
        assert_eq!(state.pane_zoom(), Some(ids[0]));
        state.dispatch(AppCommand::ActivateTab(ids[2]), &context);
        assert_eq!(
            state.pane_zoom(),
            None,
            "zoom cannot collapse an unrelated group"
        );
        let plan = state.pane_close_plan(ids[0]).unwrap();
        state.dispatch(
            AppCommand::RenamePaneGroup {
                view: ids[2],
                label: "Unrelated".into(),
            },
            &context,
        );
        assert_eq!(
            state.pane_close_plan(ids[0]),
            Some(plan.clone()),
            "unrelated edits do not change this group's close preflight"
        );
        let discard = plan.documents.iter().map(|document| document.id).collect();
        state.dispatch(AppCommand::ClosePaneGroup { plan, discard }, &context);
        assert!(state.take_pane_error().is_none());
        assert!(state.pane_group(ids[2]).is_some());
        assert!(state.pane_group(ids[0]).is_none());
    }

    #[test]
    fn panes_presets_swap_direction_and_keyboard_resize_preserve_views() {
        let (mut state, context, ids) = state();
        for (source, target, side) in [
            (ids[1], ids[0], PaneSide::Right),
            (ids[2], ids[0], PaneSide::Below),
            (ids[3], ids[1], PaneSide::Below),
        ] {
            state.dispatch(
                AppCommand::CombinePanes {
                    source,
                    target,
                    side,
                    expected_revision: state.pane_revision(),
                },
                &context,
            );
            assert!(state.take_pane_error().is_none());
        }
        let before: std::collections::HashSet<_> = state.pane_members(ids[0]).into_iter().collect();
        state.dispatch(
            AppCommand::SetPanePreset {
                view: ids[0],
                preset: festerm_config::PanePreset::Grid,
            },
            &context,
        );
        assert!(state.take_pane_error().is_none());
        state.dispatch(AppCommand::ActivateTab(ids[0]), &context);
        state.dispatch(AppCommand::FocusPaneDirection(PaneSide::Below), &context);
        assert_eq!(state.active(), ids[2]);
        let old_tree = state.pane_group(ids[0]).unwrap().tree.clone();
        state.dispatch(
            AppCommand::ResizeFocusedPane {
                axis: festerm_config::PaneAxis::Y,
                increase: true,
            },
            &context,
        );
        assert!(state.take_pane_error().is_none());
        assert_ne!(state.pane_group(ids[0]).unwrap().tree, old_tree);
        let a = state
            .pane_group(ids[0])
            .unwrap()
            .tree
            .leaf(&ids[0])
            .unwrap();
        state.dispatch(
            AppCommand::SwapPanes {
                first: ids[0],
                second: ids[3],
                expected_revision: state.pane_revision(),
            },
            &context,
        );
        assert!(state.take_pane_error().is_none());
        assert_eq!(
            state.pane_group(ids[0]).unwrap().tree.leaf(&ids[0]),
            Some(a)
        );
        assert_eq!(
            state
                .pane_members(ids[0])
                .into_iter()
                .collect::<std::collections::HashSet<_>>(),
            before
        );
        assert_eq!(state.documents.borrow().len(), 5);
        let chip = state.presentation_chip(ids[0]);
        state.dispatch(AppCommand::CloseTab(ids[0]), &context);
        assert_eq!(
            state.presentation_chip(ids[3]),
            chip,
            "closing the first leaf must not replace the group chip"
        );
    }

    #[test]
    fn panes_group_close_preflights_collective_final_dirty_views_across_windows() {
        let (mut state, context, ids) = state();
        state.set_active(ids[0]);
        let document = state.active_document().unwrap();
        let documents = state.documents.clone();
        if let super::super::TabContent::TextEditor(editor) = &mut state.active_tab_mut().content {
            editor.type_for_gallery(&documents, "unsaved");
        }
        state.documents.borrow_mut().retain(document);
        state.open_text_document(document);
        let second = state.active();
        state.dispatch(
            AppCommand::CombinePanes {
                source: second,
                target: ids[0],
                side: PaneSide::Right,
                expected_revision: state.pane_revision(),
            },
            &context,
        );
        let plan = state.pane_close_plan(ids[0]).unwrap();
        assert_eq!(plan.documents.len(), 1);
        assert_eq!(plan.documents[0].views, 2);
        assert!(state.document_close_consequence(ids[0]).is_none());
        let original = state.pane_groups.clone();
        state.dispatch(
            AppCommand::ClosePaneGroup {
                plan: plan.clone(),
                discard: Vec::new(),
            },
            &context,
        );
        assert!(state.take_pane_error().is_some());
        assert_eq!(state.pane_groups, original);
        let mut sibling = AppState::with_launcher(Configuration::empty());
        sibling.adopt_documents(documents.clone());
        documents.borrow_mut().retain(document);
        sibling.open_text_document(document);
        state.dispatch(
            AppCommand::ClosePaneGroup {
                plan,
                discard: vec![document],
            },
            &context,
        );
        assert!(
            state.take_pane_error().is_some(),
            "global view-count changes invalidate the old preflight"
        );
        assert_eq!(state.pane_groups, original);
        let plan = state.pane_close_plan(ids[0]).unwrap();
        assert!(
            plan.documents.is_empty(),
            "another window retains the dirty buffer"
        );
        state.dispatch(
            AppCommand::ClosePaneGroup {
                plan,
                discard: Vec::new(),
            },
            &context,
        );
        assert!(state.take_pane_error().is_none());
        assert!(state.pane_group(ids[0]).is_none());
        assert!(documents.borrow().get(document).unwrap().text().is_dirty());
        assert_eq!(documents.borrow().get(document).unwrap().views(), 1);
        assert_eq!(sibling.active_document(), Some(document));
    }

    #[test]
    fn panes_combine_separate_preserve_live_views_and_document_counts() {
        let (mut state, context, ids) = state();
        let documents = state.documents.borrow().len();
        let placements: Vec<_> = ids
            .iter()
            .map(|id| {
                state
                    .tabs
                    .iter()
                    .find(|tab| tab.id == *id)
                    .unwrap()
                    .pane_id
                    .clone()
            })
            .collect();
        for (source, target, side) in [
            (ids[1], ids[0], PaneSide::Right),
            (ids[2], ids[0], PaneSide::Below),
            (ids[3], ids[1], PaneSide::Below),
        ] {
            state.dispatch(
                AppCommand::CombinePanes {
                    source,
                    target,
                    side,
                    expected_revision: state.pane_revision(),
                },
                &context,
            );
            assert!(state.take_pane_error().is_none());
        }
        assert_eq!(
            state.pane_members(ids[0]),
            vec![ids[0], ids[2], ids[1], ids[3]]
        );
        assert_eq!(state.documents.borrow().len(), documents);
        assert_eq!(state.tabs().len(), 6);
        state.dispatch(AppCommand::SeparatePaneGroup(ids[0]), &context);
        assert!(state.pane_group(ids[0]).is_none());
        assert_eq!(state.tabs().len(), 6);
        assert_eq!(state.documents.borrow().len(), documents);
        state.dispatch(
            AppCommand::CombinePanes {
                source: ids[1],
                target: ids[0],
                side: PaneSide::Right,
                expected_revision: state.pane_revision(),
            },
            &context,
        );
        let group = state.pane_group(ids[0]).unwrap();
        for (index, id) in ids[..2].iter().enumerate() {
            let Some(PaneTree::Leaf { pane_id, .. }) = group.tree.leaf(id) else {
                panic!("composed leaf")
            };
            assert_eq!(
                pane_id, placements[index],
                "separation must retain placement identity"
            );
        }
    }

    #[test]
    fn panes_capacity_stale_and_geometry_refusals_are_atomic() {
        let (mut state, context, ids) = state();
        for source in &ids[1..4] {
            state.dispatch(
                AppCommand::CombinePanes {
                    source: *source,
                    target: ids[0],
                    side: PaneSide::Right,
                    expected_revision: state.pane_revision(),
                },
                &context,
            );
        }
        let before = state.pane_groups.clone();
        state.dispatch(
            AppCommand::CombinePanes {
                source: ids[4],
                target: ids[0],
                side: PaneSide::Below,
                expected_revision: state.pane_revision(),
            },
            &context,
        );
        assert!(state.take_pane_error().is_some());
        assert_eq!(state.pane_groups, before);
        state.dispatch(
            AppCommand::CombinePanes {
                source: ids[1],
                target: ids[0],
                side: PaneSide::Below,
                expected_revision: u64::MAX,
            },
            &context,
        );
        assert!(state.take_pane_error().is_some());
        assert_eq!(state.pane_groups, before);
    }
}
