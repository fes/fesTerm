//! Isolated JavaScript source snapshots, bounded wire protocol, and worker supervision.
//!
//! No application/session authority is held here. Every host operation must be
//! authorized by the caller's broker, including observation and result release.

mod engine;
pub mod platform;
pub mod protocol;
pub mod snapshot;
mod supervisor;
mod worker;

pub use supervisor::{cleanup_failed, AdmissionPermit, Event, Supervisor, WorkerProgram};
pub use worker::{worker_entry, worker_stdio};

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
    #[error("automation worker deadline exceeded")]
    Deadline,
    #[error("automation worker cleanup failed")]
    Cleanup,
    #[error("automation worker failed: {0}")]
    WorkerFailed(&'static str),
}
