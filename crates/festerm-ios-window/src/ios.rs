use block2::RcBlock;
use objc2::{
    define_class, rc::Retained, runtime::ProtocolObject, ClassType, MainThreadOnly, Message,
};
use objc2_foundation::{
    MainThreadMarker, NSDictionary, NSNotification, NSNotificationCenter, NSObject,
    NSObjectProtocol, NSString, NSURL,
};
use objc2_ui_kit::{
    UIApplication, UIKeyboardDidChangeFrameNotification, UIKeyboardWillChangeFrameNotification,
    UIPasteboard, UIScene, UISceneConnectionOptions, UISceneDelegate, UISceneSession, UIView,
    UIWindowScene, UIWindowSceneDelegate,
};
use raw_window_handle::{RawWindowHandle, WindowHandle};
use std::{cell::Cell, ptr::NonNull, sync::Arc};

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

/// The general pasteboard's text. Main thread only; `None` elsewhere or when
/// the pasteboard holds no string.
pub fn clipboard_text() -> Option<String> {
    let _main_thread = MainThreadMarker::new()?;
    let pasteboard = UIPasteboard::generalPasteboard();
    // SAFETY: plain property read on a valid pasteboard.
    unsafe { pasteboard.string() }.map(|text| text.to_string())
}

/// Replaces the general pasteboard's text. Main thread only.
pub fn set_clipboard_text(text: &str) -> bool {
    let Some(_main_thread) = MainThreadMarker::new() else {
        return false;
    };
    let pasteboard = UIPasteboard::generalPasteboard();
    let text = NSString::from_str(text);
    // SAFETY: plain property write with a valid NSString.
    unsafe { pasteboard.setString(Some(&text)) };
    true
}

/// Opens an `https://` URL in the system browser. Main thread only; other
/// schemes are refused so a hostile string cannot launch another app.
pub fn open_url(url: &str) -> bool {
    let Some(main_thread) = MainThreadMarker::new() else {
        return false;
    };
    if !url.starts_with("https://") {
        return false;
    }
    let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) else {
        return false;
    };
    let application = UIApplication::sharedApplication(main_thread);
    // SAFETY: valid URL and an empty options dictionary; no completion block.
    unsafe {
        application.openURL_options_completionHandler(&url, &NSDictionary::new(), None);
    }
    true
}

/// Class name referenced by `UISceneDelegateClassName` in the iOS Info.plist.
pub const SCENE_DELEGATE_CLASS: &str = "FesTermSceneDelegate";

define_class!(
    /// Minimal UIScene adoption (required from iOS 27). winit 0.30 creates its
    /// window in `didFinishLaunching`, before the scene connects, so the
    /// delegate attaches any scene-less window when the scene arrives.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FesTermSceneDelegate"]
    struct SceneDelegate;

    unsafe impl NSObjectProtocol for SceneDelegate {}

    unsafe impl UISceneDelegate for SceneDelegate {
        #[unsafe(method(scene:willConnectToSession:options:))]
        fn scene_will_connect(
            &self,
            scene: &UIScene,
            _session: &UISceneSession,
            _options: &UISceneConnectionOptions,
        ) {
            let mtm = MainThreadMarker::from(self);
            if let Some(window_scene) = scene.downcast_ref::<UIWindowScene>() {
                CONNECTED_SCENE.set(Some(window_scene.retain()));
                attach_windows(mtm, window_scene);
            }
        }
    }

    unsafe impl UIWindowSceneDelegate for SceneDelegate {}
);

thread_local! {
    static CONNECTED_SCENE: Cell<Option<Retained<UIWindowScene>>> = const { Cell::new(None) };
}

/// Attaches windows created after the scene connected (winit's window usually
/// is) to it. Idempotent; main thread only.
pub fn attach_scene_windows() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if let Some(scene) = CONNECTED_SCENE.take() {
        attach_windows(mtm, &scene);
        CONNECTED_SCENE.set(Some(scene));
    }
}

#[allow(deprecated)] // `UIApplication.windows` is the only list of scene-less windows.
fn attach_windows(mtm: MainThreadMarker, scene: &UIWindowScene) {
    let windows = UIApplication::sharedApplication(mtm).windows();
    eprintln!(
        "festerm-mobile: scene connected, {} window(s) present",
        windows.count()
    );
    for index in 0..windows.count() {
        let window = windows.objectAtIndex(index);
        if window.windowScene().is_none() {
            window.setWindowScene(Some(scene));
            eprintln!("festerm-mobile: attached a scene-less window to the scene");
        }
    }
}

/// Registers the scene delegate class with the Objective-C runtime. Call
/// before the event loop starts `UIApplicationMain`; main thread only.
pub fn register_scene_delegate() -> bool {
    let _ = SceneDelegate::class();
    true
}
