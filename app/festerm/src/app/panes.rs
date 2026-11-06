use super::*;
use festerm_config::{PaneAxis, PanePreset, PaneSide};

#[derive(Clone, Copy)]
pub(super) struct PaneMove {
    view: TabId,
    revision: u64,
    native: bool,
}

#[derive(Default)]
pub(super) struct PaneControls {
    arranging: bool,
    movement: Option<PaneMove>,
    menu: Option<TabId>,
    pressed: Option<(TabId, bool)>,
    target: Option<TabId>,
    pub(super) pointer_owned: bool,
    released: Option<egui::Pos2>,
    cancelled_drag: bool,
}

impl PaneControls {
    pub(super) fn blocks_input(&self) -> bool {
        self.arranging
            || self.movement.is_some()
            || self.menu.is_some()
            || self.pressed.is_some()
            || self.cancelled_drag
            || self.pointer_owned
    }
}

#[derive(Clone, Copy)]
enum PaneDestination {
    Beside(TabId, PaneSide),
    Swap(TabId),
    Detach,
    Close,
}

fn pane_handle_rect(rect: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(
            rect.center().x - (rect.width() - 16.0).min(220.0) / 2.0,
            rect.top() + 5.0,
        ),
        egui::vec2((rect.width() - 16.0).clamp(24.0, 220.0), 28.0),
    )
}

impl FesTermApp {
    pub(super) fn begin_pane_rearrange(&mut self, context: &egui::Context) {
        self.pane_layout_menu = None;
        self.pane_controls.arranging = true;
        self.pane_controls.pointer_owned = true;
        crate::keyboard::retire_composition(context);
        self.cancel_clipboard_paste(context);
    }

    pub(super) fn finish_pane_rearrange(&mut self, context: &egui::Context) {
        self.pane_controls = PaneControls {
            pointer_owned: true,
            ..Default::default()
        };
        egui::DragAndDrop::take_payload::<PaneMove>(context);
        if let Some(session) = self.state.session_tab_mut(self.state.active()) {
            session.view.request_focus_on_next_frame();
        }
    }

    pub(super) fn pane_handle_press(&self, event: &egui::Event) -> bool {
        if self.overlays.blocks_terminal_input()
            || self.palette.is_open()
            || self.pane_layout_menu.is_some()
            || self.pane_close.is_some()
            || self.pane_details_open
            || !(self.state.show_pane_handles() || self.pane_controls.arranging)
            || self.state.pane_group(self.state.active()).is_none()
        {
            return false;
        }
        matches!(event, egui::Event::PointerButton {
            pos, button:egui::PointerButton::Primary, pressed:true, ..
        } if self.pane_rects.iter().any(|(_, rect)| pane_handle_rect(*rect).contains(*pos)))
    }

    pub(super) fn own_pane_handle_press(&mut self, event: &egui::Event) {
        if let egui::Event::PointerButton { pos, .. } = event {
            if let Some((view, rect)) = self
                .pane_rects
                .iter()
                .find(|(_, rect)| pane_handle_rect(*rect).contains(*pos))
            {
                self.pane_controls.pressed =
                    Some((*view, pos.x >= pane_handle_rect(*rect).right() - 28.0));
                self.pane_controls.pointer_owned = true;
            }
        }
    }

    pub(super) fn record_pane_pointer_release(&mut self, event: &egui::Event) {
        if let egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            ..
        } = event
        {
            self.pane_controls.released = Some(*pos);
            if self.pane_controls.cancelled_drag {
                self.pane_controls.cancelled_drag = false;
                self.pane_controls.pointer_owned = true;
            }
        }
    }

    pub(super) fn prepare_pane_controls(&mut self, context: &egui::Context) {
        self.pane_controls.pointer_owned = false;
        self.pane_controls.released = None;
        if self.pane_controls.movement.is_some_and(|movement| {
            movement.native
                && egui::DragAndDrop::payload::<PaneMove>(context).is_none_or(|payload| {
                    payload.view != movement.view || payload.revision != movement.revision
                })
        }) {
            self.pane_controls.movement = None;
            self.pane_controls.pressed = None;
            self.pane_controls.menu = None;
            self.pane_controls.cancelled_drag = context.input(|input| input.pointer.primary_down());
            self.pane_controls.pointer_owned = true;
        }
        if context.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::WindowFocused(false)))
        }) || !self.state.composable(self.state.active())
        {
            self.pane_controls = PaneControls::default();
            egui::DragAndDrop::take_payload::<PaneMove>(context);
            egui::DragAndDrop::take_payload::<ChipId>(context);
            self.pane_controls.pointer_owned = false;
        }
        if self
            .pane_controls
            .pressed
            .is_some_and(|(view, _)| !self.state.composable(view))
        {
            self.pane_controls.pressed = None;
        }
        if let Some(movement) = self.pane_controls.movement {
            if movement.revision != self.state.pane_revision()
                || !self.state.composable(movement.view)
            {
                self.pane_controls.movement = None;
                self.pane_controls.menu = None;
                self.pane_controls.pressed = None;
                self.pane_controls.cancelled_drag =
                    movement.native && context.input(|input| input.pointer.primary_down());
                self.pane_controls.pointer_owned = true;
                egui::DragAndDrop::take_payload::<PaneMove>(context);
                egui::DragAndDrop::take_payload::<ChipId>(context);
                self.overlays.transient_notice = Some((
                    "Pane movement cancelled because the layout changed. Try again.".into(),
                    Instant::now() + Duration::from_secs(5),
                ));
            }
        }
        if !self.overlays.blocks_terminal_input()
            && !self.palette.is_open()
            && self.pane_layout_menu.is_none()
            && self.pane_controls.blocks_input()
            && context
                .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            if self.pane_controls.movement.is_some() || self.pane_controls.menu.is_some() {
                self.pane_controls.movement = None;
                self.pane_controls.menu = None;
                self.pane_controls.pointer_owned = true;
                egui::DragAndDrop::take_payload::<PaneMove>(context);
            } else {
                self.finish_pane_rearrange(context);
            }
        }
        if self.pane_controls.blocks_input() {
            crate::keyboard::retire_composition(context);
            self.cancel_clipboard_paste(context);
        }
    }

    fn start_pane_movement(&mut self, view: TabId, native: bool, context: &egui::Context) {
        let movement = PaneMove {
            view,
            revision: self.state.pane_revision(),
            native,
        };
        self.pane_controls.movement = Some(movement);
        self.pane_controls.target = None;
        self.pane_controls.menu = None;
        self.pane_controls.pointer_owned = true;
        crate::keyboard::retire_composition(context);
        self.cancel_clipboard_paste(context);
        if native {
            egui::DragAndDrop::set_payload(context, movement);
        }
    }

    fn complete_pane_movement(&mut self, action: PaneDestination, context: &egui::Context) {
        let Some(movement) = self.pane_controls.movement.take() else {
            return;
        };
        self.pane_controls.menu = None;
        self.pane_controls.pointer_owned = true;
        egui::DragAndDrop::take_payload::<PaneMove>(context);
        egui::DragAndDrop::take_payload::<ChipId>(context);
        if movement.revision != self.state.pane_revision() {
            self.overlays.transient_notice = Some((
                "The layout changed. Select the source again.".into(),
                Instant::now() + Duration::from_secs(5),
            ));
            return;
        }
        let command = match action {
            PaneDestination::Beside(target, side) => AppCommand::CombinePanes {
                source: movement.view,
                target,
                side,
                expected_revision: movement.revision,
            },
            PaneDestination::Swap(second) => AppCommand::SwapPanes {
                first: movement.view,
                second,
                expected_revision: movement.revision,
            },
            PaneDestination::Detach => AppCommand::DetachPane {
                view: movement.view,
                expected_revision: movement.revision,
            },
            PaneDestination::Close => {
                self.request_close_tab(movement.view, context);
                return;
            }
        };
        self.state.dispatch(command, context);
    }

    pub(super) fn show_pane_controls(&mut self, ui: &mut egui::Ui) {
        let context = ui.ctx().clone();
        let deferred_id = Self::ordered_keyboard_events_id(&context);
        if self.overlays.blocks_terminal_input()
            || self.palette.is_open()
            || self.pane_layout_menu.is_some()
            || self.pane_close.is_some()
            || self.pane_details_open
            || context
                .data(|data| data.get_temp::<Vec<egui::Event>>(deferred_id))
                .is_some_and(|events| !events.is_empty())
        {
            return;
        }
        let pointer = context.pointer_interact_pos();
        if self.pane_controls.movement.is_none()
            && pointer.is_some_and(|pos| self.pane_rects.iter().any(|(_, rect)| rect.contains(pos)))
        {
            if let Some(chip) = egui::DragAndDrop::payload::<ChipId>(&context) {
                if let Some(view) = self.tab_id_for_chip(*chip) {
                    if self.state.pane_group(view).is_none() && self.state.composable(view) {
                        self.start_pane_movement(view, true, &context);
                    }
                }
            }
        }
        let mut begin = None;
        let mut menu = None;
        if let Some((view, is_menu)) = self.pane_controls.pressed {
            if let Some(pos) = self.pane_controls.released {
                self.pane_controls.pressed = None;
                self.pane_controls.pointer_owned = true;
                if self
                    .pane_rects
                    .iter()
                    .any(|(id, rect)| *id == view && pane_handle_rect(*rect).contains(pos))
                {
                    if is_menu {
                        menu = Some(view);
                    } else {
                        begin = Some((view, false));
                    }
                }
            } else if !is_menu && context.input(|input| input.pointer.is_decidedly_dragging()) {
                self.pane_controls.pressed = None;
                begin = Some((view, true));
            }
        }
        if self.state.pane_group(self.state.active()).is_some() {
            for (view, rect) in self.pane_rects.clone() {
                let hotspot = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 34.0));
                let visible = self.pane_controls.arranging
                    || self.pane_controls.menu == Some(view)
                    || (self.state.show_pane_handles()
                        && pointer.is_some_and(|pos| hotspot.contains(pos)));
                if !visible {
                    continue;
                }
                let handle = pane_handle_rect(rect);
                let name = self.pane_label(view);
                let short = if name.chars().count() > 24 {
                    format!("{}…", name.chars().take(23).collect::<String>())
                } else {
                    name.clone()
                };
                egui::Area::new(egui::Id::new((
                    "pane-handle",
                    context.viewport_id(),
                    view.chip_id(),
                )))
                .order(egui::Order::Foreground)
                .fixed_pos(handle.min)
                .movable(false)
                .show(&context, |ui| {
                    egui::Frame::popup(&context.global_style())
                        .inner_margin(egui::Margin::symmetric(4, 2))
                        .show(ui, |ui| {
                            ui.set_min_width(handle.width() - 8.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                let response = ui
                                    .add_sized(
                                        egui::vec2(handle.width() - 36.0, 24.0),
                                        egui::Button::new((
                                            egui::Atom::custom(
                                                ui.id().with("grip"),
                                                egui::vec2(14.0, 14.0),
                                            ),
                                            egui::RichText::new(short).size(12.0),
                                            egui::Atom::grow(),
                                        ))
                                        .gap(6.0)
                                        .truncate()
                                        .sense(egui::Sense::click_and_drag()),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::Grab);
                                response.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::Button,
                                        true,
                                        format!("Move {name} pane"),
                                    )
                                });
                                let icon = egui::Rect::from_center_size(
                                    response.rect.left_center() + egui::vec2(10.0, 0.0),
                                    egui::vec2(14.0, 14.0),
                                );
                                festerm_ui_egui::icon::paint(
                                    ui.painter(),
                                    festerm_ui_egui::icon::Icon::PaneHandle,
                                    icon,
                                    theme::TEXT_SECONDARY,
                                );
                                if response.drag_started() {
                                    begin = Some((view, true));
                                } else if response.clicked() {
                                    begin = Some((view, false));
                                }
                                let more =
                                    ui.add_sized(egui::vec2(24.0, 24.0), egui::Button::new("…"));
                                more.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::Button,
                                        true,
                                        format!("Actions for {name}"),
                                    )
                                });
                                if more.clicked() {
                                    menu = Some(view);
                                }
                            });
                        });
                });
            }
        }
        if let Some((view, native)) = begin {
            self.start_pane_movement(view, native, &context);
        }
        if let Some(view) = menu {
            self.pane_controls.menu = if self.pane_controls.menu == Some(view) {
                None
            } else {
                Some(view)
            };
            self.pane_controls.pointer_owned = true;
        }
        if let Some(view) = self.pane_controls.menu {
            if let Some((_, rect)) = self.pane_rects.iter().find(|(id, _)| *id == view) {
                let handle = pane_handle_rect(*rect);
                let mut chosen = None;
                let response =
                    egui::Area::new(egui::Id::new(("pane-handle-menu", context.viewport_id())))
                        .order(egui::Order::Foreground)
                        .fixed_pos(handle.left_bottom() + egui::vec2(0.0, 4.0))
                        .movable(false)
                        .show(&context, |ui| {
                            egui::Frame::popup(&context.global_style()).show(ui, |ui| {
                                if ui.button("Move pane…").clicked() {
                                    chosen = Some(None);
                                }
                                if ui.button("Detach to own tab").clicked() {
                                    chosen = Some(Some(PaneDestination::Detach));
                                }
                                ui.separator();
                                if ui
                                    .button(
                                        egui::RichText::new("Close pane…")
                                            .color(theme::STATUS_ERROR),
                                    )
                                    .clicked()
                                {
                                    chosen = Some(Some(PaneDestination::Close));
                                }
                            });
                        });
                if let Some(chosen) = chosen {
                    self.start_pane_movement(view, false, &context);
                    if let Some(action) = chosen {
                        self.complete_pane_movement(action, &context);
                    }
                } else if context.input(|input| input.pointer.primary_pressed())
                    && pointer.is_some_and(|pos| {
                        !handle.contains(pos) && !response.response.rect.contains(pos)
                    })
                {
                    self.pane_controls.menu = None;
                    self.pane_controls.pointer_owned = true;
                }
            } else {
                self.pane_controls.menu = None;
            }
        }
        if self.pane_controls.arranging {
            egui::Area::new(egui::Id::new((
                "pane-rearrange-done",
                context.viewport_id(),
            )))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -36.0))
            .movable(false)
            .show(&context, |ui| {
                egui::Frame::popup(&context.global_style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Rearranging panes");
                        if ui.button("Done rearranging").clicked() {
                            self.finish_pane_rearrange(&context);
                        }
                    });
                });
            });
        }
        let Some(movement) = self.pane_controls.movement else {
            return;
        };
        let source = self.pane_label(movement.view);
        let target = self
            .pane_controls
            .target
            .filter(|view| *view != movement.view && self.state.composable(*view))
            .and_then(|view| {
                self.pane_rects
                    .iter()
                    .find(|(id, _)| *id == view)
                    .or_else(|| self.pane_rects.first())
                    .map(|(_, rect)| (view, *rect))
            })
            .or_else(|| {
                self.pane_rects
                    .iter()
                    .find(|(view, rect)| {
                        *view != movement.view && pointer.is_some_and(|pos| rect.contains(pos))
                    })
                    .copied()
            })
            .or_else(|| {
                self.pane_rects
                    .iter()
                    .find(|(view, _)| *view != movement.view)
                    .copied()
            })
            .or_else(|| {
                self.state
                    .tabs()
                    .iter()
                    .find(|tab| tab.id != movement.view && self.state.composable(tab.id))
                    .and_then(|tab| self.pane_rects.first().map(|(_, rect)| (tab.id, *rect)))
            });
        let mut action = None;
        if let Some((target, rect)) = target {
            let width = (rect.width() - 24.0).min(300.0);
            let pos = rect.center() - egui::vec2(width / 2.0, 94.0);
            egui::Area::new(egui::Id::new(("pane-destinations", context.viewport_id())))
                .order(egui::Order::Foreground)
                .fixed_pos(pos)
                .movable(false)
                .show(&context, |ui| {
                    egui::Frame::popup(&context.global_style()).show(ui, |ui| {
                        ui.set_width(width);
                        let choices = self
                            .state
                            .tabs()
                            .iter()
                            .filter(|tab| tab.id != movement.view && self.state.composable(tab.id))
                            .map(|tab| (tab.id, self.pane_label(tab.id)))
                            .collect::<Vec<_>>();
                        egui::ComboBox::from_id_salt("pane-move-target")
                            .selected_text(format!("Target: {}", self.pane_label(target)))
                            .show_ui(ui, |ui| {
                                for (view, label) in choices {
                                    ui.selectable_value(
                                        &mut self.pane_controls.target,
                                        Some(view),
                                        label,
                                    );
                                }
                            });
                        let mut destination =
                            |ui: &mut egui::Ui, label: &str, choice: PaneDestination| {
                                let error = match choice {
                                    PaneDestination::Beside(view, side) => self
                                        .state
                                        .planned_pane_split(movement.view, view, side)
                                        .err(),
                                    _ => None,
                                };
                                let response = ui.add_enabled(
                                    error.is_none(),
                                    egui::Button::new(
                                        if matches!(choice, PaneDestination::Close) {
                                            egui::RichText::new(label).color(theme::STATUS_ERROR)
                                        } else {
                                            egui::RichText::new(label)
                                        },
                                    )
                                    .min_size(egui::vec2(70.0, 28.0)),
                                );
                                if let Some(error) = error {
                                    response.clone().on_disabled_hover_text(error);
                                }
                                if response.clicked()
                                    || (response.enabled()
                                        && movement.native
                                        && self
                                            .pane_controls
                                            .released
                                            .is_some_and(|pos| response.rect.contains(pos)))
                                {
                                    action = Some(choice);
                                }
                            };
                        ui.horizontal(|ui| {
                            ui.add_space(80.0);
                            destination(
                                ui,
                                "Above",
                                PaneDestination::Beside(target, PaneSide::Above),
                            );
                        });
                        ui.horizontal(|ui| {
                            destination(
                                ui,
                                "Left",
                                PaneDestination::Beside(target, PaneSide::Left),
                            );
                            destination(ui, "Swap", PaneDestination::Swap(target));
                            destination(
                                ui,
                                "Right",
                                PaneDestination::Beside(target, PaneSide::Right),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.add_space(80.0);
                            destination(
                                ui,
                                "Below",
                                PaneDestination::Beside(target, PaneSide::Below),
                            );
                        });
                        ui.separator();
                        ui.label(format!("Moving: {source}"));
                        ui.horizontal(|ui| {
                            if self.state.pane_group(movement.view).is_some() {
                                destination(ui, "Detach", PaneDestination::Detach);
                            }
                            destination(ui, "Close…", PaneDestination::Close);
                        });
                    });
                });
        }
        if !self.focus_mode && self.state.pane_group(movement.view).is_some() {
            if let Some(rect) =
                chrome::pane_tab_strip_target(&context).filter(|rect| rect.width() >= 100.0)
            {
                let hovered = pointer.is_some_and(|pos| rect.contains(pos));
                ui.painter()
                    .with_clip_rect(rect)
                    .rect_filled(rect, 4.0, theme::SURFACE_OVERLAY);
                ui.painter().with_clip_rect(rect).rect_stroke(
                    rect,
                    4.0,
                    egui::Stroke::new(1.0, theme::BORDER_ACTIVE),
                    egui::StrokeKind::Inside,
                );
                ui.painter().with_clip_rect(rect).text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("Detach {source} here"),
                    egui::FontId::proportional(12.0),
                    if hovered {
                        theme::TEXT_PRIMARY
                    } else {
                        theme::TEXT_SECONDARY
                    },
                );
                if movement.native
                    && self
                        .pane_controls
                        .released
                        .is_some_and(|pos| rect.contains(pos))
                {
                    action = Some(PaneDestination::Detach);
                }
            }
        }
        if let Some(action) = action {
            self.complete_pane_movement(action, &context);
        } else if movement.native && self.pane_controls.released.is_some() {
            self.pane_controls.movement = None;
            self.pane_controls.pointer_owned = true;
            egui::DragAndDrop::take_payload::<PaneMove>(&context);
            egui::DragAndDrop::take_payload::<ChipId>(&context);
        }
    }
}

impl FesTermApp {
    pub(super) fn pane_focus_target(&self, event: &egui::Event) -> Option<TabId> {
        if self.overlays.blocks_terminal_input()
            || self.palette.is_open()
            || self.pane_layout_menu.is_some()
            || self.pane_details_open
            || self.pane_close.is_some()
            || self.pane_controls.blocks_input()
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
            if response.dragged() && !self.pane_controls.blocks_input() {
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
        if self.pane_rects.len() > 1 {
            if let Some((_, rect)) = self.pane_rects.iter().find(|(view, _)| *view == active) {
                ui.painter().rect_stroke(
                    rect.shrink(1.5 / ui.ctx().pixels_per_point()),
                    0.0,
                    egui::Stroke::new(
                        1.0 / ui.ctx().pixels_per_point(),
                        if self.state.pane_high_contrast() {
                            theme::BORDER_ACTIVE
                        } else {
                            theme::BORDER_PANE_FOCUS
                        },
                    ),
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
        let mut rearrange = false;
        let mut move_view = false;
        let response = egui::Modal::new(egui::Id::new("pane-layout-menu")).show(context, |ui| {
            ui.set_width(420.0);
            ui.heading("Pane layout");
            if let Some(group) = &group {
                ui.label(&group.label);
                ui.add_space(8.0);
                for member in group.tree.views() {
                    let unread = self
                        .state
                        .session_tab(member)
                        .is_some_and(|s| s.has_new_output_since_active);
                    let label = format!(
                        "{}{}",
                        self.pane_label(member),
                        if unread { " • unread" } else { "" }
                    );
                    if ui.selectable_label(member == focused, label).clicked() {
                        command = Some(AppCommand::ActivateTab(member));
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    for (label, preset, count) in [
                        ("Columns", PanePreset::Columns, 2),
                        ("Rows", PanePreset::Rows, 2),
                        ("Grid", PanePreset::Grid, 4),
                    ] {
                        ui.add_enabled_ui(group.tree.views().len() == count, |ui| {
                            let (rect, response) = ui
                                .allocate_exact_size(egui::vec2(88.0, 66.0), egui::Sense::click());
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    ui.is_enabled(),
                                    format!("{label} preset"),
                                )
                            });
                            let preview = egui::Rect::from_center_size(
                                rect.center() - egui::vec2(0.0, 8.0),
                                egui::vec2(64.0, 32.0),
                            );
                            let stroke = egui::Stroke::new(
                                1.0,
                                if response.hovered() {
                                    theme::BORDER_ACTIVE
                                } else {
                                    theme::BORDER_SUBTLE
                                },
                            );
                            ui.painter()
                                .rect_filled(preview, 3.0, theme::SURFACE_TERMINAL);
                            ui.painter().rect_stroke(
                                preview,
                                3.0,
                                stroke,
                                egui::StrokeKind::Inside,
                            );
                            if preset != PanePreset::Rows {
                                ui.painter().line_segment(
                                    [
                                        egui::pos2(preview.center().x, preview.top()),
                                        egui::pos2(preview.center().x, preview.bottom()),
                                    ],
                                    stroke,
                                );
                            }
                            if preset != PanePreset::Columns {
                                ui.painter().line_segment(
                                    [
                                        egui::pos2(preview.left(), preview.center().y),
                                        egui::pos2(preview.right(), preview.center().y),
                                    ],
                                    stroke,
                                );
                            }
                            ui.painter().text(
                                rect.center_bottom() - egui::vec2(0.0, 3.0),
                                egui::Align2::CENTER_BOTTOM,
                                label,
                                egui::FontId::proportional(12.0),
                                theme::TEXT_PRIMARY,
                            );
                            if response.clicked() {
                                command = Some(AppCommand::SetPanePreset {
                                    view: focused,
                                    preset,
                                });
                            }
                        });
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Rearrange panes").clicked() {
                        rearrange = true;
                    }
                    if ui.button("Equalize").clicked() {
                        command = Some(AppCommand::EqualizePanes(focused));
                    }
                    if ui
                        .button(if self.state.pane_zoom().is_some() {
                            "Restore panes"
                        } else {
                            "Zoom focused"
                        })
                        .clicked()
                    {
                        command = Some(AppCommand::ZoomPane(focused));
                    }
                });
                ui.menu_button("Focused pane actions…", |ui| {
                    if ui.button("Move or swap…").clicked() {
                        move_view = true;
                        ui.close();
                    }
                    if ui.button("Detach to own tab").clicked() {
                        command = Some(AppCommand::SeparatePane(focused));
                        ui.close();
                    }
                    if ui.button("Separate all panes").clicked() {
                        command = Some(AppCommand::SeparatePaneGroup(focused));
                        ui.close();
                    }
                    ui.menu_button("Keyboard sizing…", |ui| {
                        for (label, axis, increase) in [
                            ("Wider", PaneAxis::X, true),
                            ("Narrower", PaneAxis::X, false),
                            ("Taller", PaneAxis::Y, true),
                            ("Shorter", PaneAxis::Y, false),
                        ] {
                            if ui.button(label).clicked() {
                                command = Some(AppCommand::ResizeFocusedPane { axis, increase });
                                ui.close();
                            }
                        }
                    });
                });
            }
            ui.menu_button("Add existing view…", |ui| {
                let mut available = false;
                for tab in self.state.tabs() {
                    if tab.id == focused
                        || !self.state.composable(tab.id)
                        || group
                            .as_ref()
                            .is_some_and(|group| group.tree.views().contains(&tab.id))
                    {
                        continue;
                    }
                    available = true;
                    ui.menu_button(self.pane_label(tab.id), |ui| {
                        for (label, side) in [
                            ("Left", PaneSide::Left),
                            ("Right", PaneSide::Right),
                            ("Above", PaneSide::Above),
                            ("Below", PaneSide::Below),
                        ] {
                            let refusal =
                                self.state.planned_pane_split(tab.id, focused, side).err();
                            let response =
                                ui.add_enabled(refusal.is_none(), egui::Button::new(label));
                            if response.clicked() {
                                command = Some(AppCommand::CombinePanes {
                                    source: tab.id,
                                    target: focused,
                                    side,
                                    expected_revision: self.state.pane_revision(),
                                });
                                ui.close();
                            }
                            if let Some(error) = refusal {
                                response.on_disabled_hover_text(error);
                            }
                        }
                    });
                }
                if !available {
                    ui.label("Launch another view normally, then add it here.");
                }
            });
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Layout changes move existing views without restarting them.")
                    .small()
                    .color(theme::TEXT_SECONDARY),
            );
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .small_button(egui::RichText::new("Close focused…").color(theme::STATUS_ERROR))
                    .clicked()
                {
                    close_view = true;
                }
                if group.is_some()
                    && ui
                        .small_button(
                            egui::RichText::new("Close group…").color(theme::STATUS_ERROR),
                        )
                        .clicked()
                {
                    self.pane_close = self
                        .state
                        .pane_close_plan(focused)
                        .map(|plan| (plan, Vec::new()));
                    self.pane_layout_menu = None;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Done").clicked() {
                        self.pane_layout_menu = None;
                    }
                });
            });
        });
        if response.should_close() {
            self.pane_layout_menu = None;
        }
        if let Some(command) = command {
            self.state.dispatch(command, context);
            self.pane_layout_menu = None;
        }
        if rearrange {
            self.begin_pane_rearrange(context);
        }
        if move_view {
            self.pane_layout_menu = None;
            self.start_pane_movement(focused, false, context);
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
