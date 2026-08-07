use std::time::Duration;

use eframe::egui;
use festerm_pty::LocalProfile;
use festerm_ui_egui::chrome::{self, ChipId, ChipStatus, ChipViewModel, ChromeAction};
use festerm_ui_egui::overlay::{self, OverlayAction};
use festerm_ui_egui::palette::{self, PaletteItem, PaletteState};

use crate::native_smoke::NativeWindowSmoke;
use crate::screens;
use crate::tabs::{AppCommand, AppState, TabContent, TabId};

const APPLICATION_TITLE: &str = "fesTerm";

/// Composition root.
///
/// `AppState` owns the always-nonempty tab collection and session/command
/// policy (`docs/application-command-model.md`); this struct wires it to the
/// `eframe` event loop, the top-of-window chrome
/// (`crates/festerm-ui-egui/src/chrome.rs`), and the native-window smoke
/// driver.
pub struct FesTermApp {
    state: AppState,
    /// The tab created at startup, retained so the native-window smoke driver
    /// (which predates and does not know about tabs) can keep addressing the
    /// one session it drives.
    primary_tab: TabId,
    window_title: String,
    native_smoke: Option<NativeWindowSmoke>,
    palette: PaletteState,
}

impl FesTermApp {
    pub fn new(context: &egui::Context) -> Self {
        let native_smoke = NativeWindowSmoke::from_environment();
        let smoke_profile = native_smoke.as_ref().map(|smoke| {
            LocalProfile::new(smoke.test_child_path()).with_arguments(smoke.test_child_arguments())
        });
        let (state, primary_tab) = AppState::with_primary_session(context, smoke_profile);
        Self {
            state,
            primary_tab,
            window_title: APPLICATION_TITLE.to_owned(),
            native_smoke,
            palette: PaletteState::default(),
        }
    }

    fn update_window_title(&mut self, context: &egui::Context) {
        let terminal_title = match &self.state.active_tab_mut().content {
            TabContent::Session(session) => session.terminal.title().to_owned(),
            TabContent::Launcher | TabContent::Settings => String::new(),
        };
        let title = Self::window_title(&terminal_title);
        if self.window_title != title {
            context.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    fn window_title(terminal_title: &str) -> String {
        match terminal_title {
            "" => APPLICATION_TITLE.to_owned(),
            terminal_title => format!("{terminal_title} - {APPLICATION_TITLE}"),
        }
    }

    /// Drains every open session's bounded backend queues, independent of
    /// which tab is active: each session chip represents "a persistent
    /// object with its own identity and state" (`docs/gui-design.md`) that
    /// must keep making progress while another tab is focused.
    fn pump_all_sessions(&mut self, context: &egui::Context) {
        let mut needs_repaint = false;
        for session in self.state.session_tabs_mut() {
            if session.controller.pump_events(&mut session.terminal) {
                needs_repaint = true;
            }
            session
                .controller
                .forward_terminal_replies(&mut session.terminal);
            session.controller.flush_pending_writes();
            session.controller.flush_pending_resize();
        }
        if needs_repaint {
            context.request_repaint();
        }
    }

    fn tab_id_for_chip(&self, chip_id: ChipId) -> Option<TabId> {
        self.state
            .tabs()
            .iter()
            .find(|tab| tab.id.chip_id() == chip_id.0)
            .map(|tab| tab.id)
    }

    /// Translates chrome gestures into `AppCommand`s and dispatches them
    /// through the single command-handling path
    /// (`docs/application-command-model.md`).
    fn dispatch_chrome_actions(&mut self, actions: Vec<ChromeAction>, context: &egui::Context) {
        for action in actions {
            match action {
                ChromeAction::NewTab => self.state.dispatch(AppCommand::NewLauncherTab, context),
                ChromeAction::OpenSettings => {
                    self.state.dispatch(AppCommand::OpenSettings, context)
                }
                ChromeAction::ToggleInspector => self
                    .state
                    .dispatch(AppCommand::ToggleSessionInspector, context),
                ChromeAction::Activate(chip_id) => {
                    if let Some(id) = self.tab_id_for_chip(chip_id) {
                        self.state.dispatch(AppCommand::ActivateTab(id), context);
                    }
                }
                ChromeAction::Close(chip_id) => {
                    if let Some(id) = self.tab_id_for_chip(chip_id) {
                        self.state.dispatch(AppCommand::CloseTab(id), context);
                    }
                }
                ChromeAction::Reorder { moved, before } => {
                    let Some(moved) = self.tab_id_for_chip(moved) else {
                        continue;
                    };
                    let before = before.and_then(|chip_id| self.tab_id_for_chip(chip_id));
                    self.state
                        .dispatch(AppCommand::ReorderTab { moved, before }, context);
                }
            }
        }
    }

    /// Builds the current frame's command-palette items: every dispatchable
    /// application action, plus one "Activate" entry per open tab so the
    /// palette also serves as the searchable session switcher required by
    /// `docs/gui-design.md` ("a searchable session switcher keyed primarily
    /// by stable identity").
    fn palette_items(&self) -> Vec<PaletteItem> {
        const NEW_LAUNCHER_TAB: u64 = 1;
        const OPEN_SETTINGS: u64 = 2;
        const START_LOCAL_SESSION: u64 = 3;
        const TOGGLE_INSPECTOR: u64 = 4;
        const CLOSE_ACTIVE_TAB: u64 = 5;
        const TOGGLE_CHIP_LAYOUT: u64 = 6;
        // Tab-scoped palette ids are offset well past the fixed action ids so
        // they never collide with a real `TabId::chip_id()` value.
        const TAB_ACTIVATE_OFFSET: u64 = 1 << 32;

        let mut items = vec![
            PaletteItem {
                id: NEW_LAUNCHER_TAB,
                label: "New Launcher Tab".to_owned(),
                hint: None,
            },
            PaletteItem {
                id: START_LOCAL_SESSION,
                label: "Start Local Shell".to_owned(),
                hint: None,
            },
            PaletteItem {
                id: OPEN_SETTINGS,
                label: "Open Settings".to_owned(),
                hint: None,
            },
            PaletteItem {
                id: TOGGLE_INSPECTOR,
                label: "Toggle Session Inspector".to_owned(),
                hint: None,
            },
            PaletteItem {
                id: CLOSE_ACTIVE_TAB,
                label: "Close Active Tab".to_owned(),
                hint: None,
            },
            PaletteItem {
                id: TOGGLE_CHIP_LAYOUT,
                label: "Toggle Chip Wrapping".to_owned(),
                hint: None,
            },
        ];
        for tab in self.state.tabs() {
            let (label, hint) = match &tab.content {
                TabContent::Launcher => ("Launcher".to_owned(), None),
                TabContent::Settings => ("Settings".to_owned(), None),
                TabContent::Session(session) => {
                    let dynamic_title = session.terminal.title();
                    let hint = (!dynamic_title.is_empty()).then(|| dynamic_title.to_owned());
                    (session.label.clone(), hint)
                }
            };
            items.push(PaletteItem {
                id: TAB_ACTIVATE_OFFSET + tab.id.chip_id(),
                label: format!("Activate: {label}"),
                hint,
            });
        }
        items
    }

    /// Applies a selected command-palette item id, translating it back into
    /// the same `AppCommand` path used by chrome gestures and shortcuts.
    fn dispatch_palette_selection(&mut self, id: u64, context: &egui::Context) {
        const TAB_ACTIVATE_OFFSET: u64 = 1 << 32;
        match id {
            1 => self.state.dispatch(AppCommand::NewLauncherTab, context),
            2 => self.state.dispatch(AppCommand::OpenSettings, context),
            3 => self.state.dispatch(AppCommand::StartLocalSession, context),
            4 => self
                .state
                .dispatch(AppCommand::ToggleSessionInspector, context),
            5 => {
                let active = self.state.active();
                self.state.dispatch(AppCommand::CloseTab(active), context);
            }
            6 => self.state.dispatch(AppCommand::ToggleChipLayout, context),
            id if id >= TAB_ACTIVATE_OFFSET => {
                let chip_id = ChipId(id - TAB_ACTIVATE_OFFSET);
                if let Some(target) = self.tab_id_for_chip(chip_id) {
                    self.state
                        .dispatch(AppCommand::ActivateTab(target), context);
                }
            }
            _ => {}
        }
    }

    /// Recognized global shortcuts (`docs/gui-design.md` "Interaction
    /// Conventions": "Exact platform shortcuts remain to be specified");
    /// these are a first, revisitable binding, tracked for confirmation in a
    /// follow-up usability pass. All bindings dispatch through the same
    /// `AppCommand` path as chip clicks and the palette.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let open_palette = ctx.input_mut(|input| {
            input.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::P,
            )
        });
        if open_palette {
            self.palette.toggle();
        }
        // While the palette is open, it owns Enter/Escape/arrow keys; avoid
        // also acting on tab-management shortcuts this frame.
        if self.palette.is_open() {
            return;
        }
        let new_tab =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::T));
        let close_tab =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::W));
        let next_tab =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::Tab));
        let previous_tab = ctx.input_mut(|input| {
            input.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Tab,
            )
        });

        if new_tab {
            self.state.dispatch(AppCommand::NewLauncherTab, ctx);
        }
        if close_tab {
            let active = self.state.active();
            self.state.dispatch(AppCommand::CloseTab(active), ctx);
        }
        if next_tab {
            self.state.dispatch(AppCommand::ActivateNextTab, ctx);
        }
        if previous_tab {
            self.state.dispatch(AppCommand::ActivatePreviousTab, ctx);
        }
    }

    /// Builds this frame's chip view models. Stable tab identity (label)
    /// always leads; a non-empty terminal-provided title is shown only as
    /// secondary metadata (`docs/gui-design.md` "Identity precedence").
    fn chip_view_models(&self) -> (Vec<ChipViewModel>, ChipId) {
        let chips = self
            .state
            .tabs()
            .iter()
            .map(|tab| {
                let (primary, secondary, status) = match &tab.content {
                    TabContent::Launcher => ("Launcher".to_owned(), None, ChipStatus::Neutral),
                    TabContent::Settings => ("Settings".to_owned(), None, ChipStatus::Neutral),
                    TabContent::Session(session) => {
                        let dynamic_title = session.terminal.title();
                        let secondary =
                            (!dynamic_title.is_empty()).then(|| dynamic_title.to_owned());
                        (session.label.clone(), secondary, session.chip_status())
                    }
                };
                ChipViewModel {
                    id: ChipId(tab.id.chip_id()),
                    primary,
                    secondary,
                    status,
                    closable: true,
                }
            })
            .collect();
        (chips, ChipId(self.state.active().chip_id()))
    }

    /// Right-side session inspector (`docs/gui-design.md` "Application chrome
    /// and session context"): hidden by default, and shows only content-free
    /// connection state and diagnostics for the active session. It never
    /// hosts Settings.
    fn show_session_inspector(&self, ui: &mut egui::Ui) {
        let TabContent::Session(session) = &self.state.active_tab().content else {
            return;
        };
        let status = session.controller.status_line();
        let diagnostics = session.controller.diagnostics_line();
        let chip_status = session.chip_status();
        egui::Panel::right("session_inspector")
            .resizable(false)
            .show(ui, |ui| {
                ui.heading("Session Inspector");
                ui.separator();
                ui.label(egui::RichText::new(&session.label).strong());
                ui.label(chip_status.accessible_label());
                ui.add_space(4.0);
                ui.label(status);
                ui.add_space(8.0);
                ui.label(egui::RichText::new(diagnostics).small().weak());
            });
    }
}

impl eframe::App for FesTermApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_all_sessions(context);
        self.update_window_title(context);
        if let Some(smoke) = self.native_smoke.as_mut() {
            if let Some(primary) = self.state.session_tab_mut(self.primary_tab) {
                smoke.drive(context, &mut primary.terminal, &mut primary.controller);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui_content(ui);
    }
}

impl FesTermApp {
    /// The full chrome/palette/session UI for one frame. Split out from
    /// [`eframe::App::ui`] so headless `egui_kittest` tests can drive it
    /// directly without constructing an `eframe::Frame` (whose fields are
    /// private to `eframe` and not test-constructible).
    fn ui_content(&mut self, ui: &mut egui::Ui) {
        self.handle_shortcuts(ui.ctx());

        let (chips, active_chip) = self.chip_view_models();
        let inspector_open = self.state.inspector_open();
        let actions = chrome::show(
            ui,
            &chips,
            active_chip,
            inspector_open,
            self.state.chip_layout(),
        );
        ui.separator();
        self.dispatch_chrome_actions(actions, &ui.ctx().clone());

        if let Some(decision) = {
            let items = self.palette_items();
            palette::show(ui.ctx(), &mut self.palette, &items)
        } {
            self.palette.close();
            if let Some(id) = decision {
                let context = ui.ctx().clone();
                self.dispatch_palette_selection(id, &context);
            }
        }

        if inspector_open {
            self.show_session_inspector(ui);
        }

        let mut screen_command = None;
        let mut overlay_action = None;
        let chip_layout = self.state.chip_layout();
        let active_tab_id = self.state.active();
        {
            let tab = self.state.active_tab_mut();
            match &mut tab.content {
                TabContent::Launcher => {
                    screen_command = screens::show_launcher(ui);
                }
                TabContent::Settings => {
                    screen_command = screens::show_settings(ui, chip_layout);
                }
                TabContent::Session(session) => {
                    let session_status = session.controller.status_line();
                    let session_diagnostics = session.controller.diagnostics_line();
                    session.view.show_with_status(
                        ui,
                        &mut session.terminal,
                        &mut session.controller,
                        &session_status,
                        &session_diagnostics,
                    );
                    session
                        .controller
                        .observe_resize_probe_terminal_state(&session.terminal);
                    session
                        .controller
                        .forward_terminal_replies(&mut session.terminal);
                    session.controller.flush_pending_writes();
                    session.controller.flush_pending_resize();
                    if session.controller.pump_events(&mut session.terminal) {
                        ui.ctx().request_repaint();
                    }
                    overlay_action = overlay::show(ui.ctx(), session.chip_status());
                }
            }
        }
        if let Some(command) = screen_command {
            let context = ui.ctx().clone();
            self.state.dispatch(command, &context);
        }
        if let Some(action) = overlay_action {
            let context = ui.ctx().clone();
            match action {
                OverlayAction::OpenDiagnostics => {
                    self.state
                        .dispatch(AppCommand::ToggleSessionInspector, &context);
                }
                OverlayAction::CloseTab => {
                    self.state
                        .dispatch(AppCommand::CloseTab(active_tab_id), &context);
                }
            }
        }

        if self.native_smoke.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
impl FesTermApp {
    /// Builds a `FesTermApp` around a launcher tab instead of a live local
    /// shell (`AppState::for_test`), so headless end-to-end UI tests do not
    /// need a real PTY and do not depend on `eframe::Frame`, which has no
    /// public/test constructor.
    fn for_test() -> Self {
        let state = AppState::for_test();
        let primary_tab = state.active();
        Self {
            state,
            primary_tab,
            window_title: APPLICATION_TITLE.to_owned(),
            native_smoke: None,
            palette: PaletteState::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    fn harness() -> Harness<'static, FesTermApp> {
        Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .build_ui_state(
                |ui, app: &mut FesTermApp| app.ui_content(ui),
                FesTermApp::for_test(),
            )
    }

    #[test]
    fn terminal_title_is_scoped_to_the_application_window() {
        assert_eq!(FesTermApp::window_title(""), APPLICATION_TITLE);
        assert_eq!(FesTermApp::window_title("editor"), "editor - fesTerm");
    }

    #[test]
    fn ctrl_t_shortcut_opens_a_new_launcher_tab_end_to_end() {
        let mut harness = harness();
        harness.run();
        let before = harness.state().state.tabs().len();

        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::T);
        harness.run();

        assert_eq!(harness.state().state.tabs().len(), before + 1);
    }

    #[test]
    fn clicking_a_chip_close_button_closes_that_tab_end_to_end() {
        let mut harness = harness();
        harness.run();
        // Open a second tab so there is one to close without emptying root.
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::T);
        harness.run();
        let before = harness.state().state.tabs().len();

        harness
            .get_all_by_label("\u{2715}")
            .next()
            .expect("at least one closable chip")
            .click();
        harness.run();

        assert_eq!(harness.state().state.tabs().len(), before - 1);
    }

    #[test]
    fn command_palette_selection_activates_the_chosen_tab_end_to_end() {
        let mut harness = harness();
        harness.run();

        harness.key_press_modifiers(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::P,
        );
        harness.run();
        assert!(harness.state().palette.is_open());

        harness.get_by_label("Open Settings").click();
        harness.run();
        assert!(!harness.state().palette.is_open());
        let settings_tab = harness.state().state.active();
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Settings
        ));

        harness.key_press_modifiers(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::P,
        );
        harness.run();
        harness.get_by_label("Activate: Launcher").click();
        harness.run();

        assert_ne!(harness.state().state.active(), settings_tab);
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Launcher
        ));
    }
}
