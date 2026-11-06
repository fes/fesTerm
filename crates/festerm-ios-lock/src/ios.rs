use crate::{
    applock::AppLock,
    locker::{
        AuthFailure, AuthMethod, AuthOutcome, Authentication, Availability, BiometryKind, Locker,
    },
    policy::LifecycleEvent,
};
use block2::RcBlock;
use objc2::{
    rc::Retained,
    runtime::{Bool, ProtocolObject},
};
use objc2_core_foundation::CFRetained;
use objc2_foundation::{
    MainThreadMarker, NSError, NSNotification, NSNotificationCenter, NSObjectProtocol, NSString,
};
use objc2_local_authentication::{
    LAAccessControlOperation, LABiometryType, LAContext, LAError, LAPolicy,
};
use objc2_security::{
    kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly, SecAccessControl, SecAccessControlCreateFlags,
};
use objc2_ui_kit::{
    UIApplicationDidBecomeActiveNotification, UIApplicationDidEnterBackgroundNotification,
    UIApplicationProtectedDataWillBecomeUnavailable, UIApplicationWillResignActiveNotification,
    UIColor, UIView, UIViewAutoresizing,
};
use raw_window_handle::{RawWindowHandle, WindowHandle};
use std::{
    cell::RefCell,
    ptr::NonNull,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

const POLICY: LAPolicy = LAPolicy::DeviceOwnerAuthentication;

/// Fresh cancellable system authentication; no passcode enters the application.
#[derive(Clone, Copy, Debug, Default)]
pub struct LaContextLocker;

struct NativeAuthentication {
    context: Retained<LAContext>,
    access: Option<CFRetained<SecAccessControl>>,
    completed: Arc<AtomicBool>,
}

// SAFETY: LAContext supports cross-thread evaluation/invalidation. The immutable
// access-control object is only retained here, never mutated or stored.
unsafe impl Send for NativeAuthentication {}

impl NativeAuthentication {
    fn cancel(self) {
        if !self.completed.load(Ordering::SeqCst) {
            // SAFETY: this is the request's retained context; invalidation is idempotent.
            unsafe { self.context.invalidate() };
        }
    }
}

fn map_error(code: isize) -> AuthFailure {
    match LAError(code) {
        LAError::UserCancel
        | LAError::SystemCancel
        | LAError::AppCancel
        | LAError::UserFallback => AuthFailure::Cancelled,
        LAError::AuthenticationFailed => AuthFailure::NotRecognized,
        LAError::BiometryLockout => AuthFailure::BiometryLockedOut,
        LAError::PasscodeNotSet => AuthFailure::PasscodeNotSet,
        _ => AuthFailure::Unavailable,
    }
}

impl Locker for LaContextLocker {
    fn availability(&self) -> Availability {
        // SAFETY: a fresh, thread-safe LAContext used only for this preflight.
        unsafe {
            let context = LAContext::new();
            // This preflight populates biometryType even when biometrics cannot
            // run. Owner authentication below remains the availability authority.
            let _ =
                context.canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthenticationWithBiometrics);
            match context.canEvaluatePolicy_error(POLICY) {
                Ok(()) => Availability::Available(match context.biometryType() {
                    LABiometryType::FaceID => BiometryKind::FaceId,
                    LABiometryType::TouchID => BiometryKind::TouchId,
                    LABiometryType::OpticID => BiometryKind::OpticId,
                    _ => BiometryKind::None,
                }),
                Err(error) if LAError(error.code()) == LAError::PasscodeNotSet => {
                    Availability::PasscodeNotSet
                }
                Err(_) => Availability::Unavailable,
            }
        }
    }

    fn authenticate(
        &self,
        method: AuthMethod,
        reason: &str,
        done: Box<dyn FnOnce(AuthOutcome) + Send>,
    ) -> Authentication {
        // SAFETY: fresh contexts and immutable access controls stay alive in the
        // cancellation handle. The reply contains only Send state, not the context.
        unsafe {
            let context = LAContext::new();
            if let Err(error) = context.canEvaluatePolicy_error(POLICY) {
                done(Err(map_error(error.code())));
                return Authentication::new(|| {});
            }
            let access = match method {
                AuthMethod::OwnerAuthentication => None,
                AuthMethod::DevicePasscode => {
                    let Some(access) = SecAccessControl::with_flags(
                        None,
                        kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly,
                        SecAccessControlCreateFlags::DevicePasscode,
                        std::ptr::null_mut(),
                    ) else {
                        done(Err(AuthFailure::Unavailable));
                        return Authentication::new(|| {});
                    };
                    Some(access)
                }
            };
            let completed = Arc::new(AtomicBool::new(false));
            let completion = completed.clone();
            let done = Mutex::new(Some(done));
            let reply = RcBlock::new(move |success: Bool, error: *mut NSError| {
                completion.store(true, Ordering::SeqCst);
                let outcome = if success.as_bool() {
                    Ok(())
                } else {
                    // SAFETY: LocalAuthentication passes a valid NSError or null.
                    Err(NonNull::new(error)
                        .map_or(AuthFailure::Unavailable, |e| map_error(e.as_ref().code())))
                };
                let done = done.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(done) = done {
                    done(outcome);
                }
            });
            let request = NativeAuthentication {
                context,
                access,
                completed,
            };
            if let Some(access) = &request.access {
                request
                    .context
                    .evaluateAccessControl_operation_localizedReason_reply(
                        access,
                        LAAccessControlOperation::UseItem,
                        &NSString::from_str(reason),
                        &reply,
                    );
            } else {
                request.context.evaluatePolicy_localizedReason_reply(
                    POLICY,
                    &NSString::from_str(reason),
                    &reply,
                );
            }
            Authentication::new(move || request.cancel())
        }
    }
}

struct Shield {
    view: Retained<UIView>,
    overlay: RefCell<Option<Retained<UIView>>>,
}

impl Shield {
    fn set_covered(&self, covered: bool) {
        let mut overlay = self.overlay.borrow_mut();
        match (covered, overlay.is_some()) {
            (true, false) => {
                let Some(mtm) = MainThreadMarker::new() else {
                    return;
                };
                let host = self
                    .view
                    .window()
                    .map_or_else(|| self.view.clone(), Retained::into_super);
                let cover = UIView::initWithFrame(mtm.alloc(), host.bounds());
                cover.setBackgroundColor(Some(&UIColor::blackColor()));
                cover.setAutoresizingMask(
                    UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight,
                );
                host.addSubview(&cover);
                *overlay = Some(cover);
            }
            (false, true) => {
                if let Some(cover) = overlay.take() {
                    cover.removeFromSuperview();
                }
            }
            _ => {}
        }
    }
}

/// Main-thread-only: feeds UIKit lifecycle into the [`AppLock`] and keeps an
/// opaque native view over the window whenever the lock says so. The overlay
/// is installed synchronously in `willResignActive`, before iOS takes the
/// app-switcher snapshot, and stays up after resume until enough locked (or
/// unlocked) frames have been presented. Call [`LockBridge::sync`] each frame.
pub struct LockBridge {
    lock: AppLock,
    shield: Rc<Shield>,
    center: Retained<NSNotificationCenter>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}

impl LockBridge {
    pub fn new(window: WindowHandle<'_>, lock: AppLock) -> Option<Self> {
        let _main_thread = MainThreadMarker::new()?;
        let RawWindowHandle::UiKit(handle) = window.as_raw() else {
            return None;
        };
        // SAFETY: WindowHandle guarantees a valid UIKit UIView pointer for this
        // call; it is retained before the borrow ends and only used on the
        // main thread.
        let view = unsafe { Retained::retain(handle.ui_view.as_ptr().cast::<UIView>()) }?;
        let shield = Rc::new(Shield {
            view,
            overlay: RefCell::new(None),
        });
        let center = NSNotificationCenter::defaultCenter();
        let mut observers = Vec::with_capacity(4);
        // SAFETY: these UIKit notification names are immutable system globals.
        let events = unsafe {
            [
                (
                    UIApplicationWillResignActiveNotification,
                    LifecycleEvent::WillResignActive,
                ),
                (
                    UIApplicationDidBecomeActiveNotification,
                    LifecycleEvent::DidBecomeActive,
                ),
                (
                    UIApplicationDidEnterBackgroundNotification,
                    LifecycleEvent::DidEnterBackground,
                ),
                (
                    UIApplicationProtectedDataWillBecomeUnavailable,
                    LifecycleEvent::DeviceLocked,
                ),
            ]
        };
        for (name, event) in events {
            let (lock, shield) = (lock.clone(), shield.clone());
            let callback = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                lock.handle_lifecycle(event);
                shield.set_covered(lock.shield_should_cover());
            });
            // SAFETY: nil queue delivers synchronously on the posting thread,
            // which UIKit makes the main thread; the Rc is only touched there
            // (set_covered re-checks). The block never reads notification data.
            observers.push(unsafe {
                center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &callback)
            });
        }
        shield.set_covered(lock.shield_should_cover());
        Some(Self {
            lock,
            shield,
            center,
            observers,
        })
    }

    /// Reconcile the overlay with the lock; call once per frame.
    pub fn sync(&self) {
        self.shield.set_covered(self.lock.shield_should_cover());
    }
}

impl Drop for LockBridge {
    fn drop(&mut self) {
        for observer in &self.observers {
            // SAFETY: each token came from this center and is still retained.
            unsafe { self.center.removeObserver((**observer).as_ref()) };
        }
        self.shield.set_covered(false);
    }
}
