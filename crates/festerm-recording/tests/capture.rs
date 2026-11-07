use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        atomic::AtomicBool,
        mpsc::{self, Receiver},
        Arc,
    },
    time::{Duration, Instant},
};

use festerm_core::{Dimensions, Terminal};
use festerm_recording::{
    ArtifactEvent, ArtifactReader, Completion, FinishReason, Pacing, Playback, PlaybackPlan,
    PreparedRecording, Recorder, RecordingId, SharedRecording, MAX_OUTPUT_BYTES,
};

fn terminal(columns: usize, rows: usize) -> Terminal {
    Terminal::new(Dimensions::new(columns, rows).unwrap()).unwrap()
}

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
    #[cfg(unix)]
    festerm_unix_security::make_private_directory(&fs::File::open(directory.path()).unwrap())
        .unwrap();
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

fn start(
    directory: &Path,
    terminals: &[&Terminal],
) -> (Recorder, Vec<festerm_recording::StreamId>, Receiver<()>) {
    let (wake, ready) = mpsc::channel();
    let (recorder, streams) =
        Recorder::start(directory, terminals, move || wake.send(()).unwrap()).unwrap();
    (recorder, streams, ready)
}

#[cfg(unix)]
#[test]
fn capture_requires_private_storage_without_a_symlink() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let parent = private_dir();
    let storage = parent.path().join("storage");
    std::fs::create_dir(&storage).unwrap();
    std::fs::set_permissions(&storage, std::fs::Permissions::from_mode(0o755)).unwrap();
    let terminal = Terminal::new(Dimensions::new(20, 3).unwrap()).unwrap();
    assert!(Recorder::start(&storage, &[&terminal], || {}).is_err());
    assert_eq!(std::fs::read_dir(&storage).unwrap().count(), 0);

    std::fs::set_permissions(&storage, std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = parent.path().join("link");
    symlink(&storage, &link).unwrap();
    assert!(Recorder::start(&link, &[&terminal], || {}).is_err());
    assert_eq!(std::fs::read_dir(&storage).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn capture_preserves_the_source_when_its_storage_is_replaced() {
    use std::os::unix::fs::PermissionsExt;

    let parent = private_dir();
    let storage = parent.path().join("storage");
    std::fs::create_dir(&storage).unwrap();
    std::fs::set_permissions(&storage, std::fs::Permissions::from_mode(0o700)).unwrap();
    let terminal = Terminal::new(Dimensions::new(20, 3).unwrap()).unwrap();
    let (mut recorder, _, ready) = start(&storage, &[&terminal]);
    let id = recorder.id();
    let moved = parent.path().join("original");
    std::fs::rename(&storage, &moved).unwrap();
    std::fs::create_dir(&storage).unwrap();
    std::fs::set_permissions(&storage, std::fs::Permissions::from_mode(0o700)).unwrap();
    recorder.stop(FinishReason::User).unwrap();
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(
        recorder.poll_completion(),
        Some(Completion::Failed { .. })
    ));
    assert!(moved.join(format!("recording-{id}.partial")).is_file());
    assert_eq!(std::fs::read_dir(&storage).unwrap().count(), 0);
}

fn completion(recorder: &mut Recorder, ready: Receiver<()>) -> Completion {
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    recorder.poll_completion().unwrap()
}

fn completed_path(completion: Completion) -> PathBuf {
    match completion {
        Completion::Complete { path, .. } => path,
        other => panic!("expected a completed recording, got {other:?}"),
    }
}

#[test]
fn active_recording_flushes_its_recent_complete_events() {
    let directory = private_dir();
    let mut terminal = terminal(4, 2);
    let (mut recorder, streams, ready) = start(directory.path(), &[&terminal]);
    let generation = terminal.screen_generation();
    terminal.ingest(b"hi");
    recorder
        .observe_output(streams[0], b"hi", generation, &terminal)
        .unwrap();
    let partial = directory
        .path()
        .join(format!("recording-{}.partial", recorder.id()));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let flushed = fs::File::open(&partial)
            .ok()
            .and_then(|file| ArtifactReader::new(file).ok())
            .is_some_and(|mut reader| {
                matches!(reader.next_event(), Ok(Some(ArtifactEvent::Started { .. })))
                    && matches!(
                        reader.next_event(),
                        Ok(Some(ArtifactEvent::Output { bytes, .. })) if bytes == b"hi"
                    )
            });
        if flushed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "active recording was not flushed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    recorder.stop(FinishReason::User).unwrap();
    assert!(matches!(
        completion(&mut recorder, ready),
        Completion::Complete { .. }
    ));
}

#[test]
fn selected_streams_share_a_timeline_and_replay_applied_mutations() {
    let directory = private_dir();
    let mut first = terminal(24, 2);
    first.ingest(b"HISTORY-PRIVATE\r\nnext\r\nvisible");
    first.ingest(b"\x1b]2;TITLE-PRIVATE\x07");
    first.ingest(b"\x1b]8;;https://private.example/\x07 link\x1b]8;;\x07");
    first.queue_input(b"INPUT-PRIVATE");
    first.queue_reply(b"REPLY-PRIVATE");
    let mut second = terminal(24, 2);
    second.ingest(b"second session");

    let (mut recorder, streams, ready) = start(directory.path(), &[&first, &second]);
    let generation = first.screen_generation();
    first.ingest(b" more");
    recorder
        .observe_output(streams[0], b" more", generation, &first)
        .unwrap();
    let generation = second.screen_generation();
    second.ingest(b"\r\nupdated");
    recorder
        .observe_output(streams[1], b"\r\nupdated", generation, &second)
        .unwrap();
    first.resize(Dimensions::new(18, 3).unwrap()).unwrap();
    recorder.observe_resize(streams[0], &first).unwrap();
    for output in [
        b"\x1b[?1049halternate".as_slice(),
        b"\x1b[?1049l returned".as_slice(),
    ] {
        let generation = first.screen_generation();
        first.ingest(output);
        recorder
            .observe_output(streams[0], output, generation, &first)
            .unwrap();
    }
    recorder.end_stream(streams[1]).unwrap();
    recorder.stop(FinishReason::User).unwrap();
    let path = completed_path(completion(&mut recorder, ready));
    assert_eq!(
        path.file_name().unwrap().to_string_lossy(),
        format!("recording-{}.fstrec", recorder.id())
    );
    let bytes = fs::read(&path).unwrap();
    for secret in [
        b"HISTORY-PRIVATE".as_slice(),
        b"INPUT-PRIVATE".as_slice(),
        b"REPLY-PRIVATE".as_slice(),
        b"TITLE-PRIVATE".as_slice(),
        b"private.example".as_slice(),
    ] {
        assert!(!bytes.windows(secret.len()).any(|window| window == secret));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
    }

    let mut reader = ArtifactReader::new(Cursor::new(bytes)).unwrap();
    let mut replay: Vec<Option<Terminal>> = vec![None, None];
    let mut last_time = Duration::ZERO;
    let mut last_output_time = None;
    let mut switch_checkpoints = 0;
    let mut finished = false;
    while let Some(event) = reader.next_event().unwrap() {
        match event {
            ArtifactEvent::Started {
                at,
                stream,
                terminal,
            }
            | ArtifactEvent::Resized {
                at,
                stream,
                terminal,
            } => {
                assert!(at >= last_time);
                last_time = at;
                replay[stream.index() - 1] = Some(terminal);
            }
            ArtifactEvent::Checkpoint {
                at,
                stream,
                terminal,
            } => {
                assert_eq!(Some(at), last_output_time);
                assert!(at >= last_time);
                last_time = at;
                switch_checkpoints += 1;
                replay[stream.index() - 1] = Some(terminal);
            }
            ArtifactEvent::Output { at, stream, bytes } => {
                assert!(at >= last_time);
                last_time = at;
                last_output_time = Some(at);
                let terminal = replay[stream.index() - 1].as_mut().unwrap();
                terminal.ingest(&bytes);
                let _ = terminal.drain_replies();
            }
            ArtifactEvent::Ended { at, .. } => {
                assert!(at >= last_time);
                last_time = at;
            }
            ArtifactEvent::Finished { at, reason } => {
                assert!(at >= last_time);
                assert_eq!(reason, FinishReason::User);
                finished = true;
            }
        }
    }
    assert!(finished);
    assert_eq!(switch_checkpoints, 2);
    for (recorded, original) in replay.into_iter().zip([&first, &second]) {
        let recorded = recorded.unwrap();
        assert_eq!(recorded.dimensions(), original.dimensions());
        assert_eq!(recorded.cursor(), original.cursor());
        assert_eq!(recorded.modes(), original.modes());
        assert!(recorded.queued_input().is_empty());
        assert!(recorded.queued_replies().is_empty());
        assert_eq!(recorded.scrollback_stats().logical_lines(), 0);
        for row in 0..original.dimensions().rows() {
            assert_eq!(recorded.row_text(row), original.row_text(row));
        }
    }
}

#[test]
fn partial_preconsent_sequences_refuse_start_without_creating_a_source() {
    let directory = private_dir();
    let mut terminal = terminal(20, 2);
    terminal.ingest(b"\x1b]2;PRIVATE-PENDING");
    assert!(Recorder::start(directory.path(), &[&terminal], || {}).is_err());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn preallocated_recording_id_binds_the_capture_to_its_saved_manifest() {
    let directory = private_dir();
    let terminal = terminal(20, 2);
    let id = RecordingId::new();
    let (wake, ready) = mpsc::channel();
    let (mut recorder, _) =
        Recorder::start_with_id(directory.path(), id, &[&terminal], move || {
            wake.send(()).unwrap()
        })
        .unwrap();
    assert_eq!(recorder.id(), id);
    assert!(Recorder::start_with_id(directory.path(), id, &[&terminal], || {}).is_err());
    recorder.stop(FinishReason::User).unwrap();
    let path = completed_path(completion(&mut recorder, ready));
    assert_eq!(
        path.file_name().unwrap(),
        format!("recording-{id}.fstrec").as_str()
    );
}

#[test]
fn prepared_sources_remain_empty_until_admitted_and_are_removed_on_refusal() {
    let directory = private_dir();
    let terminal = terminal(20, 2);
    let id = RecordingId::new();
    let prepared = PreparedRecording::create(directory.path(), id).unwrap();
    let partial = directory.path().join(format!("recording-{id}.partial"));
    assert_eq!(fs::metadata(&partial).unwrap().len(), 0);
    assert!(SharedRecording::start_prepared_with_privacy_loss_flag(
        prepared,
        &[&terminal],
        Arc::new(AtomicBool::new(true)),
        || {},
    )
    .is_err());
    assert!(!partial.exists());

    let prepared = PreparedRecording::create(directory.path(), id).unwrap();
    let (wake, ready) = mpsc::channel();
    let (recording, streams) = SharedRecording::start_prepared_with_privacy_loss_flag(
        prepared,
        &[&terminal],
        Arc::new(AtomicBool::new(false)),
        move || wake.send(()).unwrap(),
    )
    .unwrap();
    assert_eq!(recording.id(), id);
    drop(streams);
    recording.stop(FinishReason::User).unwrap();
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(
        recording.poll_completion().unwrap(),
        Some(Completion::Complete { .. })
    ));
}

#[cfg(windows)]
#[test]
fn windows_recording_source_is_current_user_only() {
    let directory = private_dir();
    let terminal = terminal(20, 2);
    let (mut recorder, _, ready) = start(directory.path(), &[&terminal]);
    recorder.stop(FinishReason::User).unwrap();
    let path = completed_path(completion(&mut recorder, ready));
    let parent =
        festerm_windows_security::lock_directory_path_without_delete_sharing(directory.path())
            .unwrap();
    let source = festerm_windows_security::open_file_no_reparse(
        &parent,
        Path::new(path.file_name().unwrap()),
    )
    .unwrap();
    assert!(festerm_windows_security::is_current_user_only(&source).unwrap());
    assert_eq!(
        festerm_windows_security::file_link_count(&source).unwrap(),
        1
    );
}

#[test]
fn repeat_of_offscreen_preconsent_character_is_checkpointed_after_output() {
    let directory = private_dir();
    let mut original = terminal(20, 2);
    original.ingest("🧬".as_bytes());
    original.ingest(b"\x1b[2J\x1b[H");
    let (mut recorder, streams, ready) = start(directory.path(), &[&original]);

    for output in [
        b"\x1b]2;not a repeat b\x07".as_slice(),
        b"\x1b[",
        b"2b",
        b"\x1b[3b",
    ] {
        let generation = original.screen_generation();
        original.ingest(output);
        recorder
            .observe_output(streams[0], output, generation, &original)
            .unwrap();
    }
    recorder.stop(FinishReason::User).unwrap();
    let path = completed_path(completion(&mut recorder, ready));
    let bytes = fs::read(path).unwrap();
    let mut reader = ArtifactReader::new(Cursor::new(&bytes)).unwrap();
    let mut replay = match reader.next_event().unwrap().unwrap() {
        ArtifactEvent::Started { terminal, .. } => terminal,
        _ => panic!("expected an initial checkpoint"),
    };
    let mut checkpoint_count = 0;
    let mut last_output_time = None;
    while let Some(event) = reader.next_event().unwrap() {
        match event {
            ArtifactEvent::Output { at, bytes, .. } => {
                replay.ingest(&bytes);
                last_output_time = Some(at);
            }
            ArtifactEvent::Checkpoint { at, terminal, .. } => {
                assert_eq!(Some(at), last_output_time);
                replay = terminal;
                checkpoint_count += 1;
            }
            ArtifactEvent::Finished { .. } => {}
            _ => panic!("unexpected recording event"),
        }
    }
    assert_eq!(checkpoint_count, 2);
    assert_eq!(replay.row_text(0), original.row_text(0));
    assert_eq!(replay.cursor(), original.cursor());

    let plan = PlaybackPlan::inspect(Cursor::new(&bytes), Pacing::default()).unwrap();
    let mut playback = Playback::new(Cursor::new(&bytes), streams[0], plan).unwrap();
    let mut final_row = None;
    while let Some(frame) = playback.next_frame().unwrap() {
        final_row = frame.terminal.and_then(|terminal| terminal.row_text(0));
    }
    assert_eq!(final_row, original.row_text(0));
}

#[test]
fn unsafe_screen_switch_stops_the_whole_recording_as_incomplete() {
    let directory = private_dir();
    let mut first = terminal(20, 2);
    let second = terminal(20, 2);
    let (mut recorder, streams, ready) = start(directory.path(), &[&first, &second]);
    let output = b"\x1b[?1049h\x1b]2;partial";
    let generation = first.screen_generation();
    first.ingest(output);
    assert!(recorder
        .observe_output(streams[0], output, generation, &first)
        .is_err());
    assert!(!recorder.is_recording());
    let path = match completion(&mut recorder, ready) {
        Completion::Incomplete { path, reason, .. } => {
            assert_eq!(reason, FinishReason::IntegrityFailure);
            path
        }
        other => panic!("expected an incomplete recording, got {other:?}"),
    };
    let mut reader = ArtifactReader::new(fs::File::open(path).unwrap()).unwrap();
    assert!(matches!(
        reader.next_event().unwrap(),
        Some(ArtifactEvent::Started { .. })
    ));
    assert!(matches!(
        reader.next_event().unwrap(),
        Some(ArtifactEvent::Started { .. })
    ));
    assert!(matches!(
        reader.next_event().unwrap(),
        Some(ArtifactEvent::Finished {
            reason: FinishReason::IntegrityFailure,
            ..
        })
    ));
}

#[test]
fn oversized_output_stops_all_streams_and_preserves_an_incomplete_source() {
    let directory = private_dir();
    let first = terminal(20, 2);
    let second = terminal(20, 2);
    let (mut recorder, streams, ready) = start(directory.path(), &[&first, &second]);
    let output = vec![b'x'; MAX_OUTPUT_BYTES + 1];
    assert!(recorder
        .observe_output(streams[1], &output, second.screen_generation(), &second)
        .is_err());
    assert!(!recorder.is_recording());
    assert!(matches!(
        completion(&mut recorder, ready),
        Completion::Incomplete {
            reason: FinishReason::ResourceLimit,
            ..
        }
    ));
}

#[test]
fn corrupt_truncated_and_oversized_frames_are_rejected() {
    let directory = private_dir();
    let terminal = terminal(20, 2);
    let (mut recorder, _, ready) = start(directory.path(), &[&terminal]);
    recorder.stop(FinishReason::User).unwrap();
    let path = completed_path(completion(&mut recorder, ready));
    let bytes = fs::read(path).unwrap();

    let mut corrupted = bytes.clone();
    corrupted[8 + 18] ^= 0x40;
    let mut reader = ArtifactReader::new(Cursor::new(corrupted)).unwrap();
    assert!(reader.next_event().is_err());

    let mut oversized = bytes.clone();
    oversized[18..22].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut reader = ArtifactReader::new(Cursor::new(oversized)).unwrap();
    assert!(reader.next_event().is_err());

    let mut truncated = bytes.clone();
    truncated.pop();
    let mut reader = ArtifactReader::new(Cursor::new(truncated)).unwrap();
    assert!(matches!(
        reader.next_event().unwrap(),
        Some(ArtifactEvent::Started { .. })
    ));
    assert!(reader.next_event().is_err());

    let mut trailing = bytes;
    trailing.push(0);
    let mut reader = ArtifactReader::new(Cursor::new(trailing)).unwrap();
    assert!(matches!(
        reader.next_event().unwrap(),
        Some(ArtifactEvent::Started { .. })
    ));
    assert!(reader.next_event().is_err());
}

fn shared_completion(recording: &SharedRecording, ready: &Receiver<()>) -> Completion {
    loop {
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        if let Some(completion) = recording.poll_completion().unwrap() {
            return completion;
        }
    }
}

#[test]
fn shared_lifecycle_stop_blocks_later_output_in_every_stream() {
    let directory = private_dir();
    let mut first = terminal(20, 2);
    let mut second = terminal(20, 2);
    let (wake, ready) = mpsc::channel();
    let (recording, streams) =
        SharedRecording::start(directory.path(), &[&first, &second], move || {
            let _ = wake.send(());
        })
        .unwrap();
    let generation = first.screen_generation();
    first.ingest(b"before");
    streams[0]
        .observe_output(b"before", generation, &first)
        .unwrap();
    let lifecycle = recording.clone();
    std::thread::spawn(move || lifecycle.stop(FinishReason::Lifecycle).unwrap())
        .join()
        .unwrap();
    let generation = second.screen_generation();
    second.ingest(b"after");
    streams[1]
        .observe_output(b"after", generation, &second)
        .unwrap();
    let path = completed_path(shared_completion(&recording, &ready));
    let mut reader = ArtifactReader::new(fs::File::open(path).unwrap()).unwrap();
    let mut output = Vec::new();
    let mut reason = None;
    while let Some(event) = reader.next_event().unwrap() {
        match event {
            ArtifactEvent::Output { bytes, .. } => output.extend(bytes),
            ArtifactEvent::Finished { reason: value, .. } => reason = Some(value),
            _ => {}
        }
    }
    assert_eq!(output, b"before");
    assert_eq!(reason, Some(FinishReason::Lifecycle));
}

#[test]
fn closing_one_shared_stream_keeps_the_other_recording() {
    let directory = private_dir();
    let first = terminal(20, 2);
    let mut second = terminal(20, 2);
    let (wake, ready) = mpsc::channel();
    let (recording, mut streams) =
        SharedRecording::start(directory.path(), &[&first, &second], move || {
            let _ = wake.send(());
        })
        .unwrap();
    let first_id = streams[0].id();
    drop(streams.remove(0));
    assert!(recording.is_recording().unwrap());
    let generation = second.screen_generation();
    second.ingest(b"still active");
    streams[0]
        .observe_output(b"still active", generation, &second)
        .unwrap();
    recording.stop(FinishReason::User).unwrap();
    let path = completed_path(shared_completion(&recording, &ready));
    let mut reader = ArtifactReader::new(fs::File::open(path).unwrap()).unwrap();
    let mut ended_first = false;
    let mut other_output = false;
    while let Some(event) = reader.next_event().unwrap() {
        match event {
            ArtifactEvent::Ended { stream, .. } if stream == first_id => ended_first = true,
            ArtifactEvent::Output { stream, bytes, .. } if stream != first_id => {
                assert!(ended_first);
                other_output = bytes == b"still active";
            }
            _ => {}
        }
    }
    assert!(ended_first && other_output);
}

#[test]
fn shared_capture_failure_is_visible_and_stops_every_stream() {
    let directory = private_dir();
    let first = terminal(20, 2);
    let second = terminal(20, 2);
    let (wake, ready) = mpsc::channel();
    let (recording, streams) =
        SharedRecording::start(directory.path(), &[&first, &second], move || {
            let _ = wake.send(());
        })
        .unwrap();
    let output = vec![b'x'; MAX_OUTPUT_BYTES + 1];
    assert!(streams[0]
        .observe_output(&output, first.screen_generation(), &first)
        .is_err());
    assert!(!recording.is_recording().unwrap());
    assert!(recording.take_failure().unwrap().unwrap().contains("limit"));
    assert!(recording.take_failure().unwrap().is_none());
    assert!(matches!(
        shared_completion(&recording, &ready),
        Completion::Incomplete {
            reason: FinishReason::ResourceLimit,
            ..
        }
    ));
}
