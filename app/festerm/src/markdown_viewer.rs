use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use eframe::egui::{
    self, text::LayoutJob, text::TextFormat, vec2, Align, Color32, FontId, RichText, Sense,
    WidgetInfo, WidgetType,
};
use festerm_markdown::{
    Block, CodeBlock, ContainerInline, HeadingBlock, HighlightStyle, HighlightedCodeLine,
    ImageInline, Inline, LinkInline, ListBlock, ListKind, LocalMarkdownSource, MarkdownDocument,
    MarkdownLoadError, MarkdownLoader, MarkdownSource, MarkdownSourceError, RawHtmlBlock,
    RemoteMarkdownSource, ResourceReferenceClass, ResourceReferenceKind, SourceSpan,
    TableAlignment, TableBlock, TaskState, TextBlock, TextMatch,
};
use festerm_ui_egui::{icon, icon::Icon, theme};

use crate::tabs::{AppCommand, ExternalLinkTarget, TabId};

const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
/// How many images a local document may load without the reader asking
/// (`docs/adr/0030-native-markdown-viewer.md`, "Automatic loading of local
/// relative images"). A document with more image references than this shows
/// its remaining images as explicit "Load local image" placeholders, so a
/// pathological document cannot turn one open into unbounded filesystem
/// work.
const MAX_AUTOMATIC_IMAGE_LOADS: usize = 64;
/// How many automatic image loads may be in flight at once. Each load owns a
/// thread, so this caps the burst a large document creates; the rest start as
/// earlier ones finish.
const MAX_CONCURRENT_AUTOMATIC_IMAGE_LOADS: usize = 4;
const OUTLINE_WIDTH: f32 = 216.0;
/// Narrower than this and the outline is hidden for the frame: the reading
/// column matters more than the navigation aid on a cramped window.
const OUTLINE_MIN_DOCUMENT_WIDTH: f32 = 360.0;
const READING_WIDTH: f32 = 860.0;
/// Horizontal breathing room kept on each side of the reading column, from
/// the mockup's `width: min(720px, calc(100% - 54px))`: the column never
/// runs edge to edge even in a narrow well.
const READING_COLUMN_SIDE_GUTTER: f32 = 27.0;
const READING_COLUMN_TOP_PADDING: f32 = 26.0;
const READING_COLUMN_BOTTOM_PADDING: f32 = 56.0;
const BODY_TEXT_SIZE: f32 = 15.0;
/// `p / li { line-height: 1.55 }` in the mockup.
const BODY_LINE_HEIGHT: f32 = BODY_TEXT_SIZE * 1.5;
/// Headings set their own, tighter leading; 1.55 around a 30px H1 left a
/// visible hole between the heading and its rule.
const HEADING_LINE_HEIGHT_RATIO: f32 = 1.25;
const PARAGRAPH_BLOCK_SPACING: f32 = 16.0;
const HEADING_PARAGRAPH_SPACING: f32 = 8.0;
const LIST_ITEM_SPACING: f32 = 6.0;
const CODE_BLOCK_PADDING_X: i8 = 14;
const CODE_BLOCK_PADDING_Y: i8 = 12;
/// `CODE_LINE_HEIGHT` adds its extra leading *below* each row's glyphs, so
/// the last line of a fenced block already carries part of the bottom
/// padding with it. Measured against the top inset, the surplus is 4pt;
/// taking it off the bottom margin makes the block's optical padding even.
const CODE_BLOCK_TRAILING_LEAD: i8 = 4;
const CODE_BLOCK_HEAD_HEIGHT: f32 = 30.0;
/// Fenced-code text size. Slightly smaller than the 15px body, matching the
/// mockup's `pre { font: 11px/1.55 }` against its 13px body.
const CODE_TEXT_SIZE: f32 = 13.0;
/// Line pitch inside a fenced block. Code lines are separate widgets, so the
/// container's default vertical item spacing would double-space them.
const CODE_LINE_SPACING: f32 = 0.0;
/// `pre { font: 11px/1.55 }` in the mockup, applied here against the 13px
/// code size.
const CODE_LINE_HEIGHT: f32 = CODE_TEXT_SIZE * 1.55;
const TABLE_CELL_PADDING_X: i8 = 10;
const TABLE_CELL_PADDING_Y: i8 = 6;
/// Narrowest a table column is allowed to become when a table has to be
/// squeezed into the reading column. Wide enough for a short word plus its
/// cell padding, so a squeezed table still wraps on word boundaries.
const TABLE_MIN_COLUMN_WIDTH: f32 = 72.0;
const MARKDOWN_PANEL_RADIUS: f32 = 6.0;
/// Toolbar control metrics, from the mockup's `.fmd-tool` rule
/// (`height: 30px; min-width: 30px; padding: 0 8px; border-radius: 5px`).
const TOOLBAR_BUTTON_HEIGHT: f32 = 30.0;
const TOOLBAR_BUTTON_PADDING_X: f32 = 8.0;
const TOOLBAR_BUTTON_GAP: f32 = 5.0;
const TOOLBAR_BUTTON_RADIUS: f32 = 5.0;
const TOOLBAR_ICON_SIZE: f32 = 15.0;
/// Gap between a toolbar control's icon and its text, matching the
/// mockup's `gap: 5px`.
const TOOLBAR_ICON_TEXT_GAP: f32 = 5.0;
const TOOLBAR_TEXT_SIZE: f32 = 11.0;
/// `.fmd-outline { padding: 13px 9px }` plus the title's own `0 7px 10px`.
const OUTLINE_PADDING_X: f32 = 9.0;
const OUTLINE_PADDING_Y: f32 = 13.0;
const OUTLINE_TITLE_INSET_X: f32 = 7.0;
const OUTLINE_TITLE_GAP_BELOW: f32 = 10.0;
const OUTLINE_ITEM_PADDING_X: f32 = 7.0;
const OUTLINE_ITEM_PADDING_Y: f32 = 6.0;
/// `.fmd-outline-item.fmd-depth2 { padding-left: 19px }` measured from the
/// item's own left edge, so each further level adds the same step.
const OUTLINE_ITEM_INDENT: f32 = 12.0;
const OUTLINE_ITEM_ACCENT_WIDTH: f32 = 2.0;
const OUTLINE_ITEM_RADIUS: f32 = 3.0;
/// Height reserved for the viewer's own footer, used only when the shared
/// application status bar is hidden. Matches the status bar's own geometry
/// so toggling it doesn't reflow the document.
const VIEWER_FOOTER_HEIGHT: f32 = 25.0;
/// Find is a floating card over the document, matching the mockup's
/// `.fmd-find` (`position: absolute; top: 16px; right: 16px`).
const FIND_CARD_WIDTH: f32 = 340.0;
const FIND_CARD_HEIGHT: f32 = 36.0;
const FIND_CARD_MARGIN: f32 = 16.0;

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
    /// Reload isn't implemented for a Markdown file opened from a remote
    /// SFTP snapshot (see issue #133): unlike a local file, refreshing it
    /// would require locating (or re-establishing) a live SFTP transport to
    /// the same verified origin the snapshot was pinned to, which the
    /// viewer does not yet do. The initial open still succeeds; only the
    /// explicit reload/refresh action hits this.
    RemoteReloadUnsupported,
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
            MarkdownViewerLoadFailure::RemoteReloadUnsupported => Self {
                title: "Reloading isn't supported for this file yet",
                detail: "This Markdown file was opened from a remote SFTP snapshot. Close and \
                         double-click it again in the SFTP file manager to refresh its content."
                    .to_owned(),
                stale_snapshot,
                source_unavailable: false,
            },
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
    focus_requested: bool,
}

impl MarkdownFindState {
    pub fn is_open(&self) -> bool {
        self.open
    }

    fn open(&mut self) {
        self.open = true;
        self.focus_requested = true;
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
        self.focus_requested = false;
    }

    fn take_focus_request(&mut self) -> bool {
        std::mem::take(&mut self.focus_requested)
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

struct PendingImageLoad {
    receiver: Receiver<Result<egui::ColorImage, String>>,
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
    pending_image_loads: BTreeMap<usize, PendingImageLoad>,
    image_errors: BTreeMap<usize, String>,
    /// How many image loads this document has started on its own (see
    /// `start_automatic_image_loads`). Counted rather than derived from
    /// `loaded_images`/`image_errors` so the `MAX_AUTOMATIC_IMAGE_LOADS`
    /// budget is spent once per document and cannot be replenished by, say,
    /// an image that fails to decode.
    automatic_image_loads: usize,
    pending_scroll: Option<PendingScroll>,
    line_heading_indices: Vec<Option<usize>>,
    outline_keyboard_focus: bool,
    status_bar_visible: bool,
}

impl MarkdownViewerTab {
    pub fn open_local(path: PathBuf) -> Self {
        let display_path = display_local_path(&path);
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
            pending_image_loads: BTreeMap::new(),
            image_errors: BTreeMap::new(),
            automatic_image_loads: 0,
            pending_scroll: None,
            line_heading_indices: Vec::new(),
            outline_keyboard_focus: false,
            status_bar_visible: true,
        };
        tab.reload();
        tab
    }

    /// Retargets this viewer at a different local file in place, so `Ctrl+O`
    /// from inside a viewer replaces the document being read rather than
    /// opening another tab.
    ///
    /// The user's *view* preferences (preview-vs-source mode, outline and
    /// status-bar visibility) carry over because they are properties of how
    /// this person likes to read, not of the document. Everything else —
    /// notably `resource_approvals` — is rebuilt from scratch: an approval
    /// is granted for one document's resources and must never be inherited
    /// by a different document (`docs/adr/0030-native-markdown-viewer.md`,
    /// "Explicit, non-persisted resource approval only").
    pub fn open_local_replacing(&mut self, path: PathBuf) {
        let mode = self.mode;
        let outline_open = self.outline_open;
        let status_bar_visible = self.status_bar_visible;
        let mut replacement = Self::open_local(path);
        replacement.mode = mode;
        replacement.outline_open = outline_open;
        replacement.status_bar_visible = status_bar_visible;
        *self = replacement;
    }

    /// Opens a Markdown document already fetched from a remote SFTP
    /// session, holding its bytes in memory rather than writing them to a
    /// temp file (issue #133). `content` is the file's full bytes as read
    /// by `SftpSession::read_markdown_snapshot` over the caller's
    /// already-authenticated connection; `display_path` is the canonical
    /// remote path to show in the UI.
    pub fn open_remote(
        source: RemoteMarkdownSource,
        display_path: String,
        content: Vec<u8>,
    ) -> Self {
        let title = Path::new(source.remote_path())
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("Markdown")
            .to_owned();
        let mut tab = Self {
            source: MarkdownSource::from(source.clone()),
            title,
            display_path: display_path.clone(),
            mode: MarkdownViewerMode::Preview,
            outline_open: true,
            outline_selected: None,
            document: None,
            error: None,
            stale_snapshot: false,
            find: MarkdownFindState::default(),
            resource_approvals: ResourceApprovalState::default(),
            loaded_images: BTreeMap::new(),
            pending_image_loads: BTreeMap::new(),
            image_errors: BTreeMap::new(),
            automatic_image_loads: 0,
            pending_scroll: None,
            line_heading_indices: Vec::new(),
            outline_keyboard_focus: false,
            status_bar_visible: true,
        };
        let result = load_remote_document(source, display_path, content);
        tab.apply_load_result(result);
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

    /// Whether this tab already shows the remote file at `host:port` +
    /// `remote_path`, so re-double-clicking the same remote Markdown file
    /// refreshes the existing tab instead of opening a duplicate. Owner and
    /// verified-fingerprint identity are deliberately not compared here:
    /// they can legitimately change across a reconnect while still
    /// referring to the same logical remote file.
    pub fn matches_remote_path(&self, host: &str, port: u16, remote_path: &str) -> bool {
        matches!(&self.source, MarkdownSource::Remote(remote)
            if remote.host() == host && remote.port() == port && remote.remote_path() == remote_path)
    }

    /// Left-hand status-bar context, mirroring the mockup's
    /// `Local Markdown · UTF-8` / `Remote Markdown · production-db` line.
    pub fn status_bar_context(&self) -> &'static str {
        match self.source {
            MarkdownSource::Local(_) => "Local Markdown",
            MarkdownSource::Remote(_) => "Remote Markdown",
        }
    }

    /// Encoding is fixed for v1 (`docs/markdown-viewer-design.md`: "The first
    /// pass accepts UTF-8 with an optional BOM"), so this is a constant
    /// rather than a decoded-per-document value.
    pub fn status_bar_encoding(&self) -> &'static str {
        "UTF-8"
    }

    /// Right-hand status-bar state. A stale snapshot or a load error outranks
    /// the steady-state "Read only", because those are the states a reader
    /// has to act on.
    pub fn status_bar_label(&self) -> &'static str {
        if let Some(error) = &self.error {
            if error.source_unavailable {
                return "Source unavailable";
            }
            return error.title;
        }
        if self.stale_snapshot {
            return "Offline snapshot · stale";
        }
        if self.document.is_some() {
            "Read only"
        } else {
            "Loading"
        }
    }

    pub fn status_bar_status(&self) -> festerm_ui_egui::chrome::ChipStatus {
        use festerm_ui_egui::chrome::ChipStatus;
        // A healthy read-only document has nothing to act on, so it shows no
        // status dot — the mockup's `.fmd-status` is plain text. The dot is
        // reserved for the states a reader has to notice.
        if self.error.is_some() {
            ChipStatus::Failed
        } else if self.stale_snapshot {
            ChipStatus::Starting
        } else {
            ChipStatus::Neutral
        }
    }

    /// Mirrors `SftpFileManagerTab`: the viewer's own footer only appears
    /// when the shared status bar is hidden, so the same facts are never
    /// shown twice.
    pub fn set_status_bar_visible(&mut self, visible: bool) {
        self.status_bar_visible = visible;
    }

    pub fn show(&mut self, ui: &mut egui::Ui, tab_id: TabId) -> Option<AppCommand> {
        self.poll_background_work(ui.ctx());
        self.start_automatic_image_loads(ui.ctx());
        let mut command = self.consume_shortcuts(ui.ctx(), tab_id);
        egui::Frame::new()
            .fill(theme::SURFACE_WINDOW)
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    // `.fmd-toolbar { padding: 5px 9px }` over the full
                    // width, with its own hairline: the outline and document
                    // below run edge to edge, so the toolbar owns the inset
                    // rather than the whole viewer sharing one margin.
                    // `.fmd-toolbar` carries only a bottom hairline in the
                    // mockup; a filled band read as a second title bar.
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(9, 5))
                        .show(ui, |ui| {
                            if command.is_none() {
                                command = self.show_toolbar(ui, tab_id);
                            }
                        });
                    let hairline = ui
                        .allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover())
                        .0;
                    ui.painter().line_segment(
                        [hairline.left_center(), hairline.right_center()],
                        egui::Stroke::new(1.0, theme::BORDER_SUBTLE),
                    );

                    let footer_height = if self.status_bar_visible {
                        0.0
                    } else {
                        VIEWER_FOOTER_HEIGHT
                    };
                    let body_height = (ui.available_height() - footer_height).max(120.0);
                    let mut document_rect = None;
                    ui.allocate_ui(vec2(ui.available_width(), body_height), |ui| {
                        ui.set_height(body_height);
                        if let Some(document) = self.document.as_ref() {
                            if document.source_text().is_empty() {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(24.0);
                                    ui.heading("This Markdown file is empty");
                                });
                            } else {
                                let mut render_state = MarkdownRenderState {
                                    mode: self.mode,
                                    outline_open: self.outline_open,
                                    outline_selected: &mut self.outline_selected,
                                    find: &self.find,
                                    resource_approvals: &self.resource_approvals,
                                    loaded_images: &self.loaded_images,
                                    pending_image_loads: &self.pending_image_loads,
                                    image_errors: &self.image_errors,
                                    pending_scroll: &mut self.pending_scroll,
                                    line_heading_indices: &self.line_heading_indices,
                                    outline_keyboard_focus: &mut self.outline_keyboard_focus,
                                };
                                document_rect =
                                    Some(render_state.show_document(ui, document).document_rect);
                            }
                        } else if let Some(error) = self.error.as_ref().cloned() {
                            self.show_error(ui, &error, tab_id, &mut command);
                        } else {
                            ui.vertical_centered(|ui| {
                                ui.add_space(24.0);
                                ui.heading("Loading Markdown");
                            });
                        }
                    });
                    if let Some(rect) = document_rect {
                        if let Some(find_command) = self.show_find_overlay(ui, rect) {
                            command = Some(find_command);
                        }
                    }
                    if !self.status_bar_visible {
                        self.show_footer(ui);
                    }
                });
            });
        command
    }

    pub fn reload(&mut self) {
        let result = match &self.source {
            MarkdownSource::Local(local) => load_local_document(local.path().clone()),
            MarkdownSource::Remote(_) => Err(MarkdownViewerLoadFailure::RemoteReloadUnsupported),
        };
        self.apply_load_result(result);
    }

    /// Shared by `reload()` and `open_remote()`: applies a load outcome
    /// (freshly parsed document or failure) to `self`, preserving the
    /// current scroll/outline anchor across the swap when possible. On a
    /// brand-new tab (`self.document` is still `None`), `current_anchor()`
    /// resolves to the top of the document, so this doubles as the
    /// "initial load" path without needing a separate first-load branch.
    fn apply_load_result(
        &mut self,
        result: Result<(String, MarkdownDocument), MarkdownViewerLoadFailure>,
    ) {
        let anchor = self.current_anchor();
        match result {
            Ok((display_path, document)) => {
                self.display_path = display_path;
                self.outline_selected = anchor
                    .heading_index
                    .or_else(|| document.headings().first().map(|_| 0));
                self.pending_scroll = Some(pending_scroll_for_anchor(&document, anchor));
                self.line_heading_indices = build_line_heading_index_lookup(&document);
                self.document = Some(document);
                self.error = None;
                self.stale_snapshot = false;
                self.resource_approvals.clear();
                self.loaded_images.clear();
                self.pending_image_loads.clear();
                self.image_errors.clear();
                self.automatic_image_loads = 0;
                self.outline_keyboard_focus = false;
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
            self.outline_selected =
                document.nearest_heading_index_at_byte(found.span().start().byte_offset());
        }
    }

    pub fn load_local_image(&mut self, reference_index: usize, context: &egui::Context) {
        if self.loaded_images.contains_key(&reference_index)
            || self.pending_image_loads.contains_key(&reference_index)
        {
            return;
        }
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
        let markdown_path = local.path().clone();
        let target = reference.target().to_owned();
        let repaint = context.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        if thread::Builder::new()
            .name(format!("festerm-markdown-image-{reference_index}"))
            .spawn(move || {
                let _ = sender.send(read_local_image(&markdown_path, &target));
                repaint.request_repaint();
            })
            .is_ok()
        {
            self.resource_approvals.approve(reference_index);
            self.image_errors.remove(&reference_index);
            self.pending_image_loads
                .insert(reference_index, PendingImageLoad { receiver });
        } else {
            self.resource_approvals.approve(reference_index);
            self.image_errors.insert(
                reference_index,
                "A background image loader could not be started.".to_owned(),
            );
        }
    }

    /// Starts loading the local images a local document references, without
    /// waiting for the reader to click each placeholder.
    ///
    /// `docs/adr/0030-native-markdown-viewer.md` requires explicit
    /// activation for resources, and that still holds for everything that
    /// leaves the machine or escapes the document's own directory: remote
    /// documents, absolute URLs and SFTP-origin references all keep their
    /// placeholders. This narrow exception covers only images a *local*
    /// document references *relatively* — files the reader already granted
    /// access to by opening the document, read with the same byte and
    /// raster-area limits as a manual load, and bounded by
    /// `MAX_AUTOMATIC_IMAGE_LOADS` / `MAX_CONCURRENT_AUTOMATIC_IMAGE_LOADS`
    /// so a hostile document cannot turn one open into unbounded work.
    /// A document whose images are all placeholders is not a readable
    /// document, which is the behaviour this restores.
    fn start_automatic_image_loads(&mut self, context: &egui::Context) {
        if local_document_source(&self.source).is_none() {
            return;
        }
        let Some(document) = &self.document else {
            return;
        };
        let mut candidates = Vec::new();
        for (index, reference) in document.resource_references().iter().enumerate() {
            if self.automatic_image_loads + candidates.len() >= MAX_AUTOMATIC_IMAGE_LOADS {
                break;
            }
            if self.pending_image_loads.len() + candidates.len()
                >= MAX_CONCURRENT_AUTOMATIC_IMAGE_LOADS
            {
                break;
            }
            if reference.kind() != ResourceReferenceKind::Image
                || reference.class() != ResourceReferenceClass::LocalRelative
            {
                continue;
            }
            // `image_errors` is the re-entry guard for a reference that has
            // already been tried and failed: without it a missing image
            // would be re-read from disk on every single frame.
            if self.loaded_images.contains_key(&index)
                || self.pending_image_loads.contains_key(&index)
                || self.image_errors.contains_key(&index)
            {
                continue;
            }
            candidates.push(index);
        }
        for index in candidates {
            self.automatic_image_loads += 1;
            self.load_local_image(index, context);
        }
    }

    fn poll_background_work(&mut self, context: &egui::Context) {
        let mut finished = Vec::new();
        for (&reference_index, pending) in &self.pending_image_loads {
            match pending.receiver.try_recv() {
                Ok(result) => {
                    match result {
                        Ok(image) => {
                            let texture = context.load_texture(
                                format!("markdown-image-{}-{}", self.title, reference_index),
                                image,
                                egui::TextureOptions::LINEAR,
                            );
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
                            self.image_errors.insert(reference_index, message);
                        }
                    }
                    finished.push(reference_index);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.image_errors.insert(
                        reference_index,
                        "The background image loader stopped unexpectedly.".to_owned(),
                    );
                    finished.push(reference_index);
                }
            }
        }
        for reference_index in finished {
            self.pending_image_loads.remove(&reference_index);
        }
    }

    fn move_outline_selection(&mut self, delta: isize) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        if document.headings().is_empty() {
            return;
        }
        let current = self.outline_selected.unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, document.headings().len() as isize - 1) as usize;
        self.outline_selected = Some(next);
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
        if self.outline_keyboard_focus {
            if context
                .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
            {
                self.move_outline_selection(1);
                return None;
            }
            if context
                .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp))
            {
                self.move_outline_selection(-1);
                return None;
            }
            if context.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
            {
                if let Some(index) = self.outline_selected {
                    self.pending_scroll = Some(PendingScroll::Heading(index));
                }
                return None;
            }
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
        // Origin and path claim the left edge; every control is right
        // aligned, matching the mockup's `.fmd-toolbar` flex row rather than
        // trailing the path in reading order.
        ui.horizontal(|ui| {
            ui.set_height(TOOLBAR_BUTTON_HEIGHT);
            ui.spacing_mut().item_spacing.x = TOOLBAR_BUTTON_GAP;
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                // `docs/markdown-viewer-design.md`: the toolbar ends in an
                // overflow menu, not a second close affordance - the tab
                // chip already carries this tab's close control, so a bare
                // "x" here duplicated it and displaced the menu the spec
                // and mockup both put in this slot.
                let overflow =
                    toolbar_button_response(ui, Some(Icon::Overflow), "", "Viewer menu", false);
                egui::Popup::menu(&overflow).show(|ui| {
                    if ui.button("Copy path").clicked() {
                        ui.ctx().copy_text(self.display_path().to_owned());
                        ui.close();
                    }
                    if ui.button("Reload").clicked() {
                        command = Some(AppCommand::ReloadMarkdown);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Close viewer").clicked() {
                        command = Some(AppCommand::CloseTab(tab_id));
                        ui.close();
                    }
                });
                if toolbar_button(
                    ui,
                    Some(Icon::Outline),
                    "",
                    if self.outline_open {
                        "Hide outline (Ctrl/Cmd+Shift+O)"
                    } else {
                        "Show outline (Ctrl/Cmd+Shift+O)"
                    },
                    self.outline_open,
                ) {
                    command = Some(AppCommand::ToggleMarkdownOutline);
                }
                if toolbar_button(
                    ui,
                    Some(Icon::Search),
                    "",
                    "Find (Ctrl/Cmd+F)",
                    self.find.is_open(),
                ) {
                    command = Some(AppCommand::OpenMarkdownFind);
                }
                // One segmented Preview/Source pair whose active half is
                // filled. The previous pair of buttons swapped their own
                // labels, so in Source mode the toolbar read "Rendered |
                // Preview" and neither half showed the current mode.
                let source_mode = matches!(self.mode, MarkdownViewerMode::Source);
                if toolbar_button(
                    ui,
                    Some(Icon::SourceView),
                    "Source",
                    "Show the Markdown source (Ctrl/Cmd+Shift+V)",
                    source_mode,
                ) && !source_mode
                {
                    command = Some(AppCommand::ToggleMarkdownPreviewSource);
                }
                if toolbar_button(
                    ui,
                    Some(Icon::RenderedView),
                    "Preview",
                    "Show the rendered document (Ctrl/Cmd+Shift+V)",
                    !source_mode,
                ) && source_mode
                {
                    command = Some(AppCommand::ToggleMarkdownPreviewSource);
                }
                if toolbar_button(ui, Some(Icon::Refresh), "", "Reload (Ctrl/Cmd+R)", false) {
                    command = Some(AppCommand::ReloadMarkdown);
                }
                ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                    icon_label(
                        ui,
                        // The same origin vocabulary the SFTP panes use, so
                        // "local" and "remote" read identically everywhere.
                        match self.source {
                            MarkdownSource::Local(_) => Icon::LocalTerminal,
                            MarkdownSource::Remote(_) => Icon::SshRemote,
                        },
                        RichText::new(match self.source {
                            MarkdownSource::Local(_) => "LOCAL",
                            MarkdownSource::Remote(_) => "REMOTE",
                        })
                        .size(TOOLBAR_TEXT_SIZE)
                        .color(theme::TEXT_PRIMARY),
                        theme::TEXT_SECONDARY,
                    );
                    // `.fmd-path { overflow: hidden; text-overflow:
                    // ellipsis }`. Eliding to a fixed character count
                    // ignored how much room the controls had actually left,
                    // so on a narrow window the path drew straight through
                    // the Preview/Source/Find buttons.
                    ui.add(
                        egui::Label::new(
                            RichText::new(elide_middle(self.display_path(), 72))
                                .size(TOOLBAR_TEXT_SIZE)
                                .monospace()
                                .color(theme::TEXT_SECONDARY),
                        )
                        .truncate(),
                    );
                });
            });
        });
        command
    }

    /// Find is a compact card floating over the top-right of the document,
    /// as in the mockup, not a full-width bar wedged between the toolbar and
    /// the body: a bar pushed the whole document down every time Find opened
    /// and its default-framed buttons read as a different application.
    fn show_find_overlay(
        &mut self,
        ui: &mut egui::Ui,
        document_rect: egui::Rect,
    ) -> Option<AppCommand> {
        if !self.find.is_open() {
            return None;
        }
        let mut command = None;
        let card_size = vec2(FIND_CARD_WIDTH, FIND_CARD_HEIGHT);
        let origin = egui::pos2(
            (document_rect.right() - FIND_CARD_MARGIN - card_size.x).max(document_rect.left()),
            document_rect.top() + FIND_CARD_MARGIN,
        );
        egui::Area::new(egui::Id::new("markdown-find-overlay"))
            .order(egui::Order::Foreground)
            .fixed_pos(origin)
            .constrain_to(document_rect)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(theme::SURFACE_OVERLAY)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .corner_radius(TOOLBAR_BUTTON_RADIUS + 1.0)
                    .inner_margin(egui::Margin::symmetric(10, 4))
                    .show(ui, |ui| {
                        // `set_width` sizes the *content*, so the card's own
                        // margins and border have to come off or the card
                        // overhangs the right edge it was inset from.
                        ui.set_width(card_size.x - 22.0);
                        ui.horizontal(|ui| {
                            // 4px margins top and bottom plus the 1px border
                            // make up the card's declared height.
                            ui.set_height(card_size.y - 10.0);
                            ui.spacing_mut().item_spacing.x = TOOLBAR_BUTTON_GAP;
                            // The controls are declared first, right to left,
                            // so they claim their intrinsic width and the
                            // query field absorbs whatever is left. Sizing
                            // the field first instead let the controls push
                            // the card past `set_width`, which ate the inset
                            // it was positioned with.
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if toolbar_button(ui, Some(Icon::Close), "", "Close Find", false) {
                                    self.find.clear();
                                }
                                if toolbar_button(
                                    ui,
                                    Some(Icon::NextMatch),
                                    "",
                                    "Next match (Enter)",
                                    false,
                                ) {
                                    command =
                                        Some(AppCommand::NavigateMarkdownFind { reverse: false });
                                }
                                if toolbar_button(
                                    ui,
                                    Some(Icon::PreviousMatch),
                                    "",
                                    "Previous match (Shift+Enter)",
                                    false,
                                ) {
                                    command =
                                        Some(AppCommand::NavigateMarkdownFind { reverse: true });
                                }
                                ui.label(
                                    RichText::new(self.find.current_label())
                                        .size(TOOLBAR_TEXT_SIZE)
                                        .color(theme::TEXT_SECONDARY),
                                );
                                ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                                    ui.spacing_mut().item_spacing.x = TOOLBAR_ICON_TEXT_GAP;
                                    let (icon_rect, _) = ui.allocate_exact_size(
                                        vec2(TOOLBAR_ICON_SIZE, TOOLBAR_ICON_SIZE),
                                        Sense::hover(),
                                    );
                                    icon::paint(
                                        ui.painter(),
                                        Icon::Search,
                                        icon_rect,
                                        theme::TEXT_SECONDARY,
                                    );
                                    let mut query = self.find.query().to_owned();
                                    let response = ui.add(
                                        egui::TextEdit::singleline(&mut query)
                                            .id(egui::Id::new("markdown-find-query"))
                                            .desired_width(ui.available_width())
                                            .frame(egui::Frame::NONE)
                                            .hint_text("Find"),
                                    );
                                    response.widget_info(|| {
                                        WidgetInfo::labeled(
                                            WidgetType::TextEdit,
                                            true,
                                            "Find Markdown",
                                        )
                                    });
                                    if self.find.take_focus_request() {
                                        response.request_focus();
                                    }
                                    if response.changed() {
                                        if let Some(document) = &self.document {
                                            self.find.set_query(document, query);
                                        } else {
                                            self.find.query = query;
                                        }
                                    }
                                });
                            });
                        });
                    });
            });
        command
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

struct MarkdownRenderState<'a> {
    mode: MarkdownViewerMode,
    outline_open: bool,
    outline_selected: &'a mut Option<usize>,
    find: &'a MarkdownFindState,
    resource_approvals: &'a ResourceApprovalState,
    loaded_images: &'a BTreeMap<usize, LoadedImage>,
    pending_image_loads: &'a BTreeMap<usize, PendingImageLoad>,
    image_errors: &'a BTreeMap<usize, String>,
    pending_scroll: &'a mut Option<PendingScroll>,
    line_heading_indices: &'a [Option<usize>],
    outline_keyboard_focus: &'a mut bool,
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
struct MarkdownDocumentLayout {
    outline_rect: Option<egui::Rect>,
    document_rect: egui::Rect,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct HeadingStyle {
    size: f32,
    text_color: Color32,
    underline: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct InlineRenderStyle {
    text_color: Color32,
    link_color: Color32,
    code_background: Color32,
    strong: bool,
    italics: bool,
    strikethrough: bool,
    code_like: bool,
    link_like: bool,
    /// Row height for this run. Body prose is laid out one inline widget at a
    /// time, so the wrapped-row pitch comes from the tallest galley in the
    /// row rather than from the container's item spacing - pinning it here is
    /// the only lever that actually moves body leading.
    line_height: Option<f32>,
}

impl InlineRenderStyle {
    fn body() -> Self {
        Self {
            text_color: theme::TEXT_PRIMARY,
            link_color: theme::ACCENT_PRIMARY,
            code_background: theme::SURFACE_TAB_ACTIVE.gamma_multiply(0.85),
            strong: false,
            italics: false,
            strikethrough: false,
            code_like: false,
            link_like: false,
            line_height: Some(BODY_LINE_HEIGHT),
        }
    }

    fn with_line_height(self, line_height: f32) -> Self {
        Self {
            line_height: Some(line_height),
            ..self
        }
    }

    fn blockquote() -> Self {
        Self {
            text_color: theme::TEXT_SECONDARY,
            italics: true,
            ..Self::body()
        }
    }

    fn with_strong(self) -> Self {
        Self {
            strong: true,
            ..self
        }
    }

    fn with_italics(self) -> Self {
        Self {
            italics: true,
            ..self
        }
    }

    fn with_strikethrough(self) -> Self {
        Self {
            strikethrough: true,
            ..self
        }
    }

    fn as_inline_code(self) -> Self {
        Self {
            code_like: true,
            italics: false,
            ..self
        }
    }

    fn as_link(self) -> Self {
        Self {
            link_like: true,
            ..self
        }
    }
}

fn heading_style(level: u8) -> HeadingStyle {
    // VS Code's Markdown preview uses a 14px body and a 2em/1.5em/1.25em
    // heading ladder. fesTerm keeps its own typeface, but matches that scale
    // against a 15px body size so documents keep the same visual hierarchy.
    match level {
        // Only H2 carries a rule, as in the mockup (`.fmd-document h2 {
        // border-bottom }`). An underlined H1 immediately above an
        // underlined H2 read as two competing section dividers.
        1 => HeadingStyle {
            size: BODY_TEXT_SIZE * 2.0,
            text_color: theme::TEXT_PRIMARY,
            underline: false,
        },
        2 => HeadingStyle {
            size: BODY_TEXT_SIZE * 1.5,
            text_color: theme::TEXT_PRIMARY,
            underline: true,
        },
        3 => HeadingStyle {
            size: BODY_TEXT_SIZE * 1.25,
            text_color: theme::TEXT_PRIMARY,
            underline: false,
        },
        4 => HeadingStyle {
            size: BODY_TEXT_SIZE,
            text_color: theme::TEXT_PRIMARY,
            underline: false,
        },
        5 => HeadingStyle {
            size: BODY_TEXT_SIZE * 0.9,
            text_color: theme::TEXT_PRIMARY,
            underline: false,
        },
        _ => HeadingStyle {
            size: BODY_TEXT_SIZE * 0.85,
            text_color: theme::TEXT_SECONDARY,
            underline: false,
        },
    }
}

fn inter_block_spacing(previous: &Block, next: &Block) -> f32 {
    match next {
        Block::Heading(heading) => {
            if heading.level() <= 2 {
                24.0
            } else {
                18.0
            }
        }
        _ if matches!(previous, Block::Heading(_)) => HEADING_PARAGRAPH_SPACING,
        Block::List(_) => 12.0,
        Block::Table(_) | Block::CodeBlock(_) | Block::BlockQuote(_) | Block::Html(_) => 14.0,
        Block::Rule { .. } => 18.0,
        _ => PARAGRAPH_BLOCK_SPACING,
    }
}

impl MarkdownRenderState<'_> {
    fn render_blocks(
        &mut self,
        ui: &mut egui::Ui,
        blocks: &[Block],
        document: &MarkdownDocument,
        text_style: InlineRenderStyle,
    ) {
        for (index, block) in blocks.iter().enumerate() {
            if let Some(previous) = index
                .checked_sub(1)
                .and_then(|previous| blocks.get(previous))
            {
                ui.add_space(inter_block_spacing(previous, block));
            }
            self.render_block(ui, block, document, text_style);
        }
    }

    /// Paints one outline row: a full-width band with a 2px leading accent
    /// rail, per the mockup's `.fmd-outline-item`. `selectable_label` cannot
    /// express this -- it sizes itself to its text and rounds a pill around
    /// it, so the selection hugged the heading instead of marking the row.
    fn show_outline_item(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        heading: &festerm_markdown::Heading,
    ) {
        let selected = *self.outline_selected == Some(index);
        // `.fmd-outline-item` gives H1 and H2 the same inset and only steps
        // in from `.fmd-depth2` onward: in a document whose H1 is the title,
        // the H2 sections read as its peers in the outline, and indenting
        // every level spent sidebar width the 216px pane does not have.
        let indent = (heading.level().saturating_sub(2) as f32) * OUTLINE_ITEM_INDENT;
        let font = FontId::proportional(TOOLBAR_TEXT_SIZE);
        let text_color = if selected {
            theme::TEXT_PRIMARY
        } else if heading.level() > 2 {
            theme::TEXT_SECONDARY
        } else {
            theme::TEXT_PRIMARY.gamma_multiply(0.86)
        };
        let width = ui.available_width();
        let text_left =
            OUTLINE_ITEM_PADDING_X + OUTLINE_ITEM_ACCENT_WIDTH + OUTLINE_ITEM_PADDING_X + indent;
        let galley = ui.painter().layout(
            heading.text().to_owned(),
            font,
            text_color,
            (width - text_left - OUTLINE_ITEM_PADDING_X).max(24.0),
        );
        let height = galley.size().y + OUTLINE_ITEM_PADDING_Y * 2.0;
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
        response.widget_info(|| {
            WidgetInfo::selected(
                WidgetType::Button,
                true,
                selected,
                format!("Heading level {}: {}", heading.level(), heading.text()),
            )
        });

        if selected || response.hovered() {
            ui.painter().rect_filled(
                rect,
                OUTLINE_ITEM_RADIUS,
                if selected {
                    // The mockup's current outline item is a muted slate
                    // wash (#1a303f), not the full-strength text-selection
                    // blue, which read as a stray text selection in the
                    // sidebar rather than a navigation state.
                    theme::SURFACE_SELECTION.gamma_multiply(0.6)
                } else {
                    theme::SURFACE_TAB_ACTIVE.gamma_multiply(0.5)
                },
            );
        }
        if selected {
            ui.painter().rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(rect.left(), rect.top()),
                    egui::pos2(rect.left() + OUTLINE_ITEM_ACCENT_WIDTH, rect.bottom()),
                ),
                0.0,
                theme::ACCENT_PRIMARY,
            );
        }
        ui.painter().galley(
            egui::pos2(rect.left() + text_left, rect.top() + OUTLINE_ITEM_PADDING_Y),
            galley,
            text_color,
        );

        if response.clicked() {
            *self.outline_selected = Some(index);
            *self.pending_scroll = Some(PendingScroll::Heading(index));
            *self.outline_keyboard_focus = true;
        }
    }

    fn show_outline(
        &mut self,
        ui: &mut egui::Ui,
        document: &MarkdownDocument,
        viewport_height: f32,
    ) -> egui::Rect {
        // A full-height panel divided from the document by a single hairline,
        // not a floating card: the mockup's `.fmd-outline` uses only
        // `border-right`, and a bordered card left an obvious gap above the
        // status bar where the card stopped short.
        let (panel_rect, _) =
            ui.allocate_exact_size(vec2(OUTLINE_WIDTH, viewport_height), Sense::hover());
        // `.fmd-outline { background: #0f151c }` is only a couple of levels
        // above the body; the tab surface was a far bigger jump and made the
        // outline read as a floating card.
        ui.painter()
            .rect_filled(panel_rect, 0.0, theme::SURFACE_TERMINAL);
        ui.painter().line_segment(
            [panel_rect.right_top(), panel_rect.right_bottom()],
            egui::Stroke::new(1.0, theme::BORDER_SUBTLE),
        );

        let content_rect = panel_rect
            .shrink2(vec2(OUTLINE_PADDING_X, OUTLINE_PADDING_Y))
            // Keep the hairline clear of the content.
            .translate(vec2(-0.5, 0.0));
        let mut content = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(content_rect)
                .layout(egui::Layout::top_down(Align::Min)),
        );
        content.spacing_mut().item_spacing.y = 0.0;
        content.horizontal(|ui| {
            ui.add_space(OUTLINE_TITLE_INSET_X);
            icon_label(
                ui,
                Icon::Outline,
                RichText::new("OUTLINE")
                    .size(10.0)
                    .color(theme::TEXT_SECONDARY),
                theme::TEXT_SECONDARY,
            );
        });
        content.add_space(OUTLINE_TITLE_GAP_BELOW);
        egui::ScrollArea::vertical()
            .id_salt("markdown-outline")
            .max_height(content.available_height())
            .show(&mut content, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (index, heading) in document.headings().iter().enumerate() {
                    self.show_outline_item(ui, index, heading);
                }
            });
        panel_rect
    }

    fn show_document(
        &mut self,
        ui: &mut egui::Ui,
        document: &MarkdownDocument,
    ) -> MarkdownDocumentLayout {
        let viewport_height = ui.available_height().max(120.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.set_height(viewport_height);
            let mut outline_rect = None;
            // Below this the outline would starve the reading column (at 400
            // logical points the 216pt outline left a 184pt well, which wraps
            // prose to two or three words a row). Collapse it for the frame
            // without disturbing the user's own toggle.
            if self.outline_open
                && ui.available_width() >= OUTLINE_WIDTH + OUTLINE_MIN_DOCUMENT_WIDTH
            {
                outline_rect = Some(self.show_outline(ui, document, viewport_height));
            }
            let document_width = ui.available_width();
            let mut document_rect = egui::Rect::NOTHING;
            ui.allocate_ui_with_layout(
                vec2(document_width, viewport_height),
                egui::Layout::top_down(Align::Min),
                |ui| {
                    document_rect = ui.max_rect();
                    egui::ScrollArea::vertical()
                        .id_salt("markdown-document")
                        .max_height(viewport_height)
                        .min_scrolled_height(viewport_height)
                        // Without this the scroll area shrinks to the width
                        // of its content, which parks the scrollbar against
                        // the reading column instead of the right edge of
                        // the well.
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            // The reading column is centred in the well, as
                            // `.fmd-document { margin: 0 auto }` does. Laying
                            // it out top-down from the left edge left the
                            // whole right half of a wide window empty.
                            let column_width = READING_WIDTH
                                .min(ui.available_width() - READING_COLUMN_SIDE_GUTTER * 2.0)
                                .max(160.0);
                            let leading = ((ui.available_width() - column_width) / 2.0).max(0.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.add_space(leading);
                                ui.allocate_ui_with_layout(
                                    vec2(column_width, 0.0),
                                    egui::Layout::top_down(Align::Min),
                                    |ui| {
                                        ui.set_max_width(column_width);
                                        ui.add_space(READING_COLUMN_TOP_PADDING);
                                        match self.mode {
                                            MarkdownViewerMode::Preview => self.render_blocks(
                                                ui,
                                                document.blocks(),
                                                document,
                                                InlineRenderStyle::body(),
                                            ),
                                            MarkdownViewerMode::Source => {
                                                self.render_source(ui, document);
                                            }
                                        }
                                        ui.add_space(READING_COLUMN_BOTTOM_PADDING);
                                    },
                                );
                            });
                        });
                },
            );
            MarkdownDocumentLayout {
                outline_rect,
                document_rect,
            }
        })
        .inner
    }

    fn render_block(
        &mut self,
        ui: &mut egui::Ui,
        block: &Block,
        document: &MarkdownDocument,
        text_style: InlineRenderStyle,
    ) {
        match block {
            Block::Paragraph(block) => render_text_block(
                ui,
                block,
                document,
                self.find,
                self.resource_approvals,
                self.loaded_images,
                self.pending_image_loads,
                self.image_errors,
                self.pending_scroll,
                self.outline_selected,
                text_style,
            ),
            Block::Heading(block) => self.render_heading_block(ui, block, document, text_style),
            Block::BlockQuote(block) => {
                // VS Code's preview uses a 5px quote bar with muted, inset
                // text. Keep the same proportions while staying on fesTerm's
                // palette instead of introducing an unrelated Markdown theme.
                let quote = egui::Frame::new()
                    .fill(theme::SURFACE_TAB_INACTIVE.gamma_multiply(0.45))
                    .corner_radius(MARKDOWN_PANEL_RADIUS)
                    .inner_margin(egui::Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        // Wrapped prose only reports the width of its widest
                        // row, so without this the panel shrank to the
                        // paragraph's ragged right edge and stopped short of
                        // the reading column that every other block spans.
                        ui.set_min_width(ui.available_width());
                        self.render_blocks(
                            ui,
                            block.blocks(),
                            document,
                            InlineRenderStyle::blockquote(),
                        );
                    });
                let bar_rect = egui::Rect::from_min_max(
                    quote.response.rect.left_top(),
                    egui::pos2(
                        quote.response.rect.left() + 5.0,
                        quote.response.rect.bottom(),
                    ),
                );
                ui.painter()
                    .rect_filled(bar_rect, 2.0, theme::BORDER_ACTIVE.gamma_multiply(0.7));
            }
            Block::List(block) => self.render_list(ui, block, document, text_style),
            Block::Table(block) => self.render_table(ui, block, document, text_style),
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
        text_style: InlineRenderStyle,
    ) {
        let style = heading_style(block.level());
        let heading_index = block.heading_index();
        let job = inline_layout_job(
            block.inlines(),
            document,
            self.find,
            FontId::proportional(style.size),
            InlineRenderStyle {
                text_color: style.text_color,
                ..text_style
                    .with_strong()
                    .with_line_height(style.size * HEADING_LINE_HEIGHT_RATIO)
            },
        );
        let response = ui.add(egui::Label::new(job).selectable(true).wrap());
        response.widget_info(|| {
            WidgetInfo::labeled(
                WidgetType::Label,
                true,
                format!("Heading level {}: {}", block.level(), block.plain_text()),
            )
        });
        if style.underline {
            ui.add_space(4.0);
            let underline = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter().line_segment(
                [underline.0.left_center(), underline.0.right_center()],
                egui::Stroke::new(1.0, theme::BORDER_SUBTLE),
            );
        }
        if matches!(*self.pending_scroll, Some(PendingScroll::Heading(index)) if index == heading_index)
        {
            response.scroll_to_me(Some(Align::Center));
            *self.pending_scroll = None;
        }
        if response.clicked() {
            *self.outline_selected = Some(heading_index);
            *self.outline_keyboard_focus = false;
        }
        if let Some(current) = self.find.current_match() {
            if overlaps(block.span(), current.span()) {
                *self.outline_selected =
                    document.nearest_heading_index_at_byte(current.span().start().byte_offset());
            }
        }
    }

    fn render_list(
        &mut self,
        ui: &mut egui::Ui,
        block: &ListBlock,
        document: &MarkdownDocument,
        text_style: InlineRenderStyle,
    ) {
        for (index, item) in block.items().iter().enumerate() {
            ui.horizontal_top(|ui| {
                let marker = match item.task_state() {
                    Some(TaskState::Checked) => "☑".to_owned(),
                    Some(TaskState::Unchecked) => "☐".to_owned(),
                    None => match block.kind() {
                        ListKind::Bullet => "•".to_owned(),
                        ListKind::Ordered { first_item_number } => {
                            let number = first_item_number + index as u64;
                            format!("{number}.")
                        }
                    },
                };
                ui.add_sized(
                    [24.0, BODY_TEXT_SIZE + 4.0],
                    egui::Label::new(
                        RichText::new(marker)
                            .size(BODY_TEXT_SIZE)
                            .color(text_style.text_color),
                    ),
                );
                ui.vertical(|ui| {
                    self.render_blocks(ui, item.blocks(), document, text_style);
                });
            });
            if index + 1 < block.items().len() {
                ui.add_space(LIST_ITEM_SPACING);
            }
        }
    }

    fn render_table(
        &mut self,
        ui: &mut egui::Ui,
        block: &TableBlock,
        document: &MarkdownDocument,
        text_style: InlineRenderStyle,
    ) {
        let column_count = block
            .rows()
            .iter()
            .map(|row| row.cells().len())
            .max()
            .unwrap_or_default();
        if column_count == 0 {
            return;
        }

        let font = FontId::proportional(BODY_TEXT_SIZE - 1.0);
        let padding_x = f32::from(TABLE_CELL_PADDING_X);
        let padding_y = f32::from(TABLE_CELL_PADDING_Y);

        // Each cell's text is laid out once, here, and the resulting galley is
        // what gets painted. `egui::Grid` cannot do this job: it sizes a column
        // from what the previous frame's cells reported, so a wrapping `Label`
        // inside it feeds its own wrapped width back in as the column's desired
        // width. Every frame the column got narrower until the text wrapped one
        // character per line, which is exactly what a wide two-column reference
        // table degenerated into.
        let mut cell_jobs: Vec<Vec<LayoutJob>> = Vec::with_capacity(block.rows().len());
        let mut natural = vec![0.0_f32; column_count];
        for row in block.rows() {
            let mut cells = Vec::with_capacity(column_count);
            for (column, cell) in row.cells().iter().enumerate() {
                let job = inline_layout_job(
                    cell.inlines(),
                    document,
                    self.find,
                    font.clone(),
                    if row.is_header() {
                        text_style.with_strong()
                    } else {
                        text_style
                    },
                );
                let mut unwrapped = job.clone();
                unwrapped.wrap.max_width = f32::INFINITY;
                let width = ui.painter().layout_job(unwrapped).size().x;
                if let Some(slot) = natural.get_mut(column) {
                    *slot = slot.max(width + padding_x * 2.0);
                }
                cells.push(job);
            }
            cell_jobs.push(cells);
        }

        // The frame's 1px inner margin sits between the reading column and the
        // cells, so the columns get to share what is left of it.
        let widths = table_column_widths(&natural, (ui.available_width() - 2.0).max(0.0));
        let total_width: f32 = widths.iter().sum();

        egui::Frame::new()
            .fill(theme::SURFACE_TAB_INACTIVE.gamma_multiply(0.35))
            .corner_radius(MARKDOWN_PANEL_RADIUS)
            .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
            .inner_margin(egui::Margin::same(1))
            .show(ui, |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt(("markdown-table", block.span().byte_range().start))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                        let mut cell_rects: Vec<Vec<egui::Rect>> =
                            Vec::with_capacity(cell_jobs.len());
                        for (row, jobs) in block.rows().iter().zip(&cell_jobs) {
                            // A row is only as tall as its tallest wrapped
                            // cell, so every cell has to be laid out before any
                            // of them can be placed.
                            let mut row_height = 0.0_f32;
                            let mut galleys = Vec::with_capacity(jobs.len());
                            for (column, job) in jobs.iter().enumerate() {
                                let width = widths
                                    .get(column)
                                    .copied()
                                    .unwrap_or(TABLE_MIN_COLUMN_WIDTH);
                                let mut wrapped = job.clone();
                                wrapped.wrap.max_width = (width - padding_x * 2.0).max(1.0);
                                let galley = ui.painter().layout_job(wrapped);
                                row_height = row_height.max(galley.size().y + padding_y * 2.0);
                                galleys.push(galley);
                            }

                            let (row_rect, _) = ui.allocate_exact_size(
                                vec2(total_width, row_height),
                                egui::Sense::hover(),
                            );
                            if row.is_header() {
                                ui.painter().rect_filled(
                                    row_rect,
                                    0.0,
                                    theme::SURFACE_TAB_ACTIVE.gamma_multiply(0.55),
                                );
                            }

                            let mut row_rects = Vec::with_capacity(galleys.len());
                            let mut left = row_rect.left();
                            for (column, galley) in galleys.into_iter().enumerate() {
                                let width = widths
                                    .get(column)
                                    .copied()
                                    .unwrap_or(TABLE_MIN_COLUMN_WIDTH);
                                let cell_rect = egui::Rect::from_min_size(
                                    egui::pos2(left, row_rect.top()),
                                    vec2(width, row_height),
                                );
                                left += width;
                                let layout = match block
                                    .alignments()
                                    .get(column)
                                    .copied()
                                    .unwrap_or(TableAlignment::None)
                                {
                                    TableAlignment::Right => {
                                        egui::Layout::right_to_left(Align::Center)
                                    }
                                    TableAlignment::Center => {
                                        egui::Layout::left_to_right(Align::Center)
                                            .with_main_align(Align::Center)
                                    }
                                    _ => egui::Layout::left_to_right(Align::Center),
                                };
                                let mut cell_ui = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(cell_rect.shrink2(vec2(padding_x, padding_y)))
                                        .layout(layout),
                                );
                                cell_ui.add(egui::Label::new(galley).selectable(true));
                                row_rects.push(cell_rect);
                            }
                            cell_rects.push(row_rects);
                        }

                        if let Some(first_row) = cell_rects.first() {
                            let table_rect = cell_rects
                                .iter()
                                .flatten()
                                .fold(first_row[0], |rect, next| rect.union(*next));
                            ui.painter().rect_stroke(
                                table_rect,
                                0.0,
                                egui::Stroke::new(1.0, theme::BORDER_SUBTLE),
                                egui::StrokeKind::Inside,
                            );
                            for row in &cell_rects {
                                for cell in row.iter().take(row.len().saturating_sub(1)) {
                                    ui.painter().line_segment(
                                        [
                                            egui::pos2(cell.right(), table_rect.top()),
                                            egui::pos2(cell.right(), table_rect.bottom()),
                                        ],
                                        egui::Stroke::new(1.0, theme::BORDER_SUBTLE),
                                    );
                                }
                            }
                            for row in cell_rects.iter().take(cell_rects.len().saturating_sub(1)) {
                                let Some(first_cell) = row.first() else {
                                    continue;
                                };
                                ui.painter().line_segment(
                                    [
                                        egui::pos2(table_rect.left(), first_cell.bottom()),
                                        egui::pos2(table_rect.right(), first_cell.bottom()),
                                    ],
                                    egui::Stroke::new(1.0, theme::BORDER_SUBTLE),
                                );
                            }
                        }
                    });
            });
    }

    fn render_code_block(&mut self, ui: &mut egui::Ui, block: &CodeBlock) {
        // The mockup separates the language/Copy head from the code with a
        // hairline (`.fmd-code-head { border-bottom }`) instead of stacking
        // both inside one padded box, which is what made the label look like
        // a stray first line of code.
        egui::Frame::new()
            .fill(theme::SURFACE_TERMINAL)
            .corner_radius(MARKDOWN_PANEL_RADIUS)
            .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let head_rect = ui
                    .allocate_exact_size(
                        vec2(ui.available_width(), CODE_BLOCK_HEAD_HEIGHT),
                        Sense::hover(),
                    )
                    .0;
                let mut head = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(head_rect.shrink2(vec2(CODE_BLOCK_PADDING_X as f32, 0.0)))
                        .layout(egui::Layout::left_to_right(Align::Center)),
                );
                head.label(
                    RichText::new(block.language().unwrap_or("text"))
                        .size(BODY_TEXT_SIZE - 4.0)
                        .monospace()
                        .color(theme::TEXT_SECONDARY),
                );
                head.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if toolbar_button(
                        ui,
                        Some(Icon::Copy),
                        "Copy",
                        "Copy the code block",
                        false,
                    ) {
                        ui.ctx().copy_text(block.code_text().to_owned());
                    }
                });
                ui.painter().line_segment(
                    [head_rect.left_bottom(), head_rect.right_bottom()],
                    egui::Stroke::new(1.0, theme::BORDER_SUBTLE),
                );

                egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: CODE_BLOCK_PADDING_X,
                        right: CODE_BLOCK_PADDING_X,
                        top: CODE_BLOCK_PADDING_Y,
                        bottom: CODE_BLOCK_PADDING_Y - CODE_BLOCK_TRAILING_LEAD,
                    })
                    .show(ui, |ui| {
                        egui::ScrollArea::horizontal()
                            .id_salt(("markdown-code", block.span().byte_range().start))
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = CODE_LINE_SPACING;
                                for line in block.highlighted_lines() {
                                    let response = ui.add(
                                        egui::Label::new(highlighted_line_job(line))
                                            .selectable(true)
                                            .wrap(),
                                    );
                                    if matches!(*self.pending_scroll, Some(PendingScroll::Byte(target)) if byte_range_contains(block.span(), target))
                                    {
                                        response.scroll_to_me(Some(Align::Center));
                                        *self.pending_scroll = None;
                                    }
                                }
                            });
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
            .fill(theme::SURFACE_TERMINAL)
            .corner_radius(MARKDOWN_PANEL_RADIUS)
            .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
            .inner_margin(egui::Margin {
                left: CODE_BLOCK_PADDING_X,
                right: CODE_BLOCK_PADDING_X,
                top: CODE_BLOCK_PADDING_Y,
                bottom: CODE_BLOCK_PADDING_Y - CODE_BLOCK_TRAILING_LEAD,
            })
            .show(ui, |ui| {
                // Fill the reading column instead of shrinking to the widest
                // source line, and use the same line pitch as a fenced block
                // - the container default double-spaced every line.
                ui.set_min_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = CODE_LINE_SPACING;
                let mut line_start = 0usize;
                for (line_index, line) in document.source_text().split_inclusive('\n').enumerate() {
                    let span = document
                        .source_span(line_start..line_start + line.len())
                        .unwrap_or_else(|| document.source_span(0..0).expect("empty span"));
                    let response = ui.add(
                        egui::Label::new(source_line_job(line, span, self.find))
                            .selectable(true)
                            .wrap(),
                    );
                    if matches!(*self.pending_scroll, Some(PendingScroll::Byte(target)) if byte_range_contains(span, target))
                    {
                        response.scroll_to_me(Some(Align::Center));
                        *self.pending_scroll = None;
                    }
                    if let Some(index) = self.line_heading_indices.get(line_index).copied().flatten() {
                        if matches!(*self.pending_scroll, Some(PendingScroll::Heading(target)) if target == index)
                        {
                            response.scroll_to_me(Some(Align::Center));
                            *self.pending_scroll = None;
                        }
                    }
                    line_start += line.len();
                }
            });
    }
}

fn pending_scroll_for_anchor(document: &MarkdownDocument, anchor: ScrollAnchor) -> PendingScroll {
    PendingScroll::Byte(byte_offset_for_anchor(document, anchor))
}

pub fn scroll_anchor_for_offset(document: &MarkdownDocument, byte_offset: usize) -> ScrollAnchor {
    let heading_index = document.nearest_heading_index_at_byte(byte_offset);
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
    Ok((display_local_path(&canonical), document))
}

/// Mirrors `load_local_document`, but for bytes already fetched from a
/// remote SFTP session (issue #133) instead of the local filesystem: no
/// disk I/O, no canonicalization, just parsing already-in-memory content
/// against the caller-supplied `RemoteMarkdownSource` identity.
fn load_remote_document(
    source: RemoteMarkdownSource,
    display_path: String,
    content: Vec<u8>,
) -> Result<(String, MarkdownDocument), MarkdownViewerLoadFailure> {
    let byte_len = content.len();
    let document = MarkdownLoader::default()
        .load(
            MarkdownSource::from(source),
            byte_len,
            &content,
            &Default::default(),
        )
        .map_err(MarkdownViewerLoadFailure::Load)?;
    Ok((display_path, document))
}

fn display_local_path(path: &Path) -> String {
    let display = path.display().to_string();
    #[cfg(target_os = "windows")]
    {
        if let Some(unc) = display.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{unc}");
        }
        if let Some(ordinary) = display.strip_prefix(r"\\?\") {
            return ordinary.to_owned();
        }
    }
    display
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
    pending_image_loads: &BTreeMap<usize, PendingImageLoad>,
    image_errors: &BTreeMap<usize, String>,
    pending_scroll: &mut Option<PendingScroll>,
    outline_selected: &mut Option<usize>,
    text_style: InlineRenderStyle,
) {
    let response = ui
        .horizontal_wrapped(|ui| {
            // Inline runs are separate widgets, so egui's default item
            // spacing would be injected between every one of them: text like
            // "(see [#50](...))" rendered as "#50   )". The source text
            // already carries the spaces that belong between runs.
            ui.spacing_mut().item_spacing.x = 0.0;
            // Row pitch comes from `TextFormat::line_height` on each section
            // (see `InlineRenderStyle::line_height`). `item_spacing.y` does
            // not move wrapped rows inside `horizontal_wrapped`.
            ui.spacing_mut().item_spacing.y = 0.0;
            render_inline_flow(
                ui,
                block.inlines(),
                document,
                find,
                approvals,
                loaded_images,
                pending_image_loads,
                image_errors,
                FontId::proportional(BODY_TEXT_SIZE),
                text_style,
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
    pending_image_loads: &BTreeMap<usize, PendingImageLoad>,
    image_errors: &BTreeMap<usize, String>,
    font: FontId,
    style: InlineRenderStyle,
    pending_scroll: &mut Option<PendingScroll>,
    outline_selected: &mut Option<usize>,
) {
    for inline in inlines {
        match inline {
            Inline::Text(text) => {
                render_text_run(ui, text.text(), text.text_span(), find, font.clone(), style);
            }
            Inline::Code(text) => {
                ui.add(
                    egui::Label::new(text_job(
                        text.text(),
                        text.text_span(),
                        find,
                        FontId::monospace(font.size),
                        style.as_inline_code(),
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
                pending_image_loads,
                image_errors,
                FontId::proportional(font.size),
                style.with_italics(),
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
                pending_image_loads,
                image_errors,
                font.clone(),
                style.with_strong(),
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
                    pending_image_loads,
                    image_errors,
                    font.clone(),
                    style,
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
                    style,
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
                    pending_image_loads,
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
                        style.as_inline_code(),
                    ))
                    .selectable(true)
                    .wrap(),
                );
            }
            // CommonMark: a single newline inside a paragraph is a *soft*
            // break and renders as a space, so the text reflows to the
            // reading column's width. Treating it as a row break made every
            // paragraph inherit the source file's hard wrapping and left a
            // ragged right edge far short of the column. Advancing the
            // cursor rather than adding a space-only widget keeps the space
            // from being pushed onto the next row as a leading indent, and
            // the space is dropped entirely when the row is already full for
            // the same reason.
            Inline::SoftBreak { .. } => {
                let width = ui.ctx().fonts_mut(|fonts| fonts.glyph_width(&font, ' '));
                if ui.available_width() > width * 2.0 {
                    ui.add_space(width);
                }
            }
            Inline::HardBreak { .. } => {
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
    pending_image_loads: &BTreeMap<usize, PendingImageLoad>,
    image_errors: &BTreeMap<usize, String>,
    font: FontId,
    style: InlineRenderStyle,
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
                        text.text_span(),
                        find,
                        if mono {
                            FontId::monospace(font.size)
                        } else {
                            font.clone()
                        },
                        if mono {
                            style.as_inline_code().with_strikethrough()
                        } else {
                            style.with_strikethrough()
                        },
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
                pending_image_loads,
                image_errors,
                font.clone(),
                style.with_strikethrough(),
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
    style: InlineRenderStyle,
    pending_scroll: &mut Option<PendingScroll>,
    outline_selected: &mut Option<usize>,
) {
    let reference = &document.resource_references()[link.reference_index()];
    let accessible_text = if link.plain_text().is_empty() {
        reference.target().to_owned()
    } else {
        link.plain_text().to_owned()
    };
    let job = if link.inlines().is_empty() {
        text_job(&accessible_text, link.span(), find, font, style.as_link())
    } else {
        inline_layout_job(link.inlines(), document, find, font, style.as_link())
    };
    let response = ui.add(egui::Button::new(job).frame(false));
    response.widget_info(|| {
        WidgetInfo::labeled(WidgetType::Button, true, format!("Link: {accessible_text}"))
    });
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

#[allow(clippy::too_many_arguments)]
fn render_image(
    ui: &mut egui::Ui,
    image: &ImageInline,
    document: &MarkdownDocument,
    approvals: &ResourceApprovalState,
    loaded_images: &BTreeMap<usize, LoadedImage>,
    pending_image_loads: &BTreeMap<usize, PendingImageLoad>,
    image_errors: &BTreeMap<usize, String>,
    pending_scroll: &mut Option<PendingScroll>,
) {
    let reference = &document.resource_references()[image.reference_index()];
    // An image owns a whole paragraph row. `render_text_block` lays inline
    // runs out in a `horizontal_wrapped` with zero item spacing, so without
    // this the group was squeezed into whatever width was left on the
    // current row and its stacked labels ran together ("Image: diagramLocal
    // resource"). Claiming the paragraph's full width both wraps the group
    // onto its own row and gives a loaded image the whole reading column.
    let full_width = ui.max_rect().width().max(1.0);
    let response = ui
        .allocate_ui_with_layout(
            vec2(full_width, 0.0),
            egui::Layout::top_down(Align::Min),
            |ui| {
                ui.set_min_width(full_width);
                ui.set_max_width(full_width);
                // `render_text_block` zeroes item spacing so inline runs butt
                // up against each other; restore the application's ambient
                // spacing inside the group or its stacked rows collide.
                ui.spacing_mut().item_spacing =
                    ui.ctx().style_of(ui.ctx().theme()).spacing.item_spacing;
                ui.group(|ui| {
                    ui.set_min_width(ui.available_width());
                    if let Some(loaded) = loaded_images.get(&image.reference_index()) {
                        let available = ui.available_width().max(1.0);
                        let native_width = loaded.size[0] as f32;
                        // Scale down to the reading column when the image is
                        // wider than it, but never scale *up* past the
                        // image's own resolution -- stretching a small icon
                        // across the column only makes it blurry.
                        // `fit_to_original_size` plus `max_width` is exactly
                        // that rule.
                        ui.add(
                            egui::Image::new(&loaded.texture)
                                .fit_to_original_size(1.0)
                                .max_width(available.min(native_width.max(1.0))),
                        );
                        if !image.alt_text().is_empty() {
                            ui.label(
                                RichText::new(image.alt_text())
                                    .small()
                                    .color(theme::TEXT_SECONDARY),
                            );
                        }
                    } else {
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
                        if pending_image_loads.contains_key(&image.reference_index()) {
                            ui.label(
                                RichText::new("Loading local image…")
                                    .small()
                                    .color(theme::TEXT_SECONDARY),
                            );
                        } else {
                            ui.label(
                                RichText::new(resource_placeholder_action(reference.class()))
                                    .small(),
                            );
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
                    }
                });
            },
        )
        .response;
    if matches!(pending_scroll, Some(PendingScroll::Byte(target)) if byte_range_contains(image.span(), *target))
    {
        response.scroll_to_me(Some(Align::Center));
        *pending_scroll = None;
    }
}

/// Chooses a width for every column of a markdown table.
///
/// A table that fits keeps its natural, unwrapped column widths, which is what
/// makes a two-column reference table read as two tidy columns instead of a
/// grid stretched across the whole reading width. A table that does not fit
/// gives every column [`TABLE_MIN_COLUMN_WIDTH`] and then shares the remaining
/// width out in proportion to how much more each column asked for, so the
/// prose column absorbs the wrapping and a short label column is not crushed
/// down to one character per line alongside it.
fn table_column_widths(natural: &[f32], available: f32) -> Vec<f32> {
    let total: f32 = natural.iter().sum();
    if natural.is_empty() || total <= available {
        return natural.to_vec();
    }
    let budget = available - TABLE_MIN_COLUMN_WIDTH * natural.len() as f32;
    let slack: f32 = natural
        .iter()
        .map(|width| (width - TABLE_MIN_COLUMN_WIDTH).max(0.0))
        .sum();
    if budget <= 0.0 || slack <= 0.0 {
        // Narrower than the floor allows; the horizontal scroll area that
        // wraps the table is what keeps the content reachable.
        return vec![TABLE_MIN_COLUMN_WIDTH; natural.len()];
    }
    natural
        .iter()
        .map(|width| {
            TABLE_MIN_COLUMN_WIDTH + (width - TABLE_MIN_COLUMN_WIDTH).max(0.0) / slack * budget
        })
        .collect()
}

fn inline_layout_job(
    inlines: &[Inline],
    document: &MarkdownDocument,
    find: &MarkdownFindState,
    font: FontId,
    style: InlineRenderStyle,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    append_inline_layout(&mut job, inlines, document, find, font, style);
    job
}

#[allow(clippy::too_many_arguments, clippy::only_used_in_recursion)]
fn append_inline_layout(
    job: &mut LayoutJob,
    inlines: &[Inline],
    document: &MarkdownDocument,
    find: &MarkdownFindState,
    font: FontId,
    style: InlineRenderStyle,
) {
    for inline in inlines {
        match inline {
            Inline::Text(text) => append_text_segments(
                job,
                text.text(),
                text.text_span(),
                find,
                font.clone(),
                style,
            ),
            Inline::Code(text) => append_text_segments(
                job,
                text.text(),
                text.text_span(),
                find,
                FontId::monospace(font.size),
                style.as_inline_code(),
            ),
            Inline::Emphasis(container) => append_inline_layout(
                job,
                container.inlines(),
                document,
                find,
                font.clone(),
                style.with_italics(),
            ),
            Inline::Strong(container) => append_inline_layout(
                job,
                container.inlines(),
                document,
                find,
                font.clone(),
                style.with_strong(),
            ),
            Inline::Strikethrough(container) => append_inline_layout(
                job,
                container.inlines(),
                document,
                find,
                font.clone(),
                style.with_strikethrough(),
            ),
            Inline::Link(link) => append_inline_layout(
                job,
                link.inlines(),
                document,
                find,
                font.clone(),
                style.as_link(),
            ),
            Inline::Image(image) => append_text_segments(
                job,
                image.alt_text(),
                image.alt_text_span(),
                find,
                font.clone(),
                style,
            ),
            Inline::RawHtml(html) => append_text_segments(
                job,
                html.literal(),
                html.span(),
                find,
                FontId::monospace(font.size),
                style.as_inline_code(),
            ),
            // See `render_inline_flow`: a soft break is a space, not a line
            // break. Only an explicit hard break starts a new line.
            Inline::SoftBreak { .. } => job.append(" ", 0.0, base_text_format(font.clone(), style)),
            Inline::HardBreak { .. } => {
                job.append("\n", 0.0, base_text_format(font.clone(), style))
            }
        }
    }
}

fn text_job(
    text: &str,
    span: SourceSpan,
    find: &MarkdownFindState,
    font: FontId,
    style: InlineRenderStyle,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    append_text_segments(&mut job, text, span, find, font, style);
    job
}

/// Emits one text run inside a `horizontal_wrapped` flow.
///
/// A run that follows inline code or a link usually begins with the space
/// that separated them. Left inside the wrapped `Label`, that space becomes
/// the run's first glyph, so whenever the run is pushed onto a fresh row the
/// paragraph gains a one-space hanging indent on that row only. Emitting the
/// space as cursor advance instead - the same treatment `Inline::SoftBreak`
/// gets, including dropping it when the row is already full - keeps every
/// row flush with the reading column's left edge.
fn render_text_run(
    ui: &mut egui::Ui,
    text: &str,
    span: SourceSpan,
    find: &MarkdownFindState,
    font: FontId,
    style: InlineRenderStyle,
) {
    if text.is_empty() {
        return;
    }
    let skip = text.len() - text.trim_start_matches(' ').len();
    if skip > 0 {
        let width = ui.ctx().fonts_mut(|fonts| fonts.glyph_width(&font, ' '));
        if ui.available_width() > width * 2.0 {
            ui.add_space(width * skip as f32);
        }
    }
    if skip >= text.len() {
        return;
    }
    let mut job = LayoutJob::default();
    append_text_segments_from(&mut job, text, span, find, font, style, skip);
    ui.add(egui::Label::new(job).selectable(true).wrap());
}

fn source_line_job(line: &str, span: SourceSpan, find: &MarkdownFindState) -> LayoutJob {
    let mut job = LayoutJob::default();
    let format = base_text_format(FontId::monospace(CODE_TEXT_SIZE), InlineRenderStyle::body());
    append_text_segments(
        &mut job,
        trim_line_ending(line),
        span,
        find,
        FontId::monospace(CODE_TEXT_SIZE),
        InlineRenderStyle::body(),
    );
    ensure_code_line_body(&mut job, format);
    apply_code_line_height(&mut job);
    job
}

/// A blank line lays out to nothing at all, which would collapse it away and
/// misalign the document against its source. Give it one space so it keeps a
/// full row.
fn ensure_code_line_body(job: &mut LayoutJob, format: TextFormat) {
    if job.sections.is_empty() {
        job.append(" ", 0.0, format);
    }
}

fn append_text_segments(
    job: &mut LayoutJob,
    text: &str,
    span: SourceSpan,
    find: &MarkdownFindState,
    font: FontId,
    style: InlineRenderStyle,
) {
    append_text_segments_from(job, text, span, find, font, style, 0);
}

/// As `append_text_segments`, but starts appending at `skip` bytes into
/// `text`. Find matches are still resolved against the whole `text` so the
/// caller's `span` stays the source of truth for byte offsets; only the
/// emitted glyphs are clipped. Callers use this to drop a run's leading
/// space without having to rebase its `SourceSpan`, which the markdown crate
/// does not expose a constructor for.
fn append_text_segments_from(
    job: &mut LayoutJob,
    text: &str,
    span: SourceSpan,
    find: &MarkdownFindState,
    font: FontId,
    style: InlineRenderStyle,
    skip: usize,
) {
    if skip >= text.len() {
        return;
    }
    let mut cursor = skip;
    let matches: Vec<(usize, usize, bool)> = find
        .matches()
        .iter()
        .enumerate()
        .filter_map(|(index, matched)| {
            overlap_with_relative_range(span, matched.span()).and_then(|range| {
                (range.end <= text.len() && range.end > skip).then_some((
                    range.start.max(skip),
                    range.end,
                    find.current_index == Some(index),
                ))
            })
        })
        .collect();
    for (start, end, current) in matches {
        if cursor < start {
            job.append(
                &text[cursor..start],
                0.0,
                base_text_format(font.clone(), style),
            );
        }
        let mut format = base_text_format(font.clone(), style);
        format.background = if current {
            theme::ACCENT_PRIMARY.gamma_multiply(0.35)
        } else {
            theme::SURFACE_SELECTION
        };
        job.append(&text[start..end], 0.0, format);
        cursor = end;
    }
    if cursor < text.len() {
        job.append(&text[cursor..], 0.0, base_text_format(font, style));
    }
}

fn highlighted_line_job(line: &HighlightedCodeLine) -> LayoutJob {
    let mut job = LayoutJob::default();
    if line.spans().is_empty() {
        job.append(
            trim_line_ending(line.text()),
            0.0,
            base_text_format(FontId::monospace(CODE_TEXT_SIZE), InlineRenderStyle::body()),
        );
    } else {
        for span in line.spans() {
            job.append(
                trim_line_ending(span.text()),
                0.0,
                text_format_from_highlight(span.style()),
            );
        }
    }
    ensure_code_line_body(
        &mut job,
        base_text_format(FontId::monospace(CODE_TEXT_SIZE), InlineRenderStyle::body()),
    );
    apply_code_line_height(&mut job);
    job
}

/// `festerm_markdown` deliberately preserves exact source text, so each code
/// line still carries its `\n`. Laying that out gives every line a second,
/// empty row - the code read as double-spaced. The terminator is presentation
/// only; the model keeps it for spans and Copy.
fn trim_line_ending(text: &str) -> &str {
    match text.strip_suffix('\n') {
        Some(text) => text.strip_suffix('\r').unwrap_or(text),
        None => text,
    }
}

/// The bundled monospace face has generous vertical metrics, so a one-line
/// galley is far taller than its font size. Pinning the row height keeps
/// fenced code and Source view at the mockup's `1.55` pitch instead of
/// rendering visibly double-spaced.
fn apply_code_line_height(job: &mut LayoutJob) {
    for section in &mut job.sections {
        section.format.line_height = Some(CODE_LINE_HEIGHT);
    }
}

fn text_format_from_highlight(style: HighlightStyle) -> TextFormat {
    let mut format = TextFormat {
        font_id: FontId::monospace(CODE_TEXT_SIZE),
        color: Color32::from_rgba_unmultiplied(
            style.foreground().red(),
            style.foreground().green(),
            style.foreground().blue(),
            style.foreground().alpha(),
        ),
        // The fenced block already paints one continuous code surface.
        // Painting the syntax theme's own per-span background on top of it
        // drew a lighter pill around every token, which broke the block into
        // ragged chips instead of the mockup's uniform `pre`.
        background: Color32::TRANSPARENT,
        ..Default::default()
    };
    if style.bold() {
        format.font_id = FontId::monospace(CODE_TEXT_SIZE + 0.5);
    }
    if style.underline() {
        format.underline = egui::Stroke::new(1.0, format.color);
    }
    if style.italic() {
        format.italics = true;
    }
    format
}

fn base_text_format(font: FontId, style: InlineRenderStyle) -> TextFormat {
    let inline_code = style.code_like;
    let mut format = TextFormat {
        font_id: font,
        color: if style.link_like {
            style.link_color
        } else {
            style.text_color
        },
        background: if inline_code && !style.link_like {
            style.code_background
        } else {
            Color32::TRANSPARENT
        },
        line_height: style.line_height,
        ..Default::default()
    };
    if style.strong {
        format.font_id.size += 0.5;
    }
    if style.strikethrough {
        format.strikethrough = egui::Stroke::new(1.0, format.color);
    }
    if style.italics {
        format.italics = true;
    }
    if style.link_like {
        format.underline = egui::Stroke::new(1.0, format.color);
    }
    format
}

/// A toolbar control shaped like the mockup's `.fmd-tool`: a fixed-height
/// pill with an optional icon, muted until it is the active choice.
///
/// The icon and text are measured and laid out explicitly rather than handed
/// to `Ui::button`, because egui sizes a button from its galley alone and
/// leaves no room to paint a leading icon into.
fn toolbar_button(
    ui: &mut egui::Ui,
    icon_name: Option<Icon>,
    label: &str,
    accessible_label: &str,
    active: bool,
) -> bool {
    toolbar_button_response(ui, icon_name, label, accessible_label, active).clicked()
}

/// As `toolbar_button`, but hands back the `Response` so a caller can anchor
/// a popup to it.
fn toolbar_button_response(
    ui: &mut egui::Ui,
    icon_name: Option<Icon>,
    label: &str,
    accessible_label: &str,
    active: bool,
) -> egui::Response {
    let font = FontId::proportional(TOOLBAR_TEXT_SIZE);
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        font,
        if active {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_SECONDARY
        },
    );
    let icon_width = icon_name
        .map(|_| TOOLBAR_ICON_SIZE + TOOLBAR_ICON_TEXT_GAP)
        .unwrap_or(0.0);
    let width =
        (TOOLBAR_BUTTON_PADDING_X * 2.0 + icon_width + galley.size().x).max(TOOLBAR_BUTTON_HEIGHT);
    let (rect, response) =
        ui.allocate_exact_size(vec2(width, TOOLBAR_BUTTON_HEIGHT), Sense::click());
    response
        .widget_info(|| WidgetInfo::selected(WidgetType::Button, true, active, accessible_label));

    if active || response.hovered() {
        let fill = if active {
            theme::SURFACE_TAB_ACTIVE
        } else {
            theme::SURFACE_TAB_INACTIVE
        };
        ui.painter().rect_filled(rect, TOOLBAR_BUTTON_RADIUS, fill);
    }
    if active {
        ui.painter().rect_stroke(
            rect,
            TOOLBAR_BUTTON_RADIUS,
            egui::Stroke::new(1.0, theme::BORDER_ACTIVE),
            egui::StrokeKind::Inside,
        );
    }

    let mut cursor = rect.left() + TOOLBAR_BUTTON_PADDING_X;
    if let Some(icon_name) = icon_name {
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(cursor + TOOLBAR_ICON_SIZE / 2.0, rect.center().y),
            egui::Vec2::splat(TOOLBAR_ICON_SIZE),
        );
        icon::paint(
            ui.painter(),
            icon_name,
            icon_rect,
            if active {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_SECONDARY
            },
        );
        cursor += TOOLBAR_ICON_SIZE + TOOLBAR_ICON_TEXT_GAP;
    }
    ui.painter().galley(
        egui::pos2(cursor, rect.center().y - galley.size().y / 2.0),
        galley,
        theme::TEXT_SECONDARY,
    );

    response.on_hover_text(accessible_label)
}

/// An icon and its text on one baseline. The icon is allocated inside an
/// explicit horizontal run so the pair stays on a single row even when the
/// caller's layout is vertical, which is how the outline title is used.
fn icon_label(ui: &mut egui::Ui, icon_name: Icon, text: RichText, color: Color32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = TOOLBAR_ICON_TEXT_GAP;
        let (rect, response) =
            ui.allocate_exact_size(egui::Vec2::splat(TOOLBAR_ICON_SIZE), Sense::hover());
        icon::paint(ui.painter(), icon_name, rect, color);
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text.text()));
        ui.label(text);
    });
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

fn build_line_heading_index_lookup(document: &MarkdownDocument) -> Vec<Option<usize>> {
    let headings = document.headings();
    let mut lookup = Vec::with_capacity(document.line_count());
    let mut heading_index = 0usize;
    let mut active_heading = None;
    let mut line_start = 0usize;
    for line in document.source_text().split_inclusive('\n') {
        while heading_index < headings.len()
            && line_start >= headings[heading_index].section_end_byte()
        {
            heading_index += 1;
        }
        if heading_index < headings.len()
            && line_start >= headings[heading_index].section_start_byte()
            && line_start < headings[heading_index].section_end_byte()
        {
            active_heading = Some(heading_index);
        }
        lookup.push(active_heading);
        line_start += line.len();
    }
    if document.source_text().is_empty() {
        lookup.push(None);
    }
    lookup
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
        commands.push(AppCommand::OpenLocalMarkdownFile {
            path,
            replacing: None,
        });
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
    use egui_kittest::{kittest::Queryable, Harness};
    use festerm_markdown::RemoteSourceOwner;
    use std::ops::Range;
    use std::time::Duration;

    /// The reference table that exposed the collapsing-column defect: two
    /// columns, one of them short labels and the other prose long enough that
    /// the pair has to be balanced rather than simply fitted.
    const REFERENCE_TABLE: &str = concat!(
        "| Task | Primary reference |\n",
        "| --- | --- |\n",
        "| Product scope and priorities | `DESIGN.md`, `ROADMAP.md` |\n",
        "| Dependency and ownership boundaries | `ARCHITECTURE.md` |\n",
    );

    /// Renders a document through the real block renderer inside a headless
    /// harness, so layout assertions measure the geometry the viewer actually
    /// draws rather than a reimplementation of it.
    fn render_markdown(markdown: &str) -> Harness<'static, ()> {
        let parsed = document(markdown);
        let find = MarkdownFindState::default();
        let approvals = ResourceApprovalState::default();
        let loaded_images = BTreeMap::new();
        let pending_image_loads = BTreeMap::new();
        let image_errors = BTreeMap::new();
        let mut outline_selected = None;
        let mut pending_scroll = None;
        let mut outline_keyboard_focus = false;
        Harness::builder().build_ui_state(
            move |ui, _state: &mut ()| {
                let mut state = MarkdownRenderState {
                    mode: MarkdownViewerMode::Preview,
                    outline_open: false,
                    outline_selected: &mut outline_selected,
                    find: &find,
                    resource_approvals: &approvals,
                    loaded_images: &loaded_images,
                    pending_image_loads: &pending_image_loads,
                    image_errors: &image_errors,
                    pending_scroll: &mut pending_scroll,
                    line_heading_indices: &[],
                    outline_keyboard_focus: &mut outline_keyboard_focus,
                };
                state.render_blocks(ui, parsed.blocks(), &parsed, InlineRenderStyle::body());
            },
            (),
        )
    }

    /// `egui::Grid` sized a column from what its cells reported last frame,
    /// and a wrapping `Label` reports its own wrapped width, so every frame
    /// the column shrank a little further until the text wrapped one
    /// character per line. Measuring the cells up front breaks that loop.
    #[test]
    fn a_table_column_that_fits_is_not_wrapped_one_character_per_line() {
        let mut harness = render_markdown(REFERENCE_TABLE);
        harness.run();
        harness.run();
        harness.run();

        let header = harness.get_by_label("Primary reference").rect();
        assert!(
            header.height() <= BODY_TEXT_SIZE * 2.0,
            "the header cell is {} px tall, so it wrapped; a column with room \
             for its text must render it on one line",
            header.height()
        );
        assert!(
            header.width() >= 90.0,
            "the header cell is only {} px wide, so its column collapsed",
            header.width()
        );
    }

    /// A table too wide for the reading column has to be squeezed, but it
    /// must still be squeezed on word boundaries rather than collapsed: this
    /// is the path the old renderer degenerated into for *every* table.
    #[test]
    fn a_table_too_wide_for_the_reading_column_still_wraps_on_words() {
        let mut harness = render_markdown(concat!(
            "| Capability | Evidence |\n",
            "| --- | --- |\n",
            "| Session restore across an application restart with every tab ",
            "reattached | Covered by the workspace persistence suite and the ",
            "native session daemon smoke test |\n",
        ));
        harness.run();
        harness.run();

        let header = harness.get_by_label("Evidence").rect();
        assert!(
            header.width() >= TABLE_MIN_COLUMN_WIDTH - f32::from(TABLE_CELL_PADDING_X) * 2.0,
            "a squeezed column fell below the floor at {} px",
            header.width()
        );
        let cell = harness
            .get_by_label(
                "Covered by the workspace persistence suite and the native \
                 session daemon smoke test",
            )
            .rect();
        assert!(
            cell.height() <= BODY_TEXT_SIZE * 8.0,
            "the squeezed cell is {} px tall, so it is wrapping far too narrow",
            cell.height()
        );
    }

    #[test]
    fn a_table_that_fits_keeps_its_natural_column_widths() {
        let widths = table_column_widths(&[120.0, 260.0], 600.0);
        assert_eq!(widths, vec![120.0, 260.0]);
    }

    #[test]
    fn a_table_that_does_not_fit_is_shared_out_across_the_reading_width() {
        let widths = table_column_widths(&[120.0, 600.0], 400.0);
        let total: f32 = widths.iter().sum();

        assert!(
            (total - 400.0).abs() <= 0.5,
            "a squeezed table should use exactly the width it has, not {total}"
        );
        assert!(
            widths.iter().all(|width| *width >= TABLE_MIN_COLUMN_WIDTH),
            "no column may be squeezed below the floor: {widths:?}"
        );
        assert!(
            widths[1] > widths[0],
            "the column that asked for more should still be the wider one: {widths:?}"
        );
        // The prose column gives up far more than the label column does.
        assert!(
            widths[0] > 90.0,
            "the short column lost too much of its {widths:?} share"
        );
    }

    #[test]
    fn a_table_narrower_than_its_floor_falls_back_to_the_floor() {
        let widths = table_column_widths(&[300.0, 300.0, 300.0], 100.0);
        assert_eq!(widths, vec![TABLE_MIN_COLUMN_WIDTH; 3]);
    }

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

    fn highlighted_sections(job: &LayoutJob) -> Vec<(Range<usize>, String)> {
        job.sections
            .iter()
            .filter(|section| section.format.background != Color32::TRANSPARENT)
            .map(|section| {
                let range = section.byte_range.start.0..section.byte_range.end.0;
                (range.clone(), job.text[range].to_owned())
            })
            .collect()
    }

    fn section_format_for_text<'a>(job: &'a LayoutJob, needle: &str) -> &'a TextFormat {
        let start = job.text.find(needle).expect("text should be present");
        let end = start + needle.len();
        &job.sections
            .iter()
            .find(|section| {
                let range = section.byte_range.start.0..section.byte_range.end.0;
                range.start <= start && range.end >= end
            })
            .expect("section should cover the requested text")
            .format
    }

    /// A scratch directory for image-loading tests. The viewer resolves
    /// relative image targets against the *canonicalised* Markdown path, so
    /// the temp root is canonicalised here too — on Windows `TEMP` is
    /// routinely an `8.3` short path that would otherwise not match.
    fn image_test_directory(label: &str) -> PathBuf {
        let root = fs::canonicalize(std::env::temp_dir()).expect("temp dir should canonicalise");
        let directory = root.join(format!(
            "festerm-markdown-image-{label}-{}-{:?}",
            std::process::id(),
            thread::current().id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("scratch directory should be creatable");
        directory
    }

    fn write_test_png(path: &Path, width: u32, height: u32) {
        image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]))
            .save(path)
            .expect("test PNG should be writable");
    }

    /// Drives the viewer's per-frame background work until `condition` holds,
    /// mirroring what `show` does every frame without needing a real frame.
    fn pump_image_loads(
        viewer: &mut MarkdownViewerTab,
        context: &egui::Context,
        mut condition: impl FnMut(&MarkdownViewerTab) -> bool,
    ) -> bool {
        for _ in 0..200 {
            viewer.poll_background_work(context);
            viewer.start_automatic_image_loads(context);
            if condition(viewer) {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// Images a local document references relatively are loaded on open. A
    /// document whose pictures are all "Load local image" buttons is not a
    /// readable document; see `start_automatic_image_loads` for why this
    /// narrow case is exempt from ADR 0030's explicit-activation rule.
    #[test]
    fn local_relative_images_load_without_the_reader_asking() {
        let directory = image_test_directory("auto");
        write_test_png(&directory.join("diagram.png"), 4, 3);
        let markdown = directory.join("readme.md");
        fs::write(&markdown, b"# Doc\n\n![A diagram](diagram.png)\n").unwrap();

        let context = egui::Context::default();
        let mut viewer = MarkdownViewerTab::open_local(markdown);
        let loaded = pump_image_loads(&mut viewer, &context, |viewer| {
            !viewer.loaded_images.is_empty()
        });

        assert!(
            loaded,
            "the local image should have loaded on its own; errors: {:?}",
            viewer.image_errors
        );
        assert!(viewer.image_errors.is_empty());
        let _ = fs::remove_dir_all(&directory);
    }

    /// The exemption stops at the document's own directory: anything that
    /// would reach the network keeps its explicit placeholder.
    #[test]
    fn absolute_url_images_are_never_loaded_automatically() {
        let directory = image_test_directory("absolute");
        let markdown = directory.join("readme.md");
        fs::write(
            &markdown,
            b"# Doc\n\n![Remote](https://example.test/diagram.png)\n",
        )
        .unwrap();

        let context = egui::Context::default();
        let mut viewer = MarkdownViewerTab::open_local(markdown);
        viewer.start_automatic_image_loads(&context);

        assert!(viewer.pending_image_loads.is_empty());
        assert!(viewer.loaded_images.is_empty());
        assert_eq!(viewer.automatic_image_loads, 0);
        let _ = fs::remove_dir_all(&directory);
    }

    /// A reference that cannot be read must be attempted once, not re-read
    /// from disk on every frame for as long as the document stays open.
    #[test]
    fn a_local_image_that_cannot_be_read_is_only_attempted_once() {
        let directory = image_test_directory("missing");
        let markdown = directory.join("readme.md");
        fs::write(&markdown, b"# Doc\n\n![Gone](missing.png)\n").unwrap();

        let context = egui::Context::default();
        let mut viewer = MarkdownViewerTab::open_local(markdown);
        let failed = pump_image_loads(&mut viewer, &context, |viewer| {
            !viewer.image_errors.is_empty()
        });

        assert!(failed, "the missing image should have recorded an error");
        for _ in 0..5 {
            viewer.start_automatic_image_loads(&context);
        }
        assert_eq!(viewer.automatic_image_loads, 1);
        assert!(viewer.pending_image_loads.is_empty());
        let _ = fs::remove_dir_all(&directory);
    }

    /// Automatic loading is bounded: a document with more image references
    /// than the concurrency cap never starts more than the cap at once.
    #[test]
    fn automatic_image_loading_is_bounded_by_the_concurrency_cap() {
        let directory = image_test_directory("bounded");
        let mut body = String::from("# Doc\n\n");
        for index in 0..(MAX_CONCURRENT_AUTOMATIC_IMAGE_LOADS * 3) {
            write_test_png(&directory.join(format!("image-{index}.png")), 2, 2);
            body.push_str(&format!("![Image {index}](image-{index}.png)\n\n"));
        }
        let markdown = directory.join("readme.md");
        fs::write(&markdown, body.as_bytes()).unwrap();

        let context = egui::Context::default();
        let mut viewer = MarkdownViewerTab::open_local(markdown);
        viewer.start_automatic_image_loads(&context);

        assert_eq!(
            viewer.pending_image_loads.len(),
            MAX_CONCURRENT_AUTOMATIC_IMAGE_LOADS
        );

        // The rest follow as the first batch drains, so the whole document
        // still ends up readable.
        let all_loaded = pump_image_loads(&mut viewer, &context, |viewer| {
            viewer.loaded_images.len() == MAX_CONCURRENT_AUTOMATIC_IMAGE_LOADS * 3
        });
        assert!(
            all_loaded,
            "expected every image to load eventually; loaded {} errors {:?}",
            viewer.loaded_images.len(),
            viewer.image_errors
        );
        let _ = fs::remove_dir_all(&directory);
    }

    /// `Ctrl+O` inside a viewer replaces the document but must not carry the
    /// previous document's resource approvals across
    /// (`docs/adr/0030-native-markdown-viewer.md`).
    #[test]
    fn replacing_a_viewers_document_drops_the_previous_documents_approvals() {
        let directory = image_test_directory("replace");
        let first = directory.join("first.md");
        let second = directory.join("second.md");
        fs::write(
            &first,
            b"# First\n\n![Remote](https://example.test/a.png)\n",
        )
        .unwrap();
        fs::write(
            &second,
            b"# Second\n\n![Remote](https://example.test/b.png)\n",
        )
        .unwrap();

        let mut viewer = MarkdownViewerTab::open_local(first);
        viewer.resource_approvals.approve(0);
        viewer.mode = MarkdownViewerMode::Source;
        viewer.outline_open = false;

        viewer.open_local_replacing(second);

        assert!(!viewer.resource_approvals.is_approved(0));
        assert!(viewer.display_path().ends_with("second.md"));
        // View preferences are the reader's, not the document's.
        assert!(matches!(viewer.mode, MarkdownViewerMode::Source));
        assert!(!viewer.outline_open);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn outline_and_preview_keep_separate_vertical_viewports() {
        let document = document(
            "# fesTerm Architecture\n\nIntro.\n\n## Architectural Goals\n\nGoals.\n\n## Dependency Direction\n\nDependencies.",
        );
        let context = egui::Context::default();
        let mut outline_selected = None;
        let find = MarkdownFindState::default();
        let approvals = ResourceApprovalState::default();
        let loaded_images = BTreeMap::new();
        let pending_image_loads = BTreeMap::new();
        let image_errors = BTreeMap::new();
        let mut pending_scroll = None;
        let mut outline_keyboard_focus = false;
        let mut layout = None;

        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    vec2(1_000.0, 700.0),
                )),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    let mut render_state = MarkdownRenderState {
                        mode: MarkdownViewerMode::Preview,
                        outline_open: true,
                        outline_selected: &mut outline_selected,
                        find: &find,
                        resource_approvals: &approvals,
                        loaded_images: &loaded_images,
                        pending_image_loads: &pending_image_loads,
                        image_errors: &image_errors,
                        pending_scroll: &mut pending_scroll,
                        line_heading_indices: &[],
                        outline_keyboard_focus: &mut outline_keyboard_focus,
                    };
                    layout = Some(render_state.show_document(ui, &document));
                });
            },
        );
        output.textures_delta.clear();

        let layout = layout.expect("the Markdown document should be laid out");
        let outline = layout.outline_rect.expect("the outline should be visible");
        assert!(outline.width() <= OUTLINE_WIDTH + 18.0);
        // The document well starts exactly where the outline panel ends -
        // they abut across a single hairline, so any overlap at all would
        // put document content under the sidebar.
        assert!(layout.document_rect.left() >= outline.right());
        assert!(
            layout.document_rect.width() > 500.0,
            "unexpected Markdown layout: {layout:?}"
        );
        assert!(
            layout.document_rect.height() > 500.0,
            "unexpected Markdown layout: {layout:?}"
        );
    }

    #[test]
    fn a_narrow_window_hides_the_outline_so_the_reading_column_survives() {
        // At 400 logical points the 216pt outline left a 184pt well, which
        // wrapped prose to two or three words a row. The outline is a
        // navigation aid; the document is the point, so the outline yields.
        let document = document("# fesTerm\n\nIntro.\n\n## Goals\n\nGoals.");
        let context = egui::Context::default();
        let mut outline_selected = None;
        let find = MarkdownFindState::default();
        let approvals = ResourceApprovalState::default();
        let loaded_images = BTreeMap::new();
        let pending_image_loads = BTreeMap::new();
        let image_errors = BTreeMap::new();
        let mut pending_scroll = None;
        let mut outline_keyboard_focus = false;
        let mut layout = None;

        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    vec2(OUTLINE_WIDTH + OUTLINE_MIN_DOCUMENT_WIDTH - 40.0, 700.0),
                )),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    let mut render_state = MarkdownRenderState {
                        mode: MarkdownViewerMode::Preview,
                        // The user's own toggle stays on; only this frame
                        // declines to draw the panel.
                        outline_open: true,
                        outline_selected: &mut outline_selected,
                        find: &find,
                        resource_approvals: &approvals,
                        loaded_images: &loaded_images,
                        pending_image_loads: &pending_image_loads,
                        image_errors: &image_errors,
                        pending_scroll: &mut pending_scroll,
                        line_heading_indices: &[],
                        outline_keyboard_focus: &mut outline_keyboard_focus,
                    };
                    layout = Some(render_state.show_document(ui, &document));
                });
            },
        );
        output.textures_delta.clear();

        let layout = layout.expect("the Markdown document should be laid out");
        assert!(
            layout.outline_rect.is_none(),
            "the outline should collapse on a narrow window: {layout:?}"
        );
        assert!(
            layout.document_rect.width() > OUTLINE_MIN_DOCUMENT_WIDTH,
            "the document should claim the whole well: {layout:?}"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn local_markdown_display_paths_hide_windows_verbatim_prefixes() {
        assert_eq!(
            display_local_path(Path::new(r"\\?\C:\work\README.md")),
            r"C:\work\README.md"
        );
        assert_eq!(
            display_local_path(Path::new(r"\\?\UNC\server\share\README.md")),
            r"\\server\share\README.md"
        );
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
    fn opening_find_requests_query_focus() {
        let mut state = MarkdownFindState::default();
        state.open();
        assert!(state.take_focus_request());
        assert!(!state.take_focus_request());
    }

    #[test]
    fn find_highlighting_tracks_inline_code_text_offsets() {
        let document = document("before `code` after");
        let mut find = MarkdownFindState::default();
        find.set_query(&document, "code".to_owned());
        let Block::Paragraph(paragraph) = &document.blocks()[0] else {
            panic!("the first block should be a paragraph");
        };

        let job = inline_layout_job(
            paragraph.inlines(),
            &document,
            &find,
            FontId::proportional(14.0),
            InlineRenderStyle::body(),
        );

        assert_eq!(job.text, "before code after");
        assert_eq!(highlighted_sections(&job), vec![(7..11, "code".to_owned())]);
    }

    #[test]
    fn find_highlighting_tracks_link_label_offsets() {
        let document = document("see [needle link](https://example.com) now");
        let mut find = MarkdownFindState::default();
        find.set_query(&document, "needle".to_owned());
        let Block::Paragraph(paragraph) = &document.blocks()[0] else {
            panic!("the first block should be a paragraph");
        };

        let job = inline_layout_job(
            paragraph.inlines(),
            &document,
            &find,
            FontId::proportional(14.0),
            InlineRenderStyle::body(),
        );

        assert_eq!(job.text, "see needle link now");
        assert_eq!(
            highlighted_sections(&job),
            vec![(4..10, "needle".to_owned())]
        );
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
    fn only_h2_carries_a_rule_under_the_heading() {
        // The mockup gives `h2` a `border-bottom` and leaves `h1` bare, so
        // the document title is not doubly separated from the first
        // paragraph by both its own rule and the following section's.
        assert_eq!(
            heading_style(1),
            HeadingStyle {
                size: 30.0,
                text_color: theme::TEXT_PRIMARY,
                underline: false,
            }
        );
        assert_eq!(
            heading_style(2),
            HeadingStyle {
                size: 22.5,
                text_color: theme::TEXT_PRIMARY,
                underline: true,
            }
        );
        assert_eq!(
            heading_style(6),
            HeadingStyle {
                size: 12.75,
                text_color: theme::TEXT_SECONDARY,
                underline: false,
            }
        );
    }

    #[test]
    fn inline_layout_styles_emphasis_links_inline_code_and_blockquotes() {
        let document = document("> *quoted* [link](https://example.com) `code`");
        let Block::BlockQuote(blockquote) = &document.blocks()[0] else {
            panic!("the first block should be a blockquote");
        };
        let Block::Paragraph(paragraph) = &blockquote.blocks()[0] else {
            panic!("the quoted content should be a paragraph");
        };

        let job = inline_layout_job(
            paragraph.inlines(),
            &document,
            &MarkdownFindState::default(),
            FontId::proportional(BODY_TEXT_SIZE),
            InlineRenderStyle::blockquote(),
        );

        let quoted = section_format_for_text(&job, "quoted");
        let link = section_format_for_text(&job, "link");
        let code = section_format_for_text(&job, "code");
        assert!(quoted.italics);
        assert_eq!(quoted.color, theme::TEXT_SECONDARY);
        assert_eq!(link.color, theme::ACCENT_PRIMARY);
        assert_eq!(link.underline.color, theme::ACCENT_PRIMARY);
        assert_eq!(
            code.background,
            theme::SURFACE_TAB_ACTIVE.gamma_multiply(0.85)
        );
        assert_eq!(code.font_id.family, egui::FontFamily::Monospace);
    }

    #[test]
    fn a_soft_break_lays_out_as_a_space_not_a_row_break() {
        // CommonMark: a single newline inside a paragraph is a soft break
        // and renders as a space. Emitting a row break instead made every
        // paragraph inherit the source file's hard wrapping, so prose
        // stopped reflowing to the reading column.
        let document = document("alpha beta\ngamma delta\n");
        let Block::Paragraph(paragraph) = &document.blocks()[0] else {
            panic!("the first block should be a paragraph");
        };
        let job = inline_layout_job(
            paragraph.inlines(),
            &document,
            &MarkdownFindState::default(),
            FontId::proportional(BODY_TEXT_SIZE),
            InlineRenderStyle::body(),
        );
        assert_eq!(job.text, "alpha beta gamma delta");
    }

    #[test]
    fn a_hard_break_still_lays_out_as_a_row_break() {
        let document = document("alpha beta\\\ngamma delta\n");
        let Block::Paragraph(paragraph) = &document.blocks()[0] else {
            panic!("the first block should be a paragraph");
        };
        let job = inline_layout_job(
            paragraph.inlines(),
            &document,
            &MarkdownFindState::default(),
            FontId::proportional(BODY_TEXT_SIZE),
            InlineRenderStyle::body(),
        );
        assert_eq!(job.text, "alpha beta\ngamma delta");
    }

    #[test]
    fn code_lines_drop_their_terminator_and_carry_the_fenced_line_height() {
        // `festerm_markdown` keeps each code line's trailing newline so
        // spans and Copy stay faithful to the source. Laying that newline
        // out gives a one-line galley two rows, which is exactly the
        // double-spacing fenced blocks and Source view used to show.
        let document = document("```rust\nlet x = 1;\nlet y = 2;\n```\n");
        let Block::CodeBlock(block) = &document.blocks()[0] else {
            panic!("the first block should be a code block");
        };
        for line in block.highlighted_lines() {
            let job = highlighted_line_job(line);
            assert!(
                !job.text.contains('\n'),
                "code line laid out with its terminator: {:?}",
                job.text
            );
            for section in &job.sections {
                assert_eq!(section.format.line_height, Some(CODE_LINE_HEIGHT));
            }
        }
    }

    #[test]
    fn body_prose_carries_the_mockup_line_height() {
        // Wrapped rows inside `horizontal_wrapped` take their pitch from
        // `TextFormat::line_height`; `item_spacing.y` measurably does not
        // move them.
        let document = document("alpha beta gamma\n");
        let Block::Paragraph(paragraph) = &document.blocks()[0] else {
            panic!("the first block should be a paragraph");
        };
        let job = inline_layout_job(
            paragraph.inlines(),
            &document,
            &MarkdownFindState::default(),
            FontId::proportional(BODY_TEXT_SIZE),
            InlineRenderStyle::body(),
        );
        assert_eq!(job.sections[0].format.line_height, Some(BODY_LINE_HEIGHT));
    }

    #[test]
    fn a_text_run_can_skip_its_leading_space_without_losing_find_offsets() {
        // A run following inline code usually starts with the separating
        // space. It is emitted as cursor advance so a wrapped row never
        // gains a hanging indent, but the find highlight still has to line
        // up with the untrimmed source offsets.
        let document = document("`code` needle here\n");
        let mut find = MarkdownFindState::default();
        find.set_query(&document, "needle".to_owned());
        let Block::Paragraph(paragraph) = &document.blocks()[0] else {
            panic!("the first block should be a paragraph");
        };
        let Inline::Text(text) = &paragraph.inlines()[1] else {
            panic!("the run after the code span should be text");
        };
        assert!(text.text().starts_with(' '));

        let mut job = LayoutJob::default();
        append_text_segments_from(
            &mut job,
            text.text(),
            text.text_span(),
            &find,
            FontId::proportional(BODY_TEXT_SIZE),
            InlineRenderStyle::body(),
            1,
        );
        assert_eq!(job.text, "needle here");
        assert_eq!(
            highlighted_sections(&job),
            vec![(0..6, "needle".to_owned())]
        );
    }

    #[test]
    fn the_status_bar_reports_the_viewer_as_read_only_without_a_state_dot() {
        // `.fmd-status` in the mockup is the application status bar showing
        // `Local Markdown - UTF-8` and `Read only`, with no state dot: the
        // viewer has no live transport to report. A failed load is the only
        // thing that lights the dot.
        let dir =
            std::env::temp_dir().join(format!("festerm-markdown-status-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp directory should be creatable");
        let path = dir.join("readme.md");
        fs::write(&path, b"# Title\n").expect("temp file should be writable");

        let mut tab = MarkdownViewerTab::open_local(path.clone());
        assert_eq!(tab.status_bar_context(), "Local Markdown");
        assert_eq!(tab.status_bar_encoding(), "UTF-8");
        assert_eq!(tab.status_bar_label(), "Read only");
        assert_eq!(
            tab.status_bar_status(),
            festerm_ui_egui::chrome::ChipStatus::Neutral
        );

        fs::remove_file(&path).ok();
        tab.reload();
        assert_eq!(
            tab.status_bar_status(),
            festerm_ui_egui::chrome::ChipStatus::Failed
        );
        fs::remove_dir_all(&dir).ok();
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
    fn source_mode_uses_precomputed_heading_lookup_per_line() {
        let document = document("# One\nalpha\n# Two\nbeta\n");
        assert_eq!(
            build_line_heading_index_lookup(&document),
            vec![Some(0), Some(0), Some(1), Some(1)]
        );
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

    fn test_remote_source(remote_path: &str) -> RemoteMarkdownSource {
        RemoteMarkdownSource::new(
            "sftp.example.test",
            22,
            RemoteSourceOwner::username("deploy").unwrap(),
            "SHA256:abc123",
            remote_path,
            1,
        )
        .expect("valid remote source fields")
    }

    #[test]
    fn open_remote_parses_in_memory_bytes_without_touching_disk() {
        let source = test_remote_source("/srv/docs/guide.md");
        let tab = MarkdownViewerTab::open_remote(
            source,
            "/srv/docs/guide.md".to_owned(),
            b"# Remote Guide\n".to_vec(),
        );
        assert_eq!(tab.title(), "guide.md");
        assert_eq!(tab.display_path(), "/srv/docs/guide.md");
        assert_eq!(tab.chip_secondary(), "Markdown · Remote");
        assert_eq!(
            tab.status_bar_status(),
            festerm_ui_egui::chrome::ChipStatus::Neutral
        );
    }

    #[test]
    fn matches_remote_path_ignores_owner_and_fingerprint() {
        let tab = MarkdownViewerTab::open_remote(
            test_remote_source("/etc/motd"),
            "/etc/motd".to_owned(),
            b"hello\n".to_vec(),
        );
        assert!(tab.matches_remote_path("sftp.example.test", 22, "/etc/motd"));
        assert!(!tab.matches_remote_path("sftp.example.test", 2222, "/etc/motd"));
        assert!(!tab.matches_remote_path("other.example.test", 22, "/etc/motd"));
        assert!(!tab.matches_remote_path("sftp.example.test", 22, "/etc/other"));
    }

    #[test]
    fn reload_on_a_remote_tab_reports_reload_unsupported_instead_of_reloading() {
        let mut tab = MarkdownViewerTab::open_remote(
            test_remote_source("/etc/motd"),
            "/etc/motd".to_owned(),
            b"hello\n".to_vec(),
        );
        assert_eq!(
            tab.status_bar_status(),
            festerm_ui_egui::chrome::ChipStatus::Neutral
        );
        tab.reload();
        assert_eq!(
            tab.status_bar_status(),
            festerm_ui_egui::chrome::ChipStatus::Failed
        );
        let error = tab.error.as_ref().expect("reload should set an error");
        assert!(!error.title.is_empty());
        assert!(error.detail.contains("SFTP file manager"));
        // The original snapshot is preserved (not cleared) on this failure,
        // matching how a failed local reload keeps showing the last good
        // content rather than blanking the viewer.
        assert!(tab.document.is_some());
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
