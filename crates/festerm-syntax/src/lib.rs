//! Syntax highlighting as a cached view of parsed text (ADR 0035).
//!
//! The rules this crate exists to keep:
//!
//! - Highlighting is **read-only over the text**. It produces spans, never
//!   bytes; it cannot dirty a document or enter its undo history.
//! - A document's tree is parsed **once per revision**, incrementally from the
//!   edit that caused it, and shared by every view of that document.
//! - Only the **visible byte range** is queried, so the cost is proportional
//!   to the window rather than to the file.
//! - Every bound — size, parse time, grammar failure — degrades to plain text
//!   and **says which**, because a silent difference between two files with
//!   the same extension is a bug report waiting to happen.

use std::ops::Range;

use tree_sitter::StreamingIterator;
use tree_sitter::{InputEdit, Language as TsLanguage, Parser, Point, Query, QueryCursor, Tree};

/// Files above this many bytes open without highlighting.
///
/// Deliberately well below [`festerm_document`]'s editability bound: a file
/// can be perfectly editable and still not worth parsing (ADR 0035 §6).
pub const MAX_HIGHLIGHT_BYTES: usize = 1024 * 1024;

/// Files above this many lines open without highlighting.
pub const MAX_HIGHLIGHT_LINES: usize = 20_000;

/// How long one parse may take before it is abandoned for that revision.
///
/// A highlighter may never make the frame late. The next revision gets a
/// fresh budget, so a document that loses a race does not lose colour for
/// good.
pub const PARSE_BUDGET_MICROS: u64 = 40_000;

/// The languages fesTerm has grammars for.
///
/// The set is closed and licence-recorded (ADR 0035 §3). A language that is
/// not here is not an error: the file opens in plain monospace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    C,
    Cpp,
    Python,
    Toml,
    Json,
    Yaml,
    Bash,
    Markdown,
    JavaScript,
    TypeScript,
}

impl Language {
    /// The name the status bar reports, so there is one answer to "what does
    /// fesTerm think this is" rather than two (ADR 0035 §4).
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::Python => "Python",
            Self::Toml => "TOML",
            Self::Json => "JSON",
            Self::Yaml => "YAML",
            Self::Bash => "Shell",
            Self::Markdown => "Markdown",
            Self::JavaScript => "JavaScript",
            Self::TypeScript => "TypeScript",
        }
    }

    /// The language a file name claims, by extension.
    fn from_extension(name: &str) -> Option<Self> {
        let extension = name.rsplit_once('.').map(|(_, ext)| ext)?;
        Some(match extension.to_ascii_lowercase().as_str() {
            "rs" => Self::Rust,
            "c" | "h" => Self::C,
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Self::Cpp,
            "py" | "pyi" => Self::Python,
            "toml" => Self::Toml,
            "json" => Self::Json,
            "yaml" | "yml" => Self::Yaml,
            "sh" | "bash" | "zsh" => Self::Bash,
            "md" | "markdown" => Self::Markdown,
            "js" | "mjs" | "cjs" | "jsx" => Self::JavaScript,
            "ts" | "mts" | "cts" | "tsx" => Self::TypeScript,
            _ => return None,
        })
    }

    /// The language an interpreter line claims, for extensionless scripts.
    fn from_shebang(first_line: &str) -> Option<Self> {
        let rest = first_line.strip_prefix("#!")?;
        let mut words = rest.split_whitespace();
        let mut command = basename(words.next()?);
        if command == "env" {
            // `#!/usr/bin/env python3` names the interpreter in the argument,
            // not in the path.
            command = basename(words.find(|word| !word.starts_with('-'))?);
        }
        Some(match command.trim_end_matches(char::is_numeric) {
            "sh" | "bash" | "zsh" | "dash" => Self::Bash,
            "python" => Self::Python,
            "node" => Self::JavaScript,
            _ => return None,
        })
    }

    /// The small set of well-known names that carry no extension.
    fn from_bare_name(name: &str) -> Option<Self> {
        Some(match name {
            "Makefile" | "makefile" | "Dockerfile" | "Containerfile" => return None,
            ".gitconfig" | ".npmrc" => Self::Toml,
            ".bashrc" | ".bash_profile" | ".zshrc" | ".profile" | "PKGBUILD" => Self::Bash,
            _ => return None,
        })
    }

    /// Extension first, then a `#!` line, then a bare name — in that order,
    /// and never reading past the first line (ADR 0035 §4).
    pub fn detect(file_name: &str, first_line: &str) -> Option<Self> {
        let name = file_name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(file_name)
            .trim();
        Self::from_extension(name)
            .or_else(|| Self::from_shebang(first_line))
            .or_else(|| Self::from_bare_name(name))
    }

    fn grammar(self) -> TsLanguage {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Self::Json => tree_sitter_json::LANGUAGE.into(),
            Self::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            Self::Bash => tree_sitter_bash::LANGUAGE.into(),
            Self::Markdown => tree_sitter_md::LANGUAGE.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        }
    }

    fn highlights_query(self) -> &'static str {
        match self {
            Self::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY,
            Self::C => tree_sitter_c::HIGHLIGHT_QUERY,
            Self::Cpp => tree_sitter_cpp::HIGHLIGHT_QUERY,
            Self::Python => tree_sitter_python::HIGHLIGHTS_QUERY,
            Self::Toml => tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            Self::Json => tree_sitter_json::HIGHLIGHTS_QUERY,
            Self::Yaml => tree_sitter_yaml::HIGHLIGHTS_QUERY,
            Self::Bash => tree_sitter_bash::HIGHLIGHT_QUERY,
            Self::Markdown => tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            Self::JavaScript => tree_sitter_javascript::HIGHLIGHT_QUERY,
            Self::TypeScript => tree_sitter_typescript::HIGHLIGHTS_QUERY,
        }
    }
}

/// What a span means, independent of the language that produced it.
///
/// Colour comes from the theme by role, not by language, so one theme change
/// recolours every grammar (ADR 0035 §5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Keyword,
    StringLiteral,
    Number,
    Comment,
    Type,
    Function,
    Punctuation,
    Variable,
    Constant,
}

impl Role {
    /// The name this role is known by in a highlight query.
    pub const fn capture_name(self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::StringLiteral => "string",
            Self::Number => "number",
            Self::Comment => "comment",
            Self::Type => "type",
            Self::Function => "function",
            Self::Punctuation => "punctuation",
            Self::Variable => "variable",
            Self::Constant => "constant",
        }
    }

    /// The role a grammar's capture belongs to.
    ///
    /// Queries name captures far more finely than the theme colours them
    /// (`string.special.path`, `function.method.call`), so the first segment
    /// decides and the remainder is deliberately ignored.
    fn from_capture(capture: &str) -> Option<Self> {
        let head = capture.split('.').next().unwrap_or(capture);
        Some(match head {
            "keyword" | "conditional" | "repeat" | "include" | "define" | "storageclass"
            | "exception" | "keyword_" => Self::Keyword,
            "string" | "character" | "text" => Self::StringLiteral,
            "number" | "float" => Self::Number,
            "comment" | "spell" => Self::Comment,
            "type" | "class" | "struct" | "enum" | "namespace" | "module" | "interface" => {
                Self::Type
            }
            "function" | "method" | "constructor" => Self::Function,
            "punctuation" | "operator" | "delimiter" | "tag" => Self::Punctuation,
            "variable" | "parameter" | "property" | "field" | "label" => Self::Variable,
            // `true` reading as `1` would be a lie told in colour.
            "constant" | "boolean" | "attribute" | "annotation" => Self::Constant,
            _ => return None,
        })
    }
}

/// One coloured run of bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub role: Role,
}

/// Why a document is not being highlighted, in the words the view repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxStatus {
    /// Highlighting is running.
    Highlighted(Language),
    /// fesTerm has no grammar for this file, which is not an error.
    UnknownLanguage,
    /// The file is past a bound worth parsing.
    TooLarge,
    /// The grammar, its query, or the parse itself failed or ran out of time.
    ParseFailed,
}

impl SyntaxStatus {
    /// The short phrase shown beside the format, or nothing when highlighting
    /// is simply working and there is nothing to explain.
    pub const fn note(self) -> Option<&'static str> {
        match self {
            Self::Highlighted(_) | Self::UnknownLanguage => None,
            Self::TooLarge => Some("No colour · file too large"),
            Self::ParseFailed => Some("No colour · could not parse"),
        }
    }

    pub const fn is_highlighted(self) -> bool {
        matches!(self, Self::Highlighted(_))
    }
}

/// The parse tree and span cache for one document, living beside its bytes.
///
/// Two views of a file — and a Split's two panes — share this, so the file is
/// parsed once however many times it is on screen (ADR 0035 §1).
pub struct DocumentSyntax {
    language: Option<Language>,
    parser: Option<Parser>,
    query: Option<Query>,
    tree: Option<Tree>,
    /// The text the tree was built from, kept so the next revision can be
    /// described to tree-sitter as an edit rather than as a new file.
    parsed_text: String,
    parsed_revision: Option<u64>,
    status: SyntaxStatus,
    cached_range: Option<Range<usize>>,
    spans: Vec<Span>,
}

impl std::fmt::Debug for DocumentSyntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentSyntax")
            .field("language", &self.language)
            .field("status", &self.status)
            .field("parsed_revision", &self.parsed_revision)
            .field("spans", &self.spans.len())
            .finish()
    }
}

impl DocumentSyntax {
    /// Prepares highlighting for a file, deciding the language once.
    pub fn new(file_name: &str, text: &str) -> Self {
        let first_line = text.lines().next().unwrap_or_default();
        let language = Language::detect(file_name, first_line);
        let mut syntax = Self {
            language,
            parser: None,
            query: None,
            tree: None,
            parsed_text: String::new(),
            parsed_revision: None,
            status: match language {
                Some(language) => SyntaxStatus::Highlighted(language),
                None => SyntaxStatus::UnknownLanguage,
            },
            cached_range: None,
            spans: Vec::new(),
        };
        if let Some(language) = language {
            syntax.prepare(language);
        }
        syntax
    }

    /// Prepares highlighting for a language already decided elsewhere, which
    /// is how the Markdown preview highlights a fenced block whose language
    /// its info string named (ADR 0035 §8).
    pub fn for_language(language: Language) -> Self {
        let mut syntax = Self {
            language: Some(language),
            parser: None,
            query: None,
            tree: None,
            parsed_text: String::new(),
            parsed_revision: None,
            status: SyntaxStatus::Highlighted(language),
            cached_range: None,
            spans: Vec::new(),
        };
        syntax.prepare(language);
        syntax
    }

    fn prepare(&mut self, language: Language) {
        let grammar = language.grammar();
        let mut parser = Parser::new();
        let Ok(()) = parser.set_language(&grammar) else {
            self.status = SyntaxStatus::ParseFailed;
            return;
        };
        let Ok(query) = Query::new(&grammar, language.highlights_query()) else {
            self.status = SyntaxStatus::ParseFailed;
            return;
        };
        self.parser = Some(parser);
        self.query = Some(query);
    }

    pub const fn language(&self) -> Option<Language> {
        self.language
    }

    pub const fn status(&self) -> SyntaxStatus {
        self.status
    }

    /// Whether this document's spans are from the given revision already.
    pub fn is_current(&self, revision: u64) -> bool {
        self.parsed_revision == Some(revision)
    }

    /// The spans covering `range` of `text` at `revision`.
    ///
    /// The tree is reparsed only when the revision has moved, and the query
    /// runs only over the range asked for, so scrolling costs a query and
    /// typing costs an incremental reparse — neither costs a file.
    pub fn spans(&mut self, text: &str, revision: u64, range: Range<usize>) -> &[Span] {
        let range = clamp_to_char_boundaries(text, range);
        if self.parsed_revision != Some(revision) {
            self.reparse(text, revision);
            self.cached_range = None;
        }
        if self.cached_range.as_ref() != Some(&range) {
            self.spans = self.query_range(text, &range);
            self.cached_range = Some(range);
        }
        &self.spans
    }

    fn reparse(&mut self, text: &str, revision: u64) {
        if self.language.is_none() {
            return;
        }
        if text.len() > MAX_HIGHLIGHT_BYTES || text.lines().count() > MAX_HIGHLIGHT_LINES {
            self.status = SyntaxStatus::TooLarge;
            self.tree = None;
            self.parsed_text.clear();
            self.parsed_revision = Some(revision);
            self.spans.clear();
            return;
        }
        let Some(parser) = self.parser.as_mut() else {
            return;
        };
        parser.reset();
        let old_tree = match (self.tree.as_mut(), self.parsed_text.as_str()) {
            (Some(tree), previous) if !previous.is_empty() => {
                edit_between(previous, text).map(|edit| {
                    tree.edit(&edit);
                    &*tree
                })
            }
            _ => None,
        };
        // A parse that cannot finish inside the budget is abandoned rather
        // than allowed to make the frame late; the next revision tries again.
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_micros(PARSE_BUDGET_MICROS);
        let mut give_up = move |_state: &tree_sitter::ParseState| {
            if std::time::Instant::now() > deadline {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        };
        let options = tree_sitter::ParseOptions::new().progress_callback(&mut give_up);
        let parsed = parser.parse_with_options(
            &mut |byte, _| text.get(byte..).unwrap_or(""),
            old_tree,
            Some(options),
        );
        match parsed {
            Some(tree) => {
                self.tree = Some(tree);
                self.parsed_text.clear();
                self.parsed_text.push_str(text);
                self.status = self
                    .language
                    .map_or(SyntaxStatus::UnknownLanguage, SyntaxStatus::Highlighted);
            }
            None => {
                self.tree = None;
                self.parsed_text.clear();
                self.status = SyntaxStatus::ParseFailed;
                self.spans.clear();
            }
        }
        self.parsed_revision = Some(revision);
    }

    fn query_range(&mut self, text: &str, range: &Range<usize>) -> Vec<Span> {
        let (Some(tree), Some(query)) = (self.tree.as_ref(), self.query.as_ref()) else {
            return Vec::new();
        };
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        let mut spans: Vec<Span> = Vec::new();
        let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
        while let Some(matched) = matches.next() {
            for capture in matched.captures() {
                let Some(role) = Role::from_capture(query.capture_names()[capture.index as usize])
                else {
                    continue;
                };
                let node = capture.node.byte_range();
                let start = node.start.max(range.start);
                let end = node.end.min(range.end);
                if start >= end {
                    continue;
                }
                spans.push(Span { start, end, role });
            }
        }
        // A later match wins where two overlap, which is tree-sitter's own
        // rule for a more specific pattern written after a general one.
        spans.sort_by_key(|span| (span.start, span.end));
        flatten(spans)
    }
}

/// Reduces overlapping spans to a disjoint, ascending run.
fn flatten(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        match out.last_mut() {
            Some(last) if span.start < last.end => {
                if span.end <= last.end && span.role != last.role {
                    // A nested, more specific capture: keep the outer run up
                    // to it, then the inner one.
                    let tail = Span {
                        start: span.end,
                        end: last.end,
                        role: last.role,
                    };
                    last.end = span.start;
                    if last.start >= last.end {
                        out.pop();
                    }
                    out.push(span);
                    if tail.start < tail.end {
                        out.push(tail);
                    }
                }
            }
            _ => out.push(span),
        }
    }
    out.retain(|span| span.start < span.end);
    out
}

/// Describes the difference between two revisions as one edit.
///
/// The common prefix and suffix are cheap to find and are enough for
/// tree-sitter to reuse everything outside them, which is the whole point of
/// keeping the previous text around.
fn edit_between(previous: &str, current: &str) -> Option<InputEdit> {
    if previous == current {
        return None;
    }
    let prefix = previous
        .as_bytes()
        .iter()
        .zip(current.as_bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let prefix = floor_boundary(previous, floor_boundary(current, prefix));
    let max_suffix = (previous.len() - prefix).min(current.len() - prefix);
    let suffix = previous
        .as_bytes()
        .iter()
        .rev()
        .zip(current.as_bytes().iter().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(max_suffix);
    let old_end = previous.len() - suffix;
    let new_end = current.len() - suffix;
    let old_end = ceil_boundary(previous, old_end);
    let new_end = ceil_boundary(current, new_end);
    Some(InputEdit {
        start_byte: prefix,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point_at(previous, prefix),
        old_end_position: point_at(previous, old_end),
        new_end_position: point_at(current, new_end),
    })
}

fn basename(word: &str) -> &str {
    word.rsplit(['/', '\\']).next().unwrap_or(word)
}

fn point_at(text: &str, offset: usize) -> Point {
    let offset = offset.min(text.len());
    let head = &text[..offset];
    let row = head.matches('\n').count();
    let column = head.rfind('\n').map_or(offset, |at| offset - at - 1);
    Point::new(row, column)
}

fn floor_boundary(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn ceil_boundary(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while offset < text.len() && !text.is_char_boundary(offset) {
        offset += 1;
    }
    offset
}

fn clamp_to_char_boundaries(text: &str, range: Range<usize>) -> Range<usize> {
    let start = floor_boundary(text, range.start);
    let end = ceil_boundary(text, range.end.max(start));
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_name_decides_the_language_before_anything_else() {
        assert_eq!(Language::detect("main.rs", ""), Some(Language::Rust));
        assert_eq!(
            Language::detect("/tmp/relay.toml", ""),
            Some(Language::Toml)
        );
        assert_eq!(
            Language::detect("deploy.sh", "#!/usr/bin/env python3"),
            Some(Language::Bash),
            "the extension is asked first, and it answered"
        );
    }

    #[test]
    fn an_extensionless_script_is_read_from_its_shebang() {
        assert_eq!(
            Language::detect("deploy", "#!/usr/bin/env bash"),
            Some(Language::Bash)
        );
        assert_eq!(
            Language::detect("build", "#!/usr/bin/python3"),
            Some(Language::Python)
        );
    }

    #[test]
    fn a_well_known_bare_name_is_recognised() {
        assert_eq!(Language::detect(".zshrc", ""), Some(Language::Bash));
    }

    #[test]
    fn a_language_without_a_grammar_is_not_an_error() {
        assert_eq!(Language::detect("notes.xyz", "plain words"), None);
        let mut syntax = DocumentSyntax::new("notes.xyz", "plain words");
        assert_eq!(syntax.status(), SyntaxStatus::UnknownLanguage);
        assert!(
            syntax.spans("plain words", 1, 0..11).is_empty(),
            "an unknown language produces no spans and no complaint"
        );
    }

    #[test]
    fn a_known_language_produces_spans_for_what_it_recognises() {
        let text = "fn main() {\n    let count = 3; // three\n}\n";
        let mut syntax = DocumentSyntax::new("main.rs", text);
        let spans = syntax.spans(text, 1, 0..text.len()).to_vec();
        assert!(syntax.status().is_highlighted());
        assert!(
            spans.iter().any(|span| span.role == Role::Keyword),
            "`fn` and `let` are keywords: {spans:?}"
        );
        assert!(
            spans.iter().any(|span| span.role == Role::Comment),
            "the trailing comment is a comment: {spans:?}"
        );
        assert!(
            spans.windows(2).all(|pair| pair[0].end <= pair[1].start),
            "spans are disjoint and ascending: {spans:?}"
        );
    }

    #[test]
    fn only_the_range_asked_for_is_queried() {
        let mut text = String::new();
        for index in 0..400 {
            text.push_str(&format!("fn f{index}() {{ let x = {index}; }}\n"));
        }
        let mut syntax = DocumentSyntax::new("wide.rs", &text);
        let window = 0..text.len() / 10;
        let spans = syntax.spans(&text, 1, window.clone()).to_vec();
        assert!(!spans.is_empty());
        assert!(
            spans
                .iter()
                .all(|span| span.start >= window.start && span.end <= window.end),
            "nothing outside the visible range is coloured"
        );
        let all = syntax.spans(&text, 1, 0..text.len()).len();
        assert!(
            all > spans.len(),
            "the whole file has more spans than one window of it"
        );
    }

    #[test]
    fn a_file_past_the_size_bound_says_why_it_has_no_colour() {
        let text = "// filler\n".repeat(MAX_HIGHLIGHT_LINES + 1);
        let mut syntax = DocumentSyntax::new("huge.rs", &text);
        assert!(syntax.spans(&text, 1, 0..100).is_empty());
        assert_eq!(syntax.status(), SyntaxStatus::TooLarge);
        assert_eq!(syntax.status().note(), Some("No colour · file too large"));
    }

    #[test]
    fn an_edit_reparses_from_the_change_and_keeps_colouring() {
        let first = "fn main() { let a = 1; }\n";
        let second = "fn main() { let a = 1; let b = 2; }\n";
        let mut syntax = DocumentSyntax::new("main.rs", first);
        let before = syntax.spans(first, 1, 0..first.len()).len();
        let after = syntax.spans(second, 2, 0..second.len()).len();
        assert!(after > before, "the new statement is coloured too");
        assert!(syntax.is_current(2));
    }

    #[test]
    fn an_edit_is_described_as_the_change_it_was() {
        let edit = edit_between("let a = 1;\nlet b = 2;\n", "let a = 1;\nlet c = 2;\n")
            .expect("the texts differ");
        assert_eq!(edit.start_byte, 15);
        assert_eq!(edit.old_end_byte, 16);
        assert_eq!(edit.new_end_byte, 16);
        assert_eq!(edit.start_position, Point::new(1, 4));
        assert!(edit_between("same", "same").is_none());
    }

    #[test]
    fn every_grammar_in_the_set_loads_and_its_query_compiles() {
        for language in [
            Language::Rust,
            Language::C,
            Language::Cpp,
            Language::Python,
            Language::Toml,
            Language::Json,
            Language::Yaml,
            Language::Bash,
            Language::Markdown,
            Language::JavaScript,
            Language::TypeScript,
        ] {
            let syntax = DocumentSyntax::for_language(language);
            assert!(
                syntax.status().is_highlighted(),
                "{} loads and its query compiles",
                language.label()
            );
        }
    }
}
