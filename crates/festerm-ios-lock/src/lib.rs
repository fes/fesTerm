//! Face ID / passcode app lock for the iOS host (ADR 0058).
//!
//! The crate is UI-framework free. The host:
//! 1. builds an [`AppLock`] before creating its window (cold launch is
//!    `Locked` when enabled),
//! 2. draws terminal/files content only while
//!    [`AppLock::should_draw_content`] is true,
//! 3. on iOS creates a [`LockBridge`] that feeds lifecycle events in and keeps
//!    a native privacy shield over the window,
//! 4. calls [`AppLock::note_frame_presented`] and `LockBridge::sync` each frame.

mod applock;
mod clock;
mod locker;
mod policy;
mod store;
pub mod ui_model;

pub use applock::{AppLock, SettingsError};
pub use clock::{ManualClock, MonotonicClock, SystemClock};
pub use locker::{AuthFailure, AuthOutcome, Availability, BiometryKind, FakeLocker, Locker};
pub use policy::{BackgroundGrace, LifecycleEvent, LockPolicy, LockSettings, LockState};
pub use store::{LoadStatus, SettingsStore};

#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "ios")]
pub use ios::{LaContextLocker, LockBridge};

#[cfg(not(target_os = "ios"))]
pub struct LockBridge;

#[cfg(not(target_os = "ios"))]
impl LockBridge {
    pub fn new(_window: raw_window_handle::WindowHandle<'_>, _lock: AppLock) -> Option<Self> {
        None
    }

    pub fn sync(&self) {}
}
