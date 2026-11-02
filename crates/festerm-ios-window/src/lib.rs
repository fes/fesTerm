//! UIKit keyboard geometry, isolated from terminal semantics and rendering.

#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "ios")]
pub use ios::{
    attach_scene_windows, clipboard_text, open_url, register_scene_delegate, set_clipboard_text,
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
}

/// No UIScene exists off iOS.
#[cfg(not(target_os = "ios"))]
pub fn attach_scene_windows() {}

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
