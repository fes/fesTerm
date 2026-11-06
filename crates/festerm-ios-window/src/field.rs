//! The text model UIKit sees for the focused form field.
//!
//! egui draws and edits form fields itself, so UIKit has no text view to drive
//! the keyboard's text-aware features (space-bar trackpad, marked text). The
//! app publishes the focused field as a [`FieldDocument`]; the native
//! `UITextInput` responder answers UIKit from this model, mirrors keyboard
//! edits optimistically, and queues them as [`FieldInput`] for egui to apply
//! in order. Offsets are `char` indices, matching egui's `CCursor`.

use std::{collections::VecDeque, ops::Range};

/// The focused field as egui drew it this frame, in view points.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldDocument {
    /// Distinguishes fields so a focus change resets the keyboard's context.
    pub field: u64,
    /// Displayed text; secure fields pass their masked glyphs, never secrets.
    pub text: String,
    pub selection: Range<usize>,
    /// Caret x for every offset `0..=chars`, so `carets.len() == chars + 1`.
    pub carets: Vec<f32>,
    pub top: f32,
    pub bottom: f32,
    /// Per-offset visual row bounds; empty for the legacy single-row fields.
    pub rows: Vec<(f32, f32)>,
    pub multiline: bool,
    pub secure: bool,
}

/// A keyboard edit for egui to apply to the focused field, in order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldKey {
    Tab,
    Escape,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Delete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldInput {
    Key(FieldKey),
    Unavailable(&'static str),
    Select(Range<usize>),
    Insert(String),
    DeleteBackward,
    Preedit {
        text: String,
        selected: Range<usize>,
    },
    Commit(String),
}

/// What changed when the app's view of the field replaced the native one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldSync {
    Unchanged,
    Selection,
    Text,
}

/// Most queued keyboard edits retained while egui is not drawing.
const QUEUE_LIMIT: usize = 256;

#[derive(Debug, Default)]
pub struct FieldModel {
    field: Option<u64>,
    chars: Vec<char>,
    selection: Range<usize>,
    marked: Option<Range<usize>>,
    carets: Vec<f32>,
    top: f32,
    bottom: f32,
    rows: Vec<(f32, f32)>,
    multiline: bool,
    secure: bool,
    queue: VecDeque<FieldInput>,
}

impl FieldModel {
    pub fn is_active(&self) -> bool {
        self.field.is_some()
    }

    pub fn secure(&self) -> bool {
        self.secure
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub fn text(&self, range: Range<usize>) -> String {
        let range = self.clamp(range);
        self.chars[range].iter().collect()
    }

    /// Adopts egui's field unless keyboard edits are still in flight, in
    /// which case the optimistic native copy stays authoritative.
    pub fn sync(&mut self, document: Option<FieldDocument>) -> FieldSync {
        let Some(document) = document else {
            let was = self.field.take().is_some();
            *self = Self::default();
            return if was {
                FieldSync::Text
            } else {
                FieldSync::Unchanged
            };
        };
        let chars: Vec<char> = document.text.chars().collect();
        self.carets = document.carets;
        self.top = document.top;
        self.bottom = document.bottom;
        self.rows = document.rows;
        self.multiline = document.multiline;
        self.secure = document.secure;
        if self.field != Some(document.field) {
            self.field = Some(document.field);
            self.chars = chars;
            self.selection = document.selection;
            self.selection = self.clamp(self.selection.clone());
            self.marked = None;
            self.queue.clear();
            return FieldSync::Text;
        }
        if !self.queue.is_empty() {
            return FieldSync::Unchanged;
        }
        if self.chars != chars {
            self.chars = chars;
            self.selection = self.clamp(document.selection);
            self.marked = None;
            return FieldSync::Text;
        }
        let selection = self.clamp(document.selection);
        if self.marked.is_none() && self.selection != selection {
            self.selection = selection;
            return FieldSync::Selection;
        }
        FieldSync::Unchanged
    }

    /// Edits for egui this frame. A selection change only leads a batch: egui
    /// applies it to the field's state before the batch's text events, so a
    /// later one waits for the next frame.
    pub fn take_batch(&mut self) -> Vec<FieldInput> {
        let mut batch = Vec::new();
        while let Some(input) = self.queue.front() {
            if matches!(input, FieldInput::Select(_)) && !batch.is_empty() {
                break;
            }
            batch.extend(self.queue.pop_front());
        }
        batch
    }

    pub fn has_pending(&self) -> bool {
        !self.queue.is_empty()
    }

    pub fn insert(&mut self, text: &str) {
        if let Some(marked) = self.marked.take() {
            self.splice(marked, text);
            self.push(FieldInput::Commit(text.to_owned()));
            return;
        }
        if !self.multiline && (text == "\n" || text == "\r") {
            self.push(FieldInput::Insert("\n".to_owned()));
            return;
        }
        self.splice(self.selection.clone(), text);
        self.push(FieldInput::Insert(text.to_owned()));
    }

    pub fn delete_backward(&mut self) {
        if let Some(marked) = self.marked.clone() {
            let mut text: Vec<char> = self.chars[marked].to_vec();
            text.pop();
            let text: String = text.into_iter().collect();
            self.set_marked(&text, text.chars().count()..text.chars().count());
            return;
        }
        let range = if self.selection.is_empty() {
            self.selection.start.saturating_sub(1)..self.selection.start
        } else {
            self.selection.clone()
        };
        if range.is_empty() {
            return;
        }
        self.splice(range, "");
        self.push(FieldInput::DeleteBackward);
    }

    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        let range = self.clamp(range);
        if self.marked.is_some() {
            self.unmark();
        }
        if range != self.selection {
            self.selection = range.clone();
            self.push(FieldInput::Select(range.clone()));
        }
        if text.is_empty() {
            if !range.is_empty() {
                self.splice(range, "");
                self.push(FieldInput::DeleteBackward);
            }
        } else {
            self.insert(text);
        }
    }

    pub fn set_selection(&mut self, range: Range<usize>) {
        let range = self.clamp(range);
        if range == self.selection || self.marked.is_some() {
            return;
        }
        self.selection = range.clone();
        if let Some(FieldInput::Select(last)) = self.queue.back_mut() {
            *last = range;
        } else {
            self.push(FieldInput::Select(range));
        }
    }

    /// `selected` is relative to `text`, in chars.
    pub fn set_marked(&mut self, text: &str, selected: Range<usize>) {
        let range = self.marked.clone().unwrap_or(self.selection.clone());
        let start = range.start;
        self.splice(range, text);
        let len = text.chars().count();
        let selected = selected.start.min(len)..selected.end.min(len).max(selected.start.min(len));
        if len == 0 {
            self.marked = None;
        } else {
            self.marked = Some(start..start + len);
        }
        self.selection = start + selected.start..start + selected.end;
        self.push(FieldInput::Preedit {
            text: text.to_owned(),
            selected,
        });
    }

    pub fn unmark(&mut self) {
        if let Some(marked) = self.marked.take() {
            let text = self.text(marked.clone());
            self.selection = marked.end..marked.end;
            self.push(FieldInput::Commit(text));
        }
    }

    /// Caret x for `offset`, clamped to the field.
    pub fn caret_x(&self, offset: usize) -> f32 {
        match self.carets.get(offset.min(self.chars.len())) {
            Some(x) => *x,
            None => self.carets.last().copied().unwrap_or(0.0),
        }
    }

    pub fn row(&self) -> (f32, f32) {
        self.row_at(self.selection.start)
    }

    pub fn row_at(&self, offset: usize) -> (f32, f32) {
        let (top, bottom) = self
            .rows
            .get(offset.min(self.chars.len()))
            .copied()
            .unwrap_or((self.top, self.bottom));
        (top, bottom.max(top + 1.0))
    }

    /// Offset whose caret is nearest `x`; the field is a single row.
    pub fn closest(&self, x: f32) -> usize {
        let (top, bottom) = self.row();
        self.closest_at(x, (top + bottom) / 2.0)
    }

    /// Prefer the nearest visual row before comparing horizontal carets.
    pub fn closest_at(&self, x: f32, y: f32) -> usize {
        let last = self.chars.len().min(self.carets.len().saturating_sub(1));
        (0..=last)
            .min_by(|a, b| {
                let center = |offset| {
                    let (top, bottom) = self.row_at(offset);
                    (top + bottom) / 2.0
                };
                (center(*a) - y)
                    .abs()
                    .total_cmp(&(center(*b) - y).abs())
                    .then_with(|| {
                        (self.caret_x(*a) - x)
                            .abs()
                            .total_cmp(&(self.caret_x(*b) - x).abs())
                    })
            })
            .unwrap_or(0)
    }

    pub fn vertical(&self, offset: usize, delta: isize) -> usize {
        let offset = offset.min(self.chars.len());
        if delta == 0 {
            return offset;
        }
        if !self.multiline {
            return if delta < 0 { 0 } else { self.chars.len() };
        }
        let mut rows = self.rows.clone();
        rows.dedup();
        let current = self.row_at(offset);
        let Some(index) = rows.iter().position(|row| *row == current) else {
            return offset;
        };
        let target = index.saturating_add_signed(delta);
        if target >= rows.len() {
            return self.chars.len();
        }
        if delta < 0 && delta.unsigned_abs() > index {
            return 0;
        }
        let (top, bottom) = rows[target];
        self.closest_at(self.caret_x(offset), (top + bottom) / 2.0)
    }

    pub fn clamp(&self, range: Range<usize>) -> Range<usize> {
        let len = self.chars.len();
        let start = range.start.min(len);
        start..range.end.clamp(start, len)
    }

    fn splice(&mut self, range: Range<usize>, text: &str) {
        let range = self.clamp(range);
        let start = range.start;
        let inserted: Vec<char> = text.chars().collect();
        let len = inserted.len();
        self.chars.splice(range, inserted);
        self.selection = start + len..start + len;
        // Carets past the edit are stale until egui republishes the field.
        let x = self.carets.get(start).copied().unwrap_or_default();
        self.carets.truncate(start + 1);
        self.carets.resize(self.chars.len() + 1, x);
        if !self.rows.is_empty() {
            let row = self.row_at(start);
            self.rows.truncate(start + 1);
            self.rows.resize(self.chars.len() + 1, row);
        }
    }

    fn push(&mut self, input: FieldInput) {
        if self.queue.len() < QUEUE_LIMIT {
            self.queue.push_back(input);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(field: u64, text: &str, caret: usize) -> FieldDocument {
        let chars = text.chars().count();
        FieldDocument {
            field,
            text: text.to_owned(),
            selection: caret..caret,
            carets: (0..=chars).map(|i| 10.0 + 8.0 * i as f32).collect(),
            top: 100.0,
            bottom: 120.0,
            rows: Vec::new(),
            multiline: false,
            secure: false,
        }
    }

    fn model(text: &str, caret: usize) -> FieldModel {
        let mut model = FieldModel::default();
        assert_eq!(model.sync(Some(document(1, text, caret))), FieldSync::Text);
        model
    }

    #[test]
    fn trackpad_moves_queue_one_coalesced_selection_ahead_of_typing() {
        let mut model = model("host", 4);
        model.set_selection(model.closest(27.0)..model.closest(27.0));
        model.set_selection(2..2);
        model.insert("X");
        model.set_selection(0..0);
        assert_eq!(model.text(0..5), "hoXst");
        assert_eq!(
            model.take_batch(),
            [FieldInput::Select(2..2), FieldInput::Insert("X".into())]
        );
        assert_eq!(model.take_batch(), [FieldInput::Select(0..0)]);
        assert!(model.take_batch().is_empty());
    }

    #[test]
    fn closest_and_caret_geometry_follow_the_published_carets() {
        let model = model("abc", 0);
        assert_eq!(model.closest(-50.0), 0);
        assert_eq!(model.closest(27.0), 2);
        assert_eq!(model.closest(500.0), 3);
        assert_eq!(model.caret_x(99), 34.0);
        assert_eq!(model.row(), (100.0, 120.0));
    }

    #[test]
    fn multiline_keyboard_uses_visual_rows_and_mirrors_return() {
        let mut field = document(1, "ab\ncd", 1);
        field.multiline = true;
        field.carets = vec![10.0, 18.0, 26.0, 10.0, 18.0, 26.0];
        field.rows = vec![(100.0, 120.0); 3];
        field.rows.extend([(120.0, 140.0); 3]);
        let mut model = FieldModel::default();
        model.sync(Some(field));
        assert_eq!(model.closest_at(18.0, 130.0), 4);
        assert_eq!(model.vertical(1, 1), 4);
        assert_eq!(model.vertical(4, -1), 1);
        assert_eq!(model.vertical(4, isize::MIN), 0);
        assert_eq!(model.vertical(1, isize::MAX), 5);
        assert_eq!(model.row_at(4), (120.0, 140.0));
        model.insert("\n");
        model.insert("X");
        assert_eq!(model.text(0..model.len()), "a\nXb\ncd");
        assert_eq!(model.selection(), 3..3);
        assert_eq!(
            model.take_batch(),
            [
                FieldInput::Insert("\n".into()),
                FieldInput::Insert("X".into())
            ]
        );
    }

    #[test]
    fn egui_is_adopted_only_when_no_keyboard_edit_is_in_flight() {
        let mut model = model("ab", 2);
        model.insert("c");
        assert_eq!(
            model.sync(Some(document(1, "ab", 2))),
            FieldSync::Unchanged,
            "a stale frame must not undo the optimistic edit"
        );
        assert_eq!(model.text(0..9), "abc");
        model.take_batch();
        assert_eq!(
            model.sync(Some(document(1, "abc", 3))),
            FieldSync::Unchanged
        );
        assert_eq!(
            model.sync(Some(document(1, "abc", 1))),
            FieldSync::Selection
        );
        assert_eq!(model.sync(Some(document(1, "xyz", 0))), FieldSync::Text);
        assert_eq!(model.sync(Some(document(2, "xyz", 0))), FieldSync::Text);
        assert_eq!(model.sync(None), FieldSync::Text);
        assert!(!model.is_active());
    }

    #[test]
    fn a_new_field_drops_edits_meant_for_the_previous_one() {
        let mut model = model("ab", 2);
        model.insert("c");
        model.sync(Some(document(2, "", 0)));
        assert!(model.take_batch().is_empty());
    }

    #[test]
    fn deletion_and_replacement_mirror_egui() {
        let mut model = model("hello", 5);
        model.delete_backward();
        model.replace(0..1, "J");
        model.replace(1..3, "");
        assert_eq!(model.text(0..9), "Jl");
        assert_eq!(model.take_batch(), [FieldInput::DeleteBackward]);
        assert_eq!(
            model.take_batch(),
            [FieldInput::Select(0..1), FieldInput::Insert("J".into())]
        );
        assert_eq!(
            model.take_batch(),
            [FieldInput::Select(1..3), FieldInput::DeleteBackward]
        );
        let mut empty = self::model("", 0);
        empty.delete_backward();
        assert!(empty.take_batch().is_empty(), "nothing to delete");
    }

    #[test]
    fn marked_text_becomes_preedit_then_commit() {
        let mut model = model("a", 1);
        model.set_marked("k", 1..1);
        model.set_marked("か", 1..1);
        model.set_selection(0..0);
        model.delete_backward();
        model.set_marked("き", 1..1);
        model.insert("木");
        model.set_marked("x", 1..1);
        model.unmark();
        assert_eq!(model.text(0..9), "a木x");
        assert_eq!(model.selection(), 3..3);
        assert_eq!(
            model.take_batch(),
            [
                FieldInput::Preedit {
                    text: "k".into(),
                    selected: 1..1
                },
                FieldInput::Preedit {
                    text: "か".into(),
                    selected: 1..1
                },
                FieldInput::Preedit {
                    text: "".into(),
                    selected: 0..0
                },
                FieldInput::Preedit {
                    text: "き".into(),
                    selected: 1..1
                },
                FieldInput::Commit("木".into()),
                FieldInput::Preedit {
                    text: "x".into(),
                    selected: 1..1
                },
                FieldInput::Commit("x".into()),
            ]
        );
    }

    #[test]
    fn return_is_queued_without_editing_the_field() {
        let mut model = model("ab", 2);
        model.insert("\n");
        assert_eq!(model.text(0..9), "ab");
        assert_eq!(model.take_batch(), [FieldInput::Insert("\n".into())]);
    }

    #[test]
    fn the_queue_is_bounded() {
        let mut model = model("", 0);
        for _ in 0..QUEUE_LIMIT + 10 {
            model.insert("a");
        }
        assert_eq!(model.take_batch().len(), QUEUE_LIMIT);
    }
}
