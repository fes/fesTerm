//! Ordinary Files worker behind the existing file-manager presentation and
//! command queue. There is no terminal, daemon session or SFTP fallback.
use super::*;
use crate::app::host_files::{
    ConnectFiles, DeleteObservation, FilesChannel, FilesEndpoint, FilesRecoveryPin, FirstFailure,
};
use festerm_ssh::RemoteTransferEndpoint;
use festerm_tunnel_bridge::{host_client::ordinary::ExpectedHost, ordinary_auth::OrdinaryHostInfo};
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
    pub(super) display_root: Option<festerm_tunnel_bridge::file_protocol::DisplayRoot>,
    pub(super) locations: Option<Vec<festerm_tunnel_bridge::file_protocol::FileLocation>>,
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
            display_root: None,
            locations: None,
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
            display_root: None,
            locations: None,
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
    pub(super) fn location(
        &self,
        path: &str,
    ) -> Option<&festerm_tunnel_bridge::file_protocol::FileLocation> {
        self.locations
            .as_ref()?
            .iter()
            .find(|root| root.contains(path))
    }
    pub(super) fn visible_path(&self, path: &str) -> String {
        if let Some(root) = self.location(path) {
            let relative = path.strip_prefix(&root.virtual_root()).unwrap_or("");
            return if relative.is_empty() {
                "/".into()
            } else {
                festerm_sftp::path::display_name(relative)
            };
        }
        if self.locations.is_some() {
            "/".into()
        } else {
            festerm_sftp::path::display_name(path)
        }
    }
    pub(super) fn display_root_for(&self, path: &str) -> String {
        if let Some(root) = self.location(path) {
            return festerm_sftp::path::display_name(root.label.as_str());
        }
        if self.locations.is_some() {
            "Allowed locations".into()
        } else {
            self.display_root.as_ref().map_or_else(
                || "Shared folder".into(),
                |root| festerm_sftp::path::display_name(root.as_str()),
            )
        }
    }
    pub(super) fn path_from_text(&self, current: &str, text: &str) -> Option<String> {
        if self.locations.is_none() {
            return Some(text.into());
        }
        let root = self.location(current)?;
        let relative =
            festerm_tunnel_bridge::file_protocol::StartDirectory::try_from(text.to_owned()).ok()?;
        Some(format!(
            "{}{}",
            root.virtual_root(),
            if relative.as_str() == "/" {
                ""
            } else {
                relative.as_str()
            }
        ))
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

pub(super) fn files_authenticated(
    info: &OrdinaryHostInfo,
    account_id: u64,
    namespaced: bool,
) -> WorkerEvent {
    WorkerEvent::FilesAuthenticated {
        pin: FilesRecoveryPin {
            account_id,
            expected_host: ExpectedHost {
                host: info.route.host.clone(),
                installation: info.installation.clone(),
                principal: info.principal.clone(),
            },
        },
        namespaced,
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
    let mut namespaced = false;
    let delete_observation = DeleteObservation::default();
    let first_failure = FirstFailure::default();
    let work = async {
        let result = match connect.await {
            Ok(mut connection) => {
                let lifetime = connection.lifetime.clone();
                let browse_namespaced = connection.browsing.client().location_chooser("/");
                namespaced =
                    browse_namespaced || connection.transfer.client().location_chooser("/");
                // Keep only the authenticated identity constraint before
                // querying Locations or Start. The tab cannot reuse a stream.
                if events
                    .send(files_authenticated(
                        connection.browsing.host_info(),
                        connection.account_id,
                        browse_namespaced,
                    ))
                    .await
                    .is_err()
                {
                    lifetime.cancel();
                    return;
                }
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
                    first_failure.clone(),
                )
                .await;
                lifetime.cancel();
                result
            }
            Err(error) => Err(error.message),
        };
        if let Err(details) = result {
            // Keep the first typed endpoint failure, not cancellation caused
            // by retiring the shared tab after that failure.
            let details = first_failure.details_or(details);
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
    reporter.report_interrupted_files_delete(&target.label, &delete_observation, &repaint);
    if let Some(transfers) = transfers {
        if let Err(error) = transfers.shutdown().await {
            reporter.report_files(
                target.label,
                error,
                FilesCleanup::Transfer { namespaced },
                &repaint,
            );
        }
    }
}

/// Called only on the newly authenticated connection, before publishing any
/// view. An explicit chooser never interprets a failed Start response.
async fn initial_navigation<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    client: &mut festerm_tunnel_bridge::file_client::Client<S>,
    preference: festerm_tunnel_bridge::file_client::NavigationPreference,
) -> Result<
    (
        Option<Vec<festerm_tunnel_bridge::file_protocol::FileLocation>>,
        Option<festerm_tunnel_bridge::file_protocol::DisplayRoot>,
        String,
    ),
    String,
> {
    use festerm_tunnel_bridge::file_client::NavigationPreference;
    client.check_current().map_err(|error| error.to_string())?;
    let locations = client
        .locations()
        .await
        .map_err(|error| error.to_string())?;
    client.check_current().map_err(|error| error.to_string())?;
    if preference == NavigationPreference::ChooseLocation && locations.is_none() {
        return Err("This host does not support choosing an allowed location. Update the host or review Start in.".into());
    }
    let display_root = client
        .display_root()
        .await
        .map_err(|error| error.to_string())?;
    let path = match preference {
        NavigationPreference::DefaultStart => client
            .start_directory()
            .await
            .map_err(|error| error.to_string())?,
        NavigationPreference::ChooseLocation => "/".into(),
    };
    client.check_current().map_err(|error| error.to_string())?;
    Ok((locations, display_root, path))
}

async fn files_operations(
    target: HostFilesLaunchTarget,
    connection: crate::app::host_files::FilesConnection,
    mut commands: tokio::sync::mpsc::Receiver<WorkerCommand>,
    notifications: SftpWorkerNotifications,
    transfers: &mut Option<SftpTransferManager>,
    delete_observation: &DeleteObservation,
    first_failure: FirstFailure,
) -> Result<(), String> {
    let SftpWorkerNotifications {
        events,
        repaint,
        connection_scope: scope,
        reporter: _,
    } = notifications;
    let mut source = FilesSource::new(&target, connection.account_id, &connection.browsing)?;
    let lifetime = connection.lifetime;
    let mut browsing = FilesEndpoint::new(
        connection.browsing,
        lifetime.clone(),
        first_failure.clone(),
        FilesChannel::Browsing,
    );
    *transfers = Some(SftpTransferManager::from_remote_endpoint(
        FilesEndpoint::new(
            connection.transfer,
            lifetime.clone(),
            first_failure,
            FilesChannel::Transfer,
        ),
    ));
    let manager = transfers.as_mut().expect("installed Files manager");
    let (locations, display_root, start_dir) = initial_navigation(
        browsing
            .client_without_reviews()
            .map_err(|error| error.to_string())?,
        connection.navigation,
    )
    .await?;
    source.locations = locations;
    source.display_root = display_root;
    let (snapshot, metadata, skipped) = browsing
        .directory_snapshot(&start_dir)
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

                    WorkerCommand::LoadRemote { path, request_id } => {
                        match browsing.directory_snapshot(&path).await {
                            Ok((snapshot, metadata, skipped)) => {
                                browsing.check_current().map_err(|error| error.to_string())?;
                                let _ = events.send(WorkerEvent::FilesDirectoryLoaded { request_id, connection_scope: scope.capture(), snapshot, metadata, skipped }).await;
                            }
                            Err(error) => { let _ = events.send(WorkerEvent::FilesDirectoryFailed { request_id, details: error.to_string() }).await; }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn desktop_files_choose_location_requires_fresh_metadata_without_start_or_folder_rpc() {
        use festerm_tunnel_bridge::{
            file_client::{Client, NavigationPreference},
            file_protocol as rpc,
            wire::{read_frame, write_frame},
        };
        for case in ["choose", "default", "legacy", "malformed", "cancel"] {
            let scope = rpc::HostScope {
                host: Default::default(),
                installation: Default::default(),
                epoch: "1".into(),
            };
            let expected = scope.clone();
            let (stream, mut peer) = tokio::io::duplex(8192);
            let cancel = CancellationToken::new();
            let retire = cancel.clone();
            let mut server = tokio::spawn(async move {
                let legacy = case == "legacy";
                let version = if legacy {
                    rpc::HOST_VERSION
                } else {
                    rpc::HOST_LOCATIONS_VERSION
                };
                let description = serde_json::json!({"version":version,"scope":expected,"filesystem":{"version":1,"write":false,"chunk_limit":rpc::CHUNK_LIMIT,"page_limit":rpc::PAGE_LIMIT,"handle_limit":rpc::HANDLE_LIMIT}});
                write_frame(
                    &mut peer,
                    rpc::HOST_DESCRIPTION,
                    &serde_json::to_vec(&description).unwrap(),
                )
                .await
                .unwrap();
                let (kind, bytes) = read_frame(&mut peer).await.unwrap();
                assert_eq!(kind, rpc::OPEN);
                let opened: rpc::HostOpen = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    opened.version,
                    if legacy {
                        rpc::HOST_REVIEWED_VERSION
                    } else {
                        rpc::HOST_LOCATIONS_VERSION
                    }
                );
                let operations = if legacy {
                    vec![]
                } else {
                    vec!["locations", "start_directory"]
                };
                let ready = serde_json::json!({"id":0,"result":{"type":"ready_v3","capabilities":{"version":3,"write":false,"operations":operations,"chunk_limit":rpc::CHUNK_LIMIT,"page_limit":rpc::PAGE_LIMIT,"handle_limit":rpc::HANDLE_LIMIT}}});
                write_frame(
                    &mut peer,
                    rpc::RESPONSE,
                    &serde_json::to_vec(&ready).unwrap(),
                )
                .await
                .unwrap();
                if !legacy {
                    let (_, bytes) = read_frame(&mut peer).await.unwrap();
                    let request: rpc::Request = serde_json::from_slice(&bytes).unwrap();
                    assert!(matches!(request.operation, rpc::Operation::Locations {}));
                    let selector = if case == "malformed" {
                        "not-a-selector"
                    } else {
                        "0123456789abcdef0123456789abcdef"
                    };
                    let reply = serde_json::json!({"id":request.id,"result":{"type":"locations","locations":[{"selector":selector,"label":"/Users/عربي","available":true,"mutations_available":false}]}});
                    write_frame(
                        &mut peer,
                        rpc::RESPONSE,
                        &serde_json::to_vec(&reply).unwrap(),
                    )
                    .await
                    .unwrap();
                    if case == "cancel" {
                        retire.cancel();
                    }
                    if case == "default" {
                        let (_, bytes) = read_frame(&mut peer).await.unwrap();
                        let request: rpc::Request = serde_json::from_slice(&bytes).unwrap();
                        assert!(matches!(
                            request.operation,
                            rpc::Operation::StartDirectory {}
                        ));
                        let reply = serde_json::json!({"id":request.id,"error":"not_found"});
                        write_frame(
                            &mut peer,
                            rpc::RESPONSE,
                            &serde_json::to_vec(&reply).unwrap(),
                        )
                        .await
                        .unwrap();
                    }
                }
                assert!(
                    read_frame(&mut peer).await.is_err(),
                    "no automatic folder/stat/mutation or failed-Start retry"
                );
            });
            let (mut client, _) = Client::open_host_reviewed(
                stream,
                &scope.host,
                &scope.installation,
                cancel.clone(),
            )
            .await
            .unwrap()
            .unwrap();
            let preference = if case == "default" {
                NavigationPreference::DefaultStart
            } else {
                NavigationPreference::ChooseLocation
            };
            let result = initial_navigation(&mut client, preference).await;
            if case == "choose" {
                let (locations, display, path) = result.unwrap();
                assert_eq!(locations.unwrap()[0].label.as_str(), "/Users/عربي");
                assert!(display.is_none());
                assert_eq!(path, "/");
                let mut browsing = FilesEndpoint::from_client(
                    client,
                    cancel.clone(),
                    FirstFailure::default(),
                    FilesChannel::Browsing,
                );
                let (snapshot, _, skipped) = browsing.directory_snapshot(&path).await.unwrap();
                assert_eq!(snapshot.path, SftpPath::remote("/"));
                assert!(snapshot.entries.is_empty());
                assert_eq!(skipped, 0);
                browsing.check_current().unwrap();
                browsing.client_without_reviews().unwrap().cancel();
            } else {
                assert!(result.is_err(), "{case} must not publish a chooser");
                if case == "legacy" {
                    assert!(result.unwrap_err().contains("does not support"));
                }
                client.cancel();
            }
            match tokio::time::timeout(std::time::Duration::from_secs(3), &mut server).await {
                Ok(result) => result.unwrap(),
                Err(error) => {
                    server.abort();
                    let _ = server.await;
                    panic!("owned scripted server did not drain: {error}");
                }
            }
        }
    }

    #[tokio::test]
    async fn desktop_files_detached_delete_reports_confirmed_removals_without_selectors() {
        use festerm_tunnel_bridge::{
            file_client::Client,
            file_protocol as rpc,
            wire::{read_frame, write_frame},
        };

        async fn request(peer: &mut tokio::io::DuplexStream) -> rpc::Request {
            let (kind, bytes) = read_frame(peer).await.unwrap();
            assert_eq!(kind, rpc::REQUEST);
            serde_json::from_slice(&bytes).unwrap()
        }

        async fn reply(peer: &mut tokio::io::DuplexStream, id: u32, result: rpc::Result) {
            let response = rpc::Response {
                id,
                result: Some(result),
                error: None,
                recovery_path: None,
            };
            write_frame(peer, rpc::RESPONSE, &serde_json::to_vec(&response).unwrap())
                .await
                .unwrap();
        }

        const SELECTOR: &str = "0123456789abcdef0123456789abcdef";
        let paths =
            ["first.txt", "pending.txt", "untouched.txt"].map(|name| format!("/{SELECTOR}/{name}"));
        let server_paths = paths.clone();
        let scope = rpc::HostScope {
            host: Default::default(),
            installation: Default::default(),
            epoch: "1".into(),
        };
        let expected = scope.clone();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let (pending_seen, pending) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let description = rpc::HostDescription {
                version: rpc::HOST_LOCATIONS_VERSION,
                scope: expected,
                filesystem: Some(rpc::Capabilities {
                    version: rpc::VERSION,
                    write: true,
                    chunk_limit: rpc::CHUNK_LIMIT,
                    page_limit: rpc::PAGE_LIMIT,
                    handle_limit: rpc::HANDLE_LIMIT,
                }),
            };
            write_frame(
                &mut peer,
                rpc::HOST_DESCRIPTION,
                &serde_json::to_vec(&description).unwrap(),
            )
            .await
            .unwrap();
            let (kind, frame) = read_frame(&mut peer).await.unwrap();
            assert_eq!(kind, rpc::OPEN);
            let open: rpc::HostOpen = serde_json::from_slice(&frame).unwrap();
            assert_eq!(open.version, rpc::HOST_LOCATIONS_VERSION);
            reply(
                &mut peer,
                0,
                rpc::Result::ReadyV3 {
                    capabilities: rpc::OperationCapabilities {
                        version: rpc::HOST_REVIEWED_VERSION,
                        write: true,
                        operations: ["locations", "start_directory", "delete"]
                            .map(str::to_owned)
                            .into(),
                        chunk_limit: rpc::CHUNK_LIMIT,
                        page_limit: rpc::PAGE_LIMIT,
                        handle_limit: rpc::HANDLE_LIMIT,
                    },
                },
            )
            .await;
            for (path, handle) in server_paths.into_iter().zip(1..=3) {
                let query = request(&mut peer).await;
                assert!(matches!(query.operation, rpc::Operation::ReviewEntry {
                    path: ref actual,
                    purpose: rpc::ReviewPurpose::Delete,
                } if actual == &path));
                reply(
                    &mut peer,
                    query.id,
                    rpc::Result::EntryReview {
                        handle,
                        metadata: rpc::Metadata {
                            kind: rpc::Kind::File,
                            size: "4".into(),
                            modified_ms: None,
                        },
                    },
                )
                .await;
            }
            let first = request(&mut peer).await;
            assert!(matches!(
                first.operation,
                rpc::Operation::Delete { review: 1 }
            ));
            reply(&mut peer, first.id, rpc::Result::Deleted).await;
            let second = request(&mut peer).await;
            assert!(matches!(
                second.operation,
                rpc::Operation::Delete { review: 2 }
            ));
            pending_seen.send(()).unwrap();
            assert!(
                read_frame(&mut peer).await.is_err(),
                "the third deletion or a replay must never be sent"
            );
        });

        let lifetime = CancellationToken::new();
        let (client, _) =
            Client::open_host_reviewed(stream, &scope.host, &scope.installation, lifetime.clone())
                .await
                .unwrap()
                .unwrap();
        let mut browsing = FilesEndpoint::from_client(
            client,
            lifetime,
            FirstFailure::default(),
            FilesChannel::Browsing,
        );
        let (review, _) = browsing
            .prepare_delete(&paths, CancellationToken::new())
            .await
            .unwrap();
        let observation = DeleteObservation::default();
        {
            let work = browsing.commit_delete(review, &observation);
            tokio::pin!(work);
            tokio::select! {
                result = pending => result.unwrap(),
                _ = &mut work => panic!("the pending deletion reply was withheld"),
                _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => panic!("the peer did not receive the second deletion"),
            }
        }
        drop(browsing);
        let (sender, notices) = mpsc::sync_channel(1);
        let reporter = SftpCleanupReporter::default();
        reporter.set_sender(sender);
        reporter.report_interrupted_files_delete(
            "Owned Files",
            &observation,
            &egui::Context::default(),
        );
        let notice = notices.try_recv().unwrap();
        assert_eq!(notice.headline, "Files deletion outcome is unknown");
        assert!(notice.detail.contains("1 of 3 removals were confirmed"));
        assert!(notice.detail.contains("Inspect the current target"));
        assert!(notice.detail.contains("will not be retried automatically"));
        assert!(!notice.detail.contains("upload") && !notice.detail.contains(SELECTOR));
        assert!(!format!("{notice:?}").contains(SELECTOR));
        assert!(matches!(
            notice._cleanup_diagnostic.as_ref().unwrap().error(),
            festerm_ssh::SftpSessionError::PublicationUncertain { path, reason }
                if path == &paths[1] && reason.starts_with("The deletion result was not observed.")
        ));
        assert!(
            observation.interrupted().is_none(),
            "the notice consumed the owner observation"
        );
        tokio::time::timeout(std::time::Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn desktop_files_start_directory_snapshot_errors_sanitize_details() {
        use egui_kittest::kittest::Queryable;
        use festerm_tunnel_bridge::{
            file_client::{Client, NavigationPreference},
            file_protocol as rpc,
            wire::{read_frame, write_frame},
        };
        const SELECTOR: &str = "0123456789abcdef0123456789abcdef";
        for (namespaced, tagged) in [(false, false), (true, false), (true, true)] {
            let raw = if namespaced {
                format!("/{SELECTOR}/starting-folder")
            } else {
                "/عربي/\u{202e}hidden\u{2066}".to_owned()
            };
            let scope = rpc::HostScope {
                host: Default::default(),
                installation: Default::default(),
                epoch: "1".into(),
            };
            let expected = scope.clone();
            let (stream, mut peer) = tokio::io::duplex(8192);
            let wire_path = raw.clone();
            let mut server = tokio::spawn(async move {
                let (version, mut operations) = if namespaced {
                    (
                        rpc::HOST_LOCATIONS_VERSION,
                        vec!["locations", "start_directory"],
                    )
                } else {
                    (rpc::HOST_VERSION, vec!["start_directory"])
                };
                if tagged {
                    operations.push("read_links");
                }
                let description = serde_json::json!({"version":version,"scope":expected,"filesystem":{"version":rpc::VERSION,"write":tagged,"chunk_limit":rpc::CHUNK_LIMIT,"page_limit":rpc::PAGE_LIMIT,"handle_limit":rpc::HANDLE_LIMIT}});
                write_frame(
                    &mut peer,
                    rpc::HOST_DESCRIPTION,
                    &serde_json::to_vec(&description).unwrap(),
                )
                .await
                .unwrap();
                assert_eq!(read_frame(&mut peer).await.unwrap().0, rpc::OPEN);
                let ready = serde_json::json!({"id":0,"result":{"type":"ready_v3","capabilities":{"version":rpc::HOST_REVIEWED_VERSION,"write":tagged,"operations":operations,"chunk_limit":rpc::CHUNK_LIMIT,"page_limit":rpc::PAGE_LIMIT,"handle_limit":rpc::HANDLE_LIMIT}}});
                write_frame(
                    &mut peer,
                    rpc::RESPONSE,
                    &serde_json::to_vec(&ready).unwrap(),
                )
                .await
                .unwrap();
                if namespaced {
                    let (_, bytes) = read_frame(&mut peer).await.unwrap();
                    let request: rpc::Request = serde_json::from_slice(&bytes).unwrap();
                    assert!(matches!(request.operation, rpc::Operation::Locations {}));
                    let reply = serde_json::json!({"id":request.id,"result":{"type":"locations","locations":[{"selector":SELECTOR,"label":"/Volumes/Allowed","available":true,"mutations_available":tagged}]}});
                    write_frame(
                        &mut peer,
                        rpc::RESPONSE,
                        &serde_json::to_vec(&reply).unwrap(),
                    )
                    .await
                    .unwrap();
                }
                let (_, bytes) = read_frame(&mut peer).await.unwrap();
                let request: rpc::Request = serde_json::from_slice(&bytes).unwrap();
                assert!(matches!(
                    request.operation,
                    rpc::Operation::StartDirectory {}
                ));
                let start = serde_json::json!({"id":request.id,"result":{"type":"start_directory","path":&wire_path}});
                write_frame(
                    &mut peer,
                    rpc::RESPONSE,
                    &serde_json::to_vec(&start).unwrap(),
                )
                .await
                .unwrap();
                let (_, bytes) = read_frame(&mut peer).await.unwrap();
                let request: rpc::Request = serde_json::from_slice(&bytes).unwrap();
                if tagged {
                    assert!(
                        matches!(request.operation, rpc::Operation::OpenReadableDirectory { path } if path == wire_path)
                    );
                    let opened = serde_json::json!({"id":request.id,"result":{"type":"readable_directory","handle":2,"metadata":{"kind":"directory","size":"0","modified_ms":"17"},"read_only":true}});
                    write_frame(
                        &mut peer,
                        rpc::RESPONSE,
                        &serde_json::to_vec(&opened).unwrap(),
                    )
                    .await
                    .unwrap();
                    let (_, bytes) = read_frame(&mut peer).await.unwrap();
                    let request: rpc::Request = serde_json::from_slice(&bytes).unwrap();
                    assert!(matches!(
                        request.operation,
                        rpc::Operation::ReadDirectory { handle: 2, .. }
                    ));
                    let page = serde_json::json!({"id":request.id,"result":{"type":"readable_entries","entries":[{"name":"notes.md","kind":"file","read_only":true}],"done":true,"skipped":0}});
                    write_frame(
                        &mut peer,
                        rpc::RESPONSE,
                        &serde_json::to_vec(&page).unwrap(),
                    )
                    .await
                    .unwrap();
                    let (_, bytes) = read_frame(&mut peer).await.unwrap();
                    let request: rpc::Request = serde_json::from_slice(&bytes).unwrap();
                    assert!(matches!(
                        request.operation,
                        rpc::Operation::Close { handle: 2 }
                    ));
                    let closed = serde_json::json!({"id":request.id,"result":{"type":"closed"}});
                    write_frame(
                        &mut peer,
                        rpc::RESPONSE,
                        &serde_json::to_vec(&closed).unwrap(),
                    )
                    .await
                    .unwrap();
                    return;
                }
                assert!(
                    matches!(request.operation, rpc::Operation::Stat { path } if path == wire_path)
                );
                let metadata = serde_json::json!({"id":request.id,"result":{"type":"stat","metadata":{"kind":"directory","size":"0","modified_ms":null}}});
                write_frame(
                    &mut peer,
                    rpc::RESPONSE,
                    &serde_json::to_vec(&metadata).unwrap(),
                )
                .await
                .unwrap();
                let (_, bytes) = read_frame(&mut peer).await.unwrap();
                let request: rpc::Request = serde_json::from_slice(&bytes).unwrap();
                assert!(
                    matches!(request.operation, rpc::Operation::OpenDirectory { path } if path == wire_path)
                );
                let response = serde_json::json!({"id":request.id,"error":"not_found"});
                write_frame(
                    &mut peer,
                    rpc::RESPONSE,
                    &serde_json::to_vec(&response).unwrap(),
                )
                .await
                .unwrap();
            });
            let (mut client, _) = Client::open_host_reviewed(
                stream,
                &scope.host,
                &scope.installation,
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(client.location_chooser("/"), namespaced);
            let (_, _, start) = initial_navigation(&mut client, NavigationPreference::DefaultStart)
                .await
                .unwrap();
            assert_eq!(start, raw, "valid wire path must not be normalized");
            let first = FirstFailure::default();
            let lifetime = CancellationToken::new();
            let mut browsing = festerm_tunnel_bridge::files_endpoint::FilesEndpoint::from_client(
                client,
                lifetime.clone(),
                first.clone(),
                FilesChannel::Browsing,
            );
            let snapshot = browsing.directory_snapshot(&start).await;
            if tagged {
                let (snapshot, metadata, skipped) = snapshot.unwrap();
                assert_eq!(snapshot.path, SftpPath::remote(&raw));
                assert_eq!(metadata.size, Some(0));
                assert_eq!(
                    metadata.modified_at,
                    Some(std::time::UNIX_EPOCH + std::time::Duration::from_millis(17)),
                );
                assert_eq!(metadata.permissions, Some(0o500));
                assert_eq!(skipped, 0);
                assert_eq!(
                    snapshot.entries[0].path,
                    SftpPath::remote(format!("{raw}/notes.md"))
                );
                assert_eq!(snapshot.entries[0].permissions, Some(0o500));
                tokio::time::timeout(std::time::Duration::from_secs(3), server)
                    .await
                    .unwrap()
                    .unwrap();
                continue;
            }
            let error = snapshot.unwrap_err();
            assert!(
                matches!(&error, festerm_ssh::SftpSessionError::RemoteOperationFailed { path, .. } if path == &raw)
            );
            let details = first.details_or(error.to_string());
            assert!(
                details.contains(&raw),
                "keep the raw diagnostic through worker reporting"
            );
            let mut tab = SftpFileManagerTab::disconnected_files(
                HostFilesLaunchTarget {
                    label: "Owned Files".into(),
                    profile_id: None,
                    account: None,
                    locator: "owned.use".into(),
                },
                PathBuf::from("/"),
                SftpPaneOrderPreference::LocalLeft,
                &egui::Context::default(),
            );
            tab.apply_event(WorkerEvent::FilesAuthenticated {
                pin: FilesRecoveryPin {
                    account_id: 42,
                    expected_host: ExpectedHost {
                        host: festerm_tunnel_bridge::owner_auth::PinnedHost {
                            host: scope.host,
                            host_key_fingerprint: format!("SHA256:{}", "A".repeat(43)),
                        },
                        installation: scope.installation,
                        principal: Default::default(),
                    },
                },
                namespaced,
            });
            tab.apply_event(WorkerEvent::ConnectionFailed {
                summary: "Could not continue the Files session.".into(),
                details: details.clone(),
            });
            assert!(
                matches!(&tab.connection_state, SftpConnectionState::Failed { details: stored, .. } if stored == &details)
            );
            let tab_id = crate::tabs::AppState::for_test().active();
            let mut harness = egui_kittest::Harness::builder().build_ui_state(
                move |ui, tab| {
                    let _ = tab.show_connection_status_banner(ui, tab_id);
                },
                tab,
            );
            harness.run();
            if namespaced {
                harness.get_by_label("Files could not open the initial folder. Choose an allowed location or review the host.");
            } else {
                harness.get_by_label(&festerm_sftp::path::display_name(&details));
            }
            assert!(harness.query_by_label(&details).is_none());
            lifetime.cancel();
            assert!(browsing.check_current().is_err());
            assert!(
                first.details_or(details).contains("browsing"),
                "existing typed first-failure retention remains in control"
            );
            match tokio::time::timeout(std::time::Duration::from_secs(3), &mut server).await {
                Ok(result) => result.unwrap(),
                Err(error) => {
                    server.abort();
                    let _ = server.await;
                    panic!("{error}");
                }
            }
        }
    }
}
