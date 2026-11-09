use std::sync::atomic::{AtomicU64, Ordering};

use eframe::egui;
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Default)]
pub(crate) struct SecretText {
    bytes: Zeroizing<Vec<u8>>,
}

impl SecretText {
    #[cfg(test)]
    pub(crate) fn new(text: impl Into<String>) -> Self {
        let mut text = text.into();
        let bytes = Zeroizing::new(text.as_bytes().to_vec());
        text.zeroize();
        Self { bytes }
    }

    pub(crate) fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes).expect("secret text remains valid UTF-8")
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.bytes.zeroize();
        self.bytes.clear();
    }

    pub(crate) fn take(&mut self) -> String {
        let result =
            String::from_utf8(self.bytes.as_slice().to_vec()).expect("secret text is valid UTF-8");
        self.clear();
        result
    }

    pub(crate) fn truncate_to_bytes(&mut self, limit: usize) -> bool {
        if self.bytes.len() <= limit {
            return false;
        }
        let mut end = limit;
        while !self.as_str().is_char_boundary(end) {
            end -= 1;
        }
        self.bytes[end..].zeroize();
        self.bytes.truncate(end);
        true
    }

    fn reserve_for(&mut self, required: usize) {
        if required <= self.bytes.capacity() {
            return;
        }
        let mut replacement = Zeroizing::new(Vec::with_capacity(required.next_power_of_two()));
        replacement.extend_from_slice(&self.bytes);
        self.clear();
        self.bytes = replacement;
    }
}

impl egui::TextBuffer for SecretText {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        self.as_str()
    }

    fn insert_text(&mut self, text: &str, char_index: egui::text::CharIndex) -> usize {
        let byte_index = self
            .as_str()
            .char_indices()
            .nth(char_index.0)
            .map_or(self.bytes.len(), |(index, _)| index);
        let old_len = self.bytes.len();
        let new_len = old_len + text.len();
        self.reserve_for(new_len);
        self.bytes.resize(new_len, 0);
        self.bytes
            .copy_within(byte_index..old_len, byte_index + text.len());
        self.bytes[byte_index..byte_index + text.len()].copy_from_slice(text.as_bytes());
        text.chars().count()
    }

    fn delete_char_range(&mut self, char_range: std::ops::Range<egui::text::CharIndex>) {
        let value = self.as_str();
        let start = value
            .char_indices()
            .nth(char_range.start.0)
            .map_or(value.len(), |(index, _)| index);
        let end = value
            .char_indices()
            .nth(char_range.end.0)
            .map_or(value.len(), |(index, _)| index);
        let old_len = self.bytes.len();
        self.bytes.copy_within(end..old_len, start);
        let new_len = old_len - (end - start);
        self.bytes[new_len..old_len].zeroize();
        self.bytes.truncate(new_len);
    }

    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<Self>()
    }
}

pub(crate) fn new_secret_widget_id() -> egui::Id {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    egui::Id::new((
        "festerm-sensitive-input",
        NEXT_ID.fetch_add(1, Ordering::Relaxed),
    ))
}

pub(crate) fn clear_secret_widget(context: &egui::Context, id: egui::Id) {
    context.data_mut(|data| {
        if let Some(mut state) = data.get_persisted::<egui::text_edit::TextEditState>(id) {
            state.clear_undoer_with(|(_, text)| text.zeroize());
        }
        data.remove::<egui::text_edit::TextEditState>(id);
    });
}

pub(crate) struct SecretWidget {
    id: egui::Id,
    context: Option<egui::Context>,
}

impl Default for SecretWidget {
    fn default() -> Self {
        Self {
            id: new_secret_widget_id(),
            context: None,
        }
    }
}

impl SecretWidget {
    pub(crate) fn id(&mut self, context: &egui::Context) -> egui::Id {
        self.context = Some(context.clone());
        self.id
    }

    pub(crate) fn clear(&self) {
        if let Some(context) = &self.context {
            clear_secret_widget(context, self.id);
        }
    }
}

impl Drop for SecretWidget {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_byte_limit_never_splits_utf8_and_take_clears_the_draft() {
        let mut secret = SecretText::new("é🙂x");
        assert!(secret.truncate_to_bytes(5));
        assert_eq!(secret.as_str(), "é");
        assert!(!secret.truncate_to_bytes(2));
        assert_eq!(secret.take(), "é");
        assert!(secret.is_empty());
    }
}
