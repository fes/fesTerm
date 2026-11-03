//! App-owned terminal keyboards. Keys use the same input path as the native
//! keyboard; no draft, shell history, autocorrection or wire encoding lives here.

use super::key_press::{paint_glyph, Fired, Glyph, KeyPress, Repeat};
use super::terminal_chrome::{self, KeyRowAction, KeyRowOutput, KeyRowState};
use super::theme::{color, type_scale};
use super::widgets::{labelled, paint_fit};
use eframe::egui::{self, Align2, Color32, CornerRadius, Rect, Response, Sense, Stroke, Ui};
use festerm_core::Key;
use festerm_ui_egui::icon::Icon;

#[cfg(test)]
#[path = "terminal_keyboard_tests.rs"]
mod tests;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum KeyboardKind {
    #[default]
    Native,
    HoldSymbols,
    ShellRow,
}

impl KeyboardKind {
    pub const ALL: [Self; 3] = [Self::Native, Self::HoldSymbols, Self::ShellRow];

    pub fn label(self) -> &'static str {
        match self {
            Self::Native => "Built-in keyboard",
            Self::HoldSymbols => "Terminal: Hold symbols",
            Self::ShellRow => "Terminal: Shell row",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::HoldSymbols => "hold-symbols",
            Self::ShellRow => "shell-row",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.key() == key.trim())
    }
}

const LETTERS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
const ALTERNATES: [&str; 3] = ["/\\-_'\"|&$~", "@#=+*()[]", "{}<>;:?"];
const SYMBOLS: [&str; 3] = ["[]{}()<>=+", "@#*%!;:?`", "/\\-_|&$"];
const BOARD: Color32 = Color32::from_rgb(27, 42, 64);
const KEY: Color32 = Color32::from_rgb(48, 65, 91);
const SPECIAL: Color32 = Color32::from_rgb(35, 51, 75);
const SELECTED: Color32 = Color32::from_rgb(139, 185, 255);
const SELECTED_INK: Color32 = Color32::from_rgb(16, 33, 59);

#[derive(Default)]
pub struct TerminalKeyboard {
    pub symbols: bool,
    /// Shared with the accessory row and extended keys so feedback is uniform.
    pub keys: KeyPress,
    bounds: Option<Rect>,
}

impl TerminalKeyboard {
    pub fn cancel(&mut self) {
        self.keys.cancel();
    }

    pub fn reset(&mut self) {
        self.symbols = false;
        self.cancel();
    }

    fn begin(&mut self, ui: &Ui, bounds: Rect) {
        let resized = self.bounds.is_some_and(|previous| previous != bounds);
        self.bounds = Some(bounds);
        self.keys.begin(ui, resized);
    }

    fn key(
        &mut self,
        ui: &Ui,
        response: &Response,
        key: Key,
        glyph: Glyph,
        repeat: Repeat,
    ) -> Option<Key> {
        self.keys
            .key(ui, response, glyph, repeat)
            .map(|fired| match fired {
                Fired::Base => key,
                Fired::Alternate(character) => Key::Character(character),
            })
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        kind: KeyboardKind,
        state: &KeyRowState,
        available_height: f32,
    ) -> KeyRowOutput {
        let shell = kind == KeyboardKind::ShellRow;
        let rows = if shell { 7.0 } else { 6.0 };
        let budget = (available_height - 80.0).max(100.0);
        let gap = if budget < 320.0 { 4.0 } else { 6.0 };
        let height = ((budget - 12.0 - gap * (rows - 1.0)) / rows).clamp(32.0, 44.0);
        let utility_height = height.max(36.0);
        let full_height = utility_height + height * (rows - 1.0) + gap * (rows - 1.0) + 12.0;
        let (outer, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), full_height.min(budget)),
            Sense::hover(),
        );
        ui.painter().rect_filled(outer, 0.0, BOARD);
        ui.painter().hline(
            outer.x_range(),
            outer.top(),
            Stroke::new(1.0, color::BORDER),
        );
        self.keys.latch(state.control || state.alt || state.shift);
        self.begin(ui, outer);
        let width = (outer.width() - 14.0).min(720.0);
        let inner = Rect::from_center_size(
            outer.center(),
            egui::vec2(width, (outer.height() - 12.0).max(1.0)),
        );
        let mut output = KeyRowOutput::default();
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
            ui.set_clip_rect(inner);
            ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
            ui.spacing_mut().interact_size.y = height;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let width = (ui.available_width() - 4.0 * 10.0) / 11.0;
                for (label, key) in [("Esc", Key::Escape), ("Tab", Key::Tab)] {
                    let response = draw_key(ui, width, utility_height, label, None, false, true);
                    if let Some(key) =
                        self.key(ui, &response, key, Glyph::text(label), Repeat::Auto)
                    {
                        output.action = KeyRowAction::Key(key);
                    }
                }
                for (label, action, active) in [
                    ("Ctrl", KeyRowAction::ToggleControl, state.control),
                    ("Alt", KeyRowAction::ToggleAlt, state.alt),
                    ("Shift", KeyRowAction::ToggleShift, state.shift),
                ] {
                    let response = draw_key(ui, width, utility_height, label, None, active, true);
                    if label == "Ctrl" {
                        output.control_rect = response.rect;
                    }
                    if self.keys.toggle(ui, &response, Glyph::text(label)) {
                        output.action = action;
                    }
                }
                for (label, key) in [
                    ("Left", Key::ArrowLeft),
                    ("Down", Key::ArrowDown),
                    ("Up", Key::ArrowUp),
                    ("Right", Key::ArrowRight),
                ] {
                    let response = arrow_key(ui, width, utility_height, label, key);
                    if let Some(key) = self.key(ui, &response, key, Glyph::Arrow(key), Repeat::Auto)
                    {
                        output.action = KeyRowAction::Key(key);
                    }
                }
                let switch = terminal_chrome::keyboard_switch_button(ui, width, utility_height);
                if self
                    .keys
                    .toggle(ui, &switch, Glyph::Icon(Icon::RemoteGlobe))
                {
                    output.action = KeyRowAction::SwitchKeyboard;
                }
                let hide =
                    terminal_chrome::keyboard_visibility_button(ui, width, utility_height, true);
                if self
                    .keys
                    .toggle(ui, &hide, Glyph::Icon(Icon::KeyboardShortcuts))
                {
                    output.action = KeyRowAction::ToggleKeyboard;
                }
            });
            let middle_height = (inner.height() - utility_height - height - gap * 2.0).max(1.0);
            egui::ScrollArea::vertical()
                .id_salt("terminal-keyboard-typing")
                .max_height(middle_height)
                .min_scrolled_height(0.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
                    if shell {
                        self.character_row(ui, ALTERNATES[0], None, height, &mut output);
                    }
                    self.character_row(
                        ui,
                        if state.shift {
                            "!@#$%^&*()"
                        } else {
                            "1234567890"
                        },
                        None,
                        height,
                        &mut output,
                    );
                    for row in 0..3 {
                        let characters = if self.symbols {
                            SYMBOLS[row]
                        } else {
                            LETTERS[row]
                        };
                        let letters = if state.shift && !self.symbols {
                            characters.to_ascii_uppercase()
                        } else {
                            characters.to_owned()
                        };
                        let alternate = (kind == KeyboardKind::HoldSymbols && !self.symbols)
                            .then_some(ALTERNATES[row]);
                        ui.push_id(row, |ui| {
                            if row == 2 {
                                ui.horizontal(|ui| {
                                    let unit = (ui.available_width() - gap * 8.0) / 9.8;
                                    let response = draw_key(
                                        ui,
                                        unit * 1.4,
                                        height,
                                        "Typing-row Shift",
                                        None,
                                        state.shift,
                                        true,
                                    );
                                    if self.keys.toggle(ui, &response, Glyph::Shift) {
                                        output.action = KeyRowAction::ToggleShift;
                                    }
                                    self.characters(
                                        ui,
                                        &letters,
                                        alternate,
                                        unit,
                                        height,
                                        &mut output,
                                    );
                                    let response = draw_key(
                                        ui,
                                        unit * 1.4,
                                        height,
                                        "Backspace",
                                        None,
                                        false,
                                        true,
                                    );
                                    if let Some(key) = self.key(
                                        ui,
                                        &response,
                                        Key::Backspace,
                                        Glyph::Backspace,
                                        Repeat::Auto,
                                    ) {
                                        output.action = KeyRowAction::Key(key);
                                    }
                                });
                            } else {
                                let inset = if row == 1 { width * 0.046 } else { 0.0 };
                                egui::Frame::new()
                                    .inner_margin(egui::Margin::symmetric(inset as i8, 0))
                                    .show(ui, |ui| {
                                        self.character_row(
                                            ui,
                                            &letters,
                                            alternate,
                                            height,
                                            &mut output,
                                        );
                                    });
                            }
                        });
                    }
                });
            ui.horizontal(|ui| {
                // Space stays centered with equal flanks: Extended, #+= and
                // comma on the left; period and Return on the right.
                let space_width = ui.available_width() * 0.48;
                let side = (ui.available_width() - space_width) / 2.0 - gap;
                let small = (side - gap * 2.0) * 0.3;
                let layer = (side - gap * 2.0 - small) / 2.0;
                let extended = draw_key(ui, layer, height, "", None, false, true);
                paint_glyph(
                    ui.painter(),
                    &Glyph::Grid,
                    extended.rect.center(),
                    1.0,
                    color::TEXT,
                );
                let extended = labelled(extended, "Extended keys", ui.is_enabled());
                if self.keys.toggle(ui, &extended, Glyph::Grid) {
                    output.action = KeyRowAction::TogglePanel;
                }
                let label = if self.symbols { "ABC" } else { "#+=" };
                let response = draw_key(ui, layer, height, label, None, self.symbols, true);
                if self.keys.toggle(ui, &response, Glyph::text(label)) {
                    self.symbols = !self.symbols;
                    self.cancel();
                    output.click = true;
                }
                for (label, width, key) in [
                    (",", small, Key::Character(',')),
                    ("Space", space_width, Key::Character(' ')),
                    (".", small, Key::Character('.')),
                    ("Return", side - gap - small, Key::Enter),
                ] {
                    let response = draw_key(
                        ui,
                        width,
                        height,
                        label,
                        None,
                        false,
                        matches!(key, Key::Enter | Key::Character(' ')),
                    );
                    if let Some(key) =
                        self.key(ui, &response, key, Glyph::text(label), Repeat::Auto)
                    {
                        output.action = KeyRowAction::Key(key);
                    }
                }
            });
        });
        output.click |= output.action != KeyRowAction::None;
        output
    }

    fn character_row(
        &mut self,
        ui: &mut Ui,
        characters: &str,
        alternates: Option<&str>,
        height: f32,
        output: &mut KeyRowOutput,
    ) {
        ui.horizontal(|ui| {
            let count = characters.chars().count() as f32;
            let width =
                (ui.available_width() - ui.spacing().item_spacing.x * (count - 1.0)) / count;
            self.characters(ui, characters, alternates, width, height, output);
        });
    }

    fn characters(
        &mut self,
        ui: &mut Ui,
        characters: &str,
        alternates: Option<&str>,
        width: f32,
        height: f32,
        output: &mut KeyRowOutput,
    ) {
        for (index, character) in characters.chars().enumerate() {
            let alternate = alternates.and_then(|row| row.chars().nth(index));
            let response = draw_key(
                ui,
                width,
                height,
                &character.to_string(),
                alternate,
                false,
                false,
            );
            let repeat = alternate.map_or(Repeat::Auto, Repeat::Hold);
            let glyph = Glyph::Text(character.to_string());
            if let Some(key) = self.key(ui, &response, Key::Character(character), glyph, repeat) {
                output.action = KeyRowAction::Key(key);
            }
        }
    }
}

fn draw_key(
    ui: &mut Ui,
    width: f32,
    height: f32,
    label: &str,
    alternate: Option<char>,
    active: bool,
    special: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), Sense::CLICK);
    let pressed = response.is_pointer_button_down_on();
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(7),
        if active || label == "Return" {
            SELECTED
        } else if special || pressed {
            SPECIAL
        } else {
            KEY
        },
    );
    let ink = if active || label == "Return" {
        SELECTED_INK
    } else {
        color::TEXT
    };
    if label == "Backspace" {
        paint_glyph(ui.painter(), &Glyph::Backspace, rect.center(), 1.0, ink);
    } else if label == "Typing-row Shift" {
        paint_glyph(ui.painter(), &Glyph::Shift, rect.center(), 1.0, ink);
    } else {
        paint_fit(
            ui.painter(),
            rect.center() + egui::vec2(0.0, if alternate.is_some() { 4.0 } else { 0.0 }),
            Align2::CENTER_CENTER,
            label,
            if special {
                type_scale::font(12.0)
            } else if label
                .chars()
                .all(|character| character.is_ascii_alphabetic())
            {
                type_scale::font(19.0)
            } else {
                type_scale::mono(
                    if label.chars().all(|character| character.is_ascii_digit()) {
                        18.0
                    } else {
                        19.0
                    },
                )
            },
            ink,
            rect.width() - 2.0,
        );
    }
    if let Some(alternate) = alternate {
        ui.painter().text(
            rect.right_top() + egui::vec2(-4.0, 3.0),
            Align2::RIGHT_TOP,
            alternate,
            type_scale::mono(10.0),
            color::ACCENT_TEXT,
        );
    }
    if matches!(
        label,
        "Ctrl" | "Alt" | "Shift" | "Typing-row Shift" | "#+=" | "ABC"
    ) {
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), active, label)
        });
        response
    } else {
        labelled(response, label, ui.is_enabled())
    }
}

fn arrow_key(ui: &mut Ui, width: f32, height: f32, label: &str, key: Key) -> Response {
    let response = draw_key(ui, width, height, "", None, false, true);
    paint_glyph(
        ui.painter(),
        &Glyph::Arrow(key),
        response.rect.center(),
        1.0,
        color::TEXT,
    );
    labelled(response, label, true)
}
