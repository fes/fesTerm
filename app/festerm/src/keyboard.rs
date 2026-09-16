//! Application-key matching and Settings presentation; protocol bytes stay in core.
use eframe::egui::{self, Key, Modifiers};
use festerm_config::{Chord, KeyboardAction as Action, KeyboardBindings, KeyboardScope as Scope};
use festerm_ui_egui::theme;

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

/// Narrows the catalogue by the two questions users actually ask: "what can
/// I press here?" and "what have I changed?".
#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum Filter {
    #[default]
    All,
    Global,
    Terminal,
    Markdown,
    Document,
    Customized,
    Unbound,
}

impl Filter {
    const ALL: [Self; 7] = [
        Self::All,
        Self::Global,
        Self::Terminal,
        Self::Markdown,
        Self::Document,
        Self::Customized,
        Self::Unbound,
    ];

    const fn title(self) -> &'static str {
        match self {
            Self::All => "All actions",
            Self::Global => "Global",
            Self::Terminal => "Terminal",
            Self::Markdown => "Markdown",
            Self::Document => "Document",
            Self::Customized => "Customized",
            Self::Unbound => "Unbound",
        }
    }

    fn matches(self, action: Action, bindings: &KeyboardBindings) -> bool {
        match self {
            Self::All => true,
            Self::Global => action.scope() == Scope::Global,
            Self::Terminal => action.scope() == Scope::Terminal,
            Self::Markdown => action.scope() == Scope::Markdown,
            Self::Document => action.scope() == Scope::Document,
            Self::Customized => customized(bindings, action),
            Self::Unbound => label(bindings, action).is_none(),
        }
    }
}

#[derive(Clone, Default)]
struct Editor {
    search: String,
    filter: Filter,
    selected: Option<Action>,
    draft: String,
    feedback: Option<String>,
}

fn customized(bindings: &KeyboardBindings, action: Action) -> bool {
    bindings.0.iter().any(|entry| entry.action == action)
}

/// Splitting on '+' is safe because the schema spells keys by name
/// ("Plus", "Equals"), so a bare '+' is never a key token.
fn keycaps(chord: &str) -> Vec<String> {
    if chord.is_empty() {
        return Vec::new();
    }
    chord.split('+').map(keycap_token).collect()
}

fn keycap_token(part: &str) -> String {
    let mac = cfg!(target_os = "macos");
    // Only U+2318 is guaranteed by the bundled UI face; the other Apple
    // modifier glyphs render as tofu, so the rest are spelled out exactly as
    // the command palette and chip hints already spell them.
    match part {
        "Primary" if mac => "⌘",
        "Primary" => "Ctrl",
        "Command" => "⌘",
        "Ctrl" if mac => "Control",
        "Alt" if mac => "Option",
        "Comma" => ",",
        "Period" => ".",
        "Plus" => "+",
        "Equals" => "=",
        "Minus" => "-",
        other => other,
    }
    .to_owned()
}

fn show_keycap(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(theme::SURFACE_FIELD)
        .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .size(12.0)
                    .color(theme::TEXT_PRIMARY),
            );
        });
}

/// Renders a chord as discrete keycaps, or an explicit "Unbound" marker so
/// an empty binding is never mistaken for a rendering gap. Right-to-left
/// layouts consume children in reverse, so the caller says which it is.
fn show_chord(ui: &mut egui::Ui, chord: &str, right_to_left: bool) {
    let caps = keycaps(chord);
    if caps.is_empty() {
        ui.label(
            egui::RichText::new("Unbound")
                .size(11.0)
                .color(theme::TEXT_MUTED),
        );
        return;
    }
    if right_to_left {
        for cap in caps.iter().rev() {
            show_keycap(ui, cap);
            ui.add_space(3.0);
        }
    } else {
        for cap in &caps {
            show_keycap(ui, cap);
            ui.add_space(3.0);
        }
    }
}

fn show_row(
    ui: &mut egui::Ui,
    bindings: &KeyboardBindings,
    action: Action,
    selected: bool,
) -> bool {
    // Reserved up front so the selected row's background can be painted
    // behind the widgets laid out below it, giving a full-width table row
    // rather than a lone highlighted word.
    let background = ui.painter().add(egui::Shape::Noop);
    let mut clicked = false;
    let row = ui.horizontal(|ui| {
        ui.add_space(4.0);
        let title = egui::RichText::new(action.title())
            .size(13.0)
            .color(if selected {
                theme::ACCENT_PRIMARY
            } else {
                theme::TEXT_PRIMARY
            });
        clicked = ui
            .scope(|ui| {
                // Scoped so it cannot reach the keycaps or any later widget:
                // the row background already carries selection, so the title
                // keeps a plain word shape instead of becoming a chip.
                ui.visuals_mut().selection.bg_fill = egui::Color32::TRANSPARENT;
                ui.selectable_label(selected, title)
                    .on_hover_text(action.description())
                    .clicked()
            })
            .inner;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(4.0);
            show_chord(
                ui,
                bindings.effective(action, cfg!(target_os = "macos")),
                true,
            );
            if customized(bindings, action) {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("Customized")
                        .size(10.0)
                        .color(theme::ACCENT_PRIMARY),
                );
            }
        });
    });
    if selected {
        ui.painter().set(
            background,
            egui::epaint::RectShape::filled(
                row.response.rect.expand2(egui::vec2(0.0, 2.0)),
                4.0,
                theme::ACCENT_PRIMARY.gamma_multiply(0.16),
            ),
        );
    }
    clicked
}

pub fn show_editor(
    ui: &mut egui::Ui,
    bindings: &KeyboardBindings,
) -> Option<crate::tabs::AppCommand> {
    let id = ui.make_persistent_id("keyboard-editor");
    let mut editor = ui.data(|data| data.get_temp::<Editor>(id).unwrap_or_default());
    let mac = cfg!(target_os = "macos");
    let mut replacement = None;
    let mut reset_all = false;

    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "Assign, unbind or restore fesTerm's own shortcuts. Unbinding an action hands \
                 that key back to the terminal; fesTerm never remaps terminal bytes.",
            )
            .size(12.0)
            .color(theme::TEXT_SECONDARY),
        )
        .wrap(),
    );
    ui.add_space(2.0);
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "Ctrl+Shift+F12 always reopens Settings, even after customization.",
            )
            .size(11.0)
            .color(theme::TEXT_MUTED),
        )
        .wrap(),
    );
    ui.add_space(8.0);

    let mut search = None;
    // The editor is shown in a Settings card that can be as narrow as a
    // phone-width window, so the search field takes whatever the label and
    // filter dropdown leave rather than a fixed width that would push the
    // controls outside the card.
    let search_width = (ui.available_width() - 330.0).clamp(90.0, 220.0);
    // Wrapping keeps every control inside the card on narrow windows instead
    // of letting the row run past its right edge.
    ui.horizontal_wrapped(|ui| {
        let search_label = ui.label(
            egui::RichText::new("Search")
                .size(12.0)
                .color(theme::TEXT_SECONDARY),
        );
        search = Some(
            ui.add(
                egui::TextEdit::singleline(&mut editor.search)
                    .hint_text("Search actions")
                    .desired_width(search_width),
            )
            .labelled_by(search_label.id),
        );
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new("Show")
                .size(12.0)
                .color(theme::TEXT_SECONDARY),
        );
        egui::ComboBox::from_id_salt("keyboard-filter")
            .width(110.0)
            .selected_text(editor.filter.title())
            .show_ui(ui, |ui| {
                for filter in Filter::ALL {
                    ui.selectable_value(&mut editor.filter, filter, filter.title());
                }
            });
    });
    // `docs/gui-design.md`: a reset appears only for a non-default value, so
    // the all-bindings reset stays out of the way until something is actually
    // customized.
    if !bindings.is_empty() {
        ui.add_space(6.0);
        reset_all = ui
            .button("Reset all keyboard bindings")
            .on_hover_text("Restore every action to its platform default chord.")
            .clicked();
    }

    if ui
        .ctx()
        .data_mut(|data| data.remove_temp::<bool>(egui::Id::new("keyboard-settings-recovery")))
        .unwrap_or(false)
    {
        if let Some(search) = &search {
            search.request_focus();
            search.scroll_to_me(Some(egui::Align::Center));
        }
    }

    if reset_all {
        replacement = Some(KeyboardBindings::default());
        editor.selected = None;
        editor.feedback = None;
    }

    ui.add_space(8.0);
    let query = editor.search.to_lowercase();
    let mut matched = 0usize;
    let mut chosen = None;
    egui::ScrollArea::vertical()
        .id_salt("keyboard-action-list")
        .max_height(280.0)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            // Grouping by scope keeps all 35 actions navigable; a flat list
            // makes the terminal-only entries indistinguishable from global ones.
            for scope in Scope::ALL {
                let actions: Vec<Action> = Action::ALL
                    .into_iter()
                    .filter(|action| {
                        action.scope() == scope
                            && editor.filter.matches(*action, bindings)
                            && (query.is_empty()
                                || action.title().to_lowercase().contains(&query)
                                || action.description().to_lowercase().contains(&query))
                    })
                    .collect();
                if actions.is_empty() {
                    continue;
                }
                matched += actions.len();
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(scope.title().to_uppercase())
                        .size(10.0)
                        .color(theme::TEXT_MUTED),
                );
                for action in actions {
                    ui.push_id(action as usize, |ui| {
                        if show_row(ui, bindings, action, editor.selected == Some(action)) {
                            chosen = Some(action);
                        }
                    });
                }
            }
            if matched == 0 {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("No actions match this search and filter.")
                        .size(12.0)
                        .color(theme::TEXT_MUTED),
                );
            }
        });

    if let Some(action) = chosen {
        editor.selected = Some(action);
        editor.draft = bindings.effective(action, mac).into();
        editor.feedback = None;
    }

    if let Some(action) = editor.selected {
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(8.0);
        // Same muted heading idiom as the scope groups above, so the pane
        // reads as a continuation of the row the user just picked.
        ui.label(
            egui::RichText::new("SELECTED ACTION")
                .size(10.0)
                .color(theme::TEXT_MUTED),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(action.title())
                .size(14.0)
                .strong()
                .color(theme::TEXT_PRIMARY),
        );
        ui.add(
            egui::Label::new(
                egui::RichText::new(action.description())
                    .size(12.0)
                    .color(theme::TEXT_SECONDARY),
            )
            .wrap(),
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Scope")
                    .size(11.0)
                    .color(theme::TEXT_MUTED),
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(action.scope().title())
                    .size(12.0)
                    .color(theme::TEXT_PRIMARY),
            );
        });
        // Kept out of the row above: a sentence inside `horizontal` cannot
        // wrap, so it would force the whole Settings card wider than the
        // window on narrow layouts.
        ui.add(
            egui::Label::new(
                egui::RichText::new(action.scope().help())
                    .size(11.0)
                    .color(theme::TEXT_MUTED),
            )
            .wrap(),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Current")
                    .size(11.0)
                    .color(theme::TEXT_MUTED),
            );
            ui.add_space(6.0);
            show_chord(ui, bindings.effective(action, mac), false);
        });
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Default")
                    .size(11.0)
                    .color(theme::TEXT_MUTED),
            );
            ui.add_space(6.0);
            show_chord(ui, action.default_chord(mac), false);
        });
        ui.add_space(8.0);
        let chord_label = ui.label(
            egui::RichText::new("Binding chord")
                .size(12.0)
                .color(theme::TEXT_SECONDARY),
        );
        ui.add(
            egui::TextEdit::singleline(&mut editor.draft)
                .hint_text("Primary+Shift+P")
                .desired_width(200.0),
        )
        .labelled_by(chord_label.id)
        .on_hover_text(
            "Combine Primary, Ctrl, Alt or Shift with A-Z, 0-9, F1-F12, Tab, Insert, Comma, \
             Period, Plus, Equals or Minus.",
        );
        ui.add(
            egui::Label::new(
                egui::RichText::new(
                    "Primary is Command on macOS and Ctrl elsewhere. Every binding needs \
                     Primary or Ctrl, except Shift+Insert.",
                )
                .size(11.0)
                .color(theme::TEXT_MUTED),
            )
            .wrap(),
        );
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let mut update = None;
            if ui.button("Assign binding").clicked() {
                update = Some(Some(editor.draft.clone()));
            }
            if ui.button("Unbind action").clicked() {
                update = Some(Some(String::new()));
            }
            // A per-action reset is only meaningful once that action has
            // been overridden (`docs/gui-design.md`).
            if customized(bindings, action) && ui.button("Restore default").clicked() {
                update = Some(None);
            }
            if let Some(update) = update {
                let mut candidate = bindings.clone();
                candidate.set(action, update);
                match candidate.validate(mac) {
                    Ok(()) => {
                        editor.draft = candidate.effective(action, mac).into();
                        editor.feedback = None;
                        replacement = Some(candidate);
                    }
                    Err(error) => editor.feedback = Some(error.into()),
                }
            }
        });
    }
    if let Some(feedback) = &editor.feedback {
        ui.add_space(6.0);
        ui.colored_label(theme::STATUS_ERROR, feedback);
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

    fn editor_harness() -> Harness<'static, KeyboardBindings> {
        Harness::builder()
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
            )
    }

    #[test]
    fn keyboard_filters_select_by_scope_binding_state_and_customization() {
        let mut customized_bindings = KeyboardBindings::default();
        customized_bindings.set(Action::NewSession, Some("Ctrl+Shift+F8".into()));

        assert!(Filter::All.matches(Action::Copy, &customized_bindings));
        assert!(Filter::Terminal.matches(Action::Copy, &customized_bindings));
        assert!(!Filter::Global.matches(Action::Copy, &customized_bindings));
        assert!(Filter::Global.matches(Action::NewSession, &customized_bindings));
        assert!(Filter::Markdown.matches(Action::MarkdownFind, &customized_bindings));
        assert!(Filter::Document.matches(Action::OpenMarkdownFile, &customized_bindings));
        assert!(Filter::Customized.matches(Action::NewSession, &customized_bindings));
        assert!(!Filter::Customized.matches(Action::Copy, &customized_bindings));

        let mut unbound = KeyboardBindings::default();
        unbound.set(Action::Copy, Some(String::new()));
        assert!(Filter::Unbound.matches(Action::Copy, &unbound));
        assert!(!Filter::Unbound.matches(Action::Copy, &customized_bindings));
    }

    #[test]
    fn keyboard_chords_render_as_discrete_platform_keycaps() {
        let mac = cfg!(target_os = "macos");
        assert!(keycaps("").is_empty());
        assert_eq!(
            keycaps("Primary+Shift+P"),
            vec![
                if mac { "⌘" } else { "Ctrl" }.to_owned(),
                "Shift".to_owned(),
                "P".to_owned(),
            ]
        );
        // Named keys keep '+' splitting unambiguous.
        assert_eq!(keycaps("Primary+Plus").last().unwrap(), "+");
        assert_eq!(keycaps("Primary+Comma").last().unwrap(), ",");
        assert_eq!(keycaps("Primary+Equals").last().unwrap(), "=");
        assert_eq!(keycaps("Ctrl+Tab").last().unwrap(), "Tab");
        // Only U+2318 is present in the bundled UI face; every other Apple
        // modifier glyph would render as tofu, so they stay spelled out.
        for token in keycaps("Ctrl+Alt+Shift+F1") {
            assert!(
                token.is_ascii(),
                "{token:?} is not guaranteed to have a glyph"
            );
        }
    }

    #[test]
    fn keyboard_every_default_chord_renders_one_keycap_per_token() {
        for action in Action::ALL {
            for mac in [true, false] {
                let chord = action.default_chord(mac);
                if chord.is_empty() {
                    continue;
                }
                assert_eq!(
                    keycaps(chord).len(),
                    chord.split('+').count(),
                    "{} must render one keycap per chord token",
                    action.title()
                );
            }
        }
    }

    #[test]
    fn keyboard_editor_groups_actions_by_scope_and_narrows_by_search() {
        let mut harness = editor_harness();
        harness.run();

        for heading in ["GLOBAL", "TERMINAL", "MARKDOWN", "DOCUMENT"] {
            assert!(
                harness.query_by_label(heading).is_some(),
                "{heading} scope group must be listed"
            );
        }

        harness.get_by_label("Search").click();
        harness.event(egui::Event::Text("markdown".into()));
        harness.run();

        assert!(harness.query_by_label("MARKDOWN").is_some());
        assert!(
            harness.query_by_label("GLOBAL").is_none(),
            "search must hide scope groups with no matching action"
        );
    }

    #[test]
    fn keyboard_editor_presents_scope_and_default_as_read_only_context() {
        let mut harness = editor_harness();
        harness.run();
        // Narrow first so the row is reachable without scrolling the list.
        harness.get_by_label("Search").click();
        harness.event(egui::Event::Text("Copy terminal selection".into()));
        harness.run();
        harness
            .get_by_role_and_label(accesskit::Role::Button, "Copy terminal selection")
            .click();
        harness.run();

        // Scope follows from the action, so it is reported with its meaning
        // rather than offered as an editable field.
        assert!(harness.query_by_label("Terminal").is_some());
        assert!(harness
            .query_by_label("Applies only while a terminal surface has input.")
            .is_some());
        assert!(harness.query_by_label("Current").is_some());
        assert!(harness.query_by_label("Default").is_some());
    }

    #[test]
    fn keyboard_editor_marks_customized_actions_and_shows_their_keycaps() {
        let mut harness = editor_harness();
        harness.run();
        assert!(harness.query_by_label("Customized").is_none());

        harness
            .get_by_role_and_label(accesskit::Role::Button, "New Session")
            .click();
        harness.run();
        harness.get_by_label("Binding chord").click();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
        harness.event(egui::Event::Text("Ctrl+Shift+F9".into()));
        harness.run();
        harness.get_by_label("Assign binding").click();
        harness.run();

        assert!(
            harness.query_by_label("Customized").is_some(),
            "an overridden action must be distinguishable from a default one"
        );
        assert_eq!(
            harness.query_all_by_label("F9").count(),
            2,
            "the assigned chord must render as keycaps in both the row and the detail pane"
        );
    }

    #[test]
    fn keyboard_editor_assigns_unbinds_resets_and_rejects_conflicts() {
        let mut harness = editor_harness();
        harness.run();
        harness
            .get_by_role_and_label(accesskit::Role::Button, "New Session")
            .click();
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
        harness.get_by_label("Restore default").click();
        harness.run();
        assert!(harness.state().is_empty());
        assert!(
            harness.query_by_label("Restore default").is_none(),
            "a per-action reset is meaningless once the action is back to its default"
        );
        assert!(
            harness
                .query_by_label("Reset all keyboard bindings")
                .is_none(),
            "the all-bindings reset must stay hidden while nothing is customized"
        );
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
        harness.get_by_label("Binding chord").click();
        harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
        harness.event(egui::Event::Text("Ctrl+Shift+F8".into()));
        harness.run();
        harness.get_by_label("Assign binding").click();
        harness.run();
        assert!(!harness.state().is_empty());
        harness.get_by_label("Reset all keyboard bindings").click();
        harness.run();
        assert!(harness.state().is_empty());
    }
}
