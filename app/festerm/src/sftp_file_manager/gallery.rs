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
}

impl Fixture {
    pub(crate) fn new(mut tab: SftpFileManagerTab, scene: Scene) -> Self {
        tab.local_loader = LocalDirectoryLoader::paused_for_test();
        let mut authentication = AuthenticationFormState::default();
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
            Scene::Transfers | Scene::TransferDetails | Scene::Collision => {
                for event in festerm_ssh::gallery_transfer_events(matches!(scene, Scene::Collision))
                {
                    tab.apply_transfer_event(event);
                }
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
        }
    }

    pub(crate) fn show(&mut self, ui: &mut Ui, id: crate::tabs::TabId) {
        if matches!(self.scene, Scene::TransferDetails) {
            self.tab.show_transfer_drawer(ui);
            return;
        }
        let command = match self.scene {
            Scene::Authentication | Scene::PrivateKey => show_authentication_required(
                ui,
                id,
                &self.tab.launch_target,
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
                show_host_key_prompt(ui, id, &self.tab.launch_target, &prompt)
            }
            _ => self.tab.show(ui, id),
        };
        assert!(
            command.is_none(),
            "capture must not start a transport or transfer"
        );
    }
}
