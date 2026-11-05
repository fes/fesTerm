//! "Browse my tunnels": signs in with the GitHub device flow, lists the
//! account's Dev Tunnels that host a fesTerm bridge, and creates an unsigned
//! Dev Tunnel profile from the one the user picks. Nothing from the sign-in is
//! retained; the profile still signs in on its own the first time it connects.

use std::sync::{mpsc, Arc};
use std::time::Duration;

use eframe::egui;
use festerm_config::{AccountProvider, DevTunnelAccount, Profile};
use festerm_secret_store::{SecretBytes, SecretStore};
use festerm_tunnel_client::{
    identity::{self, DevicePrompt, Identity},
    service::{self, TunnelSummary},
    Error, Https,
};
use festerm_ui_egui::controls::{action_button_monospace, ActionButtonRole};
use festerm_ui_egui::theme;
use tokio_util::sync::CancellationToken;

use super::FesTermApp;

enum Event {
    Device(DevicePrompt),
    /// A new sign-in finished and its credential is in the store.
    Signed(DevTunnelAccount),
    Listed(Result<(String, Vec<TunnelSummary>), Error>),
    LocalRemoval(Result<festerm_config::LocalTunnelHosting, &'static str>),
}

enum Stage {
    Working,
    Device(DevicePrompt),
    Pick {
        login: String,
        tunnels: Vec<TunnelSummary>,
        /// The tunnel awaiting delete confirmation.
        confirm_delete: Option<String>,
        rename: Option<(String, String)>,
    },
    Error(Error),
}

pub(super) struct Browser {
    cancel: CancellationToken,
    receiver: mpsc::Receiver<Event>,
    stage: Stage,
    focus_cancel: bool,
    store: Arc<dyn SecretStore>,
    account: Option<DevTunnelAccount>,
    _lease: Option<std::fs::File>,
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Browser {
    /// With an account, lists its tunnels using the stored credential, first
    /// deleting `delete` if given. Without one, runs the GitHub device flow and
    /// stores the result as an account.
    fn start(
        account: Option<DevTunnelAccount>,
        store: Arc<dyn SecretStore>,
        context: &egui::Context,
        delete: Option<String>,
        rename: Option<(String, String)>,
    ) -> Self {
        let cancel = CancellationToken::new();
        let (sender, receiver) = mpsc::sync_channel(2);
        let mut browser = Self {
            cancel: cancel.clone(),
            receiver,
            stage: Stage::Working,
            focus_cancel: true,
            store: Arc::clone(&store),
            account: account.clone(),
            _lease: None,
        };
        let Some(client_id) = identity::registered_client_id() else {
            browser.stage = Stage::Error(Error::Registration);
            return browser;
        };
        let lease = match super::tunnel_workflow::account_lease(account.as_ref()) {
            Ok(lease) => lease,
            Err(error) => {
                browser.stage = Stage::Error(error);
                return browser;
            }
        };
        browser._lease = lease;
        let context = context.clone();
        let spawned = std::thread::Builder::new()
            .name("native-tunnel-browse".into())
            .spawn(move || {
                let notify = |event| {
                    let sent = sender.try_send(event).is_ok();
                    context.request_repaint();
                    sent
                };
                if let (Some(account), Some(locator)) = (account.as_ref(), delete.as_deref()) {
                    if cancel.is_cancelled() {
                        return;
                    }
                    match super::profile_hosting::revoke_cloud_browser_host(account.account_id(), locator, &cancel) {
                        Ok(Some(binding)) => {
                            notify(Event::LocalRemoval(Ok(binding)));
                            return;
                        }
                        Err(message) => {
                            notify(Event::LocalRemoval(Err(message)));
                            return;
                        }
                        Ok(None) => {}
                    }
                }
                let outcome = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| Error::Network)
                    .and_then(|runtime| {
                        runtime.block_on(async {
                            tokio::select! {
                                _ = cancel.cancelled() => Err(Error::Cancelled),
                                _ = tokio::time::sleep(Duration::from_secs(1800)) => Err(Error::Expired),
                                value = run(account, &store, client_id, &cancel, &notify, delete.as_deref(), rename.as_ref()) => value,
                            }
                        })
                    });
                notify(match outcome {
                    Ok(Outcome::Listed(login, tunnels)) => Event::Listed(Ok((login, tunnels))),
                    Ok(Outcome::Signed(account)) => Event::Signed(account),
                    Err(error) => Event::Listed(Err(error)),
                });
            });
        if spawned.is_err() {
            browser.stage = Stage::Error(Error::Network);
        }
        browser
    }
}

/// Lists an account's tunnels with its stored credential, refreshing it when
/// needed. The caller holds the account lease.
pub(super) async fn list_for_account(
    account: DevTunnelAccount,
    store: &Arc<dyn SecretStore>,
    cancel: &CancellationToken,
) -> Result<Vec<TunnelSummary>, Error> {
    let client_id = identity::registered_client_id().ok_or(Error::Registration)?;
    match run(
        Some(account),
        store,
        client_id,
        cancel,
        &|_| true,
        None,
        None,
    )
    .await?
    {
        Outcome::Listed(_, tunnels) => Ok(tunnels),
        Outcome::Signed(_) => Err(Error::Response),
    }
}

enum Outcome {
    Listed(String, Vec<TunnelSummary>),
    Signed(DevTunnelAccount),
}

async fn run(
    account: Option<DevTunnelAccount>,
    store: &Arc<dyn SecretStore>,
    client_id: &str,
    cancel: &CancellationToken,
    notify: &impl Fn(Event) -> bool,
    delete: Option<&str>,
    rename: Option<&(String, String)>,
) -> Result<Outcome, Error> {
    let http = Https::new()?;
    let Some(account) = account else {
        let identity = identity::sign_in(&http, client_id, None, cancel, |prompt| {
            if !notify(Event::Device(prompt)) {
                cancel.cancel();
            }
        })
        .await?;
        let reference = store
            .put(&SecretBytes::copy_from_slice(&identity.encode_private()?))
            .map_err(|_| Error::Store)?;
        return DevTunnelAccount::new(
            format!("github-{}", identity.account.login.to_ascii_lowercase()),
            AccountProvider::Github,
            identity.account.id,
            identity.account.login.clone(),
            reference,
        )
        .map(Outcome::Signed)
        .map_err(|_| Error::Response);
    };
    let secret = store
        .get(account.credential_reference())
        .map_err(|_| Error::Store)?;
    let mut identity = secret
        .with_bytes(|bytes| Identity::decode_private(bytes, account.account_id(), client_id))?;
    if identity.needs_sign_in() {
        identity = identity.refresh(&http, client_id, cancel).await?;
        store
            .update(
                account.credential_reference(),
                &SecretBytes::copy_from_slice(&identity.encode_private()?),
            )
            .map_err(|_| Error::Store)?;
    }
    if let Some(tunnel) = delete {
        service::delete(&identity, tunnel, cancel).await?;
    }
    if let Some((tunnel, friendly_name)) = rename {
        service::update_description(&identity, tunnel, friendly_name, cancel).await?;
    }
    let tunnels = service::list(&identity, cancel).await?;
    Ok(Outcome::Listed(identity.account.login.clone(), tunnels))
}

/// The name a tunnel is listed under.
pub(super) fn display_name(summary: &TunnelSummary) -> String {
    summary
        .description
        .as_deref()
        .or(summary.name.as_deref())
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(&summary.locator)
        .to_owned()
}

impl FesTermApp {
    /// Opens the shared-account sign-in (`None`) or the tunnel picker for an
    /// existing account.
    pub(super) fn begin_tunnel_browser(
        &mut self,
        account: Option<String>,
        context: &egui::Context,
    ) {
        if self.tunnel_browser.is_some() || self.native_tunnel.is_some() {
            return;
        }
        let Ok(store) = &self.secret_store else {
            self.secure_storage_feedback = Some("Secure storage unavailable. Cannot sign in.");
            return;
        };
        let store = Arc::clone(store);
        let account = match account {
            Some(key) => match self.state.configuration().account(&key) {
                Some(account) => Some(account.clone()),
                None => return,
            },
            None => None,
        };
        self.tunnel_browser = Some(Browser::start(account, store, context, None, None));
        self.overlays.native_tunnel = true;
        context.request_repaint();
    }

    pub(super) fn poll_tunnel_browser(&mut self, context: &egui::Context) {
        let Some(browser) = self.tunnel_browser.as_mut() else {
            return;
        };
        let mut signed = None;
        let mut local_removal = None;
        while let Ok(event) = browser.receiver.try_recv() {
            browser.stage = match event {
                Event::Device(prompt) => Stage::Device(prompt),
                Event::Signed(account) => {
                    signed = Some(account);
                    continue;
                }
                Event::Listed(Ok((login, tunnels))) => Stage::Pick {
                    login,
                    tunnels,
                    confirm_delete: None,
                    rename: None,
                },
                Event::Listed(Err(error)) => Stage::Error(error),
                Event::LocalRemoval(result) => {
                    local_removal = Some(result);
                    continue;
                }
            };
            browser.focus_cancel = true;
        }
        if let Some(result) = local_removal {
            self.tunnel_browser = None;
            self.overlays.native_tunnel = false;
            match result {
                Ok(binding) => self.queue_cloud_browser_host_removal(binding),
                Err(message) => self.secure_storage_feedback = Some(message),
            }
            return;
        }
        if let Some(account) = signed {
            let store = self
                .tunnel_browser
                .take()
                .map(|browser| Arc::clone(&browser.store));
            self.overlays.native_tunnel = false;
            self.save_signed_in_account(account, store, context);
        }
    }

    /// Adds a freshly signed-in account, replacing the credential of an
    /// existing account for the same GitHub user so its profiles keep working.
    fn save_signed_in_account(
        &mut self,
        account: DevTunnelAccount,
        store: Option<Arc<dyn SecretStore>>,
        context: &egui::Context,
    ) {
        let configuration = self.state.configuration();
        let previous = configuration
            .accounts()
            .iter()
            .find(|known| known.account_id() == account.account_id())
            .map(|known| known.credential_reference().duplicate_for_transport());
        let new_reference = account.credential_reference().duplicate_for_transport();
        let login = account.login().to_owned();
        let mut key = None;
        let replacement = configuration.with_account(account).map(|(config, added)| {
            key = Some(added);
            config
        });
        let saved = self.apply_configuration_save(
            replacement,
            super::ConfigurationStartupStatus::ProfileSaveFailure,
            crate::configuration_startup::ConfigurationReloader::save_profile,
        );
        if let Some(store) = store {
            let discarded = if saved { previous } else { Some(new_reference) };
            if let Some(reference) = discarded {
                let _ = store.delete(&reference);
            }
        }
        if let Some(key) = key.filter(|_| saved) {
            context.data_mut(|data| {
                data.insert_temp(signed_in_account_id(), SignedInAccount(key));
            });
            context.request_repaint();
        }
        self.secure_storage_feedback = Some(if saved {
            let _ = login;
            "Signed in. The account is available to every Dev Tunnel profile."
        } else {
            "The account could not be saved. Sign in again."
        });
    }

    /// Signs an account out: deletes its credential and unlinks its profiles.
    pub(super) fn remove_dev_tunnel_account(&mut self, key: &str) {
        let Some(credential) = self
            .state
            .configuration()
            .account(key)
            .map(|account| account.credential_reference().duplicate_for_transport())
        else {
            return;
        };
        let previous: Vec<Profile> = self
            .state
            .configuration()
            .profiles()
            .iter()
            .filter(|profile| {
                profile
                    .as_dev_tunnel()
                    .is_some_and(|tunnel| tunnel.account() == Some(key))
            })
            .cloned()
            .collect();
        let saved = self.apply_configuration_save(
            self.state.configuration().without_account(key),
            super::ConfigurationStartupStatus::ProfileSaveFailure,
            crate::configuration_startup::ConfigurationReloader::save_profile,
        );
        if saved {
            if let Ok(store) = &self.secret_store {
                let _ = store.delete(&credential);
                for profile in &previous {
                    self.retire_native_tunnel_credentials(Some(profile));
                }
            }
        }
    }

    /// `(key, login, profiles using it)` for the Settings account list.
    pub(super) fn dev_tunnel_account_rows(&self) -> Vec<(String, String, usize)> {
        let configuration = self.state.configuration();
        configuration
            .accounts()
            .iter()
            .map(|account| {
                (
                    account.identifier().to_owned(),
                    account.login().to_owned(),
                    configuration.account_profile_count(account.identifier()),
                )
            })
            .collect()
    }

    pub(super) fn show_tunnel_browser(&mut self, context: &egui::Context, escape: bool) {
        let Some(browser) = self.tunnel_browser.as_mut() else {
            return;
        };
        let configuration = self.state.configuration();
        let mut dismiss = escape;
        let mut picked = None;
        let mut delete = None;
        let mut rename = None;
        egui::Modal::new(egui::Id::new("browse_dev_tunnels")).show(context, |ui| {
            super::confirmations::prepare_confirmation(ui, context, 520.0);
            ui.heading("Dev Tunnels");
            super::confirmations::confirmation_body(ui, |ui| match &mut browser.stage {
                Stage::Working => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Working…");
                    });
                }
                Stage::Device(prompt) => {
                    ui.label("Sign in to Microsoft Dev Tunnels");
                    ui.label("Enter this code on GitHub:");
                    ui.add_space(6.0);
                    if action_button_monospace(ui, ActionButtonRole::Accent, &prompt.user_code)
                        .on_hover_text("Copy code and open GitHub")
                        .clicked()
                    {
                        ui.ctx().copy_text(prompt.user_code.clone());
                        ui.ctx()
                            .open_url(egui::OpenUrl::new_tab(identity::VERIFICATION_URI));
                    }
                    ui.add_space(6.0);
                    ui.label("Waiting for sign-in…");
                }
                Stage::Pick {
                    login,
                    tunnels,
                    confirm_delete: confirm_delete @ Some(_),
                    ..
                } => {
                    let locator = confirm_delete.clone().unwrap_or_default();
                    let locator = &locator;
                    let title = tunnels
                        .iter()
                        .find(|tunnel| &tunnel.locator == locator)
                        .map_or_else(|| locator.clone(), display_name);
                    ui.label(format!("GitHub · {login}"));
                    ui.label(format!("Delete the Dev Tunnel “{title}” ({locator})?"));
                    ui.label(
                        "Its host becomes unreachable until its bridge is set up again. \
                         This cannot be undone. For a locally managed desktop host, \
                         deletion also revokes local exposure; its persistent shell is kept.",
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if super::confirmations::dialog_action(
                            ui,
                            ActionButtonRole::DangerOutline,
                            "Delete tunnel",
                        )
                        .clicked()
                        {
                            delete = Some(locator.clone());
                        }
                        if super::confirmations::dialog_action(
                            ui,
                            ActionButtonRole::Secondary,
                            "Keep",
                        )
                        .clicked()
                        {
                            *confirm_delete = None;
                        }
                    });
                }
                Stage::Pick {
                    login,
                    tunnels,
                    confirm_delete,
                    rename: rename_draft,
                } => {
                    ui.label(format!("GitHub · {login}"));
                    ui.label("Choose a tunnel; its friendly name will prefill the profile name.");
                    ui.add_space(6.0);
                    if tunnels.is_empty() {
                        ui.label("No fesTerm bridge tunnels found for this account.");
                    }
                    for tunnel in tunnels {
                        let saved = configuration.profiles().iter().any(|profile| {
                            profile.as_dev_tunnel().is_some_and(|profile| {
                                profile.tunnel_name() == Some(&tunnel.locator)
                            })
                        });
                        let title = display_name(tunnel);
                        let label = format!(
                            "{title}{}{}",
                            if tunnel.active { " · active" } else { "" },
                            if saved { " · saved" } else { "" }
                        );
                        ui.horizontal(|ui| {
                            if super::confirmations::dialog_action(
                                ui,
                                if tunnel.active {
                                    ActionButtonRole::Accent
                                } else {
                                    ActionButtonRole::Secondary
                                },
                                &label,
                            )
                            .clicked()
                            {
                                picked = Some(tunnel.clone());
                            }
                            if super::confirmations::dialog_action(
                                ui,
                                ActionButtonRole::Secondary,
                                if rename_draft
                                    .as_ref()
                                    .is_some_and(|(id, _)| id == &tunnel.locator)
                                {
                                    "Editing name"
                                } else {
                                    "Rename"
                                },
                            )
                            .clicked()
                            {
                                *rename_draft = Some((
                                    tunnel.locator.clone(),
                                    tunnel.description.clone().unwrap_or_default(),
                                ));
                            }
                            if super::confirmations::dialog_action(
                                ui,
                                ActionButtonRole::DangerOutline,
                                "Delete",
                            )
                            .on_hover_text("Delete this Dev Tunnel")
                            .clicked()
                            {
                                *confirm_delete = Some(tunnel.locator.clone());
                            }
                        });
                        ui.small(format!("Tunnel ID: {}", tunnel.locator));
                        let mut cancel_rename = false;
                        if let Some((locator, friendly_name)) = rename_draft
                            .as_mut()
                            .filter(|(locator, _)| locator == &tunnel.locator)
                        {
                            ui.vertical(|ui| {
                                ui.label(format!("Friendly name for {locator}"));
                                ui.text_edit_singleline(friendly_name);
                                ui.horizontal(|ui| {
                                    if super::confirmations::dialog_action(
                                        ui,
                                        ActionButtonRole::Accent,
                                        "Save",
                                    )
                                    .clicked()
                                    {
                                        rename = Some((
                                            locator.clone(),
                                            friendly_name.trim().to_owned(),
                                        ));
                                    }
                                    if super::confirmations::dialog_action(
                                        ui,
                                        ActionButtonRole::Secondary,
                                        "Cancel",
                                    )
                                    .clicked()
                                    {
                                        cancel_rename = true;
                                    }
                                });
                            });
                        }
                        if cancel_rename {
                            *rename_draft = None;
                        }
                    }
                }
                Stage::Error(error) => {
                    ui.colored_label(theme::STATUS_ERROR, error.to_string());
                }
            });
            ui.add_space(10.0);
            let cancel = super::confirmations::dialog_action(
                ui,
                ActionButtonRole::Secondary,
                if matches!(browser.stage, Stage::Pick { .. } | Stage::Error(_)) {
                    "Close"
                } else {
                    "Cancel"
                },
            );
            if browser.focus_cancel {
                cancel.request_focus();
                browser.focus_cancel = false;
            }
            dismiss |= cancel.clicked();
        });
        if let Some(locator) = delete {
            let account = self.tunnel_browser.as_ref().and_then(|b| b.account.clone());
            let store = self.tunnel_browser.as_ref().map(|b| Arc::clone(&b.store));
            if let (Some(account), Some(store)) = (account, store) {
                // Drop the old browser first so its account lease is released.
                self.tunnel_browser = None;
                self.tunnel_browser = Some(Browser::start(
                    Some(account),
                    store,
                    context,
                    Some(locator),
                    None,
                ));
            }
            return;
        }
        if let Some(rename) = rename {
            let account = self.tunnel_browser.as_ref().and_then(|b| b.account.clone());
            let store = self.tunnel_browser.as_ref().map(|b| Arc::clone(&b.store));
            if let (Some(account), Some(store)) = (account, store) {
                self.tunnel_browser = None;
                self.tunnel_browser = Some(Browser::start(
                    Some(account),
                    store,
                    context,
                    None,
                    Some(rename),
                ));
            }
            return;
        }
        if let Some(tunnel) = picked {
            self.hand_picked_tunnel_to_form(context, &tunnel);
            dismiss = true;
        }
        if dismiss {
            self.tunnel_browser = None;
            self.overlays.native_tunnel = false;
        }
    }

    /// Leaves the chosen tunnel for the form that opened the picker (the
    /// profile editor or the New Session form), which fills its tunnel field
    /// on its next frame.
    fn hand_picked_tunnel_to_form(&mut self, context: &egui::Context, tunnel: &TunnelSummary) {
        let configuration = self.state.configuration();
        let account = self
            .tunnel_browser
            .as_ref()
            .and_then(|browser| match &browser.stage {
                Stage::Pick { login, .. } => configuration
                    .accounts()
                    .iter()
                    .find(|account| account.login() == login)
                    .map(|account| account.identifier().to_owned()),
                _ => None,
            });
        context.data_mut(|data| {
            data.insert_temp(
                picked_tunnel_id(),
                PickedTunnel {
                    account,
                    locator: tunnel.locator.clone(),
                    friendly_name: display_name(tunnel),
                },
            )
        });
        context.request_repaint();
    }
}

/// A tunnel chosen in the picker, waiting for its form to consume it.
#[derive(Clone, Default)]
pub(crate) struct PickedTunnel {
    pub(crate) account: Option<String>,
    pub(crate) locator: String,
    pub(crate) friendly_name: String,
}

impl PickedTunnel {
    pub(crate) fn seed_profile_name(&self, name: &mut String) {
        if name.trim().is_empty() {
            *name = self.friendly_name.clone();
        }
    }
}

fn picked_tunnel_id() -> egui::Id {
    egui::Id::new("picked_dev_tunnel")
}

/// An account that just finished signing in, waiting for the open form to
/// select it.
#[derive(Clone, Default)]
struct SignedInAccount(String);

fn signed_in_account_id() -> egui::Id {
    egui::Id::new("signed_in_dev_tunnel_account")
}

/// Takes the identifier of the account that just signed in, if any.
pub(crate) fn take_signed_in_account(context: &egui::Context) -> Option<String> {
    context
        .data_mut(|data| data.remove_temp::<SignedInAccount>(signed_in_account_id()))
        .map(|account| account.0)
}

/// Takes the tunnel the picker just chose, if any.
pub(crate) fn take_picked_tunnel(context: &egui::Context) -> Option<PickedTunnel> {
    context.data_mut(|data| data.remove_temp::<PickedTunnel>(picked_tunnel_id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listed_tunnel_description_is_its_friendly_name() {
        let tunnel = TunnelSummary {
            locator: "abc123.use".into(),
            name: Some("festerm-abc123".into()),
            description: Some("Raspberry Pi 5".into()),
            active: true,
        };
        assert_eq!(display_name(&tunnel), "Raspberry Pi 5");
    }

    #[test]
    fn picked_tunnel_is_taken_exactly_once() {
        let context = egui::Context::default();
        assert!(take_picked_tunnel(&context).is_none());
        context.data_mut(|data| {
            data.insert_temp(
                picked_tunnel_id(),
                PickedTunnel {
                    account: Some("acct".into()),
                    locator: "abc123.use".into(),
                    friendly_name: "Raspberry Pi".into(),
                },
            )
        });
        let picked = take_picked_tunnel(&context).expect("picked");
        assert_eq!(picked.locator, "abc123.use");
        assert_eq!(picked.friendly_name, "Raspberry Pi");
        assert_eq!(picked.account.as_deref(), Some("acct"));
        assert!(take_picked_tunnel(&context).is_none());
    }

    #[test]
    fn picked_tunnel_seeds_only_an_empty_profile_name() {
        let picked = PickedTunnel {
            friendly_name: "Raspberry Pi 5".into(),
            ..PickedTunnel::default()
        };
        let mut name = String::new();
        picked.seed_profile_name(&mut name);
        assert_eq!(name, "Raspberry Pi 5");
        name = "My own name".into();
        picked.seed_profile_name(&mut name);
        assert_eq!(name, "My own name");
    }

    #[test]
    fn signed_in_account_is_taken_exactly_once() {
        let context = egui::Context::default();
        assert!(take_signed_in_account(&context).is_none());
        context.data_mut(|data| {
            data.insert_temp(signed_in_account_id(), SignedInAccount("acct".into()))
        });
        assert_eq!(take_signed_in_account(&context).as_deref(), Some("acct"));
        assert!(take_signed_in_account(&context).is_none());
    }
}
