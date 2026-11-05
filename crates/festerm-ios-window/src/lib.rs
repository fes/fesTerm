//! UIKit keyboard geometry, isolated from terminal semantics and rendering.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyboardMode {
    Hidden,
    System,
    Terminal,
    /// The system keyboard edits the published form field via `UITextInput`.
    Field,
}

mod field;
pub use field::{FieldDocument, FieldInput, FieldModel, FieldSync};
mod dictation;
pub use dictation::{
    dictation_permission_prompt_in_flight, Dictation, DictationPhase, DictationText,
    MAX_DICTATION_BYTES,
};
mod document;
#[cfg(all(
    target_os = "ios",
    target_abi = "sim",
    feature = "native-document-smoke"
))]
pub use document::DocumentSmoke;
mod document_writer;
pub use document::{
    DocumentFile, DocumentFileError, DocumentFileEvent, DocumentFileSnapshot, DocumentPicker,
    MAX_DOCUMENT_BYTES, MAX_NATIVE_DOCUMENTS,
};
pub use document_writer::{
    local_file_mutation_epoch, try_local_file_mutation, DocumentWritePermit,
};

#[cfg(target_os = "ios")]
mod speech;

#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "ios")]
pub use ios::{
    attach_window, clipboard_text, open_url, register_scene_delegate, set_clipboard_text,
    KeyboardBridge, SCENE_DELEGATE_CLASS,
};

#[cfg(not(target_os = "ios"))]
pub struct KeyboardBridge;

#[cfg(not(target_os = "ios"))]
impl KeyboardBridge {
    pub fn new(
        _window: raw_window_handle::WindowHandle<'_>,
        _repaint: impl Fn() + Send + Sync + 'static,
    ) -> Option<Self> {
        None
    }

    pub fn occluded_height_fraction(&self) -> f32 {
        0.0
    }

    pub fn set_keyboard_mode(&self, _mode: KeyboardMode) -> Result<(), &'static str> {
        Ok(())
    }

    pub fn play_input_click(&self) {}

    pub fn set_field_document(&self, _document: Option<FieldDocument>) {}

    pub fn take_field_input(&self) -> Vec<FieldInput> {
        Vec::new()
    }

    pub fn document_picker(&self) -> Option<DocumentPicker> {
        None
    }
}

/// No UIScene exists off iOS.
#[cfg(not(target_os = "ios"))]
pub fn attach_window(_window: raw_window_handle::WindowHandle<'_>) {}

/// No UIScene delegate exists off iOS.
#[cfg(not(target_os = "ios"))]
pub fn register_scene_delegate() -> bool {
    false
}

/// The system pasteboard text; always `None` off iOS. Reading may prompt the
/// user ("Allow Paste") on recent iOS versions, so call it only from an
/// explicit paste action.
#[cfg(not(target_os = "ios"))]
pub fn clipboard_text() -> Option<String> {
    None
}

/// Replaces the system pasteboard text; returns whether it was written.
#[cfg(not(target_os = "ios"))]
pub fn set_clipboard_text(_text: &str) -> bool {
    false
}

/// Opens an `https://` URL in the system browser; always `false` off iOS.
#[cfg(not(target_os = "ios"))]
pub fn open_url(_url: &str) -> bool {
    false
}
