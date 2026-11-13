use super::*;
use crate::app::gallery::Scene;

macro_rules! app_scene {
    ($id:literal, $kind:ident, $title:literal, $route:literal) => {
        Scenario {
            id: $id,
            section: "workflow-surfaces",
            title: $title,
            caption: concat!($route, " Synthetic production-widget capture; no live transport, personal profile, vault or clipboard."),
            capture: || capture_app(Scene::$kind),
        }
    };
}

pub(super) fn scenarios() -> Vec<Scenario> {
    let mut scenes = vec![
        app_scene!("ssh-trust-unknown", UnknownKey, "SSH: first-seen host key", "New Session > SSH > Connect > unknown host. Exit: N/Escape cancels; Y accepts once; A remembers."),
        app_scene!("ssh-trust-changed", ChangedKey, "SSH: changed host key", "Connect to a host whose saved key differs. Exit: Escape cancels; deliberate typed yes replaces trust."),
        app_scene!("ssh-password-prompt", Password, "SSH: live password prompt", "SSH > Connect > verify host > worker requests password. Exit: submit transient password or cancel."),
        app_scene!("ssh-password-retry", PasswordRetry, "SSH: rejected password and retry", "SSH password prompt > rejected authentication > next transient prompt. Exit: retry or Escape; no actual password is entered."),
        app_scene!("terminal-find-matches", TerminalFind, "Terminal Find: matches", "Live terminal > Find (palette/menu/shortcut) > enter test. Exit: Escape or Close returns focus."),
        app_scene!("terminal-find-no-match", TerminalFindEmpty, "Terminal Find: no matches", "Terminal > Find > query absent from history. Exit: clear/change query or Close."),
        app_scene!("session-starting", Starting, "Session: starting", "Connect/start a session while transport startup is pending. Exit: connection succeeds/fails, or close tab."),
        app_scene!("session-disconnected", Disconnected, "Session: disconnected history", "Live SSH > lose connection. Exit: Reconnect, diagnostics or Close Tab."),
        app_scene!("session-exited", Exited, "Session: exited history", "Local/remote process exits. Exit: inspect/copy history, relaunch where supported, or Close Tab."),
        app_scene!("session-failed", Failed, "Session: startup failure", "Start a session > transport refuses startup. Exit: diagnostics/recovery or Close Tab."),
        app_scene!("terminal-focus-mode", FocusMode, "Terminal: Focus Mode", "Live terminal > palette > Focus Mode. Exit: toggle Focus Mode again; Escape belongs to the terminal."),
        app_scene!("port-forward-manager", ForwardManager, "Live Port Forward Manager", "Live SSH > palette/shortcut > Port Forward Manager. Exit: Close/Escape; mappings are live-only."),
        app_scene!("port-forward-validation", ForwardError, "Port Forward Manager: validation error", "Port Forward Manager > add a mapping with invalid bind port. Exit: correct fields or Close."),
        app_scene!("safety-quit", Quit, "Quit with live sessions", "Quit with a live session. Exit: Cancel preserves sessions; explicit Quit ends them."),
        app_scene!("safety-close-window", CloseWindow, "Close window with live sessions", "Close a secondary window containing live sessions. Exit: Cancel or deliberate Close Window."),
        app_scene!("safety-install-update", InstallUpdate, "Install update with live sessions", "About > ready update > install while sessions run. Exit: Cancel preserves update/session; explicit install."),
        app_scene!("safety-restart-update", RestartUpdate, "Restart after update with live sessions", "Installed update > restart while sessions run. Exit: Cancel or explicit restart."),
        app_scene!("safety-file-drop", FileDrop, "Local file-drop insertion preview", "Drop files on a live local terminal. Exit: Cancel or explicit insertion; no Enter is sent."),
        app_scene!("safety-reset-settings", ResetSettings, "Reset interface settings", "Settings > change an interface preference > Reset. Exit: Cancel or deliberate Reset."),
        app_scene!("safety-native-terminate", NativeTerminate, "Terminate a durable native session", "Launcher > Running Sessions > native row menu > Terminate. Exit: Cancel or deliberate termination."),
        app_scene!("safety-native-takeover", NativeTakeover, "Take over an attached native session", "Launcher > native session > Take over. Exit: Cancel or explicit takeover of the same generation."),
        app_scene!("document-open-refusal", OpenRefusal, "Document open refusal", "Open a binary/unsupported/oversize file. Exit: acknowledge; existing documents stay unchanged."),
        app_scene!("contextual-notice", TransientNotice, "Contextual refusal notice", "Attempt an unsupported file drop (for example onto SSH). Exit: timed dismissal; nothing is inserted."),
        app_scene!("inspector-live", Inspector, "Live production Session Inspector", "Live session > Inspector control/palette. Exit: Close/Escape/outside click restores prior owner."),
        app_scene!("terminal-rectangle-copy", RectangleCopy, "Bounded rectangle copy", "Terminal > palette > Bounded rectangle copy. Click two corners or drag to select; Enter/Copy accepts, Escape cancels. Capture stops before copying."),
        app_scene!("terminal-rectangle-selected", RectangleSelected, "Bounded rectangle: selected cells", "Bounded rectangle copy > click two corners. Exit: Escape cancels; explicit Copy is separate and never activated by capture."),
        app_scene!("terminal-smart-selection", SmartSelection, "Smart paragraph selection", "Terminal > local smart-selection command > Paragraph. Highlight/instruction or honest refusal is retained; Copy is always a separate deliberate action. Escape cancels."),
        app_scene!("terminal-smart-refusal", SmartSelectionRefusal, "Smart selection: unavailable target", "Smart Select Paragraph at a blank/border target. Exit: notice expires; choose ordinary drag or another target."),
    ];
    macro_rules! form {
        ($id:literal, $kind:ident, $title:literal, $route:literal) => {
            scenes.push(Scenario {
                id: $id,
                section: "workflow-surfaces",
                title: $title,
                caption: $route,
                capture: || capture_form(crate::screens::gallery::Scene::$kind),
            });
        };
    }
    form!("local-launch-form", Local, "Customized Local Shell form", "Settings > customize local shell; New Session > Local. Exit: Start or Back/Escape. Synthetic executable and directory; no shell is started.");
    form!(
        "local-launch-error",
        LocalError,
        "Local Shell validation",
        "Local Shell form > submit invalid executable. Exit: correct fields or Back/Escape."
    );
    form!(
        "ssh-separate-fields",
        SshSeparate,
        "SSH separate destination fields",
        "New Session > SSH > Use separate fields. Exit: switch notation or Back."
    );
    form!("ssh-private-key", SshKey, "SSH private-key authentication", "New Session > SSH > Private key. Key and passphrase are empty; no credential is read. Exit: switch auth or Back.");
    form!(
        "ssh-certificate",
        SshCertificate,
        "SSH certificate authentication",
        "New Session > SSH > Certificate. Exit: switch auth or Back."
    );
    form!("ssh-durable-recovery", SshDurable, "SSH durable-session options", "SSH > enable durable remote session > select provider/recovery. Exit: disable or Back. No provider probe is run.");
    form!(
        "ssh-validation",
        SshInvalid,
        "SSH form validation",
        "SSH > Connect with invalid fields. Exit: correct fields or Back."
    );
    form!(
        "sftp-text-mode-form",
        SftpTextMode,
        "SFTP terminal-mode alternative",
        "New Session > SFTP > turn graphical file manager off. Exit: Connect or Back."
    );
    form!("serial-validation", SerialInvalid, "Serial validation", "New Session > Serial > invalid baud > Open. No device discovery or device open is performed.");
    form!(
        "launcher-search-empty",
        LauncherNoMatch,
        "Launcher profile search without results",
        "New Session > Saved Profiles search > unmatched query. Exit: clear the query."
    );
    form!(
        "launcher-running-collapsed",
        LauncherCollapsed,
        "Collapsed running-session groups",
        "New Session > collapse native/tmux/screen group headings. Exit: expand each heading."
    );
    macro_rules! sftp {
        ($id:literal, $kind:ident, $title:literal, $route:literal) => {
            scenes.push(Scenario {
                id: $id,
                section: "workflow-surfaces",
                title: $title,
                caption: $route,
                capture: || capture_sftp(crate::sftp_file_manager::gallery::Scene::$kind),
            });
        };
    }
    sftp!("sftp-authentication", Authentication, "GUI SFTP: authentication", "Saved GUI SFTP profile > Connect, or edit a failed connection. Exit: Connect/Cancel; fixture never connects.");
    sftp!(
        "sftp-private-key",
        PrivateKey,
        "GUI SFTP: private key",
        "GUI SFTP authentication > Private key. Exit: change auth method or Cancel."
    );
    sftp!(
        "sftp-trust-unknown",
        UnknownKey,
        "GUI SFTP: unknown host",
        "GUI SFTP > Connect > first-seen host key. Exit: Reject/Accept Once/Accept and Remember."
    );
    sftp!(
        "sftp-trust-changed",
        ChangedKey,
        "GUI SFTP: changed host",
        "GUI SFTP > host presents changed key. Exit: deliberate yes/Enter or Cancel."
    );
    sftp!(
        "sftp-connecting",
        Connecting,
        "GUI SFTP: connecting",
        "Submit GUI SFTP authentication > connection pending. Exit: success or error."
    );
    sftp!(
        "sftp-connection-failed",
        Failed,
        "GUI SFTP: first connection failed",
        "GUI SFTP connection fails before first listing. Exit: Retry or Edit connection."
    );
    sftp!("sftp-disconnected", Disconnected, "GUI SFTP: stale listing after disconnect", "Connected file manager > connection lost. Exit: Retry/Edit connection; retained listing stays visible.");
    sftp!("sftp-operation-error", OperationError, "GUI SFTP: operation refusal", "File manager > open an unsupported/oversize file. Exit: dismiss banner or choose another file.");
    sftp!("sftp-transfer-history", Transfers, "GUI SFTP: transfer progress and history", "Select local/remote files > Upload/Download. Synthetic typed worker events show running, completed and failed rows; no files are transferred.");
    sftp!("sftp-transfer-details", TransferDetails, "GUI SFTP: isolated transfer drawer", "File manager > Transfers drawer. Isolated production drawer shows live progress and completed/failed history without the surrounding panes. Scroll for remaining rows.");
    sftp!("sftp-collision", Collision, "GUI SFTP: collision decision", "Transfer > existing destination. Exit: Replace/Skip/Keep Both/Cancel; optional apply-to-all. Synthetic worker event; no overwrite.");
    sftp!("sftp-browser-narrow", Narrow, "GUI SFTP: narrow pane switcher", "Resize GUI SFTP below split-view threshold. Switch Local/Remote to navigate. Exit: widen window.");
    sftp!(
        "sftp-browser-swapped",
        Swapped,
        "GUI SFTP: remote-left preference",
        "Settings > SFTP pane order > remote left, then return to file manager."
    );
    sftp!(
        "sftp-browser-empty",
        Empty,
        "GUI SFTP: empty directories",
        "Navigate each pane to an empty synthetic directory. Exit: Back/Up."
    );
    sftp!("sftp-local-context-menu", LocalMenu, "GUI SFTP: local file context menu", "Local pane > right-click a file > Reveal in Finder/Explorer. Exit: Escape; no native file manager is opened.");
    sftp!("sftp-path-editing", PathEditing, "GUI SFTP: direct path editing", "Local pane > click path / location shortcut. Exit: Escape restores navigation; Enter would request a new directory, never pressed here.");
    sftp!("sftp-directory-loading", DirectoryLoading, "GUI SFTP: directory requests pending", "Navigate/refresh a directory > bounded listing pending. Exit: response/error, or another navigation; no loader runs in capture.");
    sftp!("sftp-filter-no-match", FilterEmpty, "GUI SFTP: filtered directory without matches", "Type an unmatched filter in a populated directory. Exit: clear the filter; files are not deleted.");
    sftp!("sftp-listing-error", ListingError, "GUI SFTP: directory listing refusal", "Navigate/refresh a remote directory > permission denied. Exit: Retry, return to a readable directory or close.");
    macro_rules! editor {
        ($id:literal, $kind:ident, $title:literal, $route:literal) => {
            scenes.push(Scenario {
                id: $id,
                section: "workflow-surfaces",
                title: $title,
                caption: $route,
                capture: || capture_editor(EditorScene::$kind),
            });
        };
    }
    editor!("editor-save-pending", Saving, "Editor: save pending", "Editor > Save > bounded write pending. Exit: completion/error; no real write worker runs in this fixture.");
    editor!(
        "editor-save-error",
        SaveError,
        "Editor: save failure",
        "Edit > Save > permission/I/O error. Exit: Retry or Save As; edits remain intact."
    );
    editor!("editor-source-missing", Missing, "Editor: source disappeared", "Open editor > source removed/renamed externally > refresh. Exit: Save As or close; buffer retained.");
    editor!(
        "editor-source-offline",
        Offline,
        "Editor: remote source offline",
        "Remote editor > transport disconnects. Exit: revalidate/reconnect, Save As, or close."
    );
    editor!("editor-recovery-error", Recovery, "Editor: retained recovery versions", "Save publication reports recovery files needing explicit review. Exit: inspect retained files, resolve explicitly or close; no overwrite is performed.");
    editor!("editor-conflict-no-source", Conflict, "Editor: conflict without comparison bytes", "Dirty document > external change with no retained source text. Compare is honestly disabled. Exit: Keep my version/Reload where available/Save As.");
    editor!(
        "editor-find-no-match",
        NoMatch,
        "Editor Find: no matches",
        "Editor > Find > unmatched query. Exit: change query or Escape."
    );
    editor!(
        "editor-find-invalid-pattern",
        InvalidPattern,
        "Editor Find: invalid expression",
        "Editor > Find > invalid regular expression. Exit: repair query or Escape."
    );
    scenes
}

#[derive(Clone, Copy)]
enum EditorScene {
    Saving,
    SaveError,
    Missing,
    Offline,
    Recovery,
    Conflict,
    NoMatch,
    InvalidPattern,
}

fn capture_editor(scene: EditorScene) -> image::RgbaImage {
    if matches!(scene, EditorScene::Offline) {
        use festerm_document::{
            Availability, DocumentBounds, DocumentOrigin, RemoteOrigin, RemoteOwner, StatusInputs,
            TextDocument,
        };
        let documents = crate::documents::DocumentRegistry::shared();
        let origin = RemoteOrigin::new(
            "artifacts.example.net",
            22,
            RemoteOwner::username("builder").unwrap(),
            "SHA256:synthetic-review",
            "/srv/releases/NOTES.md",
            1,
        )
        .unwrap();
        let id = documents.borrow_mut().adopt(
            DocumentOrigin::Remote(origin),
            TextDocument::from_bytes(
                synthetic_markdown_prose().as_bytes(),
                DocumentBounds::DEFAULT,
            )
            .unwrap(),
            false,
        );
        let mut editor = TextEditorTab::new(id, &documents);
        editor.type_for_gallery(&documents, "\nUnsaved remote edits.\n");
        documents
            .borrow_mut()
            .get_mut(id)
            .unwrap()
            .status_inputs_for_gallery(StatusInputs {
                availability: Availability::Offline,
                ..StatusInputs::default()
            });
        let tab = AppState::for_test().active();
        let mut harness = gallery_harness(
            egui::vec2(900.0, 820.0),
            move |ui, state: &mut (crate::documents::SharedDocuments, TextEditorTab)| {
                let _ = state.1.show(ui, tab, &state.0);
            },
            (documents, editor),
        );
        return finish(&mut harness);
    }
    render_text_editor_in(
        Some("\nLocal unsaved fixture edits.\n"),
        crate::text_editor::EditorMode::Edit,
        Some(&|documents, editor, _| {
            use festerm_document::{
                Availability, ConflictState, SaveError, SaveProgress, StatusInputs,
                UnavailableReason,
            };
            let mut inputs = StatusInputs::default();
            match scene {
                EditorScene::Saving => inputs.save = SaveProgress::InFlight,
                EditorScene::SaveError => {
                    inputs.last_error = Some(SaveError::new(
                        "Could not save",
                        "Synthetic permission denied; your edits have not been discarded.",
                    ))
                }
                EditorScene::Missing => {
                    inputs.availability = Availability::Unavailable(UnavailableReason::Missing)
                }
                EditorScene::Offline => inputs.availability = Availability::Offline,
                EditorScene::Recovery => {
                    inputs.recovery_error = Some(SaveError::new(
                        "Recovery files retained",
                        "Review the retained original and edited versions before replacing either.",
                    ))
                }
                EditorScene::Conflict => {
                    inputs.conflict = Some(ConflictState::new(
                        "The source changed; comparison bytes are unavailable.",
                    ))
                }
                EditorScene::NoMatch => editor.open_find_for_gallery("not-in-this-fixture", None),
                EditorScene::InvalidPattern => editor.open_find_for_gallery("[unterminated", None),
            }
            documents
                .borrow_mut()
                .get_mut(editor.document())
                .unwrap()
                .status_inputs_for_gallery(inputs);
        }),
    )
}

fn capture_sftp(scene: crate::sftp_file_manager::gallery::Scene) -> image::RgbaImage {
    let id = AppState::for_test().active();
    let context = egui::Context::default();
    let tab = SftpFileManagerTab::for_gallery(
        "Artifact files".into(),
        "builder".into(),
        "artifacts.example.net".into(),
        22,
        synthetic_local_project_snapshot(),
        synthetic_remote_releases_snapshot(),
        None,
        None,
        SftpPaneOrderPreference::LocalLeft,
        &context,
    );
    let state = crate::sftp_file_manager::gallery::Fixture::new(tab, scene);
    let width = if matches!(scene, crate::sftp_file_manager::gallery::Scene::Narrow) {
        460.0
    } else {
        1180.0
    };
    let height = if matches!(
        scene,
        crate::sftp_file_manager::gallery::Scene::Transfers
            | crate::sftp_file_manager::gallery::Scene::Collision
    ) {
        1180.0
    } else {
        820.0
    };
    let mut harness = gallery_harness(
        egui::vec2(width, height),
        move |ui, state: &mut crate::sftp_file_manager::gallery::Fixture| state.show(ui, id),
        state,
    );
    for _ in 0..3 {
        harness.step();
    }
    if matches!(scene, crate::sftp_file_manager::gallery::Scene::LocalMenu) {
        let pos = harness.get_by_label("deploy.sh").rect().center();
        secondary_click(&mut harness, pos);
        assert!(harness
            .query_by_label(crate::sftp_file_manager::gallery::local_menu_label())
            .is_some());
    }
    harness.remove_cursor();
    harness.step();
    let image = harness.render().expect("render synthetic SFTP state");
    if matches!(
        scene,
        crate::sftp_file_manager::gallery::Scene::TransferDetails
    ) {
        trim_to_content(&image, GALLERY_MARGIN)
    } else {
        image
    }
}

fn capture_app(scene: Scene) -> image::RgbaImage {
    let app = crate::app::gallery::fixture(scene, &egui::Context::default());
    let mut harness = gallery_harness(
        egui::vec2(1000.0, 760.0),
        |ui, app: &mut crate::app::FesTermApp| app.ui_content(ui),
        app,
    );
    for _ in 0..4 {
        harness.step();
    }
    if matches!(scene, Scene::SmartSelection) {
        let context = harness.ctx.clone();
        crate::app::gallery::advance_smart_selection(harness.state_mut(), &context);
        harness.step();
    }
    if matches!(scene, Scene::PasswordRetry) {
        crate::app::gallery::advance_password_retry(harness.state_mut());
        harness.step();
    }
    if matches!(scene, Scene::RectangleSelected) {
        for pos in [egui::pos2(45.0, 76.0), egui::pos2(330.0, 114.0)] {
            harness.event(egui::Event::PointerMoved(pos));
            for pressed in [true, false] {
                harness.event(egui::Event::PointerButton {
                    pos,
                    pressed,
                    button: egui::PointerButton::Primary,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            harness.step();
        }
    }
    let expected = match scene {
        Scene::UnknownKey => {
            Some("ED25519 key fingerprint is SHA256:synthetic-review-fingerprint.")
        }
        Scene::ChangedKey => Some("@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @"),
        Scene::Password => Some("builder@build.example.com's password:"),
        Scene::PasswordRetry => Some("Permission denied, please try again."),
        Scene::ResetSettings => Some("Reset interface settings?"),
        Scene::NativeTerminate => Some("Terminate native session?"),
        Scene::NativeTakeover => Some("Take over native session?"),
        _ => None,
    };
    if let Some(label) = expected {
        assert!(
            harness.query_by_label(label).is_some(),
            "{scene:?} must expose its actual production surface"
        );
    }

    harness.remove_cursor();
    harness.step();
    harness.render().expect("render synthetic workflow surface")
}

fn secondary_click<State>(harness: &mut Harness<'_, State>, pos: egui::Pos2) {
    harness.event(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos,
            pressed,
            button: egui::PointerButton::Secondary,
            modifiers: egui::Modifiers::NONE,
        });
    }
    harness.run();
}

fn capture_form(scene: crate::screens::gallery::Scene) -> image::RgbaImage {
    let configuration = synthetic_configuration();
    let tab = AppState::for_test().active();
    let mut installed = false;
    let sessions: Vec<_> = synthetic_resumable_sessions()
        .into_iter()
        .map(crate::discovery::NativeSession::fixture)
        .collect();
    let mut harness = gallery_harness(
        egui::vec2(1000.0, 1000.0),
        move |ui, ()| {
            if !installed {
                crate::screens::gallery::install(ui.ctx(), tab, scene);
                installed = true;
            }
            let _ = screens::show_launcher(
                ui,
                tab,
                &configuration,
                true,
                None,
                false,
                &sessions,
                &synthetic_tmux_sessions(),
                &synthetic_screen_sessions(),
                &[],
            );
        },
        (),
    );
    finish(&mut harness)
}
