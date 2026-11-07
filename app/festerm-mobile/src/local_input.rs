use super::{
    terminal_chrome::{KeyRowAction, KeyRowState},
    terminal_keyboard::{KeyboardContext, KeyboardKind},
    theme, MobileApp, Surface,
};
use eframe::egui;
use festerm_core::Key;
use festerm_ios_window::DictationPhase;

impl MobileApp {
    pub(super) fn source_input(&self) -> bool {
        self.surface == Surface::Documents
            && !self.editor.open
            && !self.shell.sessions_sheet
            && self.tab_menu.is_none()
            && self.confirm_close.is_none()
            && self.documents.source_field().is_some()
    }

    fn local_field(&self) -> Option<egui::Id> {
        if self.dictation_open() {
            Some(super::dictation::draft_id())
        } else if self.source_input() {
            self.documents.source_field()
        } else {
            None
        }
    }

    pub(super) fn show_local_input(&mut self, ui: &mut egui::Ui) {
        if !self.lifecycle.get().active || (!self.dictation_open() && !self.source_input()) {
            return;
        }
        let context = if self.dictation_open() {
            KeyboardContext::Draft
        } else {
            KeyboardContext::Source
        };
        let height = ui.available_height()
            - if self.dictation_open() {
                super::dictation::panel_height(ui.available_height())
            } else {
                0.0
            };
        let ready = if context == KeyboardContext::Draft {
            self.dictation
                .as_ref()
                .is_some_and(|speech| speech.phase() == DictationPhase::Review)
        } else {
            self.local_field()
                .is_some_and(|id| ui.ctx().memory(|memory| memory.has_focus(id)))
        };
        let mut action = KeyRowAction::None;
        egui::Panel::bottom("shared-local-keyboard")
            .frame(egui::Frame::new().fill(theme::color::TERMINAL_CHROME))
            .show_separator_line(false)
            .show(ui, |ui| {
                if self.keyboard_kind != KeyboardKind::Native && self.keyboard_visible {
                    let state = KeyRowState {
                        control: false,
                        alt: false,
                        shift: self.shift,
                        panel_open: false,
                        keyboard_visible: true,
                        paste: self.caps.paste,
                    };
                    let output = ui
                        .add_enabled_ui(ready, |ui| {
                            self.terminal_keyboard.show_for(
                                ui,
                                self.keyboard_kind,
                                &state,
                                height,
                                context,
                            )
                        })
                        .inner;
                    action = output.action;
                    if output.click {
                        if let Some(keyboard) = &self.keyboard {
                            keyboard.play_input_click();
                        }
                    }
                } else {
                    use super::key_press::{paint_glyph, Glyph};
                    use festerm_ui_egui::icon::Icon;
                    ui.horizontal(|ui| {
                        for (label, glyph, next, enabled) in [
                            ("Tab key", Glyph::text("Tab"), KeyRowAction::Tab, ready),
                            (
                                "Left key",
                                Glyph::Arrow(Key::ArrowLeft),
                                KeyRowAction::Key(Key::ArrowLeft),
                                ready,
                            ),
                            (
                                "Right key",
                                Glyph::Arrow(Key::ArrowRight),
                                KeyRowAction::Key(Key::ArrowRight),
                                ready,
                            ),
                            (
                                "Mic",
                                Glyph::Icon(Icon::Microphone),
                                KeyRowAction::Dictate,
                                ready,
                            ),
                            (
                                "Switch keyboard",
                                Glyph::Grid,
                                KeyRowAction::SwitchKeyboard,
                                true,
                            ),
                            (
                                if self.keyboard_visible {
                                    "Hide keyboard"
                                } else {
                                    "Show keyboard"
                                },
                                Glyph::Icon(Icon::KeyboardShortcuts),
                                KeyRowAction::ToggleKeyboard,
                                true,
                            ),
                        ] {
                            let response = ui
                                .add_enabled_ui(enabled, |ui| {
                                    super::terminal_chrome::key_button(
                                        ui,
                                        44.0,
                                        44.0,
                                        label,
                                        false,
                                        |painter, rect, tint| {
                                            paint_glyph(painter, &glyph, rect.center(), 1.0, tint)
                                        },
                                    )
                                })
                                .inner;
                            if self.terminal_keyboard.keys.toggle(ui, &response, glyph) {
                                action = next;
                            }
                        }
                    });
                }
            });
        self.apply_local_key(ui.ctx(), action);
        self.show_dictation(ui);
    }

    pub(super) fn apply_local_key(&mut self, ctx: &egui::Context, action: KeyRowAction) {
        match action {
            KeyRowAction::None => return,
            KeyRowAction::SelectKeyboard(kind) => {
                self.choose_keyboard(kind);
                return;
            }
            KeyRowAction::SwitchKeyboard => {
                self.choose_keyboard(if self.keyboard_kind == KeyboardKind::Native {
                    self.last_custom_keyboard
                } else {
                    KeyboardKind::Native
                });
                return;
            }
            KeyRowAction::ToggleKeyboard => {
                self.keyboard_visible = !self.keyboard_visible;
                self.terminal_keyboard.cancel();
                return;
            }
            _ => {}
        }
        let Some(field) = self.local_field() else {
            return;
        };
        if !ctx.memory(|memory| memory.has_focus(field)) {
            return;
        }
        if self.dictation_open()
            && !self
                .dictation
                .as_ref()
                .is_some_and(|speech| speech.phase() == DictationPhase::Review)
        {
            return;
        }
        if action == KeyRowAction::Dictate {
            self.pending_local_dictation = Some(field);
            return;
        }
        if action == KeyRowAction::ToggleShift {
            self.shift = !self.shift;
            return;
        }
        let modifiers = egui::Modifiers {
            shift: self.shift || action == KeyRowAction::Outdent,
            command: matches!(
                action,
                KeyRowAction::Undo | KeyRowAction::Redo | KeyRowAction::SelectAll
            ),
            ..Default::default()
        };
        let key = match action {
            KeyRowAction::Undo => Some(egui::Key::Z),
            KeyRowAction::Redo => Some(egui::Key::Y),
            KeyRowAction::SelectAll => Some(egui::Key::A),
            KeyRowAction::Tab | KeyRowAction::Outdent | KeyRowAction::Key(Key::Tab) => {
                Some(egui::Key::Tab)
            }
            KeyRowAction::Key(Key::Enter) => Some(egui::Key::Enter),
            KeyRowAction::Key(Key::Backspace) => Some(egui::Key::Backspace),
            KeyRowAction::Key(Key::ArrowLeft) => Some(egui::Key::ArrowLeft),
            KeyRowAction::Key(Key::ArrowUp) => Some(egui::Key::ArrowUp),
            KeyRowAction::Key(Key::ArrowDown) => Some(egui::Key::ArrowDown),
            KeyRowAction::Key(Key::ArrowRight) => Some(egui::Key::ArrowRight),
            _ => None,
        };
        ctx.input_mut(|input| {
            if let Some(key) = key {
                for pressed in [true, false] {
                    input.events.push(egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers,
                    });
                }
            } else {
                match action {
                    KeyRowAction::Key(Key::Character(character)) => {
                        input.events.push(egui::Event::Text(character.to_string()));
                        self.shift = false;
                    }
                    KeyRowAction::Paste => {
                        if let Some(text) = festerm_ios_window::clipboard_text() {
                            input.events.push(egui::Event::Paste(text));
                        } else {
                            self.input_notice = Some("The clipboard has no text to paste");
                        }
                    }
                    KeyRowAction::Copy => input.events.push(egui::Event::Copy),
                    _ => {}
                }
            }
        });
    }

    pub(super) fn finish_local_dictation_action(&mut self, ctx: &egui::Context) {
        let Some(field) = self.pending_local_dictation.take() else {
            return;
        };
        if !self.lifecycle.get().active
            || self.local_field() != Some(field)
            || !ctx.memory(|memory| memory.has_focus(field))
        {
            return;
        }
        if self.dictation_open() {
            if self.dictation_destination_valid() {
                self.dictation.as_mut().expect("active dictation").resume();
            }
        } else {
            self.begin_dictation(ctx);
        }
    }
}
