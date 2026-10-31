//! Glue between the shell (tabs, editor, sheets) and the Dev Tunnel backend.

use std::sync::Arc;

use super::model::{HostDecision, HostVerification};
use super::profiles::TunnelSpec;
use super::tunnel_link::{
    self, describe_error, holder_label, ConnectFlow, ConnectStage, Job, Poll, Resume, SignInAction,
    SignInFlow, SignInView, TunnelList,
};
use super::*;
use festerm_mobile_tunnels::{Attachment, HostTrust, SignInState, TunnelClient, TunnelError};
use std::time::Duration;

const NOT_CONFIGURED: &str = "Dev Tunnel sign-in is not available in this build.";

impl MobileApp {
    /// Injects the tunnel backend (tests and previews).
    #[cfg(test)]
    pub(super) fn with_tunnel_client(mut self, client: Arc<TunnelClient>) -> Self {
        self.tunnel.client = Some(client);
        self
    }

    fn tunnel_client(&mut self) -> Option<Arc<TunnelClient>> {
        if self.tunnel.client.is_none() {
            let store = self.profiles.as_ref()?.store_handle();
            self.tunnel.client = TunnelClient::new(store).ok().map(Arc::new);
        }
        self.tunnel.client.clone()
    }

    /// Signed-in GitHub accounts as `(id, login)`.
    pub(super) fn tunnel_accounts(&mut self) -> Vec<(String, String)> {
        self.tunnel_client()
            .map(|client| {
                client
                    .accounts()
                    .into_iter()
                    .map(|account| (account.id, account.login))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn sign_out_account(&mut self, id: &str) {
        if let Some(client) = self.tunnel_client() {
            if let Err(error) = client.sign_out(id) {
                self.notice = Some(describe_error(&error));
            }
        }
        if self
            .tunnel
            .list
            .as_ref()
            .is_some_and(|list| list.account == id)
        {
            self.tunnel.list = None;
        }
    }

    pub(super) fn start_sign_in(
        &mut self,
        ctx: &egui::Context,
        expected: Option<String>,
        resume: Resume,
    ) {
        let Some(client) = self.tunnel_client() else {
            self.notice = Some(NOT_CONFIGURED.to_owned());
            return;
        };
        let known = expected.filter(|id| client.accounts().iter().any(|account| account.id == *id));
        let start_for = known.clone();
        let start = Job::spawn(ctx, move |cancel| {
            client.begin_sign_in(start_for.as_deref(), cancel)
        });
        self.tunnel.sign_in = Some(SignInFlow {
            start: Some(start),
            active: None,
            error: None,
            resume,
        });
    }

    pub(super) fn refresh_tunnel_list(&mut self, ctx: &egui::Context, account: String) {
        let Some(client) = self.tunnel_client() else {
            return;
        };
        let target = account.clone();
        let job = Job::spawn(ctx, move |cancel| client.list_tunnels(&target, cancel));
        self.tunnel.list = Some(TunnelList {
            account,
            job: Some(job),
            items: Vec::new(),
            error: None,
        });
    }

    /// Connects (or re-attaches `tab`) to a saved Dev Tunnel profile.
    pub(super) fn start_tunnel_connect(&mut self, ctx: &egui::Context, id: &str, tab: Option<u64>) {
        self.notice = None;
        self.tunnel.attach_error = None;
        let Some(spec) = self
            .profiles
            .as_ref()
            .and_then(|profiles| profiles.tunnel_spec(id))
        else {
            self.notice = Some("That profile is no longer saved".to_owned());
            return;
        };
        let Some(client) = self.tunnel_client() else {
            self.notice = Some(NOT_CONFIGURED.to_owned());
            return;
        };
        let signed_in = spec
            .account
            .as_ref()
            .filter(|account| client.accounts().iter().any(|known| known.id == **account))
            .cloned();
        let Some(account) = signed_in else {
            self.start_sign_in(
                ctx,
                spec.account.clone(),
                Resume::Connect {
                    profile: id.to_owned(),
                    tab,
                },
            );
            return;
        };
        let trusted = client.trusted_host_for(&spec.profile);
        let tunnel = spec.tunnel.clone();
        let job = Job::spawn(ctx, move |cancel| {
            client.describe_tunnel(&account, &tunnel, trusted.as_ref(), cancel)
        });
        self.tunnel.connect = Some(ConnectFlow {
            spec,
            tab,
            stage: ConnectStage::Describing(job),
        });
    }

    fn begin_attach(&mut self, ctx: &egui::Context, spec: TunnelSpec, tab: Option<u64>) {
        let Some(client) = self.tunnel_client() else {
            return;
        };
        let notifier = Arc::new(live_ssh::Repaint(ctx.clone()));
        let profile = spec.profile.clone();
        let job = Job::spawn(ctx, move |cancel| client.attach(&profile, notifier, cancel));
        self.tunnel.connect = Some(ConnectFlow {
            spec,
            tab,
            stage: ConnectStage::Attaching(job),
        });
    }

    fn fail_tunnel_connect(&mut self, ctx: &egui::Context, flow: ConnectFlow, error: TunnelError) {
        if error.needs_sign_in() {
            self.start_sign_in(
                ctx,
                flow.spec.account.clone(),
                Resume::Connect {
                    profile: flow.spec.profile.as_str().to_owned(),
                    tab: flow.tab,
                },
            );
            return;
        }
        let message = match &error {
            TunnelError::HostChanged { .. } => {
                "The host identity changed, so fesTerm refused to connect. If you trust the new \
                 host, create the profile again to verify it."
                    .to_owned()
            }
            other => describe_error(other),
        };
        if flow.tab.is_some() {
            self.tunnel.attach_error = Some(message);
        } else {
            self.notice = Some(message);
        }
    }

    fn install_attachment(
        &mut self,
        ctx: &egui::Context,
        spec: TunnelSpec,
        tab: Option<u64>,
        attachment: Attachment,
    ) {
        let recovered = attachment.take_recovered_terminal();
        let live = live_ssh::LiveSsh::from_tunnel(attachment, ctx);
        let terminal = recovered.unwrap_or_else(|| self.blank_terminal());
        match tab {
            Some(id) if self.tabs.iter().any(|tab| tab.id == id) => {
                self.activate_tab(id);
            }
            _ => {
                self.park_active();
                let id = self.push_tab(spec.name.clone(), None);
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                    tab.tunnel = Some(spec);
                }
                self.active_tab = Some(id);
            }
        }
        self.terminal = terminal;
        self.view = self.fresh_view();
        *self.sink.live.connection.borrow_mut() = Some(live);
        self.workspace = workspaces::Workspace::Terminal;
        self.surface = Surface::Terminal;
        self.control = false;
        self.alt = false;
    }

    /// Advances sign-in, tunnel listing and connect flows once per frame.
    pub(super) fn poll_tunnels(&mut self, ctx: &egui::Context) {
        self.poll_sign_in(ctx);
        if let Some(list) = self.tunnel.list.as_mut() {
            if let Some(job) = &list.job {
                match job.poll() {
                    Poll::Pending => {}
                    Poll::Done(Ok(items)) => {
                        list.items = items;
                        list.job = None;
                    }
                    Poll::Done(Err(error)) => {
                        list.error = Some(describe_error(&error));
                        list.job = None;
                    }
                    Poll::Lost => list.job = None,
                }
            }
        }
        let Some(flow) = self.tunnel.connect.take() else {
            return;
        };
        match flow.stage {
            ConnectStage::Describing(ref job) => match job.poll() {
                Poll::Pending => self.tunnel.connect = Some(flow),
                Poll::Lost => {}
                Poll::Done(Err(error)) => self.fail_tunnel_connect(ctx, flow, error),
                Poll::Done(Ok(description)) => {
                    let ConnectFlow { spec, tab, .. } = flow;
                    if description.trust == HostTrust::Trusted {
                        self.begin_attach(ctx, spec, tab);
                    } else {
                        self.tunnel.connect = Some(ConnectFlow {
                            spec,
                            tab,
                            stage: ConnectStage::Verify(description),
                        });
                    }
                }
            },
            ConnectStage::Verify(_) => self.tunnel.connect = Some(flow),
            ConnectStage::Attaching(ref job) => match job.poll() {
                Poll::Pending => self.tunnel.connect = Some(flow),
                Poll::Lost => {}
                Poll::Done(Err(error)) => self.fail_tunnel_connect(ctx, flow, error),
                Poll::Done(Ok(attachment)) => {
                    let ConnectFlow { spec, tab, .. } = flow;
                    self.install_attachment(ctx, spec, tab, attachment);
                }
            },
        }
    }

    fn poll_sign_in(&mut self, ctx: &egui::Context) {
        let Some(mut flow) = self.tunnel.sign_in.take() else {
            return;
        };
        if let Some(job) = &flow.start {
            match job.poll() {
                Poll::Pending => {}
                Poll::Done(Ok(sign_in)) => {
                    flow.active = Some(sign_in);
                    flow.start = None;
                }
                Poll::Done(Err(error)) => {
                    flow.error = Some(describe_error(&error));
                    flow.start = None;
                }
                Poll::Lost => flow.start = None,
            }
        }
        if let Some(sign_in) = &flow.active {
            match sign_in.state() {
                SignInState::Waiting => ctx.request_repaint_after(Duration::from_millis(500)),
                SignInState::Failed(error) => {
                    flow.error = Some(describe_error(&error));
                    flow.active = None;
                }
                SignInState::Done(account) => {
                    self.tunnel.attach_error = None;
                    match flow.resume {
                        Resume::Connect { profile, tab } => {
                            self.start_tunnel_connect(ctx, &profile, tab);
                        }
                        Resume::Nothing => {
                            self.editor.tunnel_account = Some(account.id.clone());
                            self.refresh_tunnel_list(ctx, account.id);
                        }
                    }
                    return;
                }
            }
        }
        self.tunnel.sign_in = Some(flow);
    }

    pub(super) fn show_tunnel_sheets(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        if let Some(flow) = &self.tunnel.sign_in {
            let view = SignInView {
                prompt: flow.active.as_ref().map(|sign_in| sign_in.prompt()),
                error: flow.error.as_deref(),
            };
            match tunnel_link::sign_in_sheet(ctx, bounds, &view) {
                SignInAction::None => {}
                SignInAction::CopyCode => {
                    if let Some(prompt) = view.prompt {
                        festerm_ios_window::set_clipboard_text(&prompt.user_code);
                    }
                }
                SignInAction::OpenBrowser => {
                    if let Some(prompt) = view.prompt {
                        festerm_ios_window::open_url(&prompt.verification_url);
                    }
                }
                SignInAction::Cancel => self.tunnel.sign_in = None,
            }
            return;
        }
        let Some(flow) = &self.tunnel.connect else {
            return;
        };
        match &flow.stage {
            ConnectStage::Describing(_) | ConnectStage::Attaching(_) => {
                let attaching = matches!(flow.stage, ConnectStage::Attaching(_));
                let (title, detail) = if attaching {
                    ("Attaching\u{2026}", "Adopting the host session.")
                } else {
                    (
                        "Checking host\u{2026}",
                        "Verifying the tunnel and its host.",
                    )
                };
                if tunnel_link::progress_sheet(ctx, bounds, title, detail) {
                    self.tunnel.connect = None;
                }
            }
            ConnectStage::Verify(description) => {
                let changed_from = match &description.trust {
                    HostTrust::Changed {
                        approved_fingerprint,
                    } => Some(approved_fingerprint.clone()),
                    _ => None,
                };
                let model = HostVerification {
                    name: flow.spec.name.clone(),
                    target: format!("Dev Tunnel \u{b7} {}", description.locator),
                    algorithm: "SHA-256".to_owned(),
                    fingerprint: description.fingerprint.clone(),
                    changed_from,
                };
                let Some(decision) = forms::host_verification_sheet(ctx, bounds, &model) else {
                    return;
                };
                let Some(ConnectFlow {
                    spec,
                    tab,
                    stage: ConnectStage::Verify(description),
                }) = self.tunnel.connect.take()
                else {
                    return;
                };
                if decision != HostDecision::Trust || model.changed_from.is_some() {
                    return;
                }
                let Some(client) = self.tunnel_client() else {
                    return;
                };
                match client.approve_host(&spec.profile, description.host) {
                    Ok(()) => self.begin_attach(ctx, spec, tab),
                    Err(error) => self.notice = Some(describe_error(&error)),
                }
            }
        }
    }

    /// Re-attach copy for the banner of a Dev Tunnel tab.
    pub(super) fn tunnel_banner_text(&self, status: &str) -> model::BannerText {
        let taken_over = status.starts_with("Taken over");
        model::BannerText {
            title: if taken_over {
                "Session taken over".to_owned()
            } else {
                "Connection interrupted".to_owned()
            },
            body: if taken_over {
                format!("{status}\nfesTerm does not reattach on its own.")
            } else {
                "The host session may still be running.\nAttaching again shows its current screen."
                    .to_owned()
            },
            action: "Attach again",
        }
    }

    #[allow(dead_code)]
    pub(super) fn holder_for_tests(by: Option<festerm_mobile_tunnels::Displacer>) -> &'static str {
        holder_label(by)
    }
}

impl MobileApp {
    /// Fills the editor's account and tunnel choices from the live backend.
    pub(super) fn sync_editor_tunnel(&mut self, ctx: &egui::Context) {
        if !self.editor.open || self.editor.kind(self.caps) != model::ProfileKind::DevTunnel {
            return;
        }
        let accounts = self.tunnel_accounts();
        let bound = self
            .editor
            .tunnel_account
            .clone()
            .filter(|id| accounts.iter().any(|(known, _)| known == id))
            .or_else(|| accounts.first().map(|(id, _)| id.clone()));
        if bound != self.editor.tunnel_account {
            self.editor.tunnel_account = bound.clone();
        }
        let listed = self.tunnel.list.as_ref().map(|list| list.account.clone());
        if let Some(account) = &bound {
            if listed.as_ref() != Some(account) {
                self.refresh_tunnel_list(ctx, account.clone());
            }
        }
        let login = bound.as_ref().and_then(|id| {
            accounts
                .iter()
                .find(|(known, _)| known == id)
                .map(|(_, login)| login.clone())
        });
        let list = self.tunnel.list.as_ref();
        self.editor.tunnel = Some(model::DevTunnelChoices {
            account_login: login,
            tunnels: list
                .map(|list| list.items.iter().map(tunnel_label).collect())
                .unwrap_or_default(),
            loading: list.is_some_and(|list| list.job.is_some()),
            error: list.and_then(|list| list.error.clone()),
        });
    }

    pub(super) fn apply_tunnel_editor_action(
        &mut self,
        ctx: &egui::Context,
        action: forms::EditorAction,
    ) {
        match action {
            forms::EditorAction::SignIn => {
                self.start_sign_in(ctx, None, Resume::Nothing);
            }
            forms::EditorAction::SignOut => {
                if let Some(account) = self.editor.tunnel_account.take() {
                    self.sign_out_account(&account);
                }
            }
            forms::EditorAction::RefreshTunnels => {
                if let Some(account) = self.editor.tunnel_account.clone() {
                    self.refresh_tunnel_list(ctx, account);
                }
            }
            forms::EditorAction::PickTunnel(index) => {
                if let Some(tunnel) = self
                    .tunnel
                    .list
                    .as_ref()
                    .and_then(|list| list.items.get(index))
                {
                    self.editor.tunnel_name = tunnel_label(tunnel);
                }
            }
            _ => {}
        }
    }

    pub(super) fn save_tunnel_editor(&mut self) -> Option<String> {
        let tunnel = self.editor.tunnel_name.trim().to_owned();
        if tunnel.is_empty() {
            self.editor.error = Some("Choose or enter a tunnel name".to_owned());
            return None;
        }
        let Some(account) = self.editor.tunnel_account.clone() else {
            self.editor.error = Some("Sign in with GitHub first".to_owned());
            return None;
        };
        let session = self.editor.session_name.trim().to_owned();
        let name = self.editor.name.trim().to_owned();
        let profiles = self.profiles.as_mut()?;
        let name = if name.is_empty() {
            profiles.suggest_name(&tunnel)
        } else {
            name
        };
        match profiles.save_tunnel(
            self.editor.editing.as_deref(),
            &name,
            Some(account),
            &tunnel,
            (!session.is_empty()).then_some(session),
            self.editor.take_over,
        ) {
            Ok(id) => {
                self.editor.error = None;
                Some(id)
            }
            Err(error) => {
                self.editor.error = Some(error);
                None
            }
        }
    }

    /// Opens the Dev Tunnel profile, reusing a tab that is still attached.
    pub(super) fn connect_tunnel_profile(&mut self, ctx: &egui::Context, id: &str) {
        self.notice = None;
        let open = self.tabs.iter().find(|tab| {
            tab.tunnel
                .as_ref()
                .is_some_and(|spec| spec.profile.as_str() == id)
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
        let stale = self
            .tabs
            .iter()
            .find(|tab| {
                tab.tunnel
                    .as_ref()
                    .is_some_and(|spec| spec.profile.as_str() == id)
            })
            .map(|tab| tab.id);
        self.start_tunnel_connect(ctx, id, stale);
    }
}

/// The name a profile stores for a listed tunnel.
fn tunnel_label(tunnel: &festerm_mobile_tunnels::TunnelInfo) -> String {
    tunnel
        .name
        .clone()
        .unwrap_or_else(|| tunnel.locator.clone())
}
