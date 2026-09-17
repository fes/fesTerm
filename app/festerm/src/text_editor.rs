//! The native text editor tab (ADR 0034, issue #166).
//!
//! The tab is a *view*: it owns where the caret is, what is scrolled into
//! sight, and which options are switched on. The text itself, the undo
//! history, and the answer to "are there unsaved changes" belong to the
//! document registry, so two tabs showing one file agree with each other
//! without either of them being in charge.

use std::path::PathBuf;

use eframe::egui::{self, vec2, Align, FontId, Sense, WidgetInfo, WidgetType};
use festerm_markdown::{LocalMarkdownSource, MarkdownSource};
use festerm_document::{
    AutoSaveControl, BannerAction, DocumentId, DocumentStatus, Severity, StatusAccent,
};
use festerm_ui_egui::{chrome::ChipStatus, icon, icon::Icon, theme};

use crate::documents::SharedDocuments;
use crate::markdown_viewer::{
    elide_middle, toolbar_button, toolbar_button_response, MarkdownPreviewPane, TOOLBAR_BUTTON_GAP,
    TOOLBAR_BUTTON_HEIGHT,
};
use crate::tabs::{AppCommand, TabId};
use crate::text_compare::ComparePane;

/// Width of the line-number gutter's digits area before padding.
const GUTTER_PADDING_X: f32 = 12.0;
const GUTTER_MIN_DIGITS: usize = 2;
/// The tighter gap inside one group of related toolbar controls.
const TOOLBAR_GROUP_GAP: f32 = 6.0;
const EDITOR_TEXT_SIZE: f32 = 13.0;
const BAR_PADDING_X: i8 = 9;
const BAR_PADDING_Y: i8 = 6;
const BANNER_ACCENT_WIDTH: f32 = 3.0;
const ORIGIN_ICON_SIZE: f32 = 12.0;
const MODE_SEGMENT_GAP: f32 = 2.0;
const SPLIT_DIVIDER_WIDTH: f32 = 9.0;
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

/// What a view is showing of its document. Per-view, not per-document: two
/// editors on one file may sit in different modes (ADR 0034 §4).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum EditorMode {
    #[default]
    Edit,
    Preview,
    Split,
}

impl EditorMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Edit => "Edit",
            Self::Preview => "Preview",
            Self::Split => "Split",
        }
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
    mode: EditorMode,
    /// Built the first time a rendered mode is asked for, so a file nobody
    /// previews never pays for a parse.
    preview: Option<MarkdownPreviewPane>,
    /// The Compare view, while it is open. Per-view: comparing is looking,
    /// not changing, so another window goes on editing (ADR 0034 §6).
    compare: Option<ComparePane>,
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
            mode: EditorMode::Edit,
            preview: None,
            compare: None,
        }
    }

    /// Only Markdown is rendered, and the file name is the only honest way to
    /// decide: guessing from content would move the toggle under the user as
    /// they type.
    pub(crate) fn renders_markdown(&self) -> bool {
        self.language_label() == "Markdown"
    }

    #[cfg(test)]
    pub(crate) const fn mode(&self) -> EditorMode {
        self.mode
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
        self.sync_compare(documents);

        let mut command = None;
        egui::Frame::new()
            .fill(theme::SURFACE_WINDOW)
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    self.show_origin_bar(ui);
                    hairline(ui);
                    if let Some(bar_command) = self.show_command_bar(ui, documents, &status) {
                        command = Some(bar_command);
                    }
                    hairline(ui);
                    // The banner stays pinned above Compare: the decision it
                    // asks for is the reason Compare is open (ADR 0034 §6).
                    if let Some(action) = show_banner(ui, &status, self.compare.is_some()) {
                        match action {
                            BannerAction::Compare => self.toggle_compare(documents),
                            other => command = banner_command(other),
                        }
                    }
                    self.show_body(ui, documents);
                });
            });
        command
    }

    /// The version the source now holds, when a conflict captured one.
    fn source_text(&self, documents: &SharedDocuments) -> Option<String> {
        documents.borrow().get(self.document).and_then(|open| {
            open.conflict()
                .and_then(|conflict| conflict.source_text().map(str::to_owned))
        })
    }

    /// Opens Compare, or closes it if it is already open — the banner button
    /// is the way back out as well as the way in.
    fn toggle_compare(&mut self, documents: &SharedDocuments) {
        if self.compare.is_some() {
            self.compare = None;
            return;
        }
        if let Some(source) = self.source_text(documents) {
            self.compare = Some(ComparePane::new(&self.buffer, &source, self.remote));
        }
    }

    /// Keeps Compare true, and closes it when the conflict it was about is
    /// resolved: a comparison against a version nobody is holding any more
    /// would be a view of the past presented as the present.
    fn sync_compare(&mut self, documents: &SharedDocuments) {
        if self.compare.is_none() {
            return;
        }
        let Some(source) = self.source_text(documents) else {
            self.compare = None;
            return;
        };
        if let Some(pane) = self.compare.as_mut() {
            pane.sync(&self.buffer, &source);
        }
    }

    #[cfg(test)]
    pub(crate) const fn compare(&self) -> Option<&ComparePane> {
        self.compare.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn open_compare_for_gallery(&mut self, documents: &SharedDocuments) {
        self.toggle_compare(documents);
    }

    /// Puts the view in a mode for the headless screenshot gallery, which
    /// cannot click the control.
    #[cfg(test)]
    pub(crate) const fn set_mode_for_gallery(&mut self, mode: EditorMode) {
        self.mode = mode;
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

    fn show_origin_bar(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(BAR_PADDING_X, BAR_PADDING_Y))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_height(TOOLBAR_BUTTON_HEIGHT);
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if self.renders_markdown() {
                            self.show_mode_control(ui);
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

    /// The Edit | Preview | Split toggle. A segmented control rather than
    /// three loose buttons, because they are one choice with three answers.
    /// Called inside the origin bar's right-to-left layout, so the segments
    /// are emitted last-first to read Edit | Preview | Split on screen.
    /// Nesting a left-to-right `Ui` here instead would claim the whole
    /// remaining row and paint over the path.
    fn show_mode_control(&mut self, ui: &mut egui::Ui) {
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = MODE_SEGMENT_GAP;
            for mode in [EditorMode::Split, EditorMode::Preview, EditorMode::Edit] {
                let selected = self.mode == mode;
                if toolbar_button(ui, None, mode.label(), mode.label(), selected) && !selected {
                    self.mode = mode;
                }
            }
        });
    }

    fn show_command_bar(
        &mut self,
        ui: &mut egui::Ui,
        documents: &SharedDocuments,
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
                    ui.scope(|ui| {
                        // Save and Auto-save are one group -- both answer
                        // "what happens to this document's bytes" -- so they
                        // sit closer to each other than to the view actions
                        // beyond the separator.
                        ui.spacing_mut().item_spacing.x = TOOLBAR_GROUP_GAP;
                        self.show_auto_save_control(ui, documents, status);
                    });
                    ui.add_space(TOOLBAR_GROUP_GAP);
                    ui.separator();
                    ui.add_space(TOOLBAR_GROUP_GAP);
                    if self.renders_markdown()
                        && toolbar_button(
                            ui,
                            None,
                            "Open in Markdown",
                            "Open in Markdown",
                            false,
                        )
                    {
                        command = Some(AppCommand::OpenTextDocumentInMarkdown);
                    }
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

    /// Auto-save sits beside Save rather than beside `Edit | Preview | Split`,
    /// because it belongs to the document every view shares, not to the view
    /// that happens to be showing it (ADR 0034 §7).
    fn show_auto_save_control(
        &self,
        ui: &mut egui::Ui,
        documents: &SharedDocuments,
        status: &DocumentStatus,
    ) {
        let auto_save = status.auto_save();
        let mut checked = auto_save.checked();
        // Sized to its wording rather than to the longest wording it could
        // ever take: reserving that much leaves a visible dead gap beside
        // Save in the state the row is almost always in, and the label only
        // grows at the same moment the banner above it changes height anyway.
        let checkbox = ui.add_enabled(
            auto_save.enabled(),
            egui::Checkbox::new(&mut checked, auto_save.label()),
        );
        let checkbox = match auto_save {
            AutoSaveControl::Paused | AutoSaveControl::Unavailable => checkbox
                .on_disabled_hover_text(status.detail())
                .on_hover_text(status.detail()),
            AutoSaveControl::On | AutoSaveControl::Off => checkbox,
        };
        if checkbox.changed() {
            if let Some(open) = documents.borrow_mut().get_mut(self.document) {
                open.set_auto_save_requested(checked);
            }
        }
    }

    fn show_body(&mut self, ui: &mut egui::Ui, documents: &SharedDocuments) {
        let footer = if self.status_bar_visible { 0.0 } else { 24.0 };
        let height = (ui.available_height() - footer).max(120.0);
        if let Some(compare) = self.compare.as_mut() {
            // Compare replaces the body rather than sitting beside it: two
            // versions side by side already use the whole width.
            ui.allocate_ui(vec2(ui.available_width(), height), |ui| {
                ui.set_height(height);
                compare.show(ui, height);
            });
            return;
        }
        let mode = if self.renders_markdown() {
            self.mode
        } else {
            // A toggle that is not offered cannot be left switched on, which
            // is what would happen to a Markdown file renamed to .txt.
            EditorMode::Edit
        };
        ui.allocate_ui(vec2(ui.available_width(), height), |ui| {
            ui.set_height(height);
            match mode {
                EditorMode::Edit => self.show_edit_pane(ui, documents, ui.available_width()),
                EditorMode::Preview => self.show_preview_pane(ui, height),
                EditorMode::Split => {
                    let pane_width = ((ui.available_width() - SPLIT_DIVIDER_WIDTH) / 2.0).max(120.0);
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.allocate_ui(vec2(pane_width, height), |ui| {
                            ui.set_height(height);
                            self.show_edit_pane(ui, documents, pane_width);
                        });
                        let (divider, _) =
                            ui.allocate_exact_size(vec2(SPLIT_DIVIDER_WIDTH, height), Sense::hover());
                        ui.painter().line_segment(
                            [divider.center_top(), divider.center_bottom()],
                            egui::Stroke::new(1.0, theme::BORDER_SUBTLE),
                        );
                        ui.allocate_ui(vec2(ui.available_width(), height), |ui| {
                            ui.set_height(height);
                            self.show_preview_pane(ui, height);
                        });
                    });
                }
            }
        });
    }

    fn show_edit_pane(&mut self, ui: &mut egui::Ui, documents: &SharedDocuments, width: f32) {
        egui::ScrollArea::vertical()
            .id_salt("text-editor-body")
            .auto_shrink([false, false])
            .max_width(width)
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    self.show_text(ui, documents);
                });
            });
    }

    /// The rendered view of the text being typed on the other side of the
    /// split — the same block renderer the Markdown tab uses, fed from this
    /// view's buffer instead of from a file.
    fn show_preview_pane(&mut self, ui: &mut egui::Ui, height: f32) {
        let pane = self.preview.get_or_insert_with(|| {
            MarkdownPreviewPane::new(preview_source(&self.origin_label), &self.buffer)
        });
        pane.sync(ui.ctx(), &self.buffer);
        ui.allocate_ui(vec2(ui.available_width(), height), |ui| {
            ui.set_height(height);
            pane.show(ui);
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

    /// Takes Undo and Redo away from the text widget and gives them to the
    /// document.
    ///
    /// `TextEdit` keeps a private undo history of the `String` it was handed,
    /// which knows nothing of the other views of this file, of a reload, or of
    /// a substitution committed as one transaction. Left alone it would undo
    /// this view's keystrokes only, and would happily reinstate text the
    /// document has since moved past. The events are consumed before the
    /// widget is built, which is the only point at which they can be taken
    /// from it (ADR 0034 §3).
    fn route_undo_shortcuts(
        &mut self,
        ui: &mut egui::Ui,
        documents: &SharedDocuments,
        body_id: egui::Id,
        read_only: bool,
    ) {
        if read_only || !ui.memory(|memory| memory.has_focus(body_id)) {
            return;
        }
        let (undo, redo) = ui.ctx().input_mut(|input| {
            // Redo first: `consume_key` ignores an extra Shift, so asking for
            // Cmd+Z first would swallow Cmd+Shift+Z as an undo.
            let redo = input.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z)
                | input.consume_key(egui::Modifiers::COMMAND, egui::Key::Y);
            let undo = input.consume_key(egui::Modifiers::COMMAND, egui::Key::Z);
            (undo, redo)
        });
        if !undo && !redo {
            return;
        }
        let mut registry = documents.borrow_mut();
        let Some(open) = registry.get_mut(self.document) else {
            return;
        };
        let text = open.text_mut();
        // A run of typing is still open as one transaction until something
        // closes it, so undo would otherwise step past the word just typed.
        text.close_transaction();
        let moved = if undo { text.undo() } else { text.redo() };
        if moved {
            self.buffer = open.text().text().to_owned();
        }
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
        let body_id = ui.id().with("text-editor-text");
        self.route_undo_shortcuts(ui, documents, body_id, read_only);
        let output = egui::TextEdit::multiline(&mut self.buffer)
            .id(body_id)
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

/// The identity the preview parses under. It only ever affects how relative
/// links and images are resolved, so a path the loader will not accept
/// degrades to a bare name rather than costing the user their preview.
fn preview_source(origin_label: &str) -> MarkdownSource {
    LocalMarkdownSource::new(PathBuf::from(origin_label))
        .or_else(|_| LocalMarkdownSource::new(PathBuf::from("preview.md")))
        .map(MarkdownSource::from)
        .expect("a bare file name is a valid Markdown source")
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

/// Renders the banner and reports the action pressed. Compare is handled by
/// the view rather than mapped to a command, because comparing changes what
/// this view shows and nothing about the document.
fn show_banner(
    ui: &mut egui::Ui,
    status: &DocumentStatus,
    comparing: bool,
) -> Option<BannerAction> {
    let mut pressed = None;
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
                            let compare = *action == BannerAction::Compare;
                            // Compare without a source version to compare
                            // against is offered disabled with the reason,
                            // rather than opening two panes one of which is
                            // empty (ADR 0034 §6).
                            let enabled = !compare || status.can_compare();
                            let response = ui
                                .add_enabled_ui(enabled, |ui| {
                                    toolbar_button_response(
                                        ui,
                                        None,
                                        action.label(),
                                        action.label(),
                                        compare && comparing,
                                    )
                                })
                                .inner;
                            let response = if enabled {
                                response
                            } else {
                                response.on_disabled_hover_text(status.compare_unavailable_reason())
                            };
                            if response.clicked() {
                                pressed = Some(*action);
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
    pressed
}

const fn banner_command(action: BannerAction) -> Option<AppCommand> {
    match action {
        BannerAction::ReloadFromSource => Some(AppCommand::ReloadTextDocument),
        BannerAction::KeepMyVersion => Some(AppCommand::KeepMyTextVersion),
        BannerAction::Retry => Some(AppCommand::SaveTextDocument),
        // Compare is handled inside the view. Save As… and Close without
        // saving arrive with the views they open; offering them before they
        // exist would be a button that does nothing.
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
    let font = FontId::monospace(LABEL_TEXT_SIZE);
    // A path is as long as somebody's directories are deep, and the mode
    // control has already claimed its end of the row; elide in the middle so
    // the file name survives rather than letting the path run through it.
    let character = ui
        .painter()
        .layout_no_wrap("0".to_owned(), font.clone(), colour)
        .size()
        .x
        .max(1.0);
    let budget = (ui.available_width() / character).floor().max(8.0) as usize;
    let text = &elide_middle(text, budget);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, colour);
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

    /// Drives the editor the way a person does: real key events through the
    /// harness, not a buffer assignment. Everything these tests assert is a
    /// consequence of keystrokes.
    fn typing_harness(
        path: &std::path::Path,
    ) -> Harness<'static, (SharedDocuments, TextEditorTab)> {
        let (documents, editor) = editor_for(path);
        let tab_id = crate::tabs::TabId::next_for_test();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(700.0, 420.0))
            .build_ui_state(
                move |ui, state: &mut (SharedDocuments, TextEditorTab)| {
                    state.1.show(ui, tab_id, &state.0);
                },
                (documents, editor),
            );
        harness.run();
        harness
    }

    fn document_text(harness: &Harness<'static, (SharedDocuments, TextEditorTab)>) -> String {
        let (documents, editor) = harness.state();
        documents
            .borrow()
            .get(editor.document())
            .unwrap()
            .text()
            .text()
            .to_owned()
    }

    #[test]
    fn ticking_auto_save_then_typing_puts_the_text_on_disk_without_pressing_save() {
        let directory = TemporaryDirectory::new("autosave-keystrokes");
        let path = directory.file("NOTES.md", "alpha\n");
        let mut harness = typing_harness(&path);

        harness.get_by_label("Auto-save").click();
        harness.run();

        let body = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
        body.focus();
        body.type_text("beta");
        harness.run();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "alpha\n",
            "nothing is written while the typing is still arriving"
        );

        // The first call is the frame that sees the new text; the debounce
        // runs from there, which is what the application's per-frame call does.
        let start = std::time::Instant::now();
        assert!(harness.state().0.borrow_mut().auto_save(start).is_empty());
        let written = harness
            .state()
            .0
            .borrow_mut()
            .auto_save(start + std::time::Duration::from_secs(5));

        assert_eq!(written.len(), 1);
        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\nbeta");
        harness.run();
        harness.get_by_label("Saved");
    }

    #[test]
    fn auto_save_is_offered_beside_save_and_holds_for_the_whole_document() {
        let directory = TemporaryDirectory::new("autosave-control");
        let path = directory.file("NOTES.md", "alpha\n");
        let mut harness = typing_harness(&path);

        harness.get_by_label("Auto-save").click();
        harness.run();

        let document = harness.state().1.document();
        assert!(
            harness
                .state()
                .0
                .borrow()
                .get(document)
                .unwrap()
                .auto_save_requested(),
            "the control writes through to the document, not to the view"
        );

        harness.get_by_label("Auto-save").click();
        harness.run();
        assert!(
            !harness
                .state()
                .0
                .borrow()
                .get(document)
                .unwrap()
                .auto_save_requested(),
            "and turning it off again discards nothing"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\n");
    }

    #[test]
    fn a_paused_auto_save_says_so_in_words_rather_than_by_a_tick_alone() {
        let directory = TemporaryDirectory::new("autosave-paused");
        let path = directory.file("NOTES.md", "alpha\n");
        let mut harness = typing_harness(&path);

        harness.get_by_label("Auto-save").click();
        harness.run();

        let body = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
        body.focus();
        body.type_text("beta");
        harness.run();

        // The file changes underneath the editor, which is the recoverable
        // interruption Auto-save pauses for.
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&path, "theirs\n").unwrap();
        let document = harness.state().1.document();
        harness.state().0.borrow_mut().refresh(document);
        harness.run();

        harness.get_by_label("Auto-save · paused");
        assert!(
            harness.query_by_label("Auto-save").is_none(),
            "the control cannot claim to be running while it is not"
        );
    }

    #[test]
    fn typing_into_the_body_reaches_the_shared_document_and_dirties_it() {
        let directory = TemporaryDirectory::new("typing");
        let path = directory.file("NOTES.md", "alpha\n");
        let mut harness = typing_harness(&path);

        let body = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
        body.focus();
        body.type_text("beta");
        harness.run();

        // Focus puts the caret at the end of the buffer, so this is an append.
        assert_eq!(document_text(&harness), "alpha\nbeta");
        let (documents, editor) = harness.state();
        assert!(documents
            .borrow()
            .get(editor.document())
            .unwrap()
            .text()
            .is_dirty());
        // Every surface agrees, because they all read one derived status.
        harness.get_by_label("Unsaved changes");
    }

    #[test]
    fn selecting_a_run_of_text_and_typing_replaces_it() {
        let directory = TemporaryDirectory::new("replace-selection");
        let path = directory.file("NOTES.md", "alpha\n");
        let mut harness = typing_harness(&path);

        let body = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
        body.focus();
        harness.run();
        // Select the whole buffer, then type over it — the ordinary way a
        // person replaces a line.
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.run();
        harness
            .get_by_role(egui::accesskit::Role::MultilineTextInput)
            .type_text("omega");
        harness.run();

        assert_eq!(document_text(&harness), "omega");
    }

    #[test]
    fn backspacing_removes_what_was_typed_from_the_shared_document() {
        let directory = TemporaryDirectory::new("backspace");
        let path = directory.file("NOTES.md", "alpha\n");
        let mut harness = typing_harness(&path);

        let body = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
        body.focus();
        body.type_text("x");
        harness.run();
        assert_eq!(document_text(&harness), "alpha\nx");
        harness.get_by_label("Unsaved changes");

        harness.key_press(egui::Key::Backspace);
        harness.run();

        assert_eq!(document_text(&harness), "alpha\n");
        // Still unsaved: dirty tracks the undo token, and retyping your way
        // back to the old text is two edits, not none. Undo — which *does*
        // return the token — is keyboard-routed in the vi phase; egui's own
        // undoer still owns Ctrl+Z here.
        harness.get_by_label("Unsaved changes");
    }

    #[test]
    fn typing_then_pressing_save_puts_the_typed_text_on_disk() {
        let directory = TemporaryDirectory::new("type-and-save");
        let path = directory.file("NOTES.md", "alpha\n");
        let mut harness = typing_harness(&path);

        let body = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
        body.focus();
        body.type_text("beta ");
        harness.run();

        // Save is a command the tab reports rather than an action it takes,
        // so the test dispatches it exactly as the application would.
        harness.get_by_label("Save").click();
        harness.run();
        let (documents, editor) = harness.state();
        let document = editor.document();
        documents.borrow_mut().save(document);

        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\nbeta ");
        assert!(!documents
            .borrow()
            .get(document)
            .unwrap()
            .text()
            .is_dirty());
    }

    #[test]
    fn typing_in_one_view_appears_in_another_view_of_the_same_file() {
        let directory = TemporaryDirectory::new("two-views-typing");
        let path = directory.file("NOTES.md", "alpha\n");
        let (documents, first) = editor_for(&path);
        let id = first.document();
        let second = TextEditorTab::new(id, &documents);
        let (first_id, second_id) = (
            crate::tabs::TabId::next_for_test(),
            crate::tabs::TabId::next_for_test(),
        );

        let mut harness = Harness::builder()
            .with_size(egui::vec2(700.0, 420.0))
            .build_ui_state(
                move |ui,
                      state: &mut (SharedDocuments, TextEditorTab, TextEditorTab)| {
                    ui.horizontal(|ui| {
                        ui.push_id("first", |ui| {
                            state.1.show(ui, first_id, &state.0);
                        });
                        ui.push_id("second", |ui| {
                            state.2.show(ui, second_id, &state.0);
                        });
                    });
                },
                (documents, first, second),
            );
        harness.run();

        let bodies = harness.get_all_by_role(egui::accesskit::Role::MultilineTextInput);
        let first_body = bodies.into_iter().next().expect("the first view has a body");
        first_body.focus();
        first_body.type_text("typed ");
        harness.run();
        harness.run();

        // The second view adopted the edit without being told about it.
        assert_eq!(harness.state().2.buffer, "alpha\ntyped ");
    }

    #[test]
    fn undo_and_redo_go_through_the_documents_own_history() {
        let directory = TemporaryDirectory::new("undo");
        let path = directory.file("NOTES.md", "alpha\n");
        let mut harness = typing_harness(&path);

        let body = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
        body.focus();
        body.type_text("beta");
        harness.run();
        assert_eq!(document_text(&harness), "alpha\nbeta");

        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        harness.run();

        assert_eq!(
            document_text(&harness),
            "alpha\n",
            "undo reaches the shared document, not just this view's widget"
        );
        assert_eq!(harness.state().1.buffer, "alpha\n");

        harness.key_press_modifiers(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::Z,
        );
        harness.run();

        assert_eq!(document_text(&harness), "alpha\nbeta", "and redo brings it back");
    }

    #[test]
    fn undoing_in_one_view_undoes_the_document_for_every_view() {
        let directory = TemporaryDirectory::new("undo-two-views");
        let path = directory.file("NOTES.md", "alpha\n");
        let (documents, first) = editor_for(&path);
        let id = first.document();
        let second = TextEditorTab::new(id, &documents);
        let (first_id, second_id) = (
            crate::tabs::TabId::next_for_test(),
            crate::tabs::TabId::next_for_test(),
        );

        let mut harness = Harness::builder()
            .with_size(egui::vec2(700.0, 420.0))
            .build_ui_state(
                move |ui, state: &mut (SharedDocuments, TextEditorTab, TextEditorTab)| {
                    ui.horizontal(|ui| {
                        ui.push_id("first", |ui| {
                            state.1.show(ui, first_id, &state.0);
                        });
                        ui.push_id("second", |ui| {
                            state.2.show(ui, second_id, &state.0);
                        });
                    });
                },
                (documents, first, second),
            );
        harness.run();

        let bodies = harness.get_all_by_role(egui::accesskit::Role::MultilineTextInput);
        let first_body = bodies.into_iter().next().expect("the first view has a body");
        first_body.focus();
        first_body.type_text("typed");
        harness.run();
        harness.run();
        assert_eq!(harness.state().2.buffer, "alpha\ntyped");

        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        harness.run();
        harness.run();

        assert_eq!(
            harness.state().1.buffer,
            "alpha\n",
            "the view that pressed it sees the undo"
        );
        assert_eq!(
            harness.state().2.buffer,
            "alpha\n",
            "and so does the view that did not, because the history is the document's"
        );
    }

    #[test]
    fn a_whole_run_of_typing_is_one_undo_rather_than_one_per_keystroke() {
        let directory = TemporaryDirectory::new("undo-run");
        let path = directory.file("NOTES.md", "alpha\n");
        let mut harness = typing_harness(&path);

        let body = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
        body.focus();
        body.type_text("bravo");
        harness.run();

        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        harness.run();

        assert_eq!(
            document_text(&harness),
            "alpha\n",
            "one press takes back the word, not its last letter"
        );
    }

    #[test]
    fn the_mode_control_is_offered_only_for_markdown() {
        let directory = TemporaryDirectory::new("mode-offer");
        let (_, markdown) = editor_for(&directory.file("NOTES.md", "# Title\n"));
        let (_, plain) = editor_for(&directory.file("hosts.txt", "127.0.0.1\n"));

        assert!(markdown.renders_markdown());
        assert!(!plain.renders_markdown());
    }

    #[test]
    fn a_markdown_editor_can_be_switched_between_its_three_modes() {
        let directory = TemporaryDirectory::new("modes");
        let path = directory.file("NOTES.md", "# Title\n\nProse.\n");
        let (documents, editor) = editor_for(&path);
        let tab_id = crate::tabs::TabId::next_for_test();

        let mut harness = Harness::builder().build_ui_state(
            move |ui, state: &mut (SharedDocuments, TextEditorTab)| {
                state.1.show(ui, tab_id, &state.0);
            },
            (documents, editor),
        );
        harness.run();
        assert_eq!(harness.state().1.mode(), EditorMode::Edit);

        harness.get_by_label("Split").click();
        harness.run();
        assert_eq!(harness.state().1.mode(), EditorMode::Split);
        // The pane beside the editor is rendering this document's text, not
        // a stale copy of a file. (Its blocks are painted rather than built
        // from widgets, so the rendering itself is not in the a11y tree.)
        let pane = harness.state().1.preview.as_ref().expect("split built a preview");
        assert_eq!(pane.rendered_text(), "# Title\n\nProse.\n");

        harness.get_by_label("Preview").click();
        harness.run();
        assert_eq!(harness.state().1.mode(), EditorMode::Preview);
    }

    #[test]
    fn a_plain_text_view_stays_in_edit_mode_even_if_its_mode_is_set() {
        let directory = TemporaryDirectory::new("plain-mode");
        let path = directory.file("hosts.txt", "# not a heading\n");
        let (documents, mut editor) = editor_for(&path);
        editor.mode = EditorMode::Preview;
        let tab_id = crate::tabs::TabId::next_for_test();

        let mut harness = Harness::builder().build_ui_state(
            move |ui, state: &mut (SharedDocuments, TextEditorTab)| {
                state.1.show(ui, tab_id, &state.0);
            },
            (documents, editor),
        );
        harness.run();

        // The body fell back to the editor, so no preview pane was built.
        assert!(harness.state().1.preview.is_none());
    }

    /// Arranges an honest conflict: type into the document through the
    /// keyboard, then change the file underneath it and let the refresh that
    /// notices raise the banner.
    fn conflicted_harness(
        directory: &TemporaryDirectory,
        mine: &str,
        theirs: &str,
    ) -> Harness<'static, (SharedDocuments, TextEditorTab)> {
        let path = directory.file("NOTES.md", "alpha\nbravo\ncharlie\n");
        let mut harness = typing_harness(&path);

        let body = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
        body.focus();
        body.type_text(mine);
        harness.run();

        fs::write(&path, theirs).unwrap();
        let id = harness.state().1.document();
        harness.state_mut().0.borrow_mut().refresh(id);
        harness.run();
        harness
    }

    #[test]
    fn pressing_compare_shows_both_versions_side_by_side() {
        let directory = TemporaryDirectory::new("compare-open");
        let mut harness =
            conflicted_harness(&directory, "delta", "alpha\nBRAVO\ncharlie\n");

        harness.get_by_label("This file changed on disk");
        assert!(harness.state().1.compare().is_none());

        harness.get_by_label("Compare").click();
        harness.run();

        let compare = harness.state().1.compare().expect("Compare is open");
        let text = compare.as_text();
        assert!(text.contains("-bravo"), "{text}");
        assert!(text.contains("+BRAVO"), "{text}");
        assert!(text.contains("-delta"), "the line only I have: {text}");

        // The banner is still there: the decision Compare exists to inform is
        // one click above the evidence.
        harness.get_by_label("This file changed on disk");
        harness.get_by_label("Keep my version");
        // Both headings name where their version is.
        harness.get_by_label("Your version · unsaved");
        harness.get_by_label("On disk");
        // And the editing body is gone, because Compare is read-only.
        assert!(
            harness
                .query_by_role(egui::accesskit::Role::MultilineTextInput)
                .is_none(),
            "Compare must not leave an editable body behind"
        );
    }

    #[test]
    fn pressing_compare_again_closes_it() {
        let directory = TemporaryDirectory::new("compare-toggle");
        let mut harness =
            conflicted_harness(&directory, "delta", "alpha\nBRAVO\ncharlie\n");

        harness.get_by_label("Compare").click();
        harness.run();
        assert!(harness.state().1.compare().is_some());

        harness.get_by_label("Compare").click();
        harness.run();
        assert!(harness.state().1.compare().is_none());
        harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
    }

    #[test]
    fn next_change_walks_the_comparison() {
        let directory = TemporaryDirectory::new("compare-navigate");
        let mut harness =
            conflicted_harness(&directory, "delta", "alpha\nBRAVO\ncharlie\n");

        harness.get_by_label("Compare").click();
        harness.run();
        assert_eq!(harness.state().1.compare().unwrap().focused_change(), None);

        harness.get_by_label("Next change").click();
        harness.run();
        assert_eq!(
            harness.state().1.compare().unwrap().focused_change(),
            Some(0)
        );

        harness.get_by_label("Previous change").click();
        harness.run();
        // Previous from the first change wraps to the last one.
        let compare = harness.state().1.compare().unwrap();
        assert_eq!(compare.focused_change(), Some(compare.change_count() - 1));
    }

    #[test]
    fn resolving_the_conflict_closes_compare() {
        let directory = TemporaryDirectory::new("compare-resolved");
        let mut harness =
            conflicted_harness(&directory, "delta", "alpha\nBRAVO\ncharlie\n");

        harness.get_by_label("Compare").click();
        harness.run();
        assert!(harness.state().1.compare().is_some());

        // Keep my version dismisses the conflict; a comparison against a
        // version nobody is holding any more would be the past shown as the
        // present.
        harness.get_by_label("Keep my version").click();
        harness.run();
        let (documents, editor) = harness.state_mut();
        let id = editor.document();
        documents.borrow_mut().get_mut(id).unwrap().keep_my_version();
        harness.run();

        assert!(harness.state().1.compare().is_none());
        harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
    }

    #[test]
    fn compare_follows_a_sibling_view_that_keeps_typing() {
        let directory = TemporaryDirectory::new("compare-live");
        let mut harness =
            conflicted_harness(&directory, "delta", "alpha\nBRAVO\ncharlie\n");

        harness.get_by_label("Compare").click();
        harness.run();
        let before = harness.state().1.compare().unwrap().change_count();

        // Another view of the same document types on.
        let (documents, editor) = harness.state_mut();
        let id = editor.document();
        let mut sibling = TextEditorTab::new(id, documents);
        sibling.type_for_gallery(documents, "echo\n");
        harness.run();

        let after = harness.state().1.compare().unwrap();
        assert!(
            after.as_text().contains("echo"),
            "Compare must show what the document now holds: {}",
            after.as_text()
        );
        assert!(after.change_count() >= before);
    }

    #[test]
    fn compare_is_disabled_with_a_reason_when_the_source_cannot_be_read() {
        let status = DocumentStatus::derive(&festerm_document::StatusInputs {
            dirty: true,
            conflict: Some(festerm_document::ConflictState::new("it changed")),
            ..Default::default()
        });

        assert!(status.actions().contains(&BannerAction::Compare));
        assert!(
            !status.can_compare(),
            "a conflict with no source text has nothing to compare against"
        );
        assert_eq!(
            status.compare_unavailable_reason(),
            "The version on the source could not be read."
        );
    }

    #[test]
    fn the_preview_reparses_only_once_typing_settles() {
        let source = MarkdownSource::from(
            LocalMarkdownSource::new(PathBuf::from("NOTES.md")).unwrap(),
        );
        let mut pane = MarkdownPreviewPane::new(source, "# One\n");
        let ctx = egui::Context::default();

        pane.sync(&ctx, "# Two\n");
        assert_eq!(pane.rendered_text(), "# One\n");

        std::thread::sleep(crate::markdown_viewer::PREVIEW_DEBOUNCE);
        pane.sync(&ctx, "# Two\n");
        assert_eq!(pane.rendered_text(), "# Two\n");
    }
}
