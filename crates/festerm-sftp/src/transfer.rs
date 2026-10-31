//! Queued, cancellable, bounded file transfers between the sandbox and a
//! remote server.
//!
//! Guarantees:
//! * Data is written to a hidden temporary file in the destination directory
//!   (`.festerm-<random>.part`, created exclusively, never through an
//!   existing path or link) and committed by rename only after every byte
//!   arrived and the size matched. A failed, cancelled or disconnected
//!   transfer never leaves a corrupt final file; local temporaries are
//!   removed, remote ones removed best-effort and reported in
//!   [`TransferInfo::leftover_remote_temp`] if the server cannot be reached.
//! * Conflicts are detected before any data moves. `Replace` is an atomic
//!   rename over the target; `KeepBoth` commits without clobbering and retries
//!   the next free `name (N).ext`.
//! * Resources are bounded: queue length, concurrency, in-flight window,
//!   chunk size, retained history, event queue, file size.
//!
//! Not resumable: an interrupted transfer restarts from byte zero (the
//! temporary is discarded rather than trusted).

use crate::{
    cancel::CancelToken,
    conflict::{
        keep_both_name, likely_identical, Conflict, ConflictPolicy, Fingerprint,
        MAX_KEEP_BOTH_ATTEMPTS,
    },
    error::{Result, SftpError},
    local::{LocalFs, PARTIAL_PREFIX, PARTIAL_SUFFIX},
    model::{Entry, EntryKind},
    path,
    remote::{Remote, SftpClient},
};
use russh_sftp::{
    client::{error::Error as RawError, rawsession::SftpResult, RawSftpSession},
    protocol::{Data, FileAttributes, OpenFlags, StatusCode},
};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{oneshot, Semaphore},
    task::{JoinHandle, JoinSet},
};

pub type TransferId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Remote file into a sandbox directory.
    Download,
    /// Sandbox file into a remote directory.
    Upload,
}

/// One file transfer. Directories are not transferred recursively.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferRequest {
    pub direction: Direction,
    /// Download: remote file path. Upload: sandbox-relative file path.
    pub source: String,
    /// Download: sandbox-relative directory. Upload: remote directory.
    pub destination_dir: String,
    /// Destination file name; defaults to the source's own name.
    pub name: Option<String>,
}

impl TransferRequest {
    pub fn download(remote_file: impl Into<String>, local_dir: impl Into<String>) -> Self {
        Self {
            direction: Direction::Download,
            source: remote_file.into(),
            destination_dir: local_dir.into(),
            name: None,
        }
    }

    pub fn upload(local_file: impl Into<String>, remote_dir: impl Into<String>) -> Self {
        Self {
            direction: Direction::Upload,
            source: local_file.into(),
            destination_dir: remote_dir.into(),
            name: None,
        }
    }

    fn target_name(&self) -> Result<String> {
        let name = match (&self.name, self.direction) {
            (Some(name), _) => name.clone(),
            (None, Direction::Download) => path::file_name_remote(&self.source)?,
            (None, Direction::Upload) => path::local_components(&self.source)?
                .last()
                .map(|s| (*s).to_owned())
                .ok_or_else(|| SftpError::InvalidPath("no file name".into()))?,
        };
        path::validate_name(&name)?;
        if name.starts_with(PARTIAL_PREFIX) && name.ends_with(PARTIAL_SUFFIX) {
            return Err(SftpError::InvalidPath(
                "name collides with the transfer temporary file pattern".into(),
            ));
        }
        Ok(name)
    }

    fn validate(&self) -> Result<()> {
        self.target_name()?;
        match self.direction {
            Direction::Download => {
                path::normalize_remote(&self.source)?;
                path::normalize_local(&self.destination_dir)?;
            }
            Direction::Upload => {
                path::normalize_local(&self.source)?;
                path::normalize_remote(&self.destination_dir)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Progress {
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    /// Smoothed transfer rate in bytes per second.
    pub rate_bytes_per_sec: f64,
    pub elapsed: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferState {
    Queued,
    Running,
    /// Waiting for [`TransferManager::resolve_conflict`].
    AwaitingConflict(Conflict),
    Completed,
    /// A conflict policy of `Skip` left the existing target untouched.
    Skipped,
    Failed(SftpError),
    Cancelled,
}

impl TransferState {
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Skipped | Self::Failed(_) | Self::Cancelled
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransferInfo {
    pub id: TransferId,
    pub direction: Direction,
    pub source: String,
    pub destination_dir: String,
    /// Final file name; settles after conflict resolution (`KeepBoth`).
    pub name: String,
    pub state: TransferState,
    pub progress: Progress,
    /// A remote temporary file that could not be removed after a failure.
    pub leftover_remote_temp: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransferEvent {
    StateChanged {
        id: TransferId,
        state: TransferState,
    },
    Progress {
        id: TransferId,
        progress: Progress,
    },
}

#[derive(Clone, Debug)]
pub struct TransferConfig {
    /// Transfers running at once (clamped to 1..=4).
    pub max_concurrent: usize,
    /// Unfinished transfers admitted (running + queued).
    pub max_queued: usize,
    /// Bytes per SFTP read/write request (clamped to 4 KiB..=256 KiB).
    pub chunk_size: usize,
    /// Requests kept in flight per transfer (clamped to 1..=16).
    pub window: usize,
    /// Largest file accepted in either direction.
    pub max_file_size: u64,
    pub progress_interval: Duration,
    /// Finished transfers kept for `snapshot()`.
    pub max_history: usize,
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 2,
            max_queued: 256,
            chunk_size: 64 * 1024,
            window: 8,
            max_file_size: 8 * 1024 * 1024 * 1024,
            progress_interval: Duration::from_millis(100),
            max_history: 256,
        }
    }
}

const MAX_PENDING_EVENTS: usize = 4096;

type EventCallback = Arc<dyn Fn(&TransferEvent) + Send + Sync>;

struct Job {
    info: TransferInfo,
    cancel: CancelToken,
    conflict_reply: Option<oneshot::Sender<ConflictPolicy>>,
}

#[derive(Default)]
struct State {
    next_id: TransferId,
    jobs: HashMap<TransferId, Job>,
    order: VecDeque<TransferId>,
    events: VecDeque<TransferEvent>,
    default_policy: Option<ConflictPolicy>,
}

struct Shared {
    config: TransferConfig,
    remote: Arc<Remote>,
    local: LocalFs,
    state: Mutex<State>,
    semaphore: Semaphore,
    callback: Mutex<Option<EventCallback>>,
}

/// Owns the transfer queue. Poll [`Self::poll_events`] (and/or install an
/// event callback to request a repaint) from the UI thread; nothing here
/// blocks.
pub struct TransferManager {
    client: SftpClient,
    shared: Arc<Shared>,
}

impl TransferManager {
    pub fn new(client: SftpClient, local: LocalFs, mut config: TransferConfig) -> Self {
        config.max_concurrent = config.max_concurrent.clamp(1, 4);
        config.chunk_size = config.chunk_size.clamp(4 * 1024, 256 * 1024);
        config.window = config.window.clamp(1, 16);
        let shared = Arc::new(Shared {
            semaphore: Semaphore::new(config.max_concurrent),
            remote: client.remote(),
            config,
            local,
            state: Mutex::new(State::default()),
            callback: Mutex::new(None),
        });
        Self { client, shared }
    }

    /// Called on the worker thread for every event; keep it tiny (e.g. wake
    /// the UI). Do not call back into the manager from it.
    pub fn set_event_callback(&self, callback: impl Fn(&TransferEvent) + Send + Sync + 'static) {
        if let Ok(mut slot) = self.shared.callback.lock() {
            *slot = Some(Arc::new(callback));
        }
    }

    pub fn enqueue(&self, request: TransferRequest) -> Result<TransferId> {
        request.validate()?;
        let name = request.target_name()?;
        let handle = self.client.handle()?;
        let (id, cancel) = {
            let mut state = lock(&self.shared.state);
            let unfinished = state
                .jobs
                .values()
                .filter(|j| !j.info.state.is_finished())
                .count();
            if unfinished >= self.shared.config.max_queued {
                return Err(SftpError::LimitExceeded("transfer queue is full".into()));
            }
            state.next_id += 1;
            let id = state.next_id;
            let cancel = CancelToken::new();
            state.jobs.insert(
                id,
                Job {
                    info: TransferInfo {
                        id,
                        direction: request.direction,
                        source: request.source.clone(),
                        destination_dir: request.destination_dir.clone(),
                        name,
                        state: TransferState::Queued,
                        progress: Progress::default(),
                        leftover_remote_temp: None,
                    },
                    cancel: cancel.clone(),
                    conflict_reply: None,
                },
            );
            state.order.push_back(id);
            (id, cancel)
        };
        self.shared.emit(
            id,
            TransferEvent::StateChanged {
                id,
                state: TransferState::Queued,
            },
        );
        let shared = Arc::clone(&self.shared);
        handle.spawn(async move { shared.run(id, request, cancel).await });
        Ok(id)
    }

    /// Cancels one transfer (queued, running or waiting on a conflict).
    pub fn cancel(&self, id: TransferId) {
        if let Some(job) = lock(&self.shared.state).jobs.get(&id) {
            job.cancel.cancel();
        }
    }

    pub fn cancel_all(&self) {
        for job in lock(&self.shared.state).jobs.values() {
            job.cancel.cancel();
        }
    }

    /// Answers an `AwaitingConflict` transfer. With `remember_for_remaining`
    /// the policy also resolves every transfer currently waiting and every
    /// later conflict ("use this choice for remaining conflicts").
    pub fn resolve_conflict(
        &self,
        id: TransferId,
        policy: ConflictPolicy,
        remember_for_remaining: bool,
    ) -> Result<()> {
        let mut state = lock(&self.shared.state);
        let waiting = state
            .jobs
            .get(&id)
            .is_some_and(|j| j.conflict_reply.is_some());
        if !waiting {
            return Err(SftpError::InvalidPath(
                "transfer is not waiting on a conflict".into(),
            ));
        }
        if remember_for_remaining {
            state.default_policy = Some(policy);
            for job in state.jobs.values_mut() {
                if let Some(reply) = job.conflict_reply.take() {
                    let _ = reply.send(policy);
                }
            }
        } else if let Some(reply) = state
            .jobs
            .get_mut(&id)
            .and_then(|j| j.conflict_reply.take())
        {
            let _ = reply.send(policy);
        }
        Ok(())
    }

    /// Policy applied to conflicts without asking; `None` asks again.
    pub fn set_default_policy(&self, policy: Option<ConflictPolicy>) {
        lock(&self.shared.state).default_policy = policy;
    }

    pub fn default_policy(&self) -> Option<ConflictPolicy> {
        lock(&self.shared.state).default_policy
    }

    /// Drains pending events (oldest first). Progress events are coalesced
    /// per transfer; `snapshot()` is always authoritative.
    pub fn poll_events(&self) -> Vec<TransferEvent> {
        lock(&self.shared.state).events.drain(..).collect()
    }

    /// All retained transfers, oldest first.
    pub fn snapshot(&self) -> Vec<TransferInfo> {
        let state = lock(&self.shared.state);
        state
            .order
            .iter()
            .filter_map(|id| state.jobs.get(id).map(|j| j.info.clone()))
            .collect()
    }

    pub fn info(&self, id: TransferId) -> Option<TransferInfo> {
        lock(&self.shared.state)
            .jobs
            .get(&id)
            .map(|j| j.info.clone())
    }

    /// Forgets finished transfers.
    pub fn clear_finished(&self) {
        let mut state = lock(&self.shared.state);
        state.jobs.retain(|_, j| !j.info.state.is_finished());
        let State { jobs, order, .. } = &mut *state;
        order.retain(|id| jobs.contains_key(id));
    }
}

impl Drop for TransferManager {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

enum Outcome {
    Done,
    Skipped,
}

enum Commit {
    /// Atomically replace whatever is there.
    Replace,
    /// Never clobber; on collision pick the next `name (N)` (keep-both only).
    NoClobber { retry_numbered: bool },
}

impl Shared {
    fn emit(&self, id: TransferId, event: TransferEvent) {
        {
            let mut state = lock(&self.state);
            let coalesce = matches!(event, TransferEvent::Progress { .. });
            if coalesce {
                if let Some(slot) = state.events.iter_mut().rev().find(
                    |e| matches!(e, TransferEvent::Progress { id: other, .. } if *other == id),
                ) {
                    *slot = event.clone();
                } else {
                    state.events.push_back(event.clone());
                }
            } else {
                state.events.push_back(event.clone());
            }
            while state.events.len() > MAX_PENDING_EVENTS {
                let drop_at = state
                    .events
                    .iter()
                    .position(|e| matches!(e, TransferEvent::Progress { .. }))
                    .unwrap_or(0);
                state.events.remove(drop_at);
            }
        }
        let callback = lock(&self.callback).clone();
        if let Some(callback) = callback {
            callback(&event);
        }
    }

    fn set_state(&self, id: TransferId, new_state: TransferState) {
        {
            let mut state = lock(&self.state);
            let Some(job) = state.jobs.get_mut(&id) else {
                return;
            };
            job.info.state = new_state.clone();
            if new_state.is_finished() {
                job.conflict_reply = None;
            }
        }
        self.emit(
            id,
            TransferEvent::StateChanged {
                id,
                state: new_state,
            },
        );
        self.prune_history();
    }

    fn prune_history(&self) {
        let mut state = lock(&self.state);
        let finished = state
            .jobs
            .values()
            .filter(|j| j.info.state.is_finished())
            .count();
        let mut excess = finished.saturating_sub(self.config.max_history);
        let order: Vec<TransferId> = state.order.iter().copied().collect();
        for id in order {
            if excess == 0 {
                break;
            }
            if state
                .jobs
                .get(&id)
                .is_some_and(|j| j.info.state.is_finished())
            {
                state.jobs.remove(&id);
                excess -= 1;
            }
        }
        let State { jobs, order, .. } = &mut *state;
        order.retain(|id| jobs.contains_key(id));
    }

    fn update_info(&self, id: TransferId, update: impl FnOnce(&mut TransferInfo)) {
        if let Some(job) = lock(&self.state).jobs.get_mut(&id) {
            update(&mut job.info);
        }
    }

    fn report(&self, id: TransferId, progress: Progress) {
        self.update_info(id, |info| info.progress = progress);
        self.emit(id, TransferEvent::Progress { id, progress });
    }

    async fn run(self: Arc<Self>, id: TransferId, request: TransferRequest, cancel: CancelToken) {
        let permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                self.set_state(id, TransferState::Cancelled);
                return;
            }
            permit = self.semaphore.acquire() => match permit {
                Ok(permit) => permit,
                Err(_) => {
                    self.set_state(id, TransferState::Failed(SftpError::Disconnected("queue closed".into())));
                    return;
                }
            },
        };
        self.set_state(id, TransferState::Running);
        let result = match request.direction {
            Direction::Download => self.download(id, &request, &cancel).await,
            Direction::Upload => self.upload(id, &request, &cancel).await,
        };
        drop(permit);
        let state = match result {
            Ok(Outcome::Done) => TransferState::Completed,
            Ok(Outcome::Skipped) => TransferState::Skipped,
            Err(SftpError::Cancelled) => TransferState::Cancelled,
            Err(error) => {
                if error.is_connection_lost() {
                    self.remote
                        .closed
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                TransferState::Failed(error)
            }
        };
        self.set_state(id, state);
    }

    /// Blocks the job until the UI (or a remembered policy) picks a policy.
    async fn policy_for(
        &self,
        id: TransferId,
        conflict: Conflict,
        cancel: &CancelToken,
    ) -> Result<ConflictPolicy> {
        let receiver = {
            let mut state = lock(&self.state);
            if let Some(policy) = state.default_policy {
                return Ok(policy);
            }
            let (sender, receiver) = oneshot::channel();
            let Some(job) = state.jobs.get_mut(&id) else {
                return Err(SftpError::Cancelled);
            };
            job.conflict_reply = Some(sender);
            receiver
        };
        self.set_state(id, TransferState::AwaitingConflict(conflict));
        let policy = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(SftpError::Cancelled),
            policy = receiver => policy.map_err(|_| SftpError::Cancelled)?,
        };
        self.set_state(id, TransferState::Running);
        Ok(policy)
    }

    async fn download(
        &self,
        id: TransferId,
        request: &TransferRequest,
        cancel: &CancelToken,
    ) -> Result<Outcome> {
        let remote_path = path::normalize_remote(&request.source)?;
        let mut name = request.target_name()?;
        let directory = self.local.resolve_dir(&request.destination_dir)?;

        self.remote.check_open()?;
        let source = self.remote.stat_entry(&remote_path).await?;
        match source.effective_kind() {
            EntryKind::File => {}
            EntryKind::Directory => return Err(SftpError::IsADirectory(remote_path)),
            EntryKind::Symlink => {
                return Err(SftpError::NotFound(format!(
                    "{remote_path} (dangling link)"
                )))
            }
            EntryKind::Other => {
                return Err(SftpError::Unsupported(format!(
                    "{remote_path} is not a regular file"
                )))
            }
        }
        let total = source.size;
        if total.is_some_and(|t| t > self.config.max_file_size) {
            return Err(SftpError::LimitExceeded(
                "file exceeds the size limit".into(),
            ));
        }
        self.update_info(id, |i| i.progress.bytes_total = total);

        let incoming = Fingerprint {
            size: total,
            modified: source.modified,
        };
        let local = self.local.clone();
        let probe_dir = directory.clone();
        let mut commit = Commit::NoClobber {
            retry_numbered: false,
        };
        if let Some(existing) = local.existing(&probe_dir, &name)? {
            let suggested = free_name(&name, |candidate| {
                let found = local.existing(&probe_dir, candidate)?;
                Ok(found.is_some())
            })?;
            let conflict = conflict_of(&existing, incoming, &suggested);
            match self.policy_for(id, conflict, cancel).await? {
                ConflictPolicy::Skip => return Ok(Outcome::Skipped),
                ConflictPolicy::Replace => {
                    if existing.kind == EntryKind::Directory {
                        return Err(SftpError::IsADirectory(name));
                    }
                    commit = Commit::Replace;
                }
                ConflictPolicy::KeepBoth => {
                    name = suggested;
                    commit = Commit::NoClobber {
                        retry_numbered: true,
                    };
                }
            }
        }
        self.update_info(id, |i| i.name = name.clone());

        let (temp_path, std_file) = create_local_temp(&directory)?;
        let outcome = self
            .download_into(
                id,
                &remote_path,
                total,
                tokio::fs::File::from_std(std_file),
                cancel,
            )
            .await;
        let finished = match outcome {
            Ok(file) => {
                finalize_local(
                    file,
                    source.modified,
                    &temp_path,
                    &directory,
                    &mut name,
                    &commit,
                )
                .await
            }
            Err(error) => Err(error),
        };
        match finished {
            Ok(final_name) => {
                self.update_info(id, |i| i.name = final_name);
                Ok(Outcome::Done)
            }
            Err(error) => {
                let _ = std::fs::remove_file(&temp_path);
                Err(error)
            }
        }
    }

    async fn download_into(
        &self,
        id: TransferId,
        remote_path: &str,
        total: Option<u64>,
        mut file: tokio::fs::File,
        cancel: &CancelToken,
    ) -> Result<tokio::fs::File> {
        let raw = self.remote.session.raw_session();
        let handle = raw
            .open(remote_path, OpenFlags::READ, FileAttributes::empty())
            .await
            .map_err(|e| SftpError::from_remote(&e, remote_path))?
            .handle;
        let result = self
            .read_loop(id, &raw, &handle, total, &mut file, cancel)
            .await;
        // Best effort; the handle dies with the session anyway.
        let closer = Arc::clone(&raw);
        tokio::spawn(async move {
            let _ = closer.close(handle).await;
        });
        result?;
        Ok(file)
    }

    async fn read_loop(
        &self,
        id: TransferId,
        raw: &Arc<RawSftpSession>,
        handle: &str,
        total: Option<u64>,
        file: &mut tokio::fs::File,
        cancel: &CancelToken,
    ) -> Result<()> {
        let mut meter = Meter::new(self.config.progress_interval);
        let mut chunk = self.config.chunk_size as u64;
        let mut written: u64 = 0;
        let mut next_request: u64 = 0;
        let mut in_flight: VecDeque<JoinHandle<(u64, SftpResult<Data>)>> = VecDeque::new();
        let result = loop {
            while in_flight.len() < self.config.window {
                let length = match total {
                    Some(total) if next_request >= total => break,
                    Some(total) => chunk.min(total - next_request),
                    None => chunk,
                };
                let (raw, handle) = (Arc::clone(raw), handle.to_owned());
                let offset = next_request;
                in_flight.push_back(tokio::spawn(async move {
                    (length, raw.read(handle, offset, length as u32).await)
                }));
                next_request += length;
            }
            let Some(front) = in_flight.pop_front() else {
                break Ok(());
            };
            let (requested, reply) = tokio::select! {
                biased;
                _ = cancel.cancelled() => break Err(SftpError::Cancelled),
                joined = front => match joined {
                    Ok(reply) => reply,
                    Err(_) => break Err(SftpError::Disconnected("read task failed".into())),
                },
            };
            match reply {
                Ok(data) => {
                    let received = data.data.len() as u64;
                    if received == 0 || received > requested {
                        break Err(SftpError::Remote("invalid read reply".into()));
                    }
                    written += received;
                    if written > self.config.max_file_size {
                        break Err(SftpError::LimitExceeded(
                            "file exceeds the size limit".into(),
                        ));
                    }
                    if let Err(error) = file.write_all(&data.data).await {
                        break Err(SftpError::from_io(&error, "write local file"));
                    }
                    if let Some(progress) = meter.tick(written, total, false) {
                        self.report(id, progress);
                    }
                    if received < requested {
                        // Server caps reads below our request: realign the pipeline.
                        abort_all(&mut in_flight);
                        chunk = received.max(4096).min(chunk);
                        next_request = written;
                    }
                }
                Err(RawError::Status(status)) if status.status_code == StatusCode::Eof => {
                    break Ok(());
                }
                Err(RawError::Limited(_)) if chunk > 4096 => {
                    abort_all(&mut in_flight);
                    chunk = (chunk / 2).max(4096);
                    next_request = written;
                }
                Err(error) => break Err(SftpError::from_remote(&error, "read remote file")),
            }
        };
        abort_all(&mut in_flight);
        result?;
        if total.is_some_and(|t| t != written) {
            return Err(SftpError::Remote(
                "remote file changed or ended early during download".into(),
            ));
        }
        file.flush()
            .await
            .map_err(|e| SftpError::from_io(&e, "flush local file"))?;
        if let Some(progress) = meter.tick(written, total.or(Some(written)), true) {
            self.report(id, progress);
        }
        Ok(())
    }

    async fn upload(
        &self,
        id: TransferId,
        request: &TransferRequest,
        cancel: &CancelToken,
    ) -> Result<Outcome> {
        let remote_dir = path::normalize_remote(&request.destination_dir)?;
        let mut name = request.target_name()?;
        let (std_file, metadata) = self.local.open_source(&request.source)?;
        let total = metadata.len();
        if total > self.config.max_file_size {
            return Err(SftpError::LimitExceeded(
                "file exceeds the size limit".into(),
            ));
        }
        self.update_info(id, |i| i.progress.bytes_total = Some(total));
        let modified = crate::local::unix_seconds(metadata.modified());

        self.remote.check_open()?;
        let directory = self.remote.stat_entry(&remote_dir).await?;
        if !directory.is_dir() {
            return Err(SftpError::NotADirectory(remote_dir));
        }
        let incoming = Fingerprint {
            size: Some(total),
            modified,
        };
        let mut commit = Commit::NoClobber {
            retry_numbered: false,
        };
        let target = path::join_remote(&remote_dir, &name)?;
        if let Some(existing) = self.remote_existing(&target).await? {
            let suggested = {
                let mut chosen = None;
                for number in 1..=MAX_KEEP_BOTH_ATTEMPTS {
                    let candidate = keep_both_name(&name, number);
                    if self
                        .remote_existing(&path::join_remote(&remote_dir, &candidate)?)
                        .await?
                        .is_none()
                    {
                        chosen = Some(candidate);
                        break;
                    }
                }
                chosen.ok_or_else(|| SftpError::LimitExceeded("no free keep-both name".into()))?
            };
            let conflict = conflict_of(&existing, incoming, &suggested);
            match self.policy_for(id, conflict, cancel).await? {
                ConflictPolicy::Skip => return Ok(Outcome::Skipped),
                ConflictPolicy::Replace => {
                    if existing.is_dir() {
                        return Err(SftpError::IsADirectory(name));
                    }
                    commit = Commit::Replace;
                }
                ConflictPolicy::KeepBoth => {
                    name = suggested;
                    commit = Commit::NoClobber {
                        retry_numbered: true,
                    };
                }
            }
        }
        self.update_info(id, |i| i.name = name.clone());

        let temp = path::join_remote(&remote_dir, &temp_name())?;
        let raw = self.remote.session.raw_session();
        let handle = raw
            .open(
                temp.as_str(),
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                FileAttributes::empty(),
            )
            .await
            .map_err(|e| SftpError::from_remote(&e, &temp))?
            .handle;
        let sent = self
            .write_loop(
                id,
                &raw,
                &handle,
                total,
                tokio::fs::File::from_std(std_file),
                cancel,
            )
            .await;
        let closed = raw.close(handle).await;
        let result = match (sent, closed) {
            (Ok(()), Ok(_)) => {
                self.finish_upload(&raw, &temp, &remote_dir, &mut name, modified, &commit)
                    .await
            }
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(SftpError::from_remote(&error, "close remote file")),
        };
        match result {
            Ok(final_name) => {
                self.update_info(id, |i| i.name = final_name);
                Ok(Outcome::Done)
            }
            Err(error) => {
                if self.remote.remove_file(&temp).await.is_err() {
                    self.update_info(id, |i| i.leftover_remote_temp = Some(temp.clone()));
                }
                Err(error)
            }
        }
    }

    async fn remote_existing(&self, remote_path: &str) -> Result<Option<Entry>> {
        match self.remote.stat_entry(remote_path).await {
            Ok(entry) => Ok(Some(entry)),
            Err(SftpError::NotFound(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn write_loop(
        &self,
        id: TransferId,
        raw: &Arc<RawSftpSession>,
        handle: &str,
        total: u64,
        mut file: tokio::fs::File,
        cancel: &CancelToken,
    ) -> Result<()> {
        let mut meter = Meter::new(self.config.progress_interval);
        let chunk = self.config.chunk_size;
        let mut in_flight: JoinSet<(u64, SftpResult<russh_sftp::protocol::Status>)> =
            JoinSet::new();
        let mut acknowledged: u64 = 0;
        let mut offset: u64 = 0;
        let result: Result<()> = async {
            loop {
                let mut buffer = vec![0u8; chunk];
                let mut filled = 0;
                while filled < chunk {
                    let read = tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Err(SftpError::Cancelled),
                        read = file.read(&mut buffer[filled..]) => read,
                    }
                    .map_err(|e| SftpError::from_io(&e, "read local file"))?;
                    if read == 0 {
                        break;
                    }
                    filled += read;
                }
                if filled == 0 {
                    break;
                }
                buffer.truncate(filled);
                if offset + filled as u64 > total {
                    return Err(SftpError::Io("local file grew during upload".into()));
                }
                while in_flight.len() >= self.config.window {
                    self.reap_write(
                        id,
                        &mut in_flight,
                        &mut acknowledged,
                        total,
                        &mut meter,
                        cancel,
                    )
                    .await?;
                }
                let (raw, handle) = (Arc::clone(raw), handle.to_owned());
                let at = offset;
                let length = filled as u64;
                in_flight.spawn(async move { (length, raw.write(handle, at, buffer).await) });
                offset += length;
            }
            while !in_flight.is_empty() {
                self.reap_write(
                    id,
                    &mut in_flight,
                    &mut acknowledged,
                    total,
                    &mut meter,
                    cancel,
                )
                .await?;
            }
            Ok(())
        }
        .await;
        in_flight.abort_all();
        result?;
        if offset != total {
            return Err(SftpError::Io("local file changed during upload".into()));
        }
        if let Some(progress) = meter.tick(acknowledged, Some(total), true) {
            self.report(id, progress);
        }
        Ok(())
    }

    async fn reap_write(
        &self,
        id: TransferId,
        in_flight: &mut JoinSet<(u64, SftpResult<russh_sftp::protocol::Status>)>,
        acknowledged: &mut u64,
        total: u64,
        meter: &mut Meter,
        cancel: &CancelToken,
    ) -> Result<()> {
        let joined = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(SftpError::Cancelled),
            joined = in_flight.join_next() => joined,
        };
        match joined {
            Some(Ok((length, Ok(_)))) => {
                *acknowledged += length;
                if let Some(progress) = meter.tick(*acknowledged, Some(total), false) {
                    self.report(id, progress);
                }
                Ok(())
            }
            Some(Ok((_, Err(error)))) => Err(SftpError::from_remote(&error, "write remote file")),
            Some(Err(_)) => Err(SftpError::Disconnected("write task failed".into())),
            None => Ok(()),
        }
    }

    async fn finish_upload(
        &self,
        raw: &Arc<RawSftpSession>,
        temp: &str,
        remote_dir: &str,
        name: &mut String,
        modified: Option<i64>,
        commit: &Commit,
    ) -> Result<String> {
        if let Some(seconds) = modified.and_then(|m| u32::try_from(m).ok()) {
            let attrs = FileAttributes {
                atime: Some(seconds),
                mtime: Some(seconds),
                ..FileAttributes::empty()
            };
            // Cosmetic: servers may refuse; the data is already complete.
            let _ = raw.setstat(temp, attrs).await;
        }
        match commit {
            Commit::Replace => {
                self.remote
                    .rename_replace(temp, &path::join_remote(remote_dir, name)?)
                    .await?;
                Ok(name.clone())
            }
            Commit::NoClobber { retry_numbered } => {
                let base = name.clone();
                let mut attempt = 0;
                loop {
                    let candidate = if attempt == 0 {
                        name.clone()
                    } else {
                        keep_both_name(&base, attempt)
                    };
                    match self
                        .remote
                        .rename_no_replace(temp, &path::join_remote(remote_dir, &candidate)?)
                        .await
                    {
                        Ok(()) => {
                            *name = candidate.clone();
                            return Ok(candidate);
                        }
                        Err(SftpError::AlreadyExists(_))
                            if *retry_numbered && attempt < MAX_KEEP_BOTH_ATTEMPTS =>
                        {
                            attempt += 1;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }
    }
}

fn abort_all<T>(handles: &mut VecDeque<JoinHandle<T>>) {
    for handle in handles.drain(..) {
        handle.abort();
    }
}

fn conflict_of(existing: &Entry, incoming: Fingerprint, suggested: &str) -> Conflict {
    let existing_fingerprint = Fingerprint {
        size: existing.size,
        modified: existing.modified,
    };
    Conflict {
        existing: existing_fingerprint,
        incoming,
        existing_is_directory: existing.is_dir(),
        likely_identical: !existing.is_dir() && likely_identical(existing_fingerprint, incoming),
        suggested_name: suggested.to_owned(),
    }
}

fn free_name(name: &str, mut exists: impl FnMut(&str) -> Result<bool>) -> Result<String> {
    for number in 1..=MAX_KEEP_BOTH_ATTEMPTS {
        let candidate = keep_both_name(name, number);
        if !exists(&candidate)? {
            return Ok(candidate);
        }
    }
    Err(SftpError::LimitExceeded("no free keep-both name".into()))
}

fn temp_name() -> String {
    let mut random = [0u8; 8];
    if getrandom::fill(&mut random).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        random = (nanos as u64 ^ u64::from(std::process::id())).to_le_bytes();
    }
    let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
    format!("{PARTIAL_PREFIX}{hex}{PARTIAL_SUFFIX}")
}

fn create_local_temp(directory: &Path) -> Result<(PathBuf, std::fs::File)> {
    for _ in 0..8 {
        let candidate = directory.join(temp_name());
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(SftpError::from_io(&error, "create temporary file")),
        }
    }
    Err(SftpError::Io(
        "could not create a unique temporary file".into(),
    ))
}

/// Durably finishes the temporary file and commits it under its final name.
async fn finalize_local(
    file: tokio::fs::File,
    modified: Option<i64>,
    temp: &Path,
    directory: &Path,
    name: &mut String,
    commit: &Commit,
) -> Result<String> {
    let std_file = file.into_std().await;
    let (temp, directory, base) = (temp.to_owned(), directory.to_owned(), name.clone());
    let (replace, retry_numbered) = match commit {
        Commit::Replace => (true, false),
        Commit::NoClobber { retry_numbered } => (false, *retry_numbered),
    };
    let committed = tokio::task::spawn_blocking(move || -> Result<String> {
        if let Some(seconds) = modified.and_then(|m| u64::try_from(m).ok()) {
            // Cosmetic: keeps conflict comparisons meaningful.
            let _ = std_file.set_modified(UNIX_EPOCH + Duration::from_secs(seconds));
        }
        std_file
            .sync_all()
            .map_err(|e| SftpError::from_io(&e, "sync local file"))?;
        drop(std_file);
        let mut attempt = 0;
        loop {
            let candidate = if attempt == 0 {
                base.clone()
            } else {
                keep_both_name(&base, attempt)
            };
            let target = directory.join(&candidate);
            let result = if replace {
                commit_replace(&temp, &target)
            } else {
                commit_no_clobber(&temp, &target)
            };
            match result {
                Ok(()) => return Ok(candidate),
                Err(SftpError::AlreadyExists(_))
                    if retry_numbered && attempt < MAX_KEEP_BOTH_ATTEMPTS =>
                {
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .map_err(|_| SftpError::Io("commit task failed".into()))??;
    *name = committed.clone();
    Ok(committed)
}

fn commit_replace(temp: &Path, target: &Path) -> Result<()> {
    if std::fs::symlink_metadata(target).is_ok_and(|m| m.is_dir()) {
        return Err(SftpError::IsADirectory(target.display().to_string()));
    }
    // rename(2) replaces the link itself, never what it points to.
    std::fs::rename(temp, target).map_err(|e| SftpError::from_io(&e, "commit download"))
}

fn commit_no_clobber(temp: &Path, target: &Path) -> Result<()> {
    match std::fs::hard_link(temp, target) {
        Ok(()) => {
            let _ = std::fs::remove_file(temp);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(SftpError::AlreadyExists(target.display().to_string()))
        }
        Err(_) => {
            // No hard-link support on this volume: best-effort exists check.
            if std::fs::symlink_metadata(target).is_ok() {
                return Err(SftpError::AlreadyExists(target.display().to_string()));
            }
            std::fs::rename(temp, target).map_err(|e| SftpError::from_io(&e, "commit download"))
        }
    }
}

struct Meter {
    started: Instant,
    last_emit: Instant,
    last_done: u64,
    rate: f64,
    interval: Duration,
}

impl Meter {
    fn new(interval: Duration) -> Self {
        let now = Instant::now();
        Self {
            started: now,
            last_emit: now,
            last_done: 0,
            rate: 0.0,
            interval,
        }
    }

    fn tick(&mut self, done: u64, total: Option<u64>, force: bool) -> Option<Progress> {
        let now = Instant::now();
        let window = now.duration_since(self.last_emit);
        if !force && window < self.interval {
            return None;
        }
        if window > Duration::ZERO {
            let instant = (done - self.last_done) as f64 / window.as_secs_f64();
            self.rate = if self.rate == 0.0 {
                instant
            } else {
                0.7 * self.rate + 0.3 * instant
            };
        }
        self.last_emit = now;
        self.last_done = done;
        Some(Progress {
            bytes_done: done,
            bytes_total: total,
            rate_bytes_per_sec: self.rate,
            elapsed: now.duration_since(self.started),
        })
    }
}
