use super::*;

pub(crate) fn local_menu_label() -> &'static str {
    reveal_in_file_manager_label()
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Scene {
    Authentication,
    PrivateKey,
    UnknownKey,
    ChangedKey,
    Connecting,
    Failed,
    Disconnected,
    OperationError,
    Transfers,
    TransferDetails,
    NewLocalDirectory,
    NewRemoteDirectory,
    DeleteLocal,
    DeleteRemote,
    TransferDetailsNarrow,
    Collision,
    Narrow,
    Swapped,
    Empty,
    LocalMenu,
    PathEditing,
    DirectoryLoading,
    FilterEmpty,
    ListingError,
}

pub(crate) struct Fixture {
    tab: SftpFileManagerTab,
    authentication: AuthenticationFormState,
    scene: Scene,
    _local_fixture: Option<tempfile::TempDir>,
}

impl Fixture {
    pub(crate) fn new(mut tab: SftpFileManagerTab, scene: Scene) -> Self {
        tab.local_loader = LocalDirectoryLoader::paused_for_test();
        let mut authentication = AuthenticationFormState::default();
        let mut local_fixture = None;
        match scene {
            Scene::PrivateKey => authentication.mode = AuthMode::PrivateKey,
            Scene::Connecting | Scene::Failed => {
                tab.has_connected_once = false;
                tab.connection_state = if matches!(scene, Scene::Connecting) {
                    SftpConnectionState::Connecting
                } else {
                    SftpConnectionState::Failed {
                        summary: "Connection failed".into(),
                        details: "Synthetic host refused the connection; no network was used."
                            .into(),
                    }
                };
            }
            Scene::Disconnected => {
                tab.connection_state = SftpConnectionState::Disconnected {
                    summary: "Connection lost".into(),
                    details: "The last directory snapshot remains visible and read-only.".into(),
                }
            }
            Scene::OperationError => {
                tab.operation_error = Some((
                    "Could not open remote file".into(),
                    "The synthetic file exceeds the bounded text-file limit.".into(),
                ))
            }
            Scene::Transfers
            | Scene::TransferDetails
            | Scene::TransferDetailsNarrow
            | Scene::Collision => {
                for event in festerm_ssh::gallery_transfer_events(matches!(scene, Scene::Collision))
                {
                    tab.apply_transfer_event(event);
                }
            }
            Scene::NewLocalDirectory | Scene::DeleteLocal => {
                let root = tempfile::tempdir().expect("owned local file-action gallery");
                fs::write(
                    root.path().join("reviewed-report.txt"),
                    b"Owned gallery scratch file",
                )
                .unwrap();
                let snapshot =
                    festerm_ssh::read_local_directory_snapshot_sync(root.path()).unwrap();
                let selected = snapshot.entries[0].path.clone();
                tab.local_pane.set_snapshot(snapshot, None);
                tab.local_pane.select_single(&selected);
                tab.review_file_operation(PaneFocus::Local, matches!(scene, Scene::DeleteLocal));
                local_fixture = Some(root);
            }
            Scene::NewRemoteDirectory | Scene::DeleteRemote => {
                let selected = tab
                    .remote_pane
                    .snapshot
                    .as_ref()
                    .unwrap()
                    .entries
                    .iter()
                    .find(|item| item.file_type == SftpEntryType::File)
                    .unwrap()
                    .path
                    .clone();
                tab.remote_pane.select_single(&selected);
                tab.review_file_operation(PaneFocus::Remote, matches!(scene, Scene::DeleteRemote));
            }
            Scene::Swapped => tab.pane_order = SftpPaneOrderPreference::RemoteLeft,
            Scene::Empty => {
                for pane in [&mut tab.local_pane, &mut tab.remote_pane] {
                    let mut snapshot = pane.snapshot.clone().unwrap();
                    snapshot.entries.clear();
                    pane.set_snapshot(snapshot, None);
                }
            }
            Scene::PathEditing => {
                tab.local_pane.editing_path = true;
                tab.local_pane.path_focus_requested = true;
            }
            Scene::DirectoryLoading => {
                for pane in [&mut tab.local_pane, &mut tab.remote_pane] {
                    pane.snapshot = None;
                    pane.loading = true;
                }
            }
            Scene::FilterEmpty => {
                for pane in [&mut tab.local_pane, &mut tab.remote_pane] {
                    pane.set_filter("unmatched-review-fixture".into());
                }
            }
            Scene::ListingError => {
                tab.remote_pane.error = Some("Could not list this directory".into());
                tab.remote_pane.details = Some(
                    "Synthetic permission denied; retained listing is not a successful refresh."
                        .into(),
                );
                tab.remote_pane.loading = false;
            }
            _ => {}
        }
        Self {
            tab,
            authentication,
            scene,
            _local_fixture: local_fixture,
        }
    }

    pub(crate) fn show(&mut self, ui: &mut Ui, id: crate::tabs::TabId) {
        if matches!(
            self.scene,
            Scene::TransferDetails | Scene::TransferDetailsNarrow
        ) {
            self.tab.show_transfer_drawer(ui);
            return;
        }
        let command = match self.scene {
            Scene::Authentication | Scene::PrivateKey => show_authentication_required(
                ui,
                id,
                self.tab
                    .launch_target
                    .sftp()
                    .expect("gallery uses SSH transport"),
                false,
                &mut self.authentication,
            ),
            Scene::UnknownKey | Scene::ChangedKey => {
                let mut prompt = HostKeyPrompt::new(
                    "artifacts.example.net",
                    22,
                    "SHA256:synthetic-review-fingerprint",
                );
                if matches!(self.scene, Scene::ChangedKey) {
                    prompt =
                        prompt.with_previously_trusted_fingerprint("SHA256:previous-fixture-key");
                }
                show_host_key_prompt(
                    ui,
                    id,
                    self.tab
                        .launch_target
                        .sftp()
                        .expect("gallery uses SSH transport"),
                    &prompt,
                )
            }
            _ => self.tab.show(ui, id),
        };
        assert!(
            command.is_none(),
            "capture must not start a transport or transfer"
        );
    }
}
