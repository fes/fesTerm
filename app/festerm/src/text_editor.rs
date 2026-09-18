//! The native text editor tab (ADR 0034, issue #166).
//!
//! The tab is a *view*: it owns where the caret is, what is scrolled into
//! sight, and which options are switched on. The text itself, the undo
//! history, and the answer to "are there unsaved changes" belong to the
//! document registry, so two tabs showing one file agree with each other
//! without either of them being in charge.

use std::path::PathBuf;

use eframe::egui::{self, vec2, Align, FontId, Sense, WidgetInfo, WidgetType};
use festerm_document::{
    AutoSaveControl, BannerAction, CompiledSearch, DocumentId, DocumentStatus, MatchRange,
    SearchOutcome, Severity, StatusAccent, SubstituteCommand, SubstituteFlags, SubstituteRange,
};
use festerm_markdown::{LocalMarkdownSource, MarkdownSource};
use festerm_ui_egui::{chrome::ChipStatus, icon, icon::Icon, theme};

use crate::documents::SharedDocuments;
use crate::markdown_viewer::{
    elide_middle, toolbar_button, toolbar_button_response, toolbar_button_width,
    toolbar_button_with_trailing, MarkdownPreviewPane, TOOLBAR_BUTTON_GAP, TOOLBAR_BUTTON_HEIGHT,
    TOOLBAR_BUTTON_PADDING_X, TOOLBAR_BUTTON_RADIUS, TOOLBAR_TEXT_SIZE,
};
use crate::tabs::{AppCommand, TabId};
use crate::text_compare::ComparePane;
use festerm_document::{SearchIntent, ViAction, ViEngine, ViKey, ViMode};

use crate::vi_command::{
    normal_mode_command, parse as parse_command, CommandArea, CommandAreaEvent, CommandError,
    CommandOutcome, CommandPrompt, ViCommand,
};

/// Width of the line-number gutter's digits area before padding.
const GUTTER_PADDING_X: f32 = 12.0;
/// How much of the text around the caret is dragged into view with it, so a
/// caret lands in the middle of something rather than hard against an edge.
const CARET_SCROLL_MARGIN: f32 = 48.0;

const GUTTER_MIN_DIGITS: usize = 2;
/// The tighter gap inside one group of related toolbar controls.
const TOOLBAR_GROUP_GAP: f32 = 6.0;
const EDITOR_TEXT_SIZE: f32 = 13.0;
const BAR_PADDING_X: i8 = 9;
const BAR_PADDING_Y: i8 = 6;
const BANNER_ACCENT_WIDTH: f32 = 3.0;
const ORIGIN_ICON_SIZE: f32 = 12.0;
const MODE_SEGMENT_GAP: f32 = 2.0;
const FIND_FIELD_WIDTH: f32 = 170.0;
/// Width reserved for the match counter, so the navigation beside it keeps
/// still while the count changes.
const FIND_SUMMARY_WIDTH: f32 = 112.0;
const FIND_WARNING_SIZE: f32 = 14.0;
const OPTIONS_MENU_WIDTH: f32 = 250.0;
const OPTIONS_MENU_GAP: f32 = 6.0;
const OPTIONS_FIELD_WIDTH: f32 = 48.0;
/// The body's own horizontal padding, named because a fixed column count has
/// to add it back to reach the requested number of columns.
const BODY_MARGIN_X: f32 = 14.0;
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
    /// `Some(n)` wraps the body at `n` columns. Visual only: the wrap moves
    /// with the setting and no line break is ever written into the document
    /// (ADR 0034 §9). `None` lets the body use the width it is given.
    pub(crate) fixed_columns: Option<usize>,
    pub(crate) vi_keys: bool,
    /// Shows the Markdown outline beside the text. Offered only while the file
    /// renders as Markdown, because a heading list of a shell script would be
    /// an empty rail taking 216 pixels from the text.
    pub(crate) outline: bool,
    /// Colours source by its syntax (ADR 0035). Off means off: no parse, no
    /// cache, no cost, which is the honest escape hatch for anyone whose file
    /// or machine makes it expensive.
    pub(crate) syntax: bool,
}

impl Default for EditorViewOptions {
    fn default() -> Self {
        Self {
            line_numbers: true,
            fixed_columns: None,
            vi_keys: false,
            outline: false,
            syntax: true,
        }
    }
}

/// The column count offered the first time the box is ticked, and the one a
/// view falls back to if it is ticked while the field is empty.
const DEFAULT_FIXED_COLUMNS: usize = 72;

impl EditorViewOptions {
    /// How a view starts out, from what the reader last chose.
    pub(crate) fn from_settings(settings: festerm_config::EditorSettings) -> Self {
        Self {
            line_numbers: settings.line_numbers(),
            fixed_columns: settings.fixed_columns().map(|columns| columns as usize),
            vi_keys: settings.vi_keys(),
            outline: settings.outline(),
            syntax: settings.syntax(),
        }
    }

    /// What to remember for the next view.
    pub(crate) fn to_settings(self) -> festerm_config::EditorSettings {
        festerm_config::EditorSettings::new(
            self.line_numbers,
            self.fixed_columns.map(|columns| columns as u32),
            self.vi_keys,
            self.outline,
            self.syntax,
        )
    }

    /// The summary the command bar shows beside the options menu, so the
    /// current setup is readable without opening it.
    fn summary(self) -> String {
        let lines = if self.line_numbers {
            "Lines"
        } else {
            "No lines"
        };
        let width = match self.fixed_columns {
            Some(columns) => format!("{columns} columns"),
            None => "Fluid width".to_owned(),
        };
        // With vi on, the marker beside this summary says so and says which
        // mode, so repeating it here would be two answers to one question.
        if self.vi_keys {
            return format!("{lines} · {width}");
        }
        format!("{lines} · {width} · Standard keys")
    }
}

/// What the options menu is holding while it is open.
///
/// The column field is edited as text rather than as a number, because a
/// half-typed count is a normal state to pass through and the body must not
/// re-wrap to "7" on the way to "72". Only a count that parses and is at
/// least one is ever applied.
#[derive(Clone, Debug, Default)]
pub(crate) struct OptionsState {
    columns_draft: String,
    invalid: bool,
}

impl OptionsState {
    /// The menu opens showing the column count the view already has, so
    /// turning the box off and on again does not lose it.
    fn from_options(options: EditorViewOptions) -> Self {
        Self {
            columns_draft: options
                .fixed_columns
                .map(|columns| columns.to_string())
                .unwrap_or_default(),
            invalid: false,
        }
    }

    /// Reads the draft as a column count, or `None` if it cannot be applied.
    fn parsed(&self) -> Option<usize> {
        self.columns_draft
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|count| *count >= 1)
    }
}

/// How many matches one view will collect and highlight. Bounded because a
/// pattern like `.*` over a multi-megabyte file would otherwise allocate a
/// range per character (ADR 0034 §10a and §11).
const MATCH_LIMIT: usize = 2_000;

/// Why the bar cannot do what was asked, in the two lengths the row needs:
/// something short enough to sit between the fields and the navigation, and
/// the whole reason for a hover.
#[derive(Clone, Debug, Eq, PartialEq)]
struct FindError {
    headline: String,
    detail: String,
}

impl FindError {
    fn from_parts(headline: &str, detail: &str) -> Self {
        Self {
            headline: headline.to_owned(),
            detail: detail.to_owned(),
        }
    }

    /// What the hover says: the whole complaint, which is routinely a
    /// sentence and cannot go inline without displacing the navigation.
    fn full(&self) -> String {
        format!("{} — {}", self.headline, self.detail)
    }
}

/// One of the verbs the Find bar offers. Named rather than inlined so the
/// row can lay them out and an overflow menu can list them from one source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FindAction {
    Previous,
    Next,
    Replace,
    ReplaceAll,
}

impl FindAction {
    fn label(self) -> &'static str {
        match self {
            FindAction::Previous => "Previous",
            FindAction::Next => "Next",
            FindAction::Replace => "Replace",
            FindAction::ReplaceAll => "Replace All",
        }
    }

    fn accessible_label(self) -> &'static str {
        match self {
            FindAction::Previous => "Previous match",
            FindAction::Next => "Next match",
            FindAction::Replace => "Replace this match",
            FindAction::ReplaceAll => "Replace every match",
        }
    }
}

/// What Find/Replace is holding for this view. Per-view, because where one
/// window is in its search says nothing about where another window is
/// (ADR 0034 §10a).
#[derive(Clone, Debug, Default)]
pub(crate) struct FindState {
    pub(crate) open: bool,
    /// Whether the Replace field is shown. Find alone is the common case and
    /// a Replace box nobody asked for is an invitation to change text by
    /// accident.
    pub(crate) replacing: bool,
    query: String,
    replacement: String,
    /// The last results that compiled. A half-typed expression keeps these
    /// rather than clearing the highlights out from under the user.
    outcome: Option<SearchOutcome>,
    /// The pattern `outcome` was found for, so results are only recomputed
    /// when the query or the text actually changed.
    searched: Option<(String, u64)>,
    error: Option<FindError>,
    current: Option<usize>,
    focus_query: bool,
    /// A match the body should select and scroll to on the next frame. The
    /// caret belongs to the text widget, so navigation asks for it rather
    /// than setting it.
    pending_selection: Option<MatchRange>,
}

impl FindState {
    /// Opens the bar, optionally with Replace showing, and asks for the Find
    /// field to take focus on the next frame.
    fn open(&mut self, replacing: bool) {
        self.open = true;
        self.replacing = replacing;
        self.focus_query = true;
    }

    fn close(&mut self) {
        self.open = false;
        self.outcome = None;
        self.searched = None;
        self.error = None;
        self.current = None;
    }

    /// Recompiles and re-runs the search when the query or the document's
    /// revision has moved on. An invalid pattern records the error and leaves
    /// the previous results standing.
    fn refresh(&mut self, text: &str, revision: u64) {
        if self.query.is_empty() {
            self.outcome = None;
            self.searched = None;
            self.error = None;
            self.current = None;
            return;
        }
        if self
            .searched
            .as_ref()
            .is_some_and(|(query, seen)| query == &self.query && *seen == revision)
        {
            return;
        }
        match CompiledSearch::compile(&self.query) {
            Ok(search) => {
                let outcome = search.find_all(text, MATCH_LIMIT);
                self.current = outcome.matches.first().map(|_| 0);
                self.outcome = Some(outcome);
                self.error = None;
                self.searched = Some((self.query.clone(), revision));
            }
            Err(error) => {
                self.error = Some(FindError::from_parts(error.headline(), error.detail()));
                self.searched = Some((self.query.clone(), revision));
            }
        }
    }

    fn matches(&self) -> &[MatchRange] {
        self.outcome
            .as_ref()
            .map_or(&[][..], |outcome| &outcome.matches)
    }

    /// `1 of 12` or `No matches` — whichever the user needs to read to know
    /// what pressing Next will do. A pattern that will not compile is
    /// reported separately, beside a warning mark.
    fn summary(&self) -> String {
        if self.query.is_empty() {
            return String::new();
        }
        let total = self.matches().len();
        if total == 0 {
            return "No matches".to_owned();
        }
        let position = self.current.map_or(1, |index| index + 1);
        let truncated = self
            .outcome
            .as_ref()
            .is_some_and(|outcome| outcome.truncated);
        if truncated {
            format!("{position} of {total}+")
        } else {
            format!("{position} of {total}")
        }
    }

    fn step(&mut self, forward: bool) -> Option<MatchRange> {
        let total = self.matches().len();
        if total == 0 {
            return None;
        }
        let index = match (self.current, forward) {
            (Some(index), true) => (index + 1) % total,
            (Some(index), false) => (index + total - 1) % total,
            (None, true) => 0,
            (None, false) => total - 1,
        };
        self.current = Some(index);
        self.matches().get(index).cloned()
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
    options_state: OptionsState,
    options_pinned_open: bool,
    caret: (usize, usize),
    status_bar_visible: bool,
    mode: EditorMode,
    /// Built the first time a rendered mode is asked for, so a file nobody
    /// previews never pays for a parse.
    preview: Option<MarkdownPreviewPane>,
    /// The tab this view is drawn in, learned on the first frame. Ids that
    /// have to survive between two different `Ui` scopes are anchored to it.
    tab: Option<TabId>,
    /// Find/Replace, while it is open, and the results it is holding.
    find: FindState,
    /// The Compare view, while it is open. Per-view: comparing is looking,
    /// not changing, so another window goes on editing (ADR 0034 §6).
    compare: Option<ComparePane>,
    /// The `:` / `/` command area (ADR 0034 §10a).
    command: CommandArea,
    /// Where the caret is in bytes, which is what `:s` needs in order to know
    /// which line "the current line" is.
    caret_offset: usize,
    /// The vi state machine. Per-view, like the option that turns it on.
    vi: ViEngine,
    /// `ZZ` and `ZQ` are two keystrokes, so the first is held here.
    vi_prefix: Option<char>,
    /// A command a Normal-mode keystroke resolved to, dispatched on the way out
    /// of `show` where an `AppCommand` can be returned.
    vi_pending_command: Option<ViCommand>,
    /// Which way the last vi search was going, so `n` repeats it and `N`
    /// reverses it rather than both meaning "forwards".
    vi_search_backward: bool,
    /// The section both halves of a split are currently showing. Whichever
    /// pane the reader scrolls, the other is brought to this heading; holding
    /// one value for both is what keeps them from chasing each other.
    sync_heading: Option<usize>,
    /// A byte offset the text pane owes the reader a scroll to.
    pending_scroll_offset: Option<usize>,
    /// Set for one frame when the options menu changed something, so the
    /// choice can be remembered for the next view.
    options_changed: bool,
    /// The byte offset of the first character visible in the text pane.
    top_visible_offset: usize,
    /// What that offset was last frame, so the pane the reader is actually
    /// moving is the one that leads.
    last_top_offset: usize,
    /// The last rectangle the body was asked to scroll into view, so a test
    /// can assert that a caret move took the viewport with it rather than
    /// only moving the caret.
    #[cfg(test)]
    last_scroll_target: Option<egui::Rect>,
    /// Whether the body held focus at the end of the last frame. egui
    /// surrenders focus on Escape before a frame begins, so asking about focus
    /// now would make Escape the one key vi mode could never see.
    vi_focused: bool,
    /// Where the engine wants the caret, applied to the widget on the next
    /// frame because the widget owns its own cursor.
    vi_caret: Option<usize>,
    /// Whether the matches on screen came from a vi search, which keeps them
    /// lit after the command area closes the way `n` and `N` will need.
    searching_with_vi: bool,
    /// Set by `:wq`, `:x` and `ZZ`: the view closes only once the save this
    /// dispatched has actually landed, so a failed write cannot take the
    /// buffer with it.
    close_after_save: bool,
}

/// One segment of the `Edit | Preview | Split` control.
///
/// Drawn rather than reusing the ordinary toolbar button because this control
/// carries more weight than the rest of the chrome: Markdown opens in Preview,
/// so this is the only way in to editing it (ADR 0034 §4). Every segment keeps
/// a resting outline so the group reads as a control at rest, and the chosen
/// one is filled in the accent rather than distinguished by a hairline.
fn mode_segment(ui: &mut egui::Ui, label: &str, selected: bool) -> bool {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::proportional(TOOLBAR_TEXT_SIZE),
        if selected {
            theme::TEXT_ON_ACCENT
        } else {
            theme::TEXT_SECONDARY
        },
    );
    let width =
        (TOOLBAR_BUTTON_PADDING_X * 2.0 + galley.size().x).max(TOOLBAR_BUTTON_HEIGHT);
    let (rect, response) =
        ui.allocate_exact_size(vec2(width, TOOLBAR_BUTTON_HEIGHT), Sense::click());
    response.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, selected, label));

    let fill = if selected {
        theme::ACCENT_ACTION
    } else if response.hovered() {
        theme::SURFACE_TAB_ACTIVE
    } else {
        theme::SURFACE_TAB_INACTIVE
    };
    ui.painter().rect_filled(rect, TOOLBAR_BUTTON_RADIUS, fill);
    ui.painter().rect_stroke(
        rect,
        TOOLBAR_BUTTON_RADIUS,
        egui::Stroke::new(
            1.0,
            if selected {
                theme::ACCENT_ACTION
            } else {
                theme::BORDER_SUBTLE
            },
        ),
        egui::StrokeKind::Inside,
    );
    ui.painter().galley(
        egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        theme::TEXT_SECONDARY,
    );
    response
        .on_hover_text(match label {
            "Edit" => "Edit the source",
            "Preview" => "Read the rendered document",
            _ => "Source and rendering side by side",
        })
        .clicked()
}

/// Whether a file name is one the editor renders as Markdown. Taken from the
/// name rather than the content, for the same reason `language_label` is: a
/// guess from content would move the toggle under the reader as they type.
fn is_markdown_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown")
    })
}

impl TextEditorTab {
    /// A view with the built-in defaults, for tests and gallery renders that
    /// are not about what the reader last chose.
    #[cfg(test)]
    pub(crate) fn new(document: DocumentId, documents: &SharedDocuments) -> Self {
        Self::with_options(document, documents, EditorViewOptions::default())
    }

    /// A view that starts out the way the reader last left one.
    pub(crate) fn with_options(
        document: DocumentId,
        documents: &SharedDocuments,
        options: EditorViewOptions,
    ) -> Self {
        let registry = documents.borrow();
        let open = registry
            .get(document)
            .expect("an editor tab is built for a document that is open");
        let title = open.origin().file_name().to_owned();
        // Markdown is opened to be read first. One tab is one document
        // (ADR 0034 §4), so Preview is a mode this view starts in rather than
        // a second tab someone has to go and find.
        let mode = if is_markdown_name(&title) {
            EditorMode::Preview
        } else {
            EditorMode::Edit
        };
        Self {
            document,
            title,
            origin_label: open.origin().qualified_label(),
            remote: open.origin().is_remote(),
            buffer: open.text().text().to_owned(),
            options,
            options_state: OptionsState::from_options(options),
            options_pinned_open: false,
            caret: (1, 1),
            status_bar_visible: true,
            mode,
            preview: None,
            compare: None,
            command: CommandArea::default(),
            caret_offset: 0,
            close_after_save: false,
            searching_with_vi: false,
            vi: ViEngine::new(),
            vi_prefix: None,
            vi_pending_command: None,
            vi_caret: None,
            vi_search_backward: false,
            sync_heading: None,
            pending_scroll_offset: None,
            options_changed: false,
            top_visible_offset: 0,
            last_top_offset: 0,
            #[cfg(test)]
            last_scroll_target: None,
            vi_focused: false,
            tab: None,
            find: FindState::default(),
        }
    }

    /// Only Markdown is rendered, and the file name is the only honest way to
    /// decide: guessing from content would move the toggle under the user as
    /// they type.
    /// Turns the outline on for a gallery capture, the way the options menu
    /// does for a reader.
    #[cfg(test)]
    pub(crate) fn set_outline_for_gallery(&mut self, outline: bool) {
        self.options.outline = outline;
    }

    pub(crate) fn renders_markdown(&self) -> bool {
        self.language_label() == "Markdown"
    }

    #[cfg(test)]
    pub(crate) const fn mode(&self) -> EditorMode {
        self.mode
    }

    /// Points this view at a different document, which is what Save As does:
    /// the view follows the file it just wrote, while any other view of the
    /// original carries on looking at the original (ADR 0034 §3).
    ///
    /// Per-view presentation is deliberately kept — the user set up this
    /// window, not this file — but Find results are dropped, because they
    /// describe offsets into text that this view is no longer showing.
    pub(crate) fn rebind(&mut self, document: DocumentId, documents: &SharedDocuments) {
        let registry = documents.borrow();
        let Some(open) = registry.get(document) else {
            return;
        };
        self.document = document;
        self.title = open.origin().file_name().to_owned();
        self.origin_label = open.origin().qualified_label();
        self.remote = open.origin().is_remote();
        self.buffer = open.text().text().to_owned();
        self.find = FindState::default();
        self.preview = None;
        self.compare = None;
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

    /// The language the status bar names.
    ///
    /// The same detection the highlighter uses, so there is one answer to
    /// "what does fesTerm think this is" rather than two (ADR 0035 §4).
    pub(crate) fn language_label(&self) -> &'static str {
        festerm_syntax::Language::detect(&self.title, self.buffer.lines().next().unwrap_or(""))
            .map_or("Text", festerm_syntax::Language::label)
    }

    /// The language, and — when there is colour missing and a reason for it —
    /// the reason, because a silent difference between two files with the
    /// same extension is a bug report waiting to happen (ADR 0035 §6).
    pub(crate) fn status_bar_language(&self, documents: &SharedDocuments) -> String {
        let language = self.language_label();
        if !self.options.syntax {
            return language.to_owned();
        }
        let note = documents
            .borrow()
            .get(self.document)
            .and_then(|open| open.syntax_status().note());
        match note {
            Some(note) => format!("{language} · {note}"),
            None => language.to_owned(),
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

    /// The chip's state, in the shapes a document has rather than the ones a
    /// connection has: a conflict is not a failed session, and unsaved text is
    /// not a session still starting (ADR 0034 §8).
    pub(crate) fn chip_status(&self, documents: &SharedDocuments) -> ChipStatus {
        match self.status(documents).map(|status| status.severity()) {
            Some(Severity::Blocking) => ChipStatus::DocumentConflict,
            Some(Severity::Warning) => ChipStatus::DocumentUnsaved,
            Some(Severity::Informational) => {
                if self.is_dirty(documents) {
                    ChipStatus::DocumentUnsaved
                } else {
                    ChipStatus::DocumentSaved
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
        self.tab = Some(tab_id);
        self.adopt_external_edits(documents);
        self.sync_compare(documents);
        self.route_find_shortcuts(ui);
        self.route_command_area_keys(ui);
        self.route_vi_keys(ui, documents);
        // Matches are collected wherever they are shown, because the Find bar
        // and a vi search are one search behind two surfaces.
        if self.highlighting_matches() {
            let revision = documents
                .borrow()
                .get(self.document)
                .map_or(0, |open| open.text().revision());
            self.find.refresh(&self.buffer, revision);
        }

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
                    if self.find.open {
                        self.show_find_bar(ui, documents);
                        hairline(ui);
                    }
                    // The banner stays pinned above Compare: the decision it
                    // asks for is the reason Compare is open (ADR 0034 §6).
                    if let Some(action) = show_banner(ui, &status, self.compare.is_some()) {
                        match action {
                            BannerAction::Compare => self.toggle_compare(documents),
                            other => command = banner_command(other),
                        }
                    }
                    self.show_body(ui, documents);
                    // The area sits immediately above the persistent status
                    // bar and never in place of it, so mode, format, position
                    // and save state stay readable while a command is being
                    // typed (ADR 0034 §10a).
                    if self.command.reserved_height() > 0.0 {
                        hairline(ui);
                    }
                    if let Some(area_command) = self.show_command_area(ui, tab_id, documents) {
                        command = Some(area_command);
                    }
                });
            });
        if let Some(pending) = self.vi_pending_command.take() {
            command = self.dispatch_command(pending, tab_id, documents);
        }
        if let Some(pending) = self.close_when_saved(tab_id, documents) {
            command = Some(pending);
        }
        if command.is_none() && self.options_changed {
            self.options_changed = false;
            command = Some(AppCommand::SetEditorSettings(self.options.to_settings()));
        }
        command
    }

    /// The word the status bar shows, or nothing when this view is not in vi
    /// mode. It is always a word: cursor shape and colour are invisible to a
    /// screen reader and unreliable under a high-contrast theme (ADR 0034 §10).
    pub(crate) fn vi_mode_label(&self) -> Option<&'static str> {
        if !self.options.vi_keys {
            return None;
        }
        match self.command.prompt() {
            Some(CommandPrompt::Ex) => Some("COMMAND"),
            Some(_) => Some("SEARCH"),
            None => Some(self.vi.mode().label()),
        }
    }

    /// The headline and detail the vi banner shows for the state the view is
    /// actually in: a mode, a command being typed, or a search being typed.
    fn vi_status_text(&self) -> (String, String) {
        match self.command.prompt() {
            Some(CommandPrompt::Ex) => {
                let input = self.command.input();
                let detail = match crate::vi_command::parse(input) {
                    Ok(command) => format!(":{input} {}", command.explanation()),
                    Err(_) => {
                        "Enter runs the command · :w saves · :q closes · :e refreshes.".to_owned()
                    }
                };
                ("vi command-line mode".to_owned(), detail)
            }
            Some(_) => {
                let matches = self.find.matches().len();
                let detail = if self.command.input().is_empty() {
                    "Rust-compatible Unicode regex · matching as it is typed.".to_owned()
                } else if matches == 1 {
                    "Rust-compatible Unicode regex · 1 match is highlighted in the current \
                     buffer."
                        .to_owned()
                } else {
                    format!(
                        "Rust-compatible Unicode regex · {matches} matches are highlighted in the \
                         current buffer."
                    )
                };
                ("vi regex search".to_owned(), detail)
            }
            None => (
                "vi compatibility is active in this view".to_owned(),
                format!(
                    "{} mode · {} · Use :w for the ordinary Save command.",
                    self.vi.mode().label(),
                    vi_mode_hint(self.vi.mode())
                ),
            ),
        }
    }

    /// The compact marker that says this view is modal, with the sentence a
    /// banner used to carry moved into its tooltip.
    fn show_vi_marker(&self, ui: &mut egui::Ui) {
        let (headline, detail) = self.vi_status_text();
        let mode = self.vi_mode_label().unwrap_or("NORMAL");
        egui::Frame::new()
            .fill(theme::SURFACE_CARD)
            .inner_margin(egui::Margin::symmetric(6, 2))
            .corner_radius(3)
            .show(ui, |ui| {
                ui.add(egui::Label::new(
                    egui::RichText::new(format!("vi · {mode}"))
                        .size(LABEL_TEXT_SIZE)
                        .strong()
                        .color(theme::ACCENT_PRIMARY),
                ))
            })
            .inner
            .on_hover_text(format!("{headline}\n{detail}"));
    }

    /// Feeds the body's keystrokes to the vi engine.
    ///
    /// Keys are taken out of the event queue rather than merely read, because
    /// in Normal mode a letter is a command and must never also arrive in the
    /// text. vi keys are live only while the body holds focus: a toolbar
    /// control, the Find fields and the command area all keep their ordinary
    /// behaviour (ADR 0034 §10).
    fn route_vi_keys(&mut self, ui: &egui::Ui, documents: &SharedDocuments) {
        if !self.options.vi_keys || self.command.is_open() {
            return;
        }
        if !self.vi_focused && !ui.memory(|memory| memory.has_focus(self.body_id())) {
            return;
        }
        let insert = matches!(self.vi.mode(), ViMode::Insert | ViMode::Replace);
        let keys = take_vi_keys(ui, insert);
        for key in keys {
            self.feed_vi_key(key, documents);
        }
    }

    fn feed_vi_key(&mut self, key: ViKey, documents: &SharedDocuments) {
        // `ZZ` and `ZQ` are two keystrokes that mean an Ex command, so the
        // pair is resolved before the engine sees a motion in the `Z`.
        if self.vi.mode() == ViMode::Normal {
            if let ViKey::Char(character) = key {
                if let Some(prefix) = self.vi_prefix.take() {
                    if let Some(command) = normal_mode_command(&format!("{prefix}{character}")) {
                        self.vi_pending_command = Some(command);
                        return;
                    }
                } else if character == 'Z' {
                    self.vi_prefix = Some('Z');
                    return;
                }
            }
        }

        let (text, caret) = {
            let registry = documents.borrow();
            let Some(open) = registry.get(self.document) else {
                return;
            };
            (open.text().text().to_owned(), self.caret_offset)
        };
        let response = self.vi.on_key(key, &text, caret);
        match response.action {
            ViAction::None => {}
            ViAction::Edit(edits) => self.apply_vi_edits(edits, documents),
            ViAction::Undo | ViAction::Redo => {
                let undo = matches!(response.action, ViAction::Undo);
                let mut registry = documents.borrow_mut();
                if let Some(open) = registry.get_mut(self.document) {
                    let text = open.text_mut();
                    text.close_transaction();
                    if if undo { text.undo() } else { text.redo() } {
                        self.buffer = open.text().text().to_owned();
                    }
                }
            }
            ViAction::Search(intent) => {
                // A search decides where the caret lands, and the engine does
                // not know where the match was. Writing its caret back here
                // would drag the view straight back off the match it just
                // found.
                self.run_search_intent(intent, documents);
                return;
            }
            ViAction::Refused(error) => {
                self.command
                    .report(CommandOutcome::Failed(CommandError::new(
                        error.headline(),
                        error.detail(),
                    )));
            }
        }
        self.vi_caret = Some(response.caret);
        self.caret_offset = response.caret;
    }

    fn apply_vi_edits(
        &mut self,
        edits: Vec<festerm_document::TextEdit>,
        documents: &SharedDocuments,
    ) {
        if edits.is_empty() {
            return;
        }
        let mut registry = documents.borrow_mut();
        let Some(open) = registry.get_mut(self.document) else {
            return;
        };
        match open.text_mut().apply_edits(edits) {
            Ok(_) => {
                self.buffer = open.text().text().to_owned();
                self.find.searched = None;
            }
            Err(refusal) => {
                let error = CommandError::new(refusal.headline(), refusal.detail());
                drop(registry);
                self.command.report(CommandOutcome::Failed(error));
            }
        }
    }

    /// `/ ? n N * #`, all of them landing on the same search the Find bar uses.
    fn run_search_intent(&mut self, intent: SearchIntent, documents: &SharedDocuments) {
        match intent {
            SearchIntent::PromptForward => self.command.open(CommandPrompt::SearchForward),
            SearchIntent::PromptBackward => self.command.open(CommandPrompt::SearchBackward),
            SearchIntent::Next => self.step_match(if self.vi_search_backward { -1 } else { 1 }),
            SearchIntent::Previous => self.step_match(if self.vi_search_backward { 1 } else { -1 }),
            SearchIntent::WordForward | SearchIntent::WordBackward => {
                // The word is escaped as a literal, so punctuation inside an
                // identifier cannot turn into pattern syntax (ADR 0034 §10a).
                let Some(word) = word_under_caret(&self.buffer, self.caret_offset) else {
                    return;
                };
                self.find.query = festerm_document::literal_word_pattern(word);
                self.find.searched = None;
                self.searching_with_vi = true;
                let revision = documents
                    .borrow()
                    .get(self.document)
                    .map_or(0, |open| open.text().revision());
                self.find.refresh(&self.buffer, revision);
                self.step_match(if intent == SearchIntent::WordForward {
                    0
                } else {
                    -1
                });
            }
        }
    }

    /// Takes the match a search should land on: the first one past the caret
    /// going forwards, the last one before it going backwards, wrapping the
    /// way vi wraps. Without this a search would always jump to the top of the
    /// file, which is not what the reader asked for from where they were.
    fn select_match_from_caret(&mut self) {
        let caret = self.caret_offset;
        let index = if self.vi_search_backward {
            self.find
                .matches()
                .iter()
                .rposition(|found| found.start < caret)
                .or_else(|| self.find.matches().len().checked_sub(1))
        } else {
            self.find
                .matches()
                .iter()
                .position(|found| found.start > caret)
                .or(Some(0))
        };
        if let Some(index) = index {
            self.find.current = Some(index);
        }
        self.step_match(0);
    }

    /// Gives the body its focus back after the command area has had it.
    ///
    /// Without this the caret is left in a field that is no longer on screen,
    /// and the next key — `n` most of all — reaches nothing at all.
    fn return_focus_to_body(&mut self) {
        if self.options.vi_keys {
            self.vi_caret = Some(self.caret_offset);
        }
    }

    /// Moves to another match and takes the caret with it.
    fn step_match(&mut self, direction: i32) {
        let total = self.find.matches().len();
        if total == 0 {
            self.command
                .report(CommandOutcome::Message("No matches".to_owned()));
            return;
        }
        let current = self.find.current.unwrap_or(0);
        let next = match direction {
            0 => current,
            step => (current as i64 + step as i64).rem_euclid(total as i64) as usize,
        };
        self.find.current = Some(next);
        if let Some(found) = self.find.matches().get(next) {
            self.vi_caret = Some(found.start);
            self.caret_offset = found.start;
        }
        let summary = self.find.summary();
        self.command.report(CommandOutcome::Message(summary));
    }

    /// Opens the command area on `:`, `/` and `?`.
    ///
    /// The characters are consumed rather than observed so they cannot also
    /// reach the body as text, and they are only live with vi keys on: in an
    /// ordinary view a colon is a colon.
    fn route_command_area_keys(&mut self, ui: &egui::Ui) {
        if !self.options.vi_keys || self.command.is_open() {
            return;
        }
        let opened = ui.input_mut(|input| {
            let mut opened = None;
            input.events.retain(|event| {
                let egui::Event::Text(text) = event else {
                    return true;
                };
                match text.as_str() {
                    ":" => opened = Some(CommandPrompt::Ex),
                    "/" => opened = Some(CommandPrompt::SearchForward),
                    "?" => opened = Some(CommandPrompt::SearchBackward),
                    _ => return true,
                }
                false
            });
            opened
        });
        if let Some(prompt) = opened {
            self.command.open(prompt);
        }
    }

    fn show_command_area(
        &mut self,
        ui: &mut egui::Ui,
        tab_id: TabId,
        documents: &SharedDocuments,
    ) -> Option<AppCommand> {
        let summary = self
            .command
            .prompt()
            .filter(|prompt| *prompt != CommandPrompt::Ex)
            .map(|_| self.find.summary());
        let event = self.command.show(ui, tab_id, summary.as_deref())?;
        match event {
            CommandAreaEvent::Changed => {
                self.preview_search(documents);
                None
            }
            CommandAreaEvent::Cancelled => {
                // An abandoned search leaves nothing lit: highlights that
                // outlived the command that made them would claim the editor
                // was still looking for something.
                if !self.find.open {
                    self.find.query.clear();
                    self.find.searched = None;
                    self.searching_with_vi = false;
                }
                self.return_focus_to_body();
                None
            }
            CommandAreaEvent::Run { prompt, line } => {
                let command = self.run_command_line(prompt, &line, tab_id, documents);
                self.return_focus_to_body();
                command
            }
        }
    }

    /// Whether the body should wash its matches, which is true for the Find bar
    /// and for a vi search alike: they are one search, shown two ways.
    fn highlighting_matches(&self) -> bool {
        self.find.open
            || self
                .command
                .prompt()
                .is_some_and(|prompt| prompt != CommandPrompt::Ex)
            || (!self.find.query.is_empty() && self.searching_with_vi)
    }

    /// A search prompt matches as it is typed, so the highlights and the count
    /// describe what Enter is about to accept rather than the last thing run.
    fn preview_search(&mut self, documents: &SharedDocuments) {
        let Some(prompt) = self.command.prompt() else {
            return;
        };
        if prompt == CommandPrompt::Ex {
            return;
        }
        // The Find bar deliberately stays shut: a vi search is not a second
        // copy of the toolbar, and opening one would move the text down the
        // moment a `/` was typed.
        self.find.query = self.command.input().to_owned();
        let revision = documents
            .borrow()
            .get(self.document)
            .map_or(0, |open| open.text().revision());
        self.find.refresh(&self.buffer, revision);
    }

    fn run_command_line(
        &mut self,
        prompt: CommandPrompt,
        line: &str,
        tab_id: TabId,
        documents: &SharedDocuments,
    ) -> Option<AppCommand> {
        if prompt != CommandPrompt::Ex {
            self.preview_search(documents);
            self.searching_with_vi = true;
            self.vi_search_backward = prompt == CommandPrompt::SearchBackward;
            self.select_match_from_caret();
            let summary = self.find.summary();
            self.command.finish(CommandOutcome::Message(summary));
            return None;
        }
        match parse_command(line) {
            Err(error) => {
                self.command.finish(CommandOutcome::Failed(error));
                None
            }
            Ok(command) => self.dispatch_command(command, tab_id, documents),
        }
    }

    fn dispatch_command(
        &mut self,
        command: ViCommand,
        tab_id: TabId,
        documents: &SharedDocuments,
    ) -> Option<AppCommand> {
        match command {
            ViCommand::Write => {
                // The write itself reports through the document's own status,
                // which every view already shows; claiming success here would
                // be guessing at a result this view has not seen.
                self.command
                    .finish(CommandOutcome::Message("Saving…".to_owned()));
                Some(AppCommand::SaveTextDocument)
            }
            ViCommand::WriteAs { .. } => {
                // A named destination still goes through the reviewed picker:
                // ADR 0034 §10 forbids `:w {path}` from overwriting silently.
                self.command
                    .finish(CommandOutcome::Message("Choose where to save.".to_owned()));
                Some(AppCommand::SaveTextDocumentAs)
            }
            ViCommand::WriteQuit => {
                self.close_after_save = true;
                self.command
                    .finish(CommandOutcome::Message("Saving…".to_owned()));
                Some(AppCommand::SaveTextDocument)
            }
            ViCommand::Quit => {
                // The ordinary close, with its ordinary dirty-close prompt.
                self.command.close();
                Some(AppCommand::CloseTab(tab_id))
            }
            ViCommand::QuitDiscarding => {
                // `:q!` means it. Asking again turns the exclamation mark into
                // decoration, and a reader who typed it has already said what
                // a dialog would ask (ADR 0034 §10).
                self.command.close();
                Some(AppCommand::DiscardAndCloseTab(tab_id))
            }
            ViCommand::Refresh { .. } => {
                self.command.close();
                Some(AppCommand::RefreshTextDocument)
            }
            ViCommand::Substitute(command) => {
                self.run_substitution(*command, documents);
                None
            }
            ViCommand::GoToLine { line } => {
                self.go_to_line(line);
                None
            }
        }
    }

    /// `:wq` closes only once the save it asked for has actually landed.
    fn close_when_saved(
        &mut self,
        tab_id: TabId,
        documents: &SharedDocuments,
    ) -> Option<AppCommand> {
        if !self.close_after_save {
            return None;
        }
        let registry = documents.borrow();
        let open = registry.get(self.document)?;
        if open.text().is_dirty() || open.conflict().is_some() {
            // The write did not land, so the request is dropped rather than
            // held: closing later, on a save the user did not connect to it,
            // would be worse than making them ask again.
            if open.conflict().is_some() {
                self.close_after_save = false;
            }
            return None;
        }
        self.close_after_save = false;
        Some(AppCommand::CloseTab(tab_id))
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

    /// Opens Find (and Replace, when a replacement is given) with a query
    /// already in it, so the gallery can show the bar in the state it spends
    /// its life in rather than empty.
    #[cfg(test)]
    /// Opens the command area with a line already typed, so the gallery can
    /// show it doing its job rather than empty.
    /// Turns vi compatibility on for a gallery render, the way the options
    /// menu does for a reader.
    #[cfg(test)]
    pub(crate) fn enable_vi_for_gallery(&mut self) {
        self.options.vi_keys = true;
        // A capture of NORMAL mode that does not show the block caret is a
        // capture of the claim rather than of the thing, so the body is given
        // its focus and its caret is put on a glyph in the prose.
        let offset = self
            .buffer
            .find("message-relay")
            .unwrap_or(0)
            .min(self.buffer.len());
        self.caret_offset = offset;
        self.vi_caret = Some(offset);
    }

    #[cfg(test)]
    pub(crate) fn open_command_area_for_gallery(&mut self, prompt: CommandPrompt, line: &str) {
        self.options.vi_keys = true;
        self.command.open_for_gallery(prompt, line);
        if prompt != CommandPrompt::Ex {
            self.find.query = line.to_owned();
        }
    }

    #[cfg(test)]
    pub(crate) fn open_find_for_gallery(&mut self, query: &str, replacement: Option<&str>) {
        self.find.open(replacement.is_some());
        self.find.query = query.to_owned();
        if let Some(replacement) = replacement {
            self.find.replacement = replacement.to_owned();
        }
    }

    /// Holds the options menu open so the gallery can photograph it. A popup
    /// is otherwise opened by a click, which a headless capture has no way to
    /// perform and then hold across the frame it renders.
    #[cfg(test)]
    pub(crate) fn open_options_for_gallery(&mut self, fixed_columns: Option<usize>) {
        self.options_pinned_open = true;
        self.options.fixed_columns = fixed_columns;
        if let Some(columns) = fixed_columns {
            self.options_state.columns_draft = columns.to_string();
        }
    }

    #[cfg(test)]
    pub(crate) fn options_for_test(&self) -> EditorViewOptions {
        self.options
    }

    #[cfg(test)]
    pub(crate) fn find_error_for_test(&self) -> Option<String> {
        self.find.error.as_ref().map(FindError::full)
    }

    #[cfg(test)]
    pub(crate) fn find_match_count_for_test(&self) -> usize {
        self.find.matches().len()
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
                if mode_segment(ui, mode.label(), selected) && !selected {
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
                    // Save As is always offered, deliberately: it is the way
                    // out of a conflict, an unavailable source, or lost
                    // permissions, which are exactly the states in which Save
                    // itself is disabled (ADR 0034 §3).
                    if toolbar_button(ui, None, "Save As\u{2026}", "Save As", false) {
                        command = Some(AppCommand::SaveTextDocumentAs);
                    }
                    if toolbar_button(ui, None, "Find", "Find", false) {
                        self.find.open(false);
                    }
                    if toolbar_button(ui, None, "Replace", "Find and replace", false) {
                        self.find.open(true);
                    }
                    ui.add_space(TOOLBAR_GROUP_GAP);
                    ui.separator();
                    ui.add_space(TOOLBAR_GROUP_GAP);
                    if toolbar_button(
                        ui,
                        None,
                        "Duplicate view",
                        "Open another view of this document",
                        false,
                    ) {
                        command = Some(AppCommand::OpenAnotherEditorView);
                    }
                    if toolbar_button(ui, Some(Icon::Refresh), "Refresh", "Refresh", false) {
                        command = Some(AppCommand::RefreshTextDocument);
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        self.show_options_menu(ui);
                        label(ui, &self.options.summary(), theme::TEXT_MUTED, false);
                        // vi mode is marked rather than explained: the status
                        // bar already names the mode, and a paragraph above
                        // the text is a paragraph a reader has to scroll past
                        // every time they look at the file.
                        if self.options.vi_keys {
                            self.show_vi_marker(ui);
                        }
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

    /// The Find/Replace bar, below the command bar and above the banner: it is
    /// Stable ids for the Find bar's two fields, anchored to the tab rather
    /// than to whichever nested `Ui` happens to be building them, so undo
    /// routing in the body can ask whether one of them holds focus.
    /// Anchored to the tab rather than to a nested `Ui`, so key routing that
    /// happens before the body is built can still ask whether it holds focus.
    fn body_id(&self) -> egui::Id {
        egui::Id::new(("text-editor-body", self.tab))
    }

    fn find_field_id(&self, index: usize) -> egui::Id {
        egui::Id::new(("text-editor-find-field", self.tab, index))
    }

    fn show_find_bar(&mut self, ui: &mut egui::Ui, documents: &SharedDocuments) {
        let (query_id, replacement_id) = (self.find_field_id(0), self.find_field_id(1));

        egui::Frame::new()
            .fill(theme::SURFACE_PANEL)
            .inner_margin(egui::Margin::symmetric(BAR_PADDING_X, BAR_PADDING_Y))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_height(TOOLBAR_BUTTON_HEIGHT);
                    ui.spacing_mut().item_spacing.x = TOOLBAR_BUTTON_GAP;
                    let find_label = label(ui, "Find", theme::TEXT_MUTED, false);
                    let query = ui
                        .add(
                            egui::TextEdit::singleline(&mut self.find.query)
                                .id(query_id)
                                .desired_width(FIND_FIELD_WIDTH)
                                .hint_text("Pattern"),
                        )
                        .labelled_by(find_label.id);
                    if std::mem::take(&mut self.find.focus_query) {
                        query.request_focus();
                    }
                    if self.find.replacing {
                        let replace_label = label(ui, "Replace", theme::TEXT_MUTED, false);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.find.replacement)
                                .id(replacement_id)
                                .desired_width(FIND_FIELD_WIDTH)
                                .hint_text("Replacement"),
                        )
                        .labelled_by(replace_label.id);
                    }

                    // The counter reads as the navigation's position, so it
                    // is spaced away from the fields and kept at a fixed
                    // width: a row that jitters sideways between "No matches"
                    // and "1 of 6" moves Next out from under the pointer.
                    ui.add_space(TOOLBAR_GROUP_GAP);
                    self.show_find_summary(ui);

                    let has_matches = !self.find.matches().is_empty();
                    // Disabled, not merely inert: a Next with nothing to go to
                    // has to say so rather than quietly doing nothing.
                    if self.find_actions_fit(ui) {
                        ui.add_enabled_ui(has_matches, |ui| {
                            for action in self.find_actions() {
                                if toolbar_button(
                                    ui,
                                    None,
                                    action.label(),
                                    action.accessible_label(),
                                    false,
                                ) {
                                    self.run_find_action(action, documents);
                                }
                            }
                        });
                    } else {
                        // Too narrow for the verbs, so they collapse into one
                        // menu rather than being clipped off the edge where
                        // they cannot be reached at all (ADR 0034 §9).
                        let overflow = toolbar_button_response(
                            ui,
                            Some(Icon::Overflow),
                            "",
                            "Find actions",
                            false,
                        );
                        let mut chosen = None;
                        egui::Popup::menu(&overflow).show(|ui| {
                            for action in self.find_actions() {
                                if ui
                                    .add_enabled(has_matches, egui::Button::new(action.label()))
                                    .clicked()
                                {
                                    chosen = Some(action);
                                    ui.close();
                                }
                            }
                        });
                        if let Some(action) = chosen {
                            self.run_find_action(action, documents);
                        }
                    }
                    // Close stays beside what it closes rather than being
                    // pushed to the far edge, where a lone glyph across a gap
                    // reads as some other control entirely.
                    ui.add_space(TOOLBAR_GROUP_GAP);
                    if toolbar_button(ui, None, "×", "Close Find", false) {
                        self.find.close();
                    }
                });
            });
    }

    /// The verbs the bar offers, in the order they are laid out. Replace only
    /// appears once a replacement can be typed.
    fn find_actions(&self) -> Vec<FindAction> {
        let mut actions = vec![FindAction::Previous, FindAction::Next];
        if self.find.replacing {
            actions.push(FindAction::Replace);
            actions.push(FindAction::ReplaceAll);
        }
        actions
    }

    fn run_find_action(&mut self, action: FindAction, documents: &SharedDocuments) {
        match action {
            FindAction::Previous => self.find.pending_selection = self.find.step(false),
            FindAction::Next => self.find.pending_selection = self.find.step(true),
            FindAction::Replace => self.replace_current(documents),
            FindAction::ReplaceAll => self.replace_all(documents),
        }
    }

    /// Whether the verbs and the close control still fit on the row. Measured
    /// rather than assumed, because the widths come from the font the user is
    /// actually running.
    fn find_actions_fit(&self, ui: &egui::Ui) -> bool {
        let actions = self.find_actions();
        let buttons: f32 = actions
            .iter()
            .map(|action| toolbar_button_width(ui, None, action.label()))
            .sum();
        let gaps = TOOLBAR_BUTTON_GAP * actions.len() as f32 + TOOLBAR_GROUP_GAP;
        let close = toolbar_button_width(ui, None, "\u{d7}");
        ui.available_width() >= buttons + gaps + close
    }

    /// The match counter, or a short note that the pattern will not compile.
    ///
    /// The error is kept to two words with the detail on hover: spelling a
    /// regex complaint out inline would push the navigation off the row, and
    /// it carries a warning mark so the state does not rest on colour alone.
    fn show_find_summary(&self, ui: &mut egui::Ui) {
        let width = FIND_SUMMARY_WIDTH;
        ui.allocate_ui_with_layout(
            vec2(width, TOOLBAR_BUTTON_HEIGHT),
            // Right-aligned inside its reserved width, so it sits against the
            // navigation it describes rather than drifting back towards the
            // fields as the count shortens.
            egui::Layout::right_to_left(Align::Center),
            |ui| {
                ui.set_width(width);
                match &self.find.error {
                    Some(error) => {
                        label(ui, "Invalid pattern", theme::STATUS_ERROR, false)
                            .on_hover_text(error.full());
                        let (rect, _) = ui.allocate_exact_size(
                            egui::Vec2::splat(FIND_WARNING_SIZE),
                            Sense::hover(),
                        );
                        icon::paint(ui.painter(), Icon::Warning, rect, theme::STATUS_ERROR);
                    }
                    None => {
                        label(ui, &self.find.summary(), theme::TEXT_MUTED, false);
                    }
                }
            },
        );
    }

    /// Replaces the match the counter is pointing at, through the same engine
    /// `:s` uses, so the Replace field's dialect is the editor's one dialect.
    fn replace_current(&mut self, documents: &SharedDocuments) {
        let Some(index) = self.find.current else {
            return;
        };
        let Some(found) = self.find.matches().get(index).cloned() else {
            return;
        };
        self.substitute(documents, Some(found));
    }

    fn replace_all(&mut self, documents: &SharedDocuments) {
        self.substitute(documents, None);
    }

    /// One substitution, committed as one undo transaction. `only` restricts it
    /// to a single match by planning over the whole document and keeping the
    /// edit that lands on it, so a single Replace and a Replace All cannot
    /// disagree about what the pattern means.
    fn substitute(&mut self, documents: &SharedDocuments, only: Option<MatchRange>) {
        let flags = SubstituteFlags {
            global: true,
            ..SubstituteFlags::default()
        };
        let command = match SubstituteCommand::from_parts(
            &self.find.query,
            &self.find.replacement,
            SubstituteRange::WholeDocument,
            flags,
        ) {
            Ok(command) => command,
            Err(error) => {
                self.find.error = Some(FindError::from_parts(error.headline(), error.detail()));
                return;
            }
        };
        let plan = match command.plan(&self.buffer, 0..0, None, MATCH_LIMIT) {
            Ok(plan) => plan,
            Err(error) => {
                self.find.error = Some(FindError::from_parts(error.headline(), error.detail()));
                return;
            }
        };
        let edits = match plan.to_text_edits(&self.buffer) {
            Ok(edits) => edits,
            Err(error) => {
                self.find.error = Some(FindError::from_parts(error.headline(), error.detail()));
                return;
            }
        };
        let edits: Vec<_> = match &only {
            Some(found) => edits
                .into_iter()
                .filter(|edit| edit.start == found.start)
                .collect(),
            None => edits,
        };
        if edits.is_empty() {
            return;
        }

        let mut registry = documents.borrow_mut();
        let Some(open) = registry.get_mut(self.document) else {
            return;
        };
        match open.text_mut().apply_edits(edits) {
            Ok(_) => {
                self.buffer = open.text().text().to_owned();
                self.find.error = None;
                // The text moved, so the results describe a document that no
                // longer exists; the next frame recollects them.
                self.find.searched = None;
            }
            Err(refusal) => {
                self.find.error =
                    Some(FindError::from_parts(refusal.headline(), &refusal.detail()));
            }
        }
    }

    /// A `:s` from the command area, committed through exactly the same path
    /// as Replace All so the two cannot disagree about what a pattern means.
    fn run_substitution(&mut self, command: SubstituteCommand, documents: &SharedDocuments) {
        let line = self.current_line_range();
        let plan = command.plan(&self.buffer, line, None, MATCH_LIMIT);
        let edits = plan.and_then(|plan| {
            let count = plan.replacement_count();
            plan.to_text_edits(&self.buffer).map(|edits| (count, edits))
        });
        let (count, edits) = match edits {
            Ok(planned) => planned,
            Err(error) => {
                self.command.finish(CommandOutcome::Failed(
                    crate::vi_command::CommandError::new(error.headline(), error.detail()),
                ));
                return;
            }
        };
        if command.flags.count_only || edits.is_empty() {
            self.command
                .finish(CommandOutcome::Message(match_count_message(count)));
            return;
        }
        let mut registry = documents.borrow_mut();
        let Some(open) = registry.get_mut(self.document) else {
            return;
        };
        match open.text_mut().apply_edits(edits) {
            Ok(_) => {
                self.buffer = open.text().text().to_owned();
                self.find.searched = None;
                drop(registry);
                self.command
                    .finish(CommandOutcome::Message(match_count_message(count)));
            }
            Err(refusal) => {
                let error =
                    crate::vi_command::CommandError::new(refusal.headline(), refusal.detail());
                drop(registry);
                self.command.finish(CommandOutcome::Failed(error));
            }
        }
    }

    /// The byte range of the line the caret is on, which is what `:s` without a
    /// range addresses.
    /// Puts the caret at the start of a line and reports where it landed.
    ///
    /// A number past the end of the file is the last line rather than a
    /// refusal: the reader asked to go as far as that, and vi has always taken
    /// them as far as there is.
    fn go_to_line(&mut self, line: crate::vi_command::GoToLine) {
        let starts = std::iter::once(0)
            .chain(
                self.buffer
                    .char_indices()
                    .filter(|(_, character)| *character == '\n')
                    .map(|(index, _)| index + 1),
            )
            .filter(|start| *start < self.buffer.len() || *start == 0)
            .collect::<Vec<_>>();
        let index = match line {
            crate::vi_command::GoToLine::Last => starts.len().saturating_sub(1),
            crate::vi_command::GoToLine::Number(number) => {
                (number.saturating_sub(1)).min(starts.len().saturating_sub(1))
            }
        };
        let offset = starts.get(index).copied().unwrap_or(0);
        self.caret_offset = offset;
        self.vi_caret = Some(offset);
        self.command
            .finish(CommandOutcome::Message(format!("Line {}", index + 1)));
    }

    fn current_line_range(&self) -> std::ops::Range<usize> {
        let offset = self.caret_offset.min(self.buffer.len());
        let start = self.buffer[..offset]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let end = self.buffer[offset..]
            .find('\n')
            .map_or(self.buffer.len(), |index| offset + index);
        start..end
    }

    /// Cmd+F and Cmd+Alt+F, taken before the body sees them so they cannot
    /// reach the text as characters, and Esc while the bar is open.
    fn route_find_shortcuts(&mut self, ui: &mut egui::Ui) {
        let (replace, find, escape) = ui.input_mut(|input| {
            // The wider combination has to be offered the key first: egui
            // matches modifiers logically, so a plain COMMAND test would
            // swallow Cmd+Alt+F and open Find without the Replace field.
            let replace = input.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::ALT,
                egui::Key::F,
            );
            let find = input.consume_key(egui::Modifiers::COMMAND, egui::Key::F);
            let escape =
                self.find.open && input.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
            (replace, find, escape)
        });
        if replace {
            self.find.open(true);
        } else if find {
            self.find.open(false);
        }
        if escape {
            self.find.close();
        }
    }

    fn show_body(&mut self, ui: &mut egui::Ui, documents: &SharedDocuments) {
        let footer = if self.status_bar_visible { 0.0 } else { 24.0 };
        // The command area is given its height before the body takes the rest,
        // because it belongs above the status bar rather than wherever the
        // text happens to end (ADR 0034 §10a).
        let command_area = self.command.reserved_height();
        let separator = if command_area > 0.0 { 1.0 } else { 0.0 };
        let height = (ui.available_height() - footer - command_area - separator).max(120.0);
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
        let outline = self.renders_markdown() && self.options.outline;
        ui.allocate_ui(vec2(ui.available_width(), height), |ui| {
            ui.set_height(height);
            if outline {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.set_height(height);
                    self.show_outline_rail(ui, height);
                    ui.allocate_ui(vec2(ui.available_width(), height), |ui| {
                        ui.set_height(height);
                        self.show_panes(ui, documents, mode, height);
                    });
                });
                return;
            }
            self.show_panes(ui, documents, mode, height);
        });
    }

    /// The optional outline beside the text (ADR 0034 §9).
    ///
    /// Editing a long Markdown file without one means scrolling to find out
    /// where you are; the viewer has had this rail since it shipped and the
    /// editor is looking at the same headings. Clicking one puts the caret at
    /// the start of that section and takes the viewport with it.
    fn show_outline_rail(&mut self, ui: &mut egui::Ui, height: f32) {
        let pane = self.preview.get_or_insert_with(|| {
            MarkdownPreviewPane::new(preview_source(&self.origin_label), &self.buffer)
        });
        pane.sync(ui.ctx(), &self.buffer);
        let headings = pane.headings().to_vec();
        let selected = self
            .sync_heading
            .or_else(|| pane.heading_at_byte(self.caret_offset));
        let clicked = crate::markdown_viewer::show_markdown_outline(
            ui,
            &headings,
            selected,
            height,
            "text-editor-outline",
        );
        if let Some(index) = clicked {
            if let Some(offset) = self
                .preview
                .as_ref()
                .and_then(|pane| pane.heading_byte(index))
            {
                self.caret_offset = offset;
                self.vi_caret = Some(offset);
                self.sync_heading = Some(index);
                if let Some(pane) = self.preview.as_mut() {
                    pane.scroll_to_heading(index);
                }
            }
        }
    }

    fn show_panes(
        &mut self,
        ui: &mut egui::Ui,
        documents: &SharedDocuments,
        mode: EditorMode,
        height: f32,
    ) {
        {
            match mode {
                EditorMode::Edit => self.show_edit_pane(ui, documents, ui.available_width()),
                EditorMode::Preview => self.show_preview_pane(ui, height),
                EditorMode::Split => {
                    let pane_width =
                        ((ui.available_width() - SPLIT_DIVIDER_WIDTH) / 2.0).max(120.0);
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.allocate_ui(vec2(pane_width, height), |ui| {
                            ui.set_height(height);
                            self.show_edit_pane(ui, documents, pane_width);
                        });
                        let (divider, _) = ui
                            .allocate_exact_size(vec2(SPLIT_DIVIDER_WIDTH, height), Sense::hover());
                        ui.painter().line_segment(
                            [divider.center_top(), divider.center_bottom()],
                            egui::Stroke::new(1.0, theme::BORDER_SUBTLE),
                        );
                        ui.allocate_ui(vec2(ui.available_width(), height), |ui| {
                            ui.set_height(height);
                            self.show_preview_pane(ui, height);
                        });
                    });
                    self.sync_split_scroll();
                }
            }
        }
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

    /// Keeps the two halves of a split looking at the same section.
    ///
    /// Reading and editing the same document side by side is only useful while
    /// the two sides agree about where they are; a preview that stays at the
    /// top while the text is three sections down is a second document. The
    /// section is the unit because it is the one both panes can name: a line
    /// of source has no height in the rendering, but a heading has a place in
    /// both.
    fn sync_split_scroll(&mut self) {
        let top_offset = self.top_visible_offset;
        // Whichever pane moved this frame is the one being read; without that
        // test the two panes pull against each other at every section
        // boundary and the scroll stops dead.
        let text_moved = top_offset != self.last_top_offset;
        self.last_top_offset = top_offset;
        let previous = self.sync_heading;
        let Some(pane) = self.preview.as_mut() else {
            return;
        };
        if text_moved {
            if let Some(index) = pane.heading_at_byte(top_offset) {
                if Some(index) != previous {
                    pane.scroll_to_heading(index);
                    self.sync_heading = Some(index);
                }
            }
            return;
        }
        if let Some(index) = pane.visible_heading() {
            if Some(index) != previous {
                self.pending_scroll_offset = pane.heading_byte(index);
                self.sync_heading = Some(index);
            }
        }
    }

    /// Paints the block caret Normal and Visual mode are owed.
    ///
    /// A modal editor whose caret looks the same in both modes is asking the
    /// reader to remember which one they are in; the shape is the one signal
    /// that is always where they are already looking. Insert and Replace keep
    /// the widget's own bar, which is the one the platform draws.
    fn paint_mode_caret(
        &self,
        ui: &egui::Ui,
        output: &egui::text_edit::TextEditOutput,
        index: usize,
    ) {
        if !self.options.vi_keys || !output.response.has_focus() {
            return;
        }
        if !matches!(
            self.vi.mode(),
            ViMode::Normal | ViMode::Visual | ViMode::VisualLine
        ) {
            return;
        }
        let offset = output.galley_pos.to_vec2();
        let here = output
            .galley
            .pos_from_cursor(egui::text::CCursor::new(index))
            .translate(offset);
        // As wide as the character it covers, which for a monospaced body is
        // the width of the next caret position along.
        let next = output
            .galley
            .pos_from_cursor(egui::text::CCursor::new(index + 1))
            .translate(offset);
        let width = if next.left() > here.left() {
            next.left() - here.left()
        } else {
            ui.painter()
                .layout_no_wrap(
                    "0".to_owned(),
                    FontId::monospace(EDITOR_TEXT_SIZE),
                    theme::TEXT_PRIMARY,
                )
                .size()
                .x
        };
        let block = egui::Rect::from_min_size(here.min, vec2(width, here.height()));
        ui.painter()
            .rect_filled(block, 1.0, theme::ACCENT_PRIMARY.gamma_multiply(0.45));
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
        read_only: bool,
    ) {
        // Undo belongs to the whole editor, not only to the body: a user who
        // has just pressed Replace All, or who has just ticked something in
        // the options menu, is holding a control rather than the text, and
        // Cmd+Z has to take the last edit back all the same. The only places
        // it must not be intercepted are the editor's own small text fields,
        // where it means "undo what I typed into this box".
        let in_small_field = ui.memory(|memory| {
            (self.find.open && (0..2).any(|index| memory.has_focus(self.find_field_id(index))))
                || memory.has_focus(self.options_field_id())
        });
        if read_only || in_small_field {
            return;
        }
        let (undo, redo) = ui.ctx().input_mut(|input| {
            // Redo first: `consume_key` ignores an extra Shift, so asking for
            // Cmd+Z first would swallow Cmd+Shift+Z as an undo.
            let redo = input.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Z,
            ) | input.consume_key(egui::Modifiers::COMMAND, egui::Key::Y);
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

    /// The options menu, anchored to the right-hand end of the command bar.
    ///
    /// Everything in here is per-view (ADR 0034 §9): two windows on one file
    /// may be set up differently without disagreeing about the text, so none
    /// of these choices touches the document or its undo history.
    fn show_options_menu(&mut self, ui: &mut egui::Ui) {
        let button = toolbar_button_with_trailing(
            ui,
            None,
            Some(Icon::Disclosure),
            "Editor options",
            "Editor options",
            false,
        );
        let mut popup = egui::Popup::menu(&button)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        if self.options_pinned_open {
            popup = popup.open_memory(Some(egui::SetOpenCommand::Bool(true)));
        }
        let before = self.options;
        popup.show(|ui| {
            ui.set_min_width(OPTIONS_MENU_WIDTH);
            ui.set_max_width(OPTIONS_MENU_WIDTH);
            label(ui, "Editor options", theme::TEXT_PRIMARY, true);
            ui.add_space(OPTIONS_MENU_GAP);
            ui.checkbox(&mut self.options.line_numbers, "Show line numbers");
            ui.add_space(OPTIONS_MENU_GAP);
            self.show_fixed_columns_control(ui);
            // The caption belongs under the control it explains: with other
            // rows between them it reads as the description of whichever
            // checkbox happens to sit above it.
            let explanation = if self.options_state.invalid {
                "A fixed column count must be a whole number of at least one."
            } else {
                "Fixed columns wrap visually at the chosen boundary. They never insert line breaks."
            };
            let color = if self.options_state.invalid {
                theme::STATUS_ERROR
            } else {
                theme::TEXT_MUTED
            };
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            ui.colored_label(
                color,
                egui::RichText::new(explanation).size(LABEL_TEXT_SIZE),
            );
            ui.add_space(OPTIONS_MENU_GAP);
            ui.checkbox(&mut self.options.vi_keys, "vi compatibility");
            ui.add_space(OPTIONS_MENU_GAP);
            ui.checkbox(&mut self.options.syntax, "Syntax highlighting");
            if self.renders_markdown() {
                ui.add_space(OPTIONS_MENU_GAP);
                ui.checkbox(&mut self.options.outline, "Show outline");
            }
        });
        if self.options != before {
            // The way a reader likes to work does not change between one file
            // and the next, so the answer outlives the view that was asked.
            self.options_changed = true;
        }
    }

    /// The "Fixed column count" row: a box that turns wrapping at a column on
    /// and off, and the count it wraps at.
    fn show_fixed_columns_control(&mut self, ui: &mut egui::Ui) {
        let field_id = self.options_field_id();
        ui.horizontal(|ui| {
            let mut enabled = self.options.fixed_columns.is_some();
            if ui.checkbox(&mut enabled, "Fixed column count").changed() {
                if enabled {
                    if self.options_state.columns_draft.trim().is_empty() {
                        self.options_state.columns_draft = DEFAULT_FIXED_COLUMNS.to_string();
                    }
                    self.apply_fixed_columns();
                } else {
                    // Turning it off is never refused, and the draft is kept
                    // so ticking the box again comes back to the same count.
                    self.options.fixed_columns = None;
                    self.options_state.invalid = false;
                }
            }
            ui.add_enabled_ui(enabled, |ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.options_state.columns_draft)
                        .id(field_id)
                        .desired_width(OPTIONS_FIELD_WIDTH)
                        .hint_text("72"),
                );
                if field.changed() {
                    self.apply_fixed_columns();
                }
            });
        });
    }

    /// Takes the draft count if it is usable. A count that does not parse, or
    /// one below one, leaves the view wrapping where it already was: an
    /// unusable number is not a new setting, it is a number still being typed.
    fn apply_fixed_columns(&mut self) {
        match self.options_state.parsed() {
            Some(columns) => {
                self.options.fixed_columns = Some(columns);
                self.options_state.invalid = false;
            }
            None => self.options_state.invalid = true,
        }
    }

    fn options_field_id(&self) -> egui::Id {
        egui::Id::new(("text-editor-columns-field", self.tab))
    }

    /// The width the body should lay out at, in points.
    ///
    /// A fixed column count is a wrap boundary, not a hard limit on the
    /// window: if the view is too narrow to show that many columns, the text
    /// wraps at the width there is rather than being clipped.
    fn body_width(&self, ui: &egui::Ui, available: f32) -> f32 {
        let Some(columns) = self.options.fixed_columns else {
            return available;
        };
        let column_width = ui
            .painter()
            .layout_no_wrap(
                "0".to_owned(),
                FontId::monospace(EDITOR_TEXT_SIZE),
                theme::TEXT_PRIMARY,
            )
            .size()
            .x;
        // The body's own horizontal margin is inside the width handed to the
        // widget, so the requested columns only fit if it is added back.
        (column_width * columns as f32 + BODY_MARGIN_X * 2.0).min(available)
    }

    fn show_text(&mut self, ui: &mut egui::Ui, documents: &SharedDocuments) {
        let read_only = documents
            .borrow()
            .get(self.document)
            .is_some_and(|open| open.read_only());
        let gutter = self.gutter_width(ui);
        let left = ui.min_rect().left();
        ui.add_space(gutter);
        let width = self.body_width(ui, ui.available_width());
        let body_id = self.body_id();
        self.route_undo_shortcuts(ui, documents, read_only);
        // Taken out of `self` before the widget borrows the buffer mutably.
        let highlights: Vec<(usize, usize, bool)> = if self.highlighting_matches() {
            let current = self.find.current;
            self.find
                .matches()
                .iter()
                .enumerate()
                .filter(|(_, found)| !found.is_empty())
                .map(|(index, found)| (found.start, found.end, Some(index) == current))
                .collect()
        } else {
            Vec::new()
        };
        // Only what is on screen is coloured (ADR 0035 §2): the range the
        // reader can see, plus a margin so a small scroll does not arrive
        // ahead of its colour.
        let spans: Vec<festerm_syntax::Span> = if self.options.syntax {
            let range = visible_byte_range(&self.buffer, self.top_visible_offset, ui);
            let mut registry = documents.borrow_mut();
            registry
                .get_mut(self.document)
                .map(|open| open.syntax_spans(range).to_vec())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
            let mut job = editor_layout_job(text.as_str(), &highlights, &spans);
            job.wrap.max_width = wrap_width;
            ui.ctx().fonts_mut(|fonts| fonts.layout_job(job))
        };
        let output = egui::TextEdit::multiline(&mut self.buffer)
            .id(body_id)
            .font(FontId::monospace(EDITOR_TEXT_SIZE))
            .desired_width(width)
            .desired_rows(1)
            .interactive(!read_only)
            .margin(egui::Margin::symmetric(BODY_MARGIN_X as i8, 8))
            .layouter(&mut layouter)
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

        if let Some(found) = self.find.pending_selection.take() {
            let start = self.buffer[..found.start.min(self.buffer.len())]
                .chars()
                .count();
            let end = self.buffer[..found.end.min(self.buffer.len())]
                .chars()
                .count();
            let mut state = output.state.clone();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(start),
                    egui::text::CCursor::new(end),
                )));
            state.store(ui.ctx(), body_id);
            ui.ctx().memory_mut(|memory| memory.request_focus(body_id));
        }

        // Where the reader's eye is, in the text's own units, so the preview
        // can be asked for the same section.
        let visible_top = ui.clip_rect().top() - output.galley_pos.y;
        let top_cursor = output
            .galley
            .cursor_from_pos(egui::vec2(0.0, visible_top.max(0.0)));
        self.top_visible_offset = self
            .buffer
            .char_indices()
            .nth(top_cursor.index.0)
            .map_or(self.buffer.len(), |(offset, _)| offset);

        if self.vi_caret.is_none() {
            if let Some(offset) = self.pending_scroll_offset.take() {
                let index = self.buffer[..offset.min(self.buffer.len())].chars().count();
                let target = output
                    .galley
                    .pos_from_cursor(egui::text::CCursor::new(index))
                    .translate(output.galley_pos.to_vec2());
                ui.scroll_to_rect(target, Some(egui::Align::TOP));
                #[cfg(test)]
                {
                    self.last_scroll_target = Some(target);
                }
            }
        }

        self.vi_focused = self.options.vi_keys && output.response.has_focus();

        // The engine decides where the caret is, but the widget owns its own
        // cursor, so the move is handed over after the body has been built.
        if let Some(offset) = self.vi_caret.take() {
            let index = self.buffer[..offset.min(self.buffer.len())].chars().count();
            let mut state = output.state.clone();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(index),
                )));
            state.store(ui.ctx(), body_id);
            ui.ctx().memory_mut(|memory| memory.request_focus(body_id));
            // A caret the reader cannot see has not moved as far as they are
            // concerned. `G`, `gg`, `n` and `:14` all land somewhere that may
            // be pages away, so the view goes with them.
            let caret = output
                .galley
                .pos_from_cursor(egui::text::CCursor::new(index))
                .translate(output.galley_pos.to_vec2());
            ui.scroll_to_rect(caret.expand(CARET_SCROLL_MARGIN), None);
            #[cfg(test)]
            {
                self.last_scroll_target = Some(caret);
            }
            self.paint_mode_caret(ui, &output, index);
            return;
        }

        let cursor_index = output
            .state
            .cursor
            .char_range()
            .map_or(0, |range| range.primary.index.0);
        self.paint_mode_caret(ui, &output, cursor_index);

        if let Some(range) = output.cursor_range {
            if let Some(open) = documents.borrow().get(self.document) {
                let offset = open.text().byte_offset_of_char(range.primary.index.0);
                self.caret = open.text().line_and_column(offset);
                self.caret_offset = offset;
            }
        }
    }
}

/// Pulls the vi-relevant keystrokes out of the event queue.
///
/// In Insert and Replace mode only the keys the engine needs are taken, so
/// arrows, selection and the platform's own editing keys keep working; in
/// Normal and Visual mode every character is a command and none of them may
/// reach the text.
fn take_vi_keys(ui: &egui::Ui, insert: bool) -> Vec<ViKey> {
    ui.input_mut(|input| {
        let mut keys = Vec::new();
        input.events.retain(|event| match event {
            egui::Event::Text(text) => {
                keys.extend(text.chars().map(ViKey::Char));
                false
            }
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => match key {
                egui::Key::Escape => {
                    keys.push(ViKey::Escape);
                    false
                }
                egui::Key::Enter => {
                    keys.push(ViKey::Enter);
                    false
                }
                egui::Key::Backspace => {
                    keys.push(ViKey::Backspace);
                    false
                }
                _ if modifiers.ctrl && !insert => {
                    match key.name().chars().next().map(|c| c.to_ascii_lowercase()) {
                        Some(letter) => {
                            keys.push(ViKey::Ctrl(letter));
                            false
                        }
                        None => true,
                    }
                }
                // Arrows and the rest are left alone even in Normal mode: they
                // move the caret the way every other application moves it, and
                // refusing them would be a bound the ADR does not ask for.
                _ => true,
            },
            _ => true,
        });
        keys
    })
}

/// The word the caret is inside, for `*` and `#`.
fn word_under_caret(text: &str, caret: usize) -> Option<&str> {
    let caret = caret.min(text.len());
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = text[..caret]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(caret, |(index, _)| index);
    let end = text[caret..]
        .char_indices()
        .find(|(_, c)| !is_word(*c))
        .map_or(text.len(), |(index, _)| caret + index);
    (start < end).then(|| &text[start..end])
}

/// `1 replacement` reads better than `1 replacements`, and the count is the
/// whole of what a substitution has to report.
fn match_count_message(count: usize) -> String {
    match count {
        0 => "No matches".to_owned(),
        1 => "1 replacement".to_owned(),
        many => format!("{many} replacements"),
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

/// The hint that tells a reader in this mode what the next useful key is.
fn vi_mode_hint(mode: ViMode) -> &'static str {
    match mode {
        ViMode::Normal => "Press i to insert",
        ViMode::Insert => "Press Esc to return to NORMAL",
        ViMode::Replace => "Press Esc to stop overwriting",
        ViMode::Visual | ViMode::VisualLine => "Press Esc to drop the selection",
    }
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

/// Lays the body out with every match behind a wash of colour and the current
/// one behind a stronger one, so "which of these is Next going to take me to"
/// is answerable by looking (ADR 0034 §10a).
/// How far above and below the visible text is parsed for colour, in lines.
///
/// A small margin only: enough that a flick of the wheel lands on coloured
/// text rather than on a frame of grey, and not so much that the cost stops
/// being proportional to the window.
const SYNTAX_MARGIN_LINES: usize = 60;

/// The byte range worth colouring for what the reader can currently see.
fn visible_byte_range(
    text: &str,
    top_offset: usize,
    ui: &egui::Ui,
) -> std::ops::Range<usize> {
    let line_height = ui.text_style_height(&egui::TextStyle::Monospace).max(1.0);
    let rows = (ui.clip_rect().height() / line_height).ceil() as usize + 1;
    let start = step_lines_back(text, top_offset, SYNTAX_MARGIN_LINES);
    let end = step_lines_forward(text, top_offset, rows + SYNTAX_MARGIN_LINES);
    start..end
}

fn step_lines_back(text: &str, from: usize, lines: usize) -> usize {
    let mut at = from.min(text.len());
    for _ in 0..=lines {
        match text[..at].rfind('\n') {
            Some(newline) => at = newline,
            None => return 0,
        }
    }
    at
}

fn step_lines_forward(text: &str, from: usize, lines: usize) -> usize {
    let mut at = from.min(text.len());
    for _ in 0..lines {
        match text[at..].find('\n') {
            Some(newline) => at += newline + 1,
            None => return text.len(),
        }
    }
    at
}

/// The colour a syntax role is painted in. One mapping, shared by the editor
/// and by the preview's fenced code, so the two cannot drift (ADR 0035 §5, §8).
pub(crate) const fn role_colour(role: festerm_syntax::Role) -> egui::Color32 {
    match role {
        festerm_syntax::Role::Keyword => theme::SYNTAX_KEYWORD,
        festerm_syntax::Role::StringLiteral => theme::SYNTAX_STRING,
        festerm_syntax::Role::Number => theme::SYNTAX_NUMBER,
        festerm_syntax::Role::Comment => theme::SYNTAX_COMMENT,
        festerm_syntax::Role::Type => theme::SYNTAX_TYPE,
        festerm_syntax::Role::Function => theme::SYNTAX_FUNCTION,
        festerm_syntax::Role::Punctuation => theme::SYNTAX_PUNCTUATION,
        festerm_syntax::Role::Variable => theme::SYNTAX_VARIABLE,
        festerm_syntax::Role::Constant => theme::SYNTAX_CONSTANT,
    }
}

/// The body's layout: syntax decides the ink, a find match decides the ground.
///
/// The two are deliberately different channels. A match keeps its wash and its
/// rule whatever the text under it is, so a keyword that matches the search is
/// still legible as both (ADR 0034 §8, ADR 0035 §5).
fn editor_layout_job(
    text: &str,
    highlights: &[(usize, usize, bool)],
    spans: &[festerm_syntax::Span],
) -> egui::text::LayoutJob {
    let font = FontId::monospace(EDITOR_TEXT_SIZE);
    let mut job = egui::text::LayoutJob::default();
    // Every boundary either channel cares about, so each run that is appended
    // is uniform in both.
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for &(start, end, _) in highlights {
        cuts.push(start);
        cuts.push(end);
    }
    for span in spans {
        cuts.push(span.start);
        cuts.push(span.end);
    }
    cuts.retain(|cut| *cut <= text.len() && text.is_char_boundary(*cut));
    cuts.sort_unstable();
    cuts.dedup();

    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end {
            continue;
        }
        let matched = highlights
            .iter()
            .find(|(match_start, match_end, _)| *match_start <= start && *match_end >= end);
        let (background, underline) = match matched {
            // The current match is filled; the rest are washed and ruled. The
            // two differ in shape as well as in colour, so which match Enter
            // will take is readable without telling two dark blues apart.
            Some((_, _, true)) => (theme::SURFACE_SELECTION, egui::Stroke::NONE),
            Some(_) => (
                theme::SEARCH_MATCH_FILL,
                egui::Stroke::new(1.0, theme::SEARCH_MATCH_RULE),
            ),
            None => (egui::Color32::TRANSPARENT, egui::Stroke::NONE),
        };
        let colour = spans
            .iter()
            .find(|span| span.start <= start && span.end >= end)
            .map_or(theme::TEXT_PRIMARY, |span| role_colour(span.role));
        job.append(
            &text[start..end],
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: colour,
                background,
                underline,
                ..Default::default()
            },
        );
    }
    job
}

fn label(ui: &mut egui::Ui, text: &str, colour: egui::Color32, strong: bool) -> egui::Response {
    let font = FontId::proportional(if strong {
        LABEL_TEXT_SIZE + 1.0
    } else {
        LABEL_TEXT_SIZE
    });
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, colour);
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    ui.painter().galley(rect.left_top(), galley, colour);
    response
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

    /// A view of `path` in Edit mode. Markdown opens in Preview for a reader
    /// (ADR 0034 §4); a test about editing starts where the reader would
    /// after pressing Edit.
    fn editor_for(path: &std::path::Path) -> (SharedDocuments, TextEditorTab) {
        let (documents, mut editor) = opened_editor_for(path);
        editor.mode = EditorMode::Edit;
        (documents, editor)
    }

    /// A second Edit-mode view of an already-open document, which is what
    /// Duplicate view gives a reader who then presses Edit.
    fn second_view_of(
        document: DocumentId,
        documents: &SharedDocuments,
    ) -> TextEditorTab {
        let mut editor = TextEditorTab::new(document, documents);
        editor.mode = EditorMode::Edit;
        editor
    }

    /// A view of `path` exactly as it opens, mode included.
    fn opened_editor_for(path: &std::path::Path) -> (SharedDocuments, TextEditorTab) {
        let documents = DocumentRegistry::shared();
        let id = documents.borrow_mut().open_local(path).unwrap();
        let editor = TextEditorTab::new(id, &documents);
        (documents, editor)
    }

    #[test]
    fn a_markdown_file_opens_in_preview_and_a_plain_one_opens_in_edit() {
        let directory = TemporaryDirectory::new("opening-mode");
        let markdown = directory.file("NOTES.md", "# alpha\n");
        let plain = directory.file("notes.txt", "alpha\n");

        assert_eq!(
            opened_editor_for(&markdown).1.mode(),
            EditorMode::Preview,
            "Markdown is opened to be read first"
        );
        assert_eq!(
            opened_editor_for(&plain).1.mode(),
            EditorMode::Edit,
            "and anything else is opened to be edited"
        );
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
    fn the_status_bar_names_the_language_the_highlighter_decided_on() {
        let directory = TemporaryDirectory::new("syntax-language");
        let path = directory.file("relay.toml", "[relay]\nlisten = \"0.0.0.0\"\n");
        let (documents, editor) = editor_for(&path);

        assert_eq!(editor.language_label(), "TOML");
        assert_eq!(
            editor.status_bar_language(&documents),
            "TOML",
            "with colour working there is nothing to explain"
        );
    }

    #[test]
    fn a_file_too_large_to_parse_says_so_where_it_says_the_language() {
        let directory = TemporaryDirectory::new("syntax-too-large");
        let source = "// filler\n".repeat(festerm_syntax::MAX_HIGHLIGHT_LINES + 1);
        let path = directory.file("huge.rs", &source);
        let (documents, editor) = editor_for(&path);

        // Asking for colour is what discovers the bound, exactly as a frame
        // would.
        let length = documents
            .borrow()
            .get(editor.document)
            .unwrap()
            .text()
            .text()
            .len();
        documents
            .borrow_mut()
            .get_mut(editor.document)
            .unwrap()
            .syntax_spans(0..length.min(4_000));

        assert_eq!(
            editor.status_bar_language(&documents),
            "Rust · No colour · file too large",
            "a silent difference between two .rs files is a bug report waiting to happen"
        );
    }

    #[test]
    fn turning_highlighting_off_stops_asking_for_colour_at_all() {
        let directory = TemporaryDirectory::new("syntax-off");
        let source = "// filler\n".repeat(festerm_syntax::MAX_HIGHLIGHT_LINES + 1);
        let path = directory.file("huge.rs", &source);
        let (documents, mut editor) = editor_for(&path);
        editor.options.syntax = false;

        assert_eq!(
            editor.status_bar_language(&documents),
            "Rust",
            "with colour off there is no bound to report and no parse to pay for"
        );
    }

    #[test]
    fn syntax_decides_the_ink_and_a_find_match_decides_the_ground() {
        let text = "fn main() {}\n";
        let spans = [festerm_syntax::Span {
            start: 0,
            end: 2,
            role: festerm_syntax::Role::Keyword,
        }];
        let job = editor_layout_job(text, &[(3, 7, true)], &spans);

        let section_for = |needle: &str| {
            let at = job.text.find(needle).expect("the run is laid out");
            job.sections
                .iter()
                .find(|section| usize::from(section.byte_range.start) == at)
                .expect("a run starts there")
                .clone()
        };
        let keyword = section_for("fn");
        assert_eq!(keyword.format.color, theme::SYNTAX_KEYWORD);
        assert_eq!(keyword.format.background, egui::Color32::TRANSPARENT);
        let matched = section_for("main");
        assert_eq!(
            matched.format.background,
            theme::SURFACE_SELECTION,
            "the current match keeps its ground whatever the syntax under it"
        );
        assert_eq!(
            job.text, text,
            "colouring text never rewrites it"
        );
    }

    #[test]
    fn with_highlighting_off_every_run_is_ordinary_text() {
        let text = "fn main() {}\n";
        let job = editor_layout_job(text, &[], &[]);

        assert!(job
            .sections
            .iter()
            .all(|section| section.format.color == theme::TEXT_PRIMARY));
    }

    #[test]
    fn typing_in_one_view_is_visible_in_another_view_of_the_same_file() {
        let directory = TemporaryDirectory::new("shared");
        let path = directory.file("notes.md", "alpha\n");
        let (documents, mut first) = editor_for(&path);
        let id = documents.borrow_mut().open_local(&path).unwrap();
        let mut second = second_view_of(id, &documents);

        first.buffer.push_str("typed\n");
        first.commit_buffer_for_test(&documents);
        second.adopt_external_edits(&documents);

        assert_eq!(second.buffer, "alpha\ntyped\n");
        assert_eq!(
            second.chip_status(&documents),
            festerm_ui_egui::chrome::ChipStatus::DocumentUnsaved,
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
            festerm_ui_egui::chrome::ChipStatus::DocumentConflict,
            "a conflict is its own state, not a session that failed to start"
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
        typing_harness_sized(path, egui::vec2(700.0, 420.0))
    }

    /// Find/Replace puts eight controls in one row, so its tests need a window
    /// wide enough to hold them; a clipped button is not in the accessibility
    /// tree and would fail for a reason that has nothing to do with the test.
    fn find_harness(path: &std::path::Path) -> Harness<'static, (SharedDocuments, TextEditorTab)> {
        typing_harness_sized(path, egui::vec2(1180.0, 420.0))
    }

    fn typing_harness_sized(
        path: &std::path::Path,
        size: egui::Vec2,
    ) -> Harness<'static, (SharedDocuments, TextEditorTab)> {
        let (documents, editor) = editor_for(path);
        let tab_id = crate::tabs::TabId::next_for_test();
        let mut harness = Harness::builder().with_size(size).build_ui_state(
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

    /// Opens Find the way the user does, types a pattern into the field the
    /// bar put focus in, and lets the frame settle.
    fn open_find_and_type(
        harness: &mut Harness<'static, (SharedDocuments, TextEditorTab)>,
        replacing: bool,
        pattern: &str,
    ) {
        let modifiers = if replacing {
            egui::Modifiers::COMMAND | egui::Modifiers::ALT
        } else {
            egui::Modifiers::COMMAND
        };
        harness.key_press_modifiers(modifiers, egui::Key::F);
        harness.run();
        type_into_find_field(harness, 0, pattern);
    }

    /// The Find bar's single-line fields in the order they are laid out: the
    /// pattern first, the replacement second. Addressed by role because both
    /// carry the accessible name of the label beside them, which is what a
    /// screen reader should read out and therefore what the test should not
    /// try to disambiguate by.
    /// Focuses a Find bar field and types into it. Focus has to be asked for
    /// and then allowed to settle: typing at whatever happens to hold focus is
    /// how a replacement ends up appended to the pattern.
    fn type_into_find_field(
        harness: &mut Harness<'static, (SharedDocuments, TextEditorTab)>,
        index: usize,
        text: &str,
    ) {
        find_field(harness, index).focus();
        harness.run();
        find_field(harness, index).type_text(text);
        harness.run();
    }

    fn find_field<'h>(
        harness: &'h Harness<'static, (SharedDocuments, TextEditorTab)>,
        index: usize,
    ) -> egui_kittest::Node<'h> {
        harness
            .query_all_by_role(egui::accesskit::Role::TextInput)
            .nth(index)
            .expect("the Find bar's field")
    }

    /// Opens the options menu by clicking it, the way a user reaches it.
    fn open_options_menu(harness: &mut Harness<'static, (SharedDocuments, TextEditorTab)>) {
        harness.get_by_label("Editor options").click();
        harness.run();
    }

    /// The column count box inside the open options menu. It is the only
    /// single-line field the editor shows while Find is closed.
    fn columns_field<'h>(
        harness: &'h Harness<'static, (SharedDocuments, TextEditorTab)>,
    ) -> egui_kittest::Node<'h> {
        harness
            .query_all_by_role(egui::accesskit::Role::TextInput)
            .next()
            .expect("the fixed column count field")
    }

    #[test]
    fn a_narrow_window_puts_the_find_verbs_in_a_menu_rather_than_off_the_edge() {
        let directory = TemporaryDirectory::new("find-narrow");
        let path = directory.file("notes.md", "alpha\nalpha\n");
        // The window the Find bar does not fit in. A clipped button is absent
        // from the accessibility tree, which is exactly the failure being
        // guarded against: unreachable, not merely unseen.
        let mut harness = typing_harness(&path);

        open_find_and_type(&mut harness, true, "alpha");
        type_into_find_field(&mut harness, 1, "beta");

        assert!(
            harness.query_by_label("Replace every match").is_none(),
            "there is no room for the verbs on this row"
        );

        harness.get_by_label("Find actions").click();
        harness.run();
        harness.get_by_label("Replace All").click();
        harness.run();

        assert_eq!(
            document_text(&harness),
            "beta\nbeta\n",
            "every action stays reachable through the menu, however narrow the window"
        );
    }

    #[test]
    fn an_uncompilable_pattern_says_so_briefly_and_keeps_its_full_reason() {
        let directory = TemporaryDirectory::new("find-brief-error");
        let path = directory.file("notes.md", "alpha\n");
        let mut harness = find_harness(&path);

        open_find_and_type(&mut harness, false, "(");

        assert!(
            harness.query_by_label("Invalid pattern").is_some(),
            "the row has to stay a row: the regex engine's full complaint would \
             push the navigation off the end of it"
        );
        assert!(
            harness
                .state()
                .1
                .find_error_for_test()
                .is_some_and(|error| error.len() > "Invalid pattern".len()),
            "and the detail has to still be there for the hover to show"
        );
    }

    #[test]
    fn turning_line_numbers_off_says_so_in_the_summary() {
        let directory = TemporaryDirectory::new("options-lines");
        let path = directory.file("notes.md", "alpha\nbeta\n");
        let mut harness = find_harness(&path);

        assert!(harness.query_by_label_contains("Lines ·").is_some());

        open_options_menu(&mut harness);
        harness.get_by_label("Show line numbers").click();
        harness.run();

        assert!(
            !harness.state().1.options.line_numbers,
            "unticking the box has to take the gutter away"
        );
        assert!(
            harness.query_by_label_contains("No lines ·").is_some(),
            "the summary beside the menu has to keep up with what the menu did"
        );
    }

    #[test]
    fn ticking_fixed_columns_offers_a_count_and_reports_it() {
        let directory = TemporaryDirectory::new("options-columns");
        let path = directory.file("notes.md", "alpha\nbeta\n");
        let mut harness = find_harness(&path);

        open_options_menu(&mut harness);
        harness.get_by_label("Fixed column count").click();
        harness.run();

        assert_eq!(
            harness.state().1.options.fixed_columns,
            Some(72),
            "the box has to come on at a usable count rather than at nothing"
        );
        assert!(harness.query_by_label_contains("72 columns").is_some());
    }

    #[test]
    fn a_column_count_that_cannot_be_used_leaves_the_view_where_it_was() {
        let directory = TemporaryDirectory::new("options-invalid");
        let path = directory.file("notes.md", "alpha\nbeta\n");
        let mut harness = find_harness(&path);

        open_options_menu(&mut harness);
        harness.get_by_label("Fixed column count").click();
        harness.run();

        // Everything selected, then replaced by a count nothing can wrap at.
        columns_field(&harness).focus();
        harness.run();
        columns_field(&harness).type_text("\u{8}\u{8}0");
        harness.run();

        assert_eq!(
            harness.state().1.options.fixed_columns,
            Some(72),
            "zero columns is not a width, so the view has to stay where it was"
        );
        assert!(
            harness.state().1.options_state.invalid,
            "and the menu has to say why nothing happened"
        );
    }

    #[test]
    fn changing_view_options_is_not_something_undo_can_take_back() {
        let directory = TemporaryDirectory::new("options-undo");
        let path = directory.file("notes.md", "alpha\n");
        let mut harness = find_harness(&path);

        harness
            .get_by_role(egui::accesskit::Role::MultilineTextInput)
            .focus();
        harness.run();
        harness
            .get_by_role(egui::accesskit::Role::MultilineTextInput)
            .type_text("beta");
        harness.run();
        let typed = document_text(&harness);
        assert!(typed.contains("beta"));

        open_options_menu(&mut harness);
        harness.get_by_label("Fixed column count").click();
        harness.run();

        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        harness.run();

        assert!(
            !document_text(&harness).contains("beta"),
            "undo has to reach past the option change to the typing, because a \
             per-view option never entered the document's history at all"
        );
        assert_eq!(
            harness.state().1.options.fixed_columns,
            Some(72),
            "and undo must not put the option back either"
        );
    }

    #[test]
    fn command_f_opens_find_and_counts_what_the_pattern_matches() {
        let directory = TemporaryDirectory::new("find-open");
        let path = directory.file("notes.md", "alpha\nbeta\nalpha\n");
        let mut harness = find_harness(&path);

        open_find_and_type(&mut harness, false, "alpha");

        assert!(
            harness.query_by_label("1 of 2").is_some(),
            "the bar has to say which match of how many, not just that it found something"
        );
    }

    #[test]
    fn a_half_typed_expression_reports_itself_and_keeps_the_last_good_results() {
        let directory = TemporaryDirectory::new("find-invalid");
        let path = directory.file("notes.md", "alpha\nalpha\n");
        let mut harness = find_harness(&path);

        open_find_and_type(&mut harness, false, "alpha");
        assert!(harness.query_by_label("1 of 2").is_some());

        // A lone `(` is the ordinary half-typed state of someone reaching for
        // a group, not a mistake worth clearing their highlights for.
        type_into_find_field(&mut harness, 0, "(");

        let (_, editor) = harness.state();
        assert!(
            editor.find_error_for_test().is_some(),
            "an invalid expression has to say so"
        );
        assert_eq!(
            editor.find_match_count_for_test(),
            2,
            "the last valid results should still be standing"
        );
    }

    #[test]
    fn next_walks_the_matches_and_wraps_once() {
        let directory = TemporaryDirectory::new("find-next");
        let path = directory.file("notes.md", "alpha\nbeta\nalpha\n");
        let mut harness = find_harness(&path);

        open_find_and_type(&mut harness, false, "alpha");
        harness.get_by_label("Next match").click();
        harness.run();
        assert!(harness.query_by_label("2 of 2").is_some());

        harness.get_by_label("Next match").click();
        harness.run();
        assert!(
            harness.query_by_label("1 of 2").is_some(),
            "Next past the last match wraps to the first"
        );
    }

    /// An editor with vi keys on, focused on its body, which is the only state
    /// in which `:` and `/` mean anything.
    fn vi_harness(path: &std::path::Path) -> Harness<'static, (SharedDocuments, TextEditorTab)> {
        let mut harness = find_harness(path);
        harness.state_mut().1.options.vi_keys = true;
        harness.run();
        harness
    }

    fn command_field<'h>(
        harness: &'h Harness<'static, (SharedDocuments, TextEditorTab)>,
    ) -> egui_kittest::Node<'h> {
        harness
            .query_all_by_role(egui::accesskit::Role::TextInput)
            .next()
            .expect("the command area's field")
    }

    /// Types into the body with vi keys live, the way a user does.
    fn vi_type(harness: &mut Harness<'static, (SharedDocuments, TextEditorTab)>, keys: &str) {
        for character in keys.chars() {
            harness
                .input_mut()
                .events
                .push(egui::Event::Text(character.to_string()));
            harness.run();
        }
    }

    fn focus_body(harness: &mut Harness<'static, (SharedDocuments, TextEditorTab)>) {
        let id = harness.state().1.body_id();
        harness.ctx.memory_mut(|memory| memory.request_focus(id));
        // egui parks a freshly focused field's cursor at the end of the text.
        // vi commands read from wherever the caret is, so the tests below pin
        // it at the start the way a reader who has just opened a file sees it.
        harness.state_mut().1.vi_caret = Some(0);
        harness.run();
    }

    #[test]
    fn in_normal_mode_a_letter_is_a_command_and_never_text() {
        let directory = TemporaryDirectory::new("vi-normal");
        let path = directory.file("notes.md", "alpha beta\n");
        let mut harness = vi_harness(&path);
        focus_body(&mut harness);

        vi_type(&mut harness, "dw");

        assert_eq!(
            document_text(&harness),
            "beta\n",
            "`dw` deletes a word rather than typing `d` and `w` into the file"
        );
    }

    #[test]
    fn the_status_bar_says_the_mode_in_words() {
        let directory = TemporaryDirectory::new("vi-mode-label");
        let path = directory.file("notes.md", "alpha\n");
        let mut harness = vi_harness(&path);
        focus_body(&mut harness);

        assert_eq!(harness.state().1.vi_mode_label(), Some("NORMAL"));
        vi_type(&mut harness, "i");
        assert_eq!(harness.state().1.vi_mode_label(), Some("INSERT"));
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert_eq!(harness.state().1.vi_mode_label(), Some("NORMAL"));
        vi_type(&mut harness, "v");
        assert_eq!(harness.state().1.vi_mode_label(), Some("VISUAL"));
        harness.key_press(egui::Key::Escape);
        harness.run();
        vi_type(&mut harness, ":");
        assert_eq!(
            harness.state().1.vi_mode_label(),
            Some("COMMAND"),
            "the command area is a state the status bar has to name too"
        );
    }

    #[test]
    fn the_vi_marker_explains_the_state_the_view_is_actually_in() {
        let directory = TemporaryDirectory::new("vi-banner");
        let path = directory.file("notes.md", "alpha\n");
        let mut harness = vi_harness(&path);
        focus_body(&mut harness);

        let state = harness.state();
        let (headline, detail) = state.1.vi_status_text();
        assert_eq!(headline, "vi compatibility is active in this view");
        assert!(
            detail.starts_with("NORMAL mode · Press i to insert"),
            "the marker names the mode and the way out of it: {detail}"
        );

        vi_type(&mut harness, ":wq");
        let state = harness.state();
        let (headline, detail) = state.1.vi_status_text();
        assert_eq!(headline, "vi command-line mode");
        assert!(
            detail.starts_with(":wq saves through the ordinary Save command"),
            "the marker explains the command that has been typed: {detail}"
        );
    }

    #[test]
    fn a_view_without_vi_keys_shows_no_mode_at_all() {
        let directory = TemporaryDirectory::new("vi-mode-absent");
        let path = directory.file("notes.md", "alpha\n");
        let harness = find_harness(&path);
        assert_eq!(harness.state().1.vi_mode_label(), None);
    }

    #[test]
    fn a_counted_operator_deletes_exactly_what_it_promised_in_one_undo() {
        let directory = TemporaryDirectory::new("vi-count");
        let path = directory.file("notes.md", "one two three four\n");
        let mut harness = vi_harness(&path);
        focus_body(&mut harness);

        vi_type(&mut harness, "2dw");
        assert_eq!(document_text(&harness), "three four\n");

        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        harness.run();
        assert_eq!(
            document_text(&harness),
            "one two three four\n",
            "a counted operator comes back in a single press"
        );
    }

    #[test]
    fn a_key_outside_the_matrix_says_so_and_leaves_the_text_alone() {
        let directory = TemporaryDirectory::new("vi-refusal");
        let path = directory.file("notes.md", "alpha\n");
        let mut harness = vi_harness(&path);
        focus_body(&mut harness);

        // A named register is recognised and deliberately declined rather
        // than half-executed.
        vi_type(&mut harness, "\"");

        assert_eq!(document_text(&harness), "alpha\n");
        assert!(
            matches!(
                harness.state().1.command.outcome(),
                Some(CommandOutcome::Failed(_))
            ),
            "an unsupported key reports itself instead of doing nothing visible"
        );
    }

    #[test]
    fn a_vi_search_and_next_walk_the_document_without_changing_it() {
        let directory = TemporaryDirectory::new("vi-search-next");
        let path = directory.file("notes.md", "alpha\nbeta\nalpha\n");
        let mut harness = vi_harness(&path);
        focus_body(&mut harness);

        vi_type(&mut harness, "/");
        assert_eq!(
            harness.state().1.command.prompt(),
            Some(CommandPrompt::SearchForward),
            "a slash opens the search prompt rather than reaching the text"
        );
        command_field(&harness).focus();
        harness.run();
        command_field(&harness).type_text("alpha");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        harness.run();

        // A search lands on the match after the caret, not at the top of the
        // file, and hands the body its focus back: without that the next key
        // reaches a field that is no longer on screen.
        assert_eq!(
            harness.state().1.find.current,
            Some(1),
            "a forward search takes the match after the caret"
        );
        assert_eq!(harness.state().1.caret_offset, 11);
        let body = harness.state().1.body_id();
        assert!(
            harness.ctx.memory(|memory| memory.has_focus(body)),
            "running a search gives the text its focus back"
        );

        // `n` now reaches the engine without anything else being clicked.
        vi_type(&mut harness, "n");

        assert_eq!(
            harness.state().1.find.current,
            Some(0),
            "`n` steps to the next match and wraps"
        );
        assert_eq!(harness.state().1.caret_offset, 0);
        assert_eq!(document_text(&harness), "alpha\nbeta\nalpha\n");

        // `N` goes back the other way.
        vi_type(&mut harness, "N");
        assert_eq!(harness.state().1.find.current, Some(1));
    }

    #[test]
    fn a_backward_search_walks_backwards_and_so_does_n() {
        let directory = TemporaryDirectory::new("vi-search-back");
        let path = directory.file("notes.md", "alpha\nbeta\nalpha\n");
        let mut harness = vi_harness(&path);
        focus_body(&mut harness);
        harness.state_mut().1.vi_caret = Some(11);
        harness.run();

        vi_type(&mut harness, "?");
        assert_eq!(
            harness.state().1.command.prompt(),
            Some(CommandPrompt::SearchBackward)
        );
        command_field(&harness).focus();
        harness.run();
        command_field(&harness).type_text("alpha");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        harness.run();

        assert_eq!(
            harness.state().1.find.current,
            Some(0),
            "a backward search takes the match before the caret"
        );
        vi_type(&mut harness, "n");
        assert_eq!(
            harness.state().1.find.current,
            Some(1),
            "`n` repeats a backward search backwards, wrapping to the end"
        );
    }

    #[test]
    fn vi_keys_are_dead_while_something_else_holds_focus() {
        let directory = TemporaryDirectory::new("vi-focus");
        let path = directory.file("notes.md", "alpha beta\n");
        let mut harness = vi_harness(&path);

        open_find_and_type(&mut harness, false, "beta");
        vi_type(&mut harness, "dw");

        assert_eq!(
            document_text(&harness),
            "alpha beta\n",
            "typing in the Find field must not run a vi operator on the document"
        );
    }

    /// Types a `:` command the way a reader does: the colon opens the area,
    /// the field takes the rest, Enter runs it.
    fn run_command(
        harness: &mut Harness<'static, (SharedDocuments, TextEditorTab)>,
        command: &str,
    ) {
        vi_type(harness, ":");
        command_field(harness).focus();
        harness.run();
        command_field(harness).type_text(command);
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        harness.run();
    }

    fn numbered_lines(count: usize) -> String {
        (1..=count)
            .map(|line| format!("line {line}\n"))
            .collect::<String>()
    }

    #[test]
    fn a_bare_number_is_a_line_address_and_the_view_goes_with_it() {
        let directory = TemporaryDirectory::new("vi-goto-line");
        let path = directory.file("notes.md", &numbered_lines(200));
        let mut harness = vi_harness(&path);
        focus_body(&mut harness);

        run_command(&mut harness, "14");

        // "line 1\n" through "line 9\n" are 7 bytes each, then two digits.
        let expected = 9 * 7 + 4 * 8;
        assert_eq!(
            harness.state().1.caret_offset,
            expected,
            "`:14` puts the caret at the start of line 14"
        );
        assert!(
            harness.state().1.last_scroll_target.is_some(),
            "a jump the reader cannot see has not happened as far as they are concerned"
        );
    }

    #[test]
    fn a_dollar_is_the_last_line_and_a_number_past_the_end_is_too() {
        let directory = TemporaryDirectory::new("vi-goto-last");
        let path = directory.file("notes.md", "one\ntwo\nthree\n");
        let mut harness = vi_harness(&path);
        focus_body(&mut harness);

        run_command(&mut harness, "$");
        assert_eq!(harness.state().1.caret_offset, 8, "`:$` is the last line");

        focus_body(&mut harness);
        run_command(&mut harness, "900");
        assert_eq!(
            harness.state().1.caret_offset,
            8,
            "a line number past the end goes as far as the file goes rather than refusing"
        );
    }

    #[test]
    fn a_caret_move_takes_the_viewport_with_it() {
        let directory = TemporaryDirectory::new("vi-caret-visible");
        let path = directory.file("notes.md", &numbered_lines(400));
        let mut harness = vi_harness(&path);
        focus_body(&mut harness);
        harness.state_mut().1.last_scroll_target = None;

        vi_type(&mut harness, "G");

        let target = harness
            .state()
            .1
            .last_scroll_target
            .expect("`G` scrolls the body to the caret it just moved");
        let top = harness
            .state()
            .1
            .last_scroll_target
            .map(|rect| rect.top())
            .unwrap_or_default();
        assert!(
            target.height() > 0.0 && top > 0.0,
            "the scroll target is the caret's own line, a long way down a 400-line file"
        );
    }

    #[test]
    fn the_outline_is_offered_only_while_the_file_renders_as_markdown() {
        let directory = TemporaryDirectory::new("editor-outline-offer");
        let markdown = directory.file("notes.md", "# Title\n\n## Section\n\nbody\n");
        let plain = directory.file("notes.txt", "# Title\n\n## Section\n\nbody\n");
        let (_, markdown_tab) = editor_for(&markdown);
        let (_, plain_tab) = editor_for(&plain);

        assert!(markdown_tab.renders_markdown());
        assert!(!plain_tab.renders_markdown());
    }

    #[test]
    fn the_outline_rail_lists_the_headings_of_the_file_being_edited() {
        let directory = TemporaryDirectory::new("editor-outline-rail");
        let path = directory.file(
            "notes.md",
            "# Title\n\nintro\n\n## Beginnings\n\nbody\n\n## Endings\n\nmore\n",
        );
        let mut harness = find_harness(&path);
        harness.state_mut().1.options.outline = true;
        harness.run();
        harness.run();
        harness.run();

        let headings = harness
            .state()
            .1
            .preview
            .as_ref()
            .expect("the outline parses the buffer it is editing")
            .headings()
            .iter()
            .map(|heading| heading.text().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(headings, ["Title", "Beginnings", "Endings"]);
        assert!(
            harness
                .query_all_by_label("Heading level 2: Endings")
                .next()
                .is_some(),
            "every heading is a row the reader can click"
        );
    }

    #[test]
    fn scrolling_the_text_brings_the_preview_to_the_same_section() {
        let directory = TemporaryDirectory::new("editor-scroll-sync");
        let mut text = String::from("# Title\n\nintro\n\n");
        text.push_str("## Beginnings\n\n");
        for line in 0..80 {
            text.push_str(&format!("beginning line {line}\n"));
        }
        text.push_str("\n## Endings\n\n");
        for line in 0..80 {
            text.push_str(&format!("ending line {line}\n"));
        }
        let path = directory.file("notes.md", &text);
        let mut harness = find_harness(&path);
        harness.state_mut().1.mode = EditorMode::Split;
        for _ in 0..3 {
            harness.run();
        }
        assert_eq!(harness.state().1.sync_heading, Some(0));

        // The reader spins the wheel over the text pane, which is the left
        // half of the split.
        for _ in 0..80 {
            harness
                .input_mut()
                .events
                .push(egui::Event::PointerMoved(egui::pos2(300.0, 250.0)));
            harness.input_mut().events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -400.0),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            });
            // Stepped rather than run: the scroll animates, so a `run` that
            // waits for the frames to settle would wait for ever.
            harness.step();
        }

        assert!(
            harness.state().1.top_visible_offset > 0,
            "the wheel moved the text pane"
        );
        assert_eq!(
            harness.state().1.sync_heading,
            Some(2),
            "the preview follows the text into the section being edited"
        );
        assert_eq!(
            harness
                .state()
                .1
                .preview
                .as_ref()
                .and_then(|pane| pane.visible_heading()),
            Some(2),
        );
    }

    #[test]
    fn a_colon_opens_the_command_area_instead_of_reaching_the_text() {
        let directory = TemporaryDirectory::new("vi-command-open");
        let path = directory.file("notes.md", "alpha\n");
        let mut harness = vi_harness(&path);

        harness
            .input_mut()
            .events
            .push(egui::Event::Text(":".into()));
        harness.run();

        assert_eq!(
            harness.state().1.command.prompt(),
            Some(CommandPrompt::Ex),
            "a colon with vi keys on opens the command area"
        );
        assert_eq!(
            document_text(&harness),
            "alpha\n",
            "the colon that opened the area must not also land in the document"
        );
    }

    #[test]
    fn without_vi_keys_a_colon_is_only_a_colon() {
        let directory = TemporaryDirectory::new("vi-command-off");
        let path = directory.file("notes.md", "alpha\n");
        let mut harness = find_harness(&path);

        harness
            .input_mut()
            .events
            .push(egui::Event::Text(":".into()));
        harness.run();

        assert!(
            !harness.state().1.command.is_open(),
            "an ordinary view has no command area to open"
        );
    }

    #[test]
    fn a_substitution_from_the_command_area_lands_in_one_undo() {
        let directory = TemporaryDirectory::new("vi-command-substitute");
        let path = directory.file("notes.md", "alpha\nbeta\nalpha\n");
        let mut harness = vi_harness(&path);

        harness
            .input_mut()
            .events
            .push(egui::Event::Text(":".into()));
        harness.run();
        command_field(&harness).focus();
        harness.run();
        command_field(&harness).type_text("%s/alpha/gamma/g");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        harness.run();

        assert_eq!(document_text(&harness), "gamma\nbeta\ngamma\n");
        assert_eq!(
            harness.state().1.command.outcome(),
            Some(&CommandOutcome::Message("2 replacements".to_owned())),
            "the area replaces itself with what the command did"
        );

        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        harness.run();
        assert_eq!(
            document_text(&harness),
            "alpha\nbeta\nalpha\n",
            "a substitution comes back in one press, like Replace All"
        );
    }

    #[test]
    fn a_substitution_without_a_range_only_touches_the_line_the_caret_is_on() {
        let directory = TemporaryDirectory::new("vi-command-current-line");
        let path = directory.file("notes.md", "alpha\nalpha\n");
        let mut harness = vi_harness(&path);
        harness.state_mut().1.caret_offset = 6;
        harness.run();

        harness
            .input_mut()
            .events
            .push(egui::Event::Text(":".into()));
        harness.run();
        command_field(&harness).focus();
        harness.run();
        command_field(&harness).type_text("s/alpha/gamma/");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        harness.run();

        assert_eq!(document_text(&harness), "alpha\ngamma\n");
    }

    #[test]
    fn a_command_that_is_not_supported_refuses_and_changes_nothing() {
        let directory = TemporaryDirectory::new("vi-command-refusal");
        let path = directory.file("notes.md", "alpha\n");
        let mut harness = vi_harness(&path);

        harness
            .input_mut()
            .events
            .push(egui::Event::Text(":".into()));
        harness.run();
        command_field(&harness).focus();
        harness.run();
        command_field(&harness).type_text("set number");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        harness.run();

        let Some(CommandOutcome::Failed(error)) = harness.state().1.command.outcome() else {
            panic!("expected a refusal");
        };
        assert_eq!(error.headline(), "Not supported");
        assert_eq!(document_text(&harness), "alpha\n");
    }

    #[test]
    fn a_search_from_the_command_area_counts_as_it_is_typed() {
        let directory = TemporaryDirectory::new("vi-command-search");
        let path = directory.file("notes.md", "alpha\nbeta\nalpha\n");
        let mut harness = vi_harness(&path);

        harness
            .input_mut()
            .events
            .push(egui::Event::Text("/".into()));
        harness.run();
        command_field(&harness).focus();
        harness.run();
        command_field(&harness).type_text("alpha");
        harness.run();

        assert_eq!(
            harness.state().1.find.summary(),
            "1 of 2",
            "the count describes what Enter is about to accept"
        );
        assert_eq!(
            document_text(&harness),
            "alpha\nbeta\nalpha\n",
            "searching never touches the text"
        );
    }

    #[test]
    fn save_and_close_closes_only_once_the_save_has_landed() {
        let directory = TemporaryDirectory::new("vi-command-wq");
        let path = directory.file("notes.md", "alpha\n");
        let (documents, mut editor) = editor_for(&path);
        let tab = crate::tabs::TabId::next_for_test();
        editor.tab = Some(tab);

        {
            let mut registry = documents.borrow_mut();
            let open = registry.get_mut(editor.document()).unwrap();
            open.text_mut()
                .apply_edits(vec![festerm_document::TextEdit {
                    start: 0,
                    removed: String::new(),
                    inserted: "x".to_owned(),
                }])
                .unwrap();
        }
        editor.buffer = "xalpha\n".to_owned();

        let command = editor.dispatch_command(ViCommand::WriteQuit, tab, &documents);
        assert!(matches!(command, Some(AppCommand::SaveTextDocument)));
        assert!(
            editor.close_when_saved(tab, &documents).is_none(),
            "a dirty document has not been written yet, so nothing closes"
        );

        documents.borrow_mut().save(editor.document()).unwrap();
        assert!(
            matches!(
                editor.close_when_saved(tab, &documents),
                Some(AppCommand::CloseTab(closed)) if closed == tab
            ),
            "the view closes once the write has actually landed"
        );
    }

    #[test]
    fn replace_all_changes_every_match_in_one_undo() {
        let directory = TemporaryDirectory::new("find-replace-all");
        let path = directory.file("notes.md", "alpha\nbeta\nalpha\n");
        let mut harness = find_harness(&path);

        open_find_and_type(&mut harness, true, "alpha");
        type_into_find_field(&mut harness, 1, "gamma");
        harness.get_by_label("Replace every match").click();
        harness.run();

        assert_eq!(document_text(&harness), "gamma\nbeta\ngamma\n");

        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        harness.run();
        assert_eq!(
            document_text(&harness),
            "alpha\nbeta\nalpha\n",
            "a replacement across the document comes back in one press"
        );
    }

    #[test]
    fn replace_uses_the_same_capture_dialect_the_command_area_does() {
        let directory = TemporaryDirectory::new("find-captures");
        let path = directory.file("notes.md", "alpha beta\n");
        let mut harness = find_harness(&path);

        open_find_and_type(&mut harness, true, "(alpha) (beta)");
        type_into_find_field(&mut harness, 1, "$2 $1");
        harness.get_by_label("Replace every match").click();
        harness.run();

        assert_eq!(document_text(&harness), "beta alpha\n");
    }

    #[test]
    fn escape_closes_find_without_touching_the_text() {
        let directory = TemporaryDirectory::new("find-escape");
        let path = directory.file("notes.md", "alpha\n");
        let mut harness = find_harness(&path);

        open_find_and_type(&mut harness, false, "alpha");
        harness.key_press(egui::Key::Escape);
        harness.run();

        assert_eq!(
            harness
                .query_all_by_role(egui::accesskit::Role::TextInput)
                .count(),
            0,
            "Escape closes the bar"
        );
        assert_eq!(document_text(&harness), "alpha\n");
    }

    #[test]
    fn undo_inside_the_find_field_does_not_take_back_the_document() {
        let directory = TemporaryDirectory::new("find-undo-boundary");
        let path = directory.file("notes.md", "alpha\n");
        let mut harness = find_harness(&path);
        let body = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
        body.focus();
        body.type_text("typed");
        harness.run();
        let before = document_text(&harness);
        assert!(before.contains("typed"));

        open_find_and_type(&mut harness, false, "alpha");
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        harness.run();

        assert_eq!(
            document_text(&harness),
            before,
            "Cmd+Z in the pattern field means the pattern, not the document"
        );
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
        assert!(!documents.borrow().get(document).unwrap().text().is_dirty());
    }

    #[test]
    fn typing_in_one_view_appears_in_another_view_of_the_same_file() {
        let directory = TemporaryDirectory::new("two-views-typing");
        let path = directory.file("NOTES.md", "alpha\n");
        let (documents, first) = editor_for(&path);
        let id = first.document();
        let second = second_view_of(id, &documents);
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
        let first_body = bodies
            .into_iter()
            .next()
            .expect("the first view has a body");
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

        assert_eq!(
            document_text(&harness),
            "alpha\nbeta",
            "and redo brings it back"
        );
    }

    #[test]
    fn undoing_in_one_view_undoes_the_document_for_every_view() {
        let directory = TemporaryDirectory::new("undo-two-views");
        let path = directory.file("NOTES.md", "alpha\n");
        let (documents, first) = editor_for(&path);
        let id = first.document();
        let second = second_view_of(id, &documents);
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
        let first_body = bodies
            .into_iter()
            .next()
            .expect("the first view has a body");
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
        let pane = harness
            .state()
            .1
            .preview
            .as_ref()
            .expect("split built a preview");
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
        let mut harness = conflicted_harness(&directory, "delta", "alpha\nBRAVO\ncharlie\n");

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
        let mut harness = conflicted_harness(&directory, "delta", "alpha\nBRAVO\ncharlie\n");

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
        let mut harness = conflicted_harness(&directory, "delta", "alpha\nBRAVO\ncharlie\n");

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
        let mut harness = conflicted_harness(&directory, "delta", "alpha\nBRAVO\ncharlie\n");

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
        documents
            .borrow_mut()
            .get_mut(id)
            .unwrap()
            .keep_my_version();
        harness.run();

        assert!(harness.state().1.compare().is_none());
        harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
    }

    #[test]
    fn compare_follows_a_sibling_view_that_keeps_typing() {
        let directory = TemporaryDirectory::new("compare-live");
        let mut harness = conflicted_harness(&directory, "delta", "alpha\nBRAVO\ncharlie\n");

        harness.get_by_label("Compare").click();
        harness.run();
        let before = harness.state().1.compare().unwrap().change_count();

        // Another view of the same document types on.
        let (documents, editor) = harness.state_mut();
        let id = editor.document();
        let mut sibling = second_view_of(id, documents);
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
        let source =
            MarkdownSource::from(LocalMarkdownSource::new(PathBuf::from("NOTES.md")).unwrap());
        let mut pane = MarkdownPreviewPane::new(source, "# One\n");
        let ctx = egui::Context::default();

        pane.sync(&ctx, "# Two\n");
        assert_eq!(pane.rendered_text(), "# One\n");

        std::thread::sleep(crate::markdown_viewer::PREVIEW_DEBOUNCE);
        pane.sync(&ctx, "# Two\n");
        assert_eq!(pane.rendered_text(), "# Two\n");
    }
}
