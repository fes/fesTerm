use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

/// A monotonic clock that keeps counting while the device sleeps.
///
/// Values are only meaningful relative to each other within one boot, which is
/// exactly what the background grace timer needs. Wall-clock time is never
/// used, so changing the date cannot extend or shorten the grace period.
pub trait MonotonicClock: Send + Sync {
    fn now(&self) -> Duration;
}

/// `CLOCK_MONOTONIC`, which on Darwin includes time spent asleep
/// (`std::time::Instant` uses `CLOCK_UPTIME_RAW`, which does not).
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

#[cfg(unix)]
impl MonotonicClock for SystemClock {
    fn now(&self) -> Duration {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a valid, exclusively borrowed timespec.
        let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        if rc != 0 {
            return Duration::ZERO;
        }
        Duration::new(
            ts.tv_sec.max(0) as u64,
            ts.tv_nsec.clamp(0, 999_999_999) as u32,
        )
    }
}

#[cfg(not(unix))]
impl MonotonicClock for SystemClock {
    fn now(&self) -> Duration {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed()
    }
}

/// Manually advanced clock for tests and fakes.
#[derive(Debug, Default)]
pub struct ManualClock {
    millis: AtomicU64,
}

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance(&self, by: Duration) {
        self.millis
            .fetch_add(by.as_millis() as u64, Ordering::SeqCst);
    }

    pub fn set(&self, to: Duration) {
        self.millis.store(to.as_millis() as u64, Ordering::SeqCst);
    }
}

impl MonotonicClock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.millis.load(Ordering::SeqCst))
    }
}
