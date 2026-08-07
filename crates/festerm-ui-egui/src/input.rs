use std::time::Instant;

use egui::{Pos2, Response, Ui};
use festerm_core::{
    Dimensions, FocusEvent, InputEvent, InputEventOutcome, Key, Modifiers, MouseButton, MouseEvent,
    MouseEventKind, MouseWheel, Terminal,
};

use crate::{
    geometry::{cell_from_point, clamped_cell_from_point, CellPosition},
    renderer::GridLayout,
    selection::{normalize_selection_position, selection_text, Selection},
    TerminalSnapshot,
};

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
    /// access to the terminal core.
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

pub(crate) fn route_egui_events(
    ui: &Ui,
    response: &Response,
    layout: GridLayout,
    terminal: &mut Terminal,
    input: InputAdapterState<'_>,
    sink: &mut impl EncodedInputSink,
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
                key, pressed: true, ..
            } if keyboard_focused => {
                if let Some(key) = translate_key(key) {
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
            let cell =
                cell_from_point(layout.rect.min, layout.dimensions, layout.metrics, position);
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
            let cell =
                cell_from_point(layout.rect.min, layout.dimensions, layout.metrics, position)
                    .or_else(|| {
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
            cell_from_point(layout.rect.min, layout.dimensions, layout.metrics, position)
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
                cell_from_point(layout.rect.min, layout.dimensions, layout.metrics, position).map(
                    |position| {
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
                    },
                )
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
