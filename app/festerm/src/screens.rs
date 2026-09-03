//! Launcher and Settings application-surface presentation.
//!
//! These are thin, product-specific screens rather than terminal chrome
//! (`crates/festerm-ui-egui/src/chrome.rs` owns the chip row). They translate
//! user gestures into `AppCommand`s per `docs/application-command-model.md`
//! and own no session or tab policy themselves; `AppState::dispatch` remains
//! the single command-handling path.

use std::path::PathBuf;

use eframe::egui::{self, vec2, ScrollArea, Sense, Stroke, TextEdit, Ui, WidgetInfo, WidgetType};
use festerm_config::{
    CredentialKind, EmojiPresentationPreference, PersistenceConfiguration, PersistenceProviderKind,
    Profile, ScrollSpeedPreference, ScrollbackLimitPreference, SftpPaneOrderPreference,
    SshPortForwardDirection, SshProfileConfiguration, TerminalFontPreference,
};
use festerm_session::{PasswordPrompt, TerminalSize};
use festerm_ssh::{
    HostIdentity, ReconnectPolicy, RecoveryPolicy, SessionStrategy, SshAuthentication,
    SshConnectionProfile, SshKeyPassphrase, SshPrivateKey, SshPrivateKeyError, SshSessionOptions,
};
use festerm_ui_egui::{chrome::ChipLayout, icon, icon::Icon, theme};

#[cfg(test)]
use festerm_config::SshPortForwardConfiguration;

use crate::port_forward_draft::PortForwardDraft as SshPortForwardDraft;
use crate::tabs::{AppCommand, PasswordToStore, PrivateKeyToStore, TabId};

/// One selectable launch option in the Launcher list: the fixed default
/// local shell, or a saved local/SSH profile.
enum LauncherItemKind<'a> {
    LocalDefault,
    NewSsh,
    NewSftp,
    NewSerial,
    LocalProfile(&'a str),
    SshProfile(&'a str),
    SftpProfile(&'a str),
    SerialProfile(&'a str),
    ResumeSession(&'a str),
}

struct LauncherItem<'a> {
    label: String,
    description: String,
    kind: LauncherItemKind<'a>,
}

impl LauncherItem<'_> {
    fn profile_id(&self) -> Option<&str> {
        match self.kind {
            LauncherItemKind::LocalDefault
            | LauncherItemKind::NewSsh
            | LauncherItemKind::NewSftp
            | LauncherItemKind::NewSerial
            | LauncherItemKind::ResumeSession(_) => None,
            LauncherItemKind::LocalProfile(id)
            | LauncherItemKind::SshProfile(id)
            | LauncherItemKind::SftpProfile(id)
            | LauncherItemKind::SerialProfile(id) => Some(id),
        }
    }

    fn remote(&self) -> bool {
        matches!(
            self.kind,
            LauncherItemKind::NewSsh
                | LauncherItemKind::NewSftp
                | LauncherItemKind::SshProfile(_)
                | LauncherItemKind::SftpProfile(_)
        )
    }

    fn command(&self) -> AppCommand {
        match self.kind {
            LauncherItemKind::LocalDefault => AppCommand::StartLocalSession,
            LauncherItemKind::NewSsh => {
                unreachable!("the New SSH Connection item opens the SSH form, not an AppCommand")
            }
            LauncherItemKind::NewSftp => {
                unreachable!("the New SFTP Connection item opens the SFTP form, not an AppCommand")
            }
            LauncherItemKind::NewSerial => {
                unreachable!(
                    "the New Serial Connection item opens the serial form, not an AppCommand"
                )
            }
            LauncherItemKind::LocalProfile(profile_id) => AppCommand::StartConfiguredLocalProfile {
                profile_id: profile_id.to_owned(),
            },
            LauncherItemKind::SshProfile(profile_id) => AppCommand::StartConfiguredSshProfile {
                profile_id: profile_id.to_owned(),
            },
            LauncherItemKind::SftpProfile(profile_id) => AppCommand::StartConfiguredSftpProfile {
                profile_id: profile_id.to_owned(),
            },
            LauncherItemKind::SerialProfile(profile_id) => {
                AppCommand::StartConfiguredSerialProfile {
                    profile_id: profile_id.to_owned(),
                }
            }
            LauncherItemKind::ResumeSession(name) => AppCommand::ResumeUnattachedSession {
                name: name.to_owned(),
            },
        }
    }
}

/// Applies one Launcher item's click/edit-icon response to its shared
/// mutable state: opens the profile editor for an edit-icon click, opens
/// the SSH/Serial connection forms for those fixed entries, or dispatches
/// the item's launch command otherwise. Shared by both the single-column
/// and multi-column (feature request #64) rendering paths so their click
/// handling can never drift apart.
fn handle_launcher_item_response(
    item: &LauncherItem<'_>,
    response: egui::Response,
    edit_response: Option<egui::Response>,
    state: &mut LauncherState,
    command: &mut Option<AppCommand>,
) {
    if edit_response.is_some_and(|edit| edit.clicked()) {
        *command = Some(AppCommand::OpenProfileEditor {
            identifier: item
                .profile_id()
                .expect("editable launcher items always carry a profile id")
                .to_owned(),
        });
    } else if response.clicked() {
        if matches!(item.kind, LauncherItemKind::NewSsh) {
            state.ssh_open = true;
            state.ssh.focus_username = true;
        } else if matches!(item.kind, LauncherItemKind::NewSftp) {
            state.sftp_open = true;
            state.sftp.focus_username = true;
        } else if matches!(item.kind, LauncherItemKind::NewSerial) {
            state.serial_open = true;
        } else {
            *command = Some(item.command());
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
    fixed_width: Option<f32>,
) -> (egui::Response, Option<egui::Response>) {
    let width = fixed_width.unwrap_or_else(|| ui.available_width().clamp(220.0, 420.0));
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
            WidgetInfo::labeled(
                WidgetType::Button,
                true,
                format!("Edit {primary} ({secondary})"),
            )
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

#[derive(Clone)]
struct DurableSessionDraft {
    enabled: bool,
    provider: PersistenceProviderKind,
    session_name: String,
    /// Set once the user manually edits the session name field directly.
    /// Until then, the session name auto-fills from the profile name as
    /// the user types it, so most profiles never need a separate manual
    /// entry.
    session_name_touched: bool,
    automatic_recovery: bool,
}

impl Default for DurableSessionDraft {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: PersistenceProviderKind::Tmux,
            session_name: "main".to_owned(),
            session_name_touched: false,
            automatic_recovery: false,
        }
    }
}

impl DurableSessionDraft {
    fn from_persistence(persistence: Option<&PersistenceConfiguration>) -> Self {
        match persistence {
            Some(persistence) => Self {
                enabled: true,
                provider: persistence.provider(),
                session_name: persistence.session_name().to_owned(),
                // An existing profile's session name was explicitly chosen
                // (by the user or a prior save), so don't let subsequent
                // profile-name edits silently overwrite it.
                session_name_touched: true,
                automatic_recovery: false,
            },
            None => Self::default(),
        }
    }

    /// Auto-fills the session name from the profile name as the user types
    /// it, unless the session name has already been manually edited.
    ///
    /// Called whenever the profile-name field changes; sanitizes the
    /// profile name to the character set [`PersistentSessionName`] accepts
    /// (lowercase ASCII alphanumerics, `-`, `_`, `.`).
    fn sync_session_name_from_profile_name(&mut self, profile_name: &str) {
        if self.session_name_touched {
            return;
        }
        self.session_name = sanitize_session_name_from_profile_name(profile_name);
    }

    fn persistence(&self) -> Result<Option<PersistenceConfiguration>, String> {
        if !self.enabled {
            return Ok(None);
        }
        let persistence = PersistenceConfiguration::new(self.provider, self.session_name.trim());
        persistence
            .validate_session_name()
            .map_err(|error| error.to_string())?;
        Ok(Some(persistence))
    }

    fn session_options(&self) -> Result<SshSessionOptions, String> {
        let strategy = self
            .persistence()?
            .as_ref()
            .map(PersistenceConfiguration::to_session_strategy)
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or(SessionStrategy::PlainShell);
        if self.enabled && self.automatic_recovery {
            let recovery = RecoveryPolicy::Automatic(ReconnectPolicy::default_automatic());
            return SshSessionOptions::with_recovery_policy(strategy, recovery)
                .map_err(|error| error.to_string());
        }
        Ok(SshSessionOptions::manual_recovery(strategy))
    }
}

/// Sanitizes a profile name into a candidate durable-session name:
/// lowercased, with any character outside
/// [`PersistentSessionName`]'s accepted set (ASCII alphanumerics, `-`,
/// `_`, `.`) collapsed to a single `-`, leading/trailing `-` trimmed, and
/// truncated to the name's maximum length.
fn sanitize_session_name_from_profile_name(profile_name: &str) -> String {
    const MAXIMUM_BYTES: usize = 64;
    let mut sanitized = String::with_capacity(profile_name.len());
    let mut last_was_separator = false;
    for character in profile_name.trim().chars() {
        let lowered = character.to_ascii_lowercase();
        if lowered.is_ascii_alphanumeric() || matches!(lowered, '-' | '_' | '.') {
            sanitized.push(lowered);
            last_was_separator = false;
        } else if !last_was_separator && !sanitized.is_empty() {
            sanitized.push('-');
            last_was_separator = true;
        }
    }
    while sanitized.ends_with('-') {
        sanitized.pop();
    }
    sanitized.truncate(MAXIMUM_BYTES);
    while !sanitized.is_char_boundary(sanitized.len()) {
        sanitized.pop();
    }
    sanitized
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
    durable_session: DurableSessionDraft,
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
            durable_session: DurableSessionDraft::default(),
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
    /// `self.durable_session.automatic_recovery` is explicitly set.
    fn session_options(&self) -> Result<SshSessionOptions, String> {
        self.durable_session.session_options()
    }

    fn prefill_from_profile(&mut self, profile: &SshProfileConfiguration) {
        self.host = profile.host().to_owned();
        self.port = profile.port().to_string();
        self.username = profile.username().to_owned();
        self.durable_session = DurableSessionDraft::from_persistence(profile.persistence());
        self.advanced_open = true;
    }

    fn prefill_saved_profile(&mut self, profile: &SshProfileConfiguration) {
        self.prefill_from_profile(profile);
        self.saved_profile_id = Some(profile.identifier().to_owned());
        self.saved_profile_has_credential = profile.credential_reference().is_some();
    }

    fn connection_profile(&self) -> Result<SshConnectionProfile, String> {
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
        SshConnectionProfile::new(
            identity,
            self.username.clone(),
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            initial_size,
        )
        .map_err(|error| error.to_string())
    }

    /// Converts the transient form into the application's typed SSH command.
    ///
    /// Taking every secret first ensures each submit attempt removes it from UI
    /// state, including attempts rejected by non-secret input validation.
    fn submit(&mut self) -> Result<AppCommand, String> {
        let password = std::mem::take(&mut self.password);
        let private_key = std::mem::take(&mut self.private_key);
        let key_passphrase = std::mem::take(&mut self.key_passphrase);
        let profile = self.connection_profile()?;
        let options = self.session_options()?;

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
                    options,
                })
            }
            SshAuthenticationMethod::Password if password.is_empty() => {
                Ok(AppCommand::StartSshSession {
                    profile,
                    authentication: SshAuthentication::interactive(),
                    options,
                })
            }
            SshAuthenticationMethod::Password => Ok(AppCommand::StartSshSession {
                profile,
                authentication: SshAuthentication::password(password),
                options,
            }),
            SshAuthenticationMethod::PrivateKey => Ok(AppCommand::StartSshSession {
                profile,
                authentication: Self::parse_private_key(private_key, key_passphrase)?,
                options,
            }),
        }
    }

    /// Converts the transient form into the application's typed SFTP command.
    fn submit_sftp(&mut self) -> Result<AppCommand, String> {
        let password = std::mem::take(&mut self.password);
        let private_key = std::mem::take(&mut self.private_key);
        let key_passphrase = std::mem::take(&mut self.key_passphrase);
        let profile = self.connection_profile()?;

        match self.authentication_method {
            SshAuthenticationMethod::Password
                if self.remember_password
                    && self.saved_profile_id.is_some()
                    && !password.is_empty() =>
            {
                Ok(AppCommand::StoreSftpPassword {
                    profile_id: self
                        .saved_profile_id
                        .clone()
                        .expect("saved profile was checked above"),
                    password: PasswordToStore::new(password),
                })
            }
            SshAuthenticationMethod::Password if password.is_empty() => {
                Ok(AppCommand::StartSftpSession {
                    profile,
                    authentication: SshAuthentication::interactive(),
                })
            }
            SshAuthenticationMethod::Password => Ok(AppCommand::StartSftpSession {
                profile,
                authentication: SshAuthentication::password(password),
            }),
            SshAuthenticationMethod::PrivateKey => Ok(AppCommand::StartSftpSession {
                profile,
                authentication: Self::parse_private_key(private_key, key_passphrase)?,
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

    fn submit_quick_connect_sftp(&mut self) -> Result<AppCommand, String> {
        self.parse_quick_connect()?;
        self.password.clear();
        self.authentication_method = SshAuthenticationMethod::Password;
        self.submit_sftp()
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

#[derive(Clone)]
struct SerialLauncherForm {
    device: String,
    baud_rate: String,
    data_bits: festerm_config::SerialDataBits,
    parity: festerm_config::SerialParity,
    stop_bits: festerm_config::SerialStopBits,
    flow_control: festerm_config::SerialFlowControl,
    discovered_ports: Vec<festerm_serial::DiscoveredPort>,
    feedback: Option<String>,
}

impl Default for SerialLauncherForm {
    fn default() -> Self {
        let discovered_ports = festerm_serial::discover_ports().unwrap_or_default();
        Self {
            device: String::new(),
            baud_rate: "115200".to_owned(),
            data_bits: festerm_config::SerialDataBits::Eight,
            parity: festerm_config::SerialParity::None,
            stop_bits: festerm_config::SerialStopBits::One,
            flow_control: festerm_config::SerialFlowControl::None,
            discovered_ports,
            feedback: None,
        }
    }
}

#[derive(Clone, Default)]
struct LauncherState {
    selected: usize,
    ssh_open: bool,
    ssh: SshLauncherForm,
    ssh_profile_prefilled: bool,
    sftp_open: bool,
    sftp: SshLauncherForm,
    sftp_profile_prefilled: bool,
    serial_open: bool,
    serial: SerialLauncherForm,
}

fn launcher_state_id(tab_id: TabId) -> egui::Id {
    egui::Id::new(("launcher_state", tab_id))
}

fn ssh_field_id(ui: &Ui, tab_id: TabId, field: &'static str) -> egui::Id {
    ui.make_persistent_id(("launcher_ssh", tab_id, field))
}

const CONTENT_SCROLLBAR_LANE: f32 = 26.0;

fn content_viewport_bottom(ui: &Ui) -> f32 {
    let mut bottom = ui.ctx().content_rect().bottom();
    if let Some(status_bar) =
        egui::containers::panel::PanelState::load(ui.ctx(), egui::Id::new("status_bar"))
    {
        bottom = bottom.min(status_bar.outer_rect.top());
    }
    bottom
}

fn configure_content_scrollbar(ui: &mut Ui) {
    let mut scroll_style = egui::style::ScrollStyle::floating();
    scroll_style.active_handle_opacity = 0.0;
    scroll_style.active_background_opacity = 0.0;
    ui.spacing_mut().scroll = scroll_style;
}

fn show_bounded_content_scroll<R>(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    body: impl FnOnce(&mut Ui) -> R,
) -> R {
    let top = ui.cursor().top();
    let height = (content_viewport_bottom(ui) - top).max(0.0);
    let scroll_rect =
        egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), height));
    ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_rect), |ui| {
        configure_content_scrollbar(ui);
        ScrollArea::vertical()
            .id_salt(id)
            .max_height(height)
            .show(ui, |ui| {
                ui.set_max_width((ui.available_width() - CONTENT_SCROLLBAR_LANE).max(0.0));
                body(ui)
            })
            .inner
    })
    .inner
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
    show_durable_session_controls(
        ui,
        tab_id,
        &mut form.durable_session,
        DurableSessionTarget::Remote,
        true,
    );
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

fn show_sftp_quick_connect(
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
                    .id_salt(("launcher_sftp", tab_id, "quick_connect"))
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
        match form.submit_quick_connect_sftp() {
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

#[derive(Clone, Copy)]
enum DurableSessionTarget {
    Local,
    Remote,
}

fn show_durable_session_controls(
    ui: &mut Ui,
    tab_id: TabId,
    draft: &mut DurableSessionDraft,
    target: DurableSessionTarget,
    show_automatic_recovery: bool,
) {
    let (heading, toggle_label, description) = match target {
        DurableSessionTarget::Local => (
            "Durable local session",
            "Use a durable local session",
            "Keep the named shell in fesTerm's local daemon, or use tmux/GNU screen.",
        ),
        DurableSessionTarget::Remote => (
            "Durable remote session",
            "Use a durable remote session",
            "Attach to the named remote tmux or screen session, creating it when needed.",
        ),
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(heading).color(theme::TEXT_PRIMARY));
        if toggle_switch(ui, draft.enabled, toggle_label).clicked() {
            draft.enabled = !draft.enabled;
            if draft.enabled && matches!(target, DurableSessionTarget::Local) {
                draft.provider = PersistenceProviderKind::FestermSessiond;
            }
        }
    });
    ssh_paragraph(ui, description);
    if !draft.enabled {
        return;
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if matches!(target, DurableSessionTarget::Local) {
            ui.radio_value(
                &mut draft.provider,
                PersistenceProviderKind::FestermSessiond,
                "fesTerm native",
            );
        }
        ui.radio_value(&mut draft.provider, PersistenceProviderKind::Tmux, "tmux");
        ui.radio_value(
            &mut draft.provider,
            PersistenceProviderKind::Screen,
            "GNU screen",
        );
    });
    if ssh_text_edit(
        ui,
        tab_id,
        "durable_session_name",
        "Session name",
        &mut draft.session_name,
        false,
        false,
    )
    .changed()
    {
        draft.session_name_touched = true;
    }
    ssh_paragraph(
        ui,
        "Use only letters, digits, hyphens, underscores, or periods.",
    );

    if show_automatic_recovery {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Recover after connection loss").color(theme::TEXT_PRIMARY),
            );
            if toggle_switch(
                ui,
                draft.automatic_recovery,
                "Automatically resume after connection loss",
            )
            .clicked()
            {
                draft.automatic_recovery = !draft.automatic_recovery;
            }
        });
    }
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

            ui.add_space(10.0);
            ssh_section_heading(ui, "Durable session");
            show_durable_session_controls(
                ui,
                tab_id,
                &mut form.durable_session,
                DurableSessionTarget::Remote,
                true,
            );

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

fn show_sftp_form(
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
                result = show_sftp_quick_connect(ui, tab_id, form, focus_username);
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
                                AppCommand::StartStoredPasswordSftpProfile {
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
                    match form.submit_sftp() {
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
    compact_launcher_grid: bool,
    resumable_sessions: &[festerm_sessiond::UnattachedSession],
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
        LauncherItem {
            label: "SFTP".to_owned(),
            description: "Transfer files over SSH".to_owned(),
            kind: LauncherItemKind::NewSftp,
        },
        LauncherItem {
            label: "Serial".to_owned(),
            description: "Open a local serial device".to_owned(),
            kind: LauncherItemKind::NewSerial,
        },
    ];
    // Resumable, unattached `festerm-sessiond` sessions (feature request
    // #70) are listed next, so a one-click "Resume" is available before the
    // saved-profile list, but still after the fixed "new session" entries.
    items.extend(resumable_sessions.iter().map(|session| {
        LauncherItem {
            label: format!("Resume: {}", session.name),
            description: session
                .working_directory
                .as_deref()
                .map(|directory| format!("{} · {directory}", session.shell))
                .unwrap_or_else(|| session.shell.clone()),
            kind: LauncherItemKind::ResumeSession(&session.name),
        }
    }));
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
    items.extend(
        profiles
            .iter()
            .filter_map(Profile::as_ssh)
            .map(|profile| LauncherItem {
                label: profile.identifier().to_owned(),
                description: format!(
                    "Saved SSH profile · SFTP · {}@{}:{}",
                    profile.username(),
                    profile.host(),
                    profile.port()
                ),
                kind: LauncherItemKind::SftpProfile(profile.identifier()),
            }),
    );
    items.extend(
        profiles
            .iter()
            .filter_map(Profile::as_serial)
            .map(|profile| LauncherItem {
                label: profile.identifier().to_owned(),
                description: format!(
                    "Saved serial profile · {} · {} baud",
                    profile.device(),
                    profile.baud_rate()
                ),
                kind: LauncherItemKind::SerialProfile(profile.identifier()),
            }),
    );

    let state_id = launcher_state_id(tab_id);
    let mut state = ui.data(|data| data.get_temp::<LauncherState>(state_id).unwrap_or_default());
    state.selected = state.selected.min(items.len().saturating_sub(1));

    if state.ssh_open {
        let mut command = None;
        let mut back_clicked = false;
        show_bounded_content_scroll(ui, (tab_id, "ssh_connection_surface"), |ui| {
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
                            command =
                                show_ssh_form(ui, tab_id, &mut state.ssh, native_store_available);
                        }
                    });
                });
            });
        });
        if back_clicked {
            state.ssh_open = false;
        }
        ui.data_mut(|data| data.insert_temp(state_id, state));
        return command;
    }

    if state.sftp_open {
        let mut command = None;
        let mut back_clicked = false;
        show_bounded_content_scroll(ui, (tab_id, "sftp_connection_surface"), |ui| {
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
                            egui::RichText::new("Connect with SFTP")
                                .size(24.0)
                                .color(theme::TEXT_PRIMARY),
                        );
                        ui.label(
                            egui::RichText::new("Enter connection details.")
                                .size(11.0)
                                .color(theme::TEXT_SECONDARY),
                        );
                        if !back_clicked {
                            command =
                                show_sftp_form(ui, tab_id, &mut state.sftp, native_store_available);
                        }
                    });
                });
            });
        });
        if back_clicked {
            state.sftp_open = false;
        }
        ui.data_mut(|data| data.insert_temp(state_id, state));
        return command;
    }

    if state.serial_open {
        let mut command = None;
        let mut back_clicked = false;
        show_bounded_content_scroll(ui, (tab_id, "serial_connection_surface"), |ui| {
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
                            egui::RichText::new("Open Serial Port")
                                .size(24.0)
                                .color(theme::TEXT_PRIMARY),
                        );
                        ui.label(
                            egui::RichText::new("Select a device and configure line settings.")
                                .size(11.0)
                                .color(theme::TEXT_SECONDARY),
                        );
                        ui.add_space(16.0);

                        // Device field with discovered-port picker
                        ui.label("Device");
                        ui.text_edit_singleline(&mut state.serial.device);
                        if !state.serial.discovered_ports.is_empty() {
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new("Available ports:")
                                    .size(11.0)
                                    .color(theme::TEXT_SECONDARY),
                            );
                            for port in &state.serial.discovered_ports {
                                let port_label = match port.description() {
                                    Some(desc) => {
                                        format!("{} — {}", port.identifier(), desc)
                                    }
                                    None => port.identifier().to_owned(),
                                };
                                if ui
                                    .selectable_label(
                                        state.serial.device == port.identifier(),
                                        &port_label,
                                    )
                                    .clicked()
                                {
                                    state.serial.device = port.identifier().to_owned();
                                }
                            }
                        }
                        if ui.button("Refresh").clicked() {
                            state.serial.discovered_ports =
                                festerm_serial::discover_ports().unwrap_or_default();
                        }

                        ui.add_space(8.0);
                        ui.label("Baud rate");
                        ui.text_edit_singleline(&mut state.serial.baud_rate);

                        ui.add_space(8.0);
                        serial_enum_combo(ui, "Data bits", &mut state.serial.data_bits);
                        serial_enum_combo(ui, "Parity", &mut state.serial.parity);
                        serial_enum_combo(ui, "Stop bits", &mut state.serial.stop_bits);
                        serial_enum_combo(ui, "Flow control", &mut state.serial.flow_control);

                        if let Some(feedback) = &state.serial.feedback {
                            ui.add_space(8.0);
                            ui.colored_label(theme::STATUS_ERROR, feedback.as_str());
                        }

                        ui.add_space(12.0);
                        if ui.button("Open").clicked() && !back_clicked {
                            let device = state.serial.device.trim();
                            let baud: Result<u32, _> = state.serial.baud_rate.trim().parse();
                            match baud {
                                Ok(baud) if baud > 0 && !device.is_empty() => {
                                    match festerm_serial::LineSettings::new(
                                        device,
                                        baud,
                                        state.serial.data_bits.into(),
                                        state.serial.parity.into(),
                                        state.serial.stop_bits.into(),
                                        state.serial.flow_control.into(),
                                    ) {
                                        Ok(settings) => {
                                            command =
                                                Some(AppCommand::StartSerialSession { settings });
                                        }
                                        Err(error) => {
                                            state.serial.feedback = Some(error.to_string());
                                        }
                                    }
                                }
                                Ok(_) => {
                                    state.serial.feedback =
                                        Some("Device and baud rate are required".to_owned());
                                }
                                Err(_) => {
                                    state.serial.feedback =
                                        Some("Baud rate must be a positive number".to_owned());
                                }
                            }
                        }
                    });
                });
            });
        });
        if back_clicked {
            state.serial_open = false;
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            state.serial_open = false;
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
        // Bound the item list's height to the room actually left above the
        // status bar and scroll instead of letting it silently run past the
        // window edge when there are enough saved profiles to overflow: an
        // unbounded list previously had entries -- and their edit icons --
        // clipped by the window/viewport boundary instead of scrolling into
        // view. Uses the same status-bar-aware `scope_builder` + `ScrollArea`
        // technique as the Settings and SSH profile editor panels.
        let panel_top = ui.cursor().top();
        let mut viewport_bottom = ui.ctx().content_rect().bottom();
        if let Some(status_bar) =
            egui::containers::panel::PanelState::load(ui.ctx(), egui::Id::new("status_bar"))
        {
            viewport_bottom = viewport_bottom.min(status_bar.outer_rect.top());
        }
        let available_height = (viewport_bottom - panel_top).max(0.0);
        let scroll_rect = egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2(ui.available_width(), available_height),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_rect), |ui| {
            configure_content_scrollbar(ui);
            egui::ScrollArea::vertical()
                .max_height(available_height)
                .show(ui, |ui| {
                    ui.set_max_width((ui.available_width() - CONTENT_SCROLLBAR_LANE).max(0.0));
                    ui.horizontal(|ui| {
                        ui.add_space(26.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            // Fixed "new session" entries (Local Shell, SSH,
                            // Serial) always render single-column, one per
                            // row, regardless of the compact-grid preference
                            // (feature request #64): the grid only applies
                            // to saved profiles, which is what tends to grow
                            // long enough to need it.
                            for (index, item) in items[..profiles_start].iter().enumerate() {
                                let (response, edit_response) = show_launcher_choice(
                                    ui,
                                    &item.label,
                                    &item.description,
                                    index == state.selected,
                                    item.remote(),
                                    item.profile_id().is_some(),
                                    None,
                                );
                                handle_launcher_item_response(
                                    item,
                                    response,
                                    edit_response,
                                    &mut state,
                                    &mut command,
                                );
                                ui.add_space(12.0);
                            }

                            let profile_items = &items[profiles_start..];
                            if !profile_items.is_empty() {
                                ui.add_space(8.0);
                                // Matches the 26px left inset applied above so
                                // the divider reads as evenly padded on both
                                // sides instead of running flush to the
                                // pane's right edge.
                                let separator_width = (ui.available_width() - 26.0).max(0.0);
                                ui.scope(|ui| {
                                    ui.set_width(separator_width);
                                    ui.separator();
                                });
                                ui.add_space(8.0);
                            }

                            // Feature request #64: when enabled and the
                            // window is wide enough for more than one
                            // column, saved profiles lay out in a
                            // responsive grid instead of a single vertical
                            // list, reducing scrolling for users with many
                            // saved profiles. Falls back to the original
                            // single-column list at narrow widths or when
                            // the preference is off.
                            const LAUNCHER_CARD_WIDTH: f32 = 260.0;
                            const LAUNCHER_CARD_SPACING: f32 = 12.0;
                            let columns = if compact_launcher_grid {
                                let available = ui.available_width();
                                (((available + LAUNCHER_CARD_SPACING)
                                    / (LAUNCHER_CARD_WIDTH + LAUNCHER_CARD_SPACING))
                                    .floor() as usize)
                                    .max(1)
                            } else {
                                1
                            };

                            if columns <= 1 {
                                for (offset, item) in profile_items.iter().enumerate() {
                                    let index = profiles_start + offset;
                                    let (response, edit_response) = show_launcher_choice(
                                        ui,
                                        &item.label,
                                        &item.description,
                                        index == state.selected,
                                        item.remote(),
                                        item.profile_id().is_some(),
                                        None,
                                    );
                                    handle_launcher_item_response(
                                        item,
                                        response,
                                        edit_response,
                                        &mut state,
                                        &mut command,
                                    );
                                    ui.add_space(12.0);
                                }
                            } else {
                                ui.spacing_mut().item_spacing.x = LAUNCHER_CARD_SPACING;
                                for (row_index, row) in profile_items.chunks(columns).enumerate() {
                                    ui.horizontal(|ui| {
                                        for (column, item) in row.iter().enumerate() {
                                            let index =
                                                profiles_start + row_index * columns + column;
                                            let (response, edit_response) = show_launcher_choice(
                                                ui,
                                                &item.label,
                                                &item.description,
                                                index == state.selected,
                                                item.remote(),
                                                item.profile_id().is_some(),
                                                Some(LAUNCHER_CARD_WIDTH),
                                            );
                                            handle_launcher_item_response(
                                                item,
                                                response,
                                                edit_response,
                                                &mut state,
                                                &mut command,
                                            );
                                        }
                                    });
                                    ui.add_space(12.0);
                                }
                            }
                        });
                    });
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
        } else if matches!(items[state.selected].kind, LauncherItemKind::NewSftp) {
            state.sftp_open = true;
            state.sftp.focus_username = true;
        } else if matches!(items[state.selected].kind, LauncherItemKind::NewSerial) {
            state.serial_open = true;
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

/// Renders a restored SFTP workspace tab without creating a transport.
pub fn show_sftp_authentication_required(
    ui: &mut Ui,
    tab_id: TabId,
    profile: &SshProfileConfiguration,
    native_store_available: bool,
) -> Option<AppCommand> {
    let state_id = launcher_state_id(tab_id);
    let mut state = ui.data(|data| data.get_temp::<LauncherState>(state_id).unwrap_or_default());
    if !state.sftp_profile_prefilled {
        state.sftp.prefill_saved_profile(profile);
        state.sftp_profile_prefilled = true;
    }

    let command = ui
        .vertical(|ui| {
            ui.add_space(24.0);
            ui.heading("SFTP authentication required");
            ui.label(format!(
                "Restored SFTP destination: {}@{}:{}",
                profile.username(),
                profile.host(),
                profile.port()
            ));
            ui.label(
                "This workspace restored destination metadata only. Enter fresh authentication \
                 below to connect; no prior connection, credential, or host trust was restored.",
            );
            show_sftp_form(ui, tab_id, &mut state.sftp, native_store_available)
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
/// `chip_layout`, `status_bar_visible`, and `show_session_details` reflect
/// the current interface preferences (`docs/gui-design.md` "Wrapping must
/// remain user-configurable"). Unlike profiles/workspace metadata, these
/// preferences are saved automatically by the composition root as soon as
/// they change; there is no separate explicit save step for them. Returns
/// commands for Settings actions; the application composition root owns
/// configuration I/O and applies successful replacements to `AppState`.
#[derive(Clone)]
pub struct SettingsViewModel {
    pub chip_layout: ChipLayout,
    pub status_bar_visible: bool,
    pub show_session_details: bool,
    pub confirm_session_close: bool,
    pub restore_workspace: bool,
    pub terminal_font: TerminalFontPreference,
    pub terminal_ligatures: bool,
    pub emoji_presentation: EmojiPresentationPreference,
    pub scroll_speed: ScrollSpeedPreference,
    pub scrollback_limit: ScrollbackLimitPreference,
    pub quick_switch_overlay: bool,
    pub compact_launcher_grid: bool,
    pub pulse_new_output_dot: bool,
    pub show_resumable_sessions: bool,
    pub default_sftp_local_directory: Option<String>,
    pub sftp_pane_order: SftpPaneOrderPreference,
}

#[derive(Clone, Default)]
struct SettingsState {
    default_sftp_local_directory: String,
    sftp_pane_order: Option<SftpPaneOrderPreference>,
    synced_value: Option<String>,
    feedback: Option<String>,
}

fn settings_sftp_directory_field_id(ui: &Ui) -> egui::Id {
    ui.make_persistent_id("settings_default_sftp_local_directory")
}

pub fn show_settings(
    ui: &mut Ui,
    settings: SettingsViewModel,
    command_palette_shortcut: &str,
    settings_shortcut: &str,
) -> Option<AppCommand> {
    let SettingsViewModel {
        chip_layout,
        status_bar_visible,
        show_session_details,
        confirm_session_close,
        restore_workspace,
        terminal_font,
        terminal_ligatures,
        emoji_presentation,
        scroll_speed,
        scrollback_limit,
        quick_switch_overlay,
        compact_launcher_grid,
        pulse_new_output_dot,
        show_resumable_sessions,
        default_sftp_local_directory,
        sftp_pane_order,
    } = settings;
    let state_id = ui.id().with("settings_state");
    let field_id = settings_sftp_directory_field_id(ui);
    let mut state = ui.data(|data| data.get_temp::<SettingsState>(state_id).unwrap_or_default());
    let model_value = default_sftp_local_directory.unwrap_or_default();
    let field_focused = ui.memory(|memory| memory.has_focus(field_id));
    if state.synced_value.as_deref() != Some(model_value.as_str()) && !field_focused {
        state.default_sftp_local_directory = model_value.clone();
        state.synced_value = Some(model_value);
        state.feedback = None;
    }
    state.sftp_pane_order.get_or_insert(sftp_pane_order);
    let mut command = None;
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        // Bound Settings' own height to whatever room is actually left
        // above the status bar (queried from its persisted panel state,
        // the same technique the SSH profile editor panel uses): the
        // card-based layout is taller than the old flat button list, and
        // without this it can paint straight into - or past - the status
        // bar instead of stopping short of it.
        let panel_top = ui.cursor().top();
        let mut viewport_bottom = ui.ctx().content_rect().bottom();
        if let Some(status_bar) =
            egui::containers::panel::PanelState::load(ui.ctx(), egui::Id::new("status_bar"))
        {
            viewport_bottom = viewport_bottom.min(status_bar.outer_rect.top());
        }
        let available_height = (viewport_bottom - panel_top).max(0.0);
        // `ScrollArea` computes its own available space via
        // `ui.available_rect_before_wrap()`. Handing it a `ui` whose
        // `max_rect` isn't already a real, bounded rect (as is the case
        // here, directly inside a `ui.horizontal`) leads to a degenerate
        // sizing pass that -- besides being wrong for layout -- also
        // breaks click routing for widgets painted via `egui::Frame`
        // inside the scroll area. Giving the scroll area its own child
        // `Ui` with an explicit, non-degenerate `max_rect` (the same
        // technique the SSH profile editor uses) avoids both problems.
        let scroll_rect = egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2(ui.available_width(), available_height),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_rect), |ui| {
            // egui's default floating scroll style reveals the bar for
            // *any* hover inside the scroll area's content, not just when
            // the pointer is actually near the bar - unlike the terminal
            // view's own history scrollbar, which stays hidden until it's
            // scrolled away from rest or the pointer is right over it.
            // Zeroing the "active" (any-content-hover) opacities while
            // keeping "interact" (hovering/dragging the bar itself) at
            // full strength reproduces that same narrower reveal condition
            // here.
            let mut scroll_style = egui::style::ScrollStyle::floating();
            scroll_style.active_handle_opacity = 0.0;
            scroll_style.active_background_opacity = 0.0;
            ui.spacing_mut().scroll = scroll_style;
            egui::ScrollArea::vertical()
                .max_height(available_height)
                .show(ui, |ui| {
                    // The scroll bar itself belongs to this scroll *frame*,
                    // not to Settings' own content: it is given its own
                    // reserved lane on the right, by keeping the cards
                    // narrower than the frame instead of shrinking the
                    // frame itself. That way the (invisible until needed)
                    // scroll bar never has to sit on top of the cards' own
                    // right edge.
                    ui.set_max_width((ui.available_width() - CONTENT_SCROLLBAR_LANE).max(0.0));
                    ui.vertical(|ui| {
                        ui.add_space(24.0);
                        ui.heading("Settings");
                        ui.add_space(2.0);

                        settings_card(ui, "Interface", |ui| {
                            if settings_segmented_row(
                                ui,
                                "Session chip layout",
                                "Keep terminal height stable with one scrolling row.",
                                &[
                                    ("Single row", !matches!(chip_layout, ChipLayout::Wrap)),
                                    ("Wrap", matches!(chip_layout, ChipLayout::Wrap)),
                                ],
                            )
                            .is_some()
                            {
                                command = Some(AppCommand::ToggleChipLayout);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Show session details in chips",
                                "Show the terminal title or launch context beneath the \
                                 session name. Off makes every chip compact and single-line, \
                                 moving the active session's detail to the status bar.",
                                show_session_details,
                            ) {
                                command = Some(AppCommand::ToggleShowSessionDetails);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Show status bar",
                                "Display sourced session state, terminal dimensions, and \
                                 the active session detail when compact chips require it.",
                                status_bar_visible,
                            ) {
                                command = Some(AppCommand::ToggleStatusBar);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Confirm before closing live sessions",
                                "Ask before terminating a running local process or \
                                 disconnecting an active remote session.",
                                confirm_session_close,
                            ) {
                                command = Some(AppCommand::ToggleConfirmSessionClose);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Workspace restore",
                                "Reopen your previously open tabs and the active tab \
                                 automatically on launch. Off by default.",
                                restore_workspace,
                            ) {
                                command = Some(AppCommand::ToggleRestoreWorkspace);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Compact multi-column New Session list",
                                "Show saved profiles in a responsive multi-column grid on \
                                 the New Session tab when the window is wide enough, \
                                 reducing vertical scrolling. Falls back to a single \
                                 column at narrow widths. Off by default.",
                                compact_launcher_grid,
                            ) {
                                command = Some(AppCommand::ToggleCompactLauncherGrid);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Pulse status dot on new background output",
                                "Slow-pulse a background tab's chip status dot when that \
                                 session has produced output since you last looked at it, \
                                 so it can quietly draw your eye without changing its \
                                 connection-state color. The active tab's own chip never \
                                 pulses. Off by default.",
                                pulse_new_output_dot,
                            ) {
                                command = Some(AppCommand::TogglePulseNewOutputDot);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Resume unattached local sessions from New Session",
                                "Surface locally running festerm-sessiond persistence \
                                 sessions that have no attached window as one-click \
                                 \"Resume\" entries on the New Session tab. Off by \
                                 default.",
                                show_resumable_sessions,
                            ) {
                                command = Some(AppCommand::ToggleShowResumableSessions);
                            }
                            ui.add_space(10.0);
                            if ui.button("Reset interface settings to defaults").clicked() {
                                command = Some(AppCommand::ResetInterfaceSettings);
                            }
                        });

                        ui.add_space(12.0);

                        settings_card(ui, "Scrolling", |ui| {
                            if let Some(selected) = settings_segmented_row(
                                ui,
                                "Scrollback limit",
                                "Maximum retained history for newly created sessions. \
                                 Existing sessions keep their current limit.",
                                &ScrollbackLimitPreference::ALL
                                    .map(|limit| (limit.label(), limit == scrollback_limit)),
                            ) {
                                command = Some(AppCommand::SetScrollbackLimit(
                                    ScrollbackLimitPreference::ALL[selected],
                                ));
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            let mut selected_speed = scroll_speed;
                            if let Some(new_speed) = settings_clickstop_row(
                                ui,
                                "Scroll speed",
                                "How far one trackpad or mouse wheel scroll step moves \
                                 through scrollback history.",
                                &ScrollSpeedPreference::ALL,
                                selected_speed,
                            ) {
                                selected_speed = new_speed;
                            }
                            if selected_speed != scroll_speed {
                                command = Some(AppCommand::SetScrollSpeed(selected_speed));
                            }
                        });

                        ui.add_space(12.0);

                        settings_card(ui, "Terminal typography", |ui| {
                            let mut selected_font = terminal_font;
                            egui::Sides::new().show(
                                ui,
                                |ui| {
                                    ui.set_max_width(ui.available_width() - 190.0);
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new("Terminal font")
                                                .color(theme::TEXT_PRIMARY),
                                        );
                                        ssh_paragraph(
                                            ui,
                                            "Choose the bundled primary face used by terminal \
                                             cells. Application text is unchanged.",
                                        );
                                    });
                                },
                                |ui| {
                                    egui::ComboBox::from_id_salt("terminal-font-family")
                                        .selected_text(terminal_font_label(selected_font))
                                        .width(160.0)
                                        .show_ui(ui, |ui| {
                                            for font in [
                                                TerminalFontPreference::JetBrainsMono,
                                                TerminalFontPreference::IosevkaTerm,
                                                TerminalFontPreference::JuliaMono,
                                                TerminalFontPreference::MapleMono,
                                            ] {
                                                ui.selectable_value(
                                                    &mut selected_font,
                                                    font,
                                                    terminal_font_label(font),
                                                );
                                            }
                                        });
                                },
                            );
                            if selected_font != terminal_font {
                                command = Some(AppCommand::SetTerminalFont(selected_font));
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Programming ligatures",
                                "Shape eligible adjacent cells together while preserving cursor, \
                                 selection, and terminal grid ownership.",
                                terminal_ligatures,
                            ) {
                                command = Some(AppCommand::ToggleTerminalLigatures);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if let Some(selected) = settings_segmented_row(
                                ui,
                                "Emoji presentation",
                                "Use bundled color artwork or deterministic monochrome fallback. \
                                 Terminal cell geometry is unchanged.",
                                &[
                                    (
                                        "Color",
                                        emoji_presentation == EmojiPresentationPreference::Color,
                                    ),
                                    (
                                        "Monochrome",
                                        emoji_presentation
                                            == EmojiPresentationPreference::Monochrome,
                                    ),
                                ],
                            ) {
                                command =
                                    Some(AppCommand::SetEmojiPresentation(if selected == 0 {
                                        EmojiPresentationPreference::Color
                                    } else {
                                        EmojiPresentationPreference::Monochrome
                                    }));
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
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ssh_paragraph(ui, "Open Settings");
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new(settings_shortcut)
                                        .size(12.0)
                                        .color(theme::TEXT_MUTED),
                                );
                            });
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Show quick-switch numbers",
                                "While the quick-switch modifier is held, briefly overlay each \
                                 eligible chip's number (1-9) in place of its usual status \
                                 presentation.",
                                quick_switch_overlay,
                            ) {
                                command = Some(AppCommand::ToggleQuickSwitchOverlay);
                            }
                        });

                        ui.add_space(12.0);

                        settings_card(ui, "SFTP", |ui| {
                            ui.horizontal_top(|ui| {
                                ui.vertical(|ui| {
                                    ui.set_max_width((ui.available_width() - 190.0).max(0.0));
                                    ui.label(
                                        egui::RichText::new("SFTP pane order")
                                            .color(theme::TEXT_PRIMARY),
                                    );
                                    ssh_paragraph(
                                        ui,
                                        "Visual order for the GUI SFTP file manager. Commands and accessibility labels still refer to Local and Remote, never Left and Right.",
                                    );
                                });
                                ui.add_space(16.0);
                                ui.vertical(|ui| {
                                    for (value, label) in [
                                        (
                                            SftpPaneOrderPreference::LocalLeft,
                                            "Local left · Remote right",
                                        ),
                                        (
                                            SftpPaneOrderPreference::RemoteLeft,
                                            "Remote left · Local right",
                                        ),
                                    ] {
                                        let response = ui.radio_value(
                                            state
                                                .sftp_pane_order
                                                .get_or_insert(sftp_pane_order),
                                            value,
                                            label,
                                        );
                                        if response.changed() {
                                            command = Some(AppCommand::SetSftpPaneOrder(value));
                                        }
                                    }
                                });
                            });
                            ui.add_space(10.0);
                            ui.horizontal_top(|ui| {
                                let mut label_id = None;
                                ui.vertical(|ui| {
                                    ui.set_max_width((ui.available_width() - 190.0).max(0.0));
                                    let label = ui.label(
                                        egui::RichText::new("Default local SFTP directory")
                                            .color(theme::TEXT_PRIMARY),
                                    );
                                    label_id = Some(label.id);
                                    ssh_paragraph(
                                        ui,
                                        "Starting local directory for new SFTP tabs. \
                                         Changing it updates only future sessions; `lcd` \
                                         affects the live session only.",
                                    );
                                });
                                ui.add_space(16.0);
                                ui.vertical(|ui| {
                                    let response = ui.add(
                                        TextEdit::singleline(
                                            &mut state.default_sftp_local_directory,
                                        )
                                        .id(field_id)
                                        .hint_text("Path to local directory")
                                        .desired_width(180.0),
                                    );
                                    let response = response
                                        .labelled_by(label_id.expect("label should be rendered"));
                                    if response.changed() {
                                        let trimmed = state.default_sftp_local_directory.trim();
                                        if trimmed.is_empty() {
                                            state.feedback = None;
                                            state.synced_value = Some(String::new());
                                            command =
                                                Some(AppCommand::SetDefaultSftpLocalDirectory(
                                                    None,
                                                ));
                                        } else if trimmed.chars().any(char::is_control) {
                                            state.feedback = Some(
                                                "Default local SFTP directory must not contain control characters."
                                                    .to_owned(),
                                            );
                                        } else {
                                            state.feedback = None;
                                            state.synced_value = Some(trimmed.to_owned());
                                            command = Some(
                                                AppCommand::SetDefaultSftpLocalDirectory(Some(
                                                    PathBuf::from(trimmed),
                                                )),
                                            );
                                        }
                                    }
                                });
                            });
                            if let Some(feedback) = &state.feedback {
                                ui.add_space(6.0);
                                ui.colored_label(theme::STATUS_ERROR, feedback);
                            }
                        });
                    });
                });
        });
    });
    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
}

const fn terminal_font_label(font: TerminalFontPreference) -> &'static str {
    match font {
        TerminalFontPreference::JetBrainsMono => "JetBrains Mono",
        TerminalFontPreference::IosevkaTerm => "Iosevka Term",
        TerminalFontPreference::JuliaMono => "JuliaMono",
        TerminalFontPreference::MapleMono => "Maple Mono",
    }
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
            ssh_section_heading(ui, title);
            ui.add_space(6.0);
            body(ui);
        });
}

/// One labeled on/off preference row: a fixed title and state-independent
/// description on the left, and a pill-shaped toggle switch on the right
/// (`docs/images/gui-mockups/settings.png`) - replacing a plain text button
/// whose entire label used to flip between "shown"/"hidden" copy. Returns
/// whether the switch was clicked this frame; the caller still owns
/// dispatching the actual `AppCommand`, matching every other control here.
fn settings_toggle_row(ui: &mut Ui, title: &str, description: &str, value: bool) -> bool {
    let mut clicked = false;
    egui::Sides::new().show(
        ui,
        |ui| {
            // Reserve room for the switch itself (and the `Sides` gap) so
            // the description wraps at measurement time instead of laying
            // out as one long unwrapped line that pushes the switch off
            // the right edge of the card.
            ui.set_max_width(ui.available_width() - 60.0);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).color(theme::TEXT_PRIMARY));
                ssh_paragraph(ui, description);
            });
        },
        |ui| {
            clicked = toggle_switch(ui, value, title).clicked();
        },
    );
    clicked
}

/// Painter-drawn pill-shaped toggle switch matching the mockup's on/off
/// control (`docs/images/gui-mockups/settings.png`): a rounded track that
/// fills with the accent color when on, and a circular knob that slides to
/// the matching side - instead of a text button whose whole label changes
/// between "shown"/"hidden" copy. An explicit accessible label is set (like
/// `paint_close_button`'s pattern) since the switch has no text of its own
/// for screen readers or headless-test queries to find.
fn toggle_switch(ui: &mut Ui, value: bool, accessible_label: &str) -> egui::Response {
    let desired_size = egui::vec2(40.0, 22.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Checkbox, true, accessible_label));

    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool(response.id, value);
        let rounding = rect.height() / 2.0;
        let track_fill = theme::SURFACE_TAB_INACTIVE.lerp_to_gamma(theme::ACCENT_PRIMARY, how_on);
        let track_stroke = theme::BORDER_SUBTLE.lerp_to_gamma(theme::ACCENT_PRIMARY, how_on);
        ui.painter().rect_filled(rect, rounding, track_fill);
        ui.painter().rect_stroke(
            rect,
            rounding,
            Stroke::new(1.0, track_stroke),
            egui::StrokeKind::Inside,
        );
        let knob_radius = rounding - 3.0;
        let knob_x = egui::lerp((rect.left() + rounding)..=(rect.right() - rounding), how_on);
        ui.painter().circle_filled(
            egui::pos2(knob_x, rect.center().y),
            knob_radius,
            egui::Color32::WHITE,
        );
    }

    response.on_hover_text(accessible_label)
}

/// One labeled multi-choice preference row: a fixed title/description on the
/// left and a segmented button group on the right
/// (`docs/images/gui-mockups/settings.png`'s "Session chip layout" row),
/// replacing a single text button whose label flipped to name the *other*
/// choice. Returns the index of a newly selected (previously inactive)
/// option; clicking the already-active option is a no-op, matching ordinary
/// segmented-control behavior.
fn settings_segmented_row(
    ui: &mut Ui,
    title: &str,
    description: &str,
    options: &[(&str, bool)],
) -> Option<usize> {
    let mut clicked = None;
    egui::Sides::new().show(
        ui,
        |ui| {
            // Same defensive width reservation as `settings_toggle_row`:
            // without it, the description can measure as one long
            // unwrapped line and push the segmented buttons off the right
            // edge of the card (and out of click range).
            ui.set_max_width(ui.available_width() - 170.0);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).color(theme::TEXT_PRIMARY));
                ssh_paragraph(ui, description);
            });
        },
        |ui| {
            ui.horizontal(|ui| {
                for (index, (label, selected)) in options.iter().enumerate() {
                    if ui.selectable_label(*selected, *label).clicked() && !*selected {
                        clicked = Some(index);
                    }
                }
            });
        },
    );
    clicked
}

/// A labeled row with a discrete, clickstop-only slider: dragging or
/// clicking only ever lands on one of `options`' exact indices, unlike a
/// continuous `egui::Slider`, since a scroll-speed multiplier is meant to be
/// chosen from a small named set (mirroring `settings_segmented_row`'s
/// discrete-choice intent) rather than fine-tuned to an arbitrary numeric
/// value. Returns the newly selected value when the slider moves to a
/// different clickstop than `selected` this frame.
fn settings_clickstop_row(
    ui: &mut Ui,
    title: &str,
    description: &str,
    options: &[ScrollSpeedPreference],
    selected: ScrollSpeedPreference,
) -> Option<ScrollSpeedPreference> {
    const SLIDER_WIDTH: f32 = 160.0;

    let mut changed = None;
    // `egui::Sides` defaults its row height to a single `interact_size.y`
    // (matching the toggle/segmented rows' one-line right side), but this
    // row's right side stacks a slider *and* a value label underneath it.
    // Reserve enough height for both stacked lines up front.
    let row_height = ui.spacing().interact_size.y * 2.0 + 4.0;
    egui::Sides::new().height(row_height).show(
        ui,
        |ui| {
            // Same defensive width reservation as the other settings rows:
            // without it the description can measure as one long unwrapped
            // line and push the slider off the right edge of the card.
            ui.set_max_width(ui.available_width() - 190.0);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).color(theme::TEXT_PRIMARY));
                ssh_paragraph(ui, description);
            });
        },
        |ui| {
            // Unlike `ui.horizontal`, plain `ui.vertical` always lays out
            // its children with `Layout::top_down(Align::Min)` and does
            // not mirror the enclosing `Sides` right-to-left direction (see
            // `egui::Ui::horizontal`, which explicitly checks
            // `placer.prefer_right_to_left()` and `ui.vertical`, which
            // does not). A bare `ui.vertical(...)` here inherited the
            // *entire* remaining card width as its rect and then
            // left-aligned the slider and label inside it, so on any card
            // wider than description-text-plus-slider, the block rendered
            // immediately after the description paragraph instead of
            // pinned to the card's right edge - squeezing the slider down
            // to a sliver-sized hit target and spilling the value label
            // over the description (reported: "you can't tell it's
            // actually a slider" and "sliding the value doesn't seem to
            // change scroll speed"). Explicitly allocating a
            // `SLIDER_WIDTH`-wide block lets the *outer* right-to-left
            // cursor place it, matching how `toggle_switch` and
            // `settings_segmented_row`'s `ui.horizontal` already anchor to
            // the right edge.
            ui.allocate_ui_with_layout(
                egui::vec2(SLIDER_WIDTH, row_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    let max_index = options.len().saturating_sub(1);
                    let mut index = selected.index().min(max_index);
                    ui.style_mut().spacing.slider_width = SLIDER_WIDTH;
                    let response = ui.add(
                        egui::Slider::new(&mut index, 0..=max_index)
                            .step_by(1.0)
                            .show_value(false),
                    );
                    if response.changed() {
                        let new_value = ScrollSpeedPreference::from_index(index);
                        if new_value != selected {
                            changed = Some(new_value);
                        }
                    }
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(selected.label())
                            .size(12.0)
                            .color(theme::TEXT_MUTED),
                    );
                },
            );
        },
    );
    changed
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
        Profile::Serial(serial) => (
            "Serial",
            serial.identifier().to_owned(),
            format!("{} · {} baud", serial.device(), serial.baud_rate()),
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
    EditSerial(SerialProfileDraft),
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
    durable_session: DurableSessionDraft,
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
            durable_session: DurableSessionDraft::default(),
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
            durable_session: DurableSessionDraft::from_persistence(local.persistence()),
            error: None,
        }
    }

    fn build(&self) -> Result<Profile, String> {
        let arguments = self
            .arguments
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let working_directory = (!self.working_directory.trim().is_empty())
            .then(|| self.working_directory.trim().to_owned());
        let profile = Profile::local(
            self.name.trim(),
            self.executable.trim(),
            arguments,
            working_directory,
        )
        .map_err(|error| error.to_string())?;
        match self.durable_session.persistence()? {
            Some(persistence) => profile
                .with_persistence(persistence.provider(), persistence.session_name())
                .map_err(|error| error.to_string()),
            None => Ok(profile),
        }
    }
}

#[derive(Clone)]
struct SshProfileDraft {
    original_id: Option<String>,
    name: String,
    host: String,
    port: String,
    username: String,
    port_forwards: Vec<SshPortForwardDraft>,
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
    durable_session: DurableSessionDraft,
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
            port_forwards: Vec::new(),
            auth_method: SshAuthenticationMethod::Password,
            password: String::new(),
            private_key: String::new(),
            key_passphrase: String::new(),
            has_stored_credential: false,
            stored_credential_kind: CredentialKind::Password,
            durable_session: DurableSessionDraft::default(),
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
            port_forwards: ssh
                .port_forwards()
                .iter()
                .map(SshPortForwardDraft::from_configuration)
                .collect(),
            auth_method: match stored_credential_kind {
                CredentialKind::Password => SshAuthenticationMethod::Password,
                CredentialKind::PrivateKey => SshAuthenticationMethod::PrivateKey,
            },
            password: String::new(),
            private_key: String::new(),
            key_passphrase: String::new(),
            has_stored_credential: ssh.credential_reference().is_some(),
            stored_credential_kind,
            durable_session: DurableSessionDraft::from_persistence(ssh.persistence()),
            error: None,
        }
    }

    fn build(&self, existing_profile: Option<&SshProfileConfiguration>) -> Result<Profile, String> {
        let port: u16 = self
            .port
            .trim()
            .parse()
            .map_err(|_| "SSH port must be a number between 1 and 65535".to_owned())?;
        let port_forwards = self
            .port_forwards
            .iter()
            .map(SshPortForwardDraft::build)
            .collect::<Result<Vec<_>, _>>()?;
        let profile = Profile::ssh(
            self.name.trim(),
            self.host.trim(),
            port,
            self.username.trim(),
            "xterm-256color",
            80,
            24,
        )
        .map_err(|error| error.to_string())?;
        let profile = match profile {
            Profile::Ssh(ssh) => Profile::Ssh(
                ssh.with_port_forwards(port_forwards)
                    .map_err(|error| error.to_string())?,
            ),
            Profile::Local(_) | Profile::Serial(_) => unreachable!("Profile::ssh returns SSH"),
        };
        let profile = match self.durable_session.persistence()? {
            Some(persistence) => profile
                .with_persistence(persistence.provider(), persistence.session_name())
                .map_err(|error| error.to_string()),
            None => Ok(profile),
        }?;
        if let Some(existing_profile) = existing_profile {
            if let Some(reference) = existing_profile.credential_reference() {
                return profile
                    .with_credential_reference_kind(
                        reference.duplicate_for_transport(),
                        existing_profile.credential_kind(),
                    )
                    .map_err(|error| error.to_string());
            }
        }
        Ok(profile)
    }
}

#[derive(Clone)]
struct SerialProfileDraft {
    original_id: Option<String>,
    name: String,
    device: String,
    baud_rate: String,
    data_bits: festerm_config::SerialDataBits,
    parity: festerm_config::SerialParity,
    stop_bits: festerm_config::SerialStopBits,
    flow_control: festerm_config::SerialFlowControl,
    error: Option<String>,
}

impl Default for SerialProfileDraft {
    fn default() -> Self {
        Self {
            original_id: None,
            name: String::new(),
            device: String::new(),
            baud_rate: "115200".to_owned(),
            data_bits: festerm_config::SerialDataBits::Eight,
            parity: festerm_config::SerialParity::None,
            stop_bits: festerm_config::SerialStopBits::One,
            flow_control: festerm_config::SerialFlowControl::None,
            error: None,
        }
    }
}

impl SerialProfileDraft {
    fn from_profile(serial: &festerm_config::SerialProfileConfiguration) -> Self {
        Self {
            original_id: Some(serial.identifier().to_owned()),
            name: serial.identifier().to_owned(),
            device: serial.device().to_owned(),
            baud_rate: serial.baud_rate().to_string(),
            data_bits: serial.data_bits(),
            parity: serial.parity(),
            stop_bits: serial.stop_bits(),
            flow_control: serial.flow_control(),
            error: None,
        }
    }

    fn build_profile(&self) -> Result<Profile, String> {
        let baud_rate: u32 = self
            .baud_rate
            .trim()
            .parse()
            .map_err(|_| "Baud rate must be a positive number".to_owned())?;
        Profile::serial(
            self.name.trim(),
            self.device.trim(),
            baud_rate,
            self.data_bits,
            self.parity,
            self.stop_bits,
            self.flow_control,
        )
        .map_err(|error| error.to_string())
    }
}

/// The standalone Profiles management surface: list, create, edit,
/// duplicate, and delete reusable local/SSH launch definitions
/// (`docs/gui-design.md` "Profile editing").
fn serial_enum_combo<T: Copy + PartialEq + SerialEnumLabels>(
    ui: &mut Ui,
    label: &str,
    current: &mut T,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        egui::ComboBox::from_id_salt(("serial_enum_combo", label))
            .selected_text(current.label())
            .show_ui(ui, |ui| {
                for (variant, variant_label) in T::all() {
                    ui.selectable_value(current, variant, variant_label);
                }
            });
    });
}

trait SerialEnumLabels: Sized {
    fn label(&self) -> &'static str;
    fn all() -> Vec<(Self, &'static str)>;
}

impl SerialEnumLabels for festerm_config::SerialDataBits {
    fn label(&self) -> &'static str {
        match self {
            Self::Five => "5",
            Self::Six => "6",
            Self::Seven => "7",
            Self::Eight => "8",
        }
    }
    fn all() -> Vec<(Self, &'static str)> {
        vec![
            (Self::Five, "5"),
            (Self::Six, "6"),
            (Self::Seven, "7"),
            (Self::Eight, "8"),
        ]
    }
}

impl SerialEnumLabels for festerm_config::SerialParity {
    fn label(&self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Odd => "Odd",
            Self::Even => "Even",
        }
    }
    fn all() -> Vec<(Self, &'static str)> {
        vec![
            (Self::None, "None"),
            (Self::Odd, "Odd"),
            (Self::Even, "Even"),
        ]
    }
}

impl SerialEnumLabels for festerm_config::SerialStopBits {
    fn label(&self) -> &'static str {
        match self {
            Self::One => "1",
            Self::Two => "2",
        }
    }
    fn all() -> Vec<(Self, &'static str)> {
        vec![(Self::One, "1"), (Self::Two, "2")]
    }
}

impl SerialEnumLabels for festerm_config::SerialFlowControl {
    fn label(&self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Software => "Software (XON/XOFF)",
            Self::Hardware => "Hardware (RTS/CTS)",
        }
    }
    fn all() -> Vec<(Self, &'static str)> {
        vec![
            (Self::None, "None"),
            (Self::Software, "Software (XON/XOFF)"),
            (Self::Hardware, "Hardware (RTS/CTS)"),
        ]
    }
}

fn profile_text_edit(
    ui: &mut Ui,
    tab_id: TabId,
    field: &'static str,
    label: &str,
    value: &mut String,
) -> egui::Response {
    profile_text_edit_inner(ui, tab_id, field, label, value, false)
}

fn profile_text_edit_with_id(
    ui: &mut Ui,
    tab_id: TabId,
    field: impl std::hash::Hash + std::fmt::Debug,
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
    field: impl std::hash::Hash + std::fmt::Debug,
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
    let dropdown_rect_id = autocomplete_id.with("suggestions-rect");
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

        // A real mouse click on a suggestion first lands here as a click
        // "elsewhere" as far as the text field is concerned, so egui drops
        // the field's focus *before* this function runs again this frame.
        // Without this fallback, `field.has_focus()` would already be false
        // by the time we decide whether to show the dropdown, so the
        // suggestion would vanish out from under the click and never
        // receive it. Keep the dropdown alive for this frame if the click
        // that just happened started inside last frame's dropdown rect.
        let last_dropdown_rect: Option<egui::Rect> =
            ui.data(|data| data.get_temp(dropdown_rect_id));
        let click_started_in_dropdown = ui.input(|input| {
            input.pointer.primary_clicked()
                && input
                    .pointer
                    .interact_pos()
                    .zip(last_dropdown_rect)
                    .is_some_and(|(pos, rect)| rect.contains(pos))
        });

        if (field.has_focus() || click_started_in_dropdown) && !suppress && !value.trim().is_empty()
        {
            let suggestions =
                festerm_pty::search_path_executables(value.trim(), EXECUTABLE_SUGGESTION_LIMIT);
            if !suggestions.is_empty() {
                ui.add_space(4.0);
                let dropdown = egui::Frame::new()
                    .fill(theme::SURFACE_TAB_INACTIVE)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .corner_radius(6.0)
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        for candidate in &suggestions {
                            let text = candidate.display().to_string();
                            // Force a single line and an explicit bright color:
                            // the default inactive-widget text style is dim
                            // (hard to read against the suggestion frame), and
                            // wrapping onto a second line makes long absolute
                            // paths harder to scan at a glance.
                            let response = ui.add(
                                egui::Button::selectable(
                                    false,
                                    egui::RichText::new(&text).color(theme::TEXT_PRIMARY),
                                )
                                .wrap_mode(egui::TextWrapMode::Extend),
                            );
                            if response.clicked() {
                                *value = text;
                                suppress = true;
                            }
                        }
                    });
                ui.data_mut(|data| data.insert_temp(dropdown_rect_id, dropdown.response.rect));
            }
        } else {
            ui.data_mut(|data| data.remove::<egui::Rect>(dropdown_rect_id));
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
                Profile::Serial(serial) => {
                    ProfilesScreenMode::EditSerial(SerialProfileDraft::from_profile(serial))
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
                    if ui.button("New Serial Profile").clicked() {
                        next_mode =
                            Some(ProfilesScreenMode::EditSerial(SerialProfileDraft::default()));
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
                                Profile::Serial(_) => AppCommand::StartConfiguredSerialProfile {
                                    profile_id: name.clone(),
                                },
                            });
                        }
                        if matches!(profile, Profile::Ssh(_))
                            && ui.button("Open SFTP").clicked()
                        {
                            command = Some(AppCommand::OpenConfiguredSftpFileManagerProfile {
                                profile_id: name.clone(),
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
                                Profile::Serial(serial) => ProfilesScreenMode::EditSerial(
                                    SerialProfileDraft::from_profile(serial),
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
                                Profile::Serial(serial) => {
                                    let mut draft = SerialProfileDraft::from_profile(serial);
                                    draft.original_id = None;
                                    draft.name = duplicate_name;
                                    ProfilesScreenMode::EditSerial(draft)
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
            show_bounded_content_scroll(ui, (tab_id, "edit_local_profile_scroll"), |ui| {
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
                        if profile_text_edit(ui, tab_id, "name", "Name", &mut draft.name).changed()
                        {
                            draft
                                .durable_session
                                .sync_session_name_from_profile_name(&draft.name);
                        }
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
                        ui.add_space(10.0);
                        ssh_section_heading(ui, "Durable session");
                        show_durable_session_controls(
                            ui,
                            tab_id,
                            &mut draft.durable_session,
                            DurableSessionTarget::Local,
                            false,
                        );
                        ssh_paragraph(
                            ui,
                            "Available only on saved Local profiles. The built-in Local Shell always starts a fresh plain shell.",
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
                            configure_content_scrollbar(ui);
                            ScrollArea::vertical()
                            .id_salt((tab_id, "edit_ssh_profile_scroll"))
                            .max_height(scroll_max_height)
                            .show(ui, |ui| {
                                ui.set_max_width(
                                    (ui.available_width() - CONTENT_SCROLLBAR_LANE).max(0.0),
                                );
                                ssh_section_heading(ui, "Connection");
                                if profile_text_edit(ui, tab_id, "name", "Name", &mut draft.name)
                                    .changed()
                                {
                                    draft
                                        .durable_session
                                        .sync_session_name_from_profile_name(&draft.name);
                                }
                                profile_text_edit(
                                    ui,
                                    tab_id,
                                    "username",
                                    "Username",
                                    &mut draft.username,
                                );
                                profile_text_edit(ui, tab_id, "host", "Host", &mut draft.host);
                                profile_text_edit(ui, tab_id, "port", "Port", &mut draft.port);
                                ui.add_space(10.0);
                                ssh_section_heading(ui, "Durable session");
                                show_durable_session_controls(
                                    ui,
                                    tab_id,
                                    &mut draft.durable_session,
                                    DurableSessionTarget::Remote,
                                    false,
                                );
                                ui.add_space(10.0);
                                ssh_section_heading(ui, "Port forwards");
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(
                                            "New rows start on 127.0.0.1 and apply on future connects.",
                                        )
                                        .size(11.0)
                                        .color(theme::TEXT_SECONDARY),
                                    );
                                    if ui.button("Add port forward").clicked() {
                                        draft.port_forwards.push(SshPortForwardDraft::default());
                                    }
                                });
                                let mut remove_forward = None;
                                for (index, forward) in draft.port_forwards.iter_mut().enumerate() {
                                    ui.add_space(8.0);
                                    egui::Frame::new()
                                        .fill(theme::SURFACE_TAB_ACTIVE)
                                        .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
                                        .corner_radius(6.0)
                                        .inner_margin(egui::Margin::same(12))
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "Forward {}",
                                                        index + 1
                                                    ))
                                                    .color(theme::TEXT_SECONDARY),
                                                );
                                                if ui
                                                    .button(format!(
                                                        "Remove forward {}",
                                                        index + 1
                                                    ))
                                                    .clicked()
                                                {
                                                    remove_forward = Some(index);
                                                }
                                            });
                                            ui.add_space(4.0);
                                            ui.horizontal(|ui| {
                                                ui.label("Direction");
                                                ui.radio_value(
                                                    &mut forward.direction,
                                                    SshPortForwardDirection::Local,
                                                    "Local",
                                                );
                                                ui.radio_value(
                                                    &mut forward.direction,
                                                    SshPortForwardDirection::Remote,
                                                    "Remote",
                                                );
                                            });
                                            profile_text_edit_with_id(
                                                ui,
                                                tab_id,
                                                ("ssh_port_forward_bind_host", index),
                                                "Bind host",
                                                &mut forward.bind_host,
                                            );
                                            profile_text_edit_with_id(
                                                ui,
                                                tab_id,
                                                ("ssh_port_forward_bind_port", index),
                                                "Bind port",
                                                &mut forward.bind_port,
                                            );
                                            profile_text_edit_with_id(
                                                ui,
                                                tab_id,
                                                ("ssh_port_forward_destination_host", index),
                                                "Destination host",
                                                &mut forward.destination_host,
                                            );
                                            profile_text_edit_with_id(
                                                ui,
                                                tab_id,
                                                ("ssh_port_forward_destination_port", index),
                                                "Destination port",
                                                &mut forward.destination_port,
                                            );
                                        });
                                }
                                if let Some(index) = remove_forward {
                                    draft.port_forwards.remove(index);
                                }
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
                                let existing_profile = draft
                                    .original_id
                                    .as_deref()
                                    .and_then(|identifier| configuration.profile(identifier))
                                    .and_then(Profile::as_ssh);
                                match draft.build(existing_profile) {
                                    Ok(profile) => {
                                        command = Some(AppCommand::SaveProfile { profile });
                                        next_mode = Some(ProfilesScreenMode::List);
                                    }
                                    Err(error) => draft.error = Some(error),
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                next_mode = Some(ProfilesScreenMode::List);
                            }
                        });
                    });
            });
        }
        ProfilesScreenMode::EditSerial(draft) => {
            show_bounded_content_scroll(ui, (tab_id, "serial_profile_editor"), |ui| {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                let heading = if draft.original_id.is_some() {
                    "Edit Serial Profile"
                } else {
                    "New Serial Profile"
                };
                ui.heading(heading);
                ui.add_space(12.0);
                profile_text_edit(ui, tab_id, "serial_name", "Name", &mut draft.name);
                profile_text_edit(ui, tab_id, "serial_device", "Device", &mut draft.device);
                profile_text_edit(
                    ui,
                    tab_id,
                    "serial_baud_rate",
                    "Baud rate",
                    &mut draft.baud_rate,
                );
                ui.add_space(8.0);
                serial_enum_combo(ui, "Data bits", &mut draft.data_bits);
                serial_enum_combo(ui, "Parity", &mut draft.parity);
                serial_enum_combo(ui, "Stop bits", &mut draft.stop_bits);
                serial_enum_combo(ui, "Flow control", &mut draft.flow_control);
                if let Some(error) = &draft.error {
                    ui.add_space(8.0);
                    ui.colored_label(theme::STATUS_ERROR, error.as_str());
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        match draft.build_profile() {
                            Ok(profile) => {
                                command = Some(AppCommand::SaveProfile { profile });
                                next_mode = Some(ProfilesScreenMode::List);
                            }
                            Err(error) => draft.error = Some(error),
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
        harness_with_profiles_and_grid(profiles, false, 520.0)
    }

    fn harness_with_profiles_and_grid(
        profiles: Vec<Profile>,
        compact_launcher_grid: bool,
        width: f32,
    ) -> Harness<'static, LauncherHarnessState> {
        harness_with_profiles_grid_and_resumable(profiles, compact_launcher_grid, width, Vec::new())
    }

    fn harness_with_profiles_grid_and_resumable(
        profiles: Vec<Profile>,
        compact_launcher_grid: bool,
        width: f32,
        resumable_sessions: Vec<festerm_sessiond::UnattachedSession>,
    ) -> Harness<'static, LauncherHarnessState> {
        Harness::builder()
            .with_size(egui::vec2(width, 560.0))
            .build_ui_state(
                move |ui, state: &mut LauncherHarnessState| {
                    if let Some(command) = show_launcher(
                        ui,
                        state.tab_id,
                        &state.profiles,
                        true,
                        None,
                        compact_launcher_grid,
                        &resumable_sessions,
                    ) {
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

    struct SettingsHarnessState {
        command: Option<AppCommand>,
    }

    fn settings_harness() -> Harness<'static, SettingsHarnessState> {
        settings_harness_with_width(520.0)
    }

    /// A wider settings harness, matching a typical desktop window rather
    /// than the other settings tests' narrow fixed harness width. Needed to
    /// reproduce the "Scroll speed" slider mispositioning regression (see
    /// `scroll_speed_slider_is_reachable_and_dispatches_the_next_clickstop`):
    /// the bug only appears once the card is wider than the description
    /// text plus the slider's own width, which the narrow harness never is.
    fn wide_settings_harness() -> Harness<'static, SettingsHarnessState> {
        settings_harness_with_width(1400.0)
    }

    fn settings_harness_with_width(width: f32) -> Harness<'static, SettingsHarnessState> {
        Harness::builder()
            .with_size(egui::vec2(width, 2000.0))
            .build_ui_state(
                |ui, state: &mut SettingsHarnessState| {
                    if let Some(command) = show_settings(
                        ui,
                        SettingsViewModel {
                            chip_layout: ChipLayout::Wrap,
                            status_bar_visible: true,
                            show_session_details: true,
                            confirm_session_close: true,
                            restore_workspace: false,
                            terminal_font: TerminalFontPreference::JetBrainsMono,
                            terminal_ligatures: false,
                            emoji_presentation: EmojiPresentationPreference::Color,
                            scroll_speed: ScrollSpeedPreference::Normal,
                            scrollback_limit: ScrollbackLimitPreference::MiB64,
                            quick_switch_overlay: false,
                            compact_launcher_grid: false,
                            pulse_new_output_dot: false,
                            show_resumable_sessions: false,
                            default_sftp_local_directory: None,
                            sftp_pane_order: SftpPaneOrderPreference::LocalLeft,
                        },
                        "Cmd+Shift+P",
                        "Cmd+Shift+S",
                    ) {
                        state.command = Some(command);
                    }
                },
                SettingsHarnessState { command: None },
            )
    }

    #[test]
    fn settings_has_no_manual_reload_or_save_controls() {
        // Regression test: Settings used to offer explicit "Reload
        // configuration"/"Save workspace" buttons; configuration now
        // save/restores automatically, so neither control (nor their
        // explanatory copy) should be present any more.
        let mut harness = settings_harness();
        harness.run();

        assert!(harness.query_by_label("Reload configuration").is_none());
        assert!(harness.query_by_label("Save workspace").is_none());
        assert!(harness
            .query_by_label("Configuration is never written automatically.")
            .is_none());
        assert!(harness
            .query_by_label("Chip layout and status bar visibility are saved automatically.")
            .is_none());
    }

    #[test]
    fn settings_has_no_configuration_card() {
        // Regression test: the "Configuration" card (startup/save status
        // copy plus native-secure-storage status) was removed from
        // Settings; that status is not shown here any more (secure storage
        // status already surfaces on the Launcher instead).
        let mut harness = settings_harness();
        harness.run();

        assert!(harness.query_by_label("Configuration").is_none());
        assert!(harness.query_by_label("Native secure storage").is_none());
    }

    #[test]
    fn settings_keyboard_card_shows_the_settings_hotkey() {
        let mut harness = settings_harness();
        harness.run();

        assert!(harness.query_by_label("Open Settings").is_some());
        assert!(harness.query_by_label("Cmd+Shift+S").is_some());
    }

    #[test]
    fn settings_toggle_chip_layout_control_returns_the_toggle_command() {
        let mut harness = settings_harness();
        harness.run();

        // The harness starts in `ChipLayout::Wrap`; clicking the *other*,
        // currently-inactive segmented option ("Single row") is what
        // selects a new value. Clicking the already-active "Wrap" option
        // is a no-op, matching ordinary segmented-control behavior.
        harness.get_by_label("Single row").click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleChipLayout)
        ));
    }

    #[test]
    fn settings_toggle_restore_workspace_control_returns_the_toggle_command() {
        // Regression test for the "Workspace restore" preference: off by
        // default, with its own explicit toggle distinct from the
        // always-autosaving chip-layout/status-bar/session-detail toggles.
        let mut harness = settings_harness();
        harness.run();

        assert!(harness
            .query_by_role_and_label(accesskit::Role::CheckBox, "Workspace restore")
            .is_some());

        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, "Workspace restore")
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleRestoreWorkspace)
        ));
    }

    #[test]
    fn settings_toggle_compact_launcher_grid_control_returns_the_toggle_command() {
        // Regression test for the "Compact multi-column New Session list"
        // preference (feature request #64): off by default, with its own
        // explicit toggle in the Interface card.
        let mut harness = settings_harness();
        harness.run();

        assert!(harness
            .query_by_role_and_label(
                accesskit::Role::CheckBox,
                "Compact multi-column New Session list"
            )
            .is_some());

        harness
            .get_by_role_and_label(
                accesskit::Role::CheckBox,
                "Compact multi-column New Session list",
            )
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleCompactLauncherGrid)
        ));
    }

    #[test]
    fn settings_auto_applies_default_sftp_local_directory_edits() {
        let mut harness = settings_harness();
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        harness.run();

        harness.get_by_label("Default local SFTP directory").focus();
        harness
            .get_by_label("Default local SFTP directory")
            .type_text(directory.to_string_lossy().as_ref());
        harness.run();

        assert!(matches!(
            harness.state().command.as_ref(),
            Some(AppCommand::SetDefaultSftpLocalDirectory(Some(path))) if path == &directory
        ));
    }

    #[test]
    fn settings_sftp_pane_order_control_dispatches_the_selected_preference() {
        let mut harness = settings_harness();
        harness.run();

        harness
            .get_by_role_and_label(accesskit::Role::RadioButton, "Remote left · Local right")
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::SetSftpPaneOrder(
                SftpPaneOrderPreference::RemoteLeft
            ))
        ));
    }

    #[test]
    fn settings_accept_missing_default_sftp_local_directory_metadata() {
        let mut harness = settings_harness();
        let missing = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("does-not-exist-default-sftp-local-directory");
        harness.run();

        harness.get_by_label("Default local SFTP directory").focus();
        harness
            .get_by_label("Default local SFTP directory")
            .type_text(missing.to_string_lossy().as_ref());
        harness.run();

        assert!(matches!(
            harness.state().command.as_ref(),
            Some(AppCommand::SetDefaultSftpLocalDirectory(Some(path))) if path == &missing
        ));
        assert!(
            harness
                .query_by_label("Default local SFTP directory must not contain control characters.")
                .is_none(),
            "ordinary path metadata must not show inline validation errors"
        );
    }

    #[test]
    fn settings_toggle_pulse_new_output_dot_control_returns_the_toggle_command() {
        // Regression test for the "Pulse status dot on new background
        // output" preference (feature request #68): off by default, with
        // its own explicit toggle in the Interface card.
        let mut harness = settings_harness();
        harness.run();

        assert!(harness
            .query_by_role_and_label(
                accesskit::Role::CheckBox,
                "Pulse status dot on new background output"
            )
            .is_some());

        harness
            .get_by_role_and_label(
                accesskit::Role::CheckBox,
                "Pulse status dot on new background output",
            )
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::TogglePulseNewOutputDot)
        ));
    }

    #[test]
    fn scroll_speed_slider_is_reachable_and_dispatches_the_next_clickstop() {
        // Regression test for `settings_clickstop_row` rendering the
        // "Scroll speed" slider unusably: unlike `ui.horizontal` (used by
        // `settings_segmented_row`), plain `ui.vertical` does not mirror
        // `egui::Sides`' right-to-left direction (see `egui::Ui::horizontal`,
        // which checks `placer.prefer_right_to_left()`, versus `ui.vertical`,
        // which always lays out `Layout::top_down(Align::Min)`). A bare
        // `ui.vertical(...)` on the right side inherited the *entire*
        // remaining card width and then left-aligned the slider inside it,
        // so on any card wider than description-text-plus-slider the block
        // rendered immediately after the description paragraph instead of
        // pinned to the card's right edge like every other settings row -
        // squeezing the slider down to a tiny hit target and spilling the
        // value label over the description (reported: "you can't tell it's
        // actually a slider" and "sliding the value doesn't seem to change
        // scroll speed"). A width at least as wide as `docs/gui-mockups`'
        // settings card is required to reproduce this: the bug was invisible
        // at the narrow fixed-size harness width used by the other settings
        // tests here.
        let mut harness = wide_settings_harness();
        harness.run();

        let slider = harness.get_by_role(accesskit::Role::Slider);
        let card_right_edge = harness
            .get_by_role_and_label(accesskit::Role::CheckBox, "Workspace restore")
            .rect()
            .right();
        assert!(
            slider.rect().width() >= 100.0,
            "expected the clickstop slider to render at its configured width, got {:?}",
            slider.rect()
        );
        assert!(
            (slider.rect().right() - card_right_edge).abs() <= 40.0,
            "expected the slider to be pinned to the card's right edge like every \
             other settings control, but it rendered at {:?} while the card's \
             right edge is at {card_right_edge}",
            slider.rect()
        );

        slider.focus();
        harness.run();
        harness.key_press(egui::Key::ArrowRight);
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::SetScrollSpeed(ScrollSpeedPreference::Fast))
        ));
    }

    #[test]
    fn settings_toggle_show_resumable_sessions_control_returns_the_toggle_command() {
        // Regression test for the "Resume unattached local sessions from
        // New Session" preference (feature request #70): off by default,
        // with its own explicit toggle in the Interface card.
        let mut harness = settings_harness();
        harness.run();

        assert!(harness
            .query_by_role_and_label(
                accesskit::Role::CheckBox,
                "Resume unattached local sessions from New Session"
            )
            .is_some());

        harness
            .get_by_role_and_label(
                accesskit::Role::CheckBox,
                "Resume unattached local sessions from New Session",
            )
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleShowResumableSessions)
        ));
    }

    #[test]
    fn settings_close_confirmation_control_returns_the_toggle_command() {
        let mut harness = settings_harness();
        harness.run();

        let label = "Confirm before closing live sessions";
        assert!(harness
            .query_by_role_and_label(accesskit::Role::CheckBox, label)
            .is_some());

        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, label)
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleConfirmSessionClose)
        ));
    }

    #[test]
    fn settings_exposes_terminal_font_ligature_and_emoji_controls() {
        let mut harness = settings_harness();
        harness.run();

        assert!(harness.query_by_label("Terminal font").is_some());
        let ligatures = "Programming ligatures";
        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, ligatures)
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleTerminalLigatures)
        ));
        assert!(harness.query_by_label("Emoji presentation").is_some());

        harness.get_by_label("Monochrome").click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::SetEmojiPresentation(
                EmojiPresentationPreference::Monochrome
            ))
        ));
    }

    #[test]
    fn settings_exposes_scrollback_limit_for_future_sessions() {
        let mut harness = settings_harness();
        harness.run();

        assert!(harness.query_by_label("Scrollback limit").is_some());
        harness.get_by_label("16 MiB").click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::SetScrollbackLimit(
                ScrollbackLimitPreference::MiB16
            ))
        ));
    }

    #[test]
    fn settings_panel_does_not_overlap_a_visible_bottom_status_bar() {
        // Regression test: the card-based Settings redesign is taller than
        // the old flat button list, so without a status-bar-aware height
        // clamp (mirroring the SSH profile editor panel's), its content
        // could paint into - or past - the bottom status bar strip. The
        // window here is tall enough for all of Settings' content to fit
        // without needing to scroll, so every widget's rect should stay
        // above the status bar; a shorter window would legitimately push
        // some content below the fold (inside the scrollable area) without
        // that being a bug, which a naive per-widget position check can't
        // distinguish from actually overlapping the status bar.
        let mut harness = Harness::builder()
            .with_size(egui::vec2(520.0, 1420.0))
            .build_ui_state(
                |ui, state: &mut SettingsHarnessState| {
                    egui::Panel::bottom("status_bar")
                        .resizable(false)
                        .show_separator_line(false)
                        .show(ui, |ui| {
                            ui.set_min_height(24.0);
                            ui.set_max_height(24.0);
                        });
                    if let Some(command) = show_settings(
                        ui,
                        SettingsViewModel {
                            chip_layout: ChipLayout::Wrap,
                            status_bar_visible: true,
                            show_session_details: true,
                            confirm_session_close: true,
                            restore_workspace: false,
                            terminal_font: TerminalFontPreference::JetBrainsMono,
                            terminal_ligatures: false,
                            emoji_presentation: EmojiPresentationPreference::Color,
                            scroll_speed: ScrollSpeedPreference::Normal,
                            scrollback_limit: ScrollbackLimitPreference::MiB64,
                            quick_switch_overlay: false,
                            compact_launcher_grid: false,
                            pulse_new_output_dot: false,
                            show_resumable_sessions: false,
                            default_sftp_local_directory: None,
                            sftp_pane_order: SftpPaneOrderPreference::LocalLeft,
                        },
                        "Cmd+Shift+P",
                        "Cmd+Shift+S",
                    ) {
                        state.command = Some(command);
                    }
                },
                SettingsHarnessState { command: None },
            );
        harness.run();
        harness.run();

        let status_bar_top =
            egui::containers::panel::PanelState::load(&harness.ctx, egui::Id::new("status_bar"))
                .expect("status bar panel state should be recorded")
                .outer_rect
                .top();
        let command_palette_rect = harness.get_by_label("Command palette").rect();
        assert!(
            command_palette_rect.max.y <= status_bar_top,
            "Settings content must stay above the status bar rather than overlapping it"
        );
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
        harness.get_by_label("Connect").scroll_to_me();
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
    fn sftp_launcher_defaults_to_quick_connect_not_the_advanced_form() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SFTP — Transfer files over SSH")
            .click();
        harness.run();

        assert!(
            harness.query_by_label("user@host").is_some(),
            "a freshly opened SFTP launcher must show the Quick Connect field"
        );
        assert!(
            harness.query_by_label("Username").is_none(),
            "the advanced form must stay hidden until 'Show advanced settings' is checked"
        );
    }

    #[test]
    fn quick_connect_with_no_password_starts_an_interactive_sftp_session() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SFTP — Transfer files over SSH")
            .click();
        harness.run();

        harness
            .get_by_label("user@host")
            .type_text("fes@10.1.2.3:2222");
        harness.run();
        harness.get_by_label("Connect").click();
        harness.run();

        let Some(AppCommand::StartSftpSession {
            profile,
            authentication: SshAuthentication::Interactive,
            ..
        }) = harness.state().command.as_ref()
        else {
            panic!("a valid SFTP quick-connect destination must start an interactive SFTP session");
        };
        assert_eq!(profile.username(), "fes");
        assert_eq!(profile.identity().host(), "10.1.2.3");
        assert_eq!(profile.identity().port(), 2222);
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
    fn quick_connect_can_attach_to_a_named_tmux_session() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host")
            .click();
        harness.run();

        harness.get_by_label("Use a durable remote session").click();
        harness.run();
        assert!(harness.query_by_label("tmux").is_some());
        assert!(harness.query_by_label("GNU screen").is_some());
        assert!(harness.query_by_label("Session name").is_some());
        assert!(harness
            .query_by_label("Automatically resume after connection loss")
            .is_some());

        let mut form = SshLauncherForm {
            quick_connect: "fes@10.1.2.3".to_owned(),
            durable_session: DurableSessionDraft {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let AppCommand::StartSshSession { options, .. } = form
            .submit_quick_connect()
            .expect("Quick Connect must return an SSH command")
        else {
            unreachable!("the SSH launcher only returns SSH commands");
        };
        assert_eq!(
            options.strategy(),
            SessionStrategy::Persistent {
                provider: festerm_ssh::PersistenceProvider::Tmux,
                session_name: festerm_ssh::PersistentSessionName::new("main").unwrap(),
            }
        );
    }

    #[test]
    fn quick_connect_rejects_an_invalid_durable_session_name_before_launch() {
        let mut form = SshLauncherForm {
            quick_connect: "fes@10.1.2.3".to_owned(),
            durable_session: DurableSessionDraft {
                enabled: true,
                session_name: "not valid".to_owned(),
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(
            form.submit_quick_connect().unwrap_err(),
            "a persistent session name may only contain ASCII letters, digits, '-', '_', or '.', and must be 1-64 bytes"
        );
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
    fn resumable_sessions_appear_before_saved_profiles_and_dispatch_resume() {
        // Feature request #70: unattached, locally running festerm-sessiond
        // sessions should surface as one-click "Resume" entries, listed
        // after the fixed "new session" entries but before saved profiles.
        let profiles = vec![
            Profile::local("development", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let resumable_sessions = vec![festerm_sessiond::UnattachedSession {
            name: "orphaned".to_owned(),
            shell: "/bin/bash".to_owned(),
            arguments: Vec::new(),
            working_directory: Some("/tmp".to_owned()),
            created_at_unix_ms: 0,
        }];
        let mut harness =
            harness_with_profiles_grid_and_resumable(profiles, false, 520.0, resumable_sessions);
        harness.run();

        let ssh_top = harness
            .get_by_label("SSH — Connect to a remote host")
            .rect()
            .top();
        let resume_node = harness.get_by_label("Resume: orphaned — /bin/bash · /tmp");
        let resume_top = resume_node.rect().top();
        let profile_top = harness
            .get_by_label("development — Saved local profile")
            .rect()
            .top();

        assert!(
            ssh_top < resume_top && resume_top < profile_top,
            "expected New Session entries, then Resume entries, then saved profiles"
        );

        resume_node.click();
        harness.run();

        assert!(matches!(
            &harness.state().command,
            Some(AppCommand::ResumeUnattachedSession { name }) if name == "orphaned"
        ));
    }

    #[test]
    fn compact_launcher_grid_off_keeps_saved_profiles_single_column() {
        // Regression test for feature request #64: with the preference off
        // (the default), saved profiles should stack vertically one per
        // row even in a window wide enough to fit multiple grid columns.
        let profiles = vec![
            Profile::local("alpha", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
            Profile::local("beta", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let mut harness = harness_with_profiles_and_grid(profiles, false, 900.0);
        harness.run();

        let alpha_rect = harness.get_by_label("alpha — Saved local profile").rect();
        let beta_rect = harness.get_by_label("beta — Saved local profile").rect();

        assert!(
            alpha_rect.top() < beta_rect.top()
                && (alpha_rect.left() - beta_rect.left()).abs() < 1.0,
            "expected alpha above beta in the same column when the grid preference is off"
        );
    }

    #[test]
    fn compact_launcher_grid_on_lays_out_saved_profiles_side_by_side_when_wide_enough() {
        // Regression test for feature request #64: with the preference on
        // and a window wide enough for multiple columns, saved profiles
        // should lay out side by side instead of one per row.
        let profiles = vec![
            Profile::local("alpha", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
            Profile::local("beta", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let mut harness = harness_with_profiles_and_grid(profiles, true, 900.0);
        harness.run();

        let alpha_rect = harness.get_by_label("alpha — Saved local profile").rect();
        let beta_rect = harness.get_by_label("beta — Saved local profile").rect();

        assert!(
            (alpha_rect.top() - beta_rect.top()).abs() < 1.0
                && alpha_rect.left() < beta_rect.left(),
            "expected alpha and beta side by side in the same row when the grid preference is on \
             and the window is wide enough for multiple columns"
        );
    }

    #[test]
    fn compact_launcher_grid_on_falls_back_to_single_column_when_narrow() {
        // Regression test for feature request #64: even with the
        // preference on, a narrow window that can only fit one card-width
        // column should fall back to the single-column layout.
        let profiles = vec![
            Profile::local("alpha", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
            Profile::local("beta", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let mut harness = harness_with_profiles_and_grid(profiles, true, 360.0);
        harness.run();

        let alpha_rect = harness.get_by_label("alpha — Saved local profile").rect();
        let beta_rect = harness.get_by_label("beta — Saved local profile").rect();

        assert!(
            alpha_rect.top() < beta_rect.top()
                && (alpha_rect.left() - beta_rect.left()).abs() < 1.0,
            "expected a single-column fallback when the window is too narrow for a second column"
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
    fn saved_sftp_profile_uses_the_ssh_profile_identity_as_its_primary_label() {
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
            .query_by_label("production — Saved SSH profile · SFTP · deploy@ssh.example.test:2200")
            .is_some());
        assert!(
            harness
                .query_by_label("production (SFTP) — Saved SFTP destination · deploy@ssh.example.test:2200")
                .is_none(),
            "saved SFTP launchers must reuse the SSH profile identity instead of inventing a second label vocabulary"
        );
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
    fn clicking_a_saved_profiles_edit_icon_opens_its_editor_instead_of_launching() {
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
            .get_by_label("Edit production (Saved SSH profile · deploy@ssh.example.test:2200)")
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::OpenProfileEditor { ref identifier })
                if identifier == "production"
        ));
    }

    #[test]
    fn launcher_list_stays_above_a_visible_status_bar_without_clipping_saved_profiles() {
        // Regression test: the item list previously had no height bound at
        // all, so saved profiles (and their edit icons) could silently run
        // into -- or past -- the bottom status bar instead of the list
        // staying above it (or scrolling, once there's too much content to
        // fit).
        let profiles: Vec<Profile> = (0..1)
            .map(|i| {
                Profile::ssh(
                    format!("host-{i}"),
                    "ssh.example.test",
                    22,
                    "deploy",
                    "xterm-256color",
                    100,
                    40,
                )
                .expect("test profile is valid")
            })
            .collect();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(520.0, 500.0))
            .build_ui_state(
                |ui, state: &mut LauncherHarnessState| {
                    egui::Panel::bottom("status_bar")
                        .resizable(false)
                        .show_separator_line(false)
                        .show(ui, |ui| {
                            ui.set_min_height(24.0);
                            ui.set_max_height(24.0);
                        });
                    if let Some(command) =
                        show_launcher(ui, state.tab_id, &state.profiles, true, None, false, &[])
                    {
                        state.command = Some(command);
                    }
                },
                LauncherHarnessState {
                    tab_id: AppState::for_test().active(),
                    profiles,
                    command: None,
                },
            );
        harness.run();
        harness.run();

        let status_bar_top =
            egui::containers::panel::PanelState::load(&harness.ctx, egui::Id::new("status_bar"))
                .expect("status bar panel state should be recorded")
                .outer_rect
                .top();
        let last_edit_rect = harness
            .get_by_label("Edit host-0 (Saved SSH profile · deploy@ssh.example.test:22)")
            .rect();
        assert!(
            last_edit_rect.max.y <= status_bar_top,
            "saved profile edit icons must stay above the status bar rather than overlapping it"
        );

        harness
            .get_by_label("Edit host-0 (Saved SSH profile · deploy@ssh.example.test:22)")
            .click();
        harness.run();
        assert!(matches!(
            harness.state().command,
            Some(AppCommand::OpenProfileEditor { ref identifier })
                if identifier == "host-0"
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
            form.session_options().unwrap().strategy(),
            SessionStrategy::Persistent {
                provider: festerm_ssh::PersistenceProvider::Tmux,
                session_name: festerm_ssh::PersistentSessionName::new("build").unwrap(),
            }
        );
        assert_eq!(form.session_options().unwrap().reconnect_policy(), None);
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
            !form.durable_session.automatic_recovery,
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
        form.durable_session.automatic_recovery = true;

        assert!(form.session_options().unwrap().reconnect_policy().is_some());
    }

    #[test]
    fn opting_into_automatic_recovery_without_persistence_has_no_effect() {
        let form = SshLauncherForm {
            host: "example.invalid".to_owned(),
            username: "test-user".to_owned(),
            durable_session: DurableSessionDraft {
                automatic_recovery: true,
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(
            form.session_options().unwrap().strategy(),
            SessionStrategy::PlainShell
        );
        assert_eq!(form.session_options().unwrap().reconnect_policy(), None);
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
            form.session_options().unwrap().strategy(),
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
    fn saved_local_profile_defaults_to_named_native_persistence() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        harness.get_by_label("New Local Profile").click();
        harness.run();
        harness.get_by_label("Name").focus();
        harness.get_by_label("Name").type_text("durable-local");
        harness.run();
        harness.get_by_label("Use a durable local session").click();
        harness.run();
        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfile {
            profile: Profile::Local(local),
        }) = harness.state().command.as_ref()
        else {
            panic!("saving a durable local profile must return a SaveProfile command");
        };
        let persistence = local
            .persistence()
            .expect("saved local profile must retain explicit persistence");
        assert_eq!(
            persistence.provider(),
            PersistenceProviderKind::FestermSessiond
        );
        assert_eq!(persistence.session_name(), "durable-local");
    }

    #[test]
    fn sanitize_session_name_from_profile_name_normalizes_case_and_separators() {
        assert_eq!(
            sanitize_session_name_from_profile_name("My Prod Server!!"),
            "my-prod-server"
        );
        assert_eq!(
            sanitize_session_name_from_profile_name("  leading and trailing  "),
            "leading-and-trailing"
        );
        assert_eq!(
            sanitize_session_name_from_profile_name("already-valid_name.1"),
            "already-valid_name.1"
        );
        assert_eq!(sanitize_session_name_from_profile_name("***"), "");
        assert_eq!(
            sanitize_session_name_from_profile_name(&"x".repeat(100)),
            "x".repeat(64)
        );
    }

    #[test]
    fn new_local_profile_session_name_tracks_the_profile_name_until_manually_edited() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        harness.get_by_label("New Local Profile").click();
        harness.run();
        harness.get_by_label("Use a durable local session").click();
        harness.run();
        harness.get_by_label("Name").focus();
        harness.get_by_label("Name").type_text("Build Box");
        harness.run();

        assert_eq!(
            harness.get_by_label("Session name").value().as_deref(),
            Some("build-box")
        );

        // Once the user edits the session name directly, further profile
        // name edits must not clobber their choice.
        harness.get_by_label("Session name").focus();
        harness.get_by_label("Session name").type_text("-pinned");
        harness.run();
        harness.get_by_label("Name").focus();
        harness.get_by_label("Name").type_text(" Two");
        harness.run();

        assert_eq!(
            harness.get_by_label("Session name").value().as_deref(),
            Some("build-box-pinned")
        );
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
    fn local_profile_executable_field_survives_a_real_pointer_click_on_a_suggestion() {
        // Unlike the sibling test above, this uses a raw `.click()` (a
        // synthetic pointer press/release), matching what a real mouse click
        // does: it first defocuses the text field as a "click elsewhere",
        // which used to hide the dropdown out from under the click before
        // the suggestion ever received it.
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
        harness
            .get_by_role_and_label(accesskit::Role::Button, &expected_label)
            .click();
        harness.run();

        assert_eq!(
            harness.get_by_label("Executable").value().as_deref(),
            Some(expected_label.as_str()),
            "a raw pointer click on a PATH suggestion must fill the field with its absolute path"
        );
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

    #[test]
    fn ssh_profile_editor_saves_named_tmux_persistence() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        harness.get_by_label("New SSH Profile").click();
        harness.run();
        for (label, value) in [
            ("Name", "build-host"),
            ("Username", "builder"),
            ("Host", "ssh.example.test"),
        ] {
            harness.get_by_label(label).click();
            harness.run();
            harness.get_by_label(label).type_text(value);
            harness.run();
        }
        harness.get_by_label("Use a durable remote session").click();
        harness.run();
        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfile { profile }) = harness.state().command.as_ref() else {
            panic!("saving the SSH profile must return a SaveProfile command");
        };
        let persistence = profile
            .persistence()
            .expect("the profile must retain durable-session settings");
        assert_eq!(persistence.provider(), PersistenceProviderKind::Tmux);
        assert_eq!(persistence.session_name(), "build-host");
    }

    #[test]
    fn editing_durable_session_settings_preserves_the_stored_credential_reference() {
        let reference = festerm_secret_store::SecretReference::generate();
        let expected_reference = reference.to_persisted_string();
        let profile = Profile::ssh(
            "prod",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_credential_reference_kind(reference, CredentialKind::PrivateKey)
        .unwrap();
        let mut harness =
            profiles_harness(festerm_config::Configuration::new(vec![profile]).unwrap());
        harness.run();

        harness.get_by_label("Edit").click();
        harness.run();
        harness.get_by_label("Use a durable remote session").click();
        harness.run();
        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfile { profile }) = harness.state().command.as_ref() else {
            panic!("editing the SSH profile must return a SaveProfile command");
        };
        assert_eq!(
            profile
                .credential_reference()
                .expect("the stored credential reference must survive the edit")
                .to_persisted_string(),
            expected_reference
        );
        assert_eq!(
            profile
                .as_ssh()
                .expect("profile remains SSH")
                .credential_kind(),
            CredentialKind::PrivateKey
        );
    }

    #[test]
    fn ssh_profile_editor_adds_a_port_forward_and_saves_it_with_the_profile() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        harness.get_by_label("New SSH Profile").click();
        harness.run();
        for (label, value) in [
            ("Name", "build-host"),
            ("Username", "builder"),
            ("Host", "ssh.example.test"),
        ] {
            harness.get_by_label(label).focus();
            harness.get_by_label(label).type_text(value);
            harness.run();
        }
        harness.get_by_label("Add port forward").click();
        harness.run();

        assert_eq!(
            harness.get_by_label("Bind host").value().as_deref(),
            Some("127.0.0.1")
        );

        for (label, value) in [
            ("Bind port", "8080"),
            ("Destination host", "app.internal"),
            ("Destination port", "80"),
        ] {
            harness.get_by_label(label).focus();
            harness.get_by_label(label).type_text(value);
            harness.run();
        }

        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfile { profile }) = harness.state().command.as_ref() else {
            panic!("saving the SSH profile must return a SaveProfile command");
        };
        let ssh = profile.as_ssh().expect("saved profile remains SSH");
        assert_eq!(ssh.port_forwards().len(), 1);
        let forward = &ssh.port_forwards()[0];
        assert_eq!(forward.direction(), SshPortForwardDirection::Local);
        assert_eq!(forward.bind_host(), "127.0.0.1");
        assert_eq!(forward.bind_port(), 8080);
        assert_eq!(forward.destination_host(), "app.internal");
        assert_eq!(forward.destination_port(), 80);
    }

    #[test]
    fn ssh_profile_editor_can_remove_a_saved_port_forward_before_saving() {
        let profile = Profile::Ssh(
            Profile::ssh(
                "prod",
                "ssh.example.test",
                22,
                "deploy",
                "xterm-256color",
                80,
                24,
            )
            .unwrap()
            .as_ssh()
            .unwrap()
            .clone()
            .with_port_forwards(vec![SshPortForwardConfiguration::new(
                SshPortForwardDirection::Local,
                "127.0.0.1",
                8080,
                "app.internal",
                80,
            )
            .unwrap()])
            .unwrap(),
        );
        let mut harness =
            profiles_harness(festerm_config::Configuration::new(vec![profile]).unwrap());
        harness.run();

        harness.get_by_label("Edit").click();
        harness.run();
        assert!(harness.query_by_label("Remove forward 1").is_some());

        harness.get_by_label("Remove forward 1").click();
        harness.run();
        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfile { profile }) = harness.state().command.as_ref() else {
            panic!("saving the SSH profile must return a SaveProfile command");
        };
        assert!(profile
            .as_ssh()
            .expect("saved profile remains SSH")
            .port_forwards()
            .is_empty());
    }

    #[test]
    fn ssh_profile_editor_rejects_an_invalid_port_forward_without_saving() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        harness.get_by_label("New SSH Profile").click();
        harness.run();
        for (label, value) in [
            ("Name", "build-host"),
            ("Username", "builder"),
            ("Host", "ssh.example.test"),
        ] {
            harness.get_by_label(label).focus();
            harness.get_by_label(label).type_text(value);
            harness.run();
        }
        harness.get_by_label("Add port forward").click();
        harness.run();
        for (label, value) in [
            ("Bind port", "0"),
            ("Destination host", "app.internal"),
            ("Destination port", "80"),
        ] {
            harness.get_by_label(label).focus();
            harness.get_by_label(label).type_text(value);
            harness.run();
        }

        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        assert!(harness.state().command.is_none());
        assert!(harness
            .query_by_label(
                "SSH port forwards must use non-empty, safe bind and destination hosts with nonzero ports"
            )
            .is_some());
    }
}
