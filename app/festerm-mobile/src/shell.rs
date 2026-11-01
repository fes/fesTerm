//! Tab shell: Profiles / Sessions / Settings screens and the All-sessions sheet
//! (mock-ups 05 and 06). Screens return actions; the app owns side effects.

use super::model::{Profile, Reattachable, SurfaceState, SurfaceSummary};
use super::profiles::TrustedHostRow;
use super::theme::{color, radius, space, type_scale};
use super::widgets::{
    self, bottom_sheet, button, card, edit_zone_clicked, footnote, labelled, list_row, paint_fit,
    screen_title, search_field, section_header, sheet_title, ButtonKind, Dot, RowSpec, TabItem,
    Trailing,
};
use eframe::egui::{self, Align2, CornerRadius, Margin, Rect, Sense, Stroke, StrokeKind, Ui};
use festerm_ui_egui::icon::Icon;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Tab {
    #[default]
    Profiles,
    Sessions,
    Settings,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Profiles, Tab::Sessions, Tab::Settings];

    fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ShellState {
    pub tab: Tab,
    pub search: String,
    pub sessions_sheet: bool,
}

pub struct ShellData<'a> {
    pub profiles: &'a [Profile],
    /// Most recently launched profiles, for "Continue working".
    pub recents: &'a [Profile],
    pub surfaces: &'a [SurfaceSummary],
    pub reattachable: &'a [Reattachable],
    pub profiles_persistent: bool,
    pub trusted_hosts: &'a [TrustedHostRow],
    pub diagnostics: &'a [(String, String)],
    pub notice: Option<&'a str>,
    /// App-lock settings, when the host has a lock.
    pub lock: Option<super::app_lock::LockView>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShellAction {
    NewProfile,
    OpenProfile(String),
    EditProfile(String),
    ForgetHost(usize),
    OpenSurface(u64),
    Reattach(usize),
    OpenDemo,
    OpenHostSetup,
    ShowSessions,
    HideSessions,
    OpenProfilesTab,
    Lock(super::app_lock::LockAction),
}

fn state_dot(state: SurfaceState) -> Dot {
    match state {
        SurfaceState::Connected => Dot::Connected,
        SurfaceState::Connecting => Dot::Warning,
        SurfaceState::Disconnected => Dot::Idle,
    }
}

fn surface_rows(ui: &mut Ui, surfaces: &[SurfaceSummary], actions: &mut Vec<ShellAction>) {
    card(ui, |ui| {
        for (index, surface) in surfaces.iter().enumerate() {
            let mut row = RowSpec::new(&surface.title);
            row.icon = Some(surface.kind.glyph());
            row.badge = surface.current.then_some("Current");
            row.subtitle = Some(&surface.detail);
            row.status = Some((state_dot(surface.state), &surface.status));
            row.trailing = if surface.current {
                Trailing::Check
            } else {
                Trailing::Chevron
            };
            row.selected = surface.current;
            row.divider = index > 0;
            if list_row(ui, &row).clicked() {
                actions.push(ShellAction::OpenSurface(surface.id));
            }
        }
    });
}

fn reattach_rows(ui: &mut Ui, reattachable: &[Reattachable], actions: &mut Vec<ShellAction>) {
    card(ui, |ui| {
        for (index, item) in reattachable.iter().enumerate() {
            let mut row = RowSpec::new(&item.title);
            row.icon = Some(Icon::Reattach);
            row.subtitle = Some(&item.detail);
            row.trailing = Trailing::Action("Reattach");
            row.divider = index > 0;
            if list_row(ui, &row).clicked() {
                actions.push(ShellAction::Reattach(index));
            }
        }
    });
}

fn continue_cards(
    ui: &mut Ui,
    recents: &[Profile],
    surfaces: &[SurfaceSummary],
    actions: &mut Vec<ShellAction>,
) {
    let gap = ui.spacing().item_spacing.x;
    let width = (ui.available_width() - gap) / 2.0;
    ui.horizontal(|ui| {
        for profile in recents.iter().take(2) {
            let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 112.0), Sense::CLICK);
            let painter = ui.painter();
            let corner = CornerRadius::same(radius::CARD);
            painter.rect_filled(
                rect,
                corner,
                if response.is_pointer_button_down_on() {
                    color::SURFACE_RAISED
                } else {
                    color::SURFACE
                },
            );
            painter.rect_stroke(
                rect,
                corner,
                Stroke::new(1.0, color::DIVIDER),
                StrokeKind::Inside,
            );
            widgets::icon_tile(
                painter,
                Rect::from_min_size(rect.min + egui::vec2(12.0, 12.0), egui::vec2(36.0, 36.0)),
                profile.kind.glyph(),
                color::ACCENT_TEXT,
            );
            paint_fit(
                painter,
                egui::pos2(rect.left() + 12.0, rect.top() + 62.0),
                Align2::LEFT_CENTER,
                &profile.name,
                type_scale::font(type_scale::BODY),
                color::TEXT,
                width - 24.0,
            );
            let (protocol, _) = profile
                .detail
                .split_once(" · ")
                .unwrap_or((&profile.detail, ""));
            let open = surfaces.iter().any(|surface| {
                surface.title == profile.name && surface.state != SurfaceState::Disconnected
            });
            let summary = if open {
                format!("{protocol} · open")
            } else {
                format!("{protocol} · saved")
            };
            paint_fit(
                painter,
                egui::pos2(rect.left() + 12.0, rect.top() + 80.0),
                Align2::LEFT_CENTER,
                &summary,
                type_scale::font(type_scale::MICRO + 0.5),
                color::TEXT_MUTED,
                width - 24.0,
            );
            painter.hline(
                (rect.left() + 12.0)..=(rect.right() - 12.0),
                rect.top() + 90.0,
                Stroke::new(1.0, color::DIVIDER),
            );
            let open = painter.text(
                egui::pos2(rect.left() + 12.0, rect.bottom() - 11.0),
                Align2::LEFT_CENTER,
                if open {
                    "Switch to session"
                } else {
                    "Open terminal"
                },
                type_scale::font(type_scale::MICRO + 0.5),
                color::ACCENT_TEXT,
            );
            widgets::paint_chevron(
                painter,
                egui::pos2(open.right() + 8.0, open.center().y),
                3.0,
                color::ACCENT_TEXT,
            );
            if labelled(response, &format!("Open {}", profile.name), true).clicked() {
                actions.push(ShellAction::OpenProfile(profile.id.clone()));
            }
        }
    });
}

fn profiles_screen(
    ui: &mut Ui,
    state: &mut ShellState,
    data: &ShellData<'_>,
    actions: &mut Vec<ShellAction>,
) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| screen_title(ui, "Profiles", "Your connections, on this iPhone"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (rect, response) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), Sense::CLICK);
            widgets::paint_plus(ui.painter(), rect.center(), 10.0, color::TEXT);
            if labelled(response, "New profile", true).clicked() {
                actions.push(ShellAction::NewProfile);
            }
        });
    });
    search_field(ui, &mut state.search, "Find a profile");
    if let Some(notice) = data.notice {
        ui.add(
            egui::Label::new(
                egui::RichText::new(notice)
                    .color(color::DANGER)
                    .size(type_scale::CAPTION + 0.5),
            )
            .wrap(),
        );
    }
    if !data.recents.is_empty() && state.search.trim().is_empty() {
        let trailing =
            (!data.surfaces.is_empty()).then(|| format!("All sessions {}", data.surfaces.len()));
        if section_header(
            ui,
            "Continue working",
            trailing.as_deref(),
            trailing.is_some(),
        )
        .is_some_and(|response| response.clicked())
        {
            actions.push(ShellAction::ShowSessions);
        }
        continue_cards(ui, data.recents, data.surfaces, actions);
    }
    let needle = state.search.trim().to_lowercase();
    let visible: Vec<&Profile> = data
        .profiles
        .iter()
        .filter(|profile| {
            needle.is_empty()
                || profile.name.to_lowercase().contains(&needle)
                || profile.detail.to_lowercase().contains(&needle)
        })
        .collect();
    let count = format!(
        "{} profile{}",
        data.profiles.len(),
        if data.profiles.len() == 1 { "" } else { "s" }
    );
    section_header(ui, "Saved profiles", Some(&count), false);
    if data.profiles.is_empty() {
        card(ui, |ui| {
            ui.add_space(space::LG);
            ui.horizontal(|ui| {
                ui.add_space(space::LG);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - space::LG);
                    ui.label(
                        egui::RichText::new("No saved profiles yet")
                            .size(type_scale::BODY)
                            .strong(),
                    );
                    footnote(ui, "Create an SSH profile to connect.");
                    ui.add_space(space::XS);
                    if button(ui, ButtonKind::Primary, "New profile", true).clicked() {
                        actions.push(ShellAction::NewProfile);
                    }
                    if true
                        && button(ui, ButtonKind::Secondary, "Open offline demo", true).clicked()
                    {
                        actions.push(ShellAction::OpenDemo);
                    }
                });
            });
            ui.add_space(space::LG);
        });
    } else if visible.is_empty() {
        footnote(ui, "No profiles match your search.");
    } else {
        card(ui, |ui| {
            for (index, profile) in visible.iter().enumerate() {
                let mut row = RowSpec::new(&profile.name);
                row.icon = Some(profile.kind.glyph());
                row.subtitle = Some(&profile.detail);
                row.trailing = Trailing::Edit;
                row.divider = index > 0;
                let response = list_row(ui, &row);
                if edit_zone_clicked(&response) {
                    actions.push(ShellAction::EditProfile(profile.id.clone()));
                } else if response.clicked() {
                    actions.push(ShellAction::OpenProfile(profile.id.clone()));
                }
            }
        });
    }
    if !data.profiles.is_empty() {
        footnote(
            ui,
            if data.profiles_persistent {
                "Profiles are saved on this iPhone. Tap Edit to change or delete one."
            } else {
                "Credentials last only until fesTerm closes; profiles are saved on this iPhone."
            },
        );
    }
}

fn sessions_screen(ui: &mut Ui, data: &ShellData<'_>, actions: &mut Vec<ShellAction>) {
    screen_title(ui, "Sessions", "Open terminals and files");
    section_header(
        ui,
        "Open surfaces",
        Some(&data.surfaces.len().to_string()),
        false,
    );
    if data.surfaces.is_empty() {
        footnote(ui, "Nothing is open. Choose a profile to start a session.");
        if button(ui, ButtonKind::Primary, "Open a profile", true).clicked() {
            actions.push(ShellAction::OpenProfilesTab);
        }
    } else {
        surface_rows(ui, data.surfaces, actions);
        footnote(ui, "Tap an open surface to switch without reconnecting.");
    }
    if !data.reattachable.is_empty() {
        section_header(ui, "Available to reattach", None, false);
        reattach_rows(ui, data.reattachable, actions);
    }
}

fn settings_screen(ui: &mut Ui, data: &ShellData<'_>, actions: &mut Vec<ShellAction>) {
    screen_title(ui, "Settings", "fesTerm on this iPhone");
    if true {
        section_header(ui, "Demo", None, false);
        card(ui, |ui| {
            let mut row = RowSpec::new("Offline demo terminal");
            row.icon = Some(Icon::LocalTerminal);
            row.subtitle = Some("Local fixture; input is discarded");
            row.trailing = Trailing::Chevron;
            if list_row(ui, &row).clicked() {
                actions.push(ShellAction::OpenDemo);
            }
        });
    }
    section_header(ui, "Dev Tunnel hosts", None, false);
    card(ui, |ui| {
        let mut row = RowSpec::new("Set up a host");
        row.icon = Some(Icon::DevTunnel);
        row.subtitle = Some("Review and copy the one-line installer");
        row.trailing = Trailing::Chevron;
        if list_row(ui, &row).clicked() {
            actions.push(ShellAction::OpenHostSetup);
        }
    });
    section_header(
        ui,
        "Trusted hosts",
        Some(&data.trusted_hosts.len().to_string()),
        false,
    );
    if data.trusted_hosts.is_empty() {
        footnote(ui, "Host keys you trust appear here.");
    } else {
        card(ui, |ui| {
            for (index, host) in data.trusted_hosts.iter().enumerate() {
                let title = format!("{}:{}", host.host, host.port);
                let mut row = RowSpec::new(&title);
                row.icon = Some(Icon::Settings);
                row.subtitle = Some(&host.fingerprint);
                row.trailing = Trailing::Action("Forget");
                row.divider = index > 0;
                if list_row(ui, &row).clicked() {
                    actions.push(ShellAction::ForgetHost(index));
                }
            }
        });
        footnote(
            ui,
            "Forgetting a key makes fesTerm ask again on the next connection.",
        );
    }
    if let Some(lock) = &data.lock {
        actions.extend(
            super::app_lock::settings_section(ui, lock)
                .into_iter()
                .map(ShellAction::Lock),
        );
    }
    section_header(ui, "Diagnostics", None, false);
    card(ui, |ui| {
        for (index, (name, value)) in data.diagnostics.iter().enumerate() {
            let mut row = RowSpec::new(name);
            row.trailing = Trailing::Text(value);
            row.divider = index > 0;
            ui.add_enabled_ui(false, |ui| list_row(ui, &row));
        }
    });
    section_header(ui, "About", None, false);
    card(ui, |ui| {
        let mut row = RowSpec::new("fesTerm");
        row.trailing = Trailing::Text(env!("CARGO_PKG_VERSION"));
        ui.add_enabled_ui(false, |ui| list_row(ui, &row));
    });
}

/// Tab content plus the bottom tab bar. Returns the actions chosen this frame.
pub fn show_tabs(ui: &mut Ui, state: &mut ShellState, data: &ShellData<'_>) -> Vec<ShellAction> {
    let mut actions = Vec::new();
    let tabs = [
        TabItem {
            glyph: Icon::SavedProfiles,
            label: "Profiles",
            badge: 0,
        },
        TabItem {
            glyph: Icon::RunningSessions,
            label: "Sessions",
            badge: data.surfaces.len(),
        },
        TabItem {
            glyph: Icon::Settings,
            label: "Settings",
            badge: 0,
        },
    ];
    egui::Panel::bottom("shell-tab-bar")
        .frame(egui::Frame::new().fill(color::BAR))
        .show_separator_line(false)
        .show(ui, |ui| {
            if let Some(index) = widgets::tab_bar(ui, &tabs, state.tab.index()) {
                state.tab = Tab::ALL[index];
            }
        });
    egui::ScrollArea::vertical()
        .id_salt(("shell-tab", state.tab.index()))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(Margin::symmetric(16, 0))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = space::SM;
                    match state.tab {
                        Tab::Profiles => profiles_screen(ui, state, data, &mut actions),
                        Tab::Sessions => sessions_screen(ui, data, &mut actions),
                        Tab::Settings => settings_screen(ui, data, &mut actions),
                    }
                    ui.add_space(space::LG);
                });
        });
    actions
}

/// All-sessions sheet (mock-up 05).
pub fn sessions_sheet(ctx: &egui::Context, bounds: Rect, data: &ShellData<'_>) -> Vec<ShellAction> {
    let mut actions = Vec::new();
    let outcome = bottom_sheet(ctx, "all-sessions", bounds, |ui| {
        if sheet_title(ui, "Sessions", Some("Done")) {
            actions.push(ShellAction::HideSessions);
        }
        section_header(
            ui,
            "Open surfaces",
            Some(&data.surfaces.len().to_string()),
            false,
        );
        surface_rows(ui, data.surfaces, &mut actions);
        if !data.reattachable.is_empty() {
            let host = data.reattachable.first().map(|item| item.host.clone());
            section_header(ui, "Available to reattach", host.as_deref(), false);
            reattach_rows(ui, data.reattachable, &mut actions);
        }
        footnote(ui, "Tap an open surface to switch without reconnecting.");
        if button(ui, ButtonKind::Primary, "+  Open a profile", true).clicked() {
            actions.push(ShellAction::OpenProfilesTab);
        }
    });
    if outcome.dismissed {
        actions.push(ShellAction::HideSessions);
    }
    actions
}
