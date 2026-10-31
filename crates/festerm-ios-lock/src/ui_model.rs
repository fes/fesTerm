//! UI-framework-free copy and view models for screens 14 and 15, so the
//! integrator only draws them.

use crate::{
    locker::{AuthFailure, BiometryKind},
    policy::BackgroundGrace,
};

pub const UNLOCK_TITLE: &str = "Unlock fesTerm";
pub const USE_PASSCODE_LABEL: &str = "Use device passcode";
pub const PROTECT_LABEL: &str = "Protect fesTerm";
pub const REQUIRE_UNLOCK_ON_LAUNCH_LABEL: &str = "On app launch";
pub const REQUIRE_UNLOCK_AFTER_DEVICE_LOCK_LABEL: &str = "After device lock";
pub const BACKGROUND_GRACE_LABEL: &str = "After switching apps";
pub const HIDE_PREVIEW_LABEL: &str = "Hide app-switcher preview";
pub const LOCK_NOW_LABEL: &str = "Lock now";
/// Must stay visible next to the lock settings.
pub const NOT_SESSION_RECOVERY_NOTE: &str =
    "App lock is not session recovery. Locking does not end SSH or Dev Tunnel sessions or resume \
     anything that has ended.";

pub struct UnlockScreen {
    pub title: &'static str,
    pub primary_button: &'static str,
    pub passcode_button: &'static str,
    pub error: Option<&'static str>,
}

impl UnlockScreen {
    pub fn new(biometry: BiometryKind, failure: Option<AuthFailure>) -> Self {
        Self {
            title: UNLOCK_TITLE,
            primary_button: biometry.button_label(),
            passcode_button: USE_PASSCODE_LABEL,
            error: failure.and_then(failure_message),
        }
    }
}

/// `None` for a plain cancel: nothing to report.
pub fn failure_message(failure: AuthFailure) -> Option<&'static str> {
    match failure {
        AuthFailure::Cancelled => None,
        AuthFailure::NotRecognized => Some("Not recognized. Try again or use your passcode."),
        AuthFailure::BiometryLockedOut => Some("Biometrics are locked. Use your device passcode."),
        AuthFailure::PasscodeNotSet => Some("Set a device passcode in Settings to unlock fesTerm."),
        AuthFailure::Unavailable => Some("Authentication is unavailable right now."),
    }
}

pub const GRACE_CHOICES: [BackgroundGrace; 5] = BackgroundGrace::ALL;
