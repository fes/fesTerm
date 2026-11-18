use super::*;

/// A reconnect retires every outstanding remote review, including queued commands.
#[derive(Clone, Debug, Default)]
pub(super) struct ConnectionScope(Arc<Mutex<Arc<()>>>);

impl ConnectionScope {
    pub(super) fn capture(&self) -> Arc<()> {
        self.0.lock().expect("SFTP connection scope lock").clone()
    }

    pub(super) fn retire(&self) {
        *self.0.lock().expect("SFTP connection scope lock") = Arc::new(());
    }

    fn matches(&self, expected: &Arc<()>) -> bool {
        Arc::ptr_eq(&self.capture(), expected)
    }
}

#[derive(Debug, Clone)]
pub(super) enum Operation {
    CreateDirectory(String),
    Delete(Vec<SftpDirectoryItem>),
}

#[derive(Debug, Clone)]
pub(super) struct Request {
    pub(super) focus: PaneFocus,
    pub(super) directory: SftpPath,
    scope: Arc<()>,
    operation: Operation,
}

#[derive(Clone)]
struct Draft {
    request: Request,
    selection: BTreeSet<String>,
    focus_initial: bool,
    admission_error: Option<String>,
}

pub(super) struct State {
    pub(super) connection: ConnectionScope,
    pub(super) displayed: Arc<()>,
    draft: Option<Draft>,
    pub(super) pending: bool,
}

impl Default for State {
    fn default() -> Self {
        let connection = ConnectionScope::default();
        let displayed = connection.capture();
        Self {
            connection,
            displayed,
            draft: None,
            pending: false,
        }
    }
}

impl State {
    pub(super) fn current_connection(&self) -> bool {
        self.connection.matches(&self.displayed)
    }
}

impl SftpFileManagerTab {
    pub(super) fn file_operation_open(&self) -> bool {
        self.file_operations.draft.is_some()
    }

    pub(super) fn review_file_operation(&mut self, focus: PaneFocus, delete: bool) {
        if self.collision_dialog.is_some()
            || self.file_operations.pending
            || (focus == PaneFocus::Remote
                && (!matches!(self.connection_state, SftpConnectionState::Ready)
                    || !self.file_operations.current_connection()))
        {
            return;
        }
        let scope = self.file_operations.displayed.clone();
        let pane = pane_mut(self, focus);
        if pane.loading || pane.stale || !pane.is_writable() {
            return;
        }
        let selection = pane.selected_paths.clone();
        let operation = if delete {
            if selection.is_empty() || selection.len() > SftpTransferManager::max_batch_items() {
                return;
            }
            let items = pane.selected_items();
            if items.len() != selection.len()
                || items.iter().any(|item| {
                    item.path.parent_directory() != pane.current_path
                        || item.path.file_name().is_err()
                        || item.file_type == SftpEntryType::Other
                })
            {
                return;
            }
            Operation::Delete(items)
        } else {
            Operation::CreateDirectory(String::new())
        };
        self.file_operations.draft = Some(Draft {
            request: Request {
                focus,
                directory: pane.current_path.clone(),
                scope,
                operation,
            },
            selection,
            focus_initial: true,
            admission_error: None,
        });
    }

    fn file_review_current(&self, draft: &Draft) -> bool {
        let pane = match draft.request.focus {
            PaneFocus::Local => &self.local_pane,
            PaneFocus::Remote => &self.remote_pane,
        };
        !pane.loading
            && !pane.stale
            && pane.is_writable()
            && pane.current_path == draft.request.directory
            && (!matches!(draft.request.operation, Operation::Delete(_))
                || pane.selected_paths == draft.selection)
            && (draft.request.focus == PaneFocus::Local
                || (matches!(self.connection_state, SftpConnectionState::Ready)
                    && self
                        .file_operations
                        .connection
                        .matches(&draft.request.scope)
                    && Arc::ptr_eq(&self.file_operations.displayed, &draft.request.scope)))
    }

    pub(super) fn show_file_operation_dialog(
        &mut self,
        ctx: &egui::Context,
        tab_id: crate::tabs::TabId,
    ) {
        let Some(mut draft) = self.file_operations.draft.take() else {
            return;
        };
        let current = self.file_review_current(&draft);
        let deleting = matches!(draft.request.operation, Operation::Delete(_));
        let mut confirm = false;
        let mut cancel = false;
        let response = egui::Modal::new(egui::Id::new(("sftp-file-operation", tab_id)))
            .show(ctx, |ui| {
                ui.set_max_width(ui.available_width().min(460.0));
                ui.heading(if deleting { "Delete permanently?" } else { "New directory" });
                ui.label(format!("{} · {}", draft.request.focus.label(), if draft.request.focus == PaneFocus::Remote {
                    format!("{}@{}:{}", self.launch_target.username, self.launch_target.host, self.launch_target.port)
                } else { "This computer".to_owned() }));
                ui.label(format!("Folder: {}", draft.request.directory.display()));
                match &mut draft.request.operation {
                    Operation::CreateDirectory(name) => {
                        let field = ui.add(TextEdit::singleline(name).hint_text("Directory name").char_limit(255));
                        field.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, "Directory name"));
                        if draft.focus_initial { field.request_focus(); }
                        if !name.is_empty() && festerm_ssh::validate_sftp_child_name(name).is_err() {
                            ui.colored_label(theme::STATUS_ERROR, "Enter a single valid folder name without leading/trailing whitespace, path separators or reserved names.");
                        }
                    }
                    Operation::Delete(items) => {
                        ui.label("This permanently removes the listed files or links and empty directories. There is no trash or undo. Non-empty directories are refused.");
                        ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                            for item in items { ui.label(item.path.display()); }
                        });
                    }
                }
                if let Some(error) = &draft.admission_error { ui.colored_label(theme::STATUS_ERROR, format!("Not started: {error}")); }
                if !current { ui.colored_label(theme::STATUS_ERROR, "The folder, selection or connection changed. Cancel, refresh and review again."); }
                ui.horizontal(|ui| {
                    let button = ui.button("Cancel");
                    if deleting && draft.focus_initial { button.request_focus(); }
                    cancel = button.clicked();
                    let valid_name = match &draft.request.operation {
                        Operation::CreateDirectory(name) => festerm_ssh::validate_sftp_child_name(name).is_ok(),
                        Operation::Delete(_) => true,
                    };
                    confirm = ui.add_enabled(current && valid_name, egui::Button::new(if deleting { "Delete permanently" } else { "Create" })).clicked();
                });
            });
        draft.focus_initial = false;
        if confirm && self.file_review_current(&draft) {
            match self.submit_worker_command(WorkerCommand::FileOperation(draft.request.clone())) {
                Ok(()) => {
                    self.file_operations.pending = true;
                    return;
                }
                Err(error) => draft.admission_error = Some(error),
            }
        }
        if !cancel && !response.should_close() {
            self.file_operations.draft = Some(draft);
        }
    }

    pub(super) fn file_operation_finished(&mut self, request: Request, result: Result<(), String>) {
        self.file_operations.pending = false;
        let error = result.err();
        if pane_mut(self, request.focus).current_path == request.directory {
            self.refresh_pane(request.focus);
        }
        if let Some(error) = error {
            self.operation_error = Some((
                "File operation did not complete".into(),
                format!("{error} Refresh and review the remaining targets before trying again."),
            ));
        }
    }
}

fn checked_request(request: &Request) -> Result<(), String> {
    match &request.operation {
        Operation::CreateDirectory(name) => {
            festerm_ssh::validate_sftp_child_name(name).map_err(|e| e.to_string())
        }
        Operation::Delete(items) => {
            if items.is_empty()
                || items.len() > SftpTransferManager::max_batch_items()
                || items.iter().any(|item| {
                    item.path.parent_directory() != request.directory
                        || item.path.file_name().is_err()
                        || item.file_type == SftpEntryType::Other
                })
            {
                Err("The reviewed selection is not a supported child of this folder.".into())
            } else {
                Ok(())
            }
        }
    }
}

fn unchanged(item: &SftpDirectoryItem, current: &SftpPathMetadata) -> bool {
    item.path == current.path
        && item.file_type == current.file_type
        && item.size.is_none_or(|size| current.size == Some(size))
        && item
            .modified_at
            .is_none_or(|time| current.modified_at == Some(time))
}

pub(super) async fn local(request: &Request) -> Result<(), String> {
    checked_request(request)?;
    let request = request.clone();
    tokio::task::spawn_blocking(move || {
        let SftpPath::Local(directory) = &request.directory else {
            return Err("Expected a Local folder.".into());
        };
        match &request.operation {
            Operation::CreateDirectory(name) => {
                fs::create_dir(directory.join(name)).map_err(|e| e.to_string())
            }
            Operation::Delete(items) => {
                for (removed, item) in items.iter().enumerate() {
                    let SftpPath::Local(path) = &item.path else {
                        return Err("Expected a Local target.".into());
                    };
                    let result = (|| {
                        let metadata = fs::symlink_metadata(path)?;
                        let kind = if metadata.file_type().is_symlink() {
                            SftpEntryType::Symlink
                        } else if metadata.is_dir() {
                            SftpEntryType::Directory
                        } else if metadata.is_file() {
                            SftpEntryType::File
                        } else {
                            SftpEntryType::Other
                        };
                        if kind != item.file_type
                            || item.size.is_some_and(|n| metadata.len() != n)
                            || item
                                .modified_at
                                .is_some_and(|t| metadata.modified().ok() != Some(t))
                        {
                            return Err(std::io::Error::other("The reviewed target changed."));
                        }
                        #[cfg(windows)]
                        let link_directory = {
                            use std::os::windows::fs::FileTypeExt;
                            metadata.file_type().is_symlink_dir()
                        };
                        #[cfg(not(windows))]
                        let link_directory = false;
                        if kind == SftpEntryType::Directory || link_directory {
                            fs::remove_dir(path)
                        } else {
                            fs::remove_file(path)
                        }
                    })();
                    if let Err(error) = result {
                        return Err(format!(
                            "Confirmed {removed} of {} removals; {}: {error}",
                            items.len(),
                            item.path.display()
                        ));
                    }
                }
                Ok(())
            }
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

fn delete_command(
    item: &SftpDirectoryItem,
    metadata: &SftpPathMetadata,
) -> Result<festerm_ssh::SftpCommand, String> {
    if !unchanged(item, metadata) {
        return Err("The reviewed target changed.".into());
    }
    let SftpPath::Remote(path) = &item.path else {
        return Err("Expected a Remote target.".into());
    };
    match item.file_type {
        SftpEntryType::Directory => Ok(festerm_ssh::SftpCommand::Rmdir { path: path.clone() }),
        SftpEntryType::File | SftpEntryType::Symlink => {
            Ok(festerm_ssh::SftpCommand::Rm { path: path.clone() })
        }
        SftpEntryType::Other => Err("Unsupported selected entry type.".into()),
    }
}

pub(super) async fn remote(
    request: &Request,
    scope: &ConnectionScope,
    session: &mut festerm_ssh::SftpSession,
) -> Result<(), String> {
    checked_request(request)?;
    if !scope.matches(&request.scope) {
        return Err("The SFTP connection changed after review.".into());
    }
    let SftpPath::Remote(directory) = &request.directory else {
        return Err("Expected a Remote folder.".into());
    };
    match &request.operation {
        Operation::CreateDirectory(name) => {
            session
                .execute(festerm_ssh::SftpCommand::Mkdir {
                    path: SftpPath::remote(directory).join_child(name).display(),
                })
                .await
                .map_err(|e| e.to_string())?;
        }
        Operation::Delete(items) => {
            for (removed, item) in items.iter().enumerate() {
                let result = async {
                    let metadata = session
                        .remote_symlink_metadata(&item.path.display())
                        .await
                        .map_err(|e| e.to_string())?
                        .ok_or("The reviewed target no longer exists.")?;
                    let command = delete_command(item, &metadata)?;
                    // No reconnect or retry: an uncertain destructive result needs a fresh review.
                    session.execute(command).await.map_err(|e| e.to_string())?;
                    Ok::<(), String>(())
                }
                .await;
                if let Err(error) = result {
                    return Err(format!(
                        "Confirmed {removed} of {} removals; {}: {error}",
                        items.len(),
                        item.path.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(super) async fn report(
    request: Request,
    result: Result<(), String>,
    events: &WorkerEventSender,
    repaint: &egui::Context,
) {
    let _ = events
        .send(WorkerEvent::FileOperationFinished { request, result })
        .await;
    repaint.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::NodeT;

    fn request(directory: SftpPath, operation: Operation) -> Request {
        Request {
            focus: if directory.location() == SftpLocation::Local {
                PaneFocus::Local
            } else {
                PaneFocus::Remote
            },
            directory,
            operation,
            scope: Arc::new(()),
        }
    }

    fn action_tab(root: &Path) -> SftpFileManagerTab {
        fs::write(root.join("owned.txt"), "owned scratch").unwrap();
        SftpFileManagerTab::for_gallery(
            "Review target".into(),
            "builder".into(),
            "artifacts.example.net".into(),
            22,
            festerm_ssh::read_local_directory_snapshot_sync(root).unwrap(),
            SftpDirectorySnapshot {
                location: SftpLocation::Remote,
                path: SftpPath::remote("/reviewed"),
                loaded_at: SystemTime::now(),
                entries: vec![SftpDirectoryItem {
                    name: "remote.txt".into(),
                    path: SftpPath::remote("/reviewed/remote.txt"),
                    file_type: SftpEntryType::File,
                    size: Some(1),
                    modified_at: None,
                    permissions: Some(0o644),
                }],
            },
            Some("owned.txt"),
            Some("remote.txt"),
            SftpPaneOrderPreference::LocalLeft,
            &egui::Context::default(),
        )
    }

    #[test]
    fn file_manager_delete_buttons_require_confirmation_and_keep_the_captured_side() {
        use egui_kittest::{kittest::Queryable, Harness};
        for focus in [PaneFocus::Local, PaneFocus::Remote] {
            let root = tempfile::tempdir().unwrap();
            let mut tab = action_tab(root.path());
            let (sender, mut receiver) = tokio::sync::mpsc::channel(GUI_SFTP_COMMAND_CAPACITY);
            tab.command_sender = sender;
            let id = crate::tabs::AppState::for_test().active();
            let mut harness = Harness::builder()
                .with_size(egui::vec2(1180.0, 820.0))
                .build_ui_state(
                    move |ui, tab: &mut SftpFileManagerTab| {
                        assert!(tab.show(ui, id).is_none());
                    },
                    tab,
                );
            harness.run();
            let label = format!("Delete selected {} items", focus.label());
            harness.get_by_label(&label).click();
            harness.run();
            assert_eq!(receiver.len(), 0, "review itself cannot perform deletion");
            harness.get_by_label("Cancel").click();
            harness.run();
            assert_eq!(receiver.len(), 0, "Cancel is inert");
            assert!(root.path().join("owned.txt").exists());
            harness.get_by_label(&label).click();
            harness.run();
            harness.get_by_label("Delete permanently").click();
            harness.run();
            let WorkerCommand::FileOperation(request) = receiver.try_recv().unwrap() else {
                panic!("one reviewed operation");
            };
            assert_eq!(request.focus, focus);
            let Operation::Delete(items) = request.operation else {
                panic!("delete");
            };
            assert_eq!(items.len(), 1);
            assert_eq!(
                items[0].path,
                if focus == PaneFocus::Local {
                    SftpPath::local(root.path().join("owned.txt"))
                } else {
                    SftpPath::remote("/reviewed/remote.txt")
                }
            );
            assert_eq!(receiver.len(), 0);
            assert!(
                root.path().join("owned.txt").exists(),
                "widgets only admit typed work"
            );
        }
    }

    #[test]
    fn file_manager_new_directory_buttons_submit_only_the_reviewed_child() {
        use egui_kittest::{kittest::Queryable, Harness};
        for focus in [PaneFocus::Local, PaneFocus::Remote] {
            let root = tempfile::tempdir().unwrap();
            let mut tab = action_tab(root.path());
            let (sender, mut receiver) = tokio::sync::mpsc::channel(GUI_SFTP_COMMAND_CAPACITY);
            tab.command_sender = sender;
            let id = crate::tabs::AppState::for_test().active();
            let mut harness = Harness::builder()
                .with_size(egui::vec2(1180.0, 820.0))
                .build_ui_state(
                    move |ui, tab: &mut SftpFileManagerTab| {
                        assert!(tab.show(ui, id).is_none());
                    },
                    tab,
                );
            harness.run();
            harness
                .get_by_label(&format!("New directory in {} folder", focus.label()))
                .click();
            harness.run();
            assert!(harness
                .get_by_label("Create")
                .accesskit_node()
                .is_disabled());
            assert_eq!(receiver.len(), 0);
            harness
                .get_by_label("Directory name")
                .type_text("reviewed-child");
            harness.run();
            harness.get_by_label("Create").click();
            harness.run();
            let WorkerCommand::FileOperation(request) = receiver.try_recv().unwrap() else {
                panic!("reviewed creation");
            };
            assert_eq!(request.focus, focus);
            assert_eq!(
                request.directory,
                if focus == PaneFocus::Local {
                    SftpPath::local(root.path())
                } else {
                    SftpPath::remote("/reviewed")
                }
            );
            let Operation::CreateDirectory(name) = request.operation else {
                panic!("create directory");
            };
            assert_eq!(name, "reviewed-child");
            assert_eq!(receiver.len(), 0);
            assert!(
                !root.path().join("reviewed-child").exists(),
                "widgets do not mutate files"
            );
        }
    }

    #[test]
    fn file_manager_trim_changing_child_names_cannot_be_admitted() {
        use egui_kittest::{kittest::Queryable, Harness};
        for name in [
            "report\u{2003}",
            "\u{2003}report",
            "report\u{a0}",
            "report\u{202f}",
            " report",
            "report ",
        ] {
            assert!(
                festerm_ssh::validate_sftp_child_name(name).is_err(),
                "{name:?}"
            );
            assert!(checked_request(&request(
                SftpPath::remote("/reviewed"),
                Operation::CreateDirectory(name.into())
            ))
            .is_err());
        }
        assert!(festerm_ssh::validate_sftp_child_name("report\u{2003}inside").is_ok());
        for focus in [PaneFocus::Local, PaneFocus::Remote] {
            let root = tempfile::tempdir().unwrap();
            let mut tab = action_tab(root.path());
            let (sender, receiver) = tokio::sync::mpsc::channel(GUI_SFTP_COMMAND_CAPACITY);
            tab.command_sender = sender;
            let id = crate::tabs::AppState::for_test().active();
            let mut harness = Harness::builder()
                .with_size(egui::vec2(1180.0, 820.0))
                .build_ui_state(
                    move |ui, tab: &mut SftpFileManagerTab| {
                        assert!(tab.show(ui, id).is_none());
                    },
                    tab,
                );
            harness.run();
            harness
                .get_by_label(&format!("New directory in {} folder", focus.label()))
                .click();
            harness.run();
            harness
                .get_by_label("Directory name")
                .type_text("report\u{2003}");
            harness.run();
            assert!(harness
                .get_by_label("Create")
                .accesskit_node()
                .is_disabled());
            assert_eq!(
                receiver.len(),
                0,
                "trim-changing review cannot queue any command"
            );
            assert!(!root.path().join("report").exists());
            assert!(!root.path().join("report\u{2003}").exists());
        }
    }

    #[test]
    fn file_manager_long_create_and_delete_dialogs_fit_narrow_windows() {
        use egui_kittest::{kittest::Queryable, Harness};
        fn rectangles(shape: &egui::Shape, out: &mut Vec<egui::epaint::RectShape>) {
            match shape {
                egui::Shape::Rect(rect) => out.push(rect.clone()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        rectangles(shape, out);
                    }
                }
                _ => {}
            }
        }
        for width in [320.0, 460.0] {
            for focus in [PaneFocus::Local, PaneFocus::Remote] {
                for delete in [false, true] {
                    let root = tempfile::tempdir().unwrap();
                    let folder_name = "long-folder-".repeat(5);
                    let filename = format!("{}report.txt", "long-file-".repeat(6));
                    let directory = root.path().join(&folder_name);
                    fs::create_dir(&directory).unwrap();
                    let mut tab = action_tab(&directory);
                    fs::rename(directory.join("owned.txt"), directory.join(&filename)).unwrap();
                    tab.local_pane.set_snapshot(
                        festerm_ssh::read_local_directory_snapshot_sync(&directory).unwrap(),
                        None,
                    );
                    tab.local_pane
                        .select_single(&SftpPath::local(directory.join(&filename)));
                    let remote_directory = SftpPath::remote(format!("/reviewed/{folder_name}"));
                    let remote_file = remote_directory.join_child(&filename);
                    tab.remote_pane.set_snapshot(
                        SftpDirectorySnapshot {
                            location: SftpLocation::Remote,
                            path: remote_directory,
                            loaded_at: SystemTime::now(),
                            entries: vec![SftpDirectoryItem {
                                name: filename,
                                path: remote_file.clone(),
                                file_type: SftpEntryType::File,
                                size: Some(1),
                                modified_at: None,
                                permissions: None,
                            }],
                        },
                        None,
                    );
                    tab.remote_pane.select_single(&remote_file);
                    tab.review_file_operation(focus, delete);
                    if let Some(Draft {
                        request:
                            Request {
                                operation: Operation::CreateDirectory(name),
                                ..
                            },
                        ..
                    }) = &mut tab.file_operations.draft
                    {
                        *name = "long-child-name-".repeat(10);
                    }
                    let id = crate::tabs::AppState::for_test().active();
                    let mut harness = Harness::builder()
                        .with_size(egui::vec2(width, 700.0))
                        .build_ui_state(
                            move |ui, tab: &mut SftpFileManagerTab| {
                                tab.show_file_operation_dialog(ui.ctx(), id)
                            },
                            tab,
                        );
                    harness.ctx.set_visuals(theme::default_visuals());
                    harness.run();
                    let mut rects = Vec::new();
                    for shape in &harness.output().shapes {
                        rectangles(&shape.shape, &mut rects);
                    }
                    let panels: Vec<_> = rects
                        .iter()
                        .filter(|r| r.fill == theme::SURFACE_OVERLAY)
                        .collect();
                    assert_eq!(panels.len(), 1, "one actual production modal frame");
                    let panel = panels[0].rect;
                    assert!(
                        panel.is_finite() && panel.left() >= -1.0 && panel.right() <= width + 1.0,
                        "{focus:?} delete={delete} modal exceeds {width}px: {panel:?}"
                    );
                    for label in [
                        "Cancel",
                        if delete {
                            "Delete permanently"
                        } else {
                            "Create"
                        },
                    ] {
                        let rect = harness.get_by_label(label).rect();
                        assert!(
                            rect.left() >= panel.left() + 4.0
                                && rect.right() <= panel.right() - 4.0,
                            "{label} loses frame padding at {width}px: {rect:?} in {panel:?}"
                        );
                        assert!(rect.left() >= 0.0 && rect.right() <= width);
                    }
                    // Reuse the existing optional gallery destination for private visual review.
                    if let Some(output) = std::env::var_os("FESTERM_UI_GALLERY_OUT") {
                        let output = PathBuf::from(output);
                        fs::create_dir_all(&output).unwrap();
                        let path = output.join(format!(
                            "dialog-{}-{}-{}.png",
                            width as u32,
                            focus.label().to_ascii_lowercase(),
                            if delete { "delete" } else { "create" }
                        ));
                        assert!(!path.exists());
                        harness.render().unwrap().save(path).unwrap();
                    }
                }
            }
        }
    }

    #[test]
    fn file_manager_stale_or_full_delete_review_never_dispatches_another_target() {
        use egui_kittest::{kittest::Queryable, Harness};
        let root = tempfile::tempdir().unwrap();
        let mut tab = action_tab(root.path());
        let (sender, mut receiver) = tokio::sync::mpsc::channel(GUI_SFTP_COMMAND_CAPACITY);
        tab.command_sender = sender;
        for _ in 0..GUI_SFTP_COMMAND_CAPACITY {
            tab.command_sender
                .try_send(WorkerCommand::Reconnect)
                .unwrap();
        }
        let id = crate::tabs::AppState::for_test().active();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1180.0, 820.0))
            .build_ui_state(
                move |ui, tab: &mut SftpFileManagerTab| {
                    assert!(tab.show(ui, id).is_none());
                },
                tab,
            );
        harness.run();
        harness.get_by_label("Delete selected Remote items").click();
        harness.run();
        harness.get_by_label("Delete permanently").click();
        harness.run();
        assert!(harness.state().file_operation_open());
        let error = harness
            .state()
            .file_operations
            .draft
            .as_ref()
            .unwrap()
            .admission_error
            .clone()
            .unwrap();
        harness.get_by_label(&format!("Not started: {error}"));
        assert_eq!(receiver.len(), GUI_SFTP_COMMAND_CAPACITY);
        receiver.try_recv().unwrap();
        harness.state_mut().file_operations.connection.retire();
        harness.run();
        assert!(harness
            .get_by_label("Delete permanently")
            .accesskit_node()
            .is_disabled());
        assert_eq!(receiver.len(), GUI_SFTP_COMMAND_CAPACITY - 1);
    }

    fn local_items(root: &Path) -> Vec<SftpDirectoryItem> {
        festerm_ssh::read_local_directory_snapshot_sync(root)
            .unwrap()
            .entries
    }

    #[test]
    fn file_manager_old_directory_event_cannot_adopt_a_new_connection_scope() {
        let root = tempfile::tempdir().unwrap();
        let mut tab = action_tab(root.path());
        let old = tab.file_operations.connection.capture();
        tab.file_operations.connection.retire();
        let new = tab.file_operations.connection.capture();
        let snapshot = tab.remote_pane.snapshot.clone().unwrap();
        tab.apply_event(WorkerEvent::RemoteDirectoryLoaded {
            connection_scope: old,
            snapshot: snapshot.clone(),
            metadata: None,
            verified_host_key_fingerprint: "fixture".into(),
        });
        assert!(!tab.file_operations.current_connection());
        tab.review_file_operation(PaneFocus::Remote, false);
        assert!(!tab.file_operation_open());
        tab.apply_event(WorkerEvent::RemoteDirectoryLoaded {
            connection_scope: new,
            snapshot,
            metadata: None,
            verified_host_key_fingerprint: "fixture".into(),
        });
        tab.review_file_operation(PaneFocus::Remote, false);
        assert!(tab.file_operation_open());
    }

    #[test]
    fn file_manager_remote_link_delete_uses_remove_and_refuses_followed_target_metadata() {
        let item = SftpDirectoryItem {
            name: "link".into(),
            path: SftpPath::remote("/reviewed/link"),
            file_type: SftpEntryType::Symlink,
            size: None,
            modified_at: None,
            permissions: None,
        };
        assert_eq!(
            delete_command(&item, &item.metadata()).unwrap(),
            festerm_ssh::SftpCommand::Rm {
                path: "/reviewed/link".into()
            }
        );
        let mut followed = item.metadata();
        followed.file_type = SftpEntryType::Directory;
        assert!(delete_command(&item, &followed).is_err());
        let mut directory = item.clone();
        directory.file_type = SftpEntryType::Directory;
        assert_eq!(
            delete_command(&directory, &directory.metadata()).unwrap(),
            festerm_ssh::SftpCommand::Rmdir {
                path: "/reviewed/link".into()
            }
        );
    }

    #[tokio::test]
    async fn file_manager_local_directory_creation_is_single_child_and_never_overwrites() {
        let root = tempfile::tempdir().unwrap();
        let parent = SftpPath::local(root.path());
        let operation = request(
            parent.clone(),
            Operation::CreateDirectory("New folder 文".into()),
        );
        local(&operation).await.unwrap();
        assert!(root.path().join("New folder 文").is_dir());
        assert!(local(&operation).await.is_err());
        for name in [
            "",
            ".",
            "..",
            "../outside",
            "/outside",
            "child/grandchild",
            "child\\grandchild",
            "C:relative",
            "NUL",
        ] {
            assert!(
                checked_request(&request(
                    parent.clone(),
                    Operation::CreateDirectory(name.into())
                ))
                .is_err(),
                "{name:?}"
            );
        }
        assert_eq!(local_items(root.path()).len(), 1);
    }

    #[tokio::test]
    async fn file_manager_local_delete_handles_files_and_empty_folders_but_preserves_nonempty() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), "owned scratch").unwrap();
        fs::create_dir(root.path().join("empty")).unwrap();
        local(&request(
            SftpPath::local(root.path()),
            Operation::Delete(local_items(root.path())),
        ))
        .await
        .unwrap();
        assert!(local_items(root.path()).is_empty());
        fs::create_dir(root.path().join("nonempty")).unwrap();
        fs::write(root.path().join("nonempty/child"), "must survive").unwrap();
        assert!(local(&request(
            SftpPath::local(root.path()),
            Operation::Delete(local_items(root.path()))
        ))
        .await
        .is_err());
        assert_eq!(
            fs::read_to_string(root.path().join("nonempty/child")).unwrap(),
            "must survive"
        );
    }

    #[tokio::test]
    async fn file_manager_delete_refuses_changed_or_outside_review_targets() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), "before").unwrap();
        let items = local_items(root.path());
        fs::write(root.path().join("file"), "changed after review").unwrap();
        assert!(local(&request(
            SftpPath::local(root.path()),
            Operation::Delete(items.clone())
        ))
        .await
        .is_err());
        let other = tempfile::tempdir().unwrap();
        assert!(local(&request(
            SftpPath::local(other.path()),
            Operation::Delete(items)
        ))
        .await
        .is_err());
        assert_eq!(
            fs::read_to_string(root.path().join("file")).unwrap(),
            "changed after review"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_manager_local_delete_removes_symlink_without_following_target() {
        let root = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::write(target.path().join("preserved"), "target").unwrap();
        std::os::unix::fs::symlink(target.path(), root.path().join("link")).unwrap();
        local(&request(
            SftpPath::local(root.path()),
            Operation::Delete(local_items(root.path())),
        ))
        .await
        .unwrap();
        assert!(local_items(root.path()).is_empty());
        assert_eq!(
            fs::read_to_string(target.path().join("preserved")).unwrap(),
            "target"
        );
    }

    #[test]
    fn file_manager_review_is_bound_to_selection_folder_and_remote_connection() {
        let mut tab = super::super::tests::test_tab();
        tab.has_connected_once = true;
        tab.connection_state = SftpConnectionState::Ready;
        let path = SftpPath::remote("/reviewed/item");
        tab.remote_pane.set_snapshot(
            SftpDirectorySnapshot {
                location: SftpLocation::Remote,
                path: SftpPath::remote("/reviewed"),
                loaded_at: SystemTime::now(),
                entries: vec![SftpDirectoryItem {
                    name: "item".into(),
                    path: path.clone(),
                    file_type: SftpEntryType::File,
                    size: Some(1),
                    modified_at: None,
                    permissions: None,
                }],
            },
            None,
        );
        tab.remote_pane.select_single(&path);
        tab.review_file_operation(PaneFocus::Remote, true);
        let draft = tab.file_operations.draft.clone().unwrap();
        assert!(tab.file_review_current(&draft));
        tab.remote_pane.selected_paths.clear();
        assert!(!tab.file_review_current(&draft));
        tab.remote_pane.select_single(&path);
        tab.remote_pane.current_path = SftpPath::remote("/another");
        assert!(!tab.file_review_current(&draft));
        tab.remote_pane.current_path = draft.request.directory.clone();
        tab.file_operations.connection.retire();
        assert!(!tab.file_review_current(&draft));
        assert!(!tab.file_operations.connection.matches(&draft.request.scope));
        // The captured request still names the reviewed target, never the replacement selection.
        let Operation::Delete(items) = &draft.request.operation else {
            panic!("delete review");
        };
        assert_eq!(items[0].path, path);
    }

    #[test]
    fn file_manager_queued_completion_is_drained_before_classifying_worker_loss() {
        let mut tab = super::super::tests::test_tab();
        tab.file_operations.pending = true;
        assert!(tab.command_sender.is_closed());
        for _ in 0..GUI_SFTP_POLL_BUDGET {
            tab.event_sender
                .try_send(WorkerEvent::LocalDirectoryFailed {
                    focus: PaneFocus::Local,
                    request_id: u64::MAX,
                    summary: "stale fixture".into(),
                    details: String::new(),
                })
                .unwrap();
        }
        tab.event_sender
            .try_send(WorkerEvent::FileOperationFinished {
                request: request(
                    SftpPath::local("/not-the-current-folder"),
                    Operation::CreateDirectory("fixture".into()),
                ),
                result: Ok(()),
            })
            .unwrap();
        tab.poll();
        assert!(tab.file_operations.pending);
        assert!(
            tab.operation_error.is_none(),
            "queued actual result must not become an invented unknown result"
        );
        tab.poll();
        assert!(!tab.file_operations.pending);
        assert!(tab.operation_error.is_none());
    }

    #[tokio::test]
    async fn file_manager_local_operations_are_serviced_while_remote_retry_waits() {
        let root = tempfile::tempdir().unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        let (events, mut results) = tokio::sync::mpsc::channel(4);
        sender
            .send(WorkerCommand::FileOperation(request(
                SftpPath::local(root.path()),
                Operation::CreateDirectory("offline-local".into()),
            )))
            .await
            .unwrap();
        sender.send(WorkerCommand::Reconnect).await.unwrap();
        assert!(
            wait_for_initial_connect_retry(&mut receiver, &events, &egui::Context::default()).await
        );
        let WorkerEvent::FileOperationFinished { result, .. } = results.recv().await.unwrap()
        else {
            panic!("local result");
        };
        result.unwrap();
        assert!(root.path().join("offline-local").is_dir());
    }
}
