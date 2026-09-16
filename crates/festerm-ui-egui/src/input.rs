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
    /// Distinguishes ordered keyboard/paste input from focus/mouse reports;
    /// no payload is exposed to this optional classification hook.
    fn begin_input_event(&mut self, _keyboard_input: bool) {}
    /// Delivers transient encoded bytes. Implementations must not retain them
    /// unless they are an active session transport.
    fn record_encoded_input(&mut self, bytes: &[u8]);

    /// Observes content-free routing metadata after every routed core event.
    fn observe_input_route(&mut self, _route: InputRoute) {}
    fn observe_input_event(
        &mut self,
        _metadata: crate::routing_trace::Metadata,
        route: InputRoute,
    ) {
        self.observe_input_route(route);
    }
    fn observe_local_selection(&mut self, _decision: &'static str) {}
    fn observe_local_gesture(
        &mut self,
        _class: &'static str,
        _decision: &'static str,
        _modifiers: u8,
    ) {
    }

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
    let metadata = crate::routing_trace::Metadata::from_input(&event);
    route_input_with_metadata(terminal, event, sink, metadata)
}

fn route_input_with_metadata(
    terminal: &mut Terminal,
    event: InputEvent,
    sink: &mut impl EncodedInputSink,
    metadata: crate::routing_trace::Metadata,
) -> InputRoute {
    sink.begin_input_event(matches!(&event, InputEvent::Key(_) | InputEvent::Paste(_)));
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
    sink.observe_input_event(metadata, route);
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
    route_mouse_input_in_viewport(terminal, event, selection, sink, 0)
}

fn route_mouse_input_in_viewport(
    terminal: &mut Terminal,
    event: MouseEvent,
    selection: &mut Selection,
    sink: &mut impl EncodedInputSink,
    viewport_offset_rows: usize,
) -> InputRoute {
    let position = CellPosition {
        column: event.column,
        row: event.row,
    };
    let route = route_input(terminal, InputEvent::Mouse(event), sink);
    match route.outcome {
        InputEventOutcome::SelectionAllowed => {
            let snapshot = TerminalSnapshot::from_terminal_viewport(terminal, viewport_offset_rows);
            let position = normalize_selection_position(snapshot, position).and_then(|position| {
                snapshot
                    .content_position(position)
                    .map(|content| (position, content))
            });
            match (event.kind, position) {
                (MouseEventKind::Press(MouseButton::Left), Some((position, content))) => {
                    selection.begin_at(position, content);
                    sink.observe_local_selection("began");
                }
                (MouseEventKind::Move { .. }, Some((position, content))) => {
                    selection.extend_at(position, content);
                    if selection.is_active() {
                        sink.observe_local_selection("extended");
                    }
                }
                (MouseEventKind::Release(MouseButton::Left), Some((position, content))) => {
                    selection.extend_at(position, content);
                    selection.finish();
                    sink.observe_local_selection("finished");
                }
                (MouseEventKind::Release(MouseButton::Left), None) => {
                    selection.finish();
                    sink.observe_local_selection("finished-outside-grid");
                }
                _ => {}
            }
        }
        InputEventOutcome::SelectionClaimed | InputEventOutcome::Encoded { .. } => {
            selection.clear();
            sink.observe_local_selection("cleared-terminal-owns-mouse");
        }
        InputEventOutcome::QueueOverflow | InputEventOutcome::Rejected => {}
    }
    route
}

/// Tracks whether the terminal previously owned keyboard input, independent of
/// egui's response state for the current frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct KeyboardOwnership {
    ime_composing: bool,
    pub(crate) terminal_owned: bool,
    /// While `Some`, every frame up to this deadline re-asserts egui
    /// keyboard focus on the terminal response rather than requesting it
    /// exactly once.
    ///
    /// A single `request_focus()` call on the frame the OS window regains
    /// focus is not reliable, for two independent reasons: winit's own
    /// `WindowEvent::Focused` notifications on macOS can arrive as a rapid,
    /// spurious back-and-forth around a real focus change (this app's own
    /// live testing observed several `false`/`true` pairs land within a
    /// single frame for one real click), so gating a reclaim on "did this
    /// view own terminal focus right before the *last* loss" is fragile -
    /// which loss in that flurry counts is unpredictable. And even a
    /// reclaim that *does* land can be silently discarded moments later:
    /// egui clears any widget's focus if that widget isn't part of the UI
    /// tree for even one single frame after gaining it (its own "dead man's
    /// switch", see `egui::memory::Focus::end_pass`), and a transient frame,
    /// a host-key/password prompt racing with session state, a one-shot
    /// overlay, or any other momentary UI branch that skips building the
    /// terminal widget, can trip that switch shortly after a real click
    /// reclaims focus, before the user starts typing. Unconditionally
    /// arming a short re-assert window on every regained-focus event, and
    /// re-requesting focus every frame within it until it actually sticks,
    /// survives both failure modes.
    reclaim_until: Option<Instant>,
}

/// How long after regaining OS window focus to keep re-asserting egui
/// keyboard focus on the terminal, in case a transient frame drops it via
/// egui's own dead-man's-switch before the user starts typing again.
pub(crate) const RECLAIM_FOCUS_WINDOW: Duration = Duration::from_millis(750);

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
        self.ime_composing = false;
        if self.terminal_owned {
            self.terminal_owned = false;
            Some(FocusEvent::Out)
        } else {
            None
        }
    }

    /// Arms a short window (`RECLAIM_FOCUS_WINDOW`) during which
    /// [`Self::reclaim_focus_due`] keeps re-asserting egui keyboard focus,
    /// unconditionally, whenever the OS window reports regaining focus.
    /// Callers should still skip acting on the result while a modal/overlay
    /// legitimately owns input focus instead.
    pub(crate) fn begin_reclaim_on_window_refocus(&mut self, now: Instant) {
        self.reclaim_until = Some(now + RECLAIM_FOCUS_WINDOW);
    }

    /// Returns whether the terminal should re-assert egui keyboard focus
    /// this frame, clearing the reclaim window once it either succeeds
    /// (`has_focus` is true) or expires.
    pub(crate) fn reclaim_focus_due(&mut self, now: Instant, has_focus: bool) -> bool {
        let Some(deadline) = self.reclaim_until else {
            return false;
        };
        if has_focus || now >= deadline {
            self.reclaim_until = None;
            return false;
        }
        true
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
    pub(crate) viewport_offset_rows: usize,
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
        viewport_offset_rows,
    } = input;
    // Whether the terminal should currently accept keyboard input.
    //
    // This intentionally does *not* depend on egui's own internal
    // widget-level focus tracking (`response.has_focus()`/`clicked()`).
    // There is only ever one terminal view rendered per frame (the active
    // tab), and every legitimate reason a keystroke should instead go
    // elsewhere - an open context menu, command palette, in-terminal
    // search, or a foreground modal/overlay - already gets folded into
    // `suppress.blackout` at the call site (`TerminalViewOptions
    // ::terminal_input_enabled` in `view.rs`/`app.rs`). Gating routing on
    // egui's own focus state *in addition* to that was a real, reproduced
    // bug: winit's macOS `WindowEvent::Focused` notifications can fire a
    // rapid, spurious sequence of false/true pairs around one real click,
    // and even a reclaimed focus can be silently dropped moments later by
    // egui's own "dead man's switch" (`egui::memory::Focus::end_pass`)
    // if the terminal widget is skipped from the UI tree for even one
    // frame - both of which left the terminal never regaining widget
    // focus after certain OS-level refocus clicks even though real
    // keystrokes were arriving. Since blackout already fully captures
    // whether something else should own input, keyboard routing no longer
    // needs a second, less reliable focus signal on top of it.
    let keyboard_focused = !suppress.blackout;
    let events = ui.input(|input| input.events.clone());
    let mut reports = InputRoutingReports::default();
    let mut focus_out_routed = false;
    let mut terminal_key_routed = false;
    if suppress.blackout {
        if selection.is_active() {
            selection.finish();
            sink.observe_local_gesture("selection", "finished-foreground-ownership", 0);
        }
        *pointer = TerminalPointerState::default();
    }

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
                egui::Event::Paste(_)
                    | egui::Event::Text(_)
                    | egui::Event::Key { .. }
                    | egui::Event::Ime(_)
            )
        {
            continue;
        }
        match event {
            egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                keyboard.ime_composing = !text.is_empty();
            }
            egui::Event::Copy if keyboard_focused => {
                match selection_text(
                    TerminalSnapshot::from_terminal_viewport(terminal, viewport_offset_rows),
                    selection,
                ) {
                    Some(text) => {
                        ui.ctx().copy_text(text);
                        selection.clear();
                        crate::routing_trace::record_local(
                            ui.ctx(),
                            "copy-request",
                            "selection-copied-and-cleared",
                            0,
                        );
                    }
                    None => crate::routing_trace::record_local(
                        ui.ctx(),
                        "copy-request",
                        "no-selection-no-input",
                        0,
                    ),
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
            egui::Event::Text(text) | egui::Event::Ime(egui::ImeEvent::Commit(text))
                if keyboard_focused =>
            {
                keyboard.ime_composing = false;
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
            } if keyboard_focused && !keyboard.ime_composing => {
                let translated = control_key(key, modifiers).or_else(|| translate_key(key));
                if let Some(key) = translated {
                    terminal_key_routed = true;
                    let event = InputEvent::Key(key);
                    let mut metadata = crate::routing_trace::Metadata::from_input(&event);
                    metadata.modifiers = u8::from(modifiers.shift)
                        | (u8::from(modifiers.alt) << 1)
                        | (u8::from(modifiers.ctrl) << 2)
                        | (u8::from(modifiers.mac_cmd) << 3);
                    record_terminal_input(
                        &mut reports,
                        selection,
                        Instant::now(),
                        route_input_with_metadata(terminal, event, sink, metadata),
                    );
                }
            }
            egui::Event::WindowFocused(focused) => {
                if focused {
                    // The OS window regaining focus doesn't guarantee egui's
                    // own widget-level keyboard focus survived intact - see
                    // `KeyboardOwnership::reclaim_until`'s doc comment - so
                    // unconditionally arm a short window to keep
                    // re-asserting it below, after the event loop.
                    keyboard.begin_reclaim_on_window_refocus(Instant::now());
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
                        viewport_offset_rows,
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
                    viewport_offset_rows,
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
                    viewport_offset_rows,
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
    } else if !suppress.blackout && keyboard.reclaim_focus_due(Instant::now(), response.has_focus())
    {
        // See `KeyboardOwnership::reclaim_until`'s doc comment: keep
        // re-asserting focus every frame for a short window after
        // regaining OS window focus, rather than requesting it exactly
        // once, so a transient frame that drops the terminal widget from
        // egui's own focus bookkeeping gets repaired on the very next
        // frame instead of leaving the terminal stranded until the user
        // clicks it directly. Skipped during a full blackout (an open
        // context menu or foreground modal) so a legitimately focused
        // overlay widget isn't clobbered.
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
    viewport_offset_rows: usize,
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
                route_mouse_input_in_viewport(
                    terminal,
                    MouseEvent {
                        kind: MouseEventKind::Press(button),
                        column: position.column,
                        row: position.row,
                        modifiers,
                    },
                    selection,
                    sink,
                    viewport_offset_rows,
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
                route_mouse_input_in_viewport(
                    terminal,
                    MouseEvent {
                        kind: MouseEventKind::Release(button),
                        column: position.column,
                        row: position.row,
                        modifiers,
                    },
                    selection,
                    sink,
                    viewport_offset_rows,
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
                    route_mouse_input_in_viewport(
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
                        viewport_offset_rows,
                    )
                })
        }
        PointerInputEvent::Wheel { delta_y, modifiers } if delta_y != 0.0 => {
            pointer.last_position.and_then(|position| {
                layout.cell_geometry().hit_test(position).map(|position| {
                    route_mouse_input_in_viewport(
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
                        viewport_offset_rows,
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
    if !modifiers.ctrl || modifiers.mac_cmd || modifiers.alt {
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

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Rect, Vec2};
    use egui_kittest::{kittest::Queryable, Harness};
    use festerm_core::{
        Dimensions, FocusEvent, InputEvent, InputEventOutcome, Key, Modifiers, MouseButton,
        MouseEvent, MouseEventKind, Terminal,
    };

    use crate::{
        geometry::{CellMetrics, CellPosition, CellRange},
        renderer::GridLayout,
        selection::Selection,
        TerminalView,
    };

    #[derive(Default)]
    struct Sink(Vec<Vec<u8>>);

    impl EncodedInputSink for Sink {
        fn record_encoded_input(&mut self, bytes: &[u8]) {
            self.0.push(bytes.to_vec());
        }
    }

    fn terminal(columns: usize, rows: usize) -> Terminal {
        Terminal::new(Dimensions::new(columns, rows).expect("valid test size"))
            .expect("test terminal allocation")
    }

    fn grid_layout(columns: usize, rows: usize) -> GridLayout {
        GridLayout {
            rect: Rect::from_min_size(
                Pos2::new(5.0, 7.0),
                Vec2::new(columns as f32 * 10.0, rows as f32 * 20.0),
            ),
            dimensions: Dimensions::new(columns, rows).expect("valid test size"),
            metrics: CellMetrics::new(10.0, 20.0).expect("valid test cell metrics"),
        }
    }

    struct HeadlessViewState {
        view: TerminalView,
        terminal: Terminal,
        sink: Sink,
    }

    impl HeadlessViewState {
        fn new() -> Self {
            Self {
                view: TerminalView::default(),
                terminal: terminal(80, 24),
                sink: Sink::default(),
            }
        }
    }

    #[test]
    fn ctrl_letter_chords_reach_the_terminal_as_control_bytes() {
        let mut harness = Harness::builder()
            .with_size(Vec2::new(800.0, 600.0))
            .build_ui_state(
                |ui, state: &mut HeadlessViewState| {
                    state.view.show(ui, &mut state.terminal, &mut state.sink);
                },
                HeadlessViewState::new(),
            );
        let grid = harness
            .state()
            .view
            .diagnostics()
            .grid_rect
            .expect("headless frame records grid geometry");
        harness.event(egui::Event::PointerButton {
            pos: grid.center(),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.event(egui::Event::PointerButton {
            pos: grid.center(),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();

        // Ctrl+B: the tmux/GNU Screen default prefix key, and the
        // motivating regression this test guards against (previously
        // swallowed instead of reaching the running program).
        harness.event(egui::Event::Key {
            key: egui::Key::B,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
        });
        harness.run();
        assert_eq!(harness.state().sink.0, vec![vec![0x02]]);

        // A macOS Cmd+<letter> application shortcut sets `mac_cmd`/`command`
        // rather than `ctrl`, and must never be reinterpreted as a Ctrl
        // chord and sent to the terminal.
        harness.event(egui::Event::Key {
            key: egui::Key::N,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                mac_cmd: true,
                command: true,
                ..Default::default()
            },
        });
        harness.run();
        assert_eq!(harness.state().sink.0, vec![vec![0x02]]);
    }

    #[test]
    fn copy_with_no_selection_never_synthesizes_a_terminal_interrupt() {
        let mut harness = Harness::builder()
            .with_size(Vec2::new(800.0, 600.0))
            .build_ui_state(
                |ui, state: &mut HeadlessViewState| {
                    state.view.show(ui, &mut state.terminal, &mut state.sink);
                },
                HeadlessViewState::new(),
            );
        harness.run();
        harness.get_by_label("Terminal viewport").click();
        harness.run();

        assert!(harness.state().view.selection().range().is_none());
        harness.event(egui::Event::Copy);
        harness.run();
        assert!(harness.state().sink.0.is_empty());
    }

    #[test]
    fn copy_with_an_active_selection_copies_it_and_then_deselects() {
        let mut state = HeadlessViewState::new();
        state.terminal.ingest(b"copy me");
        let mut harness = Harness::builder()
            .with_size(Vec2::new(800.0, 600.0))
            .build_ui_state(
                |ui, state: &mut HeadlessViewState| {
                    state.view.show(ui, &mut state.terminal, &mut state.sink);
                },
                state,
            );
        harness.run();
        harness
            .state_mut()
            .view
            .selection
            .begin(CellPosition { column: 0, row: 0 });
        harness
            .state_mut()
            .view
            .selection
            .extend(CellPosition { column: 6, row: 0 });
        harness.state_mut().view.selection.finish();
        assert!(harness.state().view.selection().range().is_some());

        harness.event(egui::Event::Copy);
        harness.run();

        // A real terminal interrupt must not also be sent for a Copy that
        // had a selection to act on.
        assert!(harness.state().sink.0.is_empty());
        assert!(harness.state().view.selection().range().is_none());
    }

    #[test]
    fn terminal_view_claims_keyboard_focus_on_its_first_frame_without_a_click() {
        // A freshly started session should be immediately typeable: the
        // user shouldn't have to click into the terminal just to start
        // sending keystrokes to it.
        let mut harness = Harness::builder()
            .with_size(Vec2::new(800.0, 600.0))
            .build_ui_state(
                |ui, state: &mut HeadlessViewState| {
                    state.view.show(ui, &mut state.terminal, &mut state.sink);
                },
                HeadlessViewState::new(),
            );
        harness.run();

        harness.event(egui::Event::Text("Q".to_owned()));
        harness.run();

        assert_eq!(
            harness.state().sink.0,
            vec![b"Q".to_vec()],
            "typed input should reach the terminal without ever clicking into it first"
        );
    }

    #[test]
    fn application_mouse_claim_prevents_local_selection() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"\x1b[?1000h");
        let mut selection = Selection::default();
        let mut sink = Sink::default();
        let route = route_mouse_input(
            &mut terminal,
            MouseEvent {
                kind: MouseEventKind::Press(MouseButton::Left),
                column: 1,
                row: 0,
                modifiers: Modifiers::NONE,
            },
            &mut selection,
            &mut sink,
        );

        assert_eq!(route.outcome, InputEventOutcome::Encoded { bytes: 6 });
        assert_eq!(selection.range(), None);
        assert_eq!(sink.0, vec![b"\x1b[M \"!".to_vec()]);
    }

    #[test]
    fn focus_out_routes_once_after_prior_terminal_keyboard_ownership() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"\x1b[?1004h");
        let mut keyboard = KeyboardOwnership::default();
        keyboard.focus_in_if_needed(true);
        let mut sink = Sink::default();

        let first = keyboard
            .focus_out_if_owned()
            .map(|focus| route_input(&mut terminal, InputEvent::Focus(focus), &mut sink));
        let second = keyboard
            .focus_out_if_owned()
            .map(|focus| route_input(&mut terminal, InputEvent::Focus(focus), &mut sink));

        assert_eq!(
            first.map(|route| route.outcome),
            Some(InputEventOutcome::Encoded { bytes: 3 })
        );
        assert_eq!(second, None);
        assert_eq!(sink.0, vec![b"\x1b[O".to_vec()]);
    }

    #[test]
    fn window_refocus_arms_reclaim_unconditionally_and_it_fires_until_it_sticks_or_expires() {
        // The reclaim window is armed on every regained-focus event
        // unconditionally (see `KeyboardOwnership::reclaim_until`'s doc
        // comment for why gating it on prior terminal ownership is
        // fragile), and keeps re-asserting focus every frame - not just
        // once - until either egui reports the terminal focused again or
        // the window fully elapses.
        let mut keyboard = KeyboardOwnership::default();
        let start = Instant::now();
        keyboard.begin_reclaim_on_window_refocus(start);

        // Still within the window and focus hasn't stuck yet: keep firing.
        assert!(keyboard.reclaim_focus_due(start + Duration::from_millis(1), false));
        assert!(keyboard.reclaim_focus_due(start + Duration::from_millis(500), false));

        // Once egui reports the terminal has focus again, the reclaim
        // window closes immediately rather than continuing to fire.
        assert!(!keyboard.reclaim_focus_due(start + Duration::from_millis(501), true));
        assert!(!keyboard.reclaim_focus_due(start + Duration::from_millis(502), false));

        // A fresh reclaim window that never sticks gives up once it fully
        // elapses.
        keyboard.begin_reclaim_on_window_refocus(start);
        assert!(!keyboard.reclaim_focus_due(start + RECLAIM_FOCUS_WINDOW, false));
    }

    #[test]
    fn ordered_drag_uses_button_state_before_same_frame_release() {
        let mut terminal = terminal(4, 2);
        terminal.ingest(b"\x1b[?1002h\x1b[?1006h");
        let mut selection = Selection::default();
        let mut pointer = TerminalPointerState::default();
        let mut sink = Sink::default();
        let layout = grid_layout(4, 2);

        for event in [
            PointerInputEvent::Button {
                position: Pos2::new(6.0, 8.0),
                button: MouseButton::Left,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            PointerInputEvent::Moved {
                position: Pos2::new(26.0, 8.0),
            },
            PointerInputEvent::Button {
                position: Pos2::new(26.0, 8.0),
                button: MouseButton::Left,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ] {
            assert!(route_pointer_event(
                event,
                layout,
                &mut terminal,
                &mut selection,
                &mut pointer,
                &mut sink,
                0,
            )
            .is_some());
        }

        assert_eq!(
            sink.0,
            vec![
                b"\x1b[<0;1;1M".to_vec(),
                b"\x1b[<32;3;1M".to_vec(),
                b"\x1b[<0;3;1m".to_vec(),
            ]
        );
    }

    #[test]
    fn application_mouse_release_outside_grid_is_captured_and_clamped() {
        let mut terminal = terminal(4, 2);
        terminal.ingest(b"\x1b[?1000h\x1b[?1006h");
        let mut selection = Selection::default();
        let mut pointer = TerminalPointerState::default();
        let mut sink = Sink::default();
        let layout = grid_layout(4, 2);

        assert!(route_pointer_event(
            PointerInputEvent::Button {
                position: Pos2::new(6.0, 8.0),
                button: MouseButton::Left,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            layout,
            &mut terminal,
            &mut selection,
            &mut pointer,
            &mut sink,
            0,
        )
        .is_some());
        assert!(route_pointer_event(
            PointerInputEvent::Button {
                position: Pos2::new(-50.0, 300.0),
                button: MouseButton::Left,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
            layout,
            &mut terminal,
            &mut selection,
            &mut pointer,
            &mut sink,
            0,
        )
        .is_some());

        assert_eq!(
            sink.0,
            vec![b"\x1b[<0;1;1M".to_vec(), b"\x1b[<0;1;2m".to_vec()]
        );
        assert_eq!(selection.range(), None);
    }

    #[test]
    fn local_selection_capture_clamps_an_outside_release() {
        let mut terminal = terminal(4, 2);
        terminal.ingest(b"ABCD");
        let mut selection = Selection::default();
        let mut pointer = TerminalPointerState::default();
        let mut sink = Sink::default();
        let layout = grid_layout(4, 2);

        for event in [
            PointerInputEvent::Button {
                position: Pos2::new(16.0, 8.0),
                button: MouseButton::Left,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            PointerInputEvent::Button {
                position: Pos2::new(200.0, 300.0),
                button: MouseButton::Left,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ] {
            assert_eq!(
                route_pointer_event(
                    event,
                    layout,
                    &mut terminal,
                    &mut selection,
                    &mut pointer,
                    &mut sink,
                    0,
                )
                .map(|route| route.outcome),
                Some(InputEventOutcome::SelectionAllowed)
            );
        }

        assert!(!selection.is_active());
        assert_eq!(
            selection.range(),
            Some(CellRange::new(
                CellPosition { column: 1, row: 0 },
                CellPosition { column: 3, row: 1 },
            ))
        );
    }

    #[test]
    fn routing_uses_core_keyboard_modes_and_drains_to_sink() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"\x1b[?1h");
        let mut sink = Sink::default();

        let route = route_input(&mut terminal, InputEvent::Key(Key::ArrowUp), &mut sink);

        assert_eq!(route.outcome, InputEventOutcome::Encoded { bytes: 3 });
        assert_eq!(route.queue_depth, 3);
        assert_eq!(sink.0, vec![b"\x1bOA".to_vec()]);
        assert!(terminal.queued_input().is_empty());
    }

    #[test]
    fn accepted_typed_input_clears_local_selection() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"selected");
        let mut selection = Selection::default();
        selection.begin(CellPosition { column: 0, row: 0 });
        selection.extend(CellPosition { column: 3, row: 0 });
        selection.finish();
        let mut sink = Sink::default();
        let mut reports = InputRoutingReports::default();

        record_terminal_input(
            &mut reports,
            &mut selection,
            Instant::now(),
            route_input(
                &mut terminal,
                InputEvent::Key(Key::Character('x')),
                &mut sink,
            ),
        );

        assert_eq!(selection.range(), None);
        assert_eq!(sink.0, vec![b"x".to_vec()]);
    }

    #[test]
    fn routing_uses_core_paste_and_focus_modes() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"\x1b[?2004h\x1b[?1004h");
        let mut sink = Sink::default();

        assert_eq!(
            route_input(
                &mut terminal,
                InputEvent::Paste("paste".to_owned()),
                &mut sink
            )
            .outcome,
            InputEventOutcome::Encoded { bytes: 17 }
        );
        assert_eq!(
            route_input(&mut terminal, InputEvent::Focus(FocusEvent::In), &mut sink).outcome,
            InputEventOutcome::Encoded { bytes: 3 }
        );
        assert_eq!(
            sink.0,
            vec![b"\x1b[200~paste\x1b[201~".to_vec(), b"\x1b[I".to_vec()]
        );
    }
}
