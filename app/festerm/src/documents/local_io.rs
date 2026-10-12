//! Private, bounded local document I/O. Only the registry applies results.

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc::{self, Receiver, SyncSender, TryRecvError},
    Arc, Mutex,
};
use std::thread;
use std::time::Instant;

use festerm_document::{
    Availability, DocumentBounds, DocumentId, SaveError, SavePoint, UnavailableReason,
};

use crate::document_store::{
    self, Generation, LoadFailure, LoadedDocument, LocalSourceAuthority, SaveExpectation,
    SaveFailure, SavedDocument,
};

use super::POLL_INTERVAL;

pub(super) const CAPACITY: usize = 16;

#[derive(Debug, Default)]
pub(super) struct Control {
    pub cancelled: AtomicBool,
    pub force: AtomicBool,
    pub saving: AtomicBool,
}

impl Control {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

#[derive(Debug)]
pub(super) struct Request {
    pub id: DocumentId,
    pub epoch: u64,
    pub revision: u64,
    pub generation: Generation,
    pub authority: LocalSourceAuthority,
    pub path: std::path::PathBuf,
    pub availability: Availability,
    pub load_error: Option<LoadFailure>,
    pub bytes: Option<Vec<u8>>,
    pub save_point: Option<SavePoint>,
    pub check_at: Instant,
    pub save_at: Option<Instant>,
    pub bounds: DocumentBounds,
    pub control: Arc<Control>,
    lease: Option<Lease>,
}

#[derive(Debug)]
struct Lease(Arc<AtomicUsize>);

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub(super) enum Check {
    Current(bool),
    Loaded(Box<LoadedDocument>),
    Failed(LoadFailure),
}

#[derive(Debug)]
pub(super) enum Result {
    Checked(Check),
    Saved(
        std::result::Result<SavedDocument, SaveFailure>,
        Option<Box<LoadedDocument>>,
    ),
}

#[derive(Debug)]
pub(super) struct Completion {
    pub request: Request,
    pub result: Result,
}

#[derive(Debug)]
enum Command {
    Register(Box<Request>),
    Wake,
}

#[derive(Debug)]
pub(super) struct Worker {
    sender: SyncSender<Command>,
    receiver: Receiver<Completion>,
    admitted: Arc<AtomicUsize>,
    capacity_waiting: Arc<AtomicBool>,
    thread: thread::JoinHandle<()>,
}

pub(super) type Executor = dyn Fn(&Request, bool) -> Result + Send + Sync;

impl Worker {
    pub fn start(
        wake: Arc<dyn Fn() + Send + Sync>,
        write_gate: Arc<Mutex<()>>,
    ) -> std::result::Result<Self, SaveError> {
        Self::start_with(wake, write_gate, Arc::new(execute))
    }

    pub fn start_with(
        wake: Arc<dyn Fn() + Send + Sync>,
        write_gate: Arc<Mutex<()>>,
        execute: Arc<Executor>,
    ) -> std::result::Result<Self, SaveError> {
        let (sender, commands) = mpsc::sync_channel(CAPACITY);
        let (results, receiver) = mpsc::sync_channel(CAPACITY);
        let admitted = Arc::new(AtomicUsize::new(0));
        let live = Arc::clone(&admitted);
        let capacity_waiting = Arc::new(AtomicBool::new(false));
        let waiting = Arc::clone(&capacity_waiting);
        let thread = thread::Builder::new()
            .name("festerm-document-io".to_owned())
            .spawn(move || {
                let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(
                        commands,
                        results,
                        Arc::clone(&wake),
                        write_gate,
                        execute,
                        waiting,
                        live,
                    )
                }))
                .is_err();
                if failed {
                    wake();
                }
            })
            .map_err(|_| error("The local document I/O worker could not be started."))?;
        Ok(Self {
            sender,
            receiver,
            admitted,
            capacity_waiting,
            thread,
        })
    }

    pub fn register(&self, build: impl FnOnce() -> Request) -> std::result::Result<(), SaveError> {
        let mut count = self.admitted.load(Ordering::Acquire);
        loop {
            if count >= CAPACITY {
                if !self.capacity_waiting.swap(true, Ordering::AcqRel) {
                    self.force();
                }
                return Err(error("The 16 local background document slots are occupied. Use Refresh or Save explicitly, or close another document."));
            }
            match self.admitted.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => count = current,
            }
        }
        // Reserve before cloning bounded document bytes, not after a refusal.
        let lease = Lease(Arc::clone(&self.admitted));
        let mut request = build();
        request.lease = Some(lease);
        self.sender
            .try_send(Command::Register(Box::new(request)))
            .map_err(|_| {
                if !self.capacity_waiting.swap(true, Ordering::AcqRel) {
                    self.force();
                }
                error("The local document I/O queue could not accept this work. Use Refresh or Save explicitly.")
            })
    }

    pub fn force(&self) {
        // A full queue already wakes the receiver. Force flags survive that
        // coalesced wake; no additional content or unbounded queue is needed.
        let _ = self.sender.try_send(Command::Wake);
    }

    #[cfg(test)]
    pub fn admitted(&self) -> usize {
        self.admitted.load(Ordering::Acquire)
    }

    pub fn drain(&self) -> std::result::Result<Vec<Completion>, SaveError> {
        let mut ready = Vec::new();
        loop {
            match self.receiver.try_recv() {
                Ok(completion) => ready.push(completion),
                Err(TryRecvError::Empty) if !self.thread.is_finished() => return Ok(ready),
                Err(_) if !ready.is_empty() => return Ok(ready),
                Err(_) => {
                    return Err(error(
                        "The local document I/O worker stopped. Use Refresh or Save explicitly.",
                    ))
                }
            }
        }
    }
}

impl Request {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: DocumentId,
        epoch: u64,
        revision: u64,
        generation: Generation,
        authority: LocalSourceAuthority,
        path: std::path::PathBuf,
        availability: Availability,
        load_error: Option<LoadFailure>,
        bytes: Option<Vec<u8>>,
        check_at: Instant,
        save_at: Option<Instant>,
        bounds: DocumentBounds,
        control: Arc<Control>,
    ) -> Self {
        Self {
            id,
            epoch,
            revision,
            generation,
            authority,
            path,
            availability,
            load_error,
            bytes,
            save_point: None,
            check_at,
            save_at,
            bounds,
            control,
            lease: None,
        }
    }
}

fn error(detail: &str) -> SaveError {
    SaveError::new("Background document I/O unavailable", detail)
}

fn run(
    commands: Receiver<Command>,
    results: SyncSender<Completion>,
    wake: Arc<dyn Fn() + Send + Sync>,
    write_gate: Arc<Mutex<()>>,
    execute: Arc<Executor>,
    capacity_waiting: Arc<AtomicBool>,
    admitted: Arc<AtomicUsize>,
) {
    let mut pending: Vec<Request> = Vec::new();
    loop {
        // Slow, continuously due I/O must not starve new commands or shutdown.
        for _ in 0..CAPACITY {
            match commands.try_recv() {
                Ok(Command::Register(request)) => pending.push(*request),
                Ok(Command::Wake) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        pending.retain(|request| !request.control.cancelled.load(Ordering::Acquire));
        if admitted.load(Ordering::Acquire) < CAPACITY
            && capacity_waiting.swap(false, Ordering::AcqRel)
        {
            wake();
        }
        let now = Instant::now();
        let due = pending.iter().position(|request| {
            request.control.force.load(Ordering::Acquire)
                || request.check_at <= now
                || request.save_at.is_some_and(|at| at <= now)
        });
        if let Some(index) = due {
            // Unchanged work returns to the tail; keep its due peers ordered.
            let mut request = pending.remove(index);
            request.control.force.store(false, Ordering::Release);
            let save = request.save_at.is_some_and(|at| at <= now);
            let _write_guard = if save {
                let Ok(guard) = write_gate.lock() else {
                    wake();
                    return;
                };
                if request.control.cancelled.load(Ordering::Acquire) {
                    continue;
                }
                Some(guard)
            } else {
                None
            };
            let result = if save {
                request.control.saving.store(true, Ordering::Release);
                let result = execute(&request, true);
                request.control.saving.store(false, Ordering::Release);
                result
            } else {
                execute(&request, false)
            };
            let unchanged = match &result {
                Result::Checked(Check::Current(current)) => {
                    *current == matches!(request.availability, Availability::Available)
                        && request.load_error.is_none()
                }
                Result::Checked(Check::Failed(error)) => match error {
                    LoadFailure::NotFound => {
                        request.availability
                            == Availability::Unavailable(UnavailableReason::Missing)
                    }
                    LoadFailure::PermissionDenied => {
                        request.availability
                            == Availability::Unavailable(UnavailableReason::PermissionDenied)
                    }
                    LoadFailure::NotAFile | LoadFailure::Refused(_) => {
                        request.availability
                            == Availability::Unavailable(UnavailableReason::NotAFile)
                    }
                    LoadFailure::Unreadable => request.load_error.as_ref() == Some(error),
                },
                _ => false,
            };
            if unchanged {
                request.check_at = Instant::now() + POLL_INTERVAL;
                pending.push(request);
                continue;
            }
            // The permit spans queued, running, and completed work. Therefore
            // all completions fit without blocking, even if the UI is asleep.
            request.bytes = None;
            if results.try_send(Completion { request, result }).is_err() {
                return;
            }
            wake();
            continue;
        }
        let deadline = pending
            .iter()
            .map(|request| {
                request
                    .save_at
                    .map_or(request.check_at, |at| at.min(request.check_at))
            })
            .min();
        let command = match deadline {
            Some(at) => commands.recv_timeout(at.saturating_duration_since(now)),
            None => commands
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match command {
            Ok(Command::Register(request)) => pending.push(*request),
            Ok(Command::Wake) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

pub(super) fn execute(request: &Request, save: bool) -> Result {
    if save {
        let Some(bytes) = &request.bytes else {
            return Result::Saved(Err(SaveFailure::Interrupted), None);
        };
        let result = document_store::save_cancellable(
            &request.path,
            bytes,
            SaveExpectation::Loaded {
                generation: request.generation,
                authority: &request.authority,
            },
            || !request.control.cancelled.load(Ordering::Acquire),
        );
        let conflict = matches!(result, Err(SaveFailure::Conflict(_)))
            .then(|| {
                document_store::load(&request.path, request.bounds)
                    .ok()
                    .map(Box::new)
            })
            .flatten();
        return Result::Saved(result, conflict);
    }
    Result::Checked(
        match document_store::freshness(&request.path, request.generation) {
            document_store::Freshness::Unchanged => {
                Check::Current(document_store::source_authority_identity_is_current(
                    &request.authority,
                    request.generation,
                ))
            }
            document_store::Freshness::Changed(_) => {
                match document_store::load(&request.path, request.bounds) {
                    Ok(loaded) => Check::Loaded(Box::new(loaded)),
                    Err(error) => Check::Failed(error),
                }
            }
            document_store::Freshness::Gone(error) => Check::Failed(error),
        },
    )
}
