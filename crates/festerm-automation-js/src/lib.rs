//! Isolated JavaScript source snapshots, bounded wire protocol, and worker supervision.
//!
//! No application/session authority is held here. Every host operation must be
//! authorized by the caller's broker, including observation and result release.

use std::time::Duration;

#[cfg(not(target_os = "ios"))]
mod engine;
pub mod platform;
#[cfg(feature = "javascript-program-dev")]
pub mod program;
#[cfg(all(feature = "javascript-program-dev", not(target_os = "ios")))]
mod program_supervisor;
#[cfg(all(feature = "javascript-program-dev", not(target_os = "ios")))]
mod program_worker;
pub mod protocol;
#[cfg(all(feature = "javascript-program-dev", not(target_os = "ios")))]
mod quickjs_program_runtime;
pub mod snapshot;
mod stdio;
mod supervisor;
#[cfg(windows)]
mod windows;
#[cfg(not(target_os = "ios"))]
mod worker;

#[cfg(feature = "javascript-program-dev")]
pub use program::{JavaScriptProgramLaunch, ProgramDeliveryFence, ProgramDeliveryPermit};
#[cfg(all(feature = "javascript-program-dev", not(target_os = "ios")))]
pub use program_supervisor::ProgramSupervisor;
#[cfg(all(feature = "javascript-program-dev", not(target_os = "ios")))]
pub use quickjs_program_runtime::QuickJsProgramRuntime;
#[cfg(all(feature = "javascript-program-dev", not(target_os = "ios")))]
pub use quickjs_program_runtime::QuickJsProgramRuntimeError;
#[cfg(all(feature = "javascript-program-dev", not(target_os = "ios")))]
pub use stdio::program_worker_stdio;
pub use stdio::worker_stdio;
pub use supervisor::{
    cleanup_failed, event_wake_channel, AdmissionPermit, DisclosureGuard, DisclosurePermit, Event,
    EventWake, Supervisor, WorkerProgram,
};
#[cfg(not(target_os = "ios"))]
pub use worker::worker_entry;

/// iOS has no qualified worker transport; do not construct an interpreter.
#[cfg(target_os = "ios")]
pub fn worker_entry(_input: impl std::io::Read, _output: impl std::io::Write) -> Result<(), Error> {
    platform::confine()
}

#[cfg(all(test, target_os = "ios"))]
mod ios_tests {
    use super::*;

    #[test]
    fn ios_worker_refuses_before_reading_source_or_writing() {
        let mut input = std::io::Cursor::new(b"untrusted source");
        let mut output = Vec::new();
        assert!(matches!(
            worker_entry(&mut input, &mut output),
            Err(Error::Unavailable(_))
        ));
        assert_eq!(input.position(), 0);
        assert!(output.is_empty());
    }
}

pub const HEAP_BYTES: usize = 64 * 1024 * 1024;
/// QuickJS stack ceiling. Windows main threads reserve only 1 MiB, so keep
/// the engine's own check well inside it rather than overflowing natively.
#[cfg(not(windows))]
pub const STACK_BYTES: usize = 1024 * 1024;
#[cfg(windows)]
pub const STACK_BYTES: usize = 512 * 1024;
pub const PROCESS_BYTES: u64 = 128 * 1024 * 1024;
pub const CPU_SECONDS: u64 = 5;
/// Last-resort native process CPU ceiling for the interactive program mode.
/// The supervisor and engine normally terminate much earlier through the
/// rolling and per-turn budgets; this exists so confinement always remains
/// finite even if their accounting fails.
pub const PROGRAM_HARD_CPU_SECONDS: u64 = 3_600;
pub const PROGRAM_CPU_WINDOW: Duration = Duration::from_secs(1);
pub const PROGRAM_CPU_WINDOW_LIMIT: Duration = Duration::from_millis(250);
pub const PROGRAM_TURN_TIMEOUT: Duration = Duration::from_secs(1);
pub const PROGRAM_TURN_CPU_LIMIT: Duration = Duration::from_millis(200);
pub const MAX_RUN_MS: u64 = 300_000;

/// Content-free errors, safe for ordinary diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("automation worker unavailable: {0}")]
    Unavailable(&'static str),
    #[error("invalid automation source snapshot")]
    Snapshot,
    #[error("automation protocol violation")]
    Protocol,
    #[error("automation worker I/O failure")]
    Io(#[from] std::io::Error),
    #[error("automation worker exited unexpectedly: {0}")]
    WorkerExited(String),
    #[error("automation worker deadline exceeded")]
    Deadline,
    #[error("automation disclosure is no longer authorized")]
    DisclosureRevoked,
    #[error("automation worker cleanup failed")]
    Cleanup,
    #[error("automation worker failed: {0}")]
    WorkerFailed(&'static str),
}
