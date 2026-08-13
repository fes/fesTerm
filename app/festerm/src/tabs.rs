//! Application-level tab and session model.
//!
//! This module is the application coordinator described in
//! `docs/application-command-model.md`: it owns the always-nonempty tab
//! collection (Launcher, Settings, Local Shell, and SSH session tabs), the active
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
use festerm_config::{
    ConfigError, Configuration, SshProfileConfiguration, WorkspaceConfiguration, WorkspaceTab,
};
use festerm_core::{Dimensions, Terminal};
use festerm_pty::{default_local_profile, LocalProfile, LocalPtyError, LocalPtySession};
use festerm_session::{
    HostKeyPrompt, Session, SessionEvent, SessionEventNotifier, SessionId, SessionLifecycle,
    SessionMetrics, SessionSendError, SessionTryReceiveError, ShutdownError, ShutdownResult,
    TerminalSize,
};
use festerm_ssh::{
    HostKeyDecisionResolutionError, HostTrustDecision, SshAuthentication, SshConnectionProfile,
    SshReconnectError, SshSession, SshSessionOptions, SshSessionStartError,
};
use festerm_ui_egui::{
    chrome::{ChipLayout, ChipStatus},
    TerminalView,
};

use crate::session_controller::{seed_session_startup_failure, terminal_size, SessionController};

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

/// Concrete transports that can occupy an application session tab.
///
/// The application owns this narrow sum type so terminal/UI code sees only
/// the common `Session` contract. SSH host-key verification remains the
/// existing `SessionEvent::HostKeyVerification` boundary handled by
/// `SessionController`; no secret material is exposed here.
pub enum ApplicationSession {
    Local(LocalPtySession),
    Ssh(SshSession),
}

/// The only host-key decisions exposed by the application during M7.
///
/// Persistent trust is deliberately not representable until M8 storage owns
/// its policy and secure persistence boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostKeyTrustDecision {
    Reject,
    AcceptOnce,
}

impl From<HostKeyTrustDecision> for HostTrustDecision {
    fn from(value: HostKeyTrustDecision) -> Self {
        match value {
            HostKeyTrustDecision::Reject => Self::Reject,
            HostKeyTrustDecision::AcceptOnce => Self::AcceptOnce,
        }
    }
}

/// An application-level failure to resolve a displayed host-key request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostKeyTrustResolutionError {
    NoPendingPrompt,
    NotSshSession,
    Transport(HostKeyDecisionResolutionError),
}

impl std::fmt::Display for HostKeyTrustResolutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPendingPrompt => formatter.write_str("no host-key prompt is pending"),
            Self::NotSshSession => formatter.write_str("the tab is not an SSH session"),
            Self::Transport(error) => error.fmt(formatter),
        }
    }
}

/// An application-level failure to request an SSH reconnect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionReconnectError {
    NotSshSession,
    Transport(SshReconnectError),
}

impl std::fmt::Display for SessionReconnectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSshSession => formatter.write_str("the tab is not an SSH session"),
            Self::Transport(error) => error.fmt(formatter),
        }
    }
}

impl ApplicationSession {
    /// Resolves a host-key prompt through an SSH session without exposing its
    /// resolver to GUI code or allowing persistent acceptance.
    pub fn resolve_host_key_prompt(
        &self,
        prompt: &HostKeyPrompt,
        decision: HostKeyTrustDecision,
    ) -> Result<(), HostKeyTrustResolutionError> {
        let Self::Ssh(session) = self else {
            return Err(HostKeyTrustResolutionError::NotSshSession);
        };
        session
            .host_key_decision_resolver()
            .resolve(prompt, decision.into())
            .map_err(HostKeyTrustResolutionError::Transport)
    }

    /// Reports whether this session can accept a nonblocking user reconnect
    /// request. Local sessions deliberately never expose this capability.
    pub fn reconnect_available(&self) -> bool {
        matches!(self, Self::Ssh(session) if session.reconnect_available())
    }

    /// Queues one user-directed reconnect on an SSH session.
    pub fn try_reconnect(&self) -> Result<(), SessionReconnectError> {
        let Self::Ssh(session) = self else {
            return Err(SessionReconnectError::NotSshSession);
        };
        session
            .try_reconnect()
            .map_err(SessionReconnectError::Transport)
    }
}

impl Session for ApplicationSession {
    fn id(&self) -> SessionId {
        match self {
            Self::Local(session) => session.id(),
            Self::Ssh(session) => session.id(),
        }
    }

    fn lifecycle(&self) -> SessionLifecycle {
        match self {
            Self::Local(session) => session.lifecycle(),
            Self::Ssh(session) => session.lifecycle(),
        }
    }

    fn metrics(&self) -> SessionMetrics {
        match self {
            Self::Local(session) => session.metrics(),
            Self::Ssh(session) => session.metrics(),
        }
    }

    fn try_send_input(&self, bytes: &[u8]) -> Result<(), SessionSendError> {
        match self {
            Self::Local(session) => session.try_send_input(bytes),
            Self::Ssh(session) => session.try_send_input(bytes),
        }
    }

    fn try_resize(&self, size: TerminalSize) -> Result<(), SessionSendError> {
        match self {
            Self::Local(session) => session.try_resize(size),
            Self::Ssh(session) => session.try_resize(size),
        }
    }

    fn try_shutdown(&self) -> Result<(), SessionSendError> {
        match self {
            Self::Local(session) => session.try_shutdown(),
            Self::Ssh(session) => session.try_shutdown(),
        }
    }

    fn try_recv_event(&self) -> Result<SessionEvent, SessionTryReceiveError> {
        match self {
            Self::Local(session) => session.try_recv_event(),
            Self::Ssh(session) => session.try_recv_event(),
        }
    }

    fn shutdown(&self, timeout: std::time::Duration) -> Result<ShutdownResult, ShutdownError> {
        match self {
            Self::Local(session) => session.shutdown(timeout),
            Self::Ssh(session) => session.shutdown(timeout),
        }
    }
}

/// A running local-shell or SSH session tab: the terminal, its controller,
/// and presentation view. `SessionController` remains the sole terminal
/// writer.
pub struct SessionTab {
    pub terminal: Terminal,
    pub controller: SessionController<ApplicationSession>,
    pub view: TerminalView,
    /// Stable primary identity (`docs/gui-design.md` "Identity precedence").
    /// Transient terminal-provided titles are shown as secondary metadata and
    /// must never replace this.
    pub label: String,
    /// Static connection metadata resolved before a session starts. This gives
    /// the chip useful secondary identity before the child emits an OSC title.
    pub launch_secondary: Option<String>,
    /// The configuration profile that explicitly launched this local session.
    ///
    /// Default, ad-hoc, and SSH sessions deliberately leave this empty. It is
    /// metadata only; no runtime process or connection state is captured.
    pub profile_identifier: Option<String>,
    /// Non-secret immutable launch facts exposed to application-owned
    /// presentation such as the Session Inspector.
    pub inspector_transport: InspectorTransport,
}

/// Narrow transport metadata safe for application chrome. Keeping this owned
/// by the tab prevents egui code from reaching into PTY or SSH backends.
pub enum InspectorTransport {
    Local,
    Ssh {
        username: String,
        host: String,
        port: u16,
    },
}

/// A restored SSH workspace surface that deliberately has no live session.
///
/// Workspace metadata contains destination details but never authentication or
/// trust material, so restoration must return the user to the transient
/// authentication form instead of starting a transport.
pub struct SshAuthenticationRequiredTab {
    pub profile: SshProfileConfiguration,
}

impl SessionTab {
    fn start_default(context: &egui::Context) -> Self {
        let dimensions = Dimensions::new(80, 24).expect("default dimensions are valid");
        let size = terminal_size(dimensions).expect("default dimensions fit PTY limits");
        let result = LocalPtySession::start_default_with_notifier(size, make_notifier(context));
        Self::from_local_session_result(
            result.map(ApplicationSession::Local),
            dimensions,
            "Local Shell",
            default_local_profile()
                .ok()
                .and_then(|profile| local_profile_secondary(&profile)),
            None,
        )
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
        let launch_secondary = smoke_profile
            .as_ref()
            .and_then(local_profile_secondary)
            .or_else(|| {
                default_local_profile()
                    .ok()
                    .and_then(|profile| local_profile_secondary(&profile))
            });
        let result = match smoke_profile {
            Some(profile) => LocalPtySession::start_with_notifier(profile, size, notifier),
            None => LocalPtySession::start_default_with_notifier(size, notifier),
        };
        Self::from_local_session_result(
            result.map(ApplicationSession::Local),
            dimensions,
            "Local Shell",
            launch_secondary,
            None,
        )
    }

    /// Starts a local PTY from reusable, secret-free profile metadata.
    fn start_local_profile(
        profile: LocalProfile,
        profile_id: &str,
        context: &egui::Context,
    ) -> Self {
        let dimensions = Dimensions::new(80, 24).expect("default dimensions are valid");
        let size = terminal_size(dimensions).expect("default dimensions fit PTY limits");
        let launch_secondary = local_profile_secondary(&profile);
        let result = LocalPtySession::start_with_notifier(profile, size, make_notifier(context));
        Self::from_local_session_result(
            result.map(ApplicationSession::Local),
            dimensions,
            profile_id,
            launch_secondary,
            Some(profile_id.to_owned()),
        )
    }

    fn start_ssh(
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
        options: SshSessionOptions,
        context: &egui::Context,
    ) -> Self {
        let size = profile.initial_size();
        let dimensions = Dimensions::new(usize::from(size.columns()), usize::from(size.rows()))
            .expect("validated SSH terminal size fits terminal dimensions");
        let label = format!("{}@{}", profile.username(), profile.identity().host());
        let launch_secondary = Some(format!(
            "SSH · {}:{}",
            profile.identity().host(),
            profile.identity().port()
        ));
        let inspector_transport = InspectorTransport::Ssh {
            username: profile.username().to_owned(),
            host: profile.identity().host().to_owned(),
            port: profile.identity().port(),
        };
        let result = SshSession::start_with_notifier_and_options(
            profile,
            authentication,
            options,
            make_notifier(context),
        )
        .map(ApplicationSession::Ssh);
        Self::from_ssh_session_result(
            result,
            dimensions,
            &label,
            launch_secondary,
            inspector_transport,
        )
    }

    fn from_local_session_result(
        result: Result<ApplicationSession, LocalPtyError>,
        dimensions: Dimensions,
        label: &str,
        launch_secondary: Option<String>,
        profile_identifier: Option<String>,
    ) -> Self {
        Self::from_session_result(
            result,
            dimensions,
            label,
            launch_secondary,
            profile_identifier,
            "Local shell",
            InspectorTransport::Local,
        )
    }

    fn from_ssh_session_result(
        result: Result<ApplicationSession, SshSessionStartError>,
        dimensions: Dimensions,
        label: &str,
        launch_secondary: Option<String>,
        inspector_transport: InspectorTransport,
    ) -> Self {
        Self::from_session_result(
            result,
            dimensions,
            label,
            launch_secondary,
            None,
            "SSH session",
            inspector_transport,
        )
    }

    fn from_session_result<E: std::fmt::Display>(
        result: Result<ApplicationSession, E>,
        dimensions: Dimensions,
        label: &str,
        launch_secondary: Option<String>,
        profile_identifier: Option<String>,
        session_name: &'static str,
        inspector_transport: InspectorTransport,
    ) -> Self {
        let mut terminal =
            Terminal::new(dimensions).expect("default terminal allocation should succeed");
        let controller = match result {
            Ok(session) => {
                tracing::info!(
                    target: "festerm::session",
                    session = %session.id(),
                    %session_name,
                    "started session"
                );
                SessionController::with_named_session(session, session_name)
            }
            Err(error) => {
                let message = error.to_string();
                tracing::error!(
                    target: "festerm::session",
                    %error,
                    %session_name,
                    "could not start session"
                );
                seed_session_startup_failure(&mut terminal, &message, session_name);
                SessionController::with_named_startup_error(message, session_name)
            }
        };
        Self {
            terminal,
            controller,
            view: TerminalView::default(),
            label: label.to_owned(),
            launch_secondary,
            profile_identifier,
            inspector_transport,
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

    /// Content-free locality/transport text for the status bar.
    pub fn system_label(&self) -> &'static str {
        match self.controller.session() {
            Some(ApplicationSession::Ssh(_)) => "Remote",
            Some(ApplicationSession::Local(_)) | None if cfg!(windows) => "Local · Windows",
            Some(ApplicationSession::Local(_)) | None if cfg!(target_os = "macos") => {
                "Local · macOS"
            }
            Some(ApplicationSession::Local(_)) | None => "Local · Linux",
        }
    }

    /// Transport-specific factual state for the persistent status bar.
    pub fn status_bar_label(&self) -> &'static str {
        match (self.controller.session(), self.chip_status()) {
            (_, ChipStatus::Starting) => "Starting",
            (_, ChipStatus::Reconnecting) => "Reconnecting",
            (_, ChipStatus::Disconnected) => "Disconnected",
            (_, ChipStatus::AuthRequired) => "Authentication required",
            (_, ChipStatus::Failed) => "Failed",
            (_, ChipStatus::Exited) => "Exited",
            (Some(ApplicationSession::Local(_)) | None, ChipStatus::Connected) => "Running",
            (Some(ApplicationSession::Ssh(_)), ChipStatus::Connected) => "Connected",
            (_, ChipStatus::Neutral) => "",
        }
    }

    /// The active SSH host-key request, if the transport is waiting for one.
    /// Local tabs never expose this UI state.
    pub fn host_key_prompt(&self) -> Option<&HostKeyPrompt> {
        matches!(self.controller.session(), Some(ApplicationSession::Ssh(_)))
            .then(|| self.controller.host_key_prompt())
            .flatten()
    }

    /// Sends a nonblocking, one-time host-key decision to the SSH worker.
    ///
    /// The prompt is cleared after either outcome so stale UI cannot submit
    /// the same decision again. A future request arrives through the normal
    /// event boundary and replaces this state.
    pub fn resolve_host_key_trust(
        &mut self,
        decision: HostKeyTrustDecision,
    ) -> Result<(), HostKeyTrustResolutionError> {
        let prompt = self
            .host_key_prompt()
            .cloned()
            .ok_or(HostKeyTrustResolutionError::NoPendingPrompt)?;
        let result = self
            .controller
            .session()
            .expect("an SSH host-key prompt requires a session")
            .resolve_host_key_prompt(&prompt, decision);
        self.controller.clear_host_key_prompt(&prompt);
        result
    }

    /// Reports whether this is an SSH tab whose current live transport can
    /// accept one user-directed reconnect request.
    pub fn reconnect_available(&self) -> bool {
        self.controller
            .session()
            .is_some_and(ApplicationSession::reconnect_available)
    }

    /// Queues a reconnect without waiting for network activity on the GUI
    /// thread. Rejection becomes ordinary content-free session diagnostics.
    pub fn request_reconnect(&mut self) -> Result<(), SessionReconnectError> {
        self.controller
            .session()
            .ok_or(SessionReconnectError::NotSshSession)?
            .try_reconnect()
    }
}

fn local_profile_secondary(profile: &LocalProfile) -> Option<String> {
    profile
        .executable()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

/// The content of one tab.
///
/// Launcher and Settings are non-session application surfaces
/// (`docs/gui-design.md` "Launcher as a tab", "Settings as an application
/// surface"); they carry no `Session`/`Terminal` pair.
pub enum TabContent {
    Launcher,
    Settings,
    SshAuthenticationRequired(SshAuthenticationRequiredTab),
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
#[derive(Debug)]
pub enum AppCommand {
    /// "New Tab opens the session launcher" (`docs/gui-design.md`
    /// "Interaction Conventions").
    OpenLauncher,
    /// Opens (or focuses) the singleton Settings application surface.
    OpenSettings,
    /// Explicitly asks the composition root to reload the configuration
    /// selected at startup. The path stays outside `AppState`; a successful
    /// candidate is installed through [`AppState::replace_configuration`].
    ReloadConfiguration,
    /// Explicitly saves a fresh metadata-only workspace snapshot through the
    /// composition root's private selected configuration source.
    SaveWorkspace,
    /// A separate action that opens the default local profile directly,
    /// bypassing the launcher for users who prefer that workflow.
    StartLocalSession,
    /// Starts a local session from a reusable configuration profile. The
    /// profile identifier is resolved only against this application's
    /// explicitly supplied immutable configuration.
    StartConfiguredLocalProfile {
        profile_id: String,
    },
    /// Starts one SSH transport from explicitly supplied, secret-free
    /// connection metadata, transient authentication, and explicit session
    /// options. Launcher invocation surfaces validate input into these typed
    /// values; this does not create a persisted profile.
    StartSshSession {
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
        options: SshSessionOptions,
    },
    /// Resolves the displayed host-key request for one specific SSH tab.
    /// This command intentionally has no persistent-trust variant.
    ResolveHostKeyTrust {
        tab: TabId,
        decision: HostKeyTrustDecision,
    },
    /// Requests one bounded fresh SSH transport attempt for `tab`. Local,
    /// stopped, or already-reconnecting tabs reject this safely.
    ReconnectSession(TabId),
    ActivateTab(TabId),
    /// Activates the next/previous tab in stable list order
    /// (`docs/gui-design.md` "Next/Previous Tab switch predictably" —
    /// predictable and independent of visual wrapping).
    ActivateNextTab,
    ActivatePreviousTab,
    CloseTab(TabId),
    /// Reorders `moved` to sit immediately before `before` (or at the end of
    /// the row if `None`), preserving the moved tab's identity/state and the
    /// current active tab.
    ReorderTab {
        moved: TabId,
        before: Option<TabId>,
    },
    /// Renames a session tab's stable primary identity (label). No-op for
    /// Launcher/Settings tabs, whose names are fixed.
    RenameTab(TabId, String),
    ToggleSessionInspector,
    /// Flips between wrapped and single-row-scroll chip layout
    /// (`docs/gui-design.md` "Wrapping must remain user-configurable").
    ToggleChipLayout,
    /// Toggles the bottom status bar on/off (`docs/gui-design.md`
    /// "Contextual status region" / "the status bar should be configurable
    /// on/off").
    ToggleStatusBar,
}

/// Owns the always-nonempty tab collection and the active-tab cursor.
pub struct AppState {
    tabs: Vec<Tab>,
    active: TabId,
    configuration: Configuration,
    inspector_open: bool,
    chip_layout: ChipLayout,
    status_bar_visible: bool,
}

impl AppState {
    /// Starts in the singleton Launcher when there is no workspace to restore.
    /// This is the ordinary product startup path; native-window smoke may
    /// still request a deterministic primary session explicitly.
    pub fn with_launcher(configuration: Configuration) -> Self {
        let id = TabId::next();
        Self {
            tabs: vec![Tab {
                id,
                content: TabContent::Launcher,
            }],
            active: id,
            configuration,
            inspector_open: false,
            chip_layout: ChipLayout::SingleRowScroll,
            status_bar_visible: true,
        }
    }

    /// Starts with one primary local shell tab, matching the M5 completion
    /// criterion that fesTerm opens a usable shell without extra steps. An
    /// optional native-window-smoke profile override replaces the default
    /// shell with the repository-owned deterministic test child (see
    /// `native_smoke.rs`). Returns the state plus that tab's id, which the
    /// caller retains for the native-window smoke driver.
    pub fn with_primary_session(
        context: &egui::Context,
        smoke_profile: Option<LocalProfile>,
        configuration: Configuration,
    ) -> (Self, TabId) {
        let session = SessionTab::start_primary(context, smoke_profile);
        let id = TabId::next();
        let state = Self {
            tabs: vec![Tab {
                id,
                content: TabContent::Session(Box::new(session)),
            }],
            active: id,
            configuration,
            inspector_open: false,
            chip_layout: ChipLayout::SingleRowScroll,
            status_bar_visible: true,
        };
        (state, id)
    }

    /// Restores only persisted workspace metadata. New process-local tab IDs
    /// are assigned in saved display order; saved identifiers select focus
    /// but are never treated as runtime `TabId` values.
    pub fn with_restored_workspace(
        context: &egui::Context,
        configuration: Configuration,
        workspace: &WorkspaceConfiguration,
    ) -> Self {
        let mut restored = Vec::with_capacity(workspace.tabs().len());
        let mut focused = None;

        for workspace_tab in workspace.tabs() {
            let id = TabId::next();
            if workspace.focused_tab_id() == Some(workspace_tab.identifier()) {
                focused = Some(id);
            }
            let content = match workspace_tab {
                WorkspaceTab::Launcher(_) => TabContent::Launcher,
                WorkspaceTab::Settings(_) => TabContent::Settings,
                WorkspaceTab::LocalSession(tab) => {
                    let local = configuration
                        .profile(tab.profile_id())
                        .and_then(festerm_config::Profile::as_local)
                        .expect("validated workspace local profile reference");
                    TabContent::Session(Box::new(SessionTab::start_local_profile(
                        local.to_local_profile(),
                        local.identifier(),
                        context,
                    )))
                }
                WorkspaceTab::SshSession(tab) => {
                    let ssh = configuration
                        .profile(tab.profile_id())
                        .and_then(festerm_config::Profile::as_ssh)
                        .expect("validated workspace SSH profile reference");
                    TabContent::SshAuthenticationRequired(SshAuthenticationRequiredTab {
                        profile: ssh.clone(),
                    })
                }
            };
            restored.push(Tab { id, content });
        }

        let active = focused.unwrap_or_else(|| restored[0].id);
        Self {
            tabs: restored,
            active,
            configuration,
            inspector_open: false,
            chip_layout: ChipLayout::SingleRowScroll,
            status_bar_visible: true,
        }
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub const fn active(&self) -> TabId {
        self.active
    }

    /// Returns the immutable, explicitly supplied profile metadata available
    /// to Launcher tabs. This method never performs configuration discovery
    /// or filesystem reads.
    pub const fn configuration(&self) -> &Configuration {
        &self.configuration
    }

    /// Atomically replaces the immutable configuration used only by future
    /// Launcher choices. Existing session tabs retain their live transports
    /// and are never stopped or reconfigured.
    pub fn replace_configuration(&mut self, configuration: Configuration) {
        self.configuration = configuration;
    }

    /// Captures only restorable workspace metadata in current display order.
    ///
    /// Runtime tab IDs, terminal state, active processes, and SSH transports
    /// never leave this application state. A session is represented only when
    /// it retained a configured local-profile identifier and that profile is
    /// still present in the document being replaced.
    pub fn capture_workspace_configuration(&self) -> Result<Configuration, ConfigError> {
        let mut tabs = Vec::new();
        let mut focused_tab_id = None;

        for tab in &self.tabs {
            let identifier = format!("tab-{}", tabs.len() + 1);
            let workspace_tab = match &tab.content {
                TabContent::Launcher => Some(WorkspaceTab::launcher(identifier.clone())?),
                TabContent::Settings => Some(WorkspaceTab::settings(identifier.clone())?),
                TabContent::SshAuthenticationRequired(ssh) => Some(WorkspaceTab::ssh_session(
                    identifier.clone(),
                    ssh.profile.identifier(),
                )?),
                TabContent::Session(session) => session
                    .profile_identifier
                    .as_deref()
                    .filter(|profile_id| {
                        self.configuration
                            .profile(profile_id)
                            .is_some_and(|profile| profile.as_local().is_some())
                    })
                    .map(|profile_id| WorkspaceTab::local_session(identifier, profile_id))
                    .transpose()?,
            };
            if let Some(workspace_tab) = workspace_tab {
                if tab.id == self.active {
                    focused_tab_id = Some(workspace_tab.identifier().to_owned());
                }
                tabs.push(workspace_tab);
            }
        }

        if tabs.is_empty() {
            tabs.push(WorkspaceTab::launcher("tab-1")?);
        }
        let focused_tab_id =
            focused_tab_id.or_else(|| tabs.first().map(|tab| tab.identifier().to_owned()));
        let workspace = WorkspaceConfiguration::new(tabs, focused_tab_id)?;
        self.configuration.with_workspace(workspace)
    }

    pub const fn inspector_open(&self) -> bool {
        self.inspector_open
    }

    pub const fn chip_layout(&self) -> ChipLayout {
        self.chip_layout
    }

    pub const fn status_bar_visible(&self) -> bool {
        self.status_bar_visible
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
                TabContent::Launcher
                | TabContent::Settings
                | TabContent::SshAuthenticationRequired(_) => None,
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
                TabContent::Launcher
                | TabContent::Settings
                | TabContent::SshAuthenticationRequired(_) => None,
            })
    }

    /// Applies one `AppCommand`. This is the single command-handling path;
    /// every invocation surface must converge here rather than implementing
    /// independent tab/session policy.
    pub fn dispatch(&mut self, command: AppCommand, context: &egui::Context) {
        match command {
            AppCommand::OpenLauncher => self.open_launcher(),
            AppCommand::OpenSettings => self.open_settings(),
            // The composition root owns the private selected file location;
            // it validates a candidate before calling `replace_configuration`.
            AppCommand::ReloadConfiguration | AppCommand::SaveWorkspace => {}
            AppCommand::StartLocalSession => self.start_local_session(context),
            AppCommand::StartConfiguredLocalProfile { profile_id } => {
                self.start_configured_local_profile(&profile_id, context)
            }
            AppCommand::StartSshSession {
                profile,
                authentication,
                options,
            } => self.execute_ssh_session(profile, authentication, options, context),
            AppCommand::ResolveHostKeyTrust { tab, decision } => {
                self.resolve_host_key_trust(tab, decision)
            }
            AppCommand::ReconnectSession(tab) => self.request_reconnect(tab),
            AppCommand::ActivateTab(id) => self.activate(id),
            AppCommand::ActivateNextTab => self.activate_relative(1),
            AppCommand::ActivatePreviousTab => self.activate_relative(-1),
            AppCommand::CloseTab(id) => self.close(id),
            AppCommand::ReorderTab { moved, before } => self.reorder(moved, before),
            AppCommand::RenameTab(id, name) => self.rename(id, name),
            AppCommand::ToggleSessionInspector => {
                if matches!(self.active_tab().content, TabContent::Session(_)) {
                    self.inspector_open = !self.inspector_open;
                }
            }
            AppCommand::ToggleChipLayout => {
                self.chip_layout = match self.chip_layout {
                    ChipLayout::Wrap => ChipLayout::SingleRowScroll,
                    ChipLayout::SingleRowScroll => ChipLayout::Wrap,
                };
            }
            AppCommand::ToggleStatusBar => {
                self.status_bar_visible = !self.status_bar_visible;
            }
        }
        // The Inspector follows session chips, but it is not a global panel
        // for Launcher, Settings, or authentication forms.
        if !matches!(self.active_tab().content, TabContent::Session(_)) {
            self.inspector_open = false;
        }
    }

    fn open_launcher(&mut self) {
        // Launcher is a singleton task surface and the window's stable empty
        // state. Every invocation path focuses the existing chip rather than
        // manufacturing duplicate launch surfaces.
        if let Some(existing) = self
            .tabs
            .iter()
            .find(|tab| matches!(tab.content, TabContent::Launcher))
        {
            self.active = existing.id;
            return;
        }
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
        self.place_session(SessionTab::start_default(context));
    }

    fn start_configured_local_profile(&mut self, profile_id: &str, context: &egui::Context) {
        let Some(profile) = self.configuration.profile(profile_id) else {
            return;
        };
        let Some(local) = profile.as_local() else {
            return;
        };
        self.place_session(SessionTab::start_local_profile(
            local.to_local_profile(),
            local.identifier(),
            context,
        ));
    }

    fn execute_ssh_session(
        &mut self,
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
        options: SshSessionOptions,
        context: &egui::Context,
    ) {
        self.place_session(SessionTab::start_ssh(
            profile,
            authentication,
            options,
            context,
        ));
    }

    fn resolve_host_key_trust(&mut self, tab: TabId, decision: HostKeyTrustDecision) {
        let Some(session) = self.session_tab_mut(tab) else {
            return;
        };
        if let Err(error) = session.resolve_host_key_trust(decision) {
            session.controller.record_host_key_resolution_error(error);
        }
    }

    fn request_reconnect(&mut self, tab: TabId) {
        let Some(session) = self.session_tab_mut(tab) else {
            return;
        };
        if let Err(error) = session.request_reconnect() {
            session
                .controller
                .record_operation_error("reconnect request", error);
        }
    }

    fn place_session(&mut self, session: SessionTab) {
        // Starting a session from the active Launcher or restored
        // authentication-required tab replaces that surface in place (same
        // position, same identity) rather than leaving it behind alongside a
        // new session tab.
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == self.active) {
            if matches!(
                tab.content,
                TabContent::Launcher | TabContent::SshAuthenticationRequired(_)
            ) {
                tab.content = TabContent::Session(Box::new(session));
                return;
            }
        }
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

    /// Moves the active cursor by `delta` positions in stable list order,
    /// wrapping around. `delta` is `1` for next, `-1` for previous. Order
    /// follows the tab list (also the drag-reorder order), independent of
    /// how chips currently wrap onto visual rows.
    fn activate_relative(&mut self, delta: i64) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == self.active) else {
            return;
        };
        let len = self.tabs.len() as i64;
        if len == 0 {
            return;
        }
        let next = (index as i64 + delta).rem_euclid(len) as usize;
        self.active = self.tabs[next].id;
    }

    /// Relocates `moved` to sit immediately before `before` (or at the end of
    /// the list if `None`), preserving the moved tab's own identity/content
    /// and leaving the active cursor pointed at whichever tab it already
    /// referenced (`docs/gui-design.md` "Drag-and-drop reorders independent
    /// session objects and should preserve their identity and state.").
    fn reorder(&mut self, moved: TabId, before: Option<TabId>) {
        if Some(moved) == before {
            return;
        }
        let Some(from) = self.tabs.iter().position(|tab| tab.id == moved) else {
            return;
        };
        let tab = self.tabs.remove(from);
        let insert_at = match before {
            Some(before_id) => self
                .tabs
                .iter()
                .position(|tab| tab.id == before_id)
                .unwrap_or(self.tabs.len()),
            None => self.tabs.len(),
        };
        self.tabs.insert(insert_at, tab);
    }

    /// Renames the session tab's stable primary identity (label). A no-op
    /// for Launcher/Settings tabs (their names are fixed chrome, not session
    /// state) and for an empty trimmed name, matching the chrome-side
    /// rename-commit rule that empty names are discarded.
    fn rename(&mut self, id: TabId, name: String) {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return;
        }
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            return;
        };
        if let TabContent::Session(session) = &mut tab.content {
            session.label = trimmed.to_owned();
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
    /// need a live PTY. `pub(crate)` so `app.rs`'s headless UI tests can also
    /// build a `FesTermApp` without a live PTY session.
    pub(crate) fn for_test() -> Self {
        Self::for_test_with_configuration(Configuration::empty())
    }

    pub(crate) fn for_test_with_configuration(configuration: Configuration) -> Self {
        Self::with_launcher(configuration)
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
    fn local_profile_secondary_uses_the_launch_executable_name() {
        let profile = LocalProfile::new("C:/Windows/System32/cmd.exe");

        assert_eq!(
            local_profile_secondary(&profile).as_deref(),
            Some("cmd.exe")
        );
    }

    #[test]
    fn configured_local_profile_command_uses_metadata_and_stable_profile_id() {
        let context = egui::Context::default();
        let configuration = Configuration::new(vec![festerm_config::Profile::local(
            "development",
            "festerm-profile-test-command-that-does-not-exist",
            vec!["--interactive".to_owned()],
            None,
        )
        .expect("test local profile is valid")])
        .expect("test configuration is valid");
        let mut state = AppState::for_test_with_configuration(configuration);
        let launcher_id = state.active();

        state.dispatch(
            AppCommand::StartConfiguredLocalProfile {
                profile_id: "development".to_owned(),
            },
            &context,
        );

        assert_eq!(state.tabs().len(), 1);
        assert_eq!(state.active(), launcher_id);
        let TabContent::Session(session) = &state.active_tab().content else {
            panic!("configured local profile must replace the active launcher");
        };
        assert_eq!(session.label, "development");
        assert_eq!(
            session.launch_secondary.as_deref(),
            Some("festerm-profile-test-command-that-does-not-exist")
        );
        assert_eq!(session.profile_identifier.as_deref(), Some("development"));
        assert!(
            session.controller.start_error().is_some(),
            "the nonexistent executable proves the configured launch path attempted its metadata"
        );
    }

    #[test]
    fn configured_local_profile_command_ignores_an_ssh_profile() {
        let context = egui::Context::default();
        let configuration = Configuration::new(vec![festerm_config::Profile::ssh(
            "production",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .expect("test SSH profile is valid")])
        .expect("test configuration is valid");
        let mut state = AppState::for_test_with_configuration(configuration);
        let launcher_id = state.active();

        state.dispatch(
            AppCommand::StartConfiguredLocalProfile {
                profile_id: "production".to_owned(),
            },
            &context,
        );

        assert_eq!(state.active(), launcher_id);
        assert!(matches!(state.active_tab().content, TabContent::Launcher));
    }

    #[test]
    fn replacing_configuration_changes_only_future_launcher_choices() {
        let context = egui::Context::default();
        let original = Configuration::new(vec![festerm_config::Profile::local(
            "original",
            "festerm-profile-test-command-that-does-not-exist",
            Vec::new(),
            None,
        )
        .expect("original local profile is valid")])
        .expect("original configuration is valid");
        let replacement = Configuration::new(vec![festerm_config::Profile::local(
            "replacement",
            "festerm-replacement-test-command-that-does-not-exist",
            Vec::new(),
            None,
        )
        .expect("replacement local profile is valid")])
        .expect("replacement configuration is valid");
        let mut state = AppState::for_test_with_configuration(original);

        state.dispatch(
            AppCommand::StartConfiguredLocalProfile {
                profile_id: "original".to_owned(),
            },
            &context,
        );
        let active = state.active();
        state.replace_configuration(replacement);

        assert_eq!(state.active(), active);
        let TabContent::Session(session) = &state.active_tab().content else {
            panic!("the already running configured session must remain a session");
        };
        assert_eq!(session.label, "original");
        assert!(state.configuration().profile("original").is_none());
        assert!(state.configuration().profile("replacement").is_some());
    }

    fn restored_workspace_configuration(
        focused_tab_id: Option<&str>,
    ) -> (Configuration, WorkspaceConfiguration) {
        let workspace = WorkspaceConfiguration::new(
            vec![
                WorkspaceTab::launcher("launcher").expect("launcher tab is valid"),
                WorkspaceTab::local_session("local", "development").expect("local tab is valid"),
                WorkspaceTab::ssh_session("remote", "production").expect("SSH tab is valid"),
                WorkspaceTab::settings("settings").expect("settings tab is valid"),
            ],
            focused_tab_id.map(str::to_owned),
        )
        .expect("workspace is valid");
        let configuration = Configuration::new_with_workspace(
            vec![
                festerm_config::Profile::local(
                    "development",
                    "festerm-workspace-local-profile-that-does-not-exist",
                    Vec::new(),
                    None,
                )
                .expect("local profile is valid"),
                festerm_config::Profile::ssh(
                    "production",
                    "ssh.example.test",
                    2200,
                    "deploy",
                    "xterm-256color",
                    100,
                    40,
                )
                .expect("SSH profile is valid"),
            ],
            workspace.clone(),
        )
        .expect("configuration is valid");
        (configuration, workspace)
    }

    #[test]
    fn workspace_restoration_recreates_ordered_fresh_local_tabs_and_focus() {
        let context = egui::Context::default();
        let (configuration, workspace) = restored_workspace_configuration(Some("local"));

        let state = AppState::with_restored_workspace(&context, configuration, &workspace);

        assert_eq!(state.tabs().len(), 4, "no default session is added");
        assert!(matches!(state.tabs()[0].content, TabContent::Launcher));
        let TabContent::Session(local) = &state.tabs()[1].content else {
            panic!("the saved local tab starts a fresh local session");
        };
        assert_eq!(local.label, "development");
        assert_eq!(
            local.launch_secondary.as_deref(),
            Some("festerm-workspace-local-profile-that-does-not-exist")
        );
        assert!(
            local.controller.start_error().is_some(),
            "the nonexistent executable proves startup used fresh profile metadata"
        );
        assert!(matches!(
            state.tabs()[2].content,
            TabContent::SshAuthenticationRequired(_)
        ));
        assert!(matches!(state.tabs()[3].content, TabContent::Settings));
        assert_eq!(state.active(), state.tabs()[1].id);
    }

    #[test]
    fn workspace_restoration_without_focus_selects_the_first_saved_tab() {
        let context = egui::Context::default();
        let (configuration, workspace) = restored_workspace_configuration(None);

        let state = AppState::with_restored_workspace(&context, configuration, &workspace);

        assert_eq!(state.active(), state.tabs()[0].id);
        assert!(matches!(state.active_tab().content, TabContent::Launcher));
    }

    #[test]
    fn restored_ssh_tab_requires_fresh_authentication_without_starting_a_session() {
        let context = egui::Context::default();
        let (configuration, workspace) = restored_workspace_configuration(Some("remote"));

        let mut state = AppState::with_restored_workspace(&context, configuration, &workspace);

        assert_eq!(state.active(), state.tabs()[2].id);
        let TabContent::SshAuthenticationRequired(tab) = &state.active_tab().content else {
            panic!("saved SSH metadata must restore an authentication-required surface");
        };
        assert_eq!(tab.profile.identifier(), "production");
        assert_eq!(tab.profile.host(), "ssh.example.test");
        assert_eq!(tab.profile.port(), 2200);
        assert_eq!(tab.profile.username(), "deploy");
        assert!(
            state.session_tabs_mut().next().is_some(),
            "only the separately restored local profile started a session"
        );
        assert_eq!(
            state.session_tabs_mut().count(),
            1,
            "the SSH restoration starts no network transport"
        );
    }

    #[test]
    fn workspace_capture_preserves_order_focus_and_omits_nonrestorable_sessions() {
        let context = egui::Context::default();
        let configuration = Configuration::new(vec![
            festerm_config::Profile::local(
                "development",
                "festerm-workspace-capture-command-that-does-not-exist",
                Vec::new(),
                None,
            )
            .unwrap(),
            festerm_config::Profile::ssh(
                "production",
                "ssh.example.test",
                22,
                "deploy",
                "xterm-256color",
                80,
                24,
            )
            .unwrap(),
        ])
        .unwrap();
        let mut state = AppState::for_test_with_configuration(configuration);
        state.dispatch(
            AppCommand::StartConfiguredLocalProfile {
                profile_id: "development".to_owned(),
            },
            &context,
        );
        state.dispatch(AppCommand::OpenSettings, &context);
        let settings = state.active();
        state.dispatch(AppCommand::OpenLauncher, &context);
        state.dispatch(AppCommand::StartLocalSession, &context);
        let TabContent::Session(ad_hoc) = &mut state.active_tab_mut().content else {
            panic!("default launch creates a session");
        };
        assert_eq!(ad_hoc.profile_identifier, None);
        let ssh = state
            .configuration()
            .profile("production")
            .and_then(festerm_config::Profile::as_ssh)
            .unwrap()
            .clone();
        state.tabs.push(Tab {
            id: TabId::next(),
            content: TabContent::SshAuthenticationRequired(SshAuthenticationRequiredTab {
                profile: ssh,
            }),
        });
        state.active = settings;

        let captured = state.capture_workspace_configuration().unwrap();
        let workspace = captured.workspace().unwrap();

        assert_eq!(
            workspace
                .tabs()
                .iter()
                .map(WorkspaceTab::identifier)
                .collect::<Vec<_>>(),
            ["tab-1", "tab-2", "tab-3"]
        );
        assert!(matches!(workspace.tabs()[0], WorkspaceTab::LocalSession(_)));
        assert!(matches!(workspace.tabs()[1], WorkspaceTab::Settings(_)));
        assert!(matches!(workspace.tabs()[2], WorkspaceTab::SshSession(_)));
        assert_eq!(workspace.focused_tab_id(), Some("tab-2"));
        assert_eq!(captured.profiles(), state.configuration().profiles());
    }

    #[test]
    fn workspace_capture_uses_a_launcher_when_every_tab_is_omitted() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        state.dispatch(AppCommand::StartLocalSession, &context);

        let captured = state.capture_workspace_configuration().unwrap();
        let workspace = captured.workspace().unwrap();

        assert!(matches!(workspace.tabs(), [WorkspaceTab::Launcher(_)]));
        assert_eq!(workspace.focused_tab_id(), Some("tab-1"));
        state.dispatch(AppCommand::CloseTab(state.active()), &context);
    }

    #[test]
    fn closing_the_final_restored_ssh_surface_returns_to_the_launcher() {
        let context = egui::Context::default();
        let workspace = WorkspaceConfiguration::new(
            vec![WorkspaceTab::ssh_session("remote", "production").expect("SSH tab is valid")],
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
            workspace.clone(),
        )
        .expect("configuration is valid");
        let mut state = AppState::with_restored_workspace(&context, configuration, &workspace);
        let restored_tab = state.active();

        state.dispatch(AppCommand::CloseTab(restored_tab), &context);

        assert_eq!(state.tabs().len(), 1);
        assert!(matches!(state.active_tab().content, TabContent::Launcher));
    }

    fn ssh_profile() -> SshConnectionProfile {
        SshConnectionProfile::new(
            festerm_ssh::HostIdentity::new("192.0.2.1", 22).expect("test host is valid"),
            "test-user",
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            TerminalSize::new(80, 24).expect("test size is valid"),
        )
        .expect("test profile is valid")
    }

    #[test]
    fn application_host_key_decisions_cannot_request_persistence() {
        assert_eq!(
            HostTrustDecision::from(HostKeyTrustDecision::Reject),
            HostTrustDecision::Reject
        );
        assert_eq!(
            HostTrustDecision::from(HostKeyTrustDecision::AcceptOnce),
            HostTrustDecision::AcceptOnce
        );
    }

    #[test]
    fn ssh_startup_failure_uses_the_existing_no_session_fallback() {
        let dimensions = Dimensions::new(80, 24).expect("test dimensions are valid");
        let session = SessionTab::from_ssh_session_result(
            Err(SshSessionStartError),
            dimensions,
            "test-user@example.invalid",
            Some("SSH · example.invalid:22".to_owned()),
            InspectorTransport::Ssh {
                username: "test-user".to_owned(),
                host: "example.invalid".to_owned(),
                port: 22,
            },
        );

        assert_eq!(
            session.controller.start_error(),
            Some("could not start SSH worker thread")
        );
        assert!(
            session
                .controller
                .status_line()
                .starts_with("SSH session unavailable:"),
            "SSH failures use the controller's ordinary startup-error state"
        );
        assert!(session
            .terminal
            .row_text(0)
            .is_some_and(|row| row.starts_with("SSH session could not start.")));
    }

    #[test]
    fn ssh_command_replaces_the_active_launcher_without_a_live_server() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let launcher_id = state.active();
        let secret = "transient-test-password";
        let command = AppCommand::StartSshSession {
            profile: ssh_profile(),
            authentication: SshAuthentication::password(secret),
            options: SshSessionOptions::new(),
        };

        assert!(
            !format!("{command:?}").contains(secret),
            "transient authentication must stay redacted in command debug output"
        );
        state.dispatch(command, &context);

        assert_eq!(state.tabs().len(), 1);
        assert_eq!(state.active(), launcher_id);
        let TabContent::Session(session) = &state.active_tab().content else {
            panic!("SSH command must place a session tab");
        };
        assert_eq!(session.label, "test-user@192.0.2.1");
        assert_eq!(session.system_label(), "Remote");
        assert!(matches!(
            session.controller.session(),
            Some(ApplicationSession::Ssh(_))
        ));

        state.dispatch(AppCommand::CloseTab(launcher_id), &context);
    }

    #[test]
    fn reconnect_command_ignores_a_non_ssh_tab() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let launcher = state.active();

        state.dispatch(AppCommand::ReconnectSession(launcher), &context);

        assert_eq!(state.active(), launcher);
        assert_eq!(state.tabs().len(), 1);
        assert!(matches!(state.active_tab().content, TabContent::Launcher));
    }

    #[test]
    fn new_launcher_command_reactivates_the_singleton() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let initial = state.active();

        state.dispatch(AppCommand::OpenLauncher, &context);

        assert_eq!(state.tabs().len(), 1);
        assert_eq!(state.active(), initial);
        assert_eq!(launcher_ids(&state), vec![initial]);
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

        state.dispatch(AppCommand::OpenSettings, &context);
        let unknown = TabId::next();
        state.dispatch(AppCommand::ActivateTab(unknown), &context);

        assert_ne!(state.active(), initial);
        assert_ne!(state.active(), unknown);
    }

    #[test]
    fn starting_a_local_session_from_the_active_launcher_replaces_it_in_place() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let launcher_id = state.active();
        assert!(matches!(state.active_tab().content, TabContent::Launcher));

        state.dispatch(AppCommand::StartLocalSession, &context);

        assert_eq!(
            state.tabs().len(),
            1,
            "the launcher tab is replaced, not left behind alongside a new session tab"
        );
        assert_eq!(
            state.active(),
            launcher_id,
            "the session tab keeps the launcher's identity/position"
        );
        assert!(matches!(state.active_tab().content, TabContent::Session(_)));
    }

    #[test]
    fn starting_a_local_session_from_a_non_launcher_active_tab_opens_a_new_tab() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        state.dispatch(AppCommand::OpenSettings, &context);
        assert!(matches!(state.active_tab().content, TabContent::Settings));

        state.dispatch(AppCommand::StartLocalSession, &context);

        assert_eq!(
            state.tabs().len(),
            3,
            "Settings and Launcher tabs are left untouched; the new session tab is added"
        );
        assert!(matches!(state.active_tab().content, TabContent::Session(_)));
    }

    #[test]
    fn toggle_chip_layout_flips_between_wrap_and_single_row_scroll() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        assert_eq!(state.chip_layout(), ChipLayout::SingleRowScroll);

        state.dispatch(AppCommand::ToggleChipLayout, &context);
        assert_eq!(state.chip_layout(), ChipLayout::Wrap);

        state.dispatch(AppCommand::ToggleChipLayout, &context);
        assert_eq!(state.chip_layout(), ChipLayout::SingleRowScroll);
    }

    #[test]
    fn toggle_status_bar_flips_visibility_and_defaults_to_shown() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        assert!(state.status_bar_visible());

        state.dispatch(AppCommand::ToggleStatusBar, &context);
        assert!(!state.status_bar_visible());

        state.dispatch(AppCommand::ToggleStatusBar, &context);
        assert!(state.status_bar_visible());
    }

    #[test]
    fn toggle_inspector_flips_state() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        assert!(!state.inspector_open());

        state.dispatch(AppCommand::ToggleSessionInspector, &context);
        assert!(!state.inspector_open(), "Launcher has no session inspector");

        state.dispatch(AppCommand::StartLocalSession, &context);

        state.dispatch(AppCommand::ToggleSessionInspector, &context);
        assert!(state.inspector_open());

        state.dispatch(AppCommand::OpenSettings, &context);
        assert!(matches!(state.active_tab().content, TabContent::Settings));
        assert!(!state.inspector_open());

        state.dispatch(AppCommand::ActivatePreviousTab, &context);
        assert!(!state.inspector_open(), "the inspector must not resurrect");

        state.dispatch(AppCommand::ToggleSessionInspector, &context);
        assert!(state.inspector_open());
    }

    #[test]
    fn closing_the_active_tab_reactivates_a_neighbor() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let first = state.active();

        state.dispatch(AppCommand::OpenSettings, &context);
        let second = state.active();
        assert_ne!(first, second);

        state.dispatch(AppCommand::CloseTab(second), &context);
        assert_eq!(state.tabs().len(), 1);
        assert_eq!(state.active(), first);
    }

    #[test]
    fn activate_next_and_previous_wrap_around_in_stable_list_order() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let first = state.active();
        state.dispatch(AppCommand::OpenSettings, &context);
        let second = state.active();
        state.dispatch(AppCommand::StartLocalSession, &context);
        let third = state.active();

        state.dispatch(AppCommand::ActivateTab(first), &context);
        state.dispatch(AppCommand::ActivateNextTab, &context);
        assert_eq!(state.active(), second);
        state.dispatch(AppCommand::ActivateNextTab, &context);
        assert_eq!(state.active(), third);
        state.dispatch(AppCommand::ActivateNextTab, &context);
        assert_eq!(state.active(), first, "next wraps back to the start");

        state.dispatch(AppCommand::ActivatePreviousTab, &context);
        assert_eq!(state.active(), third, "previous wraps back to the end");
    }

    #[test]
    fn reorder_moves_a_tab_before_a_target_without_changing_active_tab() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let first = state.active();
        state.dispatch(AppCommand::OpenSettings, &context);
        let second = state.active();
        state.dispatch(AppCommand::ActivateTab(first), &context);

        // Order is [first, second]; move second before first.
        state.dispatch(
            AppCommand::ReorderTab {
                moved: second,
                before: Some(first),
            },
            &context,
        );

        let order: Vec<TabId> = state.tabs().iter().map(|tab| tab.id).collect();
        assert_eq!(order, vec![second, first]);
        assert_eq!(
            state.active(),
            first,
            "reordering must not change which tab is active"
        );
    }

    #[test]
    fn reorder_to_the_end_when_before_is_none() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let first = state.active();
        state.dispatch(AppCommand::OpenSettings, &context);
        let second = state.active();

        state.dispatch(
            AppCommand::ReorderTab {
                moved: first,
                before: None,
            },
            &context,
        );

        let order: Vec<TabId> = state.tabs().iter().map(|tab| tab.id).collect();
        assert_eq!(order, vec![second, first]);
    }

    #[test]
    fn reorder_ignores_an_unknown_moved_id_or_moving_before_itself() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let only = state.active();
        let unknown = TabId::next();

        state.dispatch(
            AppCommand::ReorderTab {
                moved: unknown,
                before: Some(only),
            },
            &context,
        );
        assert_eq!(state.tabs().len(), 1);

        state.dispatch(
            AppCommand::ReorderTab {
                moved: only,
                before: Some(only),
            },
            &context,
        );
        assert_eq!(state.tabs()[0].id, only);
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
