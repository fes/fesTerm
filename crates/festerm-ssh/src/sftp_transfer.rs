use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::SystemTime,
};

use tokio::{
    fs,
    sync::mpsc::{
        channel,
        error::{TryRecvError, TrySendError},
        Receiver, Sender,
    },
    task::JoinHandle,
};

use crate::sftp::{
    display_path, join_path_segment, join_remote_name_to_local_directory, join_remote_path,
    local_error, read_local_directory_for_planning, read_local_directory_snapshot,
    read_local_path_metadata, remote_file_name, validate_remote_local_file_name, SftpEntryType,
    SftpSession, SftpSessionError, SFTP_CANCELLATION_CLEANUP_TIMEOUT,
};
use crate::sftp_planning::{Budgeted, PlanningQueue, PlanningReservation, SharedPlanningBudget};

const TEMP_SUFFIX: &str = ".festerm-part";
const TRANSFER_COMMAND_QUEUE_CAPACITY: usize = 64;
const TRANSFER_EVENT_QUEUE_CAPACITY: usize = 128;
const TRANSFER_CALLBACK_EVENT_BUFFER_CAPACITY: usize = TRANSFER_COMMAND_QUEUE_CAPACITY + 1;
const MAX_TRANSFER_BATCH_ITEMS: usize = 256;
const MAX_QUEUED_TRANSFER_ITEMS: usize = 1_024;
const MAX_TRANSFER_PLAN_ITEMS: usize = 65_536;
const MAX_TRANSFER_PLAN_MEMORY_PROXY_BYTES: usize = 64 * 1024 * 1024;
// Covers container/node bookkeeping beyond the source, destination, and name text.
pub(crate) const TRANSFER_PLAN_ITEM_OVERHEAD_BYTES: usize = 512;

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
    CleanupIncomplete {
        error: SftpSessionError,
    },
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
    BatchTooLarge {
        requested: usize,
        maximum: usize,
    },
    TransferQueueSaturated {
        requested: usize,
        available: usize,
        capacity: usize,
    },
    CommandQueueSaturated {
        capacity: usize,
    },
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
            Self::BatchTooLarge { requested, maximum } => write!(
                formatter,
                "transfer batch contains {requested} items, exceeding the maximum of {maximum}"
            ),
            Self::TransferQueueSaturated {
                requested,
                available,
                capacity,
            } => write!(
                formatter,
                "SFTP transfer queue is saturated: requested {requested} slots with {available} available out of {capacity}"
            ),
            Self::CommandQueueSaturated { capacity } => write!(
                formatter,
                "SFTP transfer command queue is saturated at its capacity of {capacity}"
            ),
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
    command_sender: Sender<WorkerCommand>,
    event_receiver: Receiver<SftpTransferEvent>,
    snapshot: Arc<Mutex<SftpTransferQueueSnapshot>>,
    admitted_items: Arc<AtomicUsize>,
    next_batch_id: AtomicU64,
    next_transfer_id: AtomicU64,
    shutdown: Option<tokio::sync::oneshot::Sender<tokio::time::Instant>>,
    worker: JoinHandle<Result<(), SftpSessionError>>,
}

impl SftpTransferManager {
    /// Existing per-batch admission bound, also used before GUI bridge enqueue.
    pub const fn max_batch_items() -> usize {
        MAX_TRANSFER_BATCH_ITEMS
    }

    pub fn new(session: SftpSession) -> Self {
        let (command_sender, command_receiver) = channel(TRANSFER_COMMAND_QUEUE_CAPACITY);
        let (event_sender, event_receiver) = channel(TRANSFER_EVENT_QUEUE_CAPACITY);
        let snapshot = Arc::new(Mutex::new(SftpTransferQueueSnapshot::default()));
        let admitted_items = Arc::new(AtomicUsize::new(0));
        let worker_snapshot = Arc::clone(&snapshot);
        let worker_admitted_items = Arc::clone(&admitted_items);
        let (shutdown, cancellation) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            run_owned_transfer_worker(
                LiveTransferBackend { session },
                worker_snapshot,
                Some(worker_admitted_items),
                command_receiver,
                event_sender,
                TransferPlanningLimits::default(),
                cancellation,
            )
            .await
        });
        Self {
            command_sender,
            event_receiver,
            snapshot,
            admitted_items,
            next_batch_id: AtomicU64::new(1),
            next_transfer_id: AtomicU64::new(1),
            shutdown: Some(shutdown),
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
        validate_batch_size(requests.len())?;
        reserve_transfer_slots(&self.admitted_items, requests.len())?;
        let batch_id = SftpTransferBatchId(self.next_batch_id.fetch_add(1, Ordering::Relaxed));
        let items = requests
            .into_iter()
            .map(|request| QueuedTransferInput {
                id: SftpTransferId(self.next_transfer_id.fetch_add(1, Ordering::Relaxed)),
                request,
            })
            .collect::<Vec<_>>();
        let transfer_ids = items.iter().map(|item| item.id).collect::<Vec<_>>();
        if let Err(error) = try_send_command(
            &self.command_sender,
            WorkerCommand::EnqueueBatch { batch_id, items },
        ) {
            self.admitted_items
                .fetch_sub(transfer_ids.len(), Ordering::AcqRel);
            return Err(error);
        }
        Ok(SftpQueuedTransferBatch {
            batch_id,
            transfer_ids,
        })
    }

    pub fn cancel_transfer(
        &self,
        transfer_id: SftpTransferId,
    ) -> Result<(), SftpTransferManagerError> {
        try_send_command(
            &self.command_sender,
            WorkerCommand::CancelTransfer(transfer_id),
        )
    }

    pub fn cancel_batch(
        &self,
        batch_id: SftpTransferBatchId,
    ) -> Result<(), SftpTransferManagerError> {
        try_send_command(&self.command_sender, WorkerCommand::CancelBatch(batch_id))
    }

    /// Cancels all work preceding this command in the manager's ordered queue.
    /// A saturated or closed queue refuses the entire action.
    pub fn cancel_all_transfers(&self) -> Result<(), SftpTransferManagerError> {
        try_send_command(&self.command_sender, WorkerCommand::CancelAllTransfers)
    }

    pub fn resolve_collision(
        &self,
        resolution: SftpCollisionResolution,
    ) -> Result<(), SftpTransferManagerError> {
        try_send_command(
            &self.command_sender,
            WorkerCommand::ResolveCollision(resolution),
        )
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

    /// Cancels owner work and allows at most two seconds for partial-file cleanup.
    pub async fn shutdown(mut self) -> Result<(), SftpSessionError> {
        let deadline = tokio::time::Instant::now() + SFTP_CANCELLATION_CLEANUP_TIMEOUT;
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(deadline);
        }
        match tokio::time::timeout_at(deadline, &mut self.worker).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(SftpSessionError::LocalOperationFailed {
                operation: "stop transfer worker",
                path: "<sftp>".to_owned(),
                reason: "transfer worker ended before reporting cleanup".to_owned(),
            }),
            Err(_) => {
                self.worker.abort();
                Err(SftpSessionError::CancellationCleanupTimedOut)
            }
        }
    }
}

impl Drop for SftpTransferManager {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(tokio::time::Instant::now() + SFTP_CANCELLATION_CLEANUP_TIMEOUT);
        }
    }
}

fn validate_batch_size(item_count: usize) -> Result<(), SftpTransferManagerError> {
    if item_count > MAX_TRANSFER_BATCH_ITEMS {
        return Err(SftpTransferManagerError::BatchTooLarge {
            requested: item_count,
            maximum: MAX_TRANSFER_BATCH_ITEMS,
        });
    }
    Ok(())
}

fn reserve_transfer_slots(
    admitted_items: &AtomicUsize,
    requested: usize,
) -> Result<(), SftpTransferManagerError> {
    let mut current = admitted_items.load(Ordering::Acquire);
    loop {
        let available = MAX_QUEUED_TRANSFER_ITEMS.saturating_sub(current);
        if requested > available {
            return Err(SftpTransferManagerError::TransferQueueSaturated {
                requested,
                available,
                capacity: MAX_QUEUED_TRANSFER_ITEMS,
            });
        }
        match admitted_items.compare_exchange_weak(
            current,
            current + requested,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Ok(()),
            Err(actual) => current = actual,
        }
    }
}

fn try_send_command(
    command_sender: &Sender<WorkerCommand>,
    command: WorkerCommand,
) -> Result<(), SftpTransferManagerError> {
    command_sender
        .try_send(command)
        .map_err(|error| match error {
            TrySendError::Full(_) => SftpTransferManagerError::CommandQueueSaturated {
                capacity: TRANSFER_COMMAND_QUEUE_CAPACITY,
            },
            TrySendError::Closed(_) => SftpTransferManagerError::ManagerClosed,
        })
}

#[derive(Debug)]
enum WorkerCommand {
    EnqueueBatch {
        batch_id: SftpTransferBatchId,
        items: Vec<QueuedTransferInput>,
    },
    CancelTransfer(SftpTransferId),
    CancelAllTransfers,
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
    admitted_items: Option<Arc<AtomicUsize>>,
    pending_events: VecDeque<SftpTransferEvent>,
    planning_budget: Option<SharedPlanningBudget>,
    snapshot_dirty: HashSet<SftpTransferId>,
    snapshot_indexes: HashMap<SftpTransferId, usize>,
    snapshot_membership_changed: bool,
    #[cfg(test)]
    snapshot_rows_updated: usize,
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
    WaitingCollision(Box<Budgeted<PendingCollision>>),
}

struct TransferPlan {
    units: PlanningQueue<TransferUnit>,
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
        remaining_units: PlanningQueue<TransferUnit>,
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
    remaining_units: PlanningQueue<TransferUnit>,
    whole_item: bool,
    reservation: PlanningReservation,
}

struct RootDirectoryDecisionContext {
    source: SftpPathMetadata,
    destination: SftpPath,
    decision: SftpCollisionDecision,
    reservation: PlanningReservation,
}

#[derive(Clone, Copy)]
pub(crate) struct TransferPlanningLimits {
    pub(crate) max_items: usize,
    pub(crate) max_memory_proxy_bytes: usize,
}

impl Default for TransferPlanningLimits {
    fn default() -> Self {
        Self {
            max_items: MAX_TRANSFER_PLAN_ITEMS,
            max_memory_proxy_bytes: MAX_TRANSFER_PLAN_MEMORY_PROXY_BYTES,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransferPlanningLimit {
    Items,
    MemoryProxyBytes,
}

#[derive(Debug)]
pub(crate) enum TransferWorkError {
    Operation(SftpSessionError),
    PlanningLimitExceeded {
        limit: TransferPlanningLimit,
        observed: usize,
        maximum: usize,
    },
    Cancelled,
    Manager(SftpTransferManagerError),
    DirectoryCloseFailed {
        operation_error: Option<Box<TransferWorkError>>,
        close_error: SftpSessionError,
    },
}

impl fmt::Display for TransferWorkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Operation(error) => error.fmt(formatter),
            Self::PlanningLimitExceeded {
                limit,
                observed,
                maximum,
            } => {
                let resource = match limit {
                    TransferPlanningLimit::Items => "items",
                    TransferPlanningLimit::MemoryProxyBytes => "memory-proxy bytes",
                };
                write!(
                    formatter,
                    "recursive transfer planning exceeded its shared {resource} limit: observed {observed}, maximum {maximum}"
                )
            }
            Self::Cancelled => formatter.write_str("transfer cancelled during planning"),
            Self::Manager(error) => error.fmt(formatter),
            Self::DirectoryCloseFailed {
                operation_error,
                close_error,
            } => {
                if let Some(error) = operation_error {
                    write!(formatter, "{error}; ")?;
                }
                write!(formatter, "remote directory close failed: {close_error}")
            }
        }
    }
}

impl From<SftpSessionError> for TransferWorkError {
    fn from(error: SftpSessionError) -> Self {
        Self::Operation(error)
    }
}

impl From<SftpTransferManagerError> for TransferWorkError {
    fn from(error: SftpTransferManagerError) -> Self {
        Self::Manager(error)
    }
}

struct PlanningControl<'a> {
    transfer_id: SftpTransferId,
    command_receiver: &'a mut Receiver<WorkerCommand>,
    event_sender: &'a Sender<SftpTransferEvent>,
}

type BackendFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SftpSessionError>> + Send + 'a>>;
type CopyFuture<'a> = Pin<Box<dyn Future<Output = Result<u64, CopyFileError>> + Send + 'a>>;
type DirectoryFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<PlanningQueue<SftpDirectoryItem>, TransferWorkError>>
            + Send
            + 'a,
    >,
>;

trait TransferBackend {
    fn protect_partial_commit(&mut self, _temporary: &SftpPath, _destination: &SftpPath) {}

    fn cleanup_interrupted_copy(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn metadata<'a>(
        &'a mut self,
        path: &'a SftpPath,
    ) -> BackendFuture<'a, Option<SftpPathMetadata>>;
    fn read_directory<'a>(
        &'a mut self,
        path: &'a SftpPath,
        budget: &'a SharedPlanningBudget,
    ) -> DirectoryFuture<'a>;
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
    fn protect_partial_commit(&mut self, temporary: &SftpPath, destination: &SftpPath) {
        self.session.protect_partial_commit(temporary, destination);
    }

    fn cleanup_interrupted_copy(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(self.session.cleanup_interrupted_transfer())
    }

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
        budget: &'a SharedPlanningBudget,
    ) -> DirectoryFuture<'a> {
        Box::pin(async move {
            match path {
                SftpPath::Local(path) => read_local_directory_for_planning(path, budget).await,
                SftpPath::Remote(path) => {
                    self.session
                        .remote_directory_for_planning(path, budget)
                        .await
                }
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
            let result = match path {
                SftpPath::Local(path) => fs::remove_file(path)
                    .await
                    .map_err(|error| local_error("remove file", path, error)),
                SftpPath::Remote(path) => self.session.remove_remote_file_exact(path).await,
            };
            if result.is_ok() {
                self.session.forget_partial_file(path);
            }
            result
        })
    }

    fn rename<'a>(
        &'a mut self,
        source: &'a SftpPath,
        destination: &'a SftpPath,
    ) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            let result = match (source, destination) {
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
            };
            if result.is_ok() {
                self.session.forget_partial_file(source);
            }
            result
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

async fn run_owned_transfer_worker<B: TransferBackend>(
    mut backend: B,
    snapshot: Arc<Mutex<SftpTransferQueueSnapshot>>,
    admitted_items: Option<Arc<AtomicUsize>>,
    command_receiver: Receiver<WorkerCommand>,
    event_sender: Sender<SftpTransferEvent>,
    planning_limits: TransferPlanningLimits,
    cancellation: tokio::sync::oneshot::Receiver<tokio::time::Instant>,
) -> Result<(), SftpSessionError> {
    let deadline = {
        let work = run_transfer_worker(
            &mut backend,
            Arc::clone(&snapshot),
            admitted_items,
            command_receiver,
            event_sender,
            planning_limits,
        );
        tokio::pin!(work);
        tokio::select! {
            biased;
            deadline = cancellation => deadline.unwrap_or_else(|_| {
                tokio::time::Instant::now() + SFTP_CANCELLATION_CLEANUP_TIMEOUT
            }),
            _ = &mut work => return Ok(()),
        }
    };
    let result = match tokio::time::timeout_at(deadline, backend.cleanup_interrupted_copy()).await {
        Ok(result) => result,
        Err(_) => Err(SftpSessionError::CancellationCleanupTimedOut),
    };
    *snapshot
        .lock()
        .expect("SFTP transfer snapshot lock is not poisoned") =
        SftpTransferQueueSnapshot::default();
    if let Err(error) = &result {
        tracing::warn!(
            target: "festerm::sftp",
            "SFTP cancellation cleanup failed; partial output may remain"
        );
        eprintln!("fesTerm: SFTP cancellation cleanup failed: {error}");
    }
    result
}

async fn run_transfer_worker<B: TransferBackend>(
    backend: &mut B,
    snapshot: Arc<Mutex<SftpTransferQueueSnapshot>>,
    admitted_items: Option<Arc<AtomicUsize>>,
    mut command_receiver: Receiver<WorkerCommand>,
    event_sender: Sender<SftpTransferEvent>,
    planning_limits: TransferPlanningLimits,
) {
    let mut state = WorkerState {
        admitted_items,
        planning_budget: Some(SharedPlanningBudget::new(planning_limits)),
        ..WorkerState::default()
    };
    loop {
        state.publish_snapshot(&snapshot);
        state
            .drain_commands(&mut command_receiver, &event_sender)
            .await;

        if let Some(transfer_id) = state.ready.pop_front() {
            state
                .process_one(
                    transfer_id,
                    backend,
                    &mut command_receiver,
                    &event_sender,
                    planning_limits,
                )
                .await;
            continue;
        }

        if command_receiver.is_closed() && state.items.is_empty() {
            break;
        }

        match command_receiver.recv().await {
            Some(command) => state.handle_command(command, &event_sender).await,
            None if state.items.is_empty() => break,
            None => break,
        }
    }
}

impl WorkerState {
    fn planning_budget(&mut self, limits: TransferPlanningLimits) -> SharedPlanningBudget {
        self.planning_budget
            .get_or_insert_with(|| SharedPlanningBudget::new(limits))
            .clone()
    }

    async fn handle_command(
        &mut self,
        command: WorkerCommand,
        event_sender: &Sender<SftpTransferEvent>,
    ) {
        let event = self.apply_command(command);
        if let Some(event) = event {
            self.emit_event(event_sender, event).await;
        }
    }

    fn apply_command(&mut self, command: WorkerCommand) -> Option<SftpTransferEvent> {
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
                    self.snapshot_dirty.insert(item.id);
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
                self.snapshot_membership_changed = true;
                Some(SftpTransferEvent::BatchQueued {
                    batch_id,
                    transfer_ids,
                })
            }
            WorkerCommand::CancelTransfer(transfer_id) => {
                self.request_cancel(transfer_id);
                None
            }
            WorkerCommand::CancelAllTransfers => {
                self.ready.clear();
                for item in self.items.values_mut() {
                    item.cancel_requested = true;
                    self.ready.push_back(item.id);
                }
                self.ready
                    .make_contiguous()
                    .sort_unstable_by_key(|id| id.raw());
                None
            }
            WorkerCommand::CancelBatch(batch_id) => {
                let mut ids = self
                    .items
                    .values()
                    .filter(|item| item.batch_id == batch_id)
                    .map(|item| item.id)
                    .collect::<Vec<_>>();
                ids.sort_by_key(|transfer_id| transfer_id.raw());
                for transfer_id in ids.into_iter().rev() {
                    self.request_cancel(transfer_id);
                }
                None
            }
            WorkerCommand::ResolveCollision(resolution) => {
                self.apply_collision_resolution_command(resolution);
                None
            }
        }
    }

    fn request_cancel(&mut self, transfer_id: SftpTransferId) {
        if let Some(item) = self.items.get_mut(&transfer_id) {
            item.cancel_requested = true;
            self.ready.retain(|queued_id| *queued_id != transfer_id);
            self.ready.push_front(transfer_id);
        }
    }

    fn apply_collision_resolution_command(&mut self, resolution: SftpCollisionResolution) {
        if let Some(transfer_id) = self.collisions.remove(&resolution.collision_id) {
            let batch_id = if let Some(item) = self.items.get_mut(&transfer_id) {
                item.active_collision = None;
                item.pending_resolution = Some(resolution.clone());
                item.state = SftpTransferState::Queued;
                self.snapshot_dirty.insert(transfer_id);
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
                let mut paused = self
                    .items
                    .values()
                    .filter(|item| item.batch_id == batch_id && item.pending_resolution.is_none())
                    .filter_map(|item| match &item.root_state {
                        TransferRootState::WaitingCollision(collision)
                            if collision
                                .value
                                .allowed_decisions()
                                .contains(&resolution.decision) =>
                        {
                            Some((item.id, collision.value.id()))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                paused.sort_by_key(|(transfer_id, _)| transfer_id.raw());
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
                        self.snapshot_dirty.insert(item_id);
                        self.ready.push_back(item_id);
                    }
                }
            }
        }
    }

    async fn drain_commands(
        &mut self,
        command_receiver: &mut Receiver<WorkerCommand>,
        event_sender: &Sender<SftpTransferEvent>,
    ) {
        for _ in 0..TRANSFER_COMMAND_QUEUE_CAPACITY {
            let Ok(command) = command_receiver.try_recv() else {
                break;
            };
            self.handle_command(command, event_sender).await;
        }
    }

    fn drain_commands_during_copy(
        &mut self,
        command_receiver: &mut Receiver<WorkerCommand>,
        event_sender: &Sender<SftpTransferEvent>,
    ) {
        self.flush_pending_events_nonblocking(event_sender);
        for _ in 0..TRANSFER_COMMAND_QUEUE_CAPACITY {
            if self.pending_events.len() >= TRANSFER_CALLBACK_EVENT_BUFFER_CAPACITY - 1 {
                break;
            }
            let Ok(command) = command_receiver.try_recv() else {
                break;
            };
            if let Some(event) = self.apply_command(command) {
                self.pending_events.push_back(event);
            }
        }
    }

    fn emit_progress(
        &mut self,
        event_sender: &Sender<SftpTransferEvent>,
        event: SftpTransferEvent,
    ) {
        self.flush_pending_events_nonblocking(event_sender);
        if let Some(SftpTransferEvent::ItemProgress { .. }) = self.pending_events.back() {
            *self
                .pending_events
                .back_mut()
                .expect("pending progress event exists") = event;
        } else if let Err(error) = event_sender.try_send(event) {
            match error {
                TrySendError::Full(event) => {
                    debug_assert!(
                        self.pending_events.len() < TRANSFER_CALLBACK_EVENT_BUFFER_CAPACITY
                    );
                    self.pending_events.push_back(event);
                }
                TrySendError::Closed(_) => self.pending_events.clear(),
            }
        }
    }

    fn flush_pending_events_nonblocking(&mut self, event_sender: &Sender<SftpTransferEvent>) {
        while let Some(event) = self.pending_events.pop_front() {
            match event_sender.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(event)) => {
                    self.pending_events.push_front(event);
                    break;
                }
                Err(TrySendError::Closed(_)) => {
                    self.pending_events.clear();
                    break;
                }
            }
        }
    }

    async fn emit_event(
        &mut self,
        event_sender: &Sender<SftpTransferEvent>,
        event: SftpTransferEvent,
    ) {
        while let Some(pending) = self.pending_events.pop_front() {
            let _ = event_sender.send(pending).await;
        }
        let _ = event_sender.send(event).await;
    }

    fn publish_snapshot(&mut self, snapshot: &Arc<Mutex<SftpTransferQueueSnapshot>>) {
        #[cfg(test)]
        {
            self.snapshot_rows_updated = 0;
        }
        if self.snapshot_dirty.is_empty() && !self.snapshot_membership_changed {
            return;
        }
        let mut snapshot = snapshot
            .lock()
            .expect("SFTP transfer snapshot lock is not poisoned");
        if std::mem::take(&mut self.snapshot_membership_changed) {
            snapshot
                .items
                .retain(|row| self.items.contains_key(&row.transfer_id));
            self.snapshot_indexes.clear();
            self.snapshot_indexes.extend(
                snapshot
                    .items
                    .iter()
                    .enumerate()
                    .map(|(index, row)| (row.transfer_id, index)),
            );
        }
        let mut changed = self.snapshot_dirty.drain().collect::<Vec<_>>();
        changed.sort_unstable_by_key(|id| id.raw());
        for id in changed {
            let Some(item) = self.items.get(&id) else {
                continue;
            };
            #[cfg(test)]
            {
                self.snapshot_rows_updated += 1;
            }
            if let Some(&index) = self.snapshot_indexes.get(&id) {
                let row = &mut snapshot.items[index];
                row.state = item.state.clone();
                row.bytes_transferred = item.bytes_transferred;
                row.total_bytes = item.total_bytes;
                if row.destination != item.destination {
                    row.destination = item.destination.clone();
                }
            } else {
                let row = SftpTransferItemSnapshot {
                    batch_id: item.batch_id,
                    transfer_id: item.id,
                    request: item.request.clone(),
                    direction: item.direction,
                    state: item.state.clone(),
                    bytes_transferred: item.bytes_transferred,
                    total_bytes: item.total_bytes,
                    destination: item.destination.clone(),
                };
                let index = snapshot
                    .items
                    .binary_search_by_key(&(row.batch_id.raw(), id.raw()), |row| {
                        (row.batch_id.raw(), row.transfer_id.raw())
                    })
                    .unwrap_or_else(|index| index);
                snapshot.items.insert(index, row);
                self.snapshot_indexes.extend(
                    snapshot.items[index..]
                        .iter()
                        .enumerate()
                        .map(|(offset, row)| (row.transfer_id, index + offset)),
                );
            }
        }
    }

    async fn process_one<B: TransferBackend>(
        &mut self,
        transfer_id: SftpTransferId,
        backend: &mut B,
        command_receiver: &mut Receiver<WorkerCommand>,
        event_sender: &Sender<SftpTransferEvent>,
        planning_limits: TransferPlanningLimits,
    ) {
        if !self.items.contains_key(&transfer_id) {
            return;
        }
        self.snapshot_dirty.insert(transfer_id);
        if self.items[&transfer_id].cancel_requested {
            self.finish_cancelled(transfer_id, event_sender).await;
            return;
        }

        if let Some(resolution) = self.items[&transfer_id].pending_resolution.clone() {
            if let Err(error) = self
                .apply_resolution(
                    transfer_id,
                    resolution,
                    backend,
                    command_receiver,
                    event_sender,
                    planning_limits,
                )
                .await
            {
                self.finish_work_error(transfer_id, error, event_sender)
                    .await;
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
                if let Err(error) = self
                    .prepare_item(
                        transfer_id,
                        backend,
                        command_receiver,
                        event_sender,
                        planning_limits,
                    )
                    .await
                {
                    self.finish_work_error(transfer_id, error, event_sender)
                        .await;
                }
            }
            TransferRootState::Ready(plan) => {
                if let Err(error) = self
                    .execute_plan_step(transfer_id, plan, backend, command_receiver, event_sender)
                    .await
                {
                    let error = match backend.cleanup_interrupted_copy().await {
                        Ok(()) => error,
                        Err(cleanup_error) => cleanup_error.into(),
                    };
                    self.finish_work_error(transfer_id, error, event_sender)
                        .await;
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
        command_receiver: &mut Receiver<WorkerCommand>,
        event_sender: &Sender<SftpTransferEvent>,
        planning_limits: TransferPlanningLimits,
    ) -> Result<(), TransferWorkError> {
        let request = self.items[&transfer_id].request.clone();
        let batch_id = self.items[&transfer_id].batch_id;
        self.items.get_mut(&transfer_id).expect("item exists").state = SftpTransferState::Planning;

        let source = backend
            .metadata(&request.source)
            .await?
            .ok_or_else(|| missing_source_error(&request.source))?;
        let destination =
            normalize_requested_destination(backend, &request.source, &request.destination).await?;
        let budget = self.planning_budget(planning_limits);
        let reservation = budget.unit(&source.path, &destination)?;
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
                        RootDirectoryDecisionContext {
                            source,
                            destination,
                            decision,
                            reservation,
                        },
                        backend,
                        command_receiver,
                        event_sender,
                        planning_limits,
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
                    item.root_state = TransferRootState::WaitingCollision(Box::new(Budgeted {
                        value: PendingCollision::RootDirectory {
                            collision: collision.clone(),
                            source,
                            destination,
                        },
                        reservation,
                    }));
                    self.emit_event(event_sender, SftpTransferEvent::Collision(collision))
                        .await;
                }
            } else {
                let mut control = PlanningControl {
                    transfer_id,
                    command_receiver,
                    event_sender,
                };
                let plan = self
                    .build_directory_plan_with_root(
                        Budgeted {
                            value: (source, destination),
                            reservation,
                        },
                        false,
                        backend,
                        Some(&mut control),
                        planning_limits,
                    )
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
                        remaining_units: PlanningQueue::new(&budget),
                        whole_item: true,
                        reservation,
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
                item.root_state = TransferRootState::WaitingCollision(Box::new(Budgeted {
                    value: PendingCollision::File {
                        collision: collision.clone(),
                        source,
                        destination,
                        remaining_units: PlanningQueue::new(&budget),
                        whole_item: true,
                    },
                    reservation,
                }));
                self.emit_event(event_sender, SftpTransferEvent::Collision(collision))
                    .await;
            }
        } else {
            let mut units = PlanningQueue::new(&budget);
            units.push_back(Budgeted {
                value: TransferUnit::CopyFile {
                    source,
                    destination,
                    replace_existing_at_commit: false,
                    whole_item: true,
                },
                reservation,
            })?;
            let item = self.items.get_mut(&transfer_id).expect("item exists");
            item.root_state = TransferRootState::Ready(TransferPlan {
                units,
                total_bytes: item.total_bytes,
            });
            item.state = SftpTransferState::Queued;
            self.ready.push_front(transfer_id);
        }
        Ok(())
    }

    #[cfg(test)]
    async fn build_directory_plan<B: TransferBackend>(
        &mut self,
        source_root: &SftpPathMetadata,
        destination_root: &SftpPath,
        replace_existing_non_directory: bool,
        backend: &mut B,
        control: Option<&mut PlanningControl<'_>>,
        limits: TransferPlanningLimits,
    ) -> Result<TransferPlan, TransferWorkError> {
        let budget = self.planning_budget(limits);
        let reservation = budget.unit(&source_root.path, destination_root)?;
        self.build_directory_plan_with_root(
            Budgeted {
                value: (source_root.clone(), destination_root.clone()),
                reservation,
            },
            replace_existing_non_directory,
            backend,
            control,
            limits,
        )
        .await
    }

    async fn build_directory_plan_with_root<B: TransferBackend>(
        &mut self,
        root: Budgeted<(SftpPathMetadata, SftpPath)>,
        replace_existing_non_directory: bool,
        backend: &mut B,
        mut control: Option<&mut PlanningControl<'_>>,
        limits: TransferPlanningLimits,
    ) -> Result<TransferPlan, TransferWorkError> {
        let budget = self.planning_budget(limits);
        let mut units = PlanningQueue::new(&budget);
        let mut total_bytes = 0_u64;
        let mut total_known = true;
        let (source_root, destination_root) = root.value;
        units.push_back(Budgeted {
            value: TransferUnit::EnsureDirectory {
                destination: destination_root.clone(),
                replace_existing_non_directory,
            },
            reservation: root.reservation,
        })?;
        let mut stack = PlanningQueue::new(&budget);
        let pair_reservation = budget.reserve(
            0,
            std::mem::size_of::<(SftpPath, SftpPath)>()
                .saturating_add(path_memory_proxy_bytes(&source_root.path))
                .saturating_add(path_memory_proxy_bytes(&destination_root)),
        )?;
        stack.push_back(Budgeted {
            value: (source_root.path.clone(), destination_root.clone()),
            reservation: pair_reservation,
        })?;
        while let Some(directory) = stack.pop_back() {
            let _directory_reservation = directory.reservation;
            let (source_directory, destination_directory) = directory.value;
            let mut entries = match control.as_deref_mut() {
                Some(control) => {
                    self.read_directory_during_planning(
                        &source_directory,
                        backend,
                        control,
                        &budget,
                    )
                    .await?
                }
                None => backend.read_directory(&source_directory, &budget).await?,
            };
            let mut child_directories = PlanningQueue::new(&budget);
            while let Some(entry) = entries.pop_front() {
                let mut reservation = entry.reservation;
                let entry = entry.value;
                if matches!(source_directory, SftpPath::Remote(_))
                    && matches!(&destination_root, SftpPath::Local(_))
                {
                    validate_remote_local_file_name(&entry.name, "plan recursive download")?;
                }
                reservation.grow(
                    0,
                    path_memory_proxy_bytes(&entry.path).saturating_add(
                        path_memory_proxy_bytes(&destination_directory)
                            .saturating_add(entry.name.capacity())
                            .saturating_add(2)
                            .saturating_mul(12),
                    ),
                )?;
                let child_destination = match (&source_directory, &destination_directory) {
                    (SftpPath::Remote(_), SftpPath::Local(directory)) => {
                        SftpPath::local(join_remote_name_to_local_directory(
                            directory,
                            &entry.name,
                            "plan recursive download",
                        )?)
                    }
                    _ => destination_directory.join_child(&entry.name),
                };
                ensure_local_child_within_root(&destination_root, &child_destination)?;
                if entry.file_type == SftpEntryType::Directory {
                    let pair_reservation = budget.reserve(
                        0,
                        std::mem::size_of::<(SftpPath, SftpPath)>()
                            .saturating_add(path_memory_proxy_bytes(&entry.path))
                            .saturating_add(path_memory_proxy_bytes(&child_destination)),
                    )?;
                    units.push_back(Budgeted {
                        value: TransferUnit::EnsureDirectory {
                            destination: child_destination.clone(),
                            replace_existing_non_directory: false,
                        },
                        reservation,
                    })?;
                    child_directories.push_back(Budgeted {
                        value: (entry.path, child_destination),
                        reservation: pair_reservation,
                    })?;
                } else {
                    if let Some(size) = entry.size {
                        if total_known {
                            if let Some(updated_total) = total_bytes.checked_add(size) {
                                total_bytes = updated_total;
                            } else {
                                total_known = false;
                            }
                        }
                    } else {
                        total_known = false;
                    }
                    units.push_back(Budgeted {
                        value: TransferUnit::CopyFile {
                            source: SftpPathMetadata {
                                path: entry.path,
                                file_type: entry.file_type,
                                size: entry.size,
                                modified_at: entry.modified_at,
                                permissions: entry.permissions,
                            },
                            destination: child_destination,
                            replace_existing_at_commit: false,
                            whole_item: false,
                        },
                        reservation,
                    })?;
                }
            }
            while let Some(directory) = child_directories.pop_back() {
                stack.push_back(directory)?;
            }
        }
        Ok(TransferPlan {
            units,
            total_bytes: if total_known { Some(total_bytes) } else { None },
        })
    }

    async fn read_directory_during_planning<B: TransferBackend>(
        &mut self,
        path: &SftpPath,
        backend: &mut B,
        control: &mut PlanningControl<'_>,
        budget: &SharedPlanningBudget,
    ) -> Result<PlanningQueue<SftpDirectoryItem>, TransferWorkError> {
        let read = backend.read_directory(path, budget);
        tokio::pin!(read);
        loop {
            tokio::select! {
                biased;
                command = control.command_receiver.recv() => {
                    match command {
                        Some(command) => {
                            self.handle_command(command, control.event_sender).await;
                            if self
                                .items
                                .get(&control.transfer_id)
                                .is_none_or(|item| item.cancel_requested)
                            {
                                return Err(TransferWorkError::Cancelled);
                            }
                        }
                        None => return read.await,
                    }
                }
                result = &mut read => return result,
            }
        }
    }

    async fn apply_root_directory_decision<B: TransferBackend>(
        &mut self,
        transfer_id: SftpTransferId,
        context: RootDirectoryDecisionContext,
        backend: &mut B,
        command_receiver: &mut Receiver<WorkerCommand>,
        event_sender: &Sender<SftpTransferEvent>,
        planning_limits: TransferPlanningLimits,
    ) -> Result<(), TransferWorkError> {
        let mut reservation = context.reservation;
        let source = context.source;
        let destination = context.destination;
        let decision = context.decision;
        match decision {
            SftpCollisionDecision::Skip => {
                self.finish_skipped(transfer_id, event_sender).await;
            }
            SftpCollisionDecision::KeepBoth => {
                let target = self
                    .first_available_keep_both_destination(backend, &destination)
                    .await?;
                reservation.grow(0, path_memory_proxy_bytes(&target))?;
                let mut control = PlanningControl {
                    transfer_id,
                    command_receiver,
                    event_sender,
                };
                let plan = self
                    .build_directory_plan_with_root(
                        Budgeted {
                            value: (source, target.clone()),
                            reservation,
                        },
                        false,
                        backend,
                        Some(&mut control),
                        planning_limits,
                    )
                    .await?;
                let item = self.items.get_mut(&transfer_id).expect("item exists");
                item.destination = Some(target);
                item.total_bytes = plan.total_bytes;
                item.root_state = TransferRootState::Ready(plan);
                item.state = SftpTransferState::Queued;
                self.ready.push_front(transfer_id);
            }
            SftpCollisionDecision::MergeFolders => {
                let mut control = PlanningControl {
                    transfer_id,
                    command_receiver,
                    event_sender,
                };
                let plan = self
                    .build_directory_plan_with_root(
                        Budgeted {
                            value: (source, destination),
                            reservation,
                        },
                        false,
                        backend,
                        Some(&mut control),
                        planning_limits,
                    )
                    .await?;
                let item = self.items.get_mut(&transfer_id).expect("item exists");
                item.total_bytes = plan.total_bytes;
                item.root_state = TransferRootState::Ready(plan);
                item.state = SftpTransferState::Queued;
                self.ready.push_front(transfer_id);
            }
            SftpCollisionDecision::Replace => {
                let mut control = PlanningControl {
                    transfer_id,
                    command_receiver,
                    event_sender,
                };
                let plan = self
                    .build_directory_plan_with_root(
                        Budgeted {
                            value: (source, destination),
                            reservation,
                        },
                        true,
                        backend,
                        Some(&mut control),
                        planning_limits,
                    )
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
        command_receiver: &mut Receiver<WorkerCommand>,
        event_sender: &Sender<SftpTransferEvent>,
        planning_limits: TransferPlanningLimits,
    ) -> Result<(), TransferWorkError> {
        let pending = {
            let item = self.items.get_mut(&transfer_id).expect("item exists");
            item.pending_resolution = None;
            std::mem::replace(&mut item.root_state, TransferRootState::Pending)
        };
        match pending {
            TransferRootState::WaitingCollision(pending) => {
                let reservation = pending.reservation;
                match pending.value {
                    PendingCollision::RootDirectory {
                        collision,
                        source,
                        destination,
                    } => {
                        if !collision.allowed_decisions.contains(&resolution.decision) {
                            return Err(SftpTransferManagerError::InvalidCollisionDecision {
                                collision_id: collision.id,
                                decision: resolution.decision,
                            }
                            .into());
                        }
                        drop(collision);
                        self.apply_root_directory_decision(
                            transfer_id,
                            RootDirectoryDecisionContext {
                                source,
                                destination,
                                decision: resolution.decision,
                                reservation,
                            },
                            backend,
                            command_receiver,
                            event_sender,
                            planning_limits,
                        )
                        .await?;
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
                            }
                            .into());
                        }
                        drop(collision);
                        self.apply_file_decision(
                            transfer_id,
                            FileDecisionContext {
                                source,
                                destination,
                                remaining_units,
                                whole_item,
                                reservation,
                            },
                            resolution.decision,
                            backend,
                            event_sender,
                        )
                        .await?;
                    }
                }
            }
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
        event_sender: &Sender<SftpTransferEvent>,
    ) -> Result<(), TransferWorkError> {
        let mut reservation = context.reservation;
        let source = context.source;
        let destination = context.destination;
        let mut remaining_units = context.remaining_units;
        let whole_item = context.whole_item;
        match decision {
            SftpCollisionDecision::Skip => {
                if whole_item && remaining_units.is_empty() {
                    self.finish_skipped(transfer_id, event_sender).await;
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
                reservation.grow(0, path_memory_proxy_bytes(&target))?;
                remaining_units.push_front(Budgeted {
                    value: TransferUnit::CopyFile {
                        source,
                        destination: target.clone(),
                        replace_existing_at_commit: false,
                        whole_item,
                    },
                    reservation,
                })?;
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
                remaining_units.push_front(Budgeted {
                    value: TransferUnit::CopyFile {
                        source,
                        destination,
                        replace_existing_at_commit: true,
                        whole_item,
                    },
                    reservation,
                })?;
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
                }
                .into());
            }
        }
        Ok(())
    }

    async fn execute_plan_step<B: TransferBackend>(
        &mut self,
        transfer_id: SftpTransferId,
        mut plan: TransferPlan,
        backend: &mut B,
        command_receiver: &mut Receiver<WorkerCommand>,
        event_sender: &Sender<SftpTransferEvent>,
    ) -> Result<(), TransferWorkError> {
        let Some(unit) = plan.units.pop_front() else {
            self.finish_completed(transfer_id, event_sender).await;
            return Ok(());
        };
        let reservation = unit.reservation;
        match unit.value {
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
                        }
                        .into());
                    }
                    None => backend.create_directory(&destination).await?,
                }
                let batch_id = self.items[&transfer_id].batch_id;
                let item = self.items.get_mut(&transfer_id).expect("item exists");
                item.root_state = TransferRootState::Ready(plan);
                item.state = SftpTransferState::Queued;
                self.emit_event(
                    event_sender,
                    SftpTransferEvent::DestinationDirectoryRefreshRequested {
                        batch_id,
                        transfer_id,
                        directory: destination.parent_directory(),
                    },
                )
                .await;
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
                                    reservation,
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
                            item.root_state =
                                TransferRootState::WaitingCollision(Box::new(Budgeted {
                                    value: PendingCollision::File {
                                        collision: collision.clone(),
                                        source,
                                        destination,
                                        remaining_units: plan.units,
                                        whole_item,
                                    },
                                    reservation,
                                }));
                            self.emit_event(event_sender, SftpTransferEvent::Collision(collision))
                                .await;
                        }
                        return Ok(());
                    }
                }

                self.emit_started_if_needed(transfer_id, event_sender, &destination)
                    .await;
                let batch_id = self.items[&transfer_id].batch_id;
                let base_bytes = self.items[&transfer_id].bytes_transferred;
                let temp_destination = self
                    .first_available_temp_destination(backend, &destination)
                    .await?;
                let mut progress = |copied_for_file: u64| -> Result<(), CopyInterrupted> {
                    self.drain_commands_during_copy(command_receiver, event_sender);
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
                        let progress_event = SftpTransferEvent::ItemProgress {
                            batch_id,
                            transfer_id,
                            current_path: source.path.clone(),
                            bytes_transferred: item.bytes_transferred,
                            total_bytes: item.total_bytes,
                        };
                        self.emit_progress(event_sender, progress_event);
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
                                backend.protect_partial_commit(&temp_destination, &destination);
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
                                self.cleanup_temporary_output(backend, event_sender).await?;
                                let item = self.items.get_mut(&transfer_id).expect("item exists");
                                item.state = SftpTransferState::AwaitingCollision(collision.id);
                                item.active_collision = Some(collision.id);
                                item.root_state =
                                    TransferRootState::WaitingCollision(Box::new(Budgeted {
                                        value: PendingCollision::File {
                                            collision: collision.clone(),
                                            source,
                                            destination,
                                            remaining_units: plan.units,
                                            whole_item,
                                        },
                                        reservation,
                                    }));
                                self.emit_event(
                                    event_sender,
                                    SftpTransferEvent::Collision(collision),
                                )
                                .await;
                                return Ok(());
                            }
                        }
                        backend.rename(&temp_destination, &destination).await?;
                        let item = self.items.get_mut(&transfer_id).expect("item exists");
                        item.root_state = TransferRootState::Ready(plan);
                        item.state = SftpTransferState::Queued;
                        self.emit_event(
                            event_sender,
                            SftpTransferEvent::DestinationDirectoryRefreshRequested {
                                batch_id,
                                transfer_id,
                                directory: destination.parent_directory(),
                            },
                        )
                        .await;
                        self.ready.push_front(transfer_id);
                    }
                    Err(CopyFileError::Cancelled) => {
                        self.cleanup_temporary_output(backend, event_sender).await?;
                        self.finish_cancelled(transfer_id, event_sender).await;
                    }
                    Err(CopyFileError::Operation(error)) => {
                        self.cleanup_temporary_output(backend, event_sender).await?;
                        return Err(error.into());
                    }
                }
            }
        }
        Ok(())
    }

    async fn cleanup_temporary_output<B: TransferBackend>(
        &mut self,
        backend: &mut B,
        event_sender: &Sender<SftpTransferEvent>,
    ) -> Result<(), SftpSessionError> {
        match backend.cleanup_interrupted_copy().await {
            Err(error @ SftpSessionError::PartialFileOwnershipUnconfirmed { .. }) => {
                tracing::warn!(
                    target: "festerm::sftp",
                    "SFTP temporary output was preserved; manual inspection is required"
                );
                eprintln!("fesTerm: SFTP cleanup is incomplete: {error}");
                self.emit_event(event_sender, SftpTransferEvent::CleanupIncomplete { error })
                    .await;
                Ok(())
            }
            result => result,
        }
    }

    async fn emit_started_if_needed(
        &mut self,
        transfer_id: SftpTransferId,
        event_sender: &Sender<SftpTransferEvent>,
        destination: &SftpPath,
    ) {
        let item = self.items.get_mut(&transfer_id).expect("item exists");
        if item.started {
            return;
        }
        item.started = true;
        item.state = SftpTransferState::Running;
        let event = SftpTransferEvent::ItemStarted {
            batch_id: item.batch_id,
            transfer_id,
            source: item.request.source.clone(),
            destination: item
                .destination
                .clone()
                .unwrap_or_else(|| destination.clone()),
            direction: item.direction,
            total_bytes: item.total_bytes,
        };
        self.emit_event(event_sender, event).await;
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

    async fn finish_work_error(
        &mut self,
        transfer_id: SftpTransferId,
        error: TransferWorkError,
        event_sender: &Sender<SftpTransferEvent>,
    ) {
        match error {
            TransferWorkError::Cancelled => {
                self.finish_cancelled(transfer_id, event_sender).await;
            }
            other => {
                self.finish_failed(transfer_id, None, other.to_string(), event_sender)
                    .await;
            }
        }
    }

    async fn finish_completed(
        &mut self,
        transfer_id: SftpTransferId,
        event_sender: &Sender<SftpTransferEvent>,
    ) {
        if let Some(item) = self.remove_item(transfer_id) {
            self.emit_event(
                event_sender,
                SftpTransferEvent::ItemCompleted {
                    batch_id: item.batch_id,
                    transfer_id,
                    destination: item.destination.unwrap_or(item.request.destination),
                    bytes_transferred: item.bytes_transferred,
                    total_bytes: item.total_bytes,
                    skipped_conflicts: item.skipped_conflicts,
                },
            )
            .await;
            self.finish_batch_if_needed(item.batch_id, event_sender)
                .await;
        }
    }

    async fn finish_failed(
        &mut self,
        transfer_id: SftpTransferId,
        destination: Option<SftpPath>,
        reason: String,
        event_sender: &Sender<SftpTransferEvent>,
    ) {
        if let Some(item) = self.remove_item(transfer_id) {
            self.emit_event(
                event_sender,
                SftpTransferEvent::ItemFailed {
                    batch_id: item.batch_id,
                    transfer_id,
                    destination: destination.or(item.destination),
                    reason,
                },
            )
            .await;
            self.finish_batch_if_needed(item.batch_id, event_sender)
                .await;
        }
    }

    async fn finish_cancelled(
        &mut self,
        transfer_id: SftpTransferId,
        event_sender: &Sender<SftpTransferEvent>,
    ) {
        if let Some(item) = self.remove_item(transfer_id) {
            self.emit_event(
                event_sender,
                SftpTransferEvent::ItemCancelled {
                    batch_id: item.batch_id,
                    transfer_id,
                    destination: item.destination,
                    bytes_transferred: item.bytes_transferred,
                    total_bytes: item.total_bytes,
                },
            )
            .await;
            self.finish_batch_if_needed(item.batch_id, event_sender)
                .await;
        }
    }

    async fn finish_skipped(
        &mut self,
        transfer_id: SftpTransferId,
        event_sender: &Sender<SftpTransferEvent>,
    ) {
        if let Some(item) = self.remove_item(transfer_id) {
            self.emit_event(
                event_sender,
                SftpTransferEvent::ItemSkipped {
                    batch_id: item.batch_id,
                    transfer_id,
                    destination: item.destination,
                },
            )
            .await;
            self.finish_batch_if_needed(item.batch_id, event_sender)
                .await;
        }
    }

    async fn finish_batch_if_needed(
        &mut self,
        batch_id: SftpTransferBatchId,
        event_sender: &Sender<SftpTransferEvent>,
    ) {
        if let Some(batch) = self.batches.get_mut(&batch_id) {
            batch.active_items = batch.active_items.saturating_sub(1);
            if batch.active_items == 0 {
                self.batches.remove(&batch_id);
                self.emit_event(event_sender, SftpTransferEvent::BatchFinished { batch_id })
                    .await;
            }
        }
    }

    fn remove_item(&mut self, transfer_id: SftpTransferId) -> Option<TransferItem> {
        let item = self.items.remove(&transfer_id)?;
        self.snapshot_membership_changed = true;
        self.snapshot_dirty.remove(&transfer_id);
        if let Some(admitted_items) = &self.admitted_items {
            admitted_items.fetch_sub(1, Ordering::AcqRel);
        }
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

pub(crate) fn path_memory_proxy_bytes(path: &SftpPath) -> usize {
    match path {
        SftpPath::Local(path) => path.capacity(),
        SftpPath::Remote(path) => path.capacity(),
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
            if let (SftpPath::Remote(_), SftpPath::Local(directory)) = (source, destination) {
                return Ok(SftpPath::local(join_remote_name_to_local_directory(
                    directory,
                    &source.file_name()?,
                    "resolve download destination",
                )?));
            }
            return Ok(destination.join_child(&source.file_name()?));
        }
    }
    Ok(destination.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs as stdfs,
        future::pending,
        sync::atomic::AtomicU64,
        time::{Duration, UNIX_EPOCH},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::{oneshot, Notify},
    };

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

    #[derive(Default)]
    struct TestBackend {
        delay_per_chunk_ms: u64,
        cleanup_delay_ms: u64,
        retain_uncertain_partial: bool,
        late_collision: Option<(PathBuf, Vec<u8>)>,
        partial_file: Option<PathBuf>,
        commit_destination: Option<PathBuf>,
        replacement_removed: Option<Arc<Notify>>,
        fail_replace_rename: bool,
        /// Signalled once `copy_file` has emitted its final progress callback,
        /// so a test can wait for a transfer to finish without racing it.
        copy_finished: Option<std::sync::Arc<Notify>>,
    }

    impl TransferBackend for TestBackend {
        fn protect_partial_commit(&mut self, temporary: &SftpPath, destination: &SftpPath) {
            assert_eq!(self.partial_file.as_ref(), Some(&real_path(temporary)));
            self.commit_destination = Some(real_path(destination));
        }

        fn cleanup_interrupted_copy(&mut self) -> BackendFuture<'_, ()> {
            Box::pin(async move {
                if self.cleanup_delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(self.cleanup_delay_ms))
                        .await;
                }
                if let (Some(temporary), Some(destination)) =
                    (self.partial_file.as_ref(), self.commit_destination.as_ref())
                {
                    let error = SftpSessionError::TransferCommitInterrupted {
                        temporary: display_path(temporary),
                        destination: display_path(destination),
                    };
                    self.partial_file = None;
                    self.commit_destination = None;
                    return Err(error);
                }
                if let Some(path) = self.partial_file.take() {
                    if self.retain_uncertain_partial {
                        return Err(SftpSessionError::PartialFileOwnershipUnconfirmed {
                            path: display_path(&path),
                        });
                    }
                    match fs::remove_file(&path).await {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(local_error("remove partial file", &path, error)),
                    }
                }
                Ok(())
            })
        }

        fn metadata<'a>(
            &'a mut self,
            path: &'a SftpPath,
        ) -> BackendFuture<'a, Option<SftpPathMetadata>> {
            Box::pin(async move { read_local_path_metadata(real_path(path).as_path()).await })
        }

        fn read_directory<'a>(
            &'a mut self,
            path: &'a SftpPath,
            budget: &'a SharedPlanningBudget,
        ) -> DirectoryFuture<'a> {
            Box::pin(async move {
                let real = real_path(path);
                let mut entries = read_local_directory_for_planning(&real, budget).await?;
                for entry in entries.entries_mut() {
                    entry.reservation.grow(
                        0,
                        path_memory_proxy_bytes(&entry.value.path).saturating_mul(2),
                    )?;
                    entry.value.path = remap(entry.value.path.clone(), path.location());
                }
                Ok(entries)
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
                    .map_err(|error| local_error("remove file", &real, error))?;
                if let Some(removed) = &self.replacement_removed {
                    removed.notify_one();
                    pending::<()>().await;
                }
                Ok(())
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
                if self.fail_replace_rename && self.commit_destination.is_some() {
                    return Err(SftpSessionError::LocalOperationFailed {
                        operation: "rename file",
                        path: display_path(&source),
                        reason: "controlled replacement commit refusal".to_owned(),
                    });
                }
                fs::rename(&source, &destination)
                    .await
                    .map_err(|error| local_error("rename file", &source, error))?;
                self.partial_file = None;
                self.commit_destination = None;
                Ok(())
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
                self.partial_file = Some(destination.clone());
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
                if let Some((path, content)) = self.late_collision.take() {
                    fs::write(&path, content).await.unwrap();
                }
                if let Some(finished) = &self.copy_finished {
                    finished.notify_one();
                }
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
            budget: &'a SharedPlanningBudget,
        ) -> DirectoryFuture<'a> {
            Box::pin(async move {
                let snapshot = self
                    .snapshots
                    .get(&path.display())
                    .ok_or_else(|| missing_source_error(path))?;
                let mut entries = PlanningQueue::new(budget);
                for entry in &snapshot.entries {
                    let reservation = budget
                        .entry(entry.name.capacity(), path_memory_proxy_bytes(&entry.path))?;
                    entries.push_back(Budgeted {
                        value: entry.clone(),
                        reservation,
                    })?;
                }
                entries.sort_by_name();
                Ok(entries)
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

    struct SlowEnumerationBackend {
        source: SftpPath,
        enumeration_started: Option<oneshot::Sender<()>>,
    }

    impl TransferBackend for SlowEnumerationBackend {
        fn metadata<'a>(
            &'a mut self,
            path: &'a SftpPath,
        ) -> BackendFuture<'a, Option<SftpPathMetadata>> {
            let metadata = (path == &self.source).then(|| SftpPathMetadata {
                path: path.clone(),
                file_type: SftpEntryType::Directory,
                size: None,
                modified_at: None,
                permissions: None,
            });
            Box::pin(async move { Ok(metadata) })
        }

        fn read_directory<'a>(
            &'a mut self,
            _path: &'a SftpPath,
            _budget: &'a SharedPlanningBudget,
        ) -> DirectoryFuture<'a> {
            let enumeration_started = self.enumeration_started.take();
            Box::pin(async move {
                if let Some(enumeration_started) = enumeration_started {
                    let _ = enumeration_started.send(());
                }
                pending::<Result<PlanningQueue<SftpDirectoryItem>, TransferWorkError>>().await
            })
        }

        fn create_directory<'a>(&'a mut self, _path: &'a SftpPath) -> BackendFuture<'a, ()> {
            Box::pin(async { unreachable!("cancelled planning should not create directories") })
        }

        fn remove_file<'a>(&'a mut self, _path: &'a SftpPath) -> BackendFuture<'a, ()> {
            Box::pin(async { unreachable!("cancelled planning should not remove files") })
        }

        fn rename<'a>(
            &'a mut self,
            _source: &'a SftpPath,
            _destination: &'a SftpPath,
        ) -> BackendFuture<'a, ()> {
            Box::pin(async { unreachable!("cancelled planning should not rename files") })
        }

        fn copy_file<'a>(
            &'a mut self,
            _source: &'a SftpPath,
            _destination: &'a SftpPath,
            _on_progress: &'a mut (dyn FnMut(u64) -> Result<(), CopyInterrupted> + Send),
        ) -> CopyFuture<'a> {
            Box::pin(async { unreachable!("cancelled planning should not copy files") })
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

    async fn spawn_worker<B: TransferBackend + Send + 'static>(
        backend: B,
    ) -> (
        Sender<WorkerCommand>,
        Receiver<SftpTransferEvent>,
        Arc<Mutex<SftpTransferQueueSnapshot>>,
    ) {
        spawn_worker_with_limits(backend, TransferPlanningLimits::default()).await
    }

    async fn spawn_worker_with_limits<B: TransferBackend + Send + 'static>(
        backend: B,
        planning_limits: TransferPlanningLimits,
    ) -> (
        Sender<WorkerCommand>,
        Receiver<SftpTransferEvent>,
        Arc<Mutex<SftpTransferQueueSnapshot>>,
    ) {
        spawn_worker_with_event_capacity(backend, planning_limits, TRANSFER_EVENT_QUEUE_CAPACITY)
            .await
    }

    async fn spawn_worker_with_event_capacity<B: TransferBackend + Send + 'static>(
        backend: B,
        planning_limits: TransferPlanningLimits,
        event_queue_capacity: usize,
    ) -> (
        Sender<WorkerCommand>,
        Receiver<SftpTransferEvent>,
        Arc<Mutex<SftpTransferQueueSnapshot>>,
    ) {
        let (command_sender, command_receiver) = channel(TRANSFER_COMMAND_QUEUE_CAPACITY);
        let (event_sender, event_receiver) = channel(event_queue_capacity);
        let snapshot = Arc::new(Mutex::new(SftpTransferQueueSnapshot::default()));
        let worker_snapshot = Arc::clone(&snapshot);
        tokio::spawn(async move {
            let mut backend = backend;
            run_transfer_worker(
                &mut backend,
                worker_snapshot,
                None,
                command_receiver,
                event_sender,
                planning_limits,
            )
            .await;
        });
        (command_sender, event_receiver, snapshot)
    }

    fn queue_batch(
        command_sender: &Sender<WorkerCommand>,
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
            .try_send(WorkerCommand::EnqueueBatch { batch_id, items })
            .expect("worker should accept queued batch");
        SftpQueuedTransferBatch {
            batch_id,
            transfer_ids,
        }
    }

    fn queue_numbered_batch(
        commands: &Sender<WorkerCommand>,
        number: u64,
        request: SftpTransferRequest,
    ) -> SftpQueuedTransferBatch {
        let batch_id = SftpTransferBatchId(number);
        let id = SftpTransferId(number);
        commands
            .try_send(WorkerCommand::EnqueueBatch {
                batch_id,
                items: vec![QueuedTransferInput { id, request }],
            })
            .unwrap();
        SftpQueuedTransferBatch {
            batch_id,
            transfer_ids: vec![id],
        }
    }

    #[test]
    fn owner_shutdown_interrupts_copy_and_reports_uncertain_partial_output() {
        let root = unique_test_directory("owner-cancel");
        recreate_directory(&root);
        let source = root.join("source.bin");
        let destination = root.join("destination");
        stdfs::create_dir(&destination).unwrap();
        stdfs::write(&source, vec![42; 1024 * 1024]).unwrap();
        let unrelated = destination.join("source.bin.festerm-part");
        stdfs::write(&unrelated, b"not this transfer").unwrap();
        test_runtime().block_on(async {
            let (commands, command_receiver) = channel(TRANSFER_COMMAND_QUEUE_CAPACITY);
            let (events, mut event_receiver) = channel(1);
            let snapshot = Arc::new(Mutex::new(SftpTransferQueueSnapshot::default()));
            let (shutdown, cancellation) = tokio::sync::oneshot::channel();
            let worker = tokio::spawn(run_owned_transfer_worker(
                TestBackend {
                    delay_per_chunk_ms: 10,
                    retain_uncertain_partial: true,
                    ..Default::default()
                },
                Arc::clone(&snapshot),
                None,
                command_receiver,
                events,
                TransferPlanningLimits::default(),
                cancellation,
            ));
            queue_batch(&commands, vec![upload_request(&source, &destination)]);
            loop {
                let event =
                    tokio::time::timeout(std::time::Duration::from_secs(3), event_receiver.recv())
                        .await
                        .unwrap()
                        .unwrap();
                if matches!(event, SftpTransferEvent::ItemProgress { .. }) {
                    break;
                }
            }
            shutdown
                .send(tokio::time::Instant::now() + SFTP_CANCELLATION_CLEANUP_TIMEOUT)
                .unwrap();
            assert!(matches!(
                tokio::time::timeout(std::time::Duration::from_secs(3), worker)
                    .await
                    .unwrap()
                    .unwrap(),
                Err(SftpSessionError::PartialFileOwnershipUnconfirmed { .. })
            ));
            assert!(snapshot.lock().unwrap().items.is_empty());
        });
        assert_eq!(stdfs::read(&unrelated).unwrap(), b"not this transfer");
        assert!(!destination.join("source.bin").exists());
        assert_eq!(stdfs::read_dir(&destination).unwrap().count(), 2);
    }

    #[test]
    fn late_collision_preserves_and_reports_output_without_losing_any_decision() {
        for decision in [
            SftpCollisionDecision::Replace,
            SftpCollisionDecision::Skip,
            SftpCollisionDecision::KeepBoth,
        ] {
            let root = unique_test_directory("late-collision-preserved-output");
            recreate_directory(&root);
            let source = root.join("source.bin");
            let destination = root.join("destination");
            stdfs::create_dir(&destination).unwrap();
            stdfs::write(&source, b"copied contents").unwrap();
            let final_path = destination.join("source.bin");
            let preserved = destination.join("source.bin.festerm-part");
            test_runtime().block_on(async {
                let (commands, mut events, snapshot) = spawn_worker(TestBackend {
                    retain_uncertain_partial: true,
                    late_collision: Some((final_path.clone(), b"late collision".to_vec())),
                    ..Default::default()
                })
                .await;
                let batch = queue_batch(&commands, vec![upload_request(&source, &destination)]);
                let (prior, collision) = tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    next_collision(&mut events),
                )
                .await
                .expect("preserving a temporary must not abandon a late collision");
                assert!(prior.iter().any(|event| matches!(
                    event,
                    SftpTransferEvent::CleanupIncomplete {
                        error: SftpSessionError::PartialFileOwnershipUnconfirmed { path },
                    } if Path::new(path) == preserved.as_path()
                )));
                assert_eq!(
                    snapshot.lock().unwrap().items[0].state,
                    SftpTransferState::AwaitingCollision(collision.id)
                );
                assert_eq!(stdfs::read(&preserved).unwrap(), b"copied contents");
                assert_eq!(stdfs::read(&final_path).unwrap(), b"late collision");
                commands
                    .send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
                        collision_id: collision.id,
                        decision,
                        scope: SftpCollisionScope::ThisItem,
                    }))
                    .await
                    .unwrap();
                let completed = tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    collect_until_batch_finished(&mut events, batch.batch_id),
                )
                .await
                .unwrap();
                assert!(!completed
                    .iter()
                    .any(|event| matches!(event, SftpTransferEvent::ItemFailed { .. })));
                if decision == SftpCollisionDecision::Skip {
                    assert!(completed
                        .iter()
                        .any(|event| matches!(event, SftpTransferEvent::ItemSkipped { .. })));
                } else {
                    let committed = completed
                        .iter()
                        .find_map(|event| match event {
                            SftpTransferEvent::ItemCompleted { destination, .. } => {
                                Some(real_path(destination))
                            }
                            _ => None,
                        })
                        .expect("approved copy must still complete");
                    assert_eq!(stdfs::read(committed).unwrap(), b"copied contents");
                }
            });
            assert_eq!(stdfs::read(&preserved).unwrap(), b"copied contents");
            if decision != SftpCollisionDecision::Replace {
                assert_eq!(stdfs::read(&final_path).unwrap(), b"late collision");
            }
        }
    }

    #[test]
    fn cancelled_copy_reports_preserved_output_and_keeps_cancelled_state() {
        let root = unique_test_directory("cancelled-copy-preserved-output");
        recreate_directory(&root);
        let source = root.join("source.bin");
        let destination = root.join("destination");
        stdfs::create_dir(&destination).unwrap();
        stdfs::write(&source, vec![42; 1024 * 1024]).unwrap();
        test_runtime().block_on(async {
            let (commands, mut events, _) = spawn_worker(TestBackend {
                delay_per_chunk_ms: 10,
                retain_uncertain_partial: true,
                ..Default::default()
            })
            .await;
            let batch = queue_batch(&commands, vec![upload_request(&source, &destination)]);
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while let Some(event) = events.recv().await {
                    if matches!(event, SftpTransferEvent::ItemProgress { .. }) {
                        return;
                    }
                }
                panic!("copy must start before cancellation");
            })
            .await
            .unwrap();
            commands
                .send(WorkerCommand::CancelTransfer(batch.transfer_ids[0]))
                .await
                .unwrap();
            let events = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                collect_until_batch_finished(&mut events, batch.batch_id),
            )
            .await
            .unwrap();
            let notice = events
                .iter()
                .position(|event| matches!(event, SftpTransferEvent::CleanupIncomplete { .. }))
                .expect("retained output must be reported");
            let cancelled = events
                .iter()
                .position(|event| matches!(event, SftpTransferEvent::ItemCancelled { .. }))
                .expect("retained output must not turn cancellation into failure");
            assert!(notice < cancelled);
            assert!(!events
                .iter()
                .any(|event| matches!(event, SftpTransferEvent::ItemFailed { .. })));
        });
        assert!(!destination.join("source.bin").exists());
        let partial = stdfs::read(destination.join("source.bin.festerm-part")).unwrap();
        assert!(!partial.is_empty());
        assert!(partial.len() < 1024 * 1024);
    }

    #[test]
    fn owner_shutdown_during_replace_preserves_the_completed_temporary_for_recovery() {
        let root = unique_test_directory("owner-cancel-replace");
        recreate_directory(&root);
        let source = root.join("source.bin");
        let destination = root.join("destination");
        stdfs::create_dir(&destination).unwrap();
        stdfs::write(&source, b"replacement contents").unwrap();
        stdfs::write(destination.join("source.bin"), b"old contents").unwrap();
        let unrelated = destination.join("unrelated.bin");
        stdfs::write(&unrelated, b"untouched").unwrap();
        test_runtime().block_on(async {
            let removed = Arc::new(Notify::new());
            let (commands, command_receiver) = channel(TRANSFER_COMMAND_QUEUE_CAPACITY);
            let (events, mut event_receiver) = channel(TRANSFER_EVENT_QUEUE_CAPACITY);
            let (shutdown, cancellation) = tokio::sync::oneshot::channel();
            let worker = tokio::spawn(run_owned_transfer_worker(
                TestBackend {
                    replacement_removed: Some(Arc::clone(&removed)),
                    ..Default::default()
                },
                Arc::new(Mutex::new(SftpTransferQueueSnapshot::default())),
                None,
                command_receiver,
                events,
                TransferPlanningLimits::default(),
                cancellation,
            ));
            queue_batch(&commands, vec![upload_request(&source, &destination)]);
            let (_, collision) = next_collision(&mut event_receiver).await;
            commands
                .send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
                    collision_id: collision.id,
                    decision: SftpCollisionDecision::Replace,
                    scope: SftpCollisionScope::ThisItem,
                }))
                .await
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(3), removed.notified())
                .await
                .unwrap();
            for _ in 0..TRANSFER_COMMAND_QUEUE_CAPACITY {
                commands
                    .try_send(WorkerCommand::CancelTransfer(SftpTransferId(1)))
                    .unwrap();
            }
            assert!(commands
                .try_send(WorkerCommand::CancelTransfer(SftpTransferId(1)))
                .is_err());
            shutdown
                .send(tokio::time::Instant::now() + SFTP_CANCELLATION_CLEANUP_TIMEOUT)
                .unwrap();
            assert!(matches!(
                tokio::time::timeout(std::time::Duration::from_secs(3), worker)
                    .await
                    .unwrap()
                    .unwrap(),
                Err(SftpSessionError::TransferCommitInterrupted { .. })
            ));
        });
        assert_eq!(stdfs::read(&source).unwrap(), b"replacement contents");
        assert_eq!(
            stdfs::read(destination.join("source.bin.festerm-part")).unwrap(),
            b"replacement contents"
        );
        assert!(!destination.join("source.bin").exists());
        assert_eq!(stdfs::read(&unrelated).unwrap(), b"untouched");
    }

    #[test]
    fn failed_replace_reports_recovery_output_and_allows_unrelated_queued_work() {
        let root = unique_test_directory("failed-replace-continues");
        recreate_directory(&root);
        let source = root.join("source.bin");
        let other = root.join("other.bin");
        let destination = root.join("destination");
        stdfs::create_dir(&destination).unwrap();
        stdfs::write(&source, b"replacement contents").unwrap();
        stdfs::write(&other, b"unrelated contents").unwrap();
        stdfs::write(destination.join("source.bin"), b"old contents").unwrap();
        test_runtime().block_on(async {
            let (commands, mut events, _) = spawn_worker(TestBackend {
                fail_replace_rename: true,
                ..Default::default()
            })
            .await;
            let batch = queue_batch(&commands, vec![upload_request(&source, &destination)]);
            let (_, collision) = next_collision(&mut events).await;
            commands
                .send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
                    collision_id: collision.id,
                    decision: SftpCollisionDecision::Replace,
                    scope: SftpCollisionScope::ThisItem,
                }))
                .await
                .unwrap();
            let failed_events = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                collect_until_batch_finished(&mut events, batch.batch_id),
            )
            .await
            .unwrap();
            assert!(failed_events.iter().any(|event| matches!(
                event,
                SftpTransferEvent::ItemFailed { reason, .. }
                    if reason.contains("commit could not be confirmed")
                        && reason.contains("source.bin.festerm-part")
            )));
            let next_batch = SftpTransferBatchId(2);
            commands
                .send(WorkerCommand::EnqueueBatch {
                    batch_id: next_batch,
                    items: vec![QueuedTransferInput {
                        id: SftpTransferId(2),
                        request: upload_request(&other, &destination),
                    }],
                })
                .await
                .unwrap();
            let events = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                collect_until_batch_finished(&mut events, next_batch),
            )
            .await
            .unwrap();
            assert!(events.iter().any(|event| matches!(
                event,
                SftpTransferEvent::ItemCompleted { transfer_id, .. }
                    if *transfer_id == SftpTransferId(2)
            )));
        });
        assert_eq!(
            stdfs::read(destination.join("source.bin.festerm-part")).unwrap(),
            b"replacement contents"
        );
        assert_eq!(
            stdfs::read(destination.join("other.bin")).unwrap(),
            b"unrelated contents"
        );
    }

    #[test]
    fn owner_cleanup_is_forced_to_stop_at_the_two_second_deadline() {
        assert_eq!(
            SFTP_CANCELLATION_CLEANUP_TIMEOUT,
            std::time::Duration::from_secs(2)
        );
        test_runtime().block_on(async {
            let (_commands, command_receiver) = channel(TRANSFER_COMMAND_QUEUE_CAPACITY);
            let (events, _event_receiver) = channel(1);
            let (shutdown, cancellation) = tokio::sync::oneshot::channel();
            let start = std::time::Instant::now();
            shutdown
                .send(tokio::time::Instant::now() + SFTP_CANCELLATION_CLEANUP_TIMEOUT)
                .unwrap();
            assert_eq!(
                run_owned_transfer_worker(
                    TestBackend {
                        cleanup_delay_ms: 10_000,
                        ..Default::default()
                    },
                    Arc::new(Mutex::new(SftpTransferQueueSnapshot::default())),
                    None,
                    command_receiver,
                    events,
                    TransferPlanningLimits::default(),
                    cancellation,
                )
                .await,
                Err(SftpSessionError::CancellationCleanupTimedOut)
            );
            assert!(start.elapsed() >= std::time::Duration::from_secs(2));
            assert!(start.elapsed() < std::time::Duration::from_secs(5));
        });
    }

    async fn collect_until_batch_finished(
        receiver: &mut Receiver<SftpTransferEvent>,
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
        receiver: &mut Receiver<SftpTransferEvent>,
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
    fn transfer_capacity_rejections_are_typed_and_deterministic() {
        assert_eq!(
            validate_batch_size(MAX_TRANSFER_BATCH_ITEMS + 1),
            Err(SftpTransferManagerError::BatchTooLarge {
                requested: MAX_TRANSFER_BATCH_ITEMS + 1,
                maximum: MAX_TRANSFER_BATCH_ITEMS,
            })
        );

        let admitted_items = AtomicUsize::new(MAX_QUEUED_TRANSFER_ITEMS);
        assert_eq!(
            reserve_transfer_slots(&admitted_items, 1),
            Err(SftpTransferManagerError::TransferQueueSaturated {
                requested: 1,
                available: 0,
                capacity: MAX_QUEUED_TRANSFER_ITEMS,
            })
        );

        let (command_sender, _command_receiver) = channel(TRANSFER_COMMAND_QUEUE_CAPACITY);
        for batch_id in 0..TRANSFER_COMMAND_QUEUE_CAPACITY {
            command_sender
                .try_send(WorkerCommand::CancelBatch(SftpTransferBatchId(
                    batch_id as u64,
                )))
                .expect("command should fit before the declared capacity");
        }
        assert_eq!(
            try_send_command(
                &command_sender,
                WorkerCommand::CancelBatch(SftpTransferBatchId(
                    TRANSFER_COMMAND_QUEUE_CAPACITY as u64,
                )),
            ),
            Err(SftpTransferManagerError::CommandQueueSaturated {
                capacity: TRANSFER_COMMAND_QUEUE_CAPACITY,
            })
        );
    }

    #[test]
    fn transfer_snapshot_updates_only_affected_rows_and_preserves_request_allocations() {
        let mut state = WorkerState::default();
        let snapshot = Arc::new(Mutex::new(SftpTransferQueueSnapshot::default()));
        state.apply_command(WorkerCommand::EnqueueBatch {
            batch_id: SftpTransferBatchId(1),
            items: (1..=1_000)
                .map(|id| QueuedTransferInput {
                    id: SftpTransferId(id),
                    request: SftpTransferRequest::new(
                        SftpPath::remote(format!("/source/{id}")),
                        SftpPath::local(format!("destination-{id}")),
                    )
                    .unwrap(),
                })
                .collect(),
        });
        state.publish_snapshot(&snapshot);
        assert_eq!(state.snapshot_rows_updated, 1_000);
        let request_allocations = snapshot
            .lock()
            .unwrap()
            .items
            .iter()
            .map(|row| match &row.request.source {
                SftpPath::Remote(path) => path.as_ptr(),
                SftpPath::Local(_) => panic!("snapshot sources must be remote"),
            })
            .collect::<Vec<_>>();
        for bytes in 0..4_096 {
            let id = SftpTransferId(500);
            state.items.get_mut(&id).unwrap().bytes_transferred = bytes;
            state.snapshot_dirty.insert(id);
            state.publish_snapshot(&snapshot);
            assert_eq!(state.snapshot_rows_updated, 1);
        }
        {
            let published = snapshot.lock().unwrap();
            assert_eq!(published.items[499].bytes_transferred, 4_095);
            for (row, allocation) in published.items.iter().zip(&request_allocations) {
                match &row.request.source {
                    SftpPath::Remote(path) => assert_eq!(path.as_ptr(), *allocation),
                    SftpPath::Local(_) => panic!("snapshot source must remain remote"),
                }
            }
        }
        state.publish_snapshot(&snapshot);
        assert_eq!(state.snapshot_rows_updated, 0);
        drop(state.remove_item(SftpTransferId(1)));
        state.publish_snapshot(&snapshot);
        assert_eq!(state.snapshot_rows_updated, 0);
        assert_eq!(snapshot.lock().unwrap().items.len(), 999);
        state
            .items
            .get_mut(&SftpTransferId(500))
            .unwrap()
            .bytes_transferred = 8_000;
        state.snapshot_dirty.insert(SftpTransferId(500));
        state.publish_snapshot(&snapshot);
        let published = snapshot.lock().unwrap();
        assert_eq!(published.items[498].transfer_id, SftpTransferId(500));
        assert_eq!(published.items[498].bytes_transferred, 8_000);
    }

    #[test]
    fn transfer_snapshot_keeps_batch_order_and_updates_resolved_collision() {
        let mut state = WorkerState::default();
        let snapshot = Arc::new(Mutex::new(SftpTransferQueueSnapshot::default()));
        for (batch, id) in [(2, 1), (1, 2)] {
            state.apply_command(WorkerCommand::EnqueueBatch {
                batch_id: SftpTransferBatchId(batch),
                items: vec![QueuedTransferInput {
                    id: SftpTransferId(id),
                    request: SftpTransferRequest::new(
                        SftpPath::remote(format!("/source/{id}")),
                        SftpPath::local(format!("destination-{id}")),
                    )
                    .unwrap(),
                }],
            });
        }
        state.publish_snapshot(&snapshot);
        assert_eq!(
            snapshot
                .lock()
                .unwrap()
                .items
                .iter()
                .map(|row| row.batch_id.raw())
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        let id = SftpTransferId(2);
        state.collisions.insert(SftpCollisionId(1), id);
        state.apply_command(WorkerCommand::ResolveCollision(SftpCollisionResolution {
            collision_id: SftpCollisionId(1),
            decision: SftpCollisionDecision::Skip,
            scope: SftpCollisionScope::ThisItem,
        }));
        state.publish_snapshot(&snapshot);
        assert_eq!(state.snapshot_rows_updated, 1);
        assert_eq!(
            snapshot.lock().unwrap().items[0].state,
            SftpTransferState::Queued
        );
    }

    #[test]
    fn paused_recursive_plan_refuses_aggregate_growth_and_cancel_restores_admission() {
        let root = unique_test_directory("aggregate-paused-plans");
        let local = root.join("local");
        let remote = root.join("remote");
        let first = local.join("first");
        let second = local.join("second");
        recreate_directory(&first);
        recreate_directory(&second);
        recreate_directory(&remote.join("first"));
        stdfs::write(first.join("a.txt"), b"new a").unwrap();
        stdfs::write(first.join("b.txt"), b"new b").unwrap();
        stdfs::write(second.join("only.txt"), b"second").unwrap();
        stdfs::write(remote.join("first").join("a.txt"), b"old a").unwrap();

        test_runtime().block_on(async {
            let work = async {
                let (commands, mut events, _snapshot) = spawn_worker_with_limits(
                    TestBackend::default(),
                    TransferPlanningLimits {
                        max_items: 3,
                        max_memory_proxy_bytes: usize::MAX,
                    },
                )
                .await;
                let first_batch =
                    queue_numbered_batch(&commands, 1, upload_request(&first, &remote));
                let (_, root_collision) = next_collision(&mut events).await;
                commands
                    .try_send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
                        collision_id: root_collision.id,
                        decision: SftpCollisionDecision::MergeFolders,
                        scope: SftpCollisionScope::ThisItem,
                    }))
                    .unwrap();
                let (_, file_collision) = next_collision(&mut events).await;
                assert_eq!(file_collision.transfer_id, first_batch.transfer_ids[0]);

                let second_batch =
                    queue_numbered_batch(&commands, 2, upload_request(&second, &remote));
                let refused =
                    collect_until_batch_finished(&mut events, second_batch.batch_id).await;
                commands
                    .try_send(WorkerCommand::CancelBatch(first_batch.batch_id))
                    .unwrap();
                collect_until_batch_finished(&mut events, first_batch.batch_id).await;

                assert!(
                    refused.iter().any(|event| matches!(
                        event,
                        SftpTransferEvent::ItemFailed { reason, .. }
                            if reason.contains("items limit")
                    )),
                    "paused recursive metadata must share admission with new planning"
                );
                assert!(
                    !remote.join("second").exists(),
                    "refused planning must not materialize its destination"
                );
                let retry_batch =
                    queue_numbered_batch(&commands, 3, upload_request(&second, &remote));
                let retried = collect_until_batch_finished(&mut events, retry_batch.batch_id).await;
                assert!(retried
                    .iter()
                    .any(|event| matches!(event, SftpTransferEvent::ItemCompleted { .. })));
                assert_eq!(
                    stdfs::read(remote.join("second").join("only.txt")).unwrap(),
                    b"second"
                );
                assert_eq!(
                    stdfs::read(remote.join("first").join("a.txt")).unwrap(),
                    b"old a"
                );
            };
            tokio::time::timeout(std::time::Duration::from_secs(10), work)
                .await
                .expect("aggregate planning control must not wait indefinitely");
        });

        stdfs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn multiple_paused_plans_preserve_decisions_and_resume_releases_shared_admission() {
        let root = unique_test_directory("multiple-paused-plans");
        let local = root.join("local");
        let remote = root.join("remote");
        for name in ["first", "second", "third"] {
            let source = local.join(name);
            recreate_directory(&source);
            stdfs::write(source.join("a.txt"), b"new a").unwrap();
            stdfs::write(source.join("b.txt"), b"new b").unwrap();
            if name != "third" {
                let destination = remote.join(name);
                recreate_directory(&destination);
                stdfs::write(destination.join("a.txt"), b"old a").unwrap();
            }
        }
        test_runtime().block_on(async {
            let work = async {
                let (commands, mut events, _snapshot) = spawn_worker_with_limits(
                    TestBackend::default(),
                    TransferPlanningLimits {
                        max_items: 6,
                        max_memory_proxy_bytes: usize::MAX,
                    },
                )
                .await;
                let mut paused = Vec::new();
                for (number, name) in [(1, "first"), (2, "second")] {
                    queue_numbered_batch(
                        &commands,
                        number,
                        upload_request(&local.join(name), &remote),
                    );
                    let (_, root_collision) = next_collision(&mut events).await;
                    commands
                        .try_send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
                            collision_id: root_collision.id,
                            decision: SftpCollisionDecision::MergeFolders,
                            scope: SftpCollisionScope::ThisItem,
                        }))
                        .unwrap();
                    let (_, collision) = next_collision(&mut events).await;
                    paused.push(collision);
                }
                let third = local.join("third");
                let refused = queue_numbered_batch(&commands, 3, upload_request(&third, &remote));
                let failures = collect_until_batch_finished(&mut events, refused.batch_id).await;
                assert!(failures.iter().any(|event| matches!(
                    event,
                    SftpTransferEvent::ItemFailed { reason, .. }
                        if reason.contains("items limit")
                )));
                assert!(!remote.join("third").exists());
                commands
                    .try_send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
                        collision_id: paused[0].id,
                        decision: SftpCollisionDecision::Skip,
                        scope: SftpCollisionScope::ThisItem,
                    }))
                    .unwrap();
                let completed =
                    collect_until_batch_finished(&mut events, SftpTransferBatchId(1)).await;
                assert!(completed.iter().any(|event| matches!(
                    event,
                    SftpTransferEvent::ItemCompleted {
                        skipped_conflicts: 1,
                        ..
                    }
                )));
                let retry = queue_numbered_batch(&commands, 4, upload_request(&third, &remote));
                let completed = collect_until_batch_finished(&mut events, retry.batch_id).await;
                assert!(completed
                    .iter()
                    .any(|event| matches!(event, SftpTransferEvent::ItemCompleted { .. })));
                commands
                    .try_send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
                        collision_id: paused[1].id,
                        decision: SftpCollisionDecision::Replace,
                        scope: SftpCollisionScope::ThisItem,
                    }))
                    .unwrap();
                collect_until_batch_finished(&mut events, SftpTransferBatchId(2)).await;
                assert_eq!(
                    stdfs::read(remote.join("first").join("a.txt")).unwrap(),
                    b"old a"
                );
                assert_eq!(
                    stdfs::read(remote.join("second").join("a.txt")).unwrap(),
                    b"new a"
                );
                assert_eq!(
                    stdfs::read(remote.join("third").join("b.txt")).unwrap(),
                    b"new b"
                );
            };
            tokio::time::timeout(std::time::Duration::from_secs(10), work)
                .await
                .expect("paused-plan resume must not wait indefinitely");
        });
        stdfs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recursive_planning_rejects_item_and_memory_proxy_limit_overflow() {
        let source_root = SftpPathMetadata {
            path: SftpPath::remote("/remote/source"),
            file_type: SftpEntryType::Directory,
            size: None,
            modified_at: None,
            permissions: None,
        };
        let destination_root = SftpPath::local("downloads");
        let snapshot = SftpDirectorySnapshot {
            location: SftpLocation::Remote,
            path: source_root.path.clone(),
            loaded_at: SystemTime::now(),
            entries: vec![SftpDirectoryItem {
                name: "report.txt".to_owned(),
                path: SftpPath::remote("/remote/source/report.txt"),
                file_type: SftpEntryType::File,
                size: Some(5),
                modified_at: None,
                permissions: None,
            }],
        };

        let mut item_backend = SnapshotBackend {
            snapshots: HashMap::from([(source_root.path.display(), snapshot.clone())]),
        };
        let item_error = test_runtime().block_on(WorkerState::default().build_directory_plan(
            &source_root,
            &destination_root,
            false,
            &mut item_backend,
            None,
            TransferPlanningLimits {
                max_items: 1,
                max_memory_proxy_bytes: usize::MAX,
            },
        ));
        assert!(matches!(
            item_error,
            Err(TransferWorkError::PlanningLimitExceeded {
                limit: TransferPlanningLimit::Items,
                observed: 2,
                maximum: 1,
            })
        ));

        let root_proxy_bytes = TRANSFER_PLAN_ITEM_OVERHEAD_BYTES
            + path_memory_proxy_bytes(&source_root.path)
            + path_memory_proxy_bytes(&destination_root);
        let mut memory_backend = SnapshotBackend {
            snapshots: HashMap::from([(source_root.path.display(), snapshot)]),
        };
        let memory_error = test_runtime().block_on(WorkerState::default().build_directory_plan(
            &source_root,
            &destination_root,
            false,
            &mut memory_backend,
            None,
            TransferPlanningLimits {
                max_items: usize::MAX,
                max_memory_proxy_bytes: root_proxy_bytes,
            },
        ));
        assert!(matches!(
            memory_error,
            Err(TransferWorkError::PlanningLimitExceeded {
                limit: TransferPlanningLimit::MemoryProxyBytes,
                maximum,
                ..
            }) if maximum == root_proxy_bytes
        ));
    }

    #[test]
    fn planning_limit_failure_is_terminal_and_observable() {
        let root = unique_test_directory("planning-limit-terminal");
        let local = root.join("local");
        let remote = root.join("remote");
        let source = local.join("source");
        recreate_directory(&source);
        recreate_directory(&remote);
        stdfs::write(source.join("report.txt"), b"report")
            .expect("could not write planning source");

        let events = test_runtime().block_on(async {
            let (command_sender, mut event_receiver, _snapshot) = spawn_worker_with_limits(
                TestBackend {
                    delay_per_chunk_ms: 0,
                    ..Default::default()
                },
                TransferPlanningLimits {
                    max_items: 1,
                    max_memory_proxy_bytes: usize::MAX,
                },
            )
            .await;
            let batch = queue_batch(&command_sender, vec![upload_request(&source, &remote)]);
            collect_until_batch_finished(&mut event_receiver, batch.batch_id).await
        });

        assert!(events.iter().any(|event| matches!(
            event,
            SftpTransferEvent::ItemFailed { reason, .. }
                if reason.contains("items limit") && reason.contains("maximum 1")
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, SftpTransferEvent::BatchFinished { .. })));
        assert!(
            !remote.join("source").exists(),
            "planning failure should occur before destination materialization"
        );

        stdfs::remove_dir_all(root).expect("could not clean planning-limit fixtures");
    }

    #[test]
    fn cancellation_interrupts_slow_recursive_enumeration() {
        test_runtime().block_on(async {
            for cancel_batch in [false, true] {
                let source = SftpPath::local(PathBuf::from(format!(
                    "slow-enumeration-source-{cancel_batch}"
                )));
                let request = SftpTransferRequest::new(
                    source.clone(),
                    SftpPath::remote(format!("/slow-enumeration-destination-{cancel_batch}")),
                )
                .expect("test transfer request should be valid");
                let (enumeration_started, wait_for_enumeration) = oneshot::channel();
                let (command_sender, mut event_receiver, _snapshot) =
                    spawn_worker(SlowEnumerationBackend {
                        source,
                        enumeration_started: Some(enumeration_started),
                    })
                    .await;
                let batch = queue_batch(&command_sender, vec![request]);

                tokio::time::timeout(Duration::from_secs(1), wait_for_enumeration)
                    .await
                    .expect("recursive enumeration should start")
                    .expect("enumeration start signal should be delivered");
                let command = if cancel_batch {
                    WorkerCommand::CancelBatch(batch.batch_id)
                } else {
                    WorkerCommand::CancelTransfer(batch.transfer_ids[0])
                };
                command_sender
                    .try_send(command)
                    .expect("cancellation command should fit");

                let events = tokio::time::timeout(
                    Duration::from_secs(1),
                    collect_until_batch_finished(&mut event_receiver, batch.batch_id),
                )
                .await
                .expect("cancellation should interrupt the awaited enumeration");
                assert!(events.iter().any(|event| matches!(
                    event,
                    SftpTransferEvent::ItemCancelled { transfer_id, .. }
                        if *transfer_id == batch.transfer_ids[0]
                )));
                assert!(!events
                    .iter()
                    .any(|event| matches!(event, SftpTransferEvent::ItemFailed { .. })));
            }
        });
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
                ..Default::default()
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
    fn progress_saturation_coalesces_without_losing_terminal_events() {
        let root = unique_test_directory("progress-coalescing");
        let local = root.join("local");
        let remote = root.join("remote");
        recreate_directory(&local);
        recreate_directory(&remote);
        stdfs::write(local.join("large.bin"), vec![b'x'; 512 * 1024])
            .expect("could not write progress source");

        let events = test_runtime().block_on(async {
            let copy_finished = std::sync::Arc::new(Notify::new());
            let (command_sender, mut receiver, _snapshot) = spawn_worker_with_event_capacity(
                TestBackend {
                    delay_per_chunk_ms: 0,
                    copy_finished: Some(copy_finished.clone()),
                    ..Default::default()
                },
                TransferPlanningLimits::default(),
                1,
            )
            .await;
            let batch = queue_batch(
                &command_sender,
                vec![upload_request(&local.join("large.bin"), &remote)],
            );

            let mut events = Vec::new();
            loop {
                let event = receiver
                    .recv()
                    .await
                    .expect("worker should emit an item-started event");
                let started = matches!(event, SftpTransferEvent::ItemStarted { .. });
                events.push(event);
                if started {
                    break;
                }
            }
            // The bound asserted below only holds while nothing is draining the
            // event channel: once the receiver starts taking events the worker
            // gets a free slot for every one removed, so any progress callback
            // still to come is delivered instead of being coalesced away.
            // Sleeping a fixed interval here and hoping the copy finished first
            // made this test fail intermittently under load, so wait for the
            // backend to tell us the transfer is genuinely done.
            tokio::time::timeout(Duration::from_secs(5), copy_finished.notified())
                .await
                .expect("the test backend should finish copying the progress source");
            events.extend(
                tokio::time::timeout(
                    Duration::from_secs(1),
                    collect_until_batch_finished(&mut receiver, batch.batch_id),
                )
                .await
                .expect("terminal events should survive progress saturation"),
            );
            events
        });

        let progress_events = events
            .iter()
            .filter_map(|event| match event {
                SftpTransferEvent::ItemProgress {
                    bytes_transferred, ..
                } => Some(*bytes_transferred),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            (1..=2).contains(&progress_events.len()),
            "the one-slot channel plus one coalesced pending slot should bound progress"
        );
        assert_eq!(progress_events.last(), Some(&(512 * 1024)));
        assert!(events
            .iter()
            .any(|event| matches!(event, SftpTransferEvent::ItemCompleted { .. })));
        assert!(events
            .iter()
            .any(|event| matches!(event, SftpTransferEvent::BatchFinished { .. })));
        assert_eq!(
            stdfs::read(remote.join("large.bin")).expect("destination should exist"),
            vec![b'x'; 512 * 1024]
        );

        stdfs::remove_dir_all(root).expect("could not clean progress fixtures");
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
                ..Default::default()
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
                .try_send(WorkerCommand::CancelTransfer(batch.transfer_ids[1]))
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
                            .try_send(WorkerCommand::CancelTransfer(batch.transfer_ids[0]))
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
    fn bulk_cancel_refuses_full_admission_then_cancels_all_items_with_one_slot() {
        let mut state = WorkerState::default();
        for batch in 0..MAX_QUEUED_TRANSFER_ITEMS / MAX_TRANSFER_BATCH_ITEMS {
            state.apply_command(WorkerCommand::EnqueueBatch {
                batch_id: SftpTransferBatchId(batch as u64 + 1),
                items: (0..MAX_TRANSFER_BATCH_ITEMS)
                    .map(|index| QueuedTransferInput {
                        id: SftpTransferId((batch * MAX_TRANSFER_BATCH_ITEMS + index) as u64 + 1),
                        request: upload_request(Path::new("fixture.bin"), Path::new("destination")),
                    })
                    .collect(),
            });
        }
        for item in state
            .items
            .values_mut()
            .filter(|item| item.id.raw() % 2 == 0)
        {
            item.state = SftpTransferState::AwaitingCollision(SftpCollisionId(item.id.raw()));
        }
        state.ready.retain(|id| id.raw() % 2 != 0);
        let initial_ready = state.ready.clone();
        let (sender, mut receiver) = channel(TRANSFER_COMMAND_QUEUE_CAPACITY);
        for _ in 0..TRANSFER_COMMAND_QUEUE_CAPACITY {
            try_send_command(
                &sender,
                WorkerCommand::CancelBatch(SftpTransferBatchId(u64::MAX)),
            )
            .unwrap();
        }
        assert!(matches!(
            try_send_command(&sender, WorkerCommand::CancelAllTransfers),
            Err(SftpTransferManagerError::CommandQueueSaturated {
                capacity: TRANSFER_COMMAND_QUEUE_CAPACITY
            })
        ));
        assert_eq!(state.ready, initial_ready);
        assert!(state.items.values().all(|item| !item.cancel_requested));
        receiver.try_recv().unwrap();
        try_send_command(&sender, WorkerCommand::CancelAllTransfers).unwrap();
        while let Ok(command) = receiver.try_recv() {
            state.apply_command(command);
        }
        assert!(state.items.values().all(|item| item.cancel_requested));
        assert_eq!(state.ready.len(), MAX_QUEUED_TRANSFER_ITEMS);
        assert_eq!(
            state.ready.iter().map(|id| id.raw()).collect::<Vec<_>>(),
            (1..=MAX_QUEUED_TRANSFER_ITEMS as u64).collect::<Vec<_>>(),
        );
        state.apply_command(WorkerCommand::CancelAllTransfers);
        assert_eq!(
            state.ready.len(),
            MAX_QUEUED_TRANSFER_ITEMS,
            "repeated cancel must not duplicate ready items"
        );
    }

    #[test]
    fn bulk_cancel_does_not_cancel_work_enqueued_after_the_command() {
        let mut state = WorkerState::default();
        let request = upload_request(Path::new("fixture.bin"), Path::new("destination"));
        state.apply_command(WorkerCommand::EnqueueBatch {
            batch_id: SftpTransferBatchId(1),
            items: vec![QueuedTransferInput {
                id: SftpTransferId(1),
                request: request.clone(),
            }],
        });
        state.apply_command(WorkerCommand::CancelAllTransfers);
        state.apply_command(WorkerCommand::EnqueueBatch {
            batch_id: SftpTransferBatchId(2),
            items: vec![QueuedTransferInput {
                id: SftpTransferId(2),
                request,
            }],
        });
        assert!(state.items[&SftpTransferId(1)].cancel_requested);
        assert!(!state.items[&SftpTransferId(2)].cancel_requested);
        assert_eq!(
            state.ready.iter().map(|id| id.raw()).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn collision_before_start_and_skip_emit_no_item_started() {
        let root = unique_test_directory("prestart-skip");
        let source = root.join("source.bin");
        let destination = root.join("destination");
        recreate_directory(&destination);
        stdfs::write(&source, b"new").unwrap();
        stdfs::write(destination.join("source.bin"), b"old").unwrap();
        test_runtime().block_on(async {
            let (commands, mut receiver, _) = spawn_worker(TestBackend::default()).await;
            let batch = queue_batch(&commands, vec![upload_request(&source, &destination)]);
            let (prior, collision) = next_collision(&mut receiver).await;
            assert_eq!(collision.transfer_id, batch.transfer_ids[0]);
            assert!(!prior.iter().any(|event| matches!(event, SftpTransferEvent::ItemStarted { .. })));
            commands.send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
                collision_id: collision.id,
                decision: SftpCollisionDecision::Skip,
                scope: SftpCollisionScope::ThisItem,
            })).await.unwrap();
            let events = collect_until_batch_finished(&mut receiver, batch.batch_id).await;
            assert!(events.iter().any(|event| matches!(event, SftpTransferEvent::ItemSkipped { transfer_id, .. } if *transfer_id == collision.transfer_id)));
            assert!(!events.iter().any(|event| matches!(event, SftpTransferEvent::ItemStarted { .. })));
        });
        assert_eq!(stdfs::read(destination.join("source.bin")).unwrap(), b"old");
        stdfs::remove_dir_all(root).expect("could not clean pre-start skip fixtures");
    }

    #[test]
    fn bulk_cancel_finishes_more_than_one_command_queue_of_collision_paused_items() {
        let root = unique_test_directory("bulk-cancel-collisions");
        let source = root.join("source.bin");
        let destination = root.join("destination");
        recreate_directory(&destination);
        stdfs::write(&source, b"new").unwrap();
        stdfs::write(destination.join("source.bin"), b"old").unwrap();
        test_runtime().block_on(async {
            let (commands, mut receiver, snapshot) = spawn_worker(TestBackend::default()).await;
            let count = TRANSFER_COMMAND_QUEUE_CAPACITY + 32;
            let batch = queue_batch(
                &commands,
                vec![upload_request(&source, &destination); count],
            );
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                let mut collisions = 0;
                while collisions < count {
                    match receiver.recv().await.expect("worker must remain open") {
                        SftpTransferEvent::Collision(_) => collisions += 1,
                        SftpTransferEvent::BatchQueued { .. } => {}
                        other => panic!("unexpected pre-start event: {other:?}"),
                    }
                }
            })
            .await
            .unwrap();
            try_send_command(&commands, WorkerCommand::CancelAllTransfers).unwrap();
            let events = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                collect_until_batch_finished(&mut receiver, batch.batch_id),
            )
            .await
            .unwrap();
            let cancelled = events
                .iter()
                .filter_map(|event| match event {
                    SftpTransferEvent::ItemCancelled { transfer_id, .. } => Some(*transfer_id),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(cancelled, batch.transfer_ids);
            assert!(!events
                .iter()
                .any(|event| matches!(event, SftpTransferEvent::ItemStarted { .. })));
            assert!(snapshot.lock().unwrap().items.is_empty());
        });
        assert_eq!(stdfs::read(destination.join("source.bin")).unwrap(), b"old");
        stdfs::remove_dir_all(root).expect("could not clean bulk cancellation fixtures");
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
                ..Default::default()
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
                            .try_send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
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
                .try_send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
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
                ..Default::default()
            })
            .await;
            let batch = queue_batch(&command_sender, vec![upload_request(&source_dir, &remote)]);

            let (_, collision) = next_collision(&mut receiver).await;
            command_sender
                .try_send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
                    collision_id: collision.id,
                    decision: SftpCollisionDecision::MergeFolders,
                    scope: SftpCollisionScope::ThisItem,
                }))
                .expect("root folder collision should resolve");

            let (_, nested_collision) = next_collision(&mut receiver).await;
            command_sender
                .try_send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
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
                None,
                TransferPlanningLimits::default(),
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
                ..Default::default()
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
                .try_send(WorkerCommand::CancelTransfer(first_collision.transfer_id))
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
                .try_send(WorkerCommand::ResolveCollision(SftpCollisionResolution {
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
