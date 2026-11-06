//! App-owned terminal keyboards. Keys use the same input path as the native
//! keyboard; no draft, shell history, autocorrection or wire encoding lives here.

use super::key_press::{paint_glyph, Fired, Glyph, KeyPress, Repeat};
use super::terminal_chrome::{self, KeyRowAction, KeyRowOutput, KeyRowState};
use super::theme::{color, radius, type_scale};
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
/// Gboard's number-row hints, so the long-press layer matches a familiar phone layout.
const ALTERNATES: [&str; 3] = ["%^~|[]<>{}", "@#&*-+=()", "_$\"':;/"];
const COMMA_ALTERNATE: char = '`';
const PERIOD_ALTERNATE: char = '?';
const SHELL_ROW: &str = "/\\-_'\"|&$~";
/// Characters the shifted number row does not already offer, quotes included.
const SYMBOLS: [&str; 3] = ["[]{}()<>=+", "-/\\|_~`'\"", ";:?!&*$"];
/// Where each letter/symbol row splits: `qwert|yuiop`, `asdfg|hjkl`, `zxcv|bnm`.
const SPLIT_AT: [usize; 3] = [5, 5, 4];
/// Auto splits keyboards at least this wide on landscape screens (iPad
/// landscape, unfolded foldables, spanned dual screens).
const AUTO_SPLIT_WIDTH: f32 = 900.0;
/// Narrowest half that keeps five letter keys comfortably tappable.
const MIN_HALF: f32 = 240.0;
/// Widest half, so each half stays within thumb reach of its edge.
const MAX_HALF: f32 = 420.0;
const MIN_CENTER_GAP: f32 = 40.0;
/// Keys stay this far from a reported hinge or fold.
const HINGE_CLEARANCE: f32 = 8.0;
const BOARD: Color32 = color::TERMINAL_CHROME;
const KEY: Color32 = color::TERMINAL_KEY;
const SPECIAL: Color32 = color::TERMINAL_KEY_MUTED;
const SELECTED: Color32 = color::TERMINAL_KEY_SELECTED;
const SELECTED_INK: Color32 = color::TERMINAL_KEY_SELECTED_INK;

/// Split-keyboard preference for the app-owned terminal keyboards.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SplitMode {
    /// Split on wide landscape screens and across a reported hinge.
    #[default]
    Auto,
    /// Split whenever both halves fit.
    Always,
    Never,
}

impl SplitMode {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Always, Self::Never];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Always => "Split",
            Self::Never => "Full",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.key() == key.trim())
    }
}

/// A vertical fold or hinge, as fractions of the window width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hinge {
    pub left: f32,
    pub right: f32,
}

/// The left and right half rectangles for a split keyboard in `area`, or
/// `None` for the full keyboard. Halves hug the outer edges, are equal width
/// and never overlap a hinge that crosses the keyboard.
pub(crate) fn split_halves(
    area: Rect,
    screen: Rect,
    hinge: Option<Hinge>,
    mode: SplitMode,
) -> Option<[Rect; 2]> {
    if mode == SplitMode::Never {
        return None;
    }
    let fold = hinge
        .filter(|hinge| {
            hinge.left.is_finite()
                && hinge.right.is_finite()
                && 0.0 <= hinge.left
                && hinge.left <= hinge.right
                && hinge.right <= 1.0
        })
        .map(|hinge| {
            (
                screen.left() + screen.width() * hinge.left,
                screen.left() + screen.width() * hinge.right,
            )
        })
        .filter(|&(left, right)| left < area.right() && right > area.left());
    let wide = area.width() >= AUTO_SPLIT_WIDTH && screen.width() > screen.height();
    if mode == SplitMode::Auto && fold.is_none() && !wide {
        return None;
    }
    let (inner_left, inner_right) = match fold {
        Some((left, right)) => (left - HINGE_CLEARANCE, right + HINGE_CLEARANCE),
        None => (
            area.center().x - MIN_CENTER_GAP / 2.0,
            area.center().x + MIN_CENTER_GAP / 2.0,
        ),
    };
    let width = (inner_left - area.left())
        .min(area.right() - inner_right)
        .min(MAX_HALF);
    if width < MIN_HALF {
        return None;
    }
    Some([
        Rect::from_min_max(
            area.left_top(),
            egui::pos2(area.left() + width, area.bottom()),
        ),
        Rect::from_min_max(
            egui::pos2(area.right() - width, area.top()),
            area.right_bottom(),
        ),
    ])
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Part {
    Full,
    Left,
    Right,
}

impl Part {
    fn slice(self, row: &str, at: usize) -> &str {
        let at = row
            .char_indices()
            .nth(at)
            .map_or(row.len(), |(index, _)| index);
        match self {
            Self::Full => row,
            Self::Left => &row[..at],
            Self::Right => &row[at..],
        }
    }
}

struct Board<'a> {
    kind: KeyboardKind,
    state: &'a KeyRowState,
    context: KeyboardContext,
    height: f32,
    utility_height: f32,
    gap: f32,
}

enum Bottom {
    Globe,
    Layer,
    Comma,
    Space,
    Period,
    Return,
}

#[derive(Default)]
pub struct TerminalKeyboard {
    pub symbols: bool,
    /// Shared with the accessory row and extended keys so feedback is uniform.
    pub keys: KeyPress,
    pub split: SplitMode,
    /// The platform-reported vertical hinge, if any (Android foldables).
    pub hinge: Option<Hinge>,
    bounds: Option<Rect>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyboardContext {
    Terminal,
    Source,
    Draft,
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
        self.show_for(ui, kind, state, available_height, KeyboardContext::Terminal)
    }

    pub(super) fn show_for(
        &mut self,
        ui: &mut Ui,
        kind: KeyboardKind,
        state: &KeyRowState,
        available_height: f32,
        context: KeyboardContext,
    ) -> KeyRowOutput {
        let shell = kind == KeyboardKind::ShellRow;
        let rows = if shell { 7.0 } else { 6.0 };
        let budget = (available_height - 80.0).max(100.0);
        let gap = if budget < 320.0 { 4.0 } else { 6.0 };
        let key_budget = budget - 12.0 - gap * (rows - 1.0);
        let height = (key_budget / rows)
            .min((key_budget - 36.0) / (rows - 1.0))
            .clamp(24.0, 44.0);
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
        let area = outer.shrink2(egui::vec2(7.0, 6.0));
        let halves = split_halves(area, ui.ctx().viewport_rect(), self.hinge, self.split);
        let board = Board {
            kind,
            state,
            context,
            height,
            utility_height,
            gap,
        };
        let mut output = KeyRowOutput::default();
        if let Some([left, right]) = halves {
            // Stagger and key proportions follow the full board the halves came from.
            let inset = (left.width() * 2.0).min(720.0) * 0.046;
            for (part, rect) in [(Part::Left, left), (Part::Right, right)] {
                self.board(ui, &board, part, rect, inset, &mut output);
            }
        } else {
            let width = area.width().min(720.0);
            let inner =
                Rect::from_center_size(outer.center(), egui::vec2(width, area.height().max(1.0)));
            self.board(ui, &board, Part::Full, inner, width * 0.046, &mut output);
        }
        output.click |= output.action != KeyRowAction::None;
        output
    }

    fn board(
        &mut self,
        ui: &mut Ui,
        board: &Board<'_>,
        part: Part,
        inner: Rect,
        inset: f32,
        output: &mut KeyRowOutput,
    ) {
        let Board {
            kind,
            state,
            context,
            height,
            utility_height,
            gap,
        } = *board;
        let shell = kind == KeyboardKind::ShellRow;
        ui.scope_builder(egui::UiBuilder::new().id_salt(part).max_rect(inner), |ui| {
            ui.set_clip_rect(inner);
            ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
            ui.spacing_mut().interact_size.y = height;
            ui.horizontal(|ui| {
                let count = if part == Part::Full { 12.0 } else { 6.0 };
                let gap = ((ui.available_width() - 24.0 * count) / (count - 1.0)).clamp(0.0, 4.0);
                ui.spacing_mut().item_spacing.x = gap;
                let width = (ui.available_width() - gap * (count - 1.0)) / count;
                // Split: Esc, Tab, Ctrl, Alt, Shift and Mic on the left half;
                // arrows, Extended keys and Hide on the right.
                let (left, right) = (part != Part::Right, part != Part::Left);
                let leading = match context {
                    KeyboardContext::Terminal => [
                        ("Esc", KeyRowAction::Key(Key::Escape)),
                        ("Tab", KeyRowAction::Key(Key::Tab)),
                    ],
                    KeyboardContext::Source => {
                        [("Undo", KeyRowAction::Undo), ("Redo", KeyRowAction::Redo)]
                    }
                    KeyboardContext::Draft => [
                        ("All", KeyRowAction::SelectAll),
                        ("Paste", KeyRowAction::Paste),
                    ],
                };
                for (label, action) in leading.into_iter().filter(|_| left) {
                    let glyph = match action {
                        KeyRowAction::Undo => Glyph::Icon(Icon::Undo),
                        KeyRowAction::Redo => Glyph::Icon(Icon::Redo),
                        KeyRowAction::Paste => Glyph::Icon(Icon::Paste),
                        _ => Glyph::text(label),
                    };
                    let response = if matches!(glyph, Glyph::Text(_)) {
                        draw_key(ui, width, utility_height, label, None, false, true)
                    } else {
                        glyph_key(ui, width, utility_height, label, glyph.clone())
                    };
                    let response = if context == KeyboardContext::Source {
                        labelled(response, &format!("Keyboard {label}"), ui.is_enabled())
                    } else {
                        response
                    };
                    let fired = if let KeyRowAction::Key(key) = action {
                        self.key(ui, &response, key, glyph.clone(), Repeat::Auto)
                            .is_some()
                    } else {
                        self.keys.toggle(ui, &response, glyph)
                    };
                    if fired {
                        output.action = action;
                    }
                }
                let shortcuts = if context == KeyboardContext::Terminal {
                    [
                        ("Ctrl", KeyRowAction::ToggleControl, state.control),
                        ("Alt", KeyRowAction::ToggleAlt, state.alt),
                    ]
                } else {
                    [
                        ("Tab", KeyRowAction::Key(Key::Tab), false),
                        if context == KeyboardContext::Source {
                            ("Outdent", KeyRowAction::Outdent, false)
                        } else {
                            ("Copy", KeyRowAction::Copy, false)
                        },
                    ]
                };
                for (label, action, active) in shortcuts
                    .into_iter()
                    .chain([("Shift", KeyRowAction::ToggleShift, state.shift)])
                    .filter(|_| left)
                {
                    let glyph = match action {
                        KeyRowAction::Copy => Glyph::Icon(Icon::Copy),
                        KeyRowAction::Outdent => Glyph::Tab { reverse: true },
                        KeyRowAction::Key(Key::Tab) => Glyph::Tab { reverse: false },
                        KeyRowAction::ToggleShift => Glyph::Shift,
                        _ => Glyph::text(label),
                    };
                    let response = if matches!(glyph, Glyph::Text(_)) {
                        draw_key(ui, width, utility_height, label, None, active, true)
                    } else {
                        glyph_key_active(ui, width, utility_height, label, glyph.clone(), active)
                    };
                    if label == "Ctrl" {
                        output.control_rect = response.rect;
                    }
                    if self.keys.toggle(ui, &response, glyph) {
                        output.action = action;
                    }
                }
                if part == Part::Left {
                    self.mic_key(ui, width, utility_height, output);
                }
                for (label, key) in [
                    ("Left", Key::ArrowLeft),
                    ("Up", Key::ArrowUp),
                    ("Down", Key::ArrowDown),
                    ("Right", Key::ArrowRight),
                ]
                .into_iter()
                .filter(|_| right)
                {
                    let response = arrow_key(ui, width, utility_height, label, key);
                    if let Some(key) = self.key(ui, &response, key, Glyph::Arrow(key), Repeat::Auto)
                    {
                        output.action = KeyRowAction::Key(key);
                    }
                }
                if part == Part::Full {
                    self.mic_key(ui, width, utility_height, output);
                }
                if right {
                    let extended = glyph_key(
                        ui,
                        width,
                        utility_height,
                        if context == KeyboardContext::Terminal {
                            "Extended keys"
                        } else {
                            "Select all"
                        },
                        Glyph::Grid,
                    );
                    if self.keys.toggle(ui, &extended, Glyph::Grid) {
                        output.action = if context == KeyboardContext::Terminal {
                            KeyRowAction::TogglePanel
                        } else {
                            KeyRowAction::SelectAll
                        };
                    }
                }
                if right {
                    let hide = terminal_chrome::keyboard_visibility_button(
                        ui,
                        width,
                        utility_height,
                        true,
                    );
                    if self
                        .keys
                        .toggle(ui, &hide, Glyph::Icon(Icon::KeyboardShortcuts))
                    {
                        output.action = KeyRowAction::ToggleKeyboard;
                    }
                }
            });
            let middle_height = (inner.height() - utility_height - height - gap * 2.0).max(1.0);
            egui::ScrollArea::vertical()
                .id_salt(("terminal-keyboard-typing", part))
                .max_height(middle_height)
                .min_scrolled_height(0.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
                    if shell {
                        self.character_row(ui, part.slice(SHELL_ROW, 5), None, height, output);
                    }
                    let digits = if state.shift {
                        "!@#$%^&*()"
                    } else {
                        "1234567890"
                    };
                    self.character_row(ui, part.slice(digits, 5), None, height, output);
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
                        let letters = part.slice(&letters, SPLIT_AT[row]);
                        let alternate = (kind == KeyboardKind::HoldSymbols && !self.symbols)
                            .then(|| part.slice(ALTERNATES[row], SPLIT_AT[row]));
                        ui.push_id(row, |ui| {
                            if row == 2 {
                                self.lower_row(ui, part, letters, alternate, height, state, output);
                            } else if row == 1 && part != Part::Full {
                                // The stagger keeps half-board keys the width of a
                                // five-key left half, so `hjkl` sits under `yuiop`.
                                ui.horizontal(|ui| {
                                    let unit = (ui.available_width() - inset - gap * 4.0) / 5.0;
                                    ui.add_space(inset);
                                    self.characters(ui, letters, alternate, unit, height, output);
                                });
                            } else {
                                let inset = if row == 1 { inset } else { 0.0 };
                                egui::Frame::new()
                                    .inner_margin(egui::Margin::symmetric(inset as i8, 0))
                                    .show(ui, |ui| {
                                        self.character_row(ui, letters, alternate, height, output);
                                    });
                            }
                        });
                    }
                });
            self.bottom_row(ui, part, kind, height, gap, output);
        });
    }

    fn mic_key(&mut self, ui: &mut Ui, width: f32, height: f32, output: &mut KeyRowOutput) {
        let mic = glyph_key(ui, width, height, "Mic", Glyph::Icon(Icon::Microphone))
            .on_hover_text("On-device dictation");
        if self.keys.toggle(ui, &mic, Glyph::Icon(Icon::Microphone)) {
            output.action = KeyRowAction::Dictate;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_row(
        &mut self,
        ui: &mut Ui,
        part: Part,
        letters: &str,
        alternate: Option<&str>,
        height: f32,
        state: &KeyRowState,
        output: &mut KeyRowOutput,
    ) {
        ui.horizontal(|ui| {
            let gap = ui.spacing().item_spacing.x;
            // Full: Shift + 7 letters + Backspace. Halves share one unit so the
            // left's Shift + `zxcv` and the right's `bnm` + Backspace line up.
            let unit = if part == Part::Full {
                (ui.available_width() - gap * 8.0) / 9.8
            } else {
                (ui.available_width() - gap * 4.0) / 5.4
            };
            if part != Part::Right {
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
            }
            self.characters(ui, letters, alternate, unit, height, output);
            if part != Part::Left {
                let width = if part == Part::Full {
                    unit * 1.4
                } else {
                    ui.available_width()
                };
                let response = draw_key(ui, width, height, "Backspace", None, false, true);
                if let Some(key) = self.key(
                    ui,
                    &response,
                    Key::Backspace,
                    Glyph::Backspace,
                    Repeat::Auto,
                ) {
                    output.action = KeyRowAction::Key(key);
                }
            }
        });
    }

    fn bottom_row(
        &mut self,
        ui: &mut Ui,
        part: Part,
        kind: KeyboardKind,
        height: f32,
        gap: f32,
        output: &mut KeyRowOutput,
    ) {
        ui.horizontal(|ui| {
            let available = ui.available_width();
            let keys = match part {
                Part::Full => {
                    // Space stays centered with equal flanks: the globe, #+= and
                    // comma on the left; period and Return on the right.
                    let space = available * 0.48;
                    let side = (available - space) / 2.0 - gap;
                    let small = (side - gap * 2.0) * 0.3;
                    let layer = (side - gap * 2.0 - small) / 2.0;
                    vec![
                        (Bottom::Globe, layer),
                        (Bottom::Layer, layer),
                        (Bottom::Comma, small),
                        (Bottom::Space, space),
                        (Bottom::Period, small),
                        (Bottom::Return, side - gap - small),
                    ]
                }
                // Each half gets its own Space bar at the inner edge, iPad style.
                Part::Left => {
                    let unit = (available - gap * 5.0) / 6.0;
                    vec![
                        (Bottom::Globe, unit),
                        (Bottom::Layer, unit),
                        (Bottom::Comma, unit * 0.8),
                        (Bottom::Space, available - unit * 2.8 - gap * 3.0),
                    ]
                }
                Part::Right => {
                    let unit = (available - gap * 5.0) / 6.0;
                    vec![
                        (Bottom::Space, available - unit * 2.4 - gap * 2.0),
                        (Bottom::Period, unit * 0.8),
                        (Bottom::Return, unit * 1.6),
                    ]
                }
            };
            let holds = kind == KeyboardKind::HoldSymbols && !self.symbols;
            for (bottom, width) in keys {
                let (label, key, alternate) = match bottom {
                    Bottom::Globe => {
                        let globe = Glyph::Icon(Icon::RemoteGlobe);
                        let switch = glyph_key(ui, width, height, "Switch keyboard", globe.clone());
                        if self.keys.toggle(ui, &switch, globe) {
                            output.action = KeyRowAction::SwitchKeyboard;
                        }
                        continue;
                    }
                    Bottom::Layer => {
                        let label = if self.symbols { "ABC" } else { "#+=" };
                        let response = draw_key(ui, width, height, label, None, self.symbols, true);
                        if self.keys.toggle(ui, &response, Glyph::text(label)) {
                            self.symbols = !self.symbols;
                            self.cancel();
                            output.click = true;
                        }
                        continue;
                    }
                    Bottom::Comma => (",", Key::Character(','), Some(COMMA_ALTERNATE)),
                    Bottom::Space => ("Space", Key::Character(' '), None),
                    Bottom::Period => (".", Key::Character('.'), Some(PERIOD_ALTERNATE)),
                    Bottom::Return => ("Return", Key::Enter, None),
                };
                let alternate = alternate.filter(|_| holds);
                let response = draw_key(
                    ui,
                    width,
                    height,
                    label,
                    alternate,
                    false,
                    matches!(key, Key::Enter | Key::Character(' ')),
                );
                let repeat = alternate.map_or(Repeat::Auto, Repeat::Hold);
                if let Some(key) = self.key(ui, &response, key, Glyph::text(label), repeat) {
                    output.action = KeyRowAction::Key(key);
                }
            }
        });
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
        CornerRadius::same(radius::KEY),
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

fn glyph_key(ui: &mut Ui, width: f32, height: f32, label: &str, glyph: Glyph) -> Response {
    glyph_key_active(ui, width, height, label, glyph, false)
}

fn glyph_key_active(
    ui: &mut Ui,
    width: f32,
    height: f32,
    label: &str,
    glyph: Glyph,
    active: bool,
) -> Response {
    let response = draw_key(ui, width, height, "", None, active, true);
    paint_glyph(
        ui.painter(),
        &glyph,
        response.rect.center(),
        1.0,
        if active { SELECTED_INK } else { color::TEXT },
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), active, label)
    });
    response
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
