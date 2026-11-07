use super::{password_buffer::PasswordBuffer, route_input, widgets, MobileApp};
use eframe::egui::{self, TextBuffer};
use festerm_core::InputEvent;
use festerm_ios_window::{Dictation, DictationPhase, MAX_DICTATION_BYTES};
use festerm_ui_egui::icon::{self, Icon};
use zeroize::{Zeroize, Zeroizing};

pub(super) fn panel_height(available_height: f32) -> f32 {
    (available_height * 0.4).clamp(150.0, 186.0)
}

pub(super) enum Destination {
    Terminal {
        #[cfg(feature = "live-ssh")]
        session: Option<std::sync::Weak<dyn festerm_session::Session>>,
    },
    Document(festerm_mobile_docs::EditTarget),
}

pub(super) fn draft_id() -> egui::Id {
    egui::Id::new("shared-dictation-draft")
}

struct ReviewBuffer(PasswordBuffer);

impl TextBuffer for ReviewBuffer {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        self.0.as_str()
    }
    fn insert_text(&mut self, text: &str, index: egui::text::CharIndex) -> usize {
        let remaining = MAX_DICTATION_BYTES.saturating_sub(self.as_str().len());
        let end = text
            .char_indices()
            .map(|(i, c)| i + c.len_utf8())
            .take_while(|end| *end <= remaining)
            .last()
            .unwrap_or(0);
        self.0.insert_text(&text[..end], index)
    }
    fn delete_char_range(&mut self, range: std::ops::Range<egui::text::CharIndex>) {
        self.0.delete_char_range(range);
    }
    fn clear(&mut self) {
        self.0.clear();
    }
    fn take(&mut self) -> String {
        self.0.take()
    }
    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<Self>()
    }
}

impl MobileApp {
    pub(super) fn dictation_open(&self) -> bool {
        self.dictation
            .as_ref()
            .is_some_and(|speech| speech.phase() != DictationPhase::Idle)
    }

    pub(super) fn cancel_dictation(&mut self) {
        self.dictation_send_pending = false;
        self.pending_local_dictation = None;
        if let Some(speech) = &mut self.dictation {
            speech.cancel();
        }
        self.dictation_tab = None;
        self.dictation_target = None;
        if let Some(ctx) = self.dictation_context.take() {
            if let Some(mut state) = egui::TextEdit::load_state(&ctx, draft_id()) {
                state.clear_undoer_with(|(_, text)| text.zeroize());
                state.store(&ctx, draft_id());
            }
            widgets::forget_local_text_edit(&ctx, draft_id());
        }
    }

    fn terminal_dictation_destination(&self) -> Destination {
        Destination::Terminal {
            #[cfg(feature = "live-ssh")]
            session: self
                .sink
                .live
                .connection
                .borrow()
                .as_ref()
                .and_then(|live| live.input_destination()),
        }
    }

    pub(super) fn dictation_destination_valid(&self) -> bool {
        if !self.lifecycle.get().active || self.editor.open || self.shell.sessions_sheet {
            return false;
        }
        match self.dictation_target.as_ref() {
            Some(Destination::Terminal {
                #[cfg(feature = "live-ssh")]
                session,
            }) => {
                if !self.terminal_surface()
                    || !self.terminal_input_ready(self.lifecycle.get())
                    || self.active_tab != self.dictation_tab
                {
                    return false;
                }
                #[cfg(feature = "live-ssh")]
                {
                    let current = self.sink.live.connection.borrow();
                    let current = current.as_ref().and_then(|live| live.input_destination());
                    match (session, current) {
                        (None, None) => true,
                        (Some(captured), Some(current)) => {
                            captured.ptr_eq(&current) && captured.strong_count() > 0
                        }
                        _ => false,
                    }
                }
                #[cfg(not(feature = "live-ssh"))]
                {
                    true
                }
            }
            Some(Destination::Document(target)) => {
                self.source_input()
                    && self.dictation_context.as_ref().is_some_and(|ctx| {
                        self.documents
                            .dictation_target(ctx)
                            .is_ok_and(|current| current == *target)
                    })
            }
            None => false,
        }
    }

    pub(super) fn begin_dictation(&mut self, ctx: &egui::Context) {
        let target = if self.source_input() {
            match self.documents.dictation_target(ctx) {
                Ok(target) => Destination::Document(target),
                Err(error) => {
                    self.documents.notice = Some(error.message());
                    return;
                }
            }
        } else if self.terminal_surface() && self.terminal_input_ready(self.lifecycle.get()) {
            self.terminal_dictation_destination()
        } else {
            return;
        };
        self.cancel_dictation();
        ctx.memory_mut(|memory| {
            if let Some(id) = memory.focused() {
                memory.surrender_focus(id);
            }
        });
        self.clear_sticky_modifiers();
        self.terminal_keyboard.cancel();
        self.gestures.cancel();
        self.keys_panel.open = false;
        self.menu_open = false;
        self.dictation_tab = self.active_tab;
        self.dictation_target = Some(target);
        self.dictation_context = Some(ctx.clone());
        self.dictation_owns_frame = true;
        self.dictation
            .get_or_insert_with(|| {
                let ctx = ctx.clone();
                Dictation::new(move || ctx.request_repaint())
            })
            .begin();
        ctx.request_repaint();
    }

    pub(super) fn insert_dictation(&mut self) -> bool {
        if !self.dictation_destination_valid() {
            self.input_notice = Some("The destination changed. Start dictation again.");
            self.documents.notice =
                Some("The destination changed. Start dictation again.".to_owned());
            self.cancel_dictation();
            return false;
        }
        let Some(speech) = &self.dictation else {
            return false;
        };
        let text = speech.with_text(|draft| {
            let error = if matches!(self.dictation_target, Some(Destination::Document(_))) {
                draft.document_insertion_error()
            } else {
                draft.insertion_error()
            };
            error.is_none().then(|| Zeroizing::new(draft.text.clone()))
        });
        let Some(text) = text else {
            return false;
        };
        let document = matches!(self.dictation_target, Some(Destination::Document(_)));
        let context = self.dictation_context.clone();
        match self
            .dictation_target
            .as_ref()
            .expect("verified destination")
        {
            Destination::Document(target) => {
                let result = self.documents.insert_dictation(
                    self.dictation_context.as_ref().expect("document context"),
                    target,
                    &text,
                );
                if let Err(error) = result {
                    self.documents.notice = Some(error.message());
                    return false;
                }
            }
            Destination::Terminal { .. } => {
                // Ordinary guarded paste, never Return/key synthesis.
                route_input(
                    &mut self.terminal,
                    InputEvent::Paste(text.to_string()),
                    &mut self.sink,
                );
            }
        }
        self.cancel_dictation();
        if document {
            self.return_dictation_to_source(context.as_ref().expect("document context"));
        }
        self.clear_sticky_modifiers();
        true
    }

    fn return_dictation_to_source(&mut self, ctx: &egui::Context) {
        if self.source_input() && self.lifecycle.get().active {
            if let Some(field) = self.documents.source_field() {
                // Quarantine draft input before publishing the source's native field.
                ctx.input_mut(|input| {
                    input.events.retain(|event| {
                        !matches!(
                            event,
                            egui::Event::Text(_)
                                | egui::Event::Paste(_)
                                | egui::Event::Copy
                                | egui::Event::Cut
                                | egui::Event::Ime(_)
                                | egui::Event::Key { .. }
                        )
                    });
                });
                ctx.memory_mut(|memory| memory.request_focus(field));
                self.dictation_source_handoff = true;
                ctx.request_repaint();
            }
        }
    }

    pub(super) fn show_dictation(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let Some(speech) = &mut self.dictation else {
            return;
        };
        speech.poll();
        let phase = speech.phase();
        if phase == DictationPhase::Idle {
            return;
        }
        let mut cancel = false;
        let mut stop = false;
        let mut send = false;
        let mut resume = false;
        let mut edit = false;
        let document = matches!(self.dictation_target, Some(Destination::Document(_)));
        let panel_height = panel_height(ui.available_height());
        egui::Panel::bottom("shared-dictation-composer")
            .exact_size(panel_height)
            .frame(
                egui::Frame::new()
                    .fill(super::theme::color::BAR)
                    .inner_margin(egui::Margin::same(6)),
            )
            .show(ui, |ui| {
                ui.spacing_mut().scroll.floating = false;
                ui.spacing_mut().item_spacing.y = 4.0;
                let card = egui::Frame::new()
                    .fill(super::theme::color::SHEET)
                    .stroke(egui::Stroke::new(1.0, super::theme::color::BORDER))
                    .corner_radius(egui::CornerRadius::same(super::theme::radius::CARD))
                    .inner_margin(egui::Margin::same(8));
                card.show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.add_space(2.0);
                    let grabber = egui::Rect::from_center_size(
                        egui::pos2(ui.max_rect().center().x, ui.cursor().top() + 2.0),
                        egui::vec2(34.0, 4.0),
                    );
                    ui.painter().rect_filled(
                        grabber,
                        egui::CornerRadius::same(2),
                        super::theme::color::TEXT_FAINT,
                    );
                    ui.add_space(12.0);

                    let field_height = (ui.available_height() - 58.0).clamp(30.0, 64.0);
                    let field_frame = egui::Frame::new()
                        .fill(super::theme::color::FIELD)
                        .stroke(egui::Stroke::new(
                            if phase == DictationPhase::Review {
                                1.5
                            } else {
                                1.0
                            },
                            if phase == DictationPhase::Review {
                                super::theme::color::PRIMARY
                            } else {
                                super::theme::color::BORDER
                            },
                        ))
                        .corner_radius(egui::CornerRadius::same(super::theme::radius::FIELD))
                        .inner_margin(egui::Margin::same(8));
                    field_frame.show(ui, |ui| {
                        ui.set_min_height(field_height - 16.0);
                        speech.with_text(|draft| {
                            if phase == DictationPhase::Review {
                                let mut buffer = ReviewBuffer(PasswordBuffer(
                                    std::mem::take(&mut draft.text).into(),
                                ));
                                ui.visuals_mut().text_cursor.stroke.color =
                                    super::theme::color::PRIMARY;
                                let output = widgets::local_text_edit(
                                    ui,
                                    draft_id(),
                                    egui::TextEdit::multiline(&mut buffer)
                                        .desired_width(ui.available_width())
                                        .desired_rows(2)
                                        .font(super::theme::type_scale::font(16.0))
                                        .margin(egui::Vec2::ZERO)
                                        .frame(egui::Frame::NONE)
                                        .hint_text("Text to insert"),
                                    false,
                                    true,
                                );
                                output.response.request_focus();
                                if let Some(mut state) =
                                    egui::text_edit::TextEditState::load(&ctx, output.response.id)
                                {
                                    state.clear_undoer_with(|(_, text)| text.zeroize());
                                    state.store(&ctx, output.response.id);
                                }
                                draft.text = buffer.take();
                                let error = if document {
                                    draft.document_insertion_error()
                                } else {
                                    draft.insertion_error()
                                };
                                if let Some(error) = error {
                                    widgets::footnote(ui, error);
                                }
                            } else {
                                let (rect, response) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), field_height - 16.0),
                                    egui::Sense::click(),
                                );
                                response.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::TextEdit,
                                        true,
                                        "Dictation transcript",
                                    )
                                });
                                if response.clicked() {
                                    edit = true;
                                }
                                let text = if let DictationPhase::Failed(message) = phase {
                                    message.to_owned()
                                } else if draft.text.is_empty() {
                                    "Listening on device".to_owned()
                                } else {
                                    draft.text.clone()
                                };
                                let color = if matches!(phase, DictationPhase::Failed(_)) {
                                    super::theme::color::DANGER
                                } else if draft.text.is_empty() {
                                    super::theme::color::TEXT_MUTED
                                } else {
                                    super::theme::color::TEXT
                                };
                                let galley = ui.painter().layout(
                                    text,
                                    super::theme::type_scale::font(16.0),
                                    color,
                                    rect.width(),
                                );
                                ui.painter().galley(rect.left_top(), galley, color);
                            }
                        });
                    });

                    if let DictationPhase::Failed(message) = phase {
                        ui.label(egui::RichText::new(message).color(super::theme::color::DANGER));
                        edit = ui.small_button("Edit text manually").clicked();
                    }

                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let waveform_width = (ui.available_width() - 252.0).max(16.0);
                        speech.with_text(|draft| {
                            paint_waveform(
                                ui,
                                waveform_width,
                                draft.levels(),
                                phase == DictationPhase::Listening,
                            );
                        });
                        ui.add_space(2.0);
                        ui.separator();
                        match phase {
                            DictationPhase::Listening => {
                                let response = control_button(
                                    ui,
                                    "Stop",
                                    super::theme::color::PRIMARY_PRESSED,
                                );
                                paint_stop(ui, response.rect);
                                stop = response.clicked();
                            }
                            DictationPhase::Review => {
                                let response = icon_button(
                                    ui,
                                    "Resume",
                                    super::theme::color::ICON_TILE,
                                    Icon::Microphone,
                                );
                                resume = response.clicked();
                            }
                            DictationPhase::Failed(_) => {
                                let response = icon_button(
                                    ui,
                                    "Try again",
                                    super::theme::color::ICON_TILE,
                                    Icon::Microphone,
                                );
                                resume = response.clicked();
                            }
                            _ => {
                                let response = ui.add_enabled(
                                    false,
                                    egui::Button::new("...").min_size(egui::vec2(44.0, 44.0)),
                                );
                                response.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::Button,
                                        false,
                                        "Stop",
                                    )
                                });
                            }
                        }
                        ui.add_space(2.0);
                        ui.separator();
                        let cancel_response =
                            control_button(ui, "Cancel", super::theme::color::ICON_TILE);
                        paint_cancel(ui, cancel_response.rect);
                        cancel = cancel_response.clicked();

                        let allowed = match phase {
                            DictationPhase::Listening | DictationPhase::Finishing => true,
                            DictationPhase::Review => speech.with_text(|draft| {
                                if document {
                                    draft.document_insertion_error().is_none()
                                } else {
                                    draft.insertion_error().is_none()
                                }
                            }),
                            _ => false,
                        };
                        let send_response = send_button(ui, allowed);
                        send = send_response.clicked();
                    });
                    if phase == DictationPhase::Authorizing {
                        widgets::footnote(ui, "Allow speech and microphone access.");
                    }
                    if phase != DictationPhase::Review {
                        widgets::publish_input_placeholder(ui, draft_id());
                    }
                });
            });
        if self.dictation_send_pending && phase == DictationPhase::Review {
            self.dictation_send_pending = false;
            send = true;
        } else if self.dictation_send_pending && matches!(phase, DictationPhase::Failed(_)) {
            self.dictation_send_pending = false;
        }
        if cancel {
            let document = matches!(self.dictation_target, Some(Destination::Document(_)));
            self.cancel_dictation();
            if document {
                self.return_dictation_to_source(&ctx);
            } else {
                ctx.memory_mut(|memory| {
                    if let Some(id) = memory.focused() {
                        memory.surrender_focus(id);
                    }
                });
            }
        } else if stop {
            self.dictation.as_mut().expect("visible dictation").stop();
        } else if send {
            match phase {
                DictationPhase::Listening => {
                    self.dictation_send_pending = true;
                    self.dictation.as_mut().expect("visible dictation").stop();
                }
                DictationPhase::Finishing => {
                    self.dictation_send_pending = true;
                }
                DictationPhase::Review => {
                    self.insert_dictation();
                }
                _ => {}
            }
        } else if edit {
            if matches!(phase, DictationPhase::Listening | DictationPhase::Finishing) {
                let speech = self.dictation.as_mut().expect("visible dictation");
                speech.stop();
                speech.with_text(|draft| draft.phase = DictationPhase::Review);
            } else {
                self.dictation
                    .as_ref()
                    .expect("visible dictation")
                    .with_text(|draft| draft.phase = DictationPhase::Review);
            }
        } else if resume {
            if matches!(phase, DictationPhase::Failed(_)) {
                self.dictation.as_mut().expect("visible dictation").begin();
            } else {
                self.dictation.as_mut().expect("visible dictation").resume();
            }
        }
    }
}

fn paint_waveform(
    ui: &mut egui::Ui,
    width: f32,
    levels: &festerm_ios_window::LevelHistory,
    active: bool,
) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 44.0), egui::Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Microphone level")
    });
    let values: Vec<f32> = levels.iter().collect();
    let pitch = 8.5;
    let capacity = ((rect.width() + 1.0) / pitch).floor() as usize;
    let count = capacity.min(values.len());
    if count == 0 {
        return;
    }
    let start = capacity - count;
    let color = if active {
        super::theme::color::PRIMARY
    } else {
        super::theme::color::TEXT_FAINT
    };
    for index in 0..count {
        let source_index = index * values.len() / count;
        let level = values[source_index];
        let height = 4.0 + ((level - 0.08) / 0.82).clamp(0.0, 1.0) * (rect.height() - 4.0);
        let x = rect.left() + (start + index) as f32 * pitch;
        let bar = egui::Rect::from_center_size(
            egui::pos2(x + 1.5, rect.center().y),
            egui::vec2(3.0, height),
        );
        ui.painter()
            .rect_filled(bar, egui::CornerRadius::same(2), color);
    }
}

fn icon_button(
    ui: &mut egui::Ui,
    label: &'static str,
    fill: egui::Color32,
    icon: Icon,
) -> egui::Response {
    let response = control_button(ui, label, fill);
    icon::paint(
        ui.painter(),
        icon,
        response.rect.shrink(6.0),
        super::theme::color::ON_PRIMARY,
    );
    response
}

fn control_button(ui: &mut egui::Ui, label: &'static str, fill: egui::Color32) -> egui::Response {
    let response = ui.add_sized(
        [44.0, 44.0],
        egui::Button::new("")
            .fill(fill)
            .corner_radius(egui::CornerRadius::same(10)),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    response
}

fn paint_cancel(ui: &egui::Ui, rect: egui::Rect) {
    let half = 8.0;
    let center = rect.center();
    let stroke = egui::Stroke::new(3.0, super::theme::color::DANGER);
    for (from, to) in [
        (egui::vec2(-half, -half), egui::vec2(half, half)),
        (egui::vec2(half, -half), egui::vec2(-half, half)),
    ] {
        ui.painter()
            .line_segment([center + from, center + to], stroke);
    }
}

fn paint_stop(ui: &egui::Ui, rect: egui::Rect) {
    let square = egui::Rect::from_center_size(rect.center(), egui::vec2(16.0, 16.0));
    ui.painter().rect_filled(
        square,
        egui::CornerRadius::same(2),
        super::theme::color::ON_PRIMARY,
    );
}

fn send_button(ui: &mut egui::Ui, enabled: bool) -> egui::Response {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new("")
            .fill(super::theme::color::PRIMARY)
            .corner_radius(egui::CornerRadius::same(18))
            .min_size(egui::vec2(110.0, 44.0)),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, "Send"));
    let color = super::theme::color::ON_PRIMARY;
    let center = egui::pos2(response.rect.left() + 24.0, response.rect.center().y);
    let plane = [
        center + egui::vec2(-9.0, -4.0),
        center + egui::vec2(9.0, -9.0),
        center + egui::vec2(2.0, 9.0),
        center + egui::vec2(-1.0, 2.0),
        center + egui::vec2(-9.0, -4.0),
    ];
    let stroke = egui::Stroke::new(1.8, color);
    ui.painter().add(egui::Shape::line(plane.to_vec(), stroke));
    ui.painter()
        .line_segment([plane[1], center + egui::vec2(-1.0, 2.0)], stroke);
    ui.painter().text(
        egui::pos2(response.rect.left() + 46.0, response.rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Send",
        egui::FontId::proportional(14.0),
        color,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{terminal_chrome::KeyRowAction, Lifecycle, Surface};
    use egui_kittest::kittest::Queryable;
    use std::{cell::Cell, rc::Rc};

    fn app() -> MobileApp {
        MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        })))
    }

    fn review(app: &mut MobileApp, text: &str) {
        app.dictation = Some(Dictation::new(|| {}));
        app.dictation_tab = app.active_tab;
        app.dictation_target = Some(app.terminal_dictation_destination());
        app.dictation.as_ref().unwrap().with_text(|draft| {
            draft.phase = DictationPhase::Review;
            draft.text = text.to_owned();
        });
    }

    #[test]
    fn dictation_insert_is_explicit_single_use_and_has_no_return_or_modifiers() {
        let mut app = app();
        review(&mut app, "echo hello");
        app.control = true;
        app.alt = true;
        app.shift = true;
        assert_eq!(app.sink.diagnostics.byte_count, 0);
        app.apply_key_row(
            &egui::Context::default(),
            KeyRowAction::Key(festerm_core::Key::Enter),
        );
        assert_eq!(app.sink.diagnostics.byte_count, 0);
        assert!(app.insert_dictation());
        assert_eq!(app.sink.diagnostics.byte_count, "echo hello".len() as u64);
        assert_eq!(app.sink.diagnostics.event_count, 1);
        assert!(!app.control && !app.alt && !app.shift);
        assert!(!app.dictation_open());
        assert!(!app.insert_dictation());
        assert_eq!(app.sink.diagnostics.event_count, 1);
    }

    #[test]
    fn send_while_listening_waits_for_review_then_inserts_once() {
        let mut app = app();
        review(&mut app, "send after stop");
        app.dictation
            .as_ref()
            .unwrap()
            .with_text(|draft| draft.phase = DictationPhase::Listening);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), app);
        harness.run();
        harness.get_by_label("Send").click();
        harness.run_steps(2);
        assert_eq!(
            harness.state().sink.diagnostics.byte_count,
            "send after stop".len() as u64
        );
        assert_eq!(harness.state().sink.diagnostics.event_count, 1);
        assert!(!harness.state().dictation_open());
    }

    #[test]
    fn send_while_finishing_waits_for_review_then_inserts_once() {
        let mut app = app();
        review(&mut app, "send after final recognition");
        app.dictation
            .as_ref()
            .unwrap()
            .with_text(|draft| draft.phase = DictationPhase::Finishing);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), app);
        harness.run();
        harness.get_by_label("Send").click();
        harness.run();
        assert!(harness.state().dictation_send_pending);
        assert_eq!(
            harness.state().dictation.as_ref().unwrap().phase(),
            DictationPhase::Finishing
        );
        assert_eq!(harness.state().sink.diagnostics.byte_count, 0);

        harness
            .state()
            .dictation
            .as_ref()
            .unwrap()
            .with_text(|draft| draft.phase = DictationPhase::Review);
        harness.run();
        assert_eq!(
            harness.state().sink.diagnostics.byte_count,
            "send after final recognition".len() as u64
        );
        assert_eq!(harness.state().sink.diagnostics.event_count, 1);
        assert!(!harness.state().dictation_open());
    }

    #[test]
    fn live_dictation_transcript_wraps_inside_its_field() {
        let transcript = "Review the deployment notes before continuing. ".repeat(4);
        let mut app = app();
        review(&mut app, &transcript);
        app.dictation
            .as_ref()
            .unwrap()
            .with_text(|draft| draft.phase = DictationPhase::Listening);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), app);
        harness.run();
        let rows = harness
            .output()
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(galley) if galley.galley.text() == transcript => {
                    Some(galley.galley.rows.len())
                }
                _ => None,
            })
            .unwrap();
        assert!(
            rows > 1,
            "long live transcripts must wrap into multiple lines"
        );
    }

    #[test]
    fn tapping_live_transcript_stops_capture_and_opens_editing() {
        let mut app = app();
        review(&mut app, "partial transcript");
        app.dictation
            .as_ref()
            .unwrap()
            .with_text(|draft| draft.phase = DictationPhase::Listening);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), app);
        harness.run();
        harness.get_by_label("Dictation transcript").click();
        harness.run();
        assert_eq!(
            harness.state().dictation.as_ref().unwrap().phase(),
            DictationPhase::Review
        );
    }

    #[test]
    fn dictation_close_frame_never_replays_queued_local_text_into_terminal() {
        use egui_kittest::kittest::Queryable;
        for action in ["Cancel", "Send"] {
            let mut app = app();
            app.choose_keyboard(crate::terminal_keyboard::KeyboardKind::HoldSymbols);
            review(&mut app, "safe");
            let inject = Rc::new(Cell::new(false));
            let queued = inject.clone();
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(390.0, 844.0))
                .build_ui_state(
                    move |ui, app: &mut MobileApp| {
                        if queued.get()
                            && ui.input(|input| {
                                input.events.iter().any(|event| {
                                    matches!(
                                        event,
                                        egui::Event::PointerButton { pressed: false, .. }
                                    )
                                })
                            })
                        {
                            queued.set(false);
                            ui.input_mut(|input| {
                                input.events.push(egui::Event::Text("queued local".into()))
                            });
                        }
                        app.show(ui);
                    },
                    app,
                );
            harness.state_mut().dictation_context = Some(harness.ctx.clone());
            harness.run();
            inject.set(true);
            harness.get_by_label(action).click();
            harness.run();
            assert!(!harness.state().dictation_open());
            assert_eq!(
                harness.state().sink.diagnostics.byte_count,
                if action == "Send" {
                    "safequeued local".len() as u64
                } else {
                    0
                }
            );
        }
    }

    #[test]
    fn dictation_refuses_unsafe_text_and_changed_target_or_lifecycle() {
        let mut app = app();
        for text in ["echo hi\nwhoami", "hi\r", "\u{1b}[A", "\t", "\u{2028}", ""] {
            review(&mut app, text);
            assert!(!app.insert_dictation());
            assert_eq!(app.sink.diagnostics.byte_count, 0);
        }
        review(&mut app, "safe");
        app.dictation_tab = Some(u64::MAX);
        assert!(!app.insert_dictation());
        assert!(!app.dictation_open());
        review(&mut app, "safe");
        app.lifecycle.set(Lifecycle::default());
        assert!(!app.insert_dictation());
        app.lifecycle.set(Lifecycle {
            active: true,
            ..Default::default()
        });
        review(&mut app, "safe");
        app.surface = Surface::Tabs;
        assert!(!app.insert_dictation());
        assert_eq!(app.sink.diagnostics.byte_count, 0);
    }

    #[test]
    fn dictation_navigation_and_suspension_clear_preview_before_any_insert() {
        let mut app = app();
        review(&mut app, "private fixture");
        let tab = app.active_tab.unwrap();
        app.activate_tab(tab);
        assert!(!app.dictation_open());
        review(&mut app, "private fixture");
        app.switch_workspace(crate::workspaces::Workspace::Files);
        assert!(!app.dictation_open());
        assert!(app
            .dictation
            .as_ref()
            .unwrap()
            .with_text(|draft| draft.text.is_empty()));
        assert_eq!(app.sink.diagnostics.byte_count, 0);
        app.switch_workspace(crate::workspaces::Workspace::Terminal);
        review(&mut app, "private fixture");
        let mut lifecycle = app.lifecycle.get();
        lifecycle.suspend();
        lifecycle.active = true;
        app.lifecycle.set(lifecycle);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), app);
        harness.run();
        assert!(!harness.state().dictation_open());
        assert_eq!(harness.state().sink.diagnostics.byte_count, 0);
    }

    fn native_lifecycle(app: &MobileApp) -> crate::NativeLifecycle {
        crate::NativeLifecycle::new(
            app.lifecycle.clone(),
            #[cfg(feature = "live-ssh")]
            app.sink.live.clone(),
        )
    }

    fn authorizing_frames(app: MobileApp) -> egui_kittest::Harness<'static, MobileApp> {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), app);
        harness.run_steps(2);
        harness
    }

    #[test]
    fn dictation_pending_authorization_survives_temporary_permission_resignation() {
        let mut app = app();
        review(&mut app, "");
        app.dictation.as_ref().unwrap().with_text(|draft| {
            draft.phase = DictationPhase::Authorizing;
        });
        let mut native = native_lifecycle(&app);
        // The permission alert resigns active; winit reports Suspended, then Resumed.
        assert!(!native.suspended(true));
        let harness = authorizing_frames(app);
        assert!(!native.resumed());
        assert_eq!(
            harness.state().dictation.as_ref().unwrap().phase(),
            DictationPhase::Authorizing
        );
        assert_eq!(native.get().suspensions, 0);
        assert_eq!(harness.state().sink.diagnostics.byte_count, 0);
    }

    #[test]
    fn dictation_permission_alert_then_background_still_suspends_and_cancels() {
        let mut app = app();
        review(&mut app, "");
        app.dictation.as_ref().unwrap().with_text(|draft| {
            draft.phase = DictationPhase::Authorizing;
        });
        let mut native = native_lifecycle(&app);
        assert!(!native.suspended(true));
        assert!(native.entered_background());
        assert!(!native.entered_background());
        assert!(!native.get().active);
        assert_eq!(native.get().suspensions, 1);
        assert!(native.resumed());
        let harness = authorizing_frames(app);
        assert!(!harness.state().dictation_open());
        // An ordinary resign (no permission alert) suspends immediately.
        assert!(native.suspended(false));
        assert!(!native.entered_background());
        assert_eq!(native.get().suspensions, 2);
    }

    #[test]
    fn dictation_editor_bounds_unicode_bytes_without_splitting_characters() {
        let mut buffer = ReviewBuffer(PasswordBuffer::default());
        let inserted =
            buffer.insert_text(&"é".repeat(MAX_DICTATION_BYTES), egui::text::CharIndex(0));
        assert_eq!(inserted, MAX_DICTATION_BYTES / 2);
        assert_eq!(buffer.as_str().len(), MAX_DICTATION_BYTES);
        assert_eq!(buffer.insert_text("a", egui::text::CharIndex(0)), 0);
        buffer.delete_char_range(egui::text::CharIndex(0)..egui::text::CharIndex(1));
        assert_eq!(buffer.insert_text("🙂", egui::text::CharIndex(0)), 0);
        assert_eq!(buffer.insert_text("é", egui::text::CharIndex(0)), 1);
        assert_eq!(buffer.as_str().len(), MAX_DICTATION_BYTES);
    }

    #[test]
    fn dictation_failed_and_manual_review_sheets_render_without_terminal_input() {
        let mut app = app();
        app.begin_dictation(&egui::Context::default());
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), app);
        harness.run();
        use egui_kittest::kittest::Queryable;
        assert!(harness.query_by_label("Edit text manually").is_some());
        harness.get_by_label("Edit text manually").click();
        harness.run();
        assert!(harness.ctx.memory(|memory| memory.has_focus(draft_id())));
        assert_eq!(
            harness.state().dictation.as_ref().unwrap().phase(),
            DictationPhase::Review
        );
        assert_eq!(harness.state().sink.diagnostics.byte_count, 0);
    }

    #[test]
    fn beginning_dictation_surrenders_the_focused_input() {
        let mut app = app();
        let ctx = egui::Context::default();
        let field = egui::Id::new("focused-input");
        ctx.memory_mut(|memory| memory.request_focus(field));

        app.begin_dictation(&ctx);

        assert!(!ctx.memory(|memory| memory.has_focus(field)));
    }
}
