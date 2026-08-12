//! Launcher and Settings application-surface presentation.
//!
//! These are thin, product-specific screens rather than terminal chrome
//! (`crates/festerm-ui-egui/src/chrome.rs` owns the chip row). They translate
//! user gestures into `AppCommand`s per `docs/application-command-model.md`
//! and own no session or tab policy themselves; `AppState::dispatch` remains
//! the single command-handling path.

use eframe::egui::{self, TextEdit, Ui};
use festerm_config::{Profile, SshProfileConfiguration};
use festerm_session::TerminalSize;
use festerm_ssh::{
    HostIdentity, ReconnectPolicy, SshAuthentication, SshConnectionProfile, SshKeyPassphrase,
    SshPrivateKey, SshPrivateKeyError, SshSessionOptions,
};
use festerm_ui_egui::chrome::ChipLayout;

use crate::configuration_startup::ConfigurationStartupStatus;
use crate::tabs::{AppCommand, TabId};

/// One selectable local launch option in the Launcher list.
struct LauncherItem<'a> {
    label: String,
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
    reconnect_enabled: bool,
    feedback: Option<String>,
}

impl SshLauncherForm {
    const DEFAULT_PORT: u16 = 22;
    /// The Launcher permits only three fresh connection attempts after a
    /// user-requested reconnect, starting after 500 ms and capped at 2 s.
    /// This bounds retained transient authentication and retry activity.
    const RECONNECT_MAXIMUM_ATTEMPTS: u8 = 3;
    const RECONNECT_INITIAL_DELAY: std::time::Duration = std::time::Duration::from_millis(500);
    const RECONNECT_MAXIMUM_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

    fn session_options(&self) -> SshSessionOptions {
        if !self.reconnect_enabled {
            return SshSessionOptions::new();
        }

        let policy = ReconnectPolicy::new(
            Self::RECONNECT_MAXIMUM_ATTEMPTS,
            Self::RECONNECT_INITIAL_DELAY,
            Self::RECONNECT_MAXIMUM_DELAY,
        )
        .expect("the fixed Launcher reconnect policy is valid");
        SshSessionOptions::with_reconnect_policy(policy)
    }

    fn prefill_from_profile(&mut self, profile: &SshProfileConfiguration) {
        self.host = profile.host().to_owned();
        self.port = profile.port().to_string();
        self.username = profile.username().to_owned();
    }

    /// Converts the transient form into the application's typed SSH command.
    ///
    /// Taking every secret first ensures each submit attempt removes it from UI
    /// state, including attempts rejected by non-secret input validation.
    fn submit(&mut self) -> Result<AppCommand, String> {
        let password = std::mem::take(&mut self.password);
        let private_key = std::mem::take(&mut self.private_key);
        let key_passphrase = std::mem::take(&mut self.key_passphrase);
        let authentication = match self.authentication_method {
            SshAuthenticationMethod::Password => SshAuthentication::password(password),
            SshAuthenticationMethod::PrivateKey => {
                Self::parse_private_key(private_key, key_passphrase)?
            }
        };
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

        Ok(AppCommand::StartSshSession {
            profile,
            authentication,
            options: self.session_options(),
        })
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

fn ssh_text_edit(
    ui: &mut Ui,
    tab_id: TabId,
    field: &'static str,
    label: &str,
    value: &mut String,
    password: bool,
) -> egui::Response {
    ui.horizontal(|ui| {
        let label = ui.label(label);
        ui.add(
            TextEdit::singleline(value)
                .id_salt(("launcher_ssh", tab_id, field))
                .password(password)
                .desired_width(220.0),
        )
        .labelled_by(label.id)
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

fn show_ssh_form(ui: &mut Ui, tab_id: TabId, form: &mut SshLauncherForm) -> Option<AppCommand> {
    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);
    ui.label(egui::RichText::new("SSH connection").strong());
    ui.label("Connect once with transient authentication. Port defaults to 22.");

    ssh_text_edit(ui, tab_id, "host", "Host", &mut form.host, false);
    ssh_text_edit(ui, tab_id, "port", "Port (optional)", &mut form.port, false);
    ssh_text_edit(
        ui,
        tab_id,
        "username",
        "Username",
        &mut form.username,
        false,
    );
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
    let submit_with_enter = match form.authentication_method {
        SshAuthenticationMethod::Password => {
            ssh_text_edit(ui, tab_id, "password", "Password", &mut form.password, true).lost_focus()
                && ui.input(|input| input.key_pressed(egui::Key::Enter))
        }
        SshAuthenticationMethod::PrivateKey => {
            ssh_multiline_secret_text_edit(
                ui,
                tab_id,
                "private_key",
                "OpenSSH private key",
                &mut form.private_key,
            );
            ui.label(
                "The key is masked, parsed only in memory, and never saved. \
                 An optional passphrase is used only for an encrypted key.",
            );
            ssh_text_edit(
                ui,
                tab_id,
                "key_passphrase",
                "Key passphrase (optional)",
                &mut form.key_passphrase,
                true,
            )
            .lost_focus()
                && ui.input(|input| input.key_pressed(egui::Key::Enter))
        }
    };
    ui.checkbox(
        &mut form.reconnect_enabled,
        format!(
            "Reconnect after a disconnect (up to {} attempts)",
            SshLauncherForm::RECONNECT_MAXIMUM_ATTEMPTS
        ),
    );
    ui.label(
        "Reconnect creates a fresh remote shell and re-verifies the host key; \
         it does not restore remote process state. Delays start at 500 ms and \
         are capped at 2 seconds. This setting applies only to this session \
         and is not saved.",
    );

    let submit_label = match form.authentication_method {
        SshAuthenticationMethod::Password => "Connect with password",
        SshAuthenticationMethod::PrivateKey => "Connect with private key",
    };
    if ui.button(submit_label).clicked() || submit_with_enter {
        match form.submit() {
            Ok(command) => {
                form.feedback = None;
                return Some(command);
            }
            Err(feedback) => form.feedback = Some(feedback),
        }
    }
    if let Some(feedback) = &form.feedback {
        ui.colored_label(egui::Color32::from_rgb(220, 110, 110), feedback);
    }
    None
}

fn show_saved_ssh_profiles(ui: &mut Ui, profiles: &[Profile]) {
    let ssh_profiles: Vec<_> = profiles.iter().filter_map(Profile::as_ssh).collect();
    if ssh_profiles.is_empty() {
        return;
    }

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Saved SSH profiles").strong());
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
            ui.label(
                "This saved profile does not launch yet. Use the transient SSH \
                 authentication form below; secure credential storage is planned for M8.",
            );
        });
    }
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
/// (persisted per-tab via `tab_id`, so multiple open launcher tabs don't
/// share selection state) and Enter launches the highlighted item, without
/// requiring the mouse. `tab_id` identifies which launcher tab this is, since
/// egui's per-frame widget memory is otherwise shared across all callers
/// within the same panel.
pub fn show_launcher(ui: &mut Ui, tab_id: TabId, profiles: &[Profile]) -> Option<AppCommand> {
    let mut items = vec![LauncherItem {
        label: "Local Shell (platform default)".to_owned(),
        profile_id: None,
    }];
    items.extend(
        profiles
            .iter()
            .filter_map(Profile::as_local)
            .map(|profile| LauncherItem {
                label: format!("{} (Local profile)", profile.identifier()),
                profile_id: Some(profile.identifier()),
            }),
    );

    let state_id = launcher_state_id(tab_id);
    let mut state = ui.data(|data| data.get_temp::<LauncherState>(state_id).unwrap_or_default());
    state.selected = state.selected.min(items.len() - 1);

    let form_has_focus = ssh_form_has_focus(ui, tab_id);
    if !form_has_focus && ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
        state.selected = (state.selected + 1) % items.len();
    }
    if !form_has_focus && ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
        state.selected = (state.selected + items.len() - 1) % items.len();
    }
    let launch_via_keyboard = !form_has_focus && ui.input(|i| i.key_pressed(egui::Key::Enter));

    let mut command = None;
    ui.vertical(|ui| {
        ui.add_space(24.0);
        ui.heading("Launcher");
        ui.label(
            "Start or connect to a session. Use \u{2191}/\u{2193} then Enter to launch the \
             highlighted option.",
        );
        ui.add_space(12.0);
        for (index, item) in items.iter().enumerate() {
            let response = ui.add(egui::Button::new(&item.label).selected(index == state.selected));
            if response.clicked() {
                command = Some(item.command());
            }
        }
        if command.is_none() {
            show_saved_ssh_profiles(ui, profiles);
            command = show_ssh_form(ui, tab_id, &mut state.ssh);
        }
    });

    if command.is_none() && launch_via_keyboard {
        command = Some(items[state.selected].command());
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
) -> Option<AppCommand> {
    let state_id = launcher_state_id(tab_id);
    let mut state = ui.data(|data| data.get_temp::<LauncherState>(state_id).unwrap_or_default());
    if !state.ssh_profile_prefilled {
        state.ssh.prefill_from_profile(profile);
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
            show_ssh_form(ui, tab_id, &mut state.ssh)
        })
        .inner;

    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
}

/// Renders the Settings application surface.
///
/// `chip_layout` reflects the current chip wrapping mode
/// (`docs/gui-design.md` "Wrapping must remain user-configurable"); this is
/// the one persistent preference implemented so far. Returns commands for
/// Settings actions; the application composition root owns configuration I/O
/// and applies successful replacements to `AppState`.
pub fn show_settings(
    ui: &mut Ui,
    chip_layout: ChipLayout,
    status_bar_visible: bool,
    configuration_status: ConfigurationStartupStatus,
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
                    if let Some(command) = show_launcher(ui, state.tab_id, &state.profiles) {
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

    fn enter_text(harness: &mut Harness<'static, LauncherHarnessState>, label: &str, text: &str) {
        harness.get_by_label(label).click();
        harness.run();
        harness.get_by_label(label).type_text(text);
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
    fn ssh_form_returns_a_typed_password_command_with_default_port() {
        let mut harness = harness();
        harness.run();
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
            "reconnect must remain disabled until the transient control is selected"
        );
    }

    #[test]
    fn ssh_form_can_opt_into_the_bounded_reconnect_policy() {
        let mut form = SshLauncherForm {
            host: "example.invalid".to_owned(),
            username: "test-user".to_owned(),
            password: "transient-test-password".to_owned(),
            reconnect_enabled: true,
            ..Default::default()
        };

        let AppCommand::StartSshSession { options, .. } =
            form.submit().expect("valid form must submit")
        else {
            unreachable!("the form only creates SSH commands");
        };

        assert_eq!(
            options.reconnect_policy(),
            Some(
                ReconnectPolicy::new(
                    SshLauncherForm::RECONNECT_MAXIMUM_ATTEMPTS,
                    SshLauncherForm::RECONNECT_INITIAL_DELAY,
                    SshLauncherForm::RECONNECT_MAXIMUM_DELAY,
                )
                .expect("the fixed Launcher reconnect policy is valid")
            )
        );
    }

    #[test]
    fn ssh_form_shows_the_transient_reconnect_control_and_warning() {
        let mut harness = harness();
        harness.run();

        assert!(harness
            .query_by_label("Reconnect after a disconnect (up to 3 attempts)")
            .is_some());
        assert!(harness
            .query_by_label(
                "Reconnect creates a fresh remote shell and re-verifies the host key; \
                 it does not restore remote process state. Delays start at 500 ms and \
                 are capped at 2 seconds. This setting applies only to this session \
                 and is not saved."
            )
            .is_some());
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
            .query_by_label("development (Local profile)")
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
    fn saved_ssh_profile_is_visible_but_not_a_launch_action() {
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
            .query_by_label(
                "This saved profile does not launch yet. Use the transient SSH \
                 authentication form below; secure credential storage is planned for M8."
            )
            .is_some());
        assert!(harness.query_by_label("production (SSH profile)").is_none());
        assert!(harness.state().command.is_none());
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
                    if let Some(command) = show_ssh_authentication_required(ui, tab_id, profile) {
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

        assert!(harness.query_by_label("OpenSSH private key").is_none());

        harness.get_by_label("Private-key authentication").click();
        harness.run();

        assert!(harness.query_by_label("Password").is_none());
        assert!(harness.query_by_label("OpenSSH private key").is_some());
        assert!(harness
            .query_by_label("Key passphrase (optional)")
            .is_some());
        assert!(harness
            .query_by_label(
                "The key is masked, parsed only in memory, and never saved. \
                 An optional passphrase is used only for an encrypted key."
            )
            .is_some());
    }

    #[test]
    fn ssh_form_shows_constructor_validation_feedback() {
        let mut harness = harness();
        harness.run();
        enter_text(&mut harness, "Host", "invalid host");

        harness.get_by_label("Connect with password").click();
        harness.run();

        assert!(harness.state().command.is_none());
        assert!(harness
            .query_by_label("SSH host must not contain whitespace")
            .is_some());
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
