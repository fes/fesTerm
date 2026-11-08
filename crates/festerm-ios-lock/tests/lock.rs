use festerm_ios_lock::{
    AppLock, AuthFailure, AuthMethod, AuthOutcome, Authentication, Availability, BackgroundGrace,
    BiometryKind, FakeLocker, LifecycleEvent, LoadStatus, LockPolicy, LockSettings, LockState,
    Locker, ManualClock, SettingsError, SettingsStore,
};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

const FACE: Availability = Availability::Available(BiometryKind::FaceId);

fn enabled(grace: BackgroundGrace) -> LockSettings {
    LockSettings {
        enabled: true,
        background_grace: grace,
        ..LockSettings::default()
    }
}

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

#[test]
fn cold_launch_is_locked_only_when_enabled_and_requested() {
    assert_eq!(
        LockPolicy::launch(LockSettings::default()).state(),
        LockState::Unlocked
    );
    assert_eq!(
        LockPolicy::launch(enabled(BackgroundGrace::Never)).state(),
        LockState::Locked
    );
    let no_launch = LockSettings {
        on_launch: false,
        ..enabled(BackgroundGrace::Never)
    };
    assert!(LockPolicy::launch(no_launch).should_draw_content());
    assert!(!LockPolicy::launch(enabled(BackgroundGrace::Never)).should_draw_content());
}

fn unlocked(settings: LockSettings) -> LockPolicy {
    let mut p = LockPolicy::launch(settings);
    assert!(p.begin_unlock());
    assert_eq!(p.state(), LockState::Unlocking);
    assert!(p.authentication_succeeded());
    p
}

#[test]
fn grace_periods_lock_at_the_boundary() {
    for (grace, limit) in [
        (BackgroundGrace::Minutes1, 60),
        (BackgroundGrace::Minutes5, 300),
        (BackgroundGrace::Minutes15, 900),
    ] {
        let mut p = unlocked(enabled(grace));
        p.handle(LifecycleEvent::WillResignActive, secs(100));
        p.handle(LifecycleEvent::DidBecomeActive, secs(100 + limit - 1));
        assert_eq!(p.state(), LockState::Unlocked, "{grace:?} just inside");
        p.handle(LifecycleEvent::WillResignActive, secs(1000));
        p.handle(LifecycleEvent::DidBecomeActive, secs(1000 + limit));
        assert_eq!(p.state(), LockState::Locked, "{grace:?} at limit");
    }
}

#[test]
fn always_and_never() {
    let mut p = unlocked(enabled(BackgroundGrace::Always));
    p.handle(LifecycleEvent::WillResignActive, secs(5));
    p.handle(LifecycleEvent::DidBecomeActive, secs(5));
    assert_eq!(p.state(), LockState::Locked);

    let mut p = unlocked(enabled(BackgroundGrace::Never));
    p.handle(LifecycleEvent::WillResignActive, secs(5));
    p.handle(LifecycleEvent::DidBecomeActive, secs(10_000_000));
    assert_eq!(p.state(), LockState::Unlocked);
}

#[test]
fn background_after_resign_does_not_restart_the_timer() {
    let mut p = unlocked(enabled(BackgroundGrace::Minutes1));
    p.handle(LifecycleEvent::WillResignActive, secs(0));
    p.handle(LifecycleEvent::DidEnterBackground, secs(50));
    p.handle(LifecycleEvent::DidBecomeActive, secs(61));
    assert_eq!(p.state(), LockState::Locked);
}

#[test]
fn authentication_dialog_does_not_restart_or_start_the_timer() {
    let mut p = LockPolicy::launch(enabled(BackgroundGrace::Minutes1));
    assert!(p.begin_unlock());
    // The Face ID sheet deactivates and reactivates the app while Unlocking.
    p.handle(LifecycleEvent::WillResignActive, secs(1));
    p.handle(LifecycleEvent::DidBecomeActive, secs(500));
    assert!(p.authentication_succeeded());
    p.handle(LifecycleEvent::WillResignActive, secs(600));
    p.handle(LifecycleEvent::DidBecomeActive, secs(630));
    assert_eq!(
        p.state(),
        LockState::Unlocked,
        "timer started only at the real switch"
    );
}

#[test]
fn device_lock_locks_only_when_requested_and_beats_a_late_success() {
    let mut p = unlocked(enabled(BackgroundGrace::Never));
    p.handle(LifecycleEvent::DeviceLocked, secs(1));
    assert_eq!(p.state(), LockState::Locked);

    let off = LockSettings {
        after_device_lock: false,
        ..enabled(BackgroundGrace::Never)
    };
    let mut p = unlocked(off);
    p.handle(LifecycleEvent::DeviceLocked, secs(1));
    assert_eq!(p.state(), LockState::Unlocked);

    let mut p = LockPolicy::launch(enabled(BackgroundGrace::Never));
    p.begin_unlock();
    p.handle(LifecycleEvent::DeviceLocked, secs(1));
    assert!(!p.authentication_succeeded());
    assert_eq!(p.state(), LockState::Locked);
}

#[test]
fn backwards_clock_fails_closed() {
    let mut p = unlocked(enabled(BackgroundGrace::Minutes15));
    p.handle(LifecycleEvent::WillResignActive, secs(1000));
    p.handle(LifecycleEvent::DidBecomeActive, secs(10));
    assert_eq!(p.state(), LockState::Locked);
}

#[test]
fn failed_authentication_returns_to_locked_and_lock_now_works() {
    let mut p = LockPolicy::launch(enabled(BackgroundGrace::Never));
    p.begin_unlock();
    p.authentication_ended_without_success();
    assert_eq!(p.state(), LockState::Locked);
    let mut p = unlocked(enabled(BackgroundGrace::Never));
    p.lock_now();
    assert_eq!(p.state(), LockState::Locked);
    // A disabled lock never locks.
    let mut p = LockPolicy::launch(LockSettings::default());
    p.lock_now();
    assert!(p.should_draw_content());
}

#[test]
fn snapshot_hiding_follows_settings_and_state() {
    let mut s = enabled(BackgroundGrace::Never);
    s.hide_app_switcher_preview = false;
    let mut p = unlocked(s);
    assert!(!p.should_hide_snapshot());
    p.lock_now();
    assert!(p.should_hide_snapshot(), "locked content is always hidden");
    s.hide_app_switcher_preview = true;
    assert!(unlocked(s).should_hide_snapshot());
    assert!(!LockPolicy::launch(LockSettings::default()).should_hide_snapshot());
}

fn app(
    settings: LockSettings,
    locker: &Arc<FakeLocker>,
) -> (
    AppLock,
    Arc<ManualClock>,
    Arc<AtomicUsize>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let store = SettingsStore::new(dir.path().join("lock.json"));
    store.save(&settings).unwrap();
    let clock = Arc::new(ManualClock::new());
    let repaints = Arc::new(AtomicUsize::new(0));
    let r = repaints.clone();
    let lock = AppLock::new(Some(store), locker.clone(), clock.clone(), move || {
        r.fetch_add(1, Ordering::SeqCst);
    });
    (lock, clock, repaints, dir)
}

#[test]
fn locked_content_is_gated_until_authentication_succeeds() {
    let locker = FakeLocker::new(FACE);
    let (lock, _clock, repaints, _dir) = app(enabled(BackgroundGrace::Never), &locker);
    assert_eq!(lock.state(), LockState::Locked);
    assert!(!lock.should_draw_content());
    assert!(lock.shield_should_cover(), "cold launch is shielded");

    locker.set_deferred(true);
    lock.begin_unlock();
    assert_eq!(lock.state(), LockState::Unlocking);
    assert!(!lock.should_draw_content());
    lock.begin_unlock();
    assert_eq!(locker.pending_count(), 1, "no second dialog");
    locker.complete_pending(Ok(()));
    assert!(lock.should_draw_content());
    assert!(repaints.load(Ordering::SeqCst) > 0);
    assert_eq!(locker.prompt_reasons(), ["Unlock fesTerm"]);
}

#[test]
fn failures_keep_it_locked_and_are_reported() {
    let locker = FakeLocker::new(FACE);
    let (lock, ..) = app(enabled(BackgroundGrace::Never), &locker);
    locker.push_outcome(Err(AuthFailure::NotRecognized));
    lock.begin_unlock();
    assert_eq!(lock.state(), LockState::Locked);
    assert_eq!(lock.last_failure(), Some(AuthFailure::NotRecognized));
    locker.push_outcome(Ok(()));
    lock.begin_unlock();
    assert!(lock.should_draw_content());
    assert_eq!(lock.last_failure(), None);
}

#[test]
fn resume_locks_synchronously_and_shield_waits_for_frames() {
    let locker = FakeLocker::new(FACE);
    let (lock, clock, ..) = app(enabled(BackgroundGrace::Minutes1), &locker);
    locker.push_outcome(Ok(()));
    lock.begin_unlock();
    lock.note_frame_presented();
    lock.note_frame_presented();
    assert!(!lock.shield_should_cover());

    lock.handle_lifecycle(LifecycleEvent::WillResignActive);
    assert!(
        lock.shield_should_cover(),
        "app switcher snapshot is covered"
    );
    clock.advance(secs(120));
    lock.handle_lifecycle(LifecycleEvent::DidBecomeActive);
    assert!(!lock.should_draw_content(), "locked before any frame");
    assert!(lock.shield_should_cover());
    lock.note_frame_presented();
    assert!(lock.shield_should_cover());
    lock.note_frame_presented();
    assert!(!lock.shield_should_cover());
}

#[test]
fn short_absence_keeps_content_and_dialogs_dont_count() {
    let locker = FakeLocker::new(FACE);
    let (lock, clock, ..) = app(enabled(BackgroundGrace::Minutes1), &locker);
    locker.set_deferred(true);
    lock.begin_unlock();
    lock.handle_lifecycle(LifecycleEvent::WillResignActive);
    clock.advance(secs(600));
    lock.handle_lifecycle(LifecycleEvent::DidBecomeActive);
    locker.complete_pending(Ok(()));
    lock.handle_lifecycle(LifecycleEvent::WillResignActive);
    clock.advance(secs(10));
    lock.handle_lifecycle(LifecycleEvent::DidBecomeActive);
    assert!(lock.should_draw_content());
}

#[test]
fn inactive_authentication_dialog_revokes_recording_consent() {
    let locker = FakeLocker::new(FACE);
    let callbacks = Arc::new(AtomicUsize::new(0));
    let observed = callbacks.clone();
    let lock = AppLock::new_with_content_lock_callback(
        None,
        locker.clone(),
        Arc::new(ManualClock::new()),
        || {},
        move || {
            observed.fetch_add(1, Ordering::SeqCst);
        },
    );
    lock.apply_settings(enabled(BackgroundGrace::Never))
        .unwrap();
    locker.set_deferred(true);
    assert_eq!(
        lock.apply_settings(LockSettings::default()),
        Err(SettingsError::AuthenticationStarted)
    );
    let generation = lock.recording_privacy_generation().unwrap();

    lock.handle_lifecycle(LifecycleEvent::WillResignActive);
    assert!(lock.authentication_prompt_in_flight());
    assert_eq!(callbacks.load(Ordering::SeqCst), 1);
    assert_eq!(lock.recording_privacy_generation(), None);
    lock.handle_lifecycle(LifecycleEvent::DidBecomeActive);
    assert_ne!(lock.recording_privacy_generation(), Some(generation));
}

#[test]
fn enabling_requires_a_passcode_and_disabling_requires_authentication() {
    let locker = FakeLocker::new(Availability::PasscodeNotSet);
    let (lock, _c, _r, dir) = app(LockSettings::default(), &locker);
    assert_eq!(
        lock.apply_settings(enabled(BackgroundGrace::Never)),
        Err(SettingsError::CannotAuthenticate(
            Availability::PasscodeNotSet
        ))
    );
    assert!(!lock.settings().enabled);

    locker.set_availability(FACE);
    lock.apply_settings(enabled(BackgroundGrace::Never))
        .unwrap();
    assert!(lock.settings().enabled);
    let (saved, status) = SettingsStore::new(dir.path().join("lock.json")).load();
    assert_eq!((saved.enabled, status), (true, LoadStatus::Loaded));
    assert!(
        lock.should_draw_content(),
        "enabling does not lock the running app"
    );

    locker.push_outcome(Err(AuthFailure::Cancelled));
    assert_eq!(
        lock.apply_settings(LockSettings::default()),
        Err(SettingsError::AuthenticationStarted)
    );
    assert!(lock.settings().enabled, "cancelled auth keeps protection");

    locker.push_outcome(Ok(()));
    let _ = lock.apply_settings(LockSettings::default());
    assert!(!lock.settings().enabled);
    assert!(SettingsStore::new(dir.path().join("lock.json")).load().0 == LockSettings::default());
}

#[test]
fn disable_dialog_does_not_consume_the_grace_timer() {
    let locker = FakeLocker::new(FACE);
    let (lock, clock, ..) = app(enabled(BackgroundGrace::Always), &locker);
    locker.push_outcome(Ok(()));
    lock.begin_unlock();
    locker.set_deferred(true);
    let _ = lock.apply_settings(LockSettings::default());
    lock.handle_lifecycle(LifecycleEvent::WillResignActive);
    clock.advance(secs(5));
    lock.handle_lifecycle(LifecycleEvent::DidBecomeActive);
    locker.complete_pending(Err(AuthFailure::Cancelled));
    assert!(lock.should_draw_content());
}

#[test]
fn missing_passcode_allows_turning_lock_off_without_authentication() {
    let locker = FakeLocker::new(Availability::PasscodeNotSet);
    let (lock, ..) = app(enabled(BackgroundGrace::Never), &locker);
    assert_eq!(lock.apply_settings(LockSettings::default()), Ok(()));
    assert!(lock.should_draw_content());
}

#[test]
fn store_round_trips_and_fails_closed_on_untrusted_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("lock.json");
    let store = SettingsStore::new(&path);
    assert_eq!(store.load(), (LockSettings::default(), LoadStatus::Missing));
    let s = LockSettings {
        enabled: true,
        on_launch: false,
        after_device_lock: false,
        background_grace: BackgroundGrace::Minutes15,
        hide_app_switcher_preview: false,
    };
    store.save(&s).unwrap();
    assert_eq!(store.load(), (s, LoadStatus::Loaded));
    assert!(std::fs::read_to_string(&path).unwrap().contains("\"15m\""));

    for bad in [
        b"{not json".to_vec(),
        br#"{"version":2,"enabled":false}"#.to_vec(),
        vec![b' '; 5000],
        br#"{"enabled":false}"#.to_vec(),
    ] {
        std::fs::write(&path, bad).unwrap();
        assert_eq!(
            store.load(),
            (LockSettings::fail_closed(), LoadStatus::Untrusted)
        );
    }
    assert!(LockSettings::fail_closed().enabled);
}

#[test]
fn unreadable_settings_lock_the_app_at_launch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lock.json");
    std::fs::write(&path, b"garbage").unwrap();
    let lock = AppLock::new(
        Some(SettingsStore::new(path)),
        FakeLocker::new(FACE),
        Arc::new(ManualClock::new()),
        || {},
    );
    assert_eq!(lock.load_status(), LoadStatus::Untrusted);
    assert!(!lock.should_draw_content());
}

#[test]
fn system_clock_is_monotonic() {
    use festerm_ios_lock::{MonotonicClock, SystemClock};
    let a = SystemClock.now();
    let b = SystemClock.now();
    assert!(b >= a);
    // The portable fallback measures from its first call, so it may read zero.
    #[cfg(unix)]
    assert!(a > Duration::ZERO);
}

#[test]
fn stale_unlock_success_after_device_lock_cannot_complete_a_new_attempt() {
    let locker = FakeLocker::new(FACE);
    let (lock, ..) = app(enabled(BackgroundGrace::Never), &locker);
    locker.set_deferred(true);
    lock.begin_unlock();
    lock.handle_lifecycle(LifecycleEvent::DeviceLocked);
    lock.handle_lifecycle(LifecycleEvent::DidBecomeActive);
    lock.begin_unlock();
    assert_eq!(locker.pending_count(), 2);
    locker.complete_pending(Ok(()));
    assert!(
        !lock.should_draw_content(),
        "the retired request must never authenticate the replacement"
    );
    locker.complete_pending(Ok(()));
    assert!(lock.should_draw_content());
}

#[test]
fn automatic_unlock_rearms_after_background_but_not_its_own_cancelled_dialog() {
    let locker = FakeLocker::new(FACE);
    let (lock, ..) = app(enabled(BackgroundGrace::Always), &locker);
    locker.set_deferred(true);
    lock.begin_automatic_unlock();
    lock.begin_automatic_unlock();
    assert_eq!(locker.pending_count(), 1);
    lock.handle_lifecycle(LifecycleEvent::WillResignActive);
    locker.complete_pending(Err(AuthFailure::Cancelled));
    assert!(lock.authentication_prompt_in_flight());
    lock.handle_lifecycle(LifecycleEvent::DidBecomeActive);
    assert!(!lock.authentication_prompt_in_flight());
    lock.begin_automatic_unlock();
    assert_eq!(locker.pending_count(), 0, "no automatic prompt loop");
    assert_eq!(lock.last_failure(), Some(AuthFailure::Cancelled));
    lock.handle_lifecycle(LifecycleEvent::WillResignActive);
    lock.handle_lifecycle(LifecycleEvent::DidEnterBackground);
    lock.handle_lifecycle(LifecycleEvent::DidBecomeActive);
    lock.begin_automatic_unlock();
    assert_eq!(locker.pending_count(), 1);
    locker.complete_pending(Ok(()));
    assert!(lock.should_draw_content());
}

#[test]
fn missing_completion_can_be_retired_by_explicit_passcode_retry() {
    let locker = FakeLocker::new(FACE);
    let (lock, ..) = app(enabled(BackgroundGrace::Never), &locker);
    locker.set_deferred(true);
    lock.begin_unlock();
    lock.begin_passcode_unlock();
    assert_eq!(locker.cancellation_count(), 1);
    assert_eq!(
        locker.prompt_methods(),
        [AuthMethod::OwnerAuthentication, AuthMethod::DevicePasscode]
    );
    locker.complete_pending(Ok(()));
    assert_eq!(lock.state(), LockState::Unlocking, "old success is ignored");
    locker.complete_pending(Err(AuthFailure::NotRecognized));
    assert_eq!(lock.state(), LockState::Locked);
    lock.retry_unlock();
    locker.complete_pending(Ok(()));
    assert!(lock.should_draw_content());
}

#[test]
fn stale_failure_after_lock_now_cannot_end_a_replacement_attempt() {
    let locker = FakeLocker::new(FACE);
    let (lock, ..) = app(enabled(BackgroundGrace::Never), &locker);
    locker.set_deferred(true);
    lock.begin_unlock();
    lock.lock_now();
    lock.begin_unlock();
    locker.complete_pending(Err(AuthFailure::Unavailable));
    assert_eq!(lock.state(), LockState::Unlocking);
    assert_eq!(lock.last_failure(), None);
    locker.complete_pending(Ok(()));
    assert!(lock.should_draw_content());
}

#[test]
fn backgrounding_cancels_unlock_without_waiting_for_a_callback() {
    let locker = FakeLocker::new(FACE);
    let (lock, ..) = app(enabled(BackgroundGrace::Never), &locker);
    locker.set_deferred(true);
    lock.begin_unlock();
    lock.handle_lifecycle(LifecycleEvent::WillResignActive);
    lock.handle_lifecycle(LifecycleEvent::DidEnterBackground);
    assert_eq!(lock.state(), LockState::Locked);
    assert_eq!(locker.cancellation_count(), 1);
    assert!(lock.shield_should_cover());
    lock.retry_unlock();
    assert_eq!(locker.pending_count(), 1, "no prompt while backgrounded");
    locker.complete_pending(Ok(()));
    assert!(
        !lock.should_draw_content(),
        "late background success is ignored"
    );
    lock.handle_lifecycle(LifecycleEvent::DidBecomeActive);
    lock.begin_automatic_unlock();
    locker.complete_pending(Ok(()));
    assert!(lock.should_draw_content());
}

#[test]
fn backgrounding_cancels_disable_and_late_success_cannot_turn_protection_off() {
    let locker = FakeLocker::new(FACE);
    let (lock, ..) = app(enabled(BackgroundGrace::Always), &locker);
    locker.push_outcome(Ok(()));
    lock.begin_unlock();
    locker.set_deferred(true);
    assert_eq!(
        lock.apply_settings(LockSettings::default()),
        Err(SettingsError::AuthenticationStarted)
    );
    lock.handle_lifecycle(LifecycleEvent::WillResignActive);
    lock.handle_lifecycle(LifecycleEvent::DidEnterBackground);
    assert_eq!(locker.cancellation_count(), 1);
    locker.complete_pending(Ok(()));
    assert!(lock.settings().enabled);
    lock.handle_lifecycle(LifecycleEvent::DidBecomeActive);
    assert_eq!(lock.state(), LockState::Locked);
}

#[test]
fn changed_settings_retire_an_old_disable_request() {
    let locker = FakeLocker::new(FACE);
    let (lock, _clock, _repaints, dir) = app(enabled(BackgroundGrace::Never), &locker);
    locker.push_outcome(Ok(()));
    lock.begin_unlock();
    locker.set_deferred(true);
    assert_eq!(
        lock.apply_settings(LockSettings::default()),
        Err(SettingsError::AuthenticationStarted)
    );
    let next = enabled(BackgroundGrace::Minutes5);
    lock.apply_settings(next).unwrap();
    locker.complete_pending(Ok(()));
    assert_eq!(lock.settings(), next);
    assert_eq!(
        SettingsStore::new(dir.path().join("lock.json")).load().0,
        next
    );
}

#[test]
fn dropping_the_last_lock_owner_cancels_the_platform_request() {
    let locker = FakeLocker::new(FACE);
    let (lock, ..) = app(enabled(BackgroundGrace::Never), &locker);
    locker.set_deferred(true);
    lock.begin_unlock();
    drop(lock);
    assert_eq!(
        locker.cancellation_count(),
        1,
        "no request/context owner cycle"
    );
    locker.complete_pending(Ok(()));
}

#[test]
fn synchronous_cancellation_reply_can_reenter_without_deadlocking() {
    struct CancelReplies(Arc<AtomicUsize>);
    impl Locker for CancelReplies {
        fn availability(&self) -> Availability {
            FACE
        }

        fn authenticate(
            &self,
            _: AuthMethod,
            _: &str,
            done: Box<dyn FnOnce(AuthOutcome) + Send>,
        ) -> Authentication {
            let cancelled = self.0.clone();
            Authentication::new(move || {
                cancelled.fetch_add(1, Ordering::SeqCst);
                done(Err(AuthFailure::Cancelled));
            })
        }
    }
    let cancelled = Arc::new(AtomicUsize::new(0));
    let lock = AppLock::new(
        None,
        Arc::new(CancelReplies(cancelled.clone())),
        Arc::new(ManualClock::new()),
        || {},
    );
    lock.apply_settings(enabled(BackgroundGrace::Never))
        .unwrap();
    lock.lock_now();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        lock.begin_unlock();
        lock.begin_passcode_unlock();
        lock.lock_now();
        let state = lock.state();
        lock.begin_unlock();
        drop(lock);
        sender.send(state).unwrap();
    });
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
        LockState::Locked
    );
    assert_eq!(cancelled.load(Ordering::SeqCst), 3);
}

#[test]
fn failed_authenticated_settings_write_is_visible_and_keeps_protection() {
    let locker = FakeLocker::new(FACE);
    let (lock, _clock, _repaints, dir) = app(enabled(BackgroundGrace::Never), &locker);
    locker.push_outcome(Ok(()));
    lock.begin_unlock();
    locker.set_deferred(true);
    let path = dir.path().join("lock.json");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(
        lock.apply_settings(LockSettings::default()),
        Err(SettingsError::AuthenticationStarted)
    );
    locker.complete_pending(Ok(()));
    assert!(lock.settings().enabled);
    assert_eq!(
        lock.settings_error(),
        Some("Could not save the lock settings.")
    );
}

#[test]
fn shield_requests_the_remaining_resume_frame_without_user_input() {
    let locker = FakeLocker::new(FACE);
    let (lock, _clock, repaints, _dir) = app(enabled(BackgroundGrace::Never), &locker);
    lock.note_frame_presented();
    assert_eq!(repaints.load(Ordering::SeqCst), 1);
    assert!(lock.shield_should_cover());
    lock.note_frame_presented();
    assert!(!lock.shield_should_cover());
    assert!(
        !lock.should_draw_content(),
        "only the opaque native cover retires"
    );
}
