//! The "Save As…" destination picker (ADR 0034 §3, issue #166).
//!
//! This is the local-filesystem sibling of the SFTP file manager's
//! `MarkdownFilePicker`: it reuses the same pane model, breadcrumb/toolbar
//! navigation, and sortable Name/Size/Modified/Type table so the two read as
//! one family of control. It diverges in the two ways a "save" differs from an
//! "open": every file is shown (a document may be written under any name, not
//! only Markdown), and the destination name comes from an explicit field the
//! user edits rather than from the row they clicked.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui::{
    self, Align, Color32, FontId, Key, Layout, RichText, ScrollArea, Sense, TextEdit, Ui,
    WidgetInfo, WidgetType,
};
use festerm_ssh::{
    SftpDirectoryItem, SftpDirectorySnapshot, SftpEntryType, SftpPath, SftpPathMetadata,
};
use festerm_ui_egui::theme;

use crate::sftp_file_manager::{
    breadcrumb_segments, font_for_text_role, format_modified, format_size, item_glyph,
    local_home_directory, paint_sftp_glyph, path_key, show_table_header_cell, show_table_text_cell,
    toolbar_icon_button, CellAlign, LocalDirectoryLoadRequest, LocalDirectoryLoader, SftpGlyph,
    SftpPaneState, SftpSortColumn, SftpTextRole, SFTP_TABLE_CELL_PADDING, SFTP_TABLE_ROW_HEIGHT,
    SFTP_TOOLBAR_NAV_GAP,
};

/// Stated verbatim per ADR 0034 §3: an existing target is described in words
/// before the fact, and the press still saves in one go.
const OVERWRITE_NOTICE: &str = "A file with this name already exists here. Saving will replace it.";
/// A directory cannot be replaced by a document, so this is a hard block.
const DIRECTORY_COLLISION_MESSAGE: &str =
    "A folder with this name already exists here, so it can't be saved over.";
const SEPARATOR_HINT: &str = "A file name cannot contain a path separator.";
const EMPTY_NAME_HINT: &str = "Enter a file name.";

/// Minimum widths for the metadata columns so "965 B" and "Yesterday 09:14"
/// stay legible; whatever is left goes to Name, the column that matters when
/// choosing a save target.
const SIZE_COLUMN_MIN_WIDTH: f32 = 72.0;
const MODIFIED_COLUMN_MIN_WIDTH: f32 = 130.0;

/// The Save As listing is Name/Size/Modified only (ADR 0034 §3). The SFTP
/// panes keep their four-column Name/Size/Modified/Type layout; this is a
/// separate three-column split so dropping Type here does not change them.
fn save_as_columns(available_width: f32) -> [f32; 3] {
    let width = available_width.max(0.0);
    let size = (width * 0.14).max(SIZE_COLUMN_MIN_WIDTH);
    let modified = (width * 0.26).max(MODIFIED_COLUMN_MIN_WIDTH);
    let name = (width - size - modified).max(0.0);
    [name, size, modified]
}

const SAVE_BUTTON_HEIGHT: f32 = 30.0;
const SAVE_BUTTON_PADDING_X: f32 = 16.0;
const SAVE_BUTTON_RADIUS: f32 = 6.0;
const BUTTON_LABEL_SIZE: f32 = 13.0;

/// What the user did in a [`SaveAsPicker`] this frame.
pub(crate) enum SaveAsOutcome {
    /// Nothing decided yet; the sheet stays open.
    Pending,
    /// The user pressed Save. `path` is the absolute local destination,
    /// already joined from the browsed directory and the file-name field.
    Save { path: PathBuf },
    /// Dismissed with Cancel or Escape.
    Cancelled,
}

enum SaveAsEvent {
    Loaded {
        request_id: u64,
        snapshot: SftpDirectorySnapshot,
        metadata: Option<SftpPathMetadata>,
    },
    Failed {
        request_id: u64,
        summary: String,
        details: String,
    },
}

/// Local-filesystem-only destination picker for "Save As…". Like
/// `MarkdownFilePicker` it owns its own directory-listing thread and event
/// channel rather than reaching into a live SSH pipeline: there is no remote
/// document origin wired up anywhere yet, so the remote destination is shown
/// but disabled and no remote listing is ever fetched.
pub(crate) struct SaveAsPicker {
    pane: SftpPaneState,
    file_name: String,
    event_sender: Sender<SaveAsEvent>,
    event_receiver: Receiver<SaveAsEvent>,
    repaint: egui::Context,
    local_loader: LocalDirectoryLoader,
    next_request_id: u64,
}

impl SaveAsPicker {
    /// `start_dir` is the directory to open in; `suggested_name` is the file
    /// name to prefill (the document's current file name).
    pub(crate) fn new(start_dir: PathBuf, suggested_name: String, repaint: egui::Context) -> Self {
        let (event_sender, event_receiver) = mpsc::channel();
        let local_loader = LocalDirectoryLoader::new("festerm-gui-save-as-local".to_owned());
        let mut picker = Self {
            pane: SftpPaneState::new(SftpPath::local(start_dir)),
            file_name: suggested_name,
            event_sender,
            event_receiver,
            repaint,
            local_loader,
            next_request_id: 0,
        };
        let start = picker.pane.current_path.clone();
        picker.load(start);
        picker
    }

    fn load(&mut self, path: SftpPath) {
        self.pane.loading = true;
        self.pane.error = None;
        self.pane.details = None;
        self.pane.path_text = path.display();
        self.pane.current_path = path.clone();
        self.next_request_id += 1;
        let request_id = self.next_request_id;
        self.pane.pending_request_id = request_id;
        let event_sender = self.event_sender.clone();
        let repaint = self.repaint.clone();
        self.local_loader.schedule(LocalDirectoryLoadRequest {
            path,
            complete: Box::new(move |result| {
                let event = match result {
                    Ok((snapshot, metadata)) => SaveAsEvent::Loaded {
                        request_id,
                        snapshot,
                        metadata,
                    },
                    Err(error) => SaveAsEvent::Failed {
                        request_id,
                        summary: "Could not load the folder.".to_owned(),
                        details: error,
                    },
                };
                let _ = event_sender.send(event);
                repaint.request_repaint();
            }),
        });
    }

    /// Applies directory-listing results that arrived since the last frame.
    /// Must be called once per frame before `ui`.
    pub(crate) fn poll(&mut self) {
        while let Ok(event) = self.event_receiver.try_recv() {
            match event {
                SaveAsEvent::Loaded {
                    request_id,
                    snapshot,
                    metadata,
                } => {
                    if request_id == self.pane.pending_request_id {
                        self.pane.set_snapshot(snapshot, metadata);
                    }
                }
                SaveAsEvent::Failed {
                    request_id,
                    summary,
                    details,
                } => {
                    if request_id == self.pane.pending_request_id {
                        self.pane.set_error(summary, details);
                    }
                }
            }
        }
    }

    fn navigate_up(&mut self) {
        let path = self.pane.current_path.parent_directory();
        self.load(path);
    }

    fn navigate_home(&mut self) {
        self.load(SftpPath::local(local_home_directory()));
    }

    fn refresh(&mut self) {
        let path = self.pane.current_path.clone();
        self.load(path);
    }

    /// The directory currently browsed, so the caller can remember it.
    pub(crate) fn current_directory(&self) -> Option<PathBuf> {
        match &self.pane.current_path {
            SftpPath::Local(path) => Some(path.clone()),
            SftpPath::Remote(_) => None,
        }
    }

    /// Renders into `ui`; the caller owns the surrounding `egui::Modal`.
    pub(crate) fn ui(&mut self, ui: &mut Ui) -> SaveAsOutcome {
        let mut outcome = SaveAsOutcome::Pending;
        let width = ui.available_width();

        self.show_destination_switch(ui, width);
        ui.add_space(8.0);
        self.show_toolbar(ui, width);
        ui.add_space(6.0);

        if let Some(summary) = self.pane.error.clone() {
            ui.colored_label(theme::STATUS_ERROR, summary);
            ui.add_space(6.0);
        }

        let entries = self.pane.visible_entries().to_vec();

        // The listing is the elastic element: the file-name field, notice and
        // button row are pinned to the bottom of the sheet, and the table
        // grows into whatever height is left rather than the buttons floating
        // in dead space above a short list.
        egui::Panel::bottom(egui::Id::new("save_as_footer"))
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                ui.add_space(8.0);
                outcome = self.show_footer(ui, &entries);
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                self.show_table_header(ui, width);
                self.show_rows(ui, width, &entries);
            });

        if ui.input(|input| input.key_pressed(Key::Escape)) {
            outcome = SaveAsOutcome::Cancelled;
        }

        outcome
    }

    fn show_destination_switch(&self, ui: &mut Ui, width: f32) {
        let gap = 6.0;
        let segment_width = ((width - gap) / 2.0).max(0.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            // The local segment is the only reachable origin, so it renders as
            // the selected half: a solid fill, no accent stroke. The stroke
            // read as a focused text field and competed with Save, which is
            // the real primary action and stays the loudest thing here.
            ui.add(
                egui::Button::new(
                    RichText::new("This host (local)")
                        .font(font_for_text_role(SftpTextRole::PaneLabel))
                        .color(theme::TEXT_PRIMARY),
                )
                .fill(theme::SURFACE_TAB_ACTIVE)
                .stroke(egui::Stroke::NONE)
                .corner_radius(5.0)
                .min_size(egui::vec2(segment_width, 30.0)),
            );
            // No remote document origin exists yet, so this half is present but
            // refuses. The label states the reason at a glance for keyboard and
            // touch users; the hover carries the fuller explanation. It stays
            // muted on the inactive fill -- nothing red, which would read as
            // broken rather than simply unavailable.
            ui.add_enabled(
                false,
                egui::Button::new(
                    RichText::new("Remote host — none connected")
                        .font(font_for_text_role(SftpTextRole::PaneLabel))
                        .color(theme::TEXT_MUTED),
                )
                .fill(theme::SURFACE_TAB_INACTIVE)
                .corner_radius(5.0)
                .min_size(egui::vec2(segment_width, 30.0)),
            )
            .on_disabled_hover_text(
                "Saving to a remote host needs a connected SFTP session, and none is available yet.",
            );
        });
    }

    fn show_toolbar(&mut self, ui: &mut Ui, width: f32) {
        ui.horizontal(|ui| {
            if toolbar_icon_button(ui, SftpGlyph::Up, "Up one level").clicked() {
                self.navigate_up();
            }
            if toolbar_icon_button(ui, SftpGlyph::Home, "Home").clicked() {
                self.navigate_home();
            }
            if toolbar_icon_button(ui, SftpGlyph::Refresh, "Refresh folder").clicked() {
                self.refresh();
            }
            ui.add_space(SFTP_TOOLBAR_NAV_GAP);
            if let Some(path) = show_breadcrumb(ui, &self.pane.current_path, width) {
                self.load(path);
            }
        });
    }

    fn show_table_header(&mut self, ui: &mut Ui, width: f32) {
        let columns = save_as_columns(width);
        let mut sort_clicked = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let headers = [
                (CellAlign::Left, "Name", SftpSortColumn::Name),
                (CellAlign::Right, "Size", SftpSortColumn::Size),
                (CellAlign::Left, "Modified", SftpSortColumn::Modified),
            ];
            for (index, (align, title, column)) in headers.into_iter().enumerate() {
                if show_table_header_cell(
                    ui,
                    columns[index],
                    align,
                    title,
                    self.pane.sort.column == column,
                    self.pane.sort.descending,
                )
                .clicked()
                {
                    sort_clicked = Some(column);
                }
            }
        });
        if let Some(column) = sort_clicked {
            self.pane.set_sort(column);
        }
    }

    fn show_rows(&mut self, ui: &mut Ui, width: f32, entries: &[SftpDirectoryItem]) {
        let columns = save_as_columns(width);
        let mut navigate_into = None;
        let mut chosen_name = None;
        let scroll_output = ScrollArea::vertical()
            .id_salt("save_as_picker_rows")
            .auto_shrink([false, false])
            .vertical_scroll_offset(self.pane.scroll_offset)
            .show(ui, |ui| {
                for item in entries {
                    let key = path_key(&item.path);
                    let selected = self.pane.selected_paths.contains(&key);
                    let row = ui
                        .horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.set_min_height(SFTP_TABLE_ROW_HEIGHT);
                            ui.set_max_height(SFTP_TABLE_ROW_HEIGHT);
                            let name_color = if selected {
                                theme::TEXT_PRIMARY
                            } else {
                                theme::TEXT_SECONDARY
                            };
                            ui.allocate_ui_with_layout(
                                egui::vec2(columns[0], SFTP_TABLE_ROW_HEIGHT),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.set_min_width(columns[0]);
                                    ui.set_max_width(
                                        (columns[0] - SFTP_TABLE_CELL_PADDING).max(0.0),
                                    );
                                    ui.add_space(SFTP_TABLE_CELL_PADDING);
                                    let (icon_rect, _) = ui.allocate_exact_size(
                                        egui::vec2(15.0, 15.0),
                                        Sense::hover(),
                                    );
                                    paint_sftp_glyph(
                                        ui.painter(),
                                        item_glyph(item),
                                        icon_rect,
                                        theme::TEXT_SECONDARY,
                                    );
                                    ui.add_space(5.0);
                                    // Selection is carried by weight as well as
                                    // the band behind it, so it survives without
                                    // colour (ADR 0034 §8).
                                    let mut name =
                                        RichText::new(item.name.clone()).color(name_color);
                                    if selected {
                                        name = name.strong();
                                    }
                                    ui.add(egui::Label::new(name).truncate());
                                },
                            );
                            show_table_text_cell(
                                ui,
                                columns[1],
                                CellAlign::Right,
                                RichText::new(format_size(item.size)).color(theme::TEXT_MUTED),
                            );
                            show_table_text_cell(
                                ui,
                                columns[2],
                                CellAlign::Left,
                                RichText::new(format_modified(item.modified_at))
                                    .color(theme::TEXT_MUTED),
                            );
                        })
                        .response;
                    let row_rect = row.rect;
                    let row_response = ui.interact(
                        row_rect,
                        ui.make_persistent_id(("save_as_picker_row", &key)),
                        Sense::click(),
                    );
                    if selected {
                        ui.painter().rect_filled(
                            row_rect,
                            0.0,
                            theme::SURFACE_TAB_ACTIVE.gamma_multiply(0.6),
                        );
                        // A left marker bar so the selected row is legible at a
                        // glance and not by hue alone.
                        let marker = egui::Rect::from_min_size(
                            row_rect.min,
                            egui::vec2(3.0, row_rect.height()),
                        );
                        ui.painter().rect_filled(marker, 0.0, theme::ACCENT_PRIMARY);
                    }
                    if row_response.clicked() {
                        self.pane.select_single(&item.path);
                        // Clicking a file offers it as the destination name;
                        // this is how a user chooses to overwrite something,
                        // and deliberately does not save on its own.
                        if item.file_type != SftpEntryType::Directory {
                            chosen_name = Some(item.name.clone());
                        }
                    }
                    if row_response.double_clicked() && item.file_type == SftpEntryType::Directory {
                        navigate_into = Some(item.path.clone());
                    }
                }
            });
        self.pane.update_scroll_offset(scroll_output.state.offset.y);
        if let Some(name) = chosen_name {
            self.file_name = name;
        }
        if let Some(path) = navigate_into {
            self.load(path);
        }
    }

    fn show_footer(&mut self, ui: &mut Ui, entries: &[SftpDirectoryItem]) -> SaveAsOutcome {
        let mut outcome = SaveAsOutcome::Pending;

        // Label inline to the left of the field, per the mockup: it ties the
        // caption to the control and costs no vertical space.
        let name_response = ui
            .horizontal(|ui| {
                let name_label = ui.label(
                    RichText::new("File name")
                        .font(font_for_text_role(SftpTextRole::PaneLabel))
                        .color(theme::TEXT_SECONDARY),
                );
                ui.add_space(8.0);
                ui.add(
                    TextEdit::singleline(&mut self.file_name)
                        .id(egui::Id::new("save_as_file_name"))
                        .desired_width(f32::INFINITY)
                        .hint_text("File name"),
                )
                .labelled_by(name_label.id)
            })
            .inner;

        let trimmed = self.file_name.trim().to_owned();
        let collision = entries
            .iter()
            .find(|item| item.name == trimmed && !trimmed.is_empty());
        let directory_collision = collision
            .map(|item| item.file_type == SftpEntryType::Directory)
            .unwrap_or(false);
        let file_collision = collision
            .map(|item| item.file_type != SftpEntryType::Directory)
            .unwrap_or(false);
        let has_separator = trimmed.chars().any(std::path::is_separator);
        let save_enabled = !trimmed.is_empty() && !has_separator && !directory_collision;

        // One message line below the field. It doubles as the inline blocker
        // hint so a disabled Save always states its reason without a hover:
        // the directory collision is a hard block (red), the separator and
        // empty-name cases are the blockers the notices did not otherwise
        // cover, and the overwrite notice is a permitted-action statement in a
        // legible weight -- never red, which is reserved for the block.
        ui.add_space(4.0);
        if directory_collision {
            ui.label(
                RichText::new(DIRECTORY_COLLISION_MESSAGE)
                    .font(font_for_text_role(SftpTextRole::DialogBody))
                    .color(theme::STATUS_ERROR),
            );
        } else if has_separator {
            ui.label(
                RichText::new(SEPARATOR_HINT)
                    .font(font_for_text_role(SftpTextRole::DialogBody))
                    .color(theme::TEXT_SECONDARY),
            );
        } else if trimmed.is_empty() {
            ui.label(
                RichText::new(EMPTY_NAME_HINT)
                    .font(font_for_text_role(SftpTextRole::DialogBody))
                    .color(theme::TEXT_SECONDARY),
            );
        } else if file_collision {
            ui.label(
                RichText::new(OVERWRITE_NOTICE)
                    .font(font_for_text_role(SftpTextRole::DialogBody))
                    .color(theme::TEXT_SECONDARY),
            );
        }

        let disabled_reason = if trimmed.is_empty() {
            EMPTY_NAME_HINT
        } else if has_separator {
            SEPARATOR_HINT
        } else {
            DIRECTORY_COLLISION_MESSAGE
        };

        let enter_saves = name_response.lost_focus()
            && ui.input(|input| input.key_pressed(Key::Enter))
            && save_enabled;

        ui.add_space(8.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let mut save = primary_button(ui, "Save", save_enabled);
            if !save_enabled {
                save = save.on_hover_text(disabled_reason);
            }
            ui.add_space(8.0);
            if ui.button("Cancel").clicked() {
                outcome = SaveAsOutcome::Cancelled;
            }
            let should_save = save_enabled && (save.clicked() || enter_saves);
            if let Some(directory) = self.current_directory().filter(|_| should_save) {
                outcome = SaveAsOutcome::Save {
                    path: directory.join(&trimmed),
                };
            }
        });

        outcome
    }
}

/// Renders the breadcrumb on a single line, collapsing from the front with a
/// leading "…" when the path is deep, so a long real path keeps its last
/// segments and the current directory visible instead of wrapping and
/// orphaning the current directory on a second line. Returns a navigation
/// target if a segment was clicked.
fn show_breadcrumb(ui: &mut Ui, path: &SftpPath, width: f32) -> Option<SftpPath> {
    let segments = breadcrumb_segments(path);
    // Keep the deepest few segments plus the current directory; anything
    // before them collapses to a single leading ellipsis.
    let keep = 3usize;
    let start = segments.len().saturating_sub(keep);
    let mut target = None;

    let separator = |ui: &mut Ui| {
        ui.label(
            RichText::new("/")
                .font(font_for_text_role(SftpTextRole::Breadcrumb))
                .color(theme::TEXT_MUTED),
        );
    };

    ui.allocate_ui_with_layout(
        egui::vec2(width, 24.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            if start > 0 {
                ui.label(
                    RichText::new("…")
                        .font(font_for_text_role(SftpTextRole::Breadcrumb))
                        .color(theme::TEXT_MUTED),
                );
                separator(ui);
            }
            for (offset, segment) in segments[start..].iter().enumerate() {
                let global_index = start + offset;
                let needs_separator = if offset == 0 {
                    start == 0 && global_index > 0 && segment.label != "/"
                } else {
                    segment.label != "/"
                };
                if needs_separator {
                    separator(ui);
                }
                let text = RichText::new(segment.label.clone())
                    .font(font_for_text_role(SftpTextRole::Breadcrumb))
                    .color(if segment.current {
                        theme::TEXT_PRIMARY
                    } else {
                        theme::TEXT_SECONDARY
                    });
                if segment.current {
                    ui.add(egui::Label::new(text).truncate());
                } else if ui
                    .add(
                        egui::Button::new(text)
                            .fill(Color32::TRANSPARENT)
                            .stroke(egui::Stroke::NONE)
                            .min_size(egui::vec2(0.0, 24.0)),
                    )
                    .clicked()
                {
                    target = Some(segment.path.clone());
                }
            }
        },
    );

    target
}

/// A primary (accent-filled) button. `text_editor::primary_button` is the
/// visual reference, but it is private to that module, so the equivalent is
/// reproduced here rather than reached across the file boundary.
fn primary_button(ui: &mut Ui, label: &str, enabled: bool) -> egui::Response {
    let font = FontId::proportional(BUTTON_LABEL_SIZE);
    let colour = if enabled {
        theme::TEXT_ON_ACCENT
    } else {
        theme::TEXT_MUTED
    };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, colour);
    let width = (SAVE_BUTTON_PADDING_X * 2.0 + galley.size().x).max(SAVE_BUTTON_HEIGHT);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, SAVE_BUTTON_HEIGHT), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));

    let mut fill = theme::ACCENT_ACTION;
    if !enabled {
        fill = fill.gamma_multiply(0.35);
    } else if response.hovered() {
        fill = fill.gamma_multiply(1.15);
    }
    ui.painter().rect_filled(rect, SAVE_BUTTON_RADIUS, fill);
    ui.painter().galley(
        egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        colour,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TemporaryDirectory {
        path: PathBuf,
    }

    impl TemporaryDirectory {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "festerm-save-as-{}-{label}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn file(&self, name: &str, contents: &str) {
            fs::write(self.path.join(name), contents).unwrap();
        }

        fn directory(&self, name: &str) {
            fs::create_dir_all(self.path.join(name)).unwrap();
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn harness_for(
        directory: &TemporaryDirectory,
        suggested: &str,
    ) -> Harness<'static, (SaveAsPicker, Option<SaveAsOutcome>)> {
        let picker = SaveAsPicker::new(
            directory.path.clone(),
            suggested.to_owned(),
            egui::Context::default(),
        );
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 700.0))
            .build_ui_state(
                move |ui, state: &mut (SaveAsPicker, Option<SaveAsOutcome>)| {
                    state.0.poll();
                    let outcome = state.0.ui(ui);
                    if !matches!(outcome, SaveAsOutcome::Pending) {
                        state.1 = Some(outcome);
                    }
                },
                (picker, None),
            );
        settle(&mut harness);
        harness
    }

    /// The listing loads on a background thread, so run frames until the
    /// directory's contents are actually in the accessibility tree before a
    /// test asserts anything that depends on them.
    fn settle(harness: &mut Harness<'static, (SaveAsPicker, Option<SaveAsOutcome>)>) {
        for _ in 0..40 {
            harness.state_mut().0.poll();
            harness.run();
            if harness.state().0.current_directory().is_some() && !harness.state().0.pane.loading {
                // A couple more frames so the newly-arrived rows are laid out.
                harness.run();
                harness.run();
                return;
            }
        }
    }

    fn type_name(
        harness: &mut Harness<'static, (SaveAsPicker, Option<SaveAsOutcome>)>,
        text: &str,
    ) {
        let field = harness.get_by_label("File name");
        field.focus();
        harness.run();
        let field = harness.get_by_label("File name");
        field.type_text(text);
        harness.run();
    }

    #[test]
    fn save_as_typing_a_new_name_and_pressing_save_reports_the_joined_path() {
        let directory = TemporaryDirectory::new("new-name");
        directory.file("README.md", "readme\n");
        let mut harness = harness_for(&directory, "");

        type_name(&mut harness, "draft.md");
        harness.get_by_label("Save").click();
        harness.run();

        match harness.state().1.as_ref().expect("an outcome") {
            SaveAsOutcome::Save { path } => {
                assert_eq!(path, &directory.path.join("draft.md"));
            }
            other => panic!("expected Save, got {}", describe(other)),
        }
    }

    #[test]
    fn save_as_cancel_reports_cancelled() {
        let directory = TemporaryDirectory::new("cancel");
        let mut harness = harness_for(&directory, "notes.md");

        harness.get_by_label("Cancel").click();
        harness.run();

        assert!(matches!(
            harness.state().1.as_ref().expect("an outcome"),
            SaveAsOutcome::Cancelled
        ));
    }

    #[test]
    fn save_as_matching_an_existing_file_states_the_overwrite_and_still_saves() {
        let directory = TemporaryDirectory::new("overwrite");
        directory.file("NOTES.md", "notes\n");
        let mut harness = harness_for(&directory, "NOTES.md");

        assert!(
            harness.query_by_label(OVERWRITE_NOTICE).is_some(),
            "the overwrite sentence has to be shown before the fact"
        );

        harness.get_by_label("Save").click();
        harness.run();

        match harness.state().1.as_ref().expect("an outcome") {
            SaveAsOutcome::Save { path } => {
                assert_eq!(path, &directory.path.join("NOTES.md"));
            }
            other => panic!("expected Save, got {}", describe(other)),
        }
    }

    #[test]
    fn save_as_matching_an_existing_directory_disables_save_and_says_why() {
        let directory = TemporaryDirectory::new("dir-collision");
        directory.directory("docs");
        let mut harness = harness_for(&directory, "docs");

        assert!(
            harness
                .query_by_label_contains("folder with this name already exists")
                .is_some(),
            "a directory collision has to explain itself"
        );

        harness.get_by_label("Save").click();
        harness.run();

        assert!(
            harness.state().1.is_none(),
            "Save must not fire while a directory would be saved over"
        );
    }

    #[test]
    fn save_as_an_empty_name_disables_save_and_hints_to_enter_one() {
        let directory = TemporaryDirectory::new("empty");
        let mut harness = harness_for(&directory, "");

        assert!(
            harness.query_by_label(EMPTY_NAME_HINT).is_some(),
            "an empty name has to state the blocker inline, not only on hover"
        );

        harness.get_by_label("Save").click();
        harness.run();

        assert!(
            harness.state().1.is_none(),
            "there is no destination to save to with an empty name"
        );
    }

    #[test]
    fn save_as_a_name_with_a_separator_disables_save_and_says_so_inline() {
        let directory = TemporaryDirectory::new("separator");
        let mut harness = harness_for(&directory, "");

        type_name(&mut harness, "sub/dir.md");

        assert!(
            harness.query_by_label(SEPARATOR_HINT).is_some(),
            "a path separator in the name has to be explained inline"
        );

        harness.get_by_label("Save").click();
        harness.run();

        assert!(
            harness.state().1.is_none(),
            "a name with a separator is not a single destination and must not save"
        );
    }

    #[test]
    fn save_as_remote_destination_is_present_but_refuses_and_explains() {
        let directory = TemporaryDirectory::new("remote");
        let harness = harness_for(&directory, "notes.md");

        let remote = harness.get_by_label("Remote host — none connected");
        assert!(
            remote.accesskit_node().is_disabled(),
            "there is no connected SFTP session, so remote must be disabled"
        );
    }

    fn describe(outcome: &SaveAsOutcome) -> &'static str {
        match outcome {
            SaveAsOutcome::Pending => "Pending",
            SaveAsOutcome::Save { .. } => "Save",
            SaveAsOutcome::Cancelled => "Cancelled",
        }
    }
}
