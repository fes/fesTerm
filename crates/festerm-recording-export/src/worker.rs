use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, TryRecvError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use festerm_recording::{
    ExportDestination, LocalStart, NameTemplates, PlaybackPlan, RecordingFont, RecordingId,
    RecordingLease, RecordingManifest, RecordingSettings, RecordingSource, RecordingSourceState,
    RecoveryPrefix,
};
use festerm_ui_egui::RecordingFrameSize;

use crate::{
    stage_recording_inner, CancelledReader, EncoderFactory, ExportCancellation, ExportOptions,
    ExportProgress, PreparedAppearance, PrivateStaging, StageOptions, StagedRecording,
};

type Wake = Arc<dyn Fn() + Send + Sync>;

pub struct ExportControl {
    cancellation: Arc<ExportCancellation>,
    wake: Wake,
}

impl ExportControl {
    pub fn new(cancellation: Arc<ExportCancellation>, wake: Wake) -> Self {
        Self { cancellation, wake }
    }
}

pub struct PublicationRequest<'a> {
    pub root: &'a Path,
    pub staged: &'a StagedRecording,
    pub destination: &'a ExportDestination,
    pub templates: &'a NameTemplates,
    pub id: RecordingId,
    pub start: LocalStart,
    pub cancelled: &'a AtomicBool,
}

pub trait ExportBackend: Send + 'static {
    type Publication: ExportPublication;

    /// Automatic deletion is opt-in; backends that do not opt in retain the
    /// source even if an older settings snapshot requested deletion.
    fn allows_automatic_source_deletion(&self) -> bool {
        false
    }

    /// Supplies verified optional faces on a worker thread. Bundled faces need
    /// no host I/O; unsupported families must be reported, not substituted.
    fn recording_font_faces(
        &self,
        font: RecordingFont,
        _cancelled: &AtomicBool,
    ) -> Result<Option<[Vec<u8>; 4]>, String> {
        if font.bundled() {
            Ok(None)
        } else {
            Err(format!(
                "{} is unavailable on this device; choose a bundled font",
                font.label()
            ))
        }
    }

    fn preflight(&self, destination: &ExportDestination) -> Result<(), String>;
    fn publish(&self, request: PublicationRequest<'_>) -> Result<Self::Publication, String>;
}

pub trait ExportPublication {
    fn names(&self) -> (String, Vec<String>);
    fn delete_source(&mut self, staged: &StagedRecording) -> Result<(), String>;
    fn finish(self) -> Result<(), String>;
}

#[derive(Default)]
struct Progress {
    stream: AtomicUsize,
    done: AtomicU64,
    total: AtomicU64,
}

#[derive(Clone, Debug)]
pub struct ExportResult {
    pub directory: String,
    pub videos: Vec<String>,
    pub retained_source: bool,
    pub recovered_prefix: bool,
}

#[derive(Clone, Debug)]
pub struct ExportPreview {
    settings: RecordingSettings,
    duration: Duration,
    fingerprint: [u8; 32],
}

impl ExportPreview {
    pub fn matches(&self, settings: &RecordingSettings) -> bool {
        &self.settings == settings
    }

    pub const fn duration(&self) -> Duration {
        self.duration
    }
}

pub struct PreviewTask {
    receiver: Receiver<Result<ExportPreview, String>>,
    cancellation: Arc<ExportCancellation>,
}

impl PreviewTask {
    pub fn start(
        root: PathBuf,
        source: PathBuf,
        id: RecordingId,
        lease: Arc<RecordingLease>,
        settings: RecordingSettings,
        control: ExportControl,
    ) -> Result<Self, String> {
        let ExportControl { cancellation, wake } = control;
        let worker_cancellation = cancellation.clone();
        let (send, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("festerm-recording-preview".to_owned())
            .spawn(move || {
                let result = (|| {
                    if source.parent() != Some(root.as_path())
                        || source.file_name()
                            != Some(std::ffi::OsStr::new(&format!("recording-{id}.fstrec")))
                    {
                        return Err("The recording source does not match the saved options".into());
                    }
                    lease
                        .verify_for(&root)
                        .map_err(|error| format!("Recording storage changed: {error}"))?;
                    let validated = settings.validate()?;
                    #[cfg(any(
                        target_os = "linux",
                        target_os = "macos",
                        target_os = "ios",
                        target_os = "android"
                    ))]
                    let (binding, file) = crate::source_unix::SourceBinding::open(&source)
                        .map_err(|error| error.to_string())?;
                    #[cfg(windows)]
                    let (binding, file) = crate::source_windows::SourceBinding::open(&source)
                        .map_err(|error| error.to_string())?;
                    let plan = PlaybackPlan::inspect_cancellable(
                        CancelledReader {
                            inner: file,
                            cancelled: worker_cancellation.flag(),
                        },
                        validated.pacing,
                        worker_cancellation.flag(),
                    )
                    .map_err(|error| format!("Could not preview recording export: {error}"))?;
                    if worker_cancellation.flag().load(Ordering::Acquire) {
                        return Err("Recording preview was cancelled".into());
                    }
                    drop(binding.open_current().map_err(|error| error.to_string())?);
                    lease
                        .verify_for(&root)
                        .map_err(|error| format!("Recording storage changed: {error}"))?;
                    Ok(ExportPreview {
                        settings,
                        duration: plan.duration(),
                        fingerprint: plan.fingerprint(),
                    })
                })();
                let _ = send.send(result);
                wake();
            })
            .map_err(|error| format!("Recording preview could not start: {error}"))?;
        Ok(Self {
            receiver,
            cancellation,
        })
    }

    pub fn cancel(&self) {
        let _ = self.cancellation.cancel();
    }

    pub fn poll(&self) -> Option<Result<ExportPreview, String>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err("Recording preview stopped unexpectedly".to_owned()))
            }
        }
    }
}

impl Drop for PreviewTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub struct PreviewedExport {
    pub root: PathBuf,
    pub source: PathBuf,
    pub manifest: RecordingManifest,
    pub lease: Arc<RecordingLease>,
    pub preview: ExportPreview,
}

pub struct ExportTask {
    receiver: Receiver<Result<ExportResult, String>>,
    progress: Arc<Progress>,
    cancellation: Arc<ExportCancellation>,
}

enum ExportInput {
    Live {
        source: PathBuf,
        lease: Arc<RecordingLease>,
        preview: Option<ExportPreview>,
    },
    Recovery {
        source: Box<RecordingSource>,
        settings: RecordingSettings,
    },
}

impl ExportTask {
    pub fn start<F: EncoderFactory + Send + 'static, B: ExportBackend>(
        root: PathBuf,
        source: PathBuf,
        manifest: RecordingManifest,
        lease: Arc<RecordingLease>,
        factory: F,
        backend: B,
        wake: Wake,
    ) -> Result<Self, String> {
        Self::start_controlled(
            root,
            source,
            manifest,
            lease,
            factory,
            backend,
            ExportControl::new(Arc::new(ExportCancellation::default()), wake),
        )
    }

    pub fn start_controlled<F: EncoderFactory + Send + 'static, B: ExportBackend>(
        root: PathBuf,
        source: PathBuf,
        manifest: RecordingManifest,
        lease: Arc<RecordingLease>,
        factory: F,
        backend: B,
        control: ExportControl,
    ) -> Result<Self, String> {
        Self::spawn(
            root,
            manifest,
            ExportInput::Live {
                source,
                lease,
                preview: None,
            },
            factory,
            backend,
            control,
        )
    }

    pub fn start_previewed<F: EncoderFactory + Send + 'static, B: ExportBackend>(
        request: PreviewedExport,
        factory: F,
        backend: B,
        control: ExportControl,
    ) -> Result<Self, String> {
        Self::spawn(
            request.root,
            request.manifest,
            ExportInput::Live {
                source: request.source,
                lease: request.lease,
                preview: Some(request.preview),
            },
            factory,
            backend,
            control,
        )
    }

    pub fn start_recovery<F: EncoderFactory + Send + 'static, B: ExportBackend>(
        root: PathBuf,
        source: RecordingSource,
        manifest: RecordingManifest,
        settings: RecordingSettings,
        factory: F,
        backend: B,
        wake: Wake,
    ) -> Result<Self, String> {
        Self::start_recovery_controlled(
            root,
            source,
            manifest,
            settings,
            factory,
            backend,
            ExportControl::new(Arc::new(ExportCancellation::default()), wake),
        )
    }

    pub fn start_recovery_controlled<F: EncoderFactory + Send + 'static, B: ExportBackend>(
        root: PathBuf,
        source: RecordingSource,
        manifest: RecordingManifest,
        settings: RecordingSettings,
        factory: F,
        backend: B,
        control: ExportControl,
    ) -> Result<Self, String> {
        Self::spawn(
            root,
            manifest,
            ExportInput::Recovery {
                source: Box::new(source),
                settings,
            },
            factory,
            backend,
            control,
        )
    }

    fn spawn<F: EncoderFactory + Send + 'static, B: ExportBackend>(
        root: PathBuf,
        manifest: RecordingManifest,
        input: ExportInput,
        mut factory: F,
        backend: B,
        control: ExportControl,
    ) -> Result<Self, String> {
        let ExportControl { cancellation, wake } = control;
        let progress = Arc::new(Progress::default());
        let worker_cancellation = cancellation.clone();
        let worker_progress = progress.clone();
        let (send, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("festerm-recording-export".to_owned())
            .spawn(move || {
                let mut last_wake = Instant::now();
                let report = |event: ExportProgress| {
                    worker_progress
                        .stream
                        .store(event.stream.index(), Ordering::Relaxed);
                    worker_progress
                        .done
                        .store(event.frames_done, Ordering::Relaxed);
                    worker_progress
                        .total
                        .store(event.total_frames, Ordering::Release);
                    if last_wake.elapsed() >= Duration::from_millis(200)
                        || event.frames_done == event.total_frames
                    {
                        last_wake = Instant::now();
                        wake();
                    }
                };
                let result = (|| {
                    if worker_cancellation.flag().load(Ordering::Acquire) {
                        return Err("Recording export was cancelled; the source was retained".into());
                    }
                    let (source, lease, settings, expected, recovery) = match input {
                        ExportInput::Live { source, lease, preview } => {
                            let (settings, expected) = match preview {
                                Some(preview) => (preview.settings, Some(preview.fingerprint)),
                                None => (manifest.settings().clone(), None),
                            };
                            (source, lease, settings, expected, None)
                        }
                        ExportInput::Recovery { source, settings } => {
                            let lease = Arc::new(
                                RecordingLease::acquire(&root).map_err(|error| {
                                    format!("Recording storage is unavailable: {error}")
                                })?,
                            );
                            manifest.verify(&lease).map_err(|error| {
                                format!("Recording options changed; the source was retained: {error}")
                            })?;
                            if source.id() != manifest.id() {
                                return Err("The recording does not match its saved options".into());
                            }
                            let prefix = match source.state() {
                                RecordingSourceState::Complete { .. } => None,
                                RecordingSourceState::Incomplete { prefix: Some(prefix), .. } => Some(*prefix),
                                _ => return Err("This recording has no validated prefix to export".into()),
                            };
                            if let Some(prefix) = prefix {
                                source.verify_with_cancellation(&lease, worker_cancellation.flag()).map_err(|error| {
                                    format!("Recording changed or inspection was cancelled; the original was retained: {error}")
                                })?;
                                let path = source.path();
                                (path, lease, settings, Some(prefix.fingerprint()), Some((*source, prefix)))
                            } else {
                                let source = if source.is_partial() {
                                    (*source).promote(&lease).map_err(|error| {
                                        format!("Recording recovery failed; inspect the source: {error}")
                                    })?
                                } else {
                                    source.verify_with_cancellation(&lease, worker_cancellation.flag()).map_err(|error| {
                                        format!("Recording changed or inspection was cancelled; the source was retained: {error}")
                                    })?;
                                    *source
                                };
                                let fingerprint = source.fingerprint();
                                (source.path(), lease, settings, Some(fingerprint), None)
                            }
                        }
                    };
                    export(
                        ExportRequest {
                            root: &root,
                            source: &source,
                            manifest,
                            settings: &settings,
                            lease: &lease,
                            cancellation: &worker_cancellation,
                            expected_fingerprint: expected,
                            recovery: recovery.as_ref().map(|(source, prefix)| (source, *prefix)),
                        },
                        &mut factory,
                        &backend,
                        report,
                    )
                })();
                let _ = send.send(result);
                wake();
            })
            .map_err(|error| format!("Recording export could not start: {error}"))?;
        Ok(Self {
            receiver,
            progress,
            cancellation,
        })
    }

    /// Returns false once source retention has been committed.
    pub fn cancel(&self) -> bool {
        self.cancellation.cancel()
    }

    pub fn progress(&self) -> (usize, u64, u64) {
        let total = self.progress.total.load(Ordering::Acquire);
        (
            self.progress.stream.load(Ordering::Relaxed),
            self.progress.done.load(Ordering::Relaxed),
            total,
        )
    }

    pub fn poll(&self) -> Option<Result<ExportResult, String>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(
                "Recording export stopped unexpectedly; check the destination and recording library"
                    .to_owned(),
            )),
        }
    }
}

impl Drop for ExportTask {
    fn drop(&mut self) {
        let _ = self.cancel();
    }
}

struct ExportRequest<'a> {
    root: &'a Path,
    source: &'a Path,
    manifest: RecordingManifest,
    settings: &'a RecordingSettings,
    lease: &'a RecordingLease,
    cancellation: &'a ExportCancellation,
    expected_fingerprint: Option<[u8; 32]>,
    recovery: Option<(&'a RecordingSource, RecoveryPrefix)>,
}

fn export<F: EncoderFactory, B: ExportBackend>(
    request: ExportRequest<'_>,
    factory: &mut F,
    backend: &B,
    report: impl FnMut(ExportProgress),
) -> Result<ExportResult, String> {
    let ExportRequest {
        root,
        source,
        manifest,
        settings,
        lease,
        cancellation,
        expected_fingerprint,
        recovery,
    } = request;
    lease
        .verify_for(root)
        .map_err(|error| format!("Recording storage changed: {error}"))?;
    manifest
        .verify(lease)
        .map_err(|error| format!("Recording options changed; the source was retained: {error}"))?;
    let expected_path = recovery.map_or_else(
        || root.join(format!("recording-{}.fstrec", manifest.id())),
        |(source, _)| source.path(),
    );
    if source != expected_path {
        return Err("The recording source does not match its saved settings".to_owned());
    }
    let settings = settings.validate()?;
    let delete_source = recovery.is_none()
        && settings.delete_source_after_export
        && backend.allows_automatic_source_deletion();
    let templates = if recovery.is_some() {
        NameTemplates::new(
            &format!("{}-recovered", settings.templates.directory_template()),
            settings.templates.session_template(),
        )
        .map_err(|error| format!("The recovered export folder name is invalid: {error}"))?
    } else {
        settings.templates.clone()
    };
    let destination = settings
        .destination
        .as_ref()
        .ok_or_else(|| "The recording export folder is unavailable".to_owned())?;
    backend.preflight(destination)?;
    if cancellation.flag().load(Ordering::Acquire) {
        return Err("Recording export was cancelled; the source was retained".to_owned());
    }
    let appearance = settings.appearance;
    let faces = backend.recording_font_faces(appearance.font, cancellation.flag())?;
    let appearance = PreparedAppearance::new(appearance.font, appearance.theme, faces)
        .map_err(|error| error.to_string())?;
    if cancellation.flag().load(Ordering::Acquire) {
        return Err("Recording export was cancelled; the source was retained".to_owned());
    }
    let staging = PrivateStaging::create(root).map_err(|error| error.to_string())?;
    let mut result: Result<_, String> = (|| {
        let options = ExportOptions {
            pacing: settings.pacing,
            size: RecordingFrameSize::new(
                usize::from(settings.appearance.width),
                usize::from(settings.appearance.height),
            )
            .map_err(|error| error.to_string())?,
            font_size: f32::from(settings.appearance.font_size_points),
        };
        let staged = stage_recording_inner(
            source,
            &staging.path(),
            StageOptions {
                export: options,
                appearance,
            },
            factory,
            cancellation.flag(),
            recovery.map(|(_, prefix)| prefix),
            report,
        )
        .map_err(|error| error.to_string())?;
        if expected_fingerprint.is_some_and(|expected| expected != staged.source_fingerprint()) {
            return Err("The recording changed after library inspection".to_owned());
        }
        if let Some((original, _)) = recovery {
            original
                .verify_with_cancellation(lease, cancellation.flag())
                .map_err(|error| {
                    format!("The interrupted source changed or inspection was cancelled: {error}")
                })?;
        }
        lease
            .verify_for(root)
            .map_err(|error| format!("Recording storage changed: {error}"))?;
        if cancellation.flag().load(Ordering::Acquire) {
            return Err("Recording export was cancelled".to_owned());
        }
        let publication = backend.publish(PublicationRequest {
            root,
            staged: &staged,
            destination,
            templates: &templates,
            id: manifest.id(),
            start: manifest.start(),
            cancelled: cancellation.flag(),
        })?;
        Ok((staged, publication))
    })();
    if let Ok((staged, _)) = &mut result {
        drop(staged.directory.take());
    }
    let cleanup = staging.cleanup().map_err(|error| error.to_string());
    let (staged, mut publication) = match (result, cleanup) {
        (Ok(value), Ok(())) => value,
        (Ok(_), Err(error)) => {
            return Err(format!(
                "MP4s were published, but private staging cleanup was incomplete; the source was retained: {error}"
            ));
        }
        (Err(error), Ok(())) => return Err(format!("{error}; the source was retained")),
        (Err(error), Err(cleanup)) => {
            return Err(format!(
                "{error}; the source was retained and private staging cleanup failed: {cleanup}"
            ));
        }
    };
    let (directory, videos) = publication.names();
    lease.verify_for(root).map_err(|error| {
        format!(
            "Videos were published but recording storage changed; the source was retained: {error}"
        )
    })?;
    if !cancellation.commit_retention() {
        return Err(format!(
            "Recording export was cancelled after publishing {directory}; the source was retained"
        ));
    }
    if cancellation.flag().load(Ordering::Acquire) {
        return Err(format!(
            "Recording export stopped after publishing {directory}; the source was retained"
        ));
    }
    if delete_source {
        publication.delete_source(&staged).map_err(|error| {
            format!("Videos were published, but source deletion could not be confirmed; check the recording library: {error}")
        })?;
        if cancellation.flag().load(Ordering::Acquire) {
            return Err(
                "Videos were published and the source was deleted, but export cleanup was interrupted; inspect the recording library"
                    .to_owned(),
            );
        }
        manifest.verify(lease).map_err(|error| {
            format!("Videos were published and the source was deleted, but its settings changed: {error}")
        })?;
        if cancellation.flag().load(Ordering::Acquire) {
            return Err(
                "Videos were published and the source was deleted, but export cleanup was interrupted; inspect the recording library"
                    .to_owned(),
            );
        }
        manifest.remove().map_err(|error| {
            format!("Videos were published and the source was deleted, but removing its settings could not be completed; refresh the library: {error}")
        })?;
    }
    if cancellation.flag().load(Ordering::Acquire) {
        return Err(format!(
            "Videos were published, but export cleanup was interrupted; inspect {directory} and the recording library"
        ));
    }
    publication.finish().map_err(|error| {
        format!("Videos were published, but export cleanup could not be confirmed; check the recording library: {error}")
    })?;
    Ok(ExportResult {
        directory,
        videos,
        retained_source: !delete_source,
        recovered_prefix: recovery.is_some(),
    })
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux", windows))]
pub struct DirectoryBackend;

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux", windows))]
impl ExportBackend for DirectoryBackend {
    type Publication = crate::PublishedRecording;

    fn allows_automatic_source_deletion(&self) -> bool {
        true
    }

    fn preflight(&self, destination: &ExportDestination) -> Result<(), String> {
        let ExportDestination::DesktopPath(path) = destination else {
            return Err("Choose a local folder for recording exports".to_owned());
        };
        open_destination(path)?;
        Ok(())
    }

    fn publish(&self, request: PublicationRequest<'_>) -> Result<Self::Publication, String> {
        let ExportDestination::DesktopPath(path) = request.destination else {
            return Err("Choose a local folder for recording exports".to_owned());
        };
        let directory = open_destination(path)?;
        let published = crate::publish_to_directory(
            request.staged,
            &directory,
            request.templates,
            request.id,
            request.start,
            request.cancelled,
        )
        .map_err(|error| error.to_string())?;
        if !destination_matches(path, &directory).map_err(|error| {
            format!("Videos may have been published, but the export folder could not be verified: {error}")
        })? {
            return Err(
                "Videos may have been published, but the selected export folder changed".to_owned(),
            );
        }
        Ok(published)
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux", windows))]
fn open_destination(path: &Path) -> Result<std::fs::File, String> {
    #[cfg(unix)]
    let opened = std::fs::File::open(path);
    #[cfg(windows)]
    let opened = festerm_windows_security::lock_directory_path_without_delete_sharing(path);
    let directory =
        opened.map_err(|error| format!("The recording export folder is unavailable: {error}"))?;
    if !destination_matches(path, &directory)? {
        return Err("The selected recording export folder changed".to_owned());
    }
    Ok(directory)
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
fn destination_matches(path: &Path, directory: &std::fs::File) -> Result<bool, String> {
    use std::os::unix::fs::MetadataExt;

    let pinned = directory.metadata().map_err(|error| error.to_string())?;
    let current = std::fs::metadata(path).map_err(|error| {
        format!("The recording export folder is unavailable or changed: {error}")
    })?;
    Ok(pinned.is_dir()
        && current.is_dir()
        && pinned.dev() == current.dev()
        && pinned.ino() == current.ino())
}

#[cfg(windows)]
fn destination_matches(path: &Path, directory: &std::fs::File) -> Result<bool, String> {
    let current = festerm_windows_security::lock_directory_path_without_delete_sharing(path)
        .map_err(|error| {
            format!("The recording export folder is unavailable or changed: {error}")
        })?;
    festerm_windows_security::same_file_identity(directory, &current)
        .map_err(|error| format!("The recording export folder could not be verified: {error}"))
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux", windows))]
impl ExportPublication for crate::PublishedRecording {
    fn names(&self) -> (String, Vec<String>) {
        (self.directory_name.clone(), self.video_names.clone())
    }

    fn delete_source(&mut self, staged: &StagedRecording) -> Result<(), String> {
        crate::delete_source_after_publication(staged, self).map_err(|error| error.to_string())
    }

    fn finish(self) -> Result<(), String> {
        Ok(())
    }
}
