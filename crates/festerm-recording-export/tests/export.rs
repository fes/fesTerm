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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
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
    let mut first = Terminal::new(Dimensions::new(20, 3).unwrap()).unwrap();
    let mut second = Terminal::new(Dimensions::new(20, 3).unwrap()).unwrap();
    first.ingest(b"first");
    second.ingest(b"second");
    let (wake, ready) = mpsc::channel();
    let (mut recorder, streams) = Recorder::start(directory, &[&first, &second], move || {
        wake.send(()).unwrap()
    })
    .unwrap();
    let generation = first.screen_generation();
    first.ingest(b" output");
    recorder
        .observe_output(streams[0], b" output", generation, &first)
        .unwrap();
    let generation = second.screen_generation();
    second.ingest(b" output");
    recorder
        .observe_output(streams[1], b" output", generation, &second)
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
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
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 3);
    assert!(source.exists());
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "windows")]
#[test]
#[ignore = "requires Windows Media Foundation and ffmpeg/ffprobe"]
fn windows_stages_readable_synchronized_mp4s() {
    check_native_synchronized_export(festerm_recording_export::WindowsEncoderFactory);
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
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
        .chunks_exact(3)
        .zip(frames[1].chunks_exact(3))
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
