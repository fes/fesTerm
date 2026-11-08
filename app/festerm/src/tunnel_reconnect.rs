//! Dev Tunnel tabs reconnect in place, and a lost one reconnects by itself once
//! per activation: when the tab becomes active, when the window regains focus,
//! or when the loss is noticed while the tab is already in front. The automatic
//! attempt never takes over another client; it may only reclaim this tab's own
//! stale attachment. A displaced, exited or failed session never reconnects by
//! itself.

use super::*;
use festerm_config::Profile;
use festerm_tunnel_bridge::protocol::ClientId;

/// Where a Dev Tunnel tab's connection came from, so it can be made again.
#[derive(Clone)]
pub(crate) enum TunnelOrigin {
    /// A saved profile, looked up again (with its current settings) on every
    /// reconnect.
    Profile(String),
    /// A connect-once tunnel that was never saved.
    Ephemeral(Box<Profile>),
}

pub(crate) struct TunnelTab {
    pub(crate) origin: TunnelOrigin,
    /// Identifies this tab, and only this tab, to a host that advertises
    /// reclaim; every reconnect of the tab presents the same id.
    pub(crate) client: ClientId,
    /// Spent by an automatic reconnect; restored by the next activation event.
    armed: bool,
}

impl TunnelTab {
    pub(crate) fn new(origin: TunnelOrigin) -> Self {
        Self {
            origin,
            client: ClientId::new(),
            armed: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TunnelReconnectRequest {
    pub(crate) tab: TabId,
    pub(crate) automatic: bool,
}

#[derive(Default)]
pub(super) struct TunnelReconnects {
    requests: Vec<TunnelReconnectRequest>,
    pending: Vec<PendingTunnelReconnect>,
    seen_active: Option<TabId>,
    seen_focused: bool,
}

struct PendingTunnelReconnect {
    target: TabId,
    receiver: Receiver<Result<TunnelReplacement, String>>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for PendingTunnelReconnect {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct TunnelReplacement {
    session: ApplicationSession,
    files: Option<crate::remote_files::RemoteFileCapability>,
    transport: InspectorTransport,
}

/// Starts one tunnel connection; the session reports its own outcome.
pub(super) struct StartedTunnel {
    pub(super) result: std::io::Result<ApplicationSession>,
    pub(super) files: Option<crate::remote_files::RemoteFileCapability>,
    pub(super) transport: InspectorTransport,
    pub(super) name: String,
}

pub(super) fn start_tunnel(
    launch: crate::tunnel_activation::Launch,
    context: &egui::Context,
) -> StartedTunnel {
    let host = launch.destination.tunnel_id.clone();
    let port = launch.destination.port;
    let name = launch.destination.target.name.clone();
    let account = launch
        .account
        .clone()
        .map(|account| (account.id, account.login));
    let mut files = None;
    let result = launch.start(make_notifier(context)).map(|remote| {
        files = remote
            .files
            .map(crate::remote_files::RemoteFileCapability::DevTunnel);
        ApplicationSession::DevTunnel(remote.session)
    });
    StartedTunnel {
        result,
        files,
        transport: InspectorTransport::DevTunnel {
            host,
            port,
            account,
        },
        name,
    }
}

/// A connection that dropped by itself, as opposed to one another client took
/// over, the remote shell ended, or that never started.
fn tunnel_lost(lifecycle: Option<SessionLifecycle>) -> bool {
    matches!(lifecycle, Some(SessionLifecycle::Disconnected(error))
        if error.message() != festerm_sessiond::DISPLACED_REASON)
}

impl SessionTab {
    pub(crate) fn tunnel(&self) -> Option<&TunnelTab> {
        self.tunnel.as_ref()
    }

    fn replace_tunnel_session(
        &mut self,
        replacement: TunnelReplacement,
        scrollback_limit: ScrollbackLimitPreference,
    ) {
        let fresh = Self::from_session_result(
            Ok::<_, std::io::Error>(replacement.session),
            Dimensions::new(80, 24).expect("default dimensions"),
            SessionResultMeta {
                label: &self.default_label,
                launch_secondary: None,
                profile_identifier: None,
                session_name: "Dev Tunnel",
                inspector_transport: replacement.transport,
                ssh_password_retry: None,
            },
        );
        self.terminal = fresh.terminal;
        self.controller = fresh.controller;
        self.view = fresh.view;
        self.inspector_transport = fresh.inspector_transport;
        self.remote_files = replacement.files;
        self.apply_frontend_terminal_configuration(scrollback_limit);
        self.search.terminal_replaced(&self.terminal);
        self.view.request_focus_on_next_frame();
    }
}

impl AppState {
    pub(crate) fn tunnel_tab(&self, tab: TabId) -> Option<&TunnelTab> {
        self.session_tab(tab).and_then(SessionTab::tunnel)
    }

    fn tunnel_auto_reconnect_enabled(&self, origin: &TunnelOrigin) -> bool {
        match origin {
            TunnelOrigin::Profile(id) => self.configuration.profile(id),
            TunnelOrigin::Ephemeral(profile) => Some(&**profile),
        }
        .and_then(Profile::as_dev_tunnel)
        .is_some_and(festerm_config::DevTunnelProfileConfiguration::auto_reconnect)
    }

    fn tunnel_reconnect_in_flight(&self, tab: TabId) -> bool {
        let state = &self.tunnel_reconnects;
        state.requests.iter().any(|request| request.tab == tab)
            || state.pending.iter().any(|pending| pending.target == tab)
    }

    /// Queues the user's Reconnect for a Dev Tunnel tab. The application owns
    /// the connection workflow, so it drains the queue.
    pub(super) fn queue_tunnel_reconnect(&mut self, tab: TabId) {
        if !self.tunnel_reconnect_in_flight(tab) {
            self.tunnel_reconnects
                .requests
                .push(TunnelReconnectRequest {
                    tab,
                    automatic: false,
                });
        }
    }

    pub(crate) fn take_tunnel_reconnect_requests(&mut self) -> Vec<TunnelReconnectRequest> {
        std::mem::take(&mut self.tunnel_reconnects.requests)
    }

    /// Completes in-place reconnects, re-arms tabs on activation events, and
    /// queues at most one automatic reconnect for the active tab.
    pub(crate) fn poll_tunnel_reconnects(&mut self, focused: bool, context: &egui::Context) {
        self.finish_tunnel_reconnects();
        if !self.tunnel_reconnects.pending.is_empty() {
            context.request_repaint_after(std::time::Duration::from_millis(25));
        }
        let active = self.active;
        let activated = self.tunnel_reconnects.seen_active != Some(active);
        let woke = focused && !self.tunnel_reconnects.seen_focused;
        self.tunnel_reconnects.seen_active = Some(active);
        self.tunnel_reconnects.seen_focused = focused;
        for tab in &mut self.tabs {
            let TabContent::Session(session) = &mut tab.content else {
                continue;
            };
            let running = matches!(
                session.controller.lifecycle(),
                Some(SessionLifecycle::Running)
            );
            if let Some(tunnel) = session.tunnel.as_mut() {
                // A connection that came up may reconnect once when it next
                // drops, so a loss noticed while the tab is in front counts.
                if running || (tab.id == active && (activated || woke)) {
                    tunnel.armed = true;
                }
            }
        }
        if !focused || self.tunnel_reconnect_in_flight(active) {
            return;
        }
        let Some(session) = self.session_tab(active) else {
            return;
        };
        let Some(tunnel) = session.tunnel.as_ref() else {
            return;
        };
        if !tunnel.armed
            || !tunnel_lost(session.controller.lifecycle())
            || !self.tunnel_auto_reconnect_enabled(&tunnel.origin)
        {
            return;
        }
        if let Some(tunnel) = self
            .session_tab_mut(active)
            .and_then(|session| session.tunnel.as_mut())
        {
            tunnel.armed = false;
        }
        self.tunnel_reconnects
            .requests
            .push(TunnelReconnectRequest {
                tab: active,
                automatic: true,
            });
        context.request_repaint();
    }

    /// Connects `launch` and, once it starts, swaps it into tab `target` in
    /// place, keeping the tab's identity and its client id.
    pub(crate) fn start_tunnel_reconnect(
        &mut self,
        target: TabId,
        load: impl FnOnce() -> std::io::Result<crate::tunnel_activation::Launch> + Send + 'static,
        context: &egui::Context,
    ) {
        if self
            .tunnel_reconnects
            .pending
            .iter()
            .any(|pending| pending.target == target)
            || self.tunnel_tab(target).is_none()
        {
            return;
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let context = context.clone();
        let handle = std::thread::Builder::new()
            .name("dev-tunnel-reconnect".into())
            .spawn(move || {
                let result = load()
                    .map_err(|error| error.to_string())
                    .and_then(|launch| {
                        let started = start_tunnel(launch, &context);
                        started
                            .result
                            .map(|session| TunnelReplacement {
                                session,
                                files: started.files,
                                transport: started.transport,
                            })
                            .map_err(|error| error.to_string())
                    });
                let _ = sender.send(result);
                context.request_repaint();
            });
        match handle {
            Ok(handle) => self.tunnel_reconnects.pending.push(PendingTunnelReconnect {
                target,
                receiver,
                handle: Some(handle),
            }),
            Err(error) => tracing::warn!(%error, "could not start Dev Tunnel reconnect"),
        }
    }

    fn finish_tunnel_reconnects(&mut self) {
        let (finished, pending): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.tunnel_reconnects.pending)
                .into_iter()
                .partition(|pending| {
                    pending
                        .handle
                        .as_ref()
                        .is_none_or(|handle| handle.is_finished())
                });
        self.tunnel_reconnects.pending = pending;
        let scrollback_limit = self.scrollback_limit;
        for mut done in finished {
            if let Some(handle) = done.handle.take() {
                let _ = handle.join();
            }
            let result = done
                .receiver
                .try_recv()
                .unwrap_or_else(|_| Err("Dev Tunnel reconnect worker failed".into()));
            match result {
                Ok(replacement) => {
                    if let Some(session) = self
                        .session_tab_mut(done.target)
                        .filter(|session| session.tunnel.is_some())
                    {
                        session.replace_tunnel_session(replacement, scrollback_limit);
                    }
                }
                Err(error) => tracing::warn!(%error, "Dev Tunnel reconnect failed"),
            }
        }
    }
}

#[cfg(test)]
impl AppState {
    /// Places a stand-in Dev Tunnel tab, connected, in front.
    pub(crate) fn place_tunnel_tab_for_test(&mut self, origin: TunnelOrigin) -> TabId {
        let launcher = self.active();
        let fake = crate::session_controller::fake::FakeSshSession::new([]);
        let mut tab = SessionTab::for_test_ssh(fake, "deploy", "tunnel.example.test", 22);
        tab.tunnel = Some(TunnelTab::new(origin));
        self.place_session(tab);
        assert_eq!(self.active(), launcher, "the session replaces the launcher");
        launcher
    }

    pub(crate) fn drop_tunnel_for_test(&mut self, tab: TabId, reason: &str) {
        self.session_tab_mut(tab)
            .unwrap()
            .controller
            .set_lifecycle_for_test(SessionLifecycle::Disconnected(
                festerm_session::SessionError::new(
                    festerm_session::SessionErrorKind::Output,
                    reason,
                ),
            ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_tunnel_tab(profile: Profile) -> (AppState, TabId) {
        let mut state = AppState::with_launcher(Configuration::new(vec![profile]).unwrap());
        let tab = state.place_tunnel_tab_for_test(TunnelOrigin::Profile("build".into()));
        (state, tab)
    }

    fn drop_connection(state: &mut AppState, tab: TabId, reason: &str) {
        state.drop_tunnel_for_test(tab, reason);
    }

    fn automatic_requests(state: &mut AppState, focused: bool) -> Vec<TunnelReconnectRequest> {
        state.poll_tunnel_reconnects(focused, &egui::Context::default());
        state.take_tunnel_reconnect_requests()
    }

    #[test]
    fn a_lost_tunnel_tab_reconnects_once_per_activation_while_in_front() {
        let profile = Profile::named_dev_tunnel("build", "build.usw3").unwrap();
        let (mut state, tab) = state_with_tunnel_tab(profile);
        let client = state.tunnel_tab(tab).unwrap().client.clone();
        // Connected while in front, then the connection drops (sleep, lock).
        assert!(automatic_requests(&mut state, true).is_empty());
        drop_connection(&mut state, tab, "transport closed");
        let once = vec![TunnelReconnectRequest {
            tab,
            automatic: true,
        }];
        assert_eq!(automatic_requests(&mut state, true), once);
        // A failed attempt leaves the tab lost without retrying in a loop.
        for _ in 0..3 {
            assert!(automatic_requests(&mut state, true).is_empty());
        }
        // Losing and regaining window focus is a new activation event.
        assert!(automatic_requests(&mut state, false).is_empty());
        assert_eq!(automatic_requests(&mut state, true), once);
        // So is switching to another tab and back.
        state.dispatch(AppCommand::OpenLauncher, &egui::Context::default());
        assert!(automatic_requests(&mut state, true).is_empty());
        state.set_active(tab);
        assert_eq!(automatic_requests(&mut state, true), once);
        // Every attempt, automatic or not, presents the same client id.
        assert_eq!(state.tunnel_tab(tab).unwrap().client, client);
        assert!(state.session_tab(tab).unwrap().reconnect_available());
    }

    #[test]
    fn displaced_or_opted_out_tunnel_tabs_never_reconnect_by_themselves() {
        let profile = Profile::named_dev_tunnel("build", "build.usw3").unwrap();
        let (mut state, tab) = state_with_tunnel_tab(profile.clone());
        drop_connection(&mut state, tab, festerm_sessiond::DISPLACED_REASON);
        assert!(automatic_requests(&mut state, true).is_empty());
        // The manual Reconnect stays available and is never automatic.
        state.dispatch(AppCommand::ReconnectSession(tab), &egui::Context::default());
        assert_eq!(
            state.take_tunnel_reconnect_requests(),
            vec![TunnelReconnectRequest {
                tab,
                automatic: false
            }]
        );

        let (mut state, tab) =
            state_with_tunnel_tab(profile.with_tunnel_auto_reconnect(false).unwrap());
        assert!(automatic_requests(&mut state, true).is_empty());
        drop_connection(&mut state, tab, "transport closed");
        assert!(automatic_requests(&mut state, true).is_empty());
        assert!(automatic_requests(&mut state, false).is_empty());
        assert!(automatic_requests(&mut state, true).is_empty());
    }
}
