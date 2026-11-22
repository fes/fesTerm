//! Optional GUI upload reporting. This interface grants no remote-file authority.
use std::{future::Future, pin::Pin};

pub type UploadRecoveryFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, UploadRecoveryError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadRecoveryError {
    Unavailable,
    Capacity,
    Changed,
    Busy,
}
impl UploadRecoveryError {
    pub const fn message(self) -> &'static str {
        match self {
            Self::Unavailable => "The interrupted-upload report could not be saved.",
            Self::Capacity => "Review interrupted uploads before starting another upload; the report limit is full.",
            Self::Busy => "Another upload report is being updated. Try again after it settles.",
            Self::Changed => "The interrupted-upload report changed; review it before starting another upload.",
        }
    }
}

/// Local report token. Dropping it must not perform filesystem or network I/O.
pub trait UploadRecoveryRecord: Send + Sync {
    fn cancelled(&self) -> bool {
        false
    }
    fn before_publication<'a>(&'a mut self, destination: &'a str) -> UploadRecoveryFuture<'a, ()>;
    fn retire(&mut self) -> UploadRecoveryFuture<'_, ()>;
}

/// Installed only by the GUI owner of the dedicated SFTP transfer session.
/// A failed begin prevents OPEN; retirement failure never changes remote success.
pub trait UploadRecoverySink: Send + Sync {
    fn cancelled(&self) -> bool {
        false
    }
    fn begin<'a>(
        &'a self,
        temporary: &'a str,
    ) -> UploadRecoveryFuture<'a, Box<dyn UploadRecoveryRecord>>;
    fn report_update_failure(&self, temporary: &str, destination: Option<&str>);
}
