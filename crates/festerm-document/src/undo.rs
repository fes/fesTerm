//! Document-scoped undo history (ADR 0034 §1).
//!
//! History belongs to the *document*, not to a view: typing in one window and
//! undoing in another must be the same history, because two histories over one
//! buffer would let a window undo an edit it never saw.
//!
//! An entry is a whole transaction. Replace-all is therefore one press of undo
//! rather than one press per match (ADR 0034 §3), and a run of ordinary typing
//! coalesces into one entry until something interesting — a newline, a caret
//! jump, a save, or a different kind of edit — closes it.

use crate::text::TextEdit;

/// One undoable transaction: the edits that were applied, in order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Transaction {
    pub(crate) serial: u64,
    pub(crate) edits: Vec<TextEdit>,
    /// Whether a following single-character insertion may be merged into this
    /// transaction instead of starting a new one.
    pub(crate) open: bool,
}

impl Transaction {
    fn weight(&self) -> usize {
        self.edits
            .iter()
            .map(|edit| edit.removed.len() + edit.inserted.len())
            .sum()
    }
}

/// A bounded stack of transactions with a redo tail.
#[derive(Clone, Debug)]
pub struct UndoHistory {
    entries: Vec<Transaction>,
    /// How many entries from the front are currently applied to the text.
    applied: usize,
    next_serial: u64,
    max_entries: usize,
    max_bytes: usize,
    used_bytes: usize,
}

impl UndoHistory {
    /// The number of transactions retained before the oldest is forgotten.
    pub const DEFAULT_MAX_ENTRIES: usize = 2_048;
    /// The retained edit text budget, which bounds history for a document
    /// edited all day without ever being closed.
    pub const DEFAULT_MAX_BYTES: usize = 8 * 1024 * 1024;

    pub fn new() -> Self {
        Self::with_limits(Self::DEFAULT_MAX_ENTRIES, Self::DEFAULT_MAX_BYTES)
    }

    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: Vec::new(),
            applied: 0,
            next_serial: 1,
            max_entries: max_entries.max(1),
            max_bytes,
            used_bytes: 0,
        }
    }

    /// Identifies the exact point in history the text currently sits at.
    ///
    /// Saving records this token; the document is dirty whenever the current
    /// token differs from it. Comparing tokens rather than counting edits is
    /// what makes undoing back to the saved content clear the dirty flag
    /// instead of leaving a document that is "modified" but identical.
    pub(crate) fn token(&self) -> Option<u64> {
        self.entries
            .get(self.applied.checked_sub(1)?)
            .map(|entry| entry.serial)
    }

    pub fn can_undo(&self) -> bool {
        self.applied > 0
    }

    pub fn can_redo(&self) -> bool {
        self.applied < self.entries.len()
    }

    /// Prevents the next edit from merging into the current transaction.
    pub(crate) fn close_transaction(&mut self) {
        if let Some(entry) = self.entries.last_mut() {
            entry.open = false;
        }
    }

    /// Records one transaction, discarding any redo tail.
    pub(crate) fn push(&mut self, edits: Vec<TextEdit>, coalescable: bool) {
        if edits.is_empty() {
            return;
        }
        self.discard_redo_tail();

        if coalescable {
            if let Some(last) = self.entries.last_mut() {
                if last.open && Self::mergeable(last, &edits) {
                    for edit in edits {
                        self.used_bytes += edit.removed.len() + edit.inserted.len();
                        Self::merge(last, edit);
                    }
                    // A merge rewrites the transaction, so it needs a new
                    // identity: a token taken before the merge must not still
                    // compare equal afterwards.
                    last.serial = self.next_serial;
                    self.next_serial += 1;
                    self.enforce_limits();
                    return;
                }
            }
        }

        let open = coalescable
            && !edits
                .iter()
                .any(|edit| edit.inserted.contains('\n') || !edit.removed.is_empty());
        let entry = Transaction {
            serial: self.next_serial,
            edits,
            open,
        };
        self.next_serial += 1;
        self.used_bytes += entry.weight();
        self.entries.push(entry);
        self.applied = self.entries.len();
        self.enforce_limits();
    }

    /// Whether `edits` continues `last` rather than starting something new.
    ///
    /// Only a single insertion that begins exactly where the previous one
    /// ended continues it, and a newline always closes the transaction so undo
    /// never swallows a whole paragraph at once.
    fn mergeable(last: &Transaction, edits: &[TextEdit]) -> bool {
        let (Some(previous), [next]) = (last.edits.last(), edits) else {
            return false;
        };
        if !next.removed.is_empty() || next.inserted.contains('\n') {
            return false;
        }
        previous.removed.is_empty() && previous.start + previous.inserted.len() == next.start
    }

    fn merge(last: &mut Transaction, edit: TextEdit) {
        if let Some(previous) = last.edits.last_mut() {
            previous.inserted.push_str(&edit.inserted);
        }
    }

    fn discard_redo_tail(&mut self) {
        while self.entries.len() > self.applied {
            if let Some(entry) = self.entries.pop() {
                self.used_bytes = self.used_bytes.saturating_sub(entry.weight());
            }
        }
    }

    /// Forgets the oldest transactions once either bound is exceeded.
    ///
    /// Forgetting history never changes the text, so an undo that is no longer
    /// available simply stops being offered rather than failing mid-way.
    fn enforce_limits(&mut self) {
        while self.entries.len() > self.max_entries
            || (self.used_bytes > self.max_bytes && self.entries.len() > 1)
        {
            let entry = self.entries.remove(0);
            self.used_bytes = self.used_bytes.saturating_sub(entry.weight());
            self.applied = self.applied.saturating_sub(1);
        }
    }

    /// Returns the transaction to invert, and steps back.
    pub(crate) fn step_back(&mut self) -> Option<Transaction> {
        let index = self.applied.checked_sub(1)?;
        self.applied = index;
        self.entries.get(index).cloned()
    }

    /// Returns the transaction to re-apply, and steps forward.
    pub(crate) fn step_forward(&mut self) -> Option<Transaction> {
        let entry = self.entries.get(self.applied).cloned()?;
        self.applied += 1;
        Some(entry)
    }

    /// Drops everything, which a reload from the source must do: the history
    /// describes edits to bytes that are no longer the document's content.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.applied = 0;
        self.used_bytes = 0;
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

impl Default for UndoHistory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(start: usize, text: &str) -> TextEdit {
        TextEdit {
            start,
            removed: String::new(),
            inserted: text.to_owned(),
        }
    }

    #[test]
    fn typing_coalesces_into_one_transaction() {
        let mut history = UndoHistory::new();
        history.push(vec![insert(0, "a")], true);
        history.push(vec![insert(1, "b")], true);
        history.push(vec![insert(2, "c")], true);
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn a_newline_closes_the_transaction() {
        let mut history = UndoHistory::new();
        history.push(vec![insert(0, "a")], true);
        history.push(vec![insert(1, "\n")], true);
        history.push(vec![insert(2, "b")], true);
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn a_caret_jump_starts_a_new_transaction() {
        let mut history = UndoHistory::new();
        history.push(vec![insert(0, "a")], true);
        history.push(vec![insert(40, "b")], true);
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn a_non_coalescable_transaction_stands_alone() {
        let mut history = UndoHistory::new();
        history.push(vec![insert(0, "a")], true);
        history.push(vec![insert(1, "x"), insert(2, "y")], false);
        history.push(vec![insert(3, "b")], true);
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn the_token_tracks_the_position_in_history() {
        let mut history = UndoHistory::new();
        assert_eq!(history.token(), None);
        history.push(vec![insert(0, "a")], false);
        let after_first = history.token();
        history.push(vec![insert(1, "b")], false);
        assert_ne!(history.token(), after_first);
        history.step_back();
        assert_eq!(history.token(), after_first);
    }

    #[test]
    fn a_merge_changes_the_token_so_a_saved_document_becomes_dirty_again() {
        let mut history = UndoHistory::new();
        history.push(vec![insert(0, "a")], true);
        let saved = history.token();
        history.push(vec![insert(1, "b")], true);
        assert_ne!(history.token(), saved);
    }

    #[test]
    fn redo_is_discarded_by_a_new_edit() {
        let mut history = UndoHistory::new();
        history.push(vec![insert(0, "a")], false);
        history.push(vec![insert(1, "b")], false);
        history.step_back();
        assert!(history.can_redo());
        history.push(vec![insert(1, "c")], false);
        assert!(!history.can_redo());
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn undo_and_redo_walk_the_same_transactions() {
        let mut history = UndoHistory::new();
        history.push(vec![insert(0, "first")], false);
        history.push(vec![insert(5, "second")], false);
        let undone = history.step_back().expect("a transaction to undo");
        assert_eq!(undone.edits[0].inserted, "second");
        let redone = history.step_forward().expect("a transaction to redo");
        assert_eq!(redone.edits[0].inserted, "second");
        assert!(!history.can_redo());
    }

    #[test]
    fn history_is_bounded_by_entry_count() {
        let mut history = UndoHistory::with_limits(3, usize::MAX);
        for index in 0..10 {
            history.push(vec![insert(index, "x")], false);
        }
        assert_eq!(history.len(), 3);
        assert!(history.can_undo());
    }

    #[test]
    fn history_is_bounded_by_retained_bytes() {
        let mut history = UndoHistory::with_limits(usize::MAX, 16);
        for index in 0..10 {
            history.push(vec![insert(index, "0123456789")], false);
        }
        assert!(history.len() <= 2, "history kept {} entries", history.len());
    }

    #[test]
    fn clearing_forgets_everything() {
        let mut history = UndoHistory::new();
        history.push(vec![insert(0, "a")], false);
        history.clear();
        assert!(!history.can_undo());
        assert!(!history.can_redo());
        assert_eq!(history.token(), None);
    }
}
