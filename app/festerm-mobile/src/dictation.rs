use super::{password_buffer::PasswordBuffer, route_input, widgets, MobileApp};
use eframe::egui::{self, TextBuffer};
use festerm_core::InputEvent;
use festerm_ios_window::{Dictation, DictationPhase, MAX_DICTATION_BYTES};
use zeroize::{Zeroize, Zeroizing};

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
        let mut retry = false;
        let mut edit = false;
        let mut insert = false;
        let mut resume = false;
        let document = matches!(self.dictation_target, Some(Destination::Document(_)));
        let draft_height = (ui.available_height() * 0.25).clamp(28.0, 76.0);
        let panel_height = (ui.available_height() * 0.4).clamp(98.0, 170.0);
        egui::Panel::bottom("shared-dictation-composer")
            .exact_size(panel_height)
            .frame(
                egui::Frame::new()
                    .fill(super::theme::color::BAR)
                    .inner_margin(egui::Margin::same(8)),
            )
            .show(ui, |ui| {
                ui.spacing_mut().scroll.floating = false;
                if panel_height <= 110.0 {
                    ui.spacing_mut().item_spacing.y = 2.0;
                }
                ui.horizontal(|ui| {
                    ui.strong(match phase {
                        DictationPhase::Listening => "Listening",
                        DictationPhase::Finishing => "Finishing...",
                        DictationPhase::Authorizing => "Permission",
                        DictationPhase::Failed(_) => "Dictation unavailable",
                        _ => "Review",
                    });
                    cancel = ui
                        .add_sized([64.0, 44.0], egui::Button::new("Cancel"))
                        .clicked();
                    if phase == DictationPhase::Listening {
                        stop = ui
                            .add_sized([64.0, 44.0], egui::Button::new("Stop"))
                            .clicked();
                    } else if phase == DictationPhase::Review {
                        resume = ui
                            .add_sized([64.0, 44.0], egui::Button::new("Resume"))
                            .clicked();
                        let allowed = speech.with_text(|draft| {
                            if document {
                                draft.document_insertion_error().is_none()
                            } else {
                                draft.insertion_error().is_none()
                            }
                        });
                        let response = ui.add_enabled(
                            allowed,
                            egui::Button::new("Insert").min_size(egui::vec2(64.0, 44.0)),
                        );
                        insert = response.clicked();
                    }
                });
                match phase {
                    DictationPhase::Authorizing => {
                        ui.label("Allow speech and microphone access.");
                    }
                    DictationPhase::Listening => {
                        speech.with_text(|draft| {
                            let label = match draft.audio_level {
                                Some(level) if level > 0.01 => "Microphone level",
                                Some(_) => "Quiet input",
                                None => "Waiting for microphone level",
                            };
                            ui.add(
                                egui::ProgressBar::new(draft.audio_level.unwrap_or(0.0))
                                    .desired_height(if panel_height <= 110.0 { 14.0 } else { 18.0 })
                                    .text(label),
                            );
                            egui::ScrollArea::vertical()
                                .max_height(draft_height)
                                .show(ui, |ui| {
                                    ui.label(if draft.text.is_empty() {
                                        "Listening on device - up to 45 seconds"
                                    } else {
                                        &draft.text
                                    });
                                });
                        });
                    }
                    DictationPhase::Finishing => {
                        speech.with_text(|draft| {
                            egui::ScrollArea::vertical()
                                .max_height(draft_height)
                                .show(ui, |ui| {
                                    ui.label(&draft.text);
                                });
                        });
                    }
                    DictationPhase::Review => {
                        speech.with_text(|draft| {
                            let mut buffer = ReviewBuffer(PasswordBuffer(
                                std::mem::take(&mut draft.text).into(),
                            ));
                            let output = egui::ScrollArea::vertical()
                                .max_height(draft_height)
                                .show(ui, |ui| {
                                    widgets::local_text_edit(
                                        ui,
                                        draft_id(),
                                        egui::TextEdit::multiline(&mut buffer)
                                            .desired_width(ui.available_width())
                                            .desired_rows(if panel_height <= 110.0 { 1 } else { 2 })
                                            .hint_text("Text to insert"),
                                        false,
                                        true,
                                    )
                                })
                                .inner;
                            let response = output.response;
                            response.request_focus();
                            if let Some(mut state) =
                                egui::text_edit::TextEditState::load(&ctx, response.id)
                            {
                                state.clear_undoer_with(|(_, text)| text.zeroize());
                                state.store(&ctx, response.id);
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
                        });
                    }
                    DictationPhase::Failed(message) => {
                        ui.label(egui::RichText::new(message).color(super::theme::color::DANGER));
                        retry = ui.button("Try again").clicked();
                        edit = ui.button("Edit text manually").clicked();
                    }
                    DictationPhase::Idle => {}
                }
                if phase != DictationPhase::Review {
                    widgets::publish_input_placeholder(ui, draft_id());
                }
            });
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
        } else if retry {
            self.dictation.as_mut().expect("visible dictation").begin();
        } else if edit {
            self.dictation
                .as_ref()
                .expect("visible dictation")
                .with_text(|draft| draft.phase = DictationPhase::Review);
        } else if insert {
            self.insert_dictation();
        } else if resume {
            self.dictation.as_mut().expect("visible dictation").resume();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{terminal_chrome::KeyRowAction, Lifecycle, Surface};
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
    fn dictation_close_frame_never_replays_queued_local_text_into_terminal() {
        use egui_kittest::kittest::Queryable;
        for action in ["Cancel", "Insert"] {
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
                if action == "Insert" {
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
}
