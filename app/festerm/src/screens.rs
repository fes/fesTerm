//! Launcher and Settings application-surface presentation.
//!
//! These are thin, product-specific screens rather than terminal chrome
//! (`crates/festerm-ui-egui/src/chrome.rs` owns the chip row). They translate
//! user gestures into `AppCommand`s per `docs/application-command-model.md`
//! and own no session or tab policy themselves; `AppState::dispatch` remains
//! the single command-handling path.

use eframe::egui::{self, vec2, Sense, Stroke, TextEdit, Ui, WidgetInfo, WidgetType};
use festerm_config::{PersistenceConfiguration, Profile, SshProfileConfiguration};
use festerm_session::TerminalSize;
use festerm_ssh::{
    HostIdentity, ReconnectPolicy, RecoveryPolicy, SessionStrategy, SshAuthentication,
    SshConnectionProfile, SshKeyPassphrase, SshPrivateKey, SshPrivateKeyError, SshSessionOptions,
};
use festerm_ui_egui::{chrome::ChipLayout, icon, icon::Icon, theme};

use crate::configuration_startup::ConfigurationStartupStatus;
use crate::tabs::{AppCommand, PasswordToStore, TabId};

/// One selectable local launch option in the Launcher list.
struct LauncherItem<'a> {
    label: String,
    description: String,
    profile_id: Option<&'a str>,
}

impl LauncherItem<'_> {
    fn command(&self) -> AppCommand {
        match self.profile_id {
            Some(profile_id) => AppCommand::StartConfiguredLocalProfile {
                profile_id: profile_id.to_owned(),
            },
            None => AppCommand::StartLocalSession,
        }
    }
}

fn show_launcher_choice(
    ui: &mut Ui,
    primary: &str,
    secondary: &str,
    selected: bool,
    remote: bool,
) -> egui::Response {
    let width = ui.available_width().clamp(220.0, 420.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 54.0), Sense::click());
    let active = selected || response.hovered();
    ui.painter().rect(
        rect,
        6.0,
        if active {
            theme::SURFACE_TAB_ACTIVE
        } else {
            theme::SURFACE_TAB_INACTIVE
        },
        Stroke::new(
            if selected { 1.5 } else { 1.0 },
            if selected {
                theme::BORDER_ACTIVE
            } else {
                theme::BORDER_SUBTLE
            },
        ),
        egui::StrokeKind::Inside,
    );

    let icon_rect = egui::Rect::from_min_size(rect.left_top() + vec2(14.0, 18.0), vec2(18.0, 16.0));
    let stroke = Stroke::new(
        1.5,
        if active {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_SECONDARY
        },
    );
    icon::paint(
        ui.painter(),
        if remote {
            Icon::SshRemote
        } else {
            Icon::LocalTerminal
        },
        icon_rect,
        stroke.color,
    );

    ui.painter().text(
        rect.left_top() + vec2(50.0, 10.0),
        egui::Align2::LEFT_TOP,
        primary,
        egui::FontId::proportional(18.0),
        theme::TEXT_PRIMARY,
    );
    ui.painter().text(
        rect.left_top() + vec2(50.0, 34.0),
        egui::Align2::LEFT_TOP,
        secondary,
        egui::FontId::proportional(11.0),
        theme::TEXT_MUTED,
    );
    response.widget_info(|| {
        WidgetInfo::labeled(WidgetType::Button, true, format!("{primary} — {secondary}"))
    });
    response
}

/// A bordered "Back" control that renders the app's own chevron glyph
/// (`icon::Icon::Back`) instead of a Unicode arrow character, since the
/// bundled font has no glyph for the arrow and would otherwise show a tofu
/// box (`docs/icon-system.md`'s painter-drawn icons avoid exactly this).
fn ssh_back_button(ui: &mut Ui) -> egui::Response {
    let text = "Back";
    let font = egui::FontId::proportional(13.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), theme::TEXT_PRIMARY);
    let icon_size = 14.0;
    let spacing = 6.0;
    let padding = vec2(10.0, 6.0);
    let size = vec2(
        icon_size + spacing + galley.size().x + padding.x * 2.0,
        galley.size().y.max(icon_size) + padding.y * 2.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    ui.painter().rect(
        rect,
        6.0,
        if hovered {
            theme::SURFACE_TAB_ACTIVE
        } else {
            theme::SURFACE_TAB_INACTIVE
        },
        Stroke::new(1.0, theme::BORDER_SUBTLE),
        egui::StrokeKind::Inside,
    );
    let icon_rect = egui::Rect::from_min_size(
        rect.left_top() + vec2(padding.x, (rect.height() - icon_size) / 2.0),
        vec2(icon_size, icon_size),
    );
    icon::paint(ui.painter(), Icon::Back, icon_rect, theme::TEXT_PRIMARY);
    ui.painter().galley(
        rect.left_top() + vec2(padding.x + icon_size + spacing, padding.y),
        galley,
        theme::TEXT_PRIMARY,
    );
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, text));
    response
}

/// Authentication method selected for one transient SSH connection attempt.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum SshAuthenticationMethod {
    #[default]
    Password,
    PrivateKey,
}

/// Per-launcher, transient SSH authentication form state.
///
/// This belongs only to egui's temporary per-tab data. In particular, it is
/// never a profile, workspace, diagnostic, or application-state field.
#[derive(Clone, Default)]
struct SshLauncherForm {
    host: String,
    port: String,
    username: String,
    authentication_method: SshAuthenticationMethod,
    password: String,
    private_key: String,
    key_passphrase: String,
    saved_profile_id: Option<String>,
    saved_profile_has_credential: bool,
    /// The saved profile's durable remote-session provider and name, if any
    /// (ADR 0018). `None` means this session is an ordinary plain shell.
    persistence: Option<PersistenceConfiguration>,
    /// Whether this launch should opt into automatic recovery. Only
    /// meaningful (and only shown in the form) when `persistence` is set:
    /// automatic recovery is never valid for a plain shell, and is never on
    /// merely because a durable-session provider is configured (ADR 0018
    /// requires an explicit, separate opt-in from persistence itself). This
    /// always starts `false`, so each launch re-opts in rather than
    /// remembering a prior choice.
    automatic_recovery: bool,
    remember_password: bool,
    feedback: Option<String>,
    /// Set whenever the form is (re)opened so the Username field can claim
    /// initial keyboard focus once, without re-stealing it on every frame.
    focus_username: bool,
}

impl SshLauncherForm {
    const DEFAULT_PORT: u16 = 22;

    /// Ordinary SSH sessions have no durable-session provider, so automatic
    /// recovery is not valid for them (ADR 0018); every reconnect is the
    /// user-initiated action available from the session Inspector once
    /// connected. A persistent session only gets automatic recovery when
    /// `self.automatic_recovery` is explicitly set, which
    /// `with_recovery_policy` can never reject for a persistent strategy,
    /// so this falls back to manual recovery only if that invariant is
    /// somehow violated.
    fn session_options(&self) -> SshSessionOptions {
        let strategy = self
            .persistence
            .as_ref()
            .and_then(|persistence| persistence.to_session_strategy().ok())
            .unwrap_or(SessionStrategy::PlainShell);
        if self.automatic_recovery {
            let recovery = RecoveryPolicy::Automatic(ReconnectPolicy::default_automatic());
            if let Ok(options) = SshSessionOptions::with_recovery_policy(strategy.clone(), recovery)
            {
                return options;
            }
        }
        SshSessionOptions::manual_recovery(strategy)
    }

    fn prefill_from_profile(&mut self, profile: &SshProfileConfiguration) {
        self.host = profile.host().to_owned();
        self.port = profile.port().to_string();
        self.username = profile.username().to_owned();
        self.persistence = profile.persistence().cloned();
        self.automatic_recovery = false;
    }

    fn prefill_saved_profile(&mut self, profile: &SshProfileConfiguration) {
        self.prefill_from_profile(profile);
        self.saved_profile_id = Some(profile.identifier().to_owned());
        self.saved_profile_has_credential = profile.credential_reference().is_some();
    }

    /// Converts the transient form into the application's typed SSH command.
    ///
    /// Taking every secret first ensures each submit attempt removes it from UI
    /// state, including attempts rejected by non-secret input validation.
    fn submit(&mut self) -> Result<AppCommand, String> {
        let password = std::mem::take(&mut self.password);
        let private_key = std::mem::take(&mut self.private_key);
        let key_passphrase = std::mem::take(&mut self.key_passphrase);
        let port = if self.port.trim().is_empty() {
            Self::DEFAULT_PORT
        } else {
            self.port
                .trim()
                .parse::<u16>()
                .map_err(|_| "SSH port must be a number between 1 and 65535".to_owned())?
        };
        let identity = HostIdentity::new(&self.host, port).map_err(|error| error.to_string())?;
        let initial_size =
            TerminalSize::new(80, 24).expect("the launcher default terminal size is valid");
        let profile = SshConnectionProfile::new(
            identity,
            self.username.clone(),
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            initial_size,
        )
        .map_err(|error| error.to_string())?;

        match self.authentication_method {
            SshAuthenticationMethod::Password
                if self.remember_password
                    && self.saved_profile_id.is_some()
                    && !password.is_empty() =>
            {
                Ok(AppCommand::StoreSshPassword {
                    profile_id: self
                        .saved_profile_id
                        .clone()
                        .expect("saved profile was checked above"),
                    password: PasswordToStore::new(password),
                    options: self.session_options(),
                })
            }
            SshAuthenticationMethod::Password => Ok(AppCommand::StartSshSession {
                profile,
                authentication: SshAuthentication::password(password),
                options: self.session_options(),
            }),
            SshAuthenticationMethod::PrivateKey => Ok(AppCommand::StartSshSession {
                profile,
                authentication: Self::parse_private_key(private_key, key_passphrase)?,
                options: self.session_options(),
            }),
        }
    }

    /// Parses an in-memory key while retaining neither its text nor passphrase.
    ///
    /// Encrypted OpenSSH keys use the SSH crate's explicit passphrase API; the
    /// parser distinguishes that case without trying to persist or log either
    /// source string.
    fn parse_private_key(
        private_key: String,
        key_passphrase: String,
    ) -> Result<SshAuthentication, String> {
        match SshPrivateKey::from_openssh(&private_key) {
            Ok(private_key) if key_passphrase.is_empty() => {
                Ok(SshAuthentication::public_key(private_key))
            }
            Ok(_) => Err("SSH private key is unencrypted; clear the passphrase".to_owned()),
            Err(SshPrivateKeyError::Encrypted) => SshPrivateKey::from_encrypted_openssh(
                &private_key,
                SshKeyPassphrase::new(key_passphrase),
            )
            .map(SshAuthentication::public_key)
            .map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        }
    }
}

#[derive(Clone, Default)]
struct LauncherState {
    selected: usize,
    ssh_open: bool,
    ssh: SshLauncherForm,
    ssh_profile_prefilled: bool,
}

fn launcher_state_id(tab_id: TabId) -> egui::Id {
    egui::Id::new(("launcher_state", tab_id))
}

fn ssh_field_id(ui: &Ui, tab_id: TabId, field: &'static str) -> egui::Id {
    ui.make_persistent_id(("launcher_ssh", tab_id, field))
}

fn ssh_form_has_focus(ui: &Ui, tab_id: TabId) -> bool {
    [
        "host",
        "port",
        "username",
        "password",
        "private_key",
        "key_passphrase",
    ]
    .into_iter()
    .map(|field| ssh_field_id(ui, tab_id, field))
    .any(|id| ui.memory(|memory| memory.has_focus(id)))
}

/// A small uppercase, muted sub-section label, matching the Session
/// Inspector's grouped-row convention (`inspector.rs`'s `section_heading`)
/// so launcher surfaces share one "quiet section" visual language rather
/// than each screen inventing its own heading style.
///
/// Unlike a plain label, this never adds space above itself: callers add
/// spacing between sections explicitly, so the very first heading in a card
/// sits right under the card's own top padding instead of compounding it.
fn ssh_section_heading(ui: &mut Ui, heading: &str) {
    ui.label(
        egui::RichText::new(heading.to_uppercase())
            .size(10.0)
            .color(theme::TEXT_MUTED),
    );
    ui.add_space(4.0);
}

/// Body copy sized and colored to match the rest of the launcher, and
/// wrapped to the card's width instead of the full window so long
/// explanatory text stays legible and doesn't visually escape its section.
fn ssh_paragraph(ui: &mut Ui, text: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .size(12.0)
                .color(theme::TEXT_SECONDARY),
        )
        .wrap(),
    );
}

fn ssh_text_edit(
    ui: &mut Ui,
    tab_id: TabId,
    field: &'static str,
    label: &str,
    value: &mut String,
    password: bool,
    request_focus: bool,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        let label = ui.add(
            egui::Label::new(egui::RichText::new(label).color(theme::TEXT_SECONDARY))
                .selectable(false),
        );
        let field = ui.add(
            TextEdit::singleline(value)
                .id_salt(("launcher_ssh", tab_id, field))
                .password(password)
                .desired_width(180.0),
        );
        if request_focus {
            field.request_focus();
        }
        field.labelled_by(label.id)
    })
    .inner
}

fn ssh_multiline_secret_text_edit(
    ui: &mut Ui,
    tab_id: TabId,
    field: &'static str,
    label: &str,
    value: &mut String,
) -> egui::Response {
    ui.vertical(|ui| {
        let label = ui.label(label);
        ui.add(
            TextEdit::multiline(value)
                .id_salt(("launcher_ssh", tab_id, field))
                .password(true)
                .desired_width(360.0)
                .desired_rows(8),
        )
        .labelled_by(label.id)
    })
    .inner
}

fn show_ssh_form(
    ui: &mut Ui,
    tab_id: TabId,
    form: &mut SshLauncherForm,
    native_store_available: bool,
) -> Option<AppCommand> {
    ui.add_space(16.0);
    let mut result = None;
    let focus_username = form.focus_username;
    form.focus_username = false;
    egui::Frame::new()
        .fill(theme::SURFACE_TAB_INACTIVE)
        .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(340.0);
            ssh_section_heading(ui, "Connection");
            ssh_text_edit(
                ui,
                tab_id,
                "username",
                "Username",
                &mut form.username,
                false,
                focus_username,
            );
            ssh_text_edit(ui, tab_id, "host", "Host", &mut form.host, false, false);
            ssh_text_edit(
                ui,
                tab_id,
                "port",
                "Port (default: 22)",
                &mut form.port,
                false,
                false,
            );

            if let Some(persistence) = form.persistence.clone() {
                ui.add_space(10.0);
                ssh_section_heading(ui, "Durable session");
                ssh_paragraph(
                    ui,
                    &format!(
                        "This profile attaches to or creates a {} session named \"{}\".",
                        persistence.provider().label(),
                        persistence.session_name()
                    ),
                );
                ui.checkbox(
                    &mut form.automatic_recovery,
                    "Automatically resume this session after a lost connection",
                );
            }

            ui.add_space(10.0);
            ssh_section_heading(ui, "Authentication");
            ui.horizontal(|ui| {
                ui.radio_value(
                    &mut form.authentication_method,
                    SshAuthenticationMethod::Password,
                    "Password authentication",
                );
                ui.radio_value(
                    &mut form.authentication_method,
                    SshAuthenticationMethod::PrivateKey,
                    "Private-key authentication",
                );
            });
            ui.add_space(4.0);
            let submit_with_enter = match form.authentication_method {
                SshAuthenticationMethod::Password => {
                    let submit = ssh_text_edit(
                        ui,
                        tab_id,
                        "password",
                        "Password",
                        &mut form.password,
                        true,
                        false,
                    )
                    .lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    ui.add_space(4.0);
                    if form.saved_profile_id.is_some() {
                        ui.checkbox(
                            &mut form.remember_password,
                            "Remember this password in native secure storage",
                        );
                    } else {
                        ssh_paragraph(ui, "Saving a password requires a saved profile.");
                    }
                    if form.saved_profile_has_credential {
                        ui.add_space(4.0);
                        if ui
                            .add_enabled(
                                native_store_available,
                                egui::Button::new("Use stored password"),
                            )
                            .clicked()
                        {
                            result = form.saved_profile_id.as_ref().map(|profile_id| {
                                AppCommand::StartStoredPasswordSshProfile {
                                    profile_id: profile_id.clone(),
                                }
                            });
                        }
                        if !native_store_available {
                            ssh_paragraph(ui, "Native secure storage is unavailable.");
                        }
                    }
                    submit
                }
                SshAuthenticationMethod::PrivateKey => {
                    ssh_multiline_secret_text_edit(
                        ui,
                        tab_id,
                        "private_key",
                        "OpenSSH private key",
                        &mut form.private_key,
                    );
                    ssh_paragraph(ui, "The key is kept in memory only, never saved.");
                    ssh_text_edit(
                        ui,
                        tab_id,
                        "key_passphrase",
                        "Key passphrase (optional)",
                        &mut form.key_passphrase,
                        true,
                        false,
                    )
                    .lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))
                }
            };

            if result.is_none() {
                ui.add_space(12.0);
                let submit_label = match form.authentication_method {
                    SshAuthenticationMethod::Password => "Connect with password",
                    SshAuthenticationMethod::PrivateKey => "Connect with private key",
                };
                if ui.button(submit_label).clicked() || submit_with_enter {
                    match form.submit() {
                        Ok(command) => {
                            form.feedback = None;
                            result = Some(command);
                        }
                        Err(feedback) => form.feedback = Some(feedback),
                    }
                }
                if let Some(feedback) = &form.feedback {
                    ui.add_space(4.0);
                    ui.colored_label(theme::STATUS_ERROR, feedback);
                }
            }
        });
    result
}

enum SavedSshProfileAction {
    OpenPasswordForm(SshProfileConfiguration),
    UseStoredPassword(String),
}

fn show_saved_ssh_profiles(
    ui: &mut Ui,
    profiles: &[Profile],
    native_store_available: bool,
) -> Option<SavedSshProfileAction> {
    let ssh_profiles: Vec<_> = profiles.iter().filter_map(Profile::as_ssh).collect();
    if ssh_profiles.is_empty() {
        return None;
    }

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Saved SSH profiles").strong());
    let mut action = None;
    for profile in ssh_profiles {
        ui.group(|ui| {
            ui.label(
                egui::RichText::new(format!("Saved SSH profile: {}", profile.identifier()))
                    .strong(),
            );
            ui.label(format!(
                "{}@{}:{} · {} · {}×{}",
                profile.username(),
                profile.host(),
                profile.port(),
                profile.terminal_type(),
                profile.initial_size().0,
                profile.initial_size().1,
            ));
            if profile.credential_reference().is_some()
                && ui
                    .add_enabled(
                        native_store_available,
                        egui::Button::new(format!(
                            "Use stored password for {}",
                            profile.identifier()
                        )),
                    )
                    .clicked()
            {
                action = Some(SavedSshProfileAction::UseStoredPassword(
                    profile.identifier().to_owned(),
                ));
            }
            if ui
                .button(format!(
                    "Enter or replace password for {}",
                    profile.identifier()
                ))
                .clicked()
            {
                action = Some(SavedSshProfileAction::OpenPasswordForm(profile.clone()));
            }
        });
    }
    action
}

/// Renders the session launcher content and returns any dispatched command.
///
/// `docs/gui-design.md` ("Session Launcher"): fast, compact, and usable
/// repeatedly rather than a wizard or onboarding flow. The SSH form is a
/// one-off connection surface: it creates no profile and retains password,
/// key text, and key passphrases only in temporary UI state until submit.
/// Saved local profiles launch through a typed application command. Saved SSH
/// metadata remains visibly non-launching until M8 secure credential storage;
/// the transient form below is the only SSH launch path.
///
/// The list is keyboard-navigable: Up/Down moves a highlighted selection
/// (persisted against the singleton Launcher's `tab_id`) and Enter launches the
/// highlighted item without requiring the mouse. The id prevents this
/// temporary state from colliding with other application-surface widgets.
pub fn show_launcher(
    ui: &mut Ui,
    tab_id: TabId,
    profiles: &[Profile],
    native_store_available: bool,
    secure_storage_status: Option<&str>,
) -> Option<AppCommand> {
    let mut items = vec![LauncherItem {
        label: "Local Shell".to_owned(),
        description: "Default shell on this computer".to_owned(),
        profile_id: None,
    }];
    items.extend(
        profiles
            .iter()
            .filter_map(Profile::as_local)
            .map(|profile| LauncherItem {
                label: profile.identifier().to_owned(),
                description: "Saved local profile".to_owned(),
                profile_id: Some(profile.identifier()),
            }),
    );

    let state_id = launcher_state_id(tab_id);
    let mut state = ui.data(|data| data.get_temp::<LauncherState>(state_id).unwrap_or_default());
    state.selected = state.selected.min(items.len());

    if state.ssh_open {
        let mut command = None;
        let mut back_clicked = false;
        ui.vertical(|ui| {
            ui.add_space(24.0);
            ui.horizontal(|ui| {
                ui.add_space(34.0);
                ui.vertical(|ui| {
                    if ssh_back_button(ui).clicked() {
                        back_clicked = true;
                    }
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("Connect with SSH")
                            .size(24.0)
                            .color(theme::TEXT_PRIMARY),
                    );
                    ui.label(
                        egui::RichText::new("Enter connection details.")
                            .size(11.0)
                            .color(theme::TEXT_SECONDARY),
                    );
                    if !back_clicked {
                        command = show_ssh_form(ui, tab_id, &mut state.ssh, native_store_available);
                    }
                });
            });
        });
        if back_clicked {
            state.ssh_open = false;
        }
        ui.data_mut(|data| data.insert_temp(state_id, state));
        return command;
    }

    let form_has_focus = ssh_form_has_focus(ui, tab_id);
    if !form_has_focus && ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
        state.selected = (state.selected + 1) % (items.len() + 1);
    }
    if !form_has_focus && ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
        state.selected = (state.selected + items.len()) % (items.len() + 1);
    }
    let launch_via_keyboard = !form_has_focus && ui.input(|i| i.key_pressed(egui::Key::Enter));

    let mut command = None;
    let mut saved_ssh_action = None;
    ui.vertical(|ui| {
        ui.add_space(24.0);
        ui.horizontal(|ui| {
            ui.add_space(34.0);
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new("New Session")
                        .size(24.0)
                        .color(theme::TEXT_PRIMARY),
                );
                ui.label(
                    egui::RichText::new("Choose a session type")
                        .size(11.0)
                        .color(theme::TEXT_SECONDARY),
                );
            });
        });
        ui.add_space(23.0);
        ui.horizontal(|ui| {
            ui.add_space(26.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (index, item) in items.iter().enumerate() {
                    let response = show_launcher_choice(
                        ui,
                        &item.label,
                        &item.description,
                        index == state.selected,
                        false,
                    );
                    if response.clicked() {
                        command = Some(item.command());
                    }
                    ui.add_space(12.0);
                }
                if show_launcher_choice(
                    ui,
                    "SSH",
                    "Connect to a remote host",
                    state.selected == items.len(),
                    true,
                )
                .clicked()
                {
                    state.ssh_open = true;
                    state.ssh.focus_username = true;
                }
                if command.is_none() && !state.ssh_open {
                    saved_ssh_action =
                        show_saved_ssh_profiles(ui, profiles, native_store_available);
                }
            });
        });
        if let Some(status) = secure_storage_status {
            ui.add_space(8.0);
            ui.colored_label(egui::Color32::from_rgb(220, 150, 80), status);
        }
    });

    if command.is_none() && launch_via_keyboard {
        if state.selected == items.len() {
            state.ssh_open = true;
            state.ssh.focus_username = true;
        } else {
            command = Some(items[state.selected].command());
        }
    }
    if let Some(action) = saved_ssh_action {
        match action {
            SavedSshProfileAction::OpenPasswordForm(profile) => {
                state.ssh = SshLauncherForm::default();
                state.ssh.prefill_saved_profile(&profile);
                state.ssh_open = true;
                state.ssh.focus_username = true;
            }
            SavedSshProfileAction::UseStoredPassword(profile_id) => {
                command = Some(AppCommand::StartStoredPasswordSshProfile { profile_id });
            }
        }
    }

    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
}

/// Renders a restored SSH workspace tab without creating a transport.
///
/// The destination metadata is copied into the existing transient form only
/// once. Passwords, keys, and host trust remain absent until the user enters
/// them and explicitly submits the form.
pub fn show_ssh_authentication_required(
    ui: &mut Ui,
    tab_id: TabId,
    profile: &SshProfileConfiguration,
    native_store_available: bool,
) -> Option<AppCommand> {
    let state_id = launcher_state_id(tab_id);
    let mut state = ui.data(|data| data.get_temp::<LauncherState>(state_id).unwrap_or_default());
    if !state.ssh_profile_prefilled {
        state.ssh.prefill_saved_profile(profile);
        state.ssh_profile_prefilled = true;
    }

    let command = ui
        .vertical(|ui| {
            ui.add_space(24.0);
            ui.heading("SSH authentication required");
            ui.label(format!(
                "Restored SSH destination: {}@{}:{}",
                profile.username(),
                profile.host(),
                profile.port()
            ));
            ui.label(
                "This workspace restored destination metadata only. Enter fresh authentication \
                 below to connect; no prior connection, credential, or host trust was restored.",
            );
            show_ssh_form(ui, tab_id, &mut state.ssh, native_store_available)
        })
        .inner;

    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
}

/// Renders the Settings application surface.
///
/// `chip_layout` and `status_bar_visible` reflect the current interface
/// preferences (`docs/gui-design.md` "Wrapping must remain user-configurable").
/// Unlike profiles/workspace metadata, these two preferences are saved
/// automatically by the composition root as soon as they change; there is no
/// separate explicit save step for them. Returns commands for Settings
/// actions; the application composition root owns configuration I/O and
/// applies successful replacements to `AppState`.
pub fn show_settings(
    ui: &mut Ui,
    chip_layout: ChipLayout,
    status_bar_visible: bool,
    configuration_status: ConfigurationStartupStatus,
    secure_storage_status: Option<&str>,
) -> Option<AppCommand> {
    let mut command = None;
    ui.vertical(|ui| {
        ui.add_space(24.0);
        ui.heading("Settings");
        ui.label("Configuration is never written automatically.");
        let configuration_message = configuration_status.settings_message();
        if configuration_status.is_problem() {
            ui.colored_label(egui::Color32::from_rgb(220, 150, 80), configuration_message);
        } else {
            ui.label(configuration_message);
        }
        if ui.button("Reload configuration").clicked() {
            command = Some(AppCommand::ReloadConfiguration);
        }
        if ui.button("Save workspace").clicked() {
            command = Some(AppCommand::SaveWorkspace);
        }
        if let Some(status) = secure_storage_status {
            ui.add_space(8.0);
            ui.label("Native secure storage");
            ui.colored_label(egui::Color32::from_rgb(220, 150, 80), status);
        }
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(12.0);
        let wrap = matches!(chip_layout, ChipLayout::Wrap);
        let label = if wrap {
            "Chip layout: wrap onto multiple rows"
        } else {
            "Chip layout: single row (scroll to see more)"
        };
        if ui.button(label).clicked() {
            command = Some(AppCommand::ToggleChipLayout);
        }
        let status_bar_label = if status_bar_visible {
            "Status bar: shown"
        } else {
            "Status bar: hidden"
        };
        if ui.button(status_bar_label).clicked() {
            command = Some(AppCommand::ToggleStatusBar);
        }
        ui.label("Chip layout and status bar visibility are saved automatically.");
        if ui.button("Reset interface settings to defaults").clicked() {
            command = Some(AppCommand::ResetInterfaceSettings);
        }
    });
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::AppState;
    use egui_kittest::{kittest::Queryable, Harness};

    struct LauncherHarnessState {
        tab_id: TabId,
        profiles: Vec<Profile>,
        command: Option<AppCommand>,
    }

    fn harness() -> Harness<'static, LauncherHarnessState> {
        harness_with_profiles(Vec::new())
    }

    fn harness_with_profiles(profiles: Vec<Profile>) -> Harness<'static, LauncherHarnessState> {
        Harness::builder()
            .with_size(egui::vec2(520.0, 560.0))
            .build_ui_state(
                |ui, state: &mut LauncherHarnessState| {
                    if let Some(command) =
                        show_launcher(ui, state.tab_id, &state.profiles, true, None)
                    {
                        state.command = Some(command);
                    }
                },
                LauncherHarnessState {
                    tab_id: AppState::for_test().active(),
                    profiles,
                    command: None,
                },
            )
    }

    #[test]
    fn settings_reload_control_returns_the_reload_command() {
        #[derive(Default)]
        struct SettingsHarnessState {
            command: Option<AppCommand>,
        }

        let mut harness = Harness::builder()
            .with_size(egui::vec2(520.0, 360.0))
            .build_ui_state(
                |ui, state: &mut SettingsHarnessState| {
                    if let Some(command) = show_settings(
                        ui,
                        ChipLayout::Wrap,
                        true,
                        ConfigurationStartupStatus::Loaded,
                        None,
                    ) {
                        state.command = Some(command);
                    }
                },
                SettingsHarnessState::default(),
            );
        harness.run();

        harness.get_by_label("Reload configuration").click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ReloadConfiguration)
        ));
    }

    #[test]
    fn settings_save_workspace_control_returns_the_save_command() {
        #[derive(Default)]
        struct SettingsHarnessState {
            command: Option<AppCommand>,
        }

        let mut harness = Harness::builder()
            .with_size(egui::vec2(520.0, 360.0))
            .build_ui_state(
                |ui, state: &mut SettingsHarnessState| {
                    if let Some(command) = show_settings(
                        ui,
                        ChipLayout::Wrap,
                        true,
                        ConfigurationStartupStatus::Loaded,
                        None,
                    ) {
                        state.command = Some(command);
                    }
                },
                SettingsHarnessState::default(),
            );
        harness.run();

        harness.get_by_label("Save workspace").click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::SaveWorkspace)
        ));
    }

    fn enter_text(harness: &mut Harness<'static, LauncherHarnessState>, label: &str, text: &str) {
        harness.get_by_label(label).click();
        harness.run();
        harness.get_by_label(label).type_text(text);
        harness.run();
    }

    fn open_ssh_form(harness: &mut Harness<'static, LauncherHarnessState>) {
        harness
            .get_by_label("SSH — Connect to a remote host")
            .click();
        harness.run();
    }

    fn generated_openssh_private_key() -> String {
        let mut random = russh::keys::key::safe_rng();
        let key = russh::keys::PrivateKey::random(&mut random, russh::keys::Algorithm::Ed25519)
            .expect("could not generate test SSH key");
        key.to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("could not encode test SSH key")
            .to_string()
    }

    fn generated_encrypted_openssh_private_key() -> (String, String) {
        let passphrase = "test encrypted-key passphrase".to_owned();
        let mut random = russh::keys::key::safe_rng();
        let key = russh::keys::PrivateKey::random(&mut random, russh::keys::Algorithm::Ed25519)
            .expect("could not generate encrypted test SSH key");
        let encrypted = key
            .encrypt(&mut random, &passphrase)
            .expect("could not encrypt test SSH key");
        (
            encrypted
                .to_openssh(russh::keys::ssh_key::LineEnding::LF)
                .expect("could not encode encrypted test SSH key")
                .to_string(),
            passphrase,
        )
    }

    #[test]
    fn opening_the_ssh_form_focuses_the_username_field() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);

        assert!(
            harness.get_by_label("Username").is_focused(),
            "Username must have initial keyboard focus when the SSH form opens"
        );
    }

    #[test]
    fn ssh_form_orders_fields_username_then_host_then_port_with_default_port_wording() {
        // Regression test pinning the requested field order (Username,
        // Host, Port) and the "(default: 22)" wording, replacing the older
        // "(optional)" / "port defaults to 22" phrasing.
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);

        let username_top = harness.get_by_label("Username").rect().top();
        let host_top = harness.get_by_label("Host").rect().top();
        let port_top = harness.get_by_label("Port (default: 22)").rect().top();

        assert!(
            username_top < host_top,
            "Username must be positioned above Host"
        );
        assert!(host_top < port_top, "Host must be positioned above Port");

        assert!(
            harness.query_by_label("Port (optional)").is_none(),
            "the old 'Port (optional)' wording must not be present"
        );
        assert!(
            harness
                .query_by_label("Port (port defaults to 22)")
                .is_none(),
            "the old 'port defaults to 22' wording must not be present"
        );
    }

    #[test]
    fn ssh_form_returns_a_typed_password_command_with_default_port() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);
        enter_text(&mut harness, "Host", "example.invalid");
        enter_text(&mut harness, "Username", "test-user");
        enter_text(&mut harness, "Password", "transient-test-password");

        harness.get_by_label("Connect with password").click();
        harness.run();

        let Some(AppCommand::StartSshSession {
            profile,
            authentication,
            options,
        }) = harness.state().command.as_ref()
        else {
            panic!("the valid SSH form must return a typed SSH command");
        };
        assert_eq!(profile.identity().host(), "example.invalid");
        assert_eq!(profile.identity().port(), 22);
        assert_eq!(profile.username(), "test-user");
        assert_eq!(
            format!("{authentication:?}"),
            "SshAuthentication::Password([REDACTED])"
        );
        assert_eq!(
            options.reconnect_policy(),
            None,
            "plain SSH sessions default to manual-only reconnect (ADR 0018)"
        );
    }

    #[test]
    fn ssh_form_never_offers_automatic_reconnect_for_a_plain_shell() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);

        assert!(
            harness.query_by_label("Reconnect").is_none(),
            "a plain SSH session has no durable-session provider, so automatic \
             recovery is not offered (ADR 0018); only the manual Inspector \
             Reconnect action applies once connected"
        );
        assert!(
            harness
                .query_by_label("Automatically resume this session after a lost connection")
                .is_none(),
            "the automatic-recovery opt-in must not be offered without a durable-session provider"
        );
    }

    #[test]
    fn saved_persistent_ssh_profile_offers_automatic_recovery_opt_in() {
        let profile = Profile::ssh(
            "build",
            "ssh.example.test",
            2200,
            "deploy",
            "xterm-256color",
            100,
            40,
        )
        .expect("test profile is valid")
        .with_persistence(festerm_config::PersistenceProviderKind::Tmux, "build")
        .expect("persistence config is valid");
        let mut harness = harness_with_profiles(vec![profile]);
        harness.run();
        harness
            .get_by_label("Enter or replace password for build")
            .click();
        harness.run();

        assert!(
            harness
                .query_by_label("Automatically resume this session after a lost connection")
                .is_some(),
            "a durable-session profile must offer the automatic-recovery opt-in checkbox"
        );

        harness
            .get_by_label("Automatically resume this session after a lost connection")
            .click();
        harness.run();
        enter_text(&mut harness, "Password", "transient-test-password");
        harness.get_by_label("Connect with password").click();
        harness.run();

        let Some(AppCommand::StartSshSession { options, .. }) = harness.state().command.as_ref()
        else {
            panic!("the valid SSH form must return a typed SSH command");
        };
        assert!(
            options.reconnect_policy().is_some(),
            "opting in to automatic recovery on a durable-session profile must \
             attach a reconnect policy (ADR 0018)"
        );
    }

    #[test]
    fn local_profile_is_keyboard_accessible_and_returns_a_typed_command() {
        let profiles = vec![
            Profile::local("development", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let mut harness = harness_with_profiles(profiles);
        harness.run();

        assert!(harness
            .query_by_label("development — Saved local profile")
            .is_some());
        harness.key_press(egui::Key::ArrowDown);
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::StartConfiguredLocalProfile { ref profile_id })
                if profile_id == "development"
        ));
    }

    #[test]
    fn saved_ssh_profile_can_open_the_password_form_without_a_stored_credential() {
        let profiles = vec![Profile::ssh(
            "production",
            "ssh.example.test",
            2200,
            "deploy",
            "xterm-256color",
            100,
            40,
        )
        .expect("test profile is valid")];
        let mut harness = harness_with_profiles(profiles);
        harness.run();

        assert!(harness
            .query_by_label("Saved SSH profile: production")
            .is_some());
        assert!(harness
            .query_by_label("Enter or replace password for production")
            .is_some());
        assert!(harness
            .query_by_label("Use stored password for production")
            .is_none());
        assert!(harness.state().command.is_none());
    }

    #[test]
    fn saved_ssh_password_form_returns_a_redacted_store_command() {
        let profile = Profile::ssh(
            "production",
            "ssh.example.test",
            2200,
            "deploy",
            "xterm-256color",
            100,
            40,
        )
        .expect("test profile is valid");
        let mut harness = harness_with_profiles(vec![profile]);
        harness.run();
        harness
            .get_by_label("Enter or replace password for production")
            .click();
        harness.step();
        harness.run();
        enter_text(&mut harness, "Password", "stored-test-password");
        harness
            .get_by_label("Remember this password in native secure storage")
            .click();
        harness.get_by_label("Connect with password").click();
        harness.run();

        let Some(AppCommand::StoreSshPassword {
            profile_id,
            password,
            ..
        }) = harness.state().command.as_ref()
        else {
            panic!("saved form must create a typed storage command");
        };
        assert_eq!(profile_id, "production");
        assert!(!format!("{password:?}").contains("stored-test-password"));
    }

    #[test]
    fn saved_ssh_profile_exposes_stored_password_action_only_with_a_reference() {
        let profile = Profile::ssh(
            "production",
            "ssh.example.test",
            2200,
            "deploy",
            "xterm-256color",
            100,
            40,
        )
        .expect("test profile is valid")
        .with_credential_reference(festerm_secret_store::SecretReference::generate())
        .expect("SSH profile accepts an opaque reference");
        let mut harness = harness_with_profiles(vec![profile]);
        harness.run();
        harness
            .get_by_label("Use stored password for production")
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::StartStoredPasswordSshProfile { ref profile_id })
                if profile_id == "production"
        ));
    }

    #[test]
    fn restored_ssh_surface_prefills_destination_and_requires_fresh_authentication() {
        #[derive(Default)]
        struct RestoredSshHarnessState {
            tab_id: Option<TabId>,
            profile: Option<SshProfileConfiguration>,
            command: Option<AppCommand>,
        }

        let profile = Profile::ssh(
            "production",
            "ssh.example.test",
            2200,
            "deploy",
            "xterm-256color",
            100,
            40,
        )
        .expect("test SSH profile is valid")
        .as_ssh()
        .expect("test profile is SSH")
        .clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(520.0, 560.0))
            .build_ui_state(
                |ui, state: &mut RestoredSshHarnessState| {
                    let tab_id = state.tab_id.expect("test tab id is set");
                    let profile = state.profile.as_ref().expect("test profile is set");
                    if let Some(command) =
                        show_ssh_authentication_required(ui, tab_id, profile, true)
                    {
                        state.command = Some(command);
                    }
                },
                RestoredSshHarnessState {
                    tab_id: Some(AppState::for_test().active()),
                    profile: Some(profile),
                    command: None,
                },
            );
        harness.run();

        assert!(harness
            .query_by_label("SSH authentication required")
            .is_some());
        assert!(harness
            .query_by_label(
                "This workspace restored destination metadata only. Enter fresh authentication \
                 below to connect; no prior connection, credential, or host trust was restored."
            )
            .is_some());
        harness.get_by_label("Password").click();
        harness
            .get_by_label("Password")
            .type_text("transient-test-password");
        harness.get_by_label("Connect with password").click();
        harness.run();

        let Some(AppCommand::StartSshSession {
            profile,
            authentication,
            ..
        }) = harness.state().command.as_ref()
        else {
            panic!("fresh authentication must be required before creating a command");
        };
        assert_eq!(profile.identity().host(), "ssh.example.test");
        assert_eq!(profile.identity().port(), 2200);
        assert_eq!(profile.username(), "deploy");
        assert_eq!(
            format!("{authentication:?}"),
            "SshAuthentication::Password([REDACTED])"
        );
    }

    #[test]
    fn ssh_form_shows_the_masked_multiline_key_input_only_for_key_authentication() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);

        assert!(harness.query_by_label("OpenSSH private key").is_none());

        harness.get_by_label("Private-key authentication").click();
        harness.run();

        assert!(harness.query_by_label("Password").is_none());
        assert!(harness.query_by_label("OpenSSH private key").is_some());
        assert!(harness
            .query_by_label("Key passphrase (optional)")
            .is_some());
        assert!(harness
            .query_by_label("The key is kept in memory only, never saved.")
            .is_some());
    }

    #[test]
    fn ssh_form_shows_constructor_validation_feedback() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);
        enter_text(&mut harness, "Host", "invalid host");

        harness.get_by_label("Connect with password").click();
        harness.run();

        assert!(harness.state().command.is_none());
        assert!(harness
            .query_by_label("SSH host must not contain whitespace")
            .is_some());
    }

    #[test]
    fn prefilling_from_a_persistent_profile_yields_persistent_session_options() {
        let profile = Profile::ssh(
            "remote",
            "example.invalid",
            22,
            "test-user",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_persistence(festerm_config::PersistenceProviderKind::Tmux, "build")
        .unwrap();

        let mut form = SshLauncherForm::default();
        form.prefill_saved_profile(profile.as_ssh().unwrap());

        assert_eq!(
            form.session_options().strategy(),
            SessionStrategy::Persistent {
                provider: festerm_ssh::PersistenceProvider::Tmux,
                session_name: festerm_ssh::PersistentSessionName::new("build").unwrap(),
            }
        );
        assert_eq!(form.session_options().reconnect_policy(), None);
    }

    #[test]
    fn prefilling_a_persistent_profile_never_defaults_automatic_recovery_on() {
        let profile = Profile::ssh(
            "remote",
            "example.invalid",
            22,
            "test-user",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_persistence(festerm_config::PersistenceProviderKind::Tmux, "build")
        .unwrap();

        let mut form = SshLauncherForm::default();
        form.prefill_saved_profile(profile.as_ssh().unwrap());

        assert!(
            !form.automatic_recovery,
            "ADR 0018 requires automatic recovery to be an explicit, separate opt-in"
        );
    }

    #[test]
    fn opting_into_automatic_recovery_only_takes_effect_for_a_persistent_profile() {
        let profile = Profile::ssh(
            "remote",
            "example.invalid",
            22,
            "test-user",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_persistence(festerm_config::PersistenceProviderKind::Screen, "editor")
        .unwrap();

        let mut form = SshLauncherForm::default();
        form.prefill_saved_profile(profile.as_ssh().unwrap());
        form.automatic_recovery = true;

        assert!(form.session_options().reconnect_policy().is_some());
    }

    #[test]
    fn opting_into_automatic_recovery_without_persistence_has_no_effect() {
        let form = SshLauncherForm {
            host: "example.invalid".to_owned(),
            username: "test-user".to_owned(),
            automatic_recovery: true,
            ..Default::default()
        };

        assert_eq!(
            form.session_options().strategy(),
            SessionStrategy::PlainShell
        );
        assert_eq!(form.session_options().reconnect_policy(), None);
    }

    #[test]
    fn prefilling_from_an_ordinary_profile_yields_plain_shell_session_options() {
        let profile = Profile::ssh(
            "remote",
            "example.invalid",
            22,
            "test-user",
            "xterm-256color",
            80,
            24,
        )
        .unwrap();

        let mut form = SshLauncherForm::default();
        form.prefill_saved_profile(profile.as_ssh().unwrap());

        assert_eq!(
            form.session_options().strategy(),
            SessionStrategy::PlainShell
        );
    }

    #[test]
    fn ssh_form_submit_clears_the_transient_password() {
        let password = "transient-test-password";
        let mut form = SshLauncherForm {
            host: "example.invalid".to_owned(),
            username: "test-user".to_owned(),
            password: password.to_owned(),
            ..Default::default()
        };

        let command = form.submit().expect("valid form must submit");

        assert!(form.password.is_empty());
        assert!(!format!("{command:?}").contains(password));
    }

    #[test]
    fn ssh_form_submits_a_parsed_transient_private_key_and_clears_all_secret_text() {
        let private_key = generated_openssh_private_key();
        let mut form = SshLauncherForm {
            host: "example.invalid".to_owned(),
            username: "test-user".to_owned(),
            authentication_method: SshAuthenticationMethod::PrivateKey,
            password: "discarded-password".to_owned(),
            private_key,
            ..Default::default()
        };

        let AppCommand::StartSshSession { authentication, .. } =
            form.submit().expect("valid private-key form must submit")
        else {
            unreachable!("the form only creates SSH commands");
        };

        assert_eq!(
            format!("{authentication:?}"),
            "SshAuthentication::PublicKey([REDACTED])"
        );
        assert!(form.password.is_empty());
        assert!(form.private_key.is_empty());
        assert!(form.key_passphrase.is_empty());
    }

    #[test]
    fn ssh_form_parses_an_encrypted_private_key_with_a_transient_passphrase() {
        let (private_key, key_passphrase) = generated_encrypted_openssh_private_key();
        let mut form = SshLauncherForm {
            host: "example.invalid".to_owned(),
            username: "test-user".to_owned(),
            authentication_method: SshAuthenticationMethod::PrivateKey,
            private_key,
            key_passphrase,
            ..Default::default()
        };

        let AppCommand::StartSshSession { authentication, .. } = form
            .submit()
            .expect("encrypted private-key form must submit")
        else {
            unreachable!("the form only creates SSH commands");
        };

        assert_eq!(
            format!("{authentication:?}"),
            "SshAuthentication::PublicKey([REDACTED])"
        );
        assert!(form.private_key.is_empty());
        assert!(form.key_passphrase.is_empty());
    }

    #[test]
    fn ssh_form_rejects_an_invalid_private_key_and_clears_all_secret_text() {
        let mut form = SshLauncherForm {
            host: "example.invalid".to_owned(),
            username: "test-user".to_owned(),
            authentication_method: SshAuthenticationMethod::PrivateKey,
            password: "discarded-password".to_owned(),
            private_key: "not an OpenSSH private key".to_owned(),
            key_passphrase: "discarded-passphrase".to_owned(),
            ..Default::default()
        };

        assert_eq!(
            form.submit().expect_err("invalid key must not submit"),
            "SSH private key is not in OpenSSH format"
        );
        assert!(form.password.is_empty());
        assert!(form.private_key.is_empty());
        assert!(form.key_passphrase.is_empty());
    }
}
