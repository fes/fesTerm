use std::{
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, Weak,
    },
};

pub(crate) const PROGRAM_MEMORY_BYTES: u64 = festerm_automation_js::PROCESS_BYTES;
pub(crate) const DEFAULT_PROGRAM_COUNT: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct JavaScriptProgramLimits {
    pub(crate) programs: usize,
    pub(crate) memory_bytes: u64,
}

impl Default for JavaScriptProgramLimits {
    fn default() -> Self {
        Self {
            programs: DEFAULT_PROGRAM_COUNT,
            memory_bytes: PROGRAM_MEMORY_BYTES * DEFAULT_PROGRAM_COUNT as u64,
        }
    }
}

#[derive(Clone)]
pub(crate) struct JavaScriptProgramCoordinator {
    shared: Arc<CoordinatorShared>,
}

struct CoordinatorShared {
    limits: JavaScriptProgramLimits,
    state: Mutex<CoordinatorState>,
}

#[derive(Default)]
struct CoordinatorState {
    programs: usize,
    memory_bytes: u64,
    admission_open: bool,
    controls: Vec<Weak<ProgramControl>>,
}

#[derive(Default)]
struct ProgramControl {
    cancelled: AtomicBool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct JavaScriptProgramQuotaError {
    pub(crate) programs: usize,
    pub(crate) memory_bytes: u64,
}

impl fmt::Display for JavaScriptProgramQuotaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "JavaScript program limit reached ({} programs, {} MiB accounted). \
             Close a JavaScript program before starting another.",
            self.programs,
            self.memory_bytes / (1024 * 1024)
        )
    }
}

pub(crate) struct JavaScriptProgramLease {
    coordinator: Weak<CoordinatorShared>,
    control: Arc<ProgramControl>,
    memory_bytes: u64,
    released: AtomicBool,
}

impl JavaScriptProgramLease {
    pub(crate) fn is_live(&self) -> bool {
        !self.control.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn cancel(&self) {
        self.control.cancelled.store(true, Ordering::Release);
    }

    pub(crate) fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(coordinator) = self.coordinator.upgrade() else {
            return;
        };
        let mut state = coordinator
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.programs = state.programs.saturating_sub(1);
        state.memory_bytes = state.memory_bytes.saturating_sub(self.memory_bytes);
    }
}

impl Drop for JavaScriptProgramLease {
    fn drop(&mut self) {
        self.release();
    }
}

impl Default for JavaScriptProgramCoordinator {
    fn default() -> Self {
        Self::new(JavaScriptProgramLimits::default())
    }
}

impl JavaScriptProgramCoordinator {
    pub(crate) fn new(limits: JavaScriptProgramLimits) -> Self {
        assert!(limits.programs > 0);
        assert!(limits.memory_bytes > 0);
        Self {
            shared: Arc::new(CoordinatorShared {
                limits,
                state: Mutex::new(CoordinatorState {
                    admission_open: true,
                    ..CoordinatorState::default()
                }),
            }),
        }
    }

    pub(crate) fn try_admit(&self) -> Result<JavaScriptProgramLease, JavaScriptProgramQuotaError> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.controls.retain(|control| control.strong_count() > 0);
        if !state.admission_open
            || state.programs >= self.shared.limits.programs
            || state
                .memory_bytes
                .checked_add(PROGRAM_MEMORY_BYTES)
                .is_none_or(|total| total > self.shared.limits.memory_bytes)
        {
            return Err(JavaScriptProgramQuotaError {
                programs: self.shared.limits.programs,
                memory_bytes: self.shared.limits.memory_bytes,
            });
        }
        state.programs += 1;
        state.memory_bytes += PROGRAM_MEMORY_BYTES;
        let control = Arc::new(ProgramControl::default());
        state.controls.push(Arc::downgrade(&control));
        Ok(JavaScriptProgramLease {
            coordinator: Arc::downgrade(&self.shared),
            control,
            memory_bytes: PROGRAM_MEMORY_BYTES,
            released: AtomicBool::new(false),
        })
    }

    pub(crate) fn active_programs(&self) -> usize {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .programs
    }

    #[cfg(test)]
    pub(crate) fn accounted_memory_bytes(&self) -> u64 {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .memory_bytes
    }

    pub(crate) fn begin_quit(&self) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .admission_open = false;
    }

    #[cfg(test)]
    pub(crate) fn cut_privacy(&self) {
        let controls: Vec<_> = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .controls
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for control in controls {
            control.cancelled.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_quota_counts_pending_live_and_closing_programs() {
        let coordinator = JavaScriptProgramCoordinator::new(JavaScriptProgramLimits {
            programs: 2,
            memory_bytes: PROGRAM_MEMORY_BYTES * 2,
        });
        let first = coordinator.try_admit().unwrap();
        let second = coordinator.try_admit().unwrap();
        assert_eq!(coordinator.active_programs(), 2);
        assert!(coordinator.try_admit().is_err());
        assert_eq!(
            coordinator.accounted_memory_bytes(),
            PROGRAM_MEMORY_BYTES * 2
        );
        drop(first);
        let replacement = coordinator.try_admit().unwrap();
        assert_eq!(coordinator.active_programs(), 2);
        drop(second);
        drop(replacement);
        assert_eq!(coordinator.active_programs(), 0);
    }

    #[test]
    fn quit_and_privacy_cut_admission_and_reuse_only_after_release() {
        let coordinator = JavaScriptProgramCoordinator::default();
        let first = coordinator.try_admit().unwrap();
        coordinator.begin_quit();
        assert!(coordinator.try_admit().is_err());
        assert!(first.is_live());
        coordinator.cut_privacy();
        assert!(!first.is_live());
        drop(first);
        assert_eq!(coordinator.active_programs(), 0);
        assert_eq!(coordinator.accounted_memory_bytes(), 0);
    }
}
