//! View-models and seams for the mobile shell.
//!
//! The profile store and open-surface source are traits so persistent
//! storage (Keychain-backed profiles, SFTP/tunnel surfaces) can plug in
//! without touching the screens. Production builds use the in-memory store
//! and the live-session source; fixtures live only in the gallery/tests.

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Profile {
    pub id: u64,
    pub name: String,
    pub kind: ProfileKind,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// Protocol and target, e.g. `SSH · dev@example.net`.
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewProfile {
    pub name: String,
    pub kind: ProfileKind,
    pub host: String,
    pub port: u16,
    pub username: String,
}

pub fn profile_detail(kind: ProfileKind, host: &str, username: &str) -> String {
    if username.is_empty() {
        format!("{} · {host}", kind.label())
    } else {
        format!("{} · {username}@{host}", kind.label())
    }
}

/// Saved-profile storage seam. The default implementation lives in memory.
pub trait ProfileStore {
    fn profiles(&self) -> Vec<Profile>;
    fn save(&mut self, profile: NewProfile) -> Profile;
    /// Whether saved profiles survive an app restart.
    fn is_persistent(&self) -> bool;
}

#[derive(Default)]
pub struct MemoryProfileStore {
    next_id: u64,
    items: Vec<Profile>,
}

impl MemoryProfileStore {
    pub fn with_profiles(profiles: Vec<Profile>) -> Self {
        Self {
            next_id: profiles
                .iter()
                .map(|profile| profile.id + 1)
                .max()
                .unwrap_or(0),
            items: profiles,
        }
    }
}

impl ProfileStore for MemoryProfileStore {
    fn profiles(&self) -> Vec<Profile> {
        self.items.clone()
    }

    fn save(&mut self, profile: NewProfile) -> Profile {
        let detail = profile_detail(profile.kind, &profile.host, &profile.username);
        let saved = Profile {
            id: self.next_id,
            name: profile.name,
            kind: profile.kind,
            host: profile.host,
            port: profile.port,
            username: profile.username,
            detail,
        };
        self.next_id += 1;
        self.items.push(saved.clone());
        saved
    }

    fn is_persistent(&self) -> bool {
        false
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobile_memory_profile_store_assigns_unique_ids_and_reports_not_persistent() {
        let mut store = MemoryProfileStore::default();
        assert!(store.profiles().is_empty() && !store.is_persistent());
        let new = |name: &str| NewProfile {
            name: name.into(),
            kind: ProfileKind::Ssh,
            host: "h".into(),
            port: 22,
            username: "u".into(),
        };
        let first = store.save(new("a"));
        let second = store.save(new("b"));
        assert_ne!(first.id, second.id);
        assert_eq!(store.profiles().len(), 2);
    }
}
