//! The connection-destination editing pane shared by every SSH-based
//! surface: the SSH launcher, the SFTP launcher, and the Profiles editor
//! for SSH/SFTP profiles.
//!
//! Each of those surfaces used to render its own Username/Host/Port fields
//! against its own state type, so behaviour drifted between them (field
//! order, the `user@host:port` shorthand, focus). They now share one pane
//! and customise around it: a surface supplies a borrowed view of its own
//! fields plus the visual style its card uses, and adds its own extras
//! (profile name, durable session, authentication) above and below.

use eframe::egui::{self, TextEdit, Ui};
use festerm_ui_egui::theme;

use crate::tabs::TabId;

/// The default SSH port, shown prefilled rather than implied by hint text.
pub(super) const DEFAULT_SSH_PORT: u16 = 22;

/// How a labeled field arranges its label and entry box.
///
/// The two launcher cards stack labels above full-width fields; the Profile
/// editors place a label beside a fixed-width field. This is purely visual:
/// the field's identity, behaviour and ordering are the same either way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum FieldStyle {
    Stacked,
    Inline,
}

impl FieldStyle {
    /// The Profile editors' fixed entry width, matching the rest of their
    /// two-column rows.
    const INLINE_WIDTH: f32 = 240.0;

    /// Width reserved for an inline label, so every entry box in a card
    /// starts on the same vertical line instead of stepping in and out
    /// with the length of each label.
    const INLINE_LABEL_LANE: f32 = 78.0;

    fn default_width(self) -> f32 {
        match self {
            Self::Stacked => f32::INFINITY,
            Self::Inline => Self::INLINE_WIDTH,
        }
    }
}

/// Presentation options for [`labeled_text_edit`], bundled so the helper
/// keeps a short argument list as surfaces add variations.
#[derive(Clone, Copy)]
pub(super) struct FieldOptions {
    style: FieldStyle,
    hint: &'static str,
    width: Option<f32>,
    password: bool,
}

impl FieldOptions {
    pub(super) fn new(style: FieldStyle) -> Self {
        Self {
            style,
            hint: "",
            width: None,
            password: false,
        }
    }

    pub(super) fn stacked() -> Self {
        Self::new(FieldStyle::Stacked)
    }

    pub(super) fn inline() -> Self {
        Self::new(FieldStyle::Inline)
    }

    pub(super) fn hint(mut self, hint: &'static str) -> Self {
        self.hint = hint;
        self
    }

    pub(super) fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub(super) fn password(mut self, password: bool) -> Self {
        self.password = password;
        self
    }
}

/// The single labeled text field every SSH-based surface is built from.
///
/// The returned `Response` is the *field's*, already associated with its
/// label, so callers can check `changed()`/`lost_focus()` and request focus
/// without reaching for the label.
pub(super) fn labeled_text_edit(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    value: &mut String,
    options: FieldOptions,
) -> egui::Response {
    let width = options
        .width
        .unwrap_or_else(|| options.style.default_width());
    let show_label = |ui: &mut Ui| {
        ui.add(
            egui::Label::new(egui::RichText::new(label).color(theme::TEXT_SECONDARY))
                .selectable(false),
        )
    };
    let show_field = |ui: &mut Ui| {
        ui.add(
            TextEdit::singleline(value)
                .id_salt(id)
                .hint_text(options.hint)
                .password(options.password)
                .desired_width(width),
        )
    };
    match options.style {
        FieldStyle::Stacked => {
            ui.vertical(|ui| {
                let label = show_label(ui);
                show_field(ui).labelled_by(label.id)
            })
            .inner
        }
        FieldStyle::Inline => {
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                let label = show_label(ui);
                pad_to_label_lane(ui, &label);
                show_field(ui).labelled_by(label.id)
            })
            .inner
        }
    }
}

/// Pads an already-drawn inline label out to the shared label lane, so
/// every control in a card starts on the same vertical line rather than
/// stepping in and out with the length of each label.
///
/// Padding explicitly rather than allocating a sized child `Ui` because
/// `allocate_ui_with_layout` reports the child's *content* size back to the
/// parent, so a lane requested that way simply collapses.
pub(super) fn pad_to_label_lane(ui: &mut Ui, label: &egui::Response) {
    let padding = FieldStyle::INLINE_LABEL_LANE - label.rect.width();
    if padding > 0.0 {
        ui.add_space(padding - ui.spacing().item_spacing.x);
    }
}

/// A borrowed view of the destination state a surface owns.
///
/// Surfaces keep their own state types (`SshLauncherForm`,
/// `SshProfileDraft`); this is how they lend the pane the five fields it
/// edits without either side knowing about the other.
pub(super) struct DestinationFields<'a> {
    pub username: &'a mut String,
    pub host: &'a mut String,
    pub port: &'a mut String,
    /// The single `user@host[:port]` shorthand.
    pub quick_connect: &'a mut String,
    /// Which notation is currently on screen: the separate fields (`true`)
    /// or the shorthand (`false`). Exactly one is ever visible.
    pub expanded: &'a mut bool,
}

impl DestinationFields<'_> {
    /// Parses the shorthand into the separate fields, reporting the first
    /// problem in the same words on every surface.
    pub(super) fn parse_quick_connect(&mut self) -> Result<(), String> {
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
        *self.username = username.to_owned();
        *self.host = host.to_owned();
        *self.port = port
            .map(str::to_owned)
            .unwrap_or_else(|| DEFAULT_SSH_PORT.to_string());
        Ok(())
    }

    /// Best-effort version of `parse_quick_connect`: fills in whatever it
    /// can and never blocks, since revealing the separate fields must
    /// always succeed even while the shorthand is half-typed.
    pub(super) fn sync_expanded_from_quick_connect(&mut self) {
        let _ = self.parse_quick_connect();
    }

    /// Inverse of `sync_expanded_from_quick_connect`, omitting the port
    /// when it is still the default so switching notations round-trips
    /// what the user actually typed.
    pub(super) fn sync_quick_connect_from_expanded(&mut self) {
        if self.username.is_empty() && self.host.is_empty() {
            return;
        }
        let port = self.port.trim();
        *self.quick_connect = if port.is_empty() || port == DEFAULT_SSH_PORT.to_string() {
            format!("{}@{}", self.username, self.host)
        } else {
            format!("{}@{}:{}", self.username, self.host, port)
        };
    }

    /// Switches notation, carrying the destination across so the newly
    /// revealed notation already shows what the user typed in the one it
    /// replaces.
    pub(super) fn toggle_notation(&mut self) {
        if *self.expanded {
            self.sync_quick_connect_from_expanded();
        } else {
            self.sync_expanded_from_quick_connect();
        }
        *self.expanded = !*self.expanded;
    }
}

/// The destination pane itself: a notation toggle plus whichever notation
/// is currently selected.
pub(super) struct DestinationPane<'a> {
    fields: DestinationFields<'a>,
    tab_id: TabId,
    /// Namespace for the field ids, so each surface keeps the widget
    /// identities (and therefore focus and undo state) it already had.
    id_prefix: &'static str,
    style: FieldStyle,
}

impl<'a> DestinationPane<'a> {
    pub(super) fn new(
        fields: DestinationFields<'a>,
        tab_id: TabId,
        id_prefix: &'static str,
        style: FieldStyle,
    ) -> Self {
        Self {
            fields,
            tab_id,
            id_prefix,
            style,
        }
    }

    fn text_edit(
        &mut self,
        ui: &mut Ui,
        field: &'static str,
        label: &str,
        hint: &'static str,
        width: Option<f32>,
    ) -> egui::Response {
        let mut options = FieldOptions::new(self.style).hint(hint);
        if let Some(width) = width {
            options = options.width(width);
        }
        let id = (self.id_prefix, self.tab_id, field);
        let value = match field {
            "username" => &mut *self.fields.username,
            "host" => &mut *self.fields.host,
            "port" => &mut *self.fields.port,
            "quick_connect" => &mut *self.fields.quick_connect,
            other => unreachable!("destination pane has no {other} field"),
        };
        labeled_text_edit(ui, id, label, value, options)
    }

    /// Renders the pane. `request_focus` places the caret on whichever
    /// field currently leads the visible notation. Returns whether Enter
    /// was pressed in a destination field, which surfaces treat as a
    /// submit without re-deriving it.
    pub(super) fn show(mut self, ui: &mut Ui, request_focus: bool) -> bool {
        ui.horizontal(|ui| {
            super::ssh_section_heading(ui, "Connection");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let toggle_label = if *self.fields.expanded {
                    "Use user@host:port"
                } else {
                    "Use separate fields"
                };
                if ui.small_button(toggle_label).clicked() {
                    self.fields.toggle_notation();
                }
            });
        });

        if !*self.fields.expanded {
            let quick =
                self.text_edit(ui, "quick_connect", "Quick connect", "user@host:port", None);
            if request_focus {
                quick.request_focus();
            }
            if quick.changed() {
                // Synced eagerly rather than only on toggle, so the
                // separate fields are already correct if the surface
                // submits, or the user switches, without touching them.
                self.fields.sync_expanded_from_quick_connect();
            }
            return enter_pressed(ui, &quick);
        }

        // Username leads because that is the reading order of the
        // `user@host:port` notation this pane toggles with, and the order
        // `docs/gui-action-graph.md`'s LAUNCH-02 specifies.
        let username = self.text_edit(ui, "username", "Username", "", None);
        if request_focus {
            username.request_focus();
        }
        let mut changed = username.changed();
        let mut submit_with_enter = enter_pressed(ui, &username);
        if self.style == FieldStyle::Stacked {
            ui.add_space(8.0);
        }

        // Wide launcher cards put Host and Port on one row; narrow cards
        // and the editors' fixed-width column stack them.
        let side_by_side = self.style == FieldStyle::Stacked && ui.available_width() >= 460.0;
        let (host, port) = if side_by_side {
            ui.horizontal(|ui| {
                let port_width = 110.0;
                let host_width =
                    (ui.available_width() - port_width - ui.spacing().item_spacing.x).max(180.0);
                let host = self.text_edit(ui, "host", "Host", "", Some(host_width));
                let port = self.text_edit(ui, "port", "Port", "", Some(port_width));
                (host, port)
            })
            .inner
        } else {
            let port_width = (self.style == FieldStyle::Stacked).then_some(110.0);
            let host = self.text_edit(ui, "host", "Host", "", None);
            let port = self.text_edit(ui, "port", "Port", "", port_width);
            (host, port)
        };
        changed |= host.changed() | port.changed();
        submit_with_enter |= enter_pressed(ui, &host) || enter_pressed(ui, &port);
        if changed {
            self.fields.sync_quick_connect_from_expanded();
        }
        submit_with_enter
    }
}

fn enter_pressed(ui: &Ui, response: &egui::Response) -> bool {
    response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
}
