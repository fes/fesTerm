use crate::{
    platform,
    protocol::{
        encode_frame, Decoder, HostRequest, HostResult, Limits, Outcome, ParentMessage,
        WorkerFailure, WorkerMessage, CHUNK_BYTES, MAX_PENDING_CALLS,
    },
    snapshot::{ApprovedBundle, Parameters},
    Error,
};
use std::{
    collections::{BTreeMap, VecDeque},
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Only an absolute install-owned executable, never a workflow-selected path.
/// The composition root remains responsible for install/signature provenance
/// and packaging. This verifies canonicality, a regular file, and Unix modes.
#[derive(Clone, Debug)]
pub struct WorkerProgram(PathBuf);

impl WorkerProgram {
    pub fn from_install_owned_absolute(path: &Path) -> Result<Self, Error> {
        if !path.is_absolute() || path.canonicalize()? != path {
            return Err(Error::Protocol);
        }
        let metadata = path.metadata()?;
        if !metadata.is_file() {
            return Err(Error::Protocol);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o022 != 0 {
                return Err(Error::Protocol);
            }
        }
        Ok(Self(path.to_owned()))
    }
}

pub enum Event {
    HostCall {
        id: u32,
        request: HostRequest,
    },
    Finished {
        outcome: Outcome,
    },
    /// Content-free infrastructure failure, distinct from script result data.
    Failed(Error),
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HostCall { id, request } => f
                .debug_struct("HostCall")
                .field("id", id)
                .field("method", &request.method())
                .finish_non_exhaustive(),
            Self::Finished { outcome } => f
                .debug_struct("Finished")
                .field("outcome", outcome)
                .finish(),
            Self::Failed(error) => f.debug_tuple("Failed").field(error).finish(),
        }
    }
}

enum Control {
    Result(u32, HostResult),
}

static CLEANUP_FAILED: AtomicBool = AtomicBool::new(false);

struct OwnedChild(Child, bool);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if (!self.1 && reap(&mut self.0).is_err()) || std::thread::panicking() {
            CLEANUP_FAILED.store(true, Ordering::SeqCst);
        }
    }
}

/// Any unreaped owned worker poisons execution for this application process.
/// The application should surface this condition and require a clean restart.
pub fn cleanup_failed() -> bool {
    CLEANUP_FAILED.load(Ordering::SeqCst)
}

/// Bounded, fresh-process-per-run supervisor. Its watchdog runs independently
/// of the application's event pumping, so a busy GUI cannot disable deadlines.
pub struct Supervisor {
    controls: mpsc::SyncSender<Control>,
    events: mpsc::Receiver<Event>,
    admission: Arc<AtomicBool>,
    join: Option<JoinHandle<Result<(), Error>>>,
}

impl Supervisor {
    pub fn spawn(
        program: &WorkerProgram,
        bundle: &ApprovedBundle,
        parameters: Parameters,
        limits: Limits,
    ) -> Result<Self, Error> {
        platform::availability()?;
        if cleanup_failed() {
            return Err(Error::Unavailable("an owned worker could not be reaped"));
        }
        limits.validate()?;
        bundle.validate_parameters(&parameters)?;
        let source = serde_json::to_vec(bundle.snapshot()).map_err(|_| Error::Snapshot)?;
        let mut frames = VecDeque::new();
        frames.push_back(encode_frame(&ParentMessage::Begin {
            digest: bundle.digest().into(),
            snapshot_bytes: source.len() as u32,
            parameters,
            limits: limits.clone(),
        })?);
        for bytes in source.chunks(CHUNK_BYTES) {
            frames.push_back(encode_frame(&ParentMessage::SourceChunk {
                bytes: bytes.to_vec(),
            })?);
        }
        frames.push_back(encode_frame(&ParentMessage::Run {})?);
        let mut command = Command::new(&program.0);
        command
            .arg("--automation-worker")
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(target_os = "linux")]
        command.env("MALLOC_ARENA_MAX", "1");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = OwnedChild(command.spawn()?, false);
        let (controls, receiver) = mpsc::sync_channel(MAX_PENDING_CALLS);
        // 64 host calls plus a reserved terminal event.
        let (sender, events) = mpsc::sync_channel(MAX_PENDING_CALLS + 1);
        let (startup, ready) = mpsc::sync_channel(1);
        let admission = Arc::new(AtomicBool::new(true));
        let open = admission.clone();
        let join = thread::Builder::new()
            .name("automation-js-watchdog".into())
            .spawn(move || supervise(child, frames, limits, receiver, sender, startup, open))?;
        match ready.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Self {
                controls,
                events,
                admission,
                join: Some(join),
            }),
            _ => {
                admission.store(false, Ordering::SeqCst);
                let result = join.join().map_err(|_| Error::Cleanup)?;
                result?;
                Err(Error::Unavailable(
                    "worker did not qualify before its startup deadline",
                ))
            }
        }
    }

    /// Call only after the trusted broker has checked *this run's* live grant,
    /// data recipients, exact target generation and lifecycle.
    pub fn host_result(&self, id: u32, result: HostResult) -> Result<(), Error> {
        if !self.admission.load(Ordering::SeqCst) {
            return Err(Error::Protocol);
        }
        if let HostResult::Workflow { value } = &result {
            crate::protocol::validate_value(value)?;
        }
        // Reject oversize on the app side before occupying a queue slot.
        encode_frame(&ParentMessage::HostResult {
            id,
            result: result.clone(),
        })?;
        self.controls
            .try_send(Control::Result(id, result))
            .map_err(|_| Error::Protocol)
    }

    /// Closes adapter admission synchronously. The broker must close its own
    /// admission before calling this; already-admitted effects are not undone.
    pub fn cancel(&self) {
        self.admission.store(false, Ordering::SeqCst);
    }

    pub fn try_event(&self) -> Result<Option<Event>, Error> {
        loop {
            match self.events.try_recv() {
                Ok(Event::HostCall { .. }) if !self.admission.load(Ordering::SeqCst) => continue,
                Ok(event) => return Ok(Some(event)),
                Err(mpsc::TryRecvError::Empty) => return Ok(None),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(if cleanup_failed() {
                        Error::Cleanup
                    } else {
                        Error::Protocol
                    })
                }
            }
        }
    }

    /// Bounded worker cleanup is performed by the watchdog, including after
    /// drop. Call explicitly to surface a reap failure and disable further runs.
    pub fn shutdown(mut self) -> Result<(), Error> {
        self.cancel();
        self.join
            .take()
            .ok_or(Error::Cleanup)?
            .join()
            .map_err(|_| Error::Cleanup)?
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.cancel();
        if let Some(join) = self.join.take() {
            // The watchdog bounds cooperation to 250ms and reap to 2s.
            if join.join().is_err() {
                CLEANUP_FAILED.store(true, Ordering::SeqCst);
            }
        }
    }
}

fn reap(child: &mut Child) -> Result<(), Error> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    if let Err(error) = child.kill() {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        return Err(error.into());
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Cleanup);
        }
        thread::sleep(Duration::from_millis(2));
    }
}

#[allow(clippy::too_many_arguments)]
fn supervise(
    mut owned: OwnedChild,
    mut frames: VecDeque<Vec<u8>>,
    limits: Limits,
    controls: mpsc::Receiver<Control>,
    events: mpsc::SyncSender<Event>,
    startup: mpsc::SyncSender<Result<(), ()>>,
    admission: Arc<AtomicBool>,
) -> Result<(), Error> {
    let child = &mut owned.0;
    let result = (|| -> Result<Outcome, Error> {
        let mut input = child.stdin.take().ok_or(Error::Protocol)?;
        let mut output = child.stdout.take().ok_or(Error::Protocol)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            platform::nonblocking(input.as_raw_fd())?;
            platform::nonblocking(output.as_raw_fd())?;
        }
        let mut decoder = Decoder::default();
        let start = Instant::now();
        let startup_deadline = start + Duration::from_secs(2);
        let run_deadline = start + Duration::from_millis(limits.run_ms);
        let mut qualified = false;
        let mut sent = 0;
        let mut next_id = 1u32;
        let mut outstanding = BTreeMap::new();
        let mut cancelling = None;
        loop {
            let now = Instant::now();
            if !qualified && now >= startup_deadline {
                return Err(Error::Unavailable("worker native qualification timed out"));
            }
            if now >= run_deadline {
                return Err(Error::Deadline);
            }
            if !admission.load(Ordering::SeqCst) && cancelling.is_none() {
                frames.clear();
                frames.push_back(encode_frame(&ParentMessage::Cancel {})?);
                sent = 0;
                cancelling = Some(now + Duration::from_millis(250));
            }
            if cancelling.is_some_and(|deadline| now >= deadline) {
                return Ok(Outcome::Cancelled {});
            }
            if qualified {
                while let Ok(Control::Result(id, result)) = controls.try_recv() {
                    if cancelling.is_some() {
                        continue;
                    }
                    let request = outstanding.remove(&id).ok_or(Error::Protocol)?;
                    result.validate_for(&request)?;
                    frames.push_back(encode_frame(&ParentMessage::HostResult { id, result })?);
                }
                if let Some(frame) = frames.front() {
                    match input.write(&frame[sent..]) {
                        Ok(0) => return Err(Error::Protocol),
                        Ok(n) => {
                            sent += n;
                            if sent == frame.len() {
                                frames.pop_front();
                                sent = 0;
                            }
                        }
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            if let Some(message) = decoder.read::<WorkerMessage>(&mut output)? {
                match message {
                    WorkerMessage::Ready { profile }
                        if !qualified && profile == platform::LINUX_PROFILE =>
                    {
                        qualified = true;
                        startup.send(Ok(())).map_err(|_| Error::Protocol)?;
                    }
                    WorkerMessage::HostCall { id, request }
                        if qualified
                            && cancelling.is_none()
                            && id == next_id
                            && id <= u32::from(limits.max_host_calls)
                            && outstanding.len() < MAX_PENDING_CALLS =>
                    {
                        request.validate()?;
                        next_id += 1;
                        outstanding.insert(id, request.clone());
                        events
                            .try_send(Event::HostCall { id, request })
                            .map_err(|_| Error::Protocol)?;
                    }
                    WorkerMessage::Finished { outcome } if qualified => {
                        if cancelling.is_some() {
                            return Ok(Outcome::Cancelled {});
                        }
                        if !outstanding.is_empty() {
                            return Ok(Outcome::UnsettledWork {});
                        }
                        return Ok(outcome);
                    }
                    WorkerMessage::Failed { code } => {
                        return Err(Error::WorkerFailed(match code {
                            WorkerFailure::Unavailable => "native policy unavailable",
                            WorkerFailure::InvalidSnapshot => "invalid source snapshot",
                            WorkerFailure::Protocol => "protocol",
                            WorkerFailure::Io => "I/O",
                            WorkerFailure::Deadline => "deadline",
                        }))
                    }
                    _ => return Err(Error::Protocol),
                }
            }
            if child.try_wait()?.is_some() {
                return Err(Error::Protocol);
            }
            thread::sleep(Duration::from_millis(1));
        }
    })();
    admission.store(false, Ordering::SeqCst);
    owned.1 = true;
    let cleanup = reap(child);
    let _ = startup.try_send(Err(()));
    if cleanup.is_err() {
        CLEANUP_FAILED.store(true, Ordering::SeqCst);
        let _ = events.try_send(Event::Failed(Error::Cleanup));
        return Err(Error::Cleanup);
    }
    match result {
        Ok(outcome) => {
            let _ = events.try_send(Event::Finished { outcome });
        }
        Err(error) => {
            let _ = events.try_send(Event::Failed(error));
        }
    }
    Ok(())
}
