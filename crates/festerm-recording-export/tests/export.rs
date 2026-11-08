use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use festerm_core::{Dimensions, Terminal};
use festerm_recording::{Completion, FinishReason, Pacing, Recorder};
use festerm_recording_export::{stage_recording, EncoderFactory, ExportOptions, VideoEncoder};
use festerm_ui_egui::RecordingFrameSize;

struct PrivateDir {
    _directory: tempfile::TempDir,
    path: PathBuf,
}

impl PrivateDir {
    fn path(&self) -> &Path {
        &self.path
    }
}

fn private_dir() -> PrivateDir {
    let directory = tempfile::tempdir().unwrap();
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
    festerm_unix_security::make_private_directory(&File::open(directory.path()).unwrap()).unwrap();
    #[cfg(windows)]
    let path = {
        let parent =
            festerm_windows_security::lock_directory_path_without_delete_sharing(directory.path())
                .unwrap();
        let child = Path::new("private");
        festerm_windows_security::create_current_user_only_directory(&parent, child).unwrap();
        directory.path().join(child)
    };
    #[cfg(not(windows))]
    let path = directory.path().to_owned();
    PrivateDir {
        _directory: directory,
        path,
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "windows"
))]
fn directory_handle(path: &Path) -> File {
    #[cfg(windows)]
    {
        festerm_windows_security::lock_directory_path_without_delete_sharing(path).unwrap()
    }
    #[cfg(not(windows))]
    {
        File::open(path).unwrap()
    }
}

fn source(directory: &Path) -> PathBuf {
    source_with_first(directory, b"first")
}

fn source_with_first(directory: &Path, text: &[u8]) -> PathBuf {
    let mut first = Terminal::new(Dimensions::new(20, 3).unwrap()).unwrap();
    let mut second = Terminal::new(Dimensions::new(20, 3).unwrap()).unwrap();
    first.ingest(text);
    second.ingest(b"second");
    let (wake, ready) = mpsc::channel();
    let (mut recorder, streams) = Recorder::start(directory, &[&first, &second], move || {
        wake.send(()).unwrap()
    })
    .unwrap();
    recorder
        .ingest_output(streams[0], &mut first, b" output")
        .unwrap();
    recorder
        .ingest_output(streams[1], &mut second, b" output")
        .unwrap();
    recorder.stop(FinishReason::User).unwrap();
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    match recorder.poll_completion().unwrap() {
        Completion::Complete { path, .. } => path,
        other => panic!("expected a completed source, got {other:?}"),
    }
}

fn options() -> ExportOptions {
    ExportOptions {
        pacing: Pacing::default(),
        size: RecordingFrameSize::new(320, 180).unwrap(),
        font_size: 18.0,
    }
}

#[derive(Default)]
struct FakeFactory {
    created: usize,
    fail_on: Option<usize>,
}

struct FakeEncoder {
    file: File,
    fail: bool,
}

impl EncoderFactory for FakeFactory {
    type Encoder = FakeEncoder;
    type Error = io::Error;

    fn create(&mut self, path: &Path, _: RecordingFrameSize) -> Result<Self::Encoder, Self::Error> {
        self.created += 1;
        #[cfg(not(windows))]
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        #[cfg(windows)]
        let file = {
            let parent = festerm_windows_security::lock_directory_path_without_delete_sharing(
                path.parent().expect("staged video has a parent"),
            )?;
            festerm_windows_security::create_current_user_only_file(
                &parent,
                Path::new(path.file_name().expect("staged video has a filename")),
            )?
        };
        Ok(FakeEncoder {
            file,
            fail: self.fail_on == Some(self.created),
        })
    }
}

impl VideoEncoder for FakeEncoder {
    type Error = io::Error;

    fn append(&mut self, rgba: &[u8]) -> Result<(), Self::Error> {
        if self.fail {
            return Err(io::Error::other("injected encoder failure"));
        }
        let checksum = rgba
            .iter()
            .fold(0u64, |sum, byte| sum.wrapping_add(u64::from(*byte)));
        self.file.write_all(&checksum.to_le_bytes())
    }

    fn finish(&mut self) -> Result<(), Self::Error> {
        self.file.flush()
    }
}

#[test]
fn stages_a_complete_synchronized_set_and_keeps_the_source() {
    let root = private_dir();
    let staging = private_dir();
    let source = source(root.path());
    let mut updates = Vec::new();
    let result = stage_recording(
        &source,
        staging.path(),
        options(),
        &mut FakeFactory::default(),
        &AtomicBool::new(false),
        |update| updates.push(update),
    )
    .unwrap();
    assert_eq!(result.videos().len(), 2);
    assert!(result.frames_per_video() >= 1);
    assert!(source.exists());
    assert_eq!(
        updates.last().unwrap().frames_done,
        updates.last().unwrap().total_frames
    );
    for (index, video) in result.videos().iter().enumerate() {
        assert_eq!(video.stream().index(), index + 1);
        assert_eq!(
            fs::metadata(video.path()).unwrap().len(),
            result.frames_per_video() * 8
        );
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(video.path()).unwrap().permissions().mode() & 0o077,
                0
            );
        }
    }
    assert_ne!(
        fs::read(result.videos()[0].path()).unwrap(),
        fs::read(result.videos()[1].path()).unwrap()
    );
}

#[test]
fn failure_or_cancellation_removes_staged_videos_but_preserves_source() {
    let root = private_dir();
    let staging = private_dir();
    let source = source(root.path());
    let error = stage_recording(
        &source,
        staging.path(),
        options(),
        &mut FakeFactory {
            fail_on: Some(2),
            ..Default::default()
        },
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap_err();
    assert!(error.to_string().contains("injected encoder failure"));
    assert!(source.exists());
    assert_eq!(fs::read_dir(staging.path()).unwrap().count(), 0);
    let cancelled = AtomicBool::new(false);
    let error = stage_recording(
        &source,
        staging.path(),
        options(),
        &mut FakeFactory::default(),
        &cancelled,
        |_| cancelled.store(true, Ordering::Release),
    )
    .unwrap_err();
    assert!(error.to_string().contains("cancelled"));
    assert!(source.exists());
    assert_eq!(fs::read_dir(staging.path()).unwrap().count(), 0);

    let cancelled = AtomicBool::new(false);
    let error = stage_recording(
        &source,
        staging.path(),
        options(),
        &mut FakeFactory::default(),
        &cancelled,
        |update| {
            if update.frames_done == update.total_frames {
                cancelled.store(true, Ordering::Release);
            }
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("cancelled"));
    assert!(source.exists());
    assert_eq!(fs::read_dir(staging.path()).unwrap().count(), 0);
}

#[test]
fn source_change_during_export_never_produces_a_complete_set() {
    let root = private_dir();
    let staging = private_dir();
    let source = source(root.path());
    let mut changed = false;
    let error = stage_recording(
        &source,
        staging.path(),
        options(),
        &mut FakeFactory::default(),
        &AtomicBool::new(false),
        |_| {
            if !changed {
                OpenOptions::new()
                    .append(true)
                    .open(&source)
                    .unwrap()
                    .write_all(b"changed")
                    .unwrap();
                changed = true;
            }
        },
    )
    .unwrap_err();
    assert!(changed);
    assert!(error.to_string().contains("recording"));
    assert!(source.exists());
    assert_eq!(fs::read_dir(staging.path()).unwrap().count(), 0);
}

#[test]
fn refuses_to_stage_into_an_existing_directory_of_files() {
    let root = private_dir();
    let staging = private_dir();
    let source = source(root.path());
    let existing = staging.path().join("existing.mp4");
    fs::write(&existing, b"preserve me").unwrap();
    assert!(stage_recording(
        &source,
        staging.path(),
        options(),
        &mut FakeFactory::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .is_err());
    assert_eq!(fs::read(existing).unwrap(), b"preserve me");
    assert!(source.exists());
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "windows"
))]
#[test]
fn publication_avoids_collisions_and_leaves_existing_exports_untouched() {
    use festerm_recording::{LocalStart, NameTemplates, RecordingId};
    use festerm_recording_export::publish_to_directory;

    let root = private_dir();
    let staging = private_dir();
    let destination = private_dir();
    let source = source(root.path());
    let staged = stage_recording(
        &source,
        staging.path(),
        options(),
        &mut FakeFactory::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    fs::write(destination.path().join("demo"), b"keep").unwrap();
    fs::create_dir(destination.path().join("demo-2")).unwrap();
    fs::write(destination.path().join("demo-2").join("keep"), b"keep").unwrap();
    let published = publish_to_directory(
        &staged,
        &directory_handle(destination.path()),
        &NameTemplates::new("demo", "session-{session_index}.mp4").unwrap(),
        RecordingId::new(),
        LocalStart::new(2026, 2, 28, 9, 6, 5).unwrap(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(published.directory_name, "demo-3");
    assert_eq!(published.video_names, ["session-01.mp4", "session-02.mp4"]);
    assert_eq!(fs::read(destination.path().join("demo")).unwrap(), b"keep");
    assert_eq!(
        fs::read(destination.path().join("demo-2").join("keep")).unwrap(),
        b"keep"
    );
    for (video, name) in staged.videos().iter().zip(&published.video_names) {
        assert_eq!(
            fs::read(video.path()).unwrap(),
            fs::read(
                destination
                    .path()
                    .join(&published.directory_name)
                    .join(name)
            )
            .unwrap()
        );
    }
    let appearance: serde_json::Value = serde_json::from_slice(
        &fs::read(
            destination
                .path()
                .join(&published.directory_name)
                .join(festerm_recording_export::APPEARANCE_FILE_NAME),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(appearance["font"], "jet_brains_mono");
    assert_eq!(appearance["font_version"], "2.304");
    assert_eq!(appearance["width"], 320);
    assert_eq!(appearance["height"], 180);
    assert_eq!(appearance["font_faces_sha256"].as_array().unwrap().len(), 4);
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 3);
    assert!(source.exists());
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "windows"
))]
#[test]
fn publication_rejects_changed_staging_and_keeps_source_for_retry() {
    use festerm_recording::{LocalStart, NameTemplates, RecordingId};
    use festerm_recording_export::publish_to_directory;

    let root = private_dir();
    let staging = private_dir();
    let destination = private_dir();
    let source = source(root.path());
    let staged = stage_recording(
        &source,
        staging.path(),
        options(),
        &mut FakeFactory::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    fs::write(staged.videos()[1].path(), b"tampered").unwrap();
    let error = publish_to_directory(
        &staged,
        &directory_handle(destination.path()),
        &NameTemplates::default(),
        RecordingId::new(),
        LocalStart::new(2026, 2, 28, 9, 6, 5).unwrap(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(error.to_string().contains("changed"));
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 0);
    assert!(source.exists());
    assert!(staged.videos()[0].path().exists());
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
#[test]
fn publication_rejects_symlinked_staged_files_and_unsafe_export_roots() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    use festerm_recording::{LocalStart, NameTemplates, RecordingId};
    use festerm_recording_export::publish_to_directory;

    let root = private_dir();
    let staging = private_dir();
    let destination = private_dir();
    let source = source(root.path());
    let staged = stage_recording(
        &source,
        staging.path(),
        options(),
        &mut FakeFactory::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let root_handle = File::open(destination.path()).unwrap();
    let id = RecordingId::new();
    let start = LocalStart::new(2026, 2, 28, 9, 6, 5).unwrap();
    let templates = NameTemplates::default();
    fs::set_permissions(destination.path(), fs::Permissions::from_mode(0o777)).unwrap();
    assert!(publish_to_directory(
        &staged,
        &root_handle,
        &templates,
        id,
        start,
        &AtomicBool::new(false),
    )
    .is_err());
    fs::set_permissions(destination.path(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_file(staged.videos()[1].path()).unwrap();
    symlink(staged.videos()[0].path(), staged.videos()[1].path()).unwrap();
    assert!(publish_to_directory(
        &staged,
        &root_handle,
        &templates,
        id,
        start,
        &AtomicBool::new(false),
    )
    .is_err());
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 0);
    assert!(source.exists());
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "windows"
))]
#[test]
fn source_deletion_requires_the_corresponding_complete_publication() {
    use festerm_recording::{LocalStart, NameTemplates, RecordingId};
    use festerm_recording_export::{delete_source_after_publication, publish_to_directory};

    let root = private_dir();
    let staging = private_dir();
    let destination = private_dir();
    let source = source(root.path());
    let staged = stage_recording(
        &source,
        staging.path(),
        options(),
        &mut FakeFactory::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let root_handle = directory_handle(destination.path());
    let templates = NameTemplates::default();
    let id = RecordingId::new();
    let start = LocalStart::new(2026, 2, 28, 9, 6, 5).unwrap();
    assert!(publish_to_directory(
        &staged,
        &root_handle,
        &templates,
        id,
        start,
        &AtomicBool::new(true),
    )
    .is_err());
    assert!(source.exists());

    let mut published = publish_to_directory(
        &staged,
        &root_handle,
        &templates,
        id,
        start,
        &AtomicBool::new(false),
    )
    .unwrap();
    delete_source_after_publication(&staged, &mut published).unwrap();
    assert!(!source.exists());
    assert!(delete_source_after_publication(&staged, &mut published).is_err());
    for name in &published.video_names {
        assert!(destination
            .path()
            .join(&published.directory_name)
            .join(name)
            .exists());
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "android",
    target_os = "windows"
))]
mod provider_publication {
    use std::{
        collections::HashMap,
        io::{Cursor, Read, Write},
        sync::{atomic::AtomicUsize, Arc, Mutex},
    };

    use festerm_recording::{LocalStart, NameTemplates, RecordingId};
    #[cfg(not(windows))]
    use festerm_recording_export::ProviderJournal;
    use festerm_recording_export::{
        delete_source_after_provider_publication, publish_to_provider, ProviderEntry,
        RecordingProvider,
    };

    use super::*;

    #[derive(Default)]
    struct Provider {
        files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        created: usize,
        fail_on: Option<usize>,
        corrupt_read: bool,
        corrupt_appearance_read: bool,
        strip_recovered_label: bool,
        cancel_during_read: Option<Arc<AtomicBool>>,
        read_calls: Arc<AtomicUsize>,
    }

    struct ProviderReader {
        contents: Cursor<Vec<u8>>,
        cancel: Option<Arc<AtomicBool>>,
        read_calls: Arc<AtomicUsize>,
    }

    impl Read for ProviderReader {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.read_calls.fetch_add(1, Ordering::Relaxed);
            if let Some(cancel) = &self.cancel {
                let limit = bytes.len().min(1);
                let count = self.contents.read(&mut bytes[..limit])?;
                cancel.store(true, Ordering::Release);
                Ok(count)
            } else {
                self.contents.read(bytes)
            }
        }
    }

    struct ProviderWriter {
        id: String,
        files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    }

    impl Write for ProviderWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.files
                .lock()
                .unwrap()
                .get_mut(&self.id)
                .unwrap()
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl RecordingProvider for Provider {
        type Error = io::Error;
        type Writer = ProviderWriter;
        type Reader = ProviderReader;

        fn create_directory(&mut self, name: &str) -> io::Result<ProviderEntry> {
            Ok(ProviderEntry {
                id: "new-directory".into(),
                display_name: if self.strip_recovered_label {
                    "renamed folder".into()
                } else {
                    format!("{name} (1)")
                },
            })
        }

        fn create_video(&mut self, _: &ProviderEntry, name: &str) -> io::Result<ProviderEntry> {
            self.created += 1;
            if self.fail_on == Some(self.created) {
                return Err(io::Error::other("injected provider failure"));
            }
            let id = format!("new-video-{}", self.created);
            self.files.lock().unwrap().insert(id.clone(), Vec::new());
            Ok(ProviderEntry {
                id,
                display_name: name.to_owned(),
            })
        }

        fn create_appearance(
            &mut self,
            directory: &ProviderEntry,
            name: &str,
        ) -> io::Result<ProviderEntry> {
            self.create_video(directory, name)
        }

        fn open_for_write(&mut self, video: &ProviderEntry) -> io::Result<Self::Writer> {
            Ok(ProviderWriter {
                id: video.id.clone(),
                files: self.files.clone(),
            })
        }

        fn open_for_read(&mut self, video: &ProviderEntry) -> io::Result<Self::Reader> {
            let mut bytes = self.files.lock().unwrap().get(&video.id).unwrap().clone();
            if self.corrupt_read
                || (self.corrupt_appearance_read
                    && video.display_name == festerm_recording_export::APPEARANCE_FILE_NAME)
            {
                bytes.push(0);
            }
            Ok(ProviderReader {
                contents: Cursor::new(bytes),
                cancel: self.cancel_during_read.clone(),
                read_calls: self.read_calls.clone(),
            })
        }
    }

    #[test]
    fn cancellation_interrupts_provider_video_verification() {
        let root = private_dir();
        let staging = private_dir();
        let source = source(root.path());
        let staged = stage_recording(
            &source,
            staging.path(),
            options(),
            &mut FakeFactory::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut provider = Provider {
            cancel_during_read: Some(cancelled.clone()),
            ..Default::default()
        };
        let error = publish_to_provider(
            &staged,
            &mut provider,
            &NameTemplates::new("demo", "session-{session_index}.mp4").unwrap(),
            RecordingId::new(),
            LocalStart::new(2026, 2, 28, 9, 6, 5).unwrap(),
            &cancelled,
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert_eq!(provider.read_calls.load(Ordering::Relaxed), 1);
        assert_eq!(provider.created, 1);
        assert!(source.exists());
    }

    #[test]
    fn provider_cannot_silently_remove_the_recovered_label() {
        use festerm_recording::{
            ExportDestination, RecordingLease, RecordingManifest, RecordingSettings,
            RecordingSource,
        };
        use festerm_recording_export::{
            ExportBackend, ExportPublication, ExportTask, ProviderPublication, PublicationRequest,
            StagedRecording,
        };

        struct Backend(Arc<Mutex<Provider>>);
        struct Publication(ProviderPublication);

        impl ExportBackend for Backend {
            type Publication = Publication;

            fn preflight(&self, _: &ExportDestination) -> Result<(), String> {
                Ok(())
            }

            fn publish(&self, request: PublicationRequest<'_>) -> Result<Publication, String> {
                let mut provider = self.0.lock().unwrap();
                publish_to_provider(
                    request.staged,
                    &mut *provider,
                    request.templates,
                    request.id,
                    request.start,
                    request.cancelled,
                    |_| Ok(()),
                )
                .map(Publication)
                .map_err(|error| error.to_string())
            }
        }

        impl ExportPublication for Publication {
            fn names(&self) -> (String, Vec<String>) {
                (
                    self.0.directory.display_name.clone(),
                    self.0
                        .videos
                        .iter()
                        .map(|video| video.display_name.clone())
                        .collect(),
                )
            }

            fn delete_source(&mut self, staged: &StagedRecording) -> Result<(), String> {
                delete_source_after_provider_publication(staged, &mut self.0)
                    .map_err(|error| error.to_string())
            }

            fn finish(self) -> Result<(), String> {
                Ok(())
            }
        }

        let root = private_dir();
        let path = source(root.path());
        let mut interrupted = fs::read(&path).unwrap();
        interrupted.pop();
        fs::write(&path, &interrupted).unwrap();
        let id = RecordingId::parse(
            path.file_stem()
                .unwrap()
                .to_str()
                .unwrap()
                .strip_prefix("recording-")
                .unwrap(),
        )
        .unwrap();
        let settings = RecordingSettings {
            destination: Some(ExportDestination::AndroidTree("content://test/tree".into())),
            ..RecordingSettings::default()
        };
        let manifest = RecordingManifest::create(
            root.path(),
            id,
            LocalStart::new(2026, 1, 2, 3, 4, 5).unwrap(),
            &settings,
        )
        .unwrap();
        let lease = RecordingLease::acquire(root.path()).unwrap();
        let source = RecordingSource::recover(root.path(), &lease)
            .unwrap()
            .remove(0);
        drop(lease);
        let provider = Arc::new(Mutex::new(Provider {
            strip_recovered_label: true,
            ..Default::default()
        }));
        let (wake, ready) = mpsc::channel();
        let task = ExportTask::start_recovery(
            root.path().to_owned(),
            source,
            manifest,
            settings,
            FakeFactory::default(),
            Backend(provider.clone()),
            Arc::new(move || {
                let _ = wake.send(());
            }),
        )
        .unwrap();
        let error = loop {
            if let Some(result) = task.poll() {
                break result.unwrap_err();
            }
            ready.recv_timeout(Duration::from_secs(60)).unwrap();
        };
        assert!(
            error.contains("removed the recovered-prefix label"),
            "{error}"
        );
        assert_eq!(provider.lock().unwrap().created, 0);
        assert_eq!(fs::read(path).unwrap(), interrupted);
    }

    #[test]
    fn publishes_all_videos_before_allowing_source_deletion() {
        let root = private_dir();
        let staging = private_dir();
        let source = source(root.path());
        let staged = stage_recording(
            &source,
            staging.path(),
            options(),
            &mut FakeFactory::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let mut provider = Provider::default();
        provider
            .files
            .lock()
            .unwrap()
            .insert("existing".into(), b"keep".to_vec());
        let mut remembered = None;
        let mut published = publish_to_provider(
            &staged,
            &mut provider,
            &NameTemplates::new("demo", "session-{session_index}.mp4").unwrap(),
            RecordingId::new(),
            LocalStart::new(2026, 2, 28, 9, 6, 5).unwrap(),
            &AtomicBool::new(false),
            |directory| {
                remembered = Some(directory.clone());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(remembered.as_ref(), Some(&published.directory));
        assert_eq!(published.directory.display_name, "demo (1)");
        assert_eq!(published.videos.len(), 2);
        assert_eq!(
            published.appearance.display_name,
            festerm_recording_export::APPEARANCE_FILE_NAME
        );
        let appearance: serde_json::Value =
            serde_json::from_slice(&provider.files.lock().unwrap()[&published.appearance.id])
                .unwrap();
        assert_eq!(appearance["font"], "jet_brains_mono");
        assert_eq!(appearance["width"], 320);
        assert_eq!(appearance["height"], 180);
        assert_eq!(provider.files.lock().unwrap()["existing"], b"keep");
        for (video, destination) in staged.videos().iter().zip(&published.videos) {
            assert_eq!(
                provider.files.lock().unwrap()[&destination.id],
                fs::read(video.path()).unwrap()
            );
        }
        assert!(source.exists());
        delete_source_after_provider_publication(&staged, &mut published).unwrap();
        assert!(!source.exists());
        assert!(delete_source_after_provider_publication(&staged, &mut published).is_err());
    }

    #[test]
    fn provider_failure_reports_partial_directory_and_retains_source() {
        let root = private_dir();
        let staging = private_dir();
        let source = source(root.path());
        let staged = stage_recording(
            &source,
            staging.path(),
            options(),
            &mut FakeFactory::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        for (fail_on, corrupt_read, corrupt_appearance_read) in [
            (Some(2), false, false),
            (None, true, false),
            (Some(3), false, false),
            (None, false, true),
        ] {
            let mut provider = Provider {
                fail_on,
                corrupt_read,
                corrupt_appearance_read,
                ..Default::default()
            };
            let mut remembered = None;
            let error = publish_to_provider(
                &staged,
                &mut provider,
                &NameTemplates::default(),
                RecordingId::new(),
                LocalStart::new(2026, 2, 28, 9, 6, 5).unwrap(),
                &AtomicBool::new(false),
                |directory| {
                    remembered = Some(directory.clone());
                    Ok(())
                },
            )
            .unwrap_err();
            assert_eq!(error.partial_directory, remembered);
            assert!(!provider.files.lock().unwrap().is_empty());
            assert!(source.exists());
        }
    }

    #[test]
    fn no_video_is_created_until_the_partial_directory_is_remembered() {
        let root = private_dir();
        let staging = private_dir();
        let source = source(root.path());
        let staged = stage_recording(
            &source,
            staging.path(),
            options(),
            &mut FakeFactory::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let mut provider = Provider::default();
        let error = publish_to_provider(
            &staged,
            &mut provider,
            &NameTemplates::default(),
            RecordingId::new(),
            LocalStart::new(2026, 2, 28, 9, 6, 5).unwrap(),
            &AtomicBool::new(false),
            |_| Err(io::Error::other("journal unavailable")),
        )
        .unwrap_err();
        assert!(error.partial_directory.is_some());
        assert_eq!(provider.created, 0);
        assert!(source.exists());
    }

    #[cfg(not(windows))]
    #[test]
    fn provider_journal_recovers_intent_partial_and_verified_publication() {
        let root = private_dir();
        let staging = private_dir();
        let journals = private_dir();
        let source = source(root.path());
        let staged = stage_recording(
            &source,
            staging.path(),
            options(),
            &mut FakeFactory::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let templates = NameTemplates::new("demo", "session-{session_index}.mp4").unwrap();
        let start = LocalStart::new(2026, 2, 28, 9, 6, 5).unwrap();
        let mut journal =
            ProviderJournal::begin(journals.path(), &staged, "tree-uri", &templates, start)
                .unwrap();
        let recovered = ProviderJournal::recover(journals.path()).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].record().destination_id(), "tree-uri");
        assert_eq!(recovered[0].record().requested_directory(), "demo");
        assert!(recovered[0].record().directory().is_none());
        assert!(recovered[0].record().published_videos().is_none());
        assert!(source
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains(&recovered[0].record().recording_id().to_string()));
        drop(recovered);

        let mut provider = Provider::default();
        let mut published = publish_to_provider(
            &staged,
            &mut provider,
            &templates,
            journal.record().recording_id(),
            start,
            &AtomicBool::new(false),
            |entry| journal.remember_directory(entry),
        )
        .unwrap();
        let recovered = ProviderJournal::recover(journals.path()).unwrap();
        assert_eq!(
            recovered[0].record().directory(),
            Some(&published.directory)
        );
        assert!(recovered[0].record().published_videos().is_none());
        drop(recovered);

        journal.mark_published(&published).unwrap();
        let recovered = ProviderJournal::recover(journals.path()).unwrap();
        assert_eq!(
            recovered[0].record().published_videos(),
            Some(published.videos.as_slice())
        );
        assert_eq!(
            recovered[0].record().published_appearance(),
            Some(&published.appearance)
        );
        drop(recovered);
        delete_source_after_provider_publication(&staged, &mut published).unwrap();
        journal.remove().unwrap();
        assert!(!source.exists());
        assert!(ProviderJournal::recover(journals.path())
            .unwrap()
            .is_empty());
    }

    #[cfg(not(windows))]
    #[test]
    fn provider_journal_rejects_replaced_or_corrupt_entries() {
        let root = private_dir();
        let staging = private_dir();
        let journals = private_dir();
        let source = source(root.path());
        let staged = stage_recording(
            &source,
            staging.path(),
            options(),
            &mut FakeFactory::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let journal = ProviderJournal::begin(
            journals.path(),
            &staged,
            "tree-uri",
            &NameTemplates::default(),
            LocalStart::new(2026, 2, 28, 9, 6, 5).unwrap(),
        )
        .unwrap();
        let entry = fs::read_dir(journals.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::write(&entry, b"not a journal").unwrap();
        assert!(ProviderJournal::recover(journals.path()).is_err());
        assert!(journal.remove().is_err());
        assert!(entry.exists());
        assert!(source.exists());
    }
}

#[cfg(windows)]
#[test]
fn windows_staging_refuses_a_multiply_linked_source() {
    let root = private_dir();
    let staging = private_dir();
    let source = source(root.path());
    let extra_name = root.path().join("additional-source.fstrec");
    fs::hard_link(&source, &extra_name).unwrap();
    let error = stage_recording(
        &source,
        staging.path(),
        options(),
        &mut FakeFactory::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap_err();
    assert!(error.to_string().contains("private"));
    assert!(source.exists());
    assert!(extra_name.exists());
    assert_eq!(fs::read_dir(staging.path()).unwrap().count(), 0);
}

#[cfg(windows)]
#[test]
fn windows_source_deletion_retains_a_changed_source() {
    use festerm_recording::{LocalStart, NameTemplates, RecordingId};
    use festerm_recording_export::{delete_source_after_publication, publish_to_directory};

    let root = private_dir();
    let staging = private_dir();
    let destination = private_dir();
    let source = source(root.path());
    let staged = stage_recording(
        &source,
        staging.path(),
        options(),
        &mut FakeFactory::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let mut published = publish_to_directory(
        &staged,
        &directory_handle(destination.path()),
        &NameTemplates::default(),
        RecordingId::new(),
        LocalStart::new(2026, 2, 28, 9, 6, 5).unwrap(),
        &AtomicBool::new(false),
    )
    .unwrap();
    OpenOptions::new()
        .append(true)
        .open(&source)
        .unwrap()
        .write_all(b"changed")
        .unwrap();
    assert!(delete_source_after_publication(&staged, &mut published).is_err());
    assert!(source.exists());
    for name in &published.video_names {
        assert!(destination
            .path()
            .join(&published.directory_name)
            .join(name)
            .exists());
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
#[test]
fn source_deletion_rejects_changed_replaced_and_mismatched_sources() {
    use std::os::unix::fs::PermissionsExt;

    use festerm_recording::{LocalStart, NameTemplates, RecordingId};
    use festerm_recording_export::{delete_source_after_publication, publish_to_directory};

    let root = private_dir();
    let staging = private_dir();
    let other_staging = private_dir();
    let destination = private_dir();
    let source_path = source(root.path());
    let other_source = source(root.path());
    let staged = stage_recording(
        &source_path,
        staging.path(),
        options(),
        &mut FakeFactory::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let other = stage_recording(
        &other_source,
        other_staging.path(),
        options(),
        &mut FakeFactory::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let root_handle = File::open(destination.path()).unwrap();
    let templates = NameTemplates::default();
    let start = LocalStart::new(2026, 2, 28, 9, 6, 5).unwrap();
    let mut published = publish_to_directory(
        &staged,
        &root_handle,
        &templates,
        RecordingId::new(),
        start,
        &AtomicBool::new(false),
    )
    .unwrap();
    let mut replacement_published = publish_to_directory(
        &staged,
        &root_handle,
        &templates,
        RecordingId::new(),
        start,
        &AtomicBool::new(false),
    )
    .unwrap();
    let mut other_published = publish_to_directory(
        &other,
        &root_handle,
        &templates,
        RecordingId::new(),
        start,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(delete_source_after_publication(&staged, &mut other_published).is_err());
    assert!(source_path.exists());
    assert!(other_source.exists());

    let mut bytes = fs::read(&source_path).unwrap();
    bytes[10] ^= 1;
    fs::write(&source_path, &bytes).unwrap();
    assert!(delete_source_after_publication(&staged, &mut published).is_err());
    assert_eq!(fs::read(&source_path).unwrap(), bytes);

    let saved = root.path().join("moved-source.fstrec");
    fs::rename(&source_path, &saved).unwrap();
    fs::write(&source_path, &bytes).unwrap();
    fs::set_permissions(&source_path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(delete_source_after_publication(&staged, &mut replacement_published).is_err());
    assert_eq!(fs::read(&source_path).unwrap(), bytes);
    assert_eq!(fs::read(saved).unwrap(), bytes);
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
#[test]
fn staging_refuses_a_symlinked_or_nonprivate_source() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let root = private_dir();
    let staging = private_dir();
    let source = source(root.path());
    let link = root.path().join("source-link.fstrec");
    symlink(&source, &link).unwrap();
    assert!(stage_recording(
        &link,
        staging.path(),
        options(),
        &mut FakeFactory::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .is_err());
    fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(stage_recording(
        &source,
        staging.path(),
        options(),
        &mut FakeFactory::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .is_err());
    assert!(source.exists());
    assert_eq!(fs::read_dir(staging.path()).unwrap().count(), 0);
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(stage_recording(
        &source,
        staging.path(),
        options(),
        &mut FakeFactory::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .is_err());
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[test]
fn avfoundation_stages_a_real_mp4_for_each_stream() {
    use festerm_recording_export::AppleEncoderFactory;

    let root = private_dir();
    let staging = private_dir();
    let source = source(root.path());
    let result = stage_recording(
        &source,
        staging.path(),
        options(),
        &mut AppleEncoderFactory,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(result.videos().len(), 2);
    for video in result.videos() {
        let bytes = fs::read(video.path()).unwrap();
        assert!(bytes.len() > 1000);
        assert!(bytes.windows(4).any(|window| window == b"ftyp"));
        assert!(bytes.windows(4).any(|window| window == b"moov"));
    }
    assert!(source.exists());
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires system GStreamer H.264/MP4 plugins and ffmpeg/ffprobe"]
fn gstreamer_stages_readable_synchronized_mp4s() {
    check_native_synchronized_export(festerm_recording_export::LinuxEncoderFactory);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires AVFoundation and ffmpeg/ffprobe"]
fn avfoundation_stages_readable_synchronized_mp4s() {
    check_native_synchronized_export(festerm_recording_export::AppleEncoderFactory);
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "requires Windows Media Foundation and ffmpeg/ffprobe"]
fn windows_stages_readable_synchronized_mp4s() {
    check_native_synchronized_export(festerm_recording_export::WindowsEncoderFactory);
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn check_native_synchronized_export<F: EncoderFactory>(mut factory: F) {
    use std::process::Command;

    let root = private_dir();
    let staging = private_dir();
    let source = source(root.path());
    let result = stage_recording(
        &source,
        staging.path(),
        options(),
        &mut factory,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(result.videos().len(), 2);

    let mut frames = Vec::new();
    for video in result.videos() {
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,width,height,avg_frame_rate,nb_frames",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(video.path())
            .output()
            .expect("ffprobe is required for this native test");
        assert!(
            probe.status.success(),
            "{}",
            String::from_utf8_lossy(&probe.stderr)
        );
        let properties = String::from_utf8(probe.stdout).unwrap();
        for expected in [
            "codec_name=h264".to_owned(),
            "width=320".to_owned(),
            "height=180".to_owned(),
            "avg_frame_rate=30/1".to_owned(),
            format!("nb_frames={}", result.frames_per_video()),
        ] {
            assert!(
                properties.lines().any(|line| line == expected),
                "{properties}"
            );
        }

        let audio = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "a",
                "-show_entries",
                "stream=codec_type",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
            ])
            .arg(video.path())
            .output()
            .expect("ffprobe is required for this native test");
        assert!(
            audio.status.success(),
            "{}",
            String::from_utf8_lossy(&audio.stderr)
        );
        assert!(
            audio.stdout.is_empty(),
            "export unexpectedly contains audio"
        );

        let decoded = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(video.path())
            .args([
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "pipe:1",
            ])
            .output()
            .expect("ffmpeg is required for this native test");
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        assert_eq!(decoded.stdout.len(), 320 * 180 * 3);
        frames.push(decoded.stdout);
    }

    let visibly_different = frames[0]
        .as_chunks::<3>()
        .0
        .iter()
        .zip(frames[1].as_chunks::<3>().0)
        .filter(|(first, second)| {
            first
                .iter()
                .zip(second.iter())
                .any(|(left, right)| left.abs_diff(*right) > 30)
        })
        .count();
    assert!(
        visibly_different > 40,
        "terminal text was not distinguishable"
    );
    assert!(source.exists());
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod worker_tests {
    use std::sync::Arc;

    use festerm_recording::{
        ExportDestination, LocalStart, PlaybackSpeed, RecordingFont, RecordingId, RecordingLease,
        RecordingManifest, RecordingSettings, RecordingSource, RecordingTheme,
    };
    use festerm_recording_export::{
        DirectoryBackend, ExportBackend, ExportCancellation, ExportControl, ExportPublication,
        ExportResult, ExportTask, PrepareTask, PreviewTask, PreviewedExport, PublicationRequest,
        PublishedRecording, StagedRecording,
    };

    use super::*;

    fn settings(destination: &Path, delete_source: bool) -> RecordingSettings {
        RecordingSettings {
            destination: Some(ExportDestination::DesktopPath(destination.to_owned())),
            delete_source_after_export: delete_source,
            ..RecordingSettings::default()
        }
    }

    fn manifest(root: &Path, source: &Path, settings: &RecordingSettings) -> RecordingManifest {
        let stem = source.file_stem().unwrap().to_str().unwrap();
        let id = RecordingId::parse(stem.strip_prefix("recording-").unwrap()).unwrap();
        RecordingManifest::create(
            root,
            id,
            LocalStart::new(2026, 1, 2, 3, 4, 5).unwrap(),
            settings,
        )
        .unwrap()
    }

    fn wait(task: &ExportTask, ready: &mpsc::Receiver<()>) -> Result<ExportResult, String> {
        loop {
            if let Some(result) = task.poll() {
                return result;
            }
            ready.recv_timeout(Duration::from_secs(60)).unwrap();
        }
    }

    fn wake() -> (Arc<dyn Fn() + Send + Sync>, mpsc::Receiver<()>) {
        let (send, ready) = mpsc::channel();
        (
            Arc::new(move || {
                let _ = send.send(());
            }),
            ready,
        )
    }

    #[test]
    fn worker_publishes_all_streams_before_applying_retention() {
        for delete_source in [false, true] {
            let root = private_dir();
            let destination = private_dir();
            let path = source(root.path());
            let options = settings(destination.path(), delete_source);
            let manifest = manifest(root.path(), &path, &options);
            let lease = Arc::new(RecordingLease::acquire(root.path()).unwrap());
            let (wake, ready) = wake();
            let task = ExportTask::start(
                root.path().to_owned(),
                path.clone(),
                manifest,
                lease,
                FakeFactory::default(),
                DirectoryBackend,
                wake,
            )
            .unwrap();
            let result = wait(&task, &ready).unwrap();
            assert_eq!(result.videos.len(), 2);
            assert_eq!(result.retained_source, !delete_source);
            assert_eq!(path.exists(), !delete_source);
            assert!(!fs::read_dir(root.path()).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("recording-stage-")
            }));
            assert_eq!(
                RecordingManifest::recover(root.path()).unwrap().len(),
                usize::from(!delete_source)
            );
            for video in result.videos {
                assert!(destination
                    .path()
                    .join(&result.directory)
                    .join(video)
                    .is_file());
            }
        }
    }

    struct OptionalFontBackend([Vec<u8>; 4]);

    impl ExportBackend for OptionalFontBackend {
        type Publication = PublishedRecording;

        fn recording_font_faces(
            &self,
            font: RecordingFont,
            _: &AtomicBool,
        ) -> Result<Option<[Vec<u8>; 4]>, String> {
            assert_eq!(font, RecordingFont::MapleMono);
            Ok(Some(self.0.clone()))
        }

        fn preflight(&self, destination: &ExportDestination) -> Result<(), String> {
            DirectoryBackend.preflight(destination)
        }

        fn publish(&self, request: PublicationRequest<'_>) -> Result<Self::Publication, String> {
            DirectoryBackend.publish(request)
        }
    }

    struct AppearanceFactory;
    struct AppearanceEncoder(FakeEncoder);

    impl EncoderFactory for AppearanceFactory {
        type Encoder = AppearanceEncoder;
        type Error = io::Error;

        fn create(
            &mut self,
            path: &Path,
            size: RecordingFrameSize,
        ) -> Result<Self::Encoder, Self::Error> {
            assert_eq!((size.width(), size.height()), (320, 180));
            FakeFactory::default()
                .create(path, size)
                .map(AppearanceEncoder)
        }
    }

    impl VideoEncoder for AppearanceEncoder {
        type Error = io::Error;

        fn append(&mut self, rgba: &[u8]) -> Result<(), Self::Error> {
            assert_eq!(rgba.len(), 320 * 180 * 4);
            assert_eq!(&rgba[..4], &[0xf7, 0xf8, 0xfa, 255]);
            self.0.append(rgba)
        }

        fn finish(&mut self) -> Result<(), Self::Error> {
            self.0.finish()
        }
    }

    fn maple_faces() -> [Vec<u8>; 4] {
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/maple-mono");
        [
            "MapleMono-Regular.ttf",
            "MapleMono-Bold.ttf",
            "MapleMono-Italic.ttf",
            "MapleMono-BoldItalic.ttf",
        ]
        .map(|name| fs::read(assets.join(name)).unwrap())
    }

    #[test]
    fn selected_appearance_is_used_for_every_stream_and_published_with_its_identity() {
        let root = private_dir();
        let destination = private_dir();
        let path = source(root.path());
        let mut options = settings(destination.path(), false);
        options.appearance.font = RecordingFont::MapleMono;
        options.appearance.theme = RecordingTheme::FesTermLight;
        options.appearance.font_size_points = 16;
        options.appearance.width = 320;
        options.appearance.height = 180;
        let manifest = manifest(root.path(), &path, &options);
        let lease = Arc::new(RecordingLease::acquire(root.path()).unwrap());
        let (wake, ready) = wake();
        let task = ExportTask::start(
            root.path().to_owned(),
            path.clone(),
            manifest,
            lease,
            AppearanceFactory,
            OptionalFontBackend(maple_faces()),
            wake,
        )
        .unwrap();
        let result = wait(&task, &ready).unwrap();
        assert_eq!(result.videos.len(), 2);
        assert!(path.exists());
        let metadata: serde_json::Value = serde_json::from_slice(
            &fs::read(
                destination
                    .path()
                    .join(result.directory)
                    .join(festerm_recording_export::APPEARANCE_FILE_NAME),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(metadata["font"], "maple_mono");
        assert_eq!(metadata["font_version"], "7.9");
        assert_eq!(metadata["theme"], "fes_term_light");
        assert_eq!(metadata["font_size_points"], 16.0);
        assert_eq!(metadata["width"], 320);
        assert_eq!(metadata["height"], 180);
        assert_eq!(metadata["recovered_prefix"], false);
    }

    #[test]
    fn unavailable_selected_font_retains_source_without_publishing() {
        let root = private_dir();
        let destination = private_dir();
        let path = source(root.path());
        let mut options = settings(destination.path(), true);
        options.appearance.font = RecordingFont::IosevkaTerm;
        let manifest = manifest(root.path(), &path, &options);
        let lease = Arc::new(RecordingLease::acquire(root.path()).unwrap());
        let (wake, ready) = wake();
        let task = ExportTask::start(
            root.path().to_owned(),
            path.clone(),
            manifest,
            lease,
            FakeFactory::default(),
            DirectoryBackend,
            wake,
        )
        .unwrap();
        assert!(wait(&task, &ready).unwrap_err().contains("unavailable"));
        assert!(path.exists());
        assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 0);
    }

    #[test]
    fn corrupted_selected_font_is_not_substituted_or_published() {
        let root = private_dir();
        let destination = private_dir();
        let path = source(root.path());
        let mut options = settings(destination.path(), true);
        options.appearance.font = RecordingFont::MapleMono;
        let mut faces = maple_faces();
        faces[0][0] ^= 1;
        let manifest = manifest(root.path(), &path, &options);
        let lease = Arc::new(RecordingLease::acquire(root.path()).unwrap());
        let (wake, ready) = wake();
        let task = ExportTask::start(
            root.path().to_owned(),
            path.clone(),
            manifest,
            lease,
            FakeFactory::default(),
            OptionalFontBackend(faces),
            wake,
        )
        .unwrap();
        assert!(wait(&task, &ready)
            .unwrap_err()
            .contains("failed verification"));
        assert!(path.exists());
        assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 0);
    }

    struct RetainingBackend;

    impl ExportBackend for RetainingBackend {
        type Publication = PublishedRecording;

        fn preflight(&self, destination: &ExportDestination) -> Result<(), String> {
            DirectoryBackend.preflight(destination)
        }

        fn publish(&self, request: PublicationRequest<'_>) -> Result<Self::Publication, String> {
            DirectoryBackend.publish(request)
        }
    }

    struct AttemptRecoveryDeletion;

    impl ExportBackend for AttemptRecoveryDeletion {
        type Publication = PublishedRecording;

        fn allows_automatic_source_deletion(&self) -> bool {
            true
        }

        fn preflight(&self, destination: &ExportDestination) -> Result<(), String> {
            DirectoryBackend.preflight(destination)
        }

        fn publish(&self, request: PublicationRequest<'_>) -> Result<Self::Publication, String> {
            let staged = request.staged;
            let mut publication = DirectoryBackend.publish(request)?;
            let error =
                festerm_recording_export::delete_source_after_publication(staged, &mut publication)
                    .expect_err("recovered source deletion must be refused");
            if !error.to_string().contains("incomplete source") {
                return Err(error.to_string());
            }
            Ok(publication)
        }
    }

    #[test]
    fn retaining_backend_preserves_sources_even_with_old_deletion_settings() {
        for recovery in [false, true] {
            let root = private_dir();
            let destination = private_dir();
            let path = source(root.path());
            let options = settings(destination.path(), true);
            let manifest = manifest(root.path(), &path, &options);
            let lease = RecordingLease::acquire(root.path()).unwrap();
            let (wake, ready) = wake();
            let task = if recovery {
                let source = RecordingSource::recover(root.path(), &lease)
                    .unwrap()
                    .remove(0);
                drop(lease);
                ExportTask::start_recovery(
                    root.path().to_owned(),
                    source,
                    manifest,
                    options,
                    FakeFactory::default(),
                    RetainingBackend,
                    wake,
                )
            } else {
                ExportTask::start(
                    root.path().to_owned(),
                    path.clone(),
                    manifest,
                    Arc::new(lease),
                    FakeFactory::default(),
                    RetainingBackend,
                    wake,
                )
            }
            .unwrap();
            let result = wait(&task, &ready).unwrap();
            assert!(result.retained_source);
            assert!(path.exists());
            assert_eq!(RecordingManifest::recover(root.path()).unwrap().len(), 1);
        }
    }

    #[test]
    fn preview_binds_the_confirmed_settings_and_source_to_export() {
        for replace_source in [false, true] {
            let root = private_dir();
            let destination = private_dir();
            let path = source(root.path());
            let saved = settings(destination.path(), true);
            let manifest = manifest(root.path(), &path, &saved);
            let lease = Arc::new(RecordingLease::acquire(root.path()).unwrap());
            let mut selected = saved.clone();
            selected.delete_source_after_export = false;
            selected.speed = PlaybackSpeed::Double;
            let (notify, ready) = wake();
            let preview_task = PreviewTask::start(
                root.path().to_owned(),
                path.clone(),
                manifest.id(),
                lease.clone(),
                selected.clone(),
                ExportControl::new(Arc::new(ExportCancellation::default()), notify),
            )
            .unwrap();
            let preview = loop {
                if let Some(result) = preview_task.poll() {
                    break result.unwrap();
                }
                ready.recv_timeout(Duration::from_secs(10)).unwrap();
            };
            assert!(preview.matches(&selected));
            assert!(!preview.matches(&saved));
            assert_eq!(
                preview.duration(),
                festerm_recording::PlaybackPlan::inspect(
                    File::open(&path).unwrap(),
                    selected.validate().unwrap().pacing,
                )
                .unwrap()
                .duration()
            );
            drop(preview_task);
            if replace_source {
                let replacement = source_with_first(root.path(), b"other");
                fs::copy(replacement, &path).unwrap();
            }
            let (notify, ready) = wake();
            let task = ExportTask::start_previewed(
                PreviewedExport {
                    root: root.path().to_owned(),
                    source: path.clone(),
                    manifest,
                    lease,
                    preview,
                },
                FakeFactory::default(),
                DirectoryBackend,
                ExportControl::new(Arc::new(ExportCancellation::default()), notify),
            )
            .unwrap();
            if replace_source {
                let error = wait(&task, &ready).unwrap_err();
                assert!(error.contains("changed"), "{error}");
                assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 0);
            } else {
                assert!(wait(&task, &ready).unwrap().retained_source);
            }
            assert!(path.exists());
        }
    }

    #[test]
    fn cancelled_blocked_preflight_releases_the_ui_and_does_not_hold_storage() {
        let root = private_dir();
        let destination = private_dir();
        let options = settings(destination.path(), true);
        let (entered, waiting) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let (wake, ready) = wake();
        let mut task = PrepareTask::start(
            root.path().to_owned(),
            options,
            move |_| {
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(60)).unwrap();
                Ok(())
            },
            wake,
        )
        .unwrap();
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        task.cancel();
        assert!(matches!(task.poll(), Some(Err(error)) if error.contains("cancelled")));
        let lease = RecordingLease::acquire(root.path()).unwrap();
        assert!(RecordingSource::recover(root.path(), &lease)
            .unwrap()
            .is_empty());
        drop(lease);
        drop(task);
        release.send(()).unwrap();
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(RecordingManifest::recover(root.path()).unwrap().is_empty());
        let lease = RecordingLease::acquire(root.path()).unwrap();
        assert!(RecordingSource::recover(root.path(), &lease)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn worker_retains_source_on_encoding_and_publication_failure() {
        for fail_encoder in [true, false] {
            let root = private_dir();
            let destination = private_dir();
            let path = source(root.path());
            let options = settings(destination.path(), true);
            let manifest = manifest(root.path(), &path, &options);
            let lease = Arc::new(RecordingLease::acquire(root.path()).unwrap());
            let (wake, ready) = wake();
            let task = ExportTask::start(
                root.path().to_owned(),
                path.clone(),
                manifest,
                lease,
                FakeFactory {
                    fail_on: fail_encoder.then_some(2),
                    ..Default::default()
                },
                MaybeFailPublication(!fail_encoder),
                wake,
            )
            .unwrap();
            let error = wait(&task, &ready).unwrap_err();
            assert!(error.contains("source was retained"), "{error}");
            assert!(path.exists());
            assert_eq!(RecordingManifest::recover(root.path()).unwrap().len(), 1);
            assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 0);
        }
    }

    struct MaybeFailPublication(bool);

    impl ExportBackend for MaybeFailPublication {
        type Publication = PublishedRecording;

        fn preflight(&self, destination: &ExportDestination) -> Result<(), String> {
            DirectoryBackend.preflight(destination)
        }

        fn publish(&self, request: PublicationRequest<'_>) -> Result<Self::Publication, String> {
            if self.0 {
                Err("injected publication failure".to_owned())
            } else {
                DirectoryBackend.publish(request)
            }
        }
    }

    struct ReplaceBeforeStaging {
        source: PathBuf,
        replacement: Vec<u8>,
        published: Arc<AtomicBool>,
    }

    impl ExportBackend for ReplaceBeforeStaging {
        type Publication = PublishedRecording;

        fn preflight(&self, destination: &ExportDestination) -> Result<(), String> {
            fs::write(&self.source, &self.replacement).map_err(|error| error.to_string())?;
            DirectoryBackend.preflight(destination)
        }

        fn publish(&self, request: PublicationRequest<'_>) -> Result<Self::Publication, String> {
            self.published.store(true, Ordering::Release);
            DirectoryBackend.publish(request)
        }
    }

    #[test]
    fn recovery_rejects_a_valid_source_changed_after_inspection() {
        let root = private_dir();
        let other = private_dir();
        let destination = private_dir();
        let path = source(root.path());
        let replacement = fs::read(source_with_first(other.path(), b"different")).unwrap();
        let options = settings(destination.path(), true);
        let manifest = manifest(root.path(), &path, &options);
        let lease = RecordingLease::acquire(root.path()).unwrap();
        let source = RecordingSource::recover(root.path(), &lease)
            .unwrap()
            .remove(0);
        drop(lease);
        let published = Arc::new(AtomicBool::new(false));
        let (wake, ready) = wake();
        let task = ExportTask::start_recovery(
            root.path().to_owned(),
            source,
            manifest,
            options,
            FakeFactory::default(),
            ReplaceBeforeStaging {
                source: path.clone(),
                replacement,
                published: published.clone(),
            },
            wake,
        )
        .unwrap();
        let error = wait(&task, &ready).unwrap_err();
        assert!(
            error.contains("changed after library inspection"),
            "{error}"
        );
        assert!(!published.load(Ordering::Acquire));
        assert!(path.exists());
        assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 0);
    }

    #[test]
    fn recovered_prefix_is_labeled_and_never_deletes_the_original() {
        let root = private_dir();
        let destination = private_dir();
        let path = source(root.path());
        let mut interrupted = fs::read(&path).unwrap();
        interrupted.pop();
        fs::write(&path, &interrupted).unwrap();
        let options = settings(destination.path(), true);
        let manifest = manifest(root.path(), &path, &options);
        let lease = RecordingLease::acquire(root.path()).unwrap();
        let source = RecordingSource::recover(root.path(), &lease)
            .unwrap()
            .remove(0);
        drop(lease);
        let (wake, ready) = wake();
        let task = ExportTask::start_recovery(
            root.path().to_owned(),
            source,
            manifest,
            options,
            FakeFactory::default(),
            AttemptRecoveryDeletion,
            wake,
        )
        .unwrap();
        let result = wait(&task, &ready).unwrap();
        assert!(result.recovered_prefix);
        assert!(result.retained_source);
        assert!(result.directory.contains("-recovered"));
        assert_eq!(result.videos.len(), 2);
        assert_eq!(fs::read(&path).unwrap(), interrupted);
        assert_eq!(RecordingManifest::recover(root.path()).unwrap().len(), 1);
        let appearance: serde_json::Value = serde_json::from_slice(
            &fs::read(
                destination
                    .path()
                    .join(&result.directory)
                    .join(festerm_recording_export::APPEARANCE_FILE_NAME),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(appearance["recovered_prefix"], true);
        for video in result.videos {
            assert!(destination
                .path()
                .join(&result.directory)
                .join(video)
                .is_file());
        }
    }

    #[test]
    fn recovery_refuses_a_changed_tail_even_when_the_prefix_is_unchanged() {
        let root = private_dir();
        let destination = private_dir();
        let path = source(root.path());
        let mut interrupted = fs::read(&path).unwrap();
        interrupted.pop();
        fs::write(&path, &interrupted).unwrap();
        let options = settings(destination.path(), true);
        let manifest = manifest(root.path(), &path, &options);
        let lease = RecordingLease::acquire(root.path()).unwrap();
        let source = RecordingSource::recover(root.path(), &lease)
            .unwrap()
            .remove(0);
        drop(lease);
        let mut replacement = interrupted;
        *replacement.last_mut().unwrap() ^= 1;
        let published = Arc::new(AtomicBool::new(false));
        let (wake, ready) = wake();
        let task = ExportTask::start_recovery(
            root.path().to_owned(),
            source,
            manifest,
            options,
            FakeFactory::default(),
            ReplaceBeforeStaging {
                source: path.clone(),
                replacement,
                published: published.clone(),
            },
            wake,
        )
        .unwrap();
        let error = wait(&task, &ready).unwrap_err();
        assert!(error.contains("changed"), "{error}");
        assert!(!published.load(Ordering::Acquire));
        assert!(path.exists());
        assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 0);
    }

    struct PauseAfterPublication {
        published: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    struct PauseBeforePublication {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl ExportBackend for PauseBeforePublication {
        type Publication = PublishedRecording;

        fn preflight(&self, destination: &ExportDestination) -> Result<(), String> {
            DirectoryBackend.preflight(destination)
        }

        fn publish(&self, request: PublicationRequest<'_>) -> Result<Self::Publication, String> {
            self.entered.send(()).map_err(|error| error.to_string())?;
            self.release
                .recv_timeout(Duration::from_secs(60))
                .map_err(|error| error.to_string())?;
            DirectoryBackend.publish(request)
        }
    }

    #[test]
    fn external_lifecycle_cancellation_before_publication_retains_source() {
        let root = private_dir();
        let destination = private_dir();
        let path = source(root.path());
        let options = settings(destination.path(), true);
        let manifest = manifest(root.path(), &path, &options);
        let lease = Arc::new(RecordingLease::acquire(root.path()).unwrap());
        let (entered, waiting) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let (wake, ready) = wake();
        let cancellation = Arc::new(ExportCancellation::default());
        let task = ExportTask::start_controlled(
            root.path().to_owned(),
            path.clone(),
            manifest,
            lease,
            FakeFactory::default(),
            PauseBeforePublication {
                entered,
                release: resume,
            },
            ExportControl::new(cancellation.clone(), wake),
        )
        .unwrap();
        waiting.recv_timeout(Duration::from_secs(60)).unwrap();
        assert!(cancellation.cancel());
        release.send(()).unwrap();
        let error = wait(&task, &ready).unwrap_err();
        assert!(error.contains("cancelled"), "{error}");
        assert!(path.exists());
        assert_eq!(RecordingManifest::recover(root.path()).unwrap().len(), 1);
        assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 0);
    }

    impl ExportBackend for PauseAfterPublication {
        type Publication = PublishedRecording;

        fn preflight(&self, destination: &ExportDestination) -> Result<(), String> {
            DirectoryBackend.preflight(destination)
        }

        fn publish(&self, request: PublicationRequest<'_>) -> Result<Self::Publication, String> {
            let published = DirectoryBackend.publish(request)?;
            self.published.send(()).map_err(|error| error.to_string())?;
            self.release
                .recv_timeout(Duration::from_secs(60))
                .map_err(|error| error.to_string())?;
            Ok(published)
        }
    }

    #[test]
    fn cancellation_after_publication_retains_source() {
        let root = private_dir();
        let destination = private_dir();
        let path = source(root.path());
        let options = settings(destination.path(), true);
        let manifest = manifest(root.path(), &path, &options);
        let lease = Arc::new(RecordingLease::acquire(root.path()).unwrap());
        let (published, publication_ready) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let (wake, ready) = wake();
        let task = ExportTask::start(
            root.path().to_owned(),
            path.clone(),
            manifest,
            lease,
            FakeFactory::default(),
            PauseAfterPublication {
                published,
                release: resume,
            },
            wake,
        )
        .unwrap();
        publication_ready
            .recv_timeout(Duration::from_secs(60))
            .unwrap();
        assert!(task.cancel());
        release.send(()).unwrap();
        let error = wait(&task, &ready).unwrap_err();
        assert!(error.contains("cancelled after publishing"), "{error}");
        assert!(path.exists());
        assert_eq!(RecordingManifest::recover(root.path()).unwrap().len(), 1);
        assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 1);
    }

    struct PauseDuringDeletion {
        entered: mpsc::Sender<()>,
        release: Arc<std::sync::Mutex<mpsc::Receiver<()>>>,
        finished: Arc<AtomicBool>,
    }

    struct PausedDeletion {
        inner: PublishedRecording,
        entered: mpsc::Sender<()>,
        release: Arc<std::sync::Mutex<mpsc::Receiver<()>>>,
        finished: Arc<AtomicBool>,
    }

    impl ExportBackend for PauseDuringDeletion {
        type Publication = PausedDeletion;

        fn allows_automatic_source_deletion(&self) -> bool {
            true
        }

        fn preflight(&self, destination: &ExportDestination) -> Result<(), String> {
            DirectoryBackend.preflight(destination)
        }

        fn publish(&self, request: PublicationRequest<'_>) -> Result<Self::Publication, String> {
            Ok(PausedDeletion {
                inner: DirectoryBackend.publish(request)?,
                entered: self.entered.clone(),
                release: self.release.clone(),
                finished: self.finished.clone(),
            })
        }
    }

    impl ExportPublication for PausedDeletion {
        fn names(&self) -> (String, Vec<String>) {
            self.inner.names()
        }

        fn delete_source(&mut self, staged: &StagedRecording) -> Result<(), String> {
            self.entered.send(()).map_err(|error| error.to_string())?;
            self.release
                .lock()
                .map_err(|error| error.to_string())?
                .recv_timeout(Duration::from_secs(60))
                .map_err(|error| error.to_string())?;
            self.inner.delete_source(staged)
        }

        fn finish(self) -> Result<(), String> {
            self.finished.store(true, Ordering::Release);
            self.inner.finish()
        }
    }

    #[test]
    fn cancellation_during_committed_deletion_stops_subsequent_cleanup() {
        let root = private_dir();
        let destination = private_dir();
        let path = source(root.path());
        let options = settings(destination.path(), true);
        let manifest = manifest(root.path(), &path, &options);
        let lease = Arc::new(RecordingLease::acquire(root.path()).unwrap());
        let (entered, waiting) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let (wake, ready) = wake();
        let finished = Arc::new(AtomicBool::new(false));
        let cancellation = Arc::new(ExportCancellation::default());
        let task = ExportTask::start_controlled(
            root.path().to_owned(),
            path.clone(),
            manifest,
            lease,
            FakeFactory::default(),
            PauseDuringDeletion {
                entered,
                release: Arc::new(std::sync::Mutex::new(resume)),
                finished: finished.clone(),
            },
            ExportControl::new(cancellation.clone(), wake),
        )
        .unwrap();
        waiting.recv_timeout(Duration::from_secs(60)).unwrap();
        assert!(!cancellation.cancel());
        assert!(cancellation.flag().load(Ordering::Acquire));
        release.send(()).unwrap();
        let error = wait(&task, &ready).unwrap_err();
        assert!(error.contains("cleanup was interrupted"), "{error}");
        assert!(!path.exists());
        assert_eq!(RecordingManifest::recover(root.path()).unwrap().len(), 1);
        assert!(!finished.load(Ordering::Acquire));
    }
}
