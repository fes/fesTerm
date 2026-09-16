//! Application-key matching and Settings presentation; protocol bytes stay in core.
use eframe::egui::{self, Key, Modifiers};
use festerm_config::{Chord, KeyboardAction as Action, KeyboardBindings};

pub fn chord(text: &str) -> Option<(Modifiers, Key)> {
    let parsed = Chord::parse(text, cfg!(target_os = "macos")).ok()??;
    let key = Key::from_name(parsed.key)?;
    Some((
        Modifiers {
            ctrl: parsed.ctrl,
            mac_cmd: parsed.command,
            command: if cfg!(target_os = "macos") {
                parsed.command
            } else {
                parsed.ctrl
            },
            alt: parsed.alt,
            shift: parsed.shift,
        },
        key,
    ))
}

pub fn label(bindings: &KeyboardBindings, action: Action) -> Option<String> {
    let text = bindings.effective(action, cfg!(target_os = "macos"));
    (!text.is_empty()).then(|| {
        text.replace(
            "Primary",
            if cfg!(target_os = "macos") {
                "⌘"
            } else {
                "Ctrl"
            },
        )
    })
}

/// Exact modifiers, unlike egui's logical subset matching. Always remove
/// repeats and releases too, so a tab switch cannot leak its held chord.
pub fn consume(context: &egui::Context, bindings: &KeyboardBindings, action: Action) -> bool {
    let Some((expected, key)) = chord(bindings.effective(action, cfg!(target_os = "macos"))) else {
        return false;
    };
    let captured = consume_exact(context, expected, key);
    if captured {
        let modifiers = u8::from(expected.shift)
            | (u8::from(expected.alt) << 1)
            | (u8::from(expected.ctrl) << 2)
            | (u8::from(expected.mac_cmd) << 3);
        festerm_ui_egui::routing_trace::record_local(
            context,
            action.title(),
            "shortcut-captured",
            modifiers,
        );
    }
    captured
}

pub fn consume_exact(context: &egui::Context, expected: Modifiers, key: Key) -> bool {
    let mut activate = false;
    let mut captured_clipboard = 0;
    context.input_mut(|input| {
        input.events.retain(|event| {
            if captured_clipboard != 0 && semantic_clipboard_kind(event) == captured_clipboard {
                captured_clipboard = 0;
                return false;
            }

            captured_clipboard = 0;
            match event {
                egui::Event::Key {
                    pressed, repeat, ..
                } if key_matches(event, expected, key) => {
                    activate |= *pressed && !*repeat;
                    captured_clipboard = clipboard_key_kind(event);
                    false
                }
                _ => true,
            }
        });
    });
    activate
}

#[derive(Clone, Copy)]
pub struct ShortcutContext {
    pub blocked: bool,
    pub palette_open: bool,
    pub terminal_input: bool,
    pub markdown_viewer: bool,
    pub open_markdown: bool,
    pub port_forward_available: bool,
    pub tab_count: usize,
}

impl ShortcutContext {
    pub fn consume(
        self,
        context: &egui::Context,
        bindings: &KeyboardBindings,
        action: Action,
    ) -> bool {
        self.allows(action) && consume(context, bindings, action)
    }

    /// Shared by dispatch and paste-barrier cancellation, not just catalogue lookup.
    pub fn allows(self, action: Action) -> bool {
        if self.blocked {
            return false;
        }
        if action == Action::CommandPalette {
            return true;
        }
        if let Some(index) = QUICK_ACTIONS
            .iter()
            .position(|candidate| *candidate == action)
        {
            return index < self.tab_count;
        }
        if self.palette_open {
            return false;
        }
        match action.scope() {
            festerm_config::KeyboardScope::Global => true,
            festerm_config::KeyboardScope::Terminal => {
                self.terminal_input
                    && (action != Action::PortForwardManager || self.port_forward_available)
            }
            festerm_config::KeyboardScope::Markdown => self.markdown_viewer,
            festerm_config::KeyboardScope::Document => self.open_markdown,
        }
    }

    pub fn activates(
        self,
        event: &egui::Event,
        bindings: &KeyboardBindings,
        global_only: bool,
    ) -> bool {
        if self.blocked
            || !matches!(
                event,
                egui::Event::Key {
                    pressed: true,
                    repeat: false,
                    ..
                }
            )
        {
            return false;
        }
        if key_matches(event, Modifiers::CTRL | Modifiers::SHIFT, Key::F12) {
            return true;
        }
        Action::ALL.into_iter().any(|action| {
            self.allows(action)
                && (!global_only
                    || matches!(
                        action.scope(),
                        festerm_config::KeyboardScope::Global
                            | festerm_config::KeyboardScope::Document
                    ))
                && chord(bindings.effective(action, cfg!(target_os = "macos")))
                    .is_some_and(|(expected, key)| key_matches(event, expected, key))
        })
    }
}

fn key_matches(event: &egui::Event, expected: Modifiers, wanted: Key) -> bool {
    let egui::Event::Key { key, modifiers, .. } = event else {
        return false;
    };
    *key == wanted
        && (modifiers.ctrl || (!cfg!(target_os = "macos") && modifiers.command)) == expected.ctrl
        && (modifiers.mac_cmd || (cfg!(target_os = "macos") && modifiers.command))
            == expected.mac_cmd
        && modifiers.alt == expected.alt
        && (modifiers.shift == expected.shift || *key == Key::Plus)
}

/// Remove only clipboard events derived from an adjacent native key.
/// Unpaired events retain widget/RequestPaste intent, not terminal authorization.
pub fn prepare_terminal_events(context: &egui::Context) {
    context.input_mut(|input| {
        let mut preceding_clipboard = 0;
        input.events.retain(|event| {
            let derived =
                preceding_clipboard != 0 && semantic_clipboard_kind(event) == preceding_clipboard;
            preceding_clipboard = clipboard_key_kind(event);
            !derived
        });
    });
}

pub fn paired_paste(context: &egui::Context) -> Option<String> {
    context.input(|input| {
        input.events.windows(2).find_map(|pair| {
            if clipboard_key_kind(&pair[0]) == 3 {
                if let egui::Event::Paste(text) = &pair[1] {
                    return Some(text.clone());
                }
            }
            None
        })
    })
}

fn semantic_clipboard_kind(event: &egui::Event) -> u8 {
    match event {
        egui::Event::Copy => 1,
        egui::Event::Cut => 2,
        egui::Event::Paste(_) => 3,
        _ => 0,
    }
}

fn clipboard_key_kind(event: &egui::Event) -> u8 {
    let egui::Event::Key {
        key,
        modifiers,
        pressed: true,
        ..
    } = event
    else {
        return 0;
    };
    if modifiers.ctrl && modifiers.alt {
        return 0;
    }
    let primary =
        modifiers.command || modifiers.mac_cmd || (!cfg!(target_os = "macos") && modifiers.ctrl);
    match key {
        Key::Copy => 1,
        Key::Cut => 2,
        Key::Paste => 3,
        Key::C if primary => 1,
        Key::X if primary => 2,
        Key::V if primary => 3,
        Key::Insert if cfg!(windows) && modifiers.ctrl => 1,
        Key::Delete if cfg!(windows) && modifiers.shift => 2,
        Key::Insert if cfg!(windows) && modifiers.shift => 3,
        _ => 0,
    }
}

pub fn composition_active(context: &egui::Context, surface: u64) -> bool {
    let id = egui::Id::new("keyboard-ime-composition");
    let owner = (surface, context.memory(|memory| memory.focused()));
    context.data(|data| data.get_temp::<(u64, Option<egui::Id>)>(id) == Some(owner))
}

pub fn composition_owns_keys(context: &egui::Context, surface: u64) -> bool {
    let id = egui::Id::new("keyboard-ime-composition");
    let owner = (surface, context.memory(|memory| memory.focused()));
    let mut composing = composition_active(context, surface);
    let mut owned = composing;
    context.input(|input| {
        for event in &input.events {
            match event {
                egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                    composing = !text.is_empty();
                    owned |= composing;
                }
                egui::Event::Ime(egui::ImeEvent::Commit(_)) | egui::Event::WindowFocused(false) => {
                    composing = false
                }
                egui::Event::Key {
                    key: Key::Escape,
                    pressed: true,
                    ..
                } => composing = false,
                _ => {}
            }
        }
    });
    context.data_mut(|data| {
        if composing {
            data.insert_temp(id, owner);
        } else {
            data.remove::<(u64, Option<egui::Id>)>(id);
        }
    });
    owned
}

pub const QUICK_ACTIONS: [Action; 9] = [
    Action::Quick1,
    Action::Quick2,
    Action::Quick3,
    Action::Quick4,
    Action::Quick5,
    Action::Quick6,
    Action::Quick7,
    Action::Quick8,
    Action::Quick9,
];

#[derive(Clone, Default)]
struct Editor {
    search: String,
    selected: Option<Action>,
    draft: String,
    feedback: Option<String>,
}

pub fn show_editor(
    ui: &mut egui::Ui,
    bindings: &KeyboardBindings,
) -> Option<crate::tabs::AppCommand> {
    let id = ui.make_persistent_id("keyboard-editor");
    let mut editor = ui.data(|data| data.get_temp::<Editor>(id).unwrap_or_default());
    let mut replacement = None;
    ui.label("Application shortcuts");
    ui.label("Unbind to allow existing terminal encoding. This does not remap terminal bytes.");
    ui.label("Recovery: Ctrl+Shift+F12 opens Settings even after customization.");
    ui.label("Primary means Command on macOS, Ctrl elsewhere. Global bindings precede widget shortcuts; modal dialogs take precedence.");
    let search_label = ui.label("Search keyboard actions");
    let search = ui
        .add(egui::TextEdit::singleline(&mut editor.search).hint_text("Action name"))
        .labelled_by(search_label.id);
    if ui
        .ctx()
        .data_mut(|data| data.remove_temp::<bool>(egui::Id::new("keyboard-settings-recovery")))
        .unwrap_or(false)
    {
        search.request_focus();
        search.scroll_to_me(Some(egui::Align::Center));
    }
    if ui.button("Reset all keyboard bindings").clicked() {
        replacement = Some(KeyboardBindings::default());
        editor.selected = None;
        editor.feedback = None;
    }
    let query = editor.search.to_lowercase();
    egui::ScrollArea::vertical()
        .id_salt("keyboard-action-list")
        .max_height(220.0)
        .show(ui, |ui| {
            for action in Action::ALL {
                if !action.title().to_lowercase().contains(&query) {
                    continue;
                }
                ui.push_id(action as usize, |ui| {
                    let current = label(bindings, action).unwrap_or_else(|| "Unbound".into());
                    let title = format!("{} — {}", action.title(), current);
                    if ui
                        .selectable_label(editor.selected == Some(action), title)
                        .clicked()
                    {
                        editor.selected = Some(action);
                        editor.draft = bindings.effective(action, cfg!(target_os = "macos")).into();
                        editor.feedback = None;
                    }
                });
            }
        });
    if let Some(action) = editor.selected {
        ui.separator();
        ui.strong(action.title());
        ui.label(format!(
            "Scope: {:?} · Default: {}",
            action.scope(),
            label(&KeyboardBindings::default(), action).unwrap_or_else(|| "Unbound".into())
        ));
        let chord_label = ui.label("Binding chord");
        ui.add(egui::TextEdit::singleline(&mut editor.draft).hint_text("Primary+Shift+P"))
            .labelled_by(chord_label.id);
        ui.horizontal_wrapped(|ui| {
            let mut update = None;
            if ui.button("Assign binding").clicked() {
                update = Some(Some(editor.draft.clone()));
            }
            if ui.button("Unbind action").clicked() {
                update = Some(Some(String::new()));
            }
            if ui.button("Reset action").clicked() {
                update = Some(None);
            }
            if let Some(update) = update {
                let mut candidate = bindings.clone();
                candidate.set(action, update);
                match candidate.validate(cfg!(target_os = "macos")) {
                    Ok(()) => {
                        editor.draft = candidate
                            .effective(action, cfg!(target_os = "macos"))
                            .into();
                        editor.feedback = None;
                        replacement = Some(candidate);
                    }
                    Err(error) => editor.feedback = Some(error.into()),
                }
            }
        });
    }
    if let Some(feedback) = &editor.feedback {
        ui.colored_label(festerm_ui_egui::theme::STATUS_ERROR, feedback);
    }
    ui.data_mut(|data| data.insert_temp(id, editor));
    replacement.map(crate::tabs::AppCommand::SetKeyboardBindings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    #[test]
    fn keyboard_cancellation_and_consumption_share_contextual_eligibility() {
        let terminal = ShortcutContext {
            blocked: false,
            palette_open: false,
            terminal_input: true,
            markdown_viewer: false,
            open_markdown: false,
            port_forward_available: false,
            tab_count: 1,
        };
        let cases = [
            (Action::MarkdownFind, terminal, false),
            (
                Action::MarkdownFind,
                ShortcutContext {
                    markdown_viewer: true,
                    terminal_input: false,
                    ..terminal
                },
                true,
            ),
            (Action::OpenMarkdownFile, terminal, false),
            (
                Action::OpenMarkdownFile,
                ShortcutContext {
                    open_markdown: true,
                    ..terminal
                },
                true,
            ),
            (Action::PortForwardManager, terminal, false),
            (
                Action::PortForwardManager,
                ShortcutContext {
                    port_forward_available: true,
                    ..terminal
                },
                true,
            ),
            (
                Action::Find,
                ShortcutContext {
                    terminal_input: false,
                    ..terminal
                },
                false,
            ),
            (
                Action::Paste,
                ShortcutContext {
                    terminal_input: false,
                    ..terminal
                },
                false,
            ),
            (
                Action::Copy,
                ShortcutContext {
                    terminal_input: false,
                    ..terminal
                },
                false,
            ),
            (
                Action::NewSession,
                ShortcutContext {
                    palette_open: true,
                    ..terminal
                },
                false,
            ),
            (
                Action::CommandPalette,
                ShortcutContext {
                    palette_open: true,
                    ..terminal
                },
                true,
            ),
            (
                Action::Quick1,
                ShortcutContext {
                    palette_open: true,
                    ..terminal
                },
                true,
            ),
            (Action::Quick9, terminal, false),
            (
                Action::Quick9,
                ShortcutContext {
                    tab_count: 9,
                    ..terminal
                },
                true,
            ),
            (
                Action::SettingsHotkey,
                ShortcutContext {
                    blocked: true,
                    ..terminal
                },
                false,
            ),
            (
                Action::CommandPalette,
                ShortcutContext {
                    blocked: true,
                    ..terminal
                },
                false,
            ),
            (
                Action::Quick1,
                ShortcutContext {
                    blocked: true,
                    ..terminal
                },
                false,
            ),
        ];
        for (action, scope, expected) in cases {
            let mut bindings = KeyboardBindings::default();
            for candidate in Action::ALL {
                bindings.set(candidate, Some(String::new()));
            }
            bindings.set(action, Some("Ctrl+F".into()));
            bindings.validate(cfg!(target_os = "macos")).unwrap();
            let event = egui::Event::Key {
                key: Key::F,
                physical_key: Some(Key::F),
                modifiers: Modifiers::CTRL,
                pressed: true,
                repeat: false,
            };
            assert_eq!(
                scope.activates(&event, &bindings, false),
                expected,
                "{action:?}"
            );
            let global = matches!(
                action.scope(),
                festerm_config::KeyboardScope::Global | festerm_config::KeyboardScope::Document
            );
            assert_eq!(
                scope.activates(&event, &bindings, true),
                expected && global,
                "{action:?}"
            );
            let context = egui::Context::default();
            let mut output = context.run_ui(
                egui::RawInput {
                    events: vec![event],
                    ..Default::default()
                },
                |ui| {
                    assert_eq!(
                        scope.consume(ui.ctx(), &bindings, action),
                        expected,
                        "{action:?}"
                    );
                    assert_eq!(
                        ui.input(|input| input.events.is_empty()),
                        expected,
                        "{action:?}"
                    );
                },
            );
            output.textures_delta.clear();
        }
    }

    #[test]
    fn keyboard_clipboard_pairing_preserves_unrelated_explicit_paste() {
        let context = egui::Context::default();
        let mut output = context.run_ui(
            egui::RawInput {
                events: vec![
                    egui::Event::Key {
                        key: Key::B,
                        physical_key: Some(Key::B),
                        pressed: true,
                        repeat: false,
                        modifiers: Modifiers::CTRL,
                    },
                    egui::Event::Paste("controlled-menu-paste".into()),
                ],
                ..Default::default()
            },
            |ui| {
                prepare_terminal_events(ui.ctx());
                assert!(ui.input(|input| input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Paste(_)))));
                consume_exact(ui.ctx(), Modifiers::CTRL, Key::B);
                assert!(ui.input(|input| input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Paste(_)))));
            },
        );
        output.textures_delta.clear();
    }

    #[test]
    fn keyboard_editor_assigns_unbinds_resets_and_rejects_conflicts() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(600.0, 850.0))
            .build_ui_state(
                |ui, bindings: &mut KeyboardBindings| {
                    if let Some(crate::tabs::AppCommand::SetKeyboardBindings(next)) =
                        show_editor(ui, bindings)
                    {
                        *bindings = next;
                    }
                },
                KeyboardBindings::default(),
            );
        harness.run();
        let title = format!(
            "New Session — {}",
            label(harness.state(), Action::NewSession).unwrap()
        );
        harness.get_by_label(&title).click();
        harness.run();
        harness.get_by_label("Binding chord").click();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
        harness.event(egui::Event::Text("Ctrl+Shift+F8".into()));
        harness.run();
        harness.get_by_label("Assign binding").click();
        harness.run();
        assert_eq!(
            harness
                .state()
                .effective(Action::NewSession, cfg!(target_os = "macos")),
            "Ctrl+Shift+F8"
        );
        harness.get_by_label("Unbind action").click();
        harness.run();
        assert_eq!(harness.state().effective(Action::NewSession, false), "");
        harness.get_by_label("Reset action").click();
        harness.run();
        assert!(harness.state().is_empty());
        harness.get_by_label("Binding chord").click();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
        harness.event(egui::Event::Text("Primary+Shift+S".into()));
        harness.run();
        harness.get_by_label("Assign binding").click();
        harness.run();
        assert!(
            harness.state().is_empty(),
            "invalid draft must not change the effective map"
        );
        assert!(harness.query_by_label("Binding overlaps another action in the same context. Clear or change that action first.").is_some());
        harness.get_by_label("Reset all keyboard bindings").click();
        harness.run();
        assert!(harness.state().is_empty());
    }
}
