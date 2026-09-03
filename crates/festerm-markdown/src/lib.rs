//! Bounded, inert Markdown loading and projection for fesTerm.
//!
//! `festerm-markdown` owns the parser-facing contract for the native Markdown
//! viewer approved by ADR 0030. It accepts already-read source bytes, applies
//! fixed bounds, decodes UTF-8 with optional BOM stripping, and projects one
//! immutable snapshot into a document model suitable for Preview, Source,
//! Outline, and Find.

use std::{
    collections::BTreeMap,
    fmt,
    ops::Range,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
};

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

/// Maximum Markdown source size accepted for one snapshot.
pub const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
/// Maximum decoded line count accepted for one snapshot.
pub const MAX_DECODED_LINES: usize = 65_536;
/// Maximum block/list nesting depth accepted for one snapshot.
pub const MAX_BLOCK_NESTING_DEPTH: usize = 16;
/// Maximum rendered table cells accepted across one snapshot.
pub const MAX_TABLE_CELLS: usize = 8_192;
/// Maximum UTF-8 payload accepted for one fenced code block.
pub const MAX_CODE_BLOCK_BYTES: usize = 256 * 1024;
/// Maximum distinct non-fragment resource references accepted for one snapshot.
pub const MAX_RESOURCE_REFERENCES: usize = 2_048;

const UTF8_BOM: &[u8; 3] = b"\xEF\xBB\xBF";
const HIGHLIGHT_THEME_NAME: &str = "base16-ocean.dark";
const BINARY_HEURISTIC_SAMPLE_CHARS: usize = 4_096;
const BINARY_HEURISTIC_CONTROL_RATIO_DENOMINATOR: usize = 32;

/// The documented v1 syntax-highlighting allowlist.
pub const SUPPORTED_HIGHLIGHT_LANGUAGES: &[&str] = &[
    "text",
    "md",
    "markdown",
    "rust",
    "toml",
    "json",
    "yaml",
    "yml",
    "shell",
    "bash",
    "sh",
    "diff",
    "powershell",
    "ps1",
];

/// The fixed bounds enforced by the v1 Markdown loader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownBounds {
    max_source_bytes: usize,
    max_decoded_lines: usize,
    max_block_nesting_depth: usize,
    max_table_cells: usize,
    max_code_block_bytes: usize,
    max_resource_references: usize,
}

impl MarkdownBounds {
    /// Repository-approved default bounds from ADR 0030.
    pub const DEFAULT: Self = Self {
        max_source_bytes: MAX_SOURCE_BYTES,
        max_decoded_lines: MAX_DECODED_LINES,
        max_block_nesting_depth: MAX_BLOCK_NESTING_DEPTH,
        max_table_cells: MAX_TABLE_CELLS,
        max_code_block_bytes: MAX_CODE_BLOCK_BYTES,
        max_resource_references: MAX_RESOURCE_REFERENCES,
    };

    pub const fn max_source_bytes(self) -> usize {
        self.max_source_bytes
    }

    pub const fn max_decoded_lines(self) -> usize {
        self.max_decoded_lines
    }

    pub const fn max_block_nesting_depth(self) -> usize {
        self.max_block_nesting_depth
    }

    pub const fn max_table_cells(self) -> usize {
        self.max_table_cells
    }

    pub const fn max_code_block_bytes(self) -> usize {
        self.max_code_block_bytes
    }

    pub const fn max_resource_references(self) -> usize {
        self.max_resource_references
    }
}

impl Default for MarkdownBounds {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Cooperative cancellation flag for bounded loading and parsing.
#[derive(Clone, Debug, Default)]
pub struct MarkdownCancellation {
    cancelled: Arc<AtomicBool>,
}

impl MarkdownCancellation {
    /// Allocates a new non-cancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation for any future poll points using this token.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Returns whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Declared identity for one Markdown source snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownSource {
    Local(LocalMarkdownSource),
    Remote(RemoteMarkdownSource),
}

impl MarkdownSource {
    /// Returns the source-class policy the viewer should apply to `target`.
    pub fn classify_target(&self, target: &str) -> ResourceReferenceClass {
        classify_target(self, target)
    }
}

impl From<LocalMarkdownSource> for MarkdownSource {
    fn from(source: LocalMarkdownSource) -> Self {
        Self::Local(source)
    }
}

impl From<RemoteMarkdownSource> for MarkdownSource {
    fn from(source: RemoteMarkdownSource) -> Self {
        Self::Remote(source)
    }
}

/// Validated local Markdown source identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalMarkdownSource {
    path: PathBuf,
}

impl LocalMarkdownSource {
    /// Creates a local source identity.
    ///
    /// Callers should prefer a canonical path when one is already available,
    /// but this type does not perform I/O or canonicalization itself.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, MarkdownSourceError> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(MarkdownSourceError::EmptyPath);
        }
        Ok(Self { path })
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

/// Validated remote Markdown source identity pinned to one verified SFTP origin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteMarkdownSource {
    host: String,
    port: u16,
    owner: RemoteSourceOwner,
    verified_host_key_fingerprint: String,
    remote_path: String,
    lifecycle_generation: u64,
}

impl RemoteMarkdownSource {
    /// Creates a remote source identity.
    pub fn new(
        host: impl Into<String>,
        port: u16,
        owner: RemoteSourceOwner,
        verified_host_key_fingerprint: impl Into<String>,
        remote_path: impl Into<String>,
        lifecycle_generation: u64,
    ) -> Result<Self, MarkdownSourceError> {
        let host = normalize_non_empty(host.into(), MarkdownSourceError::EmptyRemoteHost)?;
        if host.chars().any(char::is_whitespace) {
            return Err(MarkdownSourceError::WhitespaceRemoteHost);
        }
        if port == 0 {
            return Err(MarkdownSourceError::ZeroRemotePort);
        }
        let verified_host_key_fingerprint = normalize_non_empty(
            verified_host_key_fingerprint.into(),
            MarkdownSourceError::EmptyVerifiedFingerprint,
        )?;
        let remote_path = normalize_non_empty(remote_path.into(), MarkdownSourceError::EmptyPath)?;
        Ok(Self {
            host: host.to_ascii_lowercase(),
            port,
            owner,
            verified_host_key_fingerprint,
            remote_path,
            lifecycle_generation,
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub const fn port(&self) -> u16 {
        self.port
    }

    pub fn owner(&self) -> &RemoteSourceOwner {
        &self.owner
    }

    pub fn verified_host_key_fingerprint(&self) -> &str {
        &self.verified_host_key_fingerprint
    }

    pub fn remote_path(&self) -> &str {
        &self.remote_path
    }

    pub const fn lifecycle_generation(&self) -> u64 {
        self.lifecycle_generation
    }
}

/// Stable remote-owner identity used to pin a snapshot to one SFTP origin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteSourceOwner {
    Username(String),
    ProfileIdentifier(String),
    UsernameAndProfile {
        username: String,
        profile_identifier: String,
    },
}

impl RemoteSourceOwner {
    pub fn username(username: impl Into<String>) -> Result<Self, MarkdownSourceError> {
        Ok(Self::Username(normalize_non_empty(
            username.into(),
            MarkdownSourceError::EmptyRemoteUsername,
        )?))
    }

    pub fn profile_identifier(
        profile_identifier: impl Into<String>,
    ) -> Result<Self, MarkdownSourceError> {
        Ok(Self::ProfileIdentifier(normalize_non_empty(
            profile_identifier.into(),
            MarkdownSourceError::EmptyRemoteProfileIdentifier,
        )?))
    }

    pub fn username_and_profile(
        username: impl Into<String>,
        profile_identifier: impl Into<String>,
    ) -> Result<Self, MarkdownSourceError> {
        Ok(Self::UsernameAndProfile {
            username: normalize_non_empty(
                username.into(),
                MarkdownSourceError::EmptyRemoteUsername,
            )?,
            profile_identifier: normalize_non_empty(
                profile_identifier.into(),
                MarkdownSourceError::EmptyRemoteProfileIdentifier,
            )?,
        })
    }
}

/// Stable category for source-identity validation failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownSourceError {
    EmptyPath,
    EmptyRemoteHost,
    WhitespaceRemoteHost,
    ZeroRemotePort,
    EmptyRemoteUsername,
    EmptyRemoteProfileIdentifier,
    EmptyVerifiedFingerprint,
}

impl fmt::Display for MarkdownSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPath => formatter.write_str("Markdown source path must not be empty"),
            Self::EmptyRemoteHost => {
                formatter.write_str("remote Markdown source host must not be empty")
            }
            Self::WhitespaceRemoteHost => {
                formatter.write_str("remote Markdown source host must not contain whitespace")
            }
            Self::ZeroRemotePort => {
                formatter.write_str("remote Markdown source port must not be zero")
            }
            Self::EmptyRemoteUsername => {
                formatter.write_str("remote Markdown source username must not be empty")
            }
            Self::EmptyRemoteProfileIdentifier => {
                formatter.write_str("remote Markdown source profile identifier must not be empty")
            }
            Self::EmptyVerifiedFingerprint => {
                formatter.write_str("remote Markdown source verified fingerprint must not be empty")
            }
        }
    }
}

impl std::error::Error for MarkdownSourceError {}

/// Loader entry point for bounded Markdown snapshots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownLoader {
    bounds: MarkdownBounds,
}

impl MarkdownLoader {
    /// Creates a loader with `bounds`.
    pub const fn new(bounds: MarkdownBounds) -> Self {
        Self { bounds }
    }

    /// Returns the bounds this loader enforces.
    pub const fn bounds(self) -> MarkdownBounds {
        self.bounds
    }

    /// Builds one immutable Markdown snapshot from already-read bytes.
    pub fn load(
        &self,
        source: MarkdownSource,
        declared_source_bytes: usize,
        bytes: &[u8],
        cancellation: &MarkdownCancellation,
    ) -> Result<MarkdownDocument, MarkdownLoadError> {
        ensure_not_cancelled(cancellation)?;

        let actual_source_bytes = bytes.len();
        let checked_source_bytes = declared_source_bytes.max(actual_source_bytes);
        if checked_source_bytes > self.bounds.max_source_bytes {
            return Err(MarkdownLoadError::OversizeInput {
                limit_bytes: self.bounds.max_source_bytes,
                actual_bytes: checked_source_bytes,
            });
        }

        let bytes = bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes);
        let source_text = std::str::from_utf8(bytes)
            .map_err(|_| MarkdownLoadError::InvalidUtf8)?
            .to_owned();
        if source_text.is_empty() {
            return Ok(MarkdownDocument::new(
                source,
                source_text,
                Vec::new(),
                Vec::new(),
            ));
        }
        if looks_binary(&source_text) {
            return Err(MarkdownLoadError::BinaryContent);
        }

        ensure_not_cancelled(cancellation)?;
        let source_index = SourceIndex::new(&source_text, self.bounds.max_decoded_lines)?;
        let parsed = ParsedDocument::parse(
            &source,
            &source_text,
            &source_index,
            self.bounds,
            cancellation,
        )?;
        Ok(
            MarkdownDocument::new(source, source_text, parsed.blocks, parsed.headings)
                .with_resources(parsed.resource_references, source_index),
        )
    }
}

impl Default for MarkdownLoader {
    fn default() -> Self {
        Self::new(MarkdownBounds::DEFAULT)
    }
}

/// One successfully loaded, decoded, and parsed Markdown snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownDocument {
    source: MarkdownSource,
    source_text: String,
    blocks: Vec<Block>,
    headings: Vec<Heading>,
    resource_references: Vec<ResourceReference>,
    line_starts: Vec<usize>,
    line_char_offsets: Vec<usize>,
    source_char_count: usize,
}

impl MarkdownDocument {
    fn new(
        source: MarkdownSource,
        source_text: String,
        blocks: Vec<Block>,
        headings: Vec<Heading>,
    ) -> Self {
        Self {
            source,
            source_text,
            blocks,
            headings,
            resource_references: Vec::new(),
            line_starts: vec![0],
            line_char_offsets: vec![0],
            source_char_count: 0,
        }
    }

    fn with_resources(
        mut self,
        resource_references: Vec<ResourceReference>,
        source_index: SourceIndex,
    ) -> Self {
        self.resource_references = resource_references;
        self.line_starts = source_index.line_starts;
        self.line_char_offsets = source_index.line_char_offsets;
        self.source_char_count = source_index.source_char_count;
        self
    }

    pub fn source(&self) -> &MarkdownSource {
        &self.source
    }

    /// Returns the exact decoded snapshot text after optional BOM stripping.
    pub fn source_text(&self) -> &str {
        &self.source_text
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    pub fn headings(&self) -> &[Heading] {
        &self.headings
    }

    pub fn resource_references(&self) -> &[ResourceReference] {
        &self.resource_references
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    pub fn source_char_count(&self) -> usize {
        self.source_char_count
    }

    /// Maps one byte range in the decoded source text to line/column metadata.
    pub fn source_span(&self, byte_range: Range<usize>) -> Option<SourceSpan> {
        if byte_range.start > byte_range.end || byte_range.end > self.source_text.len() {
            return None;
        }
        Some(self.source_span_unchecked(byte_range))
    }

    /// Finds non-overlapping literal matches in the decoded source text.
    pub fn find_matches(&self, query: &str) -> Vec<TextMatch> {
        if query.is_empty() {
            return Vec::new();
        }

        self.source_text
            .match_indices(query)
            .map(|(start, matched)| TextMatch {
                span: self.source_span_unchecked(start..start + matched.len()),
            })
            .collect()
    }

    /// Returns the deepest heading whose section currently owns `byte_offset`.
    pub fn nearest_heading_at_byte(&self, byte_offset: usize) -> Option<&Heading> {
        self.headings.iter().rev().find(|heading| {
            heading.section_start_byte <= byte_offset && byte_offset < heading.section_end_byte
        })
    }

    /// Returns the index of the deepest heading whose section owns `byte_offset`.
    pub fn nearest_heading_index_at_byte(&self, byte_offset: usize) -> Option<usize> {
        self.headings.iter().rposition(|heading| {
            heading.section_start_byte <= byte_offset && byte_offset < heading.section_end_byte
        })
    }

    fn source_span_unchecked(&self, byte_range: Range<usize>) -> SourceSpan {
        SourceSpan {
            byte_start: byte_range.start,
            byte_end: byte_range.end,
            start: self.source_position(byte_range.start),
            end: self.source_position(byte_range.end),
        }
    }

    fn source_position(&self, byte_offset: usize) -> SourcePosition {
        let line_index = self
            .line_starts
            .partition_point(|line_start| *line_start <= byte_offset)
            .saturating_sub(1);
        let line_start = self.line_starts[line_index];
        let char_offset = self.line_char_offsets[line_index]
            + self.source_text[line_start..byte_offset].chars().count();
        let column = self.source_text[line_start..byte_offset].chars().count();
        SourcePosition {
            byte_offset,
            char_offset,
            line_index,
            column,
        }
    }
}

/// One block-level node in the Markdown preview model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Block {
    Paragraph(TextBlock),
    Heading(HeadingBlock),
    BlockQuote(BlockQuoteBlock),
    List(ListBlock),
    Table(TableBlock),
    CodeBlock(CodeBlock),
    Html(RawHtmlBlock),
    Rule { span: SourceSpan },
}

impl Block {
    pub fn span(&self) -> SourceSpan {
        match self {
            Self::Paragraph(block) => block.span,
            Self::Heading(block) => block.span,
            Self::BlockQuote(block) => block.span,
            Self::List(block) => block.span,
            Self::Table(block) => block.span,
            Self::CodeBlock(block) => block.span,
            Self::Html(block) => block.span,
            Self::Rule { span } => *span,
        }
    }
}

/// Preview paragraph or similar inline-only text container.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextBlock {
    span: SourceSpan,
    plain_text: String,
    inlines: Vec<Inline>,
}

impl TextBlock {
    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub fn plain_text(&self) -> &str {
        &self.plain_text
    }

    pub fn inlines(&self) -> &[Inline] {
        &self.inlines
    }
}

/// Preview heading block linked to a stable outline entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadingBlock {
    span: SourceSpan,
    heading_index: usize,
    level: u8,
    plain_text: String,
    inlines: Vec<Inline>,
}

impl HeadingBlock {
    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub const fn heading_index(&self) -> usize {
        self.heading_index
    }

    pub const fn level(&self) -> u8 {
        self.level
    }

    pub fn plain_text(&self) -> &str {
        &self.plain_text
    }

    pub fn inlines(&self) -> &[Inline] {
        &self.inlines
    }
}

/// Preview block quote container.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockQuoteBlock {
    span: SourceSpan,
    blocks: Vec<Block>,
}

impl BlockQuoteBlock {
    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }
}

/// Preview list container.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListBlock {
    span: SourceSpan,
    kind: ListKind,
    items: Vec<ListItem>,
}

impl ListBlock {
    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub fn kind(&self) -> ListKind {
        self.kind
    }

    pub fn items(&self) -> &[ListItem] {
        &self.items
    }
}

/// Ordered or unordered list metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListKind {
    Bullet,
    Ordered { first_item_number: u64 },
}

/// One list item, optionally representing a task-list checkbox state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListItem {
    span: SourceSpan,
    task_state: Option<TaskState>,
    blocks: Vec<Block>,
}

impl ListItem {
    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub fn task_state(&self) -> Option<TaskState> {
        self.task_state
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }
}

/// Inert task-list state rendered by Preview.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskState {
    Checked,
    Unchecked,
}

/// Preview table model with bounded cell count.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableBlock {
    span: SourceSpan,
    alignments: Vec<TableAlignment>,
    rows: Vec<TableRow>,
}

impl TableBlock {
    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub fn alignments(&self) -> &[TableAlignment] {
        &self.alignments
    }

    pub fn rows(&self) -> &[TableRow] {
        &self.rows
    }
}

/// Stable table-cell alignment value without exposing `pulldown-cmark` types.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableAlignment {
    None,
    Left,
    Center,
    Right,
}

/// One table row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableRow {
    span: SourceSpan,
    is_header: bool,
    cells: Vec<TableCell>,
}

impl TableRow {
    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub const fn is_header(&self) -> bool {
        self.is_header
    }

    pub fn cells(&self) -> &[TableCell] {
        &self.cells
    }
}

/// One table cell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableCell {
    span: SourceSpan,
    plain_text: String,
    inlines: Vec<Inline>,
}

impl TableCell {
    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub fn plain_text(&self) -> &str {
        &self.plain_text
    }

    pub fn inlines(&self) -> &[Inline] {
        &self.inlines
    }
}

/// One fenced or indented code block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeBlock {
    span: SourceSpan,
    language: Option<String>,
    code_text: String,
    highlighted_lines: Vec<HighlightedCodeLine>,
}

impl CodeBlock {
    pub fn span(&self) -> SourceSpan {
        self.span
    }

    /// Returns the normalized fenced language label when one is supported.
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Returns the exact decoded code payload only, excluding UI chrome.
    pub fn code_text(&self) -> &str {
        &self.code_text
    }

    pub fn highlighted_lines(&self) -> &[HighlightedCodeLine] {
        &self.highlighted_lines
    }
}

/// Highlighted code for one line, preserving exact source text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HighlightedCodeLine {
    text: String,
    spans: Vec<HighlightedSpan>,
}

impl HighlightedCodeLine {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn spans(&self) -> &[HighlightedSpan] {
        &self.spans
    }
}

/// One syntax-highlighted span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HighlightedSpan {
    text: String,
    style: HighlightStyle,
}

impl HighlightedSpan {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn style(&self) -> HighlightStyle {
        self.style
    }
}

/// UI-agnostic color and font metadata derived from `syntect`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HighlightStyle {
    foreground: RgbaColor,
    background: RgbaColor,
    bold: bool,
    italic: bool,
    underline: bool,
}

impl HighlightStyle {
    pub fn foreground(self) -> RgbaColor {
        self.foreground
    }

    pub fn background(self) -> RgbaColor {
        self.background
    }

    pub const fn bold(self) -> bool {
        self.bold
    }

    pub const fn italic(self) -> bool {
        self.italic
    }

    pub const fn underline(self) -> bool {
        self.underline
    }
}

/// RGBA color derived from `syntect` themes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RgbaColor {
    red: u8,
    green: u8,
    blue: u8,
    alpha: u8,
}

impl RgbaColor {
    pub const fn red(self) -> u8 {
        self.red
    }

    pub const fn green(self) -> u8 {
        self.green
    }

    pub const fn blue(self) -> u8 {
        self.blue
    }

    pub const fn alpha(self) -> u8 {
        self.alpha
    }
}

/// Inert raw HTML preserved as literal source text.
///
/// The viewer must never interpret this as DOM or embedded content; it exists
/// only so Preview can render escaped source or an explicit placeholder without
/// reparsing the original document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawHtmlBlock {
    span: SourceSpan,
    literal: String,
}

impl RawHtmlBlock {
    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub fn literal(&self) -> &str {
        &self.literal
    }
}

/// One inline node in the preview model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Inline {
    Text(TextInline),
    Code(TextInline),
    Emphasis(ContainerInline),
    Strong(ContainerInline),
    Strikethrough(ContainerInline),
    Link(LinkInline),
    Image(ImageInline),
    RawHtml(RawHtmlInline),
    SoftBreak { span: SourceSpan },
    HardBreak { span: SourceSpan },
}

impl Inline {
    pub fn span(&self) -> SourceSpan {
        match self {
            Self::Text(inline) => inline.span,
            Self::Code(inline) => inline.span,
            Self::Emphasis(inline) => inline.span,
            Self::Strong(inline) => inline.span,
            Self::Strikethrough(inline) => inline.span,
            Self::Link(inline) => inline.span,
            Self::Image(inline) => inline.span,
            Self::RawHtml(inline) => inline.span,
            Self::SoftBreak { span } | Self::HardBreak { span } => *span,
        }
    }

    pub fn plain_text(&self) -> String {
        let mut plain_text = String::new();
        append_inline_plain_text(self, &mut plain_text);
        plain_text
    }
}

/// One inline text leaf.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextInline {
    span: SourceSpan,
    text: String,
}

impl TextInline {
    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

/// One inline container.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerInline {
    span: SourceSpan,
    inlines: Vec<Inline>,
}

impl ContainerInline {
    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub fn inlines(&self) -> &[Inline] {
        &self.inlines
    }
}

/// One link inline referencing a stable resource entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkInline {
    span: SourceSpan,
    reference_index: usize,
    plain_text: String,
    inlines: Vec<Inline>,
}

impl LinkInline {
    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub const fn reference_index(&self) -> usize {
        self.reference_index
    }

    pub fn plain_text(&self) -> &str {
        &self.plain_text
    }

    pub fn inlines(&self) -> &[Inline] {
        &self.inlines
    }
}

/// One image inline referencing a stable resource entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageInline {
    span: SourceSpan,
    reference_index: usize,
    alt_text: String,
}

impl ImageInline {
    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub const fn reference_index(&self) -> usize {
        self.reference_index
    }

    pub fn alt_text(&self) -> &str {
        &self.alt_text
    }
}

/// Inert inline raw HTML preserved as literal source text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawHtmlInline {
    span: SourceSpan,
    literal: String,
}

impl RawHtmlInline {
    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub fn literal(&self) -> &str {
        &self.literal
    }
}

/// Stable heading-outline entry with section ownership metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Heading {
    level: u8,
    text: String,
    anchor: String,
    span: SourceSpan,
    parent_index: Option<usize>,
    section_start_byte: usize,
    section_end_byte: usize,
}

impl Heading {
    pub const fn level(&self) -> u8 {
        self.level
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn anchor(&self) -> &str {
        &self.anchor
    }

    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub const fn parent_index(&self) -> Option<usize> {
        self.parent_index
    }

    pub const fn section_start_byte(&self) -> usize {
        self.section_start_byte
    }

    pub const fn section_end_byte(&self) -> usize {
        self.section_end_byte
    }
}

/// One typed resource reference captured from a link or image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceReference {
    kind: ResourceReferenceKind,
    class: ResourceReferenceClass,
    target: String,
    title: Option<String>,
    label_text: Option<String>,
    alt_text: Option<String>,
    span: SourceSpan,
    is_autolink: bool,
}

impl ResourceReference {
    pub fn kind(&self) -> ResourceReferenceKind {
        self.kind
    }

    pub fn class(&self) -> ResourceReferenceClass {
        self.class
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn label_text(&self) -> Option<&str> {
        self.label_text.as_deref()
    }

    pub fn alt_text(&self) -> Option<&str> {
        self.alt_text.as_deref()
    }

    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub const fn is_autolink(&self) -> bool {
        self.is_autolink
    }
}

/// Link or image reference kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ResourceReferenceKind {
    Link,
    Image,
}

/// Policy class used by the future UI/resource-loading layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ResourceReferenceClass {
    DocumentFragment,
    LocalRelative,
    RemoteRelativeViaSftpOrigin,
    HttpsAbsolute,
    DangerousScheme,
}

/// One Find match in decoded source text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextMatch {
    span: SourceSpan,
}

impl TextMatch {
    pub fn span(&self) -> SourceSpan {
        self.span
    }
}

/// Line/column metadata for one byte range in decoded source text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceSpan {
    byte_start: usize,
    byte_end: usize,
    start: SourcePosition,
    end: SourcePosition,
}

impl SourceSpan {
    pub fn byte_range(self) -> Range<usize> {
        self.byte_start..self.byte_end
    }

    pub const fn start(self) -> SourcePosition {
        self.start
    }

    pub const fn end(self) -> SourcePosition {
        self.end
    }
}

/// One decoded source position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourcePosition {
    byte_offset: usize,
    char_offset: usize,
    line_index: usize,
    column: usize,
}

impl SourcePosition {
    pub const fn byte_offset(self) -> usize {
        self.byte_offset
    }

    pub const fn char_offset(self) -> usize {
        self.char_offset
    }

    pub const fn line_index(self) -> usize {
        self.line_index
    }

    pub const fn column(self) -> usize {
        self.column
    }
}

/// Stable, content-free loading failure categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLoadError {
    Cancelled,
    InvalidUtf8,
    BinaryContent,
    OversizeInput {
        limit_bytes: usize,
        actual_bytes: usize,
    },
    TooManyLines {
        limit: usize,
        actual: usize,
    },
    ExcessiveNesting {
        limit: usize,
        actual: usize,
    },
    TooManyTableCells {
        limit: usize,
        actual: usize,
    },
    CodeBlockTooLarge {
        limit_bytes: usize,
        actual_bytes: usize,
    },
    TooManyResourceReferences {
        limit: usize,
        actual: usize,
    },
    ParseModelInvariant,
}

impl fmt::Display for MarkdownLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("Markdown loading was cancelled"),
            Self::InvalidUtf8 => formatter.write_str("Markdown source is not valid UTF-8"),
            Self::BinaryContent => formatter.write_str("Markdown source appears to contain binary content"),
            Self::OversizeInput { limit_bytes, actual_bytes } => write!(
                formatter,
                "Markdown source exceeds the {}-byte limit ({actual_bytes} bytes declared or received)",
                limit_bytes
            ),
            Self::TooManyLines { limit, actual } => write!(
                formatter,
                "Markdown source exceeds the {}-line limit ({actual} lines)",
                limit
            ),
            Self::ExcessiveNesting { limit, actual } => write!(
                formatter,
                "Markdown source exceeds the maximum nesting depth of {limit} (observed {actual})"
            ),
            Self::TooManyTableCells { limit, actual } => write!(
                formatter,
                "Markdown source exceeds the maximum table-cell count of {limit} (observed {actual})"
            ),
            Self::CodeBlockTooLarge { limit_bytes, actual_bytes } => write!(
                formatter,
                "Markdown source exceeds the maximum fenced code block size of {} bytes ({actual_bytes} bytes)",
                limit_bytes
            ),
            Self::TooManyResourceReferences { limit, actual } => write!(
                formatter,
                "Markdown source exceeds the maximum resource-reference count of {limit} (observed {actual})"
            ),
            Self::ParseModelInvariant => formatter.write_str("Markdown source could not be projected into the bounded document model"),
        }
    }
}

impl std::error::Error for MarkdownLoadError {}

#[derive(Clone, Debug)]
struct ParsedDocument {
    blocks: Vec<Block>,
    headings: Vec<Heading>,
    resource_references: Vec<ResourceReference>,
}

impl ParsedDocument {
    fn parse(
        source: &MarkdownSource,
        source_text: &str,
        source_index: &SourceIndex,
        bounds: MarkdownBounds,
        cancellation: &MarkdownCancellation,
    ) -> Result<Self, MarkdownLoadError> {
        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_TASKLISTS);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_GFM);

        let parser = Parser::new_ext(source_text, options).into_offset_iter();
        let mut builder = DocumentBuilder::new(source, source_text, source_index, bounds);
        for (index, (event, range)) in parser.enumerate() {
            if index % 128 == 0 {
                ensure_not_cancelled(cancellation)?;
            }
            builder.push(event, range)?;
        }
        builder.finish(source_text.len())
    }
}

#[derive(Clone, Debug)]
struct SourceIndex {
    line_starts: Vec<usize>,
    line_char_offsets: Vec<usize>,
    source_char_count: usize,
}

impl SourceIndex {
    fn new(source_text: &str, max_lines: usize) -> Result<Self, MarkdownLoadError> {
        let mut line_starts = vec![0];
        let mut line_char_offsets = vec![0];
        let mut chars_seen = 0;

        for (index, character) in source_text.char_indices() {
            chars_seen += 1;
            if character == '\n' {
                line_starts.push(index + character.len_utf8());
                line_char_offsets.push(chars_seen);
            }
        }

        if line_starts.len() > max_lines {
            return Err(MarkdownLoadError::TooManyLines {
                limit: max_lines,
                actual: line_starts.len(),
            });
        }

        Ok(Self {
            line_starts,
            line_char_offsets,
            source_char_count: source_text.chars().count(),
        })
    }

    fn span(&self, source_text: &str, byte_range: Range<usize>) -> SourceSpan {
        SourceSpan {
            byte_start: byte_range.start,
            byte_end: byte_range.end,
            start: self.position(source_text, byte_range.start),
            end: self.position(source_text, byte_range.end),
        }
    }

    fn position(&self, source_text: &str, byte_offset: usize) -> SourcePosition {
        let line_index = self
            .line_starts
            .partition_point(|line_start| *line_start <= byte_offset)
            .saturating_sub(1);
        let line_start = self.line_starts[line_index];
        let column = source_text[line_start..byte_offset].chars().count();
        SourcePosition {
            byte_offset,
            char_offset: self.line_char_offsets[line_index] + column,
            line_index,
            column,
        }
    }
}

#[derive(Clone, Debug)]
struct DocumentBuilder<'a> {
    source: &'a MarkdownSource,
    source_text: &'a str,
    source_index: &'a SourceIndex,
    bounds: MarkdownBounds,
    root: Vec<Block>,
    stack: Vec<Frame>,
    headings: Vec<Heading>,
    anchor_counts: BTreeMap<String, usize>,
    resource_references: Vec<ResourceReference>,
    distinct_resources:
        std::collections::BTreeSet<(ResourceReferenceKind, ResourceReferenceClass, String)>,
    table_cells: usize,
    current_block_depth: usize,
}

impl<'a> DocumentBuilder<'a> {
    fn new(
        source: &'a MarkdownSource,
        source_text: &'a str,
        source_index: &'a SourceIndex,
        bounds: MarkdownBounds,
    ) -> Self {
        Self {
            source,
            source_text,
            source_index,
            bounds,
            root: Vec::new(),
            stack: Vec::new(),
            headings: Vec::new(),
            anchor_counts: BTreeMap::new(),
            resource_references: Vec::new(),
            distinct_resources: std::collections::BTreeSet::new(),
            table_cells: 0,
            current_block_depth: 0,
        }
    }

    fn push(&mut self, event: Event<'_>, range: Range<usize>) -> Result<(), MarkdownLoadError> {
        match event {
            Event::Start(tag) => self.start_tag(tag, range),
            Event::End(tag_end) => self.end_tag(tag_end, range),
            Event::Text(text) => self.push_text(text.into_string(), range),
            Event::Code(text) => self.push_inline(Inline::Code(TextInline {
                span: self.span(range),
                text: text.into_string(),
            })),
            Event::Html(text) => self.push_html_block_line(text.into_string(), range),
            Event::InlineHtml(text) => self.push_inline(Inline::RawHtml(RawHtmlInline {
                span: self.span(range),
                literal: text.into_string(),
            })),
            Event::SoftBreak => self.push_inline(Inline::SoftBreak {
                span: self.span(range),
            }),
            Event::HardBreak => self.push_inline(Inline::HardBreak {
                span: self.span(range),
            }),
            Event::Rule => self.push_block(Block::Rule {
                span: self.span(range),
            }),
            Event::TaskListMarker(checked) => {
                let state = if checked {
                    TaskState::Checked
                } else {
                    TaskState::Unchecked
                };
                let Some(item) = self.stack.iter_mut().rev().find_map(|frame| match frame {
                    Frame::ListItem(item) => Some(item),
                    _ => None,
                }) else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                item.task_state = Some(state);
                Ok(())
            }
            Event::InlineMath(_) | Event::DisplayMath(_) | Event::FootnoteReference(_) => self
                .push_inline(Inline::Text(TextInline {
                    span: self.span(range.clone()),
                    text: self.source_slice(range).to_owned(),
                })),
        }
    }

    fn finish(mut self, source_len: usize) -> Result<ParsedDocument, MarkdownLoadError> {
        if !self.stack.is_empty() {
            return Err(MarkdownLoadError::ParseModelInvariant);
        }
        for index in 0..self.headings.len() {
            let current_level = self.headings[index].level;
            let end = self
                .headings
                .iter()
                .skip(index + 1)
                .find(|heading| heading.level <= current_level)
                .map(|heading| heading.span.start().byte_offset())
                .unwrap_or(source_len);
            self.headings[index].section_end_byte = end;
        }
        Ok(ParsedDocument {
            blocks: self.root,
            headings: self.headings,
            resource_references: self.resource_references,
        })
    }

    fn start_tag(&mut self, tag: Tag<'_>, range: Range<usize>) -> Result<(), MarkdownLoadError> {
        if matches!(
            tag,
            Tag::Paragraph
                | Tag::Heading { .. }
                | Tag::BlockQuote(_)
                | Tag::CodeBlock(_)
                | Tag::HtmlBlock
                | Tag::List(_)
                | Tag::Table(_)
                | Tag::TableHead
                | Tag::TableRow
                | Tag::TableCell
        ) {
            self.flush_open_list_item_inlines(range.start)?;
        }
        match tag {
            Tag::Paragraph => {
                self.enter_block()?;
                self.stack
                    .push(Frame::Paragraph(TextContainerFrame::new(range.start)));
            }
            Tag::Heading { level, .. } => {
                self.enter_block()?;
                self.stack.push(Frame::Heading(HeadingFrame::new(
                    range.start,
                    heading_level(level),
                )));
            }
            Tag::BlockQuote(_) => {
                self.enter_block()?;
                self.stack
                    .push(Frame::BlockQuote(BlockContainerFrame::new(range.start)));
            }
            Tag::CodeBlock(kind) => {
                self.enter_block()?;
                self.stack
                    .push(Frame::CodeBlock(CodeBlockFrame::new(range.start, kind)));
            }
            Tag::HtmlBlock => {
                self.enter_block()?;
                self.stack
                    .push(Frame::HtmlBlock(HtmlBlockFrame::new(range.start)));
            }
            Tag::List(first_item_number) => {
                self.enter_block()?;
                self.stack
                    .push(Frame::List(ListFrame::new(range.start, first_item_number)));
            }
            Tag::Item => {
                self.enter_block()?;
                self.stack
                    .push(Frame::ListItem(ListItemFrame::new(range.start)));
            }
            Tag::Table(alignments) => {
                self.enter_block()?;
                self.stack
                    .push(Frame::Table(TableFrame::new(range.start, alignments)));
            }
            Tag::TableHead => {
                self.enter_block()?;
                self.stack
                    .push(Frame::TableRow(TableRowFrame::new(range.start, true)));
            }
            Tag::TableRow => {
                self.enter_block()?;
                self.stack
                    .push(Frame::TableRow(TableRowFrame::new(range.start, false)));
            }
            Tag::TableCell => {
                self.enter_block()?;
                self.table_cells += 1;
                if self.table_cells > self.bounds.max_table_cells {
                    return Err(MarkdownLoadError::TooManyTableCells {
                        limit: self.bounds.max_table_cells,
                        actual: self.table_cells,
                    });
                }
                self.stack
                    .push(Frame::TableCell(TextContainerFrame::new(range.start)));
            }
            Tag::Emphasis => self
                .stack
                .push(Frame::Emphasis(InlineContainerFrame::new(range.start))),
            Tag::Strong => self
                .stack
                .push(Frame::Strong(InlineContainerFrame::new(range.start))),
            Tag::Strikethrough => self
                .stack
                .push(Frame::Strikethrough(InlineContainerFrame::new(range.start))),
            Tag::Link {
                link_type,
                dest_url,
                title,
                ..
            } => self.stack.push(Frame::Link(LinkFrame::new(
                range.start,
                dest_url.into_string(),
                option_string(title.into_string()),
                matches!(link_type, pulldown_cmark::LinkType::Autolink),
            ))),
            Tag::Image {
                link_type,
                dest_url,
                title,
                ..
            } => self.stack.push(Frame::Image(ImageFrame::new(
                range.start,
                dest_url.into_string(),
                option_string(title.into_string()),
                matches!(link_type, pulldown_cmark::LinkType::Autolink),
            ))),
            _ => {}
        }
        Ok(())
    }

    fn end_tag(&mut self, tag_end: TagEnd, range: Range<usize>) -> Result<(), MarkdownLoadError> {
        match tag_end {
            TagEnd::Paragraph => {
                let Some(Frame::Paragraph(frame)) = self.stack.pop() else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                self.exit_block();
                self.push_block(Block::Paragraph(TextBlock {
                    span: self.span(frame.start..range.end),
                    plain_text: frame.plain_text(),
                    inlines: frame.inlines,
                }))
            }
            TagEnd::Heading(tag_level) => {
                let Some(Frame::Heading(frame)) = self.stack.pop() else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                self.exit_block();
                let start = frame.start();
                let level = frame.level;
                let plain_text = frame.plain_text();
                let inlines = frame.inlines();
                let span = self.span(start..range.end);
                let heading_index = self.headings.len();
                let parent_index = self
                    .headings
                    .iter()
                    .enumerate()
                    .rev()
                    .find_map(|(index, heading)| (heading.level < level).then_some(index));
                let anchor = self.allocate_anchor(&plain_text);
                self.headings.push(Heading {
                    level,
                    text: plain_text.clone(),
                    anchor,
                    span,
                    parent_index,
                    section_start_byte: span.start().byte_offset(),
                    section_end_byte: range.end,
                });
                self.push_block(Block::Heading(HeadingBlock {
                    span,
                    heading_index,
                    level: heading_level(tag_level),
                    plain_text,
                    inlines,
                }))
            }
            TagEnd::BlockQuote(_) => {
                let Some(Frame::BlockQuote(frame)) = self.stack.pop() else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                self.exit_block();
                self.push_block(Block::BlockQuote(BlockQuoteBlock {
                    span: self.span(frame.start..range.end),
                    blocks: frame.blocks,
                }))
            }
            TagEnd::CodeBlock => {
                let Some(Frame::CodeBlock(frame)) = self.stack.pop() else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                self.exit_block();
                let code_text = frame.code_text;
                let actual_bytes = code_text.len();
                if actual_bytes > self.bounds.max_code_block_bytes {
                    return Err(MarkdownLoadError::CodeBlockTooLarge {
                        limit_bytes: self.bounds.max_code_block_bytes,
                        actual_bytes,
                    });
                }
                let language = normalize_code_language(frame.kind.as_deref());
                let highlighted_lines = highlight_code(language.as_deref(), &code_text);
                self.push_block(Block::CodeBlock(CodeBlock {
                    span: self.span(frame.start..range.end),
                    language,
                    code_text,
                    highlighted_lines,
                }))
            }
            TagEnd::HtmlBlock => {
                let Some(Frame::HtmlBlock(frame)) = self.stack.pop() else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                self.exit_block();
                self.push_block(Block::Html(RawHtmlBlock {
                    span: self.span(frame.start..range.end),
                    literal: frame.literal,
                }))
            }
            TagEnd::List(is_ordered) => {
                let Some(Frame::List(frame)) = self.stack.pop() else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                self.exit_block();
                let kind = if is_ordered {
                    ListKind::Ordered {
                        first_item_number: frame.first_item_number.unwrap_or(1),
                    }
                } else {
                    ListKind::Bullet
                };
                self.push_block(Block::List(ListBlock {
                    span: self.span(frame.start..range.end),
                    kind,
                    items: frame.items,
                }))
            }
            TagEnd::Item => {
                self.flush_open_list_item_inlines(range.end)?;
                let Some(Frame::ListItem(frame)) = self.stack.pop() else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                self.exit_block();
                let span = self.span(frame.start..range.end);
                let Some(Frame::List(list)) = self.stack.last_mut() else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                list.items.push(ListItem {
                    span,
                    task_state: frame.task_state,
                    blocks: frame.blocks,
                });
                Ok(())
            }
            TagEnd::Table => {
                let Some(Frame::Table(frame)) = self.stack.pop() else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                self.exit_block();
                self.push_block(Block::Table(TableBlock {
                    span: self.span(frame.start..range.end),
                    alignments: frame.alignments,
                    rows: frame.rows,
                }))
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                let Some(Frame::TableRow(frame)) = self.stack.pop() else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                self.exit_block();
                let span = self.span(frame.start..range.end);
                let Some(Frame::Table(table)) = self.stack.last_mut() else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                table.rows.push(TableRow {
                    span,
                    is_header: frame.is_header,
                    cells: frame.cells,
                });
                Ok(())
            }
            TagEnd::TableCell => {
                let Some(Frame::TableCell(frame)) = self.stack.pop() else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                self.exit_block();
                let span = self.span(frame.start..range.end);
                let Some(Frame::TableRow(row)) = self.stack.last_mut() else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                row.cells.push(TableCell {
                    span,
                    plain_text: frame.plain_text(),
                    inlines: frame.inlines,
                });
                Ok(())
            }
            TagEnd::Emphasis => self.finish_inline_container(range, FrameKind::Emphasis),
            TagEnd::Strong => self.finish_inline_container(range, FrameKind::Strong),
            TagEnd::Strikethrough => self.finish_inline_container(range, FrameKind::Strikethrough),
            TagEnd::Link => {
                let Some(Frame::Link(frame)) = self.stack.pop() else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                let LinkFrame {
                    start,
                    target,
                    title,
                    is_autolink,
                    inline,
                } = frame;
                let inlines = inline.inlines;
                let span = self.span(start..range.end);
                let plain_text = inline_plain_text(&inlines);
                let reference_index = self.push_resource_reference(ResourceReference {
                    kind: ResourceReferenceKind::Link,
                    class: classify_target(self.source, &target),
                    target,
                    title,
                    label_text: option_string(plain_text.clone()),
                    alt_text: None,
                    span,
                    is_autolink,
                })?;
                self.push_inline(Inline::Link(LinkInline {
                    span,
                    reference_index,
                    plain_text,
                    inlines,
                }))
            }
            TagEnd::Image => {
                let Some(Frame::Image(frame)) = self.stack.pop() else {
                    return Err(MarkdownLoadError::ParseModelInvariant);
                };
                let ImageFrame {
                    start,
                    target,
                    title,
                    is_autolink,
                    inline,
                } = frame;
                let inlines = inline.inlines;
                let span = self.span(start..range.end);
                let alt_text = inline_plain_text(&inlines);
                let reference_index = self.push_resource_reference(ResourceReference {
                    kind: ResourceReferenceKind::Image,
                    class: classify_target(self.source, &target),
                    target,
                    title,
                    label_text: None,
                    alt_text: option_string(alt_text.clone()),
                    span,
                    is_autolink,
                })?;
                self.push_inline(Inline::Image(ImageInline {
                    span,
                    reference_index,
                    alt_text,
                }))
            }
            _ => Ok(()),
        }
    }

    fn finish_inline_container(
        &mut self,
        range: Range<usize>,
        kind: FrameKind,
    ) -> Result<(), MarkdownLoadError> {
        let frame = match kind {
            FrameKind::Emphasis => match self.stack.pop() {
                Some(Frame::Emphasis(frame)) => frame,
                _ => return Err(MarkdownLoadError::ParseModelInvariant),
            },
            FrameKind::Strong => match self.stack.pop() {
                Some(Frame::Strong(frame)) => frame,
                _ => return Err(MarkdownLoadError::ParseModelInvariant),
            },
            FrameKind::Strikethrough => match self.stack.pop() {
                Some(Frame::Strikethrough(frame)) => frame,
                _ => return Err(MarkdownLoadError::ParseModelInvariant),
            },
        };
        let inline = match kind {
            FrameKind::Emphasis => Inline::Emphasis(ContainerInline {
                span: self.span(frame.start..range.end),
                inlines: frame.inlines,
            }),
            FrameKind::Strong => Inline::Strong(ContainerInline {
                span: self.span(frame.start..range.end),
                inlines: frame.inlines,
            }),
            FrameKind::Strikethrough => Inline::Strikethrough(ContainerInline {
                span: self.span(frame.start..range.end),
                inlines: frame.inlines,
            }),
        };
        self.push_inline(inline)
    }

    fn push_text(&mut self, text: String, range: Range<usize>) -> Result<(), MarkdownLoadError> {
        if text.is_empty() {
            return Ok(());
        }
        if let Some(Frame::CodeBlock(frame)) = self.stack.last_mut() {
            frame.code_text.push_str(&text);
            return Ok(());
        }
        if let Some(Frame::HtmlBlock(frame)) = self.stack.last_mut() {
            frame.literal.push_str(&text);
            return Ok(());
        }
        self.push_inline(Inline::Text(TextInline {
            span: self.span(range),
            text,
        }))
    }

    fn push_html_block_line(
        &mut self,
        text: String,
        range: Range<usize>,
    ) -> Result<(), MarkdownLoadError> {
        if let Some(Frame::HtmlBlock(frame)) = self.stack.last_mut() {
            frame.literal.push_str(&text);
            return Ok(());
        }
        self.push_block(Block::Html(RawHtmlBlock {
            span: self.span(range),
            literal: text,
        }))
    }

    fn push_inline(&mut self, inline: Inline) -> Result<(), MarkdownLoadError> {
        let Some(frame) = self.stack.last_mut() else {
            return Err(MarkdownLoadError::ParseModelInvariant);
        };
        match frame {
            Frame::ListItem(item) => {
                let paragraph = item.tight_paragraph.get_or_insert_with(|| {
                    TextContainerFrame::new(inline.span().start().byte_offset())
                });
                paragraph.inlines.push(inline);
            }
            Frame::Paragraph(text)
            | Frame::TableCell(text)
            | Frame::Heading(HeadingFrame { inline: text, .. }) => {
                text.inlines.push(inline);
            }
            Frame::Emphasis(text)
            | Frame::Strong(text)
            | Frame::Strikethrough(text)
            | Frame::Link(LinkFrame { inline: text, .. })
            | Frame::Image(ImageFrame { inline: text, .. }) => {
                text.inlines.push(inline);
            }
            Frame::BlockQuote(_)
            | Frame::List(_)
            | Frame::Table(_)
            | Frame::TableRow(_)
            | Frame::CodeBlock(_)
            | Frame::HtmlBlock(_) => {
                return Err(MarkdownLoadError::ParseModelInvariant);
            }
        }
        Ok(())
    }

    fn push_block(&mut self, block: Block) -> Result<(), MarkdownLoadError> {
        if let Some(frame) = self.stack.last_mut() {
            match frame {
                Frame::BlockQuote(container) => {
                    container.blocks.push(block);
                    return Ok(());
                }
                Frame::ListItem(item) => {
                    item.blocks.push(block);
                    return Ok(());
                }
                Frame::List(_)
                | Frame::Table(_)
                | Frame::TableRow(_)
                | Frame::TableCell(_)
                | Frame::Paragraph(_)
                | Frame::Heading(_)
                | Frame::Emphasis(_)
                | Frame::Strong(_)
                | Frame::Strikethrough(_)
                | Frame::Link(_)
                | Frame::Image(_)
                | Frame::CodeBlock(_)
                | Frame::HtmlBlock(_) => return Err(MarkdownLoadError::ParseModelInvariant),
            }
        }
        self.root.push(block);
        Ok(())
    }

    fn flush_open_list_item_inlines(&mut self, end_byte: usize) -> Result<(), MarkdownLoadError> {
        let paragraph = {
            let Some(Frame::ListItem(item)) = self.stack.last_mut() else {
                return Ok(());
            };
            item.tight_paragraph.take()
        };
        let Some(paragraph) = paragraph else {
            return Ok(());
        };
        let block = Block::Paragraph(TextBlock {
            span: self.span(paragraph.start..end_byte),
            plain_text: paragraph.plain_text(),
            inlines: paragraph.inlines,
        });
        let Some(Frame::ListItem(item)) = self.stack.last_mut() else {
            return Err(MarkdownLoadError::ParseModelInvariant);
        };
        item.blocks.push(block);
        Ok(())
    }

    fn push_resource_reference(
        &mut self,
        reference: ResourceReference,
    ) -> Result<usize, MarkdownLoadError> {
        if reference.class != ResourceReferenceClass::DocumentFragment {
            let key = (reference.kind, reference.class, reference.target.clone());
            if self.distinct_resources.insert(key)
                && self.distinct_resources.len() > self.bounds.max_resource_references
            {
                return Err(MarkdownLoadError::TooManyResourceReferences {
                    limit: self.bounds.max_resource_references,
                    actual: self.distinct_resources.len(),
                });
            }
        }
        let index = self.resource_references.len();
        self.resource_references.push(reference);
        Ok(index)
    }

    fn allocate_anchor(&mut self, text: &str) -> String {
        let base = slugify_heading(text);
        let count = self.anchor_counts.entry(base.clone()).or_insert(0);
        let anchor = if *count == 0 {
            base
        } else {
            format!("{}-{}", base, count)
        };
        *count += 1;
        anchor
    }

    fn span(&self, byte_range: Range<usize>) -> SourceSpan {
        self.source_index.span(self.source_text(), byte_range)
    }

    fn source_text(&self) -> &str {
        self.source_text
    }

    fn source_slice(&self, byte_range: Range<usize>) -> &str {
        &self.source_text()[byte_range]
    }

    fn enter_block(&mut self) -> Result<(), MarkdownLoadError> {
        self.current_block_depth += 1;
        if self.current_block_depth > self.bounds.max_block_nesting_depth {
            return Err(MarkdownLoadError::ExcessiveNesting {
                limit: self.bounds.max_block_nesting_depth,
                actual: self.current_block_depth,
            });
        }
        Ok(())
    }

    fn exit_block(&mut self) {
        self.current_block_depth = self.current_block_depth.saturating_sub(1);
    }
}

#[derive(Clone, Copy, Debug)]
enum FrameKind {
    Emphasis,
    Strong,
    Strikethrough,
}

#[derive(Clone, Debug)]
enum Frame {
    Paragraph(TextContainerFrame),
    Heading(HeadingFrame),
    BlockQuote(BlockContainerFrame),
    List(ListFrame),
    ListItem(ListItemFrame),
    Table(TableFrame),
    TableRow(TableRowFrame),
    TableCell(TextContainerFrame),
    CodeBlock(CodeBlockFrame),
    HtmlBlock(HtmlBlockFrame),
    Emphasis(InlineContainerFrame),
    Strong(InlineContainerFrame),
    Strikethrough(InlineContainerFrame),
    Link(LinkFrame),
    Image(ImageFrame),
}

#[derive(Clone, Debug)]
struct TextContainerFrame {
    start: usize,
    inlines: Vec<Inline>,
}

impl TextContainerFrame {
    fn new(start: usize) -> Self {
        Self {
            start,
            inlines: Vec::new(),
        }
    }

    fn plain_text(&self) -> String {
        inline_plain_text(&self.inlines)
    }
}

#[derive(Clone, Debug)]
struct InlineContainerFrame {
    start: usize,
    inlines: Vec<Inline>,
}

impl InlineContainerFrame {
    fn new(start: usize) -> Self {
        Self {
            start,
            inlines: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct HeadingFrame {
    level: u8,
    inline: TextContainerFrame,
}

impl HeadingFrame {
    fn new(start: usize, level: u8) -> Self {
        Self {
            level,
            inline: TextContainerFrame::new(start),
        }
    }

    fn plain_text(&self) -> String {
        self.inline.plain_text()
    }

    fn start(&self) -> usize {
        self.inline.start
    }

    fn inlines(self) -> Vec<Inline> {
        self.inline.inlines
    }
}

#[derive(Clone, Debug)]
struct BlockContainerFrame {
    start: usize,
    blocks: Vec<Block>,
}

impl BlockContainerFrame {
    fn new(start: usize) -> Self {
        Self {
            start,
            blocks: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct ListFrame {
    start: usize,
    first_item_number: Option<u64>,
    items: Vec<ListItem>,
}

impl ListFrame {
    fn new(start: usize, first_item_number: Option<u64>) -> Self {
        Self {
            start,
            first_item_number,
            items: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct ListItemFrame {
    start: usize,
    task_state: Option<TaskState>,
    tight_paragraph: Option<TextContainerFrame>,
    blocks: Vec<Block>,
}

impl ListItemFrame {
    fn new(start: usize) -> Self {
        Self {
            start,
            task_state: None,
            tight_paragraph: None,
            blocks: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct TableFrame {
    start: usize,
    alignments: Vec<TableAlignment>,
    rows: Vec<TableRow>,
}

impl TableFrame {
    fn new(start: usize, alignments: Vec<Alignment>) -> Self {
        Self {
            start,
            alignments: alignments.into_iter().map(TableAlignment::from).collect(),
            rows: Vec::new(),
        }
    }
}

impl From<Alignment> for TableAlignment {
    fn from(value: Alignment) -> Self {
        match value {
            Alignment::None => Self::None,
            Alignment::Left => Self::Left,
            Alignment::Center => Self::Center,
            Alignment::Right => Self::Right,
        }
    }
}

#[derive(Clone, Debug)]
struct TableRowFrame {
    start: usize,
    is_header: bool,
    cells: Vec<TableCell>,
}

impl TableRowFrame {
    fn new(start: usize, is_header: bool) -> Self {
        Self {
            start,
            is_header,
            cells: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct CodeBlockFrame {
    start: usize,
    kind: Option<String>,
    code_text: String,
}

impl CodeBlockFrame {
    fn new(start: usize, kind: CodeBlockKind<'_>) -> Self {
        Self {
            start,
            kind: match kind {
                CodeBlockKind::Indented => None,
                CodeBlockKind::Fenced(info) => Some(info.into_string()),
            },
            code_text: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct HtmlBlockFrame {
    start: usize,
    literal: String,
}

impl HtmlBlockFrame {
    fn new(start: usize) -> Self {
        Self {
            start,
            literal: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct LinkFrame {
    start: usize,
    target: String,
    title: Option<String>,
    is_autolink: bool,
    inline: InlineContainerFrame,
}

impl LinkFrame {
    fn new(start: usize, target: String, title: Option<String>, is_autolink: bool) -> Self {
        Self {
            start,
            target,
            title,
            is_autolink,
            inline: InlineContainerFrame::new(start),
        }
    }
}

#[derive(Clone, Debug)]
struct ImageFrame {
    start: usize,
    target: String,
    title: Option<String>,
    is_autolink: bool,
    inline: InlineContainerFrame,
}

impl ImageFrame {
    fn new(start: usize, target: String, title: Option<String>, is_autolink: bool) -> Self {
        Self {
            start,
            target,
            title,
            is_autolink,
            inline: InlineContainerFrame::new(start),
        }
    }
}

fn ensure_not_cancelled(cancellation: &MarkdownCancellation) -> Result<(), MarkdownLoadError> {
    if cancellation.is_cancelled() {
        Err(MarkdownLoadError::Cancelled)
    } else {
        Ok(())
    }
}

fn normalize_non_empty(
    value: String,
    error: MarkdownSourceError,
) -> Result<String, MarkdownSourceError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(error)
    } else {
        Ok(trimmed.to_owned())
    }
}

fn option_string(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn looks_binary(source_text: &str) -> bool {
    let mut inspected = 0;
    let mut suspicious = 0;
    for character in source_text.chars().take(BINARY_HEURISTIC_SAMPLE_CHARS) {
        inspected += 1;
        if character == '\0' {
            return true;
        }
        if character.is_control() && !matches!(character, '\n' | '\r' | '\t' | '\u{000C}') {
            suspicious += 1;
            if suspicious * BINARY_HEURISTIC_CONTROL_RATIO_DENOMINATOR > inspected {
                return true;
            }
        }
    }
    false
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn slugify_heading(text: &str) -> String {
    let mut anchor = String::new();
    let mut previous_was_dash = false;
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            anchor.push(character);
            previous_was_dash = false;
        } else if (character.is_whitespace() || character == '-')
            && !anchor.is_empty()
            && !previous_was_dash
        {
            anchor.push('-');
            previous_was_dash = true;
        }
    }
    anchor.trim_matches('-').to_owned().if_empty_then("section")
}

fn classify_target(source: &MarkdownSource, target: &str) -> ResourceReferenceClass {
    if target.starts_with('#') {
        return ResourceReferenceClass::DocumentFragment;
    }
    if target.starts_with("https://") {
        return ResourceReferenceClass::HttpsAbsolute;
    }
    if target.starts_with("//") || has_uri_scheme(target) {
        return ResourceReferenceClass::DangerousScheme;
    }
    match source {
        MarkdownSource::Local(_) => ResourceReferenceClass::LocalRelative,
        MarkdownSource::Remote(_) => ResourceReferenceClass::RemoteRelativeViaSftpOrigin,
    }
}

fn has_uri_scheme(target: &str) -> bool {
    let Some((scheme, _rest)) = target.split_once(':') else {
        return false;
    };
    let mut characters = scheme.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

fn normalize_code_language(language: Option<&str>) -> Option<String> {
    let language = language?;
    let token = language.split_whitespace().next()?.trim();
    if token.is_empty() {
        return None;
    }
    let normalized = token.to_ascii_lowercase();
    let mapped = match normalized.as_str() {
        "rs" => "rust",
        "md" => "markdown",
        "yml" => "yaml",
        "ps" | "ps1" => "powershell",
        other => other,
    };
    SUPPORTED_HIGHLIGHT_LANGUAGES
        .iter()
        .copied()
        .find(|supported| *supported == mapped)
        .map(str::to_owned)
}

fn highlight_code(language: Option<&str>, code_text: &str) -> Vec<HighlightedCodeLine> {
    let syntax_set = syntax_set();
    let theme = highlight_theme();
    let syntax = language
        .and_then(|language| syntax_set.find_syntax_by_token(language))
        .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut lines = Vec::new();
    for line in LinesWithEndings::from(code_text) {
        let spans = highlighter
            .highlight_line(line, syntax_set)
            .unwrap_or_default()
            .into_iter()
            .map(|(style, text)| HighlightedSpan {
                text: text.to_owned(),
                style: HighlightStyle {
                    foreground: rgba(style.foreground),
                    background: rgba(style.background),
                    bold: style.font_style.contains(FontStyle::BOLD),
                    italic: style.font_style.contains(FontStyle::ITALIC),
                    underline: style.font_style.contains(FontStyle::UNDERLINE),
                },
            })
            .collect();
        lines.push(HighlightedCodeLine {
            text: line.to_owned(),
            spans,
        });
    }
    if code_text.is_empty() {
        lines.push(HighlightedCodeLine {
            text: String::new(),
            spans: Vec::new(),
        });
    }
    lines
}

fn syntax_set() -> &'static SyntaxSet {
    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn highlight_theme() -> &'static syntect::highlighting::Theme {
    static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();
    let theme_set = THEME_SET.get_or_init(ThemeSet::load_defaults);
    theme_set
        .themes
        .get(HIGHLIGHT_THEME_NAME)
        .expect("default syntect theme must exist")
}

fn rgba(color: syntect::highlighting::Color) -> RgbaColor {
    RgbaColor {
        red: color.r,
        green: color.g,
        blue: color.b,
        alpha: color.a,
    }
}

fn inline_plain_text(inlines: &[Inline]) -> String {
    let mut plain_text = String::new();
    for inline in inlines {
        append_inline_plain_text(inline, &mut plain_text);
    }
    plain_text
}

fn append_inline_plain_text(inline: &Inline, plain_text: &mut String) {
    match inline {
        Inline::Text(text) | Inline::Code(text) => plain_text.push_str(text.text()),
        Inline::Emphasis(container)
        | Inline::Strong(container)
        | Inline::Strikethrough(container) => {
            for child in container.inlines() {
                append_inline_plain_text(child, plain_text);
            }
        }
        Inline::Link(link) => {
            for child in link.inlines() {
                append_inline_plain_text(child, plain_text);
            }
        }
        Inline::Image(image) => plain_text.push_str(image.alt_text()),
        Inline::RawHtml(raw) => plain_text.push_str(raw.literal()),
        Inline::SoftBreak { .. } | Inline::HardBreak { .. } => plain_text.push('\n'),
    }
}

trait StringExt {
    fn if_empty_then(self, fallback: &str) -> String;
}

impl StringExt for String {
    fn if_empty_then(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_owned()
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_local(source_text: &[u8]) -> Result<MarkdownDocument, MarkdownLoadError> {
        MarkdownLoader::default().load(
            LocalMarkdownSource::new("/docs/readme.md").unwrap().into(),
            source_text.len(),
            source_text,
            &MarkdownCancellation::new(),
        )
    }

    fn local_document(text: &str) -> MarkdownDocument {
        load_local(text.as_bytes()).unwrap()
    }

    fn remote_source() -> MarkdownSource {
        RemoteMarkdownSource::new(
            "example.test",
            22,
            RemoteSourceOwner::username_and_profile("fes", "demo-profile").unwrap(),
            "SHA256:demo",
            "/remote/docs/readme.md",
            7,
        )
        .unwrap()
        .into()
    }

    #[test]
    fn parses_representative_commonmark_and_gfm_document() {
        let markdown = "# Title\n\nParagraph with ~~strike~~ and <https://example.com>.\n\n- [x] done\n- [ ] todo\n  > quoted\n  > - nested\n\n| Name | Value |\n| --- | ---: |\n| one | 1 |\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n";
        let document = local_document(markdown);

        assert_eq!(document.headings().len(), 1);
        assert_eq!(document.headings()[0].text(), "Title");
        assert_eq!(document.headings()[0].anchor(), "title");

        let list = document
            .blocks()
            .iter()
            .find_map(|block| match block {
                Block::List(list) => Some(list),
                _ => None,
            })
            .unwrap();
        assert_eq!(list.items()[0].task_state(), Some(TaskState::Checked));
        assert_eq!(list.items()[1].task_state(), Some(TaskState::Unchecked));

        let table = document
            .blocks()
            .iter()
            .find_map(|block| match block {
                Block::Table(table) => Some(table),
                _ => None,
            })
            .unwrap();
        assert_eq!(table.alignments()[1], TableAlignment::Right);
        assert!(table.rows()[0].is_header());
        assert_eq!(table.rows()[1].cells()[0].plain_text(), "one");

        let code = document
            .blocks()
            .iter()
            .find_map(|block| match block {
                Block::CodeBlock(code) => Some(code),
                _ => None,
            })
            .unwrap();
        assert_eq!(code.language(), Some("rust"));
        assert_eq!(code.code_text(), "fn main() {\n    println!(\"hi\");\n}\n");
        assert!(!code.highlighted_lines().is_empty());

        let paragraph = document
            .blocks()
            .iter()
            .find_map(|block| match block {
                Block::Paragraph(text) => Some(text),
                _ => None,
            })
            .unwrap();
        assert!(paragraph
            .inlines()
            .iter()
            .any(|inline| matches!(inline, Inline::Strikethrough(_))));

        let link = document.resource_references()[0].clone();
        assert_eq!(link.kind(), ResourceReferenceKind::Link);
        assert_eq!(link.class(), ResourceReferenceClass::HttpsAbsolute);
        assert!(link.is_autolink());
        assert_eq!(link.target(), "https://example.com");
    }

    #[test]
    fn rejects_declared_oversize_input() {
        let error = MarkdownLoader::default()
            .load(
                LocalMarkdownSource::new("/docs/readme.md").unwrap().into(),
                MAX_SOURCE_BYTES + 1,
                b"short",
                &MarkdownCancellation::new(),
            )
            .unwrap_err();
        assert_eq!(
            error,
            MarkdownLoadError::OversizeInput {
                limit_bytes: MAX_SOURCE_BYTES,
                actual_bytes: MAX_SOURCE_BYTES + 1,
            }
        );
    }

    #[test]
    fn rejects_invalid_utf8() {
        let error = load_local(&[0xff, 0xfe, 0xfd]).unwrap_err();
        assert_eq!(error, MarkdownLoadError::InvalidUtf8);
    }

    #[test]
    fn rejects_binary_heuristic_content() {
        let error = load_local(b"abc\0def").unwrap_err();
        assert_eq!(error, MarkdownLoadError::BinaryContent);
    }

    #[test]
    fn rejects_too_many_lines() {
        let markdown = format!("{}x", "x\n".repeat(MAX_DECODED_LINES));
        let error = load_local(markdown.as_bytes()).unwrap_err();
        assert_eq!(
            error,
            MarkdownLoadError::TooManyLines {
                limit: MAX_DECODED_LINES,
                actual: MAX_DECODED_LINES + 1,
            }
        );
    }

    #[test]
    fn rejects_excessive_nesting() {
        let mut markdown = String::new();
        for _ in 0..=MAX_BLOCK_NESTING_DEPTH {
            markdown.push_str("> ");
        }
        markdown.push_str("too deep\n");
        let error = load_local(markdown.as_bytes()).unwrap_err();
        assert_eq!(
            error,
            MarkdownLoadError::ExcessiveNesting {
                limit: MAX_BLOCK_NESTING_DEPTH,
                actual: MAX_BLOCK_NESTING_DEPTH + 1,
            }
        );
    }

    #[test]
    fn rejects_too_many_table_cells() {
        let mut markdown = String::from("| h |\n| - |\n");
        for _ in 0..MAX_TABLE_CELLS {
            markdown.push_str("| x |\n");
        }
        let error = load_local(markdown.as_bytes()).unwrap_err();
        assert_eq!(
            error,
            MarkdownLoadError::TooManyTableCells {
                limit: MAX_TABLE_CELLS,
                actual: MAX_TABLE_CELLS + 1,
            }
        );
    }

    #[test]
    fn rejects_oversize_code_block() {
        let code = "a".repeat(MAX_CODE_BLOCK_BYTES + 1);
        let markdown = format!("```text\n{code}\n```\n");
        let error = load_local(markdown.as_bytes()).unwrap_err();
        assert_eq!(
            error,
            MarkdownLoadError::CodeBlockTooLarge {
                limit_bytes: MAX_CODE_BLOCK_BYTES,
                actual_bytes: MAX_CODE_BLOCK_BYTES + 2,
            }
        );
    }

    #[test]
    fn rejects_too_many_distinct_resource_references() {
        let mut markdown = String::new();
        for index in 0..=MAX_RESOURCE_REFERENCES {
            markdown.push_str(&format!("[link{index}](file-{index}.md)\n\n"));
        }
        let error = load_local(markdown.as_bytes()).unwrap_err();
        assert_eq!(
            error,
            MarkdownLoadError::TooManyResourceReferences {
                limit: MAX_RESOURCE_REFERENCES,
                actual: MAX_RESOURCE_REFERENCES + 1,
            }
        );
    }

    #[test]
    fn strips_utf8_bom() {
        let document = load_local(b"\xEF\xBB\xBF# Title\n").unwrap();
        assert_eq!(document.source_text(), "# Title\n");
        assert_eq!(document.headings()[0].text(), "Title");
    }

    #[test]
    fn preserves_raw_html_as_inert_literal_source() {
        let document = local_document("<div>block</div>\n\nText <span>inline</span> tail\n");
        let html_block = document
            .blocks()
            .iter()
            .find_map(|block| match block {
                Block::Html(html) => Some(html),
                _ => None,
            })
            .unwrap();
        assert_eq!(html_block.literal(), "<div>block</div>\n");

        let paragraph = document
            .blocks()
            .iter()
            .find_map(|block| match block {
                Block::Paragraph(text) => Some(text),
                _ => None,
            })
            .unwrap();
        assert!(paragraph.inlines().iter().any(|inline| match inline {
            Inline::RawHtml(raw) => raw.literal() == "<span>",
            _ => false,
        }));
    }

    #[test]
    fn reports_find_match_offsets_and_nearest_heading() {
        let document = local_document("# Intro\n\né security\n## Next\nsecurity again\n");
        let matches = document.find_matches("security");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].span().start().line_index(), 2);
        assert_eq!(matches[0].span().start().column(), 2);
        assert_eq!(matches[0].span().start().char_offset(), 11);
        assert_eq!(
            document
                .nearest_heading_at_byte(matches[0].span().start().byte_offset())
                .unwrap()
                .text(),
            "Intro"
        );
        assert_eq!(
            document
                .nearest_heading_at_byte(matches[1].span().start().byte_offset())
                .unwrap()
                .text(),
            "Next"
        );
    }

    #[test]
    fn code_block_copy_text_excludes_label_and_highlighting_markup() {
        let document = local_document("```rust\nfn main() {}\n```\n");
        let code = document
            .blocks()
            .iter()
            .find_map(|block| match block {
                Block::CodeBlock(code) => Some(code),
                _ => None,
            })
            .unwrap();
        assert_eq!(code.language(), Some("rust"));
        assert_eq!(code.code_text(), "fn main() {}\n");
        assert!(code.highlighted_lines()[0]
            .spans()
            .iter()
            .all(|span| !span.text().contains("rust")));
    }

    #[test]
    fn classifies_remote_relative_resources_without_loading_them() {
        let document = MarkdownLoader::default()
            .load(
                remote_source(),
                20,
                b"![alt](image.png)\n",
                &MarkdownCancellation::new(),
            )
            .unwrap();
        let reference = &document.resource_references()[0];
        assert_eq!(reference.kind(), ResourceReferenceKind::Image);
        assert_eq!(
            reference.class(),
            ResourceReferenceClass::RemoteRelativeViaSftpOrigin
        );
        assert_eq!(reference.alt_text(), Some("alt"));
        assert_eq!(reference.target(), "image.png");
    }

    #[test]
    fn cancellation_is_observed_before_work() {
        let cancellation = MarkdownCancellation::new();
        cancellation.cancel();
        let error = MarkdownLoader::default()
            .load(
                LocalMarkdownSource::new("/docs/readme.md").unwrap().into(),
                0,
                b"",
                &cancellation,
            )
            .unwrap_err();
        assert_eq!(error, MarkdownLoadError::Cancelled);
    }
}
