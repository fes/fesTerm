//! New-profile editor, host verification and password sheets.
//!
//! Layouts follow mock-ups 10-12; behaviour hooks arrive through view-model
//! structs and returned actions so callers own all connection side effects.

use super::model::{Capabilities, DevTunnelChoices, HostDecision, HostVerification, ProfileKind};
use super::theme::{color, radius, space, type_scale};
use super::widgets::{
    bottom_sheet, button, button_sized, caption_label, card, footnote, labelled, paint_fit,
    segmented, sheet_title, text_field, ButtonKind,
};
use eframe::egui::{self, Align2, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui};
use festerm_ui_egui::icon::{self, Icon};

#[derive(Clone, Debug, Default)]
pub struct EditorState {
    pub open: bool,
    pub kind_index: usize,
    pub name: String,
    pub host: String,
    pub port: String,
    pub username: String,
    pub session_mode: usize,
    pub session_name: String,
    pub error: Option<&'static str>,
    /// Reviewing the previous connection rather than creating a profile.
    pub reconnect: bool,
    pub tunnel: Option<DevTunnelChoices>,
}

impl EditorState {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        if self.port.is_empty() {
            self.port = "22".to_owned();
        }
    }

    pub fn open_new(&mut self) {
        self.open = true;
        self.reconnect = false;
        self.error = None;
        self.kind_index = 0;
        self.name.clear();
        self.host.clear();
        self.username.clear();
        self.port = "22".to_owned();
    }

    pub fn open_reconnect(&mut self) {
        self.open = true;
        self.reconnect = true;
        self.error = None;
        if self.port.is_empty() {
            self.port = "22".to_owned();
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.error = None;
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorAction {
    None,
    Cancel,
    SaveOnly,
    SaveAndConnect,
}

fn select_field(ui: &mut Ui, caption: &str, value: &str) {
    caption_label(ui, caption);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), space::FIELD_HEIGHT),
        Sense::CLICK,
    );
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(radius::FIELD), color::FIELD);
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius::FIELD),
        Stroke::new(1.0, color::BORDER),
        StrokeKind::Inside,
    );
    paint_fit(
        painter,
        egui::pos2(rect.left() + 14.0, rect.center().y),
        Align2::LEFT_CENTER,
        value,
        type_scale::font(type_scale::BODY),
        color::TEXT,
        rect.width() - 56.0,
    );
    icon::paint(
        painter,
        Icon::Disclosure,
        Rect::from_center_size(
            egui::pos2(rect.right() - 22.0, rect.center().y),
            egui::vec2(16.0, 16.0),
        ),
        color::TEXT_MUTED,
    );
    labelled(response, caption, true);
}

fn info_row(ui: &mut Ui, title: &str, value: &str, trailing_chevron: bool, glyph: Option<Icon>) {
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
    labelled(response, title, trailing_chevron);
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
                if state.reconnect {
                    "Reconnect"
                } else {
                    "New profile"
                },
                type_scale::font(type_scale::NAV_TITLE),
                color::TEXT,
                rect.width() - 180.0,
            );
        });
    let connect_label = match (state.reconnect, kind) {
        (true, _) => "Connect",
        (false, ProfileKind::DevTunnel) => "Save & Attach",
        (false, ProfileKind::Sftp) => "Save & Open",
        (false, ProfileKind::Ssh) => "Save & Connect",
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
                if !state.reconnect {
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
                        can_connect || kind != ProfileKind::Ssh,
                        egui::vec2(width - secondary - gap, space::BUTTON_HEIGHT),
                    )
                    .clicked()
                    {
                        action = EditorAction::SaveAndConnect;
                    }
                } else if button(ui, ButtonKind::Primary, connect_label, can_connect).clicked() {
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
                    if kinds.len() > 1 && !state.reconnect {
                        let items: Vec<(Option<Icon>, &str)> = kinds
                            .iter()
                            .map(|kind| (Some(kind.glyph()), kind.label()))
                            .collect();
                        segmented(ui, &items, &mut state.kind_index, true);
                    }
                    text_field(ui, "Profile name", &mut state.name, "prod-web", inner_width);
                    match kind {
                        ProfileKind::DevTunnel => dev_tunnel_fields(ui, state),
                        _ => ssh_fields(ui, state, caps, inner_width, kind),
                    }
                    if let Some(error) = state.error {
                        ui.label(egui::RichText::new(error).color(color::DANGER));
                    }
                    ui.add_space(space::LG);
                });
            });
        });
    action
}

fn ssh_fields(
    ui: &mut Ui,
    state: &mut EditorState,
    caps: Capabilities,
    width: f32,
    kind: ProfileKind,
) {
    let gap = ui.spacing().item_spacing.x;
    let port_width = 84.0;
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
        if caps.key_authentication {
            info_row(ui, "SSH key", "Personal key", true, None);
            footnote(ui, "Credentials are kept in Keychain, not in profiles.");
        } else {
            info_row(ui, "Password", "Asked when connecting", false, None);
            footnote(ui, "Passwords are never saved.");
        }
    });
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
        text_field(ui, "Session name", &mut state.session_name, "work", width);
        footnote(
            ui,
            "Attach to this remote session, or create it if missing.",
        );
    }
}

fn dev_tunnel_fields(ui: &mut Ui, state: &mut EditorState) {
    let Some(choices) = state.tunnel.as_ref() else {
        footnote(ui, "Dev Tunnel sign-in is not available yet.");
        return;
    };
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        caption_label(ui, "Account");
        info_row(
            ui,
            &choices.account_name,
            &choices.account_user,
            true,
            Some(Icon::Profile),
        );
        footnote(ui, "Your account can be reused by other tunnel profiles.");
    });
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        select_field(
            ui,
            "Tunnel",
            choices
                .tunnels
                .get(choices.tunnel)
                .map_or("", String::as_str),
        );
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
            icon::paint(
                ui.painter(),
                Icon::HostKeyVerification,
                rect,
                color::SUCCESS,
            );
            ui.label(
                egui::RichText::new("Verified host · fesTerm bridge")
                    .size(type_scale::CAPTION)
                    .color(color::SUCCESS),
            );
        });
    });
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        select_field(
            ui,
            "Existing remote session",
            choices
                .sessions
                .get(choices.session)
                .map_or("", String::as_str),
        );
    });
    if let Some(session) = choices.sessions.get(choices.session) {
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(space::MD);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(28.0, 56.0), Sense::hover());
                icon::paint(
                    ui.painter(),
                    Icon::DevTunnel,
                    Rect::from_center_size(rect.center(), egui::vec2(24.0, 24.0)),
                    color::TEXT_MUTED,
                );
                ui.vertical(|ui| {
                    ui.add_space(space::SM);
                    ui.label(
                        egui::RichText::new("Resume the work already running")
                            .size(type_scale::CAPTION + 0.5)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new("Remote session · unattached")
                            .size(type_scale::CAPTION)
                            .color(color::TEXT_MUTED),
                    );
                    ui.label(
                        egui::RichText::new(format!("This connection attaches to {session}."))
                            .size(type_scale::CAPTION)
                            .color(color::TEXT_MUTED),
                    );
                    ui.add_space(space::SM);
                });
            });
        });
    }
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
            "Trust lasts until fesTerm closes; a changed identity is rejected.",
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
