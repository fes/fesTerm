use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use eframe::egui::{
    self, text::LayoutJob, text::TextFormat, vec2, Align, Color32, FontId, RichText, Sense,
    WidgetInfo, WidgetType,
};
use festerm_markdown::{
    Block, CodeBlock, ContainerInline, HeadingBlock, HighlightStyle, HighlightedCodeLine,
    ImageInline, Inline, LinkInline, ListBlock, ListKind, LocalMarkdownSource, MarkdownDocument,
    MarkdownLoadError, MarkdownLoader, MarkdownSource, MarkdownSourceError, RawHtmlBlock,
    ResourceReferenceClass, ResourceReferenceKind, SourceSpan, TableAlignment, TableBlock,
    TaskState, TextBlock, TextMatch,
};
use festerm_ui_egui::{icon, icon::Icon, theme};

use crate::tabs::{AppCommand, ExternalLinkTarget, TabId};

const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
const OUTLINE_WIDTH: f32 = 220.0;
const READING_WIDTH: f32 = 860.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownViewerMode {
    Preview,
    Source,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingScroll {
    Heading(usize),
    Byte(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScrollAnchor {
    heading_index: Option<usize>,
    section_offset_numerator: usize,
    section_offset_denominator: usize,
}

impl ScrollAnchor {
    fn top() -> Self {
        Self {
            heading_index: None,
            section_offset_numerator: 0,
            section_offset_denominator: 1,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LocalLoadError {
    NotFound,
    PermissionDenied,
    NotAFile,
    Io,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum MarkdownViewerLoadFailure {
    Source(MarkdownSourceError),
    Load(MarkdownLoadError),
    Local(LocalLoadError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownViewerErrorState {
    pub title: &'static str,
    pub detail: String,
    pub stale_snapshot: bool,
    pub source_unavailable: bool,
}

impl MarkdownViewerErrorState {
    fn from_source_error(error: MarkdownSourceError, stale_snapshot: bool) -> Self {
        let detail = error.to_string();
        match error {
            MarkdownSourceError::EmptyPath => Self {
                title: "Markdown source is unavailable",
                detail,
                stale_snapshot,
                source_unavailable: true,
            },
            MarkdownSourceError::EmptyRemoteHost
            | MarkdownSourceError::WhitespaceRemoteHost
            | MarkdownSourceError::ZeroRemotePort
            | MarkdownSourceError::EmptyRemoteUsername
            | MarkdownSourceError::EmptyRemoteProfileIdentifier
            | MarkdownSourceError::EmptyVerifiedFingerprint => Self {
                title: "Markdown source identity is invalid",
                detail,
                stale_snapshot,
                source_unavailable: true,
            },
        }
    }

    fn from_load_error(error: MarkdownLoadError, stale_snapshot: bool) -> Self {
        let title = match error {
            MarkdownLoadError::Cancelled => "Markdown loading was cancelled",
            MarkdownLoadError::InvalidUtf8 => "Markdown source is not valid UTF-8",
            MarkdownLoadError::BinaryContent => "Markdown source appears to contain binary content",
            MarkdownLoadError::OversizeInput { .. } => "Markdown source exceeds the size limit",
            MarkdownLoadError::TooManyLines { .. } => "Markdown source exceeds the line limit",
            MarkdownLoadError::ExcessiveNesting { .. } => {
                "Markdown source exceeds the nesting limit"
            }
            MarkdownLoadError::TooManyTableCells { .. } => {
                "Markdown source exceeds the table limit"
            }
            MarkdownLoadError::CodeBlockTooLarge { .. } => {
                "Markdown source exceeds the code-block limit"
            }
            MarkdownLoadError::TooManyResourceReferences { .. } => {
                "Markdown source exceeds the resource-reference limit"
            }
            MarkdownLoadError::ParseModelInvariant => {
                "Markdown source could not be rendered safely"
            }
        };
        Self {
            title,
            detail: error.to_string(),
            stale_snapshot,
            source_unavailable: false,
        }
    }

    fn from_local_error(error: LocalLoadError, stale_snapshot: bool) -> Self {
        let (title, detail, unavailable) = match error {
            LocalLoadError::NotFound => (
                "Markdown source is unavailable",
                "The local file no longer exists.".to_owned(),
                true,
            ),
            LocalLoadError::PermissionDenied => (
                "Markdown source could not be read",
                "The local file could not be read because permission was denied.".to_owned(),
                false,
            ),
            LocalLoadError::NotAFile => (
                "Markdown source is unavailable",
                "The selected path is not a regular file.".to_owned(),
                true,
            ),
            LocalLoadError::Io => (
                "Markdown source could not be read",
                "The local file could not be read.".to_owned(),
                false,
            ),
        };
        Self {
            title,
            detail,
            stale_snapshot,
            source_unavailable: unavailable,
        }
    }

    fn from_failure(failure: MarkdownViewerLoadFailure, stale_snapshot: bool) -> Self {
        match failure {
            MarkdownViewerLoadFailure::Source(error) => {
                Self::from_source_error(error, stale_snapshot)
            }
            MarkdownViewerLoadFailure::Load(error) => Self::from_load_error(error, stale_snapshot),
            MarkdownViewerLoadFailure::Local(error) => {
                Self::from_local_error(error, stale_snapshot)
            }
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResourceApprovalState {
    approved: HashSet<usize>,
}

impl ResourceApprovalState {
    fn is_approved(&self, reference_index: usize) -> bool {
        self.approved.contains(&reference_index)
    }

    fn approve(&mut self, reference_index: usize) {
        self.approved.insert(reference_index);
    }

    fn clear(&mut self) {
        self.approved.clear();
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MarkdownFindState {
    open: bool,
    query: String,
    matches: Vec<TextMatch>,
    current_index: Option<usize>,
}

impl MarkdownFindState {
    pub fn is_open(&self) -> bool {
        self.open
    }

    fn open(&mut self) {
        self.open = true;
    }

    fn query(&self) -> &str {
        &self.query
    }

    fn set_query(&mut self, document: &MarkdownDocument, query: String) {
        self.query = query;
        self.recompute(document, None);
        self.open = true;
    }

    fn clear(&mut self) {
        self.query.clear();
        self.matches.clear();
        self.current_index = None;
        self.open = false;
    }

    fn matches(&self) -> &[TextMatch] {
        &self.matches
    }

    fn current_match(&self) -> Option<&TextMatch> {
        self.current_index.and_then(|index| self.matches.get(index))
    }

    fn next(&mut self) -> Option<&TextMatch> {
        self.advance(false)
    }

    fn previous(&mut self) -> Option<&TextMatch> {
        self.advance(true)
    }

    fn current_label(&self) -> String {
        match (self.current_index, self.matches.len()) {
            (Some(index), total) if total > 0 => format!("{} of {}", index + 1, total),
            _ => "0 of 0".to_owned(),
        }
    }

    fn recompute(&mut self, document: &MarkdownDocument, preserved_span: Option<SourceSpan>) {
        self.matches = document.find_matches(&self.query);
        self.current_index = if self.matches.is_empty() {
            None
        } else if let Some(span) = preserved_span {
            self.matches
                .iter()
                .position(|candidate| candidate.span() == span)
                .or(Some(0))
        } else {
            Some(0)
        };
    }

    fn restore_for_reload(&mut self, document: &MarkdownDocument) {
        let preserved_span = self.current_match().map(TextMatch::span);
        if self.query.is_empty() {
            self.matches.clear();
            self.current_index = None;
            return;
        }
        self.recompute(document, preserved_span);
        self.open = true;
    }

    fn advance(&mut self, reverse: bool) -> Option<&TextMatch> {
        let total = self.matches.len();
        if total == 0 {
            self.current_index = None;
            return None;
        }
        let current = self.current_index.unwrap_or(0);
        self.current_index = Some(if reverse {
            (current + total - 1) % total
        } else {
            (current + 1) % total
        });
        self.current_match()
    }
}

struct LoadedImage {
    texture: egui::TextureHandle,
    size: [usize; 2],
}

pub struct MarkdownViewerTab {
    source: MarkdownSource,
    title: String,
    display_path: String,
    mode: MarkdownViewerMode,
    outline_open: bool,
    outline_selected: Option<usize>,
    document: Option<MarkdownDocument>,
    error: Option<MarkdownViewerErrorState>,
    stale_snapshot: bool,
    find: MarkdownFindState,
    resource_approvals: ResourceApprovalState,
    loaded_images: BTreeMap<usize, LoadedImage>,
    image_errors: BTreeMap<usize, String>,
    pending_scroll: Option<PendingScroll>,
}

impl MarkdownViewerTab {
    pub fn open_local(path: PathBuf) -> Self {
        let display_path = path.display().to_string();
        let title = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("Markdown")
            .to_owned();
        let source_path = fs::canonicalize(&path).unwrap_or(path.clone());
        let source = LocalMarkdownSource::new(source_path.clone())
            .map(MarkdownSource::from)
            .unwrap_or_else(|_| {
                MarkdownSource::from(
                    LocalMarkdownSource::new(PathBuf::from("Markdown"))
                        .expect("fallback path is valid"),
                )
            });
        let mut tab = Self {
            source,
            title,
            display_path,
            mode: MarkdownViewerMode::Preview,
            outline_open: true,
            outline_selected: None,
            document: None,
            error: None,
            stale_snapshot: false,
            find: MarkdownFindState::default(),
            resource_approvals: ResourceApprovalState::default(),
            loaded_images: BTreeMap::new(),
            image_errors: BTreeMap::new(),
            pending_scroll: None,
        };
        tab.reload();
        tab
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn chip_secondary(&self) -> &'static str {
        match self.source {
            MarkdownSource::Local(_) => "Markdown · Local",
            MarkdownSource::Remote(_) => "Markdown · Remote",
        }
    }

    pub fn display_path(&self) -> &str {
        &self.display_path
    }

    pub fn matches_local_path(&self, path: &Path) -> bool {
        let Ok(candidate) = fs::canonicalize(path) else {
            return false;
        };
        matches!(&self.source, MarkdownSource::Local(local) if local.path() == &candidate)
    }

    pub fn show(&mut self, ui: &mut egui::Ui, tab_id: TabId) -> Option<AppCommand> {
        let mut command = self.consume_shortcuts(ui.ctx(), tab_id);
        egui::Frame::new()
            .fill(theme::SURFACE_WINDOW)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    if command.is_none() {
                        command = self.show_toolbar(ui, tab_id);
                    }
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);
                    if let Some(document) = self.document.clone() {
                        if document.source_text().is_empty() {
                            ui.vertical_centered(|ui| {
                                ui.add_space(24.0);
                                ui.heading("This Markdown file is empty");
                            });
                        } else {
                            self.show_document(ui, &document);
                        }
                    } else if let Some(error) = self.error.clone() {
                        self.show_error(ui, &error, tab_id, &mut command);
                    } else {
                        ui.vertical_centered(|ui| {
                            ui.add_space(24.0);
                            ui.heading("Loading Markdown");
                        });
                    }
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);
                    self.show_footer(ui);
                });
            });
        command
    }

    pub fn reload(&mut self) {
        let anchor = self.current_anchor();
        let result = match &self.source {
            MarkdownSource::Local(local) => load_local_document(local.path().clone()),
            MarkdownSource::Remote(_) => Err(MarkdownViewerLoadFailure::Source(
                MarkdownSourceError::EmptyRemoteHost,
            )),
        };
        match result {
            Ok((display_path, document)) => {
                self.display_path = display_path;
                self.outline_selected = anchor
                    .heading_index
                    .or_else(|| document.headings().first().map(|_| 0));
                self.pending_scroll = Some(pending_scroll_for_anchor(&document, anchor));
                self.document = Some(document);
                self.error = None;
                self.stale_snapshot = false;
                self.resource_approvals.clear();
                self.loaded_images.clear();
                self.image_errors.clear();
                if let Some(document) = &self.document {
                    self.find.restore_for_reload(document);
                }
            }
            Err(failure) => {
                let has_snapshot = self.document.is_some();
                self.stale_snapshot = has_snapshot;
                self.error = Some(MarkdownViewerErrorState::from_failure(
                    failure,
                    has_snapshot,
                ));
            }
        }
    }

    pub fn toggle_mode(&mut self) {
        let anchor = self.current_anchor();
        self.mode = match self.mode {
            MarkdownViewerMode::Preview => MarkdownViewerMode::Source,
            MarkdownViewerMode::Source => MarkdownViewerMode::Preview,
        };
        if let Some(document) = &self.document {
            self.pending_scroll = Some(pending_scroll_for_anchor(document, anchor));
        }
    }

    pub fn toggle_outline(&mut self) {
        self.outline_open = !self.outline_open;
    }

    pub fn open_find(&mut self) {
        self.find.open();
    }

    pub fn handle_escape(&mut self) -> bool {
        if self.find.is_open() || !self.find.query().is_empty() {
            self.find.clear();
            return false;
        }
        true
    }

    pub fn advance_find(&mut self, reverse: bool) {
        let next = if reverse {
            self.find.previous()
        } else {
            self.find.next()
        };
        if let (Some(document), Some(found)) = (&self.document, next) {
            self.pending_scroll = Some(PendingScroll::Byte(found.span().start().byte_offset()));
            self.outline_selected = document
                .nearest_heading_at_byte(found.span().start().byte_offset())
                .and_then(|heading| {
                    document
                        .headings()
                        .iter()
                        .position(|candidate| candidate == heading)
                });
        }
    }

    pub fn load_local_image(&mut self, reference_index: usize, context: &egui::Context) {
        let Some(document) = &self.document else {
            return;
        };
        let Some(reference) = document.resource_references().get(reference_index) else {
            return;
        };
        let Some(local) = local_document_source(&self.source) else {
            self.image_errors.insert(
                reference_index,
                "Only local Markdown documents can load local images.".to_owned(),
            );
            return;
        };
        if reference.kind() != ResourceReferenceKind::Image {
            return;
        }
        if reference.class() != ResourceReferenceClass::LocalRelative {
            self.image_errors.insert(
                reference_index,
                resource_placeholder_action(reference.class()).to_owned(),
            );
            return;
        }
        match read_local_image(local.path(), reference.target()) {
            Ok(image) => {
                let texture = context.load_texture(
                    format!("markdown-image-{}-{}", self.title, reference_index),
                    image,
                    egui::TextureOptions::LINEAR,
                );
                self.resource_approvals.approve(reference_index);
                self.loaded_images.insert(
                    reference_index,
                    LoadedImage {
                        size: texture.size(),
                        texture,
                    },
                );
                self.image_errors.remove(&reference_index);
            }
            Err(message) => {
                self.resource_approvals.approve(reference_index);
                self.image_errors.insert(reference_index, message);
            }
        }
    }

    fn consume_shortcuts(&mut self, context: &egui::Context, tab_id: TabId) -> Option<AppCommand> {
        if context.input_mut(|input| {
            input.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::V,
            )
        }) {
            return Some(AppCommand::ToggleMarkdownPreviewSource);
        }
        if context.input_mut(|input| {
            input.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::O,
            )
        }) {
            return Some(AppCommand::ToggleMarkdownOutline);
        }
        if context.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::R)) {
            return Some(AppCommand::ReloadMarkdown);
        }
        if context.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
            return Some(AppCommand::OpenMarkdownFind);
        }
        if self.find.is_open() && context.input(|input| input.key_pressed(egui::Key::Enter)) {
            return Some(AppCommand::NavigateMarkdownFind {
                reverse: context.input(|input| input.modifiers.shift),
            });
        }
        if context.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            && self.handle_escape()
        {
            return Some(AppCommand::CloseTab(tab_id));
        }
        None
    }

    fn show_toolbar(&mut self, ui: &mut egui::Ui, tab_id: TabId) -> Option<AppCommand> {
        let mut command = None;
        ui.horizontal_wrapped(|ui| {
            icon_label(
                ui,
                Icon::MarkdownDocument,
                match self.source {
                    MarkdownSource::Local(_) => "Local",
                    MarkdownSource::Remote(_) => "Remote",
                },
            );
            ui.label(
                RichText::new(elide_middle(self.display_path(), 72))
                    .small()
                    .monospace(),
            );
            ui.add_space(12.0);
            if small_toolbar_button(
                ui,
                Icon::RenderedView,
                match self.mode {
                    MarkdownViewerMode::Preview => "Preview",
                    MarkdownViewerMode::Source => "Rendered",
                },
                "Toggle Preview/Source (Ctrl/Cmd+Shift+V)",
            ) {
                command = Some(AppCommand::ToggleMarkdownPreviewSource);
            }
            if small_toolbar_button(
                ui,
                Icon::SourceView,
                match self.mode {
                    MarkdownViewerMode::Preview => "Source",
                    MarkdownViewerMode::Source => "Preview",
                },
                "Toggle Preview/Source (Ctrl/Cmd+Shift+V)",
            ) {
                command = Some(AppCommand::ToggleMarkdownPreviewSource);
            }
            if small_toolbar_button(ui, Icon::Search, "Find", "Find (Ctrl/Cmd+F)") {
                command = Some(AppCommand::OpenMarkdownFind);
            }
            if small_toolbar_button(
                ui,
                Icon::Outline,
                if self.outline_open {
                    "Hide outline"
                } else {
                    "Show outline"
                },
                "Toggle outline (Ctrl/Cmd+Shift+O)",
            ) {
                command = Some(AppCommand::ToggleMarkdownOutline);
            }
            if small_toolbar_button(ui, Icon::Reconnect, "Reload", "Reload (Ctrl/Cmd+R)") {
                command = Some(AppCommand::ReloadMarkdown);
            }
            if small_toolbar_button(ui, Icon::Overflow, "Close", "Close viewer") {
                command = Some(AppCommand::CloseTab(tab_id));
            }
        });
        if self.find.is_open() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let mut query = self.find.query().to_owned();
                let response = ui.add_sized(
                    vec2(220.0, 24.0),
                    egui::TextEdit::singleline(&mut query).hint_text("Find"),
                );
                response.widget_info(|| {
                    WidgetInfo::labeled(WidgetType::TextEdit, true, "Find Markdown")
                });
                if response.changed() {
                    if let Some(document) = &self.document {
                        self.find.set_query(document, query);
                    } else {
                        self.find.query = query;
                    }
                }
                ui.label(self.find.current_label());
                if ui.small_button("Previous").clicked() {
                    command = Some(AppCommand::NavigateMarkdownFind { reverse: true });
                }
                if ui.small_button("Next").clicked() {
                    command = Some(AppCommand::NavigateMarkdownFind { reverse: false });
                }
                if ui.small_button("Clear").clicked() {
                    self.find.clear();
                }
            });
        }
        command
    }

    fn show_document(&mut self, ui: &mut egui::Ui, document: &MarkdownDocument) {
        ui.horizontal(|ui| {
            if self.outline_open {
                egui::Frame::new()
                    .fill(theme::SURFACE_TAB_INACTIVE)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(OUTLINE_WIDTH);
                        icon_label(ui, Icon::Outline, "Outline");
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical()
                            .id_salt("markdown-outline")
                            .show(ui, |ui| {
                                for (index, heading) in document.headings().iter().enumerate() {
                                    let selected = self.outline_selected == Some(index);
                                    ui.horizontal(|ui| {
                                        ui.add_space(
                                            (heading.level().saturating_sub(1) as f32) * 12.0,
                                        );
                                        let response =
                                            ui.selectable_label(selected, heading.text());
                                        response.widget_info(|| {
                                            WidgetInfo::labeled(
                                                WidgetType::Button,
                                                true,
                                                format!(
                                                    "Heading level {}: {}",
                                                    heading.level(),
                                                    heading.text()
                                                ),
                                            )
                                        });
                                        if response.clicked() {
                                            self.outline_selected = Some(index);
                                            self.pending_scroll =
                                                Some(PendingScroll::Heading(index));
                                        }
                                    });
                                }
                            });
                    });
                ui.add_space(12.0);
            }
            egui::ScrollArea::vertical()
                .id_salt("markdown-document")
                .show(ui, |ui| {
                    ui.set_max_width(READING_WIDTH.min(ui.available_width()));
                    match self.mode {
                        MarkdownViewerMode::Preview => {
                            for block in document.blocks() {
                                self.render_block(ui, block, document);
                                ui.add_space(10.0);
                            }
                        }
                        MarkdownViewerMode::Source => {
                            self.render_source(ui, document);
                        }
                    }
                });
        });
    }

    fn show_error(
        &mut self,
        ui: &mut egui::Ui,
        error: &MarkdownViewerErrorState,
        _tab_id: TabId,
        command: &mut Option<AppCommand>,
    ) {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.heading(error.title);
            ui.add_space(6.0);
            ui.label(&error.detail);
            if error.stale_snapshot {
                ui.label(
                    RichText::new("Showing the last complete snapshot.")
                        .small()
                        .color(theme::TEXT_SECONDARY),
                );
            }
            ui.add_space(8.0);
            if ui.button("Retry").clicked() {
                *command = Some(AppCommand::ReloadMarkdown);
            }
        });
    }

    fn show_footer(&self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(match self.source {
                    MarkdownSource::Local(_) => "Local",
                    MarkdownSource::Remote(_) => "Remote",
                })
                .small()
                .color(theme::TEXT_SECONDARY),
            );
            ui.label(RichText::new("UTF-8").small().color(theme::TEXT_SECONDARY));
            if self.stale_snapshot {
                let label = if self
                    .error
                    .as_ref()
                    .is_some_and(|error| error.source_unavailable)
                {
                    "Source unavailable"
                } else {
                    "Stale snapshot"
                };
                ui.label(RichText::new(label).small().color(theme::STATUS_STARTING));
            }
            if let Some(error) = &self.error {
                ui.label(
                    RichText::new(error.title)
                        .small()
                        .color(theme::STATUS_ERROR),
                );
            }
        });
    }

    fn render_block(&mut self, ui: &mut egui::Ui, block: &Block, document: &MarkdownDocument) {
        match block {
            Block::Paragraph(block) => render_text_block(
                ui,
                block,
                document,
                &self.find,
                &self.resource_approvals,
                &self.loaded_images,
                &self.image_errors,
                &mut self.pending_scroll,
                &mut self.outline_selected,
            ),
            Block::Heading(block) => self.render_heading_block(ui, block, document),
            Block::BlockQuote(block) => {
                egui::Frame::new()
                    .fill(theme::SURFACE_TAB_INACTIVE)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        for nested in block.blocks() {
                            self.render_block(ui, nested, document);
                        }
                    });
            }
            Block::List(block) => self.render_list(ui, block, document),
            Block::Table(block) => self.render_table(ui, block, document),
            Block::CodeBlock(block) => self.render_code_block(ui, block),
            Block::Html(block) => self.render_html_block(ui, block),
            Block::Rule { .. } => {
                ui.separator();
            }
        }
    }

    fn render_heading_block(
        &mut self,
        ui: &mut egui::Ui,
        block: &HeadingBlock,
        document: &MarkdownDocument,
    ) {
        let size = match block.level() {
            1 => 28.0,
            2 => 24.0,
            3 => 20.0,
            4 => 18.0,
            _ => 16.0,
        };
        let heading_index = block.heading_index();
        let selected = self.outline_selected == Some(heading_index);
        let mut job = inline_layout_job(
            block.inlines(),
            document,
            &self.find,
            FontId::proportional(size),
            false,
        );
        if selected {
            for section in &mut job.sections {
                section.format.background = theme::SURFACE_SELECTION;
            }
        }
        let response = ui.add(egui::Label::new(job).selectable(true).wrap());
        response.widget_info(|| {
            WidgetInfo::labeled(
                WidgetType::Label,
                true,
                format!("Heading level {}: {}", block.level(), block.plain_text()),
            )
        });
        if matches!(self.pending_scroll, Some(PendingScroll::Heading(index)) if index == heading_index)
        {
            response.scroll_to_me(Some(Align::Center));
            self.pending_scroll = None;
        }
        if response.clicked() {
            self.outline_selected = Some(heading_index);
        }
        if let Some(current) = self.find.current_match() {
            if overlaps(block.span(), current.span()) {
                self.outline_selected = document
                    .nearest_heading_at_byte(current.span().start().byte_offset())
                    .and_then(|heading| {
                        document
                            .headings()
                            .iter()
                            .position(|candidate| candidate == heading)
                    });
            }
        }
    }

    fn render_list(&mut self, ui: &mut egui::Ui, block: &ListBlock, document: &MarkdownDocument) {
        for (index, item) in block.items().iter().enumerate() {
            ui.horizontal_top(|ui| {
                match item.task_state() {
                    Some(TaskState::Checked) => {
                        ui.label("☑");
                    }
                    Some(TaskState::Unchecked) => {
                        ui.label("☐");
                    }
                    None => match block.kind() {
                        ListKind::Bullet => {
                            ui.label("•");
                        }
                        ListKind::Ordered { first_item_number } => {
                            let number = first_item_number + index as u64;
                            ui.label(format!("{number}."));
                        }
                    },
                };
                ui.vertical(|ui| {
                    for nested in item.blocks() {
                        self.render_block(ui, nested, document);
                    }
                });
            });
        }
    }

    fn render_table(&mut self, ui: &mut egui::Ui, block: &TableBlock, document: &MarkdownDocument) {
        egui::Frame::new()
            .fill(theme::SURFACE_TAB_INACTIVE)
            .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    egui::Grid::new(("markdown-table", block.span().byte_range().start))
                        .striped(true)
                        .show(ui, |ui| {
                            for row in block.rows() {
                                for (column, cell) in row.cells().iter().enumerate() {
                                    let alignment = block
                                        .alignments()
                                        .get(column)
                                        .copied()
                                        .unwrap_or(TableAlignment::None);
                                    let mut job = inline_layout_job(
                                        cell.inlines(),
                                        document,
                                        &self.find,
                                        FontId::proportional(14.0),
                                        row.is_header(),
                                    );
                                    if row.is_header() {
                                        for section in &mut job.sections {
                                            section.format.font_id = FontId::proportional(14.0);
                                        }
                                    }
                                    match alignment {
                                        TableAlignment::Right => {
                                            ui.with_layout(
                                                egui::Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    ui.add(
                                                        egui::Label::new(job.clone())
                                                            .selectable(true)
                                                            .wrap(),
                                                    );
                                                },
                                            );
                                        }
                                        _ => {
                                            ui.add(egui::Label::new(job).selectable(true).wrap());
                                        }
                                    }
                                }
                                ui.end_row();
                            }
                        });
                });
            });
    }

    fn render_code_block(&mut self, ui: &mut egui::Ui, block: &CodeBlock) {
        egui::Frame::new()
            .fill(theme::SURFACE_TAB_INACTIVE)
            .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(block.language().unwrap_or("text")).small().monospace().color(theme::TEXT_SECONDARY));
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(block.code_text().to_owned());
                    }
                });
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    for line in block.highlighted_lines() {
                        let response = ui.add(egui::Label::new(highlighted_line_job(line)).selectable(true).wrap());
                        if matches!(self.pending_scroll, Some(PendingScroll::Byte(target)) if byte_range_contains(block.span(), target)) {
                            response.scroll_to_me(Some(Align::Center));
                            self.pending_scroll = None;
                        }
                    }
                });
            });
    }

    fn render_html_block(&mut self, ui: &mut egui::Ui, block: &RawHtmlBlock) {
        egui::Frame::new()
            .fill(theme::SURFACE_TAB_INACTIVE)
            .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.label(RichText::new("HTML not rendered").strong());
                ui.add(
                    egui::Label::new(RichText::new(block.literal()).monospace().small())
                        .selectable(true)
                        .wrap(),
                );
            });
    }

    fn render_source(&mut self, ui: &mut egui::Ui, document: &MarkdownDocument) {
        egui::Frame::new()
            .fill(theme::SURFACE_TAB_INACTIVE)
            .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                let mut line_start = 0usize;
                for line in document.source_text().split_inclusive('\n') {
                    let span = document
                        .source_span(line_start..line_start + line.len())
                        .unwrap_or_else(|| document.source_span(0..0).expect("empty span"));
                    let response = ui.add(egui::Label::new(source_line_job(line, span, &self.find)).selectable(true).wrap());
                    if matches!(self.pending_scroll, Some(PendingScroll::Byte(target)) if byte_range_contains(span, target)) {
                        response.scroll_to_me(Some(Align::Center));
                        self.pending_scroll = None;
                    }
                    if let Some(index) = heading_index_for_line(document, span.start().byte_offset()) {
                        if matches!(self.pending_scroll, Some(PendingScroll::Heading(target)) if target == index) {
                            response.scroll_to_me(Some(Align::Center));
                            self.pending_scroll = None;
                        }
                    }
                    line_start += line.len();
                }
            });
    }

    fn current_anchor(&self) -> ScrollAnchor {
        if let (Some(document), Some(current)) = (&self.document, self.find.current_match()) {
            return scroll_anchor_for_offset(document, current.span().start().byte_offset());
        }
        if let (Some(document), Some(index)) = (&self.document, self.outline_selected) {
            if let Some(heading) = document.headings().get(index) {
                return scroll_anchor_for_offset(document, heading.section_start_byte());
            }
        }
        self.document
            .as_ref()
            .map(|document| scroll_anchor_for_offset(document, 0))
            .unwrap_or_else(ScrollAnchor::top)
    }
}

fn pending_scroll_for_anchor(document: &MarkdownDocument, anchor: ScrollAnchor) -> PendingScroll {
    PendingScroll::Byte(byte_offset_for_anchor(document, anchor))
}

pub fn scroll_anchor_for_offset(document: &MarkdownDocument, byte_offset: usize) -> ScrollAnchor {
    let heading_index = document
        .nearest_heading_at_byte(byte_offset)
        .and_then(|heading| {
            document
                .headings()
                .iter()
                .position(|candidate| candidate == heading)
        });
    let (section_start, section_end) = heading_index
        .and_then(|index| document.headings().get(index))
        .map(|heading| (heading.section_start_byte(), heading.section_end_byte()))
        .unwrap_or((0, document.source_text().len()));
    let denominator = section_end.saturating_sub(section_start).max(1);
    let numerator = byte_offset.saturating_sub(section_start).min(denominator);
    ScrollAnchor {
        heading_index,
        section_offset_numerator: numerator,
        section_offset_denominator: denominator,
    }
}

pub fn byte_offset_for_anchor(document: &MarkdownDocument, anchor: ScrollAnchor) -> usize {
    let (section_start, section_end) = anchor
        .heading_index
        .and_then(|index| document.headings().get(index))
        .map(|heading| (heading.section_start_byte(), heading.section_end_byte()))
        .unwrap_or((0, document.source_text().len()));
    let span = section_end.saturating_sub(section_start);
    if span == 0 {
        return section_start;
    }
    section_start
        + span.saturating_mul(anchor.section_offset_numerator)
            / anchor.section_offset_denominator.max(1)
}

fn load_local_document(
    path: PathBuf,
) -> Result<(String, MarkdownDocument), MarkdownViewerLoadFailure> {
    let canonical = fs::canonicalize(&path).map_err(map_local_io_error)?;
    let metadata = fs::metadata(&canonical).map_err(map_local_io_error)?;
    if !metadata.is_file() {
        return Err(MarkdownViewerLoadFailure::Local(LocalLoadError::NotAFile));
    }
    let bytes = fs::read(&canonical).map_err(map_local_io_error)?;
    let source =
        LocalMarkdownSource::new(canonical.clone()).map_err(MarkdownViewerLoadFailure::Source)?;
    let document = MarkdownLoader::default()
        .load(
            source.into(),
            metadata.len() as usize,
            &bytes,
            &Default::default(),
        )
        .map_err(MarkdownViewerLoadFailure::Load)?;
    Ok((canonical.display().to_string(), document))
}

fn local_document_source(source: &MarkdownSource) -> Option<&LocalMarkdownSource> {
    match source {
        MarkdownSource::Local(local) => Some(local),
        MarkdownSource::Remote(_) => None,
    }
}

fn map_local_io_error(error: std::io::Error) -> MarkdownViewerLoadFailure {
    use std::io::ErrorKind;
    MarkdownViewerLoadFailure::Local(match error.kind() {
        ErrorKind::NotFound => LocalLoadError::NotFound,
        ErrorKind::PermissionDenied => LocalLoadError::PermissionDenied,
        _ => LocalLoadError::Io,
    })
}

fn read_local_image(markdown_path: &Path, target: &str) -> Result<egui::ColorImage, String> {
    let Some(parent) = markdown_path.parent() else {
        return Err("The Markdown file has no parent directory for relative resources.".to_owned());
    };
    let candidate = parent.join(target);
    let canonical = fs::canonicalize(&candidate)
        .map_err(|_| "The requested local image could not be found.".to_owned())?;
    let metadata = fs::metadata(&canonical)
        .map_err(|_| "The requested local image could not be read.".to_owned())?;
    if !metadata.is_file() {
        return Err("The requested local image is not a regular file.".to_owned());
    }
    let extension = canonical
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .unwrap_or_default();
    if extension == "svg" {
        return Err("SVG images remain blocked in the Markdown viewer.".to_owned());
    }
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "Local images must not exceed {} bytes.",
            MAX_IMAGE_BYTES
        ));
    }
    let bytes = fs::read(&canonical)
        .map_err(|_| "The requested local image could not be read.".to_owned())?;
    let decoded = image::load_from_memory(&bytes)
        .map_err(|_| "Only bounded local raster images can be loaded here.".to_owned())?
        .to_rgba8();
    let (width, height) = decoded.dimensions();
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err("The requested local image exceeds the raster-area limit.".to_owned());
    }
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        decoded.as_raw(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn render_text_block(
    ui: &mut egui::Ui,
    block: &TextBlock,
    document: &MarkdownDocument,
    find: &MarkdownFindState,
    approvals: &ResourceApprovalState,
    loaded_images: &BTreeMap<usize, LoadedImage>,
    image_errors: &BTreeMap<usize, String>,
    pending_scroll: &mut Option<PendingScroll>,
    outline_selected: &mut Option<usize>,
) {
    let response = ui
        .horizontal_wrapped(|ui| {
            render_inline_flow(
                ui,
                block.inlines(),
                document,
                find,
                approvals,
                loaded_images,
                image_errors,
                FontId::proportional(15.0),
                false,
                pending_scroll,
                outline_selected,
            );
        })
        .response;
    if let Some(current) = find.current_match() {
        if overlaps(block.span(), current.span()) {
            response.scroll_to_me(Some(Align::Center));
            *pending_scroll = None;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_inline_flow(
    ui: &mut egui::Ui,
    inlines: &[Inline],
    document: &MarkdownDocument,
    find: &MarkdownFindState,
    approvals: &ResourceApprovalState,
    loaded_images: &BTreeMap<usize, LoadedImage>,
    image_errors: &BTreeMap<usize, String>,
    font: FontId,
    strong: bool,
    pending_scroll: &mut Option<PendingScroll>,
    outline_selected: &mut Option<usize>,
) {
    for inline in inlines {
        match inline {
            Inline::Text(text) => {
                ui.add(
                    egui::Label::new(text_job(
                        text.text(),
                        text.span(),
                        find,
                        font.clone(),
                        strong,
                        false,
                        false,
                    ))
                    .selectable(true)
                    .wrap(),
                );
            }
            Inline::Code(text) => {
                ui.add(
                    egui::Label::new(text_job(
                        text.text(),
                        text.span(),
                        find,
                        FontId::monospace(font.size),
                        strong,
                        false,
                        false,
                    ))
                    .selectable(true)
                    .wrap(),
                );
            }
            Inline::Emphasis(container) => render_inline_flow(
                ui,
                container.inlines(),
                document,
                find,
                approvals,
                loaded_images,
                image_errors,
                FontId::proportional(font.size),
                strong,
                pending_scroll,
                outline_selected,
            ),
            Inline::Strong(container) => render_inline_flow(
                ui,
                container.inlines(),
                document,
                find,
                approvals,
                loaded_images,
                image_errors,
                font.clone(),
                true,
                pending_scroll,
                outline_selected,
            ),
            Inline::Strikethrough(container) => {
                render_struck_inline_flow(
                    ui,
                    container,
                    document,
                    find,
                    approvals,
                    loaded_images,
                    image_errors,
                    font.clone(),
                    strong,
                    pending_scroll,
                    outline_selected,
                );
            }
            Inline::Link(link) => {
                render_link(
                    ui,
                    link,
                    document,
                    find,
                    font.clone(),
                    strong,
                    pending_scroll,
                    outline_selected,
                );
            }
            Inline::Image(image) => {
                render_image(
                    ui,
                    image,
                    document,
                    approvals,
                    loaded_images,
                    image_errors,
                    pending_scroll,
                );
            }
            Inline::RawHtml(html) => {
                ui.add(
                    egui::Label::new(text_job(
                        html.literal(),
                        html.span(),
                        find,
                        FontId::monospace(font.size),
                        strong,
                        false,
                        true,
                    ))
                    .selectable(true)
                    .wrap(),
                );
            }
            Inline::SoftBreak { .. } | Inline::HardBreak { .. } => {
                ui.end_row();
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_struck_inline_flow(
    ui: &mut egui::Ui,
    container: &ContainerInline,
    document: &MarkdownDocument,
    find: &MarkdownFindState,
    approvals: &ResourceApprovalState,
    loaded_images: &BTreeMap<usize, LoadedImage>,
    image_errors: &BTreeMap<usize, String>,
    font: FontId,
    strong: bool,
    pending_scroll: &mut Option<PendingScroll>,
    outline_selected: &mut Option<usize>,
) {
    for inline in container.inlines() {
        match inline {
            Inline::Text(text) | Inline::Code(text) => {
                let mono = matches!(inline, Inline::Code(_));
                ui.add(
                    egui::Label::new(text_job(
                        text.text(),
                        text.span(),
                        find,
                        if mono {
                            FontId::monospace(font.size)
                        } else {
                            font.clone()
                        },
                        strong,
                        true,
                        false,
                    ))
                    .selectable(true)
                    .wrap(),
                );
            }
            _ => render_inline_flow(
                ui,
                std::slice::from_ref(inline),
                document,
                find,
                approvals,
                loaded_images,
                image_errors,
                font.clone(),
                strong,
                pending_scroll,
                outline_selected,
            ),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_link(
    ui: &mut egui::Ui,
    link: &LinkInline,
    document: &MarkdownDocument,
    find: &MarkdownFindState,
    font: FontId,
    strong: bool,
    pending_scroll: &mut Option<PendingScroll>,
    outline_selected: &mut Option<usize>,
) {
    let reference = &document.resource_references()[link.reference_index()];
    let text = if link.plain_text().is_empty() {
        reference.target().to_owned()
    } else {
        link.plain_text().to_owned()
    };
    let response = ui.add(
        egui::Button::new(text_job(
            &text,
            link.span(),
            find,
            font,
            strong,
            false,
            false,
        ))
        .frame(false),
    );
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("Link: {text}")));
    let response = response.on_hover_text(reference.target());
    if matches!(pending_scroll, Some(PendingScroll::Byte(target)) if byte_range_contains(link.span(), *target))
    {
        response.scroll_to_me(Some(Align::Center));
        *pending_scroll = None;
    }
    if response.clicked() {
        match reference.class() {
            ResourceReferenceClass::DocumentFragment => {
                let anchor = reference.target().trim_start_matches('#');
                *outline_selected = document
                    .headings()
                    .iter()
                    .position(|heading| heading.anchor() == anchor);
                if let Some(index) = *outline_selected {
                    *pending_scroll = Some(PendingScroll::Heading(index));
                }
            }
            ResourceReferenceClass::HttpsAbsolute => {
                ui.ctx().memory_mut(|memory| {
                    memory.data.insert_temp(
                        egui::Id::new("markdown-external-link"),
                        reference.target().to_owned(),
                    );
                });
            }
            ResourceReferenceClass::LocalRelative => {
                if reference.target().ends_with(".md") || reference.target().ends_with(".markdown")
                {
                    if let Some(local) = local_document_source(document.source()) {
                        if let Some(parent) = local.path().parent() {
                            let next = parent.join(reference.target());
                            ui.ctx().memory_mut(|memory| {
                                memory
                                    .data
                                    .insert_temp(egui::Id::new("markdown-local-link"), next);
                            });
                        }
                    }
                }
            }
            ResourceReferenceClass::RemoteRelativeViaSftpOrigin
            | ResourceReferenceClass::DangerousScheme => {}
        }
    }
}

fn render_image(
    ui: &mut egui::Ui,
    image: &ImageInline,
    document: &MarkdownDocument,
    approvals: &ResourceApprovalState,
    loaded_images: &BTreeMap<usize, LoadedImage>,
    image_errors: &BTreeMap<usize, String>,
    pending_scroll: &mut Option<PendingScroll>,
) {
    let reference = &document.resource_references()[image.reference_index()];
    let response = ui
        .group(|ui| {
            ui.label(
                RichText::new(format!("Image: {}", image.alt_text()))
                    .small()
                    .strong(),
            );
            ui.label(
                RichText::new(resource_class_label(reference.class()))
                    .small()
                    .color(theme::TEXT_SECONDARY),
            );
            if let Some(image) = loaded_images.get(&image.reference_index()) {
                let size = egui::Vec2::new(image.size[0] as f32, image.size[1] as f32);
                ui.add(
                    egui::Image::new(&image.texture)
                        .max_width(320.0)
                        .max_height(240.0)
                        .fit_to_exact_size(size.min(vec2(320.0, 240.0))),
                );
            } else {
                ui.label(RichText::new(resource_placeholder_action(reference.class())).small());
                if reference.class() == ResourceReferenceClass::LocalRelative
                    && !approvals.is_approved(image.reference_index())
                    && ui.small_button("Load local image").clicked()
                {
                    ui.ctx().memory_mut(|memory| {
                        memory.data.insert_temp(
                            egui::Id::new("markdown-load-image"),
                            image.reference_index(),
                        );
                    });
                }
                if let Some(message) = image_errors.get(&image.reference_index()) {
                    ui.label(RichText::new(message).small().color(theme::STATUS_ERROR));
                }
            }
        })
        .response;
    if matches!(pending_scroll, Some(PendingScroll::Byte(target)) if byte_range_contains(image.span(), *target))
    {
        response.scroll_to_me(Some(Align::Center));
        *pending_scroll = None;
    }
}

fn inline_layout_job(
    inlines: &[Inline],
    document: &MarkdownDocument,
    find: &MarkdownFindState,
    font: FontId,
    strong: bool,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    append_inline_layout(
        &mut job, inlines, document, find, font, strong, false, false,
    );
    job
}

#[allow(clippy::too_many_arguments, clippy::only_used_in_recursion)]
fn append_inline_layout(
    job: &mut LayoutJob,
    inlines: &[Inline],
    document: &MarkdownDocument,
    find: &MarkdownFindState,
    font: FontId,
    strong: bool,
    strikethrough: bool,
    code: bool,
) {
    for inline in inlines {
        match inline {
            Inline::Text(text) => append_text_segments(
                job,
                text.text(),
                text.span(),
                find,
                if code {
                    FontId::monospace(font.size)
                } else {
                    font.clone()
                },
                strong,
                strikethrough,
                false,
            ),
            Inline::Code(text) => append_text_segments(
                job,
                text.text(),
                text.span(),
                find,
                FontId::monospace(font.size),
                strong,
                strikethrough,
                false,
            ),
            Inline::Emphasis(container) => append_inline_layout(
                job,
                container.inlines(),
                document,
                find,
                font.clone(),
                strong,
                strikethrough,
                code,
            ),
            Inline::Strong(container) => append_inline_layout(
                job,
                container.inlines(),
                document,
                find,
                font.clone(),
                true,
                strikethrough,
                code,
            ),
            Inline::Strikethrough(container) => append_inline_layout(
                job,
                container.inlines(),
                document,
                find,
                font.clone(),
                strong,
                true,
                code,
            ),
            Inline::Link(link) => append_text_segments(
                job,
                link.plain_text(),
                link.span(),
                find,
                font.clone(),
                strong,
                strikethrough,
                false,
            ),
            Inline::Image(image) => append_text_segments(
                job,
                image.alt_text(),
                image.span(),
                find,
                font.clone(),
                strong,
                strikethrough,
                false,
            ),
            Inline::RawHtml(html) => append_text_segments(
                job,
                html.literal(),
                html.span(),
                find,
                FontId::monospace(font.size),
                strong,
                strikethrough,
                true,
            ),
            Inline::SoftBreak { .. } => job.append(
                "\n",
                0.0,
                base_text_format(font.clone(), strong, strikethrough, false),
            ),
            Inline::HardBreak { .. } => job.append(
                "\n",
                0.0,
                base_text_format(font.clone(), strong, strikethrough, false),
            ),
        }
    }
}

fn text_job(
    text: &str,
    span: SourceSpan,
    find: &MarkdownFindState,
    font: FontId,
    strong: bool,
    strikethrough: bool,
    code_like: bool,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    append_text_segments(
        &mut job,
        text,
        span,
        find,
        font,
        strong,
        strikethrough,
        code_like,
    );
    job
}

fn source_line_job(line: &str, span: SourceSpan, find: &MarkdownFindState) -> LayoutJob {
    let mut job = LayoutJob::default();
    append_text_segments(
        &mut job,
        line,
        span,
        find,
        FontId::monospace(14.0),
        false,
        false,
        false,
    );
    job
}

#[allow(clippy::too_many_arguments)]
fn append_text_segments(
    job: &mut LayoutJob,
    text: &str,
    span: SourceSpan,
    find: &MarkdownFindState,
    font: FontId,
    strong: bool,
    strikethrough: bool,
    code_like: bool,
) {
    if text.is_empty() {
        return;
    }
    let mut cursor = 0usize;
    let matches: Vec<(usize, usize, bool)> = find
        .matches()
        .iter()
        .enumerate()
        .filter_map(|(index, matched)| {
            overlap_with_relative_range(span, matched.span())
                .map(|range| (range.start, range.end, find.current_index == Some(index)))
        })
        .collect();
    for (start, end, current) in matches {
        if cursor < start {
            job.append(
                &text[cursor..start],
                0.0,
                base_text_format(font.clone(), strong, strikethrough, code_like),
            );
        }
        let mut format = base_text_format(font.clone(), strong, strikethrough, code_like);
        format.background = if current {
            theme::ACCENT_PRIMARY.gamma_multiply(0.35)
        } else {
            theme::SURFACE_SELECTION
        };
        job.append(&text[start..end], 0.0, format);
        cursor = end;
    }
    if cursor < text.len() {
        job.append(
            &text[cursor..],
            0.0,
            base_text_format(font, strong, strikethrough, code_like),
        );
    }
}

fn highlighted_line_job(line: &HighlightedCodeLine) -> LayoutJob {
    let mut job = LayoutJob::default();
    if line.spans().is_empty() {
        job.append(
            line.text(),
            0.0,
            base_text_format(FontId::monospace(14.0), false, false, true),
        );
        return job;
    }
    for span in line.spans() {
        job.append(span.text(), 0.0, text_format_from_highlight(span.style()));
    }
    job
}

fn text_format_from_highlight(style: HighlightStyle) -> TextFormat {
    let mut format = TextFormat {
        font_id: FontId::monospace(14.0),
        color: Color32::from_rgba_unmultiplied(
            style.foreground().red(),
            style.foreground().green(),
            style.foreground().blue(),
            style.foreground().alpha(),
        ),
        background: Color32::from_rgba_unmultiplied(
            style.background().red(),
            style.background().green(),
            style.background().blue(),
            style.background().alpha(),
        ),
        ..Default::default()
    };
    if style.bold() {
        format.font_id = FontId::monospace(14.5);
    }
    if style.underline() {
        format.underline = egui::Stroke::new(1.0, format.color);
    }
    if style.italic() {
        format.italics = true;
    }
    format
}

fn base_text_format(
    font: FontId,
    strong: bool,
    strikethrough: bool,
    code_like: bool,
) -> TextFormat {
    let mut format = TextFormat {
        font_id: font,
        color: theme::TEXT_PRIMARY,
        background: if code_like {
            theme::SURFACE_TAB_ACTIVE
        } else {
            Color32::TRANSPARENT
        },
        ..Default::default()
    };
    if strong {
        format.font_id.size += 0.5;
    }
    if strikethrough {
        format.strikethrough = egui::Stroke::new(1.0, format.color);
    }
    format
}

fn small_toolbar_button(
    ui: &mut egui::Ui,
    icon_name: Icon,
    label: &str,
    accessible_label: &str,
) -> bool {
    let response = ui.button(label);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, accessible_label));
    let rect = response.rect.shrink2(vec2(
        response.rect.width() - 16.0,
        response.rect.height() - 16.0,
    ));
    icon::paint(ui.painter(), icon_name, rect, theme::TEXT_SECONDARY);
    response.on_hover_text(accessible_label).clicked()
}

fn icon_label(ui: &mut egui::Ui, icon_name: Icon, text: &str) {
    let (rect, response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
    icon::paint(ui.painter(), icon_name, rect, theme::TEXT_SECONDARY);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    ui.label(RichText::new(text).small().color(theme::TEXT_SECONDARY));
}

fn elide_middle(value: &str, max_chars: usize) -> String {
    let total = value.chars().count();
    if total <= max_chars {
        return value.to_owned();
    }
    let prefix = max_chars / 2;
    let suffix = max_chars.saturating_sub(prefix + 1);
    format!(
        "{}…{}",
        value.chars().take(prefix).collect::<String>(),
        value
            .chars()
            .skip(total.saturating_sub(suffix))
            .collect::<String>()
    )
}

fn overlaps(left: SourceSpan, right: SourceSpan) -> bool {
    left.byte_range().start < right.byte_range().end
        && right.byte_range().start < left.byte_range().end
}

fn byte_range_contains(span: SourceSpan, byte: usize) -> bool {
    span.byte_range().start <= byte && byte < span.byte_range().end
}

fn overlap_with_relative_range(
    container: SourceSpan,
    matched: SourceSpan,
) -> Option<std::ops::Range<usize>> {
    let start = container.byte_range().start.max(matched.byte_range().start);
    let end = container.byte_range().end.min(matched.byte_range().end);
    (start < end)
        .then(|| (start - container.byte_range().start)..(end - container.byte_range().start))
}

fn heading_index_for_line(document: &MarkdownDocument, byte_offset: usize) -> Option<usize> {
    document
        .nearest_heading_at_byte(byte_offset)
        .and_then(|heading| {
            document
                .headings()
                .iter()
                .position(|candidate| candidate == heading)
        })
}

fn resource_class_label(class: ResourceReferenceClass) -> &'static str {
    match class {
        ResourceReferenceClass::DocumentFragment => "Document fragment",
        ResourceReferenceClass::LocalRelative => "Local resource",
        ResourceReferenceClass::RemoteRelativeViaSftpOrigin => "Remote resource",
        ResourceReferenceClass::HttpsAbsolute => "External link",
        ResourceReferenceClass::DangerousScheme => "Blocked resource",
    }
}

fn resource_placeholder_action(class: ResourceReferenceClass) -> &'static str {
    match class {
        ResourceReferenceClass::DocumentFragment => "Document fragments do not load as images.",
        ResourceReferenceClass::LocalRelative => {
            "Use Load local image to view this local raster image."
        }
        ResourceReferenceClass::RemoteRelativeViaSftpOrigin => {
            "Remote image loading is deferred until the GUI SFTP browser lands."
        }
        ResourceReferenceClass::HttpsAbsolute => {
            "Network images remain blocked in the Markdown viewer."
        }
        ResourceReferenceClass::DangerousScheme => {
            "This resource scheme is blocked in the Markdown viewer."
        }
    }
}

pub fn take_viewer_commands(context: &egui::Context) -> Vec<AppCommand> {
    let mut commands = Vec::new();
    if let Some(target) = context.memory(|memory| {
        memory
            .data
            .get_temp::<String>(egui::Id::new("markdown-external-link"))
    }) {
        commands.push(AppCommand::OpenExternalLink {
            target: ExternalLinkTarget::new(target),
        });
        context.memory_mut(|memory| {
            memory
                .data
                .remove::<String>(egui::Id::new("markdown-external-link"));
        });
    }
    if let Some(path) = context.memory(|memory| {
        memory
            .data
            .get_temp::<PathBuf>(egui::Id::new("markdown-local-link"))
    }) {
        commands.push(AppCommand::OpenLocalMarkdownFile { path });
        context.memory_mut(|memory| {
            memory
                .data
                .remove::<PathBuf>(egui::Id::new("markdown-local-link"));
        });
    }
    if let Some(reference_index) = context.memory(|memory| {
        memory
            .data
            .get_temp::<usize>(egui::Id::new("markdown-load-image"))
    }) {
        commands.push(AppCommand::LoadMarkdownLocalImage { reference_index });
        context.memory_mut(|memory| {
            memory
                .data
                .remove::<usize>(egui::Id::new("markdown-load-image"));
        });
    }
    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(text: &str) -> MarkdownDocument {
        MarkdownLoader::default()
            .load(
                LocalMarkdownSource::new("/docs/readme.md").unwrap().into(),
                text.len(),
                text.as_bytes(),
                &Default::default(),
            )
            .unwrap()
    }

    #[test]
    fn find_navigation_wraps_in_both_directions() {
        let document = document("alpha beta alpha beta");
        let mut state = MarkdownFindState::default();
        state.set_query(&document, "beta".to_owned());
        assert_eq!(state.current_label(), "1 of 2");
        state.next();
        assert_eq!(state.current_label(), "2 of 2");
        state.next();
        assert_eq!(state.current_label(), "1 of 2");
        state.previous();
        assert_eq!(state.current_label(), "2 of 2");
    }

    #[test]
    fn same_source_reload_preserves_current_match_when_possible() {
        let document = document("alpha beta alpha beta");
        let mut state = MarkdownFindState::default();
        state.set_query(&document, "beta".to_owned());
        state.next();
        let before = state.current_match().unwrap().span();
        state.restore_for_reload(&document);
        assert_eq!(state.current_match().unwrap().span(), before);
    }

    #[test]
    fn mode_switch_anchor_round_trips_within_heading_section() {
        let document = document("# One\n\nalpha\n\n# Two\n\nbeta\n");
        let offset = document.source_text().find("beta").unwrap();
        let anchor = scroll_anchor_for_offset(&document, offset);
        assert_eq!(anchor.heading_index, Some(1));
        assert_eq!(byte_offset_for_anchor(&document, anchor), offset);
    }

    #[test]
    fn resource_approval_is_per_reference_and_clears() {
        let mut approvals = ResourceApprovalState::default();
        assert!(!approvals.is_approved(2));
        approvals.approve(2);
        assert!(approvals.is_approved(2));
        assert!(!approvals.is_approved(3));
        approvals.clear();
        assert!(!approvals.is_approved(2));
    }

    #[test]
    fn markdown_source_errors_build_content_free_error_states() {
        for error in [
            MarkdownSourceError::EmptyPath,
            MarkdownSourceError::EmptyRemoteHost,
            MarkdownSourceError::WhitespaceRemoteHost,
            MarkdownSourceError::ZeroRemotePort,
            MarkdownSourceError::EmptyRemoteUsername,
            MarkdownSourceError::EmptyRemoteProfileIdentifier,
            MarkdownSourceError::EmptyVerifiedFingerprint,
        ] {
            let state = MarkdownViewerErrorState::from_source_error(error, true);
            assert!(!state.title.is_empty());
            assert!(!state.detail.is_empty());
            assert!(state.stale_snapshot);
        }
    }

    #[test]
    fn markdown_load_errors_build_content_free_error_states() {
        let errors = [
            MarkdownLoadError::Cancelled,
            MarkdownLoadError::InvalidUtf8,
            MarkdownLoadError::BinaryContent,
            MarkdownLoadError::OversizeInput {
                limit_bytes: 1,
                actual_bytes: 2,
            },
            MarkdownLoadError::TooManyLines {
                limit: 1,
                actual: 2,
            },
            MarkdownLoadError::ExcessiveNesting {
                limit: 1,
                actual: 2,
            },
            MarkdownLoadError::TooManyTableCells {
                limit: 1,
                actual: 2,
            },
            MarkdownLoadError::CodeBlockTooLarge {
                limit_bytes: 1,
                actual_bytes: 2,
            },
            MarkdownLoadError::TooManyResourceReferences {
                limit: 1,
                actual: 2,
            },
            MarkdownLoadError::ParseModelInvariant,
        ];
        for error in errors {
            let state = MarkdownViewerErrorState::from_load_error(error, false);
            assert!(!state.title.is_empty());
            assert!(!state.detail.is_empty());
            assert!(!state.source_unavailable);
        }
    }

    #[test]
    fn local_load_errors_build_actionable_error_states() {
        for error in [
            LocalLoadError::NotFound,
            LocalLoadError::PermissionDenied,
            LocalLoadError::NotAFile,
            LocalLoadError::Io,
        ] {
            let state = MarkdownViewerErrorState::from_local_error(error, true);
            assert!(!state.title.is_empty());
            assert!(!state.detail.is_empty());
            assert!(state.stale_snapshot);
        }
    }
}
