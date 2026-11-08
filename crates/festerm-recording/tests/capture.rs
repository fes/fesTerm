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

use festerm_core::{ColorScheme, Dimensions, Terminal, MAX_RECORDING_SCROLLBACK_BYTES};
use festerm_recording::{
    ArtifactEvent, ArtifactReader, Completion, FinishReason, Pacing, Playback, PlaybackPlan,
    PreparedRecording, Recorder, RecordingId, SharedRecording, TypingSpeed, MAX_OUTPUT_BYTES,
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
    let (recorder, streams) = Recorder::start(directory, terminals, move || {
        let _ = wake.send(());
    })
    .unwrap();
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
    #[cfg(windows)]
    let expected_partial_bytes = {
        let reference = private_dir();
        let mut terminal = terminal(4, 2);
        let (mut recorder, streams, ready) = start(reference.path(), &[&terminal]);
        recorder
            .ingest_output(streams[0], &mut terminal, b"hi")
            .unwrap();
        recorder.stop(FinishReason::User).unwrap();
        let bytes = fs::read(completed_path(completion(&mut recorder, ready))).unwrap();
        let mut input = Cursor::new(bytes);
        {
            let mut reader = ArtifactReader::new(&mut input).unwrap();
            assert!(matches!(
                reader.next_event().unwrap(),
                Some(ArtifactEvent::Started { .. })
            ));
            assert!(matches!(
                reader.next_event().unwrap(),
                Some(ArtifactEvent::Output { bytes, .. }) if bytes == b"hi"
            ));
        }
        input.position()
    };
    let directory = private_dir();
    let mut terminal = terminal(4, 2);
    let (mut recorder, streams, ready) = start(directory.path(), &[&terminal]);
    recorder
        .ingest_output(streams[0], &mut terminal, b"hi")
        .unwrap();
    let partial = directory
        .path()
        .join(format!("recording-{}.partial", recorder.id()));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        #[cfg(not(windows))]
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
        #[cfg(windows)]
        let flushed =
            fs::metadata(&partial).is_ok_and(|metadata| metadata.len() == expected_partial_bytes);
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
    let path = completed_path(completion(&mut recorder, ready));
    let mut reader = ArtifactReader::new(fs::File::open(path).unwrap()).unwrap();
    assert!(matches!(
        reader.next_event().unwrap(),
        Some(ArtifactEvent::Started { .. })
    ));
    assert!(matches!(
        reader.next_event().unwrap(),
        Some(ArtifactEvent::Output { bytes, .. }) if bytes == b"hi"
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
    recorder
        .ingest_output(streams[0], &mut first, b" more")
        .unwrap();
    recorder
        .ingest_output(streams[1], &mut second, b"\r\nupdated")
        .unwrap();
    first.resize(Dimensions::new(18, 2).unwrap()).unwrap();
    recorder.observe_resize(streams[0], &first).unwrap();
    for output in [
        b"\x1b[?1049halternate".as_slice(),
        b"\x1b[?1049l returned".as_slice(),
    ] {
        recorder
            .ingest_output(streams[0], &mut first, output)
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
    let mut resized = false;
    let mut finished = false;
    while let Some(event) = reader.next_event().unwrap() {
        match event {
            ArtifactEvent::Started {
                at,
                stream,
                terminal,
                ..
            } => {
                assert!(at >= last_time);
                last_time = at;
                replay[stream.index() - 1] = Some(terminal);
            }
            ArtifactEvent::Resized {
                at,
                stream,
                dimensions,
            } => {
                assert!(at >= last_time);
                last_time = at;
                replay[stream.index() - 1]
                    .as_mut()
                    .unwrap()
                    .resize(dimensions)
                    .unwrap();
                resized = true;
            }
            ArtifactEvent::Output {
                at,
                stream,
                bytes,
                repeats,
            } => {
                assert!(at >= last_time);
                last_time = at;
                let terminal = replay[stream.index() - 1].as_mut().unwrap();
                terminal.ingest_recorded_output(&bytes, repeats).unwrap();
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
            ArtifactEvent::Reset { .. } | ArtifactEvent::Configured { .. } => {
                panic!("unexpected recording event")
            }
        }
    }
    assert!(finished);
    assert!(resized);
    for (index, (recorded, original)) in replay.into_iter().zip([&first, &second]).enumerate() {
        let recorded = recorded.unwrap();
        assert_eq!(recorded.dimensions(), original.dimensions());
        assert_eq!(recorded.modes(), original.modes());
        assert!(recorded.queued_input().is_empty());
        assert!(recorded.queued_replies().is_empty());
        for row in 0..original.dimensions().rows() {
            let text = recorded.row_text(row).unwrap();
            assert!(!text.contains("HISTORY-PRIVATE"));
            if index == 1 {
                assert_eq!(Some(text), original.row_text(row));
            }
        }
    }
}

#[test]
fn resizing_into_preconsent_history_stops_before_the_unfaithful_event() {
    let directory = private_dir();
    let mut live = terminal(20, 2);
    live.ingest(b"HISTORY-PRIVATE\r\none\r\nvisible");
    let (mut recorder, streams, ready) = start(directory.path(), &[&live]);

    live.resize(Dimensions::new(20, 3).unwrap()).unwrap();
    assert!((0..3).any(|row| live.row_text(row).unwrap().contains("HISTORY-PRIVATE")));
    recorder.observe_resize(streams[0], &live).unwrap();
    recorder.stop(FinishReason::User).unwrap();
    let Completion::Incomplete {
        path,
        reason: FinishReason::IntegrityFailure,
        detail,
        ..
    } = completion(&mut recorder, ready)
    else {
        panic!("resize exposing unavailable history must end incompletely");
    };
    assert!(detail.unwrap().contains("pre-consent"));
    let bytes = fs::read(path).unwrap();
    assert!(!bytes
        .windows(b"HISTORY-PRIVATE".len())
        .any(|window| window == b"HISTORY-PRIVATE"));
    let mut reader = ArtifactReader::new(Cursor::new(bytes)).unwrap();
    while let Some(event) = reader.next_event().unwrap() {
        assert!(!matches!(event, ArtifactEvent::Resized { .. }));
    }
}

#[test]
fn switching_to_a_preconsent_screen_stops_without_importing_it() {
    let directory = private_dir();
    let mut live = terminal(20, 2);
    live.ingest(b"PRIMARY-PRIVATE\x1b[?1049hvisible alternate");
    let (mut recorder, streams, ready) = start(directory.path(), &[&live]);
    recorder
        .ingest_output(streams[0], &mut live, b"\x1b[?1049l")
        .unwrap();
    assert!(live.row_text(0).unwrap().contains("PRIMARY-PRIVATE"));
    recorder.stop(FinishReason::User).unwrap();
    let Completion::Incomplete {
        path,
        reason: FinishReason::IntegrityFailure,
        ..
    } = completion(&mut recorder, ready)
    else {
        panic!("switching to an unavailable screen must end incompletely");
    };
    let bytes = fs::read(path).unwrap();
    assert!(!bytes
        .windows(b"PRIMARY-PRIVATE".len())
        .any(|window| window == b"PRIMARY-PRIVATE"));
    let mut reader = ArtifactReader::new(Cursor::new(bytes)).unwrap();
    while let Some(event) = reader.next_event().unwrap() {
        assert!(!matches!(event, ArtifactEvent::Output { .. }));
    }
}

#[test]
fn a_brief_switch_to_a_preconsent_screen_is_checked_before_switching_back() {
    let directory = private_dir();
    let mut live = terminal(20, 2);
    live.ingest(b"\x1b[?47hALT-PRIVATE\x1b[?47lvisible");
    let (mut recorder, streams, ready) = start(directory.path(), &[&live]);
    recorder
        .ingest_output(streams[0], &mut live, b"\x1b[?47h\x1b[?47l")
        .unwrap();
    recorder.stop(FinishReason::User).unwrap();
    let Completion::Incomplete {
        path,
        reason: FinishReason::IntegrityFailure,
        ..
    } = completion(&mut recorder, ready)
    else {
        panic!("a brief switch to an unavailable screen must end incompletely");
    };
    let bytes = fs::read(path).unwrap();
    assert!(!bytes
        .windows(b"ALT-PRIVATE".len())
        .any(|window| window == b"ALT-PRIVATE"));
}

#[test]
fn ambiguous_color_text_refuses_start_without_creating_a_source() {
    let directory = private_dir();
    let mut live = terminal(20, 2);
    let mut ansi = ColorScheme::DEFAULT_ANSI;
    ansi[1] = ColorScheme::DEFAULT_BACKGROUND;
    live.set_color_scheme(ColorScheme::new(
        ColorScheme::DEFAULT_FOREGROUND,
        ColorScheme::DEFAULT_BACKGROUND,
        ColorScheme::DEFAULT_FOREGROUND,
        ansi,
    ));
    live.ingest(b"\x1b[31mCOLOR-PRIVATE");
    assert!(Recorder::start(directory.path(), &[&live], || {}).is_err());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn invisible_unicode_refuses_start_without_creating_a_source() {
    for text in ["\u{00a0}", "X\u{034f}", "👩\u{200d}💻"] {
        let directory = private_dir();
        let mut live = terminal(20, 2);
        live.ingest(text.as_bytes());
        assert!(Recorder::start(directory.path(), &[&live], || {}).is_err());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}

#[test]
fn ordinary_visible_unicode_is_preserved_in_the_initial_checkpoint() {
    let directory = private_dir();
    let mut live = terminal(20, 2);
    live.ingest("é界🧬".as_bytes());
    let (mut recorder, _, ready) = start(directory.path(), &[&live]);
    recorder.stop(FinishReason::User).unwrap();
    let path = completed_path(completion(&mut recorder, ready));
    let mut reader = ArtifactReader::new(fs::File::open(path).unwrap()).unwrap();
    let Some(ArtifactEvent::Started { terminal, .. }) = reader.next_event().unwrap() else {
        panic!("expected the initial checkpoint");
    };
    assert!(terminal.row_text(0).unwrap().contains("é界🧬"));
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
fn repeat_of_offscreen_preconsent_character_is_recorded_only_when_repeated() {
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
        recorder
            .ingest_output(streams[0], &mut original, output)
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
    let mut repeat_count = 0;
    while let Some(event) = reader.next_event().unwrap() {
        match event {
            ArtifactEvent::Output { bytes, repeats, .. } => {
                repeat_count += repeats.len();
                replay.ingest_recorded_output(&bytes, repeats).unwrap();
            }
            ArtifactEvent::Finished { .. } => {}
            _ => panic!("unexpected recording event"),
        }
    }
    assert_eq!(repeat_count, 2);
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
fn concealed_preconsent_repeat_uses_a_same_width_blank_until_visible() {
    let directory = private_dir();
    let mut live = terminal(20, 2);
    live.ingest("🧬".as_bytes());
    live.ingest(b"\x1b[2J\x1b[H\x1b[8m");
    let (mut recorder, streams, ready) = start(directory.path(), &[&live]);
    recorder
        .ingest_output(streams[0], &mut live, b"\x1b[2b")
        .unwrap();
    let concealed_cursor = live.cursor();
    recorder
        .ingest_output(streams[0], &mut live, b"\x1b[0m\x1b[2b")
        .unwrap();
    recorder.stop(FinishReason::User).unwrap();
    let bytes = fs::read(completed_path(completion(&mut recorder, ready))).unwrap();
    let mut reader = ArtifactReader::new(Cursor::new(&bytes)).unwrap();
    let mut replay = match reader.next_event().unwrap().unwrap() {
        ArtifactEvent::Started { terminal, .. } => terminal,
        _ => panic!("expected an initial checkpoint"),
    };
    let ArtifactEvent::Output {
        bytes: first,
        repeats,
        ..
    } = reader.next_event().unwrap().unwrap()
    else {
        panic!("expected concealed repeat output");
    };
    assert_eq!(repeats, vec![(3, Some('\u{3000}'))]);
    replay.ingest_recorded_output(&first, repeats).unwrap();
    assert_eq!(replay.cursor(), concealed_cursor);
    assert!(!replay.row_text(0).unwrap().contains("🧬"));
    let ArtifactEvent::Output {
        bytes: second,
        repeats,
        ..
    } = reader.next_event().unwrap().unwrap()
    else {
        panic!("expected visible repeat output");
    };
    assert_eq!(repeats, vec![(7, Some('🧬'))]);
    replay.ingest_recorded_output(&second, repeats).unwrap();
    assert_eq!(replay.cursor(), live.cursor());
    assert_eq!(replay.cell_ref(4, 0).unwrap().text(), "🧬");
    assert_eq!(replay.cell_ref(6, 0).unwrap().text(), "🧬");
}

#[test]
fn repeat_of_ambiguous_text_stops_before_an_unfaithful_event() {
    for style in [
        b"\x1b[38;2;0;0;0;48;2;0;0;0m".as_slice(),
        b"\x1b[2;38;2;130;130;130;48;2;100;100;100m".as_slice(),
    ] {
        let directory = private_dir();
        for secret in ["🧬", "X"] {
            let mut live = terminal(20, 2);
            live.ingest(secret.as_bytes());
            live.ingest(b"\x1b[2J\x1b[H");
            live.ingest(style);
            let (mut recorder, streams, ready) = start(directory.path(), &[&live]);
            recorder
                .ingest_output(streams[0], &mut live, b"\x1b[2b")
                .unwrap();
            recorder.stop(FinishReason::User).unwrap();
            let Completion::Incomplete {
                path,
                reason: FinishReason::IntegrityFailure,
                ..
            } = completion(&mut recorder, ready)
            else {
                panic!("an ambiguous repeat must not silently lose visible text");
            };
            let mut reader = ArtifactReader::new(fs::File::open(path).unwrap()).unwrap();
            let Some(ArtifactEvent::Started { terminal, .. }) = reader.next_event().unwrap() else {
                panic!("expected the safe initial checkpoint");
            };
            assert!(!terminal.row_text(0).unwrap().contains(secret));
            while let Some(event) = reader.next_event().unwrap() {
                assert!(!matches!(event, ArtifactEvent::Output { .. }));
            }
        }
    }
}

#[test]
fn repeat_of_invisible_preconsent_character_stops_before_persisting_it() {
    let directory = private_dir();
    let mut live = terminal(20, 2);
    live.ingest("\u{00a0}".as_bytes());
    live.ingest(b"\x1b[2J\x1b[H");
    let (mut recorder, streams, ready) = start(directory.path(), &[&live]);
    recorder
        .ingest_output(streams[0], &mut live, b"\x1b[2b")
        .unwrap();
    recorder.stop(FinishReason::User).unwrap();
    let Completion::Incomplete {
        path,
        reason: FinishReason::IntegrityFailure,
        ..
    } = completion(&mut recorder, ready)
    else {
        panic!("an invisible pre-consent repeat must not be persisted");
    };
    let mut reader = ArtifactReader::new(fs::File::open(path).unwrap()).unwrap();
    let Some(ArtifactEvent::Started { terminal, .. }) = reader.next_event().unwrap() else {
        panic!("expected the safe initial checkpoint");
    };
    assert!(!terminal.row_text(0).unwrap().contains('\u{00a0}'));
    while let Some(event) = reader.next_event().unwrap() {
        assert!(!matches!(event, ArtifactEvent::Output { .. }));
    }
}

#[test]
fn repeat_does_not_extend_a_hidden_preconsent_grapheme_anchor() {
    let directory = private_dir();
    let mut live = terminal(20, 2);
    live.ingest("\x1b[8m🇦".as_bytes());
    let (mut recorder, streams, ready) = start(directory.path(), &[&live]);
    recorder
        .ingest_output(streams[0], &mut live, b"\x1b[0m\x1b[b")
        .unwrap();
    recorder.stop(FinishReason::User).unwrap();
    let bytes = fs::read(completed_path(completion(&mut recorder, ready))).unwrap();
    let mut reader = ArtifactReader::new(Cursor::new(bytes)).unwrap();
    let ArtifactEvent::Started {
        terminal: mut replay,
        ..
    } = reader.next_event().unwrap().unwrap()
    else {
        panic!("expected an initial checkpoint");
    };
    let ArtifactEvent::Output { bytes, repeats, .. } = reader.next_event().unwrap().unwrap() else {
        panic!("expected repeat output");
    };
    replay.ingest_recorded_output(&bytes, repeats).unwrap();
    assert_eq!(replay.cursor(), live.cursor());
    assert_eq!(replay.cell_ref(0, 0).unwrap().text(), " ");
    assert_eq!(replay.cell_ref(1, 0).unwrap().text(), "🇦");
    assert_eq!(live.cell_ref(0, 0).unwrap().text(), "🇦");
    assert_eq!(live.cell_ref(1, 0).unwrap().text(), "🇦");
}

#[test]
fn concealed_preconsent_grapheme_cannot_change_replay_geometry() {
    let directory = private_dir();
    let mut live = terminal(20, 2);
    live.ingest(b"\x1b[8m#");
    let (mut recorder, streams, ready) = start(directory.path(), &[&live]);
    recorder
        .ingest_output(streams[0], &mut live, "\u{fe0f}\u{20e3}".as_bytes())
        .unwrap();
    recorder.stop(FinishReason::User).unwrap();
    let Completion::Incomplete {
        path,
        reason: FinishReason::IntegrityFailure,
        ..
    } = completion(&mut recorder, ready)
    else {
        panic!("different hidden grapheme geometry must stop the recording");
    };
    let mut reader = ArtifactReader::new(fs::File::open(path).unwrap()).unwrap();
    let Some(ArtifactEvent::Started { terminal, .. }) = reader.next_event().unwrap() else {
        panic!("expected the safe initial checkpoint");
    };
    assert_eq!(terminal.cell_ref(0, 0).unwrap().text(), " ");
    while let Some(event) = reader.next_event().unwrap() {
        assert!(!matches!(event, ArtifactEvent::Output { .. }));
    }
}

#[test]
fn concealed_unrelated_unicode_does_not_exhaust_visibility_checks() {
    let directory = private_dir();
    let mut live = terminal(80, 2);
    live.ingest(b"\x1b[8mX");
    let (mut recorder, streams, ready) = start(directory.path(), &[&live]);
    recorder
        .ingest_output(streams[0], &mut live, "é".repeat(32).as_bytes())
        .unwrap();
    recorder.stop(FinishReason::User).unwrap();
    assert!(matches!(
        completion(&mut recorder, ready),
        Completion::Complete { .. }
    ));
}

#[test]
fn fragmented_output_and_resizes_continue_without_live_checkpoints() {
    let directory = private_dir();
    let mut first = terminal(20, 2);
    let second = terminal(20, 2);
    let (mut recorder, streams, ready) = start(directory.path(), &[&first, &second]);
    for (output, resize) in [
        (b"\x1b[?1049h\x1b]2;partial".as_slice(), true),
        (b" title\x07\x1b[31".as_slice(), true),
        (b"m\xf0\x9f".as_slice(), true),
        (b"\xa7\xac\x1b[2b".as_slice(), false),
    ] {
        recorder
            .ingest_output(streams[0], &mut first, output)
            .unwrap();
        if resize {
            first.resize(Dimensions::new(21, 3).unwrap()).unwrap();
            recorder.observe_resize(streams[0], &first).unwrap();
        }
    }
    recorder.stop(FinishReason::User).unwrap();
    let path = completed_path(completion(&mut recorder, ready));
    let bytes = fs::read(path).unwrap();
    for pacing in [
        Pacing::default(),
        Pacing {
            typing: Some(TypingSpeed::new(15).unwrap()),
            ..Pacing::default()
        },
    ] {
        let plan = PlaybackPlan::inspect(Cursor::new(&bytes), pacing).unwrap();
        let mut playback = Playback::new(Cursor::new(&bytes), streams[0], plan).unwrap();
        let mut final_row = None;
        while let Some(frame) = playback.next_frame().unwrap() {
            final_row = frame.terminal.and_then(|terminal| terminal.row_text(0));
        }
        assert_eq!(final_row, first.row_text(0));
    }
}

#[test]
fn recording_stops_before_a_resize_after_replay_history_is_lost() {
    let directory = private_dir();
    let mut live = terminal(24, 2);
    let (mut recorder, streams, ready) = start(directory.path(), &[&live]);
    let output = b"post-consent line text\r\n".repeat(64);
    for _ in 0..2_000 {
        if live.scrollback_stats().charged_bytes() > MAX_RECORDING_SCROLLBACK_BYTES + 1024 * 1024 {
            break;
        }
        recorder
            .ingest_output(streams[0], &mut live, &output)
            .unwrap();
    }
    assert!(live.scrollback_stats().charged_bytes() > MAX_RECORDING_SCROLLBACK_BYTES + 1024 * 1024);
    live.resize(Dimensions::new(24, 3).unwrap()).unwrap();
    recorder.observe_resize(streams[0], &live).unwrap();
    recorder.stop(FinishReason::User).unwrap();
    let Completion::Incomplete {
        path,
        reason: FinishReason::ResourceLimit,
        detail: Some(detail),
        ..
    } = completion(&mut recorder, ready)
    else {
        panic!("resize after history loss must stop capture as incomplete");
    };
    assert!(detail.contains("history"));
    let mut reader = ArtifactReader::new(fs::File::open(path).unwrap()).unwrap();
    let mut saw_output = false;
    loop {
        match reader.next_event().unwrap().unwrap() {
            ArtifactEvent::Started { .. } => {}
            ArtifactEvent::Output { .. } => saw_output = true,
            ArtifactEvent::Finished { reason, .. } => {
                assert_eq!(reason, FinishReason::ResourceLimit);
                break;
            }
            _ => panic!("the unsafe resize must not be written"),
        }
    }
    assert!(saw_output);
}

#[test]
fn oversized_output_stops_all_streams_and_preserves_an_incomplete_source() {
    let directory = private_dir();
    let first = terminal(20, 2);
    let mut second = Terminal::with_scrollback_limit(Dimensions::new(20, 2).unwrap(), 0).unwrap();
    let (mut recorder, streams, ready) = start(directory.path(), &[&first, &second]);
    let output = vec![b'x'; MAX_OUTPUT_BYTES + 1];
    assert!(recorder
        .ingest_output(streams[1], &mut second, &output)
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
fn too_many_repeat_annotations_stop_capture_without_dropping_live_output() {
    let directory = private_dir();
    let mut live = Terminal::with_scrollback_limit(Dimensions::new(20, 2).unwrap(), 0).unwrap();
    live.ingest(b"A");
    let (mut recorder, streams, ready) = start(directory.path(), &[&live]);
    let output = b"\x1b[b".repeat(festerm_recording::MAX_OUTPUT_REPEATS + 1);
    assert!(recorder
        .ingest_output(streams[0], &mut live, &output)
        .is_err());
    assert!(live.row_text(0).unwrap().contains('A'));
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
    streams[0].ingest_output(&mut first, b"before").unwrap();
    let lifecycle = recording.clone();
    std::thread::spawn(move || lifecycle.stop(FinishReason::Lifecycle).unwrap())
        .join()
        .unwrap();
    streams[1].ingest_output(&mut second, b"after").unwrap();
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
    streams[0]
        .ingest_output(&mut second, b"still active")
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
    let mut first = Terminal::with_scrollback_limit(Dimensions::new(20, 2).unwrap(), 0).unwrap();
    let second = terminal(20, 2);
    let (wake, ready) = mpsc::channel();
    let (recording, streams) =
        SharedRecording::start(directory.path(), &[&first, &second], move || {
            let _ = wake.send(());
        })
        .unwrap();
    let output = vec![b'x'; MAX_OUTPUT_BYTES + 1];
    assert!(streams[0].ingest_output(&mut first, &output).is_err());
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
