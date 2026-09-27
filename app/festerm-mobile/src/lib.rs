//! Phase 1 iOS feasibility host. No transport, credentials, or persistence.

mod terminal_gesture;

use eframe::egui;
use festerm_core::{Dimensions, InputEvent, Key, Modifiers, Terminal};
use festerm_ui_egui::{
    route_input, EncodedInputSink, InputRoute, InputSinkDiagnostics, TerminalView,
    TerminalViewOptions,
};
use std::{cell::Cell, rc::Rc};

/// The egui mesh renderer does not need the full WebGPU limits. In particular,
/// iOS Simulator Metal exposes 15 inter-stage variables, below the default 16.
/// Keep device validation enabled and request the portable downlevel baseline,
/// retaining adapter texture dimensions for full-resolution phone/iPad surfaces.
fn mobile_device_descriptor(
    supported: eframe::wgpu::Limits,
) -> eframe::wgpu::DeviceDescriptor<'static> {
    eframe::wgpu::DeviceDescriptor {
        label: Some("fesTerm mobile renderer"),
        required_limits: eframe::wgpu::Limits::downlevel_defaults().using_resolution(supported),
        ..Default::default()
    }
}

pub fn mobile_wgpu_configuration() -> eframe::egui_wgpu::WgpuConfiguration {
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.device_descriptor =
        std::sync::Arc::new(|adapter| mobile_device_descriptor(adapter.limits()));
    eframe::egui_wgpu::WgpuConfiguration {
        wgpu_setup: setup.into(),
        ..Default::default()
    }
}

pub const SCROLLBACK_LIMIT: usize = 256 * 1024;

/// Only content-free lifecycle observations cross the event-loop/UI seam.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lifecycle {
    pub active: bool,
    pub resumes: u64,
    pub suspensions: u64,
    pub memory_warnings: u64,
}

impl Lifecycle {
    pub fn resume(&mut self) {
        if !self.active {
            self.resumes = self.resumes.saturating_add(1);
            self.active = true;
        }
    }

    pub fn suspend(&mut self) {
        if self.active {
            self.suspensions = self.suspensions.saturating_add(1);
            self.active = false;
        }
    }

    pub fn memory_warning(&mut self) {
        self.memory_warnings = self.memory_warnings.saturating_add(1);
    }
}

/// Deliberately discards bytes: the spike must not pretend to be a shell or
/// save what a tester types. Uses the renderer's existing input contract.
#[derive(Default)]
pub struct ProbeSink {
    diagnostics: InputSinkDiagnostics,
}

impl EncodedInputSink for ProbeSink {
    fn record_encoded_input(&mut self, bytes: &[u8]) {
        self.diagnostics.byte_count = self
            .diagnostics
            .byte_count
            .saturating_add(bytes.len() as u64);
    }

    fn observe_input_route(&mut self, route: InputRoute) {
        self.diagnostics.event_count = self.diagnostics.event_count.saturating_add(1);
        self.diagnostics.last_outcome = Some(route.outcome);
        self.diagnostics.last_queue_depth = route.queue_depth;
    }

    fn input_diagnostics(&self) -> Option<InputSinkDiagnostics> {
        Some(self.diagnostics)
    }
}

pub fn fixture_terminal() -> Terminal {
    let mut terminal = Terminal::with_scrollback_limit(
        Dimensions::new(80, 24).expect("fixed valid dimensions"),
        SCROLLBACK_LIMIT,
    )
    .expect("bounded fixture grid");
    terminal.ingest(b"\x1b[1;36mfesTerm iOS rendering spike\x1b[0m\r\n");
    terminal.ingest(b"Offline fixture. No SSH connection or local shell.\r\n");
    for row in 1..=80 {
        terminal.ingest(
            format!(
                "\x1b[32m{row:02}\x1b[0m  ANSI  \u{00e9}  \u{03bb}  \u{4e2d}\u{6587}  \u{1f680}\r\n"
            )
            .as_bytes(),
        );
    }
    terminal.ingest(b"Input is counted and discarded, never executed.\r\n");
    terminal
}

pub struct MobileApp {
    reported_first_ui: bool,
    terminal: Terminal,
    view: TerminalView,
    sink: ProbeSink,
    lifecycle: Rc<Cell<Lifecycle>>,
    observed_memory_warnings: u64,
    observed_suspensions: u64,
    keyboard: Option<festerm_ios_window::KeyboardBridge>,
    control: bool,
    alt: bool,
    toolbar_rect: Option<egui::Rect>,
    gestures: terminal_gesture::TerminalGesture,
}

impl MobileApp {
    pub fn new(lifecycle: Rc<Cell<Lifecycle>>) -> Self {
        Self {
            reported_first_ui: false,
            terminal: fixture_terminal(),
            view: TerminalView::default(),
            sink: ProbeSink::default(),
            lifecycle,
            observed_memory_warnings: 0,
            observed_suspensions: 0,
            keyboard: None,
            control: false,
            alt: false,
            toolbar_rect: None,
            gestures: terminal_gesture::TerminalGesture::default(),
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let occlusion = self
            .keyboard
            .as_ref()
            .map_or(0.0, |keyboard| keyboard.occluded_height_fraction());
        self.show_with_occlusion(ui, occlusion);
    }

    pub fn with_keyboard(mut self, keyboard: Option<festerm_ios_window::KeyboardBridge>) -> Self {
        self.keyboard = keyboard;
        self
    }

    fn show_with_occlusion(&mut self, ui: &mut egui::Ui, occlusion: f32) {
        // Accessory taps must not blur the terminal and restart UIKit's keyboard.
        ui.ctx().options_mut(|options| {
            options.input_options.surrender_focus_on = egui::SurrenderFocusOn::Never;
        });
        let mut safe_rect = ui.ctx().content_rect().intersect(ui.max_rect());
        let viewport = ui.ctx().viewport_rect();
        safe_rect.max.y = safe_rect
            .max
            .y
            .min(viewport.bottom() - viewport.height() * occlusion.clamp(0.0, 1.0))
            .max(safe_rect.min.y);
        ui.scope_builder(egui::UiBuilder::new().max_rect(safe_rect), |ui| {
            ui.set_clip_rect(safe_rect);
            self.show_content(ui);
        });
    }

    fn send_key(&mut self, key: Key) {
        let mut modifiers = Modifiers::NONE;
        if self.control {
            modifiers = modifiers.with(Modifiers::CONTROL);
        }
        if self.alt {
            modifiers = modifiers.with(Modifiers::ALT);
        }
        route_input(
            &mut self.terminal,
            InputEvent::ModifiedKey { key, modifiers },
            &mut self.sink,
        );
        self.control = false;
        self.alt = false;
    }

    fn consume_sticky_text(&mut self, events: &mut Vec<egui::Event>) {
        if !self.lifecycle.get().active {
            return;
        }
        events.retain_mut(|event| {
            if !self.control && !self.alt {
                return true;
            }
            match event {
                egui::Event::Text(text) | egui::Event::Ime(egui::ImeEvent::Commit(text)) => {
                    // One-shot latch on the first character; preserve the rest
                    // of a multi-character commit in order through the encoder.
                    for character in std::mem::take(text).chars() {
                        self.send_key(Key::Character(character));
                    }
                    // Preserve the empty commit event so the shared adapter
                    // finishes any preedit composition without duplicate text.
                    true
                }
                _ => true,
            }
        });
    }

    fn show_content(&mut self, ui: &mut egui::Ui) {
        let lifecycle = self.lifecycle.get();
        if lifecycle.suspensions != self.observed_suspensions {
            self.gestures.cancel();
            self.control = false;
            self.alt = false;
            self.observed_suspensions = lifecycle.suspensions;
        }
        if lifecycle.memory_warnings != self.observed_memory_warnings {
            self.gestures.cancel();
            // Preserve grid/history and discard only reconstructible view caches.
            let font_size = self.view.font_size_points();
            self.view = TerminalView::default();
            self.view
                .zoom_by_factor(font_size / self.view.font_size_points());
            self.observed_memory_warnings = lifecycle.memory_warnings;
        }
        egui::Panel::top("spike-status").show(ui, |ui| {
            ui.label("fesTerm · iOS feasibility spike");
            ui.small("Offline fixture · input counted, never executed");
            ui.small(format!(
                "Resume {} · suspend {} · memory {} · input {} bytes",
                lifecycle.resumes,
                lifecycle.suspensions,
                lifecycle.memory_warnings,
                self.sink.diagnostics.byte_count,
            ));
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(egui::Button::new("Reset fixture").sense(egui::Sense::CLICK))
                    .clicked()
                {
                    self.gestures.cancel();
                    self.terminal = fixture_terminal();
                    self.view = TerminalView::default();
                    self.sink = ProbeSink::default();
                    self.control = false;
                    self.alt = false;
                }
            });
        });
        let toolbar = egui::Panel::bottom("terminal-keys").show(ui, |ui| {
            ui.add_enabled_ui(lifecycle.active, |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt("extra-keys")
                    .show(ui, |ui| {
                        ui.spacing_mut().interact_size.y = 44.0;
                        ui.horizontal(|ui| {
                            // Terminal keys act like a keyboard, not focus targets.
                            if ui
                                .add(
                                    egui::Button::selectable(self.control, "Ctrl")
                                        .sense(egui::Sense::CLICK),
                                )
                                .clicked()
                            {
                                self.control = !self.control;
                            }
                            if ui
                                .add(
                                    egui::Button::selectable(self.alt, "Alt")
                                        .sense(egui::Sense::CLICK),
                                )
                                .clicked()
                            {
                                self.alt = !self.alt;
                            }
                            for (label, key) in [("Esc", Key::Escape), ("Tab", Key::Tab)] {
                                if ui
                                    .add(egui::Button::new(label).sense(egui::Sense::CLICK))
                                    .clicked()
                                {
                                    self.send_key(key);
                                }
                            }
                        });
                    });
            });
        });
        self.toolbar_rect = Some(toolbar.response.rect);
        ui.input_mut(|input| self.consume_sticky_text(&mut input.events));
        let terminal_rect = ui.available_rect_before_wrap();
        let gesture = ui.input_mut(|input| {
            let any_touches = input.any_touches();
            self.gestures.update(
                &mut input.events,
                input.time,
                terminal_rect,
                any_touches,
                lifecycle.active && input.focused,
            )
        });
        if let Some(key) = gesture.key {
            self.send_key(key);
        }
        if let Some(factor) = gesture.zoom_factor {
            self.view.zoom_by_factor(factor);
        }
        if let Some(delay) = gesture.repaint_after {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(delay));
        }
        self.view.show_with_options(
            ui,
            &mut self.terminal,
            &mut self.sink,
            TerminalViewOptions {
                paste_available: false,
                terminal_input_enabled: lifecycle.active && !gesture.block_pointer,
                keyboard_input_enabled: lifecycle.active,
                persistent_keyboard_focus: lifecycle.active,
                // No clipboard/paste policy is implemented by this probe.
                defer_paste_to_application: true,
                ..Default::default()
            },
        );
        self.gestures.paint_helper(ui, terminal_rect);
        if lifecycle.active {
            // Persistent request for the system keyboard, independent of egui
            // focus on the accessory buttons. UIKit owns keyboard presentation.
            let rect = self.view.diagnostics().grid_rect.unwrap_or(terminal_rect);
            ui.output_mut(|output| {
                output.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Normal,
                    rect,
                    cursor_rect: egui::Rect::from_min_size(
                        rect.left_bottom(),
                        egui::vec2(1.0, 1.0),
                    ),
                    should_interrupt_composition: false,
                })
            });
        }
        // Explicitly reject unsupported application intents; keep queues bounded.
        self.view.take_paste_requests();
        self.view.take_clipboard_read_request();
        self.view.take_link_requests();
        self.view.take_find_request();
    }
}

impl eframe::App for MobileApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
        if !self.reported_first_ui {
            eprintln!("festerm-mobile: first UI built");
            self.reported_first_ui = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobile_gpu_limits_accept_simulator_downlevel_capabilities() {
        let simulator = eframe::wgpu::Limits {
            max_texture_dimension_2d: 8192,
            ..eframe::wgpu::Limits::downlevel_defaults()
        };
        // Reproduce the rejected default request, then check the descriptor
        // used by the mobile host against the same constrained adapter.
        assert!(!eframe::wgpu::Limits::default().check_limits(&simulator));
        let device = mobile_device_descriptor(simulator.clone());
        assert!(device.required_limits.check_limits(&simulator));
        assert!(device.required_features.is_empty());
        assert!(device.required_limits.max_texture_dimension_2d >= 4096);
    }

    #[test]
    fn mobile_pinch_resizes_only_terminal_and_preserves_zoom_on_memory_warning() {
        for (width, height) in [
            (390.0, 844.0),
            (834.0, 1194.0),
            (1194.0, 834.0),
            (375.0, 1024.0),
        ] {
            let lifecycle = Rc::new(Cell::new(Lifecycle {
                active: true,
                ..Default::default()
            }));
            let mut app = MobileApp::new(lifecycle.clone());
            app.terminal.ingest(b"\x1b[?1003h\x1b[?1006h");
            let ctx = egui::Context::default();
            let render = |app: &mut MobileApp, time, events| {
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, height),
                        )),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ui| app.show_with_occlusion(ui, 0.35),
                );
                let ime = output.platform_output.ime.is_some();
                output.drop_without_applying_deltas();
                assert!(ime);
            };
            render(&mut app, 0.0, vec![]);
            render(&mut app, 0.01, vec![]);
            let toolbar = app.toolbar_rect.unwrap();
            let center = app.view.diagnostics().grid_rect.unwrap().center();
            let columns = app.terminal.dimensions().columns();
            let touch = |id, phase, dx| egui::Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(id),
                phase,
                pos: center + egui::vec2(dx, 0.0),
                force: None,
            };
            render(
                &mut app,
                0.1,
                vec![
                    touch(1, egui::TouchPhase::Start, -50.0),
                    touch(2, egui::TouchPhase::Start, 50.0),
                    egui::Event::PointerButton {
                        pos: center - egui::vec2(50.0, 0.0),
                        pressed: true,
                        button: egui::PointerButton::Primary,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            render(
                &mut app,
                0.2,
                vec![
                    touch(1, egui::TouchPhase::Move, -75.0),
                    touch(2, egui::TouchPhase::Move, 75.0),
                    egui::Event::PointerMoved(center - egui::vec2(75.0, 0.0)),
                ],
            );
            render(&mut app, 0.3, vec![]);
            assert_eq!(app.view.font_size_points(), 21.0);
            assert!(app.terminal.dimensions().columns() < columns);
            assert_eq!(app.toolbar_rect, Some(toolbar));
            assert!(app.view.diagnostics().grid_rect.unwrap().bottom() <= toolbar.top());
            assert!(app.gestures.helper().is_none());
            assert_eq!(ctx.zoom_factor(), 1.0); // App chrome must not zoom.
            let mut state = lifecycle.get();
            state.memory_warning();
            lifecycle.set(state);
            render(&mut app, 0.4, vec![touch(2, egui::TouchPhase::Move, 100.0)]);
            render(
                &mut app,
                0.5,
                vec![
                    touch(1, egui::TouchPhase::End, -75.0),
                    touch(2, egui::TouchPhase::End, 100.0),
                    egui::Event::PointerButton {
                        pos: center - egui::vec2(75.0, 0.0),
                        pressed: false,
                        button: egui::PointerButton::Primary,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::PointerGone,
                ],
            );
            render(&mut app, 1.0, vec![]);
            assert_eq!(app.view.font_size_points(), 21.0);
            assert_eq!(app.sink.diagnostics.byte_count, 0);
            assert!(app.terminal.queued_input().is_empty());
        }
    }

    #[test]
    fn mobile_arrow_gesture_keeps_keyboard_and_does_not_leak_mouse_reports() {
        for (width, height) in [(390.0, 844.0), (834.0, 1194.0), (1194.0, 834.0)] {
            let lifecycle = Rc::new(Cell::new(Lifecycle {
                active: true,
                ..Default::default()
            }));
            let mut app = MobileApp::new(lifecycle);
            app.terminal.ingest(b"\x1b[?1000h\x1b[?1006h");
            let ctx = egui::Context::default();
            let render = |app: &mut MobileApp, time, events| {
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, height),
                        )),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ui| app.show_with_occlusion(ui, 0.35),
                );
                let ime = output.platform_output.ime.is_some();
                output.drop_without_applying_deltas();
                assert!(ime);
            };
            render(&mut app, 0.0, vec![]);
            render(&mut app, 0.01, vec![]);
            let origin = app.view.diagnostics().grid_rect.unwrap().center();
            let moved = origin + egui::vec2(30.0, 0.0);
            let touch = |phase, pos| egui::Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(1),
                phase,
                pos,
                force: None,
            };
            let mouse = |pressed, pos| egui::Event::PointerButton {
                pos,
                pressed,
                button: egui::PointerButton::Primary,
                modifiers: egui::Modifiers::NONE,
            };
            render(
                &mut app,
                0.1,
                vec![
                    touch(egui::TouchPhase::Start, origin),
                    egui::Event::PointerMoved(origin),
                    mouse(true, origin),
                ],
            );
            render(&mut app, 0.56, vec![]);
            assert_eq!(app.gestures.helper(), Some(None));
            assert_eq!(app.sink.diagnostics.byte_count, 0);
            render(
                &mut app,
                0.6,
                vec![
                    touch(egui::TouchPhase::Move, moved),
                    egui::Event::PointerMoved(moved),
                ],
            );
            assert_eq!(app.sink.diagnostics.byte_count, 3);
            assert_eq!(
                app.gestures.helper(),
                Some(Some(terminal_gesture::Direction::Right))
            );
            render(
                &mut app,
                0.61,
                vec![
                    touch(egui::TouchPhase::End, moved),
                    mouse(false, moved),
                    egui::Event::PointerGone,
                ],
            );
            render(&mut app, 2.0, vec![]);
            assert_eq!(app.sink.diagnostics.byte_count, 3);
            assert!(app.gestures.helper().is_none());
            assert!(app.terminal.queued_input().is_empty());
        }
    }

    #[test]
    fn mobile_persistent_keyboard_does_not_restart_ime_between_frames() {
        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut app = MobileApp::new(lifecycle.clone());
        let ctx = egui::Context::default();
        let render = |app: &mut MobileApp, occlusion, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(390.0, 844.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| app.show_with_occlusion(ui, occlusion),
            )
        };
        for _ in 0..2 {
            render(&mut app, 0.0, vec![]).drop_without_applying_deltas();
        }
        assert!(ctx.memory(|memory| memory.focused().is_some()));
        let mut checked_frames = 0;
        let mut assert_stable = |output: egui::FullOutput| {
            checked_frames += 1;
            let interrupt = output
                .platform_output
                .ime
                .as_ref()
                .map(|ime| ime.should_interrupt_composition);
            output.drop_without_applying_deltas();
            assert_eq!(
                interrupt,
                Some(false),
                "final platform output must not hide/reopen the native keyboard (frame {checked_frames})"
            );
        };
        for occlusion in [0.0, 0.2, 0.36, 0.36, 0.0, 0.36] {
            assert_stable(render(&mut app, occlusion, vec![]));
        }
        let toolbar = app.toolbar_rect.unwrap();
        let grid = app.view.diagnostics().grid_rect.unwrap();
        for point in [
            toolbar.min + egui::vec2(22.0, 22.0),
            toolbar.min + egui::vec2(57.0, 22.0),
            toolbar.min + egui::vec2(92.0, 22.0),
            toolbar.min + egui::vec2(128.0, 22.0),
            grid.center(),
        ] {
            for pressed in [true, false] {
                assert_stable(render(
                    &mut app,
                    0.36,
                    vec![
                        egui::Event::PointerMoved(point),
                        egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                ));
            }
            assert_stable(render(&mut app, 0.36, vec![]));
        }
        assert!(app.sink.diagnostics.byte_count > 0);
        for event in [
            egui::Event::Text("x".into()),
            egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "x".into(),
                active_range_chars: None,
            }),
            egui::Event::Ime(egui::ImeEvent::Commit("x".into())),
        ] {
            assert_stable(render(&mut app, 0.36, vec![event]));
        }
        for key in [
            egui::Key::Enter,
            egui::Key::Backspace,
            egui::Key::Tab,
            egui::Key::ArrowUp,
        ] {
            assert_stable(render(
                &mut app,
                0.36,
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            ));
        }
        let mut state = lifecycle.get();
        state.suspend();
        lifecycle.set(state);
        let output = render(&mut app, 0.0, vec![]);
        assert!(output.platform_output.ime.is_none());
        output.drop_without_applying_deltas();
        state.resume();
        lifecycle.set(state);
        render(&mut app, 0.36, vec![]).drop_without_applying_deltas();
        assert_stable(render(&mut app, 0.36, vec![]));
    }

    #[test]
    fn mobile_phone_ipad_and_split_view_keep_terminal_above_persistent_keyboard() {
        for (width, height, keyboard_height) in [
            (390.0, 844.0, 300.0),
            (834.0, 1194.0, 360.0),
            (1194.0, 834.0, 300.0),
            (375.0, 1024.0, 340.0),
            (844.0, 390.0, 160.0),
        ] {
            let lifecycle = Rc::new(Cell::new(Lifecycle {
                active: true,
                ..Default::default()
            }));
            let mut app = MobileApp::new(lifecycle.clone());
            let ctx = egui::Context::default();
            let render = |app: &mut MobileApp, occlusion, events| {
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, height),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ui| app.show_with_occlusion(ui, occlusion),
                )
            };
            for _ in 0..2 {
                let output = render(&mut app, keyboard_height / height, vec![]);
                assert!(output.platform_output.ime.is_some());
                output.drop_without_applying_deltas();
            }
            let grid = app.view.diagnostics().grid_rect.unwrap();
            let toolbar = app.toolbar_rect.unwrap();
            assert!(grid.bottom() <= toolbar.top());
            assert!(toolbar.bottom() <= height - keyboard_height + 0.5);
            assert!(grid.height() > 0.0);
            // Tap Ctrl through the real UI: it must not dismiss the keyboard.
            let point = toolbar.min + egui::vec2(22.0, 22.0);
            for pressed in [true, false] {
                let output = render(
                    &mut app,
                    keyboard_height / height,
                    vec![
                        egui::Event::PointerMoved(point),
                        egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
                assert!(output.platform_output.ime.is_some());
                output.drop_without_applying_deltas();
            }
            assert!(app.control);
            render(
                &mut app,
                keyboard_height / height,
                vec![egui::Event::Text("c".into())],
            )
            .drop_without_applying_deltas();
            assert_eq!(app.sink.diagnostics.byte_count, 1);
            assert!(!app.control);
            // A hardware keyboard reclaims the system keyboard's region but
            // leaves the same terminal-key row available on iPhone and iPad.
            render(&mut app, 0.0, vec![]).drop_without_applying_deltas();
            assert!(app.view.diagnostics().grid_rect.unwrap().height() > grid.height());
            let mut state = lifecycle.get();
            state.suspend();
            lifecycle.set(state);
            let output = render(&mut app, 0.0, vec![egui::Event::Text("ignored".into())]);
            assert!(output.platform_output.ime.is_none());
            output.drop_without_applying_deltas();
            assert_eq!(app.sink.diagnostics.byte_count, 1);
        }
    }

    #[test]
    fn mobile_sticky_modifiers_are_one_shot_and_preserve_ime_commit_boundaries() {
        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut app = MobileApp::new(lifecycle);
        app.control = true;
        app.alt = true;
        let mut events = vec![egui::Event::Ime(egui::ImeEvent::Commit("cx".into()))];
        app.consume_sticky_text(&mut events);
        assert_eq!(app.sink.diagnostics.byte_count, 3); // ESC, Ctrl-C, literal x.
        assert!(!app.control && !app.alt);
        assert!(
            matches!(&events[0], egui::Event::Ime(egui::ImeEvent::Commit(text)) if text.is_empty())
        );
        assert!(app.terminal.queued_input().is_empty());
    }

    #[test]
    fn mobile_lifecycle_is_idempotent_and_counts_resume_after_suspend() {
        let mut state = Lifecycle::default();
        state.suspend();
        state.resume();
        state.resume();
        state.suspend();
        state.suspend();
        state.memory_warning();
        state.resume();
        assert_eq!(
            state,
            Lifecycle {
                active: true,
                resumes: 2,
                suspensions: 1,
                memory_warnings: 1
            }
        );
    }

    #[test]
    fn mobile_probe_uses_core_encoding_without_echoing_or_retaining_input() {
        let mut terminal = fixture_terminal();
        let before = terminal.row_text(terminal.dimensions().rows() - 2);
        let mut sink = ProbeSink::default();
        let route = route_input(&mut terminal, InputEvent::Key(Key::ArrowUp), &mut sink);
        assert_eq!(route.delivered_bytes, 3);
        assert_eq!(sink.diagnostics.byte_count, 3);
        assert_eq!(sink.diagnostics.event_count, 1);
        assert!(terminal.queued_input().is_empty());
        assert_eq!(terminal.row_text(terminal.dimensions().rows() - 2), before);
    }

    #[test]
    fn mobile_fixture_renders_at_phone_width_and_preserves_grid_on_memory_warning() {
        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut app = MobileApp::new(lifecycle.clone());
        let ctx = egui::Context::default();
        let render = |app: &mut MobileApp| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(390.0, 844.0),
                    )),
                    safe_area_insets: Some(egui::SafeAreaInsets(egui::epaint::MarginF32 {
                        top: 59.0,
                        bottom: 34.0,
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                |ui| app.show(ui),
            )
        };
        render(&mut app).drop_without_applying_deltas();
        let output = render(&mut app);
        assert!(!output.shapes.is_empty());
        output.drop_without_applying_deltas();
        assert!(app.terminal.dimensions().columns() < 80);
        let grid = app.view.diagnostics().grid_rect.unwrap();
        assert!(grid.top() >= 59.0);
        assert!(grid.bottom() <= 844.0 - 34.0);
        let before = app.terminal.row_text(0);
        let mut state = lifecycle.get();
        state.memory_warning();
        lifecycle.set(state);
        render(&mut app).drop_without_applying_deltas();
        assert_eq!(app.terminal.row_text(0), before);
        assert_eq!(app.observed_memory_warnings, 1);
    }
}
