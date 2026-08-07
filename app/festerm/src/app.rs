use std::time::Duration;

use eframe::egui;
use festerm_pty::LocalProfile;
use festerm_ui_egui::chrome::{self, ChipId, ChipStatus, ChipViewModel, ChromeAction};

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
            }
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
        egui::Panel::right("session_inspector")
            .resizable(false)
            .show(ui, |ui| {
                ui.heading("Session Inspector");
                ui.separator();
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
        let (chips, active_chip) = self.chip_view_models();
        let inspector_open = self.state.inspector_open();
        let actions = chrome::show(ui, &chips, active_chip, inspector_open);
        ui.separator();
        self.dispatch_chrome_actions(actions, &ui.ctx().clone());

        if inspector_open {
            self.show_session_inspector(ui);
        }

        let mut launcher_command = None;
        {
            let tab = self.state.active_tab_mut();
            match &mut tab.content {
                TabContent::Launcher => {
                    launcher_command = screens::show_launcher(ui);
                }
                TabContent::Settings => screens::show_settings(ui),
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
                }
            }
        }
        if let Some(command) = launcher_command {
            let context = ui.ctx().clone();
            self.state.dispatch(command, &context);
        }

        if self.native_smoke.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_title_is_scoped_to_the_application_window() {
        assert_eq!(FesTermApp::window_title(""), APPLICATION_TITLE);
        assert_eq!(FesTermApp::window_title("editor"), "editor - fesTerm");
    }
}
