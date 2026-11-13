//! Synthetic application states for the reviewer-facing surface inventory.

use super::*;
use crate::overlay_state::{
    NativeTakeoverSelection, OpenRefusalNotice, PendingNativeTakeoverConfirmation,
    PendingNativeTerminationConfirmation, PendingQuitConfirmation, QuitConfirmationPurpose,
};
use festerm_session::{
    PasswordPrompt, SessionError, SessionErrorKind, SessionEvent, SessionLifecycle,
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Scene {
    UnknownKey,
    ChangedKey,
    Password,
    PasswordRetry,
    TerminalFind,
    TerminalFindEmpty,
    Starting,
    Disconnected,
    Exited,
    Failed,
    FocusMode,
    ForwardManager,
    ForwardError,
    Quit,
    CloseWindow,
    InstallUpdate,
    RestartUpdate,
    FileDrop,
    ResetSettings,
    NativeTerminate,
    NativeTakeover,
    OpenRefusal,
    TransientNotice,
    Inspector,
    RectangleCopy,
    RectangleSelected,
    SmartSelection,
    SmartSelectionRefusal,
}

pub(crate) fn fixture(scene: Scene, context: &egui::Context) -> FesTermApp {
    let fingerprint = "SHA256:synthetic-review-fingerprint";
    let events = match scene {
        Scene::UnknownKey | Scene::ChangedKey => {
            let mut prompt = HostKeyPrompt::new("build.example.com", 22, fingerprint);
            if matches!(scene, Scene::ChangedKey) {
                prompt = prompt.with_previously_trusted_fingerprint("SHA256:previous-fixture-key");
            }
            vec![SessionEvent::HostKeyVerification(prompt)]
        }
        Scene::Password | Scene::PasswordRetry => vec![SessionEvent::PasswordRequested(
            PasswordPrompt::new("builder", "build.example.com", 0, false),
        )],
        _ => Vec::new(),
    };
    let (mut app, tab, _) = FesTermApp::for_test_with_fake_ssh_session(events);
    app.configure_session_for_gallery(false, context);
    {
        let session = app.state.session_tab_mut(tab).unwrap();
        session.label = "Build server".into();
        session.terminal.ingest(
            b"builder@build:~/project$ cargo test\r\nrunning 24 tests\r\ntest result: ok. 24 passed\r\nbuilder@build:~/project$ ",
        );
        session.controller.pump_events(&mut session.terminal);
        if matches!(
            scene,
            Scene::UnknownKey | Scene::ChangedKey | Scene::Password | Scene::PasswordRetry
        ) {
            session
                .controller
                .set_lifecycle_for_test(SessionLifecycle::Starting);
        }
    }
    match scene {
        Scene::Starting | Scene::Disconnected | Scene::Exited | Scene::Failed => {
            let lifecycle = match scene {
                Scene::Starting => SessionLifecycle::Starting,
                Scene::Disconnected => SessionLifecycle::Disconnected(SessionError::new(
                    SessionErrorKind::Output,
                    "Synthetic connection lost; no live service was contacted.",
                )),
                Scene::Exited => {
                    SessionLifecycle::Exited(festerm_session::SessionExit::with_exit_code(0))
                }
                _ => SessionLifecycle::Failed(SessionError::new(
                    SessionErrorKind::Spawn,
                    "Synthetic connection refused.",
                )),
            };
            app.state
                .session_tab_mut(tab)
                .unwrap()
                .controller
                .set_lifecycle_for_test(lifecycle);
        }
        Scene::TerminalFind | Scene::TerminalFindEmpty => {
            let session = app.state.session_tab_mut(tab).unwrap();
            session.search.open();
            let query = if matches!(scene, Scene::TerminalFind) {
                "test"
            } else {
                "not-in-the-fixture"
            };
            session.search.set_query(&session.terminal, query.into());
        }
        Scene::FocusMode => app.focus_mode = true,
        Scene::ForwardManager | Scene::ForwardError => {
            let mut manager = LivePortForwardManager::new(tab);
            if matches!(scene, Scene::ForwardError) {
                manager.error = Some("Bind port must be between 1 and 65535.".into());
            }
            app.overlays.port_forward_manager = Some(manager);
        }
        Scene::Quit | Scene::CloseWindow | Scene::InstallUpdate | Scene::RestartUpdate => {
            let purpose = match scene {
                Scene::CloseWindow => QuitConfirmationPurpose::CloseWindow,
                Scene::InstallUpdate => QuitConfirmationPurpose::InstallUpdate,
                Scene::RestartUpdate => QuitConfirmationPurpose::RestartAfterUpdate,
                _ => QuitConfirmationPurpose::Quit,
            };
            app.overlays.pending_quit = Some(PendingQuitConfirmation {
                counts: app.state.live_session_counts(),
                cancel_focus_requested: false,
                purpose,
            });
        }
        Scene::FileDrop => {
            let session = app.state.session_tab_mut(tab).unwrap();
            session.inspector_transport = InspectorTransport::Local { persistence: None };
            app.overlays.pending_file_drop = Some(PendingFileDropConfirmation {
                _human_input_intent: session.controller.begin_pending_human_input(),
                tab,
                identity: "Fixture shell".into(),
                text: "/work/project/notes.md /work/project/release notes.txt".into(),
                path_count: 2,
                lifecycle_generation: session.controller.lifecycle_generation(),
                cancel_focus_requested: false,
            });
        }
        Scene::ResetSettings => {
            app.state.dispatch(AppCommand::OpenSettings, context);
            app.overlays.pending_settings_reset = Some(PendingSettingsResetConfirmation {
                cancel_focus_requested: false,
            });
        }
        Scene::NativeTerminate | Scene::NativeTakeover => {
            app.state.dispatch(AppCommand::OpenLauncher, context);
            let session =
                crate::discovery::NativeSession::fixture(festerm_sessiond::UnattachedSession {
                    name: "Review build shell".into(),
                    pid: 4242,
                    endpoint: "fixture-only-endpoint".into(),
                    created_at_unix_ms: 1_794_000_000_000,
                    shell: "/bin/sh".into(),
                    arguments: Vec::new(),
                    working_directory: None,
                });
            if matches!(scene, Scene::NativeTerminate) {
                app.overlays.pending_native_termination =
                    Some(PendingNativeTerminationConfirmation {
                        session,
                        cancel_focus_requested: false,
                    });
            } else {
                app.overlays.pending_native_takeover = Some(PendingNativeTakeoverConfirmation {
                    selection: NativeTakeoverSelection::Discovered(session),
                    cancel_focus_requested: false,
                });
            }
        }
        Scene::OpenRefusal => {
            app.overlays.open_refusal = Some(OpenRefusalNotice {
                title: None,
                acknowledgement_path: None,
                name: "binary-fixture.dat".into(),
                path: "/work/project/binary-fixture.dat".into(),
                headline: "File cannot be opened as text".into(),
                detail: "The selected fixture contains binary data. No content was loaded.".into(),
            });
        }
        Scene::TransientNotice => {
            app.overlays.transient_notice = Some((
                "File drop only inserts paths into a local session, never a remote or serial one."
                    .into(),
                Instant::now(),
            ));
        }
        Scene::Inspector => {
            app.state
                .dispatch(AppCommand::ToggleSessionInspector, context);
        }
        Scene::RectangleCopy | Scene::RectangleSelected => {
            app.state
                .dispatch(AppCommand::BeginRectangleCopy(tab), context);
        }
        Scene::SmartSelection | Scene::SmartSelectionRefusal => {
            if matches!(scene, Scene::SmartSelection) {
                app.state
                    .session_tab_mut(tab)
                    .unwrap()
                    .terminal
                    .ingest(b"\x1b[1;1H");
            }
            if matches!(scene, Scene::SmartSelectionRefusal) {
                app.state.dispatch(
                    AppCommand::SmartSelect(tab, festerm_ui_egui::SmartSelectionScope::Paragraph),
                    context,
                );
            }
        }
        _ => {}
    }
    app
}

pub(crate) fn advance_smart_selection(app: &mut FesTermApp, context: &egui::Context) {
    let tab = app.state.active();
    app.state.dispatch(
        AppCommand::SmartSelect(tab, festerm_ui_egui::SmartSelectionScope::Paragraph),
        context,
    );
}

pub(crate) fn advance_password_retry(app: &mut FesTermApp) {
    let id = app.state.active();
    let session = app.state.session_tab_mut(id).unwrap();
    let Some(crate::tabs::ApplicationSession::TestSsh(fake)) = session.controller.session() else {
        panic!("retry fixture must retain its synthetic SSH transport");
    };
    fake.push_event(SessionEvent::PasswordRequested(PasswordPrompt::new(
        "builder",
        "build.example.com",
        1,
        true,
    )));
    session.controller.pump_events(&mut session.terminal);
    session
        .controller
        .set_lifecycle_for_test(SessionLifecycle::Starting);
}
