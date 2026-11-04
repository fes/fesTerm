//! Glue between the shell (tabs, editor, sheets) and the Dev Tunnel backend.

use std::sync::Arc;

use super::model::{HostDecision, HostVerification};
use super::profiles::TunnelSpec;
use super::tunnel_link::{
    self, describe_error, holder_label, ConnectFlow, ConnectStage, Job, Poll, Resume, SignInAction,
    SignInFlow, SignInView, TunnelList,
};
use super::*;
use festerm_mobile_tunnels::{
    AttachMode, Attachment, ClientId, HostTrust, SignInState, TunnelClient, TunnelError,
};
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
            started: None,
            active: None,
            error: None,
            resume,
        });
    }

    pub(super) fn refresh_tunnel_list(&mut self, ctx: &egui::Context, account: String) {
        let Some(client) = self.tunnel_client() else {
            return;
        };
        eprintln!("festerm-mobile: listing tunnels for {account}");
        let target = account.clone();
        let job = Job::spawn(ctx, move |cancel| client.list_tunnels(&target, cancel));
        self.tunnel.list = Some(TunnelList {
            account,
            job: Some(job),
            items: Vec::new(),
            error: None,
        });
    }

    /// Deletes a listed tunnel from the service, then lists the account again.
    fn delete_listed_tunnel(&mut self, ctx: &egui::Context, index: usize) {
        let Some(client) = self.tunnel_client() else {
            return;
        };
        let Some(list) = self.tunnel.list.as_mut().filter(|list| list.job.is_none()) else {
            return;
        };
        let Some(tunnel) = list.items.get(index) else {
            return;
        };
        if self.editor.tunnel_name == tunnel.locator {
            self.editor.tunnel_name.clear();
        }
        let account = list.account.clone();
        let locator = tunnel.locator.clone();
        list.job = Some(Job::spawn(ctx, move |cancel| {
            client.delete_tunnel(&account, &locator, cancel)?;
            client.list_tunnels(&account, cancel)
        }));
        list.error = None;
    }

    fn rename_listed_tunnel(&mut self, ctx: &egui::Context, index: usize) {
        let Some(client) = self.tunnel_client() else {
            return;
        };
        let Some(list) = self.tunnel.list.as_mut().filter(|list| list.job.is_none()) else {
            return;
        };
        let Some(tunnel) = list.items.get(index) else {
            return;
        };
        let account = list.account.clone();
        let locator = tunnel.locator.clone();
        let description = self.editor.friendly_name_draft.trim().to_owned();
        list.job = Some(Job::spawn(ctx, move |cancel| {
            client.update_tunnel_description(&account, &locator, &description, cancel)?;
            client.list_tunnels(&account, cancel)
        }));
        self.editor.renaming_tunnel = None;
        self.editor.friendly_name_draft.clear();
        list.error = None;
    }

    /// Connects (or re-attaches `tab`) to a saved Dev Tunnel profile. An
    /// `automatic` reconnect never prompts and only reclaims the tab's own
    /// attachment (ADR 0057).
    pub(super) fn start_tunnel_connect(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        tab: Option<u64>,
        automatic: bool,
    ) {
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
            if automatic {
                self.tunnel.attach_error = Some(describe_error(&TunnelError::SignInRequired));
                return;
            }
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
        let client = tab
            .and_then(|id| self.tabs.iter().find(|tab| tab.id == id))
            .and_then(|tab| tab.tunnel_client.clone())
            .unwrap_or_default();
        self.tunnel.connect = Some(ConnectFlow {
            spec,
            tab,
            client,
            automatic,
            stage: ConnectStage::Describing(job),
        });
    }

    fn begin_attach(
        &mut self,
        ctx: &egui::Context,
        spec: TunnelSpec,
        tab: Option<u64>,
        client: ClientId,
        automatic: bool,
    ) {
        let Some(tunnels) = self.tunnel_client() else {
            return;
        };
        let notifier = Arc::new(live_ssh::Repaint(ctx.clone()));
        let profile = spec.profile.clone();
        let id = client.clone();
        let mode = if automatic {
            AttachMode::Automatic
        } else {
            AttachMode::Manual
        };
        let target = tab.and_then(|tab| {
            if self.active_tab == Some(tab) {
                self.sink
                    .live
                    .connection
                    .borrow()
                    .as_ref()
                    .and_then(live_ssh::LiveSsh::tunnel_target)
            } else {
                self.sink
                    .live
                    .background
                    .borrow()
                    .iter()
                    .find(|(id, _)| *id == tab)
                    .and_then(|(_, live)| live.tunnel_target())
            }
        });
        let job = Job::spawn(ctx, move |cancel| {
            if automatic {
                let target = target.ok_or(TunnelError::SessionNotFound)?;
                tunnels.resume_as(&profile, &id, &target, notifier, cancel)
            } else {
                tunnels.attach_as(&profile, &id, mode, notifier, cancel)
            }
        });
        self.tunnel.connect = Some(ConnectFlow {
            spec,
            tab,
            client,
            automatic,
            stage: ConnectStage::Attaching(job),
        });
    }

    /// Reconnects the active Dev Tunnel tab once per activation (app resume or
    /// tab switch) when its profile allows it and the session was lost.
    pub(super) fn auto_reconnect_tunnel(&mut self, ctx: &egui::Context) {
        if self.tunnel.connect.is_some() || self.tunnel.sign_in.is_some() {
            return;
        }
        let Some(id) = self.active_tab else {
            return;
        };
        let lost = self
            .sink
            .live
            .connection
            .borrow()
            .as_ref()
            .is_some_and(live_ssh::LiveSsh::tunnel_lost);
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            return;
        };
        if !lost || !tab.reconnect_armed {
            return;
        }
        let Some(profile) = tab
            .tunnel
            .as_ref()
            .map(|spec| spec.profile.as_str().to_owned())
        else {
            return;
        };
        tab.reconnect_armed = false;
        let enabled = self
            .profiles
            .as_ref()
            .and_then(|profiles| profiles.tunnel_spec(&profile))
            .is_some_and(|spec| spec.auto_reconnect);
        if enabled {
            self.start_tunnel_connect(ctx, &profile, Some(id), true);
        }
    }

    fn fail_tunnel_connect(&mut self, ctx: &egui::Context, flow: ConnectFlow, error: TunnelError) {
        if error.needs_sign_in() && !flow.automatic {
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
        client: ClientId,
        attachment: Attachment,
    ) {
        if tab.is_some_and(|id| !self.tabs.iter().any(|tab| tab.id == id)) {
            let _ = attachment.session().try_shutdown();
            return;
        }
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
        if let Some(tab) = self
            .active_tab
            .and_then(|id| self.tabs.iter_mut().find(|tab| tab.id == id))
        {
            tab.tunnel_client = Some(client);
            tab.reconnect_armed = true;
        }
        self.terminal = terminal;
        self.view = self.fresh_view();
        *self.sink.live.connection.borrow_mut() = Some(live);
        self.workspace = workspaces::Workspace::Terminal;
        self.surface = Surface::Terminal;
        self.clear_sticky_modifiers();
    }

    /// Advances sign-in, tunnel listing and connect flows once per frame.
    pub(super) fn poll_tunnels(&mut self, ctx: &egui::Context) {
        self.poll_sign_in(ctx);
        if let Some(list) = self.tunnel.list.as_mut() {
            if let Some(job) = &list.job {
                match job.poll() {
                    Poll::Pending => {}
                    Poll::Done(Ok(items)) => {
                        eprintln!("festerm-mobile: listed {} tunnel(s)", items.len());
                        list.items = items;
                        list.job = None;
                    }
                    Poll::Done(Err(error)) => {
                        eprintln!("festerm-mobile: tunnel listing failed: {error:?}");
                        list.error = Some(describe_error(&error));
                        list.job = None;
                    }
                    Poll::Lost => {
                        eprintln!("festerm-mobile: tunnel listing stopped without a result");
                        list.error = Some("Listing stopped. Refresh to try again.".to_owned());
                        list.job = None;
                    }
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
                    if description.trust == HostTrust::Trusted {
                        let ConnectFlow {
                            spec,
                            tab,
                            client,
                            automatic,
                            ..
                        } = flow;
                        self.begin_attach(ctx, spec, tab, client, automatic);
                    } else if flow.automatic {
                        self.tunnel.attach_error = Some(
                            "The host needs verifying. Tap Reconnect to review it.".to_owned(),
                        );
                    } else {
                        self.tunnel.connect = Some(ConnectFlow {
                            stage: ConnectStage::Verify(description),
                            ..flow
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
                    let ConnectFlow {
                        spec, tab, client, ..
                    } = flow;
                    self.install_attachment(ctx, spec, tab, client, attachment);
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
                    flow.started = flow.start.take();
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
                            self.start_tunnel_connect(ctx, &profile, tab, false);
                        }
                        Resume::Nothing => {
                            self.select_tunnel_account(account.id.clone());
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
        #[cfg(test)]
        if let Some(prompt) = &self.tunnel.preview_prompt {
            let view = SignInView {
                prompt: Some(prompt),
                error: None,
            };
            tunnel_link::sign_in_sheet(ctx, bounds, &view);
            return;
        }
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
                    client: tab_client,
                    automatic,
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
                    Ok(()) => self.begin_attach(ctx, spec, tab, tab_client, automatic),
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
                "Another client now holds this session.\nfesTerm does not reattach on its own."
                    .to_owned()
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
    /// Changing identity invalidates account-scoped discovery and the draft target.
    fn select_tunnel_account(&mut self, account: String) {
        if self.editor.tunnel_account.as_ref() != Some(&account) {
            self.editor.tunnel_name.clear();
            self.editor.session_name.clear();
            self.editor.confirm_delete_tunnel = false;
            self.editor.renaming_tunnel = None;
            self.editor.friendly_name_draft.clear();
            self.editor.tunnel = None;
            self.tunnel.list = None;
            self.editor.tunnel_account = Some(account);
        }
    }

    /// Fills the editor's account and tunnel choices from the live backend.
    pub(super) fn sync_editor_tunnel(&mut self, ctx: &egui::Context) {
        if !self.editor.open || self.editor.kind(self.caps) != model::ProfileKind::DevTunnel {
            return;
        }
        #[cfg(test)]
        if self.tunnel.preview_editor {
            return;
        }
        if self.tunnel_client().is_none() {
            return;
        }
        let accounts = self.tunnel_accounts();
        let bound = self
            .editor
            .tunnel_account
            .clone()
            .or_else(|| {
                if self.editor.editing.is_none() {
                    accounts.first().map(|(id, _)| id.clone())
                } else {
                    None
                }
            });
        if bound != self.editor.tunnel_account {
            self.editor.tunnel_account = bound.clone();
        }
        let listed = self.tunnel.list.as_ref().map(|list| list.account.clone());
        if let Some(account) = bound
            .as_ref()
            .filter(|id| accounts.iter().any(|(known, _)| known == *id))
        {
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
        let list = self
            .tunnel
            .list
            .as_ref()
            .filter(|list| Some(&list.account) == bound.as_ref());
        self.editor.tunnel = Some(model::DevTunnelChoices {
            accounts,
            account_login: login,
            tunnels: list
                .map(|list| {
                    list.items
                        .iter()
                        .map(|tunnel| model::DevTunnelChoice {
                            locator: tunnel.locator.clone(),
                            name: tunnel.name.clone(),
                            description: tunnel.description.clone(),
                        })
                        .collect()
                })
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
            forms::EditorAction::SelectTunnelAccount(index) => {
                let account = self
                    .editor
                    .tunnel
                    .as_ref()
                    .and_then(|choices| choices.accounts.get(index))
                    .map(|(id, _)| id.clone());
                if let Some(account) = account {
                    if self.tunnel_accounts().iter().any(|(id, _)| id == &account) {
                        self.select_tunnel_account(account);
                    }
                }
            }
            forms::EditorAction::SignIn => {
                self.start_sign_in(ctx, None, Resume::Nothing);
            }
            forms::EditorAction::SignOut => {
                if let Some(account) = self.editor.tunnel_account.clone() {
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
                    .cloned()
                {
                    let friendly_name = tunnel_profile_default_name(
                        self.editor.name.trim().is_empty(),
                        self.editor.editing.is_some(),
                        &tunnel,
                    );
                    self.editor.tunnel_name = tunnel.locator;
                    if let Some(name) = friendly_name {
                        self.editor.name = name;
                    }
                }
            }
            forms::EditorAction::DeleteTunnel(index) => self.delete_listed_tunnel(ctx, index),
            forms::EditorAction::RenameTunnel(index) => self.rename_listed_tunnel(ctx, index),
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
            super::profiles::TunnelBehaviour {
                take_over: self.editor.take_over,
                auto_reconnect: self.editor.auto_reconnect,
            },
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
        self.start_tunnel_connect(ctx, id, stale, false);
    }
}

/// The name a profile stores for a listed tunnel.
fn tunnel_label(tunnel: &festerm_mobile_tunnels::TunnelInfo) -> String {
    tunnel
        .description
        .clone()
        .or_else(|| tunnel.name.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| tunnel.locator.clone())
}

fn tunnel_profile_default_name(
    name_is_empty: bool,
    editing: bool,
    tunnel: &festerm_mobile_tunnels::TunnelInfo,
) -> Option<String> {
    (name_is_empty && !editing).then(|| {
        let label: String = tunnel_label(tunnel)
            .chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect();
        let name = if label.trim().is_empty() {
            tunnel.locator.as_str()
        } else {
            label.trim()
        };
        name.chars()
            .take(festerm_mobile_profiles::MAX_NAME_CHARS)
            .collect::<String>()
            .trim_end()
            .to_owned()
    })
}

#[cfg(test)]
mod friendly_name_tests {
    use super::*;

    #[test]
    fn picked_tunnel_friendly_name_defaults_only_a_new_blank_profile_name() {
        let tunnel = festerm_mobile_tunnels::TunnelInfo {
            locator: "abc123.use".into(),
            name: Some("festerm-abc123".into()),
            description: Some("Raspberry Pi 5".into()),
            active: true,
        };
        assert_eq!(
            tunnel_profile_default_name(true, false, &tunnel).as_deref(),
            Some("Raspberry Pi 5")
        );
        assert_eq!(tunnel_profile_default_name(false, false, &tunnel), None);
        assert_eq!(tunnel_profile_default_name(true, true, &tunnel), None);
    }

    #[test]
    fn picked_tunnel_profile_name_respects_ios_name_constraints() {
        let mut tunnel = festerm_mobile_tunnels::TunnelInfo {
            locator: "abc123.use".into(),
            name: None,
            description: Some("\u{754c}".repeat(100)),
            active: true,
        };
        assert_eq!(
            tunnel_profile_default_name(true, false, &tunnel),
            Some("\u{754c}".repeat(80))
        );

        tunnel.description = Some(" Production\nserver ".into());
        assert_eq!(
            tunnel_profile_default_name(true, false, &tunnel).as_deref(),
            Some("Production server")
        );

        tunnel.description = Some("\0".into());
        assert_eq!(
            tunnel_profile_default_name(true, false, &tunnel).as_deref(),
            Some("abc123.use")
        );
    }
}
