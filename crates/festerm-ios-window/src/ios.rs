use block2::RcBlock;
use objc2::{
    define_class, msg_send,
    rc::{Retained, Weak},
    runtime::ProtocolObject,
    sel, ClassType, DeclaredClass, MainThreadOnly, Message,
};
use objc2_foundation::{
    MainThreadMarker, NSDictionary, NSNotification, NSNotificationCenter, NSObject,
    NSObjectProtocol, NSString, NSURL,
};
use objc2_ui_kit::{
    UIApplication, UIDevice, UIInputViewAudioFeedback, UIKeyInput,
    UIKeyboardDidChangeFrameNotification, UIKeyboardWillChangeFrameNotification, UIPasteboard,
    UIScene, UISceneConnectionOptions, UISceneDelegate, UISceneSession,
    UITextAutocapitalizationType, UITextAutocorrectionType, UITextInputTraits,
    UITextSmartDashesType, UITextSmartQuotesType, UITextSpellCheckingType, UIView, UIWindow,
    UIWindowScene, UIWindowSceneDelegate,
};
use raw_window_handle::{RawWindowHandle, WindowHandle};
use std::{cell::Cell, ptr::NonNull, sync::Arc};

use crate::KeyboardMode;

/// Main-thread-only: owns a retain on the host view and removes observers on
/// drop. Notification callbacks capture only a Send + Sync repaint closure.
pub struct KeyboardBridge {
    view: Retained<UIView>,
    center: Retained<NSNotificationCenter>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    terminal_responder: Retained<TerminalResponder>,
}

struct TerminalResponderIvars {
    target: Weak<UIView>,
    empty_input: Retained<TerminalInputView>,
}

define_class!(
    #[unsafe(super(UIView))]
    #[thread_kind = MainThreadOnly]
    #[name = "FesTermTerminalInputView"]
    struct TerminalInputView;

    unsafe impl NSObjectProtocol for TerminalInputView {}

    unsafe impl UIInputViewAudioFeedback for TerminalInputView {
        #[unsafe(method(enableInputClicksWhenVisible))]
        fn enable_input_clicks_when_visible(&self) -> bool {
            true
        }
    }
);

define_class!(
    /// Keep UIKit's hardware text delivery without showing a second software
    /// keyboard. Forward to winit's existing UIKeyInput rather than storing text.
    #[unsafe(super(UIView))]
    #[thread_kind = MainThreadOnly]
    #[name = "FesTermTerminalKeyboardResponder"]
    #[ivars = TerminalResponderIvars]
    struct TerminalResponder;

    unsafe impl NSObjectProtocol for TerminalResponder {}

    impl TerminalResponder {
        #[unsafe(method(canBecomeFirstResponder))]
        fn can_become_first_responder(&self) -> bool { true }

        #[unsafe(method(inputView))]
        fn input_view(&self) -> &UIView {
            &self.ivars().empty_input
        }
    }

    unsafe impl UITextInputTraits for TerminalResponder {
        #[unsafe(method(autocapitalizationType))]
        fn autocapitalization_type(&self) -> UITextAutocapitalizationType {
            UITextAutocapitalizationType::None
        }
        #[unsafe(method(autocorrectionType))]
        fn autocorrection_type(&self) -> UITextAutocorrectionType {
            UITextAutocorrectionType::No
        }
        #[unsafe(method(spellCheckingType))]
        fn spell_checking_type(&self) -> UITextSpellCheckingType {
            UITextSpellCheckingType::No
        }
        #[unsafe(method(smartQuotesType))]
        fn smart_quotes_type(&self) -> UITextSmartQuotesType {
            UITextSmartQuotesType::No
        }
        #[unsafe(method(smartDashesType))]
        fn smart_dashes_type(&self) -> UITextSmartDashesType {
            UITextSmartDashesType::No
        }
    }

    unsafe impl UIKeyInput for TerminalResponder {
        #[unsafe(method(hasText))]
        fn has_text(&self) -> bool { true }

        #[unsafe(method(insertText:))]
        fn insert_text(&self, text: &NSString) {
            if let Some(target) = self.ivars().target.load() {
                // SAFETY: selectors are checked when the bridge is constructed;
                // the retained winit view is accessed only on the main thread.
                let _: () = unsafe { msg_send![&*target, insertText: text] };
            }
        }

        #[unsafe(method(deleteBackward))]
        fn delete_backward(&self) {
            if let Some(target) = self.ivars().target.load() {
                // SAFETY: same checked winit UIKeyInput receiver as insertText:.
                let _: () = unsafe { msg_send![&*target, deleteBackward] };
            }
        }
    }
);

impl KeyboardBridge {
    pub fn new(
        window: WindowHandle<'_>,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Option<Self> {
        let main_thread = MainThreadMarker::new()?;
        let RawWindowHandle::UiKit(handle) = window.as_raw() else {
            return None;
        };
        // SAFETY: WindowHandle guarantees a valid UIKit UIView pointer for
        // this call; retain it before the handle borrow ends. UIKit access
        // is confined to this main-thread-only type (UIView is !Send/!Sync).
        let view = unsafe { Retained::retain(handle.ui_view.as_ptr().cast::<UIView>()) }?;
        if !view.respondsToSelector(sel!(insertText:))
            || !view.respondsToSelector(sel!(deleteBackward))
        {
            eprintln!("festerm-mobile: host view has no keyboard input receiver");
            return None;
        }
        let input = TerminalInputView::alloc(main_thread).set_ivars(());
        // SAFETY: UIView initializer on a newly allocated main-thread object.
        let empty_input: Retained<TerminalInputView> = unsafe { msg_send![super(input), init] };
        let responder = TerminalResponder::alloc(main_thread).set_ivars(TerminalResponderIvars {
            target: Weak::from_retained(&view),
            empty_input,
        });
        // SAFETY: UIView initializer on a newly allocated main-thread object.
        let terminal_responder: Retained<TerminalResponder> =
            unsafe { msg_send![super(responder), init] };
        view.addSubview(&terminal_responder);
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
            terminal_responder,
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

    pub fn set_keyboard_mode(&self, mode: KeyboardMode) -> Result<(), &'static str> {
        match mode {
            KeyboardMode::Terminal => {
                if self.view.isFirstResponder() && !self.view.resignFirstResponder() {
                    return Err("The built-in keyboard could not be dismissed");
                }
                if !self.terminal_responder.isFirstResponder() {
                    if !self.terminal_responder.becomeFirstResponder() {
                        return Err(
                            "Hardware keyboard input is unavailable; switch to the built-in keyboard",
                        );
                    }
                    self.terminal_responder.reloadInputViews();
                }
            }
            KeyboardMode::System | KeyboardMode::Hidden => {
                if self.terminal_responder.isFirstResponder()
                    && !self.terminal_responder.resignFirstResponder()
                {
                    return Err("The terminal keyboard could not release input");
                }
                if mode == KeyboardMode::System {
                    if !self.view.isFirstResponder() && !self.view.becomeFirstResponder() {
                        return Err("The built-in keyboard is unavailable");
                    }
                } else if self.view.isFirstResponder() && !self.view.resignFirstResponder() {
                    return Err("The built-in keyboard could not be dismissed");
                }
            }
        }
        Ok(())
    }

    pub fn play_input_click(&self) {
        if self.terminal_responder.isFirstResponder() {
            UIDevice::currentDevice(MainThreadMarker::from(&*self.terminal_responder))
                .playInputClick();
        }
    }
}

impl Drop for KeyboardBridge {
    fn drop(&mut self) {
        self.terminal_responder.resignFirstResponder();
        self.terminal_responder.removeFromSuperview();
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
                attach_pending(mtm);
            }
        }
    }

    unsafe impl UIWindowSceneDelegate for SceneDelegate {}
);

thread_local! {
    static CONNECTED_SCENE: Cell<Option<Retained<UIWindowScene>>> = const { Cell::new(None) };
    static APP_WINDOW: Cell<Option<Retained<UIWindow>>> = const { Cell::new(None) };
}

/// Hands winit's window (the one hosting `window`'s view) to the scene
/// delegate. iOS 27 omits scene-less windows from `UIApplication.windows`, so
/// the window is taken from the view rather than searched for. Main thread
/// only; attaches immediately if the scene already connected.
pub fn attach_window(window: WindowHandle<'_>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let RawWindowHandle::UiKit(handle) = window.as_raw() else {
        return;
    };
    // SAFETY: WindowHandle guarantees a valid UIKit UIView pointer for this
    // call; it is retained before the borrow ends, on the main thread.
    let Some(view) = (unsafe { Retained::retain(handle.ui_view.as_ptr().cast::<UIView>()) }) else {
        return;
    };
    let Some(window) = view.window() else {
        eprintln!("festerm-mobile: renderer view has no window");
        return;
    };
    APP_WINDOW.set(Some(window));
    attach_pending(mtm);
}

fn attach_pending(_mtm: MainThreadMarker) {
    let scene = CONNECTED_SCENE.take();
    let window = APP_WINDOW.take();
    if let (Some(scene), Some(window)) = (&scene, &window) {
        if window.windowScene().is_none() {
            window.setWindowScene(Some(scene));
            window.makeKeyAndVisible();
            eprintln!("festerm-mobile: attached the app window to the scene");
        }
    }
    CONNECTED_SCENE.set(scene);
    APP_WINDOW.set(window);
}

/// Registers the scene delegate class with the Objective-C runtime. Call
/// before the event loop starts `UIApplicationMain`; main thread only.
pub fn register_scene_delegate() -> bool {
    let _ = SceneDelegate::class();
    true
}
