//! Isolated JavaScript source snapshots, bounded wire protocol, and worker supervision.
//!
//! No application/session authority is held here. Every host operation must be
//! authorized by the caller's broker, including observation and result release.

#[cfg(not(target_os = "ios"))]
mod engine;
pub mod platform;
pub mod protocol;
pub mod snapshot;
mod stdio;
mod supervisor;
#[cfg(windows)]
mod windows;
#[cfg(not(target_os = "ios"))]
mod worker;

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
pub const STACK_BYTES: usize = 1024 * 1024;
pub const PROCESS_BYTES: u64 = 128 * 1024 * 1024;
pub const CPU_SECONDS: u64 = 5;
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
