//! Application-level tab and session model.
//!
//! This module is the application coordinator described in
//! `docs/application-command-model.md`: it owns the always-nonempty tab
//! collection (Launcher, Settings, and Local Shell session tabs), the active
//! tab cursor, and `AppCommand` dispatch. Invocation surfaces (chip clicks,
//! launcher buttons, and future shortcuts/command palette entries) send the
//! same `AppCommand` values here rather than each implementing their own
//! session or tab policy.
//!
//! It does not implement terminal protocol semantics: each session tab still
//! routes session output through the single-writer `Terminal` +
//! `SessionController` pair defined in `session_controller.rs`.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use eframe::egui;
use festerm_core::{Dimensions, Terminal};
use festerm_pty::{LocalProfile, LocalPtyError, LocalPtySession};
use festerm_session::{Session, SessionEventNotifier, SessionLifecycle};
use festerm_ui_egui::{chrome::ChipStatus, TerminalView};

use crate::session_controller::{seed_session_failure, terminal_size, SessionController};

/// Stable application-level tab identifier.
///
/// Distinct from `festerm_session::SessionId`: Launcher and Settings tabs
/// have no backend session, and a tab's identity must outlive any particular
/// session attempt.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TabId(u64);

impl TabId {
    fn next() -> Self {
        static NEXT_TAB_ID: AtomicU64 = AtomicU64::new(1);
        Self(NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// Opaque numeric form used only to correlate chip presentation identity;
    /// it carries no terminal content.
    pub const fn chip_id(self) -> u64 {
        self.0
    }
}

/// Uses egui's thread-safe wake mechanism instead of polling for PTY output.
struct EguiRepaintNotifier(egui::Context);

impl SessionEventNotifier for EguiRepaintNotifier {
    fn notify(&self) {
        self.0.request_repaint();
    }
}

fn make_notifier(context: &egui::Context) -> Arc<dyn SessionEventNotifier> {
    Arc::new(EguiRepaintNotifier(context.clone()))
}

/// A running local-shell session tab: the terminal, its controller, and the
/// presentation view. `SessionController` remains the sole terminal writer.
pub struct SessionTab {
    pub terminal: Terminal,
    pub controller: SessionController<LocalPtySession>,
    pub view: TerminalView,
    /// Stable primary identity (`docs/gui-design.md` "Identity precedence").
    /// Transient terminal-provided titles are shown as secondary metadata and
    /// must never replace this.
    pub label: String,
}

impl SessionTab {
    fn start_default(context: &egui::Context) -> Self {
        let dimensions = Dimensions::new(80, 24).expect("default dimensions are valid");
        let size = terminal_size(dimensions).expect("default dimensions fit PTY limits");
        let result = LocalPtySession::start_default_with_notifier(size, make_notifier(context));
        Self::from_session_result(result, dimensions, "Local Shell")
    }

    /// Starts the application's first tab, honoring an optional
    /// native-window-smoke profile override (see `native_smoke.rs`). Used
    /// only once, for the initial tab at startup.
    pub(crate) fn start_primary(
        context: &egui::Context,
        smoke_profile: Option<LocalProfile>,
    ) -> Self {
        let dimensions = Dimensions::new(80, 24).expect("default dimensions are valid");
        let size = terminal_size(dimensions).expect("default dimensions fit PTY limits");
        let notifier = make_notifier(context);
        let result = match smoke_profile {
            Some(profile) => LocalPtySession::start_with_notifier(profile, size, notifier),
            None => LocalPtySession::start_default_with_notifier(size, notifier),
        };
        Self::from_session_result(result, dimensions, "Local Shell")
    }

    fn from_session_result(
        result: Result<LocalPtySession, LocalPtyError>,
        dimensions: Dimensions,
        label: &str,
    ) -> Self {
        let mut terminal =
            Terminal::new(dimensions).expect("default terminal allocation should succeed");
        let controller = match result {
            Ok(session) => {
                tracing::info!(
                    target: "festerm::session",
                    session = %session.id(),
                    "started local shell session"
                );
                SessionController::with_session(session)
            }
            Err(error) => {
                let message = error.to_string();
                tracing::error!(
                    target: "festerm::session",
                    %error,
                    "could not start local shell"
                );
                seed_session_failure(&mut terminal, &message);
                SessionController::with_startup_error(message)
            }
        };
        Self {
            terminal,
            controller,
            view: TerminalView::default(),
            label: label.to_owned(),
        }
    }

    /// Compact, accessible connection-state vocabulary for the chip status
    /// dot (`docs/gui-design.md` "Connection states").
    pub fn chip_status(&self) -> ChipStatus {
        if self.controller.start_error().is_some() {
            return ChipStatus::Failed;
        }
        match self.controller.lifecycle() {
            None | Some(SessionLifecycle::Starting) => ChipStatus::Starting,
            Some(SessionLifecycle::Running) => ChipStatus::Connected,
            Some(SessionLifecycle::Stopping) => ChipStatus::Disconnected,
            Some(SessionLifecycle::Exited(_) | SessionLifecycle::Stopped) => ChipStatus::Exited,
            Some(SessionLifecycle::Failed(_)) => ChipStatus::Failed,
        }
    }
}

/// The content of one tab.
///
/// Launcher and Settings are non-session application surfaces
/// (`docs/gui-design.md` "Launcher as a tab", "Settings as an application
/// surface"); they carry no `Session`/`Terminal` pair.
pub enum TabContent {
    Launcher,
    Settings,
    Session(Box<SessionTab>),
}

pub struct Tab {
    pub id: TabId,
    pub content: TabContent,
}

/// Product-level application actions dispatched from any invocation surface
/// (chip row, launcher buttons, and future shortcuts/command palette), per
/// `docs/application-command-model.md`. UI code must not implement its own
/// copy of these operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppCommand {
    /// "New Tab opens the session launcher" (`docs/gui-design.md`
    /// "Interaction Conventions").
    NewLauncherTab,
    /// Opens (or focuses) the singleton Settings application surface.
    OpenSettings,
    /// A separate action that opens the default local profile directly,
    /// bypassing the launcher for users who prefer that workflow.
    StartLocalSession,
    ActivateTab(TabId),
    CloseTab(TabId),
    ToggleSessionInspector,
}

/// Owns the always-nonempty tab collection and the active-tab cursor.
pub struct AppState {
    tabs: Vec<Tab>,
    active: TabId,
    inspector_open: bool,
}

impl AppState {
    /// Starts with one primary local shell tab, matching the M5 completion
    /// criterion that fesTerm opens a usable shell without extra steps. An
    /// optional native-window-smoke profile override replaces the default
    /// shell with the repository-owned deterministic test child (see
    /// `native_smoke.rs`). Returns the state plus that tab's id, which the
    /// caller retains for the native-window smoke driver.
    pub fn with_primary_session(
        context: &egui::Context,
        smoke_profile: Option<LocalProfile>,
    ) -> (Self, TabId) {
        let session = SessionTab::start_primary(context, smoke_profile);
        let id = TabId::next();
        let state = Self {
            tabs: vec![Tab {
                id,
                content: TabContent::Session(Box::new(session)),
            }],
            active: id,
            inspector_open: false,
        };
        (state, id)
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub const fn active(&self) -> TabId {
        self.active
    }

    pub const fn inspector_open(&self) -> bool {
        self.inspector_open
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        let active = self.active;
        self.tabs
            .iter_mut()
            .find(|tab| tab.id == active)
            .expect("active tab id always refers to a live tab")
    }

    pub fn active_tab(&self) -> &Tab {
        let active = self.active;
        self.tabs
            .iter()
            .find(|tab| tab.id == active)
            .expect("active tab id always refers to a live tab")
    }

    pub fn session_tab_mut(&mut self, id: TabId) -> Option<&mut SessionTab> {
        self.tabs
            .iter_mut()
            .find(|tab| tab.id == id)
            .and_then(|tab| match &mut tab.content {
                TabContent::Session(session) => Some(session.as_mut()),
                TabContent::Launcher | TabContent::Settings => None,
            })
    }

    /// Every running session tab, independent of which is active. Each open
    /// session remains a persistent object that must keep draining its
    /// bounded backend queues even while another tab is focused.
    pub fn session_tabs_mut(&mut self) -> impl Iterator<Item = &mut SessionTab> {
        self.tabs
            .iter_mut()
            .filter_map(|tab| match &mut tab.content {
                TabContent::Session(session) => Some(session.as_mut()),
                TabContent::Launcher | TabContent::Settings => None,
            })
    }

    /// Applies one `AppCommand`. This is the single command-handling path;
    /// every invocation surface must converge here rather than implementing
    /// independent tab/session policy.
    pub fn dispatch(&mut self, command: AppCommand, context: &egui::Context) {
        match command {
            AppCommand::NewLauncherTab => self.open_launcher(),
            AppCommand::OpenSettings => self.open_settings(),
            AppCommand::StartLocalSession => self.start_local_session(context),
            AppCommand::ActivateTab(id) => self.activate(id),
            AppCommand::CloseTab(id) => self.close(id),
            AppCommand::ToggleSessionInspector => self.inspector_open = !self.inspector_open,
        }
    }

    fn open_launcher(&mut self) {
        // A fresh launcher tab is pushed each time rather than deduplicated:
        // launcher tabs are disposable, and "Users can keep a launcher tab
        // open while other sessions run" (docs/gui-design.md).
        let id = TabId::next();
        self.tabs.push(Tab {
            id,
            content: TabContent::Launcher,
        });
        self.active = id;
    }

    fn open_settings(&mut self) {
        // Settings is a singleton application surface with its own chip.
        if let Some(existing) = self
            .tabs
            .iter()
            .find(|tab| matches!(tab.content, TabContent::Settings))
        {
            self.active = existing.id;
            return;
        }
        let id = TabId::next();
        self.tabs.push(Tab {
            id,
            content: TabContent::Settings,
        });
        self.active = id;
    }

    fn start_local_session(&mut self, context: &egui::Context) {
        let session = SessionTab::start_default(context);
        let id = TabId::next();
        self.tabs.push(Tab {
            id,
            content: TabContent::Session(Box::new(session)),
        });
        self.active = id;
    }

    fn activate(&mut self, id: TabId) {
        if self.tabs.iter().any(|tab| tab.id == id) {
            self.active = id;
        }
    }

    fn close(&mut self, id: TabId) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let removed = self.tabs.remove(index);
        if let TabContent::Session(session) = removed.content {
            session.controller.shutdown();
        }
        if self.tabs.is_empty() {
            // Root state must never go empty (`docs/gui-design.md` "Root
            // Application States"): return to the launcher rather than an
            // undefined window.
            self.open_launcher();
            return;
        }
        if self.active == id {
            let next_index = index.min(self.tabs.len() - 1);
            self.active = self.tabs[next_index].id;
        }
    }
}

#[cfg(test)]
impl AppState {
    /// Test-only constructor that starts with a Launcher tab instead of
    /// spawning a real local shell, so dispatch/tab-lifecycle tests do not
    /// need a live PTY.
    fn for_test() -> Self {
        let id = TabId::next();
        Self {
            tabs: vec![Tab {
                id,
                content: TabContent::Launcher,
            }],
            active: id,
            inspector_open: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launcher_ids(state: &AppState) -> Vec<TabId> {
        state
            .tabs()
            .iter()
            .filter(|tab| matches!(tab.content, TabContent::Launcher))
            .map(|tab| tab.id)
            .collect()
    }

    #[test]
    fn new_launcher_tab_opens_and_activates_an_additional_tab() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let initial = state.active();

        state.dispatch(AppCommand::NewLauncherTab, &context);

        assert_eq!(state.tabs().len(), 2);
        assert_ne!(state.active(), initial, "new launcher tab becomes active");
        assert_eq!(
            launcher_ids(&state).len(),
            2,
            "launcher tabs are not deduplicated"
        );
    }

    #[test]
    fn open_settings_is_a_singleton_and_reactivates_the_existing_tab() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();

        state.dispatch(AppCommand::OpenSettings, &context);
        assert_eq!(state.tabs().len(), 2);
        let settings_id = state.active();
        assert!(matches!(state.active_tab().content, TabContent::Settings));

        // Switch away, then request Settings again: it must reactivate the
        // same tab rather than creating a second one.
        let launcher_id = launcher_ids(&state)[0];
        state.dispatch(AppCommand::ActivateTab(launcher_id), &context);
        assert_eq!(state.active(), launcher_id);

        state.dispatch(AppCommand::OpenSettings, &context);
        assert_eq!(state.tabs().len(), 2, "Settings is a singleton chip");
        assert_eq!(state.active(), settings_id);
    }

    #[test]
    fn activate_ignores_an_unknown_tab_id() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let initial = state.active();

        state.dispatch(AppCommand::NewLauncherTab, &context);
        let unknown = TabId::next();
        state.dispatch(AppCommand::ActivateTab(unknown), &context);

        assert_ne!(state.active(), initial);
        assert_ne!(state.active(), unknown);
    }

    #[test]
    fn toggle_inspector_flips_state() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        assert!(!state.inspector_open());

        state.dispatch(AppCommand::ToggleSessionInspector, &context);
        assert!(state.inspector_open());

        state.dispatch(AppCommand::ToggleSessionInspector, &context);
        assert!(!state.inspector_open());
    }

    #[test]
    fn closing_the_active_tab_reactivates_a_neighbor() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let first = state.active();

        state.dispatch(AppCommand::NewLauncherTab, &context);
        let second = state.active();
        assert_ne!(first, second);

        state.dispatch(AppCommand::CloseTab(second), &context);
        assert_eq!(state.tabs().len(), 1);
        assert_eq!(state.active(), first);
    }

    #[test]
    fn closing_the_last_tab_returns_to_the_launcher_rather_than_going_empty() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let only = state.active();

        state.dispatch(AppCommand::CloseTab(only), &context);

        assert_eq!(state.tabs().len(), 1, "root state is never empty");
        assert!(matches!(state.active_tab().content, TabContent::Launcher));
    }
}
