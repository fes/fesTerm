//! Phone-width client navigation. Only returned actions can start HOST work.
use super::*;
use crate::widgets::{
    button, button_sized, card as surface_card, footnote, labelled, paint_fit, section_header,
    ButtonKind,
};

fn card<R>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    surface_card(ui, |ui| {
        egui::Frame::new()
            .inner_margin(egui::Margin::same(8))
            .show(ui, contents)
            .inner
    })
}

fn navigation(ui: &mut egui::Ui, title: &str) -> bool {
    ui.horizontal(|ui| {
        let done = button_sized(
            ui,
            ButtonKind::Secondary,
            "Done",
            true,
            egui::vec2(64.0, 44.0),
        );
        ui.add_sized(
            egui::vec2(ui.available_width(), 44.0),
            egui::Label::new(egui::RichText::new(title).heading()).truncate(),
        );
        done.clicked()
    })
    .inner
}

pub(super) fn host_disclosure(
    ui: &mut egui::Ui,
    name: &str,
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
    labelled(
        response,
        &format!("{} {name}", if expanded { "Collapse" } else { "Expand" }),
        enabled,
    )
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
        let mut selection = None;
        let mut saved_connect = None;
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
                        ui.spacing_mut().item_spacing.x = 8.0;
                        egui::Frame::new()
                            .inner_margin(egui::Margin::same(8))
                            .show(ui, |ui| {
                                if navigation(
                                    ui,
                                    if self.hosts.editing {
                                        "Dev Tunnel profile"
                                    } else {
                                        "Find & connect"
                                    },
                                ) {
                                    action = Some(Action::Close);
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
                                if self.hosts.quick_new {
                                    self.show_host_quick_form(ui, &mut action);
                                } else {
                                    section_header(ui, "GitHub accounts", None, false);
                                    if accounts.is_empty() {
                                        footnote(ui, "Sign in to find your hosts.");
                                    }
                                    for (index, account) in accounts.iter().enumerate() {
                                        let find = if self.hosts.inventory.is_some() {
                                            secondary
                                        } else {
                                            primary
                                        };
                                        if find(
                                            ui,
                                            &format!("Find hosts · @{}", account.login),
                                            !self.hosts.busy(),
                                        ) {
                                            action = Some(Action::FindAccount(index));
                                        }
                                    }
                                    if secondary(ui, "Manage accounts in Settings", true) {
                                        action = Some(Action::Settings);
                                    }
                                    if !recipes.is_empty() {
                                        section_header(ui, "Saved profiles", None, false);
                                        card(ui, |ui| {
                                            for recipe in &recipes {
                                                if primary(
                                                    ui,
                                                    &format!("Connect · {}", recipe.name),
                                                    !self.hosts.busy(),
                                                ) {
                                                    saved_connect =
                                                        Some(recipe.id.as_str().to_owned());
                                                }
                                                if secondary(
                                                    ui,
                                                    &format!("Edit · {}", recipe.name),
                                                    !self.hosts.busy(),
                                                ) {
                                                    selection = Some(recipe.clone());
                                                }
                                            }
                                        });
                                    }
                                    if self.hosts.discovered && self.hosts.hosts.is_empty() {
                                        footnote(
                                            ui,
                                            "No hosts found. Your saved profiles are unchanged.",
                                        );
                                    }
                                    let hosts = self.hosts.hosts.clone();
                                    if !hosts.is_empty() {
                                        if let Some(account) = accounts
                                            .iter()
                                            .find(|account| account.id == self.hosts.account)
                                        {
                                            section_header(
                                                ui,
                                                &format!("Hosts · @{}", account.login),
                                                None,
                                                false,
                                            );
                                        }
                                    }
                                    for (index, host) in hosts.iter().enumerate() {
                                        let selected = self.hosts.locator == host.locator
                                            && !self.hosts.host_collapsed;
                                        let name = host.name.as_deref().unwrap_or(&host.locator);
                                        card(ui, |ui| {
                                            if host_disclosure(
                                                ui,
                                                name,
                                                selected,
                                                !self.hosts.busy(),
                                            )
                                            .clicked()
                                            {
                                                action = Some(Action::SelectHost(index));
                                            }
                                            if selected {
                                                self.show_selected_host(ui, &mut action);
                                            }
                                        });
                                    }
                                    if !self.hosts.locator.is_empty()
                                        && !hosts
                                            .iter()
                                            .any(|host| host.locator == self.hosts.locator)
                                    {
                                        section_header(ui, "Selected host", None, false);
                                        let title = self
                                            .hosts
                                            .selected
                                            .as_ref()
                                            .and_then(|recipe| recipe.host_hint.as_ref())
                                            .map(|hint| hint.name.as_str())
                                            .unwrap_or(&self.hosts.locator);
                                        ui.label(title);
                                        self.show_selected_host(ui, &mut action);
                                    } else if self.hosts.selected.is_some()
                                        && self.hosts.locator.is_empty()
                                    {
                                        footnote(
                            ui,
                            "Choose this profile's host, or enter its locator in Details.",
                        );
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
                                        ui.text_edit_singleline(&mut self.hosts.locator);
                                        if secondary(ui, "Review another host", !self.hosts.busy())
                                        {
                                            action = Some(Action::NewReview);
                                        }
                                        self.show_host_recovery(ui, &mut action);
                                        ui.collapsing("Import public profiles", |ui| {
                                            ui.add(
                                                egui::TextEdit::multiline(&mut self.hosts.import)
                                                    .char_limit(64 * 1024),
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
        if let Some(id) = saved_connect {
            self.connect_saved_host_profile(ctx, &id);
            return;
        }
        if let Some(recipe) = selection {
            self.select_host_recipe(recipe);
            self.hosts.begin_profile_edit();
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
        if let Some(action) = action {
            self.hosts.error = None;
            if let Err(error) = self.host_action(ctx, action) {
                self.hosts.error = Some(error);
            }
        }
    }

    fn show_selected_host(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        let busy = self.hosts.busy();
        if self.hosts.saved_connect.is_some() {
            ui.label("Connecting saved profile…");
            return;
        }
        if self.hosts.selected.is_some() && secondary(ui, "Save / edit profile", !busy) {
            *action = Some(Action::EditProfile);
        }
        if self.hosts.selected.is_some() && secondary(ui, "New profile for this host", !busy) {
            *action = Some(Action::NewProfile);
        }
        if self.hosts.receipt.is_none() {
            ui.label("Verify host");
            footnote(
                ui,
                "Use a protected approval, or prepare a key for approval on the host.",
            );
            if self.hosts.selected.is_some() && primary(ui, "Load protected approval", !busy) {
                *action = Some(Action::Load);
            }
            ui.collapsing("First connection · advanced", |ui| {
                ui.label("Independent host fingerprint");
                ui.text_edit_singleline(&mut self.hosts.fingerprint);
                ui.label("Host owner binding (JSON)");
                ui.add(
                    egui::TextEdit::multiline(&mut *self.hosts.prerequisite)
                        .hint_text("Owner binding from this host")
                        .char_limit(4096),
                );
                ui.checkbox(
                    &mut self.hosts.trust,
                    "I independently verified this host, account and owner binding",
                );
                if primary(ui, "Prepare owner key", self.hosts.trust && !busy) {
                    *action = Some(Action::Prepare);
                }
            });
            return;
        }
        if !self.hosts.enrolled {
            let receipt = self.hosts.receipt.as_ref().expect("checked receipt");
            ui.label("Approve this key on the host");
            ui.collapsing("Owner public key", |ui| {
                let key = receipt
                    .public_key()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                ui.label(&key);
                if let Ok(fingerprint) = owner_key_fingerprint(&receipt.public_key()) {
                    ui.label(fingerprint);
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
        let inventory = self.hosts.inventory.as_ref().expect("checked inventory");
        if inventory.sessions.is_empty() {
            footnote(ui, "No authorized sessions listed.");
        }
        if !inventory.unknown.is_empty() {
            footnote(ui, "Some session states are unknown.");
        }
        for (index, session) in inventory.sessions.iter().enumerate() {
            let permitted = inventory.capabilities.attach
                && selected_target(self.hosts.selected.as_ref(), &session.target).is_ok();
            let connect = if self.hosts.saved
                && permitted
                && self.hosts.selected.as_ref().is_some_and(|recipe| {
                    matches!(
                        recipe.behavior,
                        ConnectionBehavior::AttachOnly { .. } | ConnectionBehavior::ResumeOnly {}
                    )
                }) {
                primary
            } else {
                secondary
            };
            if connect(
                ui,
                &format!("Connect · {}", session.name),
                permitted && !busy,
            ) {
                *action = Some(Action::Attach(index));
            }
            ui.collapsing(format!("Session details · {}", session.name), |ui| {
                ui.label(format!(
                    "{} · generation {}",
                    session.target.logical,
                    session.target.generation.get()
                ));
                if secondary(
                    ui,
                    "End session…",
                    inventory.capabilities.explicit_end && !busy,
                ) {
                    *action = Some(Action::ReviewEnd(index));
                }
            });
        }
        if self.hosts.quick_creation_allowed() && secondary(ui, "+ New session", !busy) {
            *action = Some(Action::QuickNew);
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
        if self.hosts.original_store.is_none() {
            footnote(ui, "Check protected retry history before Create or End.");
            if secondary(ui, "Check protected history", !busy) {
                *action = Some(Action::RecoverOriginals);
            }
        }
        if secondary(ui, "Refresh sessions", !busy) {
            *action = Some(Action::Resolve);
        }
    }

    fn show_host_quick_form(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        section_header(ui, "New session", None, false);
        let frozen = self.hosts.selected.as_ref().is_some_and(|recipe| {
            self.hosts
                .originals
                .iter()
                .any(|original| original.recipe.id == recipe.id)
        });
        ui.label("Session name");
        ui.add_enabled(
            !frozen && !self.hosts.busy(),
            egui::TextEdit::singleline(&mut self.hosts.quick_name)
                .hint_text("Session name")
                .char_limit(128),
        );
        footnote(ui, "Durable shell. Disconnecting leaves it running.");
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
                "Retry create and connect"
            } else {
                "Create and connect"
            },
            self.hosts.valid_quick_name()
                && !self.hosts.busy()
                && self.hosts.quick_creation_allowed(),
        ) {
            *action = Some(Action::QuickCreate);
        }
        if secondary(ui, "Cancel", !self.hosts.busy()) {
            *action = Some(Action::CancelEdit);
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
                        .and_then(|host| host.name.as_deref())
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
        ui.label("Profile name");
        ui.add(egui::TextEdit::singleline(&mut self.hosts.profile_name).hint_text("Profile name"));
        ui.checkbox(
            &mut self.hosts.create_if_missing,
            "Create session if missing",
        );
        if self.hosts.create_if_missing {
            ui.label("Session name");
            ui.add(
                egui::TextEdit::singleline(&mut self.hosts.session_name)
                    .hint_text("Session name")
                    .char_limit(128),
            );
            footnote(
                ui,
                "Durable session. Creation permission is checked on Connect.",
            );
            footnote(
                ui,
                "Uses this profile's logical session identity, not a global name match.",
            );
        } else {
            section_header(ui, "Existing session", None, false);
            if let Some(inventory) = &self.hosts.inventory {
                for (index, session) in inventory.sessions.iter().enumerate() {
                    ui.selectable_value(
                        &mut self.hosts.existing_session,
                        Some(index),
                        &session.name,
                    );
                }
            } else {
                footnote(ui, "The saved session identity is kept. List authorized sessions to choose another.");
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
