//! Phone-width client navigation. Only returned actions can start HOST work.
use super::*;
use crate::widgets::{
    button, button_sized, card as surface_card, footnote, labelled, paint_fit, section_header,
    ButtonKind,
};

// HOST fields must publish the same native document as ordinary mobile forms.
// Bare TextEdit selects the fallback system IME without the field edit bridge.
fn host_field(
    ui: &mut egui::Ui,
    name: &str,
    edit: egui::TextEdit<'_>,
    multiline: bool,
) -> egui::Response {
    let id = ui.make_persistent_id(("host_client_field", name));
    let edit = if multiline {
        edit.margin(egui::Margin::same(8))
    } else {
        let padding = ((44.0 - ui.text_style_height(&egui::TextStyle::Body)) / 2.0)
            .ceil()
            .max(8.0) as i8;
        edit.margin(egui::Margin::symmetric(8, padding))
            .vertical_align(egui::Align::Center)
    };
    let response = crate::widgets::local_text_edit(ui, id, edit, false, multiline)
        .response
        .response;
    crate::widgets::name_field(response, name)
}

pub(super) fn pairing_scope(hosts: &HostUi) -> egui::Id {
    egui::Id::new((
        "host_pairing",
        &hosts.account,
        &hosts.locator,
        hosts
            .selected
            .as_ref()
            .map(|recipe| (&recipe.id, &recipe.host.id)),
    ))
}

pub(super) fn pairing_fields(
    ui: &mut egui::Ui,
    scope: egui::Id,
    fingerprint: &mut String,
    prerequisite: &mut String,
) {
    ui.push_id(scope, |ui| {
        ui.label("Independent host fingerprint");
        host_field(
            ui,
            "Independent host fingerprint",
            egui::TextEdit::singleline(fingerprint),
            false,
        );
        ui.label("Host connection details");
        host_field(
            ui,
            "Host connection details",
            egui::TextEdit::multiline(prerequisite)
                .hint_text("Paste the host's connection details")
                .char_limit(4096),
            true,
        );
    });
}

pub(super) fn host_display_name(host: &TunnelSummary) -> &str {
    host.description
        .as_deref()
        .into_iter()
        .chain(host.name.as_deref())
        .map(str::trim)
        .find(|name| !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control))
        .unwrap_or(&host.locator)
}

pub(super) fn selected_host_title(hosts: &HostUi) -> &str {
    hosts
        .hosts
        .iter()
        .find(|host| host.locator == hosts.locator)
        .map(host_display_name)
        .or_else(|| {
            hosts
                .selected
                .as_ref()
                .and_then(|recipe| recipe.host_hint.as_ref())
                .map(|hint| hint.name.as_str())
        })
        .unwrap_or(if hosts.locator.is_empty() {
            "Selected host"
        } else {
            &hosts.locator
        })
}

fn card<R>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    surface_card(ui, |ui| {
        egui::Frame::new()
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;
                contents(ui)
            })
            .inner
    })
}

fn navigation(ui: &mut egui::Ui, title: &str, back: bool) -> bool {
    ui.horizontal(|ui| {
        let done = button_sized(
            ui,
            ButtonKind::Secondary,
            if back { "Back" } else { "Done" },
            true,
            egui::vec2(64.0, 44.0),
        );
        let title_width = (ui.available_width() - 64.0 - ui.spacing().item_spacing.x).max(0.0);
        ui.add_sized(
            egui::vec2(title_width, 44.0),
            egui::Label::new(egui::RichText::new(title).heading()).truncate(),
        );
        ui.add_space(64.0);
        done.clicked()
    })
    .inner
}

pub(super) fn host_disclosure(
    ui: &mut egui::Ui,
    name: &str,
    locator: Option<&str>,
    expanded: bool,
    enabled: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 44.0),
        if enabled {
            egui::Sense::CLICK
        } else {
            egui::Sense::hover()
        },
    );
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let center = egui::pos2(rect.left() + 16.0, rect.center().y);
    let points = if expanded {
        [
            egui::vec2(-5.0, -3.0),
            egui::vec2(5.0, -3.0),
            egui::vec2(0.0, 4.0),
        ]
    } else {
        [
            egui::vec2(-3.0, -5.0),
            egui::vec2(-3.0, 5.0),
            egui::vec2(4.0, 0.0),
        ]
    };
    painter.add(egui::Shape::convex_polygon(
        points.into_iter().map(|point| center + point).collect(),
        ui.visuals().text_color(),
        egui::Stroke::NONE,
    ));
    paint_fit(
        &painter,
        egui::pos2(rect.left() + 32.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(16.0),
        ui.visuals().text_color(),
        rect.width() - 40.0,
    );
    let action = if expanded { "Collapse" } else { "Expand" };
    let label = match locator {
        Some(locator) => format!("{action} {name} · {locator}"),
        None => format!("{action} {name}"),
    };
    labelled(response, &label, enabled)
}

fn primary(ui: &mut egui::Ui, label: &str, enabled: bool) -> bool {
    button(ui, ButtonKind::Primary, label, enabled).clicked()
}

fn secondary(ui: &mut egui::Ui, label: &str, enabled: bool) -> bool {
    button(ui, ButtonKind::Secondary, label, enabled).clicked()
}

fn notice(error: &str) -> &str {
    if error.contains("Sign in") || error.contains("sign-in") {
        "Sign in again from Settings."
    } else if error.contains("timed out") || error.contains("timeout") {
        "Host timed out. Try again."
    } else if error.contains("creation") && error.contains("denied") {
        "This host does not allow new sessions."
    } else {
        error.split("; ").next().unwrap_or(error)
    }
}

pub(super) fn creation_notice(disposition: host_client::EnsureDisposition) -> &'static str {
    match disposition {
        host_client::EnsureDisposition::Created => "Started a new shell.",
        host_client::EnsureDisposition::Resumed => "Resumed the existing session.",
        host_client::EnsureDisposition::Recreated => "Previous session ended. Started a new shell.",
    }
}

impl MobileApp {
    pub(super) fn show_host_connection_notice(&mut self, ctx: &egui::Context) {
        let Some((disposition, started)) = self.hosts.connection_notice else {
            return;
        };
        let elapsed = started.elapsed();
        let lifetime = std::time::Duration::from_secs(8);
        if elapsed >= lifetime {
            self.hosts.connection_notice = None;
            return;
        }
        if self.active_is_host() && self.host_foreground() {
            egui::Area::new(egui::Id::new("mobile-host-connection-notice"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 64.0))
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(320.0);
                        ui.label(creation_notice(disposition));
                    });
                });
            ctx.request_repaint_after(lifetime - elapsed);
        }
    }

    #[cfg(test)]
    pub(super) fn show_hosts(&mut self, ctx: &egui::Context) {
        self.show_hosts_in_bounds(ctx, ctx.content_rect());
    }

    pub(crate) fn show_hosts_in_bounds(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        let (accounts, recipes) = self
            .profiles
            .as_ref()
            .and_then(|profiles| {
                let store = profiles.store_handle();
                let store = store.lock().ok()?;
                Some((
                    store.accounts().to_vec(),
                    store.host_recipes().recipes().to_vec(),
                ))
            })
            .unwrap_or_default();
        let old_locator = self.hosts.locator.clone();
        let old_fingerprint = self.hosts.fingerprint.clone();
        let old_prerequisite = self.hosts.prerequisite.clone();
        let old_consent = (self.hosts.trust, self.hosts.enrolled, self.hosts.launch);
        let mut action = None;
        egui::Window::new("Dev Tunnel")
            .id(egui::Id::new("mobile-hosts"))
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .fixed_pos(bounds.min)
            .fixed_size(bounds.size())
            .vscroll(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 16.0).clamp(0.0, 560.0);
                    let inset = (ui.available_width() - width) / 2.0;
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add_space(inset);
                    ui.vertical(|ui| {
                        ui.set_width(width);
                        ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
                        ui.spacing_mut().interact_size.y = 44.0;
                        egui::Frame::new()
                            .inner_margin(egui::Margin::same(8))
                            .show(ui, |ui| {
                                let detail =
                                    !self.hosts.locator.is_empty() && !self.hosts.host_collapsed;
                                let (title, header_action) = if self.hosts.editing {
                                    ("Dev Tunnel profile", Action::Close)
                                } else if self.hosts.session_save.is_some() {
                                    ("Save as profile", Action::CancelSessionSave)
                                } else if self.hosts.quick_new {
                                    ("New host shell", Action::CancelEdit)
                                } else if detail {
                                    (selected_host_title(&self.hosts), Action::BackHosts)
                                } else {
                                    ("Find & Connect", Action::Close)
                                };
                                if navigation(ui, title, !matches!(header_action, Action::Close)) {
                                    action = Some(header_action);
                                }
                                if self.hosts.busy() {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label(if self.hosts.quick_check.is_some() {
                                            "Checking protected history…"
                                        } else {
                                            "Working…"
                                        });
                                    });
                                }
                                if let Some(error) = &self.hosts.error {
                                    ui.colored_label(ui.visuals().error_fg_color, notice(error));
                                }
                                if self.hosts.editing {
                                    self.show_host_profile_form(ui, &mut action);
                                    return;
                                }
                                if self.hosts.session_save.is_some() {
                                    self.show_session_save_form(ui, &mut action);
                                    return;
                                }
                                if self.hosts.quick_new {
                                    let scope = self
                                        .hosts
                                        .selected
                                        .as_ref()
                                        .map(|recipe| recipe.id.as_str().to_owned());
                                    ui.push_id(("new-host-shell", scope), |ui| {
                                        self.show_host_quick_form(ui, &mut action)
                                    });
                                    return;
                                }
                                if !self.hosts.locator.is_empty() && !self.hosts.host_collapsed {
                                    if let Some(account) = accounts
                                        .iter()
                                        .find(|account| account.id == self.hosts.account)
                                    {
                                        footnote(ui, &format!("@{}", account.login));
                                    }
                                    self.show_selected_host(ui, &recipes, &mut action);
                                } else {
                                    if self.hosts.profile_host_review.is_some() {
                                        section_header(ui, "Choose profile host", None, false);
                                        for (index, recipe) in recipes.iter().enumerate() {
                                            let name = recipe
                                                .host_hint
                                                .as_ref()
                                                .map(|hint| hint.name.as_str())
                                                .unwrap_or("Saved host");
                                            if secondary(
                                                ui,
                                                &format!("Use saved host · {name}"),
                                                !self.hosts.busy(),
                                            ) {
                                                action =
                                                    Some(Action::SelectSavedProfileHost(index));
                                            }
                                        }
                                        if secondary(ui, "Cancel host selection", true) {
                                            action = Some(Action::CancelProfileHost);
                                        }
                                    }
                                    section_header(ui, "GitHub account", None, false);
                                    if accounts.is_empty()
                                        && primary(ui, "Sign in to GitHub", !self.hosts.busy())
                                    {
                                        action = Some(Action::SignIn);
                                    }
                                    if !accounts.is_empty() {
                                        let current = accounts
                                            .iter()
                                            .position(|account| account.id == self.hosts.account);
                                        let selected = current
                                            .map(|index| format!("@{}", accounts[index].login))
                                            .unwrap_or_else(|| "Select account".into());
                                        ui.horizontal(|ui| {
                                            let width = (ui.available_width() - 12.0) / 2.0;
                                            egui::ComboBox::from_id_salt("host-account")
                                                .selected_text(&selected)
                                                .width(width)
                                                .show_ui(ui, |ui| {
                                                    for (index, account) in
                                                        accounts.iter().enumerate()
                                                    {
                                                        if ui
                                                            .selectable_label(
                                                                current == Some(index),
                                                                format!("@{}", account.login),
                                                            )
                                                            .clicked()
                                                        {
                                                            action =
                                                                Some(Action::FindAccount(index));
                                                        }
                                                    }
                                                    ui.separator();
                                                    if ui
                                                        .selectable_label(false, "Manage accounts…")
                                                        .clicked()
                                                    {
                                                        action = Some(Action::Settings);
                                                    }
                                                });
                                            if let Some(index) = current {
                                                let enabled = !self.hosts.busy();
                                                if labelled(
                                                    button_sized(
                                                        ui,
                                                        ButtonKind::Secondary,
                                                        "Refresh hosts",
                                                        enabled,
                                                        egui::vec2(width, 44.0),
                                                    ),
                                                    &format!(
                                                        "Refresh hosts · @{}",
                                                        accounts[index].login
                                                    ),
                                                    enabled,
                                                )
                                                .clicked()
                                                {
                                                    action = Some(Action::FindAccount(index));
                                                }
                                            }
                                        });
                                    } else if ui
                                        .add(
                                            egui::Button::new("Manage accounts…")
                                                .frame(false)
                                                .min_size(egui::vec2(0.0, 44.0)),
                                        )
                                        .clicked()
                                    {
                                        action = Some(Action::Settings);
                                    }
                                    section_header(ui, "Hosts", None, false);
                                    if self.hosts.discovered && self.hosts.hosts.is_empty() {
                                        footnote(
                                            ui,
                                            "No hosts found. Your saved profiles are unchanged.",
                                        );
                                    }
                                    for (index, host) in self.hosts.hosts.clone().iter().enumerate()
                                    {
                                        let name = host_display_name(host);
                                        let locator = (self
                                            .hosts
                                            .hosts
                                            .iter()
                                            .filter(|other| host_display_name(other) == name)
                                            .take(2)
                                            .count()
                                            == 2)
                                            .then_some(host.locator.as_str());
                                        card(ui, |ui| {
                                            if host_disclosure(
                                                ui,
                                                name,
                                                locator,
                                                false,
                                                !self.hosts.busy(),
                                            )
                                            .clicked()
                                            {
                                                action = Some(Action::SelectHost(index));
                                            }
                                            if let Some(locator) = locator {
                                                ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(locator).small().weak(),
                                                    )
                                                    .wrap(),
                                                );
                                            }
                                        });
                                        ui.add_space(12.0);
                                    }
                                }
                                egui::CollapsingHeader::new("Details & recovery")
                                    .open(
                                        (self.hosts.end_review.is_some()
                                            || self.hosts.forget_review.is_some())
                                        .then_some(true),
                                    )
                                    .show(ui, |ui| {
                                        if let Some(error) = &self.hosts.error {
                                            ui.label(error);
                                        }
                                        ui.label("Host locator");
                                        host_field(
                                            ui,
                                            "Host locator",
                                            egui::TextEdit::singleline(&mut self.hosts.locator),
                                            false,
                                        );
                                        if secondary(ui, "Review another host", !self.hosts.busy())
                                        {
                                            action = Some(Action::NewReview);
                                        }
                                        if self.hosts.original_store.is_none()
                                            && secondary(
                                                ui,
                                                "Check protected history",
                                                !self.hosts.busy(),
                                            )
                                        {
                                            action = Some(Action::RecoverOriginals);
                                        }
                                        self.show_host_recovery(ui, &mut action);
                                        ui.collapsing("Import public profiles", |ui| {
                                            host_field(
                                                ui,
                                                "Import public profiles",
                                                egui::TextEdit::multiline(&mut self.hosts.import)
                                                    .char_limit(64 * 1024),
                                                true,
                                            );
                                            ui.checkbox(
                                                &mut self.hosts.import_reviewed,
                                                "I reviewed this public metadata",
                                            );
                                            if secondary(
                                                ui,
                                                "Import profiles",
                                                self.hosts.import_reviewed,
                                            ) {
                                                action = Some(Action::Import);
                                            }
                                        });
                                    });
                            });
                    });
                });
            });
        #[cfg(all(test, unix))]
        if self.hosts.gallery_display.is_some() {
            return;
        }
        if old_locator != self.hosts.locator
            || old_fingerprint != self.hosts.fingerprint
            || old_prerequisite != self.hosts.prerequisite
            || (old_consent.0 && !self.hosts.trust)
            || (old_consent.1 && !self.hosts.enrolled)
            || (self.hosts.busy()
                && old_consent != (self.hosts.trust, self.hosts.enrolled, self.hosts.launch))
        {
            self.hosts.clear_approval();
            action = None;
        }
        if let Some(review) = &mut self.hosts.end_review {
            let response = egui::Modal::new(egui::Id::new((
                "terminate_host_session",
                review.intent.operation().to_string(),
            )))
            .show(ctx, |ui| {
                let (confirm, cancel) = display::end_review(
                    ui,
                    display::EndDisplay {
                        name: &review.name,
                        host: &review.host_name,
                    },
                    &mut review.confirmed,
                    &mut review.cancel_focused,
                );
                if cancel {
                    action = Some(Action::CancelEnd);
                } else if confirm {
                    action = Some(Action::ConfirmEnd);
                }
            });
            if response.should_close() {
                action = Some(Action::CancelEnd);
            }
        }
        if let Some(action) = action {
            self.hosts.error = None;
            if let Err(error) = self.host_action(ctx, action) {
                self.hosts.error = Some(error);
            }
        }
    }

    fn show_selected_host(
        &mut self,
        ui: &mut egui::Ui,
        recipes: &[PublicRecipe],
        action: &mut Option<Action>,
    ) {
        let busy = self.hosts.busy();
        if self.hosts.saved_connect.is_some() {
            ui.label("Connecting saved profile…");
            return;
        }
        if self.hosts.ordinary_selected && self.hosts.inventory.is_none() {
            if busy {
                ui.label("Loading authorized sessions…");
            } else if primary(ui, "Retry host connection", true) {
                *action = Some(Action::Ordinary);
            }
            if secondary(ui, "Advanced owner connection…", !busy) {
                *action = Some(Action::AdvancedOwner);
            }
            return;
        }
        if !self.hosts.ordinary_selected && self.hosts.receipt.is_none() {
            ui.label("Verify host");
            footnote(
                ui,
                "On the host, open Settings > Dev Tunnels > Remote access > Advanced · manually pair an owner device.",
            );
            if self.hosts.selected.is_some() && primary(ui, "Load protected approval", !busy) {
                *action = Some(Action::Load);
            }
            ui.collapsing("Connect this device", |ui| {
                let scope = pairing_scope(&self.hosts);
                pairing_fields(
                    ui,
                    scope,
                    &mut self.hosts.fingerprint,
                    &mut self.hosts.prerequisite,
                );
                ui.checkbox(
                    &mut self.hosts.trust,
                    "I independently verified this host, account and owner binding",
                );
                if primary(ui, "Prepare this device", self.hosts.trust && !busy) {
                    *action = Some(Action::Prepare);
                }
            });
            return;
        }
        if !self.hosts.ordinary_selected && !self.hosts.enrolled {
            let receipt = self.hosts.receipt.as_ref().expect("checked receipt");
            ui.label("On the host, choose Approve a client and paste this device's public key and fingerprint.");
            ui.collapsing("Owner public key", |ui| {
                let key = receipt
                    .public_key()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                ui.label(&key);
                if let Ok(fingerprint) = owner_key_fingerprint(&receipt.public_key()) {
                    ui.label(&fingerprint);
                    if secondary(ui, "Copy fingerprint", true) {
                        ui.ctx().copy_text(fingerprint);
                    }
                }
                if secondary(ui, "Copy public key", true) {
                    ui.ctx().copy_text(key);
                }
            });
            ui.label(&self.hosts.fingerprint);
            ui.checkbox(
                &mut self.hosts.enrolled,
                "I verified the fingerprint and approved this key on this host",
            );
            return;
        }
        if self.hosts.inventory.is_none() {
            if primary(ui, "List authorized sessions", !busy) {
                *action = Some(Action::Resolve);
            }
            return;
        }
        if self.hosts.profile_host_review.is_some() {
            if primary(ui, "Use this host", !busy) {
                *action = Some(Action::UseProfileHost);
            }
            return;
        }
        let inventory = self.hosts.inventory.as_ref().expect("checked inventory");
        ui.horizontal(|ui| {
            let create = self.hosts.quick_creation_allowed();
            let width = if create {
                (ui.available_width() - 12.0) / 2.0
            } else {
                ui.available_width()
            };
            if button_sized(
                ui,
                ButtonKind::Secondary,
                "Refresh sessions",
                !busy,
                egui::vec2(width, 44.0),
            )
            .clicked()
            {
                *action = Some(Action::Resolve);
            }
            if create
                && button_sized(
                    ui,
                    ButtonKind::Primary,
                    "New host shell",
                    !busy,
                    egui::vec2(width, 44.0),
                )
                .clicked()
            {
                *action = Some(Action::QuickNew);
            }
        });
        if !inventory.capabilities.creation_allowed {
            footnote(ui, "New shell creation is disabled by this host.");
        }
        ui.add_space(12.0);
        section_header(ui, "Sessions", None, false);
        if inventory.sessions.is_empty() && inventory.profiles.is_empty() {
            footnote(ui, "No authorized sessions listed.");
        }
        if !inventory.unknown.is_empty() {
            footnote(ui, "Some session states are unknown.");
        }
        for (index, session) in inventory.sessions.iter().enumerate() {
            let permitted = inventory.capabilities.attach
                && selected_target(self.hosts.selected.as_ref(), &session.target).is_ok();
            let saved = self.hosts.selected.as_ref().is_some_and(|selected| {
                recipes
                    .iter()
                    .any(|recipe| recipe_saves_target(recipe, &selected.host, &session.target))
            });
            ui.push_id(
                (
                    "host-session",
                    session.target.logical.to_string(),
                    session.target.generation.get(),
                ),
                |ui| {
                    card(ui, |ui| {
                        ui.horizontal(|ui| {
                            let width = (ui.available_width() - 108.0).max(0.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(width, 48.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    ui.spacing_mut().item_spacing.y = 4.0;
                                    ui.label(egui::RichText::new(&session.name).strong());
                                    footnote(
                                        ui,
                                        if matches!(
                                            session.lifetime,
                                            festerm_host_protocol::Lifetime::Persistent {}
                                        ) {
                                            "Persistent session · Running"
                                        } else {
                                            "Temporary session · Running"
                                        },
                                    );
                                },
                            );
                            if labelled(
                                button_sized(
                                    ui,
                                    ButtonKind::Primary,
                                    "Connect",
                                    permitted && !busy,
                                    egui::vec2(96.0, 44.0),
                                ),
                                &format!("Connect · {}", session.name),
                                permitted && !busy,
                            )
                            .clicked()
                            {
                                *action = Some(Action::Attach(index));
                            }
                        });
                        ui.horizontal(|ui| {
                            let width = (ui.available_width() - 88.0).max(120.0);
                            ui.allocate_ui(egui::vec2(width, 44.0), |ui| {
                                if saved {
                                    footnote(ui, "Saved locally · edit in Profiles");
                                } else if button_sized(
                                    ui,
                                    ButtonKind::Secondary,
                                    "Save as profile…",
                                    !busy,
                                    egui::vec2(width, 44.0),
                                )
                                .clicked()
                                {
                                    *action = Some(Action::SaveSession(index));
                                }
                            });
                            ui.menu_button("Details", |ui| {
                                ui.label(format!(
                                    "{} · generation {}",
                                    session.target.logical,
                                    session.target.generation.get()
                                ));
                            });
                        });
                        if inventory.capabilities.explicit_end
                            && !inventory
                                .unknown
                                .iter()
                                .any(|unknown| unknown.logical == session.target.logical)
                        {
                            ui.add_space(12.0);
                            if button(ui, ButtonKind::Destructive, "Terminate…", !busy).clicked()
                            {
                                *action = Some(Action::ReviewEnd(index));
                            }
                        }
                    });
                },
            );
            ui.add_space(12.0);
        }
        if inventory.capabilities.profiles {
            for (index, profile) in inventory.profiles.iter().enumerate() {
                // An advertised profile already represented by a running session
                // is one remote item. Labels never determine identity.
                if inventory
                    .sessions
                    .iter()
                    .any(|session| session.target.logical == profile.logical)
                {
                    continue;
                }
                let unknown = inventory
                    .unknown
                    .iter()
                    .any(|target| target.logical == profile.logical);
                let permitted = !unknown
                    && inventory.capabilities.ensure_profile
                    && inventory.capabilities.attach
                    && inventory.capabilities.creation_allowed;
                ui.push_id(("host-shared", profile.logical.to_string()), |ui| {
                    card(ui, |ui| {
                        ui.label(egui::RichText::new(&profile.name).strong());
                        footnote(
                            ui,
                            if unknown {
                                "Shared shell · State unknown"
                            } else {
                                "Shared shell · Not started"
                            },
                        );
                        if labelled(
                            button(
                                ui,
                                ButtonKind::Primary,
                                "Start & Connect",
                                permitted && !busy,
                            ),
                            &format!("Start & Connect · {}", profile.name),
                            permitted && !busy,
                        )
                        .clicked()
                        {
                            *action = Some(Action::EnsureProfile(index));
                        }
                    });
                });
                ui.add_space(12.0);
            }
        }
        if let Some(recipe) = &self.hosts.selected {
            if matches!(
                recipe.behavior,
                ConnectionBehavior::ResumeOrCreate | ConnectionBehavior::AlwaysCreate
            ) {
                ui.label(format!("Session · {}", recipe.session_name()));
                egui::CollapsingHeader::new("Launch details")
                    .open(requires_launch_review(recipe).then_some(true))
                    .show(ui, |ui| {
                        ui.label(format!("Logical session · {}", recipe.logical_session));
                        ui.label(format!("Shell · {:?}", recipe.launch.shell));
                        ui.label(format!("Arguments · {:?}", recipe.launch.args));
                        ui.label(format!("Directory · {:?}", recipe.launch.cwd));
                        ui.label(format!("Lifetime · {:?}", recipe.lifetime));
                    });
                ui.checkbox(
                    &mut self.hosts.launch,
                    "Confirm this session and saved launch settings",
                );
                let allowed = match recipe.behavior {
                    ConnectionBehavior::ResumeOrCreate => {
                        inventory.capabilities.ensure && inventory.capabilities.attach
                    }
                    _ => self.hosts.quick_creation_allowed(),
                };
                let label = if self.hosts.selected_original.is_some() {
                    "Retry original action"
                } else {
                    "Connect saved session"
                };
                if primary(
                    ui,
                    label,
                    allowed && self.hosts.launch && self.hosts.original_store.is_some() && !busy,
                ) {
                    *action = Some(Action::Create);
                }
            }
        }
    }

    fn show_host_quick_form(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        ui.label("Host");
        ui.label(selected_host_title(&self.hosts));
        let frozen = self.hosts.selected.as_ref().is_some_and(|recipe| {
            self.hosts
                .originals
                .iter()
                .any(|original| original.recipe.id == recipe.id)
        });
        ui.label("Session name");
        let enabled = !frozen && !self.hosts.busy();
        ui.add_enabled_ui(enabled, |ui| {
            host_field(
                ui,
                "New session name",
                egui::TextEdit::singleline(&mut self.hosts.quick_name)
                    .hint_text("Session name")
                    .char_limit(128),
                false,
            );
        });
        ui.label("Shell");
        ui.label("Host default shell");
        footnote(ui, "Persistent session. Disconnecting leaves it running.");
        ui.add_enabled_ui(enabled, |ui| {
            if ui
                .checkbox(&mut self.hosts.quick_save, "Save as a local profile")
                .changed()
                && self.hosts.quick_save
                && self.hosts.quick_profile_name.is_empty()
            {
                self.hosts.quick_profile_name = self.hosts.quick_name.clone();
            }
            if self.hosts.quick_save {
                ui.label("Profile name");
                host_field(
                    ui,
                    "New shell local profile name",
                    egui::TextEdit::singleline(&mut self.hosts.quick_profile_name).char_limit(200),
                    false,
                );
            }
        });
        if frozen {
            footnote(ui, "Retry keeps the original request and session identity.");
        }
        let review = (!self.hosts.launch)
            .then(|| {
                self.hosts.selected.as_ref().and_then(|recipe| {
                    self.hosts.originals.iter().position(|original| {
                        original.recipe.id == recipe.id && !original.intent.is_presented()
                    })
                })
            })
            .flatten();
        if let Some(index) = review {
            if primary(ui, "Review original action", !self.hosts.busy()) {
                *action = Some(Action::ReviewOriginal(index));
            }
        } else if primary(
            ui,
            if frozen {
                "Retry Create & Connect"
            } else {
                "Create & Connect"
            },
            self.hosts.valid_quick_name()
                && (!self.hosts.quick_save
                    || self.hosts.selected.as_ref().is_some_and(|base| {
                        PublicRecipe::persistent(
                            self.hosts.quick_profile_name.trim(),
                            base.host.clone(),
                            Recipe::default(),
                        )
                        .is_ok()
                    }))
                && !self.hosts.busy()
                && self.hosts.quick_creation_allowed(),
        ) {
            *action = Some(Action::QuickCreate);
        }
        if secondary(ui, "Cancel", !self.hosts.busy()) {
            *action = Some(Action::CancelEdit);
        }
    }

    fn show_session_save_form(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        let busy = self.hosts.busy();
        let Some(review) = self.hosts.session_save.as_mut() else {
            return;
        };
        ui.label(review.recipe.session_name());
        footnote(
            ui,
            if matches!(
                review.recipe.lifetime,
                festerm_host_protocol::Lifetime::Persistent {}
            ) {
                "Persistent session"
            } else {
                "Temporary session"
            },
        );
        footnote(ui, &format!("@{}", review.account.login));
        if let Some(host) = &review.recipe.host_hint {
            footnote(ui, &host.name);
        }
        ui.push_id(("save-session", review.recipe.id.as_str()), |ui| {
            ui.label("Profile name");
            host_field(
                ui,
                "Session local profile name",
                egui::TextEdit::singleline(&mut review.recipe.name).char_limit(200),
                false,
            );
        });
        footnote(
            ui,
            "Adds a shortcut to this session in this device's Profiles.",
        );
        if primary(
            ui,
            "Save profile",
            review.recipe.validate().is_ok() && !busy,
        ) {
            *action = Some(Action::ConfirmSessionSave);
        }
        if secondary(ui, "Cancel", !busy) {
            *action = Some(Action::CancelSessionSave);
        }
    }

    fn show_host_profile_form(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        if let Some(recipe) = &self.hosts.selected {
            let host = recipe
                .host_hint
                .as_ref()
                .map(|hint| hint.name.as_str())
                .or_else(|| {
                    self.hosts
                        .hosts
                        .iter()
                        .find(|host| host.locator == self.hosts.locator)
                        .map(host_display_name)
                })
                .unwrap_or("Saved host");
            let AccountReference::Github { account_id } = recipe.host.account;
            let login = self
                .host_account()
                .filter(|account| account.account_id == account_id)
                .map(|account| format!("@{}", account.login))
                .unwrap_or_else(|| format!("GitHub account {account_id} · sign in from Settings"));
            ui.label(format!("Host · {host}"));
            ui.label(login);
            ui.collapsing("Bound identity", |ui| {
                ui.label(format!("Host ID · {}", recipe.host.id));
                ui.label(format!("GitHub account ID · {account_id}"));
                ui.label(format!("Logical session · {}", recipe.logical_session));
            });
        }
        if secondary(ui, "Choose another host…", !self.hosts.busy()) {
            *action = Some(Action::ChooseProfileHost);
        }
        ui.label("Profile name");
        host_field(
            ui,
            "Profile name",
            egui::TextEdit::singleline(&mut self.hosts.profile_name).hint_text("Profile name"),
            false,
        );
        ui.checkbox(
            &mut self.hosts.create_if_missing,
            "Create session if missing",
        );
        if self.hosts.create_if_missing {
            ui.label("Session name");
            if host_field(
                ui,
                "Profile session name",
                egui::TextEdit::singleline(&mut self.hosts.session_name)
                    .hint_text("Session name")
                    .char_limit(128),
                false,
            )
            .changed()
            {
                self.hosts.reconcile_profile_session_name();
            }
        } else {
            section_header(ui, "Existing session", None, false);
            if let Some(inventory) = &self.hosts.inventory {
                for (index, session) in inventory.sessions.iter().enumerate() {
                    if ui
                        .selectable_value(
                            &mut self.hosts.existing_session,
                            Some(index),
                            &session.name,
                        )
                        .clicked()
                    {
                        self.hosts.session_name = session.name.clone();
                    }
                }
            } else {
                footnote(
                    ui,
                    "The saved session identity is kept. List authorized sessions to choose another.",
                );
            }
        }
        let valid = self.hosts.profile_draft().is_ok();
        if primary(ui, "Save profile", valid && !self.hosts.busy()) {
            *action = Some(Action::SaveProfile);
        }
        if secondary(ui, "Cancel", true) {
            *action = Some(Action::CancelEdit);
        }
    }
}
