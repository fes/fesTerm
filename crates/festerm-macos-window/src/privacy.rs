//! Beta macOS automation privacy monitor (ADR 0065).
//!
//! Loss is signalled once, and permanently, when any of these is observed:
//!
//! - a `CGSessionCopyCurrentDictionary` poll, every [`POLL_INTERVAL`] on a
//!   dedicated thread, reporting a locked screen, a session no longer on the
//!   console, or no session dictionary at all;
//! - the `com.apple.screenIsLocked` distributed notification;
//! - `NSWorkspaceWillSleepNotification` or
//!   `NSWorkspaceSessionDidResignActiveNotification` (fast user switching).
//!
//! The poll is the guaranteed source. The notifications are delivered on the
//! main thread's run loop and only shorten detection while it runs. None of
//! these sources has a documented delivery bound: this is an observed-event
//! cut, not an atomic physical cutoff. Installation fails closed when the
//! session is already lost or the monitor thread cannot start.

use std::ffi::c_void;
use std::io;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSNotification, NSObjectProtocol};

/// Upper bound between polls when no notification is delivered.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFNotificationCenterRef = *const c_void;
type CFNotificationCallback = extern "C" fn(
    CFNotificationCenterRef,
    *mut c_void,
    CFStringRef,
    *const c_void,
    CFDictionaryRef,
);

const UTF8: u32 = 0x0800_0100;
const DELIVER_IMMEDIATELY: isize = 4;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithBytes(
        allocator: *const c_void,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external: u8,
    ) -> CFStringRef;
    fn CFDictionaryGetValue(dictionary: CFDictionaryRef, key: *const c_void) -> *const c_void;
    fn CFGetTypeID(value: CFTypeRef) -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(value: CFTypeRef) -> u8;
    fn CFRelease(value: CFTypeRef);
    fn CFNotificationCenterGetDistributedCenter() -> CFNotificationCenterRef;
    fn CFNotificationCenterAddObserver(
        center: CFNotificationCenterRef,
        observer: *const c_void,
        callback: CFNotificationCallback,
        name: CFStringRef,
        object: *const c_void,
        behavior: isize,
    );
    fn CFNotificationCenterRemoveEveryObserver(
        center: CFNotificationCenterRef,
        observer: *const c_void,
    );
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
}

/// Owned, immutable CFString released on drop.
struct CfString(CFStringRef);
// SAFETY: immutable CFStrings are thread-safe and released exactly once.
unsafe impl Send for CfString {}

impl CfString {
    fn new(value: &str) -> io::Result<Self> {
        // SAFETY: `value` is valid UTF-8 for its length; a null allocator
        // selects the default allocator.
        let string = unsafe {
            CFStringCreateWithBytes(ptr::null(), value.as_ptr(), value.len() as isize, UTF8, 0)
        };
        if string.is_null() {
            Err(io::Error::other("CFString allocation failed"))
        } else {
            Ok(Self(string))
        }
    }
}

impl Drop for CfString {
    fn drop(&mut self) {
        // SAFETY: the string was created by this wrapper and is released once.
        unsafe { CFRelease(self.0) }
    }
}

/// Session state reported by CoreGraphics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionState {
    Private,
    Lost,
}

/// Classifies the two relevant session-dictionary booleans. A missing lock
/// key means unlocked; a missing console key is treated as off-console.
pub(crate) const fn classify(
    screen_locked: Option<bool>,
    on_console: Option<bool>,
) -> SessionState {
    match (screen_locked, on_console) {
        (Some(false) | None, Some(true)) => SessionState::Private,
        _ => SessionState::Lost,
    }
}

struct Keys {
    locked: CfString,
    on_console: CfString,
}

impl Keys {
    fn new() -> io::Result<Self> {
        Ok(Self {
            locked: CfString::new("CGSSessionScreenIsLocked")?,
            on_console: CfString::new("kCGSSessionOnConsoleKey")?,
        })
    }

    fn session_state(&self) -> SessionState {
        // SAFETY: CGSessionCopyCurrentDictionary follows the Copy rule; the
        // dictionary is released below and values are read only before that.
        unsafe {
            let dictionary = CGSessionCopyCurrentDictionary();
            if dictionary.is_null() {
                return SessionState::Lost;
            }
            let read = |key: &CfString| {
                let value = CFDictionaryGetValue(dictionary, key.0);
                (!value.is_null() && CFGetTypeID(value) == CFBooleanGetTypeID())
                    .then(|| CFBooleanGetValue(value) != 0)
            };
            let state = classify(read(&self.locked), read(&self.on_console));
            CFRelease(dictionary);
            state
        }
    }
}

/// Observer context. It is leaked once per monitor because main-thread
/// notification delivery can race with removal; `Drop` clears the callback so
/// the leaked remainder holds no application state.
struct Shared {
    lost: AtomicBool,
    callback: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Shared {
    fn signal(&self) {
        if !self.lost.swap(true, Ordering::AcqRel) {
            let callback = self
                .callback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(callback) = callback {
                callback();
            }
        }
    }
}

extern "C" fn screen_locked(
    _center: CFNotificationCenterRef,
    observer: *mut c_void,
    _name: CFStringRef,
    _object: *const c_void,
    _user_info: CFDictionaryRef,
) {
    // SAFETY: `observer` is a leaked `Shared` that is never freed.
    let shared = unsafe { &*(observer as *const Shared) };
    shared.signal();
}

struct Stop {
    stopped: Mutex<bool>,
    wake: Condvar,
}

/// Block-observer tokens; only ever passed back to `removeObserver:`.
struct Tokens(Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>);
// SAFETY: NSNotificationCenter registration and removal are thread-safe, and
// the opaque tokens are used for nothing else.
unsafe impl Send for Tokens {}
unsafe impl Sync for Tokens {}

/// Signals automation privacy loss once; dropping it stops monitoring.
pub struct PrivacyMonitor {
    shared: &'static Shared,
    stop: Arc<Stop>,
    thread: Option<JoinHandle<()>>,
    workspace: Tokens,
    distributed: bool,
}

impl PrivacyMonitor {
    /// Starts monitoring. `callback` runs at most once, on loss or monitor
    /// failure, from the monitor or main thread. Returns an error (after
    /// invoking `callback`) if monitoring cannot start or privacy is already
    /// lost.
    pub fn install(callback: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        let shared: &'static Shared = Box::leak(Box::new(Shared {
            lost: AtomicBool::new(false),
            callback: Mutex::new(Some(callback)),
        }));
        let mut monitor = Self {
            shared,
            stop: Arc::new(Stop {
                stopped: Mutex::new(false),
                wake: Condvar::new(),
            }),
            thread: None,
            workspace: Tokens(Vec::new()),
            distributed: false,
        };
        match monitor.start() {
            Ok(()) => Ok(monitor),
            Err(error) => {
                shared.signal();
                Err(error)
            }
        }
    }

    /// Whether loss has been observed.
    pub fn is_lost(&self) -> bool {
        self.shared.lost.load(Ordering::Acquire)
    }

    fn start(&mut self) -> io::Result<()> {
        let keys = Keys::new()?;
        self.observe_distributed()?;
        self.observe_workspace();
        // Observers are registered before the first check so a lock between
        // them is still observed.
        if keys.session_state() == SessionState::Lost {
            self.shared.signal();
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "macOS session is locked or not on the console",
            ));
        }
        let shared = self.shared;
        let stop = self.stop.clone();
        self.thread = Some(
            thread::Builder::new()
                .name("festerm-privacy-monitor".into())
                .spawn(move || poll(shared, &keys, &stop))?,
        );
        Ok(())
    }

    fn observe_distributed(&mut self) -> io::Result<()> {
        let name = CfString::new("com.apple.screenIsLocked")?;
        // SAFETY: the observer is a leaked `Shared`, valid forever; the
        // registration is removed in `Drop`.
        unsafe {
            CFNotificationCenterAddObserver(
                CFNotificationCenterGetDistributedCenter(),
                self.observer(),
                screen_locked,
                name.0,
                ptr::null(),
                DELIVER_IMMEDIATELY,
            );
        }
        self.distributed = true;
        Ok(())
    }

    fn observe_workspace(&mut self) {
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        // SAFETY: these are AppKit-provided notification-name constants.
        let names = unsafe {
            [
                objc2_app_kit::NSWorkspaceWillSleepNotification,
                objc2_app_kit::NSWorkspaceSessionDidResignActiveNotification,
            ]
        };
        for name in names {
            let shared = self.shared;
            let block = RcBlock::new(move |_: NonNull<NSNotification>| shared.signal());
            // SAFETY: the block captures only a `'static` reference; a nil
            // queue runs it on the posting thread, and the registration is
            // removed in `Drop`.
            let token = unsafe {
                center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
            };
            self.workspace.0.push(token);
        }
    }

    fn observer(&self) -> *const c_void {
        ptr::from_ref(self.shared).cast()
    }
}

fn poll(shared: &'static Shared, keys: &Keys, stop: &Stop) {
    let mut stopped = stop
        .stopped
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while !*stopped && !shared.lost.load(Ordering::Acquire) {
        stopped = stop
            .wake
            .wait_timeout(stopped, POLL_INTERVAL)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0;
        if !*stopped && keys.session_state() == SessionState::Lost {
            shared.signal();
        }
    }
}

impl Drop for PrivacyMonitor {
    fn drop(&mut self) {
        *self
            .stop
            .stopped
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.stop.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if self.distributed {
            // SAFETY: removes this monitor's registration only.
            unsafe {
                CFNotificationCenterRemoveEveryObserver(
                    CFNotificationCenterGetDistributedCenter(),
                    self.observer(),
                );
            }
        }
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        for token in self.workspace.0.drain(..) {
            // SAFETY: each token was returned by this center's block API.
            unsafe { center.removeObserver(token.as_ref()) };
        }
        self.shared
            .callback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

#[cfg(test)]
mod tests {
    use super::{classify, SessionState};

    #[test]
    fn only_unlocked_console_sessions_are_private() {
        assert_eq!(classify(None, Some(true)), SessionState::Private);
        assert_eq!(classify(Some(false), Some(true)), SessionState::Private);
        assert_eq!(classify(Some(true), Some(true)), SessionState::Lost);
        assert_eq!(classify(None, Some(false)), SessionState::Lost);
        assert_eq!(classify(Some(false), None), SessionState::Lost);
    }
}
