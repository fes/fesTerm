//! Application-key matching and Settings presentation; protocol bytes stay in core.
use eframe::egui::{self, Key, Modifiers, Sense, Stroke, UiBuilder, WidgetInfo, WidgetType};
use festerm_config::{Chord, KeyboardAction as Action, KeyboardBindings, KeyboardScope as Scope};
use festerm_ui_egui::{
    controls::{self, ActionButtonRole},
    icon,
    icon::Icon,
    theme,
};

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
    recording: bool,
}

/// Set by the editor while it is capturing a chord, and read by
/// `App::handle_shortcuts` before it dispatches anything. Without this the
/// captured keys would fire the shortcut they are being bound to.
const RECORDING_ID: &str = "keyboard-editor-recording";
/// Where `App::handle_shortcuts` parks the frame's key events for the editor,
/// which runs later in the same frame and so cannot read `input.events`.
const RECORDED_EVENTS_ID: &str = "keyboard-editor-recorded-events";

/// True while the keyboard editor is waiting for the user to press a chord.
pub fn recording(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp::<bool>(egui::Id::new(RECORDING_ID)))
        .unwrap_or(false)
}

/// Hands this frame's input to the editor instead of the shortcut dispatcher.
/// The events are deliberately not forwarded anywhere else: a chord being
/// recorded must not also reach the terminal or trigger its own action. The
/// arming flag is cleared here and re-armed by the editor on every frame it
/// draws, so closing Settings mid-capture releases the keyboard next frame
/// instead of swallowing input forever.
pub fn stash_recorded_events(ctx: &egui::Context, events: Vec<egui::Event>) {
    ctx.data_mut(|data| {
        data.remove_temp::<bool>(egui::Id::new(RECORDING_ID));
        data.insert_temp(egui::Id::new(RECORDED_EVENTS_ID), events);
    });
}

/// Canonical chord text for a pressed key, or `None` for keys the schema
/// cannot express. Modifier order matches `KeyboardAction::default_chord`.
fn recorded_chord(key: egui::Key, modifiers: egui::Modifiers, mac: bool) -> Option<String> {
    let key = match key {
        egui::Key::A => "A",
        egui::Key::B => "B",
        egui::Key::C => "C",
        egui::Key::D => "D",
        egui::Key::E => "E",
        egui::Key::F => "F",
        egui::Key::G => "G",
        egui::Key::H => "H",
        egui::Key::I => "I",
        egui::Key::J => "J",
        egui::Key::K => "K",
        egui::Key::L => "L",
        egui::Key::M => "M",
        egui::Key::N => "N",
        egui::Key::O => "O",
        egui::Key::P => "P",
        egui::Key::Q => "Q",
        egui::Key::R => "R",
        egui::Key::S => "S",
        egui::Key::T => "T",
        egui::Key::U => "U",
        egui::Key::V => "V",
        egui::Key::W => "W",
        egui::Key::X => "X",
        egui::Key::Y => "Y",
        egui::Key::Z => "Z",
        egui::Key::Num0 => "0",
        egui::Key::Num1 => "1",
        egui::Key::Num2 => "2",
        egui::Key::Num3 => "3",
        egui::Key::Num4 => "4",
        egui::Key::Num5 => "5",
        egui::Key::Num6 => "6",
        egui::Key::Num7 => "7",
        egui::Key::Num8 => "8",
        egui::Key::Num9 => "9",
        egui::Key::F1 => "F1",
        egui::Key::F2 => "F2",
        egui::Key::F3 => "F3",
        egui::Key::F4 => "F4",
        egui::Key::F5 => "F5",
        egui::Key::F6 => "F6",
        egui::Key::F7 => "F7",
        egui::Key::F8 => "F8",
        egui::Key::F9 => "F9",
        egui::Key::F10 => "F10",
        egui::Key::F11 => "F11",
        egui::Key::F12 => "F12",
        egui::Key::Tab => "Tab",
        egui::Key::Insert => "Insert",
        egui::Key::Comma => "Comma",
        egui::Key::Period => "Period",
        egui::Key::Plus => "Plus",
        egui::Key::Equals => "Equals",
        egui::Key::Minus => "Minus",
        _ => return None,
    };
    let mut chord = String::new();
    // Primary is what the schema stores for the portable modifier: Command on
    // macOS, Ctrl everywhere else. Recording the platform-specific spelling
    // would produce a binding that stops working on the user's other machine.
    if (mac && modifiers.mac_cmd) || (!mac && modifiers.ctrl) {
        chord.push_str("Primary+");
    } else if mac && modifiers.ctrl {
        chord.push_str("Ctrl+");
    }
    if modifiers.alt {
        chord.push_str("Alt+");
    }
    if modifiers.shift {
        chord.push_str("Shift+");
    }
    chord.push_str(key);
    Some(chord)
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

/// Keycap columns are counted from the right: column 0 is the key itself,
/// column 1 the modifier before it, and so on. Chords are right-anchored and
/// read as one unit, so compacting from the key outwards keeps each chord
/// contiguous and keeps the key column — the one people scan — straight,
/// without every row reserving space for modifiers it does not use.
const CHORD_SLOTS: usize = 5;
/// Gap between two occupied keycap columns.
const CHORD_COLUMN_GAP: f32 = 6.0;
const ROW_HEIGHT: f32 = 72.0;
const KEYCAP_HEIGHT: f32 = 30.0;
const KEYCAP_MIN_WIDTH: f32 = 34.0;
const KEYCAP_HORIZONTAL_PADDING: f32 = 18.0;

fn keycap_slots(chord: &str) -> [Option<String>; CHORD_SLOTS] {
    let mut slots: [Option<String>; CHORD_SLOTS] = Default::default();
    for (column, part) in chord
        .split('+')
        .filter(|part| !part.is_empty())
        .rev()
        .enumerate()
        .take(CHORD_SLOTS)
    {
        slots[column] = Some(keycap_token(part));
    }
    slots
}

/// Width `show_keycap` will occupy: the label plus the frame's margins and
/// stroke. Column placement only needs this to be consistent, not exact —
/// each cell's padding is the difference between two of these measurements.
fn keycap_width(ui: &egui::Ui, text: &str) -> f32 {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(12.0),
        theme::TEXT_PRIMARY,
    );
    (galley.size().x + KEYCAP_HORIZONTAL_PADDING).max(KEYCAP_MIN_WIDTH)
}

/// The width each keycap column needs across every row that will be drawn,
/// so alignment holds across scope groups and not just within one. Columns no
/// row uses stay at zero and are skipped entirely.
fn chord_columns(
    ui: &egui::Ui,
    bindings: &KeyboardBindings,
    actions: &[Action],
) -> [f32; CHORD_SLOTS] {
    let mac = cfg!(target_os = "macos");
    let mut widths = [0.0f32; CHORD_SLOTS];
    for action in actions {
        for (slot, cap) in keycap_slots(bindings.effective(*action, mac))
            .iter()
            .enumerate()
        {
            if let Some(cap) = cap {
                widths[slot] = widths[slot].max(keycap_width(ui, cap));
            }
        }
    }
    widths
}

/// One column of a row's chord: the keycap centred in the column's width, or
/// blank space holding the column open for the rows that do use it.
fn show_keycap_cell(ui: &mut egui::Ui, cap: Option<&str>, width: f32) {
    let Some(cap) = cap else {
        ui.add_space(width);
        return;
    };
    let padding = (width - keycap_width(ui, cap)).max(0.0) / 2.0;
    ui.add_space(padding);
    show_keycap(ui, cap);
    ui.add_space(padding);
}

fn show_keycap(ui: &mut egui::Ui, text: &str) {
    let width = keycap_width(ui, text);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, KEYCAP_HEIGHT), egui::Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, ui.is_enabled(), text));
    ui.painter().rect(
        rect,
        5.0,
        theme::SURFACE_FIELD,
        Stroke::new(1.0, theme::BORDER_SUBTLE),
        egui::StrokeKind::Inside,
    );
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(12.0),
        theme::TEXT_PRIMARY,
    );
    ui.painter().galley(
        rect.center() - galley.size() / 2.0,
        galley,
        theme::TEXT_PRIMARY,
    );
}

fn chord_width(ui: &egui::Ui, chord: &str) -> f32 {
    let caps = keycaps(chord);
    if caps.is_empty() {
        return ui
            .painter()
            .layout_no_wrap(
                "Unbound".to_owned(),
                egui::FontId::proportional(11.0),
                theme::TEXT_MUTED,
            )
            .size()
            .x;
    }
    caps.iter().map(|cap| keycap_width(ui, cap) + 3.0).sum()
}

/// Renders a chord as discrete keycaps, or an explicit "Unbound" marker so
/// an empty binding is never mistaken for a rendering gap. Right-to-left
/// layouts consume children in reverse, so the caller says which it is.
/// One `label: value` line in the expanded editor's metadata block, with the
/// labels sharing a fixed lane so the values line up under each other.
fn metadata_row(ui: &mut egui::Ui, label: &str, value: impl FnOnce(&mut egui::Ui)) {
    const LABEL_LANE: f32 = 66.0;
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(LABEL_LANE, 0.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_width(LABEL_LANE);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(label)
                            .size(14.0)
                            .color(theme::TEXT_MUTED),
                    )
                    .selectable(false),
                );
            },
        );
        value(ui);
    });
}

fn show_chord(ui: &mut egui::Ui, chord: &str) {
    let caps = keycaps(chord);
    if caps.is_empty() {
        ui.label(
            egui::RichText::new("Unbound")
                .size(11.0)
                .color(theme::TEXT_MUTED),
        );
        return;
    }
    for cap in &caps {
        show_keycap(ui, cap);
        ui.add_space(3.0);
    }
}

fn keyboard_separator(ui: &mut egui::Ui) {
    let y = ui.cursor().top();
    let rect = ui.available_rect_before_wrap();
    ui.painter().hline(
        rect.left()..=rect.right(),
        y,
        Stroke::new(1.0, theme::BORDER_SUBTLE),
    );
    ui.add_space(1.0);
}

fn secondary_action_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    controls::action_button(ui, ActionButtonRole::Secondary, label)
}

fn secondary_action_button_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
) -> egui::Response {
    controls::action_button_enabled(ui, enabled, ActionButtonRole::Secondary, label)
}

fn show_search_field(ui: &mut egui::Ui, search: &mut String, width: f32) -> egui::Response {
    controls::SearchField {
        width,
        height: 42.0,
        icon_inset: 18.0,
        icon_size: 14.0,
        text_size: 14.0,
        hint: "Search actions…",
    }
    .show(ui, search)
}

fn show_row(
    ui: &mut egui::Ui,
    bindings: &KeyboardBindings,
    action: Action,
    selected: bool,
    columns: &[f32; CHORD_SLOTS],
) -> bool {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, ROW_HEIGHT), Sense::click());
    response
        .widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), action.title()));
    let changed = customized(bindings, action);
    if selected {
        ui.painter().rect_filled(
            rect.expand2(egui::vec2(0.0, 1.0)),
            0.0,
            theme::ACCENT_PRIMARY.gamma_multiply(0.12),
        );
    }
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 0.0, theme::SURFACE_OVERLAY.gamma_multiply(0.45));
    }

    let chord = bindings.effective(action, cfg!(target_os = "macos"));
    let chevron_width = 26.0;
    let chord_total_width = columns
        .iter()
        .copied()
        .filter(|width| *width > 0.0)
        .sum::<f32>()
        + CHORD_COLUMN_GAP
            * columns
                .iter()
                .filter(|width| **width > 0.0)
                .count()
                .saturating_sub(1) as f32;
    let chord_total_width = if chord.is_empty() {
        chord_width(ui, chord)
    } else {
        chord_total_width
    };
    let right_width = chord_total_width + chevron_width + 34.0;
    let text_rect = egui::Rect::from_min_max(
        rect.left_top() + egui::vec2(18.0, 11.0),
        egui::pos2(
            (rect.right() - right_width).max(rect.left() + 120.0),
            rect.bottom(),
        ),
    );
    let chord_rect = egui::Rect::from_min_max(
        egui::pos2((rect.right() - right_width).max(rect.left()), rect.top()),
        rect.right_bottom(),
    );
    let title_font = egui::FontId::proportional(16.0);
    let title_galley =
        ui.painter()
            .layout_no_wrap(action.title().to_owned(), title_font, theme::TEXT_PRIMARY);
    ui.painter().galley(
        text_rect.left_top(),
        title_galley.clone(),
        theme::TEXT_PRIMARY,
    );
    if changed {
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(
                text_rect.left() + title_galley.size().x + 10.0,
                text_rect.top() + 1.0,
            ),
            egui::vec2(94.0, 20.0),
        );
        ui.put(
            badge_rect,
            egui::Label::new(
                egui::RichText::new("Customized")
                    .size(12.0)
                    .color(theme::ACCENT_PRIMARY),
            ),
        )
        .on_hover_text("Changed from the default chord.");
    }
    let description_rect = egui::Rect::from_min_max(
        egui::pos2(text_rect.left(), text_rect.top() + 28.0),
        text_rect.right_bottom(),
    );
    ui.scope_builder(UiBuilder::new().max_rect(description_rect), |ui| {
        ui.vertical(|ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(action.description())
                        .size(14.0)
                        .color(theme::TEXT_SECONDARY),
                )
                .truncate(),
            );
        });
    });
    ui.scope_builder(UiBuilder::new().max_rect(chord_rect), |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(10.0);
            let (chevron, chevron_response) =
                ui.allocate_exact_size(egui::Vec2::splat(16.0), Sense::hover());
            chevron_response.widget_info(|| {
                WidgetInfo::labeled(
                    WidgetType::Label,
                    ui.is_enabled(),
                    if selected { "⌄" } else { "›" },
                )
            });
            icon::paint(
                ui.painter(),
                if selected {
                    Icon::SectionExpanded
                } else {
                    Icon::SectionCollapsed
                },
                chevron,
                theme::TEXT_MUTED,
            );
            ui.add_space(8.0);
            if chord.is_empty() {
                ui.label(
                    egui::RichText::new("Unbound")
                        .size(11.0)
                        .color(theme::TEXT_MUTED),
                );
            } else {
                ui.spacing_mut().item_spacing.x = 0.0;
                let slots = keycap_slots(chord);
                let mut gap = false;
                for slot in 0..CHORD_SLOTS {
                    if columns[slot] <= 0.0 {
                        continue;
                    }
                    if gap {
                        ui.add_space(CHORD_COLUMN_GAP);
                    }
                    show_keycap_cell(ui, slots[slot].as_deref(), columns[slot]);
                    gap = true;
                }
            }
        });
    });
    response.clicked()
}

pub fn show_editor(
    ui: &mut egui::Ui,
    bindings: &KeyboardBindings,
) -> Option<crate::tabs::AppCommand> {
    let id = ui.make_persistent_id("keyboard-editor");
    let mut editor = ui.data(|data| data.get_temp::<Editor>(id).unwrap_or_default());
    let mac = cfg!(target_os = "macos");
    let mut replacement = None;

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
    ui.add_space(10.0);

    let mut search = None;
    let toolbar_width = ui.available_width();
    let toolbar_height = 58.0;
    let toolbar_response = ui.allocate_ui_with_layout(
        egui::vec2(toolbar_width, toolbar_height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            let reset_width = 122.0;
            let filter_width = 142.0;
            let search_width =
                (ui.available_width() - reset_width - filter_width - 32.0).clamp(120.0, 500.0);
            let search_response = show_search_field(ui, &mut editor.search, search_width);
            search = Some(search_response);
            ui.add_space(12.0);
            egui::ComboBox::from_id_salt("keyboard-filter")
                .width(filter_width)
                .selected_text(editor.filter.title())
                .show_ui(ui, |ui| {
                    for filter in Filter::ALL {
                        ui.selectable_value(&mut editor.filter, filter, filter.title());
                    }
                });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Enabled only when something is actually overridden, but always present:
                // hiding it entirely left users unable to tell whether a reset exists.
                ui.add_space(2.0);
                secondary_action_button_enabled(ui, !bindings.is_empty(), "Reset all…")
                    .on_hover_text("Restore every action to its platform default chord.")
                    .on_disabled_hover_text("Every action already uses its default chord.")
                    .clicked()
            })
            .inner
        },
    );
    let reset_all = toolbar_response.inner;

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

    keyboard_separator(ui);
    let query = editor.search.to_lowercase();
    let mut chosen = None;
    let visible: Vec<Action> = Action::ALL
        .into_iter()
        .filter(|action| {
            editor.filter.matches(*action, bindings)
                && (query.is_empty()
                    || action.title().to_lowercase().contains(&query)
                    || action.description().to_lowercase().contains(&query))
        })
        .collect();
    // Measured across every visible row, so the key column stays straight
    // across scope groups rather than restarting at each heading.
    let columns = chord_columns(ui, bindings, &visible);
    // No inner scroll area: the whole Settings page already scrolls, and a
    // short nested viewport made 35 actions painful to page through.
    ui.vertical(|ui| {
        ui.set_min_width(ui.available_width());
        // Grouping by scope keeps all 35 actions navigable; a flat list
        // makes the terminal-only entries indistinguishable from global ones.
        for scope in Scope::ALL {
            let actions: Vec<Action> = visible
                .iter()
                .copied()
                .filter(|action| action.scope() == scope)
                .collect();
            if actions.is_empty() {
                continue;
            }
            ui.add_space(14.0);
            ui.label(
                egui::RichText::new(scope.title().to_uppercase())
                    .size(13.0)
                    .color(theme::TEXT_MUTED),
            );
            ui.add_space(14.0);
            for action in actions {
                ui.push_id(action as usize, |ui| {
                    keyboard_separator(ui);
                    if show_row(
                        ui,
                        bindings,
                        action,
                        editor.selected == Some(action),
                        &columns,
                    ) {
                        chosen = Some(action);
                    }
                    // Expanded in place: pushing the editor to the bottom of
                    // a 35-row table meant scrolling away from the row being
                    // changed and back again to see the result.
                    if editor.selected == Some(action) {
                        // The indent ties the editor to its row, but on a
                        // phone-width window that space is needed by the
                        // controls themselves.
                        let indent = if ui.available_width() < 420.0 {
                            6.0
                        } else {
                            16.0
                        };
                        let card_width = (ui.available_width() - indent * 2.0).max(240.0);
                        ui.horizontal(|ui| {
                            ui.add_space(indent);
                            ui.allocate_ui_with_layout(
                                egui::vec2(card_width, 246.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    egui::Frame::new()
                                        .fill(theme::SURFACE_PANEL)
                                        .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                                        .corner_radius(4.0)
                                        .inner_margin(egui::Margin::symmetric(18, 18))
                                        .show(ui, |ui| {
                                            if let Some(next) =
                                                show_detail(ui, bindings, action, &mut editor, mac)
                                            {
                                                replacement = Some(next);
                                            }
                                        });
                                },
                            );
                        });
                        ui.add_space(4.0);
                    }
                });
            }
        }
        if visible.is_empty() {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("No actions match this search and filter.")
                    .size(12.0)
                    .color(theme::TEXT_MUTED),
            );
        }
    });

    if let Some(action) = chosen {
        // Clicking the open action again closes it: the row is the control
        // that opened the editor, so it is also the one to shut it.
        if editor.selected == Some(action) {
            editor.selected = None;
            editor.feedback = None;
        } else {
            editor.selected = Some(action);
            editor.draft = bindings.effective(action, mac).into();
            editor.feedback = None;
        }
        editor.recording = false;
    }

    editor.recording = editor.recording && editor.selected.is_some();
    // Re-armed on every frame the editor is visible, and cleared by
    // `handle_shortcuts` as it stashes. Closing Settings mid-capture therefore
    // releases the keyboard on the next frame instead of swallowing input.
    let recording = editor.recording;
    ui.ctx().data_mut(|data| {
        data.insert_temp(egui::Id::new(RECORDING_ID), recording);
        if !recording {
            data.remove_temp::<Vec<egui::Event>>(egui::Id::new(RECORDED_EVENTS_ID));
        }
    });
    ui.data_mut(|data| data.insert_temp(id, editor));
    replacement.map(crate::tabs::AppCommand::SetKeyboardBindings)
}

/// The expanded editor for one action, drawn immediately under its row so
/// assigning a binding does not mean scrolling away from the list. Returns a
/// replacement map when the action's binding changed.
fn show_detail(
    ui: &mut egui::Ui,
    bindings: &KeyboardBindings,
    action: Action,
    editor: &mut Editor,
    mac: bool,
) -> Option<KeyboardBindings> {
    let mut replacement = None;

    // Recording is resolved before the field is drawn so a chord captured
    // this frame is already in the box the user is looking at.
    let mut assign_recorded = false;
    if editor.recording {
        let events = ui
            .ctx()
            .data_mut(|data| {
                data.remove_temp::<Vec<egui::Event>>(egui::Id::new(RECORDED_EVENTS_ID))
            })
            .unwrap_or_default();
        for event in events {
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            if key == egui::Key::Escape {
                editor.recording = false;
                break;
            }
            match recorded_chord(key, modifiers, mac) {
                Some(chord) => {
                    editor.draft = chord;
                    editor.recording = false;
                    editor.feedback = None;
                    assign_recorded = true;
                }
                None => {
                    editor.feedback = Some(
                        "That key cannot be bound; use A-Z, 0-9, F1-F12, Tab, Insert, Comma, \
                         Period, Plus, Equals or Minus."
                            .into(),
                    )
                }
            }
            break;
        }
    }

    let mut update = if assign_recorded {
        Some(Some(editor.draft.clone()))
    } else {
        None
    };
    let current_chord = bindings.effective(action, mac);
    // `horizontal` centres its children on the cross axis, which would float
    // the shorter of the two columns; the editor's columns must share a top.
    ui.horizontal_top(|ui| {
        let available = ui.available_width();
        let gap = 16.0;
        let mut right_width = (available * 0.38).clamp(220.0, 360.0);
        right_width = right_width.min((available - gap - 240.0).max(200.0));
        let left_width = (available - right_width - gap).max(220.0);
        ui.allocate_ui_with_layout(
            egui::vec2(left_width, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(left_width);
                ui.label(
                    egui::RichText::new(action.title())
                        .size(22.0)
                        .color(theme::TEXT_PRIMARY),
                );
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(action.description())
                            .size(15.0)
                            .color(theme::TEXT_SECONDARY),
                    )
                    .wrap(),
                );
                ui.add_space(14.0);
                // One row per fact with a fixed label lane: chords vary in
                // width, and on a single wrapped line a long one drags the
                // next label onto its own row and breaks the alignment.
                metadata_row(ui, "Scope", |ui| {
                    ui.label(
                        egui::RichText::new(action.scope().title())
                            .size(14.0)
                            .color(theme::TEXT_PRIMARY),
                    );
                    ui.add_space(10.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(action.scope().help())
                                .size(13.0)
                                .color(theme::TEXT_MUTED),
                        )
                        .wrap(),
                    );
                });
                ui.add_space(6.0);
                metadata_row(ui, "Default", |ui| {
                    show_chord(ui, action.default_chord(mac));
                });
                ui.add_space(6.0);
                metadata_row(ui, "Current", |ui| {
                    show_chord(ui, current_chord);
                });
                ui.add_space(14.0);
                let chord_label = ui.label(
                    egui::RichText::new("Binding chord")
                        .size(12.0)
                        .color(theme::TEXT_SECONDARY),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut editor.draft)
                            .hint_text("Primary+Shift+P")
                            .desired_width(210.0),
                    )
                    .labelled_by(chord_label.id)
                    .on_hover_text(
                        "Type a chord literally, or use Record shortcut to capture one. Combine \
                         Primary, Ctrl, Alt or Shift with A-Z, 0-9, F1-F12, Tab, Insert, Comma, \
                         Period, Plus, Equals or Minus.",
                    );
                    ui.add_space(6.0);
                    if secondary_action_button(ui, "Assign binding").clicked() {
                        update = Some(Some(editor.draft.clone()));
                    }
                });
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
                ui.add_space(12.0);
                ui.horizontal_wrapped(|ui| {
                    if controls::action_button(ui, ActionButtonRole::DangerOutline, "Clear binding")
                        .clicked()
                    {
                        update = Some(Some(String::new()));
                    }
                    if ui
                        .add_enabled_ui(customized(bindings, action), |ui| {
                            secondary_action_button(ui, "Restore default")
                        })
                        .inner
                        .on_hover_text("Put this action back on its default chord.")
                        .on_disabled_hover_text("This action already uses its default chord.")
                        .clicked()
                    {
                        update = Some(None);
                    }
                });
            },
        );
        ui.add_space(gap);
        ui.allocate_ui_with_layout(
            egui::vec2(right_width, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::Frame::new()
                    .fill(theme::SURFACE_FIELD)
                    .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(18, 20))
                    .show(ui, |ui| {
                        ui.set_width(right_width - 36.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new(if editor.recording {
                                    "Recording shortcut"
                                } else {
                                    "Current shortcut"
                                })
                                .size(15.0)
                                .color(if editor.recording {
                                    theme::ACCENT_PRIMARY
                                } else {
                                    theme::TEXT_SECONDARY
                                }),
                            );
                            ui.add_space(12.0);
                            // egui does not retroactively centre a run of
                            // widgets on the layout's main axis, so the
                            // leading offset is measured explicitly.
                            let panel_width = right_width - 36.0;
                            ui.horizontal(|ui| {
                                let chord = chord_width(ui, current_chord);
                                ui.add_space(((panel_width - chord) / 2.0).max(0.0));
                                show_chord(ui, current_chord);
                            });
                            ui.add_space(16.0);
                            let record_label = if editor.recording {
                                "Press shortcut…"
                            } else {
                                "Record shortcut"
                            };
                            let capture = controls::action_button_sized(
                                ui,
                                [(right_width - 36.0).min(240.0), 46.0],
                                ActionButtonRole::Accent,
                                record_label,
                            )
                            .on_hover_text("Capture the next key combination you press.");
                            if capture.clicked() {
                                editor.recording = !editor.recording;
                                editor.feedback = None;
                            }
                            ui.add_space(10.0);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(if editor.recording {
                                        "Listening — press the combination you want, or Escape to cancel."
                                    } else {
                                        "Captured keys are not sent to the terminal."
                                    })
                                    .size(11.0)
                                    .color(if editor.recording {
                                        theme::ACCENT_PRIMARY
                                    } else {
                                        theme::TEXT_MUTED
                                    }),
                                )
                                .wrap(),
                            );
                        });
                    });
            },
        );
        if let Some(update) = update.take() {
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
    if let Some(feedback) = &editor.feedback {
        ui.add_space(6.0);
        ui.colored_label(theme::STATUS_ERROR, feedback);
    }
    replacement
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

    /// A control that is present but inert. Disabled affordances are how the
    /// editor answers "does a reset exist?" before anything is customized.
    fn is_disabled(harness: &Harness<'static, KeyboardBindings>, label: &str) -> bool {
        use egui_kittest::kittest::NodeT;
        harness.get_by_label(label).accesskit_node().is_disabled()
    }

    fn editor_harness() -> Harness<'static, KeyboardBindings> {
        Harness::builder()
            .with_size(egui::vec2(600.0, 2400.0))
            .build_ui_state(
                |ui, bindings: &mut KeyboardBindings| {
                    // Mirrors `App::handle_shortcuts`, which parks input for
                    // the editor rather than dispatching it while a chord is
                    // being captured.
                    if recording(ui.ctx()) {
                        let events = ui
                            .ctx()
                            .input_mut(|input| std::mem::take(&mut input.events));
                        stash_recorded_events(ui.ctx(), events);
                    }
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
    fn selecting_an_action_expands_the_editor_under_its_own_row() {
        let mut harness = editor_harness();
        harness.run();
        harness
            .get_by_role_and_label(accesskit::Role::Button, "New Session")
            .click();
        harness.run();

        let row = harness
            .get_by_role_and_label(accesskit::Role::Button, "New Session")
            .rect();
        let next_row = harness
            .get_by_role_and_label(accesskit::Role::Button, "Start Local Shell")
            .rect();
        let field = harness.get_by_label("Binding chord").rect();
        assert!(
            field.top() > row.bottom(),
            "the editor must open under the row it belongs to, not at the end of the table"
        );
        assert!(
            field.bottom() < next_row.top(),
            "the editor must push the following rows down rather than overlap them"
        );

        harness
            .get_by_role_and_label(accesskit::Role::Button, "Start Local Shell")
            .click();
        harness.run();
        assert_eq!(
            harness.query_all_by_label("Binding chord").count(),
            1,
            "choosing another action must close the editor that was already open"
        );
        let moved = harness.get_by_label("Binding chord").rect();
        let row = harness
            .get_by_role_and_label(accesskit::Role::Button, "Start Local Shell")
            .rect();
        assert!(
            moved.top() > row.bottom(),
            "the editor must follow the selection to its new row"
        );
    }

    #[test]
    fn chord_keycaps_compact_into_columns_anchored_on_the_key() {
        let mac = cfg!(target_os = "macos");
        let primary = if mac { "⌘" } else { "Ctrl" };

        // Column 0 is the key, and every chord fills leftwards from it, so
        // the key column stays straight no matter how many modifiers precede
        // it and no row reserves space for modifiers it does not use.
        let palette = keycap_slots("Primary+Shift+P");
        assert_eq!(palette[0].as_deref(), Some("P"));
        assert_eq!(palette[1].as_deref(), Some("Shift"));
        assert_eq!(palette[2].as_deref(), Some(primary));
        assert!(
            palette[3].is_none() && palette[4].is_none(),
            "unused columns must be the outermost ones, not gaps inside the chord"
        );

        let short = keycap_slots("Primary+O");
        assert_eq!(short[0].as_deref(), Some("O"), "the key shares one column");
        assert_eq!(
            short[1].as_deref(),
            Some(primary),
            "a shorter chord compacts towards the key rather than leaving a hole"
        );
        assert!(short[2].is_none());

        let control = keycap_slots("Ctrl+Shift+Tab");
        assert_eq!(control[0].as_deref(), Some("Tab"));
        assert_eq!(control[1].as_deref(), Some("Shift"));
        assert_eq!(
            control[2].as_deref(),
            Some(if mac { "Control" } else { "Ctrl" })
        );
        assert_eq!(
            keycap_slots("Primary+Alt+R")[1].as_deref(),
            Some(if mac { "Option" } else { "Alt" })
        );
        assert!(
            keycap_slots("").iter().all(Option::is_none),
            "an unbound action occupies no column"
        );

        // Every default chord must fit the columns the renderer reserves, or
        // a keycap would be dropped instead of drawn.
        for action in Action::ALL {
            let chord = action.default_chord(mac);
            let drawn = keycap_slots(chord)
                .iter()
                .filter(|slot| slot.is_some())
                .count();
            let expected = if chord.is_empty() {
                0
            } else {
                chord.split('+').count()
            };
            assert_eq!(drawn, expected, "every part of {chord} needs a column");
            assert!(expected <= CHORD_SLOTS);
        }
    }

    #[test]
    fn clicking_the_open_action_again_closes_its_editor() {
        let mut harness = editor_harness();
        harness.run();
        harness
            .get_by_role_and_label(accesskit::Role::Button, "New Session")
            .click();
        harness.run();
        assert!(
            harness.query_by_label("⌄").is_some(),
            "expanded rows must advertise their collapse affordance"
        );
        assert_eq!(
            harness.query_all_by_label("Binding chord").count(),
            1,
            "the first click must open the editor"
        );
        harness
            .get_by_role_and_label(accesskit::Role::Button, "New Session")
            .click();
        harness.run();
        assert!(
            harness.query_by_label("⌄").is_none(),
            "the chevron must return to the collapsed state"
        );
        assert_eq!(
            harness.query_all_by_label("Binding chord").count(),
            0,
            "clicking the open action again must close it"
        );
    }

    #[test]
    fn recorded_chords_use_the_portable_modifier_spelling() {
        let primary = Modifiers {
            mac_cmd: true,
            command: true,
            ..Default::default()
        };
        assert_eq!(
            recorded_chord(Key::P, primary | Modifiers::SHIFT, true).as_deref(),
            Some("Primary+Shift+P"),
            "Command on macOS must record as the portable Primary, not Command"
        );
        assert_eq!(
            recorded_chord(Key::P, Modifiers::CTRL | Modifiers::SHIFT, false).as_deref(),
            Some("Primary+Shift+P"),
            "Ctrl off macOS is the same portable modifier"
        );
        assert_eq!(
            recorded_chord(Key::R, Modifiers::CTRL, true).as_deref(),
            Some("Ctrl+R"),
            "macOS Control is a modifier of its own and stays spelled out"
        );
        assert_eq!(
            recorded_chord(Key::R, primary | Modifiers::ALT, true).as_deref(),
            Some("Primary+Alt+R"),
            "modifier order must match the stored default chords"
        );
        assert_eq!(
            recorded_chord(Key::Comma, primary, true).as_deref(),
            Some("Primary+Comma")
        );
        assert_eq!(
            recorded_chord(Key::Num1, primary, true).as_deref(),
            Some("Primary+1")
        );
        assert!(
            recorded_chord(Key::ArrowLeft, primary, true).is_none(),
            "keys the chord schema cannot express must be rejected, not recorded"
        );
        for action in Action::ALL {
            for mac in [true, false] {
                let chord = action.default_chord(mac);
                if chord.is_empty() {
                    continue;
                }
                assert!(
                    Chord::parse(chord, mac).is_ok(),
                    "recorder ordering is only canonical if defaults parse: {chord}"
                );
            }
        }
    }

    #[test]
    fn keyboard_editor_captures_a_pressed_chord_instead_of_firing_it() {
        let mac = cfg!(target_os = "macos");
        let mut harness = editor_harness();
        harness.run();
        harness
            .get_by_role_and_label(accesskit::Role::Button, "New Session")
            .click();
        harness.run();
        assert!(
            !recording(&harness.ctx),
            "the editor must not hold the keyboard until capture is requested"
        );
        harness.get_by_label("Record shortcut").click();
        harness.run();
        assert!(
            recording(&harness.ctx),
            "requesting capture must tell the dispatcher to hand over its input"
        );

        harness.key_press_modifiers(Modifiers::CTRL | Modifiers::SHIFT, Key::F8);
        harness.run();
        harness.run();
        assert_eq!(
            harness.state().effective(Action::NewSession, mac),
            if mac {
                "Ctrl+Shift+F8"
            } else {
                "Primary+Shift+F8"
            },
            "the pressed combination must become the binding"
        );
        assert!(
            !recording(&harness.ctx),
            "capture must end once a chord has been taken"
        );
    }

    #[test]
    fn keyboard_capture_is_cancelled_by_escape_and_leaves_the_binding_alone() {
        let mut harness = editor_harness();
        harness.run();
        harness
            .get_by_role_and_label(accesskit::Role::Button, "New Session")
            .click();
        harness.run();
        harness.get_by_label("Record shortcut").click();
        harness.run();
        harness.key_press(Key::Escape);
        harness.run();
        harness.run();
        assert!(
            !recording(&harness.ctx),
            "Escape must release the keyboard back to the rest of the app"
        );
        assert!(
            harness.state().is_empty(),
            "a cancelled capture must not change any binding"
        );
    }

    #[test]
    fn stashing_recorded_events_releases_the_keyboard_for_the_next_frame() {
        // The editor re-arms every frame it draws, so a Settings screen that
        // stops rendering mid-capture cannot strand the keyboard.
        let ctx = egui::Context::default();
        ctx.data_mut(|data| data.insert_temp(egui::Id::new(RECORDING_ID), true));
        assert!(recording(&ctx));
        stash_recorded_events(&ctx, Vec::new());
        assert!(!recording(&ctx));
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
        assert!(
            harness
                .query_by_label("Open the New Session launcher.")
                .is_some(),
            "collapsed rows must show each action description"
        );

        harness.get_by_label("Search actions…").click();
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
        harness
            .get_by_role_and_label(accesskit::Role::Button, "New Session")
            .click();
        harness.run();

        // Scope follows from the action, so it is reported as read-only
        // metadata -- with the sentence explaining what it means -- rather
        // than offered as an editable field.
        assert!(harness.query_by_label("Global").is_some());
        assert!(harness
            .query_by_label("Applies anywhere in fesTerm.")
            .is_some());
        assert!(harness.query_by_label("Default").is_some());
        assert!(harness.query_by_label("Current").is_some());
        assert!(harness.query_by_label("Binding chord").is_some());
        assert!(harness.query_by_label("Current shortcut").is_some());
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
            harness.query_all_by_label("Customized").count(),
            1,
            "only the customized action should show the badge"
        );
        assert_eq!(
            harness.query_all_by_label("F9").count(),
            3,
            "the assigned chord must render in the row, metadata, and current-shortcut panel"
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
        harness.get_by_label("Clear binding").click();
        harness.run();
        assert_eq!(harness.state().effective(Action::NewSession, false), "");
        harness.get_by_label("Restore default").click();
        harness.run();
        assert!(harness.state().is_empty());
        assert!(
            is_disabled(&harness, "Restore default"),
            "a per-action reset must stay visible but inert once the action is back to its default"
        );
        assert!(
            is_disabled(&harness, "Reset all…"),
            "the all-bindings reset must stay visible but inert while nothing is customized"
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
        harness.get_by_label("Reset all…").click();
        harness.run();
        assert!(harness.state().is_empty());
    }
}
