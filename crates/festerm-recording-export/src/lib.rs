//! Offline export into caller-owned private staging.
//!
//! The host owns destination authorization and source retention. Encoding
//! stages videos privately; supported publishers expose only complete sets.

use std::{
    fmt,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use festerm_recording::{Pacing, Playback, PlaybackPlan, StreamId};
use festerm_ui_egui::{RecordingFrameSize, RecordingRenderer};
use sha2::{Digest, Sha256};

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod publish_unix;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod source_unix;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use publish_unix::{delete_source_after_publication, publish_to_directory, PublishedRecording};

/// A bounded sequential video encoder. Files must be created without replacing
/// an existing path; an unfinished encoder must release its native resources.
pub trait VideoEncoder {
    type Error: fmt::Display;

    fn append(&mut self, rgba: &[u8]) -> Result<(), Self::Error>;
    fn finish(&mut self) -> Result<(), Self::Error>;
}

/// Creates encoders in an app-private staging directory.
pub trait EncoderFactory {
    type Encoder: VideoEncoder;
    type Error: fmt::Display;

    fn create(
        &mut self,
        path: &Path,
        size: RecordingFrameSize,
    ) -> Result<Self::Encoder, Self::Error>;
}

/// Failure to plan or render the export, including cleanup failures.
#[derive(Debug)]
pub struct ExportError(String);

impl ExportError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ExportError {}

/// One complete video in private staging, not yet published.
#[derive(Debug)]
pub struct StagedVideo {
    stream: StreamId,
    path: PathBuf,
    fingerprint: [u8; 32],
    length: u64,
}

impl StagedVideo {
    pub const fn stream(&self) -> StreamId {
        self.stream
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// A complete synchronized set. The host must publish all videos before
/// applying its source-deletion policy.
#[derive(Debug)]
pub struct StagedRecording {
    videos: Vec<StagedVideo>,
    frames_per_video: u64,
    duration: Duration,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    directory: File,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    source: source_unix::SourceBinding,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    source_fingerprint: [u8; 32],
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    stage_id: uuid::Uuid,
}

impl StagedRecording {
    pub fn videos(&self) -> &[StagedVideo] {
        &self.videos
    }

    pub const fn frames_per_video(&self) -> u64 {
        self.frames_per_video
    }

    pub const fn duration(&self) -> Duration {
        self.duration
    }
}

/// Frame progress across all streams in the set.
#[derive(Clone, Copy, Debug)]
pub struct ExportProgress {
    pub stream: StreamId,
    pub frames_done: u64,
    pub total_frames: u64,
}

/// Rendering and shared pacing selected for one export.
#[derive(Clone, Copy, Debug)]
pub struct ExportOptions {
    pub pacing: Pacing,
    pub size: RecordingFrameSize,
    pub font_size: f32,
}

/// Renders all streams with a common schedule into an empty private directory.
///
/// The caller must exclusively own `staging_directory` and ensure it is
/// app-private with the platform's recommended backup handling. `source` stays
/// untouched even on cancellation or failure. A successful result is not yet a
/// published export.
pub fn stage_recording<F: EncoderFactory>(
    source: &Path,
    staging_directory: &Path,
    options: ExportOptions,
    factory: &mut F,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(ExportProgress),
) -> Result<StagedRecording, ExportError> {
    if !staging_directory.is_dir() {
        return Err(ExportError::new("private video staging is unavailable"));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let staging_handle = File::open(staging_directory)
        .map_err(|error| ExportError::new(format!("could not open video staging: {error}")))?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    source_unix::require_private(&staging_handle, true, "video staging directory")?;
    let mut existing = fs::read_dir(staging_directory)
        .map_err(|error| ExportError::new(format!("could not inspect video staging: {error}")))?;
    if existing
        .next()
        .transpose()
        .map_err(|error| ExportError::new(format!("could not inspect video staging: {error}")))?
        .is_some()
    {
        return Err(ExportError::new(
            "private video staging must be an empty, exclusively owned directory",
        ));
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(ExportError::new("recording export was cancelled"));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let (source_binding, source_file) = source_unix::SourceBinding::open(source)?;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let source_file = File::open(source)
        .map_err(|error| ExportError::new(format!("could not open recording source: {error}")))?;
    let plan = PlaybackPlan::inspect(source_file, options.pacing)
        .map_err(|error| ExportError::new(format!("could not plan recording export: {error}")))?;
    let total_frames = plan
        .frame_count()
        .checked_mul(plan.stream_count() as u64)
        .ok_or_else(|| ExportError::new("recording export frame count overflows"))?;
    let mut renderer =
        RecordingRenderer::new(options.size, options.font_size).map_err(|error| {
            ExportError::new(format!("could not create recording renderer: {error}"))
        })?;
    let mut paths = Vec::with_capacity(plan.stream_count());
    let result = (|| {
        let mut frames_done = 0;
        for index in 1..=plan.stream_count() {
            let stream = StreamId::from_index(index)
                .expect("validated recording stream count is within the limit");
            let path = staging_directory.join(format!("stream-{index:02}.mp4"));
            paths.push(StagedVideo {
                stream,
                path,
                fingerprint: [0; 32],
                length: 0,
            });
            let path = &paths.last().expect("video path was added").path;
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let source_file = source_binding.open_current()?;
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let source_file = File::open(source).map_err(|error| {
                ExportError::new(format!("could not reopen recording source: {error}"))
            })?;
            let mut playback =
                Playback::new(source_file, stream, plan.clone()).map_err(|error| {
                    ExportError::new(format!("could not replay recording: {error}"))
                })?;
            let mut encoder = factory.create(path, options.size).map_err(|error| {
                ExportError::new(format!("could not create MP4 encoder: {error}"))
            })?;
            while let Some(frame) = playback
                .next_frame()
                .map_err(|error| ExportError::new(format!("recording replay failed: {error}")))?
            {
                if cancelled.load(Ordering::Acquire) {
                    return Err(ExportError::new("recording export was cancelled"));
                }
                if frame.changed {
                    renderer.render(frame.terminal).map_err(|error| {
                        ExportError::new(format!("recording render failed: {error}"))
                    })?;
                }
                encoder
                    .append(renderer.pixels())
                    .map_err(|error| ExportError::new(format!("MP4 encoding failed: {error}")))?;
                frames_done += 1;
                progress(ExportProgress {
                    stream,
                    frames_done,
                    total_frames,
                });
            }
            if cancelled.load(Ordering::Acquire) {
                return Err(ExportError::new("recording export was cancelled"));
            }
            encoder
                .finish()
                .map_err(|error| ExportError::new(format!("MP4 finalization failed: {error}")))?;
            if cancelled.load(Ordering::Acquire) {
                return Err(ExportError::new("recording export was cancelled"));
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let file = festerm_unix_security::open_file_nofollow(
                &staging_handle,
                Path::new(path.file_name().expect("staged video has a filename")),
            );
            #[cfg(target_os = "windows")]
            let file = fs::OpenOptions::new().read(true).write(true).open(path);
            #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
            let file = File::open(path);
            let mut file = file
                .map_err(|error| ExportError::new(format!("could not open staged MP4: {error}")))?;
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            source_unix::make_staged_file_private(&staging_handle, &file)?;
            file.sync_all()
                .map_err(|error| ExportError::new(format!("could not sync staged MP4: {error}")))?;
            let (fingerprint, length) = fingerprint(&mut file).map_err(|error| {
                ExportError::new(format!("could not verify staged MP4: {error}"))
            })?;
            let video = paths.last_mut().expect("video path was added");
            video.fingerprint = fingerprint;
            video.length = length;
        }
        Ok(())
    })();

    if let Err(error) = result {
        let mut cleanup_errors = Vec::new();
        for video in &paths {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let cleanup = festerm_unix_security::remove_private_staging_file(
                &staging_handle,
                Path::new(video.path.file_name().expect("staged video has a filename")),
            );
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let cleanup = fs::remove_file(&video.path);
            if let Err(cleanup) = cleanup {
                if cleanup.kind() != std::io::ErrorKind::NotFound {
                    cleanup_errors.push(format!("{}: {cleanup}", video.path.display()));
                }
            }
        }
        if !cleanup_errors.is_empty() {
            return Err(ExportError::new(format!(
                "{error}; private staged videos could not be removed: {}",
                cleanup_errors.join("; ")
            )));
        }
        return Err(error);
    }

    Ok(StagedRecording {
        videos: paths,
        frames_per_video: plan.frame_count(),
        duration: plan.duration(),
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        directory: staging_handle,
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        source: source_binding,
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        source_fingerprint: plan.fingerprint(),
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        stage_id: uuid::Uuid::new_v4(),
    })
}

fn fingerprint(source: &mut impl Read) -> io::Result<([u8; 32], u64)> {
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            return Ok((hash.finalize().into(), bytes));
        }
        hash.update(&buffer[..count]);
        bytes = bytes
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("file size overflows"))?;
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple {
    use std::path::Path;

    use festerm_recording_apple::{AppleEncoder, VideoError};
    use festerm_ui_egui::RecordingFrameSize;

    use crate::{EncoderFactory, VideoEncoder};

    impl VideoEncoder for AppleEncoder {
        type Error = VideoError;

        fn append(&mut self, rgba: &[u8]) -> Result<(), Self::Error> {
            AppleEncoder::append(self, rgba)
        }

        fn finish(&mut self) -> Result<(), Self::Error> {
            AppleEncoder::finish(self)
        }
    }

    /// Uses AVFoundation/VideoToolbox without an external media process.
    #[derive(Default)]
    pub struct AppleEncoderFactory;

    impl EncoderFactory for AppleEncoderFactory {
        type Encoder = AppleEncoder;
        type Error = VideoError;

        fn create(
            &mut self,
            path: &Path,
            size: RecordingFrameSize,
        ) -> Result<Self::Encoder, Self::Error> {
            AppleEncoder::new(path, size.width() as u32, size.height() as u32)
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::AppleEncoderFactory;

#[cfg(target_os = "linux")]
mod linux {
    use std::path::Path;

    use festerm_recording_linux::{LinuxEncoder, VideoError};
    use festerm_ui_egui::RecordingFrameSize;

    use crate::{EncoderFactory, VideoEncoder};

    impl VideoEncoder for LinuxEncoder {
        type Error = VideoError;

        fn append(&mut self, rgba: &[u8]) -> Result<(), Self::Error> {
            LinuxEncoder::append(self, rgba)
        }

        fn finish(&mut self) -> Result<(), Self::Error> {
            LinuxEncoder::finish(self)
        }
    }

    /// Loads installed GStreamer libraries and plugins only when exporting.
    #[derive(Default)]
    pub struct LinuxEncoderFactory;

    impl EncoderFactory for LinuxEncoderFactory {
        type Encoder = LinuxEncoder;
        type Error = VideoError;

        fn create(
            &mut self,
            path: &Path,
            size: RecordingFrameSize,
        ) -> Result<Self::Encoder, Self::Error> {
            LinuxEncoder::new(path, size.width() as u32, size.height() as u32)
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::LinuxEncoderFactory;

#[cfg(target_os = "windows")]
mod windows {
    use std::path::Path;

    use festerm_recording_windows::{VideoError, WindowsEncoder};
    use festerm_ui_egui::RecordingFrameSize;

    use crate::{EncoderFactory, VideoEncoder};

    impl VideoEncoder for WindowsEncoder {
        type Error = VideoError;

        fn append(&mut self, rgba: &[u8]) -> Result<(), Self::Error> {
            WindowsEncoder::append(self, rgba)
        }

        fn finish(&mut self) -> Result<(), Self::Error> {
            WindowsEncoder::finish(self)
        }
    }

    /// Loads Windows Media Foundation only when exporting.
    #[derive(Default)]
    pub struct WindowsEncoderFactory;

    impl EncoderFactory for WindowsEncoderFactory {
        type Encoder = WindowsEncoder;
        type Error = VideoError;

        fn create(
            &mut self,
            path: &Path,
            size: RecordingFrameSize,
        ) -> Result<Self::Encoder, Self::Error> {
            WindowsEncoder::new(path, size.width() as u32, size.height() as u32)
        }
    }
}

#[cfg(target_os = "windows")]
pub use windows::WindowsEncoderFactory;

#[cfg(target_os = "android")]
mod android {
    use std::path::Path;

    use festerm_android_platform::{AndroidEncoder, VideoError};
    use festerm_ui_egui::RecordingFrameSize;

    use crate::{EncoderFactory, VideoEncoder};

    impl VideoEncoder for AndroidEncoder {
        type Error = VideoError;

        fn append(&mut self, rgba: &[u8]) -> Result<(), Self::Error> {
            AndroidEncoder::append(self, rgba)
        }

        fn finish(&mut self) -> Result<(), Self::Error> {
            AndroidEncoder::finish(self)
        }
    }

    /// Uses Android's system MediaCodec and MediaMuxer.
    #[derive(Default)]
    pub struct AndroidEncoderFactory;

    impl EncoderFactory for AndroidEncoderFactory {
        type Encoder = AndroidEncoder;
        type Error = VideoError;

        fn create(
            &mut self,
            path: &Path,
            size: RecordingFrameSize,
        ) -> Result<Self::Encoder, Self::Error> {
            AndroidEncoder::new(path, size.width() as u32, size.height() as u32)
        }
    }
}

#[cfg(target_os = "android")]
pub use android::AndroidEncoderFactory;
