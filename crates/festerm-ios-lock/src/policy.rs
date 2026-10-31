use serde::{Deserialize, Serialize};
use std::time::Duration;

/// How long the app may be away before it locks again ("after switching apps").
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BackgroundGrace {
    #[serde(rename = "always")]
    Always,
    #[serde(rename = "1m")]
    Minutes1,
    #[serde(rename = "5m")]
    Minutes5,
    #[serde(rename = "15m")]
    Minutes15,
    #[serde(rename = "never")]
    Never,
}

impl BackgroundGrace {
    pub const ALL: [Self; 5] = [
        Self::Always,
        Self::Minutes1,
        Self::Minutes5,
        Self::Minutes15,
        Self::Never,
    ];

    /// `None` means the app never locks because of switching away.
    pub const fn duration(self) -> Option<Duration> {
        match self {
            Self::Always => Some(Duration::ZERO),
            Self::Minutes1 => Some(Duration::from_secs(60)),
            Self::Minutes5 => Some(Duration::from_secs(5 * 60)),
            Self::Minutes15 => Some(Duration::from_secs(15 * 60)),
            Self::Never => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Always => "Immediately",
            Self::Minutes1 => "After 1 minute",
            Self::Minutes5 => "After 5 minutes",
            Self::Minutes15 => "After 15 minutes",
            Self::Never => "Never",
        }
    }
}

/// User-visible app-lock settings (mockup screen 15).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LockSettings {
    /// "Protect fesTerm".
    pub enabled: bool,
    /// Require unlock on app launch.
    pub on_launch: bool,
    /// Require unlock after the device was locked while the app was away.
    pub after_device_lock: bool,
    /// Require unlock after switching apps for this long.
    pub background_grace: BackgroundGrace,
    /// Cover the app-switcher snapshot.
    pub hide_app_switcher_preview: bool,
}

impl Default for LockSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            on_launch: true,
            after_device_lock: true,
            background_grace: BackgroundGrace::Minutes1,
            hide_app_switcher_preview: true,
        }
    }
}

impl LockSettings {
    /// Used when the settings file cannot be trusted: never silently unprotect.
    pub const fn fail_closed() -> Self {
        Self {
            enabled: true,
            on_launch: true,
            after_device_lock: true,
            background_grace: BackgroundGrace::Always,
            hide_app_switcher_preview: true,
        }
    }
}

/// Gate for sensitive content. Only `Unlocked` may draw terminal or file content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockState {
    Locked,
    /// A system authentication dialog is up.
    Unlocking,
    Unlocked,
}

/// Application lifecycle input, independent of UIKit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleEvent {
    WillResignActive,
    DidBecomeActive,
    DidEnterBackground,
    /// The device locked (protected data became unavailable).
    DeviceLocked,
}

/// Pure lock state machine. Time is an injected monotonic `Duration`.
#[derive(Clone, Debug)]
pub struct LockPolicy {
    settings: LockSettings,
    state: LockState,
    inactive_since: Option<Duration>,
}

impl LockPolicy {
    /// Cold launch: locks before anything is drawn when `on_launch` is set.
    pub fn launch(settings: LockSettings) -> Self {
        let state = if settings.enabled && settings.on_launch {
            LockState::Locked
        } else {
            LockState::Unlocked
        };
        Self {
            settings,
            state,
            inactive_since: None,
        }
    }

    pub fn settings(&self) -> LockSettings {
        self.settings
    }

    pub fn state(&self) -> LockState {
        self.state
    }

    pub fn should_draw_content(&self) -> bool {
        self.state == LockState::Unlocked
    }

    /// Whether the app-switcher snapshot must be covered while inactive.
    pub fn should_hide_snapshot(&self) -> bool {
        self.settings.enabled
            && (self.settings.hide_app_switcher_preview || self.state != LockState::Unlocked)
    }

    pub fn handle(&mut self, event: LifecycleEvent, now: Duration) -> LockState {
        if !self.settings.enabled {
            self.inactive_since = None;
            return self.state;
        }
        match event {
            LifecycleEvent::WillResignActive | LifecycleEvent::DidEnterBackground => {
                // The authentication dialog itself deactivates the app; it
                // must neither start nor restart the grace timer.
                if self.state == LockState::Unlocked && self.inactive_since.is_none() {
                    self.inactive_since = Some(now);
                }
            }
            LifecycleEvent::DeviceLocked => {
                if self.settings.after_device_lock && self.state != LockState::Locked {
                    self.state = LockState::Locked;
                }
            }
            LifecycleEvent::DidBecomeActive => {
                if let Some(since) = self.inactive_since.take() {
                    if self.state == LockState::Unlocked && self.grace_expired(since, now) {
                        self.state = LockState::Locked;
                    }
                }
            }
        }
        self.state
    }

    fn grace_expired(&self, since: Duration, now: Duration) -> bool {
        match self.settings.background_grace.duration() {
            None => false,
            // A clock that went backwards cannot prove the grace was respected.
            Some(grace) => now.checked_sub(since).is_none_or(|away| away >= grace),
        }
    }

    /// `Locked -> Unlocking`; false when no dialog should be presented.
    pub fn begin_unlock(&mut self) -> bool {
        if self.state == LockState::Locked {
            self.state = LockState::Unlocking;
            true
        } else {
            false
        }
    }

    /// Only an in-flight unlock can succeed; a lock that engaged meanwhile wins.
    pub fn authentication_succeeded(&mut self) -> bool {
        if self.state == LockState::Unlocking {
            self.state = LockState::Unlocked;
            self.inactive_since = None;
            true
        } else {
            false
        }
    }

    pub fn authentication_ended_without_success(&mut self) {
        if self.state == LockState::Unlocking {
            self.state = LockState::Locked;
        }
    }

    /// "Lock now".
    pub fn lock_now(&mut self) {
        if self.settings.enabled {
            self.state = LockState::Locked;
        }
    }

    /// Callers must have authenticated before disabling an enabled lock.
    pub fn set_settings(&mut self, settings: LockSettings) {
        self.settings = settings;
        if !settings.enabled {
            self.state = LockState::Unlocked;
            self.inactive_since = None;
        }
    }
}
