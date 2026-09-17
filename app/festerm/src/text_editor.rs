//! The native text editor tab (ADR 0034, issue #166).
//!
//! The tab is a *view*: it owns where the caret is, what is scrolled into
//! sight, and which options are switched on. The text itself, the undo
//! history, and the answer to "are there unsaved changes" belong to the
//! document registry, so two tabs showing one file agree with each other
//! without either of them being in charge.

use eframe::egui::{self, vec2, Align, FontId, Sense, WidgetInfo, WidgetType};
use festerm_document::{BannerAction, DocumentId, DocumentStatus, Severity, StatusAccent};
use festerm_ui_egui::{chrome::ChipStatus, icon, icon::Icon, theme};

use crate::documents::SharedDocuments;
use crate::markdown_viewer::{toolbar_button, TOOLBAR_BUTTON_GAP, TOOLBAR_BUTTON_HEIGHT};
use crate::tabs::{AppCommand, TabId};

/// Width of the line-number gutter's digits area before padding.
const GUTTER_PADDING_X: f32 = 12.0;
const GUTTER_MIN_DIGITS: usize = 2;
const EDITOR_TEXT_SIZE: f32 = 13.0;
const BAR_PADDING_X: i8 = 9;
const BAR_PADDING_Y: i8 = 6;
const BANNER_ACCENT_WIDTH: f32 = 3.0;
const ORIGIN_ICON_SIZE: f32 = 12.0;
const BANNER_PADDING_X: i8 = BAR_PADDING_X;
const BANNER_PADDING_Y: i8 = BAR_PADDING_Y;


const LABEL_TEXT_SIZE: f32 = 11.0;
const PRIMARY_BUTTON_PADDING_X: f32 = 11.0;
const PRIMARY_BUTTON_RADIUS: f32 = 5.0;

/// Per-view presentation options (ADR 0034 §4). These are deliberately view
/// scoped: two editors on one file may be set up differently without
/// disagreeing about the text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EditorViewOptions {
    pub(crate) line_numbers: bool,
    pub(crate) fluid_width: bool,
}

impl Default for EditorViewOptions {
    fn default() -> Self {
        Self {
            line_numbers: true,
            fluid_width: true,
        }
    }
}

impl EditorViewOptions {
    /// The summary the command bar shows beside the options menu, so the
    /// current setup is readable without opening it.
    fn summary(self) -> String {
        let lines = if self.line_numbers {
            "Lines"
        } else {
            "No lines"
        };
        let width = if self.fluid_width {
            "Fluid width"
        } else {
            "Reading width"
        };
        format!("{lines} · {width} · Standard keys")
    }
}

/// One editor tab.
pub(crate) struct TextEditorTab {
    document: DocumentId,
    title: String,
    origin_label: String,
    remote: bool,
    /// The widget's copy of the text. egui's `TextEdit` needs a `String` it
    /// can own edits in; the document is told about them straight afterwards,
    /// so this never drifts by more than the inside of one frame.
    buffer: String,
    options: EditorViewOptions,
    caret: (usize, usize),
    status_bar_visible: bool,
}

impl TextEditorTab {
    pub(crate) fn new(document: DocumentId, documents: &SharedDocuments) -> Self {
        let registry = documents.borrow();
        let open = registry
            .get(document)
            .expect("an editor tab is built for a document that is open");
        Self {
            document,
            title: open.origin().file_name().to_owned(),
            origin_label: open.origin().qualified_label(),
            remote: open.origin().is_remote(),
            buffer: open.text().text().to_owned(),
            options: EditorViewOptions::default(),
            caret: (1, 1),
            status_bar_visible: true,
        }
    }

    pub(crate) const fn document(&self) -> DocumentId {
        self.document
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn origin_label(&self) -> &str {
        &self.origin_label
    }

    pub(crate) const fn set_status_bar_visible(&mut self, visible: bool) {
        self.status_bar_visible = visible;
    }

    /// The language the status bar names, inferred only from the file name:
    /// guessing from content would change under the user as they type.
    pub(crate) fn language_label(&self) -> &'static str {
        match self
            .title
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .as_deref()
        {
            Some("md" | "markdown") => "Markdown",
            Some("rs") => "Rust",
            Some("toml") => "TOML",
            Some("json") => "JSON",
            Some("yaml" | "yml") => "YAML",
            Some("sh" | "bash" | "zsh") => "Shell",
            Some("py") => "Python",
            _ => "Text",
        }
    }

    /// Encoding, line ending, and indentation, exactly as the file itself
    /// uses them rather than as the editor would prefer.
    pub(crate) fn status_bar_encoding(&self, documents: &SharedDocuments) -> String {
        documents.borrow().get(self.document).map_or_else(
            || "UTF-8".to_owned(),
            |open| {
                let text = open.text();
                format!(
                    "{} · {} · {}",
                    text.encoding().label(),
                    text.line_ending().label(),
                    text.indentation().label()
                )
            },
        )
    }

    /// The short phrase the status bar shows beside the dot.
    pub(crate) fn status_bar_label(&self, documents: &SharedDocuments) -> &'static str {
        self.status(documents)
            .map_or("Editor", |status| status.short_label())
    }

    pub(crate) fn status_bar_position(&self) -> String {
        format!("Ln {}, Col {}", self.caret.0, self.caret.1)
    }

    pub(crate) fn status_bar_size(&self, documents: &SharedDocuments) -> String {
        let bytes = documents
            .borrow()
            .get(self.document)
            .map_or(0, |open| open.text().to_bytes().len());
        format!("{bytes} bytes")
    }

    /// The dot the tab chip shows. Severity rather than connection state:
    /// what matters about an editor from another tab is whether it is holding
    /// changes that are not on disk.
    pub(crate) fn chip_status(&self, documents: &SharedDocuments) -> ChipStatus {
        match self.status(documents).map(|status| status.severity()) {
            Some(Severity::Blocking) => ChipStatus::Failed,
            Some(Severity::Warning) => ChipStatus::Reconnecting,
            Some(Severity::Informational) => {
                if self.is_dirty(documents) {
                    ChipStatus::Starting
                } else {
                    ChipStatus::Connected
                }
            }
            None => ChipStatus::Neutral,
        }
    }

    pub(crate) fn is_dirty(&self, documents: &SharedDocuments) -> bool {
        documents
            .borrow()
            .get(self.document)
            .is_some_and(|open| open.text().is_dirty())
    }

    pub(crate) fn status(&self, documents: &SharedDocuments) -> Option<DocumentStatus> {
        documents
            .borrow()
            .get(self.document)
            .map(super::documents::OpenDocument::status)
    }

    /// Renders the tab and reports the one command it produced, if any.
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        tab_id: TabId,
        documents: &SharedDocuments,
    ) -> Option<AppCommand> {
        let Some(status) = self.status(documents) else {
            // The document was released underneath this view, which can only
            // happen if the tab outlived its registration; close rather than
            // draw a view of nothing.
            return Some(AppCommand::CloseTab(tab_id));
        };
        self.adopt_external_edits(documents);

        let mut command = None;
        egui::Frame::new()
            .fill(theme::SURFACE_WINDOW)
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    self.show_origin_bar(ui, documents, &status);
                    hairline(ui);
                    if let Some(bar_command) = self.show_command_bar(ui, &status) {
                        command = Some(bar_command);
                    }
                    hairline(ui);
                    if let Some(banner_command) = show_banner(ui, &status) {
                        command = Some(banner_command);
                    }
                    self.show_body(ui, documents);
                });
            });
        command
    }

    /// Appends text the way typing would, for the headless screenshot
    /// gallery, which has no keyboard.
    #[cfg(test)]
    pub(crate) fn type_for_gallery(&mut self, documents: &SharedDocuments, text: &str) {
        self.buffer.push_str(text);
        self.commit_buffer_for_test(documents);
    }

    #[cfg(test)]
    fn commit_buffer_for_test(&mut self, documents: &SharedDocuments) {
        let mut registry = documents.borrow_mut();
        let open = registry.get_mut(self.document).unwrap();
        open.text_mut().sync_from_view(&self.buffer).unwrap();
    }

    /// Picks up text a sibling view, a reload, or an undo changed, without
    /// stepping on what is being typed here this frame.
    fn adopt_external_edits(&mut self, documents: &SharedDocuments) {
        let registry = documents.borrow();
        if let Some(open) = registry.get(self.document) {
            if open.text().text() != self.buffer {
                self.buffer = open.text().text().to_owned();
            }
        }
    }

    fn show_origin_bar(
        &mut self,
        ui: &mut egui::Ui,
        documents: &SharedDocuments,
        status: &DocumentStatus,
    ) {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(BAR_PADDING_X, BAR_PADDING_Y))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_height(TOOLBAR_BUTTON_HEIGHT);
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        let auto_save = status.auto_save();
                        let mut checked = auto_save.checked();
                        let checkbox = ui.add_enabled(
                            auto_save.enabled(),
                            egui::Checkbox::new(&mut checked, "Auto-save"),
                        );
                        if checkbox.changed() {
                            if let Some(open) = documents.borrow_mut().get_mut(self.document) {
                                open.set_auto_save_requested(checked);
                            }
                        }
                        ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                            let icon = if self.remote {
                                Icon::SshRemote
                            } else {
                                Icon::LocalTerminal
                            };
                            let (icon_rect, _) = ui.allocate_exact_size(
                                egui::Vec2::splat(ORIGIN_ICON_SIZE),
                                Sense::hover(),
                            );
                            icon::paint(ui.painter(), icon, icon_rect, theme::TEXT_SECONDARY);
                            label(
                                ui,
                                if self.remote { "REMOTE" } else { "LOCAL" },
                                theme::TEXT_PRIMARY,
                                true,
                            );
                            monospace_label(ui, &self.origin_label, theme::TEXT_SECONDARY);
                        });
                    });
                });
            });
    }

    fn show_command_bar(
        &mut self,
        ui: &mut egui::Ui,
        status: &DocumentStatus,
    ) -> Option<AppCommand> {
        let mut command = None;
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(BAR_PADDING_X, BAR_PADDING_Y))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_height(TOOLBAR_BUTTON_HEIGHT);
                    ui.spacing_mut().item_spacing.x = TOOLBAR_BUTTON_GAP;
                    ui.add_enabled_ui(status.can_save(), |ui| {
                        if primary_button(ui, "Save", status.can_save()) {
                            command = Some(AppCommand::SaveTextDocument);
                        }
                    });
                    if toolbar_button(ui, Some(Icon::Refresh), "Refresh", "Refresh", false) {
                        command = Some(AppCommand::RefreshTextDocument);
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        label(ui, &self.options.summary(), theme::TEXT_MUTED, false);
                    });
                });
            });
        command
    }

    fn show_body(&mut self, ui: &mut egui::Ui, documents: &SharedDocuments) {
        let footer = if self.status_bar_visible { 0.0 } else { 24.0 };
        let height = (ui.available_height() - footer).max(120.0);
        ui.allocate_ui(vec2(ui.available_width(), height), |ui| {
            ui.set_height(height);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        self.show_text(ui, documents);
                    });
                });
        });
    }

    /// The width the line-number column needs for the document it is beside.
    fn gutter_width(&self, ui: &egui::Ui) -> f32 {
        if !self.options.line_numbers {
            return 0.0;
        }
        let digits = self.buffer.lines().count().max(1).to_string().len();
        let digits = digits.max(GUTTER_MIN_DIGITS);
        let digit_width = ui
            .painter()
            .layout_no_wrap(
                "0".repeat(digits),
                FontId::monospace(EDITOR_TEXT_SIZE),
                theme::TEXT_MUTED,
            )
            .size()
            .x;
        digit_width + GUTTER_PADDING_X * 2.0
    }

    fn show_text(&mut self, ui: &mut egui::Ui, documents: &SharedDocuments) {
        let read_only = documents
            .borrow()
            .get(self.document)
            .is_some_and(|open| open.read_only());
        let gutter = self.gutter_width(ui);
        let left = ui.min_rect().left();
        ui.add_space(gutter);
        let width = ui.available_width();
        let output = egui::TextEdit::multiline(&mut self.buffer)
            .font(FontId::monospace(EDITOR_TEXT_SIZE))
            .desired_width(width)
            .desired_rows(1)
            .interactive(!read_only)
            .margin(egui::Margin::symmetric(14, 8))
            .show(ui);

        if gutter > 0.0 {
            paint_line_numbers(ui, &output, left, gutter);
        }

        if output.response.changed() {
            let mut registry = documents.borrow_mut();
            if let Some(open) = registry.get_mut(self.document) {
                if open.text_mut().sync_from_view(&self.buffer).is_err() {
                    // The edit breached a bound, so it never happened; put the
                    // widget back in step with the text that still stands.
                    self.buffer = open.text().text().to_owned();
                }
            }
        }

        if let Some(range) = output.cursor_range {
            if let Some(open) = documents.borrow().get(self.document) {
                let offset = open.text().byte_offset_of_char(range.primary.index.0);
                self.caret = open.text().line_and_column(offset);
            }
        }
    }
}

/// Numbers are painted against the laid-out text rather than against a guess
/// at line height, so a wrapped line keeps one number and the column cannot
/// drift away from the content beside it.
fn paint_line_numbers(
    ui: &egui::Ui,
    output: &egui::text_edit::TextEditOutput,
    left: f32,
    width: f32,
) {
    let painter = ui.painter();
    let font = FontId::monospace(EDITOR_TEXT_SIZE);
    let right = left + width - GUTTER_PADDING_X;
    let mut number = 1usize;
    let mut starts_line = true;
    for row in &output.galley.rows {
        if starts_line {
            painter.text(
                egui::pos2(right, output.galley_pos.y + row.pos.y),
                egui::Align2::RIGHT_TOP,
                number.to_string(),
                font.clone(),
                theme::TEXT_MUTED,
            );
        }
        starts_line = row.ends_with_newline;
        if starts_line {
            number += 1;
        }
    }
}

/// The one filled button in the tab. Save is the action the whole editor is
/// built around, so it carries the accent rather than sitting flat beside
/// commands that merely change the view.
fn primary_button(ui: &mut egui::Ui, label_text: &str, enabled: bool) -> bool {
    let font = FontId::proportional(LABEL_TEXT_SIZE);
    let colour = if enabled {
        theme::TEXT_ON_ACCENT
    } else {
        theme::TEXT_MUTED
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label_text.to_owned(), font, colour);
    let width = (PRIMARY_BUTTON_PADDING_X * 2.0 + galley.size().x).max(TOOLBAR_BUTTON_HEIGHT);
    let (rect, response) =
        ui.allocate_exact_size(vec2(width, TOOLBAR_BUTTON_HEIGHT), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label_text));

    let mut fill = theme::ACCENT_ACTION;
    if !enabled {
        fill = fill.gamma_multiply(0.35);
    } else if response.hovered() {
        fill = fill.gamma_multiply(1.15);
    }
    ui.painter().rect_filled(rect, PRIMARY_BUTTON_RADIUS, fill);
    ui.painter().galley(
        egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        colour,
    );
    response.clicked()
}

fn show_banner(ui: &mut egui::Ui, status: &DocumentStatus) -> Option<AppCommand> {
    let mut command = None;
    let accent = accent_colour(status.accent());
    egui::Frame::new()
        .fill(theme::SURFACE_PANEL)
        .inner_margin(egui::Margin {
            left: BANNER_PADDING_X,
            right: BAR_PADDING_X,
            top: BANNER_PADDING_Y,
            bottom: BANNER_PADDING_Y,
        })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let top = ui.min_rect().top();
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                label(ui, status.headline(), theme::TEXT_PRIMARY, true);
                label(ui, status.detail(), theme::TEXT_SECONDARY, false);
                if !status.actions().is_empty() {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = TOOLBAR_BUTTON_GAP;
                        for action in status.actions() {
                            if toolbar_button(ui, None, action.label(), action.label(), false) {
                                command = banner_command(*action);
                            }
                        }
                    });
                }
            });
            let bottom = ui.min_rect().bottom();
            let left = ui.min_rect().left() - f32::from(BANNER_PADDING_X);
            ui.painter().rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(left, top),
                    egui::pos2(left + BANNER_ACCENT_WIDTH, bottom),
                ),
                0.0,
                accent,
            );
        });
    command
}

const fn banner_command(action: BannerAction) -> Option<AppCommand> {
    match action {
        BannerAction::ReloadFromSource => Some(AppCommand::ReloadTextDocument),
        BannerAction::KeepMyVersion => Some(AppCommand::KeepMyTextVersion),
        BannerAction::Retry => Some(AppCommand::SaveTextDocument),
        // Compare, Save As…, and Close without saving arrive with the views
        // they open; offering them before they exist would be a button that
        // does nothing.
        _ => None,
    }
}

const fn accent_colour(accent: StatusAccent) -> egui::Color32 {
    match accent {
        StatusAccent::Settled => theme::STATUS_RUNNING,
        StatusAccent::Working | StatusAccent::Warning => theme::STATUS_STARTING,
        StatusAccent::Failing => theme::STATUS_ERROR,
    }
}

fn hairline(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().line_segment(
        [rect.left_center(), rect.right_center()],
        egui::Stroke::new(1.0, theme::BORDER_SUBTLE),
    );
}

/// Paths are read character by character, so they are set in the same
/// monospace the Markdown viewer uses for them.
fn monospace_label(ui: &mut egui::Ui, text: &str, colour: egui::Color32) {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), FontId::monospace(LABEL_TEXT_SIZE), colour);
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    ui.painter().galley(rect.left_top(), galley, colour);
}

fn label(ui: &mut egui::Ui, text: &str, colour: egui::Color32, strong: bool) {
    let font = FontId::proportional(if strong {
        LABEL_TEXT_SIZE + 1.0
    } else {
        LABEL_TEXT_SIZE
    });
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, colour);
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    ui.painter().galley(rect.left_top(), galley, colour);
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;
    use festerm_document::Severity;

    use crate::documents::DocumentRegistry;

    use super::*;

    struct TemporaryDirectory {
        path: PathBuf,
    }

    impl TemporaryDirectory {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "festerm-text-editor-{}-{label}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn file(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.path.join(name);
            fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn editor_for(path: &std::path::Path) -> (SharedDocuments, TextEditorTab) {
        let documents = DocumentRegistry::shared();
        let id = documents.borrow_mut().open_local(path).unwrap();
        let editor = TextEditorTab::new(id, &documents);
        (documents, editor)
    }

    #[test]
    fn an_editor_names_the_file_and_where_it_came_from() {
        let directory = TemporaryDirectory::new("origin");
        let path = directory.file("NOTES.md", "alpha\n");
        let (_documents, editor) = editor_for(&path);

        assert_eq!(editor.title(), "NOTES.md");
        assert!(editor.origin_label().ends_with("NOTES.md"));
        assert_eq!(editor.language_label(), "Markdown");
    }

    #[test]
    fn the_status_bar_reports_the_file_as_it_actually_is() {
        let directory = TemporaryDirectory::new("status");
        let path = directory.file("windows.txt", "alpha\r\nbeta\r\n");
        let (documents, editor) = editor_for(&path);

        assert_eq!(
            editor.status_bar_encoding(&documents),
            "UTF-8 · CRLF · Spaces: 4"
        );
        assert_eq!(editor.language_label(), "Text");
        assert_eq!(editor.status_bar_position(), "Ln 1, Col 1");
        // The bytes written back, not the bytes held: the file keeps its CRLF.
        assert_eq!(editor.status_bar_size(&documents), "13 bytes");
    }

    #[test]
    fn typing_in_one_view_is_visible_in_another_view_of_the_same_file() {
        let directory = TemporaryDirectory::new("shared");
        let path = directory.file("notes.md", "alpha\n");
        let (documents, mut first) = editor_for(&path);
        let id = documents.borrow_mut().open_local(&path).unwrap();
        let mut second = TextEditorTab::new(id, &documents);

        first.buffer.push_str("typed\n");
        first.commit_buffer_for_test(&documents);
        second.adopt_external_edits(&documents);

        assert_eq!(second.buffer, "alpha\ntyped\n");
        assert_eq!(
            second.chip_status(&documents),
            festerm_ui_egui::chrome::ChipStatus::Starting,
            "a document with unsaved changes should say so on every chip"
        );
    }

    #[test]
    fn a_clean_document_shows_no_unsaved_marker() {
        let directory = TemporaryDirectory::new("clean");
        let path = directory.file("notes.md", "alpha\n");
        let (documents, editor) = editor_for(&path);

        assert!(!editor.is_dirty(&documents));
        assert_eq!(
            editor.status(&documents).unwrap().severity(),
            Severity::Informational
        );
        assert_eq!(editor.status_bar_label(&documents), "Saved");
    }

    #[test]
    fn a_conflict_is_the_loudest_thing_the_editor_says() {
        let directory = TemporaryDirectory::new("conflict");
        let path = directory.file("notes.md", "alpha\n");
        let (documents, mut editor) = editor_for(&path);
        editor.buffer.push_str("mine\n");
        editor.commit_buffer_for_test(&documents);

        fs::write(&path, "theirs\n").unwrap();
        let id = editor.document();
        documents.borrow_mut().refresh(id);

        let status = editor.status(&documents).unwrap();
        assert_eq!(status.severity(), Severity::Blocking);
        assert!(!status.can_save());
        assert!(status.actions().contains(&BannerAction::KeepMyVersion));
        assert_eq!(
            editor.chip_status(&documents),
            festerm_ui_egui::chrome::ChipStatus::Failed
        );
    }

    /// A real pass, so the chrome is proved to lay out and the text the user
    /// would read is actually present in the accessibility tree.
    #[test]
    fn the_editor_draws_its_chrome_and_its_text() {
        let directory = TemporaryDirectory::new("draw");
        let path = directory.file("NOTES.md", "alpha\nbeta\n");
        let (documents, editor) = editor_for(&path);
        let tab_id = crate::tabs::TabId::next_for_test();

        let mut harness = Harness::builder().build_ui_state(
            move |ui, state: &mut (SharedDocuments, TextEditorTab)| {
                state.1.show(ui, tab_id, &state.0);
            },
            (documents, editor),
        );
        harness.run();

        // Present rather than merely constructed: `get_by_label` panics when
        // the label is missing from the accessibility tree.
        harness.get_by_label("LOCAL");
        harness.get_by_label("Saved");
        harness.get_by_label("Save");
        harness.get_by_label("Refresh");
        harness.get_by_label("Auto-save");
    }
}
