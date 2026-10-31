//! Concurrent sessions. The app keeps one active terminal in its own fields
//! (the input path and tests rely on that); every other tab is parked here and
//! its worker keeps running in the suspension handle's background list.

use super::model::{Reattachable, SurfaceKind, SurfaceState, SurfaceSummary};
use super::profiles::SessionSpec;
use super::*;

pub(super) struct SessionTab {
    pub id: u64,
    pub title: String,
    /// `None` is the offline demo terminal.
    pub spec: Option<SessionSpec>,
    parked: Option<(Terminal, TerminalView)>,
}

/// A durable remote session offered for reattachment.
pub(super) struct ReattachTarget {
    pub profile: String,
    pub row: Reattachable,
}

#[cfg(feature = "live-ssh")]
const NEW_SHELL_DIVIDER: &[u8] =
    b"\x1b[?1049l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[0m\r\n";

#[cfg(feature = "live-ssh")]
fn surface_state(live: &live_ssh::LiveSsh) -> SurfaceState {
    if live.can_reset() {
        SurfaceState::Disconnected
    } else if live.input_enabled() {
        SurfaceState::Connected
    } else {
        SurfaceState::Connecting
    }
}

impl MobileApp {
    fn blank_terminal(&self) -> Terminal {
        Terminal::with_scrollback_limit(self.terminal.dimensions(), SCROLLBACK_LIMIT)
            .expect("existing valid bounded grid")
    }

    pub(super) fn fresh_view(&self) -> TerminalView {
        let font_size = self.view.font_size_points();
        let mut view = mobile_terminal_view();
        view.zoom_by_factor(font_size / view.font_size_points());
        view
    }

    fn release_focus(&mut self) {
        self.gestures.cancel();
        self.control = false;
        self.alt = false;
        if let Some(focus) = self.view.relinquish_transient_input() {
            route_input(&mut self.terminal, InputEvent::Focus(focus), &mut self.sink);
        }
    }

    /// Moves the active terminal (and its worker) into its tab.
    fn park_active(&mut self) {
        let Some(id) = self.active_tab.take() else {
            return;
        };
        self.release_focus();
        let blank = self.blank_terminal();
        let terminal = std::mem::replace(&mut self.terminal, blank);
        let fresh = self.fresh_view();
        let view = std::mem::replace(&mut self.view, fresh);
        #[cfg(feature = "live-ssh")]
        if let Some(live) = self.sink.live.connection.borrow_mut().take() {
            self.sink.live.background.borrow_mut().push((id, live));
        }
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.parked = Some((terminal, view));
        }
    }

    /// Makes `id` the active terminal without touching its connection.
    pub(super) fn activate_tab(&mut self, id: u64) {
        if self.active_tab == Some(id) {
            self.workspace = workspaces::Workspace::Terminal;
            return;
        }
        let Some(position) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        self.park_active();
        let Some((terminal, view)) = self.tabs[position].parked.take() else {
            return;
        };
        self.terminal = terminal;
        self.view = view;
        #[cfg(feature = "live-ssh")]
        {
            let mut background = self.sink.live.background.borrow_mut();
            if let Some(index) = background.iter().position(|(tab, _)| *tab == id) {
                let (_, live) = background.remove(index);
                *self.sink.live.connection.borrow_mut() = Some(live);
            }
        }
        self.active_tab = Some(id);
        self.workspace = workspaces::Workspace::Terminal;
        self.keys_panel.open = false;
    }

    fn push_tab(&mut self, title: String, spec: Option<SessionSpec>) -> u64 {
        let id = self.next_tab;
        self.next_tab += 1;
        self.tabs.push(SessionTab {
            id,
            title,
            spec,
            parked: None,
        });
        id
    }

    pub(super) fn open_local_tab(&mut self) {
        if let Some(tab) = self.tabs.iter().find(|tab| tab.spec.is_none()) {
            let id = tab.id;
            self.activate_tab(id);
            return;
        }
        self.park_active();
        let id = self.push_tab("Offline demo".to_owned(), None);
        self.terminal = fixture_terminal();
        self.view = self.fresh_view();
        self.active_tab = Some(id);
        self.workspace = workspaces::Workspace::Terminal;
    }

    pub(super) fn active_spec(&self) -> Option<&SessionSpec> {
        let id = self.active_tab?;
        self.tabs
            .iter()
            .find(|tab| tab.id == id)
            .and_then(|tab| tab.spec.as_ref())
    }

    pub(super) fn active_is_local(&self) -> bool {
        self.active_tab.is_some() && self.active_spec().is_none()
    }

    #[cfg(feature = "live-ssh")]
    fn connect_request(&self, spec: &SessionSpec) -> Result<live_ssh::ConnectRequest, String> {
        let profiles = self
            .profiles
            .as_ref()
            .ok_or_else(|| "Saved profiles are unavailable".to_owned())?;
        let identity = festerm_ssh::HostIdentity::new(&spec.ssh.host, spec.ssh.port)
            .map_err(|_| "Invalid SSH host or port".to_owned())?;
        Ok(live_ssh::ConnectRequest {
            host: spec.ssh.host.clone(),
            port: spec.ssh.port,
            username: spec.ssh.username.clone(),
            authentication: profiles.authentication(&spec.ssh.auth),
            strategy: spec.strategy().unwrap_or_default(),
            pin: profiles.pinned_fingerprint(&identity),
        })
    }

    /// Opens a new tab for `spec` and starts connecting it.
    #[cfg(feature = "live-ssh")]
    pub(super) fn open_ssh_tab(
        &mut self,
        spec: SessionSpec,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let request = self.connect_request(&spec)?;
        let live = live_ssh::LiveSsh::connect(request, self.terminal.dimensions(), ctx)
            .map_err(str::to_owned)?;
        if let (Some(profiles), Some(id)) = (self.profiles.as_mut(), spec.profile.as_ref()) {
            profiles.mark_used(id.as_str());
        }
        self.park_active();
        let id = self.push_tab(spec.name.clone(), Some(spec));
        self.terminal = self.blank_terminal();
        self.view = self.fresh_view();
        *self.sink.live.connection.borrow_mut() = Some(live);
        self.active_tab = Some(id);
        self.workspace = workspaces::Workspace::Terminal;
        self.control = false;
        self.alt = false;
        Ok(())
    }

    /// Connects an existing saved profile, switching to its open tab when it
    /// already has a live session.
    #[cfg(feature = "live-ssh")]
    pub(super) fn open_profile(&mut self, id: &str, ctx: &egui::Context) -> Result<(), String> {
        let spec = self
            .profiles
            .as_ref()
            .and_then(|profiles| profiles.spec(id))
            .ok_or_else(|| "That profile is no longer saved".to_owned())?;
        self.open_ssh_tab(spec, ctx)
    }

    /// Starts a fresh worker in the active tab. History stays; the divider
    /// says honestly whether the remote shell was replaced or reattached.
    #[cfg(feature = "live-ssh")]
    pub(super) fn reconnect_active(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let spec = self
            .active_spec()
            .cloned()
            .ok_or_else(|| "This session cannot be reconnected".to_owned())?;
        let request = self.connect_request(&spec)?;
        let live = live_ssh::LiveSsh::connect(request, self.terminal.dimensions(), ctx)
            .map_err(str::to_owned)?;
        self.terminal.ingest(NEW_SHELL_DIVIDER);
        let divider = match spec.persistent_label() {
            Some(label) => format!("\x1b[2m── reconnecting · reattaching {label} ──\x1b[0m\r\n"),
            None => "\x1b[2m── reconnected · new shell ──\x1b[0m\r\n".to_owned(),
        };
        self.terminal.ingest(divider.as_bytes());
        if let (Some(profiles), Some(id)) = (self.profiles.as_mut(), spec.profile.as_ref()) {
            profiles.mark_used(id.as_str());
        }
        *self.sink.live.connection.borrow_mut() = Some(live);
        self.control = false;
        self.alt = false;
        Ok(())
    }

    pub(super) fn close_tab(&mut self, id: u64) {
        if self.active_tab == Some(id) {
            self.release_focus();
            #[cfg(feature = "live-ssh")]
            {
                *self.sink.live.connection.borrow_mut() = None;
            }
            self.terminal = self.blank_terminal();
            self.view = self.fresh_view();
            self.active_tab = None;
            self.surface = Surface::Tabs;
        } else {
            #[cfg(feature = "live-ssh")]
            self.sink
                .live
                .background
                .borrow_mut()
                .retain(|(tab, _)| *tab != id);
        }
        self.tabs.retain(|tab| tab.id != id);
    }

    /// Pumps parked sessions so background tabs keep receiving output.
    #[cfg(feature = "live-ssh")]
    pub(super) fn poll_background(&mut self, ctx: &egui::Context) {
        let mut background = self.sink.live.background.borrow_mut();
        for (id, live) in background.iter_mut() {
            let parked = self
                .tabs
                .iter_mut()
                .find(|tab| tab.id == *id)
                .and_then(|tab| tab.parked.as_mut());
            if let Some((terminal, _)) = parked {
                live.poll(terminal, ctx);
            }
        }
    }

    pub(super) fn tab_surfaces(&self) -> Vec<SurfaceSummary> {
        let on_terminal = self.surface == Surface::Terminal;
        self.tabs
            .iter()
            .map(|tab| {
                let active = self.active_tab == Some(tab.id);
                let (kind, detail, state, status) = match &tab.spec {
                    None => (
                        SurfaceKind::Local,
                        "Local fixture · no shell".to_owned(),
                        SurfaceState::Disconnected,
                        "Input is discarded".to_owned(),
                    ),
                    Some(spec) => {
                        let (state, status) = self.tab_connection(tab.id, active);
                        let mut detail = spec.detail();
                        if let Some(label) = spec.persistent_label() {
                            detail = format!("{detail} · {label}");
                        }
                        (SurfaceKind::Ssh, detail, state, status)
                    }
                };
                SurfaceSummary {
                    id: tab.id,
                    title: tab.title.clone(),
                    kind,
                    detail,
                    state,
                    status,
                    current: active && on_terminal,
                }
            })
            .collect()
    }

    #[cfg(feature = "live-ssh")]
    fn tab_connection(&self, id: u64, active: bool) -> (SurfaceState, String) {
        if active {
            if let Some(live) = self.sink.live.connection.borrow().as_ref() {
                return (surface_state(live), live.status().to_owned());
            }
        } else if let Some((_, live)) = self
            .sink
            .live
            .background
            .borrow()
            .iter()
            .find(|(tab, _)| *tab == id)
        {
            return (surface_state(live), live.status().to_owned());
        }
        (SurfaceState::Disconnected, "Not connected".to_owned())
    }

    #[cfg(not(feature = "live-ssh"))]
    fn tab_connection(&self, _id: u64, _active: bool) -> (SurfaceState, String) {
        (SurfaceState::Disconnected, "Not connected".to_owned())
    }

    /// Saved tmux/Screen profiles with no open tab: the remote session may
    /// still be running, so they are offered for reattachment.
    pub(super) fn reattach_targets(&self) -> Vec<ReattachTarget> {
        let Some(profiles) = &self.profiles else {
            return Vec::new();
        };
        profiles
            .rows(self.caps)
            .into_iter()
            .filter(|row| row.kind == model::ProfileKind::Ssh)
            .filter_map(|row| {
                let spec = profiles.spec(&row.id)?;
                let label = spec.persistent_label()?;
                let open = self.tabs.iter().any(|tab| {
                    tab.spec
                        .as_ref()
                        .and_then(|open| open.profile.as_ref())
                        .is_some_and(|open| Some(open) == spec.profile.as_ref())
                });
                (!open).then(|| ReattachTarget {
                    profile: row.id,
                    row: Reattachable {
                        title: row.name,
                        detail: label,
                        host: spec.ssh.host.clone(),
                    },
                })
            })
            .collect()
    }
}

#[cfg(all(test, feature = "live-ssh"))]
impl MobileApp {
    /// Test and gallery helper: an active tab for `spec` without a worker.
    pub(super) fn fixture_ssh_tab(&mut self, spec: SessionSpec) {
        self.park_active();
        let id = self.push_tab(spec.name.clone(), Some(spec));
        self.active_tab = Some(id);
    }
}
