//! AppKit integration kept outside the cross-platform application crate.

#[cfg(target_os = "macos")]
use std::ptr::NonNull;

/// Moves macOS's standard traffic lights down into fesTerm's integrated
/// chrome band. The view pointer originates from winit's AppKit window handle
/// and is used only during native-window creation on the main thread.
#[cfg(target_os = "macos")]
pub fn offset_traffic_lights(ns_view: NonNull<std::ffi::c_void>, points: f64) {
    use objc2_app_kit::{NSView, NSWindowButton};
    use objc2_foundation::NSPoint;

    // SAFETY: winit supplies a live NSView pointer for the root window handle;
    // this function runs while that window is being created on the main thread.
    let ns_view = unsafe { ns_view.cast::<NSView>().as_ref() };
    let Some(ns_window) = ns_view.window() else {
        return;
    };

    for button_kind in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        let Some(button) = ns_window.standardWindowButton(button_kind) else {
            continue;
        };
        let frame = button.frame();
        button.setFrameOrigin(NSPoint::new(frame.origin.x, frame.origin.y - points));
    }
}

#[cfg(not(target_os = "macos"))]
pub fn offset_traffic_lights(_: (), _: f64) {}
