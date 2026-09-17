//! The editable text of one document, and the byte-level policy it must be
//! written back with (ADR 0034 §1, §5).
//!
//! Text is held normalised to `\n` internally so every offset, line index, and
//! edit means one thing, while the file's original encoding and line ending
//! are remembered and re-applied on save. Opening a CRLF file, typing one
//! character, and saving must not rewrite every line in the file.

use crate::bounds::{DocumentBounds, RefusalReason};
use crate::undo::UndoHistory;

/// The line ending a document is written back with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineEnding {
    Lf,
    Crlf,
    Cr,
}

impl LineEnding {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::Crlf => "CRLF",
            Self::Cr => "CR",
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
            Self::Cr => "\r",
        }
    }
}

/// The encoding a document was decoded from and is written back as.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
}

impl Encoding {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf8Bom => "UTF-8 with BOM",
        }
    }
}

/// The indentation the document appears to use.
///
/// This is a read-out in this release (ADR 0034 §8): it describes what was
/// found, and changing it is deliberately not offered, because that would
/// rewrite bytes the user did not ask to change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Indentation {
    Spaces(usize),
    Tabs,
}

impl Indentation {
    pub fn label(self) -> String {
        match self {
            Self::Spaces(width) => format!("Spaces: {width}"),
            Self::Tabs => "Tabs".to_owned(),
        }
    }
}

/// One replacement of a byte range with new text.
///
/// `start` is a byte offset into the normalised text and `removed` is exactly
/// what was there, so an edit can be inverted without consulting the buffer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextEdit {
    pub start: usize,
    pub removed: String,
    pub inserted: String,
}

impl TextEdit {
    fn end(&self) -> usize {
        self.start + self.removed.len()
    }

    fn inverse(&self) -> Self {
        Self {
            start: self.start,
            removed: self.inserted.clone(),
            inserted: self.removed.clone(),
        }
    }
}

/// Why a prepared set of edits was not committed.
///
/// A refusal is always total: nothing is changed, so the caller can show what
/// happened and leave the user's document exactly as they left it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditRefusal {
    /// The edits were not in ascending, non-overlapping order, which is the
    /// only order whose offsets stay meaningful while they are applied.
    OutOfOrder,
    /// An edit reached past the end of the text, or fell inside a character.
    OutOfBounds,
    /// The text an edit expected to remove is not what is there any more, so
    /// the plan was built against content that has since changed.
    Stale,
    /// The result would have breached a document bound.
    Refused(RefusalReason),
}

impl EditRefusal {
    pub fn headline(&self) -> &'static str {
        match self {
            Self::OutOfOrder | Self::OutOfBounds => "These changes could not be applied",
            Self::Stale => "This file changed while the changes were being prepared",
            Self::Refused(reason) => reason.headline(),
        }
    }
}

/// The text of one document, shared by every view of it.
#[derive(Clone, Debug)]
pub struct TextDocument {
    text: String,
    encoding: Encoding,
    line_ending: LineEnding,
    indentation: Indentation,
    bounds: DocumentBounds,
    undo: UndoHistory,
    saved_token: Option<u64>,
    /// Bumped by every change to the content, including undo and redo.
    ///
    /// The undo token cannot stand in for this: a run of coalesced keystrokes
    /// is deliberately one transaction with one token, so a debounced
    /// auto-save reading the token would believe an actively typed document
    /// had settled (ADR 0034 §7).
    revision: u64,
}

impl TextDocument {
    /// Decodes bytes into an editable document, or refuses with the limit that
    /// stopped it.
    pub fn from_bytes(bytes: &[u8], bounds: DocumentBounds) -> Result<Self, RefusalReason> {
        bounds.check_declared_size(bytes.len())?;

        let (encoding, body) = match bytes.strip_prefix(b"\xef\xbb\xbf") {
            Some(rest) => (Encoding::Utf8Bom, rest),
            None => (Encoding::Utf8, bytes),
        };
        if body.contains(&0) {
            return Err(RefusalReason::BinaryContent);
        }
        let decoded = std::str::from_utf8(body).map_err(|_| RefusalReason::NotUtf8)?;

        let line_ending = detect_line_ending(decoded);
        let text = normalise(decoded);
        let line_count = text.lines().count().max(1);
        if line_count > bounds.max_lines() {
            return Err(RefusalReason::TooManyLines {
                lines: line_count,
                limit: bounds.max_lines(),
            });
        }
        if let Some(line) = first_overlong_line(&text, bounds.max_line_bytes()) {
            return Err(RefusalReason::LineTooLong {
                line,
                limit: bounds.max_line_bytes(),
            });
        }

        Ok(Self {
            indentation: detect_indentation(&text),
            text,
            encoding,
            line_ending,
            bounds,
            undo: UndoHistory::new(),
            saved_token: None,
            revision: 0,
        })
    }

    /// The normalised text every view reads and every find searches.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub const fn encoding(&self) -> Encoding {
        self.encoding
    }

    pub const fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    pub const fn indentation(&self) -> Indentation {
        self.indentation
    }

    pub const fn bounds(&self) -> DocumentBounds {
        self.bounds
    }

    /// Re-applies the original encoding and line ending, producing exactly the
    /// bytes a save must write.
    pub fn to_bytes(&self) -> Vec<u8> {
        let body = match self.line_ending {
            LineEnding::Lf => self.text.clone(),
            ending => self.text.replace('\n', ending.as_str()),
        };
        let mut bytes = Vec::with_capacity(body.len() + 3);
        if self.encoding == Encoding::Utf8Bom {
            bytes.extend_from_slice(b"\xef\xbb\xbf");
        }
        bytes.extend_from_slice(body.as_bytes());
        bytes
    }

    /// A counter that changes whenever the content does, so a caller can tell
    /// "nothing has happened since I last looked" from "something has".
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether the buffer differs from the content last successfully saved or
    /// loaded.
    pub fn is_dirty(&self) -> bool {
        self.undo.token() != self.saved_token
    }

    /// Records that the current content is what the source now holds.
    pub fn mark_saved(&mut self) {
        self.saved_token = self.undo.token();
        self.undo.close_transaction();
    }

    /// Replaces the buffer with freshly loaded content, as a reload does.
    ///
    /// Undo history is dropped rather than rebased: it describes edits to
    /// bytes that are no longer the document's content, and replaying it over
    /// new text would produce something nobody wrote.
    pub fn reload_from(&mut self, bytes: &[u8]) -> Result<(), RefusalReason> {
        let replacement = Self::from_bytes(bytes, self.bounds)?;
        self.text = replacement.text;
        self.encoding = replacement.encoding;
        self.line_ending = replacement.line_ending;
        self.indentation = replacement.indentation;
        self.undo.clear();
        self.saved_token = None;
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    pub fn can_undo(&self) -> bool {
        self.undo.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.undo.can_redo()
    }

    /// Ends the current coalescing run, so the next keystroke starts a fresh
    /// undo transaction. Saving and moving the caret both do this.
    pub fn close_transaction(&mut self) {
        self.undo.close_transaction();
    }

    /// Applies one replacement as its own undo transaction.
    pub fn replace(
        &mut self,
        range: std::ops::Range<usize>,
        replacement: &str,
    ) -> Result<(), RefusalReason> {
        let edit = self.edit_for(range, replacement);
        self.apply_transaction(vec![edit], false)
    }

    /// Adopts text a view has already modified, deriving the smallest edit
    /// that explains the change.
    ///
    /// This is the seam for an editing widget that owns its own `String`: it
    /// hands back the whole buffer, and the document recovers the one edit
    /// rather than treating every frame as a wholesale replacement — which
    /// would make undo useless and every keystroke a 4 MB copy.
    pub fn sync_from_view(&mut self, updated: &str) -> Result<bool, RefusalReason> {
        if updated == self.text {
            return Ok(false);
        }
        let prefix = common_prefix(&self.text, updated);
        let suffix = common_suffix(&self.text[prefix..], &updated[prefix..]);
        let removed = self.text[prefix..self.text.len() - suffix].to_owned();
        let inserted = updated[prefix..updated.len() - suffix].to_owned();
        let coalescable = removed.is_empty() && inserted.chars().count() == 1;
        self.apply_transaction(
            vec![TextEdit {
                start: prefix,
                removed,
                inserted,
            }],
            coalescable,
        )?;
        Ok(true)
    }

    /// Replaces every occurrence of `query` as **one** undo transaction
    /// (ADR 0034 §3), returning how many were replaced.
    pub fn replace_all(&mut self, query: &str, replacement: &str) -> Result<usize, RefusalReason> {
        if query.is_empty() {
            return Ok(0);
        }
        let mut edits = Vec::new();
        let mut search_from = 0;
        while let Some(found) = self.text[search_from..].find(query) {
            let start = search_from + found;
            edits.push(TextEdit {
                start,
                removed: query.to_owned(),
                inserted: replacement.to_owned(),
            });
            search_from = start + query.len();
        }
        let count = edits.len();
        if count > 0 {
            self.apply_transaction(edits, false)?;
        }
        Ok(count)
    }

    /// Commits a prepared set of edits as **one** undo transaction.
    ///
    /// This is the seam a substitution lands through (ADR 0034 §10a): a
    /// `:%s` that changes fifty lines must be one press of undo, not fifty.
    /// The edits are validated against the text they were planned from before
    /// anything is changed, so a plan built from a stale buffer — a sibling
    /// view typed into it, or the file was reloaded — is refused rather than
    /// applied at offsets that now mean something else.
    pub fn apply_edits(&mut self, edits: Vec<TextEdit>) -> Result<usize, EditRefusal> {
        if edits.is_empty() {
            return Ok(0);
        }
        let mut previous_end = 0usize;
        for edit in &edits {
            let end = edit.end();
            if edit.start < previous_end {
                return Err(EditRefusal::OutOfOrder);
            }
            if end > self.text.len()
                || !self.text.is_char_boundary(edit.start)
                || !self.text.is_char_boundary(end)
            {
                return Err(EditRefusal::OutOfBounds);
            }
            if self.text[edit.start..end] != edit.removed {
                return Err(EditRefusal::Stale);
            }
            previous_end = end;
        }
        let count = edits.len();
        self.apply_transaction(edits, false)
            .map_err(EditRefusal::Refused)?;
        Ok(count)
    }

    /// Undoes one transaction, returning whether anything changed.
    pub fn undo(&mut self) -> bool {
        let Some(transaction) = self.undo.step_back() else {
            return false;
        };
        // Forward order, deliberately: every edit's `start` is in the
        // coordinates of the text *before* the transaction, so undoing the
        // earliest one first restores those coordinates for the ones that
        // follow. Walking backwards would apply each inverse at an offset the
        // still-applied earlier edits have already moved.
        for edit in &transaction.edits {
            let inverse = edit.inverse();
            splice(&mut self.text, &inverse);
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// Redoes one transaction, returning whether anything changed.
    pub fn redo(&mut self) -> bool {
        let Some(transaction) = self.undo.step_forward() else {
            return false;
        };
        // Reverse order, for the same reason applying does it: a later edit's
        // offsets are only valid while the text before it is untouched.
        for edit in transaction.edits.iter().rev() {
            splice(&mut self.text, edit);
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// The number of lines, counting a trailing newline as ending the last
    /// line rather than starting an extra one.
    pub fn line_count(&self) -> usize {
        self.text.lines().count().max(1)
    }

    pub fn byte_len(&self) -> usize {
        self.text.len()
    }

    /// The one-based line and column of a byte offset, for the status bar.
    ///
    /// The column counts characters rather than bytes, because a column that
    /// jumps by three when the caret passes an accented letter is wrong in the
    /// only way a user would notice.
    /// Converts a character index — what a text widget reports its caret at —
    /// into the byte offset every other method here speaks in.
    pub fn byte_offset_of_char(&self, characters: usize) -> usize {
        self.text
            .char_indices()
            .nth(characters)
            .map_or(self.text.len(), |(offset, _)| offset)
    }

    pub fn line_and_column(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.text.len());
        let before = &self.text[..offset];
        let line = before.matches('\n').count() + 1;
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        let column = self.text[line_start..offset].chars().count() + 1;
        (line, column)
    }

    /// Builds an edit that replaces `range`, clamped to the buffer and to char
    /// boundaries so a caller with a stale offset cannot panic the editor.
    fn edit_for(&self, range: std::ops::Range<usize>, replacement: &str) -> TextEdit {
        let start = floor_char_boundary(&self.text, range.start.min(self.text.len()));
        let end = floor_char_boundary(&self.text, range.end.min(self.text.len())).max(start);
        TextEdit {
            start,
            removed: self.text[start..end].to_owned(),
            inserted: replacement.to_owned(),
        }
    }

    /// Applies edits, refusing any transaction that would push the document
    /// past its bounds. A refused transaction changes nothing at all.
    fn apply_transaction(
        &mut self,
        edits: Vec<TextEdit>,
        coalescable: bool,
    ) -> Result<(), RefusalReason> {
        let mut candidate = self.text.clone();
        for edit in edits.iter().rev() {
            debug_assert!(edit.end() <= candidate.len());
            splice(&mut candidate, edit);
        }
        self.check_bounds(&candidate)?;
        self.text = candidate;
        self.undo.push(edits, coalescable);
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    fn check_bounds(&self, candidate: &str) -> Result<(), RefusalReason> {
        self.bounds.check_declared_size(candidate.len())?;
        let lines = candidate.lines().count().max(1);
        if lines > self.bounds.max_lines() {
            return Err(RefusalReason::TooManyLines {
                lines,
                limit: self.bounds.max_lines(),
            });
        }
        if let Some(line) = first_overlong_line(candidate, self.bounds.max_line_bytes()) {
            return Err(RefusalReason::LineTooLong {
                line,
                limit: self.bounds.max_line_bytes(),
            });
        }
        Ok(())
    }
}

fn splice(text: &mut String, edit: &TextEdit) {
    text.replace_range(edit.start..edit.end(), &edit.inserted);
}

/// Normalises CRLF and lone CR to LF without changing anything else.
fn normalise(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(character);
        }
    }
    out
}

/// Picks the ending a file is written back with: the one it mostly uses, and
/// LF when it has no line endings at all to copy.
fn detect_line_ending(text: &str) -> LineEnding {
    let crlf = text.matches("\r\n").count();
    let cr = text.matches('\r').count() - crlf;
    let lf = text.matches('\n').count() - crlf;
    if crlf >= lf && crlf >= cr && crlf > 0 {
        LineEnding::Crlf
    } else if cr > lf && cr > 0 {
        LineEnding::Cr
    } else {
        LineEnding::Lf
    }
}

/// Reports the most common leading indentation, preferring tabs when any
/// indented line uses them.
fn detect_indentation(text: &str) -> Indentation {
    let mut tabs = 0_usize;
    let mut widths: [usize; 9] = [0; 9];
    for line in text.lines() {
        if line.starts_with('\t') {
            tabs += 1;
            continue;
        }
        let spaces = line.len() - line.trim_start_matches(' ').len();
        if spaces > 0 && spaces <= 8 {
            widths[spaces] += 1;
        }
    }
    let space_lines: usize = widths.iter().sum();
    if tabs > space_lines {
        return Indentation::Tabs;
    }
    // Indentation is reported as the smallest width that actually appears,
    // because a file indented in fours also contains lines indented by eight.
    for (width, count) in widths.iter().enumerate().skip(1) {
        if *count > 0 {
            return Indentation::Spaces(width);
        }
    }
    Indentation::Spaces(4)
}

fn first_overlong_line(text: &str, limit: usize) -> Option<usize> {
    text.lines()
        .position(|line| line.len() > limit)
        .map(|index| index + 1)
}

fn common_prefix(left: &str, right: &str) -> usize {
    let limit = left.len().min(right.len());
    let mut index = 0;
    while index < limit && left.as_bytes()[index] == right.as_bytes()[index] {
        index += 1;
    }
    floor_char_boundary(left, index)
}

fn common_suffix(left: &str, right: &str) -> usize {
    let limit = left.len().min(right.len());
    let mut index = 0;
    while index < limit
        && left.as_bytes()[left.len() - index - 1] == right.as_bytes()[right.len() - index - 1]
    {
        index += 1;
    }
    let boundary = ceil_char_boundary(left, left.len() - index);
    left.len() - boundary
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {

    /// Undo has to walk the opposite way round from apply, or a transaction
    /// whose edits change length puts the earlier ones back in the wrong
    /// place -- the failure a substitution across many lines would hit first.
    #[test]
    fn undoing_a_multi_edit_transaction_restores_the_original_exactly() {
        let mut document = TextDocument::from_bytes(b"ab--ab", DocumentBounds::default()).unwrap();
        let edits = vec![
            TextEdit {
                start: 0,
                removed: "ab".to_owned(),
                inserted: "LONGER".to_owned(),
            },
            TextEdit {
                start: 4,
                removed: "ab".to_owned(),
                inserted: "X".to_owned(),
            },
        ];

        assert_eq!(document.apply_edits(edits), Ok(2));
        assert_eq!(document.text(), "LONGER--X");

        assert!(document.undo());
        assert_eq!(
            document.text(),
            "ab--ab",
            "every edit goes back where it was"
        );

        assert!(document.redo());
        assert_eq!(document.text(), "LONGER--X", "and comes back the same way");
    }

    #[test]
    fn a_whole_substitution_is_one_press_of_undo() {
        let mut document =
            TextDocument::from_bytes(b"one\ntwo\nthree\n", DocumentBounds::default()).unwrap();
        let edits = vec![
            TextEdit {
                start: 0,
                removed: "one".to_owned(),
                inserted: "1".to_owned(),
            },
            TextEdit {
                start: 4,
                removed: "two".to_owned(),
                inserted: "2".to_owned(),
            },
            TextEdit {
                start: 8,
                removed: "three".to_owned(),
                inserted: "3".to_owned(),
            },
        ];

        assert_eq!(document.apply_edits(edits), Ok(3));
        assert_eq!(document.text(), "1\n2\n3\n");

        assert!(document.undo());
        assert_eq!(document.text(), "one\ntwo\nthree\n");
        assert!(!document.can_undo(), "three lines changed, one transaction");
    }

    #[test]
    fn edits_planned_against_text_that_has_since_changed_are_refused_whole() {
        let mut document =
            TextDocument::from_bytes(b"alpha bravo", DocumentBounds::default()).unwrap();
        let stale = vec![TextEdit {
            start: 6,
            removed: "charlie".to_owned(),
            inserted: "x".to_owned(),
        }];

        assert_eq!(document.apply_edits(stale), Err(EditRefusal::OutOfBounds));

        let wrong_content = vec![TextEdit {
            start: 0,
            removed: "delta".to_owned(),
            inserted: "x".to_owned(),
        }];
        assert_eq!(document.apply_edits(wrong_content), Err(EditRefusal::Stale));
        assert_eq!(document.text(), "alpha bravo", "a refusal changes nothing");
        assert!(!document.can_undo());
    }

    #[test]
    fn overlapping_edits_are_refused_rather_than_applied_at_drifting_offsets() {
        let mut document = TextDocument::from_bytes(b"abcdef", DocumentBounds::default()).unwrap();
        let overlapping = vec![
            TextEdit {
                start: 0,
                removed: "abc".to_owned(),
                inserted: "x".to_owned(),
            },
            TextEdit {
                start: 2,
                removed: "cd".to_owned(),
                inserted: "y".to_owned(),
            },
        ];

        assert_eq!(
            document.apply_edits(overlapping),
            Err(EditRefusal::OutOfOrder)
        );
        assert_eq!(document.text(), "abcdef");
    }

    #[test]
    fn an_edit_landing_inside_a_character_is_refused() {
        let mut document =
            TextDocument::from_bytes("héllo".as_bytes(), DocumentBounds::default()).unwrap();
        let split = vec![TextEdit {
            start: 2,
            removed: "\u{a9}".to_owned(),
            inserted: "x".to_owned(),
        }];

        assert_eq!(document.apply_edits(split), Err(EditRefusal::OutOfBounds));
        assert_eq!(document.text(), "héllo");
    }

    use super::*;

    fn document(text: &str) -> TextDocument {
        TextDocument::from_bytes(text.as_bytes(), DocumentBounds::DEFAULT)
            .expect("the fixture should be editable")
    }

    #[test]
    fn a_crlf_file_is_edited_as_lf_and_written_back_as_crlf() {
        let mut doc = document("alpha\r\nbeta\r\n");
        assert_eq!(doc.text(), "alpha\nbeta\n");
        assert_eq!(doc.line_ending(), LineEnding::Crlf);
        doc.replace(0..5, "ALPHA").unwrap();
        assert_eq!(doc.to_bytes(), b"ALPHA\r\nbeta\r\n".to_vec());
    }

    #[test]
    fn a_bom_survives_a_round_trip() {
        let mut doc =
            TextDocument::from_bytes("\u{feff}alpha\n".as_bytes(), DocumentBounds::DEFAULT)
                .unwrap();
        assert_eq!(doc.encoding(), Encoding::Utf8Bom);
        assert_eq!(doc.text(), "alpha\n");
        doc.replace(5..5, "!").unwrap();
        assert_eq!(doc.to_bytes(), "\u{feff}alpha!\n".as_bytes().to_vec());
    }

    #[test]
    fn a_file_with_no_line_endings_is_written_back_with_lf() {
        let doc = document("alpha");
        assert_eq!(doc.line_ending(), LineEnding::Lf);
    }

    #[test]
    fn a_mixed_file_is_written_back_with_its_dominant_ending() {
        let doc = document("a\r\nb\r\nc\nd\r\n");
        assert_eq!(doc.line_ending(), LineEnding::Crlf);
        let doc = document("a\nb\nc\r\nd\n");
        assert_eq!(doc.line_ending(), LineEnding::Lf);
    }

    #[test]
    fn a_classic_mac_file_keeps_lone_carriage_returns() {
        let doc = document("alpha\rbeta\r");
        assert_eq!(doc.line_ending(), LineEnding::Cr);
        assert_eq!(doc.text(), "alpha\nbeta\n");
        assert_eq!(doc.to_bytes(), b"alpha\rbeta\r".to_vec());
    }

    #[test]
    fn binary_and_invalid_utf8_are_refused() {
        assert_eq!(
            TextDocument::from_bytes(b"ok\0nope", DocumentBounds::DEFAULT).unwrap_err(),
            RefusalReason::BinaryContent
        );
        assert_eq!(
            TextDocument::from_bytes(&[0xff, 0xfe, 0x41], DocumentBounds::DEFAULT).unwrap_err(),
            RefusalReason::NotUtf8
        );
    }

    #[test]
    fn oversize_line_and_byte_limits_are_refused_with_their_limit() {
        let bounds = DocumentBounds::new(64, 4, 16);
        let error = TextDocument::from_bytes(&[b'a'; 65], bounds).unwrap_err();
        assert!(matches!(error, RefusalReason::TooLarge { limit: 64, .. }));

        let error = TextDocument::from_bytes(b"a\nb\nc\nd\ne\n", bounds).unwrap_err();
        assert!(matches!(
            error,
            RefusalReason::TooManyLines { limit: 4, .. }
        ));

        let error = TextDocument::from_bytes(b"aaaaaaaaaaaaaaaaaaaa\n", bounds).unwrap_err();
        assert_eq!(error, RefusalReason::LineTooLong { line: 1, limit: 16 });
    }

    #[test]
    fn a_new_document_is_clean_and_becomes_dirty_on_the_first_edit() {
        let mut doc = document("alpha\n");
        assert!(!doc.is_dirty());
        doc.replace(0..0, "x").unwrap();
        assert!(doc.is_dirty());
        doc.mark_saved();
        assert!(!doc.is_dirty());
    }

    #[test]
    fn undoing_back_to_the_saved_content_is_not_dirty() {
        let mut doc = document("alpha\n");
        doc.mark_saved();
        doc.replace(0..0, "x").unwrap();
        assert!(doc.is_dirty());
        doc.undo();
        assert_eq!(doc.text(), "alpha\n");
        assert!(!doc.is_dirty());
    }

    #[test]
    fn undoing_past_a_save_leaves_the_document_dirty_again() {
        let mut doc = document("alpha\n");
        doc.replace(0..0, "x").unwrap();
        doc.mark_saved();
        assert!(!doc.is_dirty());
        doc.undo();
        assert_eq!(doc.text(), "alpha\n");
        assert!(doc.is_dirty());
    }

    #[test]
    fn a_view_edit_is_recovered_as_one_minimal_edit() {
        let mut doc = document("the quick brown fox\n");
        assert!(doc.sync_from_view("the quick red fox\n").unwrap());
        assert_eq!(doc.text(), "the quick red fox\n");
        doc.undo();
        assert_eq!(doc.text(), "the quick brown fox\n");
    }

    #[test]
    fn an_unchanged_view_records_no_edit() {
        let mut doc = document("alpha\n");
        assert!(!doc.sync_from_view("alpha\n").unwrap());
        assert!(!doc.can_undo());
        assert!(!doc.is_dirty());
    }

    #[test]
    fn a_multibyte_edit_does_not_split_a_character() {
        let mut doc = document("café au lait\n");
        assert!(doc.sync_from_view("café au lait!\n").unwrap());
        assert_eq!(doc.text(), "café au lait!\n");
        doc.undo();
        assert_eq!(doc.text(), "café au lait\n");

        let mut doc = document("naïve\n");
        assert!(doc.sync_from_view("naive\n").unwrap());
        assert_eq!(doc.text(), "naive\n");
    }

    #[test]
    fn typing_coalesces_but_undo_still_reaches_the_start() {
        let mut doc = document("");
        for (index, character) in "hello".char_indices() {
            let updated = format!("{}{character}", &"hello"[..index]);
            doc.sync_from_view(&updated).unwrap();
        }
        assert_eq!(doc.text(), "hello");
        assert!(doc.undo());
        assert_eq!(doc.text(), "");
    }

    #[test]
    fn replace_all_is_one_undo_transaction() {
        let mut doc = document("a b a b a\n");
        assert_eq!(doc.replace_all("a", "X").unwrap(), 3);
        assert_eq!(doc.text(), "X b X b X\n");
        assert!(doc.undo());
        assert_eq!(doc.text(), "a b a b a\n");
        assert!(doc.redo());
        assert_eq!(doc.text(), "X b X b X\n");
    }

    #[test]
    fn replace_all_handles_a_replacement_containing_the_query() {
        let mut doc = document("aa\n");
        assert_eq!(doc.replace_all("a", "aa").unwrap(), 2);
        assert_eq!(doc.text(), "aaaa\n");
    }

    #[test]
    fn replace_all_with_an_empty_query_does_nothing() {
        let mut doc = document("alpha\n");
        assert_eq!(doc.replace_all("", "x").unwrap(), 0);
        assert_eq!(doc.text(), "alpha\n");
    }

    #[test]
    fn an_edit_that_would_exceed_the_bounds_changes_nothing() {
        let bounds = DocumentBounds::new(16, 8, 16);
        let mut doc = TextDocument::from_bytes(b"alpha\n", bounds).unwrap();
        let error = doc
            .sync_from_view("alpha and a great deal more\n")
            .unwrap_err();
        assert!(matches!(error, RefusalReason::TooLarge { .. }));
        assert_eq!(doc.text(), "alpha\n");
        assert!(!doc.is_dirty());
    }

    #[test]
    fn a_reload_replaces_the_text_and_forgets_the_history() {
        let mut doc = document("alpha\n");
        doc.replace(0..5, "beta").unwrap();
        assert!(doc.can_undo());
        doc.reload_from(b"gamma\n").unwrap();
        assert_eq!(doc.text(), "gamma\n");
        assert!(!doc.can_undo());
        assert!(!doc.is_dirty());
    }

    #[test]
    fn a_refused_reload_leaves_the_buffer_alone() {
        let mut doc = document("alpha\n");
        assert!(doc.reload_from(b"bad\0bytes").is_err());
        assert_eq!(doc.text(), "alpha\n");
    }

    #[test]
    fn a_caret_measured_in_characters_maps_onto_bytes() {
        let doc = document("bêta\n");
        assert_eq!(doc.byte_offset_of_char(0), 0);
        assert_eq!(doc.byte_offset_of_char(2), 3);
        assert_eq!(doc.byte_offset_of_char(99), doc.text().len());
        assert_eq!(doc.line_and_column(doc.byte_offset_of_char(2)), (1, 3));
    }

    #[test]
    fn line_and_column_are_one_based_and_count_characters() {
        let doc = document("alpha\nbêta gamma\n");
        assert_eq!(doc.line_and_column(0), (1, 1));
        assert_eq!(doc.line_and_column(5), (1, 6));
        assert_eq!(doc.line_and_column(6), (2, 1));
        // After "bêta " — five characters, six bytes.
        assert_eq!(doc.line_and_column(12), (2, 6));
        assert_eq!(doc.line_count(), 2);
    }

    #[test]
    fn an_out_of_range_offset_is_clamped_rather_than_panicking() {
        let doc = document("alpha\n");
        assert_eq!(doc.line_and_column(9_999), (2, 1));
    }

    #[test]
    fn indentation_is_reported_from_the_file() {
        assert_eq!(
            document("a\n    b\n    c\n").indentation(),
            Indentation::Spaces(4)
        );
        assert_eq!(
            document("a\n  b\n  c\n").indentation(),
            Indentation::Spaces(2)
        );
        assert_eq!(document("a\n\tb\n\tc\n").indentation(), Indentation::Tabs);
        assert_eq!(
            document("no indentation\n").indentation(),
            Indentation::Spaces(4)
        );
    }

    #[test]
    fn labels_are_the_words_the_status_bar_shows() {
        assert_eq!(LineEnding::Crlf.label(), "CRLF");
        assert_eq!(Encoding::Utf8.label(), "UTF-8");
        assert_eq!(Indentation::Spaces(4).label(), "Spaces: 4");
        assert_eq!(Indentation::Tabs.label(), "Tabs");
    }

    #[test]
    fn an_empty_file_is_editable() {
        let doc = document("");
        assert_eq!(doc.text(), "");
        assert_eq!(doc.line_count(), 1);
        assert_eq!(doc.byte_len(), 0);
        assert!(!doc.is_dirty());
    }
}
