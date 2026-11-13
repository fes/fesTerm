//! Offline host choices and explicit, request-bound discovery presentation.

use super::*;
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
    request: Option<egui::Id>,
    /// Hosts from the last successful read; kept when a later refresh fails.
    hosts: Option<Vec<HostChoice>>,
    error: Option<String>,
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
        for account in &mut state.accounts {
            account.request = None;
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
        state.feedback = Some(message.to_owned());
        data.insert_temp(state_id(tab), state);
    });
    context.request_repaint();
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
                    request: None,
                    hosts: Some(vec![HostChoice {
                        account,
                        name: "Build workstation".into(),
                        locator: "fixture.use".into(),
                        reference: None,
                    }]),
                    error: unavailable.then(|| "Host unavailable. Try again.".to_owned()),
                    queued: false,
                }],
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

/// Queues a host refresh for every signed-in account. The picker starts the
/// queued requests; the app runs up to its parallel discovery limit.
pub(crate) fn queue_profile_refresh(
    context: &egui::Context,
    tab: TabId,
    configuration: &Configuration,
) {
    context.data_mut(|data| {
        let mut state = data.get_temp::<State>(state_id(tab)).unwrap_or_default();
        sync_accounts(&mut state, configuration);
        for row in &mut state.accounts {
            row.queued = true;
        }
        data.insert_temp(state_id(tab), state);
    });
}

fn sync_accounts(state: &mut State, configuration: &Configuration) {
    state
        .accounts
        .retain(|row| configuration.account(&row.account).is_some());
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
    }
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

/// Account-grouped host dropdown with an explicit Refresh. Hosts without a
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
    }
    state.for_profile = true;
    sync_accounts(&mut state, configuration);
    for row in &mut state.accounts {
        if row.hosts.is_none() {
            row.hosts = cached_discovery(ui.ctx(), &row.account);
        }
    }
    let remembered = remembered_hosts(ui.ctx());
    let mut pick = None;
    let mut refresh = false;
    let mut host_label = None;
    ui.horizontal(|ui| {
        host_label = Some(ui.label("Host").id);
        if enabled && !configuration.accounts().is_empty() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = state
                    .accounts
                    .iter()
                    .any(|row| row.request.is_some() || row.queued);
                if ui
                    .add_enabled(!busy, egui::Button::new("Refresh").small())
                    .on_hover_text("Find hosts again for every signed-in account")
                    .clicked()
                {
                    refresh = true;
                }
            });
        }
    });
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
        if let Some(label) = host_label {
            combo.response.labelled_by(label);
        }
    });
    if enabled {
        for account in configuration.accounts() {
            let Some(row) = state
                .accounts
                .iter()
                .find(|row| row.account == account.identifier())
            else {
                continue;
            };
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
            }
        }
        if let Some(message) = &state.feedback {
            ui.label(egui::RichText::new(message).color(theme::TEXT_MUTED));
        }
    }
    if refresh {
        for row in &mut state.accounts {
            row.queued = true;
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
    }
    state.for_profile = for_profile;
    state
        .accounts
        .retain(|row| configuration.account(&row.account).is_some());
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
                &format!("Find hosts · @{}", account.login()),
            )
            .clicked()
            {
                let request = egui::Id::new(uuid::Uuid::new_v4());
                row.request = Some(request);
                row.error = None;
                command = Some(AppCommand::FindDevTunnelHosts {
                    tab_id: tab,
                    request,
                    account: row.account.clone(),
                });
            }
            if row.request.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Finding hosts…");
                });
            }
            if let Some(error) = &row.error {
                ui.colored_label(theme::STATUS_ERROR, error);
            }
            if let Some(hosts) = &row.hosts {
                if hosts.is_empty() {
                    ui.label("No hosts found.");
                }
                for host in hosts {
                    if action_button(ui, ActionButtonRole::Secondary, &host.name).clicked() {
                        let request = egui::Id::new(uuid::Uuid::new_v4());
                        state.request = Some(request);
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
    ui.data_mut(|data| data.insert_temp(id, state));
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    #[test]
    fn host_workspace_discovery_is_explicit_with_accessible_targets_and_cancel_fencing() {
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
        harness.run();
        assert!(harness.state().is_none());
        let button = harness.get_by_label("Find hosts · @developer");
        assert!(button.rect().height() >= 24.0);
        button.click();
        harness.run_steps(1);
        let Some(AppCommand::FindDevTunnelHosts {
            request, account, ..
        }) = harness.state()
        else {
            panic!("explicit discovery command")
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
    fn host_workspace_open_is_offline_and_stale_discovery_cannot_publish() {
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
    fn profile_host_picker_refreshes_every_account_and_groups_hosts_by_account() {
        let configuration = Configuration::empty()
            .with_account(account("alice-key", 1, "alice"))
            .unwrap()
            .0
            .with_account(account("bob-key", 2, "bob"))
            .unwrap()
            .0;
        let tab = TabId::next_for_test();
        let mut harness = picker_harness(configuration.clone(), tab);
        queue_profile_refresh(&harness.ctx, tab, &configuration);
        harness.run_steps(4);
        let started = discoveries(harness.state());
        assert_eq!(
            started
                .iter()
                .map(|(_, account)| account.as_str())
                .collect::<Vec<_>>(),
            ["alice-key", "bob-key"],
            "every signed-in account refreshes without a Find hosts click"
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
        queue_profile_refresh(&harness.ctx, tab, &configuration);
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

        harness.get_by_label("Refresh").click();
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
        harness.get_by_label("Refresh").click();
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
        assert!(disabled.query_by_label("Refresh").is_none());

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
