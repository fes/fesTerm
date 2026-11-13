//! Offline host choices and explicit, request-bound discovery presentation.

use super::*;
use festerm_config::host_recipes::{HostHint, HostReference, PublicRecipe};
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
    hosts: Option<Vec<HostChoice>>,
    error: Option<String>,
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
                }],
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

pub(crate) fn show_for_profile(
    ui: &mut Ui,
    tab: TabId,
    configuration: &Configuration,
) -> Option<AppCommand> {
    show_mode(ui, tab, configuration, true)
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
}
