//! GUI review custody: metadata and an opaque batch ID only. All native and
//! protocol handles remain in the already-authenticated browsing worker.
use super::*;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(super) struct Review {
    pub(super) batch: u64,
    pub(super) stop: CancellationToken,
}
pub(super) struct Preparation {
    id: uuid::Uuid,
    request: Request,
    selection: BTreeSet<String>,
    pub(super) stop: CancellationToken,
}

pub(in super::super) fn paths(
    request: &Request,
    scope: &ConnectionScope,
) -> Result<Vec<String>, String> {
    checked_request(request)?;
    if request.focus != PaneFocus::Remote || !scope.matches(&request.scope) {
        return Err("The reviewed Files connection changed.".into());
    }
    let Operation::Delete(items) = &request.operation else {
        return Err("Expected a deletion selection.".into());
    };
    if items.len() > 8
        || items.iter().any(|item| {
            !matches!(
                item.file_type,
                SftpEntryType::File | SftpEntryType::Directory
            )
        })
    {
        return Err(
            "Select up to 8 regular files or empty folders. Links are not supported.".into(),
        );
    }
    items
        .iter()
        .map(|item| match &item.path {
            SftpPath::Remote(path) => Ok(path.clone()),
            _ => Err("Expected a rooted remote path.".into()),
        })
        .collect()
}

impl SftpFileManagerTab {
    pub(in super::super) fn prepare_rooted_delete(&mut self) {
        if !self.remote_delete_available() {
            self.operation_error = Some(("Remote delete is unavailable".into(), "Deletion is unavailable for this connection. Review the host’s separate delete permission, then reconnect. Upload permission alone does not allow deleting.".into()));
            return;
        }
        if !self.file_operations_idle()
            || self.collision_dialog.is_some()
            || !matches!(self.connection_state, SftpConnectionState::Ready)
            || !self.file_operations.current_connection()
        {
            return;
        }
        if self.remote_pane.selected_paths.len() > 8 {
            self.operation_error = Some((
                "Could not review deletion".into(),
                "Select up to 8 regular files or empty folders.".into(),
            ));
            return;
        }
        let pane = &mut self.remote_pane;
        if pane.loading || pane.stale || !pane.is_writable() {
            return;
        }
        let selection = pane.selected_paths.clone();
        let request = Request {
            focus: PaneFocus::Remote,
            directory: pane.current_path.clone(),
            scope: self.file_operations.displayed.clone(),
            operation: Operation::Delete(pane.selected_items()),
        };
        if selection.is_empty()
            || matches!(&request.operation, Operation::Delete(items) if items.len() != selection.len())
        {
            return;
        }
        if let Err(error) = paths(&request, &self.file_operations.connection) {
            self.operation_error = Some(("Could not review deletion".into(), error));
            return;
        }
        let id = uuid::Uuid::new_v4();
        let stop = CancellationToken::new();
        if self
            .submit_worker_command(WorkerCommand::PrepareReviewedDelete {
                request: request.clone(),
                id,
                stop: stop.clone(),
            })
            .is_ok()
        {
            self.file_operations.preparing = Some(Preparation {
                id,
                request,
                selection,
                stop,
            });
        }
    }
    pub(in super::super) fn show_rooted_delete_preparation(&mut self, ui: &mut egui::Ui) {
        if self.file_operations.preparing.is_none() {
            return;
        }
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Reviewing the exact selected files and empty folders…");
            if ui.button("Cancel review").clicked() {
                self.file_operations.clear_review();
            }
        });
    }
    pub(in super::super) fn rooted_delete_prepared(
        &mut self,
        request: Request,
        id: uuid::Uuid,
        result: Result<(u64, Vec<SftpDirectoryItem>), String>,
    ) {
        if self
            .file_operations
            .preparing
            .as_ref()
            .is_none_or(|preparing| preparing.id != id)
        {
            return;
        }
        let preparing = self
            .file_operations
            .preparing
            .take()
            .expect("matched review preparation");
        let result = result.and_then(|(batch, items)| {
            let Operation::Delete(expected) = &preparing.request.operation else {
                return Err("The selection changed.".into());
            };
            if items.len() != expected.len()
                || items.iter().zip(expected).any(|(actual, old)| {
                    actual.path != old.path || actual.file_type != old.file_type
                })
            {
                return Err(
                    "A selected entry changed kind or path. Refresh and review again.".into(),
                );
            }
            Ok((batch, items))
        });
        match result {
            Ok((batch, items)) => {
                let mut draft = Draft {
                    request,
                    selection: preparing.selection,
                    focus_initial: true,
                    admission_error: None,
                    rooted_review: Some(Review {
                        batch,
                        stop: preparing.stop.clone(),
                    }),
                };
                draft.request.operation = Operation::Delete(items);
                if !preparing.stop.is_cancelled()
                    && self.remote_delete_available()
                    && self.file_review_current(&draft)
                {
                    self.file_operations.draft = Some(draft);
                } else {
                    preparing.stop.cancel();
                }
            }
            Err(error) => {
                preparing.stop.cancel();
                self.operation_error = Some((
                    "Deletion review did not complete".into(),
                    format!("{error} No deletion was sent."),
                ));
            }
        }
    }
    pub(in super::super) fn rooted_delete_finished(
        &mut self,
        request: Request,
        outcome: crate::app::host_files::DeleteOutcome,
    ) {
        self.file_operations.pending = false;
        if self.remote_pane.current_path == request.directory {
            self.refresh_pane(PaneFocus::Remote);
        }
        if let Some((unknown, reason)) = outcome.failure {
            let untouched = outcome
                .total
                .saturating_sub(outcome.confirmed + usize::from(unknown));
            self.operation_error = Some((
                if unknown {
                    "Deletion outcome is unknown"
                } else {
                    "Deletion stopped"
                }
                .into(),
                if unknown {
                    format!("Confirmed {} of {} removals. One target has an unobserved result; {untouched} later targets were not sent. {reason} Inspect the current target before any new review. Do not retry automatically.", outcome.confirmed, outcome.total)
                } else {
                    format!("Confirmed {} of {} removals; {untouched} remaining targets were not deleted by this batch. {reason} Refresh and review explicitly.", outcome.confirmed, outcome.total)
                },
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(
        root: &Path,
        allowed: bool,
    ) -> (
        SftpFileManagerTab,
        tokio::sync::mpsc::Receiver<WorkerCommand>,
    ) {
        let mut tab = super::super::tests::action_tab(root);
        tab.launch_target = FileManagerLaunchTarget::HostFiles(HostFilesLaunchTarget {
            label: "owned Files".into(),
            profile_id: None,
            account: None,
            locator: "owned.use".into(),
        });
        tab.host_files_cancel = Some(CancellationToken::new());
        tab.files_source = Some(host_files::FilesSource::reviewed_delete_fixture(allowed));
        let (sender, receiver) = tokio::sync::mpsc::channel(GUI_SFTP_COMMAND_CAPACITY);
        tab.command_sender = sender;
        (tab, receiver)
    }
    fn prepared(tab: &mut SftpFileManagerTab, command: WorkerCommand) -> CancellationToken {
        let WorkerCommand::PrepareReviewedDelete { request, id, stop } = command else {
            panic!("expected native preparation");
        };
        let Operation::Delete(mut items) = request.operation.clone() else {
            unreachable!();
        };
        items[0].size = Some(13);
        tab.apply_event(WorkerEvent::ReviewedDeletePrepared {
            request,
            id,
            result: Ok((71, items)),
        });
        stop
    }
    #[test]
    fn files_delete_dialog_requires_all_prepared_handles_and_cancel_closes_review() {
        use egui_kittest::{kittest::Queryable, Harness};
        let root = tempfile::tempdir().unwrap();
        let (mut tab, mut commands) = tab(root.path(), true);
        tab.review_file_operation(PaneFocus::Remote, true);
        assert!(
            !tab.file_operation_open(),
            "no partial or pre-identity confirmation"
        );
        assert!(tab.file_operations.preparing.is_some());
        let stop = prepared(&mut tab, commands.try_recv().unwrap());
        assert!(tab.file_operation_open());
        assert!(commands.is_empty(), "preparation does not mutate");
        let id = crate::tabs::AppState::for_test().active();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1180.0, 820.0))
            .build_ui_state(
                move |ui, tab: &mut SftpFileManagerTab| {
                    tab.show(ui, id);
                },
                tab,
            );
        harness.run();
        assert!(harness.query_by_label("Delete permanently").is_some());
        assert!(harness.query_by_label("File · 13 B · Modified —").is_some());
        harness.get_by_label("Cancel").click();
        harness.run();
        assert!(stop.is_cancelled());
        assert!(!harness.state().file_operation_open());
        assert!(commands.is_empty(), "Cancel sends no Delete");
    }
    #[test]
    fn files_delete_write_only_link_and_oversize_selections_cannot_prepare() {
        let root = tempfile::tempdir().unwrap();
        let (mut tab, commands) = tab(root.path(), false);
        tab.review_file_operation(PaneFocus::Remote, true);
        assert!(commands.is_empty());
        assert!(tab.file_operations.preparing.is_none());
        let request = Request {
            focus: PaneFocus::Remote,
            directory: SftpPath::remote("/reviewed"),
            scope: tab.file_operations.displayed.clone(),
            operation: Operation::Delete(vec![SftpDirectoryItem {
                name: "link".into(),
                path: SftpPath::remote("/reviewed/link"),
                file_type: SftpEntryType::Symlink,
                size: None,
                modified_at: None,
                permissions: None,
            }]),
        };
        assert!(paths(&request, &tab.file_operations.connection).is_err());
        let mut request = request;
        request.operation = Operation::Delete(
            (0..9)
                .map(|index| SftpDirectoryItem {
                    name: index.to_string(),
                    path: SftpPath::remote(format!("/reviewed/{index}")),
                    file_type: SftpEntryType::File,
                    size: None,
                    modified_at: None,
                    permissions: None,
                })
                .collect(),
        );
        assert!(paths(&request, &tab.file_operations.connection).is_err());
        assert!(commands.is_empty());
        let (mut bounded, commands) = self::tab(root.path(), true);
        let entries = (0..9)
            .map(|index| SftpDirectoryItem {
                name: index.to_string(),
                path: SftpPath::remote(format!("/reviewed/{index}")),
                file_type: SftpEntryType::File,
                size: None,
                modified_at: None,
                permissions: None,
            })
            .collect::<Vec<_>>();
        bounded.remote_pane.selected_paths =
            entries.iter().map(|item| path_key(&item.path)).collect();
        bounded.remote_pane.snapshot.as_mut().unwrap().entries = entries;
        bounded.review_file_operation(PaneFocus::Remote, true);
        assert!(
            commands.is_empty(),
            "oversize selection is refused before any review handle request"
        );
        assert!(bounded.file_operations.preparing.is_none());
        assert!(bounded
            .operation_error
            .as_ref()
            .unwrap()
            .1
            .contains("up to 8"));
    }
    #[test]
    fn files_delete_queued_review_after_account_retirement_is_not_disclosed_but_unknown_survives() {
        let root = tempfile::tempdir().unwrap();
        let (mut tab, mut commands) = tab(root.path(), true);
        tab.review_file_operation(PaneFocus::Remote, true);
        let WorkerCommand::PrepareReviewedDelete { request, id, stop } =
            commands.try_recv().unwrap()
        else {
            unreachable!();
        };
        let Operation::Delete(mut items) = request.operation.clone() else {
            unreachable!();
        };
        items[0].name = "late-metadata-canary".into();
        tab.event_sender
            .try_send(WorkerEvent::ReviewedDeletePrepared {
                request: request.clone(),
                id,
                result: Ok((5, items)),
            })
            .unwrap();
        tab.event_sender
            .try_send(WorkerEvent::ReviewedDeleteFinished {
                request,
                outcome: crate::app::host_files::DeleteOutcome {
                    confirmed: 1,
                    total: 3,
                    failure: Some((true, "unobserved result".into())),
                },
            })
            .unwrap();
        tab.host_files_cancel.as_ref().unwrap().cancel();
        tab.poll();
        assert!(stop.is_cancelled());
        assert!(!tab.file_operation_open());
        assert!(tab.file_operations.preparing.is_none());
        assert!(!tab
            .remote_pane
            .snapshot
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .any(|item| item.name == "late-metadata-canary"));
        let (title, detail) = tab.operation_error.as_ref().unwrap();
        assert_eq!(title, "Deletion outcome is unknown");
        assert!(detail.contains("Confirmed 1 of 3"));
        assert!(detail.contains("1 later targets were not sent"));
        assert!(
            commands.is_empty(),
            "retirement must not schedule remote refresh"
        );
    }
    #[test]
    fn files_delete_prepared_review_waits_behind_existing_transfer_collision_dialog() {
        use egui_kittest::{kittest::Queryable, Harness};
        let root = tempfile::tempdir().unwrap();
        let (mut tab, mut commands) = tab(root.path(), true);
        tab.review_file_operation(PaneFocus::Remote, true);
        let stop = prepared(&mut tab, commands.try_recv().unwrap());
        for event in festerm_ssh::gallery_transfer_events(true) {
            let collision = matches!(event, SftpTransferEvent::Collision(_));
            tab.apply_transfer_event(event);
            if collision {
                break;
            }
        }
        assert!(tab.collision_dialog.is_some());
        let id = crate::tabs::AppState::for_test().active();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1180.0, 820.0))
            .build_ui_state(
                move |ui, tab: &mut SftpFileManagerTab| {
                    tab.show(ui, id);
                },
                tab,
            );
        harness.run();
        assert!(
            harness.query_by_label("Delete permanently").is_none(),
            "the collision stays the sole foreground confirmation"
        );
        assert!(
            harness.state().file_operation_open(),
            "exact bounded review remains retained"
        );
        assert!(!stop.is_cancelled());
        assert!(
            commands.is_empty(),
            "neither confirmation mutates automatically"
        );
    }
}
