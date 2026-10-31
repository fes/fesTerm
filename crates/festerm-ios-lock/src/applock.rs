use crate::{
    clock::MonotonicClock,
    locker::{AuthFailure, AuthOutcome, Availability, BiometryKind, Locker},
    policy::{LifecycleEvent, LockPolicy, LockSettings, LockState},
    store::{LoadStatus, SettingsStore},
};
use std::sync::{Arc, Mutex, MutexGuard};

/// Frames that must be presented after launch/resume before the native
/// privacy shield may come off, so a stale terminal frame is never exposed.
const SHIELD_FRAMES_AFTER_RESUME: u8 = 2;

#[derive(Debug, Eq, PartialEq)]
pub enum SettingsError {
    /// Enabling needs a device passcode (or biometrics) to be set up.
    CannotAuthenticate(Availability),
    /// Disabling needs the owner to authenticate first; a dialog was started.
    AuthenticationStarted,
    Persist(String),
}

enum Pending {
    Unlock,
    Disable(LockSettings),
}

struct Inner {
    policy: LockPolicy,
    store: Option<SettingsStore>,
    load_status: LoadStatus,
    active: bool,
    shield_frames: u8,
    pending: Option<Pending>,
    last_failure: Option<AuthFailure>,
}

struct Shared {
    inner: Mutex<Inner>,
    locker: Arc<dyn Locker>,
    clock: Arc<dyn MonotonicClock>,
    repaint: Box<dyn Fn() + Send + Sync>,
}

/// Thread-safe handle: the lifecycle bridge, the egui host and the Locker
/// callback all share one instance.
#[derive(Clone)]
pub struct AppLock {
    shared: Arc<Shared>,
}

impl AppLock {
    /// Loads settings and engages the cold-launch lock before anything draws.
    pub fn new(
        store: Option<SettingsStore>,
        locker: Arc<dyn Locker>,
        clock: Arc<dyn MonotonicClock>,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let (settings, load_status) = store
            .as_ref()
            .map_or((LockSettings::default(), LoadStatus::Missing), |s| s.load());
        let policy = LockPolicy::launch(settings);
        let shield_frames = if settings.enabled {
            SHIELD_FRAMES_AFTER_RESUME
        } else {
            0
        };
        Self {
            shared: Arc::new(Shared {
                inner: Mutex::new(Inner {
                    policy,
                    store,
                    load_status,
                    active: true,
                    shield_frames,
                    pending: None,
                    last_failure: None,
                }),
                locker,
                clock,
                repaint: Box::new(repaint),
            }),
        }
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.shared.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn state(&self) -> LockState {
        self.inner().policy.state()
    }

    /// Terminal and file content must be drawn only when this is true.
    pub fn should_draw_content(&self) -> bool {
        self.inner().policy.should_draw_content()
    }

    pub fn settings(&self) -> LockSettings {
        self.inner().policy.settings()
    }

    /// `Untrusted` means the file was unreadable and the lock failed closed.
    pub fn load_status(&self) -> LoadStatus {
        self.inner().load_status
    }

    pub fn last_failure(&self) -> Option<AuthFailure> {
        self.inner().last_failure
    }

    pub fn availability(&self) -> Availability {
        self.shared.locker.availability()
    }

    pub fn biometry(&self) -> BiometryKind {
        match self.availability() {
            Availability::Available(kind) => kind,
            _ => BiometryKind::None,
        }
    }

    /// Whether a native view must cover the window right now.
    pub fn shield_should_cover(&self) -> bool {
        let inner = self.inner();
        let settings = inner.policy.settings();
        if !settings.enabled {
            return false;
        }
        if !inner.active {
            return inner.policy.should_hide_snapshot();
        }
        inner.shield_frames > 0
    }

    /// Call once per presented frame (after honouring `should_draw_content`).
    pub fn note_frame_presented(&self) {
        let mut inner = self.inner();
        inner.shield_frames = inner.shield_frames.saturating_sub(1);
    }

    /// Lifecycle input; the state is updated synchronously so a lock engages
    /// before the next frame.
    pub fn handle_lifecycle(&self, event: LifecycleEvent) {
        let now = self.shared.clock.now();
        {
            let mut inner = self.inner();
            match event {
                LifecycleEvent::WillResignActive => inner.active = false,
                LifecycleEvent::DidBecomeActive => {
                    inner.active = true;
                    if inner.policy.settings().enabled {
                        inner.shield_frames = SHIELD_FRAMES_AFTER_RESUME;
                    }
                }
                _ => {}
            }
            // An authentication dialog we started deactivates the app; that
            // must not restart or consume the grace timer.
            let from_dialog =
                inner.pending.is_some() && !matches!(event, LifecycleEvent::DeviceLocked);
            if !from_dialog {
                inner.policy.handle(event, now);
            }
        }
        (self.shared.repaint)();
    }

    pub fn lock_now(&self) {
        self.inner().policy.lock_now();
        (self.shared.repaint)();
    }

    /// "Use Face ID" / "Use device passcode": both run the owner-authentication
    /// policy, which falls back to the passcode.
    pub fn begin_unlock(&self) {
        {
            let mut inner = self.inner();
            if !inner.policy.begin_unlock() {
                return;
            }
            inner.pending = Some(Pending::Unlock);
            inner.last_failure = None;
        }
        self.authenticate("Unlock fesTerm");
    }

    fn authenticate(&self, reason: &str) {
        let shared = self.shared.clone();
        // The mutex is not held here: a locker may complete synchronously.
        self.shared.locker.authenticate(
            reason,
            Box::new(move |outcome| AppLock { shared }.finish(outcome)),
        );
    }

    fn finish(&self, outcome: AuthOutcome) {
        {
            let mut inner = self.inner();
            let pending = inner.pending.take();
            match (outcome, pending) {
                (Ok(()), Some(Pending::Unlock)) => {
                    inner.policy.authentication_succeeded();
                }
                (Ok(()), Some(Pending::Disable(next))) => {
                    if inner.persist(&next).is_ok() {
                        inner.policy.set_settings(next);
                    }
                }
                (Err(failure), _) => {
                    inner.policy.authentication_ended_without_success();
                    inner.last_failure = Some(failure);
                }
                (Ok(()), None) => {}
            }
        }
        (self.shared.repaint)();
    }

    /// Applies settings from the settings screen. Enabling requires that the
    /// device can authenticate; disabling an enabled lock requires the owner to
    /// authenticate (`AuthenticationStarted`; poll `settings()` afterwards).
    /// Other changes apply at once. Nothing changes if persisting fails.
    pub fn apply_settings(&self, next: LockSettings) -> Result<(), SettingsError> {
        let current = self.settings();
        if next.enabled && !current.enabled {
            let availability = self.availability();
            if !matches!(availability, Availability::Available(_)) {
                return Err(SettingsError::CannotAuthenticate(availability));
            }
        }
        if !next.enabled && current.enabled {
            // Without a device passcode the OS cannot authenticate anyone, and
            // anyone holding the device already has full access.
            if self.availability() != Availability::PasscodeNotSet {
                {
                    let mut inner = self.inner();
                    if inner.pending.is_some() {
                        return Ok(());
                    }
                    inner.pending = Some(Pending::Disable(next));
                }
                self.authenticate("Turn off fesTerm app lock");
                return Err(SettingsError::AuthenticationStarted);
            }
        }
        let mut inner = self.inner();
        inner
            .persist(&next)
            .map_err(|e| SettingsError::Persist(e.to_string()))?;
        inner.policy.set_settings(next);
        drop(inner);
        (self.shared.repaint)();
        Ok(())
    }
}

impl Inner {
    fn persist(&self, settings: &LockSettings) -> std::io::Result<()> {
        match &self.store {
            Some(store) => store.save(settings),
            None => Ok(()),
        }
    }
}
