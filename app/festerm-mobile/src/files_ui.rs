//! Files screen (mock-ups 07-09): stacked Remote and This iPhone panes, the
//! transfers sheet and the overwrite decision sheet.

use super::files::{
    format_date, format_size, is_markdown, ConflictView, FilesEvent, FilesSession, Link, Pane,
    RowState, TransferRow,
};
use super::theme::{color, radius, space, type_scale};
use super::widgets::{
    bottom_sheet, button, button_sized, card, footnote, labelled, paint_chevron, paint_fit,
    sheet_title, ButtonKind,
};
use eframe::egui::{self, Align2, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Ui};
use festerm_sftp::{ConflictPolicy, Direction, Entry, EntryKind, SortKey};

const ROW_HEIGHT: f32 = 50.0;
const PANE_CHROME: f32 = 44.0 + 44.0 + 32.0;

#[derive(Default)]
pub(crate) struct FilesOutcome {
    pub(crate) retry: bool,
    pub(crate) events: Vec<FilesEvent>,
}

fn folder_glyph(painter: &egui::Painter, rect: Rect, tint: egui::Color32) {
    let body = Rect::from_min_max(
        egui::pos2(rect.left() + 1.0, rect.top() + rect.height() * 0.28),
        egui::pos2(rect.right() - 1.0, rect.bottom() - 1.0),
    );
    let stroke = Stroke::new(1.6, tint);
    painter.rect_stroke(body, CornerRadius::same(3), stroke, StrokeKind::Inside);
    painter.line_segment(
        [
            egui::pos2(rect.left() + 1.0, body.top()),
            egui::pos2(rect.left() + 1.0, rect.top() + 2.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.left() + 1.0, rect.top() + 2.0),
            egui::pos2(rect.left() + rect.width() * 0.42, rect.top() + 2.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.left() + rect.width() * 0.42, rect.top() + 2.0),
            egui::pos2(rect.left() + rect.width() * 0.52, body.top()),
        ],
        stroke,
    );
}

fn file_glyph(painter: &egui::Painter, rect: Rect, tint: egui::Color32) {
    let page = Rect::from_center_size(
        rect.center(),
        egui::vec2(rect.width() * 0.72, rect.height()),
    );
    let stroke = Stroke::new(1.6, tint);
    painter.rect_stroke(page, CornerRadius::same(3), stroke, StrokeKind::Inside);
    for fraction in [0.42, 0.6, 0.78] {
        let y = page.top() + page.height() * fraction;
        painter.line_segment(
            [
                egui::pos2(page.left() + 4.0, y),
                egui::pos2(page.right() - 4.0, y),
            ],
            Stroke::new(1.2, tint.gamma_multiply(0.7)),
        );
    }
}

fn arrow_glyph(painter: &egui::Painter, center: Pos2, up: bool, tint: egui::Color32) {
    let stroke = Stroke::new(1.8, tint);
    let direction = if up { -1.0 } else { 1.0 };
    painter.line_segment(
        [
            egui::pos2(center.x, center.y - 6.0 * direction),
            egui::pos2(center.x, center.y + 6.0 * direction),
        ],
        stroke,
    );
    let tip = egui::pos2(center.x, center.y + 6.0 * direction);
    painter.line_segment(
        [tip, egui::pos2(tip.x - 4.5, tip.y - 4.5 * direction)],
        stroke,
    );
    painter.line_segment(
        [tip, egui::pos2(tip.x + 4.5, tip.y - 4.5 * direction)],
        stroke,
    );
}

fn filter_glyph(painter: &egui::Painter, center: Pos2, tint: egui::Color32) {
    let stroke = Stroke::new(1.8, tint);
    for (half, y) in [(8.0, -5.0), (5.0, 0.0), (2.0, 5.0)] {
        painter.line_segment(
            [
                egui::pos2(center.x - half, center.y + y),
                egui::pos2(center.x + half, center.y + y),
            ],
            stroke,
        );
    }
}

fn icon_button(
    ui: &mut Ui,
    label: &str,
    active: bool,
    paint: impl FnOnce(&egui::Painter, Pos2, egui::Color32),
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), Sense::CLICK);
    paint(
        ui.painter(),
        rect.center(),
        if active {
            color::ACCENT_TEXT
        } else {
            color::TEXT_MUTED
        },
    );
    labelled(response, label, ui.is_enabled()).on_hover_text(label)
}

fn tint_for(entry: &Entry) -> egui::Color32 {
    let lower = entry.display_name.to_lowercase();
    if entry.kind == EntryKind::Other {
        color::TEXT_MUTED
    } else if entry.is_dir() {
        color::ACCENT_TEXT
    } else if is_markdown(&lower) {
        color::SUCCESS
    } else if [".gz", ".tar", ".zip", ".tgz", ".xz", ".bz2"]
        .iter()
        .any(|ext| lower.ends_with(ext))
    {
        color::WARNING
    } else {
        color::TEXT_MUTED
    }
}

/// Returns `(tapped, checked_zone)` for one directory row.
fn entry_row(ui: &mut Ui, entry: &Entry, selected: bool, select_folder: bool) -> egui::Response {
    let available = entry.kind != EntryKind::Other;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_HEIGHT),
        if available {
            Sense::CLICK
        } else {
            Sense::hover()
        },
    );
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    if selected {
        painter.rect_filled(rect, 0.0, color::SELECTED);
    } else if response.is_pointer_button_down_on() {
        painter.rect_filled(rect, 0.0, color::SURFACE_RAISED);
    }
    painter.hline(
        rect.x_range(),
        rect.bottom(),
        Stroke::new(1.0, color::DIVIDER),
    );
    let glyph = Rect::from_center_size(
        egui::pos2(rect.left() + 28.0, rect.center().y),
        egui::vec2(22.0, 22.0),
    );
    let tint = tint_for(entry);
    if !available {
        festerm_ui_egui::icon::paint(&painter, festerm_ui_egui::icon::Icon::Error, glyph, tint);
    } else if entry.is_dir() {
        folder_glyph(&painter, glyph, tint);
    } else {
        file_glyph(&painter, glyph, tint);
    }
    let mut right = rect.right() - space::MD;
    if entry.is_dir() {
        paint_chevron(
            &painter,
            egui::pos2(right - 4.0, rect.center().y),
            5.0,
            color::TEXT_MUTED,
        );
    } else if selected {
        let disc = egui::pos2(right - 10.0, rect.center().y);
        painter.circle_filled(disc, 10.0, color::PRIMARY);
        super::widgets::check_mark(&painter, disc, color::ON_PRIMARY);
        right -= 28.0;
    } else if available {
        painter.circle_stroke(
            egui::pos2(right - 10.0, rect.center().y),
            10.0,
            Stroke::new(1.5, color::BORDER),
        );
        right -= 28.0;
    }
    if select_folder {
        right -= 56.0;
    }
    let name = if entry.name_safe {
        entry.display_name.as_str()
    } else {
        "(unsafe name)"
    };
    paint_fit(
        &painter,
        egui::pos2(rect.left() + 52.0, rect.center().y - 9.0),
        Align2::LEFT_CENTER,
        name,
        type_scale::font(type_scale::BODY),
        color::TEXT,
        right - rect.left() - 60.0,
    );
    let mut detail = Vec::new();
    if !available {
        detail.push("Unavailable".to_owned());
    } else if entry.is_dir() {
        detail.push("Folder".to_owned());
    } else if let Some(size) = entry.size {
        detail.push(format_size(size));
    }
    if let Some(modified) = entry.modified {
        detail.push(format_date(modified));
    }
    if entry.symlink.is_some() {
        detail.push("link".to_owned());
    }
    paint_fit(
        &painter,
        egui::pos2(rect.left() + 52.0, rect.center().y + 10.0),
        Align2::LEFT_CENTER,
        &detail.join(" · "),
        type_scale::font(type_scale::MICRO + 0.5),
        color::TEXT_MUTED,
        right - rect.left() - 60.0,
    );
    if available {
        labelled(response, name, true)
    } else {
        labelled(
            response.on_hover_text("Unavailable"),
            &format!("{name} — Unavailable"),
            false,
        )
    }
}

fn crumb_row(ui: &mut Ui, pane: &mut Pane, root_label: &str, ctx: &egui::Context) {
    let crumbs = pane.crumbs(root_label);
    let last = crumbs.len().saturating_sub(1);
    let mut goto = None;
    egui::ScrollArea::horizontal()
        .id_salt(("crumbs", root_label))
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .stick_to_right(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (index, (label, path)) in crumbs.iter().enumerate() {
                    if index > 0 {
                        ui.label(
                            egui::RichText::new(
                                if pane.fs.kind() == festerm_sftp::PaneKind::Remote
                                    && root_label != "/"
                                    && root_label != "Shared folder"
                                {
                                    if root_label.contains('\\') {
                                        "\\"
                                    } else {
                                        "/"
                                    }
                                } else {
                                    "›"
                                },
                            )
                            .size(type_scale::CAPTION)
                            .color(color::TEXT_FAINT),
                        );
                    }
                    let text = egui::RichText::new(label)
                        .size(type_scale::CAPTION + 1.0)
                        .color(if index == last {
                            color::TEXT
                        } else {
                            color::ACCENT_TEXT
                        });
                    if ui.add(egui::Label::new(text).sense(Sense::CLICK)).clicked() && index != last
                    {
                        goto = Some(path.clone());
                    }
                }
            });
        });
    if let Some(path) = goto {
        pane.go_to(&path, ctx);
    }
}

fn footer_text(pane: &Pane) -> String {
    let count = pane.visible().len();
    let items = format!("{count} item{}", if count == 1 { "" } else { "s" });
    let selected_files = pane.selected_files();
    let files = selected_files.len();
    let unknown_size = selected_files.iter().any(|entry| entry.size.is_none());
    let selected = pane.selected.len();
    if selected == 0 {
        format!("{items} · No selection")
    } else if selected != files || unknown_size {
        // Folder contents and unreported file sizes do not establish a total.
        format!("{items} · {selected} selected")
    } else if selected == 1 {
        format!(
            "{items} · 1 selected · {}",
            format_size(pane.selected_bytes())
        )
    } else {
        format!(
            "{items} · {selected} selected · {}",
            format_size(pane.selected_bytes())
        )
    }
}

/// One pane card. Returns whether "Open" (Markdown) was tapped.
#[derive(Clone, Copy, Default)]
struct PaneControls {
    create: bool,
    select_folders: bool,
    delete: Option<bool>,
}

#[derive(Default)]
struct PaneOutcome {
    open: bool,
    create: bool,
    delete: bool,
}

fn pane_card(
    ui: &mut Ui,
    pane: &mut Pane,
    title: &str,
    root_label: &str,
    height: f32,
    ctx: &egui::Context,
    controls: PaneControls,
) -> PaneOutcome {
    let mut outcome = PaneOutcome::default();
    let select_folders = controls.select_folders;
    let size = egui::vec2(ui.available_width(), height);
    let layout = egui::Layout::top_down(egui::Align::Min);
    ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt(title)
            .max_rect(
                ui.available_rect_before_wrap()
                    .with_max_y(ui.cursor().top() + size.y)
                    .with_max_x(ui.cursor().left() + size.x),
            )
            .layout(layout),
        |ui| {
            ui.set_min_height(height);
            ui.set_max_height(height);
            card(ui, |ui| {
                ui.set_min_height(height - 2.0);
                ui.horizontal(|ui| {
                    ui.set_height(44.0);
                    ui.add_space(space::MD);
                    ui.label(
                        egui::RichText::new(title)
                            .size(type_scale::NAV_TITLE)
                            .strong()
                            .color(color::TEXT),
                    );
                    if pane.loading() {
                        ui.add(egui::Spinner::new().size(14.0));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(4.0);
                        let menu =
                            icon_button(ui, "Sort and view options", false, |painter, c, tint| {
                                for dx in [-6.0, 0.0, 6.0] {
                                    painter.circle_filled(egui::pos2(c.x + dx, c.y), 1.8, tint);
                                }
                            });
                        egui::Popup::menu(&menu).show(|ui| {
                            if let Some(enabled) = controls.delete {
                                if ui
                                    .add_enabled(
                                        enabled,
                                        egui::Button::new("Delete selected")
                                            .min_size(egui::vec2(0.0, 44.0)),
                                    )
                                    .clicked()
                                {
                                    outcome.delete = true;
                                    ui.close();
                                }
                                ui.separator();
                            }
                            for (key, label) in [
                                (SortKey::Name, "Sort by name"),
                                (SortKey::Size, "Sort by size"),
                                (SortKey::Modified, "Sort by date"),
                            ] {
                                if ui.selectable_label(pane.sort == key, label).clicked() {
                                    pane.set_sort(key, ctx);
                                    ui.close();
                                }
                            }
                            if ui
                                .checkbox(&mut pane.show_hidden, "Show hidden files")
                                .changed()
                            {
                                pane.refresh(ctx);
                                ui.close();
                            }
                        });
                        let filter =
                            icon_button(ui, "Filter", pane.filter_open, |painter, c, tint| {
                                filter_glyph(painter, c, tint);
                            });
                        if filter.clicked() {
                            pane.filter_open = !pane.filter_open;
                            if !pane.filter_open {
                                pane.filter.clear();
                            }
                        }
                        let label = if pane.fs.kind() == festerm_sftp::PaneKind::Remote {
                            "New remote folder"
                        } else {
                            "New local folder"
                        };
                        let create = ui
                            .add_enabled_ui(controls.create, |ui| {
                                icon_button(ui, label, false, |painter, c, tint| {
                                    festerm_ui_egui::icon::paint(
                                        painter,
                                        festerm_ui_egui::icon::Icon::NewFolder,
                                        Rect::from_center_size(c, egui::vec2(20.0, 20.0)),
                                        tint,
                                    );
                                })
                            })
                            .inner;
                        outcome.create = create.clicked();
                        let refresh = icon_button(ui, "Refresh", false, |painter, c, tint| {
                            festerm_ui_egui::icon::paint(
                                painter,
                                festerm_ui_egui::icon::Icon::Refresh,
                                Rect::from_center_size(c, egui::vec2(20.0, 20.0)),
                                tint,
                            );
                        });
                        if refresh.clicked() {
                            pane.refresh(ctx);
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.set_height(30.0);
                    ui.add_space(space::MD);
                    let up = ui
                        .add_enabled_ui(pane.can_go_up(), |ui| {
                            icon_button(ui, "Parent folder", false, |painter, c, tint| {
                                arrow_glyph(painter, c, true, tint);
                            })
                        })
                        .inner;
                    if up.clicked() {
                        pane.go_up(ctx);
                    }
                    crumb_row(ui, pane, root_label, ctx);
                });
                if pane.filter_open {
                    ui.horizontal(|ui| {
                        ui.add_space(space::MD);
                        ui.add(
                            egui::TextEdit::singleline(&mut pane.filter)
                                .hint_text("Filter this folder")
                                .desired_width(ui.available_width() - space::MD)
                                .char_limit(80),
                        );
                    });
                }
                ui.painter().hline(
                    ui.min_rect().x_range(),
                    ui.cursor().top(),
                    Stroke::new(1.0, color::DIVIDER),
                );
                let list_height =
                    (height - PANE_CHROME - if pane.filter_open { 36.0 } else { 0.0 })
                        .max(ROW_HEIGHT);
                let mut tapped = None;
                egui::ScrollArea::vertical()
                    .id_salt(("rows", title))
                    .max_height(list_height)
                    .min_scrolled_height(list_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if let Some(error) = &pane.error {
                            ui.add_space(space::SM);
                            ui.horizontal(|ui| {
                                ui.add_space(space::MD);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(error)
                                            .size(type_scale::CAPTION)
                                            .color(color::DANGER),
                                    )
                                    .wrap(),
                                );
                            });
                        }
                        let entries = pane.visible();
                        if entries.is_empty() && pane.listing.is_some() && !pane.loading() {
                            ui.add_space(space::MD);
                            ui.horizontal(|ui| {
                                ui.add_space(space::MD);
                                footnote(ui, "This folder is empty");
                            });
                        }
                        for entry in entries {
                            let selected = pane.selected.contains(&entry.name);
                            let select_folder = select_folders
                                && entry.is_dir()
                                && entry.name_safe
                                && entry.symlink.is_none();
                            let row = entry_row(ui, entry, selected, select_folder);
                            let mut select = false;
                            if select_folder {
                                let rect = Rect::from_center_size(
                                    egui::pos2(row.rect.right() - 48.0, row.rect.center().y),
                                    egui::vec2(44.0, 44.0),
                                );
                                let mut checked = selected;
                                select = labelled(
                                    ui.put(rect, egui::Checkbox::without_text(&mut checked)),
                                    &format!("Select folder {}", entry.display_name),
                                    true,
                                )
                                .clicked();
                            }
                            if (select || row.clicked()) && entry.name_safe {
                                tapped = Some((entry.name.clone(), entry.is_dir() && !select));
                            }
                        }
                    });
                if let Some((name, dir)) = tapped {
                    if dir {
                        pane.open_dir(&name, ctx);
                    } else {
                        pane.toggle(&name);
                    }
                }
                ui.horizontal(|ui| {
                    ui.set_height(32.0);
                    ui.add_space(space::MD);
                    ui.label(
                        egui::RichText::new(footer_text(pane))
                            .size(type_scale::CAPTION)
                            .color(color::TEXT_MUTED),
                    );
                    let single_markdown = pane.selected_files().len() == 1
                        && pane
                            .selected_files()
                            .first()
                            .is_some_and(|entry| is_markdown(&entry.name));
                    if single_markdown {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(space::MD);
                            if ui
                                .add(
                                    egui::Label::new(
                                        egui::RichText::new("Open")
                                            .size(type_scale::CAPTION + 1.0)
                                            .color(color::ACCENT_TEXT),
                                    )
                                    .sense(Sense::CLICK),
                                )
                                .clicked()
                            {
                                outcome.open = true;
                            }
                        });
                    }
                });
            });
        },
    );
    outcome
}

fn transfer_summary(session: &FilesSession) -> (String, bool) {
    let active = session.active_count();
    if active == 0 {
        ("No active transfers".to_owned(), false)
    } else {
        (
            format!(
                "{active} active transfer{}",
                if active == 1 { "" } else { "s" }
            ),
            true,
        )
    }
}

/// Upload / Download buttons for the selections on either pane.
fn transfer_buttons(ui: &mut Ui, session: &mut FilesSession) {
    let upload_count = session
        .local
        .as_ref()
        .map_or(0, |pane| pane.selected_files().len());
    let download_count = session
        .remote
        .as_ref()
        .map_or(0, |pane| pane.selected_files().len());
    ui.horizontal(|ui| {
        let gap = ui.spacing().item_spacing.x;
        let width = (ui.available_width() - gap) / 2.0;
        let size = egui::vec2(width, 48.0);
        let upload = if upload_count == 0 {
            "Upload".to_owned()
        } else {
            format!("Upload {upload_count}")
        };
        let download = if download_count == 0 {
            "Download".to_owned()
        } else {
            format!("Download {download_count}")
        };
        let up = button_sized(
            ui,
            ButtonKind::Primary,
            &upload,
            upload_count > 0 && session.remote_write,
            size,
        );
        let down = button_sized(
            ui,
            ButtonKind::Secondary,
            &download,
            download_count > 0,
            size,
        );
        if up.clicked() {
            session.start_transfer(true);
        }
        if down.clicked() {
            session.start_transfer(false);
        }
    });
}

/// Panes switch from stacked to side by side at this width (points).
const WIDE_FILES_WIDTH: f32 = 700.0;
const FILES_MAX_WIDTH: f32 = 1100.0;

/// The Files screen body below the header.
pub(crate) fn show(ui: &mut Ui, session: &mut FilesSession) -> FilesOutcome {
    #[cfg(feature = "host-clients")]
    session.clear_retired_delete_recovery();
    let mut outcome = FilesOutcome::default();
    let ctx = ui.ctx().clone();
    // Pane minimum heights may expand max_rect; sheets belong to the original
    // visible viewport, including its existing native-keyboard clip.
    let modal_bounds = ui.max_rect().intersect(ui.clip_rect());
    let remote_root_label = session.remote_root_label();
    #[cfg(feature = "host-clients")]
    let select_folders = session.deletion_available();
    #[cfg(not(feature = "host-clients"))]
    let select_folders = false;
    if !session.is_ready() {
        connecting_body(ui, session, &mut outcome);
        return outcome;
    }
    ui.spacing_mut().item_spacing.y = space::SM;
    let width = ui.available_width();
    let wide = width >= WIDE_FILES_WIDTH;
    let side = ((width - FILES_MAX_WIDTH) / 2.0).clamp(16.0, 200.0) as i8;
    let margin = egui::Margin::symmetric(side, 0);
    egui::ScrollArea::vertical()
        .id_salt("files-short-viewport")
        .max_height(modal_bounds.height())
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new().inner_margin(margin).show(ui, |ui| {
                if let Some(notice) = session.notice.clone() {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&notice)
                                    .size(type_scale::CAPTION)
                                    .color(color::WARNING),
                            )
                            .wrap(),
                        );
                    });
                }
                #[cfg(feature = "host-clients")]
                if let Some(path) = session.deletion_recovery() {
                    footnote(ui, &format!("Recovery copy in {}: {path}. Keep this copy until recovery is complete.", crate::platform_text("This iPhone", "This device")));
                }
                #[cfg(feature = "host-clients")]
                let delete = select_folders.then(|| session.can_review_delete());
                #[cfg(not(feature = "host-clients"))]
                let delete = None;
                let remote_controls = PaneControls {
                    create: session.remote_write,
                    select_folders,
                    delete,
                };
                #[cfg(feature = "host-clients")]
                let local_delete = session
                    .local_deletion_available()
                    .then(|| session.can_review_delete_side(true));
                #[cfg(not(feature = "host-clients"))]
                let local_delete: Option<bool> = None;
                let local_controls = PaneControls {
                    create: true,
                    select_folders: local_delete.is_some(),
                    delete: local_delete,
                };
                let (mut remote, mut local) = (PaneOutcome::default(), PaneOutcome::default());
                // Keep the lists in the actual visible body. On a very short screen the
                // outer scroll area retains usable rows and 44-point controls.
                let remaining = (modal_bounds.bottom() - ui.cursor().top()).max(0.0);
                if wide {
                    let pane_height = (remaining - 48.0 - 52.0 - 2.0 * space::SM)
                        .clamp(PANE_CHROME + ROW_HEIGHT + 2.0, 720.0);
                    let gap = space::MD;
                    let half = (ui.available_width() - gap) / 2.0;
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        ui.vertical(|ui| {
                            ui.set_width(half);
                            if let Some(pane) = session.remote.as_mut() {
                                remote = pane_card(
                                    ui,
                                    pane,
                                    "Remote",
                                    &remote_root_label,
                                    pane_height,
                                    &ctx,
                                    remote_controls,
                                );
                            }
                        });
                        ui.vertical(|ui| {
                            ui.set_width(half);
                            if let Some(pane) = session.local.as_mut() {
                                local = pane_card(
                                    ui,
                                    pane,
                                    "This device",
                                    "Documents",
                                    pane_height,
                                    &ctx,
                                    local_controls,
                                );
                            }
                        });
                    });
                    transfer_buttons(ui, session);
                } else {
                    let pane_height = ((remaining - 48.0 - 52.0 - 3.0 * space::SM) / 2.0)
                        .clamp(PANE_CHROME + ROW_HEIGHT + 2.0, 520.0);
                    if let Some(pane) = session.remote.as_mut() {
                        remote = pane_card(
                            ui,
                            pane,
                            "Remote",
                            &remote_root_label,
                            pane_height,
                            &ctx,
                            remote_controls,
                        );
                    }
                    transfer_buttons(ui, session);
                    if let Some(pane) = session.local.as_mut() {
                        local = pane_card(
                            ui,
                            pane,
                            super::platform_text("This iPhone", "This device"),
                            "Documents",
                            pane_height,
                            &ctx,
                            local_controls,
                        );
                    }
                }
                if remote.create {
                    session.new_folder(true);
                }
                if local.create {
                    session.new_folder(false);
                }
                #[cfg(feature = "host-clients")]
                if remote.delete {
                    session.review_delete();
                }
                #[cfg(feature = "host-clients")]
                if local.delete {
                    session.review_delete_side(true);
                }
                if remote.open {
                    outcome.events.extend(session.open_markdown(true));
                }
                if local.open {
                    outcome.events.extend(session.open_markdown(false));
                }
                let (summary, active) = transfer_summary(session);
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 52.0), Sense::CLICK);
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(radius::CARD), color::SURFACE);
                ui.painter().rect_stroke(
                    rect,
                    CornerRadius::same(radius::CARD),
                    Stroke::new(1.0, color::DIVIDER),
                    StrokeKind::Inside,
                );
                paint_fit(
                    ui.painter(),
                    egui::pos2(rect.left() + 14.0, rect.center().y - 8.0),
                    Align2::LEFT_CENTER,
                    "Transfers",
                    type_scale::font(type_scale::BODY),
                    color::TEXT,
                    rect.width() - 60.0,
                );
                paint_fit(
                    ui.painter(),
                    egui::pos2(rect.left() + 14.0, rect.center().y + 10.0),
                    Align2::LEFT_CENTER,
                    &summary,
                    type_scale::font(type_scale::CAPTION),
                    if active {
                        color::ACCENT_TEXT
                    } else {
                        color::TEXT_MUTED
                    },
                    rect.width() - 60.0,
                );
                paint_chevron(
                    ui.painter(),
                    egui::pos2(rect.right() - 20.0, rect.center().y),
                    5.0,
                    color::TEXT_MUTED,
                );
                if labelled(response, "Transfers", true).clicked() {
                    session.transfers_sheet = true;
                }
            });
        });
    if session.folder.is_some() {
        let mut create = false;
        let mut cancel = false;
        let result = bottom_sheet(&ctx, "files-new-folder", modal_bounds, |ui| {
            let Some(draft) = session.folder.as_mut() else {
                return;
            };
            sheet_title(ui, "New folder", None);
            footnote(
                ui,
                if draft.remote {
                    "Create in the selected remote folder"
                } else {
                    "Create in the selected local folder"
                },
            );
            ui.add_enabled_ui(!draft.busy(), |ui| {
                let width = ui.available_width();
                super::widgets::text_field(ui, "Folder name", &mut draft.name, "New folder", width);
            });
            if let Some(error) = &draft.error {
                footnote(ui, error);
            }
            if draft.busy() {
                ui.add(egui::Spinner::new());
                footnote(ui, "Creating folder…");
            }
            create = button(
                ui,
                ButtonKind::Primary,
                "Create folder",
                !draft.busy() && !draft.name.is_empty(),
            )
            .clicked();
            cancel = button(ui, ButtonKind::Secondary, "Cancel", !draft.busy()).clicked();
        });
        if create {
            session.create_folder(&ctx);
        }
        if cancel
            || (result.dismissed && !session.folder.as_ref().is_some_and(|draft| draft.busy()))
        {
            session.folder = None;
        }
    }
    #[cfg(feature = "host-clients")]
    delete_sheet(&ctx, modal_bounds, session);
    outcome
}

#[cfg(feature = "host-clients")]
fn delete_sheet(ctx: &egui::Context, bounds: Rect, session: &mut FilesSession) {
    if session.deletion.is_none() {
        return;
    }
    let mut confirm = false;
    let mut cancel = false;
    let result = bottom_sheet(ctx, "files-reviewed-delete", bounds, |ui| {
        let draft = session.deletion.as_ref().expect("open review");
        sheet_title(ui, "Delete selected entries?", None);
        let folder = if draft.local() {
            let relative = draft.path().trim_start_matches('/');
            if relative.is_empty() {
                "Documents".to_owned()
            } else {
                format!("Documents/{relative}")
            }
        } else {
            let root = session.remote_root_label();
            let root = if root.is_empty() {
                "Shared folder"
            } else {
                &root
            };
            let relative = draft.path().trim_start_matches('/');
            if relative.is_empty() {
                root.to_owned()
            } else {
                format!("{}/{relative}", root.trim_end_matches('/'))
            }
        };
        let label = if draft.local() {
            crate::platform_text("This iPhone", "This device")
        } else {
            "Remote"
        };
        footnote(
            ui,
            &festerm_sftp::path::display_name(&format!("{label}: {folder}")),
        );
        footnote(
            ui,
            "Only the reviewed files or empty folders will be removed. This cannot be undone.",
        );
        if let Some(review) = &draft.review {
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .show(ui, |ui| {
                    for (index, name) in review.names.iter().enumerate() {
                        ui.label(name);
                        if draft.local() {
                            if let Some(detail) = review
                                .local_details
                                .as_ref()
                                .and_then(|items| items.get(index))
                            {
                                let label = match detail {
                                    crate::files::deletion::LocalDetail::File(size) => {
                                        format!("File · {}", format_size(*size))
                                    }
                                    crate::files::deletion::LocalDetail::EmptyFolder => {
                                        "Empty folder".to_owned()
                                    }
                                };
                                footnote(ui, &label);
                            }
                        }
                    }
                });
        } else {
            ui.add(egui::Spinner::new());
            footnote(ui, "Reviewing selected entries…");
        }
        if draft.committing() {
            footnote(ui, "Deleting…");
        }
        confirm = button(
            ui,
            ButtonKind::Primary,
            "Delete reviewed entries",
            draft.ready(),
        )
        .clicked();
        cancel = button(ui, ButtonKind::Secondary, "Cancel", !draft.committing()).clicked();
    });
    if confirm {
        session.commit_delete();
    }
    if cancel
        || (result.dismissed
            && !session
                .deletion
                .as_ref()
                .is_some_and(|draft| draft.committing()))
    {
        session.cancel_delete();
    }
}

fn connecting_body(ui: &mut Ui, session: &mut FilesSession, outcome: &mut FilesOutcome) {
    ui.add_space(space::XL * 2.0);
    ui.vertical_centered(|ui| match &session.link {
        Link::Failed(message) => {
            ui.label(
                egui::RichText::new("Could not open files")
                    .size(type_scale::TITLE)
                    .strong()
                    .color(color::TEXT),
            );
            ui.add_space(space::SM);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(message)
                        .size(type_scale::BODY)
                        .color(color::TEXT_MUTED),
                )
                .wrap(),
            );
            #[cfg(feature = "host-clients")]
            if let Some(result) = session
                .deletion_result()
                .filter(|result| !message.contains(result))
            {
                footnote(ui, &format!("Last deletion: {result}"));
            }
            ui.add_space(space::LG);
            ui.allocate_ui(egui::vec2(220.0, 48.0), |ui| {
                if button(ui, ButtonKind::Primary, "Reconnect", true).clicked() {
                    outcome.retry = true;
                }
            });
        }
        Link::Connecting(status) => {
            ui.add(egui::Spinner::new().size(28.0));
            ui.add_space(space::MD);
            ui.label(
                egui::RichText::new(format!("{status}…"))
                    .size(type_scale::BODY)
                    .color(color::TEXT_MUTED),
            );
        }
        _ => {
            ui.label(
                egui::RichText::new("Waiting for your confirmation")
                    .size(type_scale::BODY)
                    .color(color::TEXT_MUTED),
            );
        }
    });
}

fn progress_bar(ui: &mut Ui, fraction: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(3), color::SURFACE_RAISED);
    let filled = Rect::from_min_size(
        rect.min,
        egui::vec2(rect.width() * fraction.clamp(0.0, 1.0), rect.height()),
    );
    ui.painter()
        .rect_filled(filled, CornerRadius::same(3), color::PRIMARY);
}

fn state_label(row: &TransferRow) -> (String, egui::Color32) {
    match row.state {
        RowState::Queued => ("Queued".to_owned(), color::TEXT_MUTED),
        RowState::Active => ("Transferring".to_owned(), color::ACCENT_TEXT),
        RowState::Awaiting => ("Needs a decision".to_owned(), color::WARNING),
        RowState::Done => ("Done".to_owned(), color::SUCCESS),
        RowState::Skipped => ("Skipped".to_owned(), color::TEXT_MUTED),
        RowState::Cancelled => ("Cancelled".to_owned(), color::TEXT_MUTED),
        RowState::Failed => (
            row.error.clone().unwrap_or_else(|| "Failed".to_owned()),
            color::DANGER,
        ),
    }
}

fn eta_text(row: &TransferRow) -> String {
    let mut text = format!("{}/s", format_size(row.rate as u64));
    if let Some(seconds) = row.eta_seconds() {
        text.push_str(&format!(" · {seconds}s left"));
    }
    text
}

/// Transfers sheet (mock-up 08). Returns a transfer to cancel, if any.
pub(crate) fn transfers_sheet(ctx: &egui::Context, bounds: Rect, session: &mut FilesSession) {
    let mut close = false;
    let mut cancel = None;
    let mut clear = false;
    let rows = session.rows.clone();
    let active = rows.iter().filter(|row| !row.state.finished()).count();
    let outcome = bottom_sheet(ctx, "transfers", bounds, |ui| {
        let title = if rows.is_empty() {
            "Transfers".to_owned()
        } else {
            format!("Transfers {}", rows.len())
        };
        close |= sheet_title(ui, &title, Some("Done"));
        if rows.is_empty() {
            footnote(ui, "Uploads and downloads you start appear here.");
        }
        for row in rows.iter().rev() {
            card(ui, |ui| {
                ui.add_space(space::SM);
                ui.horizontal(|ui| {
                    ui.add_space(space::MD);
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width() - space::MD);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&row.name)
                                        .size(type_scale::BODY)
                                        .strong()
                                        .color(color::TEXT),
                                )
                                .truncate(),
                            );
                            if !row.state.finished() {
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let (rect, response) = ui.allocate_exact_size(
                                            egui::vec2(32.0, 32.0),
                                            Sense::click(),
                                        );
                                        let stroke = Stroke::new(1.6, color::TEXT_MUTED);
                                        let half = 5.0;
                                        let center = rect.center();
                                        ui.painter().line_segment(
                                            [
                                                center + egui::vec2(-half, -half),
                                                center + egui::vec2(half, half),
                                            ],
                                            stroke,
                                        );
                                        ui.painter().line_segment(
                                            [
                                                center + egui::vec2(-half, half),
                                                center + egui::vec2(half, -half),
                                            ],
                                            stroke,
                                        );
                                        if labelled(response, "Cancel transfer", true).clicked() {
                                            cancel = Some(row.id);
                                        }
                                    },
                                );
                            }
                        });
                        let target = match row.direction {
                            Direction::Upload => format!("Remote · {}", row.target),
                            Direction::Download => row.target.clone(),
                        };
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(target)
                                    .size(type_scale::CAPTION)
                                    .color(color::TEXT_MUTED),
                            )
                            .truncate(),
                        );
                        match row.state {
                            RowState::Active => {
                                let fraction = row
                                    .total
                                    .filter(|total| *total > 0)
                                    .map_or(0.0, |total| row.done as f32 / total as f32);
                                progress_bar(ui, fraction);
                                ui.horizontal(|ui| {
                                    let progress = match row.total {
                                        Some(total) => format!(
                                            "{} of {}",
                                            format_size(row.done),
                                            format_size(total)
                                        ),
                                        None => format_size(row.done),
                                    };
                                    ui.label(
                                        egui::RichText::new(progress)
                                            .size(type_scale::CAPTION)
                                            .color(color::TEXT_MUTED),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new(eta_text(row))
                                                    .size(type_scale::CAPTION)
                                                    .color(color::TEXT_MUTED),
                                            );
                                        },
                                    );
                                });
                            }
                            _ => {
                                let (label, tint) = state_label(row);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(label)
                                            .size(type_scale::CAPTION)
                                            .color(tint),
                                    )
                                    .wrap(),
                                );
                            }
                        }
                    });
                });
                ui.add_space(space::SM);
            });
        }
        if rows.iter().any(|row| row.state.finished())
            && button(ui, ButtonKind::Secondary, "Clear finished", true).clicked()
        {
            clear = true;
        }
        if active > 0 {
            footnote(ui, "Keep fesTerm open until transfers finish.");
        }
    });
    if outcome.dismissed {
        close = true;
    }
    if let Some(transfers) = session.transfers.clone() {
        if let Some(id) = cancel {
            transfers.cancel(id);
        }
        if clear {
            transfers.clear_finished();
        }
    }
    if close {
        session.transfers_sheet = false;
    }
}

fn facts_text(facts: super::files::FileFacts) -> (String, String) {
    (
        facts.size.map_or("Unknown size".to_owned(), format_size),
        facts
            .modified
            .map_or("Unknown date".to_owned(), format_date),
    )
}

fn compare_column(ui: &mut Ui, heading: &str, facts: super::files::FileFacts) {
    let (size, date) = facts_text(facts);
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.label(
            egui::RichText::new(heading)
                .size(type_scale::MICRO)
                .color(color::TEXT_FAINT),
        );
        ui.label(
            egui::RichText::new(size)
                .size(type_scale::BODY)
                .strong()
                .color(color::TEXT),
        );
        ui.label(
            egui::RichText::new(date)
                .size(type_scale::CAPTION)
                .color(color::TEXT_MUTED),
        );
    });
}

/// Pure layout of the decision sheet so the gallery can render it from a view-model.
pub(crate) fn conflict_sheet_content(
    ui: &mut Ui,
    name: &str,
    location: &str,
    view: &ConflictView,
    remember: &mut bool,
) -> Option<ConflictPolicy> {
    let mut choice = None;
    ui.add_space(space::SM);
    ui.label(
        egui::RichText::new("File already exists")
            .size(type_scale::TITLE)
            .strong()
            .color(color::TEXT),
    );
    ui.add(
        egui::Label::new(
            egui::RichText::new(name)
                .size(type_scale::BODY)
                .strong()
                .color(color::TEXT),
        )
        .truncate(),
    );
    ui.add(
        egui::Label::new(
            egui::RichText::new(location)
                .size(type_scale::CAPTION)
                .color(color::TEXT_MUTED),
        )
        .truncate(),
    );
    card(ui, |ui| {
        ui.add_space(space::SM);
        ui.horizontal(|ui| {
            ui.add_space(space::MD);
            let half = (ui.available_width() - space::MD) / 2.0;
            ui.allocate_ui(egui::vec2(half, 60.0), |ui| {
                compare_column(ui, "ON THIS iPHONE", view.iphone);
            });
            ui.allocate_ui(egui::vec2(half, 60.0), |ui| {
                compare_column(ui, "ON REMOTE", view.remote);
            });
        });
        ui.add_space(space::SM);
    });
    let incoming_remote = view.direction == Direction::Download;
    let text = if view.existing_is_directory {
        "A folder with this name exists. Keep both, or skip this file.".to_owned()
    } else if !view.allowed.contains(&ConflictPolicy::Replace) {
        "Choose an available action for this item.".to_owned()
    } else if view.likely_identical {
        "These look identical (same size and time).".to_owned()
    } else if incoming_remote {
        super::platform_text(
            "Choose what to do with this download. Replacing the iPhone file cannot be undone.",
            "Choose what to do with this download. Replacing the local file cannot be undone.",
        )
        .to_owned()
    } else {
        "Choose what to do with this upload. Replacing the remote file cannot be undone.".to_owned()
    };
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .size(type_scale::CAPTION + 0.5)
                .color(color::TEXT_MUTED),
        )
        .wrap(),
    );
    if view.can_remember {
        ui.horizontal(|ui| {
            let (rect, response) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), Sense::CLICK);
            let tint = if *remember {
                color::PRIMARY
            } else {
                color::BORDER
            };
            ui.painter().rect_stroke(
                rect.shrink(2.0),
                CornerRadius::same(5),
                Stroke::new(1.6, tint),
                StrokeKind::Inside,
            );
            if *remember {
                ui.painter()
                    .rect_filled(rect.shrink(2.0), CornerRadius::same(5), color::PRIMARY);
                super::widgets::check_mark(ui.painter(), rect.center(), color::ON_PRIMARY);
            }
            let label = ui.add(
                egui::Label::new(
                    egui::RichText::new("Use this choice for remaining conflicts")
                        .size(type_scale::CAPTION + 0.5)
                        .color(color::TEXT),
                )
                .sense(Sense::CLICK),
            );
            if labelled(response, "Use this choice for remaining conflicts", true).clicked()
                || label.clicked()
            {
                *remember = !*remember;
            }
        });
    } else {
        *remember = false;
    }
    if button(
        ui,
        ButtonKind::Primary,
        "Keep both",
        view.allowed.contains(&ConflictPolicy::KeepBoth),
    )
    .clicked()
    {
        choice = Some(ConflictPolicy::KeepBoth);
    }
    ui.label(
        egui::RichText::new(format!(
            "{} as {}",
            if incoming_remote {
                "Downloads"
            } else {
                "Uploads"
            },
            view.suggested_name
        ))
        .size(type_scale::CAPTION)
        .color(color::TEXT_FAINT),
    );
    if button(
        ui,
        ButtonKind::Secondary,
        "Skip file",
        view.allowed.contains(&ConflictPolicy::Skip),
    )
    .clicked()
    {
        choice = Some(ConflictPolicy::Skip);
    }
    if button(
        ui,
        ButtonKind::Destructive,
        if incoming_remote {
            super::platform_text("Replace iPhone file", "Replace local file")
        } else {
            "Replace remote file"
        },
        !view.existing_is_directory && view.allowed.contains(&ConflictPolicy::Replace),
    )
    .clicked()
    {
        choice = Some(ConflictPolicy::Replace);
    }
    choice
}

/// The overwrite decision sheet (mock-up 09) for the first waiting transfer.
pub(crate) fn conflict_sheet(ctx: &egui::Context, bounds: Rect, session: &mut FilesSession) {
    let Some(row) = session.conflict().cloned() else {
        return;
    };
    let Some(view) = row.conflict.clone() else {
        return;
    };
    let location = match row.direction {
        Direction::Upload => format!("Remote · {}", row.target),
        Direction::Download => super::platform_text("On this iPhone", "On this device").to_owned(),
    };
    let mut remember = session.remember;
    let mut choice = None;
    let outcome = bottom_sheet(ctx, "conflict", bounds, |ui| {
        choice = conflict_sheet_content(ui, &row.name, &location, &view, &mut remember);
    });
    session.remember = remember;
    let skip = outcome.dismissed;
    let choice = choice.or(skip.then_some(ConflictPolicy::Skip));
    if let (Some(policy), Some(transfers)) = (choice, session.transfers.clone()) {
        transfers.resolve(row.id, policy, session.remember);
        session.remember = false;
    }
}
