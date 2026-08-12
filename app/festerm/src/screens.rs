//! Launcher and Settings application-surface presentation.
//!
//! These are thin, product-specific screens rather than terminal chrome
//! (`crates/festerm-ui-egui/src/chrome.rs` owns the chip row). They translate
//! user gestures into `AppCommand`s per `docs/application-command-model.md`
//! and own no session or tab policy themselves; `AppState::dispatch` remains
//! the single command-handling path.

use eframe::egui::{self, TextEdit, Ui};
use festerm_session::TerminalSize;
use festerm_ssh::{HostIdentity, SshAuthentication, SshConnectionProfile};
use festerm_ui_egui::chrome::ChipLayout;

use crate::tabs::{AppCommand, TabId};

/// One selectable launch option in the launcher list, alongside the
/// `AppCommand` it dispatches when chosen (by click or via keyboard).
struct LauncherItem {
    label: &'static str,
    command: fn() -> AppCommand,
}

fn start_local_session_command() -> AppCommand {
    AppCommand::StartLocalSession
}

/// Per-launcher, transient SSH password form state.
///
/// This belongs only to egui's temporary per-tab data. In particular, it is
/// never a profile, workspace, diagnostic, or application-state field.
#[derive(Clone, Default)]
struct SshLauncherForm {
    host: String,
    port: String,
    username: String,
    password: String,
    feedback: Option<String>,
}

impl SshLauncherForm {
    const DEFAULT_PORT: u16 = 22;

    /// Converts the transient form into the application's typed SSH command.
    ///
    /// Taking the password first ensures every submit attempt removes it from
    /// UI state, including attempts rejected by non-secret input validation.
    fn submit(&mut self) -> Result<AppCommand, String> {
        let password = std::mem::take(&mut self.password);
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
            authentication: SshAuthentication::password(password),
        })
    }
}

#[derive(Clone, Default)]
struct LauncherState {
    selected: usize,
    ssh: SshLauncherForm,
}

fn launcher_state_id(tab_id: TabId) -> egui::Id {
    egui::Id::new(("launcher_state", tab_id))
}

fn ssh_field_id(ui: &Ui, tab_id: TabId, field: &'static str) -> egui::Id {
    ui.make_persistent_id(("launcher_ssh", tab_id, field))
}

fn ssh_form_has_focus(ui: &Ui, tab_id: TabId) -> bool {
    ["host", "port", "username", "password"]
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

fn show_ssh_form(ui: &mut Ui, tab_id: TabId, form: &mut SshLauncherForm) -> Option<AppCommand> {
    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);
    ui.label(egui::RichText::new("SSH password connection").strong());
    ui.label("Connect once with a transient password. Port defaults to 22.");

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
    let password_response =
        ssh_text_edit(ui, tab_id, "password", "Password", &mut form.password, true);

    let submit_with_enter =
        password_response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
    if ui.button("Connect with password").clicked() || submit_with_enter {
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

/// Renders the session launcher content and returns any dispatched command.
///
/// `docs/gui-design.md` ("Session Launcher"): fast, compact, and usable
/// repeatedly rather than a wizard or onboarding flow. The SSH password form
/// is a one-off connection surface: it creates no profile and retains its
/// password only in temporary UI state until submit. Saved profiles and other
/// authentication methods remain later work.
///
/// The list is keyboard-navigable: Up/Down moves a highlighted selection
/// (persisted per-tab via `tab_id`, so multiple open launcher tabs don't
/// share selection state) and Enter launches the highlighted item, without
/// requiring the mouse. `tab_id` identifies which launcher tab this is, since
/// egui's per-frame widget memory is otherwise shared across all callers
/// within the same panel.
pub fn show_launcher(ui: &mut Ui, tab_id: TabId) -> Option<AppCommand> {
    let items = [LauncherItem {
        label: "Local Shell (platform default)",
        command: start_local_session_command,
    }];

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
            let response = ui.add(egui::Button::new(item.label).selected(index == state.selected));
            if response.clicked() {
                command = Some((item.command)());
            }
        }
        if command.is_none() {
            command = show_ssh_form(ui, tab_id, &mut state.ssh);
        }
    });

    if command.is_none() && launch_via_keyboard {
        command = Some((items[state.selected].command)());
    }

    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
}

/// Renders the Settings application surface.
///
/// Versioned, persisted configuration (`festerm-config`) is M8 work and not
/// implemented yet. This establishes Settings as its own first-class
/// application surface with a dedicated chip today, per `docs/gui-design.md`
/// ("Settings as an application surface"): Settings never lives inside the
/// session inspector.
///
/// `chip_layout` reflects the current chip wrapping mode
/// (`docs/gui-design.md` "Wrapping must remain user-configurable"); this is
/// the one persistent preference implemented so far. Returns a command when
/// the user toggles it, dispatched through the same single command path as
/// every other invocation surface.
pub fn show_settings(
    ui: &mut Ui,
    chip_layout: ChipLayout,
    status_bar_visible: bool,
) -> Option<AppCommand> {
    let mut command = None;
    ui.vertical(|ui| {
        ui.add_space(24.0);
        ui.heading("Settings");
        ui.label(
            "Versioned, persisted configuration is not implemented yet. \
             Settings exists as its own application surface now so future \
             preferences have a stable, discoverable home.",
        );
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
        command: Option<AppCommand>,
    }

    fn harness() -> Harness<'static, LauncherHarnessState> {
        Harness::builder()
            .with_size(egui::vec2(520.0, 560.0))
            .build_ui_state(
                |ui, state: &mut LauncherHarnessState| {
                    if let Some(command) = show_launcher(ui, state.tab_id) {
                        state.command = Some(command);
                    }
                },
                LauncherHarnessState {
                    tab_id: AppState::for_test().active(),
                    command: None,
                },
            )
    }

    fn enter_text(harness: &mut Harness<'static, LauncherHarnessState>, label: &str, text: &str) {
        harness.get_by_label(label).click();
        harness.run();
        harness.get_by_label(label).type_text(text);
        harness.run();
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
}
