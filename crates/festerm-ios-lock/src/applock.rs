use crate::{
    clock::MonotonicClock,
    locker::{
        AuthFailure, AuthMethod, AuthOutcome, Authentication, Availability, BiometryKind, Locker,
    },
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

struct Request {
    id: u64,
    purpose: Pending,
    authentication: Option<Authentication>,
}

struct Inner {
    policy: LockPolicy,
    store: Option<SettingsStore>,
    load_status: LoadStatus,
    active: bool,
    shield_frames: u8,
    pending: Option<Request>,
    next_request: u64,
    automatic_unlock: bool,
    dialog_inactive: bool,
    backgrounded: bool,
    last_failure: Option<AuthFailure>,
    settings_error: Option<&'static str>,
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
        let automatic_unlock = policy.state() == LockState::Locked;
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
                    next_request: 0,
                    automatic_unlock,
                    dialog_inactive: false,
                    backgrounded: false,
                    last_failure: None,
                    settings_error: None,
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

    pub fn settings_error(&self) -> Option<&'static str> {
        self.inner().settings_error
    }

    /// Includes a dialog's inactivity interval even if its reply arrived first.
    pub fn authentication_prompt_in_flight(&self) -> bool {
        let inner = self.inner();
        inner.pending.is_some() || inner.dialog_inactive
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
        let more = {
            let mut inner = self.inner();
            inner.shield_frames = inner.shield_frames.saturating_sub(1);
            inner.shield_frames > 0
        };
        if more {
            (self.shared.repaint)();
        }
    }

    /// Lifecycle input; the state is updated synchronously so a lock engages
    /// before the next frame.
    pub fn handle_lifecycle(&self, event: LifecycleEvent) {
        let now = self.shared.clock.now();
        let retired = {
            let mut inner = self.inner();
            match event {
                LifecycleEvent::WillResignActive => {
                    inner.active = false;
                    inner.dialog_inactive |= inner.pending.is_some();
                    if !inner.dialog_inactive {
                        inner.policy.handle(event, now);
                    }
                    None
                }
                LifecycleEvent::DidEnterBackground | LifecycleEvent::DeviceLocked => {
                    inner.active = false;
                    inner.backgrounded = true;
                    inner.dialog_inactive = false;
                    inner.automatic_unlock = false;
                    let retired = inner.retire();
                    inner.policy.handle(event, now);
                    retired
                }
                LifecycleEvent::DidBecomeActive => {
                    let resumed = !inner.active || inner.backgrounded;
                    let from_dialog = inner.dialog_inactive && !inner.backgrounded;
                    inner.active = true;
                    inner.dialog_inactive = false;
                    inner.backgrounded = false;
                    if inner.policy.settings().enabled {
                        inner.shield_frames = SHIELD_FRAMES_AFTER_RESUME;
                    }
                    if !from_dialog {
                        inner.policy.handle(event, now);
                        if resumed && inner.policy.state() == LockState::Locked {
                            inner.automatic_unlock = true;
                            inner.last_failure = None;
                        }
                    }
                    None
                }
            }
        };
        // Native invalidation may reply synchronously; never hold the policy mutex.
        drop(retired);
        (self.shared.repaint)();
    }

    pub fn lock_now(&self) {
        let retired = {
            let mut inner = self.inner();
            let retired = inner.retire();
            inner.policy.lock_now();
            inner.automatic_unlock = inner.policy.state() == LockState::Locked;
            retired
        };
        drop(retired);
        (self.shared.repaint)();
    }

    /// Starts once per cold launch or genuine foreground lock interval.
    pub fn begin_automatic_unlock(&self) {
        self.start_unlock(AuthMethod::OwnerAuthentication, false, true);
    }

    /// Starts an unlock only when another request is not already pending.
    pub fn begin_unlock(&self) {
        self.start_unlock(AuthMethod::OwnerAuthentication, false, false);
    }

    /// Explicit retries can retire a request even when its native reply was lost.
    pub fn retry_unlock(&self) {
        self.start_unlock(AuthMethod::OwnerAuthentication, true, false);
    }

    pub fn begin_passcode_unlock(&self) {
        self.start_unlock(AuthMethod::DevicePasscode, true, false);
    }

    fn start_unlock(&self, method: AuthMethod, replace: bool, automatic: bool) {
        let (id, retired) = {
            let mut inner = self.inner();
            if !inner.active
                || inner.policy.state() == LockState::Unlocked
                || (automatic && !inner.automatic_unlock)
                || (!replace && inner.pending.is_some())
            {
                return;
            }
            let Some(id) = inner.next_request.checked_add(1) else {
                inner.last_failure = Some(AuthFailure::Unavailable);
                inner.automatic_unlock = false;
                return;
            };
            let retired = inner.retire();
            inner.policy.begin_unlock();
            inner.next_request = id;
            inner.pending = Some(Request {
                id,
                purpose: Pending::Unlock,
                authentication: None,
            });
            inner.automatic_unlock = false;
            inner.last_failure = None;
            (id, retired)
        };
        drop(retired);
        self.authenticate(id, method, "Unlock fesTerm");
    }

    fn authenticate(&self, id: u64, method: AuthMethod, reason: &str) {
        let shared = Arc::downgrade(&self.shared);
        // The mutex is not held here: a locker may complete synchronously.
        let mut authentication = Some(self.shared.locker.authenticate(
            method,
            reason,
            Box::new(move |outcome| {
                if let Some(shared) = shared.upgrade() {
                    AppLock { shared }.finish(id, outcome);
                }
            }),
        ));
        {
            let mut inner = self.inner();
            if let Some(pending) = inner.pending.as_mut().filter(|pending| pending.id == id) {
                pending.authentication = authentication.take();
            }
        }
        drop(authentication);
        (self.shared.repaint)();
    }

    fn finish(&self, id: u64, outcome: AuthOutcome) {
        let completed = {
            let mut inner = self.inner();
            if inner
                .pending
                .as_ref()
                .is_none_or(|pending| pending.id != id)
            {
                return;
            }
            let request = inner.pending.take().expect("matching authentication");
            match (outcome, request.purpose) {
                (Ok(()), Pending::Unlock) => {
                    inner.policy.authentication_succeeded();
                }
                (Ok(()), Pending::Disable(next)) => match inner.persist(&next) {
                    Ok(()) => inner.policy.set_settings(next),
                    Err(_) => inner.settings_error = Some("Could not save the lock settings."),
                },
                (Err(failure), _) => {
                    inner.policy.authentication_ended_without_success();
                    inner.last_failure = Some(failure);
                }
            }
            request.authentication
        };
        drop(completed);
        (self.shared.repaint)();
    }

    /// Applies settings from the settings screen. Enabling requires that the
    /// device can authenticate; disabling an enabled lock requires the owner to
    /// authenticate (`AuthenticationStarted`; poll `settings()` afterwards).
    /// Other changes apply at once. Nothing changes if persisting fails.
    pub fn apply_settings(&self, next: LockSettings) -> Result<(), SettingsError> {
        let availability = self.availability();
        let mut inner = self.inner();
        let current = inner.policy.settings();
        if next.enabled && !current.enabled && !matches!(availability, Availability::Available(_)) {
            return Err(SettingsError::CannotAuthenticate(availability));
        }
        inner.settings_error = None;
        if !next.enabled && current.enabled {
            // Without a device passcode the OS cannot authenticate anyone, and
            // anyone holding the device already has full access.
            if availability != Availability::PasscodeNotSet {
                if inner.pending.is_some() {
                    return Err(SettingsError::AuthenticationStarted);
                }
                let id = inner
                    .next_request
                    .checked_add(1)
                    .ok_or(SettingsError::CannotAuthenticate(Availability::Unavailable))?;
                inner.next_request = id;
                inner.pending = Some(Request {
                    id,
                    purpose: Pending::Disable(next),
                    authentication: None,
                });
                inner.last_failure = None;
                drop(inner);
                self.authenticate(
                    id,
                    AuthMethod::OwnerAuthentication,
                    "Turn off fesTerm app lock",
                );
                return Err(SettingsError::AuthenticationStarted);
            }
        }
        inner
            .persist(&next)
            .map_err(|e| SettingsError::Persist(e.to_string()))?;
        inner.policy.set_settings(next);
        let retired = inner.retire();
        if !next.enabled {
            inner.automatic_unlock = false;
        }
        drop(inner);
        drop(retired);
        (self.shared.repaint)();
        Ok(())
    }
}

impl Inner {
    fn retire(&mut self) -> Option<Authentication> {
        let request = self.pending.take()?;
        self.policy.authentication_ended_without_success();
        request.authentication
    }

    fn persist(&self, settings: &LockSettings) -> std::io::Result<()> {
        match &self.store {
            Some(store) => store.save(settings),
            None => Ok(()),
        }
    }
}
