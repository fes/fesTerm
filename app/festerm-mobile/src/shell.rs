//! Tab shell: Profiles / Sessions / Settings screens and the All-sessions sheet
//! (mock-ups 05 and 06). Screens return actions; the app owns side effects.

use super::model::{Profile, Reattachable, SurfaceState, SurfaceSummary};
use super::profiles::TrustedHostRow;
use super::terminal_keyboard::KeyboardKind;
use super::theme::{color, radius, space, type_scale};
use super::widgets::{
    self, bottom_sheet, button, card, edit_zone_clicked, footnote, labelled, list_row,
    list_row_height, paint_fit, screen_title, search_field, section_header, sheet_title, swipe_row,
    ButtonKind, Dot, RowSpec, TabItem, Trailing,
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
    /// A saved profile lifted by touch-and-hold, following the finger.
    pub reorder: Option<ProfileDrag>,
}

#[derive(Clone, Debug)]
pub struct ProfileDrag {
    id: String,
    /// Where the finger holds the row, measured from the row's top.
    grab: f32,
    /// The finger's last known height. A lifted touch reports no position,
    /// so the drop uses this.
    finger: f32,
}

/// Finger distance from the list's edge that scrolls it while reordering.
const REORDER_EDGE: f32 = 56.0;

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
    pub text_size: TextSize,
    pub default_keyboard: KeyboardKind,
}

/// Terminal text size preset. Pinch-zoom still adjusts a session freely; this
/// sets the size every terminal starts at.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextSize {
    Small,
    #[default]
    Normal,
    Large,
}

impl TextSize {
    pub const ALL: [TextSize; 3] = [TextSize::Small, TextSize::Normal, TextSize::Large];

    pub fn points(self) -> f32 {
        match self {
            Self::Small => 9.5,
            Self::Normal => 11.5,
            Self::Large => 14.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Normal => "Normal",
            Self::Large => "Large",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Normal => "normal",
            Self::Large => "large",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|size| size.key() == key.trim())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShellAction {
    NewProfile,
    OpenProfile(String),
    EditProfile(String),
    ForgetHost(usize),
    OpenSurface(u64),
    CloseSurface(u64),
    Reattach(usize),
    OpenDemo,
    OpenHostSetup,
    ShowSessions,
    HideSessions,
    OpenProfilesTab,
    Lock(super::app_lock::LockAction),
    SetTextSize(TextSize),
    SetDefaultKeyboard(KeyboardKind),
    /// Move a saved profile before another, or to the end when `before` is
    /// `None`.
    MoveProfile {
        id: String,
        before: Option<String>,
    },
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
            let swipe = swipe_row(
                ui,
                ui.id().with(("surface-row", surface.id)),
                "Close",
                list_row_height(&row),
                |ui| list_row(ui, &row),
            );
            if swipe.action {
                actions.push(ShellAction::CloseSurface(surface.id));
            } else if swipe.opened {
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
                    ui.add_space(space::SM);
                    if button(ui, ButtonKind::Secondary, "Open offline demo", true).clicked() {
                        actions.push(ShellAction::OpenDemo);
                    }
                });
            });
            ui.add_space(space::LG);
        });
    } else if visible.is_empty() {
        footnote(ui, "No profiles match your search.");
    } else {
        saved_profile_rows(ui, state, &visible, needle.is_empty(), actions);
    }
    if !data.profiles.is_empty() {
        footnote(
            ui,
            if data.profiles_persistent {
                "Profiles are saved on this iPhone. Tap Edit to change or delete one; \
                 touch and hold one to drag it into a new order."
            } else {
                "Credentials last only until fesTerm closes; profiles are saved on this iPhone."
            },
        );
    }
}

fn profile_row<'a>(profile: &'a Profile, divider: bool) -> RowSpec<'a> {
    let mut row = RowSpec::new(&profile.name);
    row.icon = Some(profile.kind.glyph());
    row.subtitle = Some(&profile.detail);
    row.trailing = Trailing::Edit;
    row.divider = divider;
    row
}

/// The saved-profile list. Touch and hold lifts a row; it follows the finger
/// (scrolling the list near its edges) and drops where the insertion line
/// shows. Reordering is unavailable while a search filters the list.
fn saved_profile_rows(
    ui: &mut Ui,
    state: &mut ShellState,
    visible: &[&Profile],
    reorderable: bool,
    actions: &mut Vec<ShellAction>,
) {
    if !reorderable
        || state
            .reorder
            .as_ref()
            .is_some_and(|drag| !visible.iter().any(|profile| profile.id == drag.id))
    {
        state.reorder = None;
    }
    let lifted = state.reorder.as_ref().map(|drag| drag.id.clone());
    let mut slots: Vec<(usize, Rect)> = Vec::with_capacity(visible.len());
    let card_rect = card(ui, |ui| {
        for (index, profile) in visible.iter().enumerate() {
            let row = profile_row(profile, index > 0);
            if lifted.as_deref() == Some(profile.id.as_str()) {
                // Keep the lifted row's slot so the list does not jump.
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), widgets::list_row_height(&row)),
                    Sense::hover(),
                );
                slots.push((index, rect));
                continue;
            }
            let response = list_row(ui, &row);
            slots.push((index, response.rect));
            if lifted.is_some() {
                continue;
            }
            let (long, tap) = if reorderable {
                widgets::long_press_or_click(ui, &response)
            } else {
                (false, response.clicked())
            };
            if long {
                let finger = ui
                    .input(|input| input.pointer.latest_pos())
                    .map_or(response.rect.center().y, |pos| pos.y);
                state.reorder = Some(ProfileDrag {
                    id: profile.id.clone(),
                    grab: finger - response.rect.top(),
                    finger,
                });
                ui.ctx().request_repaint();
            } else if tap && edit_zone_clicked(&response) {
                actions.push(ShellAction::EditProfile(profile.id.clone()));
            } else if tap {
                actions.push(ShellAction::OpenProfile(profile.id.clone()));
            }
        }
        ui.min_rect()
    });
    let Some(mut drag) = state.reorder.clone() else {
        return;
    };
    let Some(from) = visible.iter().position(|profile| profile.id == drag.id) else {
        state.reorder = None;
        return;
    };
    // Lifting a touch releases the button and forgets the pointer in the same
    // frame; a cancelled touch forgets it without a release.
    let (down, position) = ui.input(|input| {
        (
            input.pointer.primary_down(),
            input.pointer.latest_pos().map(|pos| pos.y),
        )
    });
    if down && position.is_none() {
        state.reorder = None;
        return;
    }
    if let Some(y) = position {
        drag.finger = y;
    }
    let finger = drag.finger;
    // The drop slot among the other rows: before the first whose middle is
    // below the finger.
    let others: Vec<(usize, Rect)> = slots
        .iter()
        .copied()
        .filter(|(index, _)| *index != from)
        .collect();
    let slot = others
        .iter()
        .position(|(_, rect)| finger < rect.center().y)
        .unwrap_or(others.len());
    if !down {
        state.reorder = None;
        if slot != from {
            actions.push(ShellAction::MoveProfile {
                id: drag.id,
                before: others
                    .get(slot)
                    .map(|(index, _)| visible[*index].id.clone()),
            });
        }
        return;
    }
    state.reorder = Some(drag.clone());
    let painter = ui.painter().with_clip_rect(ui.clip_rect());
    if slot != from {
        let y = others
            .get(slot)
            .map_or_else(|| card_rect.bottom(), |(_, rect)| rect.top());
        painter.hline(
            (card_rect.left() + 8.0)..=(card_rect.right() - 8.0),
            y,
            Stroke::new(3.0, color::PRIMARY),
        );
    }
    let row = profile_row(visible[from], false);
    let height = widgets::list_row_height(&row);
    let top = (finger - drag.grab).clamp(
        card_rect.top() - height / 2.0,
        card_rect.bottom() - height / 2.0,
    );
    egui::Area::new(egui::Id::new("profile-reorder-lift"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(card_rect.left(), top))
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(color::SURFACE_RAISED)
                .stroke(Stroke::new(1.0, color::BORDER))
                .corner_radius(CornerRadius::same(radius::CARD))
                .shadow(egui::Shadow {
                    offset: [0, 6],
                    blur: 18,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(140),
                })
                .show(ui, |ui| {
                    ui.set_width(card_rect.width() - 2.0);
                    list_row(ui, &row);
                });
        });
    let view = ui.clip_rect();
    let edge = if finger < view.top() + REORDER_EDGE {
        (view.top() + REORDER_EDGE - finger).min(REORDER_EDGE)
    } else if finger > view.bottom() - REORDER_EDGE {
        -(finger - (view.bottom() - REORDER_EDGE)).min(REORDER_EDGE)
    } else {
        0.0
    };
    if edge != 0.0 {
        ui.scroll_with_delta(egui::vec2(0.0, edge * 0.25));
    }
    ui.ctx().request_repaint();
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
        footnote(
            ui,
            "Tap an open surface to switch without reconnecting. Swipe left to close it.",
        );
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
    section_header(ui, "Terminal", None, false);
    let mut index = TextSize::ALL
        .iter()
        .position(|size| *size == data.text_size)
        .unwrap_or(1);
    let items = TextSize::ALL.map(|size| (None, size.label()));
    if widgets::segmented(ui, &items, &mut index, false) {
        actions.push(ShellAction::SetTextSize(TextSize::ALL[index]));
    }
    footnote(
        ui,
        "Text size for terminals. Pinch to zoom a single session.",
    );
    section_header(ui, "Default keyboard", None, false);
    card(ui, |ui| {
        for (index, kind) in KeyboardKind::ALL.into_iter().enumerate() {
            let mut row = RowSpec::new(kind.label());
            row.selected = data.default_keyboard == kind;
            row.trailing = if row.selected {
                Trailing::Check
            } else {
                Trailing::None
            };
            row.divider = index > 0;
            if list_row(ui, &row).clicked() {
                actions.push(ShellAction::SetDefaultKeyboard(kind));
            }
        }
    });
    footnote(
        ui,
        "Used when fesTerm starts. Switching from More > Keyboard or the globe key also changes this default.",
    );
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
    if state.tab != Tab::Profiles {
        state.reorder = None;
    }
    // A lifted profile owns the finger; the list must not also scroll by drag.
    let scroll_source = if state.reorder.is_some() {
        egui::scroll_area::ScrollSource {
            drag: egui::scroll_area::DragScroll::Never,
            ..Default::default()
        }
    } else {
        egui::scroll_area::ScrollSource::default()
    };
    egui::ScrollArea::vertical()
        .id_salt(("shell-tab", state.tab.index()))
        .scroll_source(scroll_source)
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
        footnote(
            ui,
            "Tap an open surface to switch without reconnecting. Swipe left to close it.",
        );
        if button(ui, ButtonKind::Primary, "+  Open a profile", true).clicked() {
            actions.push(ShellAction::OpenProfilesTab);
        }
    });
    if outcome.dismissed {
        actions.push(ShellAction::HideSessions);
    }
    actions
}
