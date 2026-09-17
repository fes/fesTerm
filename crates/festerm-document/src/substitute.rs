//! Bounded `:s` parsing and planning for the shared editor regex dialect.
//!
//! This module intentionally stops before mutation. The app can turn a
//! `SubstitutePlan` into validated `TextEdit`s with `to_text_edits()` and pass
//! them to `TextDocument::apply_edits`, preserving ADR 0034's one-undo-step
//! substitution rule without coupling this parser to the document mutator.

use crate::{search::CompiledSearch, text::TextEdit, MatchRange, SearchError};
use std::{fmt, ops::Range};

const DEFAULT_REPLACEMENT_BYTE_LIMIT: usize = 4 * 1024 * 1024;

/// A user-facing substitution failure with no planned side effects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubstituteError {
    headline: String,
    detail: String,
}

impl SubstituteError {
    pub fn new(headline: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            headline: headline.into(),
            detail: detail.into(),
        }
    }

    pub fn headline(&self) -> &str {
        &self.headline
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl From<SearchError> for SubstituteError {
    fn from(error: SearchError) -> Self {
        Self::new(error.headline().to_owned(), error.detail().to_owned())
    }
}

impl fmt::Display for SubstituteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} — {}", self.headline, self.detail)
    }
}

impl std::error::Error for SubstituteError {}

/// The only Ex ranges accepted by ADR 0034 §10a.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubstituteRange {
    CurrentLine,
    WholeDocument,
    VisualSelection,
}

/// Supported `:s` flags.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SubstituteFlags {
    pub global: bool,
    pub confirm: bool,
    pub case_insensitive: bool,
    pub case_sensitive: bool,
    pub count_only: bool,
}

/// A parsed substitution command that is still independent of any document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubstituteCommand {
    pub range: SubstituteRange,
    pub pattern: String,
    pub replacement: String,
    pub flags: SubstituteFlags,
    replacement_tokens: Vec<ReplacementToken>,
}

impl SubstituteCommand {
    /// Parses `:s`, `:%s`, and `:'<,'>s`, rejecting every other Ex address
    /// before any replacement planning can begin.
    pub fn parse(command: &str) -> Result<Self, SubstituteError> {
        let (range, rest) = parse_range(command)?;
        let Some(rest) = rest.strip_prefix('/') else {
            return Err(SubstituteError::new(
                "Missing substitution delimiter",
                "Substitution commands must use / as the delimiter: :s/pattern/replacement/flags.",
            ));
        };
        let (pattern, rest) = take_delimited(rest)?;
        let (replacement, replacement_tokens, flags_text) = take_replacement_delimited(rest)?;
        Self::from_tokens(
            pattern,
            replacement,
            range,
            parse_flags(flags_text)?,
            replacement_tokens,
        )
    }

    /// Builds a substitution from already-separated Find/Replace fields.
    ///
    /// Toolbar callers already own literal pattern and replacement strings, so
    /// routing them through a synthetic `:%s/.../.../` command would add a
    /// fragile delimiter-escaping round trip. This constructor shares the same
    /// replacement token model as `parse`, preserving ADR 0034 §10a's one
    /// dialect while avoiding command-line delimiter syntax entirely.
    pub fn from_parts(
        pattern: &str,
        replacement: &str,
        range: SubstituteRange,
        flags: SubstituteFlags,
    ) -> Result<Self, SubstituteError> {
        Self::from_tokens(
            pattern.to_owned(),
            replacement.to_owned(),
            range,
            validate_flags(flags)?,
            tokenize_literal_replacement(replacement)?,
        )
    }

    fn from_tokens(
        pattern: String,
        replacement: String,
        range: SubstituteRange,
        flags: SubstituteFlags,
        replacement_tokens: Vec<ReplacementToken>,
    ) -> Result<Self, SubstituteError> {
        Ok(Self {
            range,
            pattern,
            replacement,
            flags,
            replacement_tokens,
        })
    }

    /// Builds a bounded replacement plan without mutating `text`.
    ///
    /// `current_line` and `visual_selection` are byte ranges supplied by the
    /// view layer because ADR 0034 keeps caret and selection state view-scoped.
    /// Without `g`, planning follows Ex line semantics and takes the first
    /// match on each addressed line; this is the least surprising resolution of
    /// ADR 0034's otherwise terse flag description.
    pub fn plan(
        &self,
        text: &str,
        current_line: Range<usize>,
        visual_selection: Option<Range<usize>>,
        limit: usize,
    ) -> Result<SubstitutePlan, SubstituteError> {
        self.plan_with_replacement_byte_limit(
            text,
            current_line,
            visual_selection,
            limit,
            DEFAULT_REPLACEMENT_BYTE_LIMIT,
        )
    }

    /// Builds a plan with an explicit replacement-byte ceiling for tests and
    /// callers that want a smaller interactive budget.
    pub fn plan_with_replacement_byte_limit(
        &self,
        text: &str,
        current_line: Range<usize>,
        visual_selection: Option<Range<usize>>,
        match_limit: usize,
        replacement_byte_limit: usize,
    ) -> Result<SubstitutePlan, SubstituteError> {
        let target = match self.range {
            SubstituteRange::CurrentLine => current_line,
            SubstituteRange::WholeDocument => 0..text.len(),
            SubstituteRange::VisualSelection => visual_selection.ok_or_else(|| {
                SubstituteError::new(
                    "Visual selection is unavailable",
                    "The :'<,'>s range can only be planned while a visual selection supplies its byte range.",
                )
            })?,
        };
        validate_text_range(text, &target)?;

        let search = CompiledSearch::compile_with_case(&self.pattern, self.flags.case_insensitive)?;
        let mut planned = ReplacementAccumulator::new(
            text,
            &self.replacement_tokens,
            self.flags.count_only,
            replacement_byte_limit,
        );
        let mut total_matches = 0;
        let mut truncated = false;

        'lines: for line in line_ranges(text, target) {
            if self.flags.global {
                for captures in search.regex().captures_iter(&text[line.clone()]) {
                    if total_matches == match_limit {
                        truncated = true;
                        break 'lines;
                    }
                    total_matches += 1;
                    planned.push(line.start, &captures)?;
                }
            } else if let Some(captures) = search.regex().captures(&text[line.clone()]) {
                if total_matches == match_limit {
                    truncated = true;
                    break;
                }
                total_matches += 1;
                planned.push(line.start, &captures)?;
            }
        }

        let replacements = planned.into_replacements();
        validate_replacement_order(&replacements)?;
        Ok(SubstitutePlan {
            replacements,
            total_matches,
            truncated,
            count_only: self.flags.count_only,
            requires_confirmation: self.flags.confirm,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ReplacementToken {
    Literal(String),
    WholeMatch,
    CaptureIndex(usize),
    NamedCapture(String),
}

/// One replacement in a plan, shaped to become a `TextEdit`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubstituteReplacement {
    pub range: MatchRange,
    pub removed: String,
    pub replacement: String,
}

/// A bounded, immutable plan that a caller may apply as one undo transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubstitutePlan {
    pub replacements: Vec<SubstituteReplacement>,
    pub total_matches: usize,
    pub truncated: bool,
    pub count_only: bool,
    pub requires_confirmation: bool,
}

impl SubstitutePlan {
    pub fn replacement_count(&self) -> usize {
        self.replacements.len()
    }

    /// Converts this plan to validated edits for `TextDocument::apply_edits`.
    ///
    /// The validation is repeated against the current text because a plan may
    /// outlive the snapshot it was built from. Stale or character-splitting
    /// ranges become a clean error here instead of a panic, and the caller's
    /// subsequent `apply_edits` call still provides the atomic one-transaction
    /// commit path.
    pub fn to_text_edits(&self, text: &str) -> Result<Vec<TextEdit>, SubstituteError> {
        self.replacements
            .iter()
            .map(|replacement| {
                validate_text_range(text, &replacement.range.range())?;
                let current = &text[replacement.range.range()];
                if current != replacement.removed {
                    return Err(SubstituteError::new(
                        "Substitution plan is stale",
                        "The document text no longer matches the text captured when the plan was built; rebuild the plan before applying it.",
                    ));
                }
                Ok(TextEdit {
                    start: replacement.range.start,
                    removed: replacement.removed.clone(),
                    inserted: replacement.replacement.clone(),
                })
            })
            .collect()
    }
}

fn parse_range(command: &str) -> Result<(SubstituteRange, &str), SubstituteError> {
    if let Some(rest) = command.strip_prefix(":'<,'>s") {
        return Ok((SubstituteRange::VisualSelection, rest));
    }
    if let Some(rest) = command.strip_prefix(":%s") {
        return Ok((SubstituteRange::WholeDocument, rest));
    }
    if let Some(rest) = command.strip_prefix(":s") {
        return Ok((SubstituteRange::CurrentLine, rest));
    }
    if command.starts_with(':') {
        return Err(SubstituteError::new(
            "Unsupported substitution range",
            "Only :s, :%s, and :'<,'>s are supported; other Ex addresses are rejected without side effects.",
        ));
    }
    Err(SubstituteError::new(
        "Substitution must start with ':'",
        "Use :s for the current line, :%s for the document, or :'<,'>s for the visual selection.",
    ))
}

fn take_delimited(input: &str) -> Result<(String, &str), SubstituteError> {
    let mut field = String::new();
    let mut chars = input.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch == '/' {
            return Ok((field, &input[index + ch.len_utf8()..]));
        }
        if ch == '\\' {
            match chars.peek().copied() {
                Some((_, '/')) | Some((_, '\\')) => {
                    let (_, escaped) = chars.next().expect("peeked character exists");
                    field.push(escaped);
                }
                _ => field.push(ch),
            }
        } else {
            field.push(ch);
        }
    }
    Err(SubstituteError::new(
        "Unterminated substitution field",
        "Pattern and replacement must both be followed by the / delimiter; escape a literal delimiter as \\/.",
    ))
}

fn take_replacement_delimited(
    input: &str,
) -> Result<(String, Vec<ReplacementToken>, &str), SubstituteError> {
    let mut display = String::new();
    let mut token_source = String::new();
    let mut chars = input.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '/' => {
                return Ok((
                    display,
                    tokenize_command_replacement(&token_source)?,
                    &input[index + ch.len_utf8()..],
                ));
            }
            '\\' => match chars.peek().copied() {
                Some((_, '/')) => {
                    chars.next();
                    display.push('/');
                    token_source.push('/');
                }
                Some((_, '\\')) => {
                    chars.next();
                    display.push('\\');
                    token_source.push('\\');
                    token_source.push('\\');
                }
                _ => {
                    display.push('\\');
                    token_source.push('\\');
                }
            },
            _ => {
                display.push(ch);
                token_source.push(ch);
            }
        }
    }
    Err(SubstituteError::new(
        "Unterminated substitution field",
        "Pattern and replacement must both be followed by the / delimiter; escape a literal delimiter as \\/.",
    ))
}

fn tokenize_command_replacement(
    replacement: &str,
) -> Result<Vec<ReplacementToken>, SubstituteError> {
    tokenize_replacement(replacement, BackslashPair::OneLiteralBackslash)
}

fn tokenize_literal_replacement(
    replacement: &str,
) -> Result<Vec<ReplacementToken>, SubstituteError> {
    tokenize_replacement(replacement, BackslashPair::TwoLiteralBackslashes)
}

#[derive(Clone, Copy)]
enum BackslashPair {
    OneLiteralBackslash,
    TwoLiteralBackslashes,
}

fn tokenize_replacement(
    replacement: &str,
    pair_mode: BackslashPair,
) -> Result<Vec<ReplacementToken>, SubstituteError> {
    let mut tokens = Vec::new();
    let mut literal = String::new();
    let mut chars = replacement.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        match ch {
            '\\' => parse_backslash_replacement(&mut chars, &mut tokens, &mut literal, pair_mode)?,
            '&' => {
                push_literal(&mut tokens, &mut literal);
                tokens.push(ReplacementToken::WholeMatch);
            }
            '$' => {
                parse_dollar_replacement(&mut chars, &mut tokens, &mut literal, &mut String::new())?
            }
            _ => literal.push(ch),
        }
    }
    push_literal(&mut tokens, &mut literal);
    Ok(tokens)
}

fn parse_backslash_replacement<I>(
    chars: &mut std::iter::Peekable<I>,
    tokens: &mut Vec<ReplacementToken>,
    literal: &mut String,
    pair_mode: BackslashPair,
) -> Result<(), SubstituteError>
where
    I: Iterator<Item = (usize, char)>,
{
    match chars.next() {
        Some((_, '\\')) => match pair_mode {
            BackslashPair::OneLiteralBackslash => literal.push('\\'),
            BackslashPair::TwoLiteralBackslashes => literal.push_str(r"\\"),
        },
        Some((_, ch @ '1'..='9')) => {
            push_literal(tokens, literal);
            tokens.push(ReplacementToken::CaptureIndex(ch as usize - '0' as usize));
        }
        Some((_, '&')) => literal.push('&'),
        Some((_, 'u' | 'U' | 'l' | 'L' | 'e' | 'E')) => {
            return Err(SubstituteError::new(
                "Case-conversion replacements are not supported",
                "ADR 0034 keeps substitution literal and capture-based only; \\u, \\U, \\l, \\L, and related escapes are rejected.",
            ));
        }
        Some((_, '=')) => {
            return Err(SubstituteError::new(
                "Expression replacements are not supported",
                "Substitutions cannot evaluate expressions or shell out; use literal text and capture references only.",
            ));
        }
        Some((_, escaped)) => {
            literal.push('\\');
            literal.push(escaped);
        }
        None => literal.push('\\'),
    }
    Ok(())
}

fn parse_dollar_replacement<I>(
    chars: &mut std::iter::Peekable<I>,
    tokens: &mut Vec<ReplacementToken>,
    literal: &mut String,
    display: &mut String,
) -> Result<(), SubstituteError>
where
    I: Iterator<Item = (usize, char)>,
{
    display.push('$');
    match chars.peek().copied() {
        Some((_, digit)) if digit.is_ascii_digit() => {
            push_literal(tokens, literal);
            let mut number = String::new();
            while chars
                .peek()
                .copied()
                .is_some_and(|(_, ch)| ch.is_ascii_digit())
            {
                let (_, digit) = chars.next().expect("peeked digit exists");
                display.push(digit);
                number.push(digit);
            }
            let index = number.parse::<usize>().map_err(|_| {
                SubstituteError::new(
                    "Capture reference is too large",
                    "Numeric capture references must fit in memory-sized integers.",
                )
            })?;
            tokens.push(ReplacementToken::CaptureIndex(index));
        }
        Some((_, '{')) => {
            push_literal(tokens, literal);
            chars.next();
            display.push('{');
            let mut name = String::new();
            for (_, ch) in chars.by_ref() {
                display.push(ch);
                if ch == '}' {
                    tokens.push(ReplacementToken::NamedCapture(name));
                    return Ok(());
                }
                name.push(ch);
            }
            return Err(SubstituteError::new(
                "Unterminated capture reference",
                "Named capture references must close with }: use ${name}.",
            ));
        }
        _ => {
            literal.push('$');
        }
    }
    Ok(())
}

fn push_literal(tokens: &mut Vec<ReplacementToken>, literal: &mut String) {
    if literal.is_empty() {
        return;
    }
    if let Some(ReplacementToken::Literal(previous)) = tokens.last_mut() {
        previous.push_str(literal);
    } else {
        tokens.push(ReplacementToken::Literal(literal.clone()));
    }
    literal.clear();
}

fn parse_flags(flags: &str) -> Result<SubstituteFlags, SubstituteError> {
    let mut parsed = SubstituteFlags::default();
    for flag in flags.chars() {
        let duplicate = match flag {
            'g' => replace_flag(&mut parsed.global),
            'c' => replace_flag(&mut parsed.confirm),
            'i' => replace_flag(&mut parsed.case_insensitive),
            'I' => replace_flag(&mut parsed.case_sensitive),
            'n' => replace_flag(&mut parsed.count_only),
            _ => {
                return Err(SubstituteError::new(
                    "Unsupported substitution flag",
                    format!("The flag '{flag}' is not supported; use only g, c, i, I, or n."),
                ));
            }
        };
        if duplicate {
            return Err(SubstituteError::new(
                "Duplicate substitution flag",
                format!("The flag '{flag}' appears more than once; keep each flag at most once."),
            ));
        }
    }
    validate_flags(parsed)
}

fn validate_flags(flags: SubstituteFlags) -> Result<SubstituteFlags, SubstituteError> {
    if flags.case_insensitive && flags.case_sensitive {
        return Err(SubstituteError::new(
            "Contradictory substitution flags",
            "Use either i for case-insensitive matching or I for case-sensitive matching, not both.",
        ));
    }
    if flags.confirm && flags.count_only {
        return Err(SubstituteError::new(
            "Contradictory substitution flags",
            "Use either c to confirm changes or n to count without changes, not both.",
        ));
    }
    Ok(flags)
}

fn replace_flag(flag: &mut bool) -> bool {
    let was_set = *flag;
    *flag = true;
    was_set
}

fn validate_text_range(text: &str, range: &Range<usize>) -> Result<(), SubstituteError> {
    if range.start > range.end || range.end > text.len() {
        return Err(SubstituteError::new(
            "Substitution range is outside the document",
            "The view supplied a byte range that does not fit the current document text.",
        ));
    }
    if !text.is_char_boundary(range.start) || !text.is_char_boundary(range.end) {
        return Err(SubstituteError::new(
            "Substitution range splits a character",
            "Ranges must use UTF-8 character boundaries so replacements cannot corrupt document text.",
        ));
    }
    Ok(())
}

fn line_ranges(text: &str, target: Range<usize>) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = target.start;
    while start < target.end {
        let relative_newline = text[start..target.end].find('\n');
        let line_end = relative_newline.map_or(target.end, |newline| start + newline);
        ranges.push(start..line_end);
        start = relative_newline.map_or(target.end, |newline| start + newline + 1);
    }
    if target.start == target.end {
        ranges.push(target);
    }
    ranges
}

struct ReplacementAccumulator<'a> {
    text: &'a str,
    tokens: &'a [ReplacementToken],
    count_only: bool,
    replacement_byte_limit: usize,
    planned_bytes: usize,
    replacements: Vec<SubstituteReplacement>,
}

impl<'a> ReplacementAccumulator<'a> {
    fn new(
        text: &'a str,
        tokens: &'a [ReplacementToken],
        count_only: bool,
        replacement_byte_limit: usize,
    ) -> Self {
        Self {
            text,
            tokens,
            count_only,
            replacement_byte_limit,
            planned_bytes: 0,
            replacements: Vec::new(),
        }
    }

    fn push(
        &mut self,
        line_start: usize,
        captures: &regex::Captures<'_>,
    ) -> Result<(), SubstituteError> {
        let Some(found) = captures.get(0) else {
            return Ok(());
        };
        if self.count_only {
            return Ok(());
        }
        let start = line_start + found.start();
        let end = line_start + found.end();
        debug_assert!(self.text.is_char_boundary(start));
        debug_assert!(self.text.is_char_boundary(end));
        let replacement = expand_replacement(
            self.tokens,
            captures,
            self.replacement_byte_limit,
            &mut self.planned_bytes,
        )?;
        self.replacements.push(SubstituteReplacement {
            range: MatchRange { start, end },
            removed: self.text[start..end].to_owned(),
            replacement,
        });
        Ok(())
    }

    fn into_replacements(self) -> Vec<SubstituteReplacement> {
        self.replacements
    }
}

fn expand_replacement(
    tokens: &[ReplacementToken],
    captures: &regex::Captures<'_>,
    replacement_byte_limit: usize,
    planned_bytes: &mut usize,
) -> Result<String, SubstituteError> {
    let mut expanded = String::new();
    for token in tokens {
        let piece = match token {
            ReplacementToken::Literal(literal) => literal.as_str(),
            ReplacementToken::WholeMatch => captures.get(0).map_or("", |found| found.as_str()),
            ReplacementToken::CaptureIndex(index) => {
                captures.get(*index).map_or("", |found| found.as_str())
            }
            ReplacementToken::NamedCapture(name) => {
                captures.name(name).map_or("", |found| found.as_str())
            }
        };
        if *planned_bytes + piece.len() > replacement_byte_limit {
            return Err(SubstituteError::new(
                "Replacement plan is too large",
                "The planned replacement text exceeds the caller's byte budget; narrow the range or use a smaller replacement.",
            ));
        }
        expanded.push_str(piece);
        *planned_bytes += piece.len();
    }
    Ok(expanded)
}

fn validate_replacement_order(
    replacements: &[SubstituteReplacement],
) -> Result<(), SubstituteError> {
    let mut previous: Option<&SubstituteReplacement> = None;
    for replacement in replacements {
        if let Some(previous) = previous {
            if replacement.range.start < previous.range.start {
                return Err(SubstituteError::new(
                    "Substitution plan is out of order",
                    "Replacement ranges must be sorted so they can be committed atomically.",
                ));
            }
            if replacement.range.start < previous.range.end
                || (replacement.range.start == previous.range.start
                    && replacement.range.end == previous.range.end)
            {
                return Err(SubstituteError::new(
                    "Substitution plan overlaps itself",
                    "Replacement ranges must be strictly non-overlapping and cannot contain duplicate zero-width edits.",
                ));
            }
        }
        previous = Some(replacement);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(command: &str, text: &str) -> SubstitutePlan {
        SubstituteCommand::parse(command)
            .unwrap()
            .plan(text, 0..text.len(), None, 100)
            .unwrap()
    }

    fn global_flags() -> SubstituteFlags {
        SubstituteFlags {
            global: true,
            ..SubstituteFlags::default()
        }
    }

    fn replacements_from_parts(replacement: &str) -> Vec<String> {
        SubstituteCommand::from_parts(
            "(a)",
            replacement,
            SubstituteRange::WholeDocument,
            global_flags(),
        )
        .unwrap()
        .plan("aa", 99..99, Some(99..99), 100)
        .unwrap()
        .replacements
        .into_iter()
        .map(|replacement| replacement.replacement)
        .collect()
    }

    fn replacements_from_command(command: &str) -> Vec<String> {
        SubstituteCommand::parse(command)
            .unwrap()
            .plan("aa", 99..99, Some(99..99), 100)
            .unwrap()
            .replacements
            .into_iter()
            .map(|replacement| replacement.replacement)
            .collect()
    }

    #[test]
    fn the_current_line_range_is_parsed() {
        let command = SubstituteCommand::parse(":s/a/b/").unwrap();
        assert_eq!(command.range, SubstituteRange::CurrentLine);
    }

    #[test]
    fn the_whole_document_range_is_parsed() {
        let command = SubstituteCommand::parse(":%s/a/b/").unwrap();
        assert_eq!(command.range, SubstituteRange::WholeDocument);
    }

    #[test]
    fn the_visual_selection_range_is_parsed() {
        let command = SubstituteCommand::parse(":'<,'>s/a/b/").unwrap();
        assert_eq!(command.range, SubstituteRange::VisualSelection);
    }

    #[test]
    fn other_ex_ranges_are_rejected_before_planning() {
        let error = SubstituteCommand::parse(":1,3s/a/b/").unwrap_err();
        assert_eq!(error.headline(), "Unsupported substitution range");
    }

    #[test]
    fn commands_without_a_colon_are_rejected() {
        let error = SubstituteCommand::parse("s/a/b/").unwrap_err();
        assert_eq!(error.headline(), "Substitution must start with ':'");
    }

    #[test]
    fn delimiter_escapes_allow_slashes_and_backslashes() {
        let command = SubstituteCommand::parse(r":s/a\/b/c\\d/").unwrap();
        assert_eq!(command.pattern, "a/b");
        assert_eq!(command.replacement, r"c\d");
    }

    #[test]
    fn from_parts_single_backslash_matches_the_equivalent_command() {
        assert_eq!(
            replacements_from_parts(r"\"),
            replacements_from_command(r":%s/(a)/\\/g")
        );
        assert_eq!(replacements_from_parts(r"\"), vec![r"\", r"\"]);
    }

    #[test]
    fn from_parts_double_backslash_matches_the_equivalent_command() {
        assert_eq!(
            replacements_from_parts(r"\\"),
            replacements_from_command(r":%s/(a)/\\\\/g")
        );
        assert_eq!(replacements_from_parts(r"\\"), vec![r"\\", r"\\"]);
    }

    #[test]
    fn from_parts_backslash_capture_alias_matches_the_equivalent_command() {
        assert_eq!(
            replacements_from_parts(r"\1"),
            replacements_from_command(r":%s/(a)/\1/g")
        );
        assert_eq!(replacements_from_parts(r"\1"), vec!["a", "a"]);
    }

    #[test]
    fn from_parts_dollar_capture_keeps_following_text_literal() {
        assert_eq!(
            replacements_from_parts("$1a"),
            replacements_from_command(r":%s/(a)/$1a/g")
        );
        assert_eq!(replacements_from_parts("$1a"), vec!["aa", "aa"]);
    }

    #[test]
    fn from_parts_ampersand_alias_matches_the_equivalent_command() {
        assert_eq!(
            replacements_from_parts("&"),
            replacements_from_command(r":%s/(a)/&/g")
        );
        assert_eq!(replacements_from_parts("&"), vec!["a", "a"]);
    }

    #[test]
    fn whole_document_planning_ignores_view_scoped_ranges() {
        let planned =
            SubstituteCommand::from_parts("a", "x", SubstituteRange::WholeDocument, global_flags())
                .unwrap()
                .plan("aa", 99..99, None, 100)
                .unwrap();
        assert_eq!(planned.replacement_count(), 2);
    }

    #[test]
    fn a_pattern_with_no_matches_returns_an_empty_successful_plan() {
        let planned =
            SubstituteCommand::from_parts("z", "x", SubstituteRange::WholeDocument, global_flags())
                .unwrap()
                .plan("aa", 0..0, None, 100)
                .unwrap();
        assert_eq!(planned.replacement_count(), 0);
        assert_eq!(planned.total_matches, 0);
    }

    #[test]
    fn confirm_and_count_only_flags_are_contradictory() {
        let error = SubstituteCommand::parse(":s/a/b/gcn").unwrap_err();
        assert_eq!(error.headline(), "Contradictory substitution flags");
    }

    #[test]
    fn case_insensitive_flags_change_matching() {
        let planned = plan(":%s/abc/x/gi", "abc ABC");
        assert_eq!(planned.replacement_count(), 2);

        let planned = plan(":%s/abc/x/gI", "abc ABC");
        assert_eq!(planned.replacement_count(), 1);
    }

    #[test]
    fn unknown_flags_are_rejected() {
        let error = SubstituteCommand::parse(":s/a/b/q").unwrap_err();
        assert_eq!(error.headline(), "Unsupported substitution flag");
    }

    #[test]
    fn duplicate_flags_are_rejected() {
        let error = SubstituteCommand::parse(":s/a/b/gg").unwrap_err();
        assert_eq!(error.headline(), "Duplicate substitution flag");
    }

    #[test]
    fn contradictory_case_flags_are_rejected() {
        let error = SubstituteCommand::parse(":s/a/b/iI").unwrap_err();
        assert_eq!(error.headline(), "Contradictory substitution flags");
    }

    #[test]
    fn replacement_aliases_expand_to_whole_match_and_numbered_captures() {
        let planned = plan(r":%s/(a)(b)/&-\1-\2/g", "ab ab");
        assert_eq!(
            planned
                .replacements
                .iter()
                .map(|r| r.replacement.as_str())
                .collect::<Vec<_>>(),
            vec!["ab-a-b", "ab-a-b"]
        );
    }

    #[test]
    fn numeric_references_do_not_absorb_following_letters_or_digits() {
        let dollar = plan(r":%s/(a)/$1a/g", "a");
        assert_eq!(dollar.replacements[0].replacement, "aa");

        let slash_letter = plan(r":%s/(a)/\1a/g", "a");
        assert_eq!(slash_letter.replacements[0].replacement, "aa");

        let slash_digit = plan(r":%s/(a)/\12/g", "a");
        assert_eq!(slash_digit.replacements[0].replacement, "a2");
    }

    #[test]
    fn delimiter_escaped_backslashes_do_not_create_capture_aliases() {
        let planned = plan(r":%s/(a)/\\1/", "a");
        assert_eq!(planned.replacements[0].replacement, r"\1");
    }

    #[test]
    fn escaped_ampersands_are_literal_text() {
        let planned = plan(r":%s/a/\&/g", "aa");
        assert_eq!(
            planned
                .replacements
                .iter()
                .map(|r| r.replacement.as_str())
                .collect::<Vec<_>>(),
            vec!["&", "&"]
        );
    }

    #[test]
    fn dollar_captures_named_captures_and_literal_dollars_are_expanded() {
        let planned = plan(r":%s/(?P<word>\w+) (\w+)/$0:${word}:$2:$/", "one two");
        assert_eq!(planned.replacements[0].replacement, "one two:one:two:$");
    }

    #[test]
    fn case_conversion_escapes_are_rejected() {
        let error = SubstituteCommand::parse(r":s/a/\u&/").unwrap_err();
        assert_eq!(
            error.headline(),
            "Case-conversion replacements are not supported"
        );
    }

    #[test]
    fn expression_replacements_are_rejected() {
        let error = SubstituteCommand::parse(r":s/a/\=system/").unwrap_err();
        assert_eq!(
            error.headline(),
            "Expression replacements are not supported"
        );
    }

    #[test]
    fn substitution_without_global_replaces_the_first_match_on_each_line() {
        let planned = plan(":%s/a/x/", "aa\naa");
        assert_eq!(planned.replacement_count(), 2);
        assert_eq!(
            planned
                .replacements
                .iter()
                .map(|r| r.range.start)
                .collect::<Vec<_>>(),
            vec![0, 3]
        );
    }

    #[test]
    fn global_substitution_replaces_every_match_in_the_range() {
        let planned = plan(":%s/a/x/g", "aa\naa");
        assert_eq!(planned.replacement_count(), 4);
    }

    #[test]
    fn global_zero_width_substitution_does_not_duplicate_line_boundaries() {
        let planned = plan(":%s/x*/Z/g", "a\nb");
        assert_eq!(
            planned
                .replacements
                .iter()
                .map(|replacement| replacement.range.clone())
                .collect::<Vec<_>>(),
            vec![
                MatchRange { start: 0, end: 0 },
                MatchRange { start: 1, end: 1 },
                MatchRange { start: 2, end: 2 },
                MatchRange { start: 3, end: 3 },
            ]
        );
    }

    #[test]
    fn visual_selection_plans_only_inside_the_supplied_selection() {
        let command = SubstituteCommand::parse(":'<,'>s/a/x/g").unwrap();
        let planned = command.plan("aa bb aa", 0..8, Some(3..8), 100).unwrap();
        assert_eq!(planned.replacement_count(), 2);
        assert_eq!(planned.replacements[0].range.start, 6);
    }

    #[test]
    fn a_missing_visual_selection_is_a_clean_error() {
        let command = SubstituteCommand::parse(":'<,'>s/a/x/").unwrap();
        let error = command.plan("a", 0..1, None, 10).unwrap_err();
        assert_eq!(error.headline(), "Visual selection is unavailable");
    }

    #[test]
    fn count_only_plans_matches_without_replacements() {
        let planned = plan(":%s/a/x/gn", "aaa");
        assert_eq!(planned.total_matches, 3);
        assert_eq!(planned.replacement_count(), 0);
        assert!(planned.count_only);
    }

    #[test]
    fn planning_stops_at_the_callers_limit() {
        let command = SubstituteCommand::parse(":%s/a/x/g").unwrap();
        let planned = command.plan("aaaa", 0..4, None, 2).unwrap();
        assert_eq!(planned.total_matches, 2);
        assert_eq!(planned.replacement_count(), 2);
        assert!(planned.truncated);
    }

    #[test]
    fn planning_stops_before_the_replacement_byte_budget_is_exceeded() {
        let command = SubstituteCommand::parse(r":%s/(aaaa)/$1$1/g").unwrap();
        let error = command
            .plan_with_replacement_byte_limit("aaaa", 0..4, None, 10, 7)
            .unwrap_err();
        assert_eq!(error.headline(), "Replacement plan is too large");
    }

    #[test]
    fn plans_convert_to_text_edits_for_one_transaction_application() {
        let planned = plan(":%s/a/x/g", "aba");
        assert_eq!(
            planned.to_text_edits("aba").unwrap(),
            vec![
                TextEdit {
                    start: 0,
                    removed: "a".to_owned(),
                    inserted: "x".to_owned(),
                },
                TextEdit {
                    start: 2,
                    removed: "a".to_owned(),
                    inserted: "x".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn stale_multibyte_plans_are_refused_instead_of_panicking() {
        let planned = plan(":%s/é/x/g", "é");
        let error = planned.to_text_edits("aé").unwrap_err();
        assert_eq!(error.headline(), "Substitution range splits a character");
    }

    #[test]
    fn stale_text_plans_are_refused_instead_of_becoming_edits() {
        let planned = plan(":%s/a/x/g", "a");
        let error = planned.to_text_edits("b").unwrap_err();
        assert_eq!(error.headline(), "Substitution plan is stale");
    }

    #[test]
    fn invalid_patterns_are_reported_without_a_plan() {
        let command = SubstituteCommand::parse(":s/[/x/").unwrap();
        let error = command.plan("a", 0..1, None, 10).unwrap_err();
        assert_eq!(error.headline(), "Invalid regex");
    }

    #[test]
    fn multibyte_replacement_ranges_are_utf8_safe() {
        let planned = plan(":%s/東京/🦀/g", "café 東京");
        let edit = &planned.replacements[0];
        let text = "café 東京";
        assert!(text.is_char_boundary(edit.range.start));
        assert!(text.is_char_boundary(edit.range.end));
        assert_eq!(&text[edit.range.range()], "東京");
    }
}
