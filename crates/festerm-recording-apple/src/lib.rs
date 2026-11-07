//! AVFoundation video encoding, isolated from the terminal and session owners.

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::{AppleEncoder, VideoError};
