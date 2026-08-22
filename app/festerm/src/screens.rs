//! Launcher and Settings application-surface presentation.
//!
//! These are thin, product-specific screens rather than terminal chrome
//! (`crates/festerm-ui-egui/src/chrome.rs` owns the chip row). They translate
//! user gestures into `AppCommand`s per `docs/application-command-model.md`
//! and own no session or tab policy themselves; `AppState::dispatch` remains
//! the single command-handling path.

use eframe::egui::{self, vec2, ScrollArea, Sense, Stroke, TextEdit, Ui, WidgetInfo, WidgetType};
use festerm_config::{CredentialKind, PersistenceConfiguration, Profile, SshProfileConfiguration};
use festerm_session::{PasswordPrompt, TerminalSize};
use festerm_ssh::{
    HostIdentity, ReconnectPolicy, RecoveryPolicy, SessionStrategy, SshAuthentication,
    SshConnectionProfile, SshKeyPassphrase, SshPrivateKey, SshPrivateKeyError, SshSessionOptions,
};
use festerm_ui_egui::{chrome::ChipLayout, icon, icon::Icon, theme};

use crate::configuration_startup::ConfigurationStartupStatus;
use crate::tabs::{AppCommand, PasswordToStore, PrivateKeyToStore, TabId};

/// One selectable launch option in the Launcher list: the fixed default
/// local shell, or a saved local/SSH profile.
enum LauncherItemKind<'a> {
    LocalDefault,
    NewSsh,
    LocalProfile(&'a str),
    SshProfile(&'a str),
}

struct LauncherItem<'a> {
    label: String,
    description: String,
    kind: LauncherItemKind<'a>,
}

impl LauncherItem<'_> {
    fn profile_id(&self) -> Option<&str> {
        match self.kind {
            LauncherItemKind::LocalDefault | LauncherItemKind::NewSsh => None,
            LauncherItemKind::LocalProfile(id) | LauncherItemKind::SshProfile(id) => Some(id),
        }
    }

    fn remote(&self) -> bool {
        matches!(
            self.kind,
            LauncherItemKind::NewSsh | LauncherItemKind::SshProfile(_)
        )
    }

    fn command(&self) -> AppCommand {
        match self.kind {
            LauncherItemKind::LocalDefault => AppCommand::StartLocalSession,
            LauncherItemKind::NewSsh => {
                unreachable!("the New SSH Connection item opens the SSH form, not an AppCommand")
            }
            LauncherItemKind::LocalProfile(profile_id) => AppCommand::StartConfiguredLocalProfile {
                profile_id: profile_id.to_owned(),
            },
            LauncherItemKind::SshProfile(profile_id) => AppCommand::StartConfiguredSshProfile {
                profile_id: profile_id.to_owned(),
            },
        }
    }
}

/// Renders one Launcher card. Saved profiles (`editable`) also get a small
/// edit-icon control in the card's upper-right corner: clicking it opens
/// that profile's editor instead of launching it, while clicking anywhere
/// else on the card launches, matching how the standalone Profiles surface
/// and this card share one visual language.
fn show_launcher_choice(
    ui: &mut Ui,
    primary: &str,
    secondary: &str,
    selected: bool,
    remote: bool,
    editable: bool,
) -> (egui::Response, Option<egui::Response>) {
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

    let edit_response = editable.then(|| {
        let edit_rect =
            egui::Rect::from_min_size(rect.right_top() + vec2(-30.0, 8.0), vec2(22.0, 22.0));
        let edit_id = response.id.with("edit");
        let edit_response = ui.interact(edit_rect, edit_id, Sense::click());
        let edit_active = edit_response.hovered();
        ui.painter().rect(
            edit_rect,
            4.0,
            if edit_active {
                theme::SURFACE_TAB_ACTIVE
            } else {
                egui::Color32::TRANSPARENT
            },
            Stroke::NONE,
            egui::StrokeKind::Inside,
        );
        icon::paint(
            ui.painter(),
            Icon::Edit,
            edit_rect.shrink(3.0),
            if edit_active {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_SECONDARY
            },
        );
        edit_response.widget_info(|| {
            WidgetInfo::labeled(WidgetType::Button, true, format!("Edit {primary}"))
        });
        edit_response
    });

    (response, edit_response)
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
#[derive(Clone)]
struct SshLauncherForm {
    host: String,
    port: String,
    username: String,
    /// The default, minimal entry point: a single `user@host[:port]` field
    /// parsed by `parse_quick_connect`. Shown instead of the full form until
    /// `advanced_open` is set, matching how most SSH clients' fast path
    /// works; IPv6 bracket notation (`user@[::1]:22`) is not specially
    /// handled and needs the advanced form's separate Host field instead.
    quick_connect: String,
    /// Whether the full connection form (separate Host/Port/persistence/
    /// authentication-method fields) is shown instead of the single Quick
    /// Connect field. Always `true` once a saved or restored profile is
    /// prefilled (`prefill_from_profile`), since its host/username are
    /// already known and Quick Connect's only purpose is fast ad-hoc entry.
    advanced_open: bool,
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

impl Default for SshLauncherForm {
    /// Port starts prefilled with the actual default (`"22"`) rather than
    /// an empty field with "(default: 22)" wording, so the box always shows
    /// the value that will actually be used.
    fn default() -> Self {
        Self {
            host: String::new(),
            port: Self::DEFAULT_PORT.to_string(),
            username: String::new(),
            quick_connect: String::new(),
            advanced_open: false,
            authentication_method: SshAuthenticationMethod::default(),
            password: String::new(),
            private_key: String::new(),
            key_passphrase: String::new(),
            saved_profile_id: None,
            saved_profile_has_credential: false,
            persistence: None,
            automatic_recovery: false,
            remember_password: false,
            feedback: None,
            focus_username: false,
        }
    }
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
        self.advanced_open = true;
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
            SshAuthenticationMethod::Password if password.is_empty() => {
                Ok(AppCommand::StartSshSession {
                    profile,
                    authentication: SshAuthentication::interactive(),
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

    /// Parses `quick_connect` ("user@host" or "user@host:port") into the
    /// same `host`/`port`/`username` fields the advanced form edits
    /// directly, then submits through the same `submit()` path with no
    /// password. That empty password is exactly what routes the connection
    /// to the in-terminal password prompt (see `submit()`'s Password
    /// branch) rather than attempting to connect with no credential.
    fn submit_quick_connect(&mut self) -> Result<AppCommand, String> {
        self.parse_quick_connect()?;
        self.password.clear();
        self.authentication_method = SshAuthenticationMethod::Password;
        self.submit()
    }

    /// See `quick_connect`'s doc comment for the notation this accepts.
    fn parse_quick_connect(&mut self) -> Result<(), String> {
        let input = self.quick_connect.trim();
        if input.is_empty() {
            return Err("Enter a destination, e.g. user@host".to_owned());
        }
        let (username, remainder) = input
            .split_once('@')
            .ok_or_else(|| "Enter a destination as user@host".to_owned())?;
        if username.is_empty() {
            return Err("Enter a username before @".to_owned());
        }
        let (host, port) = match remainder.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (remainder, None),
        };
        if host.is_empty() {
            return Err("Enter a host after @".to_owned());
        }
        self.username = username.to_owned();
        self.host = host.to_owned();
        self.port = port
            .map(str::to_owned)
            .unwrap_or_else(|| Self::DEFAULT_PORT.to_string());
        Ok(())
    }

    /// Best-effort version of `parse_quick_connect` for toggling to the
    /// advanced form: fills in whatever `username`/`host`/`port` it can from
    /// `quick_connect`, but never surfaces or blocks on a parse error, since
    /// revealing the advanced form must always succeed.
    fn sync_advanced_from_quick_connect(&mut self) {
        let _ = self.parse_quick_connect();
    }

    /// Inverse of `sync_advanced_from_quick_connect`: composes the advanced
    /// form's `username`/`host`/`port` back into the single quick-connect
    /// field, omitting the port when it is still the default, so toggling
    /// back and forth round-trips what the user actually typed.
    fn sync_quick_connect_from_advanced(&mut self) {
        if self.username.is_empty() && self.host.is_empty() {
            return;
        }
        let port = self.port.trim();
        self.quick_connect = if port.is_empty() || port == Self::DEFAULT_PORT.to_string() {
            format!("{}@{}", self.username, self.host)
        } else {
            format!("{}@{}:{}", self.username, self.host, port)
        };
    }

    /// Reveals the advanced form, carrying forward whatever destination the
    /// user already typed into Quick Connect and clearing any stale
    /// feedback from that surface (item 4: feedback must not persist across
    /// a toggle it no longer describes).
    fn open_advanced_settings(&mut self) {
        self.sync_advanced_from_quick_connect();
        self.feedback = None;
        self.advanced_open = true;
        self.focus_username = true;
    }

    /// Inverse of `open_advanced_settings`: returns to Quick Connect,
    /// carrying the advanced form's destination back into the single field
    /// and clearing any stale feedback from the advanced form.
    fn close_advanced_settings(&mut self) {
        self.sync_quick_connect_from_advanced();
        self.feedback = None;
        self.advanced_open = false;
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
                .desired_rows(5),
        )
        .labelled_by(label.id)
    })
    .inner
}

/// The default, minimal launcher surface for a fresh SSH connection: a
/// single `user@host[:port]` field and a Connect button, matching how most
/// SSH clients' fast path works (`SshLauncherForm::quick_connect`).
/// Submitting always goes through `submit_quick_connect`, which leaves the
/// password empty so the connection starts interactively (host-key-first,
/// then `show_ssh_live_password_prompt`) rather than collecting a password
/// blind before a connection exists — exactly mirroring how other SSH
/// clients defer the password prompt until it's actually needed.
fn show_ssh_quick_connect(
    ui: &mut Ui,
    tab_id: TabId,
    form: &mut SshLauncherForm,
    focus_quick_connect: bool,
) -> Option<AppCommand> {
    let mut result = None;
    ssh_section_heading(ui, "Quick Connect");
    let submit_with_enter = ui
        .horizontal(|ui| {
            ui.add_space(2.0);
            let label = ui.add(
                egui::Label::new(egui::RichText::new("user@host").color(theme::TEXT_SECONDARY))
                    .selectable(false),
            );
            let field = ui.add(
                TextEdit::singleline(&mut form.quick_connect)
                    .id_salt(("launcher_ssh", tab_id, "quick_connect"))
                    .hint_text("example@169.254.1.1")
                    .desired_width(220.0),
            );
            if focus_quick_connect {
                field.request_focus();
            }
            let field = field.labelled_by(label.id);
            field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
        })
        .inner;
    ui.add_space(8.0);
    if ui.button("Connect").clicked() || submit_with_enter {
        match form.submit_quick_connect() {
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
    ui.add_space(10.0);
    let mut show_advanced = form.advanced_open;
    if ui
        .checkbox(&mut show_advanced, "Show advanced settings")
        .changed()
        && show_advanced
    {
        form.open_advanced_settings();
    }
    result
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
            if !form.advanced_open {
                result = show_ssh_quick_connect(ui, tab_id, form, focus_username);
                return;
            }
            let mut show_advanced = form.advanced_open;
            if ui
                .checkbox(&mut show_advanced, "Show advanced settings")
                .changed()
                && !show_advanced
            {
                form.close_advanced_settings();
            }
            ui.add_space(10.0);
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
            ssh_text_edit(ui, tab_id, "port", "Port", &mut form.port, false, false);

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

/// Renders the session launcher content and returns any dispatched command.
///
/// `docs/gui-design.md` ("Session Launcher"): fast, compact, and usable
/// repeatedly rather than a wizard or onboarding flow. The SSH form is a
/// one-off connection surface: it creates no profile and retains password,
/// key text, and key passphrases only in temporary UI state until submit.
/// Saved local and SSH profiles both launch directly from their own card,
/// with no password prompt in this surface: a saved SSH profile with a
/// stored native credential launches with it, and one without launches
/// through the same openssh-style in-terminal interactive prompt Quick
/// Connect uses. Entering or replacing a saved SSH profile's stored
/// password lives in the Profiles editor, not here.
///
/// The list is keyboard-navigable: Up/Down moves a highlighted selection
/// (persisted against the singleton Launcher's `tab_id`), Tab cycles through
/// it the same way, and Enter launches the highlighted item without
/// requiring the mouse. The id prevents this temporary state from colliding
/// with other application-surface widgets.
pub fn show_launcher(
    ui: &mut Ui,
    tab_id: TabId,
    profiles: &[Profile],
    native_store_available: bool,
    secure_storage_status: Option<&str>,
) -> Option<AppCommand> {
    let mut items = vec![
        LauncherItem {
            label: "Local Shell".to_owned(),
            description: "Default shell on this computer".to_owned(),
            kind: LauncherItemKind::LocalDefault,
        },
        LauncherItem {
            label: "SSH".to_owned(),
            description: "Connect to a remote host".to_owned(),
            kind: LauncherItemKind::NewSsh,
        },
    ];
    // Saved profiles are listed last, after the two fixed "new session"
    // entries above, with a subtle separator (rendered when painting the
    // list below) marking where they start.
    let profiles_start = items.len();
    items.extend(
        profiles
            .iter()
            .filter_map(Profile::as_local)
            .map(|profile| LauncherItem {
                label: profile.identifier().to_owned(),
                description: "Saved local profile".to_owned(),
                kind: LauncherItemKind::LocalProfile(profile.identifier()),
            }),
    );
    items.extend(
        profiles
            .iter()
            .filter_map(Profile::as_ssh)
            .map(|profile| LauncherItem {
                label: profile.identifier().to_owned(),
                description: format!(
                    "Saved SSH profile · {}@{}:{}",
                    profile.username(),
                    profile.host(),
                    profile.port()
                ),
                kind: LauncherItemKind::SshProfile(profile.identifier()),
            }),
    );

    let state_id = launcher_state_id(tab_id);
    let mut state = ui.data(|data| data.get_temp::<LauncherState>(state_id).unwrap_or_default());
    state.selected = state.selected.min(items.len().saturating_sub(1));

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
    let cycle_forward = !form_has_focus
        && ui.input(|i| {
            i.key_pressed(egui::Key::ArrowDown)
                || (i.key_pressed(egui::Key::Tab) && !i.modifiers.shift)
        });
    let cycle_backward = !form_has_focus
        && ui.input(|i| {
            i.key_pressed(egui::Key::ArrowUp)
                || (i.key_pressed(egui::Key::Tab) && i.modifiers.shift)
        });
    if cycle_forward {
        state.selected = (state.selected + 1) % items.len();
    }
    if cycle_backward {
        state.selected = (state.selected + items.len() - 1) % items.len();
    }
    let launch_via_keyboard = !form_has_focus && ui.input(|i| i.key_pressed(egui::Key::Enter));

    let mut command = None;
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
                    if index == profiles_start && profiles_start < items.len() {
                        ui.add_space(8.0);
                        // Matches the 26px left inset applied above so the
                        // divider reads as evenly padded on both sides
                        // instead of running flush to the pane's right edge.
                        let separator_width = (ui.available_width() - 26.0).max(0.0);
                        ui.scope(|ui| {
                            ui.set_width(separator_width);
                            ui.separator();
                        });
                        ui.add_space(8.0);
                    }
                    let (response, edit_response) = show_launcher_choice(
                        ui,
                        &item.label,
                        &item.description,
                        index == state.selected,
                        item.remote(),
                        item.profile_id().is_some(),
                    );
                    if edit_response.is_some_and(|edit| edit.clicked()) {
                        command = Some(AppCommand::OpenProfileEditor {
                            identifier: item
                                .profile_id()
                                .expect("editable launcher items always carry a profile id")
                                .to_owned(),
                        });
                    } else if response.clicked() {
                        if matches!(item.kind, LauncherItemKind::NewSsh) {
                            state.ssh_open = true;
                            state.ssh.focus_username = true;
                        } else {
                            command = Some(item.command());
                        }
                    }
                    ui.add_space(12.0);
                }
            });
        });
        if let Some(status) = secure_storage_status {
            ui.add_space(8.0);
            ui.colored_label(egui::Color32::from_rgb(220, 150, 80), status);
        }
    });

    if command.is_none() && launch_via_keyboard {
        if matches!(items[state.selected].kind, LauncherItemKind::NewSsh) {
            state.ssh_open = true;
            state.ssh.focus_username = true;
        } else {
            command = Some(items[state.selected].command());
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

/// Ephemeral per-tab state for the live-session in-terminal password prompt
/// below: only a transient input buffer and the most recently seen prompt
/// attempt, so a fresh attempt (e.g. after a rejected retry) claims focus
/// and starts from an empty field. Following the same `ui.data_mut`/
/// `insert_temp` pattern as `show_ssh_form`'s state, since resolving the
/// prompt is owned entirely by the already-connected SSH worker — no
/// persistent UI struct is needed on `SessionTab`.
#[derive(Clone, Default)]
struct SshLivePasswordPromptState {
    password: String,
    last_attempt: Option<u8>,
    /// Completed prompt/outcome lines from earlier attempts on this same
    /// connection, appended to (never replaced) so failed retries scroll
    /// upward exactly like a real terminal transcript.
    history: Vec<String>,
}

/// Renders a blinking block cursor at roughly the rate real terminal
/// emulators use (~530ms), requesting a repaint so the blink keeps
/// animating a pty-styled prompt that has no live terminal view underneath
/// it to drive redraws on its own.
pub(crate) fn pty_cursor_glyph(ui: &Ui) -> &'static str {
    const BLINK_INTERVAL_SECS: f64 = 0.53;
    let time = ui.input(|input| input.time);
    let visible = (time / BLINK_INTERVAL_SECS) as i64 % 2 == 0;
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_secs_f64(BLINK_INTERVAL_SECS));
    if visible {
        "█"
    } else {
        " "
    }
}

/// Renders the openssh-style, in-terminal password prompt for a session
/// that is already connected (host key already verified) and now waiting
/// for a password, mimicking `ssh`'s own `user@host's password:` line
/// instead of collecting a credential before a connection exists.
///
/// Submitting dispatches `AppCommand::ResolveSshPassword`, feeding the
/// value directly into the live worker rather than starting a fresh
/// connection.
pub fn show_ssh_live_password_prompt(
    ui: &mut Ui,
    tab_id: TabId,
    prompt: &PasswordPrompt,
) -> Option<AppCommand> {
    if !festerm_ui_egui::terminal_fonts_installed(ui.ctx()) {
        // Mirrors `TerminalView`'s own guard: the named terminal font
        // family only becomes usable after egui rebuilds its atlas at the
        // next pass boundary, so skip laying out text with it this frame.
        festerm_ui_egui::install_terminal_fonts(ui.ctx());
        ui.ctx().request_repaint();
        return None;
    }
    let state_id = ui.id().with(("ssh_live_password_prompt", tab_id));
    let mut state: SshLivePasswordPromptState =
        ui.data_mut(|data| data.get_temp(state_id).unwrap_or_default());
    let prompt_line = format!("{}@{}'s password:", prompt.username(), prompt.host());
    if state.last_attempt != Some(prompt.attempt()) {
        // A new attempt arrived: fold the just-finished attempt's prompt
        // (and, if it was rejected, ssh's own "Permission denied" line)
        // into the growing transcript, then start the next attempt fresh.
        if state.last_attempt.is_some() {
            state.history.push(prompt_line.clone());
            if prompt.previous_attempt_failed() {
                state
                    .history
                    .push("Permission denied, please try again.".to_owned());
            }
        }
        state.last_attempt = Some(prompt.attempt());
        state.password.clear();
    }

    let font = festerm_ui_egui::terminal_font(festerm_ui_egui::DEFAULT_TERMINAL_FONT_SIZE);
    let mono = |text: String, color: egui::Color32| {
        egui::RichText::new(text).font(font.clone()).color(color)
    };
    let mut command = None;
    egui::Frame::new()
        .fill(theme::SURFACE_TERMINAL)
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.label(mono(
                format!("ssh {}@{}", prompt.username(), prompt.host()),
                theme::TEXT_SECONDARY,
            ));
            ui.add_space(4.0);
            for line in &state.history {
                ui.label(mono(line.clone(), theme::TEXT_PRIMARY));
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(mono(prompt_line, theme::TEXT_PRIMARY));
                ui.label(mono(pty_cursor_glyph(ui).to_owned(), theme::TEXT_PRIMARY));
            });
        });

    // Keyboard-driven and unechoed, matching real `ssh`: no terminal view
    // or text field is shown this frame to compete for these keys, and
    // typed characters are captured without ever being reflected on
    // screen (not even as masked dots).
    let submitted = ui.ctx().input_mut(|input| {
        let mut submit = false;
        input.events.retain(|event| match event {
            egui::Event::Text(text) => {
                state.password.push_str(text);
                false
            }
            egui::Event::Key {
                key: egui::Key::Backspace,
                pressed: true,
                ..
            } => {
                state.password.pop();
                false
            }
            egui::Event::Key {
                key: egui::Key::Enter,
                pressed: true,
                ..
            } => {
                submit = true;
                false
            }
            _ => true,
        });
        submit
    });
    if submitted {
        let password = std::mem::take(&mut state.password);
        command = Some(AppCommand::ResolveSshPassword {
            tab: tab_id,
            password,
        });
    }

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
    command_palette_shortcut: &str,
) -> Option<AppCommand> {
    let mut command = None;
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        ui.vertical(|ui| {
            ui.add_space(24.0);
            ui.heading("Settings");
            ui.add_space(2.0);
            ssh_paragraph(ui, "Configuration is never written automatically.");
            ui.add_space(16.0);

            settings_card(ui, "Configuration", |ui| {
                let configuration_message = configuration_status.settings_message();
                if configuration_status.is_problem() {
                    ui.colored_label(theme::STATUS_ERROR, configuration_message);
                } else {
                    ssh_paragraph(ui, configuration_message);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Reload configuration").clicked() {
                        command = Some(AppCommand::ReloadConfiguration);
                    }
                    if ui.button("Save workspace").clicked() {
                        command = Some(AppCommand::SaveWorkspace);
                    }
                });
                if let Some(status) = secure_storage_status {
                    ui.add_space(10.0);
                    ssh_section_heading(ui, "Native secure storage");
                    ui.colored_label(theme::STATUS_ERROR, status);
                }
            });

            ui.add_space(12.0);

            settings_card(ui, "Interface", |ui| {
                let wrap = matches!(chip_layout, ChipLayout::Wrap);
                let label = if wrap {
                    "Chip layout: wrap onto multiple rows"
                } else {
                    "Chip layout: single row (compact, then scroll)"
                };
                if ui.button(label).clicked() {
                    command = Some(AppCommand::ToggleChipLayout);
                }
                ui.add_space(6.0);
                let status_bar_label = if status_bar_visible {
                    "Status bar: shown"
                } else {
                    "Status bar: hidden"
                };
                if ui.button(status_bar_label).clicked() {
                    command = Some(AppCommand::ToggleStatusBar);
                }
                ui.add_space(8.0);
                ssh_paragraph(
                    ui,
                    "Chip layout and status bar visibility are saved automatically.",
                );
                ui.add_space(10.0);
                if ui.button("Reset interface settings to defaults").clicked() {
                    command = Some(AppCommand::ResetInterfaceSettings);
                }
            });

            ui.add_space(12.0);

            settings_card(ui, "Keyboard", |ui| {
                ui.horizontal(|ui| {
                    ssh_paragraph(ui, "Command palette");
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(command_palette_shortcut)
                            .size(12.0)
                            .color(theme::TEXT_MUTED),
                    );
                });
            });
        });
    });
    command
}

/// A titled card matching the launcher/profile-editor "quiet section" visual
/// language (`ssh_section_heading` + a bordered, rounded surface), so
/// Settings groups related controls the same way the rest of the app does
/// instead of a flat, plain list of buttons.
fn settings_card(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(theme::SURFACE_TAB_INACTIVE)
        .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(420.0);
            ssh_section_heading(ui, title);
            ui.add_space(6.0);
            body(ui);
        });
}

/// One row's stable identifying summary in the Profiles list, without
/// exposing credential material.
fn profile_summary(profile: &Profile) -> (&'static str, String, String) {
    match profile {
        Profile::Local(local) => {
            let mut description = local.executable().to_owned();
            if !local.arguments().is_empty() {
                description.push(' ');
                description.push_str(&local.arguments().join(" "));
            }
            ("Local", local.identifier().to_owned(), description)
        }
        Profile::Ssh(ssh) => (
            "SSH",
            ssh.identifier().to_owned(),
            format!("{}@{}:{}", ssh.username(), ssh.host(), ssh.port()),
        ),
    }
}

/// Which staged view the Profiles surface is currently showing. Multi-field
/// edits are staged behind Save; Cancel discards them
/// (`docs/gui-design.md` "Profile editing").
#[derive(Clone, Default)]
enum ProfilesScreenMode {
    #[default]
    List,
    EditLocal(LocalProfileDraft),
    EditSsh(SshProfileDraft),
    ConfirmDelete {
        identifier: String,
        references: usize,
    },
}

#[derive(Clone, Default)]
struct ProfilesScreenState {
    mode: ProfilesScreenMode,
}

fn profiles_state_id(tab_id: TabId) -> egui::Id {
    egui::Id::new(("profiles_state", tab_id))
}

#[derive(Clone)]
struct LocalProfileDraft {
    /// `None` while creating a new profile; `Some` while editing an
    /// existing one, so Save always upserts by this original identifier
    /// rather than the (possibly just-edited) name field.
    original_id: Option<String>,
    name: String,
    executable: String,
    arguments: String,
    working_directory: String,
    error: Option<String>,
}

impl Default for LocalProfileDraft {
    /// A brand-new Local profile defaults its executable to this
    /// platform's actual default shell (`$SHELL`/`COMSPEC`, matching the
    /// Local Shell launcher card) rather than leaving it empty.
    fn default() -> Self {
        Self {
            original_id: None,
            name: String::new(),
            executable: festerm_pty::default_local_profile()
                .map(|profile| profile.executable().display().to_string())
                .unwrap_or_default(),
            arguments: String::new(),
            working_directory: String::new(),
            error: None,
        }
    }
}

impl LocalProfileDraft {
    fn from_profile(local: &festerm_config::LocalProfileConfiguration) -> Self {
        Self {
            original_id: Some(local.identifier().to_owned()),
            name: local.identifier().to_owned(),
            executable: local.executable().to_owned(),
            arguments: local.arguments().join(" "),
            working_directory: local
                .working_directory()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            error: None,
        }
    }

    fn build(&self) -> Result<Profile, festerm_config::ConfigError> {
        let arguments = self
            .arguments
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let working_directory = (!self.working_directory.trim().is_empty())
            .then(|| self.working_directory.trim().to_owned());
        Profile::local(
            self.name.trim(),
            self.executable.trim(),
            arguments,
            working_directory,
        )
    }
}

#[derive(Clone)]
struct SshProfileDraft {
    original_id: Option<String>,
    name: String,
    host: String,
    port: String,
    username: String,
    /// Which credential kind the editor's authentication section is
    /// currently showing entry fields for. Independent of
    /// `stored_credential_kind`, which reflects what is actually saved —
    /// switching this radio only changes which fields are visible/active
    /// until "Save password"/"Save private key" is clicked.
    auth_method: SshAuthenticationMethod,
    /// Transient plaintext password entry for "remember/replace password"
    /// in the profile editor (item 5: relocated here from the launcher's
    /// live-connect form). Never persisted to disk directly — only ever
    /// sent to the composition root's secret store worker via
    /// `AppCommand::StoreProfilePassword` and cleared immediately after.
    password: String,
    /// Transient OpenSSH private-key text for "remember/replace private
    /// key" in the profile editor. Like `password`, never persisted
    /// directly — only ever sent to the composition root's secret store
    /// worker via `AppCommand::StoreProfilePrivateKey` and cleared
    /// immediately after.
    private_key: String,
    /// Transient optional passphrase for `private_key`, cleared alongside it.
    key_passphrase: String,
    has_stored_credential: bool,
    /// Which kind of credential is actually stored for this profile.
    /// Meaningless unless `has_stored_credential` is true.
    stored_credential_kind: CredentialKind,
    error: Option<String>,
}

impl Default for SshProfileDraft {
    /// A brand-new SSH profile defaults its port to "22" in the text box
    /// (matching Quick Connect's `SshLauncherForm::DEFAULT_PORT`) rather
    /// than leaving it empty.
    fn default() -> Self {
        Self {
            original_id: None,
            name: String::new(),
            host: String::new(),
            port: SshLauncherForm::DEFAULT_PORT.to_string(),
            username: String::new(),
            auth_method: SshAuthenticationMethod::Password,
            password: String::new(),
            private_key: String::new(),
            key_passphrase: String::new(),
            has_stored_credential: false,
            stored_credential_kind: CredentialKind::Password,
            error: None,
        }
    }
}

impl SshProfileDraft {
    fn from_profile(ssh: &SshProfileConfiguration) -> Self {
        let stored_credential_kind = ssh.credential_kind();
        Self {
            original_id: Some(ssh.identifier().to_owned()),
            name: ssh.identifier().to_owned(),
            host: ssh.host().to_owned(),
            port: ssh.port().to_string(),
            username: ssh.username().to_owned(),
            auth_method: match stored_credential_kind {
                CredentialKind::Password => SshAuthenticationMethod::Password,
                CredentialKind::PrivateKey => SshAuthenticationMethod::PrivateKey,
            },
            password: String::new(),
            private_key: String::new(),
            key_passphrase: String::new(),
            has_stored_credential: ssh.credential_reference().is_some(),
            stored_credential_kind,
            error: None,
        }
    }

    fn build(&self) -> Result<Profile, ()> {
        let port: u16 = self.port.trim().parse().map_err(|_| ())?;
        Profile::ssh(
            self.name.trim(),
            self.host.trim(),
            port,
            self.username.trim(),
            "xterm-256color",
            80,
            24,
        )
        .map_err(|_| ())
    }
}

/// The standalone Profiles management surface: list, create, edit,
/// duplicate, and delete reusable local/SSH launch definitions
/// (`docs/gui-design.md` "Profile editing").
fn profile_text_edit(
    ui: &mut Ui,
    tab_id: TabId,
    field: &'static str,
    label: &str,
    value: &mut String,
) -> egui::Response {
    profile_text_edit_inner(ui, tab_id, field, label, value, false)
}

fn profile_password_edit(
    ui: &mut Ui,
    tab_id: TabId,
    field: &'static str,
    label: &str,
    value: &mut String,
) -> egui::Response {
    profile_text_edit_inner(ui, tab_id, field, label, value, true)
}

fn profile_text_edit_inner(
    ui: &mut Ui,
    tab_id: TabId,
    field: &'static str,
    label: &str,
    value: &mut String,
    password: bool,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        let label = ui.add(
            egui::Label::new(egui::RichText::new(label).color(theme::TEXT_SECONDARY))
                .selectable(false),
        );
        let field = ui.add(
            TextEdit::singleline(value)
                .id_salt(("profiles_form", tab_id, field))
                .password(password)
                .desired_width(240.0),
        );
        field.labelled_by(label.id)
    })
    .inner
}

/// Maximum number of `PATH` matches offered below the Local profile
/// executable field as the user types.
const EXECUTABLE_SUGGESTION_LIMIT: usize = 6;

/// The Local profile editor's executable field, with a live `PATH`-search
/// dropdown: as the user types a bare command name (e.g. `cmd`), this
/// offers up to [`EXECUTABLE_SUGGESTION_LIMIT`] concrete absolute paths
/// found on `PATH` so they can pin down exactly which one to launch
/// instead of relying on fesTerm's own search order at spawn time.
/// Selecting a suggestion fills in its absolute path; leaving the field as
/// a bare name is equally valid — it is resolved against `PATH` normally
/// when the profile launches.
fn local_executable_field(ui: &mut Ui, autocomplete_id: egui::Id, value: &mut String) {
    ui.vertical(|ui| {
        let field = ui
            .horizontal(|ui| {
                let label = ui.add(
                    egui::Label::new(
                        egui::RichText::new("Executable").color(theme::TEXT_SECONDARY),
                    )
                    .selectable(false),
                );
                let field = ui.add(TextEdit::singleline(value).desired_width(240.0));
                field.labelled_by(label.id)
            })
            .inner;

        let mut suppress = ui.data(|data| data.get_temp::<bool>(autocomplete_id).unwrap_or(false));
        if field.changed() {
            suppress = false;
        }

        if field.has_focus() && !suppress && !value.trim().is_empty() {
            let suggestions =
                festerm_pty::search_path_executables(value.trim(), EXECUTABLE_SUGGESTION_LIMIT);
            if !suggestions.is_empty() {
                ui.add_space(4.0);
                egui::Frame::new()
                    .fill(theme::SURFACE_TAB_INACTIVE)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .corner_radius(6.0)
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        for candidate in &suggestions {
                            let text = candidate.display().to_string();
                            if ui.selectable_label(false, &text).clicked() {
                                *value = text;
                                suppress = true;
                            }
                        }
                    });
            }
        }
        ui.data_mut(|data| data.insert_temp(autocomplete_id, suppress));
    });
}

pub fn show_profiles(
    ui: &mut Ui,
    tab_id: TabId,
    configuration: &festerm_config::Configuration,
    pending_edit: Option<String>,
) -> Option<AppCommand> {
    let state_id = profiles_state_id(tab_id);
    let mut state = ui.data(|data| {
        data.get_temp::<ProfilesScreenState>(state_id)
            .unwrap_or_default()
    });
    let mut command = None;

    if let Some(identifier) = pending_edit {
        if let Some(profile) = configuration.profile(&identifier) {
            state.mode = match profile {
                Profile::Local(local) => {
                    ProfilesScreenMode::EditLocal(LocalProfileDraft::from_profile(local))
                }
                Profile::Ssh(ssh) => {
                    ProfilesScreenMode::EditSsh(SshProfileDraft::from_profile(ssh))
                }
            };
        }
    }

    let mut next_mode = None;
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        ui.vertical(|ui| {
    match &mut state.mode {
        ProfilesScreenMode::List => {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.heading("Profiles");
                ui.label("Reusable local shell and SSH launch definitions.");
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("New Local Profile").clicked() {
                        next_mode =
                            Some(ProfilesScreenMode::EditLocal(LocalProfileDraft::default()));
                    }
                    if ui.button("New SSH Profile").clicked() {
                        next_mode = Some(ProfilesScreenMode::EditSsh(SshProfileDraft::default()));
                    }
                });
                ui.add_space(12.0);
                ui.separator();
                if configuration.profiles().is_empty() {
                    ui.add_space(12.0);
                    ui.label("No profiles saved yet.");
                }
                for profile in configuration.profiles() {
                    let (kind, name, description) = profile_summary(profile);
                    ui.add_space(8.0);
                    // Drag-and-drop reorder (`Configuration::with_reordered_profiles`),
                    // reflected in the Launcher's own profile ordering too.
                    // Only the chip frame itself is a drag source, matching
                    // the chrome chip row's press-and-hold-anywhere-on-the-
                    // card convention; the Connect/Edit/Duplicate/Delete
                    // buttons sit outside it and are unaffected.
                    let drag_id = egui::Id::new("profile_reorder_source").with(&name);
                    let mut row_rect = None;
                    ui.horizontal(|ui| {
                        let drag_response = ui.dnd_drag_source(drag_id, name.clone(), |ui| {
                            egui::Frame::new()
                                .fill(theme::SURFACE_TAB_INACTIVE)
                                .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
                                .corner_radius(8.0)
                                .inner_margin(egui::Margin::symmetric(14, 10))
                                .show(ui, |ui| {
                                    ui.set_width(220.0);
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(&name).strong());
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{kind} · {description}"
                                            ))
                                            .size(11.0)
                                            .color(theme::TEXT_SECONDARY),
                                        );
                                    });
                                });
                        });
                        row_rect = Some(drag_response.response.rect);
                        ui.add_space(8.0);
                        if ui.button("Connect").clicked() {
                            command = Some(match profile {
                                Profile::Local(_) => AppCommand::StartConfiguredLocalProfile {
                                    profile_id: name.clone(),
                                },
                                Profile::Ssh(_) => AppCommand::StartConfiguredSshProfile {
                                    profile_id: name.clone(),
                                },
                            });
                        }
                        if ui.button("Edit").clicked() {
                            next_mode = Some(match profile {
                                Profile::Local(local) => ProfilesScreenMode::EditLocal(
                                    LocalProfileDraft::from_profile(local),
                                ),
                                Profile::Ssh(ssh) => ProfilesScreenMode::EditSsh(
                                    SshProfileDraft::from_profile(ssh),
                                ),
                            });
                        }
                        if ui.button("Duplicate").clicked() {
                            let duplicate_name = format!("{name}-copy");
                            next_mode = Some(match profile {
                                Profile::Local(local) => {
                                    let mut draft = LocalProfileDraft::from_profile(local);
                                    draft.original_id = None;
                                    draft.name = duplicate_name;
                                    ProfilesScreenMode::EditLocal(draft)
                                }
                                Profile::Ssh(ssh) => {
                                    let mut draft = SshProfileDraft::from_profile(ssh);
                                    draft.original_id = None;
                                    draft.name = duplicate_name;
                                    ProfilesScreenMode::EditSsh(draft)
                                }
                            });
                        }
                        if ui.button("Delete").clicked() {
                            next_mode = Some(ProfilesScreenMode::ConfirmDelete {
                                identifier: name.clone(),
                                references: configuration.workspace_tab_references(&name),
                            });
                        }
                    });
                    // Only commit the reorder when the drag is released over
                    // this row (not continuously while hovering): each
                    // reorder is persisted to disk immediately
                    // (`ConfigurationReloader::reorder_profiles`), so
                    // dispatching on every hovered frame during a drag would
                    // write the configuration file dozens of times per
                    // second.
                    if let Some(rect) = row_rect {
                        if let Some(dragged) = egui::DragAndDrop::payload::<String>(ui.ctx()) {
                            let released = ui.input(|i| i.pointer.any_released());
                            if let Some(pointer_pos) = ui.ctx().pointer_interact_pos() {
                                if *dragged != name && released && rect.contains(pointer_pos) {
                                    command = Some(AppCommand::ReorderProfiles {
                                        moved: (*dragged).clone(),
                                        before: Some(name.clone()),
                                    });
                                }
                            }
                        }
                    }
                }
                // A trailing drop target lets a drag be released past the
                // last profile row to move it to the end of the list.
                if !configuration.profiles().is_empty() {
                    let (end_rect, _) =
                        ui.allocate_exact_size(vec2(220.0, 12.0), Sense::hover());
                    if let Some(dragged) = egui::DragAndDrop::payload::<String>(ui.ctx()) {
                        let released = ui.input(|i| i.pointer.any_released());
                        if let Some(pointer_pos) = ui.ctx().pointer_interact_pos() {
                            if released
                                && configuration
                                    .profiles()
                                    .last()
                                    .is_some_and(|last| last.identifier() != dragged.as_str())
                                && end_rect.contains(pointer_pos)
                            {
                                command = Some(AppCommand::ReorderProfiles {
                                    moved: (*dragged).clone(),
                                    before: None,
                                });
                            }
                        }
                    }
                }
            });
        }
        ProfilesScreenMode::EditLocal(draft) => {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.heading(if draft.original_id.is_some() {
                    "Edit Local Profile"
                } else {
                    "New Local Profile"
                });
                ui.add_space(16.0);
                egui::Frame::new()
                    .fill(theme::SURFACE_TAB_INACTIVE)
                    .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(340.0);
                        ssh_section_heading(ui, "Profile");
                        profile_text_edit(ui, tab_id, "name", "Name", &mut draft.name);
                        local_executable_field(
                            ui,
                            profiles_state_id(tab_id).with("executable_autocomplete"),
                            &mut draft.executable,
                        );
                        profile_text_edit(
                            ui,
                            tab_id,
                            "arguments",
                            "Arguments (space-separated)",
                            &mut draft.arguments,
                        );
                        profile_text_edit(
                            ui,
                            tab_id,
                            "working_directory",
                            "Working directory (optional)",
                            &mut draft.working_directory,
                        );
                        if let Some(error) = &draft.error {
                            ui.add_space(6.0);
                            ui.colored_label(theme::STATUS_ERROR, error);
                        }
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                match draft.build() {
                                    Ok(profile) => {
                                        command = Some(AppCommand::SaveProfile { profile });
                                        next_mode = Some(ProfilesScreenMode::List);
                                    }
                                    Err(_) => {
                                        draft.error = Some(
                                            "Enter a name and a non-empty executable.".to_owned(),
                                        );
                                    }
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                next_mode = Some(ProfilesScreenMode::List);
                            }
                        });
                    });
            });
        }
        ProfilesScreenMode::EditSsh(draft) => {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.heading(if draft.original_id.is_some() {
                    "Edit SSH Profile"
                } else {
                    "New SSH Profile"
                });
                ui.add_space(16.0);
                egui::Frame::new()
                    .fill(theme::SURFACE_TAB_INACTIVE)
                    .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(340.0);
                        // Private-key authentication adds a tall multiline
                        // secret field that can otherwise push Save/Cancel
                        // below the window's bottom edge. Rather than
                        // wrapping the whole bordered panel in a scroll area
                        // (which either grows it to fill whatever height the
                        // surrounding layout happens to report as
                        // "available" — collapsing it to a sliver in
                        // practice — or clips the panel's own border), only
                        // the fields scroll, *inside* the panel; the panel
                        // itself keeps its natural (small-form) height and
                        // only grows a scrollbar once content would run past
                        // the actual window height. `available_height()` is
                        // unreliable here: this Frame auto-sizes to its
                        // content, so on the pass that decides how tall to
                        // make itself its own max_rect is degenerate (zero
                        // height) -- a chicken-and-egg problem for any
                        // auto-sizing container in immediate mode. Instead,
                        // measure the *absolute* remaining space in the real
                        // viewport: the current cursor's vertical position
                        // (screen coordinates, valid even when max_rect
                        // isn't) down to the bottom of the window, minus
                        // room for the Save/Cancel row below -- and, if the
                        // bottom status bar is showing, its exact reserved
                        // area (queried directly from its own persisted
                        // panel state rather than guessed) so the panel
                        // never overlaps it.
                        let panel_top = ui.cursor().top();
                        let mut viewport_bottom = ui.ctx().content_rect().bottom();
                        if let Some(status_bar) = egui::containers::panel::PanelState::load(
                            ui.ctx(),
                            egui::Id::new("status_bar"),
                        ) {
                            viewport_bottom = viewport_bottom.min(status_bar.outer_rect.top());
                        }
                        let scroll_max_height = (viewport_bottom - panel_top - 56.0).max(120.0);
                        // `ScrollArea` computes its own available space via
                        // `ui.available_rect_before_wrap()`, which is
                        // degenerate (zero height) here because the
                        // enclosing Frame hasn't settled on its own size
                        // yet -- an auto-sizing container doesn't know its
                        // height until after its content is laid out. Give
                        // the scroll area its own child `Ui` with a real
                        // (non-degenerate) max_rect reflecting the budget we
                        // just computed, so its internal sizing sees actual
                        // numbers instead of zero. Unlike `set_min_height`,
                        // this doesn't force the surrounding Frame to grow:
                        // the child `Ui`'s *allocated* size still comes from
                        // what was actually drawn, so the panel keeps
                        // shrinking to fit short content and only grows a
                        // scrollbar when content would truly overflow.
                        let scroll_rect = egui::Rect::from_min_size(
                            ui.cursor().min,
                            egui::vec2(ui.available_width(), scroll_max_height),
                        );
                        ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_rect), |ui| {
                            ScrollArea::vertical()
                            .id_salt((tab_id, "edit_ssh_profile_scroll"))
                            .max_height(scroll_max_height)
                            .show(ui, |ui| {
                                ssh_section_heading(ui, "Connection");
                                profile_text_edit(ui, tab_id, "name", "Name", &mut draft.name);
                                profile_text_edit(
                                    ui,
                                    tab_id,
                                    "username",
                                    "Username",
                                    &mut draft.username,
                                );
                                profile_text_edit(ui, tab_id, "host", "Host", &mut draft.host);
                                profile_text_edit(ui, tab_id, "port", "Port", &mut draft.port);
                                if let Some(profile_id) = draft.original_id.clone() {
                                    ui.add_space(10.0);
                                    ssh_section_heading(ui, "Authentication");
                                    ui.horizontal(|ui| {
                                        ui.radio_value(
                                            &mut draft.auth_method,
                                            SshAuthenticationMethod::Password,
                                            "Password authentication",
                                        );
                                        ui.radio_value(
                                            &mut draft.auth_method,
                                            SshAuthenticationMethod::PrivateKey,
                                            "Private-key authentication",
                                        );
                                    });
                                    ui.add_space(4.0);
                                    match draft.auth_method {
                                        SshAuthenticationMethod::Password => {
                                            ssh_paragraph(
                                                ui,
                                                if draft.has_stored_credential
                                                    && draft.stored_credential_kind
                                                        == CredentialKind::Password
                                                {
                                                    "A password is stored in native secure storage for this profile. Enter a new one below to replace it."
                                                } else {
                                                    "Enter a password to remember it in native secure storage, or leave this blank to be prompted at connect time."
                                                },
                                            );
                                            ui.add_space(4.0);
                                            profile_password_edit(
                                                ui,
                                                tab_id,
                                                "password",
                                                "Password",
                                                &mut draft.password,
                                            );
                                            ui.add_space(4.0);
                                            if ui
                                                .add_enabled(
                                                    !draft.password.is_empty(),
                                                    egui::Button::new("Save password"),
                                                )
                                                .clicked()
                                            {
                                                command = Some(AppCommand::StoreProfilePassword {
                                                    profile_id,
                                                    password: PasswordToStore::new(
                                                        std::mem::take(&mut draft.password),
                                                    ),
                                                });
                                                draft.has_stored_credential = true;
                                                draft.stored_credential_kind =
                                                    CredentialKind::Password;
                                            }
                                        }
                                        SshAuthenticationMethod::PrivateKey => {
                                            ssh_paragraph(
                                                ui,
                                                if draft.has_stored_credential
                                                    && draft.stored_credential_kind
                                                        == CredentialKind::PrivateKey
                                                {
                                                    "A private key is stored in native secure storage for this profile. Enter a new one below to replace it."
                                                } else {
                                                    "Enter an OpenSSH private key to remember it in native secure storage."
                                                },
                                            );
                                            ui.add_space(4.0);
                                            ssh_multiline_secret_text_edit(
                                                ui,
                                                tab_id,
                                                "private_key",
                                                "OpenSSH private key",
                                                &mut draft.private_key,
                                            );
                                            profile_password_edit(
                                                ui,
                                                tab_id,
                                                "key_passphrase",
                                                "Key passphrase (optional)",
                                                &mut draft.key_passphrase,
                                            );
                                            ui.add_space(4.0);
                                            if ui
                                                .add_enabled(
                                                    !draft.private_key.trim().is_empty(),
                                                    egui::Button::new("Save private key"),
                                                )
                                                .clicked()
                                            {
                                                let passphrase = if draft.key_passphrase.is_empty()
                                                {
                                                    None
                                                } else {
                                                    Some(std::mem::take(
                                                        &mut draft.key_passphrase,
                                                    ))
                                                };
                                                command =
                                                    Some(AppCommand::StoreProfilePrivateKey {
                                                        profile_id,
                                                        private_key: PrivateKeyToStore::new(
                                                            std::mem::take(
                                                                &mut draft.private_key,
                                                            ),
                                                            passphrase,
                                                        ),
                                                    });
                                                draft.has_stored_credential = true;
                                                draft.stored_credential_kind =
                                                    CredentialKind::PrivateKey;
                                            }
                                        }
                                    }
                                }
                            });
                        });
                        // Kept outside the scroll area (but still inside the
                        // bordered panel) so Save/Cancel — and any error —
                        // stay pinned and reachable without scrolling, even
                        // when the fields above are tall enough to scroll.
                        if let Some(error) = &draft.error {
                            ui.add_space(6.0);
                            ui.colored_label(theme::STATUS_ERROR, error);
                        }
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                match draft.build() {
                                    Ok(profile) => {
                                        command = Some(AppCommand::SaveProfile { profile });
                                        next_mode = Some(ProfilesScreenMode::List);
                                    }
                                    Err(()) => {
                                        draft.error = Some(
                                            "Enter a valid name, host, numeric port, and username."
                                                .to_owned(),
                                        );
                                    }
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                next_mode = Some(ProfilesScreenMode::List);
                            }
                        });
                    });
            });
        }
        ProfilesScreenMode::ConfirmDelete {
            identifier,
            references,
        } => {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.heading("Delete profile?");
                ui.label(format!("This will permanently delete \"{identifier}\"."));
                if *references > 0 {
                    ui.colored_label(
                        theme::STATUS_ERROR,
                        format!(
                            "{references} saved workspace tab{} currently launch{} from this profile and will block deletion until removed.",
                            if *references == 1 { "" } else { "s" },
                            if *references == 1 { "s" } else { "" },
                        ),
                    );
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        command = Some(AppCommand::DeleteProfile {
                            identifier: identifier.clone(),
                        });
                        next_mode = Some(ProfilesScreenMode::List);
                    }
                });
            });
        }
    }
        });
    });
    if let Some(mode) = next_mode {
        state.mode = mode;
    }

    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::AppState;
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };

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
                        "Cmd+Shift+P",
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
                        "Cmd+Shift+P",
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
        // Quick Connect is the default surface; these tests exercise the
        // full advanced form, so reveal it the same way a user would.
        harness.get_by_label("Show advanced settings").click();
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
    fn ssh_form_orders_fields_username_then_host_then_port_prefilled_with_22() {
        // Regression test pinning the requested field order (Username,
        // Host, Port) and that Port is prefilled with the actual default
        // value (not left empty with "(default: 22)"-style wording).
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);

        let username_top = harness.get_by_label("Username").rect().top();
        let host_top = harness.get_by_label("Host").rect().top();
        let port_top = harness.get_by_label("Port").rect().top();

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
            harness.query_by_label("Port (default: 22)").is_none(),
            "the old 'Port (default: 22)' wording must not be present"
        );
    }

    #[test]
    fn ssh_launcher_form_prefills_the_port_field_with_22() {
        assert_eq!(
            SshLauncherForm::default().port,
            "22",
            "the Port field must show the actual default value rather than staying empty"
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
    fn ssh_launcher_defaults_to_quick_connect_not_the_advanced_form() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host")
            .click();
        harness.run();

        assert!(
            harness.query_by_label("user@host").is_some(),
            "a freshly opened SSH launcher must show the Quick Connect field"
        );
        assert!(
            harness.query_by_label("Username").is_none(),
            "the advanced form must stay hidden until 'Show advanced settings' is checked"
        );
    }

    #[test]
    fn quick_connect_focuses_its_field_when_the_launcher_opens() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host")
            .click();
        harness.run();

        assert!(
            harness.get_by_label("user@host").is_focused(),
            "Quick Connect's field must have initial keyboard focus"
        );
    }

    #[test]
    fn quick_connect_with_no_password_opens_the_in_terminal_password_prompt() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host")
            .click();
        harness.run();

        harness.get_by_label("user@host").type_text("fes@10.1.2.3");
        harness.run();
        harness.get_by_label("Connect").click();
        harness.run();

        let Some(AppCommand::StartSshSession {
            profile,
            authentication: SshAuthentication::Interactive,
            ..
        }) = harness.state().command.as_ref()
        else {
            panic!(
                "Quick Connect with no password must start an interactive (host-key-first) SSH session"
            );
        };
        assert_eq!(profile.username(), "fes");
        assert_eq!(profile.identity().host(), "10.1.2.3");
        assert_eq!(profile.identity().port(), 22);
    }

    #[test]
    fn quick_connect_parses_an_explicit_port() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host")
            .click();
        harness.run();

        harness
            .get_by_label("user@host")
            .type_text("fes@10.1.2.3:2222");
        harness.run();
        harness.get_by_label("Connect").click();
        harness.run();

        let Some(AppCommand::StartSshSession {
            profile,
            authentication: SshAuthentication::Interactive,
            ..
        }) = harness.state().command.as_ref()
        else {
            panic!("a valid quick-connect destination must start an interactive SSH session");
        };
        assert_eq!(profile.identity().port(), 2222);
    }

    #[test]
    fn quick_connect_rejects_a_destination_with_no_at_sign() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host")
            .click();
        harness.run();

        harness.get_by_label("user@host").type_text("10.1.2.3");
        harness.run();
        harness.get_by_label("Connect").click();
        harness.run();

        assert!(
            harness.state().command.is_none(),
            "an invalid quick-connect destination must not dispatch a command"
        );
        assert!(harness
            .query_by_label("Enter a destination as user@host")
            .is_some());
    }

    #[test]
    fn checking_show_advanced_settings_reveals_the_full_form_and_focuses_username() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host")
            .click();
        harness.run();

        harness.get_by_label("Show advanced settings").click();
        harness.run();

        assert!(
            harness.query_by_label("Username").is_some(),
            "checking 'Show advanced settings' must reveal the full form"
        );
        assert!(
            harness.get_by_label("Username").is_focused(),
            "revealing the advanced form must move focus to Username"
        );
    }

    #[test]
    fn close_advanced_settings_carries_a_non_default_port_into_quick_connect() {
        let mut form = SshLauncherForm {
            username: "example".to_owned(),
            host: "169.254.1.1".to_owned(),
            port: "4096".to_owned(),
            advanced_open: true,
            ..Default::default()
        };

        form.close_advanced_settings();

        assert_eq!(form.quick_connect, "example@169.254.1.1:4096");
        assert!(!form.advanced_open);
    }

    #[test]
    fn close_advanced_settings_omits_the_default_port() {
        let mut form = SshLauncherForm {
            username: "example".to_owned(),
            host: "169.254.1.1".to_owned(),
            advanced_open: true,
            ..Default::default()
        };

        form.close_advanced_settings();

        assert_eq!(form.quick_connect, "example@169.254.1.1");
    }

    #[test]
    fn open_advanced_settings_parses_the_quick_connect_destination() {
        let mut form = SshLauncherForm {
            quick_connect: "example@169.254.1.1:4096".to_owned(),
            ..Default::default()
        };

        form.open_advanced_settings();

        assert_eq!(form.username, "example");
        assert_eq!(form.host, "169.254.1.1");
        assert_eq!(form.port, "4096");
        assert!(form.advanced_open);
    }

    #[test]
    fn open_advanced_settings_clears_stale_feedback() {
        let mut form = SshLauncherForm {
            feedback: Some("Enter a destination, e.g. user@host".to_owned()),
            ..Default::default()
        };

        form.open_advanced_settings();

        assert!(form.feedback.is_none());
    }

    #[test]
    fn close_advanced_settings_clears_stale_feedback() {
        let mut form = SshLauncherForm {
            feedback: Some("SSH host must not contain whitespace".to_owned()),
            advanced_open: true,
            ..Default::default()
        };

        form.close_advanced_settings();

        assert!(form.feedback.is_none());
    }

    #[test]
    fn toggling_advanced_settings_clears_quick_connect_feedback() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host")
            .click();
        harness.run();

        harness.get_by_label("Connect").click();
        harness.run();
        assert!(harness
            .query_by_label("Enter a destination, e.g. user@host")
            .is_some());

        harness.get_by_label("Show advanced settings").click();
        harness.run();

        assert!(
            harness
                .query_by_label("Enter a destination, e.g. user@host")
                .is_none(),
            "stale Quick Connect feedback must not survive a toggle to the advanced form"
        );
    }

    #[test]
    fn toggling_quick_connect_clears_advanced_form_feedback() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);
        enter_text(&mut harness, "Host", "invalid host");

        harness.get_by_label("Connect with password").click();
        harness.run();
        assert!(harness
            .query_by_label("SSH host must not contain whitespace")
            .is_some());

        harness.get_by_label("Show advanced settings").click();
        harness.run();

        assert!(
            harness
                .query_by_label("SSH host must not contain whitespace")
                .is_none(),
            "stale advanced-form feedback must not survive a toggle back to Quick Connect"
        );
    }

    #[test]
    fn advanced_form_with_an_empty_password_starts_an_interactive_session_instead_of_connecting() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);
        enter_text(&mut harness, "Host", "example.invalid");
        enter_text(&mut harness, "Username", "test-user");

        harness.get_by_label("Connect with password").click();
        harness.run();

        let Some(AppCommand::StartSshSession {
            profile,
            authentication: SshAuthentication::Interactive,
            ..
        }) = harness.state().command.as_ref()
        else {
            panic!(
                "submitting the advanced form with no password must start an interactive \
                 (host-key-first) session, not attempt to connect with no credential"
            );
        };
        assert_eq!(profile.username(), "test-user");
        assert_eq!(profile.identity().host(), "example.invalid");
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
    fn saved_ssh_profile_card_launches_directly_without_a_stored_credential() {
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
            .get_by_label("build — Saved SSH profile · deploy@ssh.example.test:2200")
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::StartConfiguredSshProfile { ref profile_id })
                if profile_id == "build"
        ));
    }

    #[test]
    fn saved_profiles_are_listed_after_local_shell_and_new_ssh_connection() {
        let profiles = vec![
            Profile::local("development", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let mut harness = harness_with_profiles(profiles);
        harness.run();

        let local_shell_top = harness
            .get_by_label("Local Shell — Default shell on this computer")
            .rect()
            .top();
        let ssh_top = harness
            .get_by_label("SSH — Connect to a remote host")
            .rect()
            .top();
        let profile_top = harness
            .get_by_label("development — Saved local profile")
            .rect()
            .top();

        assert!(
            local_shell_top < ssh_top && ssh_top < profile_top,
            "expected Local Shell, then New SSH Connection, then saved profiles, got tops: \
             {local_shell_top}, {ssh_top}, {profile_top}"
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
    fn saved_ssh_profile_appears_as_a_launcher_card_without_password_ui() {
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
            .query_by_label("production — Saved SSH profile · deploy@ssh.example.test:2200")
            .is_some());
        assert!(
            harness
                .query_by_label("Enter or replace password for production")
                .is_none(),
            "password entry belongs to the Profiles editor, not the Launcher"
        );
        assert!(harness.state().command.is_none());
    }

    #[test]
    fn saved_ssh_profile_card_dispatches_a_configured_launch_regardless_of_stored_credential() {
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
            .get_by_label("production — Saved SSH profile · deploy@ssh.example.test:2200")
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::StartConfiguredSshProfile { ref profile_id })
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

    fn test_ssh_password_prompt() -> PasswordPrompt {
        PasswordPrompt::new("test-user", "192.0.2.1", 1, false)
    }

    #[test]
    fn ssh_live_password_prompt_shows_the_ssh_style_prompt_line() {
        #[derive(Default)]
        struct PromptHarnessState {
            prompt: Option<PasswordPrompt>,
            command: Option<AppCommand>,
        }

        let mut harness = Harness::builder()
            .with_size(egui::vec2(420.0, 200.0))
            .build_ui_state(
                |ui, state: &mut PromptHarnessState| {
                    let prompt = state.prompt.as_ref().expect("prompt must be set");
                    if let Some(command) =
                        show_ssh_live_password_prompt(ui, AppState::for_test().active(), prompt)
                    {
                        state.command = Some(command);
                    }
                },
                PromptHarnessState {
                    prompt: Some(test_ssh_password_prompt()),
                    command: None,
                },
            );
        // Installing the bundled terminal font family binds it to the atlas
        // only after the pass boundary, so a first frame is needed before
        // the pty-styled prompt can lay out text with it.
        harness.run();
        harness.run();

        assert!(harness
            .query_by_label("test-user@192.0.2.1's password:")
            .is_some());
    }

    #[test]
    fn ssh_live_password_prompt_submits_a_typed_password_on_enter() {
        struct PromptHarnessState {
            prompt: PasswordPrompt,
            tab_id: TabId,
            command: Option<AppCommand>,
        }

        let tab_id = AppState::for_test().active();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(420.0, 200.0))
            .build_ui_state(
                |ui, state: &mut PromptHarnessState| {
                    if let Some(command) =
                        show_ssh_live_password_prompt(ui, state.tab_id, &state.prompt)
                    {
                        state.command = Some(command);
                    }
                },
                PromptHarnessState {
                    prompt: test_ssh_password_prompt(),
                    tab_id,
                    command: None,
                },
            );
        // Installing the bundled terminal font family binds it to the atlas
        // only after the pass boundary, so a first frame is needed before
        // the pty-styled prompt can lay out text with it.
        harness.run();
        harness.run();

        // Typed characters are never reflected on screen (matching real
        // `ssh`), so there is no field to type into — the prompt captures
        // raw text/key events directly, exactly like the host-key [y/N]
        // prompt does.
        for character in "typed-test-password".chars() {
            harness.event(egui::Event::Text(character.to_string()));
        }
        harness.key_press(egui::Key::Enter);
        harness.run();

        let Some(AppCommand::ResolveSshPassword { tab, password }) =
            harness.state().command.as_ref()
        else {
            panic!("submitting the live password prompt must return a resolve command");
        };
        assert_eq!(*tab, tab_id);
        assert_eq!(password, "typed-test-password");
    }

    struct ProfilesHarnessState {
        tab_id: TabId,
        configuration: festerm_config::Configuration,
        command: Option<AppCommand>,
    }

    fn profiles_harness(
        configuration: festerm_config::Configuration,
    ) -> Harness<'static, ProfilesHarnessState> {
        Harness::builder()
            .with_size(egui::vec2(560.0, 640.0))
            .build_ui_state(
                |ui, state: &mut ProfilesHarnessState| {
                    if let Some(command) =
                        show_profiles(ui, state.tab_id, &state.configuration, None)
                    {
                        state.command = Some(command);
                    }
                },
                ProfilesHarnessState {
                    tab_id: AppState::for_test().active(),
                    configuration,
                    command: None,
                },
            )
    }

    #[test]
    fn dragging_a_profile_card_onto_another_reorders_it() {
        let profiles = vec![
            Profile::local("alpha", "sh", Vec::new(), None).unwrap(),
            Profile::local("beta", "sh", Vec::new(), None).unwrap(),
            Profile::local("gamma", "sh", Vec::new(), None).unwrap(),
        ];
        let configuration = festerm_config::Configuration::new(profiles).unwrap();
        let mut harness = profiles_harness(configuration);
        harness.run();

        let from = harness.get_by_label("alpha").rect().center();
        let to = harness.get_by_label("gamma").rect().center();

        harness.drag_at(from);
        harness.run();
        let steps = 8;
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            harness.hover_at(from + (to - from) * t);
            harness.run();
        }
        harness.drop_at(to);
        harness.run();

        assert!(
            matches!(
                harness.state().command,
                Some(AppCommand::ReorderProfiles {
                    ref moved,
                    ref before,
                }) if moved == "alpha" && before.as_deref() == Some("gamma")
            ),
            "observed command: {:?}",
            harness.state().command
        );
    }

    #[test]
    fn dragging_a_profile_card_past_the_last_row_moves_it_to_the_end() {
        let profiles = vec![
            Profile::local("alpha", "sh", Vec::new(), None).unwrap(),
            Profile::local("beta", "sh", Vec::new(), None).unwrap(),
        ];
        let configuration = festerm_config::Configuration::new(profiles).unwrap();
        let mut harness = profiles_harness(configuration);
        harness.run();

        let from = harness.get_by_label("alpha").rect().center();
        let beta_rect = harness.get_by_label("beta").rect();
        let to = beta_rect.center() + egui::vec2(0.0, beta_rect.height() * 3.0);

        harness.drag_at(from);
        harness.run();
        let steps = 8;
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            harness.hover_at(from + (to - from) * t);
            harness.run();
        }
        harness.drop_at(to);
        harness.run();

        assert!(
            matches!(
                harness.state().command,
                Some(AppCommand::ReorderProfiles {
                    ref moved,
                    ref before,
                }) if moved == "alpha" && before.is_none()
            ),
            "observed command: {:?}",
            harness.state().command
        );
    }

    #[test]
    fn profiles_list_shows_no_profiles_saved_yet_when_empty() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        assert!(harness.query_by_label("No profiles saved yet.").is_some());
    }

    #[test]
    fn profiles_new_local_profile_flow_returns_a_save_profile_command() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        harness.get_by_label("New Local Profile").click();
        harness.run();

        harness.get_by_label("Name").focus();
        harness.get_by_label("Name").type_text("dev-shell");
        harness.run();
        harness.get_by_label("Executable").focus();
        harness.get_by_label("Executable").type_text("/bin/zsh");
        harness.run();

        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfile {
            profile: Profile::Local(local),
        }) = harness.state().command.as_ref()
        else {
            panic!("saving a valid local profile draft must return a SaveProfile command");
        };
        assert_eq!(local.identifier(), "dev-shell");
    }

    #[test]
    fn profiles_new_local_profile_flow_reports_an_error_for_an_empty_name() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        harness.get_by_label("New Local Profile").click();
        harness.run();

        harness.get_by_label("Save").click();
        harness.run();

        assert!(harness.state().command.is_none());
        assert!(harness
            .query_by_label("Enter a name and a non-empty executable.")
            .is_some());
    }

    #[test]
    fn local_profile_executable_field_offers_path_matches_and_selecting_one_fills_absolute_path() {
        // `cargo` must be resolvable on `PATH` for `cargo test` itself to be
        // running, so this environment always has at least one real match
        // without this test needing to mutate the process-wide `PATH`.
        let Some(expected_path) = festerm_pty::search_path_executables("cargo", 1)
            .into_iter()
            .next()
        else {
            panic!("`cargo` must be discoverable on PATH while running under `cargo test`");
        };

        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();
        harness.get_by_label("New Local Profile").click();
        harness.run();
        harness.get_by_label("Executable").focus();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.get_by_label("Executable").type_text("cargo");
        harness.run();

        let expected_label = expected_path.display().to_string();
        // `click_accesskit()` dispatches a direct accesskit click action rather
        // than a synthetic pointer press/release, which reliably lands on the
        // suggestion regardless of exact pixel geometry.
        harness
            .get_by_role_and_label(accesskit::Role::Button, &expected_label)
            .click_accesskit();
        harness.run();

        assert_eq!(
            harness.get_by_label("Executable").value().as_deref(),
            Some(expected_label.as_str()),
            "selecting a PATH suggestion must fill the field with its absolute path"
        );
        assert!(
            harness.query_by_label(&expected_label).is_none(),
            "the suggestion dropdown must be hidden immediately after a selection"
        );
    }

    #[test]
    fn profiles_delete_flow_returns_a_delete_profile_command() {
        let profile = Profile::local("dev-shell", "/bin/zsh", Vec::new(), None).unwrap();
        let mut harness =
            profiles_harness(festerm_config::Configuration::new(vec![profile]).unwrap());
        harness.run();

        harness.get_by_label("Delete").click();
        harness.run();
        assert!(harness.query_by_label("Delete profile?").is_some());

        harness.get_by_label("Delete").click();
        harness.run();

        let Some(AppCommand::DeleteProfile { identifier }) = harness.state().command.as_ref()
        else {
            panic!("confirming deletion must return a DeleteProfile command");
        };
        assert_eq!(identifier, "dev-shell");
    }

    #[test]
    fn ssh_profile_editor_panel_stays_compact_instead_of_stretching_to_fill_the_window() {
        // Regression test for a panel that, when its scroll area filled
        // "available height" reported by the surrounding layout, either
        // collapsed to a sliver or stretched to match whatever height that
        // layout reported — instead of sizing to its own (short) content.
        let profile = Profile::ssh(
            "prod",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 900.0))
            .build_ui_state(
                |ui, state: &mut ProfilesHarnessState| {
                    if let Some(command) =
                        show_profiles(ui, state.tab_id, &state.configuration, None)
                    {
                        state.command = Some(command);
                    }
                },
                ProfilesHarnessState {
                    tab_id: AppState::for_test().active(),
                    configuration: festerm_config::Configuration::new(vec![profile]).unwrap(),
                    command: None,
                },
            );
        harness.run();

        harness.get_by_label("Edit").click();
        harness.run();

        // A short connection-details-only form (password auth by default)
        // should keep "Save" well above a 900px-tall window rather than
        // stretching the panel to fill it.
        assert!(harness.get_by_label("Save").rect().max.y < 500.0);
        // With ample room, the whole form fits without needing to scroll at
        // all -- once content doesn't exceed the available height, egui's
        // default `ScrollBarVisibility::VisibleWhenNeeded` keeps the
        // scrollbar hidden (it may still exist in the accessibility tree,
        // just marked hidden).
        let scroll_bar = harness.query_by_role(accesskit::Role::ScrollBar);
        assert!(
            scroll_bar.is_none_or(|node| node.accesskit_node().is_hidden()),
            "scroll bar should not be visible when the form fits comfortably"
        );
    }

    #[test]
    fn ssh_profile_editor_panel_does_not_overlap_a_visible_bottom_status_bar() {
        // Regression test: the editor's height budget must account for the
        // app's bottom status bar (reserved via `egui::Panel::bottom`), not
        // just the raw window height, or the panel ends up sized as if that
        // strip weren't there and visually runs into/under it.
        let profile = Profile::ssh(
            "prod",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 350.0))
            .build_ui_state(
                |ui, state: &mut ProfilesHarnessState| {
                    egui::Panel::bottom("status_bar")
                        .resizable(false)
                        .show_separator_line(false)
                        .show(ui, |ui| {
                            ui.set_min_height(24.0);
                            ui.set_max_height(24.0);
                        });
                    if let Some(command) =
                        show_profiles(ui, state.tab_id, &state.configuration, None)
                    {
                        state.command = Some(command);
                    }
                },
                ProfilesHarnessState {
                    tab_id: AppState::for_test().active(),
                    configuration: festerm_config::Configuration::new(vec![profile]).unwrap(),
                    command: None,
                },
            );
        harness.run();

        harness.get_by_label("Edit").click();
        harness.run();

        let status_bar_top =
            egui::containers::panel::PanelState::load(&harness.ctx, egui::Id::new("status_bar"))
                .expect("status bar panel state should be recorded")
                .outer_rect
                .top();
        assert!(
            harness.get_by_label("Save").rect().max.y < status_bar_top,
            "the editor panel must stay above the status bar rather than overlapping it"
        );
    }

    #[test]
    fn ssh_profile_editor_offers_a_password_field_that_dispatches_store_profile_password() {
        let profile = Profile::ssh(
            "prod",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap();
        let mut harness =
            profiles_harness(festerm_config::Configuration::new(vec![profile]).unwrap());
        harness.run();

        harness.get_by_label("Edit").click();
        harness.run();
        assert!(harness.query_by_label("Edit SSH Profile").is_some());

        // No stored credential yet, so the field starts empty and "Save
        // password" is disabled until something is typed.
        assert!(harness
            .query_by_label(
                "Enter a password to remember it in native secure storage, or leave this blank to be prompted at connect time."
            )
            .is_some());

        harness.get_by_label("Password").focus();
        harness.get_by_label("Password").type_text("hunter2");
        harness.run();

        // The password-authentication panel is taller than the harness
        // viewport (matching the private-key panel that motivated wrapping
        // this editor in a `ScrollArea`), so "Save password" starts
        // scrolled out of view; scroll it into view before clicking, same
        // as a real user would.
        harness.get_by_label("Save password").scroll_to_me();
        harness.run();
        harness.get_by_label("Save password").click();
        harness.run();

        let Some(AppCommand::StoreProfilePassword { profile_id, .. }) =
            harness.state().command.as_ref()
        else {
            panic!("clicking Save password must return a StoreProfilePassword command");
        };
        assert_eq!(profile_id, "prod");
    }

    #[test]
    fn ssh_profile_editor_offers_a_private_key_field_that_dispatches_store_profile_private_key() {
        let profile = Profile::ssh(
            "prod",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap();
        let mut harness =
            profiles_harness(festerm_config::Configuration::new(vec![profile]).unwrap());
        harness.run();

        harness.get_by_label("Edit").click();
        harness.run();
        assert!(harness.query_by_label("Edit SSH Profile").is_some());

        harness
            .get_by_label("Private-key authentication")
            .scroll_to_me();
        harness.run();
        harness.get_by_label("Private-key authentication").click();
        harness.run();
        assert!(harness
            .query_by_label("Enter an OpenSSH private key to remember it in native secure storage.")
            .is_some());
        // Switching methods must not surface the password-authentication
        // fields at the same time.
        assert!(harness.query_by_label("Password").is_none());

        harness.get_by_label("OpenSSH private key").focus();
        harness.get_by_label("OpenSSH private key").type_text(
            "-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n-----END OPENSSH PRIVATE KEY-----",
        );
        harness.run();

        // As above: the private-key panel is taller than the harness
        // viewport, so "Save private key" starts scrolled out of view.
        harness.get_by_label("Save private key").scroll_to_me();
        harness.run();
        harness.get_by_label("Save private key").click();
        harness.run();

        let Some(AppCommand::StoreProfilePrivateKey { profile_id, .. }) =
            harness.state().command.as_ref()
        else {
            panic!("clicking Save private key must return a StoreProfilePrivateKey command");
        };
        assert_eq!(profile_id, "prod");
    }
}
