//! UIKit keyboard geometry, isolated from terminal semantics and rendering.

#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "ios")]
pub use ios::KeyboardBridge;

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
