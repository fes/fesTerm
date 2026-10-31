use festerm_ios_lock::{
    AppLock, AuthFailure, Availability, BackgroundGrace, BiometryKind, FakeLocker, LifecycleEvent,
    LoadStatus, LockPolicy, LockSettings, LockState, ManualClock, SettingsError, SettingsStore,
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
    assert!(b >= a && a > Duration::ZERO);
}
