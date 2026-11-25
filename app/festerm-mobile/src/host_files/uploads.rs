//! Presentation adapter over the existing bounded neutral transfer queue.
use super::*;
use festerm_ssh::{
    SftpCollision, SftpCollisionDecision, SftpCollisionResolution, SftpCollisionScope, SftpPath,
    SftpTransferId, SftpTransferManager, SftpTransferRequest, SftpTransferState,
};
use festerm_tunnel_bridge::files_endpoint::{FilesChannel, FilesEndpoint, FirstFailure};
use std::path::PathBuf;

struct Item {
    transfer: SftpTransferId,
    source: Option<PathBuf>,
    started: std::time::Instant,
}
struct Queue {
    manager: Option<SftpTransferManager>,
    items: HashMap<u64, Item>,
    collisions: HashMap<u64, SftpCollision>,
    cleanup: Option<String>,
    closed: bool,
}
pub(super) struct Uploads {
    queue: Mutex<Queue>,
    sources: Arc<local_upload::Sources>,
    state: Arc<Mutex<State>>,
    failure: FirstFailure,
}
impl Uploads {
    pub(super) fn new(
        files: Files,
        sources: Arc<local_upload::Sources>,
        state: Arc<Mutex<State>>,
        cancel: CancellationToken,
    ) -> Self {
        let failure = FirstFailure::default();
        let endpoint = FilesEndpoint::new(files, cancel, failure.clone(), FilesChannel::Transfer);
        Self::from_endpoint(endpoint, sources, state, failure)
    }
    pub(super) fn from_endpoint(
        endpoint: impl festerm_ssh::RemoteTransferEndpoint,
        sources: Arc<local_upload::Sources>,
        state: Arc<Mutex<State>>,
        failure: FirstFailure,
    ) -> Self {
        let manager = SftpTransferManager::from_remote_endpoint_with_upload_sources(
            endpoint,
            sources.clone(),
        );
        Self {
            queue: Mutex::new(Queue {
                manager: Some(manager),
                items: HashMap::new(),
                collisions: HashMap::new(),
                cleanup: None,
                closed: false,
            }),
            sources,
            state,
            failure,
        }
    }
    pub(super) fn enqueue(&self, id: u64, relative: &str, directory: &str) -> Result<(), String> {
        let source = self
            .sources
            .admit(relative)
            .map_err(|_| "The selected local file changed or is unavailable")?;
        let mut queue = self.queue.lock().unwrap();
        let result = (|| {
            let manager = queue.manager.as_ref().ok_or("Connection closed")?;
            let request =
                SftpTransferRequest::new(SftpPath::local(&source), SftpPath::remote(directory))
                    .map_err(|_| "Invalid upload destination")?;
            let admitted = manager
                .enqueue_batch(vec![request])
                .map_err(|_| "Upload queue is unavailable or full")?;
            queue.items.insert(
                id,
                Item {
                    transfer: admitted.transfer_ids[0],
                    source: Some(source.clone()),
                    started: std::time::Instant::now(),
                },
            );
            Ok(())
        })();
        if result.is_err() {
            self.sources.release(&source);
        }
        result
    }
    pub(super) fn poll(&self) {
        let mut queue = self.queue.lock().unwrap();
        let Some(manager) = queue.manager.as_mut() else {
            return;
        };
        let mut events = Vec::new();
        let mut closed = false;
        for _ in 0..128 {
            match manager.try_recv_event() {
                Ok(event) => events.push(event),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    closed = true;
                    break;
                }
            }
        }
        let snapshot = manager.snapshot();
        queue.closed |= closed;
        let awaiting = snapshot
            .items
            .iter()
            .filter(|item| matches!(item.state, SftpTransferState::AwaitingCollision(_)))
            .map(|item| item.transfer_id)
            .collect::<std::collections::HashSet<_>>();
        let mut state = self.state.lock().unwrap();
        for entry in snapshot.items {
            let Some((&id, item)) = queue
                .items
                .iter_mut()
                .find(|(_, item)| item.transfer == entry.transfer_id)
            else {
                continue;
            };
            let Some(row) = state.rows.iter_mut().find(|row| row.id == id) else {
                continue;
            };
            // Terminal events are retained history; the same ID never reuses.
            // A live snapshot published before that event cannot revive it.
            if row.state.finished() {
                continue;
            }
            row.done = row.done.max(entry.bytes_transferred);
            if entry.total_bytes.is_some() {
                row.total = entry.total_bytes;
            }
            row.rate = row.done as f64 / item.started.elapsed().as_secs_f64().max(0.001);
            row.error = None;
            row.state = match entry.state {
                SftpTransferState::Queued | SftpTransferState::Planning => {
                    if row.state == RowState::Active {
                        RowState::Active
                    } else {
                        RowState::Queued
                    }
                }
                SftpTransferState::Running => RowState::Active,
                SftpTransferState::AwaitingCollision(_) => RowState::Awaiting,
                SftpTransferState::Completed => RowState::Done,
                SftpTransferState::Skipped => RowState::Skipped,
                SftpTransferState::Cancelled => RowState::Cancelled,
                SftpTransferState::Failed { reason }
                | SftpTransferState::PublicationUncertain { reason } => {
                    row.error = Some(reason);
                    RowState::Failed
                }
            };
            if row.state.finished() {
                if let Some(source) = item.source.take() {
                    self.sources.release(&source);
                }
            }
        }
        // Snapshots are published outside active copy. Keep known Active/bytes
        // over a stale Queued snapshot and apply events in wire-worker order;
        // a current Awaiting snapshot must not be undone by older progress.
        // Terminal outcomes stay in bounded mobile history until cleared.
        for event in events {
            use festerm_ssh::SftpTransferEvent as E;
            let (transfer, status, done, total, error) = match event {
                E::Collision(collision) => {
                    if let Some((&id, _)) = queue
                        .items
                        .iter()
                        .find(|(_, item)| item.transfer == collision.transfer_id)
                    {
                        if let Some(row) = state.rows.iter_mut().find(|row| row.id == id) {
                            row.state = RowState::Awaiting;
                        }
                        queue.collisions.insert(id, collision);
                    }
                    continue;
                }
                E::ItemProgress {
                    transfer_id,
                    bytes_transferred,
                    total_bytes,
                    ..
                } => {
                    if let Some((&id, item)) = queue
                        .items
                        .iter()
                        .find(|(_, item)| item.transfer == transfer_id)
                    {
                        if let Some(row) = state.rows.iter_mut().find(|row| row.id == id) {
                            if !awaiting.contains(&transfer_id) && !row.state.finished() {
                                row.state = RowState::Active;
                                row.conflict = None;
                            }
                            row.done = row.done.max(bytes_transferred);
                            if total_bytes.is_some() {
                                row.total = total_bytes;
                            }
                            row.rate =
                                row.done as f64 / item.started.elapsed().as_secs_f64().max(0.001);
                        }
                    }
                    continue;
                }
                E::ItemStarted {
                    transfer_id,
                    total_bytes,
                    ..
                } => {
                    if let Some((&id, item)) = queue
                        .items
                        .iter_mut()
                        .find(|(_, item)| item.transfer == transfer_id)
                    {
                        item.started = std::time::Instant::now();
                        if let Some(row) = state.rows.iter_mut().find(|row| row.id == id) {
                            if !awaiting.contains(&transfer_id) && !row.state.finished() {
                                row.state = RowState::Active;
                                row.conflict = None;
                            }
                            if total_bytes.is_some() {
                                row.total = total_bytes;
                            }
                        }
                    }
                    continue;
                }
                E::ItemCompleted {
                    transfer_id,
                    bytes_transferred,
                    total_bytes,
                    ..
                } => (
                    transfer_id,
                    RowState::Done,
                    Some(bytes_transferred),
                    total_bytes,
                    None,
                ),
                E::ItemSkipped { transfer_id, .. } => {
                    (transfer_id, RowState::Skipped, None, None, None)
                }
                E::ItemCancelled {
                    transfer_id,
                    bytes_transferred,
                    total_bytes,
                    ..
                } => (
                    transfer_id,
                    RowState::Cancelled,
                    Some(bytes_transferred),
                    total_bytes,
                    None,
                ),
                E::ItemFailed {
                    transfer_id,
                    reason,
                    ..
                }
                | E::ItemPublicationUncertain {
                    transfer_id,
                    reason,
                    ..
                } => (transfer_id, RowState::Failed, None, None, Some(reason)),
                E::CleanupIncomplete { .. } => {
                    queue.cleanup = Some("Couldn’t confirm upload cleanup. Inspect the destination before starting new work.".into());
                    continue;
                }
                _ => continue,
            };
            let Some((&id, item)) = queue
                .items
                .iter_mut()
                .find(|(_, item)| item.transfer == transfer)
            else {
                continue;
            };
            if let Some(row) = state.rows.iter_mut().find(|row| row.id == id) {
                row.state = status;
                if let Some(done) = done {
                    row.done = done;
                }
                if total.is_some() {
                    row.total = total;
                }
                row.error = error;
                row.conflict = None;
            }
            if let Some(source) = item.source.take() {
                self.sources.release(&source);
            }
            queue.collisions.remove(&id);
        }
        for (id, collision) in &queue.collisions {
            if let Some(row) = state
                .rows
                .iter_mut()
                .find(|row| row.id == *id && row.state == RowState::Awaiting)
            {
                fn facts(meta: &festerm_ssh::SftpPathMetadata) -> super::super::files::FileFacts {
                    super::super::files::FileFacts {
                        size: meta.size,
                        modified: meta
                            .modified_at
                            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                            .and_then(|v| i64::try_from(v.as_secs()).ok()),
                    }
                }
                row.conflict = Some(super::super::files::ConflictView {
                    direction: Direction::Upload,
                    allowed: collision
                        .allowed_decisions
                        .iter()
                        .filter_map(|decision| match decision {
                            SftpCollisionDecision::Skip => Some(ConflictPolicy::Skip),
                            SftpCollisionDecision::KeepBoth => Some(ConflictPolicy::KeepBoth),
                            SftpCollisionDecision::Replace => Some(ConflictPolicy::Replace),
                            SftpCollisionDecision::MergeFolders => None,
                        })
                        .collect(),
                    // Every mobile enqueue is its own batch; no cross-batch promise.
                    can_remember: false,
                    iphone: facts(&collision.source),
                    remote: facts(&collision.destination),
                    suggested_name: collision
                        .proposed_keep_both_destination
                        .file_name()
                        .unwrap_or_default()
                        .to_owned(),
                    existing_is_directory: collision.destination.file_type
                        == festerm_ssh::SftpEntryType::Directory,
                    likely_identical: false,
                });
            }
        }
    }
    pub(super) fn cancel(&self, id: u64) -> bool {
        let queue = self.queue.lock().unwrap();
        let Some(item) = queue.items.get(&id) else {
            return false;
        };
        if let Some(manager) = &queue.manager {
            let _ = manager.cancel_transfer(item.transfer);
        }
        true
    }
    pub(super) fn resolve(&self, id: u64, policy: ConflictPolicy, _remember: bool) {
        let mut queue = self.queue.lock().unwrap();
        let Some(collision) = queue.collisions.get(&id) else {
            return;
        };
        let decision = match policy {
            ConflictPolicy::Skip => SftpCollisionDecision::Skip,
            ConflictPolicy::KeepBoth => SftpCollisionDecision::KeepBoth,
            ConflictPolicy::Replace => SftpCollisionDecision::Replace,
        };
        if !collision.allowed_decisions.contains(&decision) {
            return;
        }
        if let Some(manager) = &queue.manager {
            if manager
                .resolve_collision(SftpCollisionResolution {
                    collision_id: collision.id,
                    decision,
                    scope: SftpCollisionScope::ThisItem,
                })
                .is_ok()
            {
                queue.collisions.remove(&id);
            }
        }
    }
    pub(super) fn clear_finished(&self) {
        let ids = self
            .state
            .lock()
            .unwrap()
            .rows
            .iter()
            .filter(|row| row.state.finished())
            .map(|row| row.id)
            .collect::<Vec<_>>();
        let mut queue = self.queue.lock().unwrap();
        for id in ids {
            if let Some(item) = queue.items.remove(&id) {
                if let Some(source) = item.source {
                    self.sources.release(&source);
                }
            }
            queue.collisions.remove(&id);
        }
    }
    pub(super) fn failure(&self) -> Option<String> {
        let message = self.failure.details_or(String::new());
        if !message.is_empty() {
            Some(message)
        } else {
            let queue = self.queue.lock().unwrap();
            queue.cleanup.clone().or_else(|| {
                queue
                    .closed
                    .then(|| "Connection closed. Reconnect to browse files.".into())
            })
        }
    }
    pub(super) async fn shutdown(&self) {
        let manager = self.queue.lock().unwrap().manager.take();
        if let Some(manager) = manager {
            if manager.shutdown().await.is_err() {
                self.queue.lock().unwrap().cleanup = Some("Couldn’t confirm upload cleanup. Inspect the destination before starting new work.".into());
            }
        }
    }
}
