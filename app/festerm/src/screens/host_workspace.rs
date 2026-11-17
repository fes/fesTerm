//! Offline recipe choices and account-scoped, request-bound host discovery.

use super::*;
use crate::app::{Reauthentication, ReauthenticationOrigin, RequestFailure};
use festerm_config::host_recipes::{AccountReference, HostHint, HostReference, PublicRecipe};
use festerm_tunnel_client::service::TunnelSummary;
use festerm_ui_egui::controls::{action_button, ActionButtonRole};

#[derive(Clone, Debug)]
pub(crate) struct HostChoice {
    pub account: String,
    pub name: String,
    pub locator: String,
    pub reference: Option<HostReference>,
}

#[derive(Clone, Debug)]
pub(crate) struct HostSessions {
    pub reference: HostReference,
    pub hint: HostHint,
    pub sessions: Vec<SavedSessionChoice>,
}

#[derive(Clone, Debug)]
pub(crate) struct SavedSessionChoice {
    pub target: festerm_tunnel_bridge::host_client::Target,
    pub name: String,
    pub lifetime: festerm_config::host_recipes::Lifetime,
}
pub(crate) fn remembered_hosts(context: &egui::Context) -> Vec<HostSessions> {
    context.data(|data| {
        data.get_temp(egui::Id::new("host_session_choices"))
            .unwrap_or_default()
    })
}

pub(crate) fn remember_host(context: &egui::Context, host: HostSessions) {
    let mut hosts = remembered_hosts(context);
    hosts.retain(|previous| previous.reference != host.reference);
    if hosts.len() >= 32 {
        hosts.remove(0);
    }
    hosts.push(host);
    context.data_mut(|data| data.insert_temp(egui::Id::new("host_session_choices"), hosts));
}

#[derive(Clone, Default)]
struct AccountResults {
    account: String,
    verified_account: Option<festerm_config::DevTunnelAccount>,
    automatic_discovery_started: bool,
    request: Option<egui::Id>,
    /// Hosts from the last successful read; kept when a later refresh fails.
    hosts: Option<Vec<HostChoice>>,
    error: Option<String>,
    reauthentication: Option<Reauthentication>,
    queued: bool,
}

const DISCOVERY_CACHE: &str = "host_discovery_cache";

fn cached_discovery(context: &egui::Context, account: &str) -> Option<Vec<HostChoice>> {
    context.data(|data| {
        data.get_temp::<Vec<(String, Vec<HostChoice>)>>(egui::Id::new(DISCOVERY_CACHE))
            .and_then(|cache| {
                cache
                    .into_iter()
                    .find(|(cached, _)| cached == account)
                    .map(|(_, hosts)| hosts)
            })
    })
}

fn remember_discovery(data: &mut egui::util::IdTypeMap, account: &str, hosts: &[HostChoice]) {
    let id = egui::Id::new(DISCOVERY_CACHE);
    let mut cache: Vec<(String, Vec<HostChoice>)> = data.get_temp(id).unwrap_or_default();
    cache.retain(|(cached, _)| cached != account);
    if cache.len() >= 32 {
        cache.remove(0);
    }
    cache.push((account.to_owned(), hosts.to_vec()));
    data.insert_temp(id, cache);
}

#[derive(Clone, Default)]
struct State {
    accounts: Vec<AccountResults>,
    request: Option<egui::Id>,
    feedback: Option<String>,
    for_profile: bool,
    reauthentication_generation: Option<egui::Id>,
    selected_reference: Option<HostReference>,
    automatic_open: bool,
}

fn state_id(tab: TabId) -> egui::Id {
    egui::Id::new(("host_workspace", tab))
}

pub(crate) fn set_connection_label(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
    label: String,
) {
    context.data_mut(|data| {
        data.insert_temp(
            egui::Id::new(("host_connection_label", tab)),
            (request, label),
        )
    });
}

pub(crate) fn connection_label(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
) -> Option<String> {
    context
        .data(|data| {
            data.get_temp::<(egui::Id, String)>(egui::Id::new(("host_connection_label", tab)))
        })
        .and_then(|(current, label)| (current == request).then_some(label))
}

pub(crate) fn cancel(context: &egui::Context, tab: TabId) {
    context.data_mut(|data| {
        let Some(mut state) = data.get_temp::<State>(state_id(tab)) else {
            return;
        };
        state.request = None;
        state.reauthentication_generation = None;
        state.automatic_open = false;
        for account in &mut state.accounts {
            account.request = None;
            account.reauthentication = None;
            account.queued = false;
            account.automatic_discovery_started = false;
        }
        data.insert_temp(state_id(tab), state);
    });
}

pub(crate) fn request_is_current(context: &egui::Context, tab: TabId, request: egui::Id) -> bool {
    context.data(|data| {
        data.get_temp::<State>(state_id(tab))
            .is_some_and(|state| state.request == Some(request))
    })
}

pub(crate) fn profile_request_is_current(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
) -> bool {
    context.data(|data| {
        data.get_temp::<State>(state_id(tab))
            .is_some_and(|state| state.for_profile && state.request == Some(request))
    })
}

pub(crate) fn discovery_is_current(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
    account: &str,
) -> bool {
    context.data(|data| {
        data.get_temp::<State>(state_id(tab)).is_some_and(|state| {
            state
                .accounts
                .iter()
                .any(|row| row.account == account && row.request == Some(request))
        })
    })
}

pub(crate) fn report_discovery(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
    account: &str,
    result: Result<Vec<TunnelSummary>, String>,
) {
    complete_discovery(context, tab, request, account, result, None);
}

pub(crate) fn report_typed_discovery(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
    account: &festerm_config::DevTunnelAccount,
    result: Result<Vec<TunnelSummary>, RequestFailure>,
) {
    let recovery = result
        .as_ref()
        .err()
        .filter(|error| error.needs_sign_in)
        .map(|_| Reauthentication {
            origin: ReauthenticationOrigin::Discovery { tab, request },
            account: account.clone(),
        });
    complete_discovery(
        context,
        tab,
        request,
        account.identifier(),
        result.map_err(|error| error.message),
        recovery,
    );
}

fn complete_discovery(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
    account: &str,
    result: Result<Vec<TunnelSummary>, String>,
    recovery: Option<Reauthentication>,
) {
    context.data_mut(|data| {
        let Some(mut state) = data.get_temp::<State>(state_id(tab)) else {
            return;
        };
        let Some(row) = state
            .accounts
            .iter_mut()
            .find(|row| row.account == account && row.request == Some(request))
        else {
            return;
        };
        row.request = None;
        row.reauthentication =
            recovery.filter(|_| state.reauthentication_generation == Some(request));
        match result {
            Ok(hosts) => {
                row.hosts = Some(
                    hosts
                        .into_iter()
                        .map(|host| HostChoice {
                            account: account.to_owned(),
                            name: crate::app::tunnel_browser::display_name(&host),
                            locator: host.locator,
                            reference: None,
                        })
                        .collect(),
                );
                row.error = None;
                row.reauthentication = None;
                let hosts = row.hosts.clone().unwrap_or_default();
                remember_discovery(data, account, &hosts);
            }
            Err(error) => row.error = Some(error),
        }
        data.insert_temp(state_id(tab), state);
    });
    context.request_repaint();
}

pub(crate) fn report_outcome(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
    message: &str,
) {
    context.data_mut(|data| {
        let Some(mut state) = data.get_temp::<State>(state_id(tab)) else {
            return;
        };
        if state.request != Some(request) {
            return;
        }
        state.request = None;
        state.reauthentication_generation = None;
        state.feedback = Some(message.to_owned());
        data.insert_temp(state_id(tab), state);
    });
    context.request_repaint();
}

#[cfg(test)]
pub(crate) fn install_browser_request_for_test(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
) {
    context.data_mut(|data| {
        data.insert_temp(
            state_id(tab),
            State {
                request: Some(request),
                ..Default::default()
            },
        )
    });
}

#[cfg(test)]
pub(crate) fn install_profile_request_for_test(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
) {
    context.data_mut(|data| {
        data.insert_temp(
            state_id(tab),
            State {
                request: Some(request),
                for_profile: true,
                ..Default::default()
            },
        )
    });
}

#[cfg(test)]
pub(crate) fn install_gallery_state(context: &egui::Context, tab: TabId, unavailable: bool) {
    let account = "github-fixture".to_owned();
    context.data_mut(|data| {
        data.insert_temp(
            state_id(tab),
            State {
                accounts: vec![AccountResults {
                    account: account.clone(),
                    verified_account: None,
                    automatic_discovery_started: true,
                    request: None,
                    hosts: Some(vec![HostChoice {
                        account,
                        name: "Build workstation".into(),
                        locator: "fixture.use".into(),
                        reference: None,
                    }]),
                    reauthentication: None,
                    error: unavailable.then(|| "Host unavailable. Try again.".to_owned()),
                    queued: false,
                }],
                automatic_open: true,
                ..Default::default()
            },
        )
    });
}

/// Synthetic two-account picker results for the screenshot gallery; no
/// discovery request is issued and spinners never resolve.
#[cfg(test)]
pub(crate) fn install_profile_picker_gallery(context: &egui::Context, tab: TabId, scene: &str) {
    let row = |account: &str, hosts: &[(&str, &str)], refreshing: bool, error: Option<&str>| {
        AccountResults {
            account: account.to_owned(),
            verified_account: None,
            automatic_discovery_started: true,
            request: refreshing.then(|| egui::Id::new(("gallery-refresh", account))),
            hosts: Some(
                hosts
                    .iter()
                    .map(|(locator, name)| HostChoice {
                        account: account.to_owned(),
                        name: (*name).to_owned(),
                        locator: (*locator).to_owned(),
                        reference: None,
                    })
                    .collect(),
            ),
            reauthentication: None,
            error: error.map(str::to_owned),
            queued: false,
        }
    };
    let refreshing = scene == "picker-refreshing";
    let accounts = vec![
        row(
            "alice-key",
            &[
                ("alice-build.use", "Build workstation"),
                ("alice-lab.use", "Lab Mac mini"),
            ],
            refreshing,
            (scene == "picker-stale").then_some("Host discovery timed out. Try again."),
        ),
        row(
            "bob-key",
            &[("bob-linux.use", "Linux workstation")],
            refreshing,
            None,
        ),
    ];
    context.data_mut(|data| {
        data.insert_temp(
            state_id(tab),
            State {
                accounts,
                for_profile: true,
                ..Default::default()
            },
        )
    });
}

fn saved_host(recipe: &PublicRecipe, configuration: &Configuration) -> Option<HostChoice> {
    let hint = recipe.host_hint.as_ref()?;
    let festerm_config::host_recipes::AccountReference::Github { account_id } = recipe.host.account;
    let account = configuration
        .accounts()
        .iter()
        .find(|account| account.account_id() == account_id)?;
    Some(HostChoice {
        account: account.identifier().to_owned(),
        name: hint.name.clone(),
        locator: hint.locator.clone(),
        reference: Some(recipe.host.clone()),
    })
}

pub(crate) fn show(ui: &mut Ui, tab: TabId, configuration: &Configuration) -> Option<AppCommand> {
    show_mode(ui, tab, configuration, false)
}

/// What the profile editor's host picker asks of its draft.
pub(crate) enum ProfileHostPick {
    /// The host's session inventory is already known; bind it offline.
    Bind(HostSessions),
    /// Discovery or a foreground host review (which owns trust decisions).
    Command(Box<AppCommand>),
}

fn sync_accounts(state: &mut State, configuration: &Configuration, context: &egui::Context) {
    state
        .accounts
        .retain(|row| configuration.account(&row.account).is_some());
    for account in configuration.accounts() {
        let row = if let Some(index) = state
            .accounts
            .iter()
            .position(|row| row.account == account.identifier())
        {
            &mut state.accounts[index]
        } else {
            state.accounts.push(AccountResults {
                account: account.identifier().to_owned(),
                ..Default::default()
            });
            state.accounts.last_mut().expect("inserted account")
        };
        if row
            .verified_account
            .as_ref()
            .is_some_and(|previous| previous != account)
        {
            // An identifier is not an identity: a replaced credential/account invalidates
            // both ready results and the outstanding worker's request witness.
            *row = AccountResults {
                account: account.identifier().to_owned(),
                ..Default::default()
            };
            state.request = None;
            state.reauthentication_generation = None;
            context.data_mut(|data| {
                let id = egui::Id::new(DISCOVERY_CACHE);
                let mut cache: Vec<(String, Vec<HostChoice>)> =
                    data.get_temp(id).unwrap_or_default();
                cache.retain(|(key, _)| key != account.identifier());
                data.insert_temp(id, cache);
            });
        }
        row.verified_account = Some(account.clone());
    }
}

fn begin_discovery(row: &mut AccountResults, tab: TabId) -> AppCommand {
    let request = egui::Id::new(uuid::Uuid::new_v4());
    row.request = Some(request);
    row.automatic_discovery_started = true;
    row.reauthentication = None;
    row.error = None;
    AppCommand::FindDevTunnelHosts {
        tab_id: tab,
        request,
        account: row.account.clone(),
    }
}

fn automatic_discovery(state: &mut State, tab: TabId, foreground: bool) -> Option<AppCommand> {
    if !foreground || state.for_profile {
        return None;
    }
    let row = state
        .accounts
        .iter_mut()
        .find(|row| !row.automatic_discovery_started && row.request.is_none() && !row.queued)?;
    let command = begin_discovery(row, tab);
    state.reauthentication_generation = row.request;
    Some(command)
}

struct PickerEntry {
    choice: HostChoice,
    remembered: Option<HostSessions>,
}

fn picker_entries(
    row: &AccountResults,
    account: &festerm_config::DevTunnelAccount,
    remembered: &[HostSessions],
    configuration: &Configuration,
) -> Vec<PickerEntry> {
    let account_id = account.account_id();
    let remembered_for = |locator: &str| {
        remembered.iter().find(|host| {
            host.hint.locator == locator
                && matches!(host.reference.account, AccountReference::Github { account_id: id } if id == account_id)
        })
    };
    let mut entries: Vec<PickerEntry> = Vec::new();
    for host in row.hosts.iter().flatten() {
        let remembered = remembered_for(&host.locator).cloned();
        entries.push(PickerEntry {
            choice: HostChoice {
                reference: remembered.as_ref().map(|host| host.reference.clone()),
                ..host.clone()
            },
            remembered,
        });
    }
    for host in remembered {
        if matches!(host.reference.account, AccountReference::Github { account_id: id } if id == account_id)
            && !entries
                .iter()
                .any(|entry| entry.choice.locator == host.hint.locator)
        {
            entries.push(PickerEntry {
                choice: HostChoice {
                    account: account.identifier().to_owned(),
                    name: host.hint.name.clone(),
                    locator: host.hint.locator.clone(),
                    reference: Some(host.reference.clone()),
                },
                remembered: Some(host.clone()),
            });
        }
    }
    for recipe in configuration.host_recipes().recipes() {
        if let Some(host) = saved_host(recipe, configuration) {
            if host.account == account.identifier()
                && !entries
                    .iter()
                    .any(|entry| entry.choice.locator == host.locator)
            {
                entries.push(PickerEntry {
                    choice: host,
                    remembered: None,
                });
            }
        }
    }
    entries
}

/// Account-grouped host dropdown with explicit per-account discovery. Hosts without a
/// remembered inventory open the foreground host review, which returns the
/// selection to the draft.
pub(crate) fn show_profile_host_picker(
    ui: &mut Ui,
    tab: TabId,
    configuration: &Configuration,
    selected: Option<&HostReference>,
    selected_label: Option<&str>,
    enabled: bool,
) -> Option<ProfileHostPick> {
    let id = state_id(tab);
    let mut state = ui.data(|data| data.get_temp::<State>(id).unwrap_or_default());
    if !state.for_profile {
        state.request = None;
        state.reauthentication_generation = None;
    }
    state.for_profile = true;
    if state.selected_reference.as_ref() != selected {
        state.reauthentication_generation = None;
        state.selected_reference = selected.cloned();
    }
    sync_accounts(&mut state, configuration, ui.ctx());
    for row in &mut state.accounts {
        if row.hosts.is_none() {
            row.hosts = cached_discovery(ui.ctx(), &row.account);
        }
    }
    let remembered = remembered_hosts(ui.ctx());
    let mut pick = None;
    let host_label = ui.label("Host").id;
    ui.add_enabled_ui(enabled, |ui| {
        let combo = egui::ComboBox::from_id_salt(("recipe_host", tab))
            .width(ui.available_width())
            .selected_text(selected_label.unwrap_or("Select a host…"))
            .show_ui(ui, |ui| {
                if configuration.accounts().is_empty() {
                    ui.label(
                        egui::RichText::new("Add a GitHub account in Settings.")
                            .color(theme::TEXT_MUTED),
                    );
                }
                for (index, account) in configuration.accounts().iter().enumerate() {
                    let Some(row) = state
                        .accounts
                        .iter()
                        .find(|row| row.account == account.identifier())
                    else {
                        continue;
                    };
                    if index > 0 {
                        ui.add_space(4.0);
                        ui.separator();
                    }
                    ui.label(
                        egui::RichText::new(format!("@{}", account.login()))
                            .size(11.0)
                            .strong()
                            .color(theme::TEXT_SECONDARY),
                    );
                    let entries = picker_entries(row, account, &remembered, configuration);
                    if entries.is_empty() {
                        let note = if row.request.is_some() || row.queued {
                            "Refreshing…"
                        } else if row.hosts.is_some() {
                            "No hosts found."
                        } else {
                            "Not refreshed yet."
                        };
                        ui.label(egui::RichText::new(note).color(theme::TEXT_MUTED));
                    }
                    for entry in entries {
                        let key = (entry.choice.account.clone(), entry.choice.locator.clone());
                        ui.push_id(key, |ui| {
                            let current = entry
                                .choice
                                .reference
                                .as_ref()
                                .is_some_and(|reference| Some(reference) == selected);
                            if ui.selectable_label(current, &entry.choice.name).clicked() {
                                pick = Some(match entry.remembered {
                                    Some(host) => ProfileHostPick::Bind(host),
                                    None => {
                                        let request = egui::Id::new(uuid::Uuid::new_v4());
                                        state.request = Some(request);
                                        state.reauthentication_generation = None;
                                        state.feedback = None;
                                        ProfileHostPick::Command(Box::new(
                                            AppCommand::ReviewDevTunnelHost {
                                                tab_id: tab,
                                                request,
                                                host: entry.choice,
                                            },
                                        ))
                                    }
                                });
                            }
                        });
                    }
                }
            });
        combo.response.labelled_by(host_label);
    });
    if enabled {
        let selected_host = selected.and_then(|reference| {
            let AccountReference::Github { account_id } = reference.account;
            let account = configuration
                .accounts()
                .iter()
                .find(|account| account.account_id() == account_id)?;
            let hint = remembered
                .iter()
                .find(|host| &host.reference == reference)
                .map(|host| &host.hint)
                .or_else(|| {
                    configuration
                        .host_recipes()
                        .recipes()
                        .iter()
                        .find(|recipe| &recipe.host == reference)
                        .and_then(|recipe| recipe.host_hint.as_ref())
                })?;
            Some(HostChoice {
                account: account.identifier().to_owned(),
                name: hint.name.clone(),
                locator: hint.locator.clone(),
                reference: Some(reference.clone()),
            })
        });
        if let Some(host) = selected_host {
            if action_button(ui, ActionButtonRole::Secondary, "Refresh sessions").clicked() {
                let request = egui::Id::new(uuid::Uuid::new_v4());
                state.request = Some(request);
                state.reauthentication_generation = None;
                state.feedback = None;
                pick = Some(ProfileHostPick::Command(Box::new(
                    AppCommand::ReviewDevTunnelHost {
                        tab_id: tab,
                        request,
                        host,
                    },
                )));
            }
        }
    }
    if enabled {
        for account in configuration.accounts() {
            let Some(row) = state
                .accounts
                .iter_mut()
                .find(|row| row.account == account.identifier())
            else {
                continue;
            };
            if ui
                .add_enabled(
                    row.request.is_none() && !row.queued,
                    egui::Button::new(format!("Find hosts · @{}", account.login())),
                )
                .clicked()
            {
                row.queued = true;
                state.reauthentication_generation = None;
                row.reauthentication = None;
            }
            if row.request.is_some() || row.queued {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new(format!("Refreshing hosts · @{}…", account.login()))
                            .color(theme::TEXT_MUTED),
                    );
                });
            } else if let Some(error) = &row.error {
                let message = if row.hosts.is_some() {
                    format!(
                        "@{}: refresh failed. Hosts are cached from the last read. {error}",
                        account.login()
                    )
                } else {
                    format!("@{}: couldn't find hosts. {error}", account.login())
                };
                ui.colored_label(theme::STATUS_ERROR, message);
                if let Some(command) =
                    reauthentication_button(ui, row, account, state.reauthentication_generation)
                {
                    pick = Some(ProfileHostPick::Command(Box::new(command)));
                }
            }
        }
        if let Some(message) = &state.feedback {
            ui.label(egui::RichText::new(message).color(theme::TEXT_MUTED));
        }
    }
    if pick.is_none() {
        if let Some(row) = state
            .accounts
            .iter_mut()
            .find(|row| row.queued && row.request.is_none())
        {
            let request = egui::Id::new(uuid::Uuid::new_v4());
            row.queued = false;
            row.request = Some(request);
            state.reauthentication_generation = Some(request);
            row.reauthentication = None;
            row.error = None;
            pick = Some(ProfileHostPick::Command(Box::new(
                AppCommand::FindDevTunnelHosts {
                    tab_id: tab,
                    request,
                    account: row.account.clone(),
                },
            )));
        }
    }
    if state.accounts.iter().any(|row| row.queued) {
        ui.ctx().request_repaint();
    }
    ui.data_mut(|data| data.insert_temp(id, state));
    pick
}

pub(crate) enum BrowserNavigation {
    Command(Box<AppCommand>),
    AccountChanged {
        account: festerm_config::DevTunnelAccount,
        request: egui::Id,
    },
}

/// The same request-bound account/host choices beside the selected host's inventory.
/// Rendering remains offline; refresh and selection use the existing app commands.
pub(crate) fn show_browser_sidebar(
    ui: &mut Ui,
    tab: TabId,
    configuration: &Configuration,
    account_key: &str,
    locator: &str,
    host_name: &str,
) -> Option<BrowserNavigation> {
    let id = state_id(tab);
    let mut state = ui.data(|data| data.get_temp::<State>(id).unwrap_or_default());
    sync_accounts(&mut state, configuration, ui.ctx());
    let mut selected = ui
        .data(|data| data.get_temp::<String>(egui::Id::new(("host_sidebar_account", tab))))
        .unwrap_or_else(|| account_key.to_owned());
    if configuration.account(&selected).is_none() {
        selected = account_key.to_owned();
    }
    let previous_selected = selected.clone();
    ui.small("GitHub account");
    egui::ComboBox::from_id_salt(("host_sidebar_account", tab))
        .selected_text(
            configuration
                .account(&selected)
                .map(|account| format!("@{}", account.login()))
                .unwrap_or_default(),
        )
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for account in configuration.accounts() {
                ui.selectable_value(
                    &mut selected,
                    account.identifier().to_owned(),
                    format!("@{}", account.login()),
                );
            }
        });
    let account_change = if selected != previous_selected {
        let request = egui::Id::new(uuid::Uuid::new_v4());
        state.request = Some(request);
        state.reauthentication_generation = None;
        configuration
            .account(&selected)
            .cloned()
            .map(|account| BrowserNavigation::AccountChanged { account, request })
    } else {
        None
    };
    let mut command = None;
    ui.add_enabled_ui(account_change.is_none(), |ui| {
        if let Some(account) = configuration.account(&selected) {
            if let Some(row) = state
                .accounts
                .iter_mut()
                .find(|row| row.account == selected)
            {
                ui.add_space(8.0);
                if action_button(ui, ActionButtonRole::Secondary, "Refresh hosts").clicked() {
                    command = Some(begin_discovery(row, tab));
                    state.reauthentication_generation = row.request;
                }
                if row.request.is_some() {
                    ui.spinner();
                }
                if let Some(error) = &row.error {
                    ui.add(egui::Label::new(error).wrap());
                    if let Some(next) =
                        reauthentication_button(ui, row, account, state.reauthentication_generation)
                    {
                        command = Some(next);
                    }
                }
                ui.add_space(16.0);
                ui.strong("Hosts");
                let remembered = remembered_hosts(ui.ctx());
                let entries = picker_entries(row, account, &remembered, configuration);
                if !locator.is_empty()
                    && selected == account_key
                    && !entries.iter().any(|entry| entry.choice.locator == locator)
                {
                    let _ = ui.selectable_label(true, host_name);
                }
                for entry in entries {
                    let host = entry.choice;
                    if ui
                        .selectable_label(
                            host.account == account_key && host.locator == locator,
                            &host.name,
                        )
                        .clicked()
                    {
                        let request = egui::Id::new(uuid::Uuid::new_v4());
                        state.request = Some(request);
                        state.reauthentication_generation = None;
                        state.feedback = None;
                        command = Some(AppCommand::ReviewDevTunnelHost {
                            tab_id: tab,
                            request,
                            host,
                        });
                    }
                }
            }
        }
    });
    ui.data_mut(|data| {
        data.insert_temp(egui::Id::new(("host_sidebar_account", tab)), selected);
        data.insert_temp(id, state);
    });
    account_change.or_else(|| command.map(|command| BrowserNavigation::Command(Box::new(command))))
}

fn show_mode(
    ui: &mut Ui,
    tab: TabId,
    configuration: &Configuration,
    for_profile: bool,
) -> Option<AppCommand> {
    ui.spacing_mut().interact_size.y = ui.spacing().interact_size.y.max(24.0);
    let id = state_id(tab);
    let mut state = ui.data(|data| data.get_temp::<State>(id).unwrap_or_default());
    if state.for_profile != for_profile {
        state.request = None;
        state.reauthentication_generation = None;
        state.automatic_open = false;
    }
    state.for_profile = for_profile;
    sync_accounts(&mut state, configuration, ui.ctx());
    if !for_profile && !state.automatic_open {
        for row in &mut state.accounts {
            row.automatic_discovery_started = false;
        }
        state.automatic_open = true;
    }
    let mut command = None;
    if let Some(message) = &state.feedback {
        ui.label(message);
    }
    if !for_profile && !configuration.host_recipes().is_empty() {
        ssh_section_heading(ui, "Saved sessions");
        for recipe in configuration.host_recipes().recipes() {
            ui.push_id(recipe.id.as_str(), |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(&recipe.name);
                    if action_button(ui, ActionButtonRole::Accent, "Connect").clicked() {
                        let request = egui::Id::new(uuid::Uuid::new_v4());
                        state.request = Some(request);
                        state.reauthentication_generation = None;
                        state.feedback = None;
                        command = Some(AppCommand::ConnectHostRecipe {
                            tab_id: tab,
                            request,
                            recipe: Box::new(recipe.clone()),
                        });
                    }
                });
            });
        }
        ui.add_space(12.0);
    }
    ssh_section_heading(ui, "Dev Tunnels");
    if configuration.accounts().is_empty() {
        ui.label("Add a GitHub account in Settings.");
        if action_button(ui, ActionButtonRole::Secondary, "Open Settings").clicked() {
            command = Some(AppCommand::OpenSettings);
        }
    }
    for account in configuration.accounts() {
        if !state
            .accounts
            .iter()
            .any(|row| row.account == account.identifier())
        {
            state.accounts.push(AccountResults {
                account: account.identifier().to_owned(),
                ..Default::default()
            });
        }
        let row = state
            .accounts
            .iter_mut()
            .find(|row| row.account == account.identifier())
            .expect("inserted account");
        ui.push_id(account.identifier(), |ui| {
            if action_button(
                ui,
                ActionButtonRole::Secondary,
                &format!("Refresh hosts · @{}", account.login()),
            )
            .clicked()
            {
                command = Some(begin_discovery(row, tab));
                state.reauthentication_generation = row.request;
            }
            if row.request.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Finding hosts…");
                });
            }
            if let Some(error) = &row.error {
                ui.colored_label(theme::STATUS_ERROR, error);
                if let Some(next) =
                    reauthentication_button(ui, row, account, state.reauthentication_generation)
                {
                    command = Some(next);
                }
            }
            if let Some(hosts) = &row.hosts {
                if hosts.is_empty() {
                    ui.label("No hosts found.");
                }
                for host in hosts {
                    if action_button(ui, ActionButtonRole::Secondary, &host.name).clicked() {
                        let request = egui::Id::new(uuid::Uuid::new_v4());
                        state.request = Some(request);
                        state.reauthentication_generation = None;
                        state.feedback = None;
                        command = Some(AppCommand::ReviewDevTunnelHost {
                            tab_id: tab,
                            request,
                            host: host.clone(),
                        });
                    }
                }
            }
        });
    }
    let mut saved = Vec::new();
    for recipe in configuration.host_recipes().recipes() {
        if let Some(host) = saved_host(recipe, configuration) {
            let key = (host.account.clone(), host.reference.clone());
            if saved.contains(&key) {
                continue;
            }
            saved.push(key);
            ui.push_id(("saved", &host.account, &host.locator), |ui| {
                if action_button(
                    ui,
                    ActionButtonRole::Secondary,
                    &format!("{} · saved", host.name),
                )
                .clicked()
                {
                    let request = egui::Id::new(uuid::Uuid::new_v4());
                    state.request = Some(request);
                    state.reauthentication_generation = None;
                    state.feedback = None;
                    command = Some(AppCommand::ReviewDevTunnelHost {
                        tab_id: tab,
                        request,
                        host: host.clone(),
                    });
                }
            });
        }
    }
    if command.is_none() {
        command = automatic_discovery(
            &mut state,
            tab,
            ui.is_enabled() && ui.input(|input| input.focused),
        );
        if command.is_some() {
            ui.ctx().request_repaint();
        }
    }
    ui.data_mut(|data| data.insert_temp(id, state));
    command
}

fn reauthentication_button(
    ui: &mut Ui,
    row: &AccountResults,
    account: &festerm_config::DevTunnelAccount,
    generation: Option<egui::Id>,
) -> Option<AppCommand> {
    let request = row.reauthentication.as_ref()?;
    let ReauthenticationOrigin::Discovery {
        request: expected, ..
    } = request.origin
    else {
        return None;
    };
    if row.request.is_some()
        || row.queued
        || generation != Some(expected)
        || &request.account != account
    {
        return None;
    }
    ui.push_id(("host_reauthentication", account.identifier()), |ui| {
        action_button(ui, ActionButtonRole::Secondary, "Sign in again")
            .clicked()
            .then(|| AppCommand::ReauthenticateHostAccount(request.clone()))
    })
    .inner
}

pub(crate) fn reauthentication_is_current(
    context: &egui::Context,
    tab: TabId,
    generation: egui::Id,
    request: &Reauthentication,
) -> bool {
    context.data(|data| {
        data.get_temp::<State>(state_id(tab)).is_some_and(|state| {
            state.reauthentication_generation == Some(generation)
                && state.accounts.iter().any(|row| {
                    row.request.is_none()
                        && !row.queued
                        && row.reauthentication.as_ref() == Some(request)
                })
        })
    })
}

pub(crate) fn clear_reauthentication(context: &egui::Context, tab: TabId) {
    context.data_mut(|data| {
        if let Some(mut state) = data.get_temp::<State>(state_id(tab)) {
            state.reauthentication_generation = None;
            for row in &mut state.accounts {
                if row.reauthentication.take().is_some() {
                    row.error = Some("Complete sign-in to refresh hosts automatically.".into());
                }
            }
            data.insert_temp(state_id(tab), state);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    #[test]
    fn host_workspace_open_discovers_once_with_accessible_refresh_and_cancel_fencing() {
        let account = festerm_config::DevTunnelAccount::new(
            "github-fixture",
            festerm_config::AccountProvider::Github,
            424242,
            "developer",
            festerm_secret_store::SecretReference::generate(),
        )
        .unwrap();
        let (configuration, _) = Configuration::empty().with_account(account).unwrap();
        let tab = TabId::next_for_test();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(360.0, 516.0))
            .build_ui_state(
                move |ui, command: &mut Option<AppCommand>| {
                    if let Some(next) = show(ui, tab, &configuration) {
                        *command = Some(next);
                    }
                },
                None,
            );
        harness.run_steps(2);
        let button = harness.get_by_label("Refresh hosts · @developer");
        assert!(button.rect().height() >= 24.0);
        harness.run_steps(4);
        let Some(AppCommand::FindDevTunnelHosts {
            request, account, ..
        }) = harness.state()
        else {
            panic!("automatic discovery command")
        };
        let (request, account) = (*request, account.clone());
        assert!(discovery_is_current(&harness.ctx, tab, request, &account));
        cancel(&harness.ctx, tab);
        report_discovery(&harness.ctx, tab, request, &account, Ok(Vec::new()));
        assert!(!discovery_is_current(&harness.ctx, tab, request, &account));
        assert!(harness
            .ctx
            .data(|data| data.get_temp::<State>(state_id(tab)))
            .unwrap()
            .accounts
            .iter()
            .all(|row| row.hosts.is_none()));
    }

    #[test]
    fn host_workspace_absent_request_rejects_stale_discovery() {
        let context = egui::Context::default();
        let tab = TabId::next_for_test();
        assert!(!request_is_current(&context, tab, egui::Id::new("absent")));
        report_discovery(
            &context,
            tab,
            egui::Id::new("stale"),
            "account",
            Ok(Vec::new()),
        );
        assert!(context
            .data(|data| data.get_temp::<State>(state_id(tab)))
            .is_none());
    }

    fn account(identifier: &str, id: u64, login: &str) -> festerm_config::DevTunnelAccount {
        festerm_config::DevTunnelAccount::new(
            identifier,
            festerm_config::AccountProvider::Github,
            id,
            login,
            festerm_secret_store::SecretReference::generate(),
        )
        .unwrap()
    }

    fn summary(locator: &str, name: &str) -> TunnelSummary {
        TunnelSummary {
            locator: locator.into(),
            name: Some(name.into()),
            description: None,
            active: true,
        }
    }

    struct AutomaticDiscovery {
        configuration: Configuration,
        commands: Vec<AppCommand>,
        foreground: bool,
    }

    fn automatic_harness(
        configuration: Configuration,
        tab: TabId,
    ) -> Harness<'static, AutomaticDiscovery> {
        Harness::builder()
            .with_size(egui::vec2(460.0, 720.0))
            .build_ui_state(
                move |ui, state: &mut AutomaticDiscovery| {
                    ui.ctx().input_mut(|input| input.focused = state.foreground);
                    if let Some(command) = show(ui, tab, &state.configuration) {
                        state.commands.push(command);
                    }
                },
                AutomaticDiscovery {
                    configuration,
                    commands: Vec::new(),
                    foreground: true,
                },
            )
    }

    fn automatic_requests(state: &AutomaticDiscovery) -> Vec<(egui::Id, String)> {
        state
            .commands
            .iter()
            .map(|command| match command {
                AppCommand::FindDevTunnelHosts {
                    request, account, ..
                } => (*request, account.clone()),
                _ => panic!("automatic discovery cannot open a browser or launch a session"),
            })
            .collect()
    }

    #[test]
    fn normal_host_browser_saved_account_lists_once_not_once_per_paint() {
        let configuration = Configuration::empty()
            .with_account(account("alice", 1, "alice"))
            .unwrap()
            .0;
        let tab = TabId::next_for_test();
        let mut harness = automatic_harness(configuration, tab);
        harness.run_steps(8);
        let requests = automatic_requests(harness.state());
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1, "alice");
        report_discovery(
            &harness.ctx,
            tab,
            requests[0].0,
            "alice",
            Ok(vec![summary("alice.use", "Alice host")]),
        );
        harness.run_steps(8);
        assert_eq!(automatic_requests(harness.state()).len(), 1);
        harness.get_by_label("Alice host");
    }

    #[test]
    fn normal_host_browser_sign_in_return_waits_for_foreground_and_lists_once() {
        let tab = TabId::next_for_test();
        let mut harness = automatic_harness(Configuration::empty(), tab);
        harness.run_steps(4);
        assert!(
            harness.state().commands.is_empty(),
            "no automatic browser without an account"
        );
        harness.state_mut().foreground = false;
        harness.state_mut().configuration = Configuration::empty()
            .with_account(account("alice", 1, "alice"))
            .unwrap()
            .0;
        harness.run_steps(4);
        assert!(harness.state().commands.is_empty());
        harness.state_mut().foreground = true;
        harness.run_steps(8);
        assert_eq!(automatic_requests(harness.state()).len(), 1);
    }

    #[test]
    fn normal_host_browser_reauthentication_refreshes_once_for_the_new_credential_snapshot() {
        let account = account("alice", 1, "alice");
        let configuration = Configuration::empty()
            .with_account(account.clone())
            .unwrap()
            .0;
        let tab = TabId::next_for_test();
        let mut harness = automatic_harness(configuration, tab);
        harness.run_steps(4);
        let old_request = automatic_requests(harness.state())[0].0;
        harness.state_mut().foreground = false;
        harness.state_mut().configuration = Configuration::empty()
            .with_account(
                account
                    .clone()
                    .with_credential(festerm_secret_store::SecretReference::generate()),
            )
            .unwrap()
            .0;
        harness.run_steps(4);
        report_typed_discovery(
            &harness.ctx,
            tab,
            old_request,
            &account,
            Err(RequestFailure::from(festerm_tunnel_client::Error::Expired)),
        );
        assert!(!discovery_is_current(
            &harness.ctx,
            tab,
            old_request,
            "alice"
        ));
        assert_eq!(automatic_requests(harness.state()).len(), 1);
        harness.state_mut().foreground = true;
        harness.run_steps(8);
        assert_eq!(automatic_requests(harness.state()).len(), 2);
        assert!(harness.query_by_label("Sign in again").is_none());
    }

    #[test]
    fn normal_host_browser_account_switch_clears_ready_hosts_and_rejects_late_result() {
        let alice = account("selected", 1, "alice");
        let configuration = Configuration::empty().with_account(alice).unwrap().0;
        let tab = TabId::next_for_test();
        let mut harness = automatic_harness(configuration, tab);
        harness.run_steps(4);
        let old_request = automatic_requests(harness.state())[0].0;
        report_discovery(
            &harness.ctx,
            tab,
            old_request,
            "selected",
            Ok(vec![summary("old.use", "Old host")]),
        );
        harness.run_steps(4);
        harness.get_by_label("Old host");
        harness.state_mut().configuration = Configuration::empty()
            .with_account(account("selected", 2, "bob"))
            .unwrap()
            .0;
        harness.run_steps(4);
        assert_eq!(automatic_requests(harness.state()).len(), 2);
        assert!(harness.query_by_label("Old host").is_none());
        report_discovery(
            &harness.ctx,
            tab,
            old_request,
            "selected",
            Ok(vec![summary("late.use", "Late old host")]),
        );
        harness.run_steps(4);
        assert!(harness.query_by_label("Late old host").is_none());
        assert!(!discovery_is_current(
            &harness.ctx,
            tab,
            old_request,
            "selected"
        ));
    }

    #[test]
    fn normal_host_browser_failure_waits_for_explicit_refresh_without_automatic_retry() {
        let configuration = Configuration::empty()
            .with_account(account("alice", 1, "alice"))
            .unwrap()
            .0;
        let tab = TabId::next_for_test();
        let mut harness = automatic_harness(configuration, tab);
        harness.run_steps(4);
        let first = automatic_requests(harness.state())[0].0;
        report_discovery(&harness.ctx, tab, first, "alice", Err("Offline".into()));
        harness.run_steps(8);
        assert_eq!(automatic_requests(harness.state()).len(), 1);
        harness.get_by_label("Refresh hosts · @alice").click();
        harness.run_steps(8);
        let requests = automatic_requests(harness.state());
        assert_eq!(requests.len(), 2);
        assert_ne!(first, requests[1].0);
        cancel(&harness.ctx, tab);
        report_discovery(
            &harness.ctx,
            tab,
            requests[1].0,
            "alice",
            Ok(vec![summary("late.use", "Cancelled host")]),
        );
        assert!(!discovery_is_current(
            &harness.ctx,
            tab,
            requests[1].0,
            "alice"
        ));
        assert!(harness
            .ctx
            .data(|data| data.get_temp::<State>(state_id(tab)))
            .unwrap()
            .accounts[0]
            .hosts
            .is_none());
    }

    fn picker_harness(
        configuration: Configuration,
        tab: TabId,
    ) -> Harness<'static, Vec<ProfileHostPick>> {
        Harness::builder()
            .with_size(egui::vec2(360.0, 600.0))
            .build_ui_state(
                move |ui, picks: &mut Vec<ProfileHostPick>| {
                    if let Some(pick) =
                        show_profile_host_picker(ui, tab, &configuration, None, None, true)
                    {
                        picks.push(pick);
                    }
                },
                Vec::new(),
            )
    }

    fn discoveries(picks: &[ProfileHostPick]) -> Vec<(egui::Id, String)> {
        picks
            .iter()
            .filter_map(|pick| match pick {
                ProfileHostPick::Command(command) => match command.as_ref() {
                    AppCommand::FindDevTunnelHosts {
                        request, account, ..
                    } => Some((*request, account.clone())),
                    _ => None,
                },
                ProfileHostPick::Bind(_) => None,
            })
            .collect()
    }

    #[test]
    fn profile_host_picker_is_offline_until_each_account_is_explicitly_discovered() {
        let configuration = Configuration::empty()
            .with_account(account("alice-key", 1, "alice"))
            .unwrap()
            .0
            .with_account(account("bob-key", 2, "bob"))
            .unwrap()
            .0;
        let tab = TabId::next_for_test();
        let mut harness = picker_harness(configuration.clone(), tab);
        harness.run_steps(4);
        assert!(
            discoveries(harness.state()).is_empty(),
            "opening is offline"
        );
        harness.get_by_label("Find hosts · @alice").click();
        harness.run_steps(4);
        assert_eq!(discoveries(harness.state()).len(), 1);
        harness.get_by_label("Find hosts · @bob").click();
        harness.run_steps(4);
        let started = discoveries(harness.state());
        assert_eq!(
            started
                .iter()
                .map(|(_, account)| account.as_str())
                .collect::<Vec<_>>(),
            ["alice-key", "bob-key"],
            "only explicitly requested accounts are discovered"
        );
        harness.get_by_label("Refreshing hosts · @alice…");
        for ((request, account), host) in started.iter().zip(["Alice build", "Bob build"]) {
            report_discovery(
                &harness.ctx,
                tab,
                *request,
                account,
                Ok(vec![summary(&format!("{account}.use"), host)]),
            );
        }
        harness.run_steps(4);
        harness.get_by_label("Host").click();
        harness.run_steps(4);
        let alice = harness.get_by_label("@alice").rect();
        let bob = harness.get_by_label("@bob").rect();
        let alice_host = harness.get_by_label("Alice build").rect();
        let bob_host = harness.get_by_label("Bob build").rect();
        assert!(alice.top() < alice_host.top() && alice_host.top() < bob.top());
        assert!(bob.top() < bob_host.top());
        harness.get_by_label("Bob build").click();
        harness.run_steps(4);
        let Some(ProfileHostPick::Command(command)) = harness.state().last() else {
            panic!("unreviewed host opens the foreground review")
        };
        let AppCommand::ReviewDevTunnelHost { request, host, .. } = command.as_ref() else {
            panic!("review, never an offline bind of an unverified host")
        };
        assert_eq!(host.account, "bob-key");
        assert!(host.reference.is_none());
        assert!(profile_request_is_current(&harness.ctx, tab, *request));
    }

    #[test]
    fn profile_host_picker_keeps_cached_hosts_and_warns_when_refresh_fails() {
        let configuration = Configuration::empty()
            .with_account(account("alice-key", 1, "alice"))
            .unwrap()
            .0;
        let tab = TabId::next_for_test();
        let mut harness = picker_harness(configuration.clone(), tab);
        harness.run_steps(4);
        harness.get_by_label("Find hosts · @alice").click();
        harness.run_steps(4);
        let (request, account) = discoveries(harness.state()).pop().unwrap();
        report_discovery(
            &harness.ctx,
            tab,
            request,
            &account,
            Err("Host discovery timed out. Try again.".into()),
        );
        harness.run_steps(4);
        harness.get_by_label("@alice: couldn't find hosts. Host discovery timed out. Try again.");

        harness.get_by_label("Find hosts · @alice").click();
        harness.run_steps(4);
        let (request, _) = discoveries(harness.state()).pop().unwrap();
        report_discovery(
            &harness.ctx,
            tab,
            request,
            &account,
            Ok(vec![summary("alice.use", "Alice build")]),
        );
        harness.run_steps(4);
        harness.get_by_label("Find hosts · @alice").click();
        harness.run_steps(4);
        let (request, _) = discoveries(harness.state()).pop().unwrap();
        report_discovery(
            &harness.ctx,
            tab,
            request,
            &account,
            Err("Host discovery timed out. Try again.".into()),
        );
        harness.run_steps(4);
        harness.get_by_label(
            "@alice: refresh failed. Hosts are cached from the last read. Host discovery timed out. Try again.",
        );
        harness.get_by_label("Host").click();
        harness.run_steps(4);
        harness.get_by_label("Alice build");

        // Every tab shares the application context, so a fresh Profiles tab
        // starts from the last successful read rather than an empty list.
        let cached = cached_discovery(&harness.ctx, &account).unwrap();
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].name, "Alice build");
    }

    #[test]
    fn profile_host_session_refresh_requires_an_explicit_click_and_foreground_review() {
        let configuration = Configuration::empty()
            .with_account(account("alice-key", 1, "alice"))
            .unwrap()
            .0;
        let reference = HostReference {
            id: "00000000-0000-4000-8000-000000000001".parse().unwrap(),
            account: AccountReference::Github { account_id: 1 },
        };
        let selected = reference.clone();
        let tab = TabId::next_for_test();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(360.0, 600.0))
            .build_ui_state(
                move |ui, picks: &mut Vec<ProfileHostPick>| {
                    if let Some(pick) = show_profile_host_picker(
                        ui,
                        tab,
                        &configuration,
                        Some(&selected),
                        Some("Saved build"),
                        true,
                    ) {
                        picks.push(pick);
                    }
                },
                Vec::new(),
            );
        remember_host(
            &harness.ctx,
            HostSessions {
                reference: reference.clone(),
                hint: HostHint {
                    name: "Saved build".into(),
                    locator: "alice.use".into(),
                },
                sessions: Vec::new(),
            },
        );
        harness.run_steps(4);
        assert!(
            harness.state().is_empty(),
            "saved host rendering remains offline"
        );
        harness.get_by_label("Refresh sessions").click();
        harness.run_steps(4);
        let Some(ProfileHostPick::Command(command)) = harness.state().last() else {
            panic!("explicit refresh opens foreground host review");
        };
        assert!(
            matches!(command.as_ref(), AppCommand::ReviewDevTunnelHost { host, .. }
            if host.reference.as_ref() == Some(&reference))
        );
    }

    #[test]
    fn profile_host_picker_binds_a_reviewed_host_offline_and_is_inert_when_disabled() {
        let configuration = Configuration::empty()
            .with_account(account("alice-key", 1, "alice"))
            .unwrap()
            .0;
        let reference = HostReference {
            id: "00000000-0000-4000-8000-000000000001".parse().unwrap(),
            account: AccountReference::Github { account_id: 1 },
        };
        let tab = TabId::next_for_test();
        let mut harness = picker_harness(configuration.clone(), tab);
        remember_host(
            &harness.ctx,
            HostSessions {
                reference: reference.clone(),
                hint: HostHint {
                    name: "Reviewed build".into(),
                    locator: "alice.use".into(),
                },
                sessions: Vec::new(),
            },
        );
        let disabled_configuration = configuration.clone();
        let mut disabled = Harness::builder().build_ui(move |ui| {
            assert!(show_profile_host_picker(
                ui,
                tab,
                &disabled_configuration,
                None,
                Some("Reviewed build"),
                false
            )
            .is_none());
        });
        disabled.run();
        assert!(disabled.query_by_label("Find hosts · @alice").is_none());

        harness.run_steps(4);
        harness.get_by_label("Host").click();
        harness.run_steps(4);
        harness.get_by_label("Reviewed build").click();
        harness.run_steps(4);
        assert!(matches!(
            harness.state().last(),
            Some(ProfileHostPick::Bind(host)) if host.reference == reference
        ));
    }
}
