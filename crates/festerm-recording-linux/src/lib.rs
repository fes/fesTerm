//! Optional system GStreamer encoder. No codec is bundled.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::{LinuxEncoder, VideoError};
