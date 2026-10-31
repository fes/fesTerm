//! View-models and seams for the mobile shell.
//!
//! Saved profiles come from `festerm-mobile-profiles` through
//! [`crate::profiles::Profiles`]; the open-surface source is a trait so the
//! gallery can inject fixtures. Fixtures live only in the gallery and tests.

use festerm_ui_egui::icon::Icon;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileKind {
    Ssh,
    DevTunnel,
    Sftp,
}

impl ProfileKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ssh => "SSH",
            Self::DevTunnel => "Dev Tunnel",
            Self::Sftp => "SFTP",
        }
    }

    pub const fn glyph(self) -> Icon {
        match self {
            Self::Ssh => Icon::SshRemote,
            Self::DevTunnel => Icon::DevTunnel,
            Self::Sftp => Icon::FileTransfer,
        }
    }
}

/// One saved-profile row for lists and cards; secrets never appear here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub kind: ProfileKind,
    /// Protocol and target, e.g. `SSH · dev@example.net`.
    pub detail: String,
    /// Seconds since the Unix epoch of the last launch, if any.
    pub last_used: Option<u64>,
}

pub fn profile_detail(kind: ProfileKind, host: &str, username: &str) -> String {
    if username.is_empty() {
        format!("{} · {host}", kind.label())
    } else {
        format!("{} · {username}@{host}", kind.label())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceKind {
    Local,
    Ssh,
    DevTunnel,
    Files,
}

impl SurfaceKind {
    pub const fn glyph(self) -> Icon {
        match self {
            Self::Local => Icon::LocalTerminal,
            Self::Ssh => Icon::SshRemote,
            Self::DevTunnel => Icon::DevTunnel,
            Self::Files => Icon::FileTransfer,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceState {
    Connected,
    Connecting,
    Disconnected,
}

/// One open surface (terminal or files) for chips, sheet and Sessions tab.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurfaceSummary {
    pub id: u64,
    pub title: String,
    pub kind: SurfaceKind,
    /// e.g. `SSH · dev@example.net`.
    pub detail: String,
    pub state: SurfaceState,
    /// e.g. `Connected · tmux / work` or `Disconnected · history available`.
    pub status: String,
    pub current: bool,
}

/// A remote session the user may reattach to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reattachable {
    pub title: String,
    pub detail: String,
    pub host: String,
}

pub trait SessionSource {
    fn surfaces(&self) -> Vec<SurfaceSummary>;
    fn reattachable(&self) -> Vec<Reattachable>;
}

#[derive(Clone, Debug, Default)]
pub struct StaticSessions {
    pub surfaces: Vec<SurfaceSummary>,
    pub reattachable: Vec<Reattachable>,
}

impl SessionSource for StaticSessions {
    fn surfaces(&self) -> Vec<SurfaceSummary> {
        self.surfaces.clone()
    }

    fn reattachable(&self) -> Vec<Reattachable> {
        self.reattachable.clone()
    }
}

/// Connection status line under the terminal header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionLine {
    pub state: SurfaceState,
    pub label: String,
    /// Right-aligned context, e.g. `tmux · work`.
    pub context: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChromeModel {
    pub title: String,
    /// Protocol and target.
    pub subtitle: String,
    pub line: ConnectionLine,
}

/// Capabilities that production wiring has not delivered yet stay hidden.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Capabilities {
    pub dev_tunnel_profiles: bool,
    pub sftp_profiles: bool,
    pub key_authentication: bool,
    pub remote_session_choice: bool,
    pub paste: bool,
}

impl Capabilities {
    /// What the production shell supports today: SSH profiles with password
    /// or key authentication, remote sessions and paste. Dev Tunnel and SFTP
    /// profile kinds stay hidden until their flows are wired.
    pub const fn production() -> Self {
        Self {
            dev_tunnel_profiles: false,
            sftp_profiles: false,
            key_authentication: true,
            remote_session_choice: true,
            paste: true,
        }
    }

    /// Every mock-up affordance, for the gallery's deterministic fixtures.
    pub const fn all() -> Self {
        Self {
            dev_tunnel_profiles: true,
            sftp_profiles: true,
            key_authentication: true,
            remote_session_choice: true,
            paste: true,
        }
    }
}

/// Fingerprint review for a first or changed host key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostVerification {
    pub name: String,
    pub target: String,
    pub algorithm: String,
    pub fingerprint: String,
    /// Previously trusted fingerprint when the key changed.
    pub changed_from: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostDecision {
    Trust,
    Cancel,
}

/// Reconnect banner after a dropped plain SSH session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconnectBanner {
    pub detail: Option<String>,
    pub status: String,
    /// `tmux · work` when reconnecting reattaches a durable remote session.
    pub reattach: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevTunnelChoices {
    pub account_name: String,
    pub account_user: String,
    pub tunnels: Vec<String>,
    pub tunnel: usize,
    pub sessions: Vec<String>,
    pub session: usize,
}
