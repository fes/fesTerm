//! Shared regex search for vi commands, Find, and substitution (ADR 0034 §10a).
//!
//! The editor deliberately exposes the Rust `regex` dialect everywhere instead
//! of keeping one syntax for vi and another for toolbar Find. This module is
//! the narrow point where patterns are compiled, bounded, and translated from
//! the few vi compatibility switches ADR 0034 keeps.

use regex::{Regex, RegexBuilder};
use std::{fmt, ops::Range};

const COMPILED_SIZE_LIMIT: usize = 8 * 1024 * 1024;
const DFA_SIZE_LIMIT: usize = 2 * 1024 * 1024;

/// A user-facing search failure with text suitable for the command area.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchError {
    headline: String,
    detail: String,
}

impl SearchError {
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

impl fmt::Display for SearchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} — {}", self.headline, self.detail)
    }
}

impl std::error::Error for SearchError {}

/// One regex match, expressed as a UTF-8-safe byte range into the searched text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchRange {
    pub start: usize,
    pub end: usize,
}

impl MatchRange {
    pub const fn range(&self) -> Range<usize> {
        self.start..self.end
    }

    pub const fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// The bounded result set used for highlights and navigation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchOutcome {
    pub matches: Vec<MatchRange>,
    pub truncated: bool,
}

impl SearchOutcome {
    /// Returns the next collected match after `caret`, wrapping inside the
    /// bounded result set rather than rescanning the whole document.
    pub fn next_match(&self, text: &str, caret: usize) -> Option<MatchRange> {
        let caret = caret.min(text.len());
        if !text.is_char_boundary(caret) {
            return None;
        }

        let current = self.valid_matches(text).find(|found| found.start == caret);
        let threshold = current
            .filter(|found| found.is_empty())
            .map_or(caret + 1, |_| next_scalar_boundary(text, caret));

        self.valid_matches(text)
            .find(|found| found.start >= threshold)
            .cloned()
            .or_else(|| {
                self.valid_matches(text)
                    .find(|found| Some(*found) != current)
                    .cloned()
            })
    }

    /// Returns the previous collected match by start position, wrapping inside
    /// the bounded result set rather than rescanning the whole document.
    pub fn previous_match(&self, text: &str, caret: usize) -> Option<MatchRange> {
        let caret = caret.min(text.len());
        if !text.is_char_boundary(caret) {
            return None;
        }

        let current = self.valid_matches(text).find(|found| found.start == caret);
        let candidate = if current.is_some_and(MatchRange::is_empty) {
            (caret > 0)
                .then(|| previous_scalar_boundary(text, caret))
                .and_then(|threshold| {
                    self.valid_matches(text)
                        .rev()
                        .find(|found| found.start <= threshold)
                        .cloned()
                })
        } else {
            self.valid_matches(text)
                .rev()
                .find(|found| found.start < caret)
                .cloned()
        };

        candidate.or_else(|| {
            self.valid_matches(text)
                .rev()
                .find(|found| Some(*found) != current)
                .cloned()
        })
    }

    /// The caret offset a caller should store after accepting a match.
    ///
    /// The navigation helpers themselves advance past a zero-width match that
    /// starts at this caret, so the durable view state can stay anchored at the
    /// current match start while repeated `n`/`N` still makes progress.
    pub fn caret_after_match(text: &str, found: &MatchRange) -> usize {
        found.start.min(text.len())
    }

    fn valid_matches<'a>(
        &'a self,
        text: &'a str,
    ) -> impl DoubleEndedIterator<Item = &'a MatchRange> + 'a {
        self.matches.iter().filter(|found| {
            found.start <= found.end
                && found.end <= text.len()
                && text.is_char_boundary(found.start)
                && text.is_char_boundary(found.end)
        })
    }
}

/// A compiled, bounded regex using the one editor-wide dialect.
#[derive(Clone, Debug)]
pub struct CompiledSearch {
    regex: Regex,
}

impl CompiledSearch {
    /// Compiles a case-sensitive search pattern.
    pub fn compile(pattern: &str) -> Result<Self, SearchError> {
        Self::compile_with_case(pattern, false)
    }

    /// Compiles a search pattern with a caller-supplied initial case mode.
    ///
    /// `:s` flags use this entry point, while Find and `/` use `compile`.
    /// Inline `(?i)` remains regex syntax; vi `\c` and `\C` are translated by
    /// a syntax-aware scan so escaped literal backslashes and character classes
    /// keep their regex meaning (ADR 0034 §10a).
    pub fn compile_with_case(pattern: &str, case_insensitive: bool) -> Result<Self, SearchError> {
        let translated = analyze_pattern(pattern)?;
        let regex = RegexBuilder::new(&translated)
            .unicode(true)
            .case_insensitive(case_insensitive)
            .size_limit(COMPILED_SIZE_LIMIT)
            .dfa_size_limit(DFA_SIZE_LIMIT)
            .build()
            .map_err(|error| classify_regex_error(pattern, &translated, error))?;
        Ok(Self { regex })
    }

    pub(crate) fn regex(&self) -> &Regex {
        &self.regex
    }

    /// Collects at most `limit` matches and reports whether more were present.
    ///
    /// Highlighting a multi-megabyte buffer must be cancellable in practice:
    /// callers can keep the last valid `SearchOutcome` when a half-typed query
    /// fails, and can show that highlights were truncated instead of allocating
    /// an unbounded vector (ADR 0034 §10a).
    pub fn find_all(&self, text: &str, limit: usize) -> SearchOutcome {
        if limit == 0 {
            return SearchOutcome {
                matches: Vec::new(),
                truncated: self.regex.find(text).is_some(),
            };
        }

        let mut matches = Vec::new();
        let mut iter = self.regex.find_iter(text);
        for found in iter.by_ref().take(limit) {
            matches.push(MatchRange {
                start: found.start(),
                end: found.end(),
            });
        }
        SearchOutcome {
            matches,
            truncated: iter.next().is_some(),
        }
    }

    /// Returns the next match from a bounded scan budget.
    pub fn next_match(&self, text: &str, caret: usize, limit: usize) -> Option<MatchRange> {
        self.find_all(text, limit).next_match(text, caret)
    }

    /// Returns the previous match from a bounded scan budget.
    pub fn previous_match(&self, text: &str, caret: usize, limit: usize) -> Option<MatchRange> {
        self.find_all(text, limit).previous_match(text, caret)
    }
}

/// Builds the vi `*`/`#` pattern for the literal word under the caret.
///
/// ADR 0034 §10a requires punctuation in an identifier to be data rather than
/// regex syntax, so the word is escaped before word-boundary anchors are added
/// when the searched text begins and ends with Unicode word characters. Symbols
/// at an edge, such as a trailing `+`, cannot be surrounded by `\b` without
/// making the literal unmatchable in Rust regex, so those tokens stay escaped
/// but unanchored; ordinary vi `*` words still get whole-word behaviour.
pub fn literal_word_pattern(word: &str) -> String {
    let escaped = regex::escape(word);
    if word
        .chars()
        .next()
        .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
        && word
            .chars()
            .last()
            .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
    {
        format!(r"\b{escaped}\b")
    } else {
        escaped
    }
}

fn analyze_pattern(pattern: &str) -> Result<String, SearchError> {
    PatternScanner::new(pattern).analyze()
}

struct PatternScanner<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> PatternScanner<'a> {
    fn new(pattern: &'a str) -> Self {
        Self {
            chars: pattern.chars().peekable(),
        }
    }

    fn analyze(mut self) -> Result<String, SearchError> {
        let mut translated = String::new();
        while let Some(ch) = self.chars.next() {
            match ch {
                '\\' => self.translate_escape(&mut translated)?,
                '[' => translated.push_str(&self.translate_character_class()),
                '(' if self.chars.peek() == Some(&'?') => {
                    self.translate_group_prefix(&mut translated)?
                }
                _ => translated.push(ch),
            }
        }
        Ok(translated)
    }

    fn translate_escape(&mut self, translated: &mut String) -> Result<(), SearchError> {
        match self.chars.next() {
            Some('\\') => translated.push_str(r"\\"),
            Some('c') => translated.push_str("(?i)"),
            Some('C') => translated.push_str("(?-i)"),
            Some(next @ '1'..='9') => return Err(backreference_error(next)),
            Some('k') => return Err(named_backreference_error()),
            Some(next) => {
                translated.push('\\');
                translated.push(next);
            }
            None => translated.push('\\'),
        }
        Ok(())
    }

    fn translate_group_prefix(&mut self, translated: &mut String) -> Result<(), SearchError> {
        translated.push('(');
        translated.push(self.chars.next().expect("peeked marker exists"));
        match self.chars.peek().copied() {
            Some('=') | Some('!') => return Err(lookaround_error()),
            Some('<') => {
                translated.push(self.chars.next().expect("peeked marker exists"));
                if matches!(self.chars.peek().copied(), Some('=') | Some('!')) {
                    return Err(lookaround_error());
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn translate_character_class(&mut self) -> String {
        let mut class = String::from("[");
        let mut case_switch = None;
        let mut states = vec![ClassState::default()];

        while let Some(ch) = self.chars.next() {
            match ch {
                '\\' => match self.chars.next() {
                    Some('\\') => {
                        class.push_str(r"\\");
                        mark_class_item(&mut states);
                    }
                    Some('c') => case_switch = Some("(?i)"),
                    Some('C') => case_switch = Some("(?-i)"),
                    Some(next) => {
                        class.push('\\');
                        class.push(next);
                        mark_class_item(&mut states);
                    }
                    None => class.push('\\'),
                },
                '[' if self.chars.peek() == Some(&':') => {
                    class.push('[');
                    class.push(self.chars.next().expect("peeked POSIX marker exists"));
                    mark_class_item(&mut states);
                    copy_posix_class(&mut self.chars, &mut class);
                }
                '[' => {
                    class.push('[');
                    mark_class_item(&mut states);
                    states.push(ClassState::default());
                }
                '^' if states
                    .last()
                    .is_some_and(|state| state.allows_literal_right_bracket) =>
                {
                    class.push('^');
                }
                ']' if states
                    .last()
                    .is_some_and(|state| state.allows_literal_right_bracket) =>
                {
                    class.push(']');
                    mark_class_item(&mut states);
                }
                ']' => {
                    class.push(']');
                    states.pop();
                    if states.is_empty() {
                        if let Some(switch) = case_switch {
                            return format!("{switch}{class}");
                        }
                        return class;
                    }
                    mark_class_item(&mut states);
                }
                _ => {
                    class.push(ch);
                    mark_class_item(&mut states);
                }
            }
        }

        class
    }
}

#[derive(Clone, Copy, Debug)]
struct ClassState {
    allows_literal_right_bracket: bool,
}

impl Default for ClassState {
    fn default() -> Self {
        Self {
            allows_literal_right_bracket: true,
        }
    }
}

fn mark_class_item(states: &mut [ClassState]) {
    if let Some(state) = states.last_mut() {
        state.allows_literal_right_bracket = false;
    }
}

fn copy_posix_class<I>(chars: &mut std::iter::Peekable<I>, class: &mut String)
where
    I: Iterator<Item = char>,
{
    let mut previous = None;
    for ch in chars.by_ref() {
        class.push(ch);
        if previous == Some(':') && ch == ']' {
            break;
        }
        previous = Some(ch);
    }
}

fn next_scalar_boundary(text: &str, offset: usize) -> usize {
    if offset >= text.len() {
        return text.len() + 1;
    }
    text[offset..]
        .chars()
        .next()
        .map_or(text.len() + 1, |ch| offset + ch.len_utf8())
}

fn previous_scalar_boundary(text: &str, offset: usize) -> usize {
    if offset == 0 {
        return 0;
    }
    text[..offset]
        .char_indices()
        .last()
        .map_or(0, |(index, _)| index)
}

fn backreference_error(_digit: char) -> SearchError {
    SearchError::new(
        "Backreferences are not supported",
        "fesTerm uses Rust regex syntax everywhere; use capture groups in replacements, not in search patterns.",
    )
}

fn named_backreference_error() -> SearchError {
    SearchError::new(
        "Named backreferences are not supported",
        "Rust regex syntax does not support matching text that was captured earlier in the same pattern.",
    )
}

fn lookaround_error() -> SearchError {
    SearchError::new(
        "Look-around is not supported",
        "fesTerm uses Rust regex syntax everywhere, which keeps searches bounded by leaving look-ahead and look-behind out.",
    )
}

fn classify_regex_error(original: &str, translated: &str, error: regex::Error) -> SearchError {
    if original != translated && error.to_string().contains("unrecognized flag") {
        return SearchError::new(
            "Case-sensitivity switch is in an invalid position",
            "Move \\c or \\C outside regex constructs where inline flags cannot appear.",
        );
    }
    SearchError::new(
        "Invalid regex",
        format!("The pattern could not be compiled as Rust regex syntax: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_is_case_sensitive_until_an_inline_flag_changes_it() {
        let search = CompiledSearch::compile("abc").unwrap();
        assert_eq!(search.find_all("abc ABC", 10).matches.len(), 1);

        let search = CompiledSearch::compile("(?i)abc").unwrap();
        assert_eq!(search.find_all("abc ABC", 10).matches.len(), 2);
    }

    #[test]
    fn vi_case_switches_are_translated_without_leaking_to_regex() {
        let search = CompiledSearch::compile(r"abc\cdef\Cghi").unwrap();
        let outcome = search.find_all("abcDEFghi abcDEFGHI", 10);
        assert_eq!(outcome.matches, vec![MatchRange { start: 0, end: 9 }]);
    }

    #[test]
    fn escaped_backslashes_before_case_letters_remain_literals() {
        let search = CompiledSearch::compile(r"\\c").unwrap();
        let outcome = search.find_all(r"\c C", 10);
        assert_eq!(outcome.matches, vec![MatchRange { start: 0, end: 2 }]);
    }

    #[test]
    fn case_switches_inside_character_classes_are_not_spliced_into_the_class() {
        let search = CompiledSearch::compile(r"[a\cb]").unwrap();
        assert_eq!(search.find_all("A B", 10).matches.len(), 2);
    }

    #[test]
    fn lookaround_characters_inside_character_classes_are_literal() {
        let search = CompiledSearch::compile(r"[(?=)]").unwrap();
        assert_eq!(search.find_all("?=x", 10).matches.len(), 2);
    }

    #[test]
    fn nested_character_classes_keep_case_switches_outside_the_class() {
        let search = CompiledSearch::compile(r"[a[^b]\c]").unwrap();
        assert!(search.find_all("b", 10).matches.is_empty());
    }

    #[test]
    fn posix_character_classes_keep_case_switches_outside_the_class() {
        let search = CompiledSearch::compile(r"[[:alpha:]\c]").unwrap();
        assert_eq!(search.find_all("A 1", 10).matches.len(), 1);
    }

    #[test]
    fn lookaround_text_after_a_nested_class_stays_literal() {
        let search = CompiledSearch::compile(r"[a[^b](?=)]").unwrap();
        assert_eq!(search.find_all("?=", 10).matches.len(), 2);
    }

    #[test]
    fn literal_right_brackets_at_class_start_are_preserved() {
        let closing = CompiledSearch::compile(r"[]]").unwrap();
        assert_eq!(closing.find_all("]x", 10).matches.len(), 1);

        let negated = CompiledSearch::compile(r"[^]]").unwrap();
        assert_eq!(negated.find_all("]x", 10).matches.len(), 1);
    }

    #[test]
    fn unsupported_regex_features_are_reported_as_editor_errors() {
        let backref = CompiledSearch::compile(r"(a)\1").unwrap_err();
        assert_eq!(backref.headline(), "Backreferences are not supported");

        let lookahead = CompiledSearch::compile(r"a(?=b)").unwrap_err();
        assert_eq!(lookahead.headline(), "Look-around is not supported");

        let invalid = CompiledSearch::compile("[").unwrap_err();
        assert_eq!(invalid.headline(), "Invalid regex");
    }

    #[test]
    fn a_zero_width_match_always_advances() {
        let search = CompiledSearch::compile("x*").unwrap();
        let outcome = search.find_all("abc", 20);
        assert_eq!(
            outcome.matches,
            vec![
                MatchRange { start: 0, end: 0 },
                MatchRange { start: 1, end: 1 },
                MatchRange { start: 2, end: 2 },
                MatchRange { start: 3, end: 3 },
            ]
        );
        assert!(!outcome.truncated);
    }

    #[test]
    fn limit_one_zero_width_navigation_does_not_repeat_the_current_match_forward() {
        let search = CompiledSearch::compile("x*").unwrap();
        assert_eq!(search.next_match("abc", 0, 1), None);
    }

    #[test]
    fn limit_one_zero_width_navigation_does_not_repeat_the_current_match_backward() {
        let search = CompiledSearch::compile("x*").unwrap();
        assert_eq!(search.previous_match("abc", 0, 1), None);
    }

    #[test]
    fn empty_document_zero_width_navigation_has_no_distinct_forward_match() {
        let search = CompiledSearch::compile("x*").unwrap();
        let outcome = search.find_all("", 10);
        assert_eq!(outcome.next_match("", 0), None);
    }

    #[test]
    fn empty_document_zero_width_navigation_has_no_distinct_backward_match() {
        let search = CompiledSearch::compile("x*").unwrap();
        let outcome = search.find_all("", 10);
        assert_eq!(outcome.previous_match("", 0), None);
    }

    #[test]
    fn stale_utf8_search_outcomes_do_not_panic_when_navigating_forward() {
        let search = CompiledSearch::compile("x*").unwrap();
        let outcome = search.find_all("é", 10);
        assert_eq!(outcome.next_match("aé", 2), None);
    }

    #[test]
    fn stale_utf8_search_outcomes_do_not_panic_when_navigating_backward() {
        let search = CompiledSearch::compile("x*").unwrap();
        let outcome = search.find_all("é", 10);
        assert_eq!(outcome.previous_match("aé", 2), None);
    }

    #[test]
    fn forward_navigation_walks_zero_width_matches_without_repeating_one() {
        let search = CompiledSearch::compile("x*").unwrap();
        let text = "aé";
        let outcome = search.find_all(text, 10);
        let mut caret = 0;
        let mut visited = Vec::new();
        for _ in 0..4 {
            let found = outcome.next_match(text, caret).unwrap();
            caret = SearchOutcome::caret_after_match(text, &found);
            visited.push(found);
        }
        assert_eq!(
            visited,
            vec![
                MatchRange { start: 1, end: 1 },
                MatchRange { start: 3, end: 3 },
                MatchRange { start: 0, end: 0 },
                MatchRange { start: 1, end: 1 },
            ]
        );
    }

    #[test]
    fn backward_navigation_walks_zero_width_matches_without_repeating_one() {
        let search = CompiledSearch::compile("x*").unwrap();
        let text = "aé";
        let outcome = search.find_all(text, 10);
        let mut caret = text.len();
        let mut visited = Vec::new();
        for _ in 0..4 {
            let found = outcome.previous_match(text, caret).unwrap();
            caret = found.start;
            visited.push(found);
        }
        assert_eq!(
            visited,
            vec![
                MatchRange { start: 1, end: 1 },
                MatchRange { start: 0, end: 0 },
                MatchRange { start: 3, end: 3 },
                MatchRange { start: 1, end: 1 },
            ]
        );
    }

    #[test]
    fn forward_and_backward_navigation_wrap_ordinary_matches_inside_the_bounded_set() {
        let search = CompiledSearch::compile("dog").unwrap();
        let text = "dog cat dog dog";
        let outcome = search.find_all(text, 2);

        assert_eq!(
            outcome.next_match(text, 0),
            Some(MatchRange { start: 8, end: 11 })
        );
        assert_eq!(
            outcome.next_match(text, 8),
            Some(MatchRange { start: 0, end: 3 })
        );
        assert_eq!(
            outcome.previous_match(text, 8),
            Some(MatchRange { start: 0, end: 3 })
        );
        assert_eq!(
            outcome.previous_match(text, 0),
            Some(MatchRange { start: 8, end: 11 })
        );
        assert!(outcome.truncated);
    }

    #[test]
    fn backward_navigation_selects_a_match_that_contains_the_caret() {
        let search = CompiledSearch::compile("abcdef").unwrap();
        let outcome = search.find_all("abcdef xx abcdef", 10);
        assert_eq!(
            outcome.previous_match("abcdef xx abcdef", 3),
            Some(MatchRange { start: 0, end: 6 })
        );
    }

    #[test]
    fn compiled_navigation_uses_the_explicit_scan_budget() {
        let search = CompiledSearch::compile("dog").unwrap();
        let text = "dog cat dog";
        assert_eq!(search.next_match(text, 0, 1), None);
    }

    #[test]
    fn match_ranges_are_safe_for_multibyte_text() {
        let text = "café 東京 🦀";
        let search = CompiledSearch::compile(r"é|東京|🦀").unwrap();
        let outcome = search.find_all(text, 10);

        for found in &outcome.matches {
            assert!(text.is_char_boundary(found.start));
            assert!(text.is_char_boundary(found.end));
            assert!(!&text[found.range()].is_empty());
        }
        assert_eq!(outcome.matches.len(), 3);
    }

    #[test]
    fn literal_word_patterns_escape_identifier_punctuation() {
        let pattern = literal_word_pattern("foo.bar");
        let search = CompiledSearch::compile(&pattern).unwrap();
        assert_eq!(search.find_all("fooXbar foo.bar", 10).matches.len(), 1);
    }

    #[test]
    fn collecting_matches_stops_at_the_callers_limit() {
        let search = CompiledSearch::compile("a").unwrap();
        let outcome = search.find_all("aaaa", 2);
        assert_eq!(outcome.matches.len(), 2);
        assert!(outcome.truncated);
    }

    #[test]
    fn a_zero_limit_reports_that_matches_were_truncated() {
        let search = CompiledSearch::compile("a").unwrap();
        let outcome = search.find_all("a", 0);
        assert!(outcome.matches.is_empty());
        assert!(outcome.truncated);
    }
}
