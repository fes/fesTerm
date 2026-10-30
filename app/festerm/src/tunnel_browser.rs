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
}

enum Stage {
    Working,
    Device(DevicePrompt),
    Pick {
        login: String,
        tunnels: Vec<TunnelSummary>,
    },
    Error(Error),
}

pub(super) struct Browser {
    cancel: CancellationToken,
    receiver: mpsc::Receiver<Event>,
    stage: Stage,
    focus_cancel: bool,
    store: Arc<dyn SecretStore>,
    _lease: Option<std::fs::File>,
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Browser {
    /// With an account, lists its tunnels using the stored credential. Without
    /// one, runs the GitHub device flow and stores the result as an account.
    fn start(
        account: Option<DevTunnelAccount>,
        store: Arc<dyn SecretStore>,
        context: &egui::Context,
    ) -> Self {
        let cancel = CancellationToken::new();
        let (sender, receiver) = mpsc::sync_channel(2);
        let mut browser = Self {
            cancel: cancel.clone(),
            receiver,
            stage: Stage::Working,
            focus_cancel: true,
            store: Arc::clone(&store),
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
                let outcome = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| Error::Network)
                    .and_then(|runtime| {
                        runtime.block_on(async {
                            tokio::select! {
                                _ = cancel.cancelled() => Err(Error::Cancelled),
                                _ = tokio::time::sleep(Duration::from_secs(1800)) => Err(Error::Expired),
                                value = run(account, &store, client_id, &cancel, &notify) => value,
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
    let tunnels = service::list(&identity, cancel).await?;
    Ok(Outcome::Listed(identity.account.login.clone(), tunnels))
}

/// A profile identifier that does not collide with an existing profile.
pub(super) fn unique_profile_id(summary: &TunnelSummary, exists: impl Fn(&str) -> bool) -> String {
    let base = summary
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .or(summary
            .description
            .as_deref()
            .filter(|text| !text.trim().is_empty()))
        .map(|text| text.chars().filter(|c| !c.is_control()).collect::<String>())
        .map(|text| text.trim().chars().take(60).collect::<String>())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| summary.locator.clone());
    if !exists(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base} ({n})"))
        .find(|candidate| !exists(candidate))
        .expect("unbounded range")
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
        self.tunnel_browser = Some(Browser::start(account, store, context));
        self.overlays.native_tunnel = true;
        context.request_repaint();
    }

    pub(super) fn poll_tunnel_browser(&mut self) {
        let Some(browser) = self.tunnel_browser.as_mut() else {
            return;
        };
        let mut signed = None;
        while let Ok(event) = browser.receiver.try_recv() {
            browser.stage = match event {
                Event::Device(prompt) => Stage::Device(prompt),
                Event::Signed(account) => {
                    signed = Some(account);
                    continue;
                }
                Event::Listed(Ok((login, tunnels))) => Stage::Pick { login, tunnels },
                Event::Listed(Err(error)) => Stage::Error(error),
            };
            browser.focus_cancel = true;
        }
        if let Some(account) = signed {
            let store = self
                .tunnel_browser
                .take()
                .map(|browser| Arc::clone(&browser.store));
            self.overlays.native_tunnel = false;
            self.save_signed_in_account(account, store);
        }
    }

    /// Adds a freshly signed-in account, replacing the credential of an
    /// existing account for the same GitHub user so its profiles keep working.
    fn save_signed_in_account(
        &mut self,
        account: DevTunnelAccount,
        store: Option<Arc<dyn SecretStore>>,
    ) {
        let configuration = self.state.configuration();
        let previous = configuration
            .accounts()
            .iter()
            .find(|known| known.account_id() == account.account_id())
            .map(|known| known.credential_reference().duplicate_for_transport());
        let new_reference = account.credential_reference().duplicate_for_transport();
        let login = account.login().to_owned();
        let replacement = configuration
            .with_account(account)
            .map(|(config, _)| config);
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
        egui::Modal::new(egui::Id::new("browse_dev_tunnels")).show(context, |ui| {
            super::confirmations::prepare_confirmation(ui, context, 520.0);
            ui.heading("Dev Tunnels");
            super::confirmations::confirmation_body(ui, |ui| match &browser.stage {
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
                    if action_button_monospace(ui, ActionButtonRole::Secondary, &prompt.user_code)
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
                Stage::Pick { login, tunnels } => {
                    ui.label(format!("GitHub · {login}"));
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
                        let title = tunnel
                            .name
                            .as_deref()
                            .or(tunnel.description.as_deref())
                            .unwrap_or(&tunnel.locator);
                        let label = format!(
                            "{title} · {}{}{}",
                            tunnel.locator,
                            if tunnel.active { " · active" } else { "" },
                            if saved { " · saved" } else { "" }
                        );
                        ui.add_enabled_ui(!saved, |ui| {
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
                        });
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
        if let Some(tunnel) = picked {
            self.create_profile_from_tunnel(&tunnel);
            dismiss = true;
        }
        if dismiss {
            self.tunnel_browser = None;
            self.overlays.native_tunnel = false;
        }
    }

    fn create_profile_from_tunnel(&mut self, tunnel: &TunnelSummary) {
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
        let id = unique_profile_id(tunnel, |id| configuration.profile(id).is_some());
        let saved = Profile::named_dev_tunnel(id, tunnel.locator.clone())
            .and_then(|profile| profile.with_tunnel_account_selection(account))
            .is_ok_and(|profile| self.save_profile(profile));
        self.secure_storage_feedback = Some(if saved {
            "Dev Tunnel profile created. Open it and choose Connect."
        } else {
            "The Dev Tunnel profile could not be created."
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(name: Option<&str>, description: Option<&str>) -> TunnelSummary {
        TunnelSummary {
            locator: "abc123.use".into(),
            name: name.map(Into::into),
            description: description.map(Into::into),
            active: true,
        }
    }

    #[test]
    fn profile_names_prefer_tunnel_name_then_description_then_locator_and_never_collide() {
        let none = |_: &str| false;
        assert_eq!(
            unique_profile_id(&summary(Some("dev-box"), Some("x")), none),
            "dev-box"
        );
        assert_eq!(
            unique_profile_id(&summary(None, Some("laptop")), none),
            "laptop"
        );
        assert_eq!(unique_profile_id(&summary(None, None), none), "abc123.use");
        let taken = |id: &str| id == "dev-box" || id == "dev-box (2)";
        assert_eq!(
            unique_profile_id(&summary(Some("dev-box"), None), taken),
            "dev-box (3)"
        );
    }
}
