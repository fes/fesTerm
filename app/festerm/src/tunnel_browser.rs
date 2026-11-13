//! "Browse my tunnels": signs in with the GitHub device flow, lists the
//! account's Dev Tunnels that host a fesTerm bridge, and creates an unsigned
//! Dev Tunnel profile from the one the user picks. Nothing from the sign-in is
//! retained; the profile still signs in on its own the first time it connects.

use std::sync::{atomic::AtomicU16, mpsc, Arc};
use std::time::Duration;

use eframe::egui;
use festerm_config::{AccountProvider, DevTunnelAccount, Profile};
use festerm_secret_store::{SecretBytes, SecretStore};
use festerm_tunnel_client::{
    identity::{self, DevicePrompt, Identity},
    service::{self, TunnelSummary},
    Error, Https,
};
use festerm_ui_egui::controls::{action_button, ActionButtonRole};
use festerm_ui_egui::theme;
use tokio_util::sync::CancellationToken;

use super::FesTermApp;
use crate::overlay_state::PendingSecret;

struct SignedAccount {
    account: DevTunnelAccount,
    secret: PendingSecret,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum AccountStatus {
    #[default]
    Unknown,
    VerifiedAtSignIn,
    LastOperationSucceeded,
    Expired,
    ApplicationChanged,
}

impl AccountStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown until checked",
            Self::VerifiedAtSignIn => "Verified at last sign-in",
            Self::LastOperationSucceeded => "Previously checked successfully",
            Self::Expired => "Sign-in expired. Sign in again.",
            Self::ApplicationChanged => "Sign-in application changed. Sign in again.",
        }
    }

    pub(crate) fn needs_sign_in(self) -> bool {
        matches!(self, Self::Expired | Self::ApplicationChanged)
    }
}

#[derive(Clone, Default)]
struct AccountObservations(Vec<(DevTunnelAccount, AccountStatus)>);

fn observations_id() -> egui::Id {
    egui::Id::new("dev_tunnel_account_observations")
}

/// Only call for a typed failure of decoding/refreshing the stored identity.
pub(super) fn credential_issue(error: Error) -> Option<AccountStatus> {
    match error {
        Error::Expired => Some(AccountStatus::Expired),
        Error::IdentityChanged => Some(AccountStatus::ApplicationChanged),
        _ => None,
    }
}

pub(super) fn record_account_status(
    context: &egui::Context,
    current: &[DevTunnelAccount],
    expected: &DevTunnelAccount,
    status: AccountStatus,
) {
    context.data_mut(|data| {
        let observations = data.get_temp_mut_or_default::<AccountObservations>(observations_id());
        observations
            .0
            .retain(|(account, _)| current.contains(account));
        if !current.contains(expected) {
            return;
        }
        if let Some((_, known)) = observations
            .0
            .iter_mut()
            .find(|(account, _)| account == expected)
        {
            *known = status;
        } else {
            observations.0.push((expected.clone(), status));
        }
    });
}

#[derive(Clone, Copy, Default, PartialEq)]
pub(super) enum DeviceAction {
    #[default]
    None,
    Copy,
    OpenBrowser,
}

/// Owned by one authorization generation, never by the render loop.
#[derive(Default)]
pub(super) struct DeviceActions {
    attempted: bool,
    copied: Option<bool>,
    opened: Option<bool>,
    // On X11 the clipboard owner must remain alive while authorization waits.
    clipboard: Option<arboard::Clipboard>,
}

impl DeviceActions {
    fn initial_with(&mut self, copy: impl FnOnce() -> bool, open: impl FnOnce() -> bool) {
        if self.attempted {
            return;
        }
        self.attempted = true;
        self.copied = Some(copy());
        self.opened = Some(open());
    }

    pub(super) fn initial(&mut self, code: &str) {
        let mut clipboard = None;
        self.initial_with(
            || match arboard::Clipboard::new() {
                Ok(mut native) => {
                    let copied = native.set_text(code).is_ok();
                    clipboard = Some(native);
                    copied
                }
                Err(_) => false,
            },
            || webbrowser::open(identity::VERIFICATION_URI).is_ok(),
        );
        if clipboard.is_some() {
            self.clipboard = clipboard;
        }
    }

    pub(super) fn handle(&mut self, action: DeviceAction, code: &str) {
        match action {
            DeviceAction::None => {}
            DeviceAction::Copy => {
                if self.clipboard.is_none() {
                    match arboard::Clipboard::new() {
                        Ok(clipboard) => self.clipboard = Some(clipboard),
                        Err(_) => {
                            self.copied = Some(false);
                            return;
                        }
                    }
                }
                self.copied = Some(
                    self.clipboard
                        .as_mut()
                        .is_some_and(|clipboard| clipboard.set_text(code).is_ok()),
                );
            }
            DeviceAction::OpenBrowser => {
                self.opened = Some(webbrowser::open(identity::VERIFICATION_URI).is_ok());
            }
        }
    }

    pub(super) fn show(&self, ui: &mut egui::Ui, code: &str) -> DeviceAction {
        ui.label("Microsoft Dev Tunnels");
        ui.monospace(code);
        match self.copied {
            Some(true) => ui.label("Code copied. Paste it into GitHub."),
            Some(false) => {
                ui.colored_label(theme::STATUS_ERROR, "Could not copy. Enter the code above.")
            }
            None => ui.label("Enter this code on GitHub:"),
        };
        match self.opened {
            Some(true) => ui.label("Browser opened."),
            Some(false) => {
                ui.colored_label(theme::STATUS_ERROR, "Could not open browser. Try again.")
            }
            None => ui.label(identity::VERIFICATION_URI),
        };
        let mut action = DeviceAction::None;
        ui.horizontal_wrapped(|ui| {
            if action_button(ui, ActionButtonRole::Secondary, "Copy again").clicked() {
                action = DeviceAction::Copy;
            }
            if action_button(ui, ActionButtonRole::Secondary, "Open browser").clicked() {
                action = DeviceAction::OpenBrowser;
            }
        });
        ui.label("Waiting for sign-in…");
        action
    }
}

enum Event {
    Device(DevicePrompt),
    /// A new sign-in finished and its credential is in the store.
    Signed(SignedAccount),
    Listed(Result<(String, Vec<TunnelSummary>), Error>),
    LocalRemoval(Result<festerm_config::LocalTunnelHosting, &'static str>),
    AccountObservation(AccountStatus),
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
    device: DeviceActions,
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
        cleanup_failure: Arc<AtomicU16>,
        context: &egui::Context,
        delete: Option<String>,
        rename: Option<(String, String)>,
        reauthenticate: bool,
    ) -> Self {
        let cancel = CancellationToken::new();
        let (sender, receiver) = mpsc::sync_channel(2);
        let mut browser = Self {
            cancel: cancel.clone(),
            receiver,
            stage: Stage::Working,
            focus_cancel: true,
            device: DeviceActions::default(),
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
                                value = run(account, &store, client_id, &cancel, &notify, delete.as_deref(), rename.as_ref(), Some((cleanup_failure, context.clone())), reauthenticate) => value,
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
        None,
        false,
    )
    .await?
    {
        Outcome::Listed(_, tunnels) => Ok(tunnels),
        Outcome::Signed(_) => Err(Error::Response),
    }
}

enum Outcome {
    Listed(String, Vec<TunnelSummary>),
    Signed(SignedAccount),
}

#[allow(clippy::too_many_arguments)]
async fn run(
    account: Option<DevTunnelAccount>,
    store: &Arc<dyn SecretStore>,
    client_id: &str,
    cancel: &CancellationToken,
    notify: &impl Fn(Event) -> bool,
    delete: Option<&str>,
    rename: Option<&(String, String)>,
    cleanup: Option<(Arc<AtomicU16>, egui::Context)>,
    reauthenticate: bool,
) -> Result<Outcome, Error> {
    let http = Https::new()?;
    let expected = if reauthenticate {
        Some(account.as_ref().ok_or(Error::AccountChanged)?.account_id())
    } else {
        None
    };
    let account = if reauthenticate { None } else { account };
    let Some(account) = account else {
        let (cleanup_failure, context) = cleanup.ok_or(Error::Store)?;
        let identity = identity::sign_in(&http, client_id, expected, cancel, |prompt| {
            if !notify(Event::Device(prompt)) {
                cancel.cancel();
            }
        })
        .await?;
        let reference = store
            .put(&SecretBytes::copy_from_slice(&identity.encode_private()?))
            .map_err(|_| Error::Store)?;
        let secret = PendingSecret::new(
            reference.duplicate_for_transport(),
            Arc::clone(store),
            cleanup_failure,
            crate::configuration_startup::SecretCleanupNotice::BrowserPending,
            context,
        );
        return DevTunnelAccount::new(
            format!("github-{}", identity.account.id),
            AccountProvider::Github,
            identity.account.id,
            identity.account.login.clone(),
            reference,
        )
        .map(|account| Outcome::Signed(SignedAccount { account, secret }))
        .map_err(|_| Error::Response);
    };
    let secret = store
        .get(account.credential_reference())
        .map_err(|_| Error::Store)?;
    let observe = |error: &Error| {
        if let Some(status) = credential_issue(*error) {
            notify(Event::AccountObservation(status));
        }
    };
    let mut identity = secret
        .with_bytes(|bytes| Identity::decode_private(bytes, account.account_id(), client_id))
        .inspect_err(observe)?;
    let refreshed = identity.needs_sign_in();
    if refreshed {
        identity = identity
            .refresh(&http, client_id, cancel)
            .await
            .inspect_err(observe)?;
        store
            .update(
                account.credential_reference(),
                &SecretBytes::copy_from_slice(&identity.encode_private()?),
            )
            .map_err(|_| Error::Store)?;
        notify(Event::AccountObservation(
            AccountStatus::LastOperationSucceeded,
        ));
    }
    if let Some(tunnel) = delete {
        service::delete(&identity, tunnel, cancel).await?;
    }
    if let Some((tunnel, friendly_name)) = rename {
        service::update_description(&identity, tunnel, friendly_name, cancel).await?;
    }
    let tunnels = service::list(&identity, cancel).await?;
    if !refreshed {
        notify(Event::AccountObservation(
            AccountStatus::LastOperationSucceeded,
        ));
    }
    Ok(Outcome::Listed(identity.account.login.clone(), tunnels))
}

/// The name a tunnel is listed under.
pub(crate) fn display_name(summary: &TunnelSummary) -> String {
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
        self.tunnel_browser = Some(Browser::start(
            account,
            store,
            self.configuration_reloader.secret_cleanup_failure(),
            context,
            None,
            None,
            false,
        ));
        self.overlays.native_tunnel = true;
        context.request_repaint();
    }

    pub(super) fn poll_tunnel_browser(&mut self, context: &egui::Context) {
        let Some(browser) = self.tunnel_browser.as_mut() else {
            return;
        };
        if browser.account.as_ref().is_some_and(|expected| {
            self.state.configuration().account(expected.identifier()) != Some(expected)
        }) {
            self.tunnel_browser = None;
            self.overlays.native_tunnel = false;
            self.secure_storage_feedback = Some("Account changed. Sign-in cancelled.");
            return;
        }
        let mut signed = None;
        let mut local_removal = None;
        while let Ok(event) = browser.receiver.try_recv() {
            browser.stage = match event {
                Event::Device(prompt) => {
                    browser.device.initial(&prompt.user_code);
                    Stage::Device(prompt)
                }
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
                Event::AccountObservation(status) => {
                    if let Some(account) = &browser.account {
                        record_account_status(
                            context,
                            self.state.configuration().accounts(),
                            account,
                            status,
                        );
                    }
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
        if let Some(signed) = signed {
            let store = self
                .tunnel_browser
                .take()
                .map(|browser| Arc::clone(&browser.store));
            self.overlays.native_tunnel = false;
            let SignedAccount { account, secret } = signed;
            let _reference = secret.release();
            self.save_signed_in_account(account, store, context);
        }
    }

    pub(super) fn reauthenticate_dev_tunnel_account(&mut self, key: &str, context: &egui::Context) {
        if self.tunnel_browser.is_some() || self.native_tunnel.is_some() {
            return;
        }
        let Some(account) = self.state.configuration().account(key).cloned() else {
            self.secure_storage_feedback = Some("Account changed. Sign in again.");
            return;
        };
        let Ok(store) = &self.secret_store else {
            self.secure_storage_feedback = Some("Secure storage unavailable. Cannot sign in.");
            return;
        };
        self.tunnel_browser = Some(Browser::start(
            Some(account),
            Arc::clone(store),
            self.configuration_reloader.secret_cleanup_failure(),
            context,
            None,
            None,
            true,
        ));
        self.overlays.native_tunnel = true;
        context.request_repaint();
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
        let unknown = self.configuration_status.save_outcome_unknown();
        let blocked = self.configuration_status.save_reconciliation_required() && !unknown
            || self.configuration_status
                == super::ConfigurationStartupStatus::ProfileSaveFailure(
                    crate::configuration_startup::ConfigurationLoadFailure::SaveBusy,
                );
        if unknown {
            self.report_unknown_configuration_save(true);
        }
        let discarded = if unknown || blocked {
            None
        } else if saved {
            previous
        } else {
            Some(new_reference)
        };
        let cleanup_failed = discarded.is_some_and(|reference| {
            store
                .as_ref()
                .is_none_or(|store| store.delete(&reference).is_err())
        });
        if blocked {
            self.publish_shared_notice(
                crate::configuration_startup::SecretCleanupNotice::BrowserRetained,
            );
        }
        if let Some(key) = key.filter(|_| saved) {
            if let Some(account) = self.state.configuration().account(&key) {
                record_account_status(
                    context,
                    self.state.configuration().accounts(),
                    account,
                    AccountStatus::VerifiedAtSignIn,
                );
            }
            context.data_mut(|data| {
                data.insert_temp(signed_in_account_id(), SignedInAccount(key));
            });
            context.request_repaint();
        }
        if cleanup_failed {
            self.report_secret_cleanup_failure(
                if saved {
                    crate::configuration_startup::SecretCleanupNotice::BrowserPrevious
                } else {
                    crate::configuration_startup::SecretCleanupNotice::BrowserSaveFailed
                },
                context,
            );
        }
        self.secure_storage_feedback = Some(if unknown {
            crate::configuration_startup::UNKNOWN_RESOURCE_SAVE_MESSAGE
        } else if blocked {
            self.secure_storage_feedback
                .expect("blocked save was reported")
        } else if cleanup_failed && saved {
            "The account was saved, but the previous native credential could not be removed. Reconcile secure storage manually."
        } else if cleanup_failed {
            "The account could not be saved, and the new native credential could not be removed. Reconcile secure storage manually."
        } else if saved {
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

    /// Public metadata and credential-snapshot-bound observations; no vault or network reads.
    pub(super) fn dev_tunnel_account_rows(
        &self,
        context: &egui::Context,
    ) -> Vec<(String, String, usize, AccountStatus)> {
        let configuration = self.state.configuration();
        context.data_mut(|data| {
            let observations =
                data.get_temp_mut_or_default::<AccountObservations>(observations_id());
            observations
                .0
                .retain(|(account, _)| configuration.accounts().contains(account));
            configuration
                .accounts()
                .iter()
                .map(|account| {
                    let status = observations
                        .0
                        .iter()
                        .find(|(known, _)| known == account)
                        .map_or(AccountStatus::Unknown, |(_, status)| *status);
                    (
                        account.identifier().to_owned(),
                        account.login().to_owned(),
                        configuration.account_profile_count(account.identifier()),
                        status,
                    )
                })
                .collect()
        })
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
        let mut device_action = DeviceAction::None;
        let mut reauthenticate = false;
        egui::Modal::new(egui::Id::new("browse_dev_tunnels")).show(context, |ui| {
            super::confirmations::prepare_confirmation(ui, context, 520.0);
            ui.heading(
                if browser.account.is_none() || matches!(browser.stage, Stage::Device(_)) {
                    "Sign in with GitHub"
                } else {
                    "Dev Tunnels"
                },
            );
            super::confirmations::confirmation_body(ui, |ui| match &mut browser.stage {
                Stage::Working => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Working…");
                    });
                }
                Stage::Device(prompt) => {
                    device_action = browser.device.show(ui, &prompt.user_code);
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
                    if action_button(ui, ActionButtonRole::Accent, "Sign in again").clicked() {
                        reauthenticate = true;
                    }
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
        if !dismiss {
            if let Stage::Device(prompt) = &browser.stage {
                browser.device.handle(device_action, &prompt.user_code);
            }
        }
        if reauthenticate && !dismiss {
            let key = browser
                .account
                .as_ref()
                .map(|account| account.identifier().to_owned());
            self.tunnel_browser = None;
            if let Some(key) = key {
                self.reauthenticate_dev_tunnel_account(&key, context);
            } else {
                self.begin_tunnel_browser(None, context);
            }
            return;
        }
        if let Some(locator) = delete {
            let account = self.tunnel_browser.as_ref().and_then(|b| b.account.clone());
            let store = self.tunnel_browser.as_ref().map(|b| Arc::clone(&b.store));
            if let (Some(account), Some(store)) = (account, store) {
                // Drop the old browser first so its account lease is released.
                self.tunnel_browser = None;
                self.tunnel_browser = Some(Browser::start(
                    Some(account),
                    store,
                    self.configuration_reloader.secret_cleanup_failure(),
                    context,
                    Some(locator),
                    None,
                    false,
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
                    self.configuration_reloader.secret_cleanup_failure(),
                    context,
                    None,
                    Some(rename),
                    false,
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
    fn account_status_is_observational_and_fenced_by_the_exact_credential_snapshot() {
        let account = DevTunnelAccount::new(
            "account",
            AccountProvider::Github,
            42,
            "user",
            festerm_secret_store::SecretReference::generate(),
        )
        .unwrap();
        let configuration = festerm_config::Configuration::empty()
            .with_account(account.clone())
            .unwrap()
            .0;
        let mut app = FesTermApp::for_test_with_configuration(configuration);
        let context = egui::Context::default();
        assert_eq!(
            app.dev_tunnel_account_rows(&context)[0].3,
            AccountStatus::Unknown
        );
        record_account_status(
            &context,
            app.state.configuration().accounts(),
            &account,
            AccountStatus::VerifiedAtSignIn,
        );
        assert_eq!(
            app.dev_tunnel_account_rows(&context)[0].3,
            AccountStatus::VerifiedAtSignIn
        );

        let replacement = DevTunnelAccount::new(
            "account",
            AccountProvider::Github,
            42,
            "user",
            festerm_secret_store::SecretReference::generate(),
        )
        .unwrap();
        app.state.replace_configuration(
            festerm_config::Configuration::empty()
                .with_account(replacement.clone())
                .unwrap()
                .0,
        );
        assert_eq!(
            app.dev_tunnel_account_rows(&context)[0].3,
            AccountStatus::Unknown
        );
        record_account_status(
            &context,
            app.state.configuration().accounts(),
            &account,
            AccountStatus::Expired,
        );
        assert_eq!(
            app.dev_tunnel_account_rows(&context)[0].3,
            AccountStatus::Unknown
        );
        record_account_status(
            &context,
            app.state.configuration().accounts(),
            &replacement,
            AccountStatus::ApplicationChanged,
        );
        assert_eq!(
            app.dev_tunnel_account_rows(&context)[0].3,
            AccountStatus::ApplicationChanged
        );
        record_account_status(
            &context,
            app.state.configuration().accounts(),
            &replacement,
            AccountStatus::LastOperationSucceeded,
        );
        assert_eq!(
            app.dev_tunnel_account_rows(&context)[0].3,
            AccountStatus::LastOperationSucceeded
        );
        app.state
            .replace_configuration(festerm_config::Configuration::empty());
        assert!(app.dev_tunnel_account_rows(&context).is_empty());
        assert_eq!(
            context.data_mut(|data| {
                data.get_temp::<AccountObservations>(observations_id())
                    .unwrap()
                    .0
                    .len()
            }),
            0
        );
    }

    #[test]
    fn account_credential_status_does_not_infer_expiry_from_generic_failures() {
        assert_eq!(
            credential_issue(Error::Expired),
            Some(AccountStatus::Expired)
        );
        assert_eq!(
            credential_issue(Error::IdentityChanged),
            Some(AccountStatus::ApplicationChanged)
        );
        for error in [
            Error::Network,
            Error::Denied,
            Error::Response,
            Error::Cancelled,
            Error::AccountChanged,
        ] {
            assert_eq!(credential_issue(error), None);
        }
    }

    #[test]
    fn account_sign_in_attempts_once_and_reports_copy_and_browser_independently() {
        for (copied, opened) in [(true, true), (true, false), (false, true), (false, false)] {
            let mut effects = DeviceActions::default();
            effects.initial_with(|| copied, || opened);
            effects.initial_with(
                || panic!("duplicate clipboard attempt"),
                || panic!("duplicate browser attempt"),
            );
            assert_eq!(effects.copied, Some(copied));
            assert_eq!(effects.opened, Some(opened));
        }
    }

    #[test]
    fn account_change_discards_queued_sign_in_prompt_before_platform_effects() {
        let context = egui::Context::default();
        let store: Arc<dyn SecretStore> = Arc::new(festerm_secret_store::MemorySecretStore::new());
        let reference = store
            .put(&SecretBytes::copy_from_slice(b"identity"))
            .unwrap();
        let account =
            DevTunnelAccount::new("account", AccountProvider::Github, 42, "user", reference)
                .unwrap();
        let mut app = FesTermApp::for_test_with_configuration(
            festerm_config::Configuration::new(Vec::new()).unwrap(),
        );
        let (sender, receiver) = mpsc::sync_channel(2);
        sender
            .try_send(Event::Device(DevicePrompt {
                user_code: "ABCD-1234".into(),
            }))
            .ok()
            .unwrap();
        let cancel = CancellationToken::new();
        app.tunnel_browser = Some(Browser {
            cancel: cancel.clone(),
            receiver,
            stage: Stage::Working,
            focus_cancel: false,
            device: DeviceActions::default(),
            store,
            account: Some(account),
            _lease: None,
        });
        app.overlays.native_tunnel = true;
        app.poll_tunnel_browser(&context);
        assert!(cancel.is_cancelled());
        assert!(app.tunnel_browser.is_none());
        assert!(!app.overlays.native_tunnel);
        assert_eq!(
            app.secure_storage_feedback,
            Some("Account changed. Sign-in cancelled.")
        );
    }

    struct FailingDeleteStore(festerm_secret_store::MemorySecretStore);
    impl SecretStore for FailingDeleteStore {
        fn put(
            &self,
            secret: &SecretBytes,
        ) -> Result<festerm_secret_store::SecretReference, festerm_secret_store::SecretStoreError>
        {
            self.0.put(secret)
        }
        fn get(
            &self,
            reference: &festerm_secret_store::SecretReference,
        ) -> Result<SecretBytes, festerm_secret_store::SecretStoreError> {
            self.0.get(reference)
        }
        fn update(
            &self,
            reference: &festerm_secret_store::SecretReference,
            secret: &SecretBytes,
        ) -> Result<(), festerm_secret_store::SecretStoreError> {
            self.0.update(reference, secret)
        }
        fn delete(
            &self,
            _: &festerm_secret_store::SecretReference,
        ) -> Result<bool, festerm_secret_store::SecretStoreError> {
            Err(festerm_secret_store::SecretStoreError::BackendFailure)
        }
    }

    #[test]
    fn browser_cancelled_queued_and_late_signin_reports_cleanup_failure() {
        for late in [false, true] {
            let context = egui::Context::default();
            let mut app =
                FesTermApp::for_test_with_configuration(festerm_config::Configuration::empty());
            let store: Arc<dyn SecretStore> = Arc::new(FailingDeleteStore(
                festerm_secret_store::MemorySecretStore::new(),
            ));
            let reference = store
                .put(&SecretBytes::copy_from_slice(b"fixture"))
                .unwrap();
            let account = DevTunnelAccount::new(
                "account",
                AccountProvider::Github,
                42,
                "user",
                reference.duplicate_for_transport(),
            )
            .unwrap();
            let secret = PendingSecret::new(
                reference.duplicate_for_transport(),
                Arc::clone(&store),
                app.configuration_reloader.secret_cleanup_failure(),
                crate::configuration_startup::SecretCleanupNotice::BrowserPending,
                context.clone(),
            );
            let event = Event::Signed(SignedAccount { account, secret });
            let (sender, receiver) = mpsc::sync_channel(1);
            let browser = Browser {
                cancel: CancellationToken::new(),
                receiver,
                stage: Stage::Working,
                focus_cancel: false,
                device: DeviceActions::default(),
                store: Arc::clone(&store),
                account: None,
                _lease: None,
            };
            if late {
                drop(browser);
                assert!(sender.try_send(event).is_err());
            } else {
                assert!(sender.try_send(event).is_ok());
                drop(browser);
            }
            app.frame_logic(&context);
            assert!(store.get(&reference).is_ok());
            assert!(app
                .overlays
                .configuration_notice
                .as_ref()
                .unwrap()
                .0
                .contains("could not be removed"));
        }
    }

    #[test]
    fn blocked_account_save_retains_a_shared_reference() {
        use crate::configuration_startup::{
            ConfigurationLoadFailure, ConfigurationReloader, SessionNameConfigurationFixture,
        };
        use festerm_config::Configuration;
        use festerm_secret_store::MemorySecretStore;
        for busy in [false, true] {
            let fixture = SessionNameConfigurationFixture::new();
            let store = Arc::new(MemorySecretStore::new());
            let shared = store
                .put(&SecretBytes::copy_from_slice(b"fixture"))
                .unwrap();
            let account = DevTunnelAccount::new(
                "account",
                AccountProvider::Github,
                42,
                "user",
                shared.duplicate_for_transport(),
            )
            .unwrap();
            let configuration = Configuration::empty()
                .with_account(account.clone())
                .unwrap()
                .0;
            configuration.save_to_path(&fixture.path).unwrap();
            let reloader = ConfigurationReloader::from_path_for_test(fixture.path.clone());
            let reloader = if busy {
                reloader.with_save_failure_for_test(ConfigurationLoadFailure::SaveBusy)
            } else {
                let first = reloader
                    .clone()
                    .with_save_failure_for_test(ConfigurationLoadFailure::OutcomeUnknown);
                assert_eq!(
                    first.save_configuration(&configuration),
                    Err(ConfigurationLoadFailure::OutcomeUnknown)
                );
                reloader
            };
            let context = egui::Context::default();
            let mut primary = FesTermApp::for_test_with_configuration(configuration.clone());
            primary.configuration_reloader = reloader;
            let (config, status, reloader, secret_store, documents) =
                primary.shared_application_services();
            let mut app = FesTermApp::secondary_window(
                &context,
                config,
                status,
                reloader,
                secret_store,
                documents,
            );
            app.save_signed_in_account(account, Some(store.clone()), &context);
            assert!(store.get(&shared).is_ok());
            assert_eq!(
                Configuration::load_from_path(&fixture.path).unwrap(),
                configuration
            );
            assert!(app
                .overlays
                .configuration_notice
                .as_ref()
                .unwrap()
                .0
                .contains("retained"));
            drop(app);
            primary.frame_logic(&context);
            assert!(primary
                .overlays
                .configuration_notice
                .as_ref()
                .unwrap()
                .0
                .contains("candidate Dev Tunnel account credential"));
        }
    }

    #[test]
    fn persistence_outcome_account_save_retains_both_secrets_only_when_unknown() {
        use crate::configuration_startup::{ConfigurationLoadFailure, ConfigurationReloader};
        use festerm_config::Configuration;
        use festerm_secret_store::MemorySecretStore;

        for failure in [
            ConfigurationLoadFailure::Unreadable,
            ConfigurationLoadFailure::OutcomeUnknown,
        ] {
            let directory = tempfile::Builder::new()
                .prefix("account-outcome-")
                .tempdir_in(std::env::current_dir().unwrap())
                .unwrap();
            let path = directory.path().join("config.toml");
            let store = Arc::new(MemorySecretStore::new());
            let old = store
                .put(&SecretBytes::copy_from_slice(b"old fixture"))
                .unwrap();
            let new = store
                .put(&SecretBytes::copy_from_slice(b"new fixture"))
                .unwrap();
            let account = |reference| {
                DevTunnelAccount::new("account", AccountProvider::Github, 42, "user", reference)
                    .unwrap()
            };
            let config = Configuration::empty()
                .with_account(account(old.duplicate_for_transport()))
                .unwrap()
                .0;
            config.save_to_path(&path).unwrap();
            let mut app = FesTermApp::for_test_with_configuration(config.clone());
            app.configuration_reloader = ConfigurationReloader::from_path_for_test(path.clone())
                .with_save_failure_for_test(failure);
            app.save_signed_in_account(
                account(new.duplicate_for_transport()),
                Some(store.clone()),
                &egui::Context::default(),
            );
            assert!(store.get(&old).is_ok());
            assert_eq!(app.state.configuration(), &config);
            assert!(!app.configuration_status.was_saved());
            if failure == ConfigurationLoadFailure::OutcomeUnknown {
                assert!(store.get(&new).is_ok());
                assert!(
                    Configuration::load_from_path(&path)
                        .unwrap()
                        .account("account")
                        .unwrap()
                        .credential_reference()
                        == &new
                );
                assert_eq!(
                    app.secure_storage_feedback,
                    Some(crate::configuration_startup::UNKNOWN_RESOURCE_SAVE_MESSAGE)
                );
                assert!(app.overlays.configuration_notice.is_some());
            } else {
                assert!(store.get(&new).is_err());
                assert_eq!(Configuration::load_from_path(&path).unwrap(), config);
            }
        }
    }

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
