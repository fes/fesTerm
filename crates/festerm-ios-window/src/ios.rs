use block2::RcBlock;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::{MainThreadMarker, NSNotification, NSNotificationCenter, NSObjectProtocol};
use objc2_ui_kit::{
    UIKeyboardDidChangeFrameNotification, UIKeyboardWillChangeFrameNotification, UIView,
};
use raw_window_handle::{RawWindowHandle, WindowHandle};
use std::{ptr::NonNull, sync::Arc};

/// Main-thread-only: owns a retain on the host view and removes observers on
/// drop. Notification callbacks capture only a Send + Sync repaint closure.
pub struct KeyboardBridge {
    view: Retained<UIView>,
    center: Retained<NSNotificationCenter>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}

impl KeyboardBridge {
    pub fn new(
        window: WindowHandle<'_>,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Option<Self> {
        let _main_thread = MainThreadMarker::new()?;
        let RawWindowHandle::UiKit(handle) = window.as_raw() else {
            return None;
        };
        // SAFETY: WindowHandle guarantees a valid UIKit UIView pointer for
        // this call; retain it before the handle borrow ends. UIKit access
        // is confined to this main-thread-only type (UIView is !Send/!Sync).
        let view = unsafe { Retained::retain(handle.ui_view.as_ptr().cast::<UIView>()) }?;
        // Create the guide before the first keyboard animation/layout pass.
        let _ = view.keyboardLayoutGuide();
        let center = NSNotificationCenter::defaultCenter();
        let repaint = Arc::new(repaint);
        let mut observers = Vec::with_capacity(2);
        // SAFETY: these UIKit notification names are immutable system globals.
        for name in unsafe {
            [
                UIKeyboardWillChangeFrameNotification,
                UIKeyboardDidChangeFrameNotification,
            ]
        } {
            let repaint = repaint.clone();
            let callback = RcBlock::new(move |_notification: NonNull<NSNotification>| repaint());
            // SAFETY: no object filter/queue, and the callback is Send + Sync,
            // captures no UIKit values, and never accesses notification data.
            observers.push(unsafe {
                center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &callback)
            });
        }
        Some(Self {
            view,
            center,
            observers,
        })
    }

    /// Docked keyboard occlusion in view-relative units. Hidden/hardware or
    /// floating keyboards do not reserve a fabricated fixed-height region.
    pub fn occluded_height_fraction(&self) -> f32 {
        let bounds = self.view.bounds();
        let frame = self.view.keyboardLayoutGuide().layoutFrame();
        let safe_bottom = self.view.safeAreaInsets().bottom;
        if bounds.size.height <= 0.0 || frame.size.height <= safe_bottom + 1.0 {
            return 0.0;
        }
        ((bounds.origin.y + bounds.size.height - frame.origin.y) / bounds.size.height)
            .clamp(0.0, 1.0) as f32
    }
}

impl Drop for KeyboardBridge {
    fn drop(&mut self) {
        for observer in &self.observers {
            // SAFETY: each token was returned by this center and is still
            // retained; removal prevents callbacks after the bridge is gone.
            unsafe { self.center.removeObserver((**observer).as_ref()) };
        }
    }
}
