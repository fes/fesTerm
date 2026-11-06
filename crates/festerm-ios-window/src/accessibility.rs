//! Native accessibility adapters for the egui AccessKit tree (ADR 0075).
//!
//! The adapter is not `Send`: create, update and drop it on the thread that
//! owns the renderer window (UIKit's main thread; Android's native loop).

pub use accesskit::{
    ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, TreeUpdate,
};

pub struct AccessibilityAdapter {
    #[cfg(target_os = "ios")]
    inner: accesskit_ios::SubclassingAdapter,
    #[cfg(target_os = "android")]
    inner: festerm_android_platform::AccessibilityAdapter,
}

impl AccessibilityAdapter {
    /// Exposes the renderer window to the platform accessibility API. Returns
    /// `None` where no adapter exists (desktop harness builds).
    #[cfg(target_os = "ios")]
    pub fn new(
        window: raw_window_handle::WindowHandle<'_>,
        activation: impl ActivationHandler + Send + 'static,
        action: impl ActionHandler + Send + 'static,
        deactivation: impl DeactivationHandler + Send + 'static,
    ) -> Option<Self> {
        objc2::MainThreadMarker::new()?;
        let raw_window_handle::RawWindowHandle::UiKit(handle) = window.as_raw() else {
            return None;
        };
        // SAFETY: WindowHandle guarantees a valid UIView for this borrow; the
        // adapter retains it, and this is the main thread.
        let inner = unsafe {
            accesskit_ios::SubclassingAdapter::new(
                handle.ui_view.as_ptr(),
                activation,
                action,
                deactivation,
            )
        };
        Some(Self { inner })
    }

    #[cfg(target_os = "android")]
    pub fn new(
        _window: raw_window_handle::WindowHandle<'_>,
        activation: impl ActivationHandler + Send + 'static,
        action: impl ActionHandler + Send + 'static,
        // The Android adapter has no deactivation signal.
        _deactivation: impl DeactivationHandler + Send + 'static,
    ) -> Option<Self> {
        match festerm_android_platform::AccessibilityAdapter::new(activation, action) {
            Ok(inner) => Some(Self { inner }),
            Err(error) => {
                eprintln!("fesTerm: {error}");
                None
            }
        }
    }

    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    pub fn new(
        _window: raw_window_handle::WindowHandle<'_>,
        _activation: impl ActivationHandler + Send + 'static,
        _action: impl ActionHandler + Send + 'static,
        _deactivation: impl DeactivationHandler + Send + 'static,
    ) -> Option<Self> {
        None
    }

    /// Applies `update` only after a platform client requested the tree.
    pub fn update_if_active(&mut self, update: impl FnOnce() -> TreeUpdate) {
        #[cfg(target_os = "ios")]
        if let Some(events) = self.inner.update_if_active(update) {
            events.raise();
        }
        #[cfg(target_os = "android")]
        self.inner.update_if_active(update);
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        drop(update);
    }
}
