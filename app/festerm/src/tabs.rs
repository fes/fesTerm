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

use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use eframe::egui;
use festerm_config::{
    ChipLayoutPreference, ConfigError, Configuration, EmojiPresentationPreference,
    InterfaceSettings, PersistenceConfiguration, PersistenceProviderKind, ScrollSpeedPreference,
    ScrollbackLimitPreference, SftpPaneOrderPreference, SshProfileConfiguration,
    TerminalFontPreference, WorkspaceConfiguration, WorkspaceTab,
};
use festerm_core::{Dimensions, Terminal};
use festerm_pty::{default_local_profile, LocalProfile, LocalPtySession};
use festerm_secret_store::{SecretBytes, SecretStore};
use festerm_serial::{LineSettings, SerialSession, SerialSessionError};
use festerm_session::{
    HostKeyPrompt, PasswordPrompt, Session, SessionErrorKind, SessionEvent, SessionEventNotifier,
    SessionId, SessionLifecycle, SessionMetrics, SessionSendError, SessionTryReceiveError,
    ShutdownError, ShutdownResult, SshPortForwardDirection, SshPortForwardRuntime,
    SshPortForwardState, TerminalSize,
};
use festerm_sessiond::PersistentSession;
use festerm_ssh::{
    HostKeyDecisionResolutionError, HostTrustDecision, PasswordDecisionResolutionError,
    SessionStrategy, SftpTerminalSession, SftpTerminalSessionStartError, SshAuthentication,
    SshConnectionProfile, SshLivenessCheckError, SshPortForwardRequestError, SshPortForwardSpec,
    SshReconnectError, SshSession, SshSessionOptions, SshSessionStartError,
};
use festerm_ui_egui::{
    chrome::{ChipLayout, ChipStatus},
    TerminalView,
};

use crate::markdown_viewer::MarkdownViewerTab;
use crate::session_controller::{seed_session_startup_failure, terminal_size, SessionController};
use crate::sftp_file_manager::{
    SftpFileManagerAuthentication, SftpFileManagerLaunchTarget, SftpFileManagerTab,
};

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
    Persistent(PersistentSession),
    Ssh(SshSession),
    Sftp(SftpTerminalSession),
    Serial(SerialSession),
    #[cfg(test)]
    TestSsh(crate::session_controller::fake::FakeSshSession),
}

/// The host-key decisions exposed by the application (ADR 0020).
///
/// `AcceptAndPersist` accepts the current connection exactly like
/// `AcceptOnce` at the SSH transport (`festerm-ssh` already treats both
/// identically for one connection); the durable trust-record write itself
/// is a composition-root concern (secret-store-adjacent, needs the
/// configuration reloader) and is intercepted in `FesTermApp::screen_command`
/// rather than handled inside `AppState::dispatch`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostKeyTrustDecision {
    Reject,
    AcceptOnce,
    AcceptAndPersist,
}

impl From<HostKeyTrustDecision> for HostTrustDecision {
    fn from(value: HostKeyTrustDecision) -> Self {
        match value {
            HostKeyTrustDecision::Reject => Self::Reject,
            HostKeyTrustDecision::AcceptOnce => Self::AcceptOnce,
            HostKeyTrustDecision::AcceptAndPersist => Self::AcceptAndPersist,
        }
    }
}

/// An application-level failure to resolve a displayed host-key request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostKeyTrustResolutionError {
    NoPendingPrompt,
    NotRemoteSession,
    Transport(HostKeyDecisionResolutionError),
}

impl std::fmt::Display for HostKeyTrustResolutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPendingPrompt => formatter.write_str("no host-key prompt is pending"),
            Self::NotRemoteSession => {
                formatter.write_str("the tab is not a remote SSH/SFTP session")
            }
            Self::Transport(error) => error.fmt(formatter),
        }
    }
}

/// An application-level failure to resolve a displayed interactive password
/// request. Mirrors [`HostKeyTrustResolutionError`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PasswordResolutionError {
    NoPendingPrompt,
    NotRemoteSession,
    Transport(PasswordDecisionResolutionError),
}

impl std::fmt::Display for PasswordResolutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPendingPrompt => formatter.write_str("no password prompt is pending"),
            Self::NotRemoteSession => {
                formatter.write_str("the tab is not a remote SSH/SFTP session")
            }
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
        let resolver = match self {
            Self::Ssh(session) => session.host_key_decision_resolver(),
            Self::Sftp(session) => session.host_key_decision_resolver(),
            Self::Local(_) | Self::Persistent(_) | Self::Serial(_) => {
                return Err(HostKeyTrustResolutionError::NotRemoteSession)
            }
            #[cfg(test)]
            Self::TestSsh(_) => return Err(HostKeyTrustResolutionError::NotRemoteSession),
        };
        resolver
            .resolve(prompt, decision.into())
            .map_err(HostKeyTrustResolutionError::Transport)
    }

    /// Resolves an interactive password prompt through an SSH session
    /// without exposing its resolver to GUI code. Mirrors
    /// [`Self::resolve_host_key_prompt`].
    pub fn resolve_password_prompt(
        &self,
        prompt: &PasswordPrompt,
        password: String,
    ) -> Result<(), PasswordResolutionError> {
        let resolver = match self {
            Self::Ssh(session) => session.password_decision_resolver(),
            Self::Sftp(session) => session.password_decision_resolver(),
            Self::Local(_) | Self::Persistent(_) | Self::Serial(_) => {
                return Err(PasswordResolutionError::NotRemoteSession)
            }
            #[cfg(test)]
            Self::TestSsh(_) => return Err(PasswordResolutionError::NotRemoteSession),
        };
        resolver
            .resolve(prompt, password)
            .map_err(PasswordResolutionError::Transport)
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

    /// Queues one on-demand SSH-level liveness probe (ADR 0018). A local
    /// session has no transport to probe, so this is always a silent no-op
    /// for it rather than an error: callers triggering this across every
    /// open tab (e.g. after an OS wake/network-change signal) should not
    /// have to distinguish tab kinds first.
    fn try_check_liveness(&self) -> Result<(), SshLivenessCheckError> {
        match self {
            Self::Ssh(session) => session.try_check_liveness(),
            Self::Local(_) | Self::Persistent(_) | Self::Sftp(_) | Self::Serial(_) => Ok(()),
            #[cfg(test)]
            Self::TestSsh(_) => Ok(()),
        }
    }

    /// Whether this session currently supports live SSH port-forward operations.
    pub fn live_port_forwarding_available(&self) -> bool {
        match self {
            Self::Ssh(session) => session.lifecycle() == SessionLifecycle::Running,
            #[cfg(test)]
            Self::TestSsh(session) => session.lifecycle() == SessionLifecycle::Running,
            Self::Local(_) | Self::Persistent(_) | Self::Sftp(_) | Self::Serial(_) => false,
        }
    }

    /// Adds one live-only SSH port forward to the connected session.
    pub fn try_add_port_forward(
        &self,
        forward: impl SshPortForwardSpec,
    ) -> Result<(), SshPortForwardRequestError> {
        match self {
            Self::Ssh(session) => session.try_add_port_forward(forward),
            #[cfg(test)]
            Self::TestSsh(session) => session.try_add_port_forward(forward),
            Self::Local(_) | Self::Persistent(_) | Self::Sftp(_) | Self::Serial(_) => {
                Err(SshPortForwardRequestError::NotRunning)
            }
        }
    }

    /// Removes one SSH port forward identified by its listening bind tuple.
    pub fn try_remove_port_forward(
        &self,
        direction: SshPortForwardDirection,
        bind_host: impl Into<String>,
        bind_port: u16,
    ) -> Result<(), SshPortForwardRequestError> {
        let bind_host = bind_host.into();
        match self {
            Self::Ssh(session) => session.try_remove_port_forward(direction, bind_host, bind_port),
            #[cfg(test)]
            Self::TestSsh(session) => {
                session.try_remove_port_forward(direction, bind_host, bind_port)
            }
            Self::Local(_) | Self::Persistent(_) | Self::Sftp(_) | Self::Serial(_) => {
                Err(SshPortForwardRequestError::NotRunning)
            }
        }
    }

    /// Requests a fresh snapshot of the connected SSH session's active port forwards.
    pub fn try_query_port_forwards(&self) -> Result<(), SshPortForwardRequestError> {
        match self {
            Self::Ssh(session) => session.try_query_port_forwards(),
            #[cfg(test)]
            Self::TestSsh(session) => session.try_query_port_forwards(),
            Self::Local(_) | Self::Persistent(_) | Self::Sftp(_) | Self::Serial(_) => {
                Err(SshPortForwardRequestError::NotRunning)
            }
        }
    }

    pub fn sftp_working_directories(&self) -> Option<(String, PathBuf)> {
        match self {
            Self::Sftp(session) => session.working_directories().map(|directories| {
                (
                    directories.remote().to_owned(),
                    directories.local().to_path_buf(),
                )
            }),
            Self::Local(_) | Self::Persistent(_) | Self::Ssh(_) | Self::Serial(_) => None,
            #[cfg(test)]
            Self::TestSsh(_) => None,
        }
    }
}

impl Session for ApplicationSession {
    fn id(&self) -> SessionId {
        match self {
            Self::Local(session) => session.id(),
            Self::Persistent(session) => session.id(),
            Self::Ssh(session) => session.id(),
            Self::Sftp(session) => session.id(),
            Self::Serial(session) => session.id(),
            #[cfg(test)]
            Self::TestSsh(session) => session.id(),
        }
    }

    fn lifecycle(&self) -> SessionLifecycle {
        match self {
            Self::Local(session) => session.lifecycle(),
            Self::Persistent(session) => session.lifecycle(),
            Self::Ssh(session) => session.lifecycle(),
            Self::Sftp(session) => session.lifecycle(),
            Self::Serial(session) => session.lifecycle(),
            #[cfg(test)]
            Self::TestSsh(session) => session.lifecycle(),
        }
    }

    fn metrics(&self) -> SessionMetrics {
        match self {
            Self::Local(session) => session.metrics(),
            Self::Persistent(session) => session.metrics(),
            Self::Ssh(session) => session.metrics(),
            Self::Sftp(session) => session.metrics(),
            Self::Serial(session) => session.metrics(),
            #[cfg(test)]
            Self::TestSsh(session) => session.metrics(),
        }
    }

    fn try_send_input(&self, bytes: &[u8]) -> Result<(), SessionSendError> {
        match self {
            Self::Local(session) => session.try_send_input(bytes),
            Self::Persistent(session) => session.try_send_input(bytes),
            Self::Ssh(session) => session.try_send_input(bytes),
            Self::Sftp(session) => session.try_send_input(bytes),
            Self::Serial(session) => session.try_send_input(bytes),
            #[cfg(test)]
            Self::TestSsh(session) => session.try_send_input(bytes),
        }
    }

    fn try_resize(&self, size: TerminalSize) -> Result<(), SessionSendError> {
        match self {
            Self::Local(session) => session.try_resize(size),
            Self::Persistent(session) => session.try_resize(size),
            Self::Ssh(session) => session.try_resize(size),
            Self::Sftp(session) => session.try_resize(size),
            Self::Serial(session) => session.try_resize(size),
            #[cfg(test)]
            Self::TestSsh(session) => session.try_resize(size),
        }
    }

    fn try_shutdown(&self) -> Result<(), SessionSendError> {
        match self {
            Self::Local(session) => session.try_shutdown(),
            Self::Persistent(session) => session.try_shutdown(),
            Self::Ssh(session) => session.try_shutdown(),
            Self::Sftp(session) => session.try_shutdown(),
            Self::Serial(session) => session.try_shutdown(),
            #[cfg(test)]
            Self::TestSsh(session) => session.try_shutdown(),
        }
    }

    fn try_recv_event(&self) -> Result<SessionEvent, SessionTryReceiveError> {
        match self {
            Self::Local(session) => session.try_recv_event(),
            Self::Persistent(session) => session.try_recv_event(),
            Self::Ssh(session) => session.try_recv_event(),
            Self::Sftp(session) => session.try_recv_event(),
            Self::Serial(session) => session.try_recv_event(),
            #[cfg(test)]
            Self::TestSsh(session) => session.try_recv_event(),
        }
    }

    fn shutdown(&self, timeout: std::time::Duration) -> Result<ShutdownResult, ShutdownError> {
        match self {
            Self::Local(session) => session.shutdown(timeout),
            Self::Persistent(session) => session.shutdown(timeout),
            Self::Ssh(session) => session.shutdown(timeout),
            Self::Sftp(session) => session.shutdown(timeout),
            Self::Serial(session) => session.shutdown(timeout),
            #[cfg(test)]
            Self::TestSsh(session) => session.shutdown(timeout),
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
    /// Whether the one-shot "scrollback limit reached" transient notice has
    /// already been shown for this tab (M9: eviction notices). Latches once
    /// `true` so continued eviction from a sustained-output workload does
    /// not repeatedly re-trigger the notice every frame.
    pub eviction_notice_shown: bool,
    /// Present only when this session authenticated with a plain password.
    /// See [`SshPasswordRetryState`].
    pub ssh_password_retry: Option<SshPasswordRetryState>,
    /// Whether this session has emitted output since the tab was last the
    /// active/focused tab (feature request #68). Set whenever output is
    /// pumped into a *non-active* tab's terminal; cleared whenever the tab
    /// becomes active. Purely a presentation cue for the chip's slow-pulse
    /// animation — it never changes `ChipStatus`/connection-state semantics.
    pub has_new_output_since_active: bool,
    /// Terminal-content find-bar state (`docs/gui-design.md`
    /// "Terminal-content search"). Never logged or persisted.
    pub search: crate::search::TerminalSearchState,
}

/// Exact per-transport counts of sessions that would lose something by
/// closing, used to build the aggregate application-quit confirmation
/// message (`docs/gui-design.md` "Closing sessions and quitting").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveSessionCounts {
    pub local: usize,
    pub ssh: usize,
    pub serial: usize,
}

impl LiveSessionCounts {
    pub const fn total(&self) -> usize {
        self.local + self.ssh + self.serial
    }
}

/// Narrow transport metadata safe for application chrome. Keeping this owned
/// by the tab prevents egui code from reaching into PTY or SSH backends.
pub enum InspectorTransport {
    Local {
        persistence: Option<InspectorPersistence>,
    },
    Ssh {
        username: String,
        host: String,
        port: u16,
        /// The durable remote-session provider and name this connection
        /// attaches to or creates, if any (ADR 0018). `None` means this is
        /// an ordinary manual-recovery plain shell. This drives the
        /// Reconnect-vs-Resume language distinction in application chrome;
        /// it is presentation metadata only and never itself a live process
        /// or connection handle.
        persistence: Option<InspectorPersistence>,
    },
    Sftp {
        username: String,
        host: String,
        port: u16,
    },
    Serial {
        device: String,
        baud_rate: u32,
        data_bits: festerm_config::SerialDataBits,
        parity: festerm_config::SerialParity,
        stop_bits: festerm_config::SerialStopBits,
        flow_control: festerm_config::SerialFlowControl,
    },
}

/// Non-secret durable-session facts surfaced to application chrome.
#[derive(Clone)]
pub struct InspectorPersistence {
    pub provider_label: &'static str,
    pub session_name: String,
}

/// A restored SSH workspace surface that deliberately has no live session.
///
/// Workspace metadata contains destination details but never authentication or
/// trust material, so restoration must return the user to the transient
/// authentication form instead of starting a transport.
pub struct SshAuthenticationRequiredTab {
    pub profile: SshProfileConfiguration,
}

/// A restored SFTP workspace surface that deliberately has no live session.
///
/// Workspace metadata contains destination details but never authentication or
/// trust material, so restoration must return the user to the transient
/// authentication form instead of starting a transport.
pub struct SftpAuthenticationRequiredTab {
    pub profile: SshProfileConfiguration,
}

/// A GUI SFTP file-manager launch surface that has destination metadata but no
/// live authenticated subsystem yet.
pub struct SftpFileManagerAuthenticationRequiredTab {
    pub target: SftpFileManagerLaunchTarget,
}

/// Maximum number of in-connection password reprompts before falling back
/// to the ordinary failed-session presentation, matching `ssh`'s own
/// default `NumberOfPasswordPrompts` limit. Interactive sessions bound their
/// own in-connection attempts at `festerm_ssh::MAX_INTERACTIVE_PASSWORD_ATTEMPTS`;
/// this bounds the outer, full-reconnect retry episode that follows an
/// explicit typed-password rejection.
pub const MAX_SSH_PASSWORD_PROMPT_ATTEMPTS: u8 = 3;

/// Retained only for a plain-password-authenticated SSH session (never
/// public-key or native-stored-password): a rejected private key or stored
/// password is a configuration problem, not something retyping in this tab
/// fixes. Lets a rejected password reprompt in-tab instead of surfacing a
/// raw failed session, bounded by `MAX_SSH_PASSWORD_PROMPT_ATTEMPTS`.
pub struct SshPasswordRetryState {
    pub profile: SshConnectionProfile,
    pub profile_identifier: Option<String>,
    pub options: SshSessionOptions,
    pub attempts: u8,
}

/// Groups `from_session_result`'s non-outcome parameters so the function
/// stays under clippy's argument-count lint; `from_local_session_result`
/// and `from_ssh_session_result` each build one of these instead of passing
/// their transport-specific fields through separately.
struct SessionResultMeta<'a> {
    label: &'a str,
    launch_secondary: Option<String>,
    profile_identifier: Option<String>,
    session_name: &'static str,
    inspector_transport: InspectorTransport,
    ssh_password_retry: Option<SshPasswordRetryState>,
}

impl SessionTab {
    fn set_scrollback_limit(&mut self, preference: ScrollbackLimitPreference) {
        self.terminal.set_scrollback_limit(preference.bytes());
    }

    /// Starts a fresh default local session. `window_dimensions`, when
    /// supplied, is the currently rendered window's terminal grid size (see
    /// `AppState::current_session_dimensions`); a new tab created from an
    /// already-sized window should start at that size rather than the
    /// application's baseline default, so it matches the window instead of
    /// visibly snapping it back to 80x24 on the very next resize.
    fn start_default(context: &egui::Context, window_dimensions: Option<Dimensions>) -> Self {
        let dimensions = window_dimensions
            .unwrap_or_else(|| Dimensions::new(80, 24).expect("default dimensions are valid"));
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
            None,
        )
    }

    /// Starts a local PTY from reusable, secret-free profile metadata.
    /// `window_dimensions` behaves like it does for `start_default`: a new
    /// tab opened against an already-sized window starts at that size.
    fn start_local_profile(
        profile: LocalProfile,
        profile_id: &str,
        persistence: Option<&PersistenceConfiguration>,
        context: &egui::Context,
        window_dimensions: Option<Dimensions>,
    ) -> Self {
        let profile = crate::environment::with_corrected_local_path(profile);
        let dimensions = window_dimensions
            .unwrap_or_else(|| Dimensions::new(80, 24).expect("default dimensions are valid"));
        let size = terminal_size(dimensions).expect("default dimensions fit PTY limits");
        let launch_secondary = local_profile_secondary(&profile);
        let inspector_persistence = persistence.map(|persistence| InspectorPersistence {
            provider_label: persistence.provider().label(),
            session_name: persistence.session_name().to_owned(),
        });
        let result = match persistence {
            Some(persistence)
                if persistence.provider() == PersistenceProviderKind::FestermSessiond =>
            {
                PersistentSession::start_with_notifier(
                    persistence.session_name(),
                    &profile,
                    size,
                    make_notifier(context),
                )
                .map(ApplicationSession::Persistent)
                .map_err(|error| error.to_string())
            }
            _ => LocalPtySession::start_with_notifier(profile, size, make_notifier(context))
                .map(ApplicationSession::Local)
                .map_err(|error| error.to_string()),
        };
        Self::from_local_session_result(
            result,
            dimensions,
            profile_id,
            launch_secondary,
            Some(profile_id.to_owned()),
            inspector_persistence,
        )
    }

    /// Attaches to an already-running, unattached `festerm-sessiond` session
    /// discovered on the Launcher's "Resume" list (feature request #70),
    /// without going through any saved profile's start-if-missing logic.
    fn start_resumed_session(name: &str, context: &egui::Context) -> Self {
        let dimensions = Dimensions::new(80, 24).expect("default dimensions are valid");
        let result = PersistentSession::resume_with_notifier(name, make_notifier(context))
            .map(ApplicationSession::Persistent)
            .map_err(|error| error.to_string());
        let inspector_persistence = Some(InspectorPersistence {
            provider_label: PersistenceProviderKind::FestermSessiond.label(),
            session_name: name.to_owned(),
        });
        Self::from_local_session_result(result, dimensions, name, None, None, inspector_persistence)
    }

    fn start_ssh(
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
        options: SshSessionOptions,
        profile_id: Option<&str>,
        prior_password_attempts: u8,
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
        let persistence = match options.strategy() {
            SessionStrategy::PlainShell => None,
            SessionStrategy::Persistent {
                provider,
                session_name,
            } => Some(InspectorPersistence {
                provider_label: provider.label(),
                session_name: session_name.as_str().to_owned(),
            }),
        };
        let inspector_transport = InspectorTransport::Ssh {
            username: profile.username().to_owned(),
            host: profile.identity().host().to_owned(),
            port: profile.identity().port(),
            persistence,
        };
        // Only a plain-password attempt can be usefully retried by
        // reprompting for a fresh password in-tab: a rejected private key or
        // native-stored password is a configuration problem, not something
        // retyping fixes (see `SshPasswordRetryState`).
        let password_retry = matches!(authentication, SshAuthentication::Password(_)).then(|| {
            SshPasswordRetryState {
                profile: profile.clone(),
                profile_identifier: profile_id.map(str::to_owned),
                options: options.clone(),
                attempts: prior_password_attempts,
            }
        });
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
            profile_id.map(str::to_owned),
            inspector_transport,
            password_retry,
        )
    }

    fn start_sftp(
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
        local_working_directory: Option<PathBuf>,
        known_host_fingerprint: Option<String>,
        profile_id: Option<&str>,
        context: &egui::Context,
    ) -> Self {
        let size = profile.initial_size();
        let dimensions = Dimensions::new(usize::from(size.columns()), usize::from(size.rows()))
            .expect("validated SFTP terminal size fits terminal dimensions");
        let label = profile_id
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}@{}", profile.username(), profile.identity().host()));
        let launch_secondary = Some(format!(
            "SFTP · {}:{}",
            profile.identity().host(),
            profile.identity().port()
        ));
        let inspector_transport = InspectorTransport::Sftp {
            username: profile.username().to_owned(),
            host: profile.identity().host().to_owned(),
            port: profile.identity().port(),
        };
        let result = SftpTerminalSession::start_with_notifier(
            profile,
            authentication,
            local_working_directory,
            known_host_fingerprint,
            make_notifier(context),
        )
        .map(ApplicationSession::Sftp);
        Self::from_sftp_session_result(
            result,
            dimensions,
            &label,
            launch_secondary,
            profile_id.map(str::to_owned),
            inspector_transport,
        )
    }

    fn from_local_session_result<E: std::fmt::Display>(
        result: Result<ApplicationSession, E>,
        dimensions: Dimensions,
        label: &str,
        launch_secondary: Option<String>,
        profile_identifier: Option<String>,
        persistence: Option<InspectorPersistence>,
    ) -> Self {
        Self::from_session_result(
            result,
            dimensions,
            SessionResultMeta {
                label,
                launch_secondary,
                profile_identifier,
                session_name: "Local shell",
                inspector_transport: InspectorTransport::Local { persistence },
                ssh_password_retry: None,
            },
        )
    }

    fn from_ssh_session_result(
        result: Result<ApplicationSession, SshSessionStartError>,
        dimensions: Dimensions,
        label: &str,
        launch_secondary: Option<String>,
        profile_identifier: Option<String>,
        inspector_transport: InspectorTransport,
        password_retry: Option<SshPasswordRetryState>,
    ) -> Self {
        Self::from_session_result(
            result,
            dimensions,
            SessionResultMeta {
                label,
                launch_secondary,
                profile_identifier,
                session_name: "SSH session",
                inspector_transport,
                ssh_password_retry: password_retry,
            },
        )
    }

    fn from_sftp_session_result(
        result: Result<ApplicationSession, SftpTerminalSessionStartError>,
        dimensions: Dimensions,
        label: &str,
        launch_secondary: Option<String>,
        profile_identifier: Option<String>,
        inspector_transport: InspectorTransport,
    ) -> Self {
        Self::from_session_result(
            result,
            dimensions,
            SessionResultMeta {
                label,
                launch_secondary,
                profile_identifier,
                session_name: "SFTP session",
                inspector_transport,
                ssh_password_retry: None,
            },
        )
    }

    /// Starts a serial session from explicit line settings, optionally tied
    /// to a saved profile. `window_dimensions` follows the same pattern as
    /// `start_default`/`start_local_profile`.
    fn start_serial(
        settings: LineSettings,
        profile_id: Option<&str>,
        context: &egui::Context,
        window_dimensions: Option<Dimensions>,
    ) -> Self {
        let dimensions = window_dimensions
            .unwrap_or_else(|| Dimensions::new(80, 24).expect("default dimensions are valid"));
        let label = profile_id
            .map(str::to_owned)
            .unwrap_or_else(|| settings.device().to_owned());
        let launch_secondary = Some(format!("Serial · {} baud", settings.baud_rate()));
        let inspector_transport = inspector_transport_from_settings(&settings);
        let result = SerialSession::open_with_notifier(settings, make_notifier(context))
            .map(ApplicationSession::Serial);
        Self::from_serial_session_result(
            result,
            dimensions,
            &label,
            launch_secondary,
            profile_id.map(str::to_owned),
            inspector_transport,
        )
    }

    fn from_serial_session_result(
        result: Result<ApplicationSession, SerialSessionError>,
        dimensions: Dimensions,
        label: &str,
        launch_secondary: Option<String>,
        profile_identifier: Option<String>,
        inspector_transport: InspectorTransport,
    ) -> Self {
        Self::from_session_result(
            result,
            dimensions,
            SessionResultMeta {
                label,
                launch_secondary,
                profile_identifier,
                session_name: "Serial session",
                inspector_transport,
                ssh_password_retry: None,
            },
        )
    }

    fn from_session_result<E: std::fmt::Display>(
        result: Result<ApplicationSession, E>,
        dimensions: Dimensions,
        meta: SessionResultMeta<'_>,
    ) -> Self {
        let SessionResultMeta {
            label,
            launch_secondary,
            profile_identifier,
            session_name,
            inspector_transport,
            ssh_password_retry,
        } = meta;
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
            eviction_notice_shown: false,
            ssh_password_retry,
            has_new_output_since_active: false,
            search: crate::search::TerminalSearchState::default(),
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
            Some(SessionLifecycle::Stopping | SessionLifecycle::Disconnected(_)) => {
                ChipStatus::Disconnected
            }
            Some(SessionLifecycle::Exited(_) | SessionLifecycle::Stopped) => ChipStatus::Exited,
            Some(SessionLifecycle::Failed(_)) => ChipStatus::Failed,
        }
    }

    /// Whether viewport-local Paste may enqueue input for this generation.
    pub fn accepts_input(&self) -> bool {
        self.controller.start_error().is_none()
            && matches!(self.controller.lifecycle(), Some(SessionLifecycle::Running))
    }

    /// Whether the terminal viewport should still deliver typed keystrokes
    /// and mouse-reporting bytes to the transport. Once a session has
    /// stopped, exited, failed, or (per ADR 0018) disconnected without an
    /// explicit user-initiated reconnect, history becomes read-only: scroll,
    /// selection, and copy keep working, but typed input must not be
    /// attempted (`docs/gui-action-graph.md` `HIST-06`/`SSH-02`). Mirrors the
    /// same "still alive" states as `close_requires_confirmation`, which is
    /// slightly looser than `accepts_input` (Paste is Running-only).
    pub fn accepts_typed_input(&self) -> bool {
        self.controller.start_error().is_none()
            && matches!(
                self.controller.lifecycle(),
                Some(SessionLifecycle::Starting | SessionLifecycle::Running)
            )
    }

    /// Whether closing this tab still ends an owned transport attempt or live
    /// transport and therefore requires explicit destructive confirmation.
    pub fn close_requires_confirmation(&self) -> bool {
        !matches!(
            &self.inspector_transport,
            InspectorTransport::Local {
                persistence: Some(_)
            }
        ) && self.controller.start_error().is_none()
            && matches!(
                self.controller.lifecycle(),
                Some(SessionLifecycle::Starting | SessionLifecycle::Running)
            )
    }

    /// Content-free locality/transport text for the status bar.
    pub fn system_label(&self) -> &'static str {
        match &self.inspector_transport {
            InspectorTransport::Ssh { .. } => "Remote",
            InspectorTransport::Sftp { .. } => "Remote",
            InspectorTransport::Serial { .. } => "Serial",
            InspectorTransport::Local { .. } if cfg!(windows) => "Local · Windows",
            InspectorTransport::Local { .. } if cfg!(target_os = "macos") => "Local · macOS",
            InspectorTransport::Local { .. } => "Local · Linux",
        }
    }

    pub fn dynamic_secondary(&self) -> Option<String> {
        self.controller
            .session()
            .and_then(ApplicationSession::sftp_working_directories)
            .map(|(remote, local)| format!("{remote} · {}", local.display()))
    }

    /// Transport-specific factual state for the persistent status bar.
    pub fn status_bar_label(&self) -> &'static str {
        match (&self.inspector_transport, self.chip_status()) {
            (_, ChipStatus::Starting) => "Starting",
            (_, ChipStatus::Reconnecting) => "Reconnecting",
            (_, ChipStatus::Disconnected) => "Disconnected",
            (_, ChipStatus::AuthRequired) => "Authentication required",
            (_, ChipStatus::Failed) => "Failed",
            (_, ChipStatus::Exited) => "Exited",
            (InspectorTransport::Local { .. }, ChipStatus::Connected) => "Running",
            (
                InspectorTransport::Ssh { .. } | InspectorTransport::Sftp { .. },
                ChipStatus::Connected,
            ) => "Connected",
            (InspectorTransport::Serial { .. }, ChipStatus::Connected) => "Open",
            (_, ChipStatus::Neutral) => "",
        }
    }

    /// The active SSH host-key request, if the transport is waiting for one.
    /// Local tabs never expose this UI state.
    pub fn host_key_prompt(&self) -> Option<&HostKeyPrompt> {
        matches!(
            self.controller.session(),
            Some(ApplicationSession::Ssh(_) | ApplicationSession::Sftp(_))
        )
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

    /// The active in-terminal interactive password request, if the transport
    /// is waiting for one. Local tabs never expose this UI state. Mirrors
    /// [`Self::host_key_prompt`].
    pub fn password_prompt(&self) -> Option<&PasswordPrompt> {
        matches!(
            self.controller.session(),
            Some(ApplicationSession::Ssh(_) | ApplicationSession::Sftp(_))
        )
        .then(|| self.controller.password_prompt())
        .flatten()
    }

    /// Sends a password value to the already-connected SSH worker for the
    /// current interactive prompt, feeding it into the live session rather
    /// than requiring it blind before a connection exists. Mirrors
    /// [`Self::resolve_host_key_trust`].
    pub fn resolve_ssh_password(
        &mut self,
        password: String,
    ) -> Result<(), PasswordResolutionError> {
        let prompt = self
            .password_prompt()
            .cloned()
            .ok_or(PasswordResolutionError::NoPendingPrompt)?;
        let result = self
            .controller
            .session()
            .expect("an SSH password prompt requires a session")
            .resolve_password_prompt(&prompt, password);
        self.controller.clear_password_prompt(&prompt);
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
        let result = self
            .controller
            .session()
            .ok_or(SessionReconnectError::NotSshSession)?
            .try_reconnect();
        if result.is_ok() {
            self.controller.advance_lifecycle_generation();
        }
        result
    }

    /// Whether the current live transport can accept SSH port-forward requests.
    pub fn live_port_forwarding_available(&self) -> bool {
        self.controller
            .session()
            .is_some_and(ApplicationSession::live_port_forwarding_available)
    }

    /// Whether this tab owns an SSH session transport, regardless of its current lifecycle state.
    pub fn is_ssh_session(&self) -> bool {
        match self.controller.session() {
            Some(ApplicationSession::Ssh(_)) => true,
            #[cfg(test)]
            Some(ApplicationSession::TestSsh(_)) => true,
            Some(ApplicationSession::Local(_))
            | Some(ApplicationSession::Persistent(_))
            | Some(ApplicationSession::Sftp(_))
            | Some(ApplicationSession::Serial(_))
            | None => false,
        }
    }

    /// The most recent live SSH port-forward snapshot for this tab.
    pub fn port_forwards(&self) -> &[SshPortForwardRuntime] {
        self.controller.port_forwards()
    }

    /// Count of currently active port forwards, excluding failed entries.
    pub fn active_port_forward_count(&self) -> usize {
        self.port_forwards()
            .iter()
            .filter(|forward| forward.state() == SshPortForwardState::Active)
            .count()
    }

    /// Requests a fresh port-forward snapshot from the live SSH worker.
    pub fn query_port_forwards(&self) -> Result<(), SshPortForwardRequestError> {
        self.controller
            .session()
            .ok_or(SshPortForwardRequestError::NotRunning)?
            .try_query_port_forwards()
    }

    /// Adds one live-only SSH port forward to the current tab's SSH session.
    pub fn add_port_forward(
        &self,
        forward: impl SshPortForwardSpec,
    ) -> Result<(), SshPortForwardRequestError> {
        self.controller
            .session()
            .ok_or(SshPortForwardRequestError::NotRunning)?
            .try_add_port_forward(forward)
    }

    /// Removes one live SSH port forward identified by its listening tuple.
    pub fn remove_port_forward(
        &self,
        direction: SshPortForwardDirection,
        bind_host: impl Into<String>,
        bind_port: u16,
    ) -> Result<(), SshPortForwardRequestError> {
        self.controller
            .session()
            .ok_or(SshPortForwardRequestError::NotRunning)?
            .try_remove_port_forward(direction, bind_host, bind_port)
    }
}

fn local_profile_secondary(profile: &LocalProfile) -> Option<String> {
    profile
        .executable()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

fn inspector_transport_from_settings(settings: &LineSettings) -> InspectorTransport {
    InspectorTransport::Serial {
        device: settings.device().to_owned(),
        baud_rate: settings.baud_rate(),
        data_bits: settings.data_bits().into(),
        parity: settings.parity().into(),
        stop_bits: settings.stop_bits().into(),
        flow_control: settings.flow_control().into(),
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
    Profiles,
    SshAuthenticationRequired(SshAuthenticationRequiredTab),
    SftpAuthenticationRequired(SftpAuthenticationRequiredTab),
    MarkdownViewer(Box<MarkdownViewerTab>),
    SftpFileManagerAuthenticationRequired(SftpFileManagerAuthenticationRequiredTab),
    SftpFileManager(Box<SftpFileManagerTab>),
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
    /// Opens (or focuses) the singleton Profiles management application
    /// surface.
    OpenProfiles,
    OpenLocalMarkdownFile {
        path: PathBuf,
    },
    /// Opens an explicit terminal hyperlink after application-owned URL
    /// validation. Terminal presentation emits intent only.
    OpenExternalLink {
        target: ExternalLinkTarget,
    },
    /// Opens (or focuses) the singleton Profiles surface directly into the
    /// editor for one existing profile, e.g. from a Launcher card's edit
    /// icon.
    OpenProfileEditor {
        identifier: String,
    },
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
    /// Starts one text-mode SFTP transport from explicitly supplied, secret-free
    /// connection metadata and transient authentication.
    StartSftpSession {
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
    },
    /// Opens the GUI SFTP authentication surface for a destination.
    OpenSftpFileManager {
        target: SftpFileManagerLaunchTarget,
    },
    /// Starts a GUI SFTP file-manager tab from explicit destination metadata
    /// and an explicit credential.
    StartSftpFileManager {
        target: SftpFileManagerLaunchTarget,
        authentication: SftpFileManagerAuthentication,
    },
    /// Starts an existing configured SSH profile by resolving its native
    /// stored password on the SSH worker. This command has no password value.
    StartStoredPasswordSshProfile {
        profile_id: String,
        options: SshSessionOptions,
    },
    /// Starts an existing configured SFTP profile by resolving its native
    /// stored password on the SFTP worker. This command has no password value.
    StartStoredPasswordSftpProfile {
        profile_id: String,
    },
    /// Launches a saved SSH profile the same way a saved local profile
    /// launches: from a Launcher/Profiles card, with no upfront password
    /// prompt in the launch surface itself. The composition root
    /// (`FesTermApp::screen_command`) resolves whether the profile has a
    /// stored native-secret credential first: with one, it behaves exactly
    /// like `StartStoredPasswordSshProfile`; without one, it is fully
    /// handled by `AppState::start_configured_ssh_profile_interactive`
    /// (openssh-style in-terminal password prompt, no composition-root
    /// resource needed). Never reaches `AppState::dispatch` for real work.
    StartConfiguredSshProfile {
        profile_id: String,
    },
    /// Launches a saved SSH profile as a text-mode SFTP tab.
    StartConfiguredSftpProfile {
        profile_id: String,
    },
    /// Opens the GUI SFTP authentication surface for a saved SSH profile.
    OpenConfiguredSftpFileManagerProfile {
        profile_id: String,
    },
    /// Starts a saved profile's GUI SFTP surface using its opaque
    /// native-store password or private-key reference.
    StartStoredSftpFileManagerProfile {
        profile_id: String,
    },
    /// Starts a serial session from explicitly supplied line settings. The
    /// Launcher's serial form validates input into a `LineSettings` value;
    /// this does not create a persisted profile.
    StartSerialSession {
        settings: LineSettings,
    },
    /// Starts a serial session from a saved serial profile. Mirrors
    /// `StartConfiguredLocalProfile`.
    StartConfiguredSerialProfile {
        profile_id: String,
    },
    ReloadMarkdown,
    ToggleMarkdownPreviewSource,
    ToggleMarkdownOutline,
    OpenMarkdownFind,
    NavigateMarkdownFind {
        reverse: bool,
    },
    LoadMarkdownLocalImage {
        reference_index: usize,
    },
    /// Requests that the composition root store a password for an existing
    /// configured SSH profile before starting it. The value is redacted from
    /// debug output and moved to the background store worker.
    StoreSshPassword {
        profile_id: String,
        password: PasswordToStore,
        options: SshSessionOptions,
    },
    /// Requests that the composition root store a password for an existing
    /// configured SSH profile before starting it as SFTP.
    StoreSftpPassword {
        profile_id: String,
        password: PasswordToStore,
    },
    /// Requests that the composition root store or replace a password for
    /// an existing configured SSH profile from the Profiles editor, with no
    /// follow-up launch (unlike `StoreSshPassword`, which is submitted from
    /// a live connect form and auto-launches once the credential is saved).
    StoreProfilePassword {
        profile_id: String,
        password: PasswordToStore,
    },
    /// Requests that the composition root store or replace a private-key
    /// credential (with an optional passphrase) for an existing configured
    /// SSH profile from the Profiles editor. Mirrors
    /// `StoreProfilePassword`, but for certificate/private-key
    /// authentication instead of a password.
    StoreProfilePrivateKey {
        profile_id: String,
        private_key: PrivateKeyToStore,
    },
    /// Resolves the displayed host-key request for one specific SSH or GUI
    /// SFTP tab.
    ///
    /// `AcceptAndPersist` (ADR 0020) additionally requires the composition
    /// root to write a durable trust record before this command reaches
    /// `AppState::dispatch`, mirroring how `StoreSshPassword` needs the
    /// configuration reloader; `FesTermApp::screen_command` reads the
    /// pending prompt and persists it first, then still routes this command
    /// through `AppState::dispatch` unchanged so the SSH-level decision is
    /// resolved identically to `AcceptOnce`.
    ResolveHostKeyTrust {
        tab: TabId,
        decision: HostKeyTrustDecision,
    },
    /// Resolves the in-terminal, live-session interactive password prompt
    /// for one specific SSH tab (see [`festerm_ssh::SshAuthentication::interactive`]):
    /// unlike `ResolveHostKeyTrust`, this feeds a value into the already-
    /// connected worker rather than a one-shot decision enum.
    ResolveSshPassword {
        tab: TabId,
        password: String,
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
    /// Moves one tab exactly one position while preserving active identity.
    MoveTabLeft(TabId),
    MoveTabRight(TabId),
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
    /// Toggles whether chips show their secondary detail line
    /// (`docs/gui-design.md` "Show session details in chips"). When off,
    /// every chip is a compact single-line chip and the active session's
    /// detail relocates to the status bar instead.
    ToggleShowSessionDetails,
    /// Toggles whether closing a live session requires confirmation.
    ToggleConfirmSessionClose,
    /// Toggles whether the open-tab list and active tab persist across
    /// restarts (`docs/gui-design.md` "Workspace restore" - explicit
    /// opt-in, off by default).
    ToggleRestoreWorkspace,
    /// Selects the bundled primary terminal face without changing
    /// application-chrome typography.
    SetTerminalFont(TerminalFontPreference),
    /// Enables or disables eligible multi-cell shaping runs. Cell ownership
    /// remains authoritative regardless of the selected font.
    ToggleTerminalLigatures,
    /// Selects the deterministic color or monochrome emoji presentation path
    /// without changing core-owned cell geometry.
    SetEmojiPresentation(EmojiPresentationPreference),
    /// Selects a clickstop scaling how far one trackpad/wheel scroll step
    /// moves the scrollback viewport (feature request #67).
    SetScrollSpeed(festerm_config::ScrollSpeedPreference),
    /// Selects the retained primary-history budget for sessions created
    /// after this preference changes.
    SetScrollbackLimit(ScrollbackLimitPreference),
    /// Toggles whether holding the quick-switch modifier (Cmd on macOS, Ctrl
    /// elsewhere) overlays each eligible chip's quick-switch number in
    /// place of its usual status presentation (feature request #69).
    ToggleQuickSwitchOverlay,
    /// Toggles whether the Launcher's New Session list uses a responsive
    /// multi-column layout for saved profiles when the window is wide
    /// enough (feature request #64).
    ToggleCompactLauncherGrid,
    /// Toggles whether a background session tab's chip status dot
    /// slow-pulses when that session has emitted output since the tab was
    /// last active (feature request #68).
    TogglePulseNewOutputDot,
    /// Toggles whether the New Session/Launcher screen surfaces locally
    /// running, unattached `festerm-sessiond` sessions as one-click
    /// "Resume" entries (feature request #70).
    ToggleShowResumableSessions,
    /// Sets or clears the default starting local directory for new SFTP sessions.
    SetDefaultSftpLocalDirectory(Option<PathBuf>),
    /// Sets the visual left/right order for GUI SFTP panes.
    SetSftpPaneOrder(SftpPaneOrderPreference),
    /// Resumes a locally running, unattached `festerm-sessiond` session by
    /// name, surfaced via the Launcher's "Resume" list (feature request
    /// #70). The composition root attaches to it and opens a new tab.
    ResumeUnattachedSession {
        name: String,
    },
    /// Resets chip layout and status-bar visibility to their defaults after
    /// explicit confirmation (`docs/gui-design.md` "Wrapping must remain
    /// user-configurable").
    ResetInterfaceSettings,
    /// Creates or edits a profile (docs/gui-design.md "Profile editing").
    /// Both are the same upsert-by-identifier write; the composition root
    /// (`FesTermApp::screen_command`) fully intercepts this to persist
    /// through the configuration reloader, mirroring `StoreSshPassword` —
    /// it never reaches `AppState::dispatch`.
    SaveProfile {
        profile: festerm_config::Profile,
    },
    /// Creates a profile and stores its initial native credential only after
    /// the profile metadata has been persisted successfully.
    SaveProfileWithCredential {
        profile: festerm_config::Profile,
        credential: ProfileCredentialToStore,
    },
    /// Deletes a profile the user has explicitly confirmed, after any
    /// workspace-tab references have been reported
    /// (`Configuration::workspace_tab_references`). Fully intercepted by the
    /// composition root like `SaveProfile`.
    DeleteProfile {
        identifier: String,
    },
    /// Reorders a saved profile via drag-and-drop on the Profiles surface
    /// (`Configuration::with_reordered_profiles`), reflected in the
    /// Launcher's own profile ordering too since both read
    /// `Configuration::profiles` in document order. Fully intercepted by the
    /// composition root like `SaveProfile`.
    ReorderProfiles {
        moved: String,
        before: Option<String>,
    },
}

/// A one-shot password value awaiting native-store insertion.
///
/// It has no public getter and redacts `Debug`, so application commands remain
/// safe to inspect in UI tests and diagnostics.
pub struct PasswordToStore(String);

/// A normalized external URL whose contents stay out of command diagnostics.
pub struct ExternalLinkTarget(String);

/// Initial native credential submitted while creating an SSH or SFTP profile.
pub enum ProfileCredentialToStore {
    Password(PasswordToStore),
    PrivateKey(PrivateKeyToStore),
}

impl std::fmt::Debug for ProfileCredentialToStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Password(_) => {
                formatter.write_str("ProfileCredentialToStore::Password([REDACTED])")
            }
            Self::PrivateKey(_) => {
                formatter.write_str("ProfileCredentialToStore::PrivateKey([REDACTED])")
            }
        }
    }
}

impl ExternalLinkTarget {
    pub(crate) fn new(target: String) -> Self {
        Self(target)
    }

    fn into_inner(self) -> String {
        self.0
    }
}

impl std::fmt::Debug for ExternalLinkTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ExternalLinkTarget([REDACTED])")
    }
}

impl PasswordToStore {
    pub(crate) fn new(password: String) -> Self {
        Self(password)
    }

    pub(crate) fn into_secret_bytes(self) -> SecretBytes {
        SecretBytes::from_secret_string(self.0)
    }
}

impl std::fmt::Debug for PasswordToStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PasswordToStore([REDACTED])")
    }
}

/// A one-shot private key + optional passphrase awaiting native-store
/// insertion.
///
/// It has no public getter and redacts `Debug`, so application commands remain
/// safe to inspect in UI tests and diagnostics.
pub struct PrivateKeyToStore {
    key_text: String,
    passphrase: Option<String>,
}

impl PrivateKeyToStore {
    pub(crate) fn new(key_text: String, passphrase: Option<String>) -> Self {
        Self {
            key_text,
            passphrase,
        }
    }

    pub(crate) fn into_secret_bytes(self) -> SecretBytes {
        festerm_ssh::encode_stored_private_key(&self.key_text, self.passphrase.as_deref())
    }
}

impl std::fmt::Debug for PrivateKeyToStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PrivateKeyToStore([REDACTED])")
    }
}

/// Converts the saved interface preference into the UI-crate layout enum
/// used by rendering. This is the one place `AppState` bridges the
/// config-crate and UI-crate chip layout types.
const fn chip_layout_from_preference(preference: ChipLayoutPreference) -> ChipLayout {
    match preference {
        ChipLayoutPreference::Wrap => ChipLayout::Wrap,
        ChipLayoutPreference::SingleRowScroll => ChipLayout::SingleRowScroll,
    }
}

/// Converts a live UI chip layout back into the persisted preference.
const fn chip_layout_to_preference(layout: ChipLayout) -> ChipLayoutPreference {
    match layout {
        ChipLayout::Wrap => ChipLayoutPreference::Wrap,
        ChipLayout::SingleRowScroll => ChipLayoutPreference::SingleRowScroll,
    }
}

/// Owns the always-nonempty tab collection and the active-tab cursor.
pub struct AppState {
    tabs: Vec<Tab>,
    active: TabId,
    configuration: Configuration,
    inspector_open: bool,
    chip_layout: ChipLayout,
    status_bar_visible: bool,
    show_session_details: bool,
    confirm_session_close: bool,
    /// Whether the open-tab list and active tab persist across restarts
    /// (`docs/gui-design.md` "Workspace restore"). Off by default and,
    /// unlike the other interface preferences here, deliberately explicit:
    /// resurrecting a previous session's tabs is a bigger behavioral change
    /// than a cosmetic chip-layout/status-bar choice, so it needs its own
    /// opt-in rather than defaulting on.
    restore_workspace: bool,
    terminal_font: TerminalFontPreference,
    terminal_ligatures: bool,
    emoji_presentation: EmojiPresentationPreference,
    scroll_speed: ScrollSpeedPreference,
    scrollback_limit: ScrollbackLimitPreference,
    quick_switch_overlay: bool,
    compact_launcher_grid: bool,
    pulse_new_output_dot: bool,
    /// Whether the New Session/Launcher screen surfaces locally running,
    /// unattached `festerm-sessiond` sessions as one-click "Resume" entries
    /// (feature request #70).
    show_resumable_sessions: bool,
    /// Visual left/right order for GUI SFTP panes.
    sftp_pane_order: SftpPaneOrderPreference,
    /// The default starting local directory for new SFTP sessions.
    default_sftp_local_directory: Option<PathBuf>,
    /// Set by `AppCommand::OpenProfileEditor` so the just-(re)activated
    /// singleton Profiles tab opens directly into that profile's editor
    /// instead of the list. Consumed once by `FesTermApp::screen_command`
    /// via `take_pending_profile_edit`, since the Profiles surface's own
    /// per-tab UI state lives in `egui`'s `ui.data`, not here.
    pending_profile_edit: Option<String>,
    /// Set whenever a tab-list mutation (open/close/reorder/rename/activate)
    /// changes what `capture_workspace_configuration` would produce, so the
    /// composition root can autosave the workspace without every invocation
    /// surface (chip row, palette, shortcuts, launcher) needing its own
    /// explicit "Save workspace" call (`docs/gui-design.md`
    /// "Configuration": save/restore is automatic, not a manual action).
    /// Consumed once per frame via `take_workspace_dirty`.
    workspace_dirty: bool,
}

impl AppState {
    /// Starts in the singleton Launcher when there is no workspace to restore.
    /// This is the ordinary product startup path; native-window smoke may
    /// still request a deterministic primary session explicitly.
    pub fn with_launcher(configuration: Configuration) -> Self {
        let id = TabId::next();
        let settings = configuration.interface_settings().clone();
        Self {
            tabs: vec![Tab {
                id,
                content: TabContent::Launcher,
            }],
            active: id,
            configuration,
            inspector_open: false,
            chip_layout: chip_layout_from_preference(settings.chip_layout()),
            status_bar_visible: settings.status_bar_visible(),
            show_session_details: settings.show_session_details(),
            confirm_session_close: settings.confirm_session_close(),
            restore_workspace: settings.restore_workspace(),
            terminal_font: settings.terminal_font(),
            terminal_ligatures: settings.terminal_ligatures(),
            emoji_presentation: settings.emoji_presentation(),
            scroll_speed: settings.scroll_speed(),
            scrollback_limit: settings.scrollback_limit(),
            quick_switch_overlay: settings.quick_switch_overlay(),
            compact_launcher_grid: settings.compact_launcher_grid(),
            pulse_new_output_dot: settings.pulse_new_output_dot(),
            show_resumable_sessions: settings.show_resumable_sessions(),
            sftp_pane_order: settings.sftp_pane_order(),
            default_sftp_local_directory: settings
                .default_sftp_local_directory()
                .map(Path::to_path_buf),
            pending_profile_edit: None,
            workspace_dirty: false,
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
        let settings = configuration.interface_settings().clone();
        let mut state = Self {
            tabs: vec![Tab {
                id,
                content: TabContent::Session(Box::new(session)),
            }],
            active: id,
            configuration,
            inspector_open: false,
            chip_layout: chip_layout_from_preference(settings.chip_layout()),
            status_bar_visible: settings.status_bar_visible(),
            show_session_details: settings.show_session_details(),
            confirm_session_close: settings.confirm_session_close(),
            restore_workspace: settings.restore_workspace(),
            terminal_font: settings.terminal_font(),
            terminal_ligatures: settings.terminal_ligatures(),
            emoji_presentation: settings.emoji_presentation(),
            scroll_speed: settings.scroll_speed(),
            scrollback_limit: settings.scrollback_limit(),
            quick_switch_overlay: settings.quick_switch_overlay(),
            compact_launcher_grid: settings.compact_launcher_grid(),
            pulse_new_output_dot: settings.pulse_new_output_dot(),
            show_resumable_sessions: settings.show_resumable_sessions(),
            sftp_pane_order: settings.sftp_pane_order(),
            default_sftp_local_directory: settings
                .default_sftp_local_directory()
                .map(Path::to_path_buf),
            pending_profile_edit: None,
            workspace_dirty: false,
        };
        state.apply_scrollback_limit_to_sessions();
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
                WorkspaceTab::Profiles(_) => TabContent::Profiles,
                WorkspaceTab::LocalSession(tab) => {
                    let local = configuration
                        .profile(tab.profile_id())
                        .and_then(festerm_config::Profile::as_local)
                        .expect("validated workspace local profile reference");
                    TabContent::Session(Box::new(SessionTab::start_local_profile(
                        local.to_local_profile(),
                        local.identifier(),
                        local.persistence(),
                        context,
                        None,
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
                WorkspaceTab::SftpSession(tab) => {
                    let ssh = configuration
                        .profile(tab.profile_id())
                        .and_then(festerm_config::Profile::as_ssh)
                        .expect("validated workspace SFTP profile reference");
                    TabContent::SftpAuthenticationRequired(SftpAuthenticationRequiredTab {
                        profile: ssh.clone(),
                    })
                }
                WorkspaceTab::SftpFileManager(tab) => {
                    let ssh = configuration
                        .profile(tab.profile_id())
                        .and_then(festerm_config::Profile::as_ssh)
                        .expect("validated GUI SFTP workspace profile reference");
                    TabContent::SftpFileManagerAuthenticationRequired(
                        SftpFileManagerAuthenticationRequiredTab {
                            target: SftpFileManagerLaunchTarget {
                                label: ssh.identifier().to_owned(),
                                username: ssh.username().to_owned(),
                                host: ssh.host().to_owned(),
                                port: ssh.port(),
                                profile_id: Some(ssh.identifier().to_owned()),
                                stored_credential_kind: ssh
                                    .credential_reference()
                                    .is_some()
                                    .then_some(ssh.credential_kind()),
                                known_host_persisted: configuration
                                    .known_host_fingerprint(ssh.host(), ssh.port())
                                    .is_some(),
                            },
                        },
                    )
                }
                WorkspaceTab::SerialSession(tab) => {
                    let serial = configuration
                        .profile(tab.profile_id())
                        .and_then(festerm_config::Profile::as_serial)
                        .expect("validated workspace serial profile reference");
                    let settings = serial
                        .to_line_settings()
                        .expect("validated serial profile line settings");
                    TabContent::Session(Box::new(SessionTab::start_serial(
                        settings,
                        Some(serial.identifier()),
                        context,
                        None,
                    )))
                }
            };
            restored.push(Tab { id, content });
        }

        let active = focused.unwrap_or_else(|| restored[0].id);
        let settings = configuration.interface_settings().clone();
        let mut state = Self {
            tabs: restored,
            active,
            configuration,
            inspector_open: false,
            chip_layout: chip_layout_from_preference(settings.chip_layout()),
            status_bar_visible: settings.status_bar_visible(),
            show_session_details: settings.show_session_details(),
            confirm_session_close: settings.confirm_session_close(),
            restore_workspace: settings.restore_workspace(),
            terminal_font: settings.terminal_font(),
            terminal_ligatures: settings.terminal_ligatures(),
            emoji_presentation: settings.emoji_presentation(),
            scroll_speed: settings.scroll_speed(),
            scrollback_limit: settings.scrollback_limit(),
            quick_switch_overlay: settings.quick_switch_overlay(),
            compact_launcher_grid: settings.compact_launcher_grid(),
            pulse_new_output_dot: settings.pulse_new_output_dot(),
            show_resumable_sessions: settings.show_resumable_sessions(),
            sftp_pane_order: settings.sftp_pane_order(),
            default_sftp_local_directory: settings
                .default_sftp_local_directory()
                .map(Path::to_path_buf),
            pending_profile_edit: None,
            workspace_dirty: false,
        };
        state.apply_scrollback_limit_to_sessions();
        state
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// Exact counts of live local processes, SSH connections, and open
    /// serial devices across every tab, for the aggregate application-quit
    /// confirmation (`docs/gui-action-graph.md` `QUIT-01`). Uses the same
    /// "still live" test as `SessionTab::close_requires_confirmation` so the
    /// aggregate summary and the per-session dialog never disagree about
    /// which sessions actually have something to lose by closing.
    pub fn live_session_counts(&self) -> LiveSessionCounts {
        let mut counts = LiveSessionCounts::default();
        for tab in &self.tabs {
            let TabContent::Session(session) = &tab.content else {
                continue;
            };
            if !session.close_requires_confirmation() {
                continue;
            }
            match session.inspector_transport {
                InspectorTransport::Local { .. } => counts.local += 1,
                InspectorTransport::Ssh { .. } | InspectorTransport::Sftp { .. } => counts.ssh += 1,
                InspectorTransport::Serial { .. } => counts.serial += 1,
            }
        }
        counts
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
                TabContent::Profiles => Some(WorkspaceTab::profiles(identifier.clone())?),
                TabContent::MarkdownViewer(_) => None,
                TabContent::SshAuthenticationRequired(ssh) => Some(WorkspaceTab::ssh_session(
                    identifier.clone(),
                    ssh.profile.identifier(),
                )?),
                TabContent::SftpAuthenticationRequired(sftp) => Some(WorkspaceTab::sftp_session(
                    identifier.clone(),
                    sftp.profile.identifier(),
                )?),
                TabContent::SftpFileManagerAuthenticationRequired(tab) => tab
                    .target
                    .profile_id
                    .as_deref()
                    .map(|profile_id| {
                        WorkspaceTab::sftp_file_manager(identifier.clone(), profile_id)
                    })
                    .transpose()?,
                TabContent::SftpFileManager(tab) => tab
                    .profile_identifier
                    .as_deref()
                    .map(|profile_id| {
                        WorkspaceTab::sftp_file_manager(identifier.clone(), profile_id)
                    })
                    .transpose()?,
                TabContent::Session(session) => session
                    .profile_identifier
                    .as_deref()
                    .and_then(|profile_id| {
                        let profile = self.configuration.profile(profile_id)?;
                        if profile.as_local().is_some() {
                            Some(WorkspaceTab::local_session(identifier.clone(), profile_id))
                        } else if profile.as_ssh().is_some()
                            && matches!(
                                session.inspector_transport,
                                InspectorTransport::Sftp { .. }
                            )
                        {
                            Some(WorkspaceTab::sftp_session(identifier.clone(), profile_id))
                        } else if profile.as_ssh().is_some() {
                            Some(WorkspaceTab::ssh_session(identifier.clone(), profile_id))
                        } else if profile.as_serial().is_some() {
                            Some(WorkspaceTab::serial_session(identifier.clone(), profile_id))
                        } else {
                            None
                        }
                    })
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

    pub const fn show_session_details(&self) -> bool {
        self.show_session_details
    }

    pub const fn confirm_session_close(&self) -> bool {
        self.confirm_session_close
    }

    pub const fn restore_workspace(&self) -> bool {
        self.restore_workspace
    }

    pub const fn terminal_font(&self) -> TerminalFontPreference {
        self.terminal_font
    }

    pub const fn terminal_ligatures(&self) -> bool {
        self.terminal_ligatures
    }

    pub const fn emoji_presentation(&self) -> EmojiPresentationPreference {
        self.emoji_presentation
    }

    pub const fn scroll_speed(&self) -> ScrollSpeedPreference {
        self.scroll_speed
    }

    pub const fn scrollback_limit(&self) -> ScrollbackLimitPreference {
        self.scrollback_limit
    }

    pub const fn quick_switch_overlay(&self) -> bool {
        self.quick_switch_overlay
    }

    pub const fn compact_launcher_grid(&self) -> bool {
        self.compact_launcher_grid
    }

    pub const fn pulse_new_output_dot(&self) -> bool {
        self.pulse_new_output_dot
    }

    pub const fn show_resumable_sessions(&self) -> bool {
        self.show_resumable_sessions
    }

    pub const fn sftp_pane_order(&self) -> SftpPaneOrderPreference {
        self.sftp_pane_order
    }

    pub fn default_sftp_local_directory(&self) -> Option<&Path> {
        self.default_sftp_local_directory.as_deref()
    }

    /// Returns the current chip-layout, status-bar, and session-detail
    /// preferences as a persistable value, for the composition root to write
    /// through after a toggle or reset.
    pub fn interface_settings(&self) -> InterfaceSettings {
        InterfaceSettings::new(
            chip_layout_to_preference(self.chip_layout),
            self.status_bar_visible,
            self.show_session_details,
            self.confirm_session_close,
            self.restore_workspace,
        )
        .with_terminal_typography(self.terminal_font, self.terminal_ligatures)
        .with_emoji_presentation(self.emoji_presentation)
        .with_scroll_speed(self.scroll_speed)
        .with_scrollback_limit(self.scrollback_limit)
        .with_quick_switch_overlay(self.quick_switch_overlay)
        .with_compact_launcher_grid(self.compact_launcher_grid)
        .with_pulse_new_output_dot(self.pulse_new_output_dot)
        .with_show_resumable_sessions(self.show_resumable_sessions)
        .with_sftp_pane_order(self.sftp_pane_order)
        .with_default_sftp_local_directory(
            self.default_sftp_local_directory
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
        )
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

    pub fn session_tab(&self, id: TabId) -> Option<&SessionTab> {
        self.tabs
            .iter()
            .find(|tab| tab.id == id)
            .and_then(|tab| match &tab.content {
                TabContent::Session(session) => Some(session.as_ref()),
                TabContent::Launcher
                | TabContent::Settings
                | TabContent::Profiles
                | TabContent::SshAuthenticationRequired(_)
                | TabContent::SftpAuthenticationRequired(_)
                | TabContent::SftpFileManagerAuthenticationRequired(_)
                | TabContent::SftpFileManager(_)
                | TabContent::MarkdownViewer(_) => None,
            })
    }

    pub fn session_tab_mut(&mut self, id: TabId) -> Option<&mut SessionTab> {
        self.tabs
            .iter_mut()
            .find(|tab| tab.id == id)
            .and_then(|tab| match &mut tab.content {
                TabContent::Session(session) => Some(session.as_mut()),
                TabContent::Launcher
                | TabContent::Settings
                | TabContent::Profiles
                | TabContent::MarkdownViewer(_)
                | TabContent::SshAuthenticationRequired(_)
                | TabContent::SftpAuthenticationRequired(_)
                | TabContent::SftpFileManagerAuthenticationRequired(_)
                | TabContent::SftpFileManager(_) => None,
            })
    }

    pub fn sftp_file_manager_tab_mut(&mut self, id: TabId) -> Option<&mut SftpFileManagerTab> {
        self.tabs
            .iter_mut()
            .find(|tab| tab.id == id)
            .and_then(|tab| match &mut tab.content {
                TabContent::SftpFileManager(session) => Some(session.as_mut()),
                TabContent::Launcher
                | TabContent::Settings
                | TabContent::Profiles
                | TabContent::MarkdownViewer(_)
                | TabContent::SshAuthenticationRequired(_)
                | TabContent::SftpAuthenticationRequired(_)
                | TabContent::SftpFileManagerAuthenticationRequired(_)
                | TabContent::Session(_) => None,
            })
    }

    pub fn apply_terminal_font_set(&mut self, font_set: festerm_ui_egui::TerminalFontSet) {
        for tab in &mut self.tabs {
            if let TabContent::Session(session) = &mut tab.content {
                session.view.set_font_set(font_set);
            }
        }
    }

    /// The terminal grid size a brand-new tab should start at, so opening a
    /// session in an already-resized window doesn't visibly snap back to the
    /// application's baseline default and get corrected only on the next
    /// resize. Prefers the active session tab's current dimensions (the one
    /// most recently rendered against the real window size); falls back to
    /// any other live session tab, then `None` (baseline default) if no
    /// session tab exists yet, e.g. a fresh Launcher-only window.
    fn current_session_dimensions(&self) -> Option<Dimensions> {
        if let TabContent::Session(active) = &self.active_tab().content {
            return Some(active.terminal.dimensions());
        }
        self.tabs.iter().find_map(|tab| match &tab.content {
            TabContent::Session(session) => Some(session.terminal.dimensions()),
            TabContent::Launcher
            | TabContent::Settings
            | TabContent::Profiles
            | TabContent::MarkdownViewer(_)
            | TabContent::SshAuthenticationRequired(_)
            | TabContent::SftpAuthenticationRequired(_)
            | TabContent::SftpFileManagerAuthenticationRequired(_)
            | TabContent::SftpFileManager(_) => None,
        })
    }

    /// Every running session tab paired with its `TabId`, independent of
    /// which is active. Used where a caller needs to compare each tab
    /// against the currently active tab (feature request #68: only a
    /// *non-active* tab's output should mark it as having new output).
    pub fn session_tabs_with_id_mut(&mut self) -> impl Iterator<Item = (TabId, &mut SessionTab)> {
        self.tabs
            .iter_mut()
            .filter_map(|tab| match &mut tab.content {
                TabContent::Session(session) => Some((tab.id, session.as_mut())),
                TabContent::Launcher
                | TabContent::Settings
                | TabContent::Profiles
                | TabContent::MarkdownViewer(_)
                | TabContent::SshAuthenticationRequired(_)
                | TabContent::SftpAuthenticationRequired(_)
                | TabContent::SftpFileManagerAuthenticationRequired(_)
                | TabContent::SftpFileManager(_) => None,
            })
    }

    /// Requests an on-demand SSH-level liveness probe (ADR 0018) on every
    /// open SSH session tab. Intended for a wake/network-change signal:
    /// each request is coalescing and nonblocking, so calling this
    /// repeatedly (e.g. once per detected event) never queues more than one
    /// pending probe per session. Errors are deliberately discarded — a
    /// session that isn't running, or that already has a probe pending, is
    /// an ordinary, expected outcome here, not a fault to surface.
    pub fn request_liveness_check_on_all_sessions(&self) {
        for tab in &self.tabs {
            if let TabContent::Session(session) = &tab.content {
                if let Some(session) = session.controller.session() {
                    let _ = session.try_check_liveness();
                }
            }
        }
    }

    /// Applies one `AppCommand`. This is the single command-handling path;
    /// every invocation surface must converge here rather than implementing
    /// independent tab/session policy.
    pub fn dispatch(&mut self, command: AppCommand, context: &egui::Context) {
        match command {
            AppCommand::OpenLauncher => self.open_launcher(),
            AppCommand::OpenSettings => self.open_settings(),
            AppCommand::OpenProfiles => self.open_profiles(),
            AppCommand::OpenLocalMarkdownFile { path } => self.open_local_markdown(path),
            AppCommand::OpenExternalLink { target } => {
                if let Some(target) = festerm_core::normalize_external_web_url(&target.into_inner())
                {
                    context.open_url(egui::OpenUrl::new_tab(target));
                }
            }
            AppCommand::OpenProfileEditor { identifier } => self.open_profile_editor(identifier),
            AppCommand::StartLocalSession => self.start_local_session(context),
            AppCommand::StartConfiguredLocalProfile { profile_id } => {
                self.start_configured_local_profile(&profile_id, context)
            }

            AppCommand::StartSshSession {
                profile,
                authentication,
                options,
            } => self.execute_ssh_session(profile, authentication, options, None, context),
            AppCommand::StartSftpSession {
                profile,
                authentication,
            } => self.execute_sftp_session(profile, authentication, None, context),
            AppCommand::OpenSftpFileManager { target } => self.open_sftp_file_manager(target),
            AppCommand::StartSftpFileManager {
                target,
                authentication,
            } => self.start_sftp_file_manager(target, authentication, context),
            AppCommand::StartStoredPasswordSshProfile { .. }
            | AppCommand::StartStoredPasswordSftpProfile { .. }
            | AppCommand::StartStoredSftpFileManagerProfile { .. }
            | AppCommand::StoreSshPassword { .. }
            | AppCommand::StoreSftpPassword { .. }
            | AppCommand::StoreProfilePassword { .. }
            | AppCommand::StoreProfilePrivateKey { .. }
            | AppCommand::StartConfiguredSshProfile { .. }
            | AppCommand::StartConfiguredSftpProfile { .. } => {}
            AppCommand::ReloadMarkdown => {
                self.with_active_markdown_viewer(|viewer| viewer.reload())
            }
            AppCommand::ToggleMarkdownPreviewSource => {
                self.with_active_markdown_viewer(|viewer| viewer.toggle_mode())
            }
            AppCommand::ToggleMarkdownOutline => {
                self.with_active_markdown_viewer(|viewer| viewer.toggle_outline())
            }
            AppCommand::OpenMarkdownFind => {
                self.with_active_markdown_viewer(|viewer| viewer.open_find())
            }
            AppCommand::NavigateMarkdownFind { reverse } => {
                self.with_active_markdown_viewer(|viewer| viewer.advance_find(reverse))
            }
            AppCommand::LoadMarkdownLocalImage { reference_index } => self
                .with_active_markdown_viewer(|viewer| {
                    viewer.load_local_image(reference_index, context)
                }),
            AppCommand::OpenConfiguredSftpFileManagerProfile { profile_id } => {
                self.open_configured_sftp_file_manager_profile(&profile_id)
            }
            AppCommand::StartSerialSession { settings } => {
                self.start_serial_session(settings, context)
            }
            AppCommand::StartConfiguredSerialProfile { profile_id } => {
                self.start_configured_serial_profile(&profile_id, context)
            }
            AppCommand::ResolveHostKeyTrust { tab, decision } => {
                self.resolve_host_key_trust(tab, decision)
            }
            AppCommand::ResolveSshPassword { tab, password } => {
                self.resolve_ssh_password(tab, password)
            }
            AppCommand::ReconnectSession(tab) => self.request_reconnect(tab),
            AppCommand::ActivateTab(id) => self.activate(id),
            AppCommand::ActivateNextTab => self.activate_relative(1),
            AppCommand::ActivatePreviousTab => self.activate_relative(-1),
            AppCommand::CloseTab(id) => self.close(id),
            AppCommand::ReorderTab { moved, before } => self.reorder(moved, before),
            AppCommand::MoveTabLeft(id) => self.move_tab(id, -1),
            AppCommand::MoveTabRight(id) => self.move_tab(id, 1),
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
            AppCommand::ToggleShowSessionDetails => {
                self.show_session_details = !self.show_session_details;
            }
            AppCommand::ToggleConfirmSessionClose => {
                self.confirm_session_close = !self.confirm_session_close;
            }
            AppCommand::ToggleRestoreWorkspace => {
                self.restore_workspace = !self.restore_workspace;
            }
            AppCommand::SetTerminalFont(font) => {
                self.terminal_font = font;
            }
            AppCommand::ToggleTerminalLigatures => {
                self.terminal_ligatures = !self.terminal_ligatures;
            }
            AppCommand::SetEmojiPresentation(presentation) => {
                self.emoji_presentation = presentation;
            }
            AppCommand::SetScrollSpeed(speed) => {
                self.scroll_speed = speed;
            }
            AppCommand::SetScrollbackLimit(limit) => {
                self.scrollback_limit = limit;
            }
            AppCommand::ToggleQuickSwitchOverlay => {
                self.quick_switch_overlay = !self.quick_switch_overlay;
            }
            AppCommand::ToggleCompactLauncherGrid => {
                self.compact_launcher_grid = !self.compact_launcher_grid;
            }
            AppCommand::TogglePulseNewOutputDot => {
                self.pulse_new_output_dot = !self.pulse_new_output_dot;
            }
            AppCommand::ToggleShowResumableSessions => {
                self.show_resumable_sessions = !self.show_resumable_sessions;
            }
            AppCommand::SetDefaultSftpLocalDirectory(path) => {
                self.default_sftp_local_directory = path;
            }
            AppCommand::SetSftpPaneOrder(order) => {
                self.sftp_pane_order = order;
            }
            AppCommand::ResumeUnattachedSession { name } => {
                self.start_resumed_session(&name, context);
            }
            AppCommand::ResetInterfaceSettings => {
                self.chip_layout =
                    chip_layout_from_preference(InterfaceSettings::DEFAULT.chip_layout());
                self.status_bar_visible = InterfaceSettings::DEFAULT.status_bar_visible();
                self.show_session_details = InterfaceSettings::DEFAULT.show_session_details();
                self.confirm_session_close = InterfaceSettings::DEFAULT.confirm_session_close();
                self.restore_workspace = InterfaceSettings::DEFAULT.restore_workspace();
                self.terminal_font = InterfaceSettings::DEFAULT.terminal_font();
                self.terminal_ligatures = InterfaceSettings::DEFAULT.terminal_ligatures();
                self.emoji_presentation = InterfaceSettings::DEFAULT.emoji_presentation();
                self.scroll_speed = InterfaceSettings::DEFAULT.scroll_speed();
                self.scrollback_limit = InterfaceSettings::DEFAULT.scrollback_limit();
                self.quick_switch_overlay = InterfaceSettings::DEFAULT.quick_switch_overlay();
                self.compact_launcher_grid = InterfaceSettings::DEFAULT.compact_launcher_grid();
                self.pulse_new_output_dot = InterfaceSettings::DEFAULT.pulse_new_output_dot();
                self.show_resumable_sessions = InterfaceSettings::DEFAULT.show_resumable_sessions();
                self.sftp_pane_order = InterfaceSettings::DEFAULT.sftp_pane_order();
                self.default_sftp_local_directory = None;
            }
            // The composition root fully intercepts these before dispatch to
            // persist through the configuration reloader (mirroring
            // `StoreSshPassword`); they never reach this match with real
            // work to do here.
            AppCommand::SaveProfile { .. }
            | AppCommand::SaveProfileWithCredential { .. }
            | AppCommand::DeleteProfile { .. }
            | AppCommand::ReorderProfiles { .. } => {}
        }
        // The Inspector follows session chips, but it is not a global panel
        // for Launcher, Settings, or authentication forms.
        if !matches!(self.active_tab().content, TabContent::Session(_)) {
            self.inspector_open = false;
        }
    }

    fn with_active_markdown_viewer(&mut self, mut apply: impl FnMut(&mut MarkdownViewerTab)) {
        let TabContent::MarkdownViewer(viewer) = &mut self.active_tab_mut().content else {
            return;
        };
        apply(viewer);
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
            self.set_active(existing.id);
            self.workspace_dirty = true;
            return;
        }
        let id = TabId::next();
        self.tabs.push(Tab {
            id,
            content: TabContent::Launcher,
        });
        self.set_active(id);
        self.workspace_dirty = true;
    }

    fn open_settings(&mut self) {
        // Settings is a singleton application surface with its own chip.
        if let Some(existing) = self
            .tabs
            .iter()
            .find(|tab| matches!(tab.content, TabContent::Settings))
        {
            self.set_active(existing.id);
            self.workspace_dirty = true;
            return;
        }
        let id = TabId::next();
        self.tabs.push(Tab {
            id,
            content: TabContent::Settings,
        });
        self.set_active(id);
        self.workspace_dirty = true;
    }

    fn open_profiles(&mut self) {
        // Profiles is a singleton application surface with its own chip,
        // like Settings.
        if let Some(existing) = self
            .tabs
            .iter()
            .find(|tab| matches!(tab.content, TabContent::Profiles))
        {
            self.set_active(existing.id);
            self.workspace_dirty = true;
            return;
        }
        let id = TabId::next();
        self.tabs.push(Tab {
            id,
            content: TabContent::Profiles,
        });
        self.set_active(id);
        self.workspace_dirty = true;
    }

    fn open_local_markdown(&mut self, path: PathBuf) {
        if let Some(existing) = self.tabs.iter().find_map(|tab| match &tab.content {
            TabContent::MarkdownViewer(viewer) if viewer.matches_local_path(&path) => Some(tab.id),
            _ => None,
        }) {
            self.set_active(existing);
            self.workspace_dirty = true;
            return;
        }
        let id = TabId::next();
        self.tabs.push(Tab {
            id,
            content: TabContent::MarkdownViewer(Box::new(MarkdownViewerTab::open_local(path))),
        });
        self.set_active(id);
        self.workspace_dirty = true;
    }

    /// Opens (or focuses) the Profiles surface and marks `identifier` to be
    /// consumed by `take_pending_profile_edit`, so the surface renders
    /// straight into that profile's editor instead of the list.
    fn open_profile_editor(&mut self, identifier: String) {
        self.open_profiles();
        self.pending_profile_edit = Some(identifier);
    }

    /// One-shot consumption of a pending profile-editor request set by
    /// `open_profile_editor`. `FesTermApp::screen_command` calls this each
    /// frame it renders the Profiles surface; the Profiles screen itself
    /// cannot read `AppState` directly.
    pub fn take_pending_profile_edit(&mut self) -> Option<String> {
        self.pending_profile_edit.take()
    }

    /// One-shot consumption of the workspace-dirty flag set by any
    /// tab-list mutation, so the composition root can autosave the
    /// workspace exactly once per frame that actually changed it, instead
    /// of on every frame or requiring a manual Save action.
    pub fn take_workspace_dirty(&mut self) -> bool {
        std::mem::take(&mut self.workspace_dirty)
    }

    fn start_local_session(&mut self, context: &egui::Context) {
        let dimensions = self.current_session_dimensions();
        self.place_session(SessionTab::start_default(context, dimensions));
    }

    fn start_resumed_session(&mut self, name: &str, context: &egui::Context) {
        self.place_session(SessionTab::start_resumed_session(name, context));
    }

    fn start_configured_local_profile(&mut self, profile_id: &str, context: &egui::Context) {
        let Some(profile) = self.configuration.profile(profile_id) else {
            return;
        };
        let Some(local) = profile.as_local() else {
            return;
        };
        let dimensions = self.current_session_dimensions();
        self.place_session(SessionTab::start_local_profile(
            local.to_local_profile(),
            local.identifier(),
            local.persistence(),
            context,
            dimensions,
        ));
    }

    fn start_serial_session(&mut self, settings: LineSettings, context: &egui::Context) {
        let dimensions = self.current_session_dimensions();
        self.place_session(SessionTab::start_serial(
            settings, None, context, dimensions,
        ));
    }

    fn start_configured_serial_profile(&mut self, profile_id: &str, context: &egui::Context) {
        let Some(profile) = self
            .configuration
            .profile(profile_id)
            .and_then(festerm_config::Profile::as_serial)
        else {
            return;
        };
        let Ok(settings) = profile.to_line_settings() else {
            return;
        };
        let dimensions = self.current_session_dimensions();
        self.place_session(SessionTab::start_serial(
            settings,
            Some(profile_id),
            context,
            dimensions,
        ));
    }

    /// Starts a saved SSH profile that has no stored native-secret
    /// credential, using the same openssh-style in-terminal interactive
    /// prompt flow as Quick Connect. Unlike a stored-password profile (which
    /// needs the composition root's secret store), this is fully handled
    /// here. The launch inherits the current window's terminal size rather
    /// than the profile's own stored initial size, matching how a local
    /// session or Quick Connect starts already sized to the open window.
    pub fn start_configured_ssh_profile_interactive(
        &mut self,
        profile_id: &str,
        context: &egui::Context,
    ) -> bool {
        let Some(profile) = self
            .configuration
            .profile(profile_id)
            .and_then(festerm_config::Profile::as_ssh)
        else {
            return false;
        };
        let size = self
            .current_session_dimensions()
            .and_then(|dimensions| terminal_size(dimensions).ok());
        let connection_profile =
            size.and_then(|size| profile.to_connection_profile_with_size(size).ok());
        let Some(connection_profile) =
            connection_profile.or_else(|| profile.to_connection_profile().ok())
        else {
            return false;
        };
        let strategy = profile
            .session_strategy()
            .unwrap_or(festerm_ssh::SessionStrategy::PlainShell);
        let Ok(options) =
            Self::with_profile_port_forwards(SshSessionOptions::manual_recovery(strategy), profile)
        else {
            return false;
        };
        self.execute_ssh_session(
            connection_profile,
            SshAuthentication::interactive(),
            options,
            Some(profile_id),
            context,
        );
        true
    }

    /// Starts a saved SSH profile as a text-mode SFTP session, using the
    /// same host-key-first interactive password flow as Quick Connect when
    /// no stored credential is present.
    pub fn start_configured_sftp_profile_interactive(
        &mut self,
        profile_id: &str,
        context: &egui::Context,
    ) -> bool {
        let Some(profile) = self
            .configuration
            .profile(profile_id)
            .and_then(festerm_config::Profile::as_ssh)
        else {
            return false;
        };
        let size = self
            .current_session_dimensions()
            .and_then(|dimensions| terminal_size(dimensions).ok());
        let connection_profile =
            size.and_then(|size| profile.to_connection_profile_with_size(size).ok());
        let Some(connection_profile) =
            connection_profile.or_else(|| profile.to_connection_profile().ok())
        else {
            return false;
        };
        self.execute_sftp_session(
            connection_profile,
            SshAuthentication::interactive(),
            Some(profile_id),
            context,
        );
        true
    }

    fn execute_ssh_session(
        &mut self,
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
        options: SshSessionOptions,
        profile_id: Option<&str>,
        context: &egui::Context,
    ) {
        // Consulted here rather than in the SSH backend so every launch path
        // (Quick Connect, Advanced form, a configured profile, or a stored-
        // password profile) benefits uniformly from a persisted trust
        // record (ADR 0020), including ad-hoc Quick Connect destinations
        // that have no saved profile at all.
        let options = match self
            .configuration
            .known_host_fingerprint(profile.identity().host(), profile.identity().port())
        {
            Some(fingerprint) => options.with_known_host_fingerprint(fingerprint),
            None => options,
        };
        // A brand-new `StartSshSession` command (Quick Connect, Advanced
        // form, or a configured profile) always begins a fresh retry
        // episode. The bounded reprompt loop for an in-connection password
        // rejection (see `reprompt_rejected_ssh_passwords`) restarts the
        // session directly rather than routing back through this dispatch
        // path, so it can carry its own running attempt count forward.
        self.place_session(SessionTab::start_ssh(
            profile,
            authentication,
            options,
            profile_id,
            0,
            context,
        ));
    }

    fn execute_sftp_session(
        &mut self,
        profile: SshConnectionProfile,
        authentication: SshAuthentication,
        profile_id: Option<&str>,
        context: &egui::Context,
    ) {
        let known_host_fingerprint = self
            .configuration
            .known_host_fingerprint(profile.identity().host(), profile.identity().port())
            .map(str::to_owned);
        self.place_session(SessionTab::start_sftp(
            profile,
            authentication,
            self.default_sftp_local_directory.clone(),
            known_host_fingerprint,
            profile_id,
            context,
        ));
    }

    fn open_sftp_file_manager(&mut self, mut target: SftpFileManagerLaunchTarget) {
        target.known_host_persisted = self
            .configuration
            .known_host_fingerprint(&target.host, target.port)
            .is_some();
        self.workspace_dirty = true;
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == self.active) {
            if matches!(tab.content, TabContent::Launcher) {
                tab.content = TabContent::SftpFileManagerAuthenticationRequired(
                    SftpFileManagerAuthenticationRequiredTab { target },
                );
                return;
            }
        }
        let id = TabId::next();
        self.tabs.push(Tab {
            id,
            content: TabContent::SftpFileManagerAuthenticationRequired(
                SftpFileManagerAuthenticationRequiredTab { target },
            ),
        });
        self.set_active(id);
    }

    fn open_configured_sftp_file_manager_profile(&mut self, profile_id: &str) {
        if let Some(target) = self.sftp_file_manager_target_for_profile(profile_id) {
            self.open_sftp_file_manager(target);
        }
    }

    fn start_sftp_file_manager(
        &mut self,
        target: SftpFileManagerLaunchTarget,
        authentication: SftpFileManagerAuthentication,
        context: &egui::Context,
    ) {
        let known_host_fingerprint = self
            .configuration
            .known_host_fingerprint(&target.host, target.port)
            .map(str::to_owned);
        let local_directory = self
            .default_sftp_local_directory
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        let tab = SftpFileManagerTab::new(
            target,
            authentication,
            known_host_fingerprint,
            local_directory,
            self.sftp_pane_order,
            context,
        );
        self.place_sftp_file_manager(tab);
    }

    fn sftp_file_manager_target_for_profile(
        &self,
        profile_id: &str,
    ) -> Option<SftpFileManagerLaunchTarget> {
        let ssh = self
            .configuration
            .profile(profile_id)
            .and_then(festerm_config::Profile::as_ssh)?;
        Some(SftpFileManagerLaunchTarget {
            label: ssh.identifier().to_owned(),
            username: ssh.username().to_owned(),
            host: ssh.host().to_owned(),
            port: ssh.port(),
            profile_id: Some(ssh.identifier().to_owned()),
            stored_credential_kind: ssh
                .credential_reference()
                .is_some()
                .then_some(ssh.credential_kind()),
            known_host_persisted: self
                .configuration
                .known_host_fingerprint(ssh.host(), ssh.port())
                .is_some(),
        })
    }

    pub(crate) fn start_stored_sftp_file_manager_profile(
        &mut self,
        profile_id: &str,
        store: Arc<dyn festerm_secret_store::SecretStore>,
        context: &egui::Context,
    ) -> bool {
        let Some(profile) = self
            .configuration
            .profile(profile_id)
            .and_then(festerm_config::Profile::as_ssh)
        else {
            return false;
        };
        let Some(reference) = profile.credential_reference() else {
            return false;
        };
        let authentication = Self::stored_sftp_file_manager_authentication(
            profile.credential_kind(),
            reference,
            store,
        );
        let Some(target) = self.sftp_file_manager_target_for_profile(profile_id) else {
            return false;
        };
        self.start_sftp_file_manager(target, authentication, context);
        true
    }

    fn stored_sftp_file_manager_authentication(
        credential_kind: festerm_config::CredentialKind,
        reference: &festerm_secret_store::SecretReference,
        store: Arc<dyn festerm_secret_store::SecretStore>,
    ) -> SftpFileManagerAuthentication {
        match credential_kind {
            festerm_config::CredentialKind::Password => {
                SftpFileManagerAuthentication::StoredPassword {
                    store,
                    reference: Arc::new(reference.duplicate_for_transport()),
                }
            }
            festerm_config::CredentialKind::PrivateKey => {
                SftpFileManagerAuthentication::StoredPrivateKey {
                    store,
                    reference: Arc::new(reference.duplicate_for_transport()),
                }
            }
        }
    }

    pub(crate) fn sftp_file_manager_target_for_tab(
        &self,
        tab_id: TabId,
    ) -> Option<SftpFileManagerLaunchTarget> {
        let session = self.session_tab(tab_id)?;
        let InspectorTransport::Ssh {
            ref username,
            ref host,
            port,
            ..
        } = session.inspector_transport
        else {
            return None;
        };
        let (profile_id, stored_credential_kind) = session
            .profile_identifier
            .as_deref()
            .and_then(|profile_id| {
                self.configuration
                    .profile(profile_id)
                    .and_then(festerm_config::Profile::as_ssh)
                    .map(|ssh| {
                        (
                            Some(ssh.identifier().to_owned()),
                            ssh.credential_reference()
                                .is_some()
                                .then_some(ssh.credential_kind()),
                        )
                    })
            })
            .unwrap_or((None, None));
        Some(SftpFileManagerLaunchTarget {
            label: session
                .profile_identifier
                .clone()
                .unwrap_or_else(|| format!("{username}@{host}")),
            username: username.clone(),
            host: host.clone(),
            port,
            profile_id,
            stored_credential_kind,
            known_host_persisted: self
                .configuration
                .known_host_fingerprint(host, port)
                .is_some(),
        })
    }

    fn with_profile_port_forwards(
        options: SshSessionOptions,
        profile: &SshProfileConfiguration,
    ) -> Result<SshSessionOptions, festerm_ssh::SshPortForwardConfigurationError> {
        options.with_profile_port_forwards(profile.port_forwards().iter())
    }

    /// Resolves only profile metadata on the application path and hands the
    /// opaque credential source to `festerm-ssh`; secret retrieval remains
    /// in that transport's worker immediately before authentication. Uses
    /// password or public-key authentication depending on the profile's
    /// stored `credential_kind`.
    pub fn start_stored_password_ssh_profile(
        &mut self,
        profile_id: &str,
        store: Arc<dyn SecretStore>,
        options: SshSessionOptions,
        context: &egui::Context,
    ) -> bool {
        let Some(profile) = self
            .configuration
            .profile(profile_id)
            .and_then(festerm_config::Profile::as_ssh)
        else {
            return false;
        };
        let Some(reference) = profile.credential_reference() else {
            return false;
        };
        let Ok(connection_profile) = profile.to_connection_profile() else {
            return false;
        };
        let authentication = match profile.credential_kind() {
            festerm_config::CredentialKind::Password => {
                SshAuthentication::stored_password(store, reference)
            }
            festerm_config::CredentialKind::PrivateKey => {
                SshAuthentication::stored_private_key(store, reference)
            }
        };
        self.execute_ssh_session(
            connection_profile,
            authentication,
            options,
            Some(profile_id),
            context,
        );
        true
    }

    /// Resolves only profile metadata on the application path and hands the
    /// opaque credential source to the SFTP worker; secret retrieval remains
    /// in that worker immediately before authentication.
    pub fn start_stored_password_sftp_profile(
        &mut self,
        profile_id: &str,
        store: Arc<dyn SecretStore>,
        context: &egui::Context,
    ) -> bool {
        let Some(profile) = self
            .configuration
            .profile(profile_id)
            .and_then(festerm_config::Profile::as_ssh)
        else {
            return false;
        };
        let Some(reference) = profile.credential_reference() else {
            return false;
        };
        let Ok(connection_profile) = profile.to_connection_profile() else {
            return false;
        };
        let authentication = match profile.credential_kind() {
            festerm_config::CredentialKind::Password => {
                SshAuthentication::stored_password(store, reference)
            }
            festerm_config::CredentialKind::PrivateKey => {
                SshAuthentication::stored_private_key(store, reference)
            }
        };
        self.execute_sftp_session(
            connection_profile,
            authentication,
            Some(profile_id),
            context,
        );
        true
    }

    fn resolve_host_key_trust(&mut self, tab: TabId, decision: HostKeyTrustDecision) {
        if let Some(session) = self.session_tab_mut(tab) {
            if let Err(error) = session.resolve_host_key_trust(decision) {
                session.controller.record_host_key_resolution_error(error);
            }
            // The "Reject"/"Accept Once" buttons steal keyboard focus from the
            // terminal for however long the prompt is on screen. Unlike the
            // close/paste/rename overlays, nothing else claims focus afterwards,
            // so without this the terminal is left silently unfocused - typing
            // does nothing until the user clicks into it.
            session.view.request_focus_on_next_frame();
            return;
        }
        let Some(tab) = self.sftp_file_manager_tab_mut(tab) else {
            return;
        };
        let _ = tab.resolve_host_key_trust(decision);
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

    fn place_session(&mut self, mut session: SessionTab) {
        session.set_scrollback_limit(self.scrollback_limit);
        self.workspace_dirty = true;
        // Starting a session from the active Launcher or restored
        // authentication-required tab replaces that surface in place (same
        // position, same identity) rather than leaving it behind alongside
        // a new session tab.
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == self.active) {
            if matches!(
                tab.content,
                TabContent::Launcher
                    | TabContent::SshAuthenticationRequired(_)
                    | TabContent::SftpAuthenticationRequired(_)
                    | TabContent::SftpFileManagerAuthenticationRequired(_)
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
        self.set_active(id);
    }

    fn place_sftp_file_manager(&mut self, tab_content: SftpFileManagerTab) {
        self.workspace_dirty = true;
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == self.active) {
            if matches!(
                tab.content,
                TabContent::Launcher | TabContent::SftpFileManagerAuthenticationRequired(_)
            ) {
                tab.content = TabContent::SftpFileManager(Box::new(tab_content));
                return;
            }
        }
        let id = TabId::next();
        self.tabs.push(Tab {
            id,
            content: TabContent::SftpFileManager(Box::new(tab_content)),
        });
        self.set_active(id);
    }

    fn apply_scrollback_limit_to_sessions(&mut self) {
        for tab in &mut self.tabs {
            if let TabContent::Session(session) = &mut tab.content {
                session.set_scrollback_limit(self.scrollback_limit);
            }
        }
    }

    fn resolve_ssh_password(&mut self, tab: TabId, password: String) {
        let Some(session) = self.session_tab_mut(tab) else {
            return;
        };
        if let Err(error) = session.resolve_ssh_password(password) {
            session.controller.record_operation_error("password", error);
        }
        // Mirrors `resolve_host_key_trust`: submitting the password steals
        // keyboard focus for however long the prompt was on screen, and
        // nothing else claims it afterwards.
        session.view.request_focus_on_next_frame();
    }

    /// Restarts any session tab whose most recent connection attempt was
    /// rejected on a plain, up-front password (`SessionErrorKind::
    /// Authentication`), up to `MAX_SSH_PASSWORD_PROMPT_ATTEMPTS`, mimicking
    /// `ssh`'s own "Permission denied, please try again." retry loop. The
    /// fresh attempt authenticates interactively (host-key-first, then an
    /// in-terminal password prompt on the new connection) rather than
    /// asking for another password blind before reconnecting. Runs across
    /// every open tab, independent of which is active, matching
    /// `session_tabs_mut`'s "keep making progress in the background" policy.
    pub fn reprompt_rejected_ssh_passwords(&mut self, context: &egui::Context) {
        let mut restarts = Vec::new();
        for (index, tab) in self.tabs.iter().enumerate() {
            let TabContent::Session(session) = &tab.content else {
                continue;
            };
            let Some(retry) = &session.ssh_password_retry else {
                continue;
            };
            if retry.attempts >= MAX_SSH_PASSWORD_PROMPT_ATTEMPTS {
                continue;
            }
            let Some(SessionLifecycle::Failed(error)) = session.controller.lifecycle() else {
                continue;
            };
            if error.kind() != SessionErrorKind::Authentication {
                continue;
            }
            restarts.push((
                index,
                retry.profile.clone(),
                retry.profile_identifier.clone(),
                retry.options.clone(),
            ));
        }
        for (index, profile, profile_identifier, options) in restarts {
            let mut restarted = SessionTab::start_ssh(
                profile,
                SshAuthentication::interactive(),
                options,
                profile_identifier.as_deref(),
                0,
                context,
            );
            restarted.set_scrollback_limit(self.scrollback_limit);
            if let Some(tab) = self.tabs.get_mut(index) {
                tab.content = TabContent::Session(Box::new(restarted));
            }
        }
    }

    fn activate(&mut self, id: TabId) {
        if self.tabs.iter().any(|tab| tab.id == id) {
            self.set_active(id);
            self.workspace_dirty = true;
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
        self.set_active(self.tabs[next].id);
        self.workspace_dirty = true;
    }

    /// Switches the active tab and, if the newly active tab is a session,
    /// clears its "new output since last active" flag (feature request
    /// #68): once the user is looking at it again there is nothing left to
    /// notify them about.
    fn set_active(&mut self, id: TabId) {
        self.active = id;
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            if let TabContent::Session(session) = &mut tab.content {
                session.has_new_output_since_active = false;
            }
        }
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
        self.workspace_dirty = true;
    }

    /// Moves a tab one place without changing which tab is active. Invalid ids
    /// and attempts to move beyond an edge are no-ops.
    fn move_tab(&mut self, id: TabId, delta: i64) {
        let Some(from) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let to = from as i64 + delta;
        if !(0..self.tabs.len() as i64).contains(&to) {
            return;
        }
        self.tabs.swap(from, to as usize);
        self.workspace_dirty = true;
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
        self.workspace_dirty = true;
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
            self.set_active(self.tabs[next_index].id);
        }
    }
}

#[cfg(test)]
impl SessionTab {
    fn for_test_ssh(
        session: crate::session_controller::fake::FakeSshSession,
        username: &str,
        host: &str,
        port: u16,
    ) -> Self {
        let dimensions = Dimensions::new(80, 24).expect("test dimensions are valid");
        let mut controller =
            SessionController::for_test(ApplicationSession::TestSsh(session.clone()));
        controller.set_lifecycle_for_test(SessionLifecycle::Running);
        Self {
            terminal: Terminal::new(dimensions).expect("terminal allocation"),
            controller,
            view: TerminalView::default(),
            label: format!("{username}@{host}"),
            launch_secondary: Some(format!("SSH · {host}:{port}")),
            profile_identifier: None,
            inspector_transport: InspectorTransport::Ssh {
                username: username.to_owned(),
                host: host.to_owned(),
                port,
                persistence: None,
            },
            eviction_notice_shown: false,
            ssh_password_retry: None,
            has_new_output_since_active: false,
            search: crate::search::TerminalSearchState::default(),
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

    pub(crate) fn replace_active_with_test_ssh_session(
        &mut self,
        session: crate::session_controller::fake::FakeSshSession,
        username: &str,
        host: &str,
        port: u16,
    ) -> TabId {
        let tab = self.active;
        let replacement = SessionTab::for_test_ssh(session, username, host, port);
        self.active_tab_mut().content = TabContent::Session(Box::new(replacement));
        tab
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
    fn external_link_policy_accepts_only_normalized_http_and_https_urls() {
        assert_eq!(
            festerm_core::normalize_external_web_url("HTTPS://Example.COM/path"),
            Some("https://example.com/path".to_owned())
        );
        for target in [
            "https://",
            "https://a b",
            "https://example.com/\u{202e}spoof",
            "https://github.com@evil.example/login",
            "mailto:user@example.com",
            "file:///etc/passwd",
            "javascript:alert(1)",
        ] {
            assert_eq!(
                festerm_core::normalize_external_web_url(target),
                None,
                "{target:?} must not reach the OS opener"
            );
        }

        let command = AppCommand::OpenExternalLink {
            target: ExternalLinkTarget::new("https://example.com/private-query".to_owned()),
        };
        assert!(!format!("{command:?}").contains("private-query"));
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
    fn configured_interactive_ssh_profile_records_its_profile_identifier() {
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

        assert!(state.start_configured_ssh_profile_interactive("production", &context));

        let TabContent::Session(session) = &state.active_tab().content else {
            panic!("configured SSH profile must place a session tab");
        };
        assert_eq!(session.profile_identifier.as_deref(), Some("production"));
        let target = state
            .sftp_file_manager_target_for_tab(state.active())
            .expect("SSH session should offer GUI SFTP");
        assert_eq!(target.profile_id.as_deref(), Some("production"));
        assert_eq!(target.stored_credential_kind, None);
    }

    #[test]
    fn stored_credential_ssh_profile_exposes_its_saved_credential_to_gui_sftp() {
        let context = egui::Context::default();
        let profile = festerm_config::Profile::ssh(
            "production",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .expect("test SSH profile is valid")
        .with_credential_reference(festerm_secret_store::SecretReference::generate())
        .expect("SSH profile accepts an opaque reference");
        let configuration = Configuration::new(vec![profile]).expect("test configuration is valid");
        let mut state = AppState::for_test_with_configuration(configuration);
        let store: Arc<dyn festerm_secret_store::SecretStore> =
            Arc::new(festerm_secret_store::MemorySecretStore::new());

        assert!(state.start_stored_password_ssh_profile(
            "production",
            store,
            SshSessionOptions::new(),
            &context,
        ));

        let TabContent::Session(session) = &state.active_tab().content else {
            panic!("stored-credential SSH profile must place a session tab");
        };
        assert_eq!(session.profile_identifier.as_deref(), Some("production"));
        let target = state
            .sftp_file_manager_target_for_tab(state.active())
            .expect("SSH session should offer GUI SFTP");
        assert_eq!(target.profile_id.as_deref(), Some("production"));
        assert_eq!(
            target.stored_credential_kind,
            Some(festerm_config::CredentialKind::Password)
        );
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
            state.session_tabs_with_id_mut().next().is_some(),
            "only the separately restored local profile started a session"
        );
        assert_eq!(
            state.session_tabs_with_id_mut().count(),
            1,
            "the SSH restoration starts no network transport"
        );
    }

    #[test]
    fn restored_sftp_tab_requires_fresh_authentication_without_starting_a_session() {
        let context = egui::Context::default();
        let workspace = WorkspaceConfiguration::new(
            vec![WorkspaceTab::sftp_session("remote", "production").expect("SFTP tab is valid")],
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

        let TabContent::SftpAuthenticationRequired(tab) = &state.active_tab().content else {
            panic!("saved SFTP metadata must restore an authentication-required surface");
        };
        assert_eq!(tab.profile.identifier(), "production");
        assert_eq!(tab.profile.host(), "ssh.example.test");
        assert_eq!(tab.profile.port(), 2200);
        assert_eq!(tab.profile.username(), "deploy");
        assert_eq!(
            state.session_tabs_with_id_mut().count(),
            0,
            "the SFTP restoration starts no network transport"
        );
    }

    #[test]
    fn gui_sftp_workspace_restore_requires_fresh_authentication() {
        let context = egui::Context::default();
        let workspace = WorkspaceConfiguration::new(
            vec![
                WorkspaceTab::sftp_file_manager("remote-files", "production")
                    .expect("GUI SFTP tab is valid"),
            ],
            Some("remote-files".to_owned()),
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

        let TabContent::SftpFileManagerAuthenticationRequired(tab) = &state.active_tab().content
        else {
            panic!("saved GUI SFTP metadata must restore an authentication-required surface");
        };
        assert_eq!(tab.target.profile_id.as_deref(), Some("production"));
        assert_eq!(tab.target.host, "ssh.example.test");
        assert_eq!(tab.target.port, 2200);
        assert_eq!(tab.target.username, "deploy");
        assert_eq!(
            state.session_tabs_with_id_mut().count(),
            0,
            "the restored GUI SFTP surface starts no live network transport"
        );
    }

    #[test]
    fn configured_ssh_profiles_can_open_gui_sftp_authentication_surfaces() {
        let context = egui::Context::default();
        let configuration = Configuration::new(vec![festerm_config::Profile::ssh(
            "production",
            "ssh.example.test",
            2200,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .expect("SSH profile is valid")])
        .expect("configuration is valid");
        let mut state = AppState::for_test_with_configuration(configuration);

        state.dispatch(
            AppCommand::OpenConfiguredSftpFileManagerProfile {
                profile_id: "production".to_owned(),
            },
            &context,
        );

        let TabContent::SftpFileManagerAuthenticationRequired(tab) = &state.active_tab().content
        else {
            panic!("opening GUI SFTP should produce an authentication-required surface");
        };
        assert_eq!(tab.target.profile_id.as_deref(), Some("production"));
        assert_eq!(tab.target.label, "production");
        assert_eq!(tab.target.host, "ssh.example.test");
        assert_eq!(tab.target.port, 2200);
        assert_eq!(tab.target.username, "deploy");
    }

    #[test]
    fn gui_sftp_builds_stored_password_and_private_key_authentication_without_loading_secrets() {
        let store: Arc<dyn festerm_secret_store::SecretStore> =
            Arc::new(festerm_secret_store::MemorySecretStore::new());
        let reference = festerm_secret_store::SecretReference::generate();

        let password = AppState::stored_sftp_file_manager_authentication(
            festerm_config::CredentialKind::Password,
            &reference,
            Arc::clone(&store),
        );
        let private_key = AppState::stored_sftp_file_manager_authentication(
            festerm_config::CredentialKind::PrivateKey,
            &reference,
            store,
        );

        assert!(matches!(
            password,
            SftpFileManagerAuthentication::StoredPassword { .. }
        ));
        assert!(matches!(
            private_key,
            SftpFileManagerAuthentication::StoredPrivateKey { .. }
        ));
        assert!(!format!("{password:?}").contains(reference.to_persisted_string().as_str()));
        assert!(!format!("{private_key:?}").contains(reference.to_persisted_string().as_str()));
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
        let sftp = state
            .configuration()
            .profile("production")
            .and_then(festerm_config::Profile::as_ssh)
            .unwrap()
            .clone();
        state.tabs.push(Tab {
            id: TabId::next(),
            content: TabContent::SftpAuthenticationRequired(SftpAuthenticationRequiredTab {
                profile: sftp,
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
            ["tab-1", "tab-2", "tab-3", "tab-4"]
        );
        assert!(matches!(workspace.tabs()[0], WorkspaceTab::LocalSession(_)));
        assert!(matches!(workspace.tabs()[1], WorkspaceTab::Settings(_)));
        assert!(matches!(workspace.tabs()[2], WorkspaceTab::SshSession(_)));
        assert!(matches!(workspace.tabs()[3], WorkspaceTab::SftpSession(_)));
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
    fn workspace_capture_omits_runtime_markdown_viewer_tabs() {
        let mut state = AppState::for_test();
        state.tabs.push(Tab {
            id: TabId::next(),
            content: TabContent::MarkdownViewer(Box::new(MarkdownViewerTab::open_local(
                PathBuf::from("/docs/readme.md"),
            ))),
        });
        state.active = state.tabs[1].id;

        let captured = state.capture_workspace_configuration().unwrap();
        let workspace = captured.workspace().unwrap();

        assert!(matches!(workspace.tabs(), [WorkspaceTab::Launcher(_)]));
        assert_eq!(workspace.focused_tab_id(), Some("tab-1"));
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
    fn host_key_trust_decisions_map_onto_the_ssh_transport_decisions_including_persistence() {
        assert_eq!(
            HostTrustDecision::from(HostKeyTrustDecision::Reject),
            HostTrustDecision::Reject
        );
        assert_eq!(
            HostTrustDecision::from(HostKeyTrustDecision::AcceptOnce),
            HostTrustDecision::AcceptOnce
        );
        // ADR 0020: the durable trust-record write is a composition-root
        // concern intercepted in `FesTermApp::screen_command`, but the
        // SSH-transport-level decision this maps onto is identical to
        // `AcceptOnce` for the current connection either way.
        assert_eq!(
            HostTrustDecision::from(HostKeyTrustDecision::AcceptAndPersist),
            HostTrustDecision::AcceptAndPersist
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
            None,
            InspectorTransport::Ssh {
                username: "test-user".to_owned(),
                host: "example.invalid".to_owned(),
                port: 22,
                persistence: None,
            },
            None,
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
    fn interactive_ssh_command_replaces_the_active_launcher_in_place() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let launcher_id = state.active();

        state.dispatch(
            AppCommand::StartSshSession {
                profile: ssh_profile(),
                authentication: SshAuthentication::interactive(),
                options: SshSessionOptions::new(),
            },
            &context,
        );

        assert_eq!(
            state.tabs().len(),
            1,
            "Quick Connect with no upfront credential must reuse the existing Launcher tab, \
             not open a second tab"
        );
        assert_eq!(state.active(), launcher_id);
        let TabContent::Session(session) = &state.active_tab().content else {
            panic!(
                "an interactive (host-key-first) SSH command must place a session tab, not a \
                 pre-connection prompt"
            );
        };
        assert_eq!(session.label, "test-user@192.0.2.1");
        assert!(matches!(
            session.controller.session(),
            Some(ApplicationSession::Ssh(_))
        ));
        // Interactive sessions authenticate on the live connection itself
        // (host key first, then an in-terminal password prompt), so they
        // never populate the full-reconnect retry state that a plain typed
        // password does.
        assert!(session.ssh_password_retry.is_none());

        state.dispatch(AppCommand::CloseTab(launcher_id), &context);
    }

    #[test]
    fn a_plain_typed_password_session_retains_full_reconnect_retry_state() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let launcher_id = state.active();

        state.dispatch(
            AppCommand::StartSshSession {
                profile: ssh_profile(),
                authentication: SshAuthentication::password("transient-test-password"),
                options: SshSessionOptions::new(),
            },
            &context,
        );

        let TabContent::Session(session) = &state.active_tab().content else {
            panic!("SSH command must place a session tab");
        };
        let retry = session
            .ssh_password_retry
            .as_ref()
            .expect("a plain typed password must retain full-reconnect retry state");
        assert_eq!(
            retry.attempts, 0,
            "a first attempt must start the retry episode at zero"
        );

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
    fn open_profiles_is_a_singleton_and_reactivates_the_existing_tab() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();

        state.dispatch(AppCommand::OpenProfiles, &context);
        assert_eq!(state.tabs().len(), 2);
        let profiles_id = state.active();
        assert!(matches!(state.active_tab().content, TabContent::Profiles));

        // Switch away, then request Profiles again: it must reactivate the
        // same tab rather than creating a second one.
        let launcher_id = launcher_ids(&state)[0];
        state.dispatch(AppCommand::ActivateTab(launcher_id), &context);
        assert_eq!(state.active(), launcher_id);

        state.dispatch(AppCommand::OpenProfiles, &context);
        assert_eq!(state.tabs().len(), 2, "Profiles is a singleton chip");
        assert_eq!(state.active(), profiles_id);
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
    fn a_new_local_session_inherits_the_current_windows_terminal_dimensions() {
        // Regression test: opening a new local session while another one is
        // already running in an already-resized window must start the new
        // session at that window's size, not visibly snap back to the
        // application's baseline default until the next resize corrects it.
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        state.dispatch(AppCommand::StartLocalSession, &context);
        let TabContent::Session(first) = &mut state.active_tab_mut().content else {
            panic!("expected a session tab");
        };
        let resized = Dimensions::new(140, 45).expect("valid dimensions");
        assert_ne!(first.terminal.dimensions(), resized);
        first
            .terminal
            .resize(resized)
            .expect("terminal resize succeeds");

        // Opening a second session from the same (non-launcher) active tab
        // opens a new tab rather than replacing the first.
        state.dispatch(AppCommand::StartLocalSession, &context);
        assert_eq!(state.tabs().len(), 2);
        let TabContent::Session(second) = &state.active_tab().content else {
            panic!("expected a session tab");
        };
        assert_eq!(
            second.terminal.dimensions(),
            resized,
            "new session must start at the current window's terminal size"
        );
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
    fn toggle_show_session_details_flips_visibility_and_defaults_to_shown() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        assert!(state.show_session_details());

        state.dispatch(AppCommand::ToggleShowSessionDetails, &context);
        assert!(!state.show_session_details());

        state.dispatch(AppCommand::ToggleShowSessionDetails, &context);
        assert!(state.show_session_details());
    }

    #[test]
    fn reset_interface_settings_also_restores_show_session_details() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        state.dispatch(AppCommand::ToggleShowSessionDetails, &context);
        assert!(!state.show_session_details());

        state.dispatch(AppCommand::ResetInterfaceSettings, &context);
        assert!(state.show_session_details());
    }

    #[test]
    fn toggle_restore_workspace_flips_state_and_resets_to_off() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        assert!(!state.restore_workspace(), "off by default");

        state.dispatch(AppCommand::ToggleRestoreWorkspace, &context);
        assert!(state.restore_workspace());

        state.dispatch(AppCommand::ResetInterfaceSettings, &context);
        assert!(!state.restore_workspace());
    }

    #[test]
    fn toggle_close_confirmation_flips_state_and_resets_to_on() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        assert!(state.confirm_session_close(), "on by default");

        state.dispatch(AppCommand::ToggleConfirmSessionClose, &context);
        assert!(!state.confirm_session_close());

        state.dispatch(AppCommand::ResetInterfaceSettings, &context);
        assert!(state.confirm_session_close());
    }

    #[test]
    fn terminal_typography_changes_and_resets_independently() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        assert_eq!(state.terminal_font(), TerminalFontPreference::JetBrainsMono);
        assert!(!state.terminal_ligatures());
        assert_eq!(
            state.emoji_presentation(),
            EmojiPresentationPreference::Color
        );

        state.dispatch(
            AppCommand::SetTerminalFont(TerminalFontPreference::JuliaMono),
            &context,
        );
        state.dispatch(AppCommand::ToggleTerminalLigatures, &context);
        state.dispatch(
            AppCommand::SetEmojiPresentation(EmojiPresentationPreference::Monochrome),
            &context,
        );

        assert_eq!(state.terminal_font(), TerminalFontPreference::JuliaMono);
        assert!(state.terminal_ligatures());
        assert_eq!(
            state.interface_settings().terminal_font(),
            TerminalFontPreference::JuliaMono
        );
        assert!(state.interface_settings().terminal_ligatures());
        assert_eq!(
            state.interface_settings().emoji_presentation(),
            EmojiPresentationPreference::Monochrome
        );

        state.dispatch(AppCommand::ResetInterfaceSettings, &context);
        assert_eq!(state.terminal_font(), TerminalFontPreference::JetBrainsMono);
        assert!(!state.terminal_ligatures());
        assert_eq!(
            state.emoji_presentation(),
            EmojiPresentationPreference::Color
        );
    }

    #[test]
    fn terminal_font_policy_applies_to_every_existing_session_view() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        state.dispatch(AppCommand::StartLocalSession, &context);
        state.dispatch(AppCommand::StartLocalSession, &context);
        assert_eq!(state.tabs().len(), 2);

        state.apply_terminal_font_set(
            festerm_ui_egui::TerminalFontSet::default().with_color_emoji(false),
        );

        for tab in state.tabs() {
            let TabContent::Session(session) = &tab.content else {
                panic!("expected only session tabs");
            };
            assert!(!session.view.color_emoji_enabled());
        }
    }

    #[test]
    fn scroll_speed_changes_and_resets_to_normal() {
        // Feature request #67: scroll speed is a Settings clickstop, not a
        // toggle, so it needs its own coverage distinct from the boolean
        // preferences above.
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        assert_eq!(state.scroll_speed(), ScrollSpeedPreference::Normal);

        state.dispatch(
            AppCommand::SetScrollSpeed(ScrollSpeedPreference::VerySlow),
            &context,
        );
        assert_eq!(state.scroll_speed(), ScrollSpeedPreference::VerySlow);
        assert_eq!(
            state.interface_settings().scroll_speed(),
            ScrollSpeedPreference::VerySlow
        );

        state.dispatch(AppCommand::ResetInterfaceSettings, &context);
        assert_eq!(state.scroll_speed(), ScrollSpeedPreference::Normal);
    }

    #[test]
    fn scrollback_limit_changes_apply_only_to_subsequent_sessions() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();

        state.dispatch(AppCommand::StartLocalSession, &context);
        let TabContent::Session(first) = &state.active_tab().content else {
            panic!("expected a session tab");
        };
        assert_eq!(
            first.terminal.scrollback_stats().limit_bytes(),
            ScrollbackLimitPreference::MiB64.bytes()
        );

        state.dispatch(
            AppCommand::SetScrollbackLimit(ScrollbackLimitPreference::Disabled),
            &context,
        );
        assert_eq!(
            state.interface_settings().scrollback_limit(),
            ScrollbackLimitPreference::Disabled
        );
        let TabContent::Session(first) = &state.tabs()[0].content else {
            panic!("expected a session tab");
        };
        assert_eq!(
            first.terminal.scrollback_stats().limit_bytes(),
            ScrollbackLimitPreference::MiB64.bytes(),
            "changing the preference must not mutate an existing session"
        );

        state.dispatch(AppCommand::StartLocalSession, &context);
        let TabContent::Session(second) = &state.active_tab().content else {
            panic!("expected a session tab");
        };
        assert_eq!(second.terminal.scrollback_stats().limit_bytes(), 0);

        state.dispatch(AppCommand::ResetInterfaceSettings, &context);
        assert_eq!(state.scrollback_limit(), ScrollbackLimitPreference::MiB64);
    }

    #[test]
    fn toggle_pulse_new_output_dot_flips_state_and_resets_to_off() {
        // Feature request #68.
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        assert!(!state.pulse_new_output_dot());

        state.dispatch(AppCommand::TogglePulseNewOutputDot, &context);
        assert!(state.pulse_new_output_dot());
        assert!(state.interface_settings().pulse_new_output_dot());

        state.dispatch(AppCommand::ResetInterfaceSettings, &context);
        assert!(!state.pulse_new_output_dot());
    }

    #[test]
    fn toggle_show_resumable_sessions_flips_state_and_resets_to_off() {
        // Feature request #70.
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        assert!(!state.show_resumable_sessions());

        state.dispatch(AppCommand::ToggleShowResumableSessions, &context);
        assert!(state.show_resumable_sessions());
        assert!(state.interface_settings().show_resumable_sessions());

        state.dispatch(AppCommand::ResetInterfaceSettings, &context);
        assert!(!state.show_resumable_sessions());
    }

    #[test]
    fn activating_a_tab_clears_its_new_output_flag() {
        // Feature request #68: switching back to a tab that had unseen
        // background output must clear the flag, since the user is now
        // looking at it and there is nothing left to notify them of.
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let first = state.active();
        state.dispatch(AppCommand::StartLocalSession, &context);
        let second = state.active();

        let TabContent::Session(session) = &mut state.active_tab_mut().content else {
            panic!("expected a session tab");
        };
        session.has_new_output_since_active = true;

        // Switching away and back to the flagged tab clears it.
        state.dispatch(AppCommand::ActivateTab(first), &context);
        state.dispatch(AppCommand::ActivateTab(second), &context);
        let TabContent::Session(session) = &state.active_tab().content else {
            panic!("expected a session tab");
        };
        assert!(!session.has_new_output_since_active);
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
    fn move_tab_commands_move_one_place_without_changing_active_identity() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let launcher = state.active();
        state.dispatch(AppCommand::OpenSettings, &context);
        let settings = state.active();

        state.dispatch(AppCommand::MoveTabLeft(settings), &context);
        assert_eq!(state.active(), settings);
        assert_eq!(state.tabs()[0].id, settings);
        assert_eq!(state.tabs()[1].id, launcher);

        state.dispatch(AppCommand::MoveTabLeft(launcher), &context);
        assert_eq!(state.active(), settings);
        assert_eq!(state.tabs()[0].id, launcher);
        assert_eq!(state.tabs()[1].id, settings);
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

    #[test]
    fn serial_startup_failure_surfaces_a_concise_error() {
        let context = egui::Context::default();
        let dimensions = Dimensions::new(80, 24).expect("test dimensions are valid");
        let settings = LineSettings::with_defaults("/dev/festerm-test-nonexistent-000")
            .expect("test line settings are valid");
        let inspector_transport = inspector_transport_from_settings(&settings);
        let result = SerialSession::open_with_notifier(settings, make_notifier(&context))
            .map(ApplicationSession::Serial);
        let session = SessionTab::from_serial_session_result(
            result,
            dimensions,
            "nonexistent-device",
            Some("Serial · 115200 baud".to_owned()),
            None,
            inspector_transport,
        );

        assert!(
            session.controller.start_error().is_some(),
            "a nonexistent serial device must surface a startup error"
        );
        assert!(
            session
                .controller
                .status_line()
                .starts_with("Serial session unavailable:"),
            "serial failures use the controller's ordinary startup-error state"
        );
        assert!(session
            .terminal
            .row_text(0)
            .is_some_and(|row| row.starts_with("Serial session could not start.")));
    }

    #[test]
    fn resuming_a_missing_session_surfaces_a_concise_error() {
        // Feature request #70: attempting to resume a session that has
        // vanished by the time the user clicks it (e.g. it exited) must
        // surface an ordinary startup error, not panic.
        let context = egui::Context::default();
        let mut state = AppState::for_test();

        state.dispatch(
            AppCommand::ResumeUnattachedSession {
                name: "nonexistent-resumable-session".to_owned(),
            },
            &context,
        );

        let TabContent::Session(session) = &state.active_tab_mut().content else {
            panic!("resuming a session always opens a session tab, even on failure");
        };
        assert!(
            session.controller.start_error().is_some(),
            "resuming a missing session must surface a startup error"
        );
    }

    #[test]
    fn serial_command_replaces_the_active_launcher_in_place() {
        let context = egui::Context::default();
        let mut state = AppState::for_test();
        let launcher_id = state.active();

        let settings = LineSettings::with_defaults("/dev/festerm-test-nonexistent-000")
            .expect("test line settings are valid");
        state.dispatch(AppCommand::StartSerialSession { settings }, &context);

        assert_eq!(state.tabs().len(), 1);
        assert_eq!(state.active(), launcher_id);
        let TabContent::Session(session) = &state.active_tab().content else {
            panic!("serial command must place a session tab");
        };
        assert_eq!(session.label, "/dev/festerm-test-nonexistent-000");
        assert_eq!(session.system_label(), "Serial");
        assert_eq!(session.status_bar_label(), "Failed");
        assert!(
            session.controller.start_error().is_some(),
            "the nonexistent device proves the serial launch path was exercised"
        );
        assert!(matches!(
            session.inspector_transport,
            InspectorTransport::Serial { .. }
        ));

        state.dispatch(AppCommand::CloseTab(launcher_id), &context);
    }

    #[test]
    fn configured_serial_profile_command_starts_a_serial_session() {
        let context = egui::Context::default();
        let configuration =
            Configuration::new(vec![festerm_config::Profile::serial_with_defaults(
                "my-device",
                "/dev/festerm-test-nonexistent-serial-profile",
            )
            .expect("test serial profile is valid")])
            .expect("test configuration is valid");
        let mut state = AppState::for_test_with_configuration(configuration);
        let launcher_id = state.active();

        state.dispatch(
            AppCommand::StartConfiguredSerialProfile {
                profile_id: "my-device".to_owned(),
            },
            &context,
        );

        assert_eq!(state.tabs().len(), 1);
        assert_eq!(state.active(), launcher_id);
        let TabContent::Session(session) = &state.active_tab().content else {
            panic!("configured serial profile must replace the active launcher");
        };
        assert_eq!(session.label, "my-device");
        assert_eq!(session.profile_identifier.as_deref(), Some("my-device"));
        assert!(
            session.controller.start_error().is_some(),
            "the nonexistent device proves the configured launch path attempted its metadata"
        );
    }
}
