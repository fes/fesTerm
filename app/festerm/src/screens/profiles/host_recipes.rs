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

#[cfg(test)]
impl crate::tabs::AppState {
    pub(crate) fn gallery_host_recipes(ui: &mut Ui, scene: &str) {
        let mut recipe = PublicRecipe::persistent(
            "Synthetic build recipe",
            HostReference {
                id: "11111111-1111-4111-8111-111111111111".parse().unwrap(),
                account: AccountReference::Github { account_id: 42 },
            },
            Recipe {
                shell: Some("/bin/sh".into()),
                args: vec!["-l".into()],
                cwd: Some("/work/synthetic".into()),
            },
        )
        .unwrap();
        recipe.id = RecipeId::parse("22222222-2222-4222-8222-222222222222").unwrap();
        recipe.logical_session = "33333333-3333-4333-8333-333333333333".parse().unwrap();
        match scene {
            "edit-temporary" => {
                recipe.lifetime = Lifetime::Temporary {
                    reconnect_grace_seconds: 60,
                }
            }
            "edit-attach-only" => {
                recipe.behavior = ConnectionBehavior::AttachOnly {
                    generation: "7".to_owned().try_into().unwrap(),
                }
            }
            "edit-always-create" => recipe.behavior = ConnectionBehavior::AlwaysCreate,
            "edit-resume" => recipe.behavior = ConnectionBehavior::ResumeOrCreate,
            _ => {}
        }
        let (configuration, _) = festerm_config::Configuration::empty()
            .with_host_recipe_command(MetadataCommand::Save(recipe.clone()))
            .unwrap();
        let portable = recipe.export().unwrap();
        let page = match scene {
            "list" | "error" => RecipePage::List,
            "add" => RecipePage::Edit(Box::default()),
            "edit" | "edit-temporary" | "edit-attach-only" | "edit-always-create"
            | "edit-resume" => RecipePage::Edit(Box::new(RecipeDraft::from_recipe(recipe.clone()))),
            "duplicate" => RecipePage::Duplicate {
                source: recipe.clone(),
                name: "Synthetic build recipe copy".into(),
            },
            "delete" => RecipePage::Delete {
                recipe: recipe.clone(),
                focus_cancel: true,
            },
            "import" | "import-review" | "import-invalid" | "import-reviewed" => {
                RecipePage::Import {
                    text: zeroize::Zeroizing::new(if scene == "import-invalid" {
                        "{\"unverified\":".into()
                    } else {
                        portable.clone()
                    }),
                    preview: matches!(scene, "import-review" | "import-reviewed")
                        .then_some(recipe.clone()),
                    reviewed: scene == "import-reviewed",
                }
            }
            "export" => RecipePage::Export(portable),
            _ => unreachable!("unknown recipe scene"),
        };
        let mut state = HostRecipesState {
            page,
            feedback: if scene == "import-invalid" {
                Some("The portable recipe is invalid or exceeds its bounded limits.")
            } else {
                (scene == "error")
                    .then_some("The recipe could not be saved. The original metadata is unchanged.")
            },
            ..HostRecipesState::default()
        };
        let _ = state.show(ui, Self::for_test().active(), &configuration, &mut None);
    }
}

#[cfg(test)]
impl crate::tabs::AppState {
    /// Installs a synthetic New Dev Tunnel profile editor for the screenshot
    /// gallery and returns its two-account configuration. Nothing connects.
    pub(crate) fn gallery_host_picker(
        context: &egui::Context,
        tab: TabId,
        scene: &str,
    ) -> festerm_config::Configuration {
        use crate::screens::host_workspace::{HostSessions, SavedSessionChoice};
        let account = |identifier: &str, id: u64, login: &str| {
            festerm_config::DevTunnelAccount::new(
                identifier,
                festerm_config::AccountProvider::Github,
                id,
                login,
                festerm_secret_store::SecretReference::generate(),
            )
            .expect("synthetic account")
        };
        let configuration = festerm_config::Configuration::empty()
            .with_account(account("alice-key", 1, "alice"))
            .unwrap()
            .0
            .with_account(account("bob-key", 2, "bob"))
            .unwrap()
            .0;
        let host = HostSessions {
            reference: HostReference {
                id: "11111111-1111-4111-8111-111111111111".parse().unwrap(),
                account: AccountReference::Github { account_id: 1 },
            },
            hint: festerm_config::host_recipes::HostHint {
                name: "Build workstation".into(),
                locator: "alice-build.use".into(),
            },
            sessions: [("build", 3), ("notes", 4), ("release", 5)]
                .into_iter()
                .map(|(name, generation)| SavedSessionChoice {
                    target: festerm_tunnel_bridge::host_client::Target {
                        logical: LogicalSessionId::new(),
                        generation: GenerationId::new(generation).unwrap(),
                    },
                    name: name.into(),
                    lifetime: Lifetime::Persistent {},
                })
                .collect(),
        };
        crate::screens::host_workspace::install_profile_picker_gallery(context, tab, scene);
        let mut draft = RecipeDraft {
            name: "Build shortcut".into(),
            hosts_refreshed: true,
            ..Default::default()
        };
        match scene {
            "session-open" | "session-existing" | "session-new" => {
                crate::screens::host_workspace::remember_host(context, host.clone());
                draft.bind_host(&host);
                match scene {
                    "session-existing" => draft.bind_session(&host.sessions[0]),
                    "session-new" => {
                        draft.session_name = "deploy-review".into();
                        draft.reconcile_session_name(&host.sessions);
                    }
                    _ => {}
                }
            }
            "session-locked" => {
                let mut recipe = PublicRecipe::persistent(
                    "Build shortcut",
                    host.reference.clone(),
                    Recipe::default(),
                )
                .unwrap();
                recipe.host_hint = Some(host.hint.clone());
                recipe.session_name = Some("build".into());
                draft = RecipeDraft::from_recipe(recipe);
            }
            _ => {}
        }
        context.data_mut(|data| {
            data.insert_temp(
                profiles_state_id(tab),
                ProfilesScreenState {
                    mode: ProfilesScreenMode::HostRecipes(Box::new(HostRecipesState {
                        page: RecipePage::Edit(Box::new(draft)),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            )
        });
        configuration
    }
}

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
    session_name: String,
    host_hint: Option<festerm_config::host_recipes::HostHint>,
    host: String,
    account: String,
    shell: String,
    arguments: String,
    cwd: String,
    lifetime: Lifetime,
    behavior: ConnectionBehavior,
    generation: String,
    hosts_refreshed: bool,
}

impl Default for RecipeDraft {
    fn default() -> Self {
        Self {
            original: None,
            id: RecipeId::new(),
            logical: LogicalSessionId::new(),
            name: String::new(),
            session_name: String::new(),
            host_hint: None,
            host: String::new(),
            account: String::new(),
            shell: String::new(),
            arguments: "[]".into(),
            cwd: String::new(),
            lifetime: Lifetime::Persistent {},
            behavior: ConnectionBehavior::default(),
            generation: String::new(),
            hosts_refreshed: false,
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
            session_name: recipe.session_name().to_owned(),
            host_hint: recipe.host_hint.clone(),
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
            hosts_refreshed: true,
            original: Some(recipe),
        }
    }

    fn host_reference(&self) -> Option<HostReference> {
        Some(HostReference {
            id: self.host.parse().ok()?,
            account: AccountReference::Github {
                account_id: self.account.parse().ok()?,
            },
        })
    }

    fn bind_host(&mut self, host: &crate::screens::host_workspace::HostSessions) {
        self.host = host.reference.id.to_string();
        let AccountReference::Github { account_id } = host.reference.account;
        self.account = account_id.to_string();
        self.host_hint = Some(host.hint.clone());
        self.logical = LogicalSessionId::new();
        self.generation.clear();
        self.lifetime = Lifetime::Persistent {};
        self.reconcile_session_name(&host.sessions);
    }

    fn bind_session(&mut self, session: &crate::screens::host_workspace::SavedSessionChoice) {
        self.logical = session.target.logical.clone();
        self.generation = session.target.generation.get().to_string();
        self.session_name = session.name.clone();
        self.lifetime = session.lifetime;
        self.behavior = ConnectionBehavior::ResumeOnly {};
    }

    /// The typed name selects an existing session only when exactly one
    /// listed session carries it; anything else is a new session that is
    /// created on first connect.
    fn reconcile_session_name(
        &mut self,
        sessions: &[crate::screens::host_workspace::SavedSessionChoice],
    ) {
        let name = self.session_name.trim();
        if sessions
            .iter()
            .any(|session| session.target.logical == self.logical && session.name == name)
        {
            return;
        }
        let mut matches = sessions.iter().filter(|session| session.name == name);
        if let (Some(session), None) = (matches.next(), matches.next()) {
            let session = session.clone();
            self.bind_session(&session);
            return;
        }
        if sessions
            .iter()
            .any(|session| session.target.logical == self.logical)
        {
            self.logical = LogicalSessionId::new();
            self.generation.clear();
            self.lifetime = Lifetime::Persistent {};
        }
        self.behavior = ConnectionBehavior::ResumeOrCreate;
    }

    fn bound_session<'a>(
        &self,
        sessions: &'a [crate::screens::host_workspace::SavedSessionChoice],
    ) -> Option<&'a crate::screens::host_workspace::SavedSessionChoice> {
        sessions
            .iter()
            .find(|session| session.target.logical == self.logical)
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
            session_name: if self.original.as_ref().is_some_and(|original| {
                original.session_name.is_none() && original.name == self.session_name
            }) {
                None
            } else {
                Some(self.session_name.trim().to_owned())
            },
            host_hint: self.host_hint.clone(),
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

/// Editable dropdown: type a new session name, or pick an authorized session
/// from the host's last listed inventory.
fn session_name_combo(
    ui: &mut Ui,
    tab: TabId,
    draft: &mut RecipeDraft,
    sessions: &[crate::screens::host_workspace::SavedSessionChoice],
    host_selected: bool,
) {
    let label = ui.label("Session name").id;
    let button_width = 28.0;
    let mut picked = None;
    let text = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let text = ui
                .add(
                    egui::TextEdit::singleline(&mut draft.session_name)
                        .min_size(egui::vec2(0.0, 24.0))
                        .id_salt(("recipe_session_name", tab))
                        .hint_text("Name a new session or choose one")
                        .char_limit(MAX_PORTABLE_BYTES)
                        .desired_width((ui.available_width() - button_width - 4.0).max(80.0)),
                )
                .labelled_by(label);
            let toggle = ui
                .add_enabled(
                    host_selected,
                    egui::Button::new("⏷").min_size(egui::vec2(button_width, 24.0)),
                )
                .on_hover_text("Existing sessions");
            toggle.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    host_selected,
                    "Existing sessions",
                )
            });
            egui::Popup::menu(&toggle)
                .width(text.rect.width() + button_width + 4.0)
                .show(|ui| {
                    if sessions.is_empty() {
                        ui.label(
                            egui::RichText::new("No sessions listed for this host yet.")
                                .color(theme::TEXT_MUTED),
                        );
                    }
                    for session in sessions {
                        ui.push_id(&session.target.logical, |ui| {
                            if ui
                                .selectable_label(
                                    draft.logical == session.target.logical,
                                    &session.name,
                                )
                                .clicked()
                            {
                                picked = Some(session.clone());
                                ui.close();
                            }
                        });
                    }
                });
            text
        })
        .inner;
    if let Some(session) = picked {
        draft.bind_session(&session);
    } else if text.changed() {
        draft.reconcile_session_name(sessions);
    }
    let name = draft.session_name.trim();
    let bound = draft.bound_session(sessions).is_some();
    let duplicates = sessions
        .iter()
        .filter(|session| session.name == name)
        .count();
    let status = if bound {
        Some("Existing session".to_owned())
    } else if duplicates > 1 {
        Some(format!(
            "Several sessions are named “{name}”. Choose one from the list."
        ))
    } else if !name.is_empty() {
        Some("New session · created when you first connect".to_owned())
    } else {
        None
    };
    if let Some(status) = status {
        ui.label(
            egui::RichText::new(status)
                .small()
                .color(theme::TEXT_SECONDARY),
        );
    }
    ui.add_space(4.0);
    let mut create = matches!(draft.behavior, ConnectionBehavior::ResumeOrCreate);
    if ui
        .add_enabled(
            bound,
            egui::Checkbox::new(&mut create, "Create session if missing"),
        )
        .changed()
    {
        draft.behavior = if create {
            ConnectionBehavior::ResumeOrCreate
        } else {
            ConnectionBehavior::ResumeOnly {}
        };
    }
    ui.add_space(8.0);
}

fn field(ui: &mut Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .min_size(egui::vec2(0.0, 24.0))
            .id_salt(label)
            .hint_text(label)
            .char_limit(MAX_PORTABLE_BYTES)
            .desired_width(f32::INFINITY),
    );
    ui.add_space(8.0);
}

fn behavior_label(behavior: &ConnectionBehavior) -> &'static str {
    match behavior {
        ConnectionBehavior::ChooseSession => "Choose a session on this host",
        ConnectionBehavior::ResumeOrCreate => "Resume logical session or create if absent",
        ConnectionBehavior::ResumeOnly {} => "Resume logical session without creating",
        ConnectionBehavior::AlwaysCreate => "Always create a new shell",
        ConnectionBehavior::AttachOnly { .. } => "Attach only to exact running generation",
    }
}

fn identity(ui: &mut Ui, recipe: &PublicRecipe) {
    if let Some(hint) = &recipe.host_hint {
        ui.label(&hint.name);
    }
    ui.label(format!("Session: {}", recipe.session_name()));
    ui.collapsing("Connection details", |ui| {
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
    });
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
    pub(super) fn new_editor() -> Self {
        Self {
            page: RecipePage::Edit(Box::default()),
            ..Default::default()
        }
    }
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
            ui.spacing_mut().interact_size.y = ui.spacing().interact_size.y.max(24.0);
            ui.heading(match &self.page {
                RecipePage::Edit(draft) if draft.original.is_some() => "Edit Dev Tunnel Profile",
                RecipePage::Edit(_) => "New Dev Tunnel Profile",
                _ => "Dev Tunnel profiles",
            });
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
                            if action_button(ui, ActionButtonRole::Accent, "New profile").clicked() {
                                next = Some(RecipePage::Edit(Box::default()));
                            }
                            if action_button(ui, ActionButtonRole::Secondary, "Import recipe").clicked() {
                                next = Some(RecipePage::Import {
                                    text: Default::default(), preview: None, reviewed: false,
                                });
                            }
                        });
                        if configuration.host_recipes().is_empty() {
                            ui.label("No saved Dev Tunnel profiles.");
                        }
                        for recipe in configuration.host_recipes().recipes() {
                            ui.push_id(recipe.id.as_str(), |ui| {
                                ui.group(|ui| {
                                    ui.strong(&recipe.name);
                                    identity(ui, recipe);
                                    ui.horizontal_wrapped(|ui| {
                                        if ui.button("Connect").clicked() {
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
                        crate::software_background::show_frame(ui, egui::Frame::new()
                            .fill(theme::SURFACE_TAB_INACTIVE)
                            .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
                            .corner_radius(8.0).inner_margin(egui::Margin::same(16)), |ui| {
                        ui.set_width(ui.available_width().min(340.0));
                        field(ui, "Profile name", &mut draft.name);
                        if draft.original.is_none() && !draft.hosts_refreshed {
                            draft.hosts_refreshed = true;
                            crate::screens::host_workspace::queue_profile_refresh(ui.ctx(), tab_id, configuration);
                        }
                        let selected = draft.host_reference();
                        match crate::screens::host_workspace::show_profile_host_picker(
                            ui,
                            tab_id,
                            configuration,
                            selected.as_ref(),
                            draft.host_hint.as_ref().map(|hint| hint.name.as_str()),
                            draft.original.is_none(),
                        ) {
                            Some(crate::screens::host_workspace::ProfileHostPick::Bind(host)) => draft.bind_host(&host),
                            Some(crate::screens::host_workspace::ProfileHostPick::Command(next)) => *command = Some(*next),
                            None => {}
                        }
                        ui.add_space(8.0);
                        let sessions = selected
                            .and_then(|reference| crate::screens::host_workspace::remembered_hosts(ui.ctx())
                                .into_iter()
                                .find(|host| host.reference == reference))
                            .map(|host| host.sessions)
                            .unwrap_or_default();
                        if draft.original.is_some() {
                            let mut create = matches!(draft.behavior, ConnectionBehavior::ResumeOrCreate);
                            if ui.checkbox(&mut create, "Create session if missing").changed() {
                                draft.behavior = if create { ConnectionBehavior::ResumeOrCreate }
                                    else { ConnectionBehavior::ResumeOnly {} };
                            }
                            ui.label("Session name");
                            ui.add_enabled(false, egui::TextEdit::singleline(&mut draft.session_name)
                                .min_size(egui::vec2(0.0, 24.0))
                                .desired_width(f32::INFINITY));
                            ui.small("The saved session identity is kept when editing.");
                            ui.add_space(8.0);
                        } else {
                            session_name_combo(ui, tab_id, draft, &sessions, !draft.host.is_empty());
                        }
                        ui.collapsing("Advanced", |ui| {
                            ui.label(OFFLINE_NOTICE);
                            ui.add_enabled_ui(draft.original.is_none(), |ui| {
                                field(ui, "Host ID (canonical UUID)", &mut draft.host);
                                field(ui, "GitHub numeric account ID (unverified hint)", &mut draft.account);
                            });
                            field(ui, "Remote shell (blank: host default)", &mut draft.shell);
                            field(ui, "Arguments (JSON string array)", &mut draft.arguments);
                            field(ui, "Remote working directory (blank: host default)", &mut draft.cwd);
                            egui::ComboBox::from_id_salt("recipe_connection_behavior")
                            .selected_text(behavior_label(&draft.behavior))
                            .show_ui(ui, |ui| {
                                for behavior in [
                                    ConnectionBehavior::ChooseSession,
                                    ConnectionBehavior::ResumeOrCreate,
                                    ConnectionBehavior::ResumeOnly {},
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
                        });
                        ui.horizontal_wrapped(|ui| {
                            if action_button(ui, ActionButtonRole::Secondary, "Cancel").clicked() {
                                next = Some(RecipePage::List);
                            }
                            if action_button_enabled(ui, draft.command().is_ok()
                                && (draft.original.is_some()
                                    || !matches!(draft.behavior, ConnectionBehavior::ResumeOnly {} | ConnectionBehavior::AttachOnly { .. })
                                    || !draft.generation.is_empty()),
                                ActionButtonRole::Accent, "Save").clicked() {
                                match draft.command() {
                                    Ok(metadata) => intent = Some(metadata),
                                    Err(error) => self.feedback = Some(error.message()),
                                }
                            }
                        });
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

pub(crate) fn select_host_for_profile(
    context: &egui::Context,
    tab: TabId,
    request: egui::Id,
    host: crate::screens::host_workspace::HostSessions,
    target: Option<festerm_tunnel_bridge::host_client::Target>,
) -> bool {
    if !crate::screens::host_workspace::profile_request_is_current(context, tab, request) {
        return false;
    }
    context.data_mut(|data| {
        let Some(mut state) = data.get_temp::<ProfilesScreenState>(profiles_state_id(tab)) else {
            return false;
        };
        let ProfilesScreenMode::HostRecipes(editor) = &mut state.mode else {
            return false;
        };
        let RecipePage::Edit(draft) = &mut editor.page else {
            return false;
        };
        if draft.original.is_some() {
            return false;
        }
        let session = match &target {
            Some(target) => {
                let Some(session) = host
                    .sessions
                    .iter()
                    .find(|session| &session.target == target)
                else {
                    return false;
                };
                Some(session)
            }
            None => None,
        };
        draft.host = host.reference.id.to_string();
        let AccountReference::Github { account_id } = host.reference.account;
        draft.account = account_id.to_string();
        draft.host_hint = Some(host.hint.clone());
        match session {
            Some(session) => draft.bind_session(session),
            None => draft.bind_host(&host),
        }
        editor.feedback = None;
        data.insert_temp(profiles_state_id(tab), state);
        true
    })
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
        harness.get_by_label("Dev Tunnel profiles").click();
        harness.run_ok();
        harness.get_by_label("New profile").click();
        harness.run_ok();
        harness.get_by_label("Profile name");
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
        harness.get_by_label("Connect").click();
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
        harness.get_by_label("Save").click();
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
        harness.get_by_label("Save");
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
    fn host_profile_creation_can_be_disabled_without_inventing_a_generation() {
        let original = recipe();
        let mut harness = harness(festerm_config::Configuration::empty());
        install(
            &harness,
            RecipePage::Edit(Box::new(RecipeDraft::from_recipe(original.clone()))),
        );
        harness.run_ok();
        harness.get_by_label("Create session if missing").click();
        harness.run_ok();
        harness.get_by_label("Save").click();
        harness.run_ok();
        let Some(AppCommand::HostRecipeFromProfiles {
            command: MetadataCommand::Edit(edit),
            ..
        }) = harness.state().command.as_ref()
        else {
            panic!("metadata edit")
        };
        assert_eq!(edit.behavior, ConnectionBehavior::ResumeOnly {});
        assert_eq!(edit.logical_session, original.logical_session);
        assert_eq!(edit.host, original.host);
    }

    #[test]
    fn profile_host_selection_returns_to_the_same_unsaved_draft_without_connecting() {
        use crate::screens::host_workspace::{HostSessions, SavedSessionChoice};
        let selected = recipe();
        let target = festerm_tunnel_bridge::host_client::Target {
            logical: selected.logical_session.clone(),
            generation: GenerationId::new(7).unwrap(),
        };
        let host = HostSessions {
            reference: selected.host.clone(),
            hint: festerm_config::host_recipes::HostHint {
                name: "Build host".into(),
                locator: "fixture.use".into(),
            },
            sessions: vec![SavedSessionChoice {
                target: target.clone(),
                name: "remote-build".into(),
                lifetime: Lifetime::Persistent {},
            }],
        };
        let mut harness = harness(festerm_config::Configuration::empty());
        let draft = RecipeDraft {
            name: "My shortcut".into(),
            ..Default::default()
        };
        install(&harness, RecipePage::Edit(Box::new(draft)));
        let tab = harness.state().tab;
        let request = egui::Id::new("pick-host-for-draft");
        crate::screens::host_workspace::install_profile_request_for_test(
            &harness.ctx,
            tab,
            request,
        );
        assert!(!select_host_for_profile(
            &harness.ctx,
            tab,
            egui::Id::new("stale"),
            host.clone(),
            Some(target.clone())
        ));
        assert!(select_host_for_profile(
            &harness.ctx,
            tab,
            request,
            host,
            Some(target.clone())
        ));
        harness.run_ok();
        let state = harness
            .ctx
            .data(|data| data.get_temp::<ProfilesScreenState>(profiles_state_id(tab)))
            .unwrap();
        let ProfilesScreenMode::HostRecipes(editor) = state.mode else {
            panic!("same editor")
        };
        let RecipePage::Edit(draft) = editor.page else {
            panic!("same draft")
        };
        assert_eq!(draft.name, "My shortcut");
        assert_eq!(draft.session_name, "remote-build");
        assert_eq!(draft.logical, target.logical);
        assert_eq!(draft.host, selected.host.id.to_string());
        assert!(harness.state().command.is_none());
        assert!(harness.state().configuration.host_recipes().is_empty());
    }

    fn listed_sessions() -> Vec<crate::screens::host_workspace::SavedSessionChoice> {
        [("build", 3), ("shared", 4), ("shared", 5)]
            .into_iter()
            .map(
                |(name, generation)| crate::screens::host_workspace::SavedSessionChoice {
                    target: festerm_tunnel_bridge::host_client::Target {
                        logical: LogicalSessionId::new(),
                        generation: GenerationId::new(generation).unwrap(),
                    },
                    name: name.into(),
                    lifetime: Lifetime::Persistent {},
                },
            )
            .collect()
    }

    #[test]
    fn typed_session_name_selects_an_exact_unique_match_or_names_a_new_session() {
        let sessions = listed_sessions();
        let mut draft = RecipeDraft::default();
        let fresh = draft.logical.clone();

        draft.session_name = " build ".into();
        draft.reconcile_session_name(&sessions);
        assert_eq!(draft.logical, sessions[0].target.logical);
        assert_eq!(draft.generation, "3");
        assert_eq!(draft.session_name, "build");
        assert_eq!(draft.behavior, ConnectionBehavior::ResumeOnly {});

        draft.session_name = "build-2".into();
        draft.reconcile_session_name(&sessions);
        assert_ne!(draft.logical, sessions[0].target.logical);
        assert_ne!(
            draft.logical, fresh,
            "a new name never reuses a listed identity"
        );
        assert!(draft.generation.is_empty());
        assert_eq!(draft.behavior, ConnectionBehavior::ResumeOrCreate);

        draft.session_name = "shared".into();
        draft.reconcile_session_name(&sessions);
        assert!(
            draft.bound_session(&sessions).is_none(),
            "ambiguous names never auto-bind"
        );
        assert_eq!(draft.behavior, ConnectionBehavior::ResumeOrCreate);

        draft.bind_session(&sessions[2]);
        draft.reconcile_session_name(&sessions);
        assert_eq!(
            draft.logical, sessions[2].target.logical,
            "an explicit choice stays bound"
        );
    }

    #[test]
    fn session_name_dropdown_picks_existing_sessions_and_labels_new_ones() {
        use crate::screens::host_workspace::HostSessions;
        let selected = recipe();
        let sessions = listed_sessions();
        let mut harness = harness(festerm_config::Configuration::empty());
        crate::screens::host_workspace::remember_host(
            &harness.ctx,
            HostSessions {
                reference: selected.host.clone(),
                hint: festerm_config::host_recipes::HostHint {
                    name: "Build host".into(),
                    locator: "fixture.use".into(),
                },
                sessions: sessions.clone(),
            },
        );
        let mut draft = RecipeDraft {
            name: "My shortcut".into(),
            ..Default::default()
        };
        draft.bind_host(&crate::screens::host_workspace::remembered_hosts(&harness.ctx)[0]);
        install(&harness, RecipePage::Edit(Box::new(draft)));
        harness.run_ok();
        assert!(harness
            .get_by_label("Create session if missing")
            .accesskit_node()
            .is_disabled());

        harness.get_by_label("Existing sessions").click();
        harness.run_ok();
        harness.get_by_label("build").click();
        harness.run_ok();
        harness.get_by_label("Existing session");
        assert!(!harness
            .get_by_label("Create session if missing")
            .accesskit_node()
            .is_disabled());
        let draft = current_draft(&harness);
        assert_eq!(draft.logical, sessions[0].target.logical);

        harness
            .get_by_role_and_label(egui::accesskit::Role::TextInput, "Session name")
            .focus();
        harness.run_ok();
        harness
            .get_by_role_and_label(egui::accesskit::Role::TextInput, "Session name")
            .type_text("-next");
        harness.run_ok();
        harness.get_by_label("New session · created when you first connect");
        assert!(harness
            .get_by_label("Create session if missing")
            .accesskit_node()
            .is_disabled());
        let draft = current_draft(&harness);
        assert_eq!(draft.session_name, "build-next");
        assert_eq!(draft.behavior, ConnectionBehavior::ResumeOrCreate);
        assert!(harness.state().command.is_none());
    }

    #[test]
    fn editing_a_saved_profile_never_retargets_its_session() {
        let original = recipe();
        let mut harness = harness(festerm_config::Configuration::empty());
        install(
            &harness,
            RecipePage::Edit(Box::new(RecipeDraft::from_recipe(original.clone()))),
        );
        harness.run_ok();
        assert!(harness.query_by_label("Existing sessions").is_none());
        assert!(harness.query_by_label("Refresh").is_none());
        harness.get_by_label("The saved session identity is kept when editing.");
        assert!(harness.state().command.is_none());
        assert_eq!(current_draft(&harness).logical, original.logical_session);
    }

    fn current_draft(harness: &Harness<'_, State>) -> RecipeDraft {
        let tab = harness.state().tab;
        let state = harness
            .ctx
            .data(|data| data.get_temp::<ProfilesScreenState>(profiles_state_id(tab)))
            .unwrap();
        let ProfilesScreenMode::HostRecipes(editor) = state.mode else {
            panic!("recipe editor")
        };
        let RecipePage::Edit(draft) = editor.page else {
            panic!("draft")
        };
        *draft
    }

    #[test]
    fn legacy_long_profile_name_remains_editable_without_explicit_session_name() {
        let mut original = recipe();
        original.name = "x".repeat(200);
        original.session_name = None;
        let mut draft = RecipeDraft::from_recipe(original.clone());
        draft.name = "Renamed local shortcut".into();
        let MetadataCommand::Edit(edit) = draft.command().unwrap() else {
            panic!("edit")
        };
        assert!(edit.session_name.is_none());
        assert_eq!(edit.logical_session, original.logical_session);
        assert_eq!(edit.name, draft.name);
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
        harness.get_by_label("Connection details").click();
        harness.run_ok();
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
