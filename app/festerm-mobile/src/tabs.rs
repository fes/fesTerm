//! Concurrent sessions. The app keeps one active terminal in its own fields
//! (the input path and tests rely on that); every other tab is parked here and
//! its worker keeps running in the suspension handle's background list.

use super::model::{Reattachable, SurfaceKind, SurfaceState, SurfaceSummary};
use super::profiles::{SessionSpec, TunnelSpec};
use super::*;

pub(super) struct SessionTab {
    #[cfg(feature = "host-clients")]
    pub host: bool,
    pub id: u64,
    pub title: String,
    /// `None` is the offline demo terminal.
    pub spec: Option<SessionSpec>,
    /// Set for Dev Tunnel tabs, which have no SSH spec.
    pub tunnel: Option<TunnelSpec>,
    /// The Dev Tunnel client identity of this tab, kept across reconnects.
    #[cfg(feature = "live-ssh")]
    pub tunnel_client: Option<festerm_mobile_tunnels::ClientId>,
    /// Whether one automatic reconnect may run; re-armed when the tab or app
    /// becomes active again.
    #[cfg(feature = "live-ssh")]
    pub reconnect_armed: bool,
    /// Output arrived while parked; cleared when the tab is shown again.
    pub has_new_output: bool,
    pub(super) parked: Option<(Terminal, TerminalView)>,
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
    pub(super) fn blank_terminal(&self) -> Terminal {
        Terminal::with_scrollback_limit(self.terminal.dimensions(), SCROLLBACK_LIMIT)
            .expect("existing valid bounded grid")
    }

    pub(super) fn fresh_view(&self) -> TerminalView {
        let font_size = self.view.font_size_points();
        let mut view = mobile_terminal_view();
        view.zoom_by_factor(font_size / view.font_size_points());
        view
    }

    pub(super) fn parked_views(&mut self) -> impl Iterator<Item = &mut TerminalView> {
        self.tabs
            .iter_mut()
            .filter_map(|tab| tab.parked.as_mut().map(|(_, view)| view))
    }

    fn release_focus(&mut self) {
        self.gestures.cancel();
        self.terminal_keyboard.cancel();
        self.clear_sticky_modifiers();
        if let Some(focus) = self.view.relinquish_transient_input() {
            route_input(&mut self.terminal, InputEvent::Focus(focus), &mut self.sink);
        }
    }

    /// Moves the active terminal (and its worker) into its tab.
    pub(super) fn park_active(&mut self) {
        self.cancel_dictation();
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
        self.cancel_dictation();
        if self.documents.activate(id) {
            self.park_active();
            self.surface = Surface::Documents;
            return;
        }
        if self.active_tab == Some(id) {
            self.workspace = workspaces::Workspace::Terminal;
            self.surface = Surface::Terminal;
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
        self.tabs[position].has_new_output = false;
        #[cfg(feature = "live-ssh")]
        {
            self.tabs[position].reconnect_armed = true;
        }
        self.workspace = workspaces::Workspace::Terminal;
        self.keys_panel.open = false;
        self.surface = Surface::Terminal;
    }

    pub(super) fn push_tab(&mut self, title: String, spec: Option<SessionSpec>) -> u64 {
        let id = self.next_tab;
        self.next_tab += 1;
        self.tabs.push(SessionTab {
            #[cfg(feature = "host-clients")]
            host: false,
            id,
            title,
            spec,
            tunnel: None,
            #[cfg(feature = "live-ssh")]
            tunnel_client: None,
            #[cfg(feature = "live-ssh")]
            reconnect_armed: false,
            has_new_output: false,
            parked: None,
        });
        id
    }

    pub(super) fn open_local_tab(&mut self) {
        if let Some(tab) = self.tabs.iter().find(|tab| {
            tab.spec.is_none() && tab.tunnel.is_none() && {
                #[cfg(feature = "host-clients")]
                {
                    !tab.host
                }
                #[cfg(not(feature = "host-clients"))]
                {
                    true
                }
            }
        }) {
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
        #[cfg(feature = "host-clients")]
        if self.active_is_host() {
            return false;
        }
        self.active_tab.is_some() && self.active_spec().is_none()
    }

    #[cfg(feature = "host-clients")]
    pub(super) fn active_is_host(&self) -> bool {
        self.tabs
            .iter()
            .any(|tab| Some(tab.id) == self.active_tab && tab.host)
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
        #[cfg(any(target_os = "android", target_os = "ios"))]
        let recording = self.recording_for_new_session();
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
        self.clear_sticky_modifiers();
        #[cfg(any(target_os = "android", target_os = "ios"))]
        self.include_new_session(id, recording);
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
        #[cfg(feature = "host-clients")]
        if self.active_is_host() {
            self.review_host_reconnection();
            return Ok(());
        }
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
        self.clear_sticky_modifiers();
        Ok(())
    }

    pub(super) fn active_tunnel(&self) -> Option<&TunnelSpec> {
        let id = self.active_tab?;
        self.tabs
            .iter()
            .find(|tab| tab.id == id)
            .and_then(|tab| tab.tunnel.as_ref())
    }

    pub(super) fn request_surface_close(&mut self, id: u64) {
        if self.documents.contains(id) {
            self.documents.request_close(id);
        } else {
            self.confirm_close = Some(id);
        }
    }

    pub(super) fn request_tab_menu(&mut self, id: u64) {
        self.cancel_dictation();
        self.clear_sticky_modifiers();
        self.terminal_keyboard.cancel();
        self.tab_menu = Some(id);
    }

    pub(super) fn show_tab_menu(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        let Some(id) = self.tab_menu else { return };
        let Some(surface) = self.surfaces().into_iter().find(|surface| surface.id == id) else {
            self.tab_menu = None;
            return;
        };
        let document = self.documents.contains(id);
        let mut activate = false;
        let mut close = false;
        #[cfg(feature = "host-clients")]
        let mut end_host = false;
        let mut cancel = false;
        let outcome = widgets::bottom_sheet(ctx, "work-tab-menu", bounds, |ui| {
            cancel |= widgets::sheet_title(ui, &surface.title, None);
            ui.label(&surface.status);
            activate =
                widgets::button(ui, widgets::ButtonKind::Secondary, "Activate", true).clicked();
            close = widgets::button(
                ui,
                widgets::ButtonKind::Destructive,
                if document {
                    "Close document view"
                } else {
                    "Close tab..."
                },
                true,
            )
            .clicked();
            #[cfg(feature = "host-clients")]
            if self.tabs.iter().any(|tab| tab.id == id && tab.host) {
                end_host = widgets::button(
                    ui,
                    widgets::ButtonKind::Destructive,
                    "End HOST session…",
                    true,
                )
                .clicked();
            }
            cancel |= widgets::button(ui, widgets::ButtonKind::Secondary, "Cancel", true).clicked();
        });
        #[cfg(feature = "host-clients")]
        if end_host {
            self.tab_menu = None;
            self.apply_shell_actions(ctx, vec![shell::ShellAction::EndHostSurface(id)]);
            return;
        }
        if activate {
            self.tab_menu = None;
            self.activate_tab(id);
        } else if close {
            self.tab_menu = None;
            self.request_surface_close(id);
        } else if cancel || outcome.dismissed {
            self.tab_menu = None;
        }
    }

    pub(super) fn close_tab(&mut self, id: u64) {
        if self.documents.contains(id) {
            self.documents.request_close(id);
            return;
        }
        #[cfg(feature = "host-clients")]
        if !self.retire_host_tab(id) {
            return;
        }
        #[cfg(feature = "live-ssh")]
        if self
            .tunnel
            .connect
            .as_ref()
            .is_some_and(|flow| flow.tab == Some(id))
        {
            self.tunnel.connect = None;
        }
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
            let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == *id) else {
                continue;
            };
            if let Some((terminal, _)) = tab.parked.as_mut() {
                if live.poll(terminal, ctx) {
                    tab.has_new_output = true;
                }
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
                    #[cfg(feature = "host-clients")]
                    None if tab.host => {
                        let (state, status) = self.tab_connection(tab.id, active);
                        (
                            SurfaceKind::DevTunnel,
                            "HOST · protected owner · explicit reconnect".to_owned(),
                            state,
                            status,
                        )
                    }
                    None if tab.tunnel.is_some() => {
                        let (state, status) = self.tab_connection(tab.id, active);
                        let detail = tab
                            .tunnel
                            .as_ref()
                            .map(TunnelSpec::detail)
                            .unwrap_or_default();
                        (SurfaceKind::DevTunnel, detail, state, status)
                    }
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
                    unread: !active && tab.has_new_output,
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
