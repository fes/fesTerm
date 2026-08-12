use std::time::Duration;

use eframe::egui;
use festerm_config::Configuration;
use festerm_pty::LocalProfile;
use festerm_ui_egui::chrome::{self, ChipId, ChipStatus, ChipViewModel, ChromeAction};
use festerm_ui_egui::overlay::{self, OverlayAction};
use festerm_ui_egui::palette::{self, PaletteItem, PaletteState};
use festerm_ui_egui::theme;

use crate::configuration_startup::{
    ConfigurationReloader, ConfigurationStartupStatus, StartupConfiguration,
};
use crate::native_smoke::NativeWindowSmoke;
use crate::screens;
use crate::tabs::{AppCommand, AppState, HostKeyTrustDecision, TabContent, TabId};

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
    /// The default local tab created when startup has no restored workspace.
    /// The native-window smoke driver (which predates tabs) uses it to address
    /// the one session it drives.
    primary_tab: Option<TabId>,
    window_title: String,
    native_smoke: Option<NativeWindowSmoke>,
    palette: PaletteState,
    configuration_status: ConfigurationStartupStatus,
    configuration_reloader: ConfigurationReloader,
}

impl FesTermApp {
    /// Builds the application around explicitly supplied, already-validated
    /// profile and optional workspace metadata.
    pub fn with_configuration(context: &egui::Context, configuration: Configuration) -> Self {
        Self::with_configuration_status(context, configuration, ConfigurationStartupStatus::Missing)
    }

    pub(crate) fn with_startup_configuration(
        context: &egui::Context,
        startup_configuration: StartupConfiguration,
    ) -> Self {
        let (configuration, status, configuration_reloader) = startup_configuration.into_parts();
        let mut app = Self::with_configuration(context, configuration);
        app.configuration_status = status;
        app.configuration_reloader = configuration_reloader;
        app
    }

    fn with_configuration_status(
        context: &egui::Context,
        configuration: Configuration,
        configuration_status: ConfigurationStartupStatus,
    ) -> Self {
        // One semantic blue-graphite default for application surfaces and
        // widgets. Terminal ANSI and explicit RGB colors remain independent.
        context.set_visuals(theme::default_visuals());
        let native_smoke = NativeWindowSmoke::from_environment();
        let smoke_profile = native_smoke.as_ref().map(|smoke| {
            LocalProfile::new(smoke.test_child_path()).with_arguments(smoke.test_child_arguments())
        });
        let (state, primary_tab) = if let Some(workspace) = configuration.workspace().cloned() {
            (
                AppState::with_restored_workspace(context, configuration, &workspace),
                None,
            )
        } else {
            let (state, primary_tab) =
                AppState::with_primary_session(context, smoke_profile, configuration);
            (state, Some(primary_tab))
        };
        Self {
            state,
            primary_tab,
            window_title: APPLICATION_TITLE.to_owned(),
            native_smoke,
            palette: PaletteState::default(),
            configuration_status,
            configuration_reloader: ConfigurationReloader::unavailable(),
        }
    }

    /// Handles the only user-triggered configuration I/O. The reloader keeps
    /// the selected path private; `AppState` receives a complete immutable
    /// replacement only after successful validation. Session state is not
    /// involved, so existing transports continue unchanged.
    fn reload_configuration(&mut self) {
        let (replacement, status) = self.configuration_reloader.reload();
        if let Some(configuration) = replacement {
            self.state.replace_configuration(configuration);
        }
        self.configuration_status = status;
    }

    /// Captures a metadata-only workspace and saves it only for an explicit
    /// Settings action. The current configuration changes only after the
    /// atomic file replacement has succeeded.
    fn save_workspace(&mut self) {
        let replacement = match self.state.capture_workspace_configuration() {
            Ok(replacement) => replacement,
            Err(_) => {
                self.configuration_status = ConfigurationStartupStatus::WorkspaceSaveFailure(
                    crate::configuration_startup::ConfigurationLoadFailure::Invalid,
                );
                return;
            }
        };
        let status = self.configuration_reloader.save_workspace(&replacement);
        if matches!(status, ConfigurationStartupStatus::WorkspaceSaved) {
            self.state.replace_configuration(replacement);
        }
        self.configuration_status = status;
    }

    fn update_window_title(&mut self, context: &egui::Context) {
        let terminal_title = match &self.state.active_tab_mut().content {
            TabContent::Session(session) => session.terminal.title().to_owned(),
            TabContent::Launcher
            | TabContent::Settings
            | TabContent::SshAuthenticationRequired(_) => String::new(),
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

    /// Reduces a terminal-provided OSC title to just its final path
    /// component for the chip row's secondary text, so a chip shows
    /// `cmd.exe` rather than `C:\WINDOWS\system32\cmd.exe`
    /// (`docs/gui-design.md` "Identity precedence": the stable label leads,
    /// and secondary terminal metadata should stay compact rather than
    /// forcing the chip to grow to fit a full path). Falls back to the
    /// original string when it has no path-like structure to extract a
    /// final component from.
    fn display_secondary(terminal_title: &str) -> String {
        std::path::Path::new(terminal_title)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| terminal_title.to_owned())
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
                ChromeAction::TogglePalette => self.palette.toggle(),
                ChromeAction::ToggleChipLayout => {
                    self.state.dispatch(AppCommand::ToggleChipLayout, context)
                }
                ChromeAction::Activate(chip_id) => {
                    if let Some(id) = self.tab_id_for_chip(chip_id) {
                        self.state.dispatch(AppCommand::ActivateTab(id), context);
                        // Re-claim keyboard focus for the now-active
                        // session's terminal (`TerminalView::
                        // request_focus_on_next_frame`): selecting a chip
                        // otherwise left focus on the chrome row until the
                        // user clicked inside the terminal themselves.
                        if let Some(tab) = self.state.session_tab_mut(id) {
                            tab.view.request_focus_on_next_frame();
                        }
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
                ChromeAction::Rename { id: chip_id, name } => {
                    if let Some(id) = self.tab_id_for_chip(chip_id) {
                        self.state
                            .dispatch(AppCommand::RenameTab(id, name), context);
                    }
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
                TabContent::SshAuthenticationRequired(tab) => (
                    tab.profile.identifier().to_owned(),
                    Some(format!(
                        "SSH authentication required · {}:{}",
                        tab.profile.host(),
                        tab.profile.port()
                    )),
                ),
                TabContent::Session(session) => {
                    let dynamic_title = session.terminal.title();
                    let hint = (!dynamic_title.is_empty())
                        .then(|| dynamic_title.to_owned())
                        .or_else(|| session.launch_secondary.clone());
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
    /// Conventions"). Tab creation/closure deliberately use Command on macOS
    /// and Ctrl+Shift on Windows/Linux, leaving plain Ctrl+T and Ctrl+W
    /// available to terminal applications such as Vim and Emacs. All bindings
    /// dispatch through the same `AppCommand` path as chip clicks and the
    /// palette.
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
        let tab_management_modifiers = if cfg!(target_os = "macos") {
            egui::Modifiers::COMMAND
        } else {
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT
        };
        let new_tab =
            ctx.input_mut(|input| input.consume_key(tab_management_modifiers, egui::Key::T));
        let close_tab =
            ctx.input_mut(|input| input.consume_key(tab_management_modifiers, egui::Key::W));
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
                    TabContent::SshAuthenticationRequired(tab) => (
                        tab.profile.identifier().to_owned(),
                        Some(format!(
                            "SSH authentication required · {}:{}",
                            tab.profile.host(),
                            tab.profile.port()
                        )),
                        ChipStatus::Neutral,
                    ),
                    TabContent::Session(session) => {
                        let dynamic_title = session.terminal.title();
                        let secondary = (!dynamic_title.is_empty())
                            .then(|| Self::display_secondary(dynamic_title))
                            .or_else(|| session.launch_secondary.clone());
                        (session.label.clone(), secondary, session.chip_status())
                    }
                };
                let renamable = matches!(tab.content, TabContent::Session(_));
                ChipViewModel {
                    id: ChipId(tab.id.chip_id()),
                    primary,
                    secondary,
                    status,
                    closable: true,
                    renamable,
                }
            })
            .collect();
        (chips, ChipId(self.state.active().chip_id()))
    }

    /// Right-side session inspector (`docs/gui-design.md` "Application chrome
    /// and session context"): hidden by default, and shows only content-free
    /// connection state and diagnostics for the active session. It never
    /// hosts Settings.
    fn show_session_inspector(&self, ui: &mut egui::Ui) -> Option<AppCommand> {
        let TabContent::Session(session) = &self.state.active_tab().content else {
            return None;
        };
        let reconnect_available = session.reconnect_available();
        let tab = self.state.active();
        let status = session.controller.status_line();
        let diagnostics = session.controller.diagnostics_line();
        let chip_status = session.chip_status();
        let mut reconnect = false;
        egui::Panel::right("session_inspector")
            .resizable(false)
            .show(ui, |ui| {
                ui.heading("Session Inspector");
                ui.separator();
                ui.label(egui::RichText::new(&session.label).strong());
                ui.label(chip_status.accessible_label());
                ui.add_space(4.0);
                ui.label(status);
                if reconnect_available {
                    ui.add_space(8.0);
                    if ui
                        .button("Reconnect")
                        .on_hover_text(
                            "Start one fresh SSH connection. The remote shell is not restored.",
                        )
                        .clicked()
                    {
                        reconnect = true;
                    }
                }
                ui.add_space(8.0);
                ui.label(egui::RichText::new(diagnostics).small().weak());
            });
        reconnect.then_some(AppCommand::ReconnectSession(tab))
    }

    /// Bottom application status bar (`docs/gui-design.md` "Contextual
    /// status region"): the left segment summarizes the active session using
    /// only genuinely available data (never fabricated shell
    /// version/encoding/line-ending metadata fesTerm doesn't track); the
    /// right segment shows connection status plus a local clock/date. The
    /// active session's grid dimensions are always shown alongside the
    /// left segment (`docs/gui-design.md` "Bottom status bar").
    fn show_status_bar(&self, ui: &mut egui::Ui) {
        let (left, dimensions, system, status, status_label) =
            match &self.state.active_tab().content {
                TabContent::Launcher
                | TabContent::Settings
                | TabContent::SshAuthenticationRequired(_) => (
                    APPLICATION_TITLE.to_owned(),
                    None,
                    None,
                    ChipStatus::Neutral,
                    "",
                ),
                TabContent::Session(session) => {
                    let secondary = (!session.terminal.title().is_empty())
                        .then(|| Self::display_secondary(session.terminal.title()))
                        .or_else(|| session.launch_secondary.clone());
                    let left = match secondary {
                        Some(secondary) => format!("{} — {}", session.label, secondary),
                        None => session.label.clone(),
                    };
                    let status = session.chip_status();
                    (
                        left,
                        session.view.dimensions_label(),
                        Some(session.system_label()),
                        status,
                        status.accessible_label(),
                    )
                }
            };
        let now = chrono::Local::now();
        let clock = now.format("%H:%M:%S").to_string();
        let date = now.format("%Y-%m-%d").to_string();
        egui::Panel::bottom("status_bar")
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(theme::SURFACE_WINDOW))
            .show(ui, |ui| {
                festerm_ui_egui::statusbar::show(
                    ui,
                    festerm_ui_egui::statusbar::StatusBarContent {
                        left: &left,
                        dimensions: dimensions.as_deref(),
                        system,
                        status,
                        status_label,
                        clock: &clock,
                        date: &date,
                    },
                );
            });
    }

    /// Shows the M7 host-trust decision only for the active SSH tab. The
    /// returned command is dispatched after UI construction, so clicking a
    /// control only signals the SSH worker and never waits for network I/O on
    /// the GUI thread.
    fn show_host_key_prompt(&self, ui: &mut egui::Ui) -> Option<AppCommand> {
        let tab = self.state.active_tab();
        let TabContent::Session(session) = &tab.content else {
            return None;
        };
        let prompt = session.host_key_prompt()?;
        let host_port = Self::canonical_host_port(prompt.host(), prompt.port());
        let fingerprint = prompt.sha256_fingerprint();
        let mut decision = None;

        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.heading("Verify SSH Host Key");
            ui.label("Verify this host before connecting:");
            ui.horizontal(|ui| {
                ui.label("Host:");
                ui.monospace(&host_port);
            });
            ui.horizontal(|ui| {
                ui.label("SHA-256 fingerprint:");
                ui.monospace(fingerprint);
            });
            ui.horizontal(|ui| {
                if ui.button("Reject").clicked() {
                    decision = Some(HostKeyTrustDecision::Reject);
                }
                if ui.button("Accept Once").clicked() {
                    decision = Some(HostKeyTrustDecision::AcceptOnce);
                }
            });
        });

        decision.map(|decision| AppCommand::ResolveHostKeyTrust {
            tab: tab.id,
            decision,
        })
    }

    fn canonical_host_port(host: &str, port: u16) -> String {
        if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        }
    }
}

impl eframe::App for FesTermApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_all_sessions(context);
        self.update_window_title(context);
        if let Some(smoke) = self.native_smoke.as_mut() {
            if let Some(primary_tab) = self.primary_tab {
                if let Some(primary) = self.state.session_tab_mut(primary_tab) {
                    smoke.drive(context, &mut primary.terminal, &mut primary.controller);
                }
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
        // No explicit separator line here: the chrome band now paints its
        // own lighter `CHROME_BACKGROUND` fill, and the natural color
        // contrast between that band and the darker terminal content below
        // it reads as the boundary (mockup: a near-invisible seam, not a
        // bright rule).
        self.dispatch_chrome_actions(actions, &ui.ctx().clone());

        if self.state.status_bar_visible() {
            self.show_status_bar(ui);
        }

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

        let inspector_command = inspector_open
            .then(|| self.show_session_inspector(ui))
            .flatten();

        let host_key_command = self.show_host_key_prompt(ui);
        let mut screen_command = None;
        let mut overlay_action = None;
        let chip_layout = self.state.chip_layout();
        let active_tab_id = self.state.active();
        {
            let tab = self.state.active_tab_mut();
            match &mut tab.content {
                TabContent::Launcher => {
                    screen_command = screens::show_launcher(
                        ui,
                        active_tab_id,
                        self.state.configuration().profiles(),
                    );
                }
                TabContent::Settings => {
                    screen_command = screens::show_settings(
                        ui,
                        chip_layout,
                        self.state.status_bar_visible(),
                        self.configuration_status,
                    );
                }
                TabContent::SshAuthenticationRequired(tab) => {
                    screen_command =
                        screens::show_ssh_authentication_required(ui, active_tab_id, &tab.profile);
                }
                TabContent::Session(session) => {
                    session
                        .view
                        .show(ui, &mut session.terminal, &mut session.controller);
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
        if let Some(command) = host_key_command {
            let context = ui.ctx().clone();
            self.state.dispatch(command, &context);
        }
        if let Some(command) = inspector_command {
            let context = ui.ctx().clone();
            self.state.dispatch(command, &context);
        }
        if let Some(command) = screen_command {
            match command {
                AppCommand::ReloadConfiguration => self.reload_configuration(),
                AppCommand::SaveWorkspace => self.save_workspace(),
                command => {
                    let context = ui.ctx().clone();
                    self.state.dispatch(command, &context);
                }
            }
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
    /// shell, so headless end-to-end UI tests do not need a real PTY and do
    /// not depend on `eframe::Frame`, which has no public/test constructor.
    fn for_test_with_configuration(configuration: Configuration) -> Self {
        let state = AppState::for_test_with_configuration(configuration);
        let primary_tab = Some(state.active());
        Self {
            state,
            primary_tab,
            window_title: APPLICATION_TITLE.to_owned(),
            native_smoke: None,
            palette: PaletteState::default(),
            configuration_status: ConfigurationStartupStatus::Missing,
            configuration_reloader: ConfigurationReloader::unavailable(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    fn harness() -> Harness<'static, FesTermApp> {
        harness_with_configuration(Configuration::empty())
    }

    fn harness_with_configuration(configuration: Configuration) -> Harness<'static, FesTermApp> {
        Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .build_ui_state(
                |ui, app: &mut FesTermApp| app.ui_content(ui),
                FesTermApp::for_test_with_configuration(configuration),
            )
    }

    fn tab_management_modifiers() -> egui::Modifiers {
        if cfg!(target_os = "macos") {
            egui::Modifiers::COMMAND
        } else {
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT
        }
    }

    #[test]
    fn terminal_title_is_scoped_to_the_application_window() {
        assert_eq!(FesTermApp::window_title(""), APPLICATION_TITLE);
        assert_eq!(FesTermApp::window_title("editor"), "editor - fesTerm");
    }

    #[test]
    fn canonical_host_port_preserves_ssh_destination_boundaries() {
        assert_eq!(
            FesTermApp::canonical_host_port("ssh.example.test", 2222),
            "ssh.example.test:2222"
        );
        assert_eq!(
            FesTermApp::canonical_host_port("2001:db8::7", 22),
            "[2001:db8::7]:22"
        );
    }

    #[test]
    fn startup_workspace_replaces_the_default_local_session() {
        let workspace = festerm_config::WorkspaceConfiguration::new(
            vec![
                festerm_config::WorkspaceTab::launcher("launcher").expect("launcher tab is valid"),
                festerm_config::WorkspaceTab::ssh_session("remote", "production")
                    .expect("SSH tab is valid"),
            ],
            Some("remote".to_owned()),
        )
        .expect("workspace is valid");
        let configuration = Configuration::new_with_workspace(
            vec![festerm_config::Profile::ssh(
                "production",
                "ssh.example.test",
                2200,
                "deploy",
                "xterm-256color",
                80,
                24,
            )
            .expect("SSH profile is valid")],
            workspace,
        )
        .expect("configuration is valid");

        let app = FesTermApp::with_configuration(&egui::Context::default(), configuration);

        assert!(app.primary_tab.is_none());
        assert_eq!(app.state.tabs().len(), 2);
        assert!(matches!(app.state.tabs()[0].content, TabContent::Launcher));
        assert!(matches!(
            app.state.tabs()[1].content,
            TabContent::SshAuthenticationRequired(_)
        ));
        assert_eq!(app.state.active(), app.state.tabs()[1].id);
    }

    #[test]
    fn default_configuration_keeps_the_primary_local_session_startup() {
        let context = egui::Context::default();
        let (mut state, primary_tab) =
            AppState::with_primary_session(&context, None, Configuration::empty());

        assert_eq!(state.tabs().len(), 1);
        assert_eq!(state.active(), primary_tab);
        assert!(matches!(state.active_tab().content, TabContent::Session(_)));
        state.dispatch(AppCommand::CloseTab(primary_tab), &context);
    }

    #[test]
    fn successful_workspace_save_replaces_configuration_after_writing() {
        let configuration = Configuration::new(vec![festerm_config::Profile::local(
            "development",
            "sh",
            Vec::new(),
            None,
        )
        .unwrap()])
        .unwrap();
        let mut app = FesTermApp::for_test_with_configuration(configuration.clone());
        let directory = std::env::current_dir().unwrap().join(format!(
            ".festerm-app-workspace-save-{}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("config.toml");
        app.configuration_reloader = ConfigurationReloader::from_path_for_test(path.clone());

        app.save_workspace();

        assert_eq!(
            app.configuration_status,
            ConfigurationStartupStatus::WorkspaceSaved
        );
        assert_eq!(
            app.state.configuration().profiles(),
            configuration.profiles()
        );
        assert!(app.state.configuration().workspace_enabled());
        assert_eq!(
            Configuration::load_from_path(&path).unwrap(),
            *app.state.configuration()
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_workspace_save_retains_configuration_without_path_or_content_leakage() {
        let configuration = Configuration::empty();
        let mut app = FesTermApp::for_test_with_configuration(configuration.clone());
        let directory = std::env::current_dir().unwrap().join(format!(
            ".festerm-app-workspace-save-failure-{}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        app.configuration_reloader = ConfigurationReloader::from_path_for_test(directory.clone());

        app.save_workspace();

        let diagnostic = app.configuration_status.settings_message();
        assert!(matches!(
            app.configuration_status,
            ConfigurationStartupStatus::WorkspaceSaveFailure(_)
        ));
        assert_eq!(app.state.configuration(), &configuration);
        assert!(!diagnostic.contains(directory.to_string_lossy().as_ref()));
        assert!(!diagnostic.contains("schema_version"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn platform_new_tab_shortcut_opens_a_new_launcher_tab_end_to_end() {
        let mut harness = harness();
        harness.run();
        let before = harness.state().state.tabs().len();

        harness.key_press_modifiers(tab_management_modifiers(), egui::Key::T);
        harness.run();

        assert_eq!(harness.state().state.tabs().len(), before + 1);
    }

    #[test]
    fn plain_control_t_and_w_are_not_tab_management_shortcuts() {
        let mut harness = harness();
        harness.run();
        let before = harness.state().state.tabs().len();

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::T);
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::W);
        harness.run();

        assert_eq!(harness.state().state.tabs().len(), before);
    }

    #[test]
    fn platform_close_tab_shortcut_closes_the_active_tab_end_to_end() {
        let mut harness = harness();
        harness.run();
        harness.key_press_modifiers(tab_management_modifiers(), egui::Key::T);
        harness.run();
        let before = harness.state().state.tabs().len();

        harness.key_press_modifiers(tab_management_modifiers(), egui::Key::W);
        harness.run();

        assert_eq!(harness.state().state.tabs().len(), before - 1);
    }

    #[test]
    fn pressing_enter_on_the_launcher_starts_the_highlighted_option_end_to_end() {
        let mut harness = harness();
        harness.run();
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Launcher
        ));

        harness.key_press(egui::Key::Enter);
        // A freshly started local shell session keeps requesting repaints
        // as it pumps live process output, so `run()` (which loops to
        // quiescence) can never stabilize here; a single `step()` is enough
        // to apply the dispatched command and observe the tab-content
        // change.
        harness.step();

        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Session(_)
        ));
    }

    #[test]
    fn configured_local_profile_launcher_action_dispatches_end_to_end() {
        let configuration = Configuration::new(vec![festerm_config::Profile::local(
            "development",
            "festerm-profile-test-command-that-does-not-exist",
            Vec::new(),
            None,
        )
        .expect("test local profile is valid")])
        .expect("test configuration is valid");
        let mut harness = harness_with_configuration(configuration);
        harness.run();

        harness.get_by_label("development (Local profile)").click();
        harness.step();

        let TabContent::Session(session) = &harness.state().state.active_tab().content else {
            panic!("the configured profile launcher action must start a session tab");
        };
        assert_eq!(session.label, "development");
    }

    #[test]
    fn clicking_a_chip_close_button_closes_that_tab_end_to_end() {
        let mut harness = harness();
        harness.run();
        // Open a second tab so there is one to close without emptying root.
        harness.key_press_modifiers(tab_management_modifiers(), egui::Key::T);
        harness.run();
        let before = harness.state().state.tabs().len();

        harness
            .get_all_by_label("Close")
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
