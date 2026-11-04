use super::{password_buffer::PasswordBuffer, route_input, widgets, MobileApp};
use eframe::egui::{self, TextBuffer};
use festerm_core::InputEvent;
use festerm_ios_window::{Dictation, DictationPhase, MAX_DICTATION_BYTES};
use zeroize::{Zeroize, Zeroizing};

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
        if let Some(speech) = &mut self.dictation {
            speech.cancel();
        }
        self.dictation_tab = None;
    }

    pub(super) fn begin_dictation(&mut self, ctx: &egui::Context) {
        if !self.terminal_surface() || !self.terminal_input_ready(self.lifecycle.get()) {
            return;
        }
        self.clear_sticky_modifiers();
        self.terminal_keyboard.cancel();
        self.gestures.cancel();
        self.keys_panel.open = false;
        self.menu_open = false;
        self.dictation_tab = self.active_tab;
        self.dictation
            .get_or_insert_with(|| {
                let ctx = ctx.clone();
                Dictation::new(move || ctx.request_repaint())
            })
            .begin();
    }

    fn insert_dictation(&mut self) -> bool {
        if !self.terminal_surface()
            || !self.terminal_input_ready(self.lifecycle.get())
            || self.active_tab != self.dictation_tab
        {
            self.cancel_dictation();
            return false;
        }
        let Some(speech) = &self.dictation else {
            return false;
        };
        let text = speech.with_text(|draft| {
            draft
                .insertion_error()
                .is_none()
                .then(|| Zeroizing::new(std::mem::take(&mut draft.text)))
        });
        let Some(text) = text else {
            return false;
        };
        self.cancel_dictation();
        self.clear_sticky_modifiers();
        // Use the ordinary shared-core paste path, never Return/key synthesis.
        route_input(
            &mut self.terminal,
            InputEvent::Paste(text.to_string()),
            &mut self.sink,
        );
        true
    }

    pub(super) fn show_dictation(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
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
        let sheet = widgets::bottom_sheet(ctx, "on-device-dictation", bounds, |ui| {
            cancel |= widgets::sheet_title(ui, "On-device dictation", Some("Cancel"));
            widgets::footnote(
                ui,
                "Audio stays on this device. Review before inserting. Not for passwords.",
            );
            ui.add_space(12.0);
            match phase {
                DictationPhase::Authorizing => {
                    ui.spinner();
                    ui.label("Waiting for speech and microphone permission...");
                }
                DictationPhase::Listening => {
                    ui.spinner();
                    ui.label("Listening - 45 second maximum");
                    speech.with_text(|draft| {
                        ui.label(&draft.text);
                    });
                    stop = ui.button("Stop and review").clicked();
                }
                DictationPhase::Review => {
                    speech.with_text(|draft| {
                        let mut buffer =
                            ReviewBuffer(PasswordBuffer(std::mem::take(&mut draft.text).into()));
                        let width = ui.available_width();
                        let response = widgets::text_field_with_limit(
                            ui,
                            "Review text",
                            &mut buffer,
                            "Text to insert",
                            width,
                            MAX_DICTATION_BYTES,
                        );
                        if let Some(mut state) =
                            egui::text_edit::TextEditState::load(ctx, response.id)
                        {
                            state.clear_undoer_with(|(_, text)| text.zeroize());
                            state.store(ctx, response.id);
                        }
                        draft.text = buffer.take();
                        widgets::footnote(
                            ui,
                            "Maximum 4096 UTF-8 bytes. Extra characters are not inserted.",
                        );
                        if let Some(error) = draft.insertion_error() {
                            widgets::footnote(ui, error);
                        }
                        insert = ui
                            .add_enabled(
                                draft.insertion_error().is_none(),
                                egui::Button::new("Insert"),
                            )
                            .clicked();
                    });
                    widgets::footnote(ui, "Insert sends text only. Press Return separately if you intend to submit it.");
                }
                DictationPhase::Failed(message) => {
                    ui.label(egui::RichText::new(message).color(super::theme::color::DANGER));
                    retry = ui.button("Try again").clicked();
                    edit = ui.button("Edit text manually").clicked();
                    widgets::footnote(ui, "Manual editing can exercise the preview on Simulator; it is not speech recognition.");
                }
                DictationPhase::Idle => {}
            }
        });
        if cancel || sheet.dismissed {
            self.cancel_dictation();
            ctx.memory_mut(|memory| {
                if let Some(id) = memory.focused() {
                    memory.surrender_focus(id);
                }
            });
        } else if stop {
            self.dictation.as_mut().expect("visible dictation").stop();
        } else if retry {
            self.begin_dictation(ctx);
        } else if edit {
            self.dictation
                .as_ref()
                .expect("visible dictation")
                .with_text(|draft| draft.phase = DictationPhase::Review);
        } else if insert {
            self.insert_dictation();
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

    #[test]
    fn dictation_pending_authorization_survives_temporary_permission_resignation() {
        let mut app = app();
        review(&mut app, "");
        app.dictation.as_ref().unwrap().with_text(|draft| {
            draft.phase = DictationPhase::Authorizing;
        });
        let mut lifecycle = app.lifecycle.get();
        lifecycle.suspend();
        lifecycle.active = true;
        app.lifecycle.set(lifecycle);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), app);
        harness.run_steps(2);
        assert_eq!(
            harness.state().dictation.as_ref().unwrap().phase(),
            DictationPhase::Authorizing
        );
        assert_eq!(harness.state().sink.diagnostics.byte_count, 0);
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
        assert!(harness.query_by_label("REVIEW TEXT").is_some());
        assert_eq!(
            harness.state().dictation.as_ref().unwrap().phase(),
            DictationPhase::Review
        );
        assert_eq!(harness.state().sink.diagnostics.byte_count, 0);
    }
}
