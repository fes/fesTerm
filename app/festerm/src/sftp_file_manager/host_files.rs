//! Ordinary Files worker behind the existing file-manager presentation and
//! command queue. There is no terminal, daemon session or SFTP fallback.
use super::*;
use crate::app::host_files::{ConnectFiles, DeleteObservation, FilesEndpoint};
use festerm_ssh::RemoteTransferEndpoint;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub(super) struct FilesSource {
    host: String,
    port: u16,
    account_id: u64,
    fingerprint: String,
    epoch: u64,
    binding: String,
    pub(super) delete_advertised: bool,
}
impl FilesSource {
    #[cfg(test)]
    pub(super) fn reviewed_delete_fixture(delete_advertised: bool) -> Self {
        Self {
            host: "owned.use".into(),
            port: 1,
            account_id: 1,
            fingerprint: "owned".into(),
            epoch: 1,
            binding: "owned-test-binding".into(),
            delete_advertised,
        }
    }

    fn new(
        target: &HostFilesLaunchTarget,
        account_id: u64,
        files: &festerm_tunnel_bridge::host_client::ordinary::Files,
    ) -> Result<Self, String> {
        let scope = files.scope();
        let info = files.host_info();
        Ok(Self {
            delete_advertised: files.delete_advertised(),
            host: target.locator.clone(),
            port: info.route.port,
            account_id,
            fingerprint: info.route.host.host_key_fingerprint.clone(),
            epoch: scope
                .epoch
                .parse()
                .map_err(|_| "The Files lifecycle scope is invalid.")?,
            binding: format!(
                "ordinary-files:{}:{}:{}:{}",
                scope.host,
                scope.installation,
                scope.epoch,
                uuid::Uuid::new_v4()
            ),
        })
    }
    pub(super) fn markdown(
        &self,
        profile_id: Option<&str>,
        path: &str,
    ) -> Option<RemoteMarkdownSource> {
        let account = format!("github:{}", self.account_id);
        let owner = match profile_id {
            Some(id) => RemoteSourceOwner::username_and_profile(account, id),
            None => RemoteSourceOwner::username(account),
        }
        .ok()?;
        RemoteMarkdownSource::new(
            &self.host,
            self.port,
            owner,
            &self.fingerprint,
            path,
            self.epoch,
        )
        .ok()
        .map(|source| {
            source
                .with_dev_tunnel_provider()
                .with_live_binding(&self.binding)
        })
    }
}

impl SftpFileManagerTab {
    pub(crate) fn new_files(
        target: HostFilesLaunchTarget,
        connect: ConnectFiles,
        cancel: CancellationToken,
        local_directory: PathBuf,
        order: SftpPaneOrderPreference,
        context: &egui::Context,
    ) -> Self {
        let worker_target = target.clone();
        Self::new_worker(
            FileManagerLaunchTarget::HostFiles(target),
            local_directory,
            order,
            context,
            Some(cancel),
            move |commands, events, repaint, owner| {
                run_files_worker(worker_target, connect, commands, events, repaint, owner)
            },
        )
    }
}

async fn run_files_worker(
    target: HostFilesLaunchTarget,
    connect: ConnectFiles,
    commands: tokio::sync::mpsc::Receiver<WorkerCommand>,
    events: WorkerEventSender,
    repaint: egui::Context,
    owner: SftpWorkerOwner,
) {
    let SftpWorkerOwner {
        connection_scope,
        mut shutdown,
        reporter,
    } = owner;
    let mut transfers = None;
    let delete_observation = DeleteObservation::default();
    let work = async {
        let result = match connect.await {
            Ok(connection) => {
                let lifetime = connection.lifetime.clone();
                let result = files_operations(
                    target.clone(),
                    connection,
                    commands,
                    SftpWorkerNotifications {
                        events: events.clone(),
                        repaint: repaint.clone(),
                        connection_scope: connection_scope.clone(),
                        reporter: reporter.clone(),
                    },
                    &mut transfers,
                    &delete_observation,
                )
                .await;
                lifetime.cancel();
                result
            }
            Err(error) => Err(error.message),
        };
        if let Err(details) = result {
            connection_scope.retire();
            let _ = events
                .send(WorkerEvent::ConnectionFailed {
                    summary: "Could not continue the Files session.".into(),
                    details,
                })
                .await;
            repaint.request_repaint();
        }
    };
    let _ = wait_for_sftp_tab_owner(work, &mut shutdown).await;
    connection_scope.retire();
    if let Some(error) = delete_observation.interrupted() {
        reporter.report_files(target.label.clone(), error, &repaint);
    }
    if let Some(transfers) = transfers {
        if let Err(error) = transfers.shutdown().await {
            reporter.report_files(target.label, error, &repaint);
        }
    }
}

async fn files_operations(
    target: HostFilesLaunchTarget,
    connection: crate::app::host_files::FilesConnection,
    mut commands: tokio::sync::mpsc::Receiver<WorkerCommand>,
    notifications: SftpWorkerNotifications,
    transfers: &mut Option<SftpTransferManager>,
    delete_observation: &DeleteObservation,
) -> Result<(), String> {
    let SftpWorkerNotifications {
        events,
        repaint,
        connection_scope: scope,
        reporter: _,
    } = notifications;
    let source = FilesSource::new(&target, connection.account_id, &connection.browsing)?;
    let lifetime = connection.lifetime;
    let mut browsing = FilesEndpoint::new(connection.browsing, lifetime.clone());
    *transfers = Some(SftpTransferManager::from_remote_endpoint(
        FilesEndpoint::new(connection.transfer, lifetime.clone()),
    ));
    let manager = transfers.as_mut().expect("installed Files manager");
    let (snapshot, metadata, skipped) = browsing
        .directory_snapshot("/")
        .await
        .map_err(|error| error.to_string())?;
    browsing
        .check_current()
        .map_err(|error| error.to_string())?;
    events
        .send(WorkerEvent::FilesConnected {
            connection_scope: scope.capture(),
            snapshot,
            metadata,
            source: source.clone(),
            skipped,
        })
        .await
        .map_err(|_| "The Files tab closed.")?;
    repaint.request_repaint();
    let mut liveness = tokio::time::interval(SFTP_LIVENESS_CHECK_INTERVAL);
    liveness.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    liveness.reset();
    loop {
        let review_stop = browsing.delete_review_stop();
        tokio::select! {
            biased;
            _ = lifetime.cancelled() => return Err("Files connection, authority or tab lifetime ended. Reconnect explicitly after reviewing the account.".into()),
            _ = async { match review_stop { Some(stop) => stop.cancelled().await, None => std::future::pending::<()>().await } } => {
                browsing.cancel_delete_review().await.map_err(|error| error.to_string())?;
            }
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()); };
                match command {
                    WorkerCommand::InspectUploadLocation(_) => {
                        let _ = events.send(WorkerEvent::TransferCommandFailed { action: "inspect interrupted SFTP upload", details: "Select a connected SFTP tab for this report.".into() }).await;
                    }

                    WorkerCommand::LoadRemote { path } => {
                        match browsing.directory_snapshot(&path).await {
                            Ok((snapshot, metadata, skipped)) => {
                                browsing.check_current().map_err(|error| error.to_string())?;
                                let _ = events.send(WorkerEvent::FilesDirectoryLoaded { connection_scope: scope.capture(), snapshot, metadata, skipped }).await;
                            }
                            Err(error) => { let _ = events.send(WorkerEvent::RemoteDirectoryFailed { summary: "Could not load this Files folder.".into(), details: error.to_string() }).await; }
                        }
                    }
                    WorkerCommand::ReadMarkdownSnapshot { request_id, path, max_bytes } => {
                        match browsing.markdown_snapshot(&path, max_bytes).await {
                            Ok(content) => { browsing.check_current().map_err(|error| error.to_string())?;
                                let _ = events.send(WorkerEvent::MarkdownSnapshotLoaded { request_id, path, content, verified_host_key_fingerprint: source.fingerprint.clone() }).await; }
                            Err(error) => { let _ = events.send(WorkerEvent::MarkdownSnapshotFailed { request_id, summary: "Could not open this Files preview.".into(), details: error.to_string() }).await; }
                        }
                    }
                    WorkerCommand::PrepareReviewedDelete { request, id, stop } => {
                        let result = match file_operations::reviewed_delete_paths(&request, &scope) {
                            Ok(paths) => browsing.prepare_delete(&paths, stop.clone()).await.map_err(|error| error.to_string()),
                            Err(error) => Err(error),
                        };
                        if result.is_ok() { browsing.check_current().map_err(|error| error.to_string())?; }
                        if events.send(WorkerEvent::ReviewedDeletePrepared { request, id, result }).await.is_err() {
                            stop.cancel();
                            browsing.cancel_delete_review().await.map_err(|error| error.to_string())?;
                            return Ok(());
                        }
                    }
                    WorkerCommand::CommitReviewedDelete { request, batch } => {
                        let outcome = if file_operations::reviewed_delete_paths(&request, &scope).is_ok() {
                            browsing.commit_delete(batch, delete_observation).await
                        } else {
                            browsing.cancel_delete_review().await.map_err(|error| error.to_string())?;
                            crate::app::host_files::DeleteOutcome { confirmed: 0, total: 0, failure: Some((false, "The Files connection changed; no deletion was sent.".into())) }
                        };
                        let _ = events.send(WorkerEvent::ReviewedDeleteFinished { request, outcome }).await;
                    }
                    WorkerCommand::FileOperation(request) => {
                        let result = if request.focus == PaneFocus::Local { file_operations::local(&request).await }
                            else { match file_operations::rooted_mkdir(&request, &scope) {
                                Ok(path) => browsing.mkdir(&path).await.map_err(|error| error.to_string()), Err(error) => Err(error),
                            }};
                        file_operations::report(request, result, &events, &repaint).await;
                    }
                    WorkerCommand::Enqueue(requests) => {
                        let queued = manager.enqueue_batch(requests.clone());
                        match queued { Ok(batch) => { let _ = events.send(WorkerEvent::TransferBatchAdmitted { transfer_ids: batch.transfer_ids, requests }).await; }
                            Err(error) => { let _ = events.send(WorkerEvent::TransferCommandFailed { action: "queue the Files transfer", details: error.to_string() }).await; } }
                    }
                    WorkerCommand::CancelTransfer(id) => { let _ = manager.cancel_transfer(id); }
                    WorkerCommand::CancelAllTransfers => { let _ = manager.cancel_all_transfers(); }
                    WorkerCommand::ResolveCollision(resolution) => { if let Err(error) = manager.resolve_collision(resolution) {
                        let _ = events.send(WorkerEvent::TransferCommandFailed { action: "resolve the Files collision", details: error.to_string() }).await;
                    }}
                    WorkerCommand::Reconnect => { let _ = events.send(WorkerEvent::TransferCommandFailed { action: "reconnect Files", details: "Reconnect requires a new explicit foreground account review.".into() }).await; }
                }
                repaint.request_repaint();
            }
            _ = liveness.tick() => {
                browsing.ping().await.map_err(|error| format!("The Files connection could not be observed: {error}. Reconnect explicitly."))?;
            }
            event = manager.recv_event() => {
                let Some(event) = event else { return Err("The Files transfer worker closed.".into()); };
                let _ = events.send(WorkerEvent::Transfer(event)).await; repaint.request_repaint();
            }
        }
    }
}
