//! Ordinary Files worker behind the existing file-manager presentation and
//! command queue. There is no terminal, daemon session or SFTP fallback.
use super::*;
use crate::app::host_files::{ConnectFiles, FilesEndpoint};
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
}
impl FilesSource {
    fn new(
        target: &HostFilesLaunchTarget,
        account_id: u64,
        files: &festerm_tunnel_bridge::host_client::ordinary::Files,
    ) -> Result<Self, String> {
        let scope = files.scope();
        let info = files.host_info();
        Ok(Self {
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
    let work = async {
        let result = match connect.await {
            Ok(connection) => {
                let lifetime = connection.lifetime.clone();
                let result = files_operations(
                    target.clone(),
                    connection,
                    commands,
                    events.clone(),
                    repaint.clone(),
                    connection_scope.clone(),
                    &mut transfers,
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
    events: WorkerEventSender,
    repaint: egui::Context,
    scope: file_operations::ConnectionScope,
    transfers: &mut Option<SftpTransferManager>,
) -> Result<(), String> {
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
        tokio::select! {
            biased;
            _ = lifetime.cancelled() => return Err("Files authority or tab lifetime ended. Reconnect explicitly after reviewing the account.".into()),
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()); };
                match command {
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
