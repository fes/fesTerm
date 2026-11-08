//! AVFoundation video encoding, isolated from the terminal and session owners.

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple;

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod storage;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::{AppleEncoder, VideoError};

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use storage::{exclude_directory_from_backup, BackupError};
