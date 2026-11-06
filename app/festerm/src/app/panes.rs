use super::*;
use festerm_config::{PaneAxis, PanePreset, PaneSide};

impl FesTermApp {
    pub(super) fn pane_focus_target(&self, event: &egui::Event) -> Option<TabId> {
        if self.overlays.blocks_terminal_input()
            || self.palette.is_open()
            || self.pane_layout_menu.is_some()
            || self.pane_details_open
            || self.pane_close.is_some()
        {
            return None;
        }
        let egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            ..
        } = event
        else {
            return None;
        };
        let (view, _) = self
            .pane_rects
            .iter()
            .find(|(_, rect)| rect.contains(*pos))?;
        (*view != self.state.active()
            && self
                .state
                .pane_group(self.state.active())
                .is_some_and(|group| group.tree.contains(view)))
        .then_some(*view)
    }

    pub(super) fn prepare_pane_geometry(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
    ) -> egui::Rect {
        let active = self.state.active();
        let minimum = self
            .state
            .tabs()
            .iter()
            .filter_map(|tab| {
                if let TabContent::Session(session) = &tab.content {
                    Some(session.view.minimum_pane_size(ui))
                } else {
                    None
                }
            })
            .fold(egui::vec2(320.0, 240.0), |a, b| a.max(b));
        self.state.pane_geometry(rect.size(), minimum);
        self.state.pane_display_scale(ui.ctx().pixels_per_point());
        let Some(group) = self.state.pane_group(active).cloned() else {
            self.pane_rects = vec![(active, rect)];
            return rect;
        };
        let constrained = !crate::desktop_panes::fits(
            &group.tree,
            rect.size(),
            minimum,
            ui.ctx().pixels_per_point(),
        );
        if self.focus_mode || self.state.pane_zoom().is_some() || constrained {
            self.pane_rects = vec![(active, rect)];
            return rect;
        }
        let scale = ui.ctx().pixels_per_point();
        self.pane_rects = crate::desktop_panes::geometry(&group.tree, rect, scale);
        let revision = self.state.pane_revision();
        for divider in crate::desktop_panes::dividers(&group.tree, rect, scale) {
            ui.painter()
                .rect_filled(divider.rect, 0.0, theme::BORDER_SUBTLE);
            let hit = match divider.axis {
                PaneAxis::X => divider.rect.expand2(egui::vec2(3.0, 0.0)),
                PaneAxis::Y => divider.rect.expand2(egui::vec2(0.0, 3.0)),
            };
            let response = ui
                .interact(
                    hit,
                    egui::Id::new(("pane-divider", &group.group_id, &divider.path)),
                    egui::Sense::drag(),
                )
                .on_hover_cursor(match divider.axis {
                    PaneAxis::X => egui::CursorIcon::ResizeHorizontal,
                    PaneAxis::Y => egui::CursorIcon::ResizeVertical,
                });
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, "Resize panes")
            });
            if response.dragged() {
                if let Some(position) = response.interact_pointer_pos() {
                    let fraction = match divider.axis {
                        PaneAxis::X => {
                            (position.x - divider.parent.min.x)
                                / (divider.parent.width() - 1.0 / scale)
                        }
                        PaneAxis::Y => {
                            (position.y - divider.parent.min.y)
                                / (divider.parent.height() - 1.0 / scale)
                        }
                    };
                    self.state.dispatch(
                        AppCommand::ResizePaneSplit {
                            view: active,
                            path: divider.path,
                            ratio: (fraction.clamp(0.05, 0.95) * 10_000.0).round() as u16,
                            expected_revision: revision,
                        },
                        ui.ctx(),
                    );
                }
            }
        }
        self.pane_rects
            .iter()
            .find(|(view, _)| *view == active)
            .map_or(rect, |(_, rect)| *rect)
    }

    pub(super) fn show_passive_panes(&mut self, ui: &mut egui::Ui) {
        let active = self.state.active();
        let documents = Rc::clone(self.state.documents());
        let fonts = self.terminal_font_set();
        let status_bar = self.state.status_bar_visible() && !self.focus_mode;
        for (view, rect) in self.pane_rects.clone() {
            if view == active {
                continue;
            }
            let Some(tab) = self.state.surface_tab_mut(view) else {
                continue;
            };
            let events = ui.input_mut(|input| std::mem::take(&mut input.events));
            ui.scope_builder(
                egui::UiBuilder::new()
                    .id_salt(("surface", view.chip_id()))
                    .max_rect(rect),
                |ui| {
                    ui.set_clip_rect(ui.clip_rect().intersect(rect));
                    ui.visuals_mut().disabled_alpha = 1.0;
                    ui.disable();
                    match &mut tab.content {
                        TabContent::Session(session) => {
                            session.view.set_font_set(fonts);
                            session.view.show_passive_in_ui(
                                ui,
                                &mut session.terminal,
                                &mut session.controller,
                            );
                        }
                        TabContent::TextEditor(editor) => {
                            editor.set_status_bar_visible(status_bar);
                            let _ = editor.show(ui, view, &documents);
                        }
                        TabContent::MarkdownViewer(viewer) => {
                            viewer.set_status_bar_visible(status_bar);
                            let _ = viewer.show(ui, view);
                        }
                        TabContent::SftpFileManager(files) => {
                            files.set_status_bar_visible(status_bar);
                            let _ = files.show(ui, view);
                        }
                        TabContent::SshAuthenticationRequired(_)
                        | TabContent::SftpAuthenticationRequired(_)
                        | TabContent::SftpFileManagerAuthenticationRequired(_) => {
                            ui.label("Authentication required. Select this pane to continue.");
                        }
                        TabContent::Launcher | TabContent::Settings | TabContent::Profiles => {}
                    }
                },
            );
            ui.input_mut(|input| input.events = events);
        }
        self.show_pane_drop_target(ui);
        if self.pane_rects.len() > 1 {
            if let Some((_, rect)) = self.pane_rects.iter().find(|(view, _)| *view == active) {
                ui.painter().rect_stroke(
                    rect.shrink(1.5 / ui.ctx().pixels_per_point()),
                    0.0,
                    egui::Stroke::new(1.0 / ui.ctx().pixels_per_point(), theme::BORDER_ACTIVE),
                    egui::StrokeKind::Inside,
                );
            }
        } else if !self.focus_mode
            && self.state.pane_zoom().is_none()
            && self.state.pane_group(active).is_some()
        {
            if let Some((_, rect)) = self.pane_rects.first() {
                ui.painter().text(
                    rect.right_top() + egui::vec2(-8.0, 8.0),
                    egui::Align2::RIGHT_TOP,
                    "Focused pane · enlarge window to restore layout",
                    egui::FontId::proportional(12.0),
                    theme::TEXT_SECONDARY,
                );
            }
        }
        if let Some(error) = self.state.take_pane_error() {
            self.overlays.transient_notice = Some((error, Instant::now() + Duration::from_secs(5)));
        }
    }

    fn pane_label(&self, view: TabId) -> String {
        self.state
            .tabs()
            .iter()
            .find(|tab| tab.id == view)
            .map_or_else(
                || "Unavailable view".into(),
                |tab| match &tab.content {
                    TabContent::Session(session) => session.label.clone(),
                    TabContent::TextEditor(editor) => editor.title().into(),
                    TabContent::MarkdownViewer(viewer) => viewer.title().into(),
                    TabContent::SftpFileManager(files) => files.label.clone(),
                    _ => format!("View {}", view.chip_id()),
                },
            )
    }

    fn show_pane_drop_target(&mut self, ui: &mut egui::Ui) {
        let context = ui.ctx().clone();
        let key = egui::Id::new("pane-chip-drag-revision");
        let Some(chip) = egui::DragAndDrop::payload::<ChipId>(&context) else {
            context.data_mut(|data| data.remove::<(ChipId, u64)>(key));
            return;
        };
        let revision = context.data_mut(|data| {
            let previous = data.get_temp::<(ChipId, u64)>(key);
            let value = previous
                .filter(|(id, _)| *id == *chip)
                .unwrap_or((*chip, self.state.pane_revision()));
            data.insert_temp(key, value);
            value.1
        });
        if self.pane_close.is_some()
            || self.pane_layout_menu.is_some()
            || self.overlays.blocks_terminal_input()
            || self.palette.is_open()
        {
            return;
        }
        let Some(source) = self.tab_id_for_chip(*chip) else {
            return;
        };
        let Some(position) = context.input(|input| input.pointer.interact_pos()) else {
            return;
        };
        let Some((target, rect)) = self
            .pane_rects
            .iter()
            .find(|(_, rect)| rect.contains(position))
            .copied()
        else {
            return;
        };
        if source == target {
            return;
        }
        let edges = [
            (position.x - rect.left(), PaneSide::Left),
            (rect.right() - position.x, PaneSide::Right),
            (position.y - rect.top(), PaneSide::Above),
            (rect.bottom() - position.y, PaneSide::Below),
        ];
        let (distance, side) = edges
            .into_iter()
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .expect("four pane edges");
        if distance > rect.width().min(rect.height()) * 0.25 {
            return;
        }
        let refusal = if self.state.pane_group(source).is_some() {
            Some("Use Pane layout to move one group member; separate a whole group before dragging it.".to_owned())
        } else if revision != self.state.pane_revision() {
            Some("The layout changed during this drag. Try again.".to_owned())
        } else {
            self.state.planned_pane_split(source, target, side).err()
        };
        let mut preview = rect;
        match side {
            PaneSide::Left => preview.max.x = rect.center().x,
            PaneSide::Right => preview.min.x = rect.center().x,
            PaneSide::Above => preview.max.y = rect.center().y,
            PaneSide::Below => preview.min.y = rect.center().y,
        }
        let color = if refusal.is_some() {
            egui::Color32::from_rgba_unmultiplied(190, 60, 60, 75)
        } else {
            theme::BORDER_ACTIVE.gamma_multiply(0.25)
        };
        ui.painter().rect_filled(preview, 0.0, color);
        ui.painter().text(
            preview.center(),
            egui::Align2::CENTER_CENTER,
            refusal
                .as_deref()
                .unwrap_or("Move live view here — no restart"),
            egui::FontId::proportional(13.0),
            theme::TEXT_PRIMARY,
        );
        if context.input(|input| input.pointer.any_released()) {
            egui::DragAndDrop::take_payload::<ChipId>(&context);
            if let Some(error) = refusal {
                self.overlays.transient_notice =
                    Some((error, Instant::now() + Duration::from_secs(5)));
            } else {
                self.state.dispatch(
                    AppCommand::CombinePanes {
                        source,
                        target,
                        side,
                        expected_revision: revision,
                    },
                    &context,
                );
            }
        }
    }

    pub(super) fn show_pane_layout_menu(&mut self, context: &egui::Context) {
        let Some(view) = self.pane_layout_menu else {
            return;
        };
        if !self.state.composable(view) {
            self.pane_layout_menu = None;
            return;
        }
        let group = self.state.pane_group(view).cloned();
        let focused = group.as_ref().map_or(view, |g| g.focused);
        let mut command = None;
        let mut close_view = false;
        let response = egui::Modal::new(egui::Id::new("pane-layout-menu")).show(context,|ui| {
            ui.set_width(440.0);
            ui.heading(group.as_ref().map_or("Pane layout",|g| g.label.as_str()));
            ui.label(format!("Focused view: {}", self.pane_label(focused)));
            if let Some(group) = &group {
                for member in group.tree.views() {
                    let unread = self.state.session_tab(member).is_some_and(|s| s.has_new_output_since_active);
                    let label = format!("{}{}{}", if member == focused { "> " } else { "" },self.pane_label(member),if unread { " • unread" } else { "" });
                    if ui.button(label).clicked() { command = Some(AppCommand::ActivateTab(member)); }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Previous pane").clicked() { command = Some(AppCommand::FocusNextPane { reverse:true }); }
                    if ui.button("Next pane").clicked() { command = Some(AppCommand::FocusNextPane { reverse:false }); }
                    if ui.button("Zoom / restore").clicked() { command = Some(AppCommand::ZoomPane(focused)); }
                });
                ui.horizontal(|ui| {
                    for (label,side) in [("Focus left",PaneSide::Left),("right",PaneSide::Right),("above",PaneSide::Above),("below",PaneSide::Below)] {
                        if ui.button(label).clicked() { command = Some(AppCommand::FocusPaneDirection(side)); }
                    }
                });
                ui.horizontal(|ui| {
                    for (label,axis,increase) in [("Wider",PaneAxis::X,true),("Narrower",PaneAxis::X,false),("Taller",PaneAxis::Y,true),("Shorter",PaneAxis::Y,false)] {
                        if ui.button(label).clicked() { command = Some(AppCommand::ResizeFocusedPane { axis,increase }); }
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Move to own tab").clicked() { command = Some(AppCommand::SeparatePane(focused)); }
                    if ui.button("Separate all").clicked() { command = Some(AppCommand::SeparatePaneGroup(focused)); }
                    if ui.button("Equalize").clicked() { command = Some(AppCommand::EqualizePanes(focused)); }
                });
                ui.horizontal(|ui| {
                    for (label,preset,count) in [("Two columns",PanePreset::Columns,2),("Two rows",PanePreset::Rows,2),("2×2 grid",PanePreset::Grid,4)] {
                        if ui.add_enabled(group.tree.views().len() == count,egui::Button::new(label)).clicked() {
                            command = Some(AppCommand::SetPanePreset { view:focused,preset });
                        }
                    }
                });
                ui.menu_button("Swap focused pane with…",|ui| {
                    for other in group.tree.views().into_iter().filter(|view| *view != focused) {
                        if ui.button(self.pane_label(other)).clicked() {
                            command = Some(AppCommand::SwapPanes { first:focused,second:other,expected_revision:self.state.pane_revision() });
                            ui.close();
                        }
                    }
                });
                if ui.button("Close group…").clicked() {
                    self.pane_close = self.state.pane_close_plan(focused).map(|plan| (plan,Vec::new()));
                    self.pane_layout_menu = None;
                }
            }
            ui.separator();
            ui.label("Move an existing live view beside the focused pane:");
            egui::ScrollArea::vertical().max_height(240.0).show(ui,|ui| {
                for tab in self.state.tabs() {
                    if tab.id == focused || !self.state.composable(tab.id) { continue; }
                    ui.horizontal_wrapped(|ui| {
                        ui.label(self.pane_label(tab.id));
                        for (label,side) in [("Left",PaneSide::Left),("Right",PaneSide::Right),("Above",PaneSide::Above),("Below",PaneSide::Below)] {
                            if ui.button(label).clicked() {
                                command = Some(AppCommand::CombinePanes { source:tab.id,target:focused,side,expected_revision:self.state.pane_revision() });
                            }
                        }
                    });
                }
            });
            ui.label("To start a new session, launch it normally, then move its live view here. No session is restarted by layout changes.");
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Close focused view…").clicked() { close_view = true; }
                if ui.button("Done").clicked() { self.pane_layout_menu = None; }
            });
        });
        if response.should_close() {
            self.pane_layout_menu = None;
        }
        if let Some(command) = command {
            self.state.dispatch(command, context);
            self.pane_layout_menu = None;
        }
        if close_view {
            self.pane_layout_menu = None;
            self.request_close_tab(focused, context);
        }
    }

    pub(super) fn show_pane_group_close(&mut self, context: &egui::Context) {
        let Some((plan, mut discard)) = self.pane_close.clone() else {
            return;
        };
        if self.state.pane_close_plan(plan.anchor).as_ref() != Some(&plan) {
            self.pane_close = None;
            self.overlays.transient_notice = Some((
                "Close group cancelled because its layout, session or document changed.".into(),
                Instant::now() + Duration::from_secs(5),
            ));
            return;
        }
        let mut cancel = context.input(|input| input.key_pressed(egui::Key::Escape));
        let mut close = false;
        let mut save = false;
        egui::Modal::new(egui::Id::new("close-pane-group")).show(context,|ui| {
            ui.set_width(440.0);
            ui.heading(format!("Close “{}”?",plan.label));
            ui.label("All of these views will close. Sessions disconnect or stop according to their existing close policy; persistent sessions detach. Nothing closes until this preflight is complete.");
            for (id,_) in &plan.members { ui.label(self.pane_label(*id)); }
            for document in &plan.documents {
                ui.separator();
                ui.label(&document.origin);
                let mut approved = discard.contains(&document.id);
                ui.checkbox(&mut approved,format!("Discard unsaved changes to {}",document.title));
                discard.retain(|id| *id != document.id);
                if approved { discard.push(document.id); }
            }
            ui.horizontal(|ui| {
                let response = ui.button("Cancel");
                if context.memory(|memory| memory.focused().is_none()) { response.request_focus(); }
                cancel |= response.clicked();
                if !plan.documents.is_empty() { save = ui.button("Save all, then close group").clicked(); }
                close = ui.add_enabled(plan.documents.iter().all(|document| discard.contains(&document.id)),egui::Button::new("Close group")).clicked();
            });
        });
        if cancel {
            self.pane_close = None;
            return;
        }
        self.pane_close = Some((plan.clone(), discard.clone()));
        if save {
            for document in &plan.documents {
                if self.state.document_requires_save_as(document.id)
                    || !matches!(
                        self.state.save_document(document.id),
                        Some(festerm_document::SaveOutcome::Saved)
                    )
                {
                    self.pane_close = None;
                    self.overlays.transient_notice = Some(("No views closed. Save the document (using Save As if needed), resolve its save warning, then try Close group again.".into(),Instant::now()+Duration::from_secs(8)));
                    return;
                }
            }
            // Saves can change document revisions. Obtain a fresh plan only
            // after every requested save succeeds; no view has closed yet.
            if let Some(saved) = self.state.pane_close_plan(plan.anchor) {
                self.state.dispatch(
                    AppCommand::ClosePaneGroup {
                        plan: saved,
                        discard: Vec::new(),
                    },
                    context,
                );
            }
            self.pane_close = None;
        } else if close {
            self.pane_close = None;
            self.state
                .dispatch(AppCommand::ClosePaneGroup { plan, discard }, context);
        }
    }
}
