//! Explicit read-only navigation for a saved upload report on the selected live SFTP tab.
use super::*;

#[derive(Clone, Debug)]
pub(super) struct Request {
    scope: Arc<()>,
    fingerprint: String,
    parent: String,
}

fn parent(temporary: &str) -> Result<String, &'static str> {
    if temporary.is_empty() || temporary.len() > 4096 || temporary.chars().any(char::is_control) {
        return Err("The report's remote path cannot be inspected.");
    }
    if !temporary.starts_with('/')
        || temporary.starts_with("//")
        || temporary.contains('\\')
        || temporary
            .split('/')
            .skip(1)
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err("Copy this report and inspect its server-specific path manually; no location was guessed.");
    }
    let (directory, leaf) = temporary.rsplit_once('/').unwrap_or((".", temporary));
    if directory.trim() != directory || leaf.is_empty() || matches!(leaf, "." | "..") {
        return Err("The report's remote path cannot be inspected.");
    }
    Ok(if directory.is_empty() {
        "/".into()
    } else if directory.len() == 2
        && directory.as_bytes()[0].is_ascii_alphabetic()
        && directory.ends_with(':')
    {
        format!("{directory}/")
    } else {
        directory.into()
    })
}

impl SftpFileManagerTab {
    pub(crate) fn upload_inspection_scope(&self) -> Option<Arc<()>> {
        (self.launch_target.sftp().is_some()
            && matches!(self.connection_state, SftpConnectionState::Ready)
            && self.file_operations.current_connection()
            && self
                .verified_host_key_fingerprint
                .as_ref()
                .is_some_and(|key| !key.is_empty()))
        .then(|| self.file_operations.displayed.clone())
    }

    pub(crate) fn inspect_interrupted_upload(
        &mut self,
        report: &crate::app::sftp_recovery::Report,
        expected_scope: &Arc<()>,
    ) -> Result<(), &'static str> {
        let target = self
            .launch_target
            .sftp()
            .ok_or("Select a connected SFTP tab for this report.")?;
        if target.host != report.endpoint.host
            || target.port != report.endpoint.port
            || target.username != report.endpoint.username
        {
            return Err(
                "Select the connected SFTP tab for this exact endpoint, then inspect again.",
            );
        }
        if !matches!(self.connection_state, SftpConnectionState::Ready)
            || !self.file_operations.connection.matches(expected_scope)
            || !Arc::ptr_eq(&self.file_operations.displayed, expected_scope)
            || !self.file_operations.current_connection()
            || self.remote_pane.loading
            || !self.file_operations_idle()
        {
            return Err("Connect normally and wait for this SFTP tab to finish loading, then inspect again.");
        }
        if self.verified_host_key_fingerprint.as_deref()
            != Some(report.endpoint.fingerprint.as_str())
            || report.endpoint.fingerprint.is_empty()
        {
            return Err("This SFTP connection has a different or unverified SSH key. The report was kept; no inspection ran.");
        }
        let request = Request {
            scope: self.file_operations.displayed.clone(),
            fingerprint: report.endpoint.fingerprint.clone(),
            parent: parent(&report.temporary)?,
        };
        self.submit_worker_command(WorkerCommand::InspectUploadLocation(request))
            .map_err(|_| "Could not queue the inspection. The report was kept; try explicitly when this tab is ready.")?;
        self.remote_pane.loading = true;
        Ok(())
    }

    pub(super) fn apply_upload_inspection(
        &mut self,
        request: Request,
        result: Result<SftpDirectorySnapshot, String>,
    ) {
        if !self.file_operations.connection.matches(&request.scope)
            || !Arc::ptr_eq(&self.file_operations.displayed, &request.scope)
            || self.verified_host_key_fingerprint.as_deref() != Some(request.fingerprint.as_str())
            || !matches!(self.connection_state, SftpConnectionState::Ready)
        {
            return; // A stale completion never changes the folder or accepted identity.
        }
        self.remote_pane.loading = false;
        match result {
            Ok(snapshot) => {
                self.remote_pane.set_snapshot(snapshot, None);
                self.remote_pane.push_history(self.remote_pane.current_path.clone());
            }
            Err(_) => self.operation_error = Some((
                "Could not inspect the interrupted upload's location.".into(),
                "The inspection was not retried. The report was kept; no deletion or resume was requested. Connect normally and inspect again explicitly.".into(),
            )),
        }
    }
}

pub(super) async fn read_once<Read, Work>(
    request: &Request,
    scope: &file_operations::ConnectionScope,
    verified: Option<&str>,
    current_remote: &mut String,
    read: Read,
) -> Result<SftpDirectorySnapshot, String>
where
    Read: FnOnce(String) -> Work,
    Work: std::future::Future<Output = Result<SftpDirectorySnapshot, String>>,
{
    if !scope.matches(&request.scope)
        || verified != Some(request.fingerprint.as_str())
        || request.fingerprint.is_empty()
    {
        return Err("The SFTP connection changed before inspection.".into());
    }
    let result = read(request.parent.clone()).await; // Exactly one existing-session read; no reconnect path.
    if !scope.matches(&request.scope) {
        return Err("The SFTP connection changed during inspection.".into());
    }
    if let Ok(snapshot) = &result {
        *current_remote = snapshot.path.display();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn fixture() -> (
        SftpFileManagerTab,
        tokio::sync::mpsc::Receiver<WorkerCommand>,
        crate::app::sftp_recovery::Report,
    ) {
        let mut tab = super::super::tests::test_tab();
        let (send, receive) = tokio::sync::mpsc::channel(8);
        tab.command_sender = send;
        tab.connection_state = SftpConnectionState::Ready;
        tab.apply_event(WorkerEvent::Connected {
            connection_scope: tab.file_operations.connection.capture(),
            remote_directory: snapshot("/current"),
            remote_metadata: None,
            verified_host_key_fingerprint: "SHA256:accepted".into(),
        });
        let target = tab.launch_target.sftp().unwrap();
        let report = crate::app::sftp_recovery::Report {
            id: uuid::Uuid::new_v4().to_string(),
            endpoint: crate::app::sftp_recovery::Endpoint {
                host: target.host.clone(),
                port: target.port,
                username: target.username.clone(),
                fingerprint: "SHA256:accepted".into(),
                profile: target.profile_id.clone(),
                label: target.label.clone(),
            },
            temporary: "/reported/file.festerm-part".into(),
            destination: None,
        };
        (tab, receive, report)
    }
    fn snapshot(path: &str) -> SftpDirectorySnapshot {
        SftpDirectorySnapshot {
            location: SftpLocation::Remote,
            path: SftpPath::remote(path),
            loaded_at: SystemTime::now(),
            entries: Vec::new(),
        }
    }
    fn run<Work: std::future::Future>(work: Work) -> Work::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(work)
    }

    #[test]
    fn upload_inspection_parent_preserves_absolute_paths_and_refuses_ambiguous_forms() {
        for (path, expected) in [
            ("/part", "/"),
            ("/a/b.festerm-part", "/a"),
            ("/C:/Users/owner/part", "/C:/Users/owner"),
            ("/with space/part", "/with space"),
        ] {
            assert_eq!(parent(path).unwrap(), expected);
        }
        for path in [
            "C:/part",
            "relative-home/part",
            "part",
            "//server/share/part",
            "/a/../part",
            "/a//part",
            "",
            "/",
            "/a/",
            "/a/..",
            "/a/.",
            "/a/\n",
            "/with space /part",
            "/space\u{a0}/part",
            r"C:\Users\owner\part",
            &"x".repeat(4097),
        ] {
            assert!(parent(path).is_err());
        }
    }

    #[test]
    fn upload_inspection_routes_only_the_selected_verified_live_tab_once_without_reconnect() {
        let (mut tab, mut queue, report) = fixture();
        let before = tab.remote_pane.current_path.clone();
        tab.inspect_interrupted_upload(&report, &tab.file_operations.displayed.clone())
            .unwrap();
        assert_eq!(tab.remote_pane.current_path, before); // No optimistic navigation.
        let WorkerCommand::InspectUploadLocation(request) = queue.try_recv().unwrap() else {
            panic!("inspection must not use reconnecting LoadRemote or an upload command")
        };
        assert!(queue.try_recv().is_err());
        let mut current_remote = "/previous".to_owned();
        let reads = Cell::new(0);
        let result = run(read_once(
            &request,
            &tab.file_operations.connection,
            Some("SHA256:accepted"),
            &mut current_remote,
            |path| {
                reads.set(reads.get() + 1);
                assert_eq!(path, "/reported");
                async move { Ok(snapshot(&path)) }
            },
        ));
        assert_eq!(reads.get(), 1);
        assert_eq!(current_remote, "/reported");
        tab.apply_event(WorkerEvent::UploadLocationInspected { request, result });
        assert_eq!(tab.remote_pane.current_path, SftpPath::remote("/reported"));
        assert!(queue.try_recv().is_err());
        // Exercise actual ordinary-pane refresh and child navigation after adoption.
        tab.navigate_to_path(PaneFocus::Remote, SftpPath::remote(current_remote.clone()));
        assert!(
            matches!(queue.try_recv().unwrap(), WorkerCommand::LoadRemote { path } if path == "/reported")
        );
        tab.remote_pane.loading = false;
        let child = tab.remote_pane.current_path.join_child("child");
        tab.navigate_to_path(PaneFocus::Remote, child);
        assert!(
            matches!(queue.try_recv().unwrap(), WorkerCommand::LoadRemote { path } if path == "/reported/child")
        );
    }

    #[test]
    fn upload_inspection_rejects_endpoint_key_unready_and_stale_authority_without_queueing() {
        for case in 0..7 {
            let (mut tab, mut queue, mut report) = fixture();
            match case {
                0 => report.endpoint.host.push_str(".other"),
                1 => report.endpoint.port += 1,
                2 => report.endpoint.username.push_str("other"),
                3 => tab.verified_host_key_fingerprint = None,
                4 => tab.verified_host_key_fingerprint = Some("SHA256:changed".into()),
                5 => tab.connection_state = SftpConnectionState::Connecting,
                _ => tab.file_operations.connection.retire(),
            }
            assert!(tab
                .inspect_interrupted_upload(&report, &tab.file_operations.displayed.clone())
                .is_err());
            assert!(queue.try_recv().is_err());
            assert!(!tab.remote_pane.loading);
        }
    }

    #[test]
    fn upload_inspection_worker_refuses_changed_key_or_generation_before_any_read() {
        for case in 0..3 {
            let (mut tab, mut queue, report) = fixture();
            tab.inspect_interrupted_upload(&report, &tab.file_operations.displayed.clone())
                .unwrap();
            let WorkerCommand::InspectUploadLocation(request) = queue.try_recv().unwrap() else {
                unreachable!()
            };
            let key = match case {
                0 => None,
                1 => Some("SHA256:changed"),
                _ => {
                    tab.file_operations.connection.retire();
                    Some("SHA256:accepted")
                }
            };
            let result = run(read_once(
                &request,
                &tab.file_operations.connection,
                key,
                &mut "/previous".to_owned(),
                |_| async { panic!("refused inspection must perform zero remote reads") },
            ));
            assert!(result.is_err());
        }
    }

    #[test]
    fn upload_inspection_failed_read_is_not_retried_and_late_completion_cannot_navigate() {
        let (mut tab, mut queue, report) = fixture();
        tab.inspect_interrupted_upload(&report, &tab.file_operations.displayed.clone())
            .unwrap();
        let WorkerCommand::InspectUploadLocation(request) = queue.try_recv().unwrap() else {
            unreachable!()
        };
        let mut current_remote = "/previous".to_owned();
        let reads = Cell::new(0);
        let result = run(read_once(
            &request,
            &tab.file_operations.connection,
            Some("SHA256:accepted"),
            &mut current_remote,
            |_| {
                reads.set(reads.get() + 1);
                async { Err("one read failed".into()) }
            },
        ));
        tab.apply_event(WorkerEvent::UploadLocationInspected {
            request: request.clone(),
            result,
        });
        assert_eq!(reads.get(), 1);
        assert!(queue.try_recv().is_err());
        assert!(tab.operation_error.is_some());
        let before = tab.remote_pane.current_path.clone();
        let scope = tab.file_operations.connection.clone();
        let result = run(read_once(
            &request,
            &scope,
            Some("SHA256:accepted"),
            &mut current_remote,
            |_| async {
                scope.retire();
                Ok(snapshot("/late"))
            },
        ));
        assert!(result.is_err());
        tab.apply_event(WorkerEvent::UploadLocationInspected {
            request,
            result: Ok(snapshot("/late")),
        });
        assert_eq!(tab.remote_pane.current_path, before);
        assert_eq!(
            tab.verified_host_key_fingerprint.as_deref(),
            Some("SHA256:accepted")
        );
    }
    #[test]
    fn upload_inspection_central_command_pins_the_selected_tab_without_starting_another_connection()
    {
        let (tab, mut queue, report) = fixture();
        let mut state =
            crate::tabs::AppState::with_launcher(festerm_config::Configuration::empty());
        let id = state.active_tab().id;
        state.active_tab_mut().content = crate::tabs::TabContent::SftpFileManager(Box::new(tab));
        state.dispatch(
            crate::tabs::AppCommand::InspectInterruptedUpload {
                id: report.id.clone(),
            },
            &egui::Context::default(),
        );
        let Some(crate::app::sftp_recovery::Action::Inspect(record_id, selected)) =
            state.take_upload_recovery_action()
        else {
            panic!("central inspection action missing")
        };
        assert_eq!(record_id, report.id);
        assert_eq!(selected.tab, id);
        let changed_target = crate::app::sftp_recovery::InspectionTarget {
            tab: crate::tabs::TabId::next_for_test(),
            connection: selected.connection.clone(),
        };
        assert!(state
            .inspect_interrupted_upload(&report, &changed_target)
            .is_err());
        assert!(queue.try_recv().is_err());
        state
            .inspect_interrupted_upload(&report, &selected)
            .unwrap();
        assert!(matches!(
            queue.try_recv().unwrap(),
            WorkerCommand::InspectUploadLocation(_)
        ));
        assert!(queue.try_recv().is_err());
        assert_eq!(state.tabs().len(), 1);
    }
}
