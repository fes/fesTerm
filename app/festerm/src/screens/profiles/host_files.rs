use super::*;

#[derive(Clone, Default)]
pub(super) struct HostFilesDraft {
    pub(super) original_id: Option<String>,
    pub(super) name: String,
    account: Option<String>,
    locator: String,
    #[cfg(feature = "experimental-dev-tunnels")]
    host_label: String,
    #[cfg(feature = "experimental-dev-tunnels")]
    pending_request: Option<egui::Id>,
    pub(super) pending_save: Option<egui::Id>,
    pub(super) error: Option<String>,
}
impl HostFilesDraft {
    pub(super) fn from_profile(
        profile: &festerm_config::DevTunnelFilesProfileConfiguration,
    ) -> Self {
        Self {
            original_id: Some(profile.identifier().into()),
            name: profile.identifier().into(),
            account: profile.account().map(str::to_owned),
            locator: profile.host().into(),
            #[cfg(feature = "experimental-dev-tunnels")]
            host_label: profile.host().into(),
            ..Default::default()
        }
    }
    fn profile(&self) -> Result<Profile, festerm_config::ConfigError> {
        Profile::dev_tunnel_files(&self.name, self.account.clone(), &self.locator)
    }
    #[cfg(feature = "experimental-dev-tunnels")]
    fn bind(&mut self, account: String, locator: String, name: String) {
        self.account = Some(account);
        self.locator = locator;
        self.host_label = name;
        self.pending_request = None;
        self.error = None;
    }
}

pub(super) fn show(
    ui: &mut Ui,
    tab: TabId,
    configuration: &Configuration,
    draft: &mut HostFilesDraft,
    command: &mut Option<AppCommand>,
    next: &mut Option<ProfilesScreenMode>,
) {
    ui.heading(if draft.original_id.is_some() {
        "Edit Dev Tunnels Files profile"
    } else {
        "New Dev Tunnels Files profile"
    });
    ui.add_enabled_ui(
        draft.original_id.is_none() && draft.pending_save.is_none(),
        |ui| {
            labeled_text_edit(
                ui,
                (tab, "files-profile-name"),
                "Profile name",
                &mut draft.name,
                FieldOptions::stacked().char_limit(128),
            );
        },
    );
    #[cfg(feature = "experimental-dev-tunnels")]
    match crate::screens::host_workspace::show_profile_host_picker(
        ui,
        tab,
        configuration,
        None,
        (!draft.host_label.is_empty()).then_some(draft.host_label.as_str()),
        draft.pending_save.is_none(),
    ) {
        Some(crate::screens::host_workspace::ProfileHostPick::Bind(host)) => {
            let festerm_config::host_recipes::AccountReference::Github { account_id } =
                host.reference.account;
            if let Some(account) = configuration
                .accounts()
                .iter()
                .find(|account| account.account_id() == account_id)
            {
                draft.bind(
                    account.identifier().into(),
                    host.hint.locator,
                    host.hint.name,
                );
            }
        }
        Some(crate::screens::host_workspace::ProfileHostPick::Command(value)) => {
            // Capture Files intent in the command itself. A stale editor must
            // refuse selection, never fall through into terminal Control work.
            *command = Some(match *value {
                AppCommand::ReviewDevTunnelHost {
                    tab_id,
                    request,
                    host,
                } => {
                    draft.pending_request = Some(request);
                    AppCommand::SelectDevTunnelFilesHost {
                        tab_id,
                        request,
                        host,
                    }
                }
                other => {
                    if let AppCommand::FindDevTunnelHosts { request, .. } = &other {
                        draft.pending_request = Some(*request);
                    }
                    other
                }
            });
        }
        None => {}
    }
    #[cfg(not(feature = "experimental-dev-tunnels"))]
    {
        ui.label("Connecting Files requires a Dev Tunnels-enabled build.");
    }
    ui.label("File access must be enabled on the host. Uploads and new folders also require its write permission.");
    ui.label("Remote delete, rename and permission changes are unavailable.");
    if let Some(error) = &draft.error {
        ui.colored_label(theme::STATUS_ERROR, error);
    }
    let profile = draft.profile();
    let ready = profile.is_ok()
        && draft
            .account
            .as_deref()
            .is_some_and(|key| configuration.account(key).is_some())
        && draft.pending_save.is_none();
    ui.horizontal_wrapped(|ui| {
        if action_button_enabled(ui, ready, ActionButtonRole::Accent, "Save profile").clicked() {
            let request = egui::Id::new(uuid::Uuid::new_v4());
            draft.pending_save = Some(request);
            *command = Some(AppCommand::SaveDevTunnelProfile {
                tab_id: tab,
                request,
                profile: profile.as_ref().unwrap().clone(),
            });
        }
        if action_button_enabled(
            ui,
            ready && cfg!(feature = "experimental-dev-tunnels"),
            ActionButtonRole::Secondary,
            "Connect once",
        )
        .clicked()
        {
            *command = Some(AppCommand::OpenDevTunnelFiles {
                target: crate::sftp_file_manager::HostFilesLaunchTarget {
                    label: draft.name.clone(),
                    profile_id: configuration
                        .profile(&draft.name)
                        .filter(|saved| Some(*saved) == profile.as_ref().ok())
                        .map(|_| draft.name.clone()),
                    account: draft.account.clone(),
                    locator: draft.locator.clone(),
                },
            });
        }
        if action_button(ui, ActionButtonRole::Secondary, "Cancel").clicked() {
            *next = Some(ProfilesScreenMode::List);
        }
    });
}

#[cfg(feature = "experimental-dev-tunnels")]
pub(crate) fn select_discovered_files_host(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
    host: &crate::screens::host_workspace::HostChoice,
) -> bool {
    if !crate::screens::host_workspace::profile_request_is_current(context, tab, request) {
        return false;
    }
    context.data_mut(|data| {
        let id = profiles_state_id(tab);
        let Some(mut state) = data.get_temp::<ProfilesScreenState>(id) else {
            return false;
        };
        let ProfilesScreenMode::EditHostFiles(draft) = &mut state.mode else {
            return false;
        };
        if draft.pending_request != Some(request) {
            return false;
        }
        draft.bind(
            host.account.clone(),
            host.locator.clone(),
            host.name.clone(),
        );
        data.insert_temp(id, state);
        true
    })
}

#[cfg(all(test, feature = "experimental-dev-tunnels"))]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};
    #[test]
    fn files_profile_host_picker_emits_files_intent_and_refuses_stale_editor_selection() {
        let account = festerm_config::DevTunnelAccount::new(
            "alice-key",
            festerm_config::AccountProvider::Github,
            1,
            "alice",
            festerm_secret_store::SecretReference::generate(),
        )
        .unwrap();
        let configuration = Configuration::empty().with_account(account).unwrap().0;
        let tab = crate::tabs::AppState::for_test().active();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(560.0, 750.0))
            .build_ui_state(
                move |ui, state: &mut (HostFilesDraft, Vec<AppCommand>)| {
                    let mut command = None;
                    let mut next = None;
                    show(
                        ui,
                        tab,
                        &configuration,
                        &mut state.0,
                        &mut command,
                        &mut next,
                    );
                    if let Some(command) = command {
                        state.1.push(command);
                    }
                },
                (
                    HostFilesDraft {
                        name: "owned-files".into(),
                        ..Default::default()
                    },
                    Vec::new(),
                ),
            );
        crate::screens::host_workspace::install_profile_picker_gallery(
            &harness.ctx,
            tab,
            "picker-populated",
        );
        harness.run();
        harness.get_by_label("Host").click();
        harness.run_steps(4);
        harness.get_by_label("Build workstation").click();
        harness.run_steps(4);
        let (request, host) = match harness.state().1.last().unwrap() {
            AppCommand::SelectDevTunnelFilesHost {
                tab_id,
                request,
                host,
            } => {
                assert_eq!(*tab_id, tab);
                (*request, host.clone())
            }
            other => {
                panic!("Files selection must not become a Control/terminal request: {other:?}")
            }
        };
        let draft = harness.state().0.clone();
        harness.ctx.data_mut(|data| {
            data.insert_temp(
                profiles_state_id(tab),
                ProfilesScreenState {
                    mode: ProfilesScreenMode::EditHostFiles(draft),
                    ..Default::default()
                },
            )
        });
        assert!(!select_discovered_files_host(
            &harness.ctx,
            tab,
            request.with("stale"),
            &host
        ));
        assert!(select_discovered_files_host(
            &harness.ctx,
            tab,
            request,
            &host
        ));
        let state = harness
            .ctx
            .data(|data| data.get_temp::<ProfilesScreenState>(profiles_state_id(tab)))
            .unwrap();
        let ProfilesScreenMode::EditHostFiles(draft) = state.mode else {
            panic!("Files editor changed kind")
        };
        assert_eq!(draft.account.as_deref(), Some("alice-key"));
        assert_eq!(draft.locator, "alice-build.use");
        assert!(draft.profile().unwrap().as_dev_tunnel_files().is_some());
        harness.ctx.data_mut(|data| {
            data.insert_temp(profiles_state_id(tab), ProfilesScreenState::default())
        });
        assert!(!select_discovered_files_host(
            &harness.ctx,
            tab,
            request,
            &host
        ));
    }
}
