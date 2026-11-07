//! Local recording primitives. The host owns consent, private storage,
//! lifecycle cuts, export, and presentation.

mod advanced_pacing;
mod artifact;
mod capture;
mod naming;
mod playback;

pub use artifact::{ArtifactError, ArtifactEvent, ArtifactReader, FinishReason, StreamId};
pub use capture::{CaptureError, Completion, Recorder};
pub use naming::{LocalStart, NameError, NameTemplates, RecordingId};
pub use playback::{
    IdleCompression, LongOutputLimit, OutputRevealRate, Pacing, Playback, PlaybackError,
    PlaybackFrame, PlaybackPlan, PlaybackSpeed, TypingSpeed,
};

/// Maximum number of streams in one synchronized recording.
pub const MAX_STREAMS: usize = 16;
/// Maximum elapsed capture time.
pub const MAX_DURATION: std::time::Duration = std::time::Duration::from_secs(2 * 60 * 60);
/// Maximum on-disk source size, including framing.
pub const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
/// Maximum output bytes in a single mutation event.
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Maximum queued capture memory, including checkpoints.
pub const MAX_QUEUE_BYTES: usize = 16 * 1024 * 1024;
