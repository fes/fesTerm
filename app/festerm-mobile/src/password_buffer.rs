//! A text buffer for secrets that wipes every superseded allocation.

use eframe::egui;
use zeroize::{Zeroize, Zeroizing};

#[derive(Default)]
pub(crate) struct PasswordBuffer(pub(crate) Zeroizing<String>);

impl PasswordBuffer {
    pub(crate) fn clear(&mut self) {
        self.0.zeroize();
    }

    pub(crate) fn take(&mut self) -> String {
        std::mem::take(&mut *self.0)
    }

    fn byte_index(&self, character: usize) -> usize {
        self.0
            .char_indices()
            .nth(character)
            .map_or(self.0.len(), |(index, _)| index)
    }
}

impl egui::TextBuffer for PasswordBuffer {
    fn is_mutable(&self) -> bool {
        true
    }
    fn as_str(&self) -> &str {
        &self.0
    }
    fn insert_text(&mut self, text: &str, character: egui::text::CharIndex) -> usize {
        let index = self.byte_index(character.0);
        let remaining = 4096_usize.saturating_sub(self.0.chars().count());
        let mut next = Zeroizing::new(String::with_capacity(self.0.len() + text.len().min(16384)));
        next.push_str(&self.0[..index]);
        let mut inserted = 0;
        for character in text.chars().take(remaining) {
            next.push(character);
            inserted += 1;
        }
        next.push_str(&self.0[index..]);
        self.0 = next; // Wipe the old allocation, including deleted/reallocated content.
        inserted
    }
    fn delete_char_range(&mut self, range: std::ops::Range<egui::text::CharIndex>) {
        let start = self.byte_index(range.start.0);
        let end = self.byte_index(range.end.0).max(start);
        let mut next = Zeroizing::new(String::with_capacity(self.0.len()));
        next.push_str(&self.0[..start]);
        next.push_str(&self.0[end..]);
        self.0 = next;
    }
    fn clear(&mut self) {
        PasswordBuffer::clear(self);
    }
    fn take(&mut self) -> String {
        PasswordBuffer::take(self)
    }
    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<Self>()
    }
}
