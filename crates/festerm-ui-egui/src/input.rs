use std::time::{Duration, Instant};

use egui::{Pos2, Response, Ui};
use festerm_core::{
    Dimensions, FocusEvent, InputEvent, InputEventOutcome, Key, Modifiers, MouseButton, MouseEvent,
    MouseEventKind, MouseWheel, Terminal,
};

use crate::{
    geometry::{clamped_cell_from_point, CellPosition},
    renderer::GridLayout,
    selection::{normalize_selection_position, selection_text, Selection},
    TerminalSnapshot,
};

/// How long a resize request should settle before an [`EncodedInputSink`]
/// forwards it to a real backend (e.g. a PTY's `ioctl(TIOCSWINSZ)` +
/// `SIGWINCH`).
///
/// The visible terminal grid reflows immediately, every frame, purely from
/// the measured window size, so a live OS-level drag can request dozens of
/// different sizes per second. An application-owned sink that debounces
/// against this interval before forwarding to its backend avoids racing a
/// child process's own resize-triggered redraw against a rapidly still-
/// changing size. Kept here (rather than duplicated in each application) so
/// the view can schedule a follow-up repaint at the same interval,
/// guaranteeing the debounced resize actually gets delivered even once the
/// window stops changing and no other frame would otherwise be scheduled.
pub const TERMINAL_RESIZE_DEBOUNCE: Duration = Duration::from_millis(120);

/// The observable result of routing an event through the core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputRoute {
    pub outcome: InputEventOutcome,
    /// Input bytes waiting before this view drains them to its application sink.
    pub queue_depth: usize,
    pub delivered_bytes: usize,
}

/// Content-free input metadata exposed by an application-owned sink.
///
/// Counters saturate in implementations so a no-session demo can remain
/// observable without retaining user input or terminal protocol bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InputSinkDiagnostics {
    pub event_count: u64,
    pub byte_count: u64,
    pub last_outcome: Option<InputEventOutcome>,
    pub last_queue_depth: usize,
}

/// Application-owned destination for bytes encoded by the core.
pub trait EncodedInputSink {
    /// Delivers transient encoded bytes. Implementations must not retain them
    /// unless they are an active session transport.
    fn record_encoded_input(&mut self, bytes: &[u8]);

    /// Observes content-free routing metadata after every routed core event.
    fn observe_input_route(&mut self, _route: InputRoute) {}

    /// Receives a resize after the application-owned terminal core accepts it.
    ///
    /// The UI does not know about PTYs or sessions. An application can forward
    /// this cell-space size to its active backend without giving the backend
    /// access to the terminal core. Implementations should debounce against
    /// [`TERMINAL_RESIZE_DEBOUNCE`] before forwarding to a real backend.
    fn record_terminal_resize(&mut self, _dimensions: Dimensions) {}

    /// Returns sink-owned, content-free diagnostics when available.
    fn input_diagnostics(&self) -> Option<InputSinkDiagnostics> {
        None
    }
}

/// Routes one typed UI event to the mode-aware core encoder and drains encoded
/// bytes into the application-owned sink. It intentionally performs no session
/// I/O itself.
pub fn route_input(
    terminal: &mut Terminal,
    event: InputEvent,
    sink: &mut impl EncodedInputSink,
) -> InputRoute {
    let outcome = terminal.handle_input(event);
    let queue_depth = terminal.queued_input().len();
    let bytes = terminal.drain_input();
    let delivered_bytes = bytes.len();
    if !bytes.is_empty() {
        sink.record_encoded_input(&bytes);
    }
    let route = InputRoute {
        outcome,
        queue_depth,
        delivered_bytes,
    };
    sink.observe_input_route(route);
    route
}

/// Routes pointer input while enforcing the core's selection-versus-terminal
/// mouse policy.
pub fn route_mouse_input(
    terminal: &mut Terminal,
    event: MouseEvent,
    selection: &mut Selection,
    sink: &mut impl EncodedInputSink,
) -> InputRoute {
    let position = CellPosition {
        column: event.column,
        row: event.row,
    };
    let route = route_input(terminal, InputEvent::Mouse(event), sink);
    match route.outcome {
        InputEventOutcome::SelectionAllowed => {
            let position =
                normalize_selection_position(TerminalSnapshot::from_terminal(terminal), position);
            match (event.kind, position) {
                (MouseEventKind::Press(MouseButton::Left), Some(position)) => {
                    selection.begin(position);
                }
                (MouseEventKind::Move { .. }, Some(position)) => selection.extend(position),
                (MouseEventKind::Release(MouseButton::Left), Some(position)) => {
                    selection.extend(position);
                    selection.finish();
                }
                (MouseEventKind::Release(MouseButton::Left), None) => selection.finish(),
                _ => {}
            }
        }
        InputEventOutcome::SelectionClaimed | InputEventOutcome::Encoded { .. } => {
            selection.clear();
        }
        InputEventOutcome::QueueOverflow | InputEventOutcome::Rejected => {}
    }
    route
}

/// Tracks whether the terminal previously owned keyboard input, independent of
/// egui's response state for the current frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct KeyboardOwnership {
    pub(crate) terminal_owned: bool,
    /// Set when the OS window loses focus while this view held terminal
    /// keyboard ownership, so a later regain of OS window focus knows this
    /// was the view the user was last typing into and should reclaim
    /// egui's own widget-level keyboard focus, not just resume in-band PTY
    /// focus reporting. Cleared once consumed.
    reclaim_focus_on_window_refocus: bool,
}

impl KeyboardOwnership {
    pub(crate) fn focus_in_if_needed(
        &mut self,
        terminal_has_keyboard_focus: bool,
    ) -> Option<FocusEvent> {
        if terminal_has_keyboard_focus && !self.terminal_owned {
            self.terminal_owned = true;
            Some(FocusEvent::In)
        } else {
            None
        }
    }

    pub(crate) fn focus_out_if_owned(&mut self) -> Option<FocusEvent> {
        if self.terminal_owned {
            self.terminal_owned = false;
            Some(FocusEvent::Out)
        } else {
            None
        }
    }

    /// Records, when the OS window is losing focus, whether this view
    /// currently owns terminal keyboard input - so it can reclaim egui's
    /// widget-level focus if the window regains focus later.
    pub(crate) fn note_window_losing_focus(&mut self) {
        if self.terminal_owned {
            self.reclaim_focus_on_window_refocus = true;
        }
    }

    /// Consumes the pending reclaim flag, if set, returning whether this
    /// view should re-request egui keyboard focus now that the OS window
    /// has regained focus.
    pub(crate) fn take_reclaim_focus_on_window_refocus(&mut self) -> bool {
        std::mem::take(&mut self.reclaim_focus_on_window_refocus)
    }
}

/// Pointer state maintained in event order across frames.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct TerminalPointerState {
    pub(crate) pressed: [bool; 3],
    pub(crate) captured: [bool; 3],
    pub(crate) last_position: Option<Pos2>,
    pub(crate) modifiers: Modifiers,
}

impl TerminalPointerState {
    fn button_index(button: MouseButton) -> usize {
        match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        }
    }

    pub(crate) fn press(
        &mut self,
        button: MouseButton,
        position: Pos2,
        capture: bool,
        modifiers: Modifiers,
    ) {
        let index = Self::button_index(button);
        self.pressed[index] = true;
        self.captured[index] = capture;
        self.last_position = Some(position);
        self.modifiers = modifiers;
    }

    pub(crate) fn release(&mut self, button: MouseButton, position: Pos2, modifiers: Modifiers) {
        let index = Self::button_index(button);
        self.pressed[index] = false;
        self.captured[index] = false;
        self.last_position = Some(position);
        self.modifiers = modifiers;
    }

    pub(crate) fn moved(&mut self, position: Pos2) {
        self.last_position = Some(position);
    }

    pub(crate) fn captured(&self) -> bool {
        self.captured
            .iter()
            .zip(self.pressed)
            .any(|(captured, pressed)| *captured && pressed)
    }

    pub(crate) fn button_captured(&self, button: MouseButton) -> bool {
        let index = Self::button_index(button);
        self.pressed[index] && self.captured[index]
    }

    pub(crate) fn held_button(&self) -> Option<MouseButton> {
        [MouseButton::Left, MouseButton::Middle, MouseButton::Right]
            .into_iter()
            .find(|button| self.pressed[Self::button_index(*button)])
    }
}

pub(crate) struct InputAdapterState<'a> {
    pub(crate) selection: &'a mut Selection,
    pub(crate) keyboard: &'a mut KeyboardOwnership,
    pub(crate) pointer: &'a mut TerminalPointerState,
}

/// Which parts of terminal input `route_egui_events` should suppress this
/// frame.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct InputSuppression {
    /// A full blackout (e.g. an open context menu or a foreground modal):
    /// history navigation, selection, and keyboard routing are all inert.
    pub(crate) blackout: bool,
    /// The session backing this view can no longer accept keystrokes (it
    /// has exited, failed, stopped, or disconnected). Unlike `blackout`,
    /// this only drops keystrokes/paste bound for the shell; selection,
    /// Copy, and scrollback navigation remain active.
    pub(crate) keystrokes: bool,
}

pub(crate) fn route_egui_events(
    ui: &Ui,
    response: &Response,
    layout: GridLayout,
    terminal: &mut Terminal,
    input: InputAdapterState<'_>,
    sink: &mut impl EncodedInputSink,
    suppress: InputSuppression,
) -> InputRoutingReports {
    let InputAdapterState {
        selection,
        keyboard,
        pointer,
    } = input;
    let keyboard_focused = response.has_focus() || response.clicked();
    let events = ui.input(|input| input.events.clone());
    let mut reports = InputRoutingReports::default();
    let mut focus_out_routed = false;
    let mut terminal_key_routed = false;

    for event in events {
        if suppress.blackout && !matches!(event, egui::Event::WindowFocused(_)) {
            continue;
        }
        // A dead/read-only session (exited, failed, stopped, disconnected)
        // still allows history navigation, selection, and Copy; only
        // keystrokes/paste bound for the (no longer listening) shell are
        // dropped here.
        if suppress.keystrokes
            && matches!(
                event,
                egui::Event::Paste(_) | egui::Event::Text(_) | egui::Event::Key { .. }
            )
        {
            continue;
        }
        match event {
            egui::Event::Copy if keyboard_focused => {
                if let Some(text) =
                    selection_text(TerminalSnapshot::from_terminal(terminal), selection)
                {
                    ui.ctx().copy_text(text);
                }
            }
            egui::Event::Paste(text) if keyboard_focused => {
                record_terminal_input(
                    &mut reports,
                    selection,
                    Instant::now(),
                    route_input(terminal, InputEvent::Paste(text), sink),
                );
            }
            egui::Event::Text(text) if keyboard_focused => {
                for character in text.chars() {
                    record_terminal_input(
                        &mut reports,
                        selection,
                        Instant::now(),
                        route_input(terminal, InputEvent::Key(Key::Character(character)), sink),
                    );
                }
            }
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } if keyboard_focused => {
                let translated = control_key(key, modifiers).or_else(|| translate_key(key));
                if let Some(key) = translated {
                    terminal_key_routed = true;
                    record_terminal_input(
                        &mut reports,
                        selection,
                        Instant::now(),
                        route_input(terminal, InputEvent::Key(key), sink),
                    );
                }
            }
            egui::Event::WindowFocused(focused) => {
                if !focused {
                    // Remember whether this view owned terminal keyboard
                    // input at the moment the OS window lost focus, so a
                    // later regain of window focus can reclaim egui's own
                    // widget focus here rather than leaving it stranded
                    // until the user clicks the terminal again.
                    keyboard.note_window_losing_focus();
                } else if keyboard.take_reclaim_focus_on_window_refocus() {
                    response.request_focus();
                }
                let focus = if focused {
                    keyboard.focus_in_if_needed(keyboard_focused)
                } else {
                    keyboard.focus_out_if_owned()
                };
                focus_out_routed |= !focused && focus.is_some();
                if let Some(focus) = focus {
                    reports.record(
                        Instant::now(),
                        route_input(terminal, InputEvent::Focus(focus), sink),
                    );
                }
            }
            egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers,
            } => {
                if let Some(button) = translate_pointer_button(button) {
                    let observed = Instant::now();
                    if let Some(route) = route_pointer_event(
                        PointerInputEvent::Button {
                            position: pos,
                            button,
                            pressed,
                            modifiers: translate_modifiers(modifiers),
                        },
                        layout,
                        terminal,
                        selection,
                        pointer,
                        sink,
                    ) {
                        reports.record(observed, route);
                    }
                }
            }
            egui::Event::PointerMoved(position) => {
                let observed = Instant::now();
                if let Some(route) = route_pointer_event(
                    PointerInputEvent::Moved { position },
                    layout,
                    terminal,
                    selection,
                    pointer,
                    sink,
                ) {
                    reports.record(observed, route);
                }
            }
            egui::Event::MouseWheel {
                delta, modifiers, ..
            } => {
                let observed = Instant::now();
                if let Some(route) = route_pointer_event(
                    PointerInputEvent::Wheel {
                        delta_y: delta.y,
                        modifiers: translate_modifiers(modifiers),
                    },
                    layout,
                    terminal,
                    selection,
                    pointer,
                    sink,
                ) {
                    reports.record(observed, route);
                }
            }
            egui::Event::PointerGone => pointer.last_position = None,
            _ => {}
        }
    }

    if terminal_key_routed {
        // egui uses Tab and arrows for widget navigation. A terminal-owned
        // key must retain the grid's keyboard ownership after routing.
        response.request_focus();
    }

    if response.lost_focus() && !focus_out_routed && !terminal_key_routed {
        if let Some(focus) = keyboard.focus_out_if_owned() {
            reports.record(
                Instant::now(),
                route_input(terminal, InputEvent::Focus(focus), sink),
            );
        }
    } else if keyboard_focused {
        if let Some(focus) = keyboard.focus_in_if_needed(true) {
            reports.record(
                Instant::now(),
                route_input(terminal, InputEvent::Focus(focus), sink),
            );
        }
    }

    reports
}

pub(crate) fn record_terminal_input(
    reports: &mut InputRoutingReports,
    selection: &mut Selection,
    observed: Instant,
    route: InputRoute,
) {
    if matches!(route.outcome, InputEventOutcome::Encoded { .. }) {
        selection.clear();
    }
    reports.record(observed, route);
}

#[derive(Default)]
pub(crate) struct InputRoutingReports {
    pub(crate) routes: Vec<InputRoute>,
    pub(crate) input_observed: Option<Instant>,
}

impl InputRoutingReports {
    pub(crate) fn record(&mut self, observed: Instant, route: InputRoute) {
        self.input_observed.get_or_insert(observed);
        self.routes.push(route);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum PointerInputEvent {
    Button {
        position: Pos2,
        button: MouseButton,
        pressed: bool,
        modifiers: Modifiers,
    },
    Moved {
        position: Pos2,
    },
    Wheel {
        delta_y: f32,
        modifiers: Modifiers,
    },
}

/// Routes ordered pointer events with event-time button/capture state.
///
/// A press beginning in the grid captures its button until release. Captured
/// movement and release outside the grid clamp to its nearest visible cell;
/// non-captured pointer input outside the grid is ignored.
pub(crate) fn route_pointer_event(
    event: PointerInputEvent,
    layout: GridLayout,
    terminal: &mut Terminal,
    selection: &mut Selection,
    pointer: &mut TerminalPointerState,
    sink: &mut impl EncodedInputSink,
) -> Option<InputRoute> {
    match event {
        PointerInputEvent::Button {
            position,
            button,
            pressed: true,
            modifiers,
        } => {
            let cell = layout.cell_geometry().hit_test(position);
            pointer.press(button, position, cell.is_some(), modifiers);
            cell.map(|position| {
                route_mouse_input(
                    terminal,
                    MouseEvent {
                        kind: MouseEventKind::Press(button),
                        column: position.column,
                        row: position.row,
                        modifiers,
                    },
                    selection,
                    sink,
                )
            })
        }
        PointerInputEvent::Button {
            position,
            button,
            pressed: false,
            modifiers,
        } => {
            let cell = layout.cell_geometry().hit_test(position).or_else(|| {
                (pointer.button_captured(button) || selection.is_active()).then(|| {
                    clamped_cell_from_point(
                        layout.rect.min,
                        layout.dimensions,
                        layout.metrics,
                        position,
                    )
                })?
            });
            let route = cell.map(|position| {
                route_mouse_input(
                    terminal,
                    MouseEvent {
                        kind: MouseEventKind::Release(button),
                        column: position.column,
                        row: position.row,
                        modifiers,
                    },
                    selection,
                    sink,
                )
            });
            pointer.release(button, position, modifiers);
            route
        }
        PointerInputEvent::Moved { position } => {
            pointer.moved(position);
            layout
                .cell_geometry()
                .hit_test(position)
                .or_else(|| {
                    (pointer.captured() || selection.is_active()).then(|| {
                        clamped_cell_from_point(
                            layout.rect.min,
                            layout.dimensions,
                            layout.metrics,
                            position,
                        )
                    })?
                })
                .map(|position| {
                    route_mouse_input(
                        terminal,
                        MouseEvent {
                            kind: MouseEventKind::Move {
                                button: pointer.held_button(),
                            },
                            column: position.column,
                            row: position.row,
                            modifiers: pointer.modifiers,
                        },
                        selection,
                        sink,
                    )
                })
        }
        PointerInputEvent::Wheel { delta_y, modifiers } if delta_y != 0.0 => {
            pointer.last_position.and_then(|position| {
                layout.cell_geometry().hit_test(position).map(|position| {
                    route_mouse_input(
                        terminal,
                        MouseEvent {
                            kind: MouseEventKind::Wheel(if delta_y > 0.0 {
                                MouseWheel::Up
                            } else {
                                MouseWheel::Down
                            }),
                            column: position.column,
                            row: position.row,
                            modifiers,
                        },
                        selection,
                        sink,
                    )
                })
            })
        }
        PointerInputEvent::Wheel { .. } => None,
    }
}

fn translate_key(key: egui::Key) -> Option<Key> {
    match key {
        egui::Key::Enter => Some(Key::Enter),
        egui::Key::Tab => Some(Key::Tab),
        egui::Key::Backspace => Some(Key::Backspace),
        egui::Key::Escape => Some(Key::Escape),
        egui::Key::ArrowUp => Some(Key::ArrowUp),
        egui::Key::ArrowDown => Some(Key::ArrowDown),
        egui::Key::ArrowLeft => Some(Key::ArrowLeft),
        egui::Key::ArrowRight => Some(Key::ArrowRight),
        _ => None,
    }
}

/// Maps a Ctrl-held key press to the terminal's C0 control-byte encoding
/// (see `festerm_core::input::control_byte`), so a chord like Ctrl+B reaches
/// the running program instead of being silently dropped: unlike ordinary
/// typing, held Ctrl suppresses the platform's `Text` event, so this is the
/// only path such a chord can reach the terminal through.
///
/// This only recognizes plain Ctrl — never the platform Command modifier
/// (`mac_cmd`) — so it never intercepts a macOS `Cmd+<letter>` application
/// shortcut (new tab, close tab, and so on), which egui reports with `ctrl`
/// left `false` and `mac_cmd`/`command` set instead.
fn control_key(key: egui::Key, modifiers: egui::Modifiers) -> Option<Key> {
    if !modifiers.ctrl || modifiers.mac_cmd {
        return None;
    }
    let character = control_key_character(key)?;
    Some(Key::Control(character))
}

/// The base character of a Ctrl chord, for the keys that have an
/// established Ctrl mapping (see `festerm_core::input::control_byte`).
fn control_key_character(key: egui::Key) -> Option<char> {
    match key {
        egui::Key::A => Some('a'),
        egui::Key::B => Some('b'),
        egui::Key::C => Some('c'),
        egui::Key::D => Some('d'),
        egui::Key::E => Some('e'),
        egui::Key::F => Some('f'),
        egui::Key::G => Some('g'),
        egui::Key::H => Some('h'),
        egui::Key::I => Some('i'),
        egui::Key::J => Some('j'),
        egui::Key::K => Some('k'),
        egui::Key::L => Some('l'),
        egui::Key::M => Some('m'),
        egui::Key::N => Some('n'),
        egui::Key::O => Some('o'),
        egui::Key::P => Some('p'),
        egui::Key::Q => Some('q'),
        egui::Key::R => Some('r'),
        egui::Key::S => Some('s'),
        egui::Key::T => Some('t'),
        egui::Key::U => Some('u'),
        egui::Key::V => Some('v'),
        egui::Key::W => Some('w'),
        egui::Key::X => Some('x'),
        egui::Key::Y => Some('y'),
        egui::Key::Z => Some('z'),
        egui::Key::Space => Some(' '),
        egui::Key::OpenBracket => Some('['),
        egui::Key::Backslash => Some('\\'),
        egui::Key::CloseBracket => Some(']'),
        _ => None,
    }
}

fn translate_pointer_button(button: egui::PointerButton) -> Option<MouseButton> {
    match button {
        egui::PointerButton::Primary => Some(MouseButton::Left),
        egui::PointerButton::Middle => Some(MouseButton::Middle),
        egui::PointerButton::Secondary => Some(MouseButton::Right),
        egui::PointerButton::Extra1 | egui::PointerButton::Extra2 => None,
    }
}

fn translate_modifiers(modifiers: egui::Modifiers) -> Modifiers {
    let mut translated = Modifiers::NONE;
    if modifiers.shift {
        translated = translated.with(Modifiers::SHIFT);
    }
    if modifiers.alt {
        translated = translated.with(Modifiers::ALT);
    }
    if modifiers.ctrl || modifiers.mac_cmd {
        translated = translated.with(Modifiers::CONTROL);
    }
    translated
}
