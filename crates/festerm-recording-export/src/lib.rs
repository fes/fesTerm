//! Offline export into caller-owned private staging.
//!
//! The host owns destination authorization and source retention. Encoding
//! stages videos privately; document providers can expose an incomplete set.

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "windows"
))]
use std::io::{Seek, Write};
use std::{
    fmt,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
    time::Duration,
};

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
))]
mod library;
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
))]
pub use library::{LibraryResult, LibrarySnapshot, LibraryTask};
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
))]
mod prepare;
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
))]
pub use prepare::{PrepareTask, PreparedCapture};
mod appearance;
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
))]
mod worker;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux", windows))]
pub use worker::DirectoryBackend;
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
))]
pub use worker::{
    ExportBackend, ExportControl, ExportPreview, ExportPublication, ExportResult, ExportTask,
    PreviewTask, PreviewedExport, PublicationRequest,
};

use appearance::PreparedAppearance;
use festerm_recording::{
    Pacing, Playback, PlaybackPlan, RecordingFont, RecordingTheme, RecoveryPrefix, StreamId,
};
use festerm_ui_egui::RecordingFrameSize;
use sha2::{Digest, Sha256};

/// Versioned appearance identity published beside the synchronized videos.
pub const APPEARANCE_FILE_NAME: &str = "recording-appearance.json";

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "android"
))]
mod provider_journal;
#[cfg(target_os = "android")]
mod publish_android;
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "android",
    target_os = "windows"
))]
mod publish_provider;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
mod publish_unix;
#[cfg(target_os = "windows")]
mod publish_windows;
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "android"
))]
mod source_unix;
#[cfg(target_os = "windows")]
mod source_windows;
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "android"
))]
mod staging_unix;
#[cfg(target_os = "windows")]
mod staging_windows;

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "android"
))]
pub use provider_journal::{ProviderJournal, ProviderJournalRecord};
#[cfg(target_os = "android")]
pub use publish_android::AndroidRecordingProvider;
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "android",
    target_os = "windows"
))]
pub use publish_provider::{
    delete_source_after_provider_publication, publish_to_provider, ProviderEntry,
    ProviderPublication, ProviderPublicationError, RecordingProvider,
};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
pub use publish_unix::{delete_source_after_publication, publish_to_directory, PublishedRecording};
#[cfg(target_os = "windows")]
pub use publish_windows::{
    delete_source_after_publication, publish_to_directory, PublishedRecording,
};
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "android"
))]
pub use staging_unix::PrivateStaging;
#[cfg(target_os = "windows")]
pub use staging_windows::PrivateStaging;

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

/// Coordinates a cancellation request with the irreversible retention step.
/// A successful cancellation prevents the source from being deleted, even if
/// publication has already finished.
#[derive(Default)]
pub struct ExportCancellation {
    cancelled: AtomicBool,
    retention: AtomicU8,
}

struct CancelledReader<'a, R> {
    inner: R,
    cancelled: &'a AtomicBool,
}

impl<R: std::io::Read> std::io::Read for CancelledReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(std::io::Error::other("recording export was cancelled"));
        }
        self.inner.read(buffer)
    }
}

impl ExportCancellation {
    /// Always requests cancellation; returns false if the retention step has
    /// already been committed and an in-flight deletion may still finish.
    pub fn cancel(&self) -> bool {
        self.cancelled.store(true, Ordering::Release);
        match self
            .retention
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) | Err(1) => true,
            Err(_) => false,
        }
    }

    pub fn flag(&self) -> &AtomicBool {
        &self.cancelled
    }

    /// Called only after verified publication and private staging cleanup.
    /// False means cancellation won and the source must be retained.
    pub fn commit_retention(&self) -> bool {
        if self.cancelled.load(Ordering::Acquire) {
            return false;
        }
        self.retention
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
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
    appearance_metadata: Vec<u8>,
    frames_per_video: u64,
    duration: Duration,
    recovered_prefix: bool,
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "android",
        target_os = "windows"
    ))]
    directory: Option<File>,
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))]
    source: source_unix::SourceBinding,
    #[cfg(target_os = "windows")]
    source: source_windows::SourceBinding,
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "android",
        target_os = "windows"
    ))]
    source_fingerprint: [u8; 32],
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "android",
        target_os = "windows"
    ))]
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

    fn require_complete_source(&self) -> Result<(), ExportError> {
        if self.recovered_prefix {
            Err(ExportError::new(
                "an incomplete source cannot be automatically deleted after recovery",
            ))
        } else {
            Ok(())
        }
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "android",
        target_os = "windows"
    ))]
    fn staging_directory(&self) -> Result<&File, ExportError> {
        self.directory
            .as_ref()
            .ok_or_else(|| ExportError::new("video staging has already been released"))
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "android",
        target_os = "windows"
    ))]
    pub const fn source_fingerprint(&self) -> [u8; 32] {
        self.source_fingerprint
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

struct StageOptions {
    export: ExportOptions,
    appearance: PreparedAppearance,
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
    progress: impl FnMut(ExportProgress),
) -> Result<StagedRecording, ExportError> {
    stage_recording_inner(
        source,
        staging_directory,
        StageOptions {
            export: options,
            appearance: PreparedAppearance::new(
                RecordingFont::JetBrainsMono,
                RecordingTheme::FesTermDark,
                None,
            )?,
        },
        factory,
        cancelled,
        None,
        progress,
    )
}

fn stage_recording_inner<F: EncoderFactory>(
    source: &Path,
    staging_directory: &Path,
    options: StageOptions,
    factory: &mut F,
    cancelled: &AtomicBool,
    recovery: Option<RecoveryPrefix>,
    mut progress: impl FnMut(ExportProgress),
) -> Result<StagedRecording, ExportError> {
    let StageOptions {
        export: options,
        appearance,
    } = options;
    if !staging_directory.is_dir() {
        return Err(ExportError::new("private video staging is unavailable"));
    }
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))]
    let staging_handle = File::open(staging_directory)
        .map_err(|error| ExportError::new(format!("could not open video staging: {error}")))?;
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))]
    source_unix::require_private(&staging_handle, true, "video staging directory")?;
    #[cfg(target_os = "windows")]
    let staging_handle =
        source_windows::open_private_directory(staging_directory, "video staging directory")?;
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
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))]
    let (source_binding, source_file) = source_unix::SourceBinding::open(source)?;
    #[cfg(target_os = "windows")]
    let (source_binding, source_file) = source_windows::SourceBinding::open(source)?;
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "android",
        target_os = "windows"
    )))]
    let source_file = File::open(source)
        .map_err(|error| ExportError::new(format!("could not open recording source: {error}")))?;
    let input = CancelledReader {
        inner: source_file,
        cancelled,
    };
    let plan = match recovery {
        Some(prefix) => PlaybackPlan::inspect_recovered_cancellable(
            prefix.reader(input),
            prefix,
            options.pacing,
            cancelled,
        ),
        None => PlaybackPlan::inspect_cancellable(input, options.pacing, cancelled),
    }
    .map_err(|error| ExportError::new(format!("could not plan recording export: {error}")))?;
    let total_frames = plan
        .frame_count()
        .checked_mul(plan.stream_count() as u64)
        .ok_or_else(|| ExportError::new("recording export frame count overflows"))?;
    let (mut renderer, appearance_metadata) =
        appearance.into_renderer(options.size, options.font_size, recovery.is_some())?;
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
            #[cfg(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "ios",
                target_os = "android",
                target_os = "windows"
            ))]
            let source_file = source_binding.open_current()?;
            #[cfg(not(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "ios",
                target_os = "android",
                target_os = "windows"
            )))]
            let source_file = File::open(source).map_err(|error| {
                ExportError::new(format!("could not reopen recording source: {error}"))
            })?;
            let input = CancelledReader {
                inner: source_file,
                cancelled,
            };
            let input: Box<dyn Read + '_> = match recovery {
                Some(prefix) => Box::new(prefix.reader(input)),
                None => Box::new(input),
            };
            let mut playback = Playback::new(input, stream, plan.clone()).map_err(|error| {
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
            #[cfg(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "ios",
                target_os = "android"
            ))]
            let file = festerm_unix_security::open_file_nofollow(
                &staging_handle,
                Path::new(path.file_name().expect("staged video has a filename")),
            );
            #[cfg(target_os = "windows")]
            let file = festerm_windows_security::open_file_read_write_no_reparse(
                &staging_handle,
                Path::new(path.file_name().expect("staged video has a filename")),
            );
            #[cfg(not(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "ios",
                target_os = "android",
                target_os = "windows"
            )))]
            let file = File::open(path);
            let mut file = file
                .map_err(|error| ExportError::new(format!("could not open staged MP4: {error}")))?;
            #[cfg(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "ios",
                target_os = "android"
            ))]
            source_unix::make_staged_file_private(&staging_handle, &file)?;
            #[cfg(target_os = "windows")]
            source_windows::require_private(&file, false, "staged MP4")?;
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
            #[cfg(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "ios",
                target_os = "android"
            ))]
            let cleanup = festerm_unix_security::remove_private_staging_file(
                &staging_handle,
                Path::new(video.path.file_name().expect("staged video has a filename")),
            );
            #[cfg(target_os = "windows")]
            let cleanup = source_windows::remove_staged_file(
                &staging_handle,
                Path::new(video.path.file_name().expect("staged video has a filename")),
            );
            #[cfg(not(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "ios",
                target_os = "android",
                target_os = "windows"
            )))]
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
        appearance_metadata,
        frames_per_video: plan.frame_count(),
        duration: plan.duration(),
        recovered_prefix: recovery.is_some(),
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "ios",
            target_os = "android",
            target_os = "windows"
        ))]
        directory: Some(staging_handle),
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "ios",
            target_os = "android",
            target_os = "windows"
        ))]
        source: source_binding,
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "ios",
            target_os = "android",
            target_os = "windows"
        ))]
        source_fingerprint: plan.fingerprint(),
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "ios",
            target_os = "android",
            target_os = "windows"
        ))]
        stage_id: uuid::Uuid::new_v4(),
    })
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "windows"
))]
fn collision_name(
    base: &str,
    collision: usize,
    recovered_prefix: bool,
) -> Result<String, ExportError> {
    let (stem, marker) = if recovered_prefix {
        (
            base.strip_suffix("-recovered")
                .ok_or_else(|| ExportError::new("recovered export folder is not marked"))?,
            "-recovered",
        )
    } else {
        (base, "")
    };
    if collision == 0 {
        return Ok(base.to_owned());
    }
    let suffix = format!("-{}", collision + 1);
    let mut end = stem.len().min(255 - suffix.len() - marker.len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    Ok(format!("{}{marker}{suffix}", &stem[..end]))
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

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "windows"
))]
fn verify_published_file(
    destination: &mut File,
    video: &StagedVideo,
    cancelled: &AtomicBool,
) -> Result<(), ExportError> {
    destination
        .rewind()
        .map_err(|error| ExportError::new(format!("could not verify destination MP4: {error}")))?;
    let mut hash = Sha256::new();
    let mut length = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(ExportError::new("recording publication was cancelled"));
        }
        let count = destination.read(&mut buffer).map_err(|error| {
            ExportError::new(format!("could not verify destination MP4: {error}"))
        })?;
        if count == 0 {
            break;
        }
        length = length
            .checked_add(count as u64)
            .filter(|length| *length <= video.length)
            .ok_or_else(|| ExportError::new("a destination MP4 did not match the staged video"))?;
        hash.update(&buffer[..count]);
    }
    if length != video.length || <[u8; 32]>::from(hash.finalize()) != video.fingerprint {
        return Err(ExportError::new(
            "a destination MP4 did not match the staged video",
        ));
    }
    Ok(())
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "windows"
))]
fn write_and_verify_appearance(
    destination: &mut File,
    staged: &StagedRecording,
    cancelled: &AtomicBool,
) -> Result<(), ExportError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(ExportError::new("recording publication was cancelled"));
    }
    destination
        .write_all(&staged.appearance_metadata)
        .and_then(|()| destination.sync_all())
        .and_then(|()| destination.rewind())
        .map_err(|error| {
            ExportError::new(format!("could not publish recording appearance: {error}"))
        })?;
    let mut verified = Vec::new();
    CancelledReader {
        inner: destination.take(staged.appearance_metadata.len() as u64 + 1),
        cancelled,
    }
    .read_to_end(&mut verified)
    .map_err(|error| ExportError::new(format!("could not verify recording appearance: {error}")))?;
    if verified != staged.appearance_metadata {
        return Err(ExportError::new(
            "published recording appearance did not match the export",
        ));
    }
    Ok(())
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

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn cancellation_always_closes_admission_even_after_retention_commit() {
        let cancelled = ExportCancellation::default();
        assert!(cancelled.cancel());
        assert!(!cancelled.commit_retention());
        assert!(cancelled.flag().load(Ordering::Acquire));

        let committed = ExportCancellation::default();
        assert!(committed.commit_retention());
        assert!(!committed.cancel());
        assert!(committed.flag().load(Ordering::Acquire));
    }

    #[test]
    fn concurrent_cancel_and_retention_have_one_winner() {
        let gate = Arc::new(ExportCancellation::default());
        let barrier = Arc::new(Barrier::new(3));
        let cancelling = {
            let gate = gate.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                gate.cancel()
            })
        };
        let committing = {
            let gate = gate.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                gate.commit_retention()
            })
        };
        barrier.wait();
        let cancelled = cancelling.join().unwrap();
        let committed = committing.join().unwrap();
        assert_ne!(cancelled, committed);
        assert!(gate.flag().load(Ordering::Acquire));
    }
}
