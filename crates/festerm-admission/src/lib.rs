//! Dependency-neutral automation admission and attachment freshness boundary.
//!
//! This boundary grants no authority. The application must retain the guard
//! through the effect or disclosure it protects.

use std::{
    marker::PhantomData,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex, MutexGuard, TryLockError,
    },
};

static AUTOMATION_ADMISSION: Mutex<()> = Mutex::new(());

/// An opaque, thread-confined guard for the process-wide admission boundary.
/// It cannot be constructed from a different mutex or shared with another
/// thread, even by reference.
///
/// The caller must not re-enter or interleave transitions of protected state
/// between a freshness check and its associated admission or disclosure.
/// Holding the guard does not prevent such interleaving on the same thread.
///
/// ```compile_fail
/// let other = std::sync::Mutex::new(());
/// let _: festerm_admission::AutomationAdmissionGuard = other.lock().unwrap();
/// ```
///
/// ```compile_fail
/// use festerm_admission::automation_admission_lock;
///
/// let guard = automation_admission_lock().unwrap();
/// std::thread::scope(|scope| {
///     let borrowed = &guard;
///     scope.spawn(move || {
///         std::hint::black_box(borrowed);
///     });
/// });
/// ```
///
/// ```compile_fail
/// use festerm_admission::automation_admission_lock;
///
/// let guard = automation_admission_lock().unwrap();
/// std::thread::scope(|scope| {
///     scope.spawn(move || {
///         std::hint::black_box(guard);
///     });
/// });
/// ```
#[derive(Debug)]
pub struct AutomationAdmissionGuard {
    _guard: MutexGuard<'static, ()>,
    _thread_confined: PhantomData<Rc<()>>,
}

impl AutomationAdmissionGuard {
    fn new(guard: MutexGuard<'static, ()>) -> Self {
        Self {
            _guard: guard,
            _thread_confined: PhantomData,
        }
    }
}

/// Serializes automation admission and disclosure with backend transitions.
///
/// Every session in this process shares the same lock.
/// A poisoned lock is never recovered: automation must refuse admission and
/// disclosure for the rest of the process. Do not call [`AutomationEpoch`]
/// methods that acquire this lock, or backend operations that can transition
/// an epoch, while holding the returned guard. The explicitly guard-taking
/// epoch methods operate within the existing boundary instead.
pub fn automation_admission_lock() -> Option<AutomationAdmissionGuard> {
    AUTOMATION_ADMISSION
        .lock()
        .ok()
        .map(AutomationAdmissionGuard::new)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutomationAdmissionTryError {
    Busy,
    Poisoned,
}

/// Attempts the same boundary without delaying a UI caller or independent
/// watchdog. Poison is permanent; contention may be retried.
pub fn try_automation_admission_lock(
) -> Result<AutomationAdmissionGuard, AutomationAdmissionTryError> {
    AUTOMATION_ADMISSION
        .try_lock()
        .map(AutomationAdmissionGuard::new)
        .map_err(|error| match error {
            TryLockError::WouldBlock => AutomationAdmissionTryError::Busy,
            TryLockError::Poisoned(_) => AutomationAdmissionTryError::Poisoned,
        })
}

/// A non-reusable automation identity for one backend transport attachment.
///
/// Backends transition this epoch before publishing lifecycle changes or
/// replacing a transport. Observation must capture an epoch while the
/// attachment is running and check it again under
/// [`automation_admission_lock`] at each disclosure boundary. Generations
/// are meaningful only with the same epoch, not across different backends.
#[derive(Debug, Default)]
pub struct AutomationEpoch {
    generation: AtomicU64,
    running: AtomicBool,
    failed: AtomicBool,
}

impl AutomationEpoch {
    /// Creates an epoch with no running attachment.
    pub const fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            running: AtomicBool::new(false),
            failed: AtomicBool::new(false),
        }
    }

    /// Invalidates the previous attachment and advances its generation.
    ///
    /// `running` may be true only once the transport is ready. Exhaustion or
    /// a poisoned admission lock permanently disables this epoch. This
    /// method acquires the admission lock and must not be called with it held.
    pub fn transition(&self, running: bool) {
        let Some(guard) = automation_admission_lock() else {
            self.fail_closed();
            return;
        };
        self.transition_under_admission(&guard, running);
    }

    /// Advances this epoch while the caller retains the admission boundary.
    pub fn transition_under_admission(&self, _guard: &AutomationAdmissionGuard, running: bool) {
        self.running.store(false, Ordering::Release);
        if self.failed.load(Ordering::Acquire) {
            return;
        }
        let Some(next) = self.generation.load(Ordering::Acquire).checked_add(1) else {
            self.fail_closed();
            return;
        };
        self.generation.store(next, Ordering::Release);
        self.running.store(running, Ordering::Release);
    }

    /// Captures the current generation only while the attachment is running.
    ///
    /// This method acquires the admission lock and must not be called with it
    /// held. A returned generation must be rechecked at disclosure time.
    pub fn capture(&self) -> Option<u64> {
        let Some(guard) = automation_admission_lock() else {
            self.fail_closed();
            return None;
        };
        self.capture_under_admission(&guard)
    }

    /// Captures a running generation while the caller retains the boundary.
    pub fn capture_under_admission(&self, _guard: &AutomationAdmissionGuard) -> Option<u64> {
        if self.failed.load(Ordering::Acquire) || !self.running.load(Ordering::Acquire) {
            return None;
        }
        Some(self.generation.load(Ordering::Acquire))
    }

    /// Checks a captured generation without taking the admission lock.
    ///
    /// The caller must hold a guard returned by [`automation_admission_lock`]
    /// throughout this check and the admission or disclosure it protects.
    pub fn is_current(&self, generation: u64) -> bool {
        !self.failed.load(Ordering::Acquire)
            && self.running.load(Ordering::Acquire)
            && self.generation.load(Ordering::Acquire) == generation
    }

    /// Checks a captured generation while retaining the admission boundary.
    pub fn is_current_under_admission(
        &self,
        _guard: &AutomationAdmissionGuard,
        generation: u64,
    ) -> bool {
        self.is_current(generation)
    }

    fn fail_closed(&self) {
        self.failed.store(true, Ordering::Release);
        self.running.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_epoch_exhaustion_is_permanently_fail_closed() {
        let epoch = AutomationEpoch::new();
        {
            let _guard = automation_admission_lock().unwrap();
            epoch.generation.store(u64::MAX - 1, Ordering::Release);
        }
        epoch.transition(true);
        assert_eq!(epoch.capture(), Some(u64::MAX));
        {
            let _guard = automation_admission_lock().unwrap();
            assert!(epoch.is_current(u64::MAX));
        }

        epoch.transition(true);
        assert_eq!(epoch.capture(), None);
        {
            let _guard = automation_admission_lock().unwrap();
            assert!(!epoch.is_current(u64::MAX));
            epoch.generation.store(0, Ordering::Release);
        }
        epoch.transition(true);
        assert_eq!(epoch.capture(), None);
    }
}
