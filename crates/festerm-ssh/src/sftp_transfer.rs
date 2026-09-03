use std::{
    collections::{HashMap, VecDeque},
    fmt,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::SystemTime,
};

use tokio::{
    fs,
    sync::mpsc::{error::TryRecvError, unbounded_channel, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
};

use crate::sftp::{
    display_path, join_path_segment, join_remote_path, local_error, read_local_directory_snapshot,
    read_local_path_metadata, remote_file_name, SftpEntryType, SftpSession, SftpSessionError,
};

const TEMP_SUFFIX: &str = ".festerm-part";

/// Which filesystem a GUI SFTP path or snapshot refers to.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SftpLocation {
    Local,
    Remote,
}

/// A fully-qualified local or remote GUI SFTP path.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum SftpPath {
    Local(PathBuf),
    Remote(String),
}

impl SftpPath {
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self::Local(path.into())
    }

    pub fn remote(path: impl Into<String>) -> Self {
        Self::Remote(path.into())
    }

    pub const fn location(&self) -> SftpLocation {
        match self {
            Self::Local(_) => SftpLocation::Local,
            Self::Remote(_) => SftpLocation::Remote,
        }
    }

    pub fn display(&self) -> String {
        match self {
            Self::Local(path) => display_path(path),
            Self::Remote(path) => path.clone(),
        }
    }

    pub fn file_name(&self) -> Result<String, SftpSessionError> {
        match self {
            Self::Local(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .ok_or_else(|| SftpSessionError::MissingFileName {
                    path: display_path(path),
                }),
            Self::Remote(path) => Ok(remote_file_name(path)?.to_owned()),
        }
    }

    pub fn parent_directory(&self) -> Self {
        match self {
            Self::Local(path) => Self::Local(
                path.parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| path.clone()),
            ),
            Self::Remote(path) => {
                let trimmed = path.trim_end_matches('/');
                if trimmed.is_empty() || trimmed == "/" {
                    Self::Remote("/".to_owned())
                } else if let Some((parent, _)) = trimmed.rsplit_once('/') {
                    if parent.is_empty() {
                        Self::Remote("/".to_owned())
                    } else {
                        Self::Remote(parent.to_owned())
                    }
                } else {
                    Self::Remote("/".to_owned())
                }
            }
        }
    }

    pub fn join_child(&self, child: &str) -> Self {
        match self {
            Self::Local(path) => Self::Local(join_path_segment(path, child)),
            Self::Remote(path) => Self::Remote(join_remote_path(path, child)),
        }
    }
}

/// Sortable metadata for one local or remote filesystem path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SftpPathMetadata {
    pub path: SftpPath,
    pub file_type: SftpEntryType,
    pub size: Option<u64>,
    pub modified_at: Option<SystemTime>,
    pub permissions: Option<u32>,
}

/// One directory-table row for the GUI SFTP surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SftpDirectoryItem {
    pub name: String,
    pub path: SftpPath,
    pub file_type: SftpEntryType,
    pub size: Option<u64>,
    pub modified_at: Option<SystemTime>,
    pub permissions: Option<u32>,
}

impl SftpDirectoryItem {
    pub fn metadata(&self) -> SftpPathMetadata {
        SftpPathMetadata {
            path: self.path.clone(),
            file_type: self.file_type,
            size: self.size,
            modified_at: self.modified_at,
            permissions: self.permissions,
        }
    }
}

/// A loaded local or remote directory snapshot.
///
/// One unified type keeps later UI sorting, filtering, stale rendering, and
/// pane-order swaps independent from which side is local or remote.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SftpDirectorySnapshot {
    pub location: SftpLocation,
    pub path: SftpPath,
    pub loaded_at: SystemTime,
    pub entries: Vec<SftpDirectoryItem>,
}

impl SftpDirectorySnapshot {
    pub async fn read_local(path: impl AsRef<Path>) -> Result<Self, SftpSessionError> {
        read_local_directory_snapshot(path.as_ref()).await
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SftpTransferBatchId(u64);

impl SftpTransferBatchId {
    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SftpTransferId(u64);

impl SftpTransferId {
    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SftpCollisionId(u64);

impl SftpCollisionId {
    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SftpTransferDirection {
    Upload,
    Download,
}

/// One queued GUI copy request. `destination` may be an existing directory
/// (copy into it using the source basename) or an exact target path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SftpTransferRequest {
    pub source: SftpPath,
    pub destination: SftpPath,
}

impl SftpTransferRequest {
    pub fn new(source: SftpPath, destination: SftpPath) -> Result<Self, SftpTransferManagerError> {
        match (source.location(), destination.location()) {
            (SftpLocation::Local, SftpLocation::Remote)
            | (SftpLocation::Remote, SftpLocation::Local) => Ok(Self {
                source,
                destination,
            }),
            _ => Err(SftpTransferManagerError::UnsupportedPathPair {
                source: source.location(),
                destination: destination.location(),
            }),
        }
    }

    pub fn direction(&self) -> SftpTransferDirection {
        match (self.source.location(), self.destination.location()) {
            (SftpLocation::Local, SftpLocation::Remote) => SftpTransferDirection::Upload,
            (SftpLocation::Remote, SftpLocation::Local) => SftpTransferDirection::Download,
            _ => unreachable!("validated in SftpTransferRequest::new"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SftpQueuedTransferBatch {
    pub batch_id: SftpTransferBatchId,
    pub transfer_ids: Vec<SftpTransferId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SftpCollisionDecision {
    Replace,
    Skip,
    KeepBoth,
    MergeFolders,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SftpCollisionScope {
    ThisItem,
    RemainingConflictsInBatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SftpCollision {
    pub id: SftpCollisionId,
    pub batch_id: SftpTransferBatchId,
    pub transfer_id: SftpTransferId,
    pub source: SftpPathMetadata,
    pub destination: SftpPathMetadata,
    pub proposed_keep_both_destination: SftpPath,
    pub allowed_decisions: Vec<SftpCollisionDecision>,
    pub can_apply_to_all: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SftpCollisionResolution {
    pub collision_id: SftpCollisionId,
    pub decision: SftpCollisionDecision,
    pub scope: SftpCollisionScope,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SftpTransferState {
    Queued,
    Planning,
    AwaitingCollision(SftpCollisionId),
    Running,
    Completed,
    Failed { reason: String },
    Cancelled,
    Skipped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SftpTransferItemSnapshot {
    pub batch_id: SftpTransferBatchId,
    pub transfer_id: SftpTransferId,
    pub request: SftpTransferRequest,
    pub direction: SftpTransferDirection,
    pub state: SftpTransferState,
    pub bytes_transferred: u64,
    pub total_bytes: Option<u64>,
    pub destination: Option<SftpPath>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SftpTransferQueueSnapshot {
    pub items: Vec<SftpTransferItemSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SftpTransferEvent {
    BatchQueued {
        batch_id: SftpTransferBatchId,
        transfer_ids: Vec<SftpTransferId>,
    },
    ItemStarted {
        batch_id: SftpTransferBatchId,
        transfer_id: SftpTransferId,
        source: SftpPath,
        destination: SftpPath,
        direction: SftpTransferDirection,
        total_bytes: Option<u64>,
    },
    ItemProgress {
        batch_id: SftpTransferBatchId,
        transfer_id: SftpTransferId,
        current_path: SftpPath,
        bytes_transferred: u64,
        total_bytes: Option<u64>,
    },
    Collision(SftpCollision),
    DestinationDirectoryRefreshRequested {
        batch_id: SftpTransferBatchId,
        transfer_id: SftpTransferId,
        directory: SftpPath,
    },
    ItemCompleted {
        batch_id: SftpTransferBatchId,
        transfer_id: SftpTransferId,
        destination: SftpPath,
        bytes_transferred: u64,
        total_bytes: Option<u64>,
        skipped_conflicts: usize,
    },
    ItemFailed {
        batch_id: SftpTransferBatchId,
        transfer_id: SftpTransferId,
        destination: Option<SftpPath>,
        reason: String,
    },
    ItemCancelled {
        batch_id: SftpTransferBatchId,
        transfer_id: SftpTransferId,
        destination: Option<SftpPath>,
        bytes_transferred: u64,
        total_bytes: Option<u64>,
    },
    ItemSkipped {
        batch_id: SftpTransferBatchId,
        transfer_id: SftpTransferId,
        destination: Option<SftpPath>,
    },
    BatchFinished {
        batch_id: SftpTransferBatchId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SftpTransferManagerError {
    EmptyBatch,
    ManagerClosed,
    UnsupportedPathPair {
        source: SftpLocation,
        destination: SftpLocation,
    },
    UnknownCollision(SftpCollisionId),
    InvalidCollisionDecision {
        collision_id: SftpCollisionId,
        decision: SftpCollisionDecision,
    },
}

impl fmt::Display for SftpTransferManagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyBatch => {
                formatter.write_str("transfer batch must contain at least one item")
            }
            Self::ManagerClosed => formatter.write_str("SFTP transfer manager is closed"),
            Self::UnsupportedPathPair {
                source,
                destination,
            } => write!(
                formatter,
                "unsupported SFTP transfer path pair: {source:?} -> {destination:?}"
            ),
            Self::UnknownCollision(collision_id) => {
                write!(
                    formatter,
                    "unknown transfer collision {}",
                    collision_id.raw()
                )
            }
            Self::InvalidCollisionDecision {
                collision_id,
                decision,
            } => write!(
                formatter,
                "collision {} does not allow decision {decision:?}",
                collision_id.raw()
            ),
        }
    }
}

impl std::error::Error for SftpTransferManagerError {}

/// Queued, cancellable GUI SFTP copy engine.
///
/// The worker runs on the caller's Tokio runtime, processes one item at a
/// time for deterministic progress ordering, and emits typed events through a
/// receiver the UI can poll or await.
pub struct SftpTransferManager {
    command_sender: UnboundedSender<WorkerCommand>,
    event_receiver: UnboundedReceiver<SftpTransferEvent>,
    snapshot: Arc<Mutex<SftpTransferQueueSnapshot>>,
    next_batch_id: AtomicU64,
    next_transfer_id: AtomicU64,
    worker: JoinHandle<()>,
}

impl SftpTransferManager {
    pub fn new(session: SftpSession) -> Self {
        let (command_sender, command_receiver) = unbounded_channel();
        let (event_sender, event_receiver) = unbounded_channel();
        let snapshot = Arc::new(Mutex::new(SftpTransferQueueSnapshot::default()));
        let worker_snapshot = Arc::clone(&snapshot);
        let worker = tokio::spawn(async move {
            run_transfer_worker(
                LiveTransferBackend { session },
                worker_snapshot,
                command_receiver,
                event_sender,
            )
            .await;
        });
        Self {
            command_sender,
            event_receiver,
            snapshot,
            next_batch_id: AtomicU64::new(1),
            next_transfer_id: AtomicU64::new(1),
            worker,
        }
    }

    pub fn enqueue_batch(
        &self,
        requests: Vec<SftpTransferRequest>,
    ) -> Result<SftpQueuedTransferBatch, SftpTransferManagerError> {
        if requests.is_empty() {
            return Err(SftpTransferManagerError::EmptyBatch);
        }
        let batch_id = SftpTransferBatchId(self.next_batch_id.fetch_add(1, Ordering::Relaxed));
        let items = requests
            .into_iter()
            .map(|request| QueuedTransferInput {
                id: SftpTransferId(self.next_transfer_id.fetch_add(1, Ordering::Relaxed)),
                request,
            })
            .collect::<Vec<_>>();
        let transfer_ids = items.iter().map(|item| item.id).collect::<Vec<_>>();
        self.command_sender
            .send(WorkerCommand::EnqueueBatch { batch_id, items })
            .map_err(|_| SftpTransferManagerError::ManagerClosed)?;
        Ok(SftpQueuedTransferBatch {
            batch_id,
            transfer_ids,
        })
    }

    pub fn cancel_transfer(
        &self,
        transfer_id: SftpTransferId,
    ) -> Result<(), SftpTransferManagerError> {
        self.command_sender
            .send(WorkerCommand::CancelTransfer(transfer_id))
            .map_err(|_| SftpTransferManagerError::ManagerClosed)
    }

    pub fn cancel_batch(
        &self,
        batch_id: SftpTransferBatchId,
    ) -> Result<(), SftpTransferManagerError> {
        self.command_sender
            .send(WorkerCommand::CancelBatch(batch_id))
            .map_err(|_| SftpTransferManagerError::ManagerClosed)
    }

    pub fn resolve_collision(
        &self,
        resolution: SftpCollisionResolution,
    ) -> Result<(), SftpTransferManagerError> {
        self.command_sender
            .send(WorkerCommand::ResolveCollision(resolution))
            .map_err(|_| SftpTransferManagerError::ManagerClosed)
    }

    pub async fn recv_event(&mut self) -> Option<SftpTransferEvent> {
        self.event_receiver.recv().await
    }

    pub fn try_recv_event(&mut self) -> Result<SftpTransferEvent, TryRecvError> {
        self.event_receiver.try_recv()
    }

    pub fn snapshot(&self) -> SftpTransferQueueSnapshot {
        self.snapshot
            .lock()
            .expect("SFTP transfer snapshot lock is not poisoned")
            .clone()
    }
}

impl Drop for SftpTransferManager {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

enum WorkerCommand {
    EnqueueBatch {
        batch_id: SftpTransferBatchId,
        items: Vec<QueuedTransferInput>,
    },
    CancelTransfer(SftpTransferId),
    CancelBatch(SftpTransferBatchId),
    ResolveCollision(SftpCollisionResolution),
}

#[derive(Clone, Debug)]
struct QueuedTransferInput {
    id: SftpTransferId,
    request: SftpTransferRequest,
}

#[derive(Default)]
struct WorkerState {
    ready: VecDeque<SftpTransferId>,
    items: HashMap<SftpTransferId, TransferItem>,
    batches: HashMap<SftpTransferBatchId, BatchState>,
    collisions: HashMap<SftpCollisionId, SftpTransferId>,
    next_collision_id: u64,
}

#[derive(Default)]
struct BatchState {
    default_decision: Option<SftpCollisionDecision>,
    active_items: usize,
}

struct TransferItem {
    batch_id: SftpTransferBatchId,
    id: SftpTransferId,
    request: SftpTransferRequest,
    direction: SftpTransferDirection,
    state: SftpTransferState,
    bytes_transferred: u64,
    total_bytes: Option<u64>,
    destination: Option<SftpPath>,
    started: bool,
    cancel_requested: bool,
    skipped_conflicts: usize,
    root_state: TransferRootState,
    pending_resolution: Option<SftpCollisionResolution>,
    active_collision: Option<SftpCollisionId>,
}

enum TransferRootState {
    Pending,
    Ready(TransferPlan),
    WaitingCollision(Box<PendingCollision>),
}

struct TransferPlan {
    units: VecDeque<TransferUnit>,
    total_bytes: Option<u64>,
}

enum TransferUnit {
    EnsureDirectory {
        destination: SftpPath,
        replace_existing_non_directory: bool,
    },
    CopyFile {
        source: SftpPathMetadata,
        destination: SftpPath,
        replace_existing_at_commit: bool,
        whole_item: bool,
    },
}

enum PendingCollision {
    RootDirectory {
        collision: SftpCollision,
        source: SftpPathMetadata,
        destination: SftpPath,
    },
    File {
        collision: SftpCollision,
        source: SftpPathMetadata,
        destination: SftpPath,
        remaining_units: VecDeque<TransferUnit>,
        whole_item: bool,
    },
}

impl PendingCollision {
    fn allowed_decisions(&self) -> &[SftpCollisionDecision] {
        match self {
            Self::RootDirectory { collision, .. } | Self::File { collision, .. } => {
                &collision.allowed_decisions
            }
        }
    }

    fn id(&self) -> SftpCollisionId {
        match self {
            Self::RootDirectory { collision, .. } | Self::File { collision, .. } => collision.id,
        }
    }
}

struct FileDecisionContext {
    source: SftpPathMetadata,
    destination: SftpPath,
    remaining_units: VecDeque<TransferUnit>,
    whole_item: bool,
}

type BackendFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SftpSessionError>> + Send + 'a>>;
type CopyFuture<'a> = Pin<Box<dyn Future<Output = Result<u64, CopyFileError>> + Send + 'a>>;

trait TransferBackend {
    fn metadata<'a>(
        &'a mut self,
        path: &'a SftpPath,
    ) -> BackendFuture<'a, Option<SftpPathMetadata>>;
    fn read_directory<'a>(
        &'a mut self,
        path: &'a SftpPath,
    ) -> BackendFuture<'a, SftpDirectorySnapshot>;
    fn create_directory<'a>(&'a mut self, path: &'a SftpPath) -> BackendFuture<'a, ()>;
    fn remove_file<'a>(&'a mut self, path: &'a SftpPath) -> BackendFuture<'a, ()>;
    fn rename<'a>(
        &'a mut self,
        source: &'a SftpPath,
        destination: &'a SftpPath,
    ) -> BackendFuture<'a, ()>;
    fn copy_file<'a>(
        &'a mut self,
        source: &'a SftpPath,
        destination: &'a SftpPath,
        on_progress: &'a mut (dyn FnMut(u64) -> Result<(), CopyInterrupted> + Send),
    ) -> CopyFuture<'a>;
}

enum CopyFileError {
    Operation(SftpSessionError),
    Cancelled,
}

struct CopyInterrupted;

struct LiveTransferBackend {
    session: SftpSession,
}

impl TransferBackend for LiveTransferBackend {
    fn metadata<'a>(
        &'a mut self,
        path: &'a SftpPath,
    ) -> BackendFuture<'a, Option<SftpPathMetadata>> {
        Box::pin(async move {
            match path {
                SftpPath::Local(path) => read_local_path_metadata(path).await,
                SftpPath::Remote(path) => self.session.remote_path_metadata_exact(path).await,
            }
        })
    }

    fn read_directory<'a>(
        &'a mut self,
        path: &'a SftpPath,
    ) -> BackendFuture<'a, SftpDirectorySnapshot> {
        Box::pin(async move {
            match path {
                SftpPath::Local(path) => read_local_directory_snapshot(path).await,
                SftpPath::Remote(path) => self.session.remote_directory_snapshot_exact(path).await,
            }
        })
    }

    fn create_directory<'a>(&'a mut self, path: &'a SftpPath) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            match path {
                SftpPath::Local(path) => fs::create_dir(path)
                    .await
                    .map_err(|error| local_error("create directory", path, error)),
                SftpPath::Remote(path) => self.session.create_remote_directory_exact(path).await,
            }
        })
    }

    fn remove_file<'a>(&'a mut self, path: &'a SftpPath) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            match path {
                SftpPath::Local(path) => fs::remove_file(path)
                    .await
                    .map_err(|error| local_error("remove file", path, error)),
                SftpPath::Remote(path) => self.session.remove_remote_file_exact(path).await,
            }
        })
    }

    fn rename<'a>(
        &'a mut self,
        source: &'a SftpPath,
        destination: &'a SftpPath,
    ) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            match (source, destination) {
                (SftpPath::Local(source), SftpPath::Local(destination)) => {
                    fs::rename(source, destination)
                        .await
                        .map_err(|error| local_error("rename file", source, error))
                }
                (SftpPath::Remote(source), SftpPath::Remote(destination)) => {
                    self.session
                        .rename_remote_path_exact(source, destination)
                        .await
                }
                _ => Err(SftpSessionError::LocalOperationFailed {
                    operation: "rename file",
                    path: format!("{} -> {}", source.display(), destination.display()),
                    reason: "rename requires matching filesystem sides".to_owned(),
                }),
            }
        })
    }

    fn copy_file<'a>(
        &'a mut self,
        source: &'a SftpPath,
        destination: &'a SftpPath,
        on_progress: &'a mut (dyn FnMut(u64) -> Result<(), CopyInterrupted> + Send),
    ) -> CopyFuture<'a> {
        Box::pin(async move {
            match (source, destination) {
                (SftpPath::Local(source), SftpPath::Remote(destination)) => {
                    let mut callback = |bytes| {
                        on_progress(bytes).map_err(|_| SftpSessionError::LocalOperationFailed {
                            operation: "cancel transfer",
                            path: display_path(source),
                            reason: "transfer cancelled".to_owned(),
                        })
                    };
                    self.session
                        .upload_local_file_exact(source, destination, &mut callback)
                        .await
                        .map_err(|error| match error {
                            SftpSessionError::LocalOperationFailed {
                                operation: "cancel transfer",
                                ..
                            } => CopyFileError::Cancelled,
                            other => CopyFileError::Operation(other),
                        })
                }
                (SftpPath::Remote(source), SftpPath::Local(destination)) => {
                    let remote_path = source.clone();
                    let mut callback = |bytes| {
                        on_progress(bytes).map_err(|_| SftpSessionError::RemoteOperationFailed {
                            operation: "cancel transfer",
                            path: remote_path.clone(),
                            reason: "transfer cancelled".to_owned(),
                        })
                    };
                    self.session
                        .download_remote_file_exact(source, destination, &mut callback)
                        .await
                        .map_err(|error| match error {
                            SftpSessionError::RemoteOperationFailed {
                                operation: "cancel transfer",
                                ..
                            } => CopyFileError::Cancelled,
                            other => CopyFileError::Operation(other),
                        })
                }
                _ => Err(CopyFileError::Operation(
                    SftpSessionError::LocalOperationFailed {
                        operation: "copy file",
                        path: format!("{} -> {}", source.display(), destination.display()),
                        reason: "copy requires local/remote or remote/local paths".to_owned(),
                    },
                )),
            }
        })
    }
}

async fn run_transfer_worker<B: TransferBackend>(
    mut backend: B,
    snapshot: Arc<Mutex<SftpTransferQueueSnapshot>>,
    mut command_receiver: UnboundedReceiver<WorkerCommand>,
    event_sender: UnboundedSender<SftpTransferEvent>,
) {
    let mut state = WorkerState::default();
    loop {
        state.publish_snapshot(&snapshot);
        state.drain_commands(&mut command_receiver, &event_sender);

        if let Some(transfer_id) = state.ready.pop_front() {
            state
                .process_one(
                    transfer_id,
                    &mut backend,
                    &mut command_receiver,
                    &event_sender,
                )
                .await;
            continue;
        }

        if command_receiver.is_closed() && state.items.is_empty() {
            break;
        }

        match command_receiver.recv().await {
            Some(command) => state.handle_command(command, &event_sender),
            None if state.items.is_empty() => break,
            None => break,
        }
    }
}

impl WorkerState {
    fn handle_command(
        &mut self,
        command: WorkerCommand,
        event_sender: &UnboundedSender<SftpTransferEvent>,
    ) {
        match command {
            WorkerCommand::EnqueueBatch { batch_id, items } => {
                let transfer_ids = items.iter().map(|item| item.id).collect::<Vec<_>>();
                self.batches.insert(
                    batch_id,
                    BatchState {
                        default_decision: None,
                        active_items: items.len(),
                    },
                );
                for item in items {
                    let direction = item.request.direction();
                    self.ready.push_back(item.id);
                    self.items.insert(
                        item.id,
                        TransferItem {
                            batch_id,
                            id: item.id,
                            request: item.request,
                            direction,
                            state: SftpTransferState::Queued,
                            bytes_transferred: 0,
                            total_bytes: None,
                            destination: None,
                            started: false,
                            cancel_requested: false,
                            skipped_conflicts: 0,
                            root_state: TransferRootState::Pending,
                            pending_resolution: None,
                            active_collision: None,
                        },
                    );
                }
                let _ = event_sender.send(SftpTransferEvent::BatchQueued {
                    batch_id,
                    transfer_ids,
                });
            }
            WorkerCommand::CancelTransfer(transfer_id) => {
                if let Some(item) = self.items.get_mut(&transfer_id) {
                    item.cancel_requested = true;
                    if matches!(
                        item.state,
                        SftpTransferState::Queued
                            | SftpTransferState::Planning
                            | SftpTransferState::AwaitingCollision(_)
                    ) {
                        self.finish_cancelled(transfer_id, event_sender);
                    }
                }
            }
            WorkerCommand::CancelBatch(batch_id) => {
                let ids = self
                    .items
                    .values()
                    .filter(|item| item.batch_id == batch_id)
                    .map(|item| item.id)
                    .collect::<Vec<_>>();
                for transfer_id in ids {
                    self.handle_command(WorkerCommand::CancelTransfer(transfer_id), event_sender);
                }
            }
            WorkerCommand::ResolveCollision(resolution) => {
                if let Some(transfer_id) = self.collisions.remove(&resolution.collision_id) {
                    let batch_id = if let Some(item) = self.items.get_mut(&transfer_id) {
                        item.active_collision = None;
                        item.pending_resolution = Some(resolution.clone());
                        item.state = SftpTransferState::Queued;
                        let batch_id = item.batch_id;
                        if matches!(
                            resolution.scope,
                            SftpCollisionScope::RemainingConflictsInBatch
                        ) {
                            if let Some(batch) = self.batches.get_mut(&item.batch_id) {
                                batch.default_decision = Some(resolution.decision);
                            }
                        }
                        self.ready.push_back(transfer_id);
                        batch_id
                    } else {
                        return;
                    };
                    if matches!(
                        resolution.scope,
                        SftpCollisionScope::RemainingConflictsInBatch
                    ) {
                        let paused = self
                            .items
                            .values()
                            .filter(|item| {
                                item.batch_id == batch_id && item.pending_resolution.is_none()
                            })
                            .filter_map(|item| match &item.root_state {
                                TransferRootState::WaitingCollision(collision)
                                    if collision
                                        .allowed_decisions()
                                        .contains(&resolution.decision) =>
                                {
                                    Some((item.id, collision.id()))
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        for (item_id, collision_id) in paused {
                            self.collisions.remove(&collision_id);
                            if let Some(item) = self.items.get_mut(&item_id) {
                                item.active_collision = None;
                                item.pending_resolution = Some(SftpCollisionResolution {
                                    collision_id,
                                    decision: resolution.decision,
                                    scope: SftpCollisionScope::ThisItem,
                                });
                                item.state = SftpTransferState::Queued;
                                self.ready.push_back(item_id);
                            }
                        }
                    }
                }
            }
        }
    }

    fn drain_commands(
        &mut self,
        command_receiver: &mut UnboundedReceiver<WorkerCommand>,
        event_sender: &UnboundedSender<SftpTransferEvent>,
    ) {
        while let Ok(command) = command_receiver.try_recv() {
            self.handle_command(command, event_sender);
        }
    }

    fn publish_snapshot(&self, snapshot: &Arc<Mutex<SftpTransferQueueSnapshot>>) {
        let mut items = self
            .items
            .values()
            .map(|item| SftpTransferItemSnapshot {
                batch_id: item.batch_id,
                transfer_id: item.id,
                request: item.request.clone(),
                direction: item.direction,
                state: item.state.clone(),
                bytes_transferred: item.bytes_transferred,
                total_bytes: item.total_bytes,
                destination: item.destination.clone(),
            })
            .collect::<Vec<_>>();
        items.sort_by_key(|item| (item.batch_id.raw(), item.transfer_id.raw()));
        *snapshot
            .lock()
            .expect("SFTP transfer snapshot lock is not poisoned") =
            SftpTransferQueueSnapshot { items };
    }

    async fn process_one<B: TransferBackend>(
        &mut self,
        transfer_id: SftpTransferId,
        backend: &mut B,
        command_receiver: &mut UnboundedReceiver<WorkerCommand>,
        event_sender: &UnboundedSender<SftpTransferEvent>,
    ) {
        if !self.items.contains_key(&transfer_id) {
            return;
        }
        if self.items[&transfer_id].cancel_requested {
            self.finish_cancelled(transfer_id, event_sender);
            return;
        }

        if let Some(resolution) = self.items[&transfer_id].pending_resolution.clone() {
            if let Err(error) = self
                .apply_resolution(transfer_id, resolution, backend, event_sender)
                .await
            {
                self.finish_failed(transfer_id, None, error.to_string(), event_sender);
            }
            return;
        }

        let root_state = std::mem::replace(
            &mut self
                .items
                .get_mut(&transfer_id)
                .expect("item exists")
                .root_state,
            TransferRootState::Pending,
        );
        match root_state {
            TransferRootState::Pending => {
                if let Err(error) = self.prepare_item(transfer_id, backend, event_sender).await {
                    self.finish_failed(transfer_id, None, error.to_string(), event_sender);
                }
            }
            TransferRootState::Ready(plan) => {
                if let Err(error) = self
                    .execute_plan_step(transfer_id, plan, backend, command_receiver, event_sender)
                    .await
                {
                    self.finish_failed(transfer_id, None, error.to_string(), event_sender);
                }
            }
            TransferRootState::WaitingCollision(collision) => {
                self.items
                    .get_mut(&transfer_id)
                    .expect("item exists")
                    .root_state = TransferRootState::WaitingCollision(collision);
            }
        }
    }

    async fn prepare_item<B: TransferBackend>(
        &mut self,
        transfer_id: SftpTransferId,
        backend: &mut B,
        event_sender: &UnboundedSender<SftpTransferEvent>,
    ) -> Result<(), SftpSessionError> {
        let request = self.items[&transfer_id].request.clone();
        let batch_id = self.items[&transfer_id].batch_id;
        self.items.get_mut(&transfer_id).expect("item exists").state = SftpTransferState::Planning;

        let source = backend
            .metadata(&request.source)
            .await?
            .ok_or_else(|| missing_source_error(&request.source))?;
        let destination =
            normalize_requested_destination(backend, &request.source, &request.destination).await?;
        let destination_metadata = backend.metadata(&destination).await?;

        {
            let item = self.items.get_mut(&transfer_id).expect("item exists");
            item.destination = Some(destination.clone());
            if source.file_type != SftpEntryType::Directory {
                item.total_bytes = source.size;
            }
        }

        if source.file_type == SftpEntryType::Directory {
            if let Some(destination_metadata) = destination_metadata {
                let allowed =
                    allowed_decisions_for(source.file_type, destination_metadata.file_type);
                if let Some(decision) = self.batch_default_for(batch_id, &allowed) {
                    self.apply_root_directory_decision(
                        transfer_id,
                        source,
                        destination,
                        decision,
                        backend,
                        event_sender,
                    )
                    .await?;
                } else {
                    let collision = self.register_collision(
                        batch_id,
                        transfer_id,
                        source.clone(),
                        destination_metadata,
                        allowed,
                    )?;
                    let item = self.items.get_mut(&transfer_id).expect("item exists");
                    item.state = SftpTransferState::AwaitingCollision(collision.id);
                    item.active_collision = Some(collision.id);
                    item.root_state = TransferRootState::WaitingCollision(Box::new(
                        PendingCollision::RootDirectory {
                            collision: collision.clone(),
                            source,
                            destination,
                        },
                    ));
                    let _ = event_sender.send(SftpTransferEvent::Collision(collision));
                }
            } else {
                let plan = self
                    .build_directory_plan(&source, &destination, false, backend)
                    .await?;
                let item = self.items.get_mut(&transfer_id).expect("item exists");
                item.total_bytes = plan.total_bytes;
                item.root_state = TransferRootState::Ready(plan);
                item.state = SftpTransferState::Queued;
                self.ready.push_front(transfer_id);
            }
        } else if let Some(destination_metadata) = destination_metadata {
            let allowed = allowed_decisions_for(source.file_type, destination_metadata.file_type);
            if let Some(decision) = self.batch_default_for(batch_id, &allowed) {
                self.apply_file_decision(
                    transfer_id,
                    FileDecisionContext {
                        source,
                        destination,
                        remaining_units: VecDeque::new(),
                        whole_item: true,
                    },
                    decision,
                    backend,
                    event_sender,
                )
                .await?;
            } else {
                let collision = self.register_collision(
                    batch_id,
                    transfer_id,
                    source.clone(),
                    destination_metadata,
                    allowed,
                )?;
                let item = self.items.get_mut(&transfer_id).expect("item exists");
                item.state = SftpTransferState::AwaitingCollision(collision.id);
                item.active_collision = Some(collision.id);
                item.root_state =
                    TransferRootState::WaitingCollision(Box::new(PendingCollision::File {
                        collision: collision.clone(),
                        source,
                        destination,
                        remaining_units: VecDeque::new(),
                        whole_item: true,
                    }));
                let _ = event_sender.send(SftpTransferEvent::Collision(collision));
            }
        } else {
            let item = self.items.get_mut(&transfer_id).expect("item exists");
            item.root_state = TransferRootState::Ready(TransferPlan {
                units: VecDeque::from([TransferUnit::CopyFile {
                    source,
                    destination,
                    replace_existing_at_commit: false,
                    whole_item: true,
                }]),
                total_bytes: item.total_bytes,
            });
            item.state = SftpTransferState::Queued;
            self.ready.push_front(transfer_id);
        }
        Ok(())
    }

    async fn build_directory_plan<B: TransferBackend>(
        &self,
        source_root: &SftpPathMetadata,
        destination_root: &SftpPath,
        replace_existing_non_directory: bool,
        backend: &mut B,
    ) -> Result<TransferPlan, SftpSessionError> {
        let mut units = VecDeque::new();
        let mut total_bytes = 0_u64;
        let mut total_known = true;
        units.push_back(TransferUnit::EnsureDirectory {
            destination: destination_root.clone(),
            replace_existing_non_directory,
        });
        let mut stack = vec![(source_root.path.clone(), destination_root.clone())];
        while let Some((source_directory, destination_directory)) = stack.pop() {
            let snapshot = backend.read_directory(&source_directory).await?;
            let mut child_directories = Vec::new();
            for entry in snapshot.entries {
                if matches!(source_directory, SftpPath::Remote(_))
                    && matches!(destination_root, SftpPath::Local(_))
                {
                    validate_remote_directory_entry_name(&entry.name)?;
                }
                let child_destination = destination_directory.join_child(&entry.name);
                ensure_local_child_within_root(destination_root, &child_destination)?;
                if entry.file_type == SftpEntryType::Directory {
                    units.push_back(TransferUnit::EnsureDirectory {
                        destination: child_destination.clone(),
                        replace_existing_non_directory: false,
                    });
                    child_directories.push((entry.path.clone(), child_destination));
                } else {
                    if let Some(size) = entry.size {
                        total_bytes += size;
                    } else {
                        total_known = false;
                    }
                    units.push_back(TransferUnit::CopyFile {
                        source: entry.metadata(),
                        destination: child_destination,
                        replace_existing_at_commit: false,
                        whole_item: false,
                    });
                }
            }
            child_directories.reverse();
            stack.extend(child_directories);
        }
        Ok(TransferPlan {
            units,
            total_bytes: if total_known { Some(total_bytes) } else { None },
        })
    }

    async fn apply_root_directory_decision<B: TransferBackend>(
        &mut self,
        transfer_id: SftpTransferId,
        source: SftpPathMetadata,
        destination: SftpPath,
        decision: SftpCollisionDecision,
        backend: &mut B,
        event_sender: &UnboundedSender<SftpTransferEvent>,
    ) -> Result<(), SftpSessionError> {
        match decision {
            SftpCollisionDecision::Skip => {
                self.finish_skipped(transfer_id, event_sender);
            }
            SftpCollisionDecision::KeepBoth => {
                let target = self
                    .first_available_keep_both_destination(backend, &destination)
                    .await?;
                let plan = self
                    .build_directory_plan(&source, &target, false, backend)
                    .await?;
                let item = self.items.get_mut(&transfer_id).expect("item exists");
                item.destination = Some(target);
                item.total_bytes = plan.total_bytes;
                item.root_state = TransferRootState::Ready(plan);
                item.state = SftpTransferState::Queued;
                self.ready.push_front(transfer_id);
            }
            SftpCollisionDecision::MergeFolders => {
                let plan = self
                    .build_directory_plan(&source, &destination, false, backend)
                    .await?;
                let item = self.items.get_mut(&transfer_id).expect("item exists");
                item.total_bytes = plan.total_bytes;
                item.root_state = TransferRootState::Ready(plan);
                item.state = SftpTransferState::Queued;
                self.ready.push_front(transfer_id);
            }
            SftpCollisionDecision::Replace => {
                let plan = self
                    .build_directory_plan(&source, &destination, true, backend)
                    .await?;
                let item = self.items.get_mut(&transfer_id).expect("item exists");
                item.total_bytes = plan.total_bytes;
                item.root_state = TransferRootState::Ready(plan);
                item.state = SftpTransferState::Queued;
                self.ready.push_front(transfer_id);
            }
        }
        Ok(())
    }

    async fn apply_resolution<B: TransferBackend>(
        &mut self,
        transfer_id: SftpTransferId,
        resolution: SftpCollisionResolution,
        backend: &mut B,
        event_sender: &UnboundedSender<SftpTransferEvent>,
    ) -> Result<(), SftpTransferManagerError> {
        let pending = {
            let item = self.items.get_mut(&transfer_id).expect("item exists");
            item.pending_resolution = None;
            std::mem::replace(&mut item.root_state, TransferRootState::Pending)
        };
        match pending {
            TransferRootState::WaitingCollision(pending) => match *pending {
                PendingCollision::RootDirectory {
                    collision,
                    source,
                    destination,
                } => {
                    if !collision.allowed_decisions.contains(&resolution.decision) {
                        return Err(SftpTransferManagerError::InvalidCollisionDecision {
                            collision_id: collision.id,
                            decision: resolution.decision,
                        });
                    }
                    self.apply_root_directory_decision(
                        transfer_id,
                        source,
                        destination,
                        resolution.decision,
                        backend,
                        event_sender,
                    )
                    .await
                    .map_err(|_| SftpTransferManagerError::UnknownCollision(collision.id))?;
                }
                PendingCollision::File {
                    collision,
                    source,
                    destination,
                    remaining_units,
                    whole_item,
                } => {
                    if !collision.allowed_decisions.contains(&resolution.decision) {
                        return Err(SftpTransferManagerError::InvalidCollisionDecision {
                            collision_id: collision.id,
                            decision: resolution.decision,
                        });
                    }
                    self.apply_file_decision(
                        transfer_id,
                        FileDecisionContext {
                            source,
                            destination,
                            remaining_units,
                            whole_item,
                        },
                        resolution.decision,
                        backend,
                        event_sender,
                    )
                    .await
                    .map_err(|_| SftpTransferManagerError::UnknownCollision(collision.id))?;
                }
            },
            other => {
                self.items
                    .get_mut(&transfer_id)
                    .expect("item exists")
                    .root_state = other;
            }
        }
        Ok(())
    }

    async fn apply_file_decision<B: TransferBackend>(
        &mut self,
        transfer_id: SftpTransferId,
        context: FileDecisionContext,
        decision: SftpCollisionDecision,
        backend: &mut B,
        event_sender: &UnboundedSender<SftpTransferEvent>,
    ) -> Result<(), SftpSessionError> {
        let FileDecisionContext {
            source,
            destination,
            mut remaining_units,
            whole_item,
        } = context;
        match decision {
            SftpCollisionDecision::Skip => {
                if whole_item && remaining_units.is_empty() {
                    self.finish_skipped(transfer_id, event_sender);
                } else {
                    let _ = (source, destination, whole_item);
                    let item = self.items.get_mut(&transfer_id).expect("item exists");
                    item.skipped_conflicts += 1;
                    item.root_state = TransferRootState::Ready(TransferPlan {
                        units: remaining_units,
                        total_bytes: item.total_bytes,
                    });
                    item.state = SftpTransferState::Queued;
                    self.ready.push_front(transfer_id);
                }
            }
            SftpCollisionDecision::KeepBoth => {
                let target = self
                    .first_available_keep_both_destination(backend, &destination)
                    .await?;
                remaining_units.push_front(TransferUnit::CopyFile {
                    source,
                    destination: target.clone(),
                    replace_existing_at_commit: false,
                    whole_item,
                });
                let item = self.items.get_mut(&transfer_id).expect("item exists");
                if whole_item {
                    item.destination = Some(target);
                }
                item.root_state = TransferRootState::Ready(TransferPlan {
                    units: remaining_units,
                    total_bytes: item.total_bytes,
                });
                item.state = SftpTransferState::Queued;
                self.ready.push_front(transfer_id);
            }
            SftpCollisionDecision::Replace => {
                remaining_units.push_front(TransferUnit::CopyFile {
                    source,
                    destination,
                    replace_existing_at_commit: true,
                    whole_item,
                });
                let item = self.items.get_mut(&transfer_id).expect("item exists");
                item.root_state = TransferRootState::Ready(TransferPlan {
                    units: remaining_units,
                    total_bytes: item.total_bytes,
                });
                item.state = SftpTransferState::Queued;
                self.ready.push_front(transfer_id);
            }
            SftpCollisionDecision::MergeFolders => {
                return Err(SftpSessionError::LocalOperationFailed {
                    operation: "copy file",
                    path: destination.display(),
                    reason: "Merge folders is only valid for directory collisions".to_owned(),
                });
            }
        }
        Ok(())
    }

    async fn execute_plan_step<B: TransferBackend>(
        &mut self,
        transfer_id: SftpTransferId,
        mut plan: TransferPlan,
        backend: &mut B,
        command_receiver: &mut UnboundedReceiver<WorkerCommand>,
        event_sender: &UnboundedSender<SftpTransferEvent>,
    ) -> Result<(), SftpSessionError> {
        let Some(unit) = plan.units.pop_front() else {
            self.finish_completed(transfer_id, event_sender);
            return Ok(());
        };
        match unit {
            TransferUnit::EnsureDirectory {
                destination,
                replace_existing_non_directory,
            } => {
                match backend.metadata(&destination).await? {
                    Some(existing) if existing.file_type == SftpEntryType::Directory => {}
                    Some(existing) if replace_existing_non_directory => {
                        if existing.file_type == SftpEntryType::Directory {
                            unreachable!();
                        }
                        backend.remove_file(&destination).await?;
                        backend.create_directory(&destination).await?;
                    }
                    Some(_) => {
                        return Err(SftpSessionError::LocalOperationFailed {
                            operation: "create directory",
                            path: destination.display(),
                            reason: "destination contains a non-directory entry".to_owned(),
                        });
                    }
                    None => backend.create_directory(&destination).await?,
                }
                let batch_id = self.items[&transfer_id].batch_id;
                let item = self.items.get_mut(&transfer_id).expect("item exists");
                item.root_state = TransferRootState::Ready(plan);
                item.state = SftpTransferState::Queued;
                let _ =
                    event_sender.send(SftpTransferEvent::DestinationDirectoryRefreshRequested {
                        batch_id,
                        transfer_id,
                        directory: destination.parent_directory(),
                    });
                self.ready.push_front(transfer_id);
            }
            TransferUnit::CopyFile {
                source,
                destination,
                replace_existing_at_commit,
                whole_item,
            } => {
                if let Some(existing) = backend.metadata(&destination).await? {
                    if !replace_existing_at_commit {
                        let allowed = allowed_decisions_for(source.file_type, existing.file_type);
                        if let Some(decision) =
                            self.batch_default_for(self.items[&transfer_id].batch_id, &allowed)
                        {
                            self.apply_file_decision(
                                transfer_id,
                                FileDecisionContext {
                                    source,
                                    destination,
                                    remaining_units: plan.units,
                                    whole_item,
                                },
                                decision,
                                backend,
                                event_sender,
                            )
                            .await?;
                        } else {
                            let collision = self.register_collision(
                                self.items[&transfer_id].batch_id,
                                transfer_id,
                                source.clone(),
                                existing,
                                allowed,
                            )?;
                            let item = self.items.get_mut(&transfer_id).expect("item exists");
                            item.state = SftpTransferState::AwaitingCollision(collision.id);
                            item.active_collision = Some(collision.id);
                            item.root_state = TransferRootState::WaitingCollision(Box::new(
                                PendingCollision::File {
                                    collision: collision.clone(),
                                    source,
                                    destination,
                                    remaining_units: plan.units,
                                    whole_item,
                                },
                            ));
                            let _ = event_sender.send(SftpTransferEvent::Collision(collision));
                        }
                        return Ok(());
                    }
                }

                self.emit_started_if_needed(transfer_id, event_sender, &destination);
                let batch_id = self.items[&transfer_id].batch_id;
                let base_bytes = self.items[&transfer_id].bytes_transferred;
                let temp_destination = self
                    .first_available_temp_destination(backend, &destination)
                    .await?;
                let mut progress = |copied_for_file: u64| -> Result<(), CopyInterrupted> {
                    self.drain_commands(command_receiver, event_sender);
                    if self
                        .items
                        .get(&transfer_id)
                        .map(|item| item.cancel_requested)
                        .unwrap_or(false)
                    {
                        return Err(CopyInterrupted);
                    }
                    if let Some(item) = self.items.get_mut(&transfer_id) {
                        item.state = SftpTransferState::Running;
                        item.bytes_transferred = base_bytes + copied_for_file;
                        let _ = event_sender.send(SftpTransferEvent::ItemProgress {
                            batch_id,
                            transfer_id,
                            current_path: source.path.clone(),
                            bytes_transferred: item.bytes_transferred,
                            total_bytes: item.total_bytes,
                        });
                    }
                    Ok(())
                };
                match backend
                    .copy_file(&source.path, &temp_destination, &mut progress)
                    .await
                {
                    Ok(_) => {
                        if let Some(existing) = backend.metadata(&destination).await? {
                            if replace_existing_at_commit {
                                backend.remove_file(&destination).await?;
                            } else {
                                let allowed =
                                    allowed_decisions_for(source.file_type, existing.file_type);
                                let collision = self.register_collision(
                                    self.items[&transfer_id].batch_id,
                                    transfer_id,
                                    source.clone(),
                                    existing,
                                    allowed,
                                )?;
                                let _ = cleanup_temp_destination(backend, &temp_destination).await;
                                let item = self.items.get_mut(&transfer_id).expect("item exists");
                                item.state = SftpTransferState::AwaitingCollision(collision.id);
                                item.active_collision = Some(collision.id);
                                item.root_state = TransferRootState::WaitingCollision(Box::new(
                                    PendingCollision::File {
                                        collision: collision.clone(),
                                        source,
                                        destination,
                                        remaining_units: plan.units,
                                        whole_item,
                                    },
                                ));
                                let _ = event_sender.send(SftpTransferEvent::Collision(collision));
                                return Ok(());
                            }
                        }
                        backend.rename(&temp_destination, &destination).await?;
                        let item = self.items.get_mut(&transfer_id).expect("item exists");
                        item.root_state = TransferRootState::Ready(plan);
                        item.state = SftpTransferState::Queued;
                        let _ = event_sender.send(
                            SftpTransferEvent::DestinationDirectoryRefreshRequested {
                                batch_id,
                                transfer_id,
                                directory: destination.parent_directory(),
                            },
                        );
                        self.ready.push_front(transfer_id);
                    }
                    Err(CopyFileError::Cancelled) => {
                        let _ = cleanup_temp_destination(backend, &temp_destination).await;
                        self.finish_cancelled(transfer_id, event_sender);
                    }
                    Err(CopyFileError::Operation(error)) => {
                        let _ = cleanup_temp_destination(backend, &temp_destination).await;
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }

    fn emit_started_if_needed(
        &mut self,
        transfer_id: SftpTransferId,
        event_sender: &UnboundedSender<SftpTransferEvent>,
        destination: &SftpPath,
    ) {
        let item = self.items.get_mut(&transfer_id).expect("item exists");
        if item.started {
            return;
        }
        item.started = true;
        item.state = SftpTransferState::Running;
        let _ = event_sender.send(SftpTransferEvent::ItemStarted {
            batch_id: item.batch_id,
            transfer_id,
            source: item.request.source.clone(),
            destination: item
                .destination
                .clone()
                .unwrap_or_else(|| destination.clone()),
            direction: item.direction,
            total_bytes: item.total_bytes,
        });
    }

    async fn first_available_keep_both_destination<B: TransferBackend>(
        &mut self,
        backend: &mut B,
        destination: &SftpPath,
    ) -> Result<SftpPath, SftpSessionError> {
        let (stem, extension) = split_name_for_copy(destination.file_name()?);
        let mut counter = 1_u32;
        loop {
            let name = if counter == 1 {
                keep_both_name(&stem, extension.as_deref(), None)
            } else {
                keep_both_name(&stem, extension.as_deref(), Some(counter))
            };
            let candidate = destination.parent_directory().join_child(&name);
            if backend.metadata(&candidate).await?.is_none() {
                return Ok(candidate);
            }
            counter += 1;
        }
    }

    async fn first_available_temp_destination<B: TransferBackend>(
        &mut self,
        backend: &mut B,
        destination: &SftpPath,
    ) -> Result<SftpPath, SftpSessionError> {
        let file_name = destination.file_name()?;
        let mut counter = 0_u32;
        loop {
            let suffix = if counter == 0 {
                TEMP_SUFFIX.to_owned()
            } else {
                format!("{TEMP_SUFFIX}-{counter}")
            };
            let candidate = destination
                .parent_directory()
                .join_child(&format!("{file_name}{suffix}"));
            if backend.metadata(&candidate).await?.is_none() {
                return Ok(candidate);
            }
            counter += 1;
        }
    }

    fn batch_default_for(
        &self,
        batch_id: SftpTransferBatchId,
        allowed: &[SftpCollisionDecision],
    ) -> Option<SftpCollisionDecision> {
        self.batches
            .get(&batch_id)
            .and_then(|batch| batch.default_decision)
            .filter(|decision| allowed.contains(decision))
    }

    fn register_collision(
        &mut self,
        batch_id: SftpTransferBatchId,
        transfer_id: SftpTransferId,
        source: SftpPathMetadata,
        destination: SftpPathMetadata,
        allowed_decisions: Vec<SftpCollisionDecision>,
    ) -> Result<SftpCollision, SftpSessionError> {
        self.next_collision_id += 1;
        let collision_id = SftpCollisionId(self.next_collision_id);
        self.collisions.insert(collision_id, transfer_id);
        Ok(SftpCollision {
            id: collision_id,
            batch_id,
            transfer_id,
            source,
            destination: destination.clone(),
            proposed_keep_both_destination: proposed_keep_both_destination(&destination.path)?,
            allowed_decisions,
            can_apply_to_all: self
                .batches
                .get(&batch_id)
                .is_some_and(|batch| batch.active_items > 1),
        })
    }

    fn finish_completed(
        &mut self,
        transfer_id: SftpTransferId,
        event_sender: &UnboundedSender<SftpTransferEvent>,
    ) {
        if let Some(item) = self.remove_item(transfer_id) {
            let _ = event_sender.send(SftpTransferEvent::ItemCompleted {
                batch_id: item.batch_id,
                transfer_id,
                destination: item.destination.unwrap_or(item.request.destination),
                bytes_transferred: item.bytes_transferred,
                total_bytes: item.total_bytes,
                skipped_conflicts: item.skipped_conflicts,
            });
            self.finish_batch_if_needed(item.batch_id, event_sender);
        }
    }

    fn finish_failed(
        &mut self,
        transfer_id: SftpTransferId,
        destination: Option<SftpPath>,
        reason: String,
        event_sender: &UnboundedSender<SftpTransferEvent>,
    ) {
        if let Some(item) = self.remove_item(transfer_id) {
            let _ = event_sender.send(SftpTransferEvent::ItemFailed {
                batch_id: item.batch_id,
                transfer_id,
                destination: destination.or(item.destination),
                reason,
            });
            self.finish_batch_if_needed(item.batch_id, event_sender);
        }
    }

    fn finish_cancelled(
        &mut self,
        transfer_id: SftpTransferId,
        event_sender: &UnboundedSender<SftpTransferEvent>,
    ) {
        if let Some(item) = self.remove_item(transfer_id) {
            let _ = event_sender.send(SftpTransferEvent::ItemCancelled {
                batch_id: item.batch_id,
                transfer_id,
                destination: item.destination,
                bytes_transferred: item.bytes_transferred,
                total_bytes: item.total_bytes,
            });
            self.finish_batch_if_needed(item.batch_id, event_sender);
        }
    }

    fn finish_skipped(
        &mut self,
        transfer_id: SftpTransferId,
        event_sender: &UnboundedSender<SftpTransferEvent>,
    ) {
        if let Some(item) = self.remove_item(transfer_id) {
            let _ = event_sender.send(SftpTransferEvent::ItemSkipped {
                batch_id: item.batch_id,
                transfer_id,
                destination: item.destination,
            });
            self.finish_batch_if_needed(item.batch_id, event_sender);
        }
    }

    fn finish_batch_if_needed(
        &mut self,
        batch_id: SftpTransferBatchId,
        event_sender: &UnboundedSender<SftpTransferEvent>,
    ) {
        if let Some(batch) = self.batches.get_mut(&batch_id) {
            batch.active_items = batch.active_items.saturating_sub(1);
            if batch.active_items == 0 {
                self.batches.remove(&batch_id);
                let _ = event_sender.send(SftpTransferEvent::BatchFinished { batch_id });
            }
        }
    }

    fn remove_item(&mut self, transfer_id: SftpTransferId) -> Option<TransferItem> {
        let item = self.items.remove(&transfer_id)?;
        self.ready.retain(|queued_id| *queued_id != transfer_id);
        if let Some(collision_id) = item.active_collision {
            self.collisions.remove(&collision_id);
        } else {
            self.collisions
                .retain(|_, mapped_id| *mapped_id != transfer_id);
        }
        Some(item)
    }
}

fn missing_source_error(path: &SftpPath) -> SftpSessionError {
    match path {
        SftpPath::Local(path) => SftpSessionError::LocalOperationFailed {
            operation: "inspect source path",
            path: display_path(path),
            reason: "source path does not exist".to_owned(),
        },
        SftpPath::Remote(path) => SftpSessionError::RemoteOperationFailed {
            operation: "inspect source path",
            path: path.clone(),
            reason: "source path does not exist".to_owned(),
        },
    }
}

fn allowed_decisions_for(
    source_type: SftpEntryType,
    destination_type: SftpEntryType,
) -> Vec<SftpCollisionDecision> {
    match (source_type, destination_type) {
        (SftpEntryType::Directory, SftpEntryType::Directory) => vec![
            SftpCollisionDecision::MergeFolders,
            SftpCollisionDecision::KeepBoth,
            SftpCollisionDecision::Skip,
        ],
        (SftpEntryType::Directory, _) => vec![
            SftpCollisionDecision::Replace,
            SftpCollisionDecision::KeepBoth,
            SftpCollisionDecision::Skip,
        ],
        (_, SftpEntryType::Directory) => {
            vec![SftpCollisionDecision::KeepBoth, SftpCollisionDecision::Skip]
        }
        _ => vec![
            SftpCollisionDecision::Replace,
            SftpCollisionDecision::KeepBoth,
            SftpCollisionDecision::Skip,
        ],
    }
}

async fn normalize_requested_destination<B: TransferBackend>(
    backend: &mut B,
    source: &SftpPath,
    destination: &SftpPath,
) -> Result<SftpPath, SftpSessionError> {
    if let Some(metadata) = backend.metadata(destination).await? {
        if metadata.file_type == SftpEntryType::Directory {
            return Ok(destination.join_child(&source.file_name()?));
        }
    }
    Ok(destination.clone())
}

fn validate_remote_directory_entry_name(name: &str) -> Result<(), SftpSessionError> {
    if name.is_empty() || matches!(name, "." | "..") || name.contains('/') || name.contains('\\') {
        return Err(SftpSessionError::LocalOperationFailed {
            operation: "plan recursive download",
            path: name.to_owned(),
            reason: "remote directory entry name is not safe for a local destination".to_owned(),
        });
    }
    Ok(())
}

fn ensure_local_child_within_root(
    destination_root: &SftpPath,
    candidate: &SftpPath,
) -> Result<(), SftpSessionError> {
    let (SftpPath::Local(root), SftpPath::Local(candidate)) = (destination_root, candidate) else {
        return Ok(());
    };
    if candidate.starts_with(root) {
        return Ok(());
    }
    Err(SftpSessionError::LocalOperationFailed {
        operation: "plan recursive download",
        path: display_path(candidate),
        reason: format!(
            "resolved destination escaped the requested local root {}",
            display_path(root)
        ),
    })
}

fn split_name_for_copy(name: String) -> (String, Option<String>) {
    if let Some((stem, extension)) = name.rsplit_once('.') {
        if !stem.is_empty() && !extension.is_empty() {
            return (stem.to_owned(), Some(extension.to_owned()));
        }
    }
    (name, None)
}

fn keep_both_name(stem: &str, extension: Option<&str>, counter: Option<u32>) -> String {
    let suffix = match counter {
        None => " (copy)".to_owned(),
        Some(counter) => format!(" (copy {counter})"),
    };
    match extension {
        Some(extension) => format!("{stem}{suffix}.{extension}"),
        None => format!("{stem}{suffix}"),
    }
}

fn proposed_keep_both_destination(destination: &SftpPath) -> Result<SftpPath, SftpSessionError> {
    let (stem, extension) = split_name_for_copy(destination.file_name()?);
    Ok(destination.parent_directory().join_child(&keep_both_name(
        &stem,
        extension.as_deref(),
        None,
    )))
}

async fn cleanup_temp_destination<B: TransferBackend>(
    backend: &mut B,
    temp_destination: &SftpPath,
) -> Result<(), SftpSessionError> {
    if backend.metadata(temp_destination).await?.is_some() {
        backend.remove_file(temp_destination).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs as stdfs, sync::atomic::AtomicU64, time::UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("could not build tokio runtime for GUI SFTP backend tests")
    }

    fn unique_test_directory(label: &str) -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-artifacts/festerm-ssh-gui-sftp");
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        root.join(format!("{label}-{}-{id}", std::process::id()))
    }

    fn recreate_directory(path: &Path) {
        if path.exists() {
            stdfs::remove_dir_all(path).expect("could not clear pre-existing test directory");
        }
        stdfs::create_dir_all(path).expect("could not create test directory");
    }

    struct TestBackend {
        delay_per_chunk_ms: u64,
    }

    impl TransferBackend for TestBackend {
        fn metadata<'a>(
            &'a mut self,
            path: &'a SftpPath,
        ) -> BackendFuture<'a, Option<SftpPathMetadata>> {
            Box::pin(async move { read_local_path_metadata(real_path(path).as_path()).await })
        }

        fn read_directory<'a>(
            &'a mut self,
            path: &'a SftpPath,
        ) -> BackendFuture<'a, SftpDirectorySnapshot> {
            Box::pin(async move {
                let real = real_path(path);
                let mut snapshot = read_local_directory_snapshot(&real).await?;
                snapshot.location = path.location();
                snapshot.path = path.clone();
                for entry in &mut snapshot.entries {
                    entry.path = remap(entry.path.clone(), path.location());
                }
                Ok(snapshot)
            })
        }

        fn create_directory<'a>(&'a mut self, path: &'a SftpPath) -> BackendFuture<'a, ()> {
            Box::pin(async move {
                let real = real_path(path);
                fs::create_dir(&real)
                    .await
                    .map_err(|error| local_error("create directory", &real, error))
            })
        }

        fn remove_file<'a>(&'a mut self, path: &'a SftpPath) -> BackendFuture<'a, ()> {
            Box::pin(async move {
                let real = real_path(path);
                fs::remove_file(&real)
                    .await
                    .map_err(|error| local_error("remove file", &real, error))
            })
        }

        fn rename<'a>(
            &'a mut self,
            source: &'a SftpPath,
            destination: &'a SftpPath,
        ) -> BackendFuture<'a, ()> {
            Box::pin(async move {
                let source = real_path(source);
                let destination = real_path(destination);
                fs::rename(&source, &destination)
                    .await
                    .map_err(|error| local_error("rename file", &source, error))
            })
        }

        fn copy_file<'a>(
            &'a mut self,
            source: &'a SftpPath,
            destination: &'a SftpPath,
            on_progress: &'a mut (dyn FnMut(u64) -> Result<(), CopyInterrupted> + Send),
        ) -> CopyFuture<'a> {
            Box::pin(async move {
                let source = real_path(source);
                let destination = real_path(destination);
                let mut reader = tokio::fs::File::open(&source).await.map_err(|error| {
                    CopyFileError::Operation(local_error("open source file", &source, error))
                })?;
                let mut writer = tokio::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&destination)
                    .await
                    .map_err(|error| {
                        CopyFileError::Operation(local_error(
                            "create destination file",
                            &destination,
                            error,
                        ))
                    })?;
                let mut total = 0_u64;
                let mut buffer = vec![0_u8; 64 * 1024];
                loop {
                    let read = reader.read(&mut buffer).await.map_err(|error| {
                        CopyFileError::Operation(local_error("read source file", &source, error))
                    })?;
                    if read == 0 {
                        break;
                    }
                    writer.write_all(&buffer[..read]).await.map_err(|error| {
                        CopyFileError::Operation(local_error(
                            "write destination file",
                            &destination,
                            error,
                        ))
                    })?;
                    total += read as u64;
                    on_progress(total).map_err(|_| CopyFileError::Cancelled)?;
                    if self.delay_per_chunk_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            self.delay_per_chunk_ms,
                        ))
                        .await;
                    }
                }
                writer.flush().await.map_err(|error| {
                    CopyFileError::Operation(local_error(
                        "flush destination file",
                        &destination,
                        error,
                    ))
                })?;
                Ok(total)
            })
        }
    }

    struct SnapshotBackend {
        snapshots: HashMap<String, SftpDirectorySnapshot>,
    }

    impl TransferBackend for SnapshotBackend {
        fn metadata<'a>(
            &'a mut self,
            _path: &'a SftpPath,
        ) -> BackendFuture<'a, Option<SftpPathMetadata>> {
            Box::pin(async { Ok(None) })
        }

        fn read_directory<'a>(
            &'a mut self,
            path: &'a SftpPath,
        ) -> BackendFuture<'a, SftpDirectorySnapshot> {
            Box::pin(async move {
                self.snapshots
                    .get(&path.display())
                    .cloned()
                    .ok_or_else(|| missing_source_error(path))
            })
        }

        fn create_directory<'a>(&'a mut self, _path: &'a SftpPath) -> BackendFuture<'a, ()> {
            Box::pin(async { unreachable!("planning test should not create directories") })
        }

        fn remove_file<'a>(&'a mut self, _path: &'a SftpPath) -> BackendFuture<'a, ()> {
            Box::pin(async { unreachable!("planning test should not remove files") })
        }

        fn rename<'a>(
            &'a mut self,
            _source: &'a SftpPath,
            _destination: &'a SftpPath,
        ) -> BackendFuture<'a, ()> {
            Box::pin(async { unreachable!("planning test should not rename files") })
        }

        fn copy_file<'a>(
            &'a mut self,
            _source: &'a SftpPath,
            _destination: &'a SftpPath,
            _on_progress: &'a mut (dyn FnMut(u64) -> Result<(), CopyInterrupted> + Send),
        ) -> CopyFuture<'a> {
            Box::pin(async { unreachable!("planning test should not copy files") })
        }
    }

    fn real_path(path: &SftpPath) -> PathBuf {
        match path {
            SftpPath::Local(path) => path.clone(),
            SftpPath::Remote(path) => PathBuf::from(path),
        }
    }

    fn remap(path: SftpPath, location: SftpLocation) -> SftpPath {
        match (location, path) {
            (SftpLocation::Local, path) => path,
            (SftpLocation::Remote, SftpPath::Local(path)) => SftpPath::Remote(display_path(&path)),
            (SftpLocation::Remote, path) => path,
        }
    }

    fn upload_request(source: &Path, destination_directory: &Path) -> SftpTransferRequest {
        SftpTransferRequest::new(
            SftpPath::local(source.to_path_buf()),
            SftpPath::remote(display_path(destination_directory)),
        )
        .expect("upload request should be valid")
    }

    async fn spawn_worker(
        backend: TestBackend,
    ) -> (
        UnboundedSender<WorkerCommand>,
        UnboundedReceiver<SftpTransferEvent>,
        Arc<Mutex<SftpTransferQueueSnapshot>>,
    ) {
        let (command_sender, command_receiver) = unbounded_channel();
        let (event_sender, event_receiver) = unbounded_channel();
        let snapshot = Arc::new(Mutex::new(SftpTransferQueueSnapshot::default()));
        tokio::spawn(run_transfer_worker(
            backend,
            Arc::clone(&snapshot),
            command_receiver,
            event_sender,
        ));
        (command_sender, event_receiver, snapshot)
    }

    fn queue_batch(
        command_sender: &UnboundedSender<WorkerCommand>,
        requests: Vec<SftpTransferRequest>,
    ) -> SftpQueuedTransferBatch {
        let batch_id = SftpTransferBatchId(1);
        let items = requests
            .into_iter()
            .enumerate()
            .map(|(index, request)| QueuedTransferInput {
                id: SftpTransferId((index + 1) as u64),
                request,
            })
            .collect::<Vec<_>>();
        let transfer_ids = items.iter().map(|item| item.id).collect::<Vec<_>>();
        command_sender
            .send(WorkerCommand::EnqueueBatch { batch_id, items })
            .expect("worker should accept queued batch");
        SftpQueuedTransferBatch {
            batch_id,
            transfer_ids,
        }
    }

    async fn collect_until_batch_finished(
        receiver: &mut UnboundedReceiver<SftpTransferEvent>,
        batch_id: SftpTransferBatchId,
    ) -> Vec<SftpTransferEvent> {
        let mut events = Vec::new();
        while let Some(event) = receiver.recv().await {
            let done = matches!(event, SftpTransferEvent::BatchFinished { batch_id: finished } if finished == batch_id);
            events.push(event);
            if done {
                break;
            }
        }
        events
    }

    async fn next_collision(
        receiver: &mut UnboundedReceiver<SftpTransferEvent>,
    ) -> (Vec<SftpTransferEvent>, SftpCollision) {
        let mut prior = Vec::new();
        while let Some(event) = receiver.recv().await {
            match event {
                SftpTransferEvent::Collision(collision) => return (prior, collision),
                other => prior.push(other),
            }
        }
        panic!("expected a collision event");
    }

    #[test]
    fn local_directory_snapshot_contains_sortable_metadata() {
        let root = unique_test_directory("local-snapshot");
        recreate_directory(&root);
        stdfs::create_dir_all(root.join("folder")).expect("could not create folder fixture");
        stdfs::write(root.join("report.txt"), b"hello").expect("could not create file fixture");

        let snapshot = test_runtime()
            .block_on(SftpDirectorySnapshot::read_local(&root))
            .expect("local snapshot should load");
        assert_eq!(snapshot.location, SftpLocation::Local);
        assert_eq!(
            snapshot.path,
            SftpPath::local(stdfs::canonicalize(&root).expect("root should canonicalize"))
        );
        assert_eq!(snapshot.entries.len(), 2);
        assert_eq!(snapshot.entries[0].name, "folder");
        assert_eq!(snapshot.entries[0].file_type, SftpEntryType::Directory);
        assert_eq!(snapshot.entries[1].name, "report.txt");
        assert_eq!(snapshot.entries[1].size, Some(5));
        assert!(snapshot.entries[1].modified_at.unwrap_or(UNIX_EPOCH) >= UNIX_EPOCH);

        stdfs::remove_dir_all(root).expect("could not clean local snapshot fixtures");
    }

    #[test]
    fn transfer_worker_completes_multiple_items_and_emits_progress() {
        let root = unique_test_directory("happy-path");
        let local = root.join("local");
        let remote = root.join("remote");
        recreate_directory(&local);
        recreate_directory(&remote);
        stdfs::write(local.join("alpha.txt"), b"alpha").expect("could not write alpha");
        stdfs::write(local.join("beta.txt"), b"beta").expect("could not write beta");

        let events = test_runtime().block_on(async {
            let (command_sender, mut receiver, snapshot) = spawn_worker(TestBackend {
                delay_per_chunk_ms: 0,
            })
            .await;
            let batch = queue_batch(
                &command_sender,
                vec![
                    upload_request(&local.join("alpha.txt"), &remote),
                    upload_request(&local.join("beta.txt"), &remote),
                ],
            );
            let events = collect_until_batch_finished(&mut receiver, batch.batch_id).await;
            assert!(
                snapshot.lock().expect("snapshot lock").items.is_empty(),
                "terminal items should leave the worker snapshot"
            );
            events
        });

        assert_eq!(
            stdfs::read(remote.join("alpha.txt")).expect("alpha should exist"),
            b"alpha"
        );
        assert_eq!(
            stdfs::read(remote.join("beta.txt")).expect("beta should exist"),
            b"beta"
        );
        assert!(
            events
                .iter()
                .filter(|event| matches!(event, SftpTransferEvent::ItemStarted { .. }))
                .count()
                >= 2
        );
        assert!(
            events
                .iter()
                .filter(|event| matches!(event, SftpTransferEvent::ItemProgress { .. }))
                .count()
                >= 2
        );
        assert!(
            events
                .iter()
                .filter(|event| matches!(event, SftpTransferEvent::ItemCompleted { .. }))
                .count()
                == 2
        );

        stdfs::remove_dir_all(root).expect("could not clean happy-path fixtures");
    }

    #[test]
    fn transfer_worker_cancels_pending_and_running_items_without_temp_leaks() {
        let root = unique_test_directory("cancellation");
        let local = root.join("local");
        let remote = root.join("remote");
        recreate_directory(&local);
        recreate_directory(&remote);
        stdfs::write(local.join("first.bin"), vec![b'a'; 256 * 1024])
            .expect("could not write first source");
        stdfs::write(local.join("second.bin"), vec![b'b'; 256 * 1024])
            .expect("could not write second source");

        test_runtime().block_on(async {
            let (command_sender, mut receiver, _snapshot) = spawn_worker(TestBackend {
                delay_per_chunk_ms: 10,
            })
            .await;
            let batch = queue_batch(
                &command_sender,
                vec![
                    upload_request(&local.join("first.bin"), &remote),
                    upload_request(&local.join("second.bin"), &remote),
                ],
            );

            command_sender
                .send(WorkerCommand::CancelTransfer(batch.transfer_ids[1]))
                .expect("should cancel pending item");

            let mut cancelled_running = false;
            let mut completed_first = false;
            while let Some(event) = receiver.recv().await {
                match event {
                    SftpTransferEvent::ItemProgress { transfer_id, .. }
                        if transfer_id == batch.transfer_ids[0] && !cancelled_running =>
                    {
                        cancelled_running = true;
                        command_sender
                            .send(WorkerCommand::CancelTransfer(batch.transfer_ids[0]))
                            .expect("should cancel running item");
                    }
                    SftpTransferEvent::ItemCancelled { transfer_id, .. }
                        if transfer_id == batch.transfer_ids[0] =>
                    {
                        completed_first = true;
                    }
                    SftpTransferEvent::BatchFinished { batch_id }
                        if batch_id == batch.batch_id && completed_first =>
                    {
                        break;
                    }
                    _ => {}
                }
            }
        });

        assert!(
            !remote.join("first.bin").exists(),
            "cancelling an in-progress item should not leave a committed file"
        );
        assert!(
            !remote.join("second.bin").exists(),
            "cancelling a pending item should not create a destination file"
        );
        assert!(
            stdfs::read_dir(&remote)
                .expect("remote directory should exist")
                .all(|entry| !entry
                    .expect("entry should load")
                    .file_name()
                    .to_string_lossy()
                    .contains(".festerm-part")),
            "temporary siblings should be cleaned up after cancellation"
        );

        stdfs::remove_dir_all(root).expect("could not clean cancellation fixtures");
    }

    #[test]
    fn collision_decisions_replace_skip_keep_both_and_apply_to_all_are_batch_scoped() {
        let root = unique_test_directory("collisions");
        let local = root.join("local");
        let remote = root.join("remote");
        recreate_directory(&local);
        recreate_directory(&remote);

        stdfs::write(local.join("replace.txt"), b"new replace")
            .expect("could not write replace source");
        stdfs::write(local.join("skip.txt"), b"new skip").expect("could not write skip source");
        stdfs::write(local.join("report.csv"), b"new report")
            .expect("could not write report source");
        stdfs::write(local.join("apply-a.txt"), b"new a").expect("could not write apply-a source");
        stdfs::write(local.join("apply-b.txt"), b"new b").expect("could not write apply-b source");
        stdfs::write(local.join("follow-up.txt"), b"new follow-up")
            .expect("could not write follow-up source");
        stdfs::write(remote.join("replace.txt"), b"old replace")
            .expect("could not seed replace target");
        stdfs::write(remote.join("skip.txt"), b"old skip").expect("could not seed skip target");
        stdfs::write(remote.join("report.csv"), b"old report")
            .expect("could not seed report target");
        stdfs::write(remote.join("report (copy).csv"), b"older report")
            .expect("could not seed report copy");
        stdfs::write(remote.join("apply-a.txt"), b"old a").expect("could not seed apply-a target");
        stdfs::write(remote.join("apply-b.txt"), b"old b").expect("could not seed apply-b target");
        stdfs::write(remote.join("follow-up.txt"), b"old follow-up")
            .expect("could not seed follow-up target");

        test_runtime().block_on(async {
            let (command_sender, mut receiver, _snapshot) = spawn_worker(TestBackend {
                delay_per_chunk_ms: 0,
            })
            .await;
            let batch = queue_batch(
                &command_sender,
                vec![
                    upload_request(&local.join("replace.txt"), &remote),
                    upload_request(&local.join("skip.txt"), &remote),
                    upload_request(&local.join("report.csv"), &remote),
                    upload_request(&local.join("apply-a.txt"), &remote),
                    upload_request(&local.join("apply-b.txt"), &remote),
                ],
            );
            let mut saw_skip = false;
            while let Some(event) = receiver.recv().await {
                match event {
                    SftpTransferEvent::Collision(collision) => {
                        let name = collision
                            .source
                            .path
                            .file_name()
                            .expect("collision source should have a file name");
                        let (decision, scope) = match name.as_str() {
                            "replace.txt" => {
                                (SftpCollisionDecision::Replace, SftpCollisionScope::ThisItem)
                            }
                            "skip.txt" => {
                                (SftpCollisionDecision::Skip, SftpCollisionScope::ThisItem)
                            }
                            "report.csv" => (
                                SftpCollisionDecision::KeepBoth,
                                SftpCollisionScope::ThisItem,
                            ),
                            "apply-a.txt" => (
                                SftpCollisionDecision::Skip,
                                SftpCollisionScope::RemainingConflictsInBatch,
                            ),
                            "apply-b.txt" => {
                                (SftpCollisionDecision::Skip, SftpCollisionScope::ThisItem)
                            }
                            unexpected => panic!("unexpected collision for {unexpected}"),
                        };
                        command_sender
                            .send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
                                collision_id: collision.id,
                                decision,
                                scope,
                            }))
                            .expect("collision should resolve");
                    }
                    SftpTransferEvent::ItemSkipped { transfer_id, .. }
                        if transfer_id == batch.transfer_ids[1] =>
                    {
                        saw_skip = true;
                    }
                    SftpTransferEvent::BatchFinished { batch_id } if batch_id == batch.batch_id => {
                        assert!(saw_skip, "the explicit skip item should be reported");
                        break;
                    }
                    _ => {}
                }
            }

            let second_batch = queue_batch(
                &command_sender,
                vec![upload_request(&local.join("follow-up.txt"), &remote)],
            );
            let (_, follow_up_collision) = next_collision(&mut receiver).await;
            assert_eq!(
                follow_up_collision
                    .source
                    .path
                    .file_name()
                    .expect("follow-up collision should name its file"),
                "follow-up.txt"
            );
            command_sender
                .send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
                    collision_id: follow_up_collision.id,
                    decision: SftpCollisionDecision::Skip,
                    scope: SftpCollisionScope::ThisItem,
                }))
                .expect("follow-up collision should resolve");
            let _ = collect_until_batch_finished(&mut receiver, second_batch.batch_id).await;
        });

        assert_eq!(
            stdfs::read(remote.join("replace.txt")).expect("replace target should exist"),
            b"new replace"
        );
        assert_eq!(
            stdfs::read(remote.join("skip.txt")).expect("skip target should exist"),
            b"old skip"
        );
        assert_eq!(
            stdfs::read(remote.join("report (copy 2).csv")).expect("keep-both target should exist"),
            b"new report"
        );
        assert_eq!(
            stdfs::read(remote.join("apply-a.txt")).expect("apply-a target should exist"),
            b"old a"
        );
        assert_eq!(
            stdfs::read(remote.join("apply-b.txt")).expect("apply-b target should exist"),
            b"old b"
        );

        stdfs::remove_dir_all(root).expect("could not clean collision fixtures");
    }

    #[test]
    fn merge_folders_preserves_destination_only_content_and_obeys_descendant_collisions() {
        let root = unique_test_directory("merge-folders");
        let local = root.join("local");
        let remote = root.join("remote");
        recreate_directory(&local);
        recreate_directory(&remote);

        let source_dir = local.join("project");
        stdfs::create_dir_all(source_dir.join("nested"))
            .expect("could not create source directory tree");
        stdfs::write(source_dir.join("nested/new.txt"), b"new nested")
            .expect("could not write nested new source");
        stdfs::write(source_dir.join("nested/conflict.txt"), b"new conflict")
            .expect("could not write nested conflict source");
        let destination_dir = remote.join("project");
        stdfs::create_dir_all(destination_dir.join("nested"))
            .expect("could not create destination tree");
        stdfs::write(destination_dir.join("nested/conflict.txt"), b"old conflict")
            .expect("could not seed nested conflict");
        stdfs::write(
            destination_dir.join("nested/destination-only.txt"),
            b"keep me",
        )
        .expect("could not seed destination-only file");

        test_runtime().block_on(async {
            let (command_sender, mut receiver, _snapshot) = spawn_worker(TestBackend {
                delay_per_chunk_ms: 0,
            })
            .await;
            let batch = queue_batch(&command_sender, vec![upload_request(&source_dir, &remote)]);

            let (_, collision) = next_collision(&mut receiver).await;
            command_sender
                .send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
                    collision_id: collision.id,
                    decision: SftpCollisionDecision::MergeFolders,
                    scope: SftpCollisionScope::ThisItem,
                }))
                .expect("root folder collision should resolve");

            let (_, nested_collision) = next_collision(&mut receiver).await;
            command_sender
                .send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
                    collision_id: nested_collision.id,
                    decision: SftpCollisionDecision::Skip,
                    scope: SftpCollisionScope::ThisItem,
                }))
                .expect("nested file collision should resolve");

            let events = collect_until_batch_finished(&mut receiver, batch.batch_id).await;
            assert!(events.iter().any(|event| matches!(
                event,
                SftpTransferEvent::ItemCompleted {
                    skipped_conflicts: 1,
                    ..
                }
            )));
        });

        assert_eq!(
            stdfs::read(destination_dir.join("nested/new.txt")).expect("merged file should exist"),
            b"new nested"
        );
        assert_eq!(
            stdfs::read(destination_dir.join("nested/conflict.txt"))
                .expect("conflict target should remain"),
            b"old conflict"
        );
        assert_eq!(
            stdfs::read(destination_dir.join("nested/destination-only.txt"))
                .expect("destination-only file should survive"),
            b"keep me"
        );

        stdfs::remove_dir_all(root).expect("could not clean merge fixtures");
    }

    #[test]
    fn recursive_download_rejects_malicious_remote_entry_names() {
        let root = unique_test_directory("reject-malicious-download-paths");
        let destination = root.join("downloads");
        let source_root = SftpPathMetadata {
            path: SftpPath::remote("/remote/source"),
            file_type: SftpEntryType::Directory,
            size: None,
            modified_at: None,
            permissions: None,
        };
        for entry_name in ["/tmp/escape.txt", "../escape.txt", ".."] {
            let mut backend = SnapshotBackend {
                snapshots: HashMap::from([(
                    source_root.path.display(),
                    SftpDirectorySnapshot {
                        location: SftpLocation::Remote,
                        path: source_root.path.clone(),
                        loaded_at: SystemTime::now(),
                        entries: vec![SftpDirectoryItem {
                            name: entry_name.to_owned(),
                            path: SftpPath::remote(format!("/remote/source/{entry_name}")),
                            file_type: SftpEntryType::File,
                            size: Some(5),
                            modified_at: None,
                            permissions: None,
                        }],
                    },
                )]),
            };
            let error = test_runtime().block_on(WorkerState::default().build_directory_plan(
                &source_root,
                &SftpPath::local(destination.clone()),
                false,
                &mut backend,
            ));
            let error = match error {
                Ok(_) => panic!("malicious remote entry name should be rejected"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("not safe") || error.to_string().contains("escaped"),
                "unexpected error for {entry_name}: {error}"
            );
        }
    }

    #[test]
    fn resolving_remaining_conflicts_after_cancelling_a_paused_item_does_not_panic() {
        let root = unique_test_directory("cancel-then-resolve-collision");
        let local = root.join("local");
        let remote = root.join("remote");
        recreate_directory(&local);
        recreate_directory(&remote);
        stdfs::write(local.join("first.txt"), b"new first").expect("could not write first source");
        stdfs::write(local.join("second.txt"), b"new second")
            .expect("could not write second source");
        stdfs::write(remote.join("first.txt"), b"old first").expect("could not seed first target");
        stdfs::write(remote.join("second.txt"), b"old second")
            .expect("could not seed second target");

        test_runtime().block_on(async {
            let (command_sender, mut receiver, _snapshot) = spawn_worker(TestBackend {
                delay_per_chunk_ms: 0,
            })
            .await;
            let batch = queue_batch(
                &command_sender,
                vec![
                    upload_request(&local.join("first.txt"), &remote),
                    upload_request(&local.join("second.txt"), &remote),
                ],
            );

            let (_, first_collision) = next_collision(&mut receiver).await;
            command_sender
                .send(WorkerCommand::CancelTransfer(first_collision.transfer_id))
                .expect("paused colliding item should cancel");

            let second_collision = loop {
                match receiver.recv().await {
                    Some(SftpTransferEvent::ItemCancelled { transfer_id, .. })
                        if transfer_id == first_collision.transfer_id => {}
                    Some(SftpTransferEvent::Collision(collision))
                        if collision.transfer_id == batch.transfer_ids[1] =>
                    {
                        break collision;
                    }
                    Some(_) => {}
                    None => panic!("worker stopped before the remaining collision resolved"),
                }
            };
            command_sender
                .send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
                    collision_id: second_collision.id,
                    decision: SftpCollisionDecision::Skip,
                    scope: SftpCollisionScope::RemainingConflictsInBatch,
                }))
                .expect("remaining collision should resolve");
            let events = collect_until_batch_finished(&mut receiver, batch.batch_id).await;
            assert!(events.iter().any(|event| matches!(
                event,
                SftpTransferEvent::ItemSkipped { transfer_id, .. }
                    if *transfer_id == second_collision.transfer_id
            )));
        });

        assert_eq!(
            stdfs::read(remote.join("first.txt")).expect("first target should remain"),
            b"old first"
        );
        assert_eq!(
            stdfs::read(remote.join("second.txt")).expect("second target should remain"),
            b"old second"
        );

        stdfs::remove_dir_all(root).expect("could not clean cancel/resolve fixtures");
    }
}
