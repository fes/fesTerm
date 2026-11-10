//! Offline presentation of the shared public recipe model.

use super::*;
use festerm_config::host_recipes::{
    AccountReference, ConnectionBehavior, DuplicatePolicy, GenerationId, HostReference, Lifetime,
    LogicalSessionId, MetadataCommand, MetadataOutcome, PublicRecipe, Recipe, RecipeError,
    RecipeId, MAX_PORTABLE_BYTES,
};

const REVIEW_NOTICE: &str =
    "Execution review required. Host/account identity and trust have not been verified here. Imported metadata never grants authority.";
const OFFLINE_NOTICE: &str =
    "Experimental · rendering and metadata editing stay offline. Review and connect is a separate foreground action; saving or importing never authorizes execution.";

#[derive(Clone, Default)]
pub(super) struct HostRecipesState {
    page: RecipePage,
    pending: Option<egui::Id>,
    feedback: Option<&'static str>,
}

#[derive(Clone, Default)]
enum RecipePage {
    #[default]
    List,
    Edit(Box<RecipeDraft>),
    Import {
        text: zeroize::Zeroizing<String>,
        preview: Option<PublicRecipe>,
        reviewed: bool,
    },
    Duplicate {
        source: PublicRecipe,
        name: String,
    },
    Delete {
        recipe: PublicRecipe,
        focus_cancel: bool,
    },
    Export(String),
}

#[derive(Clone)]
struct RecipeDraft {
    original: Option<PublicRecipe>,
    id: RecipeId,
    logical: LogicalSessionId,
    name: String,
    host: String,
    account: String,
    shell: String,
    arguments: String,
    cwd: String,
    lifetime: Lifetime,
    behavior: ConnectionBehavior,
    generation: String,
}

impl Default for RecipeDraft {
    fn default() -> Self {
        Self {
            original: None,
            id: RecipeId::new(),
            logical: LogicalSessionId::new(),
            name: String::new(),
            host: String::new(),
            account: String::new(),
            shell: String::new(),
            arguments: "[]".into(),
            cwd: String::new(),
            lifetime: Lifetime::Persistent {},
            behavior: ConnectionBehavior::default(),
            generation: String::new(),
        }
    }
}

impl RecipeDraft {
    fn from_recipe(recipe: PublicRecipe) -> Self {
        let AccountReference::Github { account_id } = recipe.host.account;
        Self {
            id: recipe.id.clone(),
            logical: recipe.logical_session.clone(),
            name: recipe.name.clone(),
            host: recipe.host.id.to_string(),
            account: account_id.to_string(),
            shell: recipe.launch.shell.clone().unwrap_or_default(),
            arguments: serde_json::to_string(&recipe.launch.args).expect("string vector"),
            cwd: recipe.launch.cwd.clone().unwrap_or_default(),
            lifetime: recipe.lifetime,
            behavior: recipe.behavior.clone(),
            generation: match recipe.behavior {
                ConnectionBehavior::AttachOnly { generation } => generation.get().to_string(),
                _ => String::new(),
            },
            original: Some(recipe),
        }
    }

    fn command(&self) -> Result<MetadataCommand, RecipeError> {
        let behavior = if matches!(self.behavior, ConnectionBehavior::AttachOnly { .. }) {
            ConnectionBehavior::AttachOnly {
                generation: GenerationId::try_from(self.generation.clone())
                    .map_err(|_| RecipeError::Invalid)?,
            }
        } else {
            self.behavior.clone()
        };
        let recipe = PublicRecipe {
            id: self.id.clone(),
            logical_session: self.logical.clone(),
            name: self.name.clone(),
            host: HostReference {
                id: self.host.parse().map_err(|_| RecipeError::Invalid)?,
                account: AccountReference::Github {
                    account_id: self.account.parse().map_err(|_| RecipeError::Invalid)?,
                },
            },
            launch: Recipe {
                shell: (!self.shell.is_empty()).then(|| self.shell.clone()),
                args: serde_json::from_str(&self.arguments).map_err(|_| RecipeError::Invalid)?,
                cwd: (!self.cwd.is_empty()).then(|| self.cwd.clone()),
            },
            lifetime: self.lifetime,
            behavior,
        };
        recipe.validate()?;
        Ok(if self.original.is_some() {
            MetadataCommand::Edit(recipe)
        } else {
            MetadataCommand::Save(recipe)
        })
    }
}

fn field(ui: &mut Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .id_salt(label)
            .hint_text(label)
            .char_limit(MAX_PORTABLE_BYTES)
            .desired_width(f32::INFINITY),
    );
}

fn behavior_label(behavior: &ConnectionBehavior) -> &'static str {
    match behavior {
        ConnectionBehavior::ChooseSession => "Choose a session on this host",
        ConnectionBehavior::ResumeOrCreate => "Resume logical session or create if absent",
        ConnectionBehavior::AlwaysCreate => "Always create a new shell",
        ConnectionBehavior::AttachOnly { .. } => "Attach only to exact running generation",
    }
}

fn identity(ui: &mut Ui, recipe: &PublicRecipe) {
    let AccountReference::Github { account_id } = recipe.host.account;
    ui.label(format!("Host ID: {}", recipe.host.id));
    ui.label(format!("GitHub account ID (unverified hint): {account_id}"));
    ui.label(format!("Logical session ID: {}", recipe.logical_session));
    ui.label(format!("Local recipe ID: {}", recipe.id.as_str()));
    ui.label(behavior_label(&recipe.behavior));
    if let ConnectionBehavior::AttachOnly { generation } = recipe.behavior {
        ui.label(format!("Exact generation: {}", generation.get()));
    }
    ui.label(REVIEW_NOTICE);
}

fn launch_summary(ui: &mut Ui, recipe: &PublicRecipe) {
    ui.label(format!(
        "Shell: {}",
        recipe.launch.shell.as_deref().unwrap_or("(host default)")
    ));
    ui.label(format!(
        "Arguments (JSON vector): {}",
        serde_json::to_string(&recipe.launch.args).expect("string vector")
    ));
    ui.label(format!(
        "Remote working directory: {}",
        recipe.launch.cwd.as_deref().unwrap_or("(host default)")
    ));
    ui.label(match recipe.lifetime {
        Lifetime::Persistent {} => "Lifetime: keep running after disconnect",
        Lifetime::Temporary { .. } => {
            "Lifetime: temporary; intentional disconnect ends shell, loss has 60-second grace"
        }
    });
}

impl HostRecipesState {
    fn submit(&mut self, tab_id: TabId, metadata: MetadataCommand) -> AppCommand {
        let request = egui::Id::new(uuid::Uuid::new_v4());
        self.pending = Some(request);
        self.feedback = None;
        AppCommand::HostRecipeFromProfiles {
            tab_id,
            request,
            command: metadata,
        }
    }

    pub(super) fn show(
        &mut self,
        ui: &mut Ui,
        tab_id: TabId,
        configuration: &festerm_config::Configuration,
        command: &mut Option<AppCommand>,
    ) -> bool {
        let mut back = false;
        let mut next = None;
        let mut intent = None;
        let mut review = None;
        show_bounded_content_scroll(ui, (tab_id, "host_recipe_metadata"), |ui| {
            ui.heading("Host/session recipes · Experimental");
            ui.label(OFFLINE_NOTICE);
            ui.label("Use public IDs and launch options only. Never enter credentials, private keys or bearer tokens.");
            if let Some(message) = self.feedback {
                ui.label(message);
            }
            ui.add_enabled_ui(self.pending.is_none(), |ui| {
                match &mut self.page {
                    RecipePage::List => {
                        ui.horizontal_wrapped(|ui| {
                            if action_button(ui, ActionButtonRole::Secondary, "Back to Profiles").clicked() {
                                back = true;
                            }
                            if action_button(ui, ActionButtonRole::Accent, "Add recipe").clicked() {
                                next = Some(RecipePage::Edit(Box::default()));
                            }
                            if action_button(ui, ActionButtonRole::Secondary, "Import recipe").clicked() {
                                next = Some(RecipePage::Import {
                                    text: Default::default(), preview: None, reviewed: false,
                                });
                            }
                        });
                        if configuration.host_recipes().is_empty() {
                            ui.label("No saved host/session recipes.");
                        }
                        for recipe in configuration.host_recipes().recipes() {
                            ui.push_id(recipe.id.as_str(), |ui| {
                                ui.group(|ui| {
                                    ui.strong(&recipe.name);
                                    identity(ui, recipe);
                                    ui.horizontal_wrapped(|ui| {
                                        if ui.button("Review and connect…").clicked() {
                                            review = Some(recipe.clone());
                                        }
                                        if ui.button("Edit metadata").clicked() {
                                            next = Some(RecipePage::Edit(Box::new(RecipeDraft::from_recipe(recipe.clone()))));
                                        }
                                        if ui.button("Duplicate…").clicked() {
                                            next = Some(RecipePage::Duplicate { source: recipe.clone(), name: format!("{} copy", recipe.name) });
                                        }
                                        if ui.button("Export recipe").clicked() {
                                            intent = Some(MetadataCommand::Export(recipe.id.clone()));
                                        }
                                        if ui.button("Delete recipe…").clicked() {
                                            next = Some(RecipePage::Delete { recipe: recipe.clone(), focus_cancel: true });
                                        }
                                    });
                                });
                            });
                        }
                    }
                    RecipePage::Edit(draft) => {
                        ui.label("Edits affect future creation only, never a running process. Host and logical identity cannot be retargeted by editing.");
                        field(ui, "Recipe name", &mut draft.name);
                        ui.add_enabled_ui(draft.original.is_none(), |ui| {
                            field(ui, "Host ID (canonical UUID)", &mut draft.host);
                            field(ui, "GitHub numeric account ID (unverified hint)", &mut draft.account);
                        });
                        ui.label(format!("Logical session ID: {}", draft.logical));
                        ui.label(REVIEW_NOTICE);
                        field(ui, "Remote shell (blank: host default)", &mut draft.shell);
                        field(ui, "Arguments (JSON string array)", &mut draft.arguments);
                        field(ui, "Remote working directory (blank: host default)", &mut draft.cwd);
                        let mut persistent = matches!(draft.lifetime, Lifetime::Persistent {});
                        if ui.checkbox(&mut persistent, "Keep running after disconnect").changed() {
                            draft.lifetime = if persistent { Lifetime::Persistent {} } else { Lifetime::Temporary { reconnect_grace_seconds: 60 } };
                        }
                        if !persistent {
                            ui.label("Temporary: intentional disconnect ends the shell; unexpected loss has a 60-second reconnection grace. This screen does not start a shell.");
                        }
                        egui::ComboBox::from_id_salt("recipe_connection_behavior")
                            .selected_text(behavior_label(&draft.behavior))
                            .show_ui(ui, |ui| {
                                for behavior in [
                                    ConnectionBehavior::ChooseSession,
                                    ConnectionBehavior::ResumeOrCreate,
                                    ConnectionBehavior::AlwaysCreate,
                                    ConnectionBehavior::AttachOnly { generation: GenerationId::new(1).expect("nonzero") },
                                ] {
                                    let label = behavior_label(&behavior);
                                    ui.selectable_value(&mut draft.behavior, behavior, label);
                                }
                            });
                        if matches!(draft.behavior, ConnectionBehavior::AttachOnly { .. }) {
                            field(ui, "Exact generation (positive decimal)", &mut draft.generation);
                        }
                        ui.horizontal_wrapped(|ui| {
                            if action_button(ui, ActionButtonRole::Secondary, "Cancel").clicked() {
                                next = Some(RecipePage::List);
                            }
                            if action_button(ui, ActionButtonRole::Accent, "Save metadata").clicked() {
                                match draft.command() {
                                    Ok(metadata) => intent = Some(metadata),
                                    Err(error) => self.feedback = Some(error.message()),
                                }
                            }
                        });
                    }
                    RecipePage::Import { text, preview, reviewed } => {
                        if let Some(recipe) = preview {
                            ui.heading("Review imported public metadata");
                            ui.strong(&recipe.name);
                            identity(ui, recipe);
                            launch_summary(ui, recipe);
                            ui.checkbox(reviewed, "I reviewed the metadata; this is not execution or trust approval");
                            ui.horizontal_wrapped(|ui| {
                                if action_button(ui, ActionButtonRole::Secondary, "Cancel").clicked() {
                                    next = Some(RecipePage::List);
                                }
                                if action_button_enabled(ui, *reviewed, ActionButtonRole::Accent, "Import metadata").clicked() {
                                    intent = Some(MetadataCommand::Import(text.to_string()));
                                }
                            });
                        } else {
                            ui.label("Paste exported public recipe JSON. Nothing is imported until review and confirmation.");
                            ui.add(egui::TextEdit::multiline(&mut **text).hint_text("Public recipe JSON").char_limit(MAX_PORTABLE_BYTES).desired_rows(8).desired_width(f32::INFINITY));
                            ui.horizontal_wrapped(|ui| {
                                if action_button(ui, ActionButtonRole::Secondary, "Cancel").clicked() {
                                    next = Some(RecipePage::List);
                                }
                                if action_button(ui, ActionButtonRole::Accent, "Preview metadata").clicked() {
                                    match PublicRecipe::import(text) {
                                        Ok(recipe) => {
                                            *preview = Some(recipe);
                                            *reviewed = false;
                                            self.feedback = None;
                                        }
                                        Err(error) => self.feedback = Some(error.message()),
                                    }
                                }
                            });
                        }
                    }
                    RecipePage::Duplicate { source, name } => {
                        ui.heading("Duplicate recipe metadata");
                        identity(ui, source);
                        field(ui, "Copy name", name);
                        ui.label("A new recipe copy receives a fresh logical session ID. An exact-session shortcut deliberately retains the original logical ID and generation.");
                        ui.horizontal_wrapped(|ui| {
                            if action_button(ui, ActionButtonRole::Secondary, "Cancel").clicked() {
                                next = Some(RecipePage::List);
                            }
                            if ui.button("Save new logical copy").clicked() {
                                intent = Some(MetadataCommand::Duplicate { source: source.id.clone(), name: name.clone(), policy: DuplicatePolicy::FreshLogicalSession });
                            }
                            if let ConnectionBehavior::AttachOnly { generation } = source.behavior {
                                if ui.button("Save exact-session shortcut").clicked() {
                                    intent = Some(MetadataCommand::Duplicate { source: source.id.clone(), name: name.clone(), policy: DuplicatePolicy::ExistingSession { generation } });
                                }
                            }
                        });
                    }
                    RecipePage::Delete { recipe, focus_cancel } => {
                        ui.heading("Delete local recipe?");
                        ui.strong(&recipe.name);
                        identity(ui, recipe);
                        ui.label("Only local recipe metadata is removed. No remote shell is ended and no host, account or trust is changed.");
                        let current = configuration.host_recipes().get(&recipe.id) == Some(recipe);
                        if !current {
                            ui.label("Recipe changed or was removed. Cancel and reopen deletion to review current metadata.");
                        }
                        let cancelled = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                        if cancelled {
                            next = Some(RecipePage::List);
                        }
                        ui.horizontal_wrapped(|ui| {
                            let cancel = action_button(ui, ActionButtonRole::Secondary, "Cancel");
                            if *focus_cancel {
                                cancel.request_focus();
                                *focus_cancel = false;
                            }
                            if cancel.clicked() {
                                next = Some(RecipePage::List);
                            }
                            if action_button_enabled(ui, current && !cancelled, ActionButtonRole::DangerOutline, "Delete metadata").clicked() {
                                intent = Some(MetadataCommand::Delete(recipe.id.clone()));
                            }
                        });
                    }
                    RecipePage::Export(text) => {
                        ui.heading("Export public recipe metadata");
                        ui.label("Public recipe JSON only. No host consent, execution approval or credentials are exported.");
                        ui.add(egui::TextEdit::multiline(text).interactive(false).desired_rows(12).desired_width(f32::INFINITY));
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Copy public recipe JSON").clicked() {
                                ui.ctx().copy_text(text.clone());
                            }
                            if action_button(ui, ActionButtonRole::Secondary, "Back to recipes").clicked() {
                                next = Some(RecipePage::List);
                            }
                        });
                    }
                }
            });
        });
        if let Some(page) = next {
            self.page = page;
            self.feedback = None;
        }
        if let Some(metadata) = intent {
            *command = Some(self.submit(tab_id, metadata));
        } else if let Some(recipe) = review {
            let request = egui::Id::new(uuid::Uuid::new_v4());
            self.pending = Some(request);
            self.feedback = None;
            *command = Some(AppCommand::ReviewHostFromProfiles {
                tab_id,
                request,
                recipe: Box::new(recipe),
            });
        }
        back
    }
}

pub(crate) fn host_recipe_request_is_current(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
) -> bool {
    context.data(|data| {
        data.get_temp::<ProfilesScreenState>(profiles_state_id(tab))
            .is_some_and(|state| matches!(state.mode, ProfilesScreenMode::HostRecipes(recipes) if recipes.pending == Some(request)))
    })
}

pub(crate) fn report_host_recipe_outcome(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
    outcome: Result<MetadataOutcome, &'static str>,
) {
    context.data_mut(|data| {
        let id = profiles_state_id(tab);
        let Some(mut state) = data.get_temp::<ProfilesScreenState>(id) else {
            return;
        };
        let ProfilesScreenMode::HostRecipes(recipes) = &mut state.mode else {
            return;
        };
        if recipes.pending != Some(request) {
            return;
        }
        recipes.pending = None;
        match outcome {
            Ok(MetadataOutcome::Exported(text)) => {
                recipes.page = RecipePage::Export(text);
                recipes.feedback = Some("Public metadata exported. No configuration was written.");
            }
            Ok(_) => {
                recipes.page = RecipePage::List;
                recipes.feedback = Some(
                    "Local recipe metadata saved. No connection or remote session was changed.",
                );
            }
            Err(message) => recipes.feedback = Some(message),
        }
        data.insert_temp(id, state);
    });
    context.request_repaint();
}

pub(crate) fn report_host_connection_started(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
) {
    context.data_mut(|data| {
        let id = profiles_state_id(tab);
        let Some(mut state) = data.get_temp::<ProfilesScreenState>(id) else { return };
        let ProfilesScreenMode::HostRecipes(recipes) = &mut state.mode else { return };
        if recipes.pending != Some(request) { return }
        recipes.pending = None;
        recipes.feedback = Some(
            "Connecting the approved existing session. Attached is reported only after frontend recovery and host activation.",
        );
        data.insert_temp(id, state);
    });
    context.request_repaint();
}

#[cfg(test)]
pub(crate) fn install_host_recipe_request_for_test(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
) {
    context.data_mut(|data| {
        data.insert_temp(
            profiles_state_id(tab),
            ProfilesScreenState {
                mode: ProfilesScreenMode::HostRecipes(Box::new(HostRecipesState {
                    pending: Some(request),
                    ..Default::default()
                })),
                ..Default::default()
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::AppState;
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };

    struct State {
        tab: TabId,
        configuration: festerm_config::Configuration,
        command: Option<AppCommand>,
    }

    fn harness(configuration: festerm_config::Configuration) -> Harness<'static, State> {
        Harness::builder()
            .with_size(egui::vec2(720.0, 1000.0))
            .build_ui_state(
                |ui, state: &mut State| {
                    if let Some(command) = show_profiles(
                        ui,
                        state.tab,
                        &state.configuration,
                        None,
                        None,
                        PersistenceProviderKind::FestermSessiond,
                    ) {
                        state.command = Some(command);
                    }
                },
                State {
                    tab: AppState::for_test().active(),
                    configuration,
                    command: None,
                },
            )
    }

    fn install(harness: &Harness<'_, State>, page: RecipePage) {
        let tab = harness.state().tab;
        harness.ctx.data_mut(|data| {
            data.insert_temp(
                profiles_state_id(tab),
                ProfilesScreenState {
                    mode: ProfilesScreenMode::HostRecipes(Box::new(HostRecipesState {
                        page,
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            );
        });
    }

    fn recipe() -> PublicRecipe {
        PublicRecipe::persistent(
            "Remote build",
            HostReference {
                id: "00000000-0000-4000-8000-000000000001".parse().unwrap(),
                account: AccountReference::Github { account_id: 42 },
            },
            Recipe {
                shell: Some("/bin/sh".into()),
                args: vec!["-c".into(), "printf 'one two'".into(), String::new()],
                cwd: Some("/project".into()),
            },
        )
        .unwrap()
    }

    #[test]
    fn dev_tunnel_host_recipe_profiles_navigation_is_metadata_only() {
        let configuration = festerm_config::Configuration::new(vec![Profile::dev_tunnel(
            "legacy",
            "legacy-public.json",
        )
        .unwrap()])
        .unwrap();
        let mut harness = harness(configuration.clone());
        harness
            .get_by_label("Host/session recipes · Experimental")
            .click();
        harness.run_ok();
        harness.get_by_label(OFFLINE_NOTICE);
        harness.get_by_label("Add recipe").click();
        harness.run_ok();
        harness.get_by_label(REVIEW_NOTICE);
        assert!(harness.state().command.is_none());
        assert_eq!(harness.state().configuration, configuration);
        harness.get_by_label("Cancel").click();
        harness.run_ok();
        assert!(harness.state().command.is_none());
    }

    #[test]
    fn selected_host_recipe_button_emits_foreground_review_not_approved_attachment() {
        let recipe = recipe();
        let (configuration, _) = festerm_config::Configuration::empty()
            .with_host_recipe_command(MetadataCommand::Save(recipe.clone()))
            .unwrap();
        let mut harness = harness(configuration.clone());
        install(&harness, RecipePage::List);
        harness.run_ok();
        assert!(harness.state().command.is_none());
        harness.get_by_label("Review and connect…").click();
        harness.run_ok();
        let Some(AppCommand::ReviewHostFromProfiles {
            tab_id,
            request,
            recipe: selected,
        }) = harness.state().command.as_ref()
        else {
            panic!("foreground intent, never protected authority")
        };
        assert_eq!(selected.as_ref(), &recipe);
        assert!(host_recipe_request_is_current(
            &harness.ctx,
            *tab_id,
            *request
        ));
        assert_eq!(harness.state().configuration, configuration);
    }

    #[test]
    fn dev_tunnel_host_recipe_editor_routes_typed_save_and_retains_failed_draft() {
        let original = recipe();
        let mut draft = RecipeDraft::from_recipe(original.clone());
        draft.name = "Future recipe".into();
        let mut harness = harness(festerm_config::Configuration::empty());
        install(&harness, RecipePage::Edit(Box::new(draft)));
        harness.run_ok();
        harness.get_by_label("Save metadata").click();
        harness.run_ok();
        let Some(AppCommand::HostRecipeFromProfiles {
            tab_id,
            request,
            command: MetadataCommand::Edit(edit),
        }) = harness.state().command.as_ref()
        else {
            panic!("typed edit only")
        };
        assert_eq!(edit.launch.args, original.launch.args);
        assert_eq!(edit.host, original.host);
        assert_eq!(edit.logical_session, original.logical_session);
        let (tab, request) = (*tab_id, *request);
        report_host_recipe_outcome(
            &harness.ctx,
            tab,
            egui::Id::new("stale"),
            Ok(MetadataOutcome::Deleted(original.id.clone())),
        );
        assert!(host_recipe_request_is_current(&harness.ctx, tab, request));
        report_host_recipe_outcome(
            &harness.ctx,
            tab,
            request,
            Err(RecipeError::Duplicate.message()),
        );
        harness.run_ok();
        harness.get_by_label(RecipeError::Duplicate.message());
        harness.get_by_label("Save metadata");
        let state = harness
            .ctx
            .data(|data| data.get_temp::<ProfilesScreenState>(profiles_state_id(tab)))
            .unwrap();
        let ProfilesScreenMode::HostRecipes(state) = state.mode else {
            panic!("recipe screen")
        };
        let RecipePage::Edit(draft) = state.page else {
            panic!("failed draft retained")
        };
        assert_eq!(draft.name, "Future recipe");
        assert_eq!(
            draft.arguments,
            serde_json::to_string(&original.launch.args).unwrap()
        );
        assert!(harness.state().configuration.host_recipes().is_empty());
    }

    #[test]
    fn dev_tunnel_host_recipe_import_requires_review_and_reports_content_free_errors() {
        let recipe = recipe();
        let text = recipe.export().unwrap();
        let mut harness = harness(festerm_config::Configuration::empty());
        install(
            &harness,
            RecipePage::Import {
                text: text.clone().into(),
                preview: None,
                reviewed: false,
            },
        );
        harness.run_ok();
        harness.get_by_label("Preview metadata").click();
        harness.run_ok();
        harness.get_by_label("Review imported public metadata");
        harness.get_by_label(REVIEW_NOTICE);
        assert!(harness.state().command.is_none());
        assert!(harness
            .get_by_label("Import metadata")
            .accesskit_node()
            .is_disabled());
        harness
            .get_by_label("I reviewed the metadata; this is not execution or trust approval")
            .click();
        harness.run_ok();
        harness.get_by_label("Import metadata").click();
        harness.run_ok();
        assert!(matches!(harness.state().command.as_ref(),
            Some(AppCommand::HostRecipeFromProfiles { command: MetadataCommand::Import(import), .. }) if import == &text));
        assert!(harness.state().configuration.host_recipes().is_empty());

        harness.state_mut().command = None;
        install(
            &harness,
            RecipePage::Import {
                text: "{\"secret\":\"DO_NOT_ECHO\"}".to_owned().into(),
                preview: None,
                reviewed: false,
            },
        );
        harness.run_ok();
        harness.get_by_label("Preview metadata").click();
        harness.run_ok();
        harness.get_by_label(RecipeError::Invalid.message());
        assert!(harness.state().command.is_none());
    }

    #[test]
    fn dev_tunnel_host_recipe_duplicate_routes_fresh_or_deliberate_exact_identity() {
        let mut source = recipe();
        let generation = GenerationId::new(73).unwrap();
        source.behavior = ConnectionBehavior::AttachOnly { generation };
        let (configuration, _) = festerm_config::Configuration::empty()
            .with_host_recipe_command(MetadataCommand::Save(source.clone()))
            .unwrap();
        let mut harness = harness(configuration.clone());
        for (label, exact) in [
            ("Save new logical copy", false),
            ("Save exact-session shortcut", true),
        ] {
            install(
                &harness,
                RecipePage::Duplicate {
                    source: source.clone(),
                    name: "Copy".into(),
                },
            );
            harness.run_ok();
            harness.get_by_label(label).click();
            harness.run_ok();
            let Some(AppCommand::HostRecipeFromProfiles { command, .. }) =
                harness.state_mut().command.take()
            else {
                panic!("metadata duplicate")
            };
            let (candidate, _) = configuration.with_host_recipe_command(command).unwrap();
            let copy = &candidate.host_recipes().recipes()[1];
            assert_ne!(copy.id, source.id);
            assert_eq!(copy.host, source.host);
            assert_eq!(copy.logical_session == source.logical_session, exact);
            assert_eq!(
                matches!(copy.behavior, ConnectionBehavior::AttachOnly { generation: actual } if actual == generation),
                exact
            );
            assert_eq!(harness.state().configuration, configuration);
        }
    }

    #[test]
    fn dev_tunnel_host_recipe_export_and_confirmed_delete_route_without_ui_mutation() {
        let recipe = recipe();
        let (configuration, _) = festerm_config::Configuration::empty()
            .with_host_recipe_command(MetadataCommand::Save(recipe.clone()))
            .unwrap();
        let mut harness = harness(configuration.clone());
        install(&harness, RecipePage::List);
        harness.run_ok();
        harness.get_by_label("Export recipe").click();
        harness.run_ok();
        let Some(AppCommand::HostRecipeFromProfiles {
            tab_id,
            request,
            command: MetadataCommand::Export(id),
        }) = harness.state().command.as_ref()
        else {
            panic!("typed export")
        };
        assert_eq!(id, &recipe.id);
        report_host_recipe_outcome(
            &harness.ctx,
            *tab_id,
            *request,
            Ok(MetadataOutcome::Exported(recipe.export().unwrap())),
        );
        harness.run_ok();
        harness.get_by_label("Public metadata exported. No configuration was written.");
        harness.get_by_label("Copy public recipe JSON");
        harness.get_by_label("Back to recipes").click();
        harness.run_ok();
        harness.state_mut().command = None;
        harness.get_by_label("Delete recipe…").click();
        harness.run_ok();
        assert!(harness.state().command.is_none());
        assert!(harness.get_by_label("Cancel").is_focused());
        harness.key_press(egui::Key::Escape);
        harness.run_ok();
        assert!(harness.state().command.is_none());
        harness.get_by_label("Edit metadata");
        harness.get_by_label("Delete recipe…").click();
        harness.run_ok();
        assert!(harness.get_by_label("Cancel").is_focused());
        harness.get_by_label("Delete metadata").click();
        harness.run_ok();
        assert!(matches!(harness.state().command.as_ref(),
            Some(AppCommand::HostRecipeFromProfiles { command: MetadataCommand::Delete(id), .. }) if id == &recipe.id));
        assert_eq!(harness.state().configuration, configuration);
        harness.state_mut().command = None;
        install(
            &harness,
            RecipePage::Delete {
                recipe: recipe.clone(),
                focus_cancel: true,
            },
        );
        harness.state_mut().configuration = festerm_config::Configuration::empty();
        harness.run_ok();
        assert!(harness
            .get_by_label("Delete metadata")
            .accesskit_node()
            .is_disabled());
        assert!(harness.state().command.is_none());
    }
}
