use crate::{DocumentPicker, FieldDocument, FieldInput, FieldKey, KeyboardMode};
use festerm_android_platform as native;
use std::{
    cell::{Cell, RefCell},
    sync::Arc,
};

pub struct KeyboardBridge {
    field: RefCell<Option<FieldDocument>>,
    error: RefCell<Option<&'static str>>,
    epoch: Cell<u64>,
}

impl KeyboardBridge {
    pub fn new(
        _window: raw_window_handle::WindowHandle<'_>,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Option<Self> {
        if let Err(error) = native::set_repaint(Arc::new(repaint)) {
            eprintln!("fesTerm: {error}");
            return None;
        }
        Some(Self {
            field: RefCell::new(None),
            error: RefCell::new(None),
            epoch: Cell::new(native::input_epoch()),
        })
    }

    pub fn occluded_height_fraction(&self) -> f32 {
        native::occlusion()
    }

    pub fn set_keyboard_mode(&self, mode: KeyboardMode) -> Result<(), &'static str> {
        if let Some(error) = self.error.borrow_mut().take() {
            return Err(error);
        }
        native::keyboard_mode(match mode {
            KeyboardMode::Hidden => 0,
            KeyboardMode::System => 1,
            KeyboardMode::Terminal => 2,
            KeyboardMode::Field => 3,
            KeyboardMode::FieldCustom => 4,
        })
    }

    pub fn play_input_click(&self) {
        if let Err(error) = native::input_click() {
            eprintln!("fesTerm: {error}");
        }
    }

    pub fn play_input_haptic(&self) {
        if let Err(error) = native::input_haptic() {
            eprintln!("fesTerm: {error}");
        }
    }

    pub fn set_field_document(&self, document: Option<FieldDocument>) {
        let epoch = native::input_epoch();
        if *self.field.borrow() == document && self.epoch.get() == epoch {
            return;
        }
        if let Some(document) = &document {
            if let Err(error) = native::field_document(
                document.field,
                &document.text,
                document.selection.start,
                document.selection.end,
                document.secure,
                document.multiline,
            ) {
                *self.error.borrow_mut() = Some(error);
                return;
            }
        }
        *self.field.borrow_mut() = document;
        self.epoch.set(epoch);
    }

    pub fn take_field_input(&self) -> Vec<FieldInput> {
        match native::take_input() {
            Err(error) => vec![FieldInput::Unavailable(error)],
            Ok(inputs) => inputs
                .into_iter()
                .filter_map(|mut input| {
                    if input.target != self.field.borrow().as_ref().map_or(0, |field| field.field) {
                        return None;
                    }
                    let text = std::mem::take(&mut input.text);
                    match input.kind {
                        1 => Some(FieldInput::Insert(text)),
                        2 => Some(FieldInput::DeleteBackward),
                        3 => Some(FieldInput::Select(input.start..input.end)),
                        4 => Some(FieldInput::Preedit {
                            text,
                            selected: input.start..input.end,
                        }),
                        5 => Some(FieldInput::Commit(text)),
                        6 => match input.start {
                            19 => Some(FieldKey::Up),
                            20 => Some(FieldKey::Down),
                            21 => Some(FieldKey::Left),
                            22 => Some(FieldKey::Right),
                            61 => Some(FieldKey::Tab),
                            111 => Some(FieldKey::Escape),
                            112 => Some(FieldKey::Delete),
                            122 => Some(FieldKey::Home),
                            123 => Some(FieldKey::End),
                            _ => None,
                        }
                        .map(FieldInput::Key),
                        _ => Some(FieldInput::Unavailable("Unsupported Android input event")),
                    }
                })
                .collect(),
        }
    }

    pub fn document_picker(&self) -> Option<DocumentPicker> {
        Some(DocumentPicker::default())
    }
}

pub fn clipboard_text() -> Option<String> {
    match native::clipboard_text() {
        Ok(text) => text,
        Err(error) => {
            eprintln!("fesTerm: {error}");
            None
        }
    }
}

pub fn set_clipboard_text(text: &str) -> bool {
    native::set_clipboard_text(text).is_ok()
}
pub fn open_url(url: &str) -> bool {
    native::open_url(url).is_ok()
}

/// Android reports whether startActivity accepted the browser invocation.
pub fn open_url_with_completion(url: &str, completion: impl Fn(bool) + Send + Sync + 'static) {
    completion(open_url(url));
}
