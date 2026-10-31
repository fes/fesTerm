//! New-profile editor, host verification and password sheets.
//!
//! Layouts follow mock-ups 10-12; behaviour hooks arrive through view-model
//! structs and returned actions so callers own all connection side effects.

use super::model::{Capabilities, DevTunnelChoices, HostDecision, HostVerification, ProfileKind};
use super::profiles::{SftpSpec, TunnelSpec};
use super::theme::{color, radius, space, type_scale};
use super::widgets::{
    bottom_sheet, button, button_sized, caption_label, card, footnote, labelled, list_row,
    paint_fit, segmented, sheet_title, switch_row, text_field, ButtonKind, RowSpec,
};
use eframe::egui::{self, Align2, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui};
use festerm_mobile_profiles::{RemoteSession, SshAuth, SshProfile};
use festerm_ui_egui::icon::{self, Icon};
use zeroize::Zeroize;

/// How the editor's profile authenticates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AuthMode {
    /// Ask for the password each time; nothing is saved.
    #[default]
    Ask,
    Password,
    Key,
}

impl AuthMode {
    pub const ALL: [AuthMode; 3] = [AuthMode::Ask, AuthMode::Password, AuthMode::Key];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0)
    }

    pub fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or_default()
    }
}

#[derive(Debug, Default)]
pub struct EditorState {
    pub open: bool,
    /// The saved profile being edited; `None` creates a new one.
    pub editing: Option<String>,
    pub kind_index: usize,
    pub name: String,
    pub host: String,
    pub port: String,
    pub username: String,
    pub auth_mode: AuthMode,
    /// Typed password; wiped as soon as it is stored.
    pub password: String,
    /// Credential currently chosen (stored but possibly not yet saved).
    pub auth: Option<SshAuth>,
    /// Credential the saved profile already uses.
    pub original_auth: Option<SshAuth>,
    pub auth_sheet: bool,
    /// Key text waiting for its passphrase; wiped after use.
    pub pending_key: String,
    pub key_passphrase: String,
    pub needs_passphrase: bool,
    pub auth_notice: Option<String>,
    pub durable_credentials: bool,
    pub session_mode: usize,
    pub session_name: String,
    /// SFTP profiles: the folder the browser opens first.
    pub start_dir: String,
    pub error: Option<String>,
    pub confirm_delete: bool,
    pub tunnel: Option<DevTunnelChoices>,
    /// Dev Tunnel profiles: the chosen (or typed) tunnel name.
    pub tunnel_name: String,
    /// Dev Tunnel profiles: the signed-in account the profile is bound to.
    #[cfg_attr(not(feature = "live-ssh"), allow(dead_code))]
    pub tunnel_account: Option<String>,
    pub take_over: bool,
}

/// The remote session name used when the field is left empty.
const DEFAULT_SESSION_NAME: &str = "work";

impl EditorState {
    pub fn open_new(&mut self) {
        self.wipe_secrets();
        *self = Self {
            open: true,
            port: "22".to_owned(),
            take_over: true,
            durable_credentials: self.durable_credentials,
            ..Self::default()
        };
    }

    pub fn open_edit(&mut self, id: &str, name: &str, ssh: &SshProfile) {
        self.wipe_secrets();
        let (session_mode, session_name) = match &ssh.remote_session {
            RemoteSession::Plain => (0, String::new()),
            RemoteSession::Tmux { name } => (1, name.clone()),
            RemoteSession::Screen { name } => (2, name.clone()),
        };
        *self = Self {
            open: true,
            editing: Some(id.to_owned()),
            name: name.to_owned(),
            host: ssh.host.clone(),
            port: ssh.port.to_string(),
            username: ssh.username.clone(),
            auth_mode: match &ssh.auth {
                SshAuth::Ask => AuthMode::Ask,
                SshAuth::Password { .. } => AuthMode::Password,
                SshAuth::Key { .. } => AuthMode::Key,
            },
            auth: Some(ssh.auth.clone()),
            original_auth: Some(ssh.auth.clone()),
            durable_credentials: self.durable_credentials,
            session_mode,
            session_name,
            ..Self::default()
        };
    }

    pub fn open_edit_tunnel(&mut self, id: &str, spec: &TunnelSpec, caps: Capabilities) {
        self.wipe_secrets();
        let kind_index = Self::kinds(caps)
            .iter()
            .position(|kind| *kind == ProfileKind::DevTunnel)
            .unwrap_or(0);
        *self = Self {
            open: true,
            editing: Some(id.to_owned()),
            kind_index,
            name: spec.name.clone(),
            tunnel_name: spec.tunnel.clone(),
            tunnel_account: spec.account.clone(),
            session_name: spec.session.clone().unwrap_or_default(),
            take_over: spec.take_over,
            durable_credentials: self.durable_credentials,
            ..Self::default()
        };
    }

    pub fn open_edit_sftp(&mut self, id: &str, spec: &SftpSpec, caps: Capabilities) {
        self.wipe_secrets();
        let kind_index = Self::kinds(caps)
            .iter()
            .position(|kind| *kind == ProfileKind::Sftp)
            .unwrap_or(0);
        *self = Self {
            open: true,
            editing: Some(id.to_owned()),
            kind_index,
            name: spec.name.clone(),
            host: spec.host.clone(),
            port: spec.port.to_string(),
            username: spec.username.clone(),
            auth_mode: match &spec.auth {
                SshAuth::Ask => AuthMode::Ask,
                SshAuth::Password { .. } => AuthMode::Password,
                SshAuth::Key { .. } => AuthMode::Key,
            },
            auth: Some(spec.auth.clone()),
            original_auth: Some(spec.auth.clone()),
            durable_credentials: self.durable_credentials,
            start_dir: spec.start_dir.clone().unwrap_or_default(),
            ..Self::default()
        };
    }

    pub fn close(&mut self) {
        self.wipe_secrets();
        self.open = false;
        self.auth_sheet = false;
        self.error = None;
        self.confirm_delete = false;
    }

    pub fn wipe_secrets(&mut self) {
        self.password.zeroize();
        self.pending_key.zeroize();
        self.key_passphrase.zeroize();
        self.needs_passphrase = false;
    }

    /// A credential stored for this edit that no saved profile uses yet.
    pub fn unsaved_auth(&self) -> Option<&SshAuth> {
        match (&self.auth, &self.original_auth) {
            (Some(current), original) if Some(current) != original.as_ref() => Some(current),
            _ => None,
        }
    }

    pub fn kinds(caps: Capabilities) -> Vec<ProfileKind> {
        let mut kinds = vec![ProfileKind::Ssh];
        if caps.dev_tunnel_profiles {
            kinds.push(ProfileKind::DevTunnel);
        }
        if caps.sftp_profiles {
            kinds.push(ProfileKind::Sftp);
        }
        kinds
    }

    pub fn kind(&self, caps: Capabilities) -> ProfileKind {
        Self::kinds(caps)
            .get(self.kind_index)
            .copied()
            .unwrap_or(ProfileKind::Ssh)
    }

    pub fn remote_session(&self) -> RemoteSession {
        let name = match self.session_name.trim() {
            "" => DEFAULT_SESSION_NAME.to_owned(),
            name => name.to_owned(),
        };
        match self.session_mode {
            1 => RemoteSession::Tmux { name },
            2 => RemoteSession::Screen { name },
            _ => RemoteSession::Plain,
        }
    }

    /// Short description of the chosen credential for the Authentication row.
    pub fn auth_summary(&self) -> (&'static str, String) {
        match (self.auth_mode, &self.auth) {
            (AuthMode::Key, Some(SshAuth::Key { key, .. })) => (
                "SSH key",
                format!(
                    "{} · {}",
                    key.algorithm,
                    short_fingerprint(&key.fingerprint)
                ),
            ),
            (AuthMode::Key, _) => ("SSH key", "Choose a key".to_owned()),
            (AuthMode::Password, _) if !self.password.is_empty() => {
                ("Password", "Will be saved".to_owned())
            }
            (AuthMode::Password, Some(SshAuth::Password { .. })) => {
                ("Password", "Saved".to_owned())
            }
            (AuthMode::Password, _) => ("Password", "Set a password".to_owned()),
            (AuthMode::Ask, _) => ("Ask each time", "Password prompt".to_owned()),
        }
    }
}

fn short_fingerprint(fingerprint: &str) -> String {
    let body = fingerprint.strip_prefix("SHA256:").unwrap_or(fingerprint);
    format!("SHA256:{}…", body.chars().take(6).collect::<String>())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorAction {
    None,
    Cancel,
    SaveOnly,
    SaveAndConnect,
    OpenAuthentication,
    Delete,
    SignIn,
    RefreshTunnels,
    PickTunnel(usize),
    SignOut,
}

fn info_row(
    ui: &mut Ui,
    title: &str,
    value: &str,
    trailing_chevron: bool,
    glyph: Option<Icon>,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 56.0),
        if trailing_chevron {
            Sense::CLICK
        } else {
            Sense::hover()
        },
    );
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(radius::FIELD), color::SURFACE);
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius::FIELD),
        Stroke::new(1.0, color::BORDER),
        StrokeKind::Inside,
    );
    let mut left = rect.left() + 14.0;
    if let Some(glyph) = glyph {
        icon::paint(
            painter,
            glyph,
            Rect::from_center_size(
                egui::pos2(left + 10.0, rect.center().y),
                egui::vec2(20.0, 20.0),
            ),
            color::TEXT_MUTED,
        );
        left += 32.0;
    }
    paint_fit(
        painter,
        egui::pos2(left, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        type_scale::font(type_scale::BODY),
        color::TEXT,
        rect.width() * 0.5,
    );
    let right = if trailing_chevron {
        rect.right() - 36.0
    } else {
        rect.right() - 14.0
    };
    paint_fit(
        painter,
        egui::pos2(right, rect.center().y),
        Align2::RIGHT_CENTER,
        value,
        type_scale::font(type_scale::CAPTION + 0.5),
        color::TEXT_MUTED,
        rect.width() * 0.4,
    );
    if trailing_chevron {
        super::widgets::paint_chevron(
            painter,
            egui::pos2(rect.right() - 22.0, rect.center().y),
            5.0,
            color::TEXT_MUTED,
        );
    }
    labelled(response, title, trailing_chevron)
}

/// Full-screen editor. Host, port and username edits are owned by `state`;
/// the caller decides how Save and Connect are carried out.
pub fn show_editor(
    ui: &mut Ui,
    state: &mut EditorState,
    caps: Capabilities,
    can_connect: bool,
) -> EditorAction {
    let mut action = EditorAction::None;
    let kinds = EditorState::kinds(caps);
    let kind = state.kind(caps);
    egui::Panel::top("editor-top")
        .frame(
            egui::Frame::new()
                .fill(color::BG)
                .inner_margin(egui::Margin::symmetric(16, 10)),
        )
        .show_separator_line(false)
        .show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), space::TOUCH),
                Sense::hover(),
            );
            let cancel = ui.interact(
                Rect::from_min_size(rect.min, egui::vec2(80.0, rect.height())),
                ui.id().with("editor-cancel"),
                Sense::CLICK,
            );
            paint_fit(
                ui.painter(),
                egui::pos2(rect.left(), rect.center().y),
                Align2::LEFT_CENTER,
                "Cancel",
                type_scale::font(type_scale::BODY),
                color::ACCENT_TEXT,
                80.0,
            );
            if labelled(cancel, "Cancel", true).clicked() {
                action = EditorAction::Cancel;
            }
            paint_fit(
                ui.painter(),
                rect.center(),
                Align2::CENTER_CENTER,
                if state.editing.is_some() {
                    "Edit profile"
                } else {
                    "New profile"
                },
                type_scale::font(type_scale::NAV_TITLE),
                color::TEXT,
                rect.width() - 180.0,
            );
        });
    let connect_label = match kind {
        ProfileKind::DevTunnel => "Save & Attach",
        ProfileKind::Sftp => "Save & Open",
        ProfileKind::Ssh => "Save & Connect",
    };
    egui::Panel::bottom("editor-bottom")
        .frame(
            egui::Frame::new()
                .fill(color::BAR)
                .stroke(Stroke::new(1.0, color::DIVIDER))
                .inner_margin(egui::Margin::symmetric(16, 12)),
        )
        .show_separator_line(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let width = ui.available_width();
                let gap = ui.spacing().item_spacing.x;
                let secondary = (width - gap) * 0.38;
                if button_sized(
                    ui,
                    ButtonKind::Secondary,
                    "Save only",
                    true,
                    egui::vec2(secondary, space::BUTTON_HEIGHT),
                )
                .clicked()
                {
                    action = EditorAction::SaveOnly;
                }
                if button_sized(
                    ui,
                    ButtonKind::Primary,
                    connect_label,
                    can_connect,
                    egui::vec2(width - secondary - gap, space::BUTTON_HEIGHT),
                )
                .clicked()
                {
                    action = EditorAction::SaveAndConnect;
                }
            });
        });
    egui::ScrollArea::vertical()
        .id_salt("editor-body")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = space::MD;
            ui.add_space(space::SM);
            let inner_width = ui.available_width() - 32.0;
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.set_width(inner_width);
                    ui.spacing_mut().item_spacing.y = space::MD;
                    if kinds.len() > 1 && state.editing.is_none() {
                        let items: Vec<(Option<Icon>, &str)> = kinds
                            .iter()
                            .map(|kind| (Some(kind.glyph()), kind.label()))
                            .collect();
                        segmented(ui, &items, &mut state.kind_index, true);
                    }
                    text_field(ui, "Profile name", &mut state.name, "prod-web", inner_width);
                    match kind {
                        ProfileKind::DevTunnel => dev_tunnel_fields(ui, state, &mut action),
                        _ => {
                            if ssh_fields(ui, state, caps, inner_width, kind) {
                                action = EditorAction::OpenAuthentication;
                            }
                        }
                    }
                    if let Some(error) = &state.error {
                        ui.label(egui::RichText::new(error).color(color::DANGER));
                    }
                    if state.editing.is_some() {
                        let label = if state.confirm_delete {
                            "Tap again to delete this profile"
                        } else {
                            "Delete profile"
                        };
                        if button(ui, ButtonKind::Destructive, label, true).clicked() {
                            if state.confirm_delete {
                                action = EditorAction::Delete;
                            } else {
                                state.confirm_delete = true;
                            }
                        }
                    }
                    ui.add_space(space::LG);
                });
            });
        });
    action
}

/// Returns whether the Authentication row was tapped.
fn ssh_fields(
    ui: &mut Ui,
    state: &mut EditorState,
    caps: Capabilities,
    width: f32,
    kind: ProfileKind,
) -> bool {
    let gap = ui.spacing().item_spacing.x;
    let port_width = 84.0;
    let mut open_auth = false;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        text_field(
            ui,
            "Host",
            &mut state.host,
            "example.net",
            width - port_width - gap,
        );
        text_field(ui, "Port", &mut state.port, "22", port_width);
    });
    text_field(ui, "Username", &mut state.username, "dev", width);
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        caption_label(ui, "Authentication");
        let (title, value) = state.auth_summary();
        open_auth = info_row(ui, title, &value, true, None).clicked();
        footnote(
            ui,
            if state.durable_credentials {
                "Credentials are kept in Keychain, not in profiles."
            } else {
                "Keychain is unavailable: credentials last only until fesTerm closes."
            },
        );
    });
    if kind == ProfileKind::Sftp {
        text_field(
            ui,
            "Start folder (optional)",
            &mut state.start_dir,
            "/srv/app or ~/projects",
            width,
        );
        footnote(
            ui,
            "Opens a file browser on this host over SFTP. Leave empty to start in your home folder.",
        );
    }
    if caps.remote_session_choice && kind == ProfileKind::Ssh {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            caption_label(ui, "Remote session");
            segmented(
                ui,
                &[(None, "Plain shell"), (None, "tmux"), (None, "Screen")],
                &mut state.session_mode,
                false,
            );
        });
        if state.session_mode != 0 {
            text_field(
                ui,
                "Session name",
                &mut state.session_name,
                DEFAULT_SESSION_NAME,
                width,
            );
            footnote(
                ui,
                "Attach to this remote session, or create it if missing. Reconnecting reattaches it.",
            );
        } else {
            footnote(
                ui,
                "A plain shell ends with the connection; reconnecting starts a new shell.",
            );
        }
    }
    open_auth
}

/// What the Authentication sheet asks the app to do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthAction {
    None,
    Done,
    PasteKey,
    ImportPending,
    GenerateKey,
    CopyPublicKey,
}

/// Bottom sheet for choosing how a profile authenticates.
pub fn authentication_sheet(
    ctx: &egui::Context,
    bounds: Rect,
    state: &mut EditorState,
    clipboard_available: bool,
) -> AuthAction {
    let mut action = AuthAction::None;
    let outcome = bottom_sheet(ctx, "authentication-method", bounds, |ui| {
        if sheet_title(ui, "Authentication", Some("Done")) {
            action = AuthAction::Done;
        }
        let mut index = state.auth_mode.index();
        segmented(
            ui,
            &[
                (None, "Ask each time"),
                (None, "Password"),
                (None, "SSH key"),
            ],
            &mut index,
            false,
        );
        state.auth_mode = AuthMode::from_index(index);
        match state.auth_mode {
            AuthMode::Ask => footnote(
                ui,
                "fesTerm asks for the password every time you connect. Nothing is saved.",
            ),
            AuthMode::Password => {
                caption_label(ui, "Password");
                ui.add_sized(
                    [ui.available_width(), space::FIELD_HEIGHT],
                    egui::TextEdit::singleline(&mut state.password)
                        .password(true)
                        .sensitive(true)
                        .hint_text(if matches!(state.auth, Some(SshAuth::Password { .. })) {
                            "Saved password (leave empty to keep)"
                        } else {
                            "Password"
                        })
                        .margin(egui::Margin::symmetric(14, 14))
                        .char_limit(1024),
                );
                footnote(
                    ui,
                    if state.durable_credentials {
                        "Saved in Keychain on this iPhone, never in the profile."
                    } else {
                        "Keychain is unavailable; this lasts only until fesTerm closes."
                    },
                );
            }
            AuthMode::Key => key_section(ui, state, clipboard_available, &mut action),
        }
        if let Some(notice) = &state.auth_notice {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(notice)
                        .size(type_scale::CAPTION + 0.5)
                        .color(color::WARNING),
                )
                .wrap(),
            );
        }
    });
    if outcome.dismissed {
        action = AuthAction::Done;
    }
    action
}

fn key_section(
    ui: &mut Ui,
    state: &mut EditorState,
    clipboard_available: bool,
    action: &mut AuthAction,
) {
    if let Some(SshAuth::Key { key, .. }) = &state.auth {
        card(ui, |ui| {
            ui.add_space(space::SM);
            ui.horizontal(|ui| {
                ui.add_space(space::MD);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - space::MD);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    caption_label(ui, &format!("{} key", key.algorithm));
                    ui.label(
                        egui::RichText::new(&key.fingerprint)
                            .monospace()
                            .size(type_scale::CAPTION + 0.5)
                            .color(color::TEXT),
                    );
                    caption_label(ui, "Public key · add to authorized_keys");
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&key.public_key)
                                .monospace()
                                .size(type_scale::CAPTION)
                                .color(color::TEXT_MUTED),
                        )
                        .wrap(),
                    );
                });
            });
            ui.add_space(space::SM);
        });
        if button(
            ui,
            ButtonKind::Secondary,
            "Copy public key",
            clipboard_available,
        )
        .clicked()
        {
            *action = AuthAction::CopyPublicKey;
        }
    } else {
        footnote(
            ui,
            "Import a private key from your clipboard, or generate a new one and add its public key to the server.",
        );
    }
    if state.needs_passphrase {
        caption_label(ui, "Key passphrase");
        ui.add_sized(
            [ui.available_width(), space::FIELD_HEIGHT],
            egui::TextEdit::singleline(&mut state.key_passphrase)
                .password(true)
                .sensitive(true)
                .hint_text("Passphrase to decrypt the key")
                .margin(egui::Margin::symmetric(14, 14))
                .char_limit(1024),
        );
        if button(ui, ButtonKind::Primary, "Unlock & import key", true).clicked() {
            *action = AuthAction::ImportPending;
        }
    } else {
        if button(
            ui,
            ButtonKind::Primary,
            "Paste private key from clipboard",
            clipboard_available,
        )
        .clicked()
        {
            *action = AuthAction::PasteKey;
        }
        if button(ui, ButtonKind::Secondary, "Generate Ed25519 key", true).clicked() {
            *action = AuthAction::GenerateKey;
        }
    }
}

fn dev_tunnel_fields(ui: &mut Ui, state: &mut EditorState, action: &mut EditorAction) {
    let choices = state.tunnel.clone().unwrap_or_default();
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        caption_label(ui, "GitHub account");
        match &choices.account_login {
            Some(login) => {
                info_row(
                    ui,
                    login,
                    "Signed in with GitHub",
                    false,
                    Some(Icon::Profile),
                );
                if button(ui, ButtonKind::Secondary, "Sign out", true).clicked() {
                    *action = EditorAction::SignOut;
                }
                footnote(ui, "Your account can be reused by other tunnel profiles.");
            }
            None => {
                if button(ui, ButtonKind::Primary, "Sign in with GitHub", true).clicked() {
                    *action = EditorAction::SignIn;
                }
                footnote(
                    ui,
                    "Sign in to list your Dev Tunnels, or type a tunnel name below.",
                );
            }
        }
    });
    text_field(
        ui,
        "Tunnel",
        &mut state.tunnel_name,
        "my-tunnel",
        ui.available_width(),
    );
    if choices.account_login.is_some() {
        if choices.loading {
            footnote(ui, "Loading your tunnels\u{2026}");
        } else if let Some(error) = &choices.error {
            ui.label(egui::RichText::new(error).color(color::DANGER));
        }
        if !choices.tunnels.is_empty() {
            card(ui, |ui| {
                for (index, name) in choices.tunnels.iter().enumerate() {
                    let mut row = RowSpec::new(name);
                    row.icon = Some(Icon::DevTunnel);
                    row.selected = *name == state.tunnel_name;
                    row.divider = index > 0;
                    if list_row(ui, &row).clicked() {
                        *action = EditorAction::PickTunnel(index);
                    }
                }
            });
        }
        if !choices.loading && button(ui, ButtonKind::Secondary, "Refresh tunnels", true).clicked()
        {
            *action = EditorAction::RefreshTunnels;
        }
    }
    text_field(
        ui,
        "Remote session (optional)",
        &mut state.session_name,
        "Host session",
        ui.available_width(),
    );
    card(ui, |ui| {
        switch_row(
            ui,
            "Take over if attached elsewhere",
            Some("Otherwise attaching is refused while another device holds the session."),
            &mut state.take_over,
            false,
        );
    });
    footnote(
        ui,
        "The host is verified the first time you connect; fesTerm refuses a changed host key.",
    );
}

/// Bottom sheet for first-time or changed host keys (mock-up 11).
pub fn host_verification_sheet(
    ctx: &egui::Context,
    bounds: Rect,
    model: &HostVerification,
) -> Option<HostDecision> {
    let mut decision = None;
    let changed = model.changed_from.is_some();
    let outcome = bottom_sheet(ctx, "host-verification", bounds, |ui| {
        if sheet_title(
            ui,
            if changed {
                "Host key changed"
            } else {
                "Verify this host"
            },
            None,
        ) {
            decision = Some(HostDecision::Cancel);
        }
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), Sense::hover());
            icon::paint(
                ui.painter(),
                if changed {
                    Icon::Warning
                } else {
                    Icon::HostKeyVerification
                },
                Rect::from_center_size(rect.center(), egui::vec2(34.0, 34.0)),
                if changed {
                    color::WARNING
                } else {
                    color::ACCENT_TEXT
                },
            );
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(&model.name)
                        .size(type_scale::NAV_TITLE)
                        .strong(),
                );
                ui.label(
                    egui::RichText::new(&model.target)
                        .size(type_scale::CAPTION)
                        .color(color::TEXT_MUTED),
                );
            });
        });
        ui.label(
            egui::RichText::new(if changed {
                "This host presented a different identity than the one trusted before. \
                 Verify it independently; this attempt must be rejected."
            } else {
                "This is your first connection to this host. Compare its fingerprint with \
                 a trusted source before continuing."
            })
            .size(type_scale::CAPTION + 0.5)
            .color(color::TEXT_MUTED),
        );
        card(ui, |ui| {
            ui.add_space(space::SM);
            ui.horizontal(|ui| {
                ui.add_space(space::MD);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    caption_label(ui, &format!("{} fingerprint", model.algorithm));
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&model.fingerprint)
                                .monospace()
                                .size(type_scale::CAPTION + 0.5)
                                .color(color::TEXT),
                        )
                        .wrap(),
                    );
                    if let Some(previous) = &model.changed_from {
                        caption_label(ui, "Previously trusted");
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(previous)
                                    .monospace()
                                    .size(type_scale::CAPTION + 0.5)
                                    .color(color::WARNING),
                            )
                            .wrap(),
                        );
                    }
                });
            });
            ui.add_space(space::SM);
        });
        footnote(
            ui,
            if changed {
                "Connections to a host whose key changed are always refused. If you expect the change, remove the saved key in Settings > Trusted hosts."
            } else {
                "Trusting saves this key on this iPhone. A changed key is refused later."
            },
        );
        if !changed && button(ui, ButtonKind::Primary, "Trust & Connect", true).clicked() {
            decision = Some(HostDecision::Trust);
        }
        if button(ui, ButtonKind::Secondary, "Cancel", true).clicked() {
            decision = Some(HostDecision::Cancel);
        }
    });
    if outcome.dismissed {
        decision = decision.or(Some(HostDecision::Cancel));
    }
    decision
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PasswordDecision {
    None,
    Submit,
    Cancel,
}

/// Password sheet; the buffer remains owned (and zeroized) by the caller.
pub fn password_sheet(
    ctx: &egui::Context,
    bounds: Rect,
    target: &str,
    retry: bool,
    password: &mut dyn egui::TextBuffer,
) -> PasswordDecision {
    let mut decision = PasswordDecision::None;
    let outcome = bottom_sheet(ctx, "password", bounds, |ui| {
        if sheet_title(ui, "Password required", None) {
            decision = PasswordDecision::Cancel;
        }
        ui.label(
            egui::RichText::new(format!("Password for {target}"))
                .size(type_scale::CAPTION + 0.5)
                .color(color::TEXT_MUTED),
        );
        if retry {
            ui.label(
                egui::RichText::new("Authentication failed. Try again or cancel.")
                    .size(type_scale::CAPTION)
                    .color(color::WARNING),
            );
        }
        ui.add_sized(
            [ui.available_width(), space::FIELD_HEIGHT],
            egui::TextEdit::singleline(password)
                .password(true)
                .sensitive(true)
                .margin(egui::Margin::symmetric(14, 14))
                .char_limit(4096),
        );
        if button(ui, ButtonKind::Primary, "Authenticate", true).clicked() {
            decision = PasswordDecision::Submit;
        }
        if button(ui, ButtonKind::Secondary, "Cancel", true).clicked() {
            decision = PasswordDecision::Cancel;
        }
    });
    // A stray scrim tap must not discard typed input; cancel is explicit.
    let _ = outcome;
    decision
}
