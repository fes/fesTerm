//! iOS feasibility host: offline by default, with opt-in transient live SSH.

#[cfg_attr(not(feature = "live-ssh"), allow(dead_code))] // SFTP needs the SSH transport
mod files;
#[cfg(test)]
mod files_fixture;
#[cfg_attr(not(feature = "live-ssh"), allow(dead_code))] // SFTP needs the SSH transport
mod files_shell;
#[cfg_attr(not(feature = "live-ssh"), allow(dead_code))] // SFTP needs the SSH transport
mod files_ui;
mod forms;
#[cfg(feature = "live-ssh")]
mod live_ssh;
#[allow(dead_code)] // design-system surface used by the gallery before production wiring
mod model;
mod password_buffer;
mod profiles;
#[cfg(feature = "live-ssh")]
mod sftp_link;
mod shell;
mod tabs;
#[allow(dead_code)] // design-system surface used by the gallery before production wiring
mod terminal_chrome;
mod terminal_gesture;
#[allow(dead_code)] // design-system surface used by the gallery before production wiring
mod theme;
#[allow(dead_code)] // design-system surface used by the gallery before production wiring
mod widgets;
mod workspaces;

#[cfg(all(test, feature = "live-ssh"))]
mod ui_gallery;

use eframe::egui;
use festerm_core::{Dimensions, InputEvent, Key, Modifiers, Terminal};
use festerm_ui_egui::{
    route_input, EncodedInputSink, InputRoute, InputSinkDiagnostics, TerminalView,
    TerminalViewOptions,
};
#[cfg(feature = "live-ssh")]
use std::cell::RefCell;
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

fn mobile_style(style: &egui::Style) -> egui::Style {
    theme::apply(style)
}

pub use profiles::Profiles;

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

/// Retires transport and sensitive prompts on the native thread without a redraw.
#[cfg(feature = "live-ssh")]
#[derive(Clone, Default)]
pub struct SuspensionHandle {
    connection: Rc<RefCell<Option<live_ssh::LiveSsh>>>,
    /// Sessions of parked tabs, keyed by tab id.
    background: Rc<RefCell<Vec<(u64, live_ssh::LiveSsh)>>>,
}

#[cfg(feature = "live-ssh")]
const SUSPENDED: &str =
    "Suspended. Reconnect to continue; a plain shell does not survive, tmux/Screen sessions do.";

#[cfg(feature = "live-ssh")]
impl SuspensionHandle {
    pub fn suspend(&self) {
        if let Some(live) = self.connection.borrow_mut().as_mut() {
            live.stop(SUSPENDED);
        }
        for (_, live) in self.background.borrow_mut().iter_mut() {
            live.stop(SUSPENDED);
        }
    }
}

/// Deliberately discards bytes: the spike must not pretend to be a shell or
/// save what a tester types. Uses the renderer's existing input contract.
#[derive(Default)]
pub struct ProbeSink {
    diagnostics: InputSinkDiagnostics,
    #[cfg(feature = "live-ssh")]
    live: SuspensionHandle,
}

impl EncodedInputSink for ProbeSink {
    fn record_encoded_input(&mut self, bytes: &[u8]) {
        #[cfg(feature = "live-ssh")]
        if let Some(live) = self.live.connection.borrow_mut().as_mut() {
            live.send(bytes);
        }
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

    fn record_terminal_resize(&mut self, dimensions: Dimensions) {
        #[cfg(feature = "live-ssh")]
        if let Some(live) = self.live.connection.borrow_mut().as_mut() {
            live.resize(dimensions);
        }
        #[cfg(not(feature = "live-ssh"))]
        let _ = dimensions;
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Surface {
    #[default]
    Tabs,
    Terminal,
}

/// Content-free view of the live SSH worker for chrome decisions.
#[cfg(feature = "live-ssh")]
struct LiveView {
    can_reset: bool,
    input_enabled: bool,
    status: &'static str,
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
    #[cfg(test)]
    control_rect: Option<egui::Rect>,
    menu_open: bool,
    gestures: terminal_gesture::TerminalGesture,
    workspace: workspaces::Workspace,
    files: Option<Box<files::FilesSession>>,
    files_ctx: files_shell::FilesContext,
    markdown: Option<workspaces::MarkdownWorkspace>,
    surface: Surface,
    shell: shell::ShellState,
    profiles: Option<profiles::Profiles>,
    caps: model::Capabilities,
    tabs: Vec<tabs::SessionTab>,
    active_tab: Option<u64>,
    next_tab: u64,
    /// One-line outcome of the last action, shown on the Profiles screen.
    notice: Option<String>,
    editor: forms::EditorState,
    keys_panel: terminal_chrome::KeysPanel,
    keyboard_visible: bool,
    /// Gallery/test fixtures; production derives these from live state.
    sessions_override: Option<model::StaticSessions>,
    chrome_override: Option<model::ChromeModel>,
    #[cfg(test)]
    rows_override: Option<Vec<model::Profile>>,
    #[cfg(feature = "live-ssh")]
    polled_frame: Option<u64>,
}

/// Phone-density default (about 46 columns on a 390pt screen); pinch-zoom
/// still moves it within the view's bounds.
const MOBILE_TERMINAL_FONT_SIZE: f32 = 11.5;

fn mobile_terminal_view() -> TerminalView {
    let mut view = TerminalView::default();
    view.zoom_by_factor(MOBILE_TERMINAL_FONT_SIZE / view.font_size_points());
    view
}

impl MobileApp {
    pub fn new(lifecycle: Rc<Cell<Lifecycle>>) -> Self {
        Self {
            reported_first_ui: false,
            terminal: fixture_terminal(),
            view: mobile_terminal_view(),
            sink: ProbeSink::default(),
            lifecycle,
            observed_memory_warnings: 0,
            observed_suspensions: 0,
            keyboard: None,
            control: false,
            alt: false,
            toolbar_rect: None,
            #[cfg(test)]
            control_rect: None,
            menu_open: false,
            gestures: terminal_gesture::TerminalGesture::default(),
            workspace: workspaces::Workspace::Terminal,
            files: None,
            files_ctx: files_shell::FilesContext::default(),
            markdown: None,
            surface: Surface::Tabs,
            shell: shell::ShellState::default(),
            profiles: None,
            caps: model::Capabilities::production(),
            tabs: Vec::new(),
            active_tab: None,
            next_tab: 1,
            notice: None,
            editor: forms::EditorState::default(),
            keys_panel: terminal_chrome::KeysPanel::default(),
            keyboard_visible: true,
            sessions_override: None,
            chrome_override: None,
            #[cfg(test)]
            rows_override: None,
            #[cfg(feature = "live-ssh")]
            polled_frame: None,
        }
    }

    /// Starts on the terminal surface, as the pre-shell spike did.
    #[cfg(test)]
    fn new_terminal(lifecycle: Rc<Cell<Lifecycle>>) -> Self {
        let mut app = Self::new(lifecycle);
        app.surface = Surface::Terminal;
        app.open_local_tab();
        app
    }

    /// Plugs in the saved-profile store (Keychain-backed in production).
    pub fn with_profiles(mut self, profiles: profiles::Profiles) -> Self {
        self.profiles = Some(profiles);
        self
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let occlusion = if self.surface == Surface::Terminal
            && self.workspace != workspaces::Workspace::Terminal
            && !self.editor.open
        {
            0.0
        } else {
            self.keyboard
                .as_ref()
                .map_or(0.0, |keyboard| keyboard.occluded_height_fraction())
        };
        self.show_with_occlusion(ui, occlusion);
    }

    pub fn with_keyboard(mut self, keyboard: Option<festerm_ios_window::KeyboardBridge>) -> Self {
        self.keyboard = keyboard;
        self
    }

    #[cfg(feature = "live-ssh")]
    pub fn with_suspension(mut self, suspension: SuspensionHandle) -> Self {
        self.sink.live = suspension;
        self
    }

    fn terminal_surface(&self) -> bool {
        self.surface == Surface::Terminal
            && self.workspace == workspaces::Workspace::Terminal
            && !self.editor.open
    }

    fn show_with_occlusion(&mut self, ui: &mut egui::Ui, occlusion: f32) {
        // Accessory taps must not blur the terminal and restart UIKit's keyboard.
        let terminal_surface = self.terminal_surface();
        ui.ctx().options_mut(|options| {
            options.input_options.surrender_focus_on = if terminal_surface {
                egui::SurrenderFocusOn::Never
            } else {
                egui::SurrenderFocusOn::Clicks
            };
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
            ui.set_style(mobile_style(ui.style()));
            ui.painter()
                .rect_filled(safe_rect, 0.0, ui.visuals().panel_fill);
            self.show_content(ui);
        });
    }

    fn send_key(&mut self, key: Key) {
        self.send_key_with(key, Modifiers::NONE);
    }

    /// Sticky Ctrl/Alt apply to the next key whichever surface produced it.
    fn send_key_with(&mut self, key: Key, extra: Modifiers) {
        let mut modifiers = extra;
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

    #[cfg(feature = "live-ssh")]
    fn live_view(&self) -> Option<LiveView> {
        self.sink
            .live
            .connection
            .borrow()
            .as_ref()
            .map(|live| LiveView {
                can_reset: live.can_reset(),
                input_enabled: live.input_enabled(),
                status: live.status(),
            })
    }

    fn chrome(&self) -> model::ChromeModel {
        use model::{ChromeModel, ConnectionLine, SurfaceState};
        if let Some(chrome) = &self.chrome_override {
            return chrome.clone();
        }
        if let Some(spec) = self.active_spec() {
            #[cfg_attr(not(feature = "live-ssh"), allow(unused_mut))]
            let mut state = SurfaceState::Disconnected;
            #[cfg_attr(not(feature = "live-ssh"), allow(unused_mut))]
            let mut label = "Not connected".to_owned();
            #[cfg(feature = "live-ssh")]
            if let Some(live) = self.live_view() {
                state = if live.can_reset {
                    SurfaceState::Disconnected
                } else if live.input_enabled {
                    SurfaceState::Connected
                } else {
                    SurfaceState::Connecting
                };
                label = live.status.to_owned();
            }
            return ChromeModel {
                title: spec.name.clone(),
                subtitle: spec.detail(),
                line: ConnectionLine {
                    state,
                    label,
                    context: spec.persistent_label(),
                },
            };
        }
        ChromeModel {
            title: "Offline demo".to_owned(),
            subtitle: "Local fixture · no shell".to_owned(),
            line: ConnectionLine {
                state: SurfaceState::Disconnected,
                label: "Offline demo · input is discarded".to_owned(),
                context: None,
            },
        }
    }

    fn surfaces(&self) -> Vec<model::SurfaceSummary> {
        if let Some(sessions) = &self.sessions_override {
            return model::SessionSource::surfaces(sessions);
        }
        self.tab_surfaces()
    }

    fn reattachable(&self) -> Vec<model::Reattachable> {
        if let Some(sessions) = &self.sessions_override {
            return model::SessionSource::reattachable(sessions);
        }
        self.reattach_targets()
            .into_iter()
            .map(|target| target.row)
            .collect()
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let lifecycle = self.lifecycle.get();
        vec![
            ("Resumes".to_owned(), lifecycle.resumes.to_string()),
            ("Suspensions".to_owned(), lifecycle.suspensions.to_string()),
            (
                "Memory warnings".to_owned(),
                lifecycle.memory_warnings.to_string(),
            ),
            (
                "Input bytes".to_owned(),
                self.sink.diagnostics.byte_count.to_string(),
            ),
        ]
    }

    fn show_content(&mut self, ui: &mut egui::Ui) {
        let lifecycle = self.lifecycle.get();
        if lifecycle.suspensions != self.observed_suspensions {
            #[cfg(feature = "live-ssh")]
            self.sink.live.suspend();
            self.gestures.cancel();
            self.control = false;
            self.alt = false;
            self.observed_suspensions = lifecycle.suspensions;
        }
        if lifecycle.memory_warnings != self.observed_memory_warnings {
            self.gestures.cancel();
            // Preserve grid/history and discard only reconstructible view caches.
            let font_size = self.view.font_size_points();
            self.view = mobile_terminal_view();
            self.view
                .zoom_by_factor(font_size / self.view.font_size_points());
            self.observed_memory_warnings = lifecycle.memory_warnings;
        }
        #[cfg(feature = "live-ssh")]
        if lifecycle.active && self.polled_frame != Some(ui.ctx().cumulative_frame_nr()) {
            self.polled_frame = Some(ui.ctx().cumulative_frame_nr());
            if let Some(live) = self.sink.live.connection.borrow_mut().as_mut() {
                live.poll(&mut self.terminal, ui.ctx());
            }
            self.persist_new_trust();
            self.poll_background(ui.ctx());
        }
        #[cfg(feature = "live-ssh")]
        if lifecycle.active {
            self.poll_files(ui.ctx());
        }
        let bounds = ui.max_rect();
        if self.editor.open {
            self.show_editor(ui);
            return;
        }
        match self.surface {
            Surface::Tabs => self.show_tabs(ui),
            Surface::Terminal => self.show_surface(ui, lifecycle),
        }
        self.show_sheets(ui.ctx(), bounds);
        self.show_files_sheets(ui.ctx(), bounds);
    }

    #[allow(clippy::too_many_arguments)]
    fn shell_data<'a>(
        &self,
        profiles: &'a [model::Profile],
        recents: &'a [model::Profile],
        surfaces: &'a [model::SurfaceSummary],
        reattachable: &'a [model::Reattachable],
        diagnostics: &'a [(String, String)],
        trusted_hosts: &'a [profiles::TrustedHostRow],
        notice: Option<&'a str>,
    ) -> shell::ShellData<'a> {
        shell::ShellData {
            profiles,
            recents,
            surfaces,
            reattachable,
            profiles_persistent: self
                .profiles
                .as_ref()
                .is_some_and(profiles::Profiles::durable_credentials),
            trusted_hosts,
            diagnostics,
            notice,
        }
    }

    fn show_tabs(&mut self, ui: &mut egui::Ui) {
        let (rows, recents, trusted) = match &self.profiles {
            Some(profiles) => (
                profiles.rows(self.caps),
                profiles.recents(self.caps, 2),
                profiles.trusted_hosts(),
            ),
            None => Default::default(),
        };
        #[cfg(test)]
        let (rows, recents) = match &self.rows_override {
            Some(rows) => (rows.clone(), rows.iter().take(2).cloned().collect()),
            None => (rows, recents),
        };
        let surfaces = self.surfaces();
        let reattachable = self.reattachable();
        let diagnostics = self.diagnostics();
        let notice = self.notice.clone();
        let data = self.shell_data(
            &rows,
            &recents,
            &surfaces,
            &reattachable,
            &diagnostics,
            &trusted,
            notice.as_deref(),
        );
        let actions = shell::show_tabs(ui, &mut self.shell, &data);
        self.apply_shell_actions(ui.ctx(), actions);
    }

    fn apply_shell_actions(&mut self, ctx: &egui::Context, actions: Vec<shell::ShellAction>) {
        use shell::ShellAction;
        for action in actions {
            match action {
                ShellAction::NewProfile => {
                    self.notice = None;
                    self.editor.open_new();
                    self.editor.durable_credentials = self.credentials_durable();
                }
                ShellAction::EditProfile(id) => self.edit_profile(&id),
                ShellAction::OpenProfile(id) => self.connect_profile(ctx, &id),
                ShellAction::ForgetHost(index) => {
                    if let Some(profiles) = self.profiles.as_mut() {
                        let hosts = profiles.trusted_hosts();
                        if let Some(host) = hosts.get(index) {
                            if let Err(error) = profiles.forget_host(&host.host, host.port) {
                                self.notice = Some(error);
                            }
                        }
                    }
                }
                ShellAction::OpenSurface(id) => {
                    self.shell.sessions_sheet = false;
                    self.activate_tab(id);
                    if self.active_tab == Some(id) {
                        self.surface = Surface::Terminal;
                    }
                }
                ShellAction::Reattach(index) => {
                    self.shell.sessions_sheet = false;
                    if let Some(target) = self.reattach_targets().into_iter().nth(index) {
                        self.connect_profile(ctx, &target.profile);
                    }
                }
                ShellAction::OpenDemo => {
                    self.open_local_tab();
                    self.shell.sessions_sheet = false;
                    self.surface = Surface::Terminal;
                }
                ShellAction::ShowSessions => self.shell.sessions_sheet = true,
                ShellAction::HideSessions => self.shell.sessions_sheet = false,
                ShellAction::OpenProfilesTab => {
                    self.shell.sessions_sheet = false;
                    self.shell.tab = shell::Tab::Profiles;
                    self.leave_terminal();
                }
            }
        }
    }

    fn credentials_durable(&self) -> bool {
        self.profiles
            .as_ref()
            .is_some_and(profiles::Profiles::durable_credentials)
    }

    fn edit_profile(&mut self, id: &str) {
        if let Some(spec) = self
            .profiles
            .as_ref()
            .and_then(|profiles| profiles.sftp_spec(id))
        {
            self.notice = None;
            self.editor.open_edit_sftp(id, &spec, self.caps);
            self.editor.durable_credentials = self.credentials_durable();
            return;
        }
        let Some((name, ssh)) = self
            .profiles
            .as_ref()
            .and_then(|profiles| profiles.ssh_profile(id))
        else {
            return;
        };
        self.notice = None;
        self.editor.open_edit(id, &name, &ssh);
        self.editor.durable_credentials = self.credentials_durable();
    }

    fn connect_profile(&mut self, ctx: &egui::Context, id: &str) {
        if let Some(spec) = self
            .profiles
            .as_ref()
            .and_then(|profiles| profiles.sftp_spec(id))
        {
            self.notice = None;
            self.open_files(spec, true, ctx);
            return;
        }
        #[cfg(feature = "live-ssh")]
        {
            self.notice = None;
            let open = self.tabs.iter().find(|tab| {
                tab.spec
                    .as_ref()
                    .and_then(|spec| spec.profile.as_ref())
                    .is_some_and(|profile| profile.as_str() == id)
            });
            let open = open.map(|tab| tab.id).filter(|open| {
                self.tab_surfaces().iter().any(|surface| {
                    surface.id == *open && surface.state != model::SurfaceState::Disconnected
                })
            });
            if let Some(open) = open {
                self.activate_tab(open);
                self.surface = Surface::Terminal;
                return;
            }
            match self.open_profile(id, ctx) {
                Ok(()) => self.surface = Surface::Terminal,
                Err(error) => self.notice = Some(error),
            }
        }
        #[cfg(not(feature = "live-ssh"))]
        {
            let _ = (ctx, id);
            self.notice = Some("SSH is not available in this build".to_owned());
        }
    }

    /// Persists a host key the user just trusted so later connections pin it.
    #[cfg(feature = "live-ssh")]
    fn persist_new_trust(&mut self) {
        let trusted = self
            .sink
            .live
            .connection
            .borrow_mut()
            .as_mut()
            .and_then(live_ssh::LiveSsh::take_new_trust);
        if let (Some((identity, fingerprint)), Some(profiles)) = (trusted, self.profiles.as_mut()) {
            if let Err(error) = profiles.trust_host(&identity, &fingerprint) {
                self.notice = Some(format!("Could not save the host key: {error}"));
            }
        }
    }

    fn leave_terminal(&mut self) {
        if self.surface == Surface::Terminal {
            self.gestures.cancel();
            self.control = false;
            self.alt = false;
            self.keys_panel.open = false;
            if self.workspace == workspaces::Workspace::Terminal {
                if let Some(focus) = self.view.relinquish_transient_input() {
                    route_input(&mut self.terminal, InputEvent::Focus(focus), &mut self.sink);
                }
            }
        }
        self.surface = Surface::Tabs;
    }

    fn show_sheets(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        if self.shell.sessions_sheet {
            let surfaces = self.surfaces();
            let reattachable = self.reattachable();
            let (profiles, diagnostics, trusted) = (Vec::new(), Vec::new(), Vec::new());
            let data = self.shell_data(
                &profiles,
                &profiles,
                &surfaces,
                &reattachable,
                &diagnostics,
                &trusted,
                None,
            );
            let actions = shell::sessions_sheet(ctx, bounds, &data);
            self.apply_shell_actions(ctx, actions);
        }
        #[cfg(feature = "live-ssh")]
        if let Some(live) = self.sink.live.connection.borrow_mut().as_mut() {
            if let Some(model) = live.host_verification() {
                if let Some(decision) = forms::host_verification_sheet(ctx, bounds, &model) {
                    live.resolve_host(decision);
                }
            } else if let Some((target, retry)) = live.password_request() {
                match forms::password_sheet(ctx, bounds, &target, retry, live.password_buffer()) {
                    forms::PasswordDecision::Submit => live.submit_password(),
                    forms::PasswordDecision::Cancel => {
                        live.stop("Disconnected. Connect again for a fresh shell.")
                    }
                    forms::PasswordDecision::None => {}
                }
            }
        }
    }

    fn show_editor(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let can_connect = cfg!(feature = "live-ssh") && self.lifecycle.get().active;
        match forms::show_editor(ui, &mut self.editor, self.caps, can_connect) {
            forms::EditorAction::None => {}
            forms::EditorAction::Cancel => self.close_editor(),
            forms::EditorAction::OpenAuthentication => self.editor.auth_sheet = true,
            forms::EditorAction::SaveOnly => {
                if self.save_editor_profile().is_some() {
                    self.editor.close();
                }
            }
            forms::EditorAction::SaveAndConnect => {
                if let Some(id) = self.save_editor_profile() {
                    self.editor.close();
                    self.connect_profile(&ctx, &id);
                }
            }
            forms::EditorAction::Delete => self.delete_editor_profile(),
        }
        if self.editor.open && self.editor.auth_sheet {
            let bounds = ui.max_rect();
            match forms::authentication_sheet(&ctx, bounds, &mut self.editor, true) {
                forms::AuthAction::None => {}
                forms::AuthAction::Done => self.finish_authentication(),
                forms::AuthAction::PasteKey => self.import_key(None),
                forms::AuthAction::ImportPending => {
                    let passphrase = std::mem::take(&mut self.editor.key_passphrase);
                    self.import_key(Some(passphrase));
                }
                forms::AuthAction::GenerateKey => self.generate_key(),
                forms::AuthAction::CopyPublicKey => {
                    if let Some(festerm_mobile_profiles::SshAuth::Key { key, .. }) =
                        &self.editor.auth
                    {
                        festerm_ios_window::set_clipboard_text(&key.public_key);
                        self.editor.auth_notice = Some("Public key copied".to_owned());
                    }
                }
            }
        }
    }

    fn close_editor(&mut self) {
        self.discard_unsaved_credential();
        self.editor.close();
    }

    fn discard_unsaved_credential(&mut self) {
        if let (Some(profiles), Some(auth)) = (&self.profiles, self.editor.unsaved_auth()) {
            profiles.discard_credential(auth);
        }
    }

    /// Replaces the credential chosen in the editor, dropping a stored but
    /// never-saved predecessor.
    fn replace_editor_credential(&mut self, auth: festerm_mobile_profiles::SshAuth) {
        self.discard_unsaved_credential();
        self.editor.auth = Some(auth);
    }

    fn import_key(&mut self, passphrase: Option<String>) {
        use profiles::KeyImportFailure;
        let Some(store) = self.profiles.as_ref() else {
            self.editor.auth_notice = Some("Saved profiles are unavailable".to_owned());
            return;
        };
        let text = match passphrase {
            Some(_) => self.editor.pending_key.clone(),
            None => festerm_ios_window::clipboard_text().unwrap_or_default(),
        };
        if text.trim().is_empty() {
            self.editor.auth_notice = Some("The clipboard has no private key to import".to_owned());
            return;
        }
        match store.import_key(&text, passphrase.as_deref().filter(|p| !p.is_empty())) {
            Ok(auth) => {
                self.editor.pending_key.clear();
                self.editor.needs_passphrase = false;
                self.editor.auth_notice = Some("Key imported".to_owned());
                self.replace_editor_credential(auth);
            }
            Err(KeyImportFailure::PassphraseRequired) => {
                self.editor.pending_key = text;
                self.editor.needs_passphrase = true;
                self.editor.auth_notice = Some(
                    if passphrase.is_some() {
                        "That passphrase did not unlock the key"
                    } else {
                        "This key is encrypted; enter its passphrase"
                    }
                    .to_owned(),
                );
            }
            Err(KeyImportFailure::Message(message)) => {
                self.editor.needs_passphrase = false;
                self.editor.auth_notice = Some(message);
            }
        }
    }

    fn generate_key(&mut self) {
        let Some(store) = self.profiles.as_ref() else {
            self.editor.auth_notice = Some("Saved profiles are unavailable".to_owned());
            return;
        };
        match store.generate_key("fesTerm iPhone") {
            Ok(auth) => {
                self.editor.auth_notice = Some(
                    "Key generated. Copy the public key into the server's authorized_keys."
                        .to_owned(),
                );
                self.replace_editor_credential(auth);
            }
            Err(error) => self.editor.auth_notice = Some(error),
        }
    }

    /// Closes the Authentication sheet, storing a typed password first.
    fn finish_authentication(&mut self) {
        if self.editor.auth_mode == forms::AuthMode::Password && !self.editor.password.is_empty() {
            let password = std::mem::take(&mut self.editor.password);
            match self
                .profiles
                .as_ref()
                .map(|store| store.store_password(password))
            {
                Some(Ok(auth)) => self.replace_editor_credential(auth),
                Some(Err(error)) => {
                    self.editor.auth_notice = Some(error);
                    return;
                }
                None => {
                    self.editor.auth_notice = Some("Saved profiles are unavailable".to_owned());
                    return;
                }
            }
        }
        self.editor.wipe_secrets();
        self.editor.auth_notice = None;
        self.editor.auth_sheet = false;
    }

    /// Validates the editor and saves it; returns the profile id.
    fn save_editor_profile(&mut self) -> Option<String> {
        use festerm_mobile_profiles::{Recovery, SshAuth, SshProfile};
        if self.editor.auth_mode == forms::AuthMode::Password && !self.editor.password.is_empty() {
            self.finish_authentication();
            if self.editor.auth_sheet {
                return None;
            }
        }
        let host = self.editor.host.trim().to_owned();
        if host.is_empty() {
            self.editor.error = Some("Enter a host to save this profile".to_owned());
            return None;
        }
        let Ok(port) = self.editor.port.trim().parse::<u16>() else {
            self.editor.error = Some("Enter a valid port (1-65535)".to_owned());
            return None;
        };
        let username = self.editor.username.trim().to_owned();
        if username.is_empty() {
            self.editor.error = Some("Enter a username".to_owned());
            return None;
        }
        let auth = match (self.editor.auth_mode, self.editor.auth.clone()) {
            (forms::AuthMode::Ask, _) => SshAuth::Ask,
            (forms::AuthMode::Password, Some(auth @ SshAuth::Password { .. })) => auth,
            (forms::AuthMode::Password, _) => {
                self.editor.error = Some("Set a password, or choose Ask each time".to_owned());
                return None;
            }
            (forms::AuthMode::Key, Some(auth @ SshAuth::Key { .. })) => auth,
            (forms::AuthMode::Key, _) => {
                self.editor.error = Some("Import or generate an SSH key first".to_owned());
                return None;
            }
        };
        let name = self.editor.name.trim().to_owned();
        let Some(profiles) = self.profiles.as_mut() else {
            self.editor.error = Some("Saved profiles are unavailable".to_owned());
            return None;
        };
        let name = if name.is_empty() {
            profiles.suggest_name(&host)
        } else {
            name
        };
        if self.editor.kind(self.caps) == model::ProfileKind::Sftp {
            let start_dir = self.editor.start_dir.trim();
            let sftp = festerm_mobile_profiles::SftpProfile {
                host,
                port,
                username,
                auth: auth.clone(),
                start_dir: (!start_dir.is_empty()).then(|| start_dir.to_owned()),
            };
            let result = profiles.save_sftp(self.editor.editing.as_deref(), &name, sftp);
            return self.finish_profile_save(result, &auth);
        }
        let ssh = SshProfile {
            host,
            port,
            username,
            auth: auth.clone(),
            remote_session: self.editor.remote_session(),
            recovery: Recovery::default(),
        };
        let result = profiles.save_ssh(self.editor.editing.as_deref(), &name, ssh);
        self.finish_profile_save(result, &auth)
    }

    fn finish_profile_save(
        &mut self,
        result: Result<String, String>,
        auth: &festerm_mobile_profiles::SshAuth,
    ) -> Option<String> {
        match result {
            Ok(id) => {
                if let (Some(original), Some(profiles)) =
                    (&self.editor.original_auth, self.profiles.as_ref())
                {
                    if original != auth {
                        profiles.discard_credential(original);
                    }
                }
                self.editor.error = None;
                Some(id)
            }
            Err(error) => {
                self.editor.error = Some(error);
                None
            }
        }
    }

    fn delete_editor_profile(&mut self) {
        let (Some(id), Some(profiles)) = (self.editor.editing.clone(), self.profiles.as_mut())
        else {
            return;
        };
        match profiles.delete(&id) {
            Ok(()) => self.editor.close(),
            Err(error) => self.editor.error = Some(error),
        }
    }

    fn switch_workspace(&mut self, workspace: workspaces::Workspace) {
        if self.workspace == workspace {
            return;
        }
        self.gestures.cancel();
        self.control = false;
        self.alt = false;
        if self.workspace == workspaces::Workspace::Terminal {
            if let Some(focus) = self.view.relinquish_transient_input() {
                route_input(&mut self.terminal, InputEvent::Focus(focus), &mut self.sink);
            }
        }
        self.workspace = workspace;
    }

    fn reset_fixture(&mut self) {
        self.switch_workspace(workspaces::Workspace::Terminal);
        self.gestures.cancel();
        if self.active_is_local() {
            self.terminal = fixture_terminal();
            self.view = self.fresh_view();
        } else {
            self.open_local_tab();
        }
        self.sink.diagnostics = InputSinkDiagnostics::default();
        self.control = false;
        self.alt = false;
        self.editor.close();
    }

    fn show_surface(&mut self, ui: &mut egui::Ui, lifecycle: Lifecycle) {
        #[cfg(feature = "live-ssh")]
        let live = self.live_view();
        #[cfg(feature = "live-ssh")]
        let session_active = live.as_ref().is_some_and(|live| !live.can_reset);
        #[cfg(not(feature = "live-ssh"))]
        let session_active = false;
        let in_terminal = self.workspace == workspaces::Workspace::Terminal;
        let chrome = self.chrome();
        let (title, subtitle) = if in_terminal {
            (chrome.title.clone(), chrome.subtitle.clone())
        } else {
            self.files_heading()
        };
        let can_open_files = in_terminal && self.active_spec().is_some();
        let mut back = false;
        let mut files = false;
        let mut close_tab = None;
        egui::Panel::top("mobile-header")
            .frame(egui::Frame::new().fill(theme::color::BG))
            .show_separator_line(false)
            .show(ui, |ui| {
                let header = terminal_chrome::header(ui, &title, &subtitle, can_open_files);
                back = header.back;
                files = header.files;
                let menu = egui::Popup::menu(&header.more).show(|ui| {
                    if session_active && ui.button("Disconnect").clicked() {
                        #[cfg(feature = "live-ssh")]
                        if let Some(live) = self.sink.live.connection.borrow_mut().as_mut() {
                            live.stop("Disconnected. Reconnect to continue.");
                        }
                        ui.close();
                    }
                    if self.active_is_local()
                        && ui
                            .add_enabled(lifecycle.active, egui::Button::new("Reset demo"))
                            .clicked()
                    {
                        self.reset_fixture();
                        ui.close();
                    }
                    if let Some(id) = self.active_tab {
                        if ui.button("Close session").clicked() {
                            close_tab = Some(id);
                            ui.close();
                        }
                    }
                    ui.separator();
                    for (name, value) in self.diagnostics() {
                        ui.weak(format!("{name}: {value}"));
                    }
                });
                self.menu_open = menu.is_some();
            });
        if let Some(id) = close_tab {
            self.close_tab(id);
            return;
        }
        if back {
            match self.workspace {
                workspaces::Workspace::Terminal => self.leave_terminal(),
                workspaces::Workspace::Markdown => {
                    self.markdown = None;
                    self.switch_workspace(workspaces::Workspace::Files);
                }
                workspaces::Workspace::Files if self.files_ctx.only => {
                    self.close_files();
                    self.switch_workspace(workspaces::Workspace::Terminal);
                    self.surface = Surface::Tabs;
                }
                workspaces::Workspace::Files => {
                    self.switch_workspace(workspaces::Workspace::Terminal)
                }
            }
            return;
        }
        if files && can_open_files {
            let ctx = ui.ctx().clone();
            self.open_files_for_active(&ctx);
            return;
        }
        if !in_terminal {
            egui::Panel::top("mobile-status")
                .frame(egui::Frame::new().fill(theme::color::BG))
                .show_separator_line(false)
                .show(ui, |_| {});
            match (self.workspace, self.markdown.as_mut()) {
                (workspaces::Workspace::Markdown, Some(markdown)) => {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(theme::space::LG as i8, 0))
                        .show(ui, |ui| markdown.show(ui));
                }
                _ => self.show_files_body(ui),
            }
            return;
        }
        // Short landscape space keeps only what typing needs.
        let compact = ui.available_height() < 340.0;
        if !compact {
            egui::Panel::top("mobile-status")
                .frame(egui::Frame::new().fill(theme::color::BG))
                .show_separator_line(false)
                .show(ui, |ui| terminal_chrome::status_line(ui, &chrome.line));
        }
        self.show_terminal(ui, lifecycle, compact);
    }

    fn show_terminal(&mut self, ui: &mut egui::Ui, lifecycle: Lifecycle, compact: bool) {
        #[cfg(feature = "live-ssh")]
        let input_ready = lifecycle.active
            && !self.shell.sessions_sheet
            && !self.editor.open
            && self
                .sink
                .live
                .connection
                .borrow()
                .as_ref()
                .is_none_or(|live| live.input_enabled());
        #[cfg(not(feature = "live-ssh"))]
        let input_ready = lifecycle.active && !self.editor.open && !self.shell.sessions_sheet;
        #[cfg(feature = "live-ssh")]
        let read_only = self
            .sink
            .live
            .connection
            .borrow()
            .as_ref()
            .is_some_and(|live| live.can_reset());
        #[cfg(not(feature = "live-ssh"))]
        let read_only = false;
        if !input_ready {
            self.control = false;
            self.alt = false;
        }
        let surfaces = self.surfaces();
        let panel_open = self.keys_panel.open && input_ready && !read_only;
        if panel_open {
            let output = egui::Panel::bottom("terminal-keys-panel")
                .frame(egui::Frame::new().fill(theme::color::BAR))
                .show_separator_line(false)
                .show(ui, |ui| {
                    terminal_chrome::keys_panel(ui, &mut self.keys_panel)
                })
                .inner;
            if let Some((key, modifiers)) = output.key {
                self.send_key_with(key, modifiers);
            }
            if output.close {
                self.keys_panel.open = false;
                self.keyboard_visible = true;
            }
        }
        if !read_only {
            let mut row_action = terminal_chrome::KeyRowAction::None;
            let toolbar = egui::Panel::bottom("terminal-keys")
                .frame(egui::Frame::new().fill(theme::color::BAR))
                .show_separator_line(false)
                .show(ui, |ui| {
                    ui.add_enabled_ui(input_ready && !self.menu_open, |ui| {
                        let output = terminal_chrome::key_row(
                            ui,
                            &terminal_chrome::KeyRowState {
                                control: self.control,
                                alt: self.alt,
                                panel_open,
                                keyboard_visible: self.keyboard_visible,
                                paste: self.caps.paste,
                            },
                        );
                        #[cfg(test)]
                        {
                            self.control_rect = Some(output.control_rect);
                        }
                        row_action = output.action;
                    });
                });
            self.toolbar_rect = Some(toolbar.response.rect);
            self.apply_key_row(row_action);
        } else {
            self.toolbar_rect = None;
            egui::Panel::bottom("terminal-keys")
                .frame(egui::Frame::new().fill(theme::color::BAR))
                .show_separator_line(false)
                .show(ui, terminal_chrome::read_only_note);
        }
        let mut chip_action = terminal_chrome::ChipAction::None;
        if !compact {
            egui::Panel::bottom("terminal-chips")
                .frame(egui::Frame::new().fill(theme::color::BAR))
                .show_separator_line(false)
                .show(ui, |ui| {
                    chip_action = terminal_chrome::chip_strip(ui, &surfaces);
                });
        }
        match chip_action {
            terminal_chrome::ChipAction::None => {}
            terminal_chrome::ChipAction::Select(id) => {
                self.activate_tab(id);
                return;
            }
            terminal_chrome::ChipAction::All => self.shell.sessions_sheet = true,
            terminal_chrome::ChipAction::New => {
                self.shell.tab = shell::Tab::Profiles;
                self.leave_terminal();
                return;
            }
        }
        #[cfg(feature = "live-ssh")]
        if read_only {
            let banner = self.reconnect_banner();
            let compact = ui.available_height() < 260.0;
            let mut reconnect = false;
            egui::Panel::bottom("mobile-recovery")
                .frame(egui::Frame::new().fill(theme::color::BAR))
                .show_separator_line(false)
                .show(ui, |ui| {
                    reconnect = terminal_chrome::reconnect_banner(ui, &banner, compact);
                });
            if reconnect {
                if let Err(error) = self.reconnect_active(ui.ctx()) {
                    self.notice = Some(error);
                }
                return;
            }
        }
        let history_ready = lifecycle.active && !self.editor.open && (input_ready || read_only);
        if input_ready && !self.menu_open {
            ui.input_mut(|input| self.consume_sticky_text(&mut input.events));
        }
        let terminal_rect = ui.available_rect_before_wrap();
        let gesture = ui.input_mut(|input| {
            let any_touches = input.any_touches();
            self.gestures.update(
                &mut input.events,
                input.time,
                terminal_rect,
                any_touches,
                input_ready && !self.menu_open && input.focused,
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
                terminal_input_enabled: history_ready && !self.menu_open && !gesture.block_pointer,
                keyboard_input_enabled: input_ready && !self.menu_open,
                persistent_keyboard_focus: input_ready && !self.menu_open,
                // No clipboard/paste policy is implemented by this probe.
                defer_paste_to_application: true,
                ..Default::default()
            },
        );
        self.gestures.paint_helper(ui, terminal_rect);
        if input_ready && self.keyboard_visible && !panel_open {
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

    fn apply_key_row(&mut self, action: terminal_chrome::KeyRowAction) {
        use terminal_chrome::KeyRowAction;
        match action {
            KeyRowAction::None => {}
            KeyRowAction::Paste => {
                if let Some(text) =
                    festerm_ios_window::clipboard_text().filter(|text| !text.is_empty())
                {
                    route_input(&mut self.terminal, InputEvent::Paste(text), &mut self.sink);
                }
            }
            KeyRowAction::Escape => self.send_key(Key::Escape),
            KeyRowAction::Tab => self.send_key(Key::Tab),
            KeyRowAction::ToggleControl => self.control = !self.control,
            KeyRowAction::ToggleAlt => self.alt = !self.alt,
            KeyRowAction::TogglePanel => self.keys_panel.open = !self.keys_panel.open,
            KeyRowAction::ToggleKeyboard => {
                self.keyboard_visible = !self.keyboard_visible;
                if self.keyboard_visible {
                    self.keys_panel.open = false;
                }
            }
        }
    }

    #[cfg(feature = "live-ssh")]
    fn reconnect_banner(&self) -> model::ReconnectBanner {
        let status = self
            .live_view()
            .map_or("Connection ended", |live| live.status);
        model::ReconnectBanner {
            detail: status
                .starts_with("Input rejected")
                .then(|| "Earlier accepted input may have executed.".to_owned()),
            status: status.to_owned(),
            reattach: self
                .active_spec()
                .and_then(profiles::SessionSpec::persistent_label),
        }
    }
}

impl eframe::App for MobileApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Safe-area strips (status bar, home indicator) match the app surface.
        theme::color::BG.to_normalized_gamma_f32()
    }

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
    fn mobile_editor_saves_edits_and_deletes_profiles_with_credentials() {
        let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle::default())))
            .with_profiles(profiles::testing::profiles());
        app.editor.open_new();
        app.editor.host = "web.example.net".to_owned();
        app.editor.username = "deploy".to_owned();
        app.editor.session_mode = 1;
        app.editor.session_name = "work".to_owned();
        app.editor.auth_mode = forms::AuthMode::Key;
        assert!(
            app.save_editor_profile().is_none(),
            "a key must be chosen first"
        );
        app.generate_key();
        assert!(app.editor.auth_notice.is_some());
        let id = app.save_editor_profile().expect("saved");
        let spec = app.profiles.as_ref().unwrap().spec(&id).unwrap();
        assert_eq!(spec.name, "web.example.net");
        assert_eq!(spec.persistent_label().as_deref(), Some("tmux · work"));
        let first_key = spec.ssh.auth.clone();

        // Replacing the key discards the old secret once the edit is saved.
        app.edit_profile(&id);
        assert_eq!(app.editor.auth_mode, forms::AuthMode::Key);
        app.generate_key();
        assert!(app.editor.unsaved_auth().is_some());
        app.save_editor_profile().expect("updated");
        let store = app.profiles.as_ref().unwrap();
        assert_ne!(
            store.credential_status(&first_key),
            festerm_mobile_profiles::CredentialStatus::Present
        );

        app.edit_profile(&id);
        app.delete_editor_profile();
        assert!(app.profiles.as_ref().unwrap().rows(app.caps).is_empty());
    }

    #[test]
    fn mobile_sftp_profiles_save_edit_open_and_keep_their_start_folder() {
        let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle::default())))
            .with_profiles(profiles::testing::profiles());
        app.editor.open_new();
        app.editor.kind_index = forms::EditorState::kinds(app.caps)
            .iter()
            .position(|kind| *kind == model::ProfileKind::Sftp)
            .expect("SFTP is offered in production");
        app.editor.host = "files.example.net".to_owned();
        app.editor.username = "ops".to_owned();
        app.editor.start_dir = " /srv/app ".to_owned();
        let id = app.save_editor_profile().expect("saved");
        let store = app.profiles.as_ref().unwrap();
        assert!(store.is_sftp(&id) && store.spec(&id).is_none());
        let spec = store.sftp_spec(&id).unwrap();
        assert_eq!(spec.start_dir.as_deref(), Some("/srv/app"));
        assert_eq!(spec.detail(), "SFTP · ops@files.example.net");

        app.editor.close();
        app.edit_profile(&id);
        assert_eq!(app.editor.kind(app.caps), model::ProfileKind::Sftp);
        assert_eq!(app.editor.start_dir, "/srv/app");
        app.editor.start_dir.clear();
        app.save_editor_profile().expect("updated");
        let spec = app.profiles.as_ref().unwrap().sftp_spec(&id).unwrap();
        assert_eq!(spec.start_dir, None);
    }

    #[cfg(feature = "live-ssh")]
    #[test]
    fn mobile_files_only_tab_returns_to_profiles_and_drops_its_session() {
        use egui_kittest::{kittest::Queryable, Harness};
        use files_fixture::{fixture_entry, FixtureFs, FixtureTransfers};
        use std::sync::Arc;

        let app = Rc::new(RefCell::new(MobileApp::new(Rc::new(Cell::new(
            Lifecycle {
                active: true,
                ..Default::default()
            },
        )))));
        let ctx = egui::Context::default();
        let fs = |kind, home| {
            Arc::new(FixtureFs {
                kind,
                home,
                entries: vec![fixture_entry("a.txt", false, 5, 1)],
            })
        };
        let session = files::FilesSession::ready(
            "release-files".to_owned(),
            "SFTP · ops@files.example.net".to_owned(),
            files::Ready {
                remote: fs(festerm_sftp::PaneKind::Remote, "/srv"),
                local: fs(festerm_sftp::PaneKind::Local, ""),
                local_root: std::path::PathBuf::from("/nonexistent-festerm-test"),
                transfers: Arc::new(FixtureTransfers::new(Vec::new())),
                start_dir: None,
            },
            &ctx,
        );
        {
            let mut app = app.borrow_mut();
            app.files = Some(Box::new(session));
            app.files_ctx.only = true;
            app.surface = Surface::Terminal;
            app.workspace = workspaces::Workspace::Files;
        }
        let displayed = app.clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                move |ui, _: &mut ()| displayed.borrow_mut().show_with_occlusion(ui, 0.0),
                (),
            );
        harness.run_steps(3);
        assert!(
            harness.query_by_label("Files").is_none(),
            "no terminal Files button"
        );
        harness.get_by_label("Back").click();
        harness.run_steps(3);
        let app = app.borrow();
        assert!(app.files.is_none());
        assert_eq!(app.surface, Surface::Tabs);
        assert_eq!(app.workspace, workspaces::Workspace::Terminal);
    }

    #[cfg(feature = "live-ssh")]
    #[test]
    fn mobile_header_menu_resets_demo_without_stale_input_and_has_no_preview_entries() {
        use egui_kittest::{kittest::Queryable, Harness};

        let app = Rc::new(RefCell::new(MobileApp::new_terminal(Rc::new(Cell::new(
            Lifecycle {
                active: true,
                ..Default::default()
            },
        )))));
        let displayed = app.clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                move |ui, _: &mut ()| displayed.borrow_mut().show_with_occlusion(ui, 0.35),
                (),
            );
        harness.run();
        app.borrow_mut().control = true;
        harness.get_by_label("More").click();
        harness.run();
        assert!(app.borrow().menu_open);
        assert!(harness.query_by_label("Files (preview)").is_none());
        assert!(harness.query_by_label("README (preview)").is_none());
        harness.get_by_label("Reset demo").click();
        harness.run();
        assert!(!app.borrow().control, "resetting releases sticky modifiers");
        app.borrow_mut().control = true;
        app.borrow_mut()
            .switch_workspace(workspaces::Workspace::Files);
        assert!(!app.borrow().control);
        app.borrow_mut()
            .switch_workspace(workspaces::Workspace::Terminal);
        assert_eq!(app.borrow().sink.diagnostics.byte_count, 0);
        app.borrow_mut().send_key(Key::Tab);
        assert!(app.borrow().sink.diagnostics.byte_count > 0);
        app.borrow_mut().editor.open_new();
        harness.run();
        assert!(app.borrow().editor.open);
        app.borrow_mut().reset_fixture();
        harness.run();
        assert!(!app.borrow().editor.open);
        assert_eq!(app.borrow().sink.diagnostics.byte_count, 0);
    }

    #[cfg(feature = "live-ssh")]
    #[test]
    fn mobile_editor_fits_measured_phone_tablet_space_and_releases_it() {
        use egui_kittest::{kittest::Queryable, Harness};
        for (width, height, occlusion) in [
            (390.0, 844.0, 0.35),
            (834.0, 1194.0, 0.30),
            (1194.0, 834.0, 0.35),
            (375.0, 1024.0, 0.35),
            (844.0, 390.0, 0.35),
            (390.0, 844.0, 0.0),
        ] {
            let app = Rc::new(RefCell::new(MobileApp::new_terminal(Rc::new(Cell::new(
                Lifecycle {
                    active: true,
                    ..Default::default()
                },
            )))));
            let displayed = app.clone();
            let mut harness = Harness::builder()
                .with_size(egui::vec2(width, height))
                .build_ui_state(
                    move |ui, _: &mut ()| displayed.borrow_mut().show_with_occlusion(ui, occlusion),
                    (),
                );
            harness.run();
            let grid_height = app.borrow().view.diagnostics().grid_rect.unwrap().height();
            // Open/close/reopen after cached panel geometry has settled.
            for _ in 0..2 {
                app.borrow_mut().editor.open_new();
                harness.run();
                let limit = height * (1.0 - occlusion) + 1.0;
                for label in ["Save & Connect", "Save only", "Cancel"] {
                    let rect = harness.get_by_label(label).rect();
                    assert!(
                        rect.bottom() <= limit,
                        "{label} {rect:?} in {width}x{height}"
                    );
                    assert!(rect.height() >= 40.0, "{label} {rect:?}");
                }
                assert_eq!(app.borrow().sink.diagnostics.byte_count, 0);
                app.borrow_mut().editor.close();
                harness.run();
                harness.run();
                let released = app.borrow().view.diagnostics().grid_rect.unwrap().height();
                assert!(
                    (released - grid_height).abs() < 2.0,
                    "{released} vs {grid_height}"
                );
            }
        }
    }

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
            let mut app = MobileApp::new_terminal(lifecycle.clone());
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
            assert_eq!(app.view.font_size_points(), 17.25);
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
            assert_eq!(app.view.font_size_points(), 17.25);
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
            let mut app = MobileApp::new_terminal(lifecycle);
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
        let mut app = MobileApp::new_terminal(lifecycle.clone());
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
            let mut app = MobileApp::new_terminal(lifecycle.clone());
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
            assert!(
                grid.bottom() <= toolbar.top(),
                "{width}x{height} {grid:?} {toolbar:?}"
            );
            assert!(toolbar.bottom() <= height - keyboard_height + 0.5);
            assert!(grid.height() > 0.0);
            // Tap Ctrl through the real UI: it must not dismiss the keyboard.
            let point = app.control_rect.expect("rendered Ctrl key").center();
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
        let mut app = MobileApp::new_terminal(lifecycle);
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
    fn mobile_key_row_and_panel_apply_modifiers_once_through_core_encoding() {
        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut app = MobileApp::new_terminal(lifecycle);
        app.apply_key_row(terminal_chrome::KeyRowAction::ToggleControl);
        assert!(app.control);
        app.send_key_with(Key::Function(5), Modifiers::NONE);
        assert!(!app.control, "Ctrl is consumed by the next key");
        app.send_key_with(Key::Tab, Modifiers::SHIFT);
        assert_eq!(app.sink.diagnostics.byte_count, 10); // CSI 15;5 ~ then CSI Z.
    }

    #[test]
    fn mobile_workspaces_limit_native_keyboard_and_terminal_gestures_to_terminal() {
        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut app = MobileApp::new_terminal(lifecycle);
        let ctx = egui::Context::default();
        let render = |app: &mut MobileApp, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(390.0, 844.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| app.show_with_occlusion(ui, 0.0),
            )
        };
        render(&mut app, vec![]).drop_without_applying_deltas();
        let output = render(&mut app, vec![]);
        assert!(output.platform_output.ime.is_some());
        output.drop_without_applying_deltas();
        render(
            &mut app,
            vec![egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "composing".into(),
                active_range_chars: None,
            })],
        )
        .drop_without_applying_deltas();

        app.control = true;
        app.switch_workspace(workspaces::Workspace::Files);
        assert!(!app.control);
        let bytes_after_focus_out = app.sink.diagnostics.byte_count;
        let output = render(&mut app, vec![]);
        assert!(output.platform_output.ime.is_none());
        assert!(!output.shapes.is_empty());
        output.drop_without_applying_deltas();

        app.switch_workspace(workspaces::Workspace::Markdown);
        let output = render(&mut app, vec![]);
        assert!(output.platform_output.ime.is_none());
        assert!(!output.shapes.is_empty());
        output.drop_without_applying_deltas();

        app.switch_workspace(workspaces::Workspace::Terminal);
        render(&mut app, vec![]).drop_without_applying_deltas();
        let output = render(
            &mut app,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(output.platform_output.ime.is_some());
        output.drop_without_applying_deltas();
        assert_eq!(
            app.sink.diagnostics.byte_count,
            bytes_after_focus_out + 1,
            "abandoning preedit must let terminal keys route after returning"
        );
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
        let mut app = MobileApp::new_terminal(lifecycle.clone());
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
