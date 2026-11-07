use std::{
    fmt,
    fs::File,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
        Arc,
    },
    time::{Duration, Instant},
};

#[cfg(not(any(unix, windows)))]
use std::fs;
#[cfg(not(any(unix, windows)))]
use std::fs::OpenOptions;

use crate::{
    artifact::{ArtifactWriter, CapturedData, CapturedEvent, FinishReason, StreamId},
    RecordingId, MAX_DURATION, MAX_OUTPUT_BYTES, MAX_QUEUE_BYTES, MAX_STREAMS,
};
use festerm_core::{Parser, RecordingCheckpoint, Terminal, MAX_RECORDING_CHECKPOINT_BYTES};

#[cfg(unix)]
#[path = "source_unix.rs"]
mod private_source;
#[cfg(windows)]
#[path = "source_windows.rs"]
mod private_source;

const MAX_QUEUED_EVENTS: usize = 512;
const WRITER_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// A content-free failure to start or continue capture.
#[derive(Debug)]
pub struct CaptureError(String);

impl CaptureError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for CaptureError {}

/// Result of asynchronously finalizing the private source artifact.
#[derive(Debug)]
pub enum Completion {
    Complete {
        path: PathBuf,
        duration: Duration,
        reason: FinishReason,
    },
    Incomplete {
        path: PathBuf,
        duration: Duration,
        reason: FinishReason,
    },
    Failed {
        path: PathBuf,
        message: String,
    },
}

#[derive(Default)]
struct FinishState {
    at_ns: AtomicU64,
    reason: AtomicU8,
}

struct Queued {
    event: CapturedEvent,
    charge: usize,
    budget: Arc<AtomicUsize>,
}

impl Drop for Queued {
    fn drop(&mut self) {
        self.budget.fetch_sub(self.charge, Ordering::AcqRel);
    }
}

/// An empty private source prepared off the UI thread after user consent.
///
/// The host must persist the matching recording manifest before preparation.
/// If preparation is abandoned, call `discard`; a process interruption leaves
/// the empty source and manifest available for explicit recovery.
pub struct PreparedRecording {
    id: RecordingId,
    file: File,
    partial_path: PathBuf,
    final_path: PathBuf,
    #[cfg(any(unix, windows))]
    source: private_source::PrivateSource,
}

impl PreparedRecording {
    pub fn create(private_directory: &Path, id: RecordingId) -> Result<Self, CaptureError> {
        let partial_path = private_directory.join(format!("recording-{id}.partial"));
        let final_path = private_directory.join(format!("recording-{id}.fstrec"));
        #[cfg(any(unix, windows))]
        let (file, source) = private_source::PrivateSource::create(&partial_path, &final_path)?;
        #[cfg(not(any(unix, windows)))]
        let file = create_private_file(&partial_path)?;
        Ok(Self {
            id,
            file,
            partial_path,
            final_path,
            #[cfg(any(unix, windows))]
            source,
        })
    }

    pub const fn id(&self) -> RecordingId {
        self.id
    }

    pub fn discard(self) -> Result<(), CaptureError> {
        let Self {
            file,
            partial_path,
            #[cfg(any(unix, windows))]
            source,
            ..
        } = self;
        drop(file);
        #[cfg(any(unix, windows))]
        let result = source.remove_partial();
        #[cfg(not(any(unix, windows)))]
        let result = fs::remove_file(&partial_path);
        result.map_err(|error| {
            CaptureError::new(format!(
                "unused private recording {} could not be removed: {error}",
                partial_path.display()
            ))
        })
    }

    fn discard_after_error(self, error: CaptureError) -> CaptureError {
        match self.discard() {
            Ok(()) => error,
            Err(cleanup) => CaptureError::new(format!("{error}; {cleanup}")),
        }
    }
}

/// An application-owned, single-writer coordinator for one synchronized capture.
///
/// The host must obtain consent first and supply an existing app-private
/// directory with its platform's recommended backup handling already applied.
/// Only terminal output and explicit visible-state checkpoints are accepted.
pub struct Recorder {
    id: RecordingId,
    started: Instant,
    sender: Option<SyncSender<Queued>>,
    budget: Arc<AtomicUsize>,
    finish: Arc<FinishState>,
    completion: Receiver<Completion>,
    completion_taken: bool,
    partial_path: PathBuf,
    next_stream: usize,
    last_event_at_ns: u64,
    active: [bool; MAX_STREAMS],
    output_parsers: [Parser; MAX_STREAMS],
}

impl Recorder {
    /// Captures the initial visible state before starting the shared clock.
    pub fn start(
        private_directory: &Path,
        terminals: &[&Terminal],
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<(Self, Vec<StreamId>), CaptureError> {
        Self::start_with_id(private_directory, RecordingId::new(), terminals, wake)
    }

    /// Starts a capture with a preallocated ID, allowing the host to durably
    /// save its per-recording settings before terminal content is captured.
    pub fn start_with_id(
        private_directory: &Path,
        id: RecordingId,
        terminals: &[&Terminal],
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<(Self, Vec<StreamId>), CaptureError> {
        Self::start_guarded(private_directory, id, terminals, None, wake)
    }

    pub(crate) fn start_guarded(
        private_directory: &Path,
        id: RecordingId,
        terminals: &[&Terminal],
        admission_closed: Option<&AtomicBool>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<(Self, Vec<StreamId>), CaptureError> {
        let initial = initial_checkpoints(terminals, admission_closed)?;
        let prepared = PreparedRecording::create(private_directory, id)?;
        Self::begin(prepared, initial, admission_closed, wake)
    }

    pub(crate) fn start_prepared_guarded(
        prepared: PreparedRecording,
        terminals: &[&Terminal],
        admission_closed: Option<&AtomicBool>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<(Self, Vec<StreamId>), CaptureError> {
        let initial = match initial_checkpoints(terminals, admission_closed) {
            Ok(initial) => initial,
            Err(error) => return Err(prepared.discard_after_error(error)),
        };
        Self::begin(prepared, initial, admission_closed, wake)
    }

    fn begin(
        prepared: PreparedRecording,
        initial: Vec<RecordingCheckpoint>,
        admission_closed: Option<&AtomicBool>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<(Self, Vec<StreamId>), CaptureError> {
        if let Err(error) = require_admission(admission_closed) {
            return Err(prepared.discard_after_error(error));
        }
        let PreparedRecording {
            id,
            file,
            partial_path,
            final_path,
            #[cfg(any(unix, windows))]
            source,
        } = prepared;
        #[cfg(any(unix, windows))]
        let cleanup = match source.try_clone() {
            Ok(cleanup) => cleanup,
            Err(error) => {
                drop(file);
                let cleanup = source.remove_partial();
                return Err(CaptureError::new(match cleanup {
                    Ok(()) => format!("recording storage could not be retained: {error}"),
                    Err(cleanup) => format!(
                        "recording storage could not be retained: {error}; \
                         the private source also could not be removed: {cleanup}"
                    ),
                }));
            }
        };
        let (sender, receiver) = mpsc::sync_channel(MAX_QUEUED_EVENTS);
        let (completion_sender, completion) = mpsc::channel();
        let budget = Arc::new(AtomicUsize::new(0));
        let finish = Arc::new(FinishState::default());
        let worker_finish = Arc::clone(&finish);
        let worker_partial = partial_path.clone();
        let spawn = std::thread::Builder::new()
            .name("festerm-recording-writer".to_owned())
            .spawn(move || {
                let result = run_writer(
                    file,
                    receiver,
                    &worker_finish,
                    &worker_partial,
                    &final_path,
                    #[cfg(any(unix, windows))]
                    source,
                );
                let _ = completion_sender.send(result);
                wake();
            });
        if let Err(error) = spawn {
            #[cfg(any(unix, windows))]
            cleanup.remove_partial().map_err(|cleanup| {
                CaptureError::new(format!(
                    "recording worker could not start: {error}; \
                     the private source also could not be removed: {cleanup}"
                ))
            })?;
            #[cfg(not(any(unix, windows)))]
            fs::remove_file(&partial_path).map_err(|cleanup| {
                CaptureError::new(format!(
                    "recording worker could not start: {error}; \
                     the private source also could not be removed: {cleanup}"
                ))
            })?;
            return Err(CaptureError::new(format!(
                "recording worker could not start: {error}"
            )));
        }

        let mut recorder = Self {
            id,
            started: Instant::now(),
            sender: Some(sender),
            budget,
            finish,
            completion,
            completion_taken: false,
            partial_path,
            next_stream: 1,
            last_event_at_ns: 0,
            active: [false; MAX_STREAMS],
            output_parsers: std::array::from_fn(|_| Parser::new()),
        };
        let mut streams = Vec::with_capacity(initial.len());
        for checkpoint in initial {
            let stream = StreamId::from_index(recorder.next_stream)
                .expect("initial stream count was checked");
            recorder.enqueue(stream, 0, CapturedData::Start(checkpoint))?;
            recorder.active[stream.index() - 1] = true;
            recorder.next_stream += 1;
            streams.push(stream);
        }
        Ok((recorder, streams))
    }

    /// Opaque identifier shared by the private source and exported filenames.
    pub const fn id(&self) -> RecordingId {
        self.id
    }

    /// Explicitly enrolls a later session on the existing timeline.
    pub fn enroll(&mut self, terminal: &Terminal) -> Result<StreamId, CaptureError> {
        let stream = StreamId::from_index(self.next_stream).ok_or_else(|| {
            self.stop_with_reason(FinishReason::ResourceLimit);
            CaptureError::new("recording reached its session limit")
        })?;
        let at_ns = self.timestamp()?;
        let checkpoint = match checkpoint(terminal) {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                self.stop_with_reason(FinishReason::IntegrityFailure);
                return Err(error);
            }
        };
        self.enqueue(stream, at_ns, CapturedData::Start(checkpoint))?;
        self.active[stream.index() - 1] = true;
        self.next_stream += 1;
        Ok(stream)
    }

    /// Observes output after it has been applied by the live terminal owner.
    ///
    /// `previous_generation` is the screen generation immediately before the
    /// mutation. A screen switch or repeat-character command additionally
    /// requires a fresh visible-state checkpoint, because the initial
    /// projection omits the inactive screen and the last printed character.
    pub fn observe_output(
        &mut self,
        stream: StreamId,
        bytes: &[u8],
        previous_generation: u64,
        terminal_after: &Terminal,
    ) -> Result<(), CaptureError> {
        self.require_active(stream)?;
        if bytes.is_empty() {
            return Ok(());
        }
        if bytes.len() > MAX_OUTPUT_BYTES {
            self.stop_with_reason(FinishReason::ResourceLimit);
            return Err(CaptureError::new(
                "terminal output exceeds the recording event limit",
            ));
        }
        let at_ns = self.timestamp()?;
        let switched = terminal_after.screen_generation() != previous_generation;
        let repeated = self.output_parsers[stream.index() - 1].observe_recording_output(bytes);
        let post_mutation = if switched || repeated {
            match checkpoint(terminal_after) {
                Ok(checkpoint) => Some(checkpoint),
                Err(error) => {
                    self.stop_with_reason(FinishReason::IntegrityFailure);
                    return Err(error);
                }
            }
        } else {
            None
        };
        self.enqueue(stream, at_ns, CapturedData::Output(bytes.to_vec()))?;
        if let Some(checkpoint) = post_mutation {
            self.enqueue(stream, at_ns, CapturedData::Checkpoint(checkpoint))?;
            self.output_parsers[stream.index() - 1] = Parser::new();
        }
        Ok(())
    }

    /// Observes a completed mutation that is not represented by output bytes.
    pub fn observe_checkpoint(
        &mut self,
        stream: StreamId,
        terminal: &Terminal,
    ) -> Result<(), CaptureError> {
        self.observe_snapshot(stream, terminal, false)
    }

    /// Observes an applied resize and its post-reflow visible screen.
    pub fn observe_resize(
        &mut self,
        stream: StreamId,
        terminal: &Terminal,
    ) -> Result<(), CaptureError> {
        self.observe_snapshot(stream, terminal, true)
    }

    fn observe_snapshot(
        &mut self,
        stream: StreamId,
        terminal: &Terminal,
        resized: bool,
    ) -> Result<(), CaptureError> {
        self.require_active(stream)?;
        let at_ns = self.timestamp()?;
        let checkpoint = match checkpoint(terminal) {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                self.stop_with_reason(FinishReason::IntegrityFailure);
                return Err(error);
            }
        };
        let data = if resized {
            CapturedData::Resize(checkpoint)
        } else {
            CapturedData::Checkpoint(checkpoint)
        };
        self.enqueue(stream, at_ns, data)?;
        self.output_parsers[stream.index() - 1] = Parser::new();
        Ok(())
    }

    /// Ends one stream; export holds its last frame until the shared stop.
    pub fn end_stream(&mut self, stream: StreamId) -> Result<(), CaptureError> {
        self.require_active(stream)?;
        let at_ns = self.timestamp()?;
        self.enqueue(stream, at_ns, CapturedData::End)?;
        self.active[stream.index() - 1] = false;
        Ok(())
    }

    /// Enforces the duration limit even when no session is producing output.
    pub fn check_duration(&mut self) -> Result<(), CaptureError> {
        self.timestamp().map(|_| ())
    }

    /// Stops all streams together without waiting for disk I/O.
    pub fn stop(&mut self, reason: FinishReason) -> Result<(), CaptureError> {
        self.stop_at(reason, Instant::now())
    }

    pub(crate) fn stop_at(
        &mut self,
        reason: FinishReason,
        at: Instant,
    ) -> Result<(), CaptureError> {
        if self.sender.is_none() {
            return Err(CaptureError::new("recording is already stopped"));
        }
        let reason = if at.saturating_duration_since(self.started) > MAX_DURATION {
            FinishReason::ResourceLimit
        } else {
            reason
        };
        self.stop_with_reason_at(reason, at);
        Ok(())
    }

    pub fn is_recording(&self) -> bool {
        self.sender.is_some()
    }

    #[cfg(test)]
    pub(crate) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Polls without blocking; the supplied wake callback runs on completion.
    pub fn poll_completion(&mut self) -> Option<Completion> {
        if self.completion_taken {
            return None;
        }
        let completion = match self.completion.try_recv() {
            Ok(completion) => completion,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Completion::Failed {
                path: self.partial_path.clone(),
                message: "recording worker stopped unexpectedly".to_owned(),
            },
        };
        self.sender.take();
        self.completion_taken = true;
        Some(completion)
    }

    fn require_active(&mut self, stream: StreamId) -> Result<(), CaptureError> {
        if self.sender.is_none() {
            return Err(CaptureError::new("recording is not active"));
        }
        if !self.active[stream.index() - 1] {
            self.stop_with_reason(FinishReason::IntegrityFailure);
            return Err(CaptureError::new("recording stream is not active"));
        }
        Ok(())
    }

    fn timestamp(&mut self) -> Result<u64, CaptureError> {
        if self.sender.is_none() {
            return Err(CaptureError::new("recording is not active"));
        }
        let elapsed = self.started.elapsed();
        if elapsed > MAX_DURATION {
            self.stop_with_reason(FinishReason::ResourceLimit);
            return Err(CaptureError::new("recording reached its duration limit"));
        }
        Ok(elapsed.as_nanos() as u64)
    }

    fn enqueue(
        &mut self,
        stream: StreamId,
        at_ns: u64,
        data: CapturedData,
    ) -> Result<(), CaptureError> {
        let charge = match event_charge(&data) {
            Ok(charge) => charge,
            Err(error) => {
                self.stop_with_reason(FinishReason::IntegrityFailure);
                return Err(error);
            }
        };
        let admitted = self
            .budget
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(charge)
                    .filter(|next| *next <= MAX_QUEUE_BYTES)
            })
            .is_ok();
        if !admitted {
            self.stop_with_reason(FinishReason::ResourceLimit);
            return Err(CaptureError::new("recording write queue is full"));
        }
        let queued = Queued {
            event: CapturedEvent {
                at_ns,
                stream,
                data,
            },
            charge,
            budget: Arc::clone(&self.budget),
        };
        let result = match &self.sender {
            Some(sender) => sender.try_send(queued),
            None => {
                drop(queued);
                return Err(CaptureError::new("recording is not active"));
            }
        };
        match result {
            Ok(()) => {
                self.last_event_at_ns = self.last_event_at_ns.max(at_ns);
                Ok(())
            }
            Err(TrySendError::Full(queued)) => {
                drop(queued);
                self.stop_with_reason(FinishReason::ResourceLimit);
                Err(CaptureError::new("recording write queue is full"))
            }
            Err(TrySendError::Disconnected(queued)) => {
                drop(queued);
                self.stop_with_reason(FinishReason::IntegrityFailure);
                Err(CaptureError::new("recording writer is unavailable"))
            }
        }
    }

    fn stop_with_reason(&mut self, reason: FinishReason) {
        self.stop_with_reason_at(reason, Instant::now());
    }

    fn stop_with_reason_at(&mut self, reason: FinishReason, at: Instant) {
        if self.sender.is_some() {
            let at_ns = (at
                .saturating_duration_since(self.started)
                .min(MAX_DURATION)
                .as_nanos() as u64)
                .max(self.last_event_at_ns);
            self.finish.at_ns.store(at_ns, Ordering::Release);
            self.finish.reason.store(reason.code(), Ordering::Release);
            self.sender.take();
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop_with_reason(FinishReason::Interrupted);
    }
}

fn require_admission(admission_closed: Option<&AtomicBool>) -> Result<(), CaptureError> {
    if admission_closed.is_some_and(|closed| closed.load(Ordering::Acquire)) {
        Err(CaptureError::new(
            "privacy monitoring became unavailable before recording started",
        ))
    } else {
        Ok(())
    }
}

fn initial_checkpoints(
    terminals: &[&Terminal],
    admission_closed: Option<&AtomicBool>,
) -> Result<Vec<RecordingCheckpoint>, CaptureError> {
    require_admission(admission_closed)?;
    if terminals.is_empty() || terminals.len() > MAX_STREAMS {
        return Err(CaptureError::new(
            "select between one and sixteen terminal sessions",
        ));
    }
    let mut initial = Vec::with_capacity(terminals.len());
    let mut total_charge = 0usize;
    for terminal in terminals {
        let checkpoint = checkpoint(terminal)?;
        let charge = checkpoint
            .estimated_allocation_bytes()
            .map_err(|error| CaptureError::new(error.to_string()))?
            .checked_add(event_overhead())
            .ok_or_else(|| CaptureError::new("recording queue size overflows"))?;
        total_charge = total_charge
            .checked_add(charge)
            .ok_or_else(|| CaptureError::new("recording queue size overflows"))?;
        if total_charge > MAX_QUEUE_BYTES {
            return Err(CaptureError::new(
                "the selected terminal screens exceed the recording queue limit",
            ));
        }
        initial.push(checkpoint);
    }
    require_admission(admission_closed)?;
    Ok(initial)
}

fn checkpoint(terminal: &Terminal) -> Result<RecordingCheckpoint, CaptureError> {
    terminal
        .recording_checkpoint(MAX_RECORDING_CHECKPOINT_BYTES)
        .map_err(|error| CaptureError::new(format!("visible screen cannot be recorded: {error}")))
}

fn event_overhead() -> usize {
    std::mem::size_of::<CapturedEvent>() + 128
}

fn event_charge(data: &CapturedData) -> Result<usize, CaptureError> {
    let payload = match data {
        CapturedData::Start(checkpoint)
        | CapturedData::Checkpoint(checkpoint)
        | CapturedData::Resize(checkpoint) => checkpoint
            .estimated_allocation_bytes()
            .map_err(|error| CaptureError::new(error.to_string()))?,
        CapturedData::Output(bytes) => bytes.len(),
        CapturedData::End => 0,
    };
    payload
        .checked_add(event_overhead())
        .ok_or_else(|| CaptureError::new("recording queue size overflows"))
}

#[cfg(not(any(unix, windows)))]
fn create_private_file(path: &Path) -> Result<File, CaptureError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    options.open(path).map_err(|error| {
        CaptureError::new(format!("private recording could not be created: {error}"))
    })
}

fn run_writer(
    file: File,
    receiver: Receiver<Queued>,
    finish: &FinishState,
    partial_path: &Path,
    final_path: &Path,
    #[cfg(any(unix, windows))] source: private_source::PrivateSource,
) -> Completion {
    let result = (|| {
        let mut writer = ArtifactWriter::new(file)?;
        let mut flush_at = Instant::now() + WRITER_FLUSH_INTERVAL;
        loop {
            match receiver.recv_timeout(flush_at.saturating_duration_since(Instant::now())) {
                Ok(queued) => writer.write_event(&queued.event)?,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if Instant::now() >= flush_at {
                writer.flush()?;
                flush_at = Instant::now() + WRITER_FLUSH_INTERVAL;
            }
        }
        let at_ns = finish.at_ns.load(Ordering::Acquire);
        let reason = FinishReason::from_code(finish.reason.load(Ordering::Acquire))
            .unwrap_or(FinishReason::Interrupted);
        writer.finish(at_ns, reason)?;
        Ok::<_, crate::ArtifactError>((Duration::from_nanos(at_ns), reason))
    })();

    let (duration, reason) = match result {
        Ok(result) => result,
        Err(error) => {
            return Completion::Failed {
                path: partial_path.to_owned(),
                message: error.to_string(),
            };
        }
    };
    if !reason.is_complete() {
        return Completion::Incomplete {
            path: partial_path.to_owned(),
            duration,
            reason,
        };
    }
    #[cfg(any(unix, windows))]
    if let Err((path, message)) = source.finalize() {
        return Completion::Failed { path, message };
    }
    #[cfg(not(any(unix, windows)))]
    if let Err(error) = fs::hard_link(partial_path, final_path) {
        return Completion::Failed {
            path: partial_path.to_owned(),
            message: format!("recording could not be finalized: {error}"),
        };
    }
    #[cfg(not(any(unix, windows)))]
    if let Err(error) = fs::remove_file(partial_path) {
        return Completion::Failed {
            path: partial_path.to_owned(),
            message: format!(
                "recording was finalized at {} but its temporary name could not be removed: {error}",
                final_path.display()
            ),
        };
    }
    Completion::Complete {
        path: final_path.to_owned(),
        duration,
        reason,
    }
}
