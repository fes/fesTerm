use std::{fs::File, thread, time::Duration};

use festerm_recording::{ArtifactEvent, ArtifactReader};
use festerm_session::SessionEvent;

use super::*;
use crate::{app::FesTermApp, application::Window};

fn two_windows() -> (
    FesTermApplication,
    TabId,
    crate::session_controller::fake::FakeSshSession,
    TabId,
    crate::session_controller::fake::FakeSshSession,
) {
    let (primary, first, first_transport) = FesTermApp::for_test_with_fake_ssh_session([]);
    let (secondary, second, second_transport) = FesTermApp::for_test_with_fake_ssh_session([]);
    let mut app = FesTermApplication::new(primary);
    app.windows.push(Window {
        id: WindowId(1),
        app: secondary,
        placement: None,
    });
    (app, first, first_transport, second, second_transport)
}

#[test]
fn consent_identity_rejects_reconnected_or_replaced_sessions() {
    let (mut app, first, _, second, _) = two_windows();
    let first_identity = SessionIdentity::capture(&app, first).unwrap();
    let second_identity = SessionIdentity::capture(&app, second).unwrap();
    assert!(first_identity.matches(&app));
    assert!(second_identity.matches(&app));

    app.windows[1]
        .app
        .automation_session_mut(second)
        .controller
        .advance_lifecycle_generation();
    assert!(first_identity.matches(&app));
    assert!(!second_identity.matches(&app));

    let identity = SessionIdentity::capture(&app, first).unwrap();
    let session = app.windows[0].app.automation_session_mut(first);
    session.controller = crate::session_controller::SessionController::for_test(
        crate::tabs::ApplicationSession::TestSsh(
            crate::session_controller::fake::FakeSshSession::new([]),
        ),
    );
    assert!(!identity.matches(&app));
}

#[test]
fn recording_rejects_sftp_sessions() {
    let (mut app, first, _, _, _) = two_windows();
    app.windows[0]
        .app
        .automation_session_mut(first)
        .inspector_transport = crate::tabs::InspectorTransport::Sftp {
        username: "user".to_owned(),
        host: "example.test".to_owned(),
        port: 22,
    };
    assert!(app.recording_session(first).is_none());
    assert!(SessionIdentity::capture(&app, first).is_none());
    assert!(!app.windows[0]
        .app
        .recording_choices()
        .iter()
        .any(|(id, _, _)| *id == first));
}

#[test]
fn desktop_consent_requires_an_installed_monitor_and_is_revoked_on_loss() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let (mut app, first, _, _, _) = two_windows();
    let parent = crate::recording_storage::test_private_parent();
    let root = crate::recording_storage::test_directory_path(parent.path()).join("private");
    let destination = tempfile::tempdir().unwrap();
    let (storage, _) = RecordingStorage::open_for_test(&root).unwrap();
    let mut coordinator = DesktopRecording::new(false);
    coordinator.storage = Some(storage);
    let settings = RecordingSettings {
        destination: Some(ExportDestination::DesktopPath(
            crate::recording_storage::test_directory_path(destination.path()),
        )),
        ..RecordingSettings::default()
    };
    let context = egui::Context::default();
    coordinator.begin(&app, vec![first], settings.clone(), false, &context);
    assert!(coordinator.prepare.is_none());
    assert!(coordinator
        .status
        .as_deref()
        .unwrap()
        .contains("Privacy monitoring"));

    let loss = Arc::new(AtomicBool::new(false));
    coordinator.privacy = Some(PrivacyGuard::for_test(loss.clone()));
    coordinator.begin(&app, vec![first], settings, false, &context);
    assert!(coordinator.prepare.is_some());
    loss.store(true, Ordering::Release);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while coordinator.starting.is_some() {
        coordinator.poll_start(&mut app, &context);
        assert!(
            std::time::Instant::now() < deadline,
            "preparation did not cancel"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(coordinator.recording.is_none());
    assert!(coordinator.status.as_deref().unwrap().contains("privacy"));
    let lease = RecordingLease::acquire(&root).unwrap();
    assert!(festerm_recording::RecordingSource::recover(&root, &lease)
        .unwrap()
        .is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn recording_rejects_non_wayland_displays_and_revokes_monitor_on_backend_loss() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let (mut app, _, _, _, _) = two_windows();
    let parent = crate::recording_storage::test_private_parent();
    let root = crate::recording_storage::test_directory_path(parent.path()).join("private");
    let (storage, _) = RecordingStorage::open_for_test(&root).unwrap();
    let mut coordinator = DesktopRecording::new(false);
    coordinator.storage = Some(storage);
    coordinator.request(
        &mut app,
        WindowId::PRIMARY,
        Request::Open,
        &egui::Context::default(),
    );
    assert!(coordinator.privacy_task.is_none());
    assert!(coordinator.status.as_deref().unwrap().contains("Wayland"));

    let loss = Arc::new(AtomicBool::new(false));
    coordinator.set_wayland_backend(true);
    coordinator.privacy = Some(PrivacyGuard::for_test(loss.clone()));
    coordinator.set_wayland_backend(false);
    assert!(loss.load(Ordering::Acquire));
    assert!(coordinator.privacy.is_none());
}

#[test]
fn desktop_quit_waits_for_a_finalized_private_recording() {
    let (mut app, first, _, _, _) = two_windows();
    let directory = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    festerm_unix_security::make_private_directory(&File::open(directory.path()).unwrap()).unwrap();
    #[cfg(windows)]
    let private_path = {
        let parent =
            festerm_windows_security::lock_directory_path_without_delete_sharing(directory.path())
                .unwrap();
        let child = std::path::Path::new("private");
        festerm_windows_security::create_current_user_only_directory(&parent, child).unwrap();
        directory.path().join(child)
    };
    #[cfg(not(windows))]
    let private_path = directory.path().to_owned();
    let terminal = &app.recording_session(first).unwrap().terminal;
    let (recording, _) = SharedRecording::start(&private_path, &[terminal], || {}).unwrap();
    let path = private_path.join(format!("recording-{}.fstrec", recording.id()));
    let mut coordinator = DesktopRecording::new(false);
    coordinator.recording = Some(recording.clone());
    let context = egui::Context::default();
    let mut output = context.run_ui(egui::RawInput::default(), |ui| {
        coordinator.defer_quit(ui.ctx());
    });
    assert!(output
        .viewport_output
        .values()
        .flat_map(|viewport| &viewport.commands)
        .any(|command| matches!(command, egui::ViewportCommand::CancelClose)));
    output.textures_delta.clear();
    assert!(!recording.is_recording().unwrap());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut close_requested = false;
    while coordinator.recording.is_some() {
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            coordinator.poll_capture(&mut app, ui.ctx());
        });
        close_requested |= output
            .viewport_output
            .values()
            .flat_map(|viewport| &viewport.commands)
            .any(|command| matches!(command, egui::ViewportCommand::Close));
        output.textures_delta.clear();
        assert!(std::time::Instant::now() < deadline, "quit did not finish");
        thread::sleep(Duration::from_millis(5));
    }
    assert!(close_requested);
    assert!(coordinator.export.is_none());
    let mut reader = ArtifactReader::new(File::open(path).unwrap()).unwrap();
    while reader.next_event().unwrap().is_some() {}
}

#[test]
fn desktop_stop_requires_a_current_duration_preview_and_export_confirmation() {
    let (mut app, first, _, _, _) = two_windows();
    let parent = crate::recording_storage::test_private_parent();
    let root = crate::recording_storage::test_directory_path(parent.path()).join("private");
    let destination = tempfile::tempdir().unwrap();
    let (storage, _) = RecordingStorage::open_for_test(&root).unwrap();
    let settings = RecordingSettings {
        destination: Some(ExportDestination::DesktopPath(
            crate::recording_storage::test_directory_path(destination.path()),
        )),
        ..RecordingSettings::default()
    };
    let terminal = &app.recording_session(first).unwrap().terminal;
    let (recording, _) = SharedRecording::start(&root, &[terminal], || {}).unwrap();
    let manifest = RecordingManifest::create(
        &root,
        recording.id(),
        festerm_recording::LocalStart::new(2026, 1, 2, 3, 4, 5).unwrap(),
        &settings,
    )
    .unwrap();
    let lease = Arc::new(RecordingLease::acquire(&root).unwrap());
    recording.stop(FinishReason::User).unwrap();
    let context = egui::Context::default();
    let mut coordinator = DesktopRecording::new(false);
    coordinator.storage = Some(storage);
    coordinator.recording = Some(recording);
    coordinator.manifest = Some(manifest);
    coordinator.lease = Some(lease);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while coordinator.recording.is_some() {
        coordinator.poll_capture(&mut app, &context);
        assert!(
            std::time::Instant::now() < deadline,
            "recording did not finish"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let path = coordinator.pending_export.as_ref().unwrap().path.clone();
    assert!(coordinator.export.is_none());
    coordinator.dialog = Some(Dialog::Export {
        draft: settings.clone(),
    });
    coordinator.handle_action(&mut app, Action::ConfirmExport(settings.clone()), &context);
    assert!(coordinator.export.is_none());
    assert!(coordinator.pending_export.is_some());
    coordinator.handle_action(&mut app, Action::PreviewExport(settings.clone()), &context);
    let preview = loop {
        if let Some(result) = coordinator.preview_task.as_ref().unwrap().poll() {
            break result.unwrap();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "preview did not finish"
        );
        thread::sleep(Duration::from_millis(5));
    };
    coordinator.preview_task = None;
    coordinator.preview = Some(preview);
    let mut changed = settings.clone();
    changed.speed = festerm_recording::PlaybackSpeed::Double;
    coordinator.handle_action(&mut app, Action::ConfirmExport(changed), &context);
    assert!(coordinator.export.is_none());
    assert!(coordinator.pending_export.is_some());
    coordinator.handle_action(&mut app, Action::Close, &context);
    assert!(coordinator.pending_export.is_none());
    assert!(path.exists());
}

#[test]
fn recording_start_and_enrollment_refuse_scrolled_viewports() {
    let (mut app, first, _, second, _) = two_windows();
    let session = app.windows[1].app.automation_session_mut(second);
    for _ in 0..32 {
        session.terminal.ingest(b"old safe output\r\n");
    }
    session.terminal.ingest(b"hidden live secret");
    session.view.reveal_document_row(&session.terminal, 0);

    let mut coordinator = DesktopRecording::new(false);
    coordinator.begin(
        &app,
        vec![second],
        RecordingSettings::default(),
        false,
        &egui::Context::default(),
    );
    assert!(coordinator
        .status
        .as_deref()
        .unwrap()
        .contains("live screen"));
    assert!(coordinator.starting.is_none());

    let directory = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    festerm_unix_security::make_private_directory(&File::open(directory.path()).unwrap()).unwrap();
    #[cfg(windows)]
    let private_path = {
        let parent =
            festerm_windows_security::lock_directory_path_without_delete_sharing(directory.path())
                .unwrap();
        let child = std::path::Path::new("private");
        festerm_windows_security::create_current_user_only_directory(&parent, child).unwrap();
        directory.path().join(child)
    };
    #[cfg(not(windows))]
    let private_path = directory.path().to_owned();
    let terminal = &app.recording_session(first).unwrap().terminal;
    let (recording, _) = SharedRecording::start(&private_path, &[terminal], || {}).unwrap();
    coordinator.recording = Some(recording.clone());
    coordinator.include(&mut app, second);
    assert!(coordinator
        .status
        .as_deref()
        .unwrap()
        .contains("live screen"));
    assert!(app
        .recording_session(second)
        .unwrap()
        .controller
        .recording_id()
        .is_none());
    recording.stop(FinishReason::User).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let path = loop {
        match recording.poll_completion().unwrap() {
            Some(Completion::Complete { path, .. }) => break path,
            Some(other) => panic!("unexpected recording completion: {other:?}"),
            None if std::time::Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            None => panic!("recording writer did not finish"),
        }
    };
    assert!(!std::fs::read(path)
        .unwrap()
        .windows(b"hidden live secret".len())
        .any(|window| window == b"hidden live secret"));
}

#[test]
fn synchronized_capture_spans_windows_and_excludes_local_input() {
    let (mut app, first, first_transport, second, second_transport) = two_windows();
    let temp = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    festerm_unix_security::make_private_directory(&File::open(temp.path()).unwrap()).unwrap();
    #[cfg(windows)]
    let private_path = {
        let parent =
            festerm_windows_security::lock_directory_path_without_delete_sharing(temp.path())
                .unwrap();
        let child = std::path::Path::new("private");
        festerm_windows_security::create_current_user_only_directory(&parent, child).unwrap();
        temp.path().join(child)
    };
    #[cfg(not(windows))]
    let private_path = temp.path().to_owned();

    let terminals = [
        &app.recording_session(first).unwrap().terminal,
        &app.recording_session(second).unwrap().terminal,
    ];
    let (recording, streams) = SharedRecording::start(&private_path, &terminals, || {}).unwrap();
    let first_stream = streams[0].id();
    let second_stream = streams[1].id();
    for (tab, stream) in [first, second].into_iter().zip(streams) {
        assert!(app.attach_recording(tab, stream));
    }
    let choices = app.recording_choices(Some(recording.id()), WindowId::PRIMARY);
    assert!(
        choices
            .iter()
            .find(|choice| choice.id == first)
            .unwrap()
            .included
    );
    assert!(
        choices
            .iter()
            .find(|choice| choice.id == second)
            .unwrap()
            .included
    );

    let session = app.windows[0].app.automation_session_mut(first);
    festerm_ui_egui::route_input(
        &mut session.terminal,
        festerm_core::InputEvent::Paste("un-echoed-private-input".to_owned()),
        &mut session.controller,
    );
    session.controller.flush_pending_writes();
    assert_eq!(first_transport.sent().concat(), b"un-echoed-private-input");
    first_transport.push_event(SessionEvent::Output(b"first output".to_vec()));
    second_transport.push_event(SessionEvent::Output(b"second output".to_vec()));
    app.windows[0]
        .app
        .automation_session_mut(first)
        .pump_session_events();
    app.windows[1]
        .app
        .automation_session_mut(second)
        .pump_session_events();
    recording.stop(FinishReason::User).unwrap();
    app.detach_recording(recording.id());

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let path = loop {
        match recording.poll_completion().unwrap() {
            Some(Completion::Complete { path, .. }) => break path,
            Some(other) => panic!("unexpected recording completion: {other:?}"),
            None if std::time::Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            None => panic!("recording writer did not finish"),
        }
    };
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes
        .windows(b"un-echoed-private-input".len())
        .any(|window| window == b"un-echoed-private-input"));
    let mut reader = ArtifactReader::new(File::open(path).unwrap()).unwrap();
    let mut output = Vec::new();
    while let Some(event) = reader.next_event().unwrap() {
        if let ArtifactEvent::Output { stream, bytes, .. } = event {
            output.push((stream, bytes));
        }
    }
    assert_eq!(
        output,
        vec![
            (first_stream, b"first output".to_vec()),
            (second_stream, b"second output".to_vec())
        ]
    );
}
