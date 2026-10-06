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

use std::collections::VecDeque;
use std::mem::size_of;

use crate::bounds::RefusalReason;
use crate::text::TextEdit;

/// One undoable transaction: the edits that were applied, in order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Transaction {
    pub(crate) serial: u64,
    pub(crate) edits: Box<[TextEdit]>,
    /// Whether a following single-character insertion may be merged into this
    /// transaction instead of starting a new one.
    pub(crate) open: bool,
}

impl Transaction {
    fn weight(&self) -> usize {
        edit_storage(&self.edits)
    }
}

fn edit_storage(edits: &[TextEdit]) -> usize {
    edits
        .iter()
        .fold(std::mem::size_of_val(edits), |bytes, edit| {
            bytes
                .saturating_add(edit.removed.capacity())
                .saturating_add(edit.inserted.capacity())
        })
}

fn slot_storage(capacity: usize) -> usize {
    capacity.saturating_mul(size_of::<Transaction>())
}

#[derive(Debug)]
pub(crate) struct PreparedUndo {
    pub(crate) edits: Box<[TextEdit]>,
    open: bool,
    merge: bool,
    replacement: Option<String>,
    retention: RetentionPlan,
}

#[derive(Debug)]
struct RetentionPlan {
    retired: usize,
    payload_bytes: usize,
    slots: Option<VecDeque<Transaction>>,
}

/// A bounded stack of transactions with a redo tail.
#[derive(Debug)]
pub struct UndoHistory {
    entries: VecDeque<Transaction>,
    /// How many entries from the front are currently applied to the text.
    applied: usize,
    base_token: Option<u64>,
    next_serial: u64,
    max_entries: usize,
    max_bytes: usize,
    payload_bytes: usize,
}

impl UndoHistory {
    /// The number of transactions retained before the oldest is forgotten.
    pub const DEFAULT_MAX_ENTRIES: usize = 2_048;
    /// The retained allocation budget, including edit descriptors, text
    /// capacities and transaction slots, for one document.
    pub const DEFAULT_MAX_BYTES: usize = 8 * 1024 * 1024;

    pub fn new() -> Self {
        Self::with_limits(Self::DEFAULT_MAX_ENTRIES, Self::DEFAULT_MAX_BYTES)
    }

    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            applied: 0,
            base_token: None,
            next_serial: 1,
            max_entries: max_entries.max(1),
            max_bytes,
            payload_bytes: 0,
        }
    }

    /// Identifies the exact point in history the text currently sits at.
    ///
    /// Saving records this token; the document is dirty whenever the current
    /// token differs from it. Comparing tokens rather than counting edits is
    /// what makes undoing back to the saved content clear the dirty flag
    /// instead of leaving a document that is "modified" but identical.
    pub(crate) fn token(&self) -> Option<u64> {
        match self.applied.checked_sub(1) {
            Some(index) => self.entries.get(index).map(|entry| entry.serial),
            None => self.base_token,
        }
    }

    pub fn can_undo(&self) -> bool {
        self.applied > 0
    }

    pub fn can_redo(&self) -> bool {
        self.applied < self.entries.len()
    }

    /// Prevents the next edit from merging into the current transaction.
    pub(crate) fn close_transaction(&mut self) {
        if let Some(index) = self.applied.checked_sub(1) {
            self.entries[index].open = false;
        }
    }

    /// Stages allocations and admission without changing history or its redo
    /// tail. The document commits this only after validating its candidate text.
    pub(crate) fn prepare(
        &self,
        mut edits: Vec<TextEdit>,
        coalescable: bool,
    ) -> Result<Option<PreparedUndo>, RefusalReason> {
        edits.retain(|edit| edit.removed != edit.inserted);
        if edits.is_empty() {
            return Ok(None);
        }
        let edits = edits.into_boxed_slice();
        let redo_bytes = self
            .entries
            .iter()
            .skip(self.applied)
            .fold(0usize, |bytes, entry| bytes.saturating_add(entry.weight()));
        let applied_bytes = self.payload_bytes - redo_bytes;
        if coalescable {
            if let Some(last) = self
                .applied
                .checked_sub(1)
                .and_then(|index| self.entries.get(index))
            {
                if last.open && Self::mergeable(last, &edits) {
                    let previous = &last.edits[last.edits.len() - 1];
                    let needed = previous
                        .inserted
                        .len()
                        .saturating_add(edits[0].inserted.len());
                    let fixed = last.weight() - previous.inserted.capacity();
                    let maximum_capacity = self
                        .max_bytes
                        .saturating_sub(fixed.saturating_add(slot_storage(1)));
                    if needed <= maximum_capacity {
                        let replacement = (needed > previous.inserted.capacity()).then(|| {
                            let capacity = previous
                                .inserted
                                .capacity()
                                .saturating_mul(2)
                                .max(needed)
                                .min(maximum_capacity);
                            let mut text = String::with_capacity(capacity);
                            text.push_str(&previous.inserted);
                            text.push_str(&edits[0].inserted);
                            text
                        });
                        let capacity = replacement
                            .as_ref()
                            .map_or(previous.inserted.capacity(), String::capacity);
                        let payload_bytes = (applied_bytes - last.weight())
                            .saturating_add(fixed.saturating_add(capacity));
                        if let Ok(retention) = self.plan_retention(payload_bytes, self.applied, 1) {
                            return Ok(Some(PreparedUndo {
                                edits,
                                open: true,
                                merge: true,
                                replacement,
                                retention,
                            }));
                        }
                    }
                    // Coalescing is optional: a fitting new edit starts a new
                    // transaction when the combined run exceeds the budget.
                }
            }
        }
        let open = coalescable
            && edits.len() == 1
            && !edits
                .iter()
                .any(|edit| edit.inserted.contains('\n') || !edit.removed.is_empty());
        let payload_bytes = applied_bytes.saturating_add(edit_storage(&edits));
        let retention = self
            .plan_retention(payload_bytes, self.applied + 1, 0)
            .map_err(|bytes| RefusalReason::UndoStorageTooLarge {
                bytes,
                limit: self.max_bytes,
            })?;
        Ok(Some(PreparedUndo {
            edits,
            open,
            merge: false,
            replacement: None,
            retention,
        }))
    }

    fn plan_retention(
        &self,
        mut payload_bytes: usize,
        mut count: usize,
        protected: usize,
    ) -> Result<RetentionPlan, usize> {
        let mut retired = 0;
        while count > self.max_entries
            || payload_bytes.saturating_add(slot_storage(count)) > self.max_bytes
        {
            if retired >= self.applied - protected {
                return Err(payload_bytes.saturating_add(slot_storage(count)));
            }
            payload_bytes -= self.entries[retired].weight();
            count -= 1;
            retired += 1;
        }
        let maximum_slots = (self.max_bytes - payload_bytes) / size_of::<Transaction>();
        let slots = if self.entries.capacity() >= count && self.entries.capacity() <= maximum_slots
        {
            None
        } else {
            let capacity = self
                .entries
                .capacity()
                .saturating_mul(2)
                .max(4)
                .max(count)
                .min(self.max_entries)
                .min(maximum_slots);
            let slots = VecDeque::with_capacity(capacity);
            let actual_bytes = payload_bytes.saturating_add(slot_storage(slots.capacity()));
            if actual_bytes > self.max_bytes {
                return Err(actual_bytes);
            }
            Some(slots)
        };
        Ok(RetentionPlan {
            retired,
            payload_bytes,
            slots,
        })
    }

    pub(crate) fn commit(&mut self, prepared: PreparedUndo) {
        while self.entries.len() > self.applied {
            self.entries.pop_back();
        }
        for _ in 0..prepared.retention.retired {
            let retired = self
                .entries
                .pop_front()
                .expect("admission retires an applied entry");
            self.base_token = Some(retired.serial);
        }
        if let Some(mut slots) = prepared.retention.slots {
            slots.extend(self.entries.drain(..));
            self.entries = slots;
        }
        if prepared.merge {
            let last = self
                .entries
                .back_mut()
                .expect("coalescing has an applied entry");
            let previous = last.edits.last_mut().expect("coalescing has an insertion");
            if let Some(replacement) = prepared.replacement {
                previous.inserted = replacement;
            } else {
                previous.inserted.push_str(&prepared.edits[0].inserted);
            }
            last.serial = self.next_serial;
        } else {
            self.entries.push_back(Transaction {
                serial: self.next_serial,
                edits: prepared.edits,
                open: prepared.open,
            });
        }
        self.next_serial += 1;
        self.applied = self.entries.len();
        self.payload_bytes = prepared.retention.payload_bytes;
        debug_assert!(self.retained_bytes() <= self.max_bytes);
    }

    fn retained_bytes(&self) -> usize {
        self.payload_bytes
            .saturating_add(slot_storage(self.entries.capacity()))
    }

    #[cfg(test)]
    pub(crate) fn push(&mut self, edits: Vec<TextEdit>, coalescable: bool) {
        if let Some(prepared) = self
            .prepare(edits, coalescable)
            .expect("fixture transaction must fit")
        {
            self.commit(prepared);
        }
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

    /// Returns the transaction to invert, and steps back.
    pub(crate) fn step_back(&mut self) -> Option<Transaction> {
        let index = self.applied.checked_sub(1)?;
        self.close_transaction();
        self.applied = index;
        self.entries.get(index).cloned()
    }

    /// Returns the transaction to re-apply, and steps forward.
    pub(crate) fn step_forward(&mut self) -> Option<Transaction> {
        let entry = self.entries.get_mut(self.applied)?;
        entry.open = false;
        let entry = entry.clone();
        self.applied += 1;
        Some(entry)
    }

    /// Drops everything, which a reload from the source must do: the history
    /// describes edits to bytes that are no longer the document's content.
    pub(crate) fn clear(&mut self) {
        self.entries = VecDeque::new();
        self.applied = 0;
        self.base_token = None;
        self.payload_bytes = 0;
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

impl Clone for UndoHistory {
    fn clone(&self) -> Self {
        let entries = self.entries.clone();
        let payload_bytes = entries
            .iter()
            .fold(0usize, |bytes, entry| bytes.saturating_add(entry.weight()));
        Self {
            entries,
            applied: self.applied,
            base_token: self.base_token,
            next_serial: self.next_serial,
            max_entries: self.max_entries,
            max_bytes: self.max_bytes,
            payload_bytes,
        }
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

    fn retained_storage(history: &UndoHistory) -> usize {
        history.entries.capacity() * std::mem::size_of::<Transaction>()
            + history
                .entries
                .iter()
                .map(|entry| {
                    entry.edits.len() * std::mem::size_of::<TextEdit>()
                        + entry
                            .edits
                            .iter()
                            .map(|edit| edit.removed.capacity() + edit.inserted.capacity())
                            .sum::<usize>()
                })
                .sum::<usize>()
    }

    #[test]
    fn undo_retention_weight_charges_descriptors_and_reserved_text() {
        let mut inserted = String::with_capacity(4_096);
        inserted.push('x');
        let expected = std::mem::size_of::<TextEdit>() + inserted.capacity();
        let transaction = Transaction {
            serial: 1,
            edits: vec![TextEdit {
                start: 0,
                removed: String::new(),
                inserted,
            }]
            .into(),
            open: false,
        };
        assert_eq!(
            transaction.weight(),
            expected,
            "undo weight must include retained metadata and reserved text"
        );
    }

    #[test]
    fn undo_retention_metadata_churn_stays_within_the_actual_storage_budget() {
        let mut history = UndoHistory::with_limits(1_024, 64 * 1024);
        for _ in 0..64 {
            history.push((0..128).map(|index| insert(index, "x")).collect(), false);
        }
        assert!(
            retained_storage(&history) <= 64 * 1024,
            "retained undo storage exceeded the approved byte ceiling: {}",
            retained_storage(&history)
        );
    }

    #[test]
    fn undo_retention_clear_releases_transaction_slot_capacity() {
        let mut history = UndoHistory::new();
        for index in 0..128 {
            history.push(vec![insert(index, "x")], false);
        }
        assert!(history.entries.capacity() >= 128);
        history.clear();
        assert_eq!(
            history.entries.capacity(),
            0,
            "reload must release retired undo allocation"
        );
        assert_eq!(retained_storage(&history), 0);
    }

    #[test]
    fn undo_retention_admission_compacts_spare_edit_descriptors() {
        let mut history = UndoHistory::with_limits(16, 1_024);
        let mut edits = Vec::with_capacity(4_096);
        edits.push(insert(0, "x"));
        let prepared = history.prepare(edits, false).unwrap().unwrap();
        assert_eq!(prepared.edits.len(), 1);
        history.commit(prepared);
        assert_eq!(history.len(), 1);
        assert_eq!(history.payload_bytes, history.entries[0].weight());
        assert_eq!(history.retained_bytes(), retained_storage(&history));
        assert!(history.retained_bytes() <= 1_024);
    }

    #[test]
    fn undo_retention_exact_byte_boundary_refuses_one_more_reserved_byte_without_mutation() {
        let mut inserted = String::with_capacity(128);
        inserted.push('x');
        let budget = slot_storage(1) + size_of::<TextEdit>() + inserted.capacity();
        let mut history = UndoHistory::with_limits(1, budget);
        history.push(
            vec![TextEdit {
                start: 0,
                removed: String::new(),
                inserted,
            }],
            false,
        );
        assert_eq!(history.retained_bytes(), budget);
        history.step_back();
        let before = history.entries.clone();
        let token = history.token();
        let serial = history.next_serial;
        let mut inserted = String::with_capacity(129);
        inserted.push('y');
        let needed = slot_storage(1) + size_of::<TextEdit>() + inserted.capacity();
        assert!(
            matches!(history.prepare(vec![TextEdit { start: 0, removed: String::new(), inserted }], false), Err(RefusalReason::UndoStorageTooLarge { bytes, limit }) if bytes == needed && limit == budget)
        );
        assert_eq!(history.entries, before);
        assert_eq!(history.token(), token);
        assert_eq!(history.next_serial, serial);
        assert_eq!(history.retained_bytes(), budget);
        assert!(history.can_redo());
    }

    #[test]
    fn undo_retention_clone_recounts_compacted_allocations_without_changing_history() {
        let mut inserted = String::with_capacity(4_096);
        inserted.push('x');
        let mut history = UndoHistory::new();
        history.push(
            vec![TextEdit {
                start: 0,
                removed: String::new(),
                inserted,
            }],
            false,
        );
        history.step_back();
        let copy = history.clone();
        assert_eq!(copy.entries, history.entries);
        assert_eq!(copy.token(), history.token());
        assert_eq!(copy.can_redo(), history.can_redo());
        assert_eq!(
            copy.payload_bytes,
            copy.entries.iter().map(Transaction::weight).sum::<usize>()
        );
        assert_eq!(copy.retained_bytes(), retained_storage(&copy));
        assert!(copy.retained_bytes() < history.retained_bytes());
    }

    #[test]
    fn undo_retention_churn_keeps_exact_accounting_across_redo_clone_and_clear() {
        let mut history = UndoHistory::with_limits(5, 256);
        for index in 0..2_048 {
            if index % 31 == 0 {
                history.clear();
            } else if index % 7 == 0 {
                history.step_back();
            } else if index % 11 == 0 {
                history.step_forward();
            } else {
                history.push(vec![insert(index, "x")], index % 2 == 0);
            }
            assert_eq!(
                history.payload_bytes,
                history
                    .entries
                    .iter()
                    .map(Transaction::weight)
                    .sum::<usize>()
            );
            assert_eq!(history.retained_bytes(), retained_storage(&history));
            assert!(history.retained_bytes() <= 256);
            assert!(history.len() <= 5);
            assert!(history.applied <= history.len());
            let copy = history.clone();
            assert_eq!(copy.entries, history.entries);
            assert_eq!(copy.token(), history.token());
            assert_eq!(
                copy.payload_bytes,
                copy.entries.iter().map(Transaction::weight).sum::<usize>()
            );
            assert!(copy.retained_bytes() <= 256);
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
        let budget = slot_storage(1) + size_of::<TextEdit>() + 10;
        let mut history = UndoHistory::with_limits(usize::MAX, budget);
        for index in 0..10 {
            history.push(vec![insert(index, "0123456789")], false);
        }
        assert_eq!(history.len(), 1);
        assert_eq!(history.retained_bytes(), budget);
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
