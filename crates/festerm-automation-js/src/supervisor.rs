use crate::{
    platform,
    protocol::{
        encode_frame, Decoder, HostRequest, HostResult, Limits, Outcome, ParentMessage,
        WorkerFailure, WorkerMessage, CHUNK_BYTES, MAX_PENDING_CALLS,
    },
    snapshot::{ApprovedBundle, BoundParameters, Parameters},
    Error,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc, Arc, Mutex, TryLockError,
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

/// A bounded, coalescing worker-event wake-up shared by application consumers.
#[derive(Clone)]
pub struct EventWake(mpsc::SyncSender<()>);

/// Creates a one-slot wake channel. Drain each worker's events after a wake;
/// the receiver may be bridged to a GUI notifier on a separate thread.
pub fn event_wake_channel() -> (EventWake, mpsc::Receiver<()>) {
    let (sender, receiver) = mpsc::sync_channel(1);
    (EventWake(sender), receiver)
}

impl EventWake {
    /// Signal without waiting; an already-pending signal is sufficient.
    pub fn notify(&self) -> Result<(), Error> {
        match self.0.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(())) => {
                Err(Error::Unavailable("worker event wake receiver closed"))
            }
        }
    }
}

fn deliver_event(
    events: &mpsc::SyncSender<Event>,
    wake: Option<&EventWake>,
    event: Event,
) -> Result<(), Error> {
    events.try_send(event).map_err(|_| Error::Protocol)?;
    if let Some(wake) = wake {
        wake.notify()?;
    }
    Ok(())
}

/// A held application-owned disclosure boundary.
pub trait DisclosurePermit {}

impl<T: ?Sized> DisclosurePermit for T {}

/// Revalidates one result at every physical write to the worker.
///
/// The application must never block in `try_enter`: use a nonblocking lock
/// attempt and return a permit holding the same boundary used to revoke this
/// result's exact run, recipient, privacy state, and all source attachments.
/// Contention returns `Ok(None)`; revoked, expired, or poisoned state returns
/// an error. The guard is not authority: the broker must first authorize the
/// result. A write admitted before revocation may finish afterward; remaining
/// bytes are checked again.
pub trait DisclosureGuard: Send + Sync {
    fn try_enter(&self) -> Result<Option<Box<dyn DisclosurePermit + '_>>, Error>;
}

struct Outbound {
    bytes: Vec<u8>,
    disclosure: Option<Arc<dyn DisclosureGuard>>,
}

impl Outbound {
    fn unguarded(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            disclosure: None,
        }
    }
}

enum Control {
    Admitted(u32),
    Result(u32, HostResult, Arc<dyn DisclosureGuard>),
}

fn stage_control(
    control: Control,
    outstanding: &mut BTreeMap<u32, (HostRequest, bool)>,
    implicitly_admitted: &mut BTreeSet<u32>,
    frames: &mut VecDeque<Outbound>,
) -> Result<(), Error> {
    match control {
        Control::Admitted(id) => {
            if let Some((_, admitted)) = outstanding.get_mut(&id) {
                if *admitted {
                    return Err(Error::Protocol);
                }
                *admitted = true;
                frames.push_back(Outbound::unguarded(encode_frame(
                    &ParentMessage::HostAdmitted { id },
                )?));
            } else if !implicitly_admitted.remove(&id) {
                return Err(Error::Protocol);
            }
        }
        Control::Result(id, result, disclosure) => {
            let (request, admitted) = outstanding.remove(&id).ok_or(Error::Protocol)?;
            result.validate_for(&request)?;
            if !admitted {
                implicitly_admitted.insert(id);
            }
            frames.push_back(Outbound {
                bytes: encode_frame(&ParentMessage::HostResult { id, result })?,
                disclosure: Some(disclosure),
            });
        }
    }
    Ok(())
}

fn write_guarded(
    admission: &Admission,
    frame: &Outbound,
    sent: usize,
    input: &mut impl Write,
) -> Result<Option<std::io::Result<usize>>, Error> {
    let result = admission.try_with_open(|permit| {
        let _disclosure = match &frame.disclosure {
            Some(guard) => match guard.try_enter()? {
                Some(permit) => Some(permit),
                None => return Ok(None),
            },
            None => None,
        };
        permit
            .commit(|_| input.write(&frame.bytes[sent..]))
            .map(Some)
    });
    match result {
        Ok(Some(result)) => result,
        Ok(None) => Ok(None),
        Err(error) => Err(error),
    }
}

static CLEANUP_FAILED: AtomicBool = AtomicBool::new(false);

const ADMISSION_OPEN: u8 = 0;
const ADMISSION_CLAIMED: u8 = 1;
const ADMISSION_CLOSED: u8 = 2;

struct Admission {
    state: AtomicU8,
    cancelled: AtomicBool,
    gate: Mutex<()>,
    deadline: Instant,
}

/// A serialized admission window, not authority to access a session.
pub struct AdmissionPermit<'a> {
    admission: &'a Admission,
}

impl AdmissionPermit<'_> {
    /// Atomically claim this admission against watchdog closure, then perform
    /// the exact prepared broker/controller commit. The claim is the admission
    /// boundary; an effect claimed before closure may execute later. Pass the
    /// supplied time to the broker and do no preparation inside `commit`.
    pub fn commit<T>(self, commit: impl FnOnce(Instant) -> T) -> Result<T, Error> {
        struct Claim<'a>(&'a AtomicU8);
        impl Drop for Claim<'_> {
            fn drop(&mut self) {
                let _ = self.0.compare_exchange(
                    ADMISSION_CLAIMED,
                    ADMISSION_OPEN,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
        }

        self.admission
            .state
            .compare_exchange(
                ADMISSION_OPEN,
                ADMISSION_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| Error::Protocol)?;
        let _claim = Claim(&self.admission.state);
        let now = Instant::now();
        if now >= self.admission.deadline {
            self.admission.stop();
            return Err(Error::Deadline);
        }
        Ok(commit(now))
    }

    pub fn deadline(&self) -> Instant {
        self.admission.deadline
    }
}

impl Admission {
    fn check(&self) -> Result<Instant, Error> {
        if self.state.load(Ordering::Acquire) != ADMISSION_OPEN {
            return Err(Error::Protocol);
        }
        let now = Instant::now();
        if now >= self.deadline {
            self.stop();
            return Err(Error::Deadline);
        }
        Ok(now)
    }

    fn with_open<T>(&self, operation: impl FnOnce(AdmissionPermit<'_>) -> T) -> Result<T, Error> {
        let _guard = self.gate.lock().map_err(|_| {
            self.cancel();
            Error::Protocol
        })?;
        self.check()?;
        Ok(operation(AdmissionPermit { admission: self }))
    }

    fn try_with_open<T>(
        &self,
        operation: impl FnOnce(AdmissionPermit<'_>) -> T,
    ) -> Result<Option<T>, Error> {
        let _guard = match self.gate.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Poisoned(_)) => {
                self.cancel();
                return Err(Error::Protocol);
            }
        };
        self.check()?;
        Ok(Some(operation(AdmissionPermit { admission: self })))
    }

    fn stop(&self) {
        self.state.store(ADMISSION_CLOSED, Ordering::Release);
    }

    fn cancel(&self) {
        self.stop();
        self.cancelled.store(true, Ordering::Release);
    }

    fn close_with<T>(&self, cut: impl FnOnce() -> T) -> T {
        struct SignalCancellation<'a>(&'a AtomicBool);
        impl Drop for SignalCancellation<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        self.stop();
        let _guard = self.gate.lock().unwrap_or_else(|error| error.into_inner());
        let _signal = SignalCancellation(&self.cancelled);
        cut()
    }
}

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
    admission: Arc<Admission>,
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
        let parameters = bundle.bind_parameters(parameters, BTreeMap::new())?;
        Self::spawn_bound(program, bundle, &parameters, limits)
    }

    /// Like `spawn`, with a nonblocking, coalesced event wake-up.
    pub fn spawn_with_wake(
        program: &WorkerProgram,
        bundle: &ApprovedBundle,
        parameters: Parameters,
        limits: Limits,
        wake: EventWake,
    ) -> Result<Self, Error> {
        platform::availability()?;
        let parameters = bundle.bind_parameters(parameters, BTreeMap::new())?;
        Self::spawn_bound_with_wake(program, bundle, &parameters, limits, wake)
    }

    /// Start with target references derived from an immutable invocation.
    /// The caller must still enforce the native grant and broker checks.
    pub fn spawn_bound(
        program: &WorkerProgram,
        bundle: &ApprovedBundle,
        parameters: &BoundParameters,
        limits: Limits,
    ) -> Result<Self, Error> {
        Self::spawn_inner(program, bundle, parameters, limits, None)
    }

    /// Like `spawn_bound`, with a nonblocking, coalesced event wake-up.
    pub fn spawn_bound_with_wake(
        program: &WorkerProgram,
        bundle: &ApprovedBundle,
        parameters: &BoundParameters,
        limits: Limits,
        wake: EventWake,
    ) -> Result<Self, Error> {
        Self::spawn_inner(program, bundle, parameters, limits, Some(wake))
    }

    fn spawn_inner(
        program: &WorkerProgram,
        bundle: &ApprovedBundle,
        parameters: &BoundParameters,
        limits: Limits,
        wake: Option<EventWake>,
    ) -> Result<Self, Error> {
        platform::availability()?;
        if cleanup_failed() {
            return Err(Error::Unavailable("an owned worker could not be reaped"));
        }
        limits.validate()?;
        parameters.validate(bundle)?;
        let source = serde_json::to_vec(bundle.snapshot()).map_err(|_| Error::Snapshot)?;
        let mut frames = VecDeque::new();
        frames.push_back(Outbound::unguarded(encode_frame(&ParentMessage::Begin {
            digest: bundle.digest().into(),
            snapshot_bytes: source.len() as u32,
            parameters: parameters.clone(),
            limits: limits.clone(),
        })?));
        for bytes in source.chunks(CHUNK_BYTES) {
            frames.push_back(Outbound::unguarded(encode_frame(
                &ParentMessage::SourceChunk {
                    bytes: bytes.to_vec(),
                },
            )?));
        }
        frames.push_back(Outbound::unguarded(encode_frame(&ParentMessage::Run {})?));
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
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(limits.run_ms))
            .ok_or(Error::Deadline)?;
        let child = OwnedChild(command.spawn()?, false);
        let (controls, receiver) = mpsc::sync_channel(MAX_PENDING_CALLS);
        // 64 host calls plus a reserved terminal event.
        let (sender, events) = mpsc::sync_channel(MAX_PENDING_CALLS + 1);
        let (startup, ready) = mpsc::sync_channel(1);
        let admission = Arc::new(Admission {
            state: AtomicU8::new(ADMISSION_OPEN),
            cancelled: AtomicBool::new(false),
            gate: Mutex::new(()),
            deadline,
        });
        let open = admission.clone();
        let join = thread::Builder::new()
            .name("automation-js-watchdog".into())
            .spawn(move || {
                supervise(child, frames, limits, receiver, sender, startup, open, wake)
            })?;
        match ready.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Self {
                controls,
                events,
                admission,
                join: Some(join),
            }),
            _ => {
                admission.cancel();
                let result = join.join().map_err(|_| Error::Cleanup)?;
                result?;
                Err(Error::Unavailable(
                    "worker did not qualify before its startup deadline",
                ))
            }
        }
    }

    /// Call only after the trusted broker has authorized this exact result.
    /// The guard must retain its live authorization until every pipe write;
    /// queuing this result neither grants authority nor guarantees delivery.
    /// Do not call supervisor methods while holding the guard's own lock.
    pub fn host_result(
        &self,
        id: u32,
        result: HostResult,
        disclosure: Arc<dyn DisclosureGuard>,
    ) -> Result<(), Error> {
        if let HostResult::Workflow { value } = &result {
            crate::protocol::validate_value(value)?;
        }
        // Reject oversize on the app side before occupying a queue slot.
        encode_frame(&ParentMessage::HostResult {
            id,
            result: result.clone(),
        })?;
        self.with_admission(|_| {
            self.controls
                .try_send(Control::Result(id, result, disclosure))
                .map_err(|_| Error::Protocol)
        })?
    }

    /// Acknowledge only after the broker has admitted this exact operation.
    /// A result sent without a separate acknowledgement also completes this
    /// admission phase. Neither acknowledgement nor queueing promises delivery.
    pub fn host_admitted(&self, id: u32) -> Result<(), Error> {
        self.with_admission(|_| {
            self.controls
                .try_send(Control::Admitted(id))
                .map_err(|_| Error::Protocol)
        })?
    }

    /// Serialize bounded, nonblocking broker/controller admission with
    /// cancellation. Use `AdmissionPermit::commit` at each actual commit,
    /// cap the broker run deadline at `deadline()`, and revalidate the broker,
    /// target and recipients there. Do not call Supervisor methods from the
    /// closure. The broker remains responsible for authorization.
    pub fn with_admission<T>(
        &self,
        operation: impl FnOnce(AdmissionPermit<'_>) -> T,
    ) -> Result<T, Error> {
        self.admission.with_open(operation)
    }

    pub fn deadline(&self) -> Instant {
        self.admission.deadline
    }

    /// Close worker admission and synchronously cut broker authority while
    /// excluding in-flight admissions. The closure must not block or call back
    /// into this supervisor; process cancellation begins after it returns.
    pub fn cancel_with<T>(&self, cut: impl FnOnce() -> T) -> T {
        self.admission.close_with(cut)
    }

    /// Close worker admission. If broker authority exists, use `cancel_with`
    /// so its synchronous cut shares the same admission boundary.
    pub fn cancel(&self) {
        self.admission.cancel();
    }

    pub fn try_event(&self) -> Result<Option<Event>, Error> {
        loop {
            match self.events.try_recv() {
                Ok(Event::HostCall { .. })
                    if self.admission.state.load(Ordering::Acquire) == ADMISSION_CLOSED
                        || Instant::now() >= self.admission.deadline =>
                {
                    continue;
                }
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
    mut frames: VecDeque<Outbound>,
    limits: Limits,
    controls: mpsc::Receiver<Control>,
    events: mpsc::SyncSender<Event>,
    startup: mpsc::SyncSender<Result<(), ()>>,
    admission: Arc<Admission>,
    wake: Option<EventWake>,
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
        let run_deadline = admission.deadline;
        let mut qualified = false;
        let mut sent = 0;
        let mut next_id = 1u32;
        let mut outstanding = BTreeMap::new();
        let mut implicitly_admitted = BTreeSet::new();
        let mut cancelling = None;
        loop {
            let now = Instant::now();
            if !qualified && now >= startup_deadline {
                return Err(Error::Unavailable("worker native qualification timed out"));
            }
            if now >= run_deadline {
                return Err(Error::Deadline);
            }
            let accepting = !admission.cancelled.load(Ordering::Acquire);
            if !accepting && cancelling.is_none() {
                frames.clear();
                frames.push_back(Outbound::unguarded(encode_frame(
                    &ParentMessage::Cancel {},
                )?));
                sent = 0;
                cancelling = Some(now + Duration::from_millis(250));
            }
            if cancelling.is_some_and(|deadline| now >= deadline) {
                return Ok(Outcome::Cancelled {});
            }
            if qualified {
                while let Ok(control) = controls.try_recv() {
                    if cancelling.is_some() {
                        continue;
                    }
                    stage_control(
                        control,
                        &mut outstanding,
                        &mut implicitly_admitted,
                        &mut frames,
                    )?;
                }
                if let Some(frame) = frames.front() {
                    let written = if cancelling.is_some() {
                        Some(input.write(&frame.bytes[sent..]))
                    } else {
                        match write_guarded(&admission, frame, sent, &mut input) {
                            Ok(written) => written,
                            Err(Error::Protocol)
                                if admission.state.load(Ordering::Acquire) == ADMISSION_CLOSED =>
                            {
                                None
                            }
                            Err(error) => return Err(error),
                        }
                    };
                    match written {
                        Some(Ok(0)) => return Err(Error::Protocol),
                        Some(Ok(n)) => {
                            sent += n;
                            if sent == frame.bytes.len() {
                                frames.pop_front();
                                sent = 0;
                            }
                        }
                        Some(Err(e))
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::Interrupted => {}
                        Some(Err(e)) => return Err(e.into()),
                        None => {}
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
                        outstanding.insert(id, (request.clone(), false));
                        deliver_event(&events, wake.as_ref(), Event::HostCall { id, request })?;
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
    admission.stop();
    owned.1 = true;
    let cleanup = reap(child);
    let _ = startup.try_send(Err(()));
    if cleanup.is_err() {
        CLEANUP_FAILED.store(true, Ordering::SeqCst);
        let _ = deliver_event(&events, wake.as_ref(), Event::Failed(Error::Cleanup));
        return Err(Error::Cleanup);
    }
    match result {
        Ok(outcome) => {
            let _ = deliver_event(&events, wake.as_ref(), Event::Finished { outcome });
        }
        Err(error) => {
            let _ = deliver_event(&events, wake.as_ref(), Event::Failed(error));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_wake_coalesces_without_blocking_and_refuses_a_lost_receiver() {
        let (wake, receiver) = event_wake_channel();
        wake.notify().unwrap();
        wake.notify().unwrap();
        assert_eq!(receiver.try_recv(), Ok(()));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        drop(receiver);
        assert!(matches!(wake.notify(), Err(Error::Unavailable(_))));
    }

    struct TestDisclosure;

    impl DisclosureGuard for TestDisclosure {
        fn try_enter(&self) -> Result<Option<Box<dyn DisclosurePermit + '_>>, Error> {
            Ok(Some(Box::new(())))
        }
    }

    struct MutexDisclosure(Arc<Mutex<bool>>);

    impl DisclosureGuard for MutexDisclosure {
        fn try_enter(&self) -> Result<Option<Box<dyn DisclosurePermit + '_>>, Error> {
            match self.0.try_lock() {
                Ok(guard) if *guard => Ok(Some(Box::new(guard))),
                Ok(_) | Err(TryLockError::Poisoned(_)) => Err(Error::DisclosureRevoked),
                Err(TryLockError::WouldBlock) => Ok(None),
            }
        }
    }

    fn disclosure() -> Arc<dyn DisclosureGuard> {
        Arc::new(TestDisclosure)
    }

    fn open_admission() -> Admission {
        Admission {
            state: AtomicU8::new(ADMISSION_OPEN),
            cancelled: AtomicBool::new(false),
            gate: Mutex::new(()),
            deadline: Instant::now() + Duration::from_secs(5),
        }
    }

    #[test]
    fn disclosure_guard_refuses_revoked_and_busy_pipe_writes() {
        let state = Arc::new(Mutex::new(true));
        let frame = Outbound {
            bytes: b"private result".to_vec(),
            disclosure: Some(Arc::new(MutexDisclosure(state.clone()))),
        };
        let admission = open_admission();
        let mut output = Vec::new();
        let mut held = state.lock().unwrap();
        assert!(write_guarded(&admission, &frame, 0, &mut output)
            .unwrap()
            .is_none());
        *held = false;
        drop(held);
        assert!(matches!(
            write_guarded(&admission, &frame, 0, &mut output),
            Err(Error::DisclosureRevoked)
        ));
        assert!(output.is_empty());
    }

    #[test]
    fn disclosure_rechecks_after_a_partial_pipe_write() {
        struct PartialWriter(Vec<u8>);

        impl Write for PartialWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let count = bytes.len().min(3);
                self.0.extend_from_slice(&bytes[..count]);
                Ok(count)
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let state = Arc::new(Mutex::new(true));
        let frame = Outbound {
            bytes: b"private result".to_vec(),
            disclosure: Some(Arc::new(MutexDisclosure(state.clone()))),
        };
        let admission = open_admission();
        let mut output = PartialWriter(Vec::new());
        assert!(matches!(
            write_guarded(&admission, &frame, 0, &mut output),
            Ok(Some(Ok(3)))
        ));
        *state.lock().unwrap() = false;
        assert!(matches!(
            write_guarded(&admission, &frame, 3, &mut output),
            Err(Error::DisclosureRevoked)
        ));
        assert_eq!(output.0, b"pri");
    }

    #[test]
    fn disclosure_write_and_source_revocation_are_serialized() {
        struct PausedWriter {
            entered: mpsc::SyncSender<()>,
            release: mpsc::Receiver<()>,
        }

        impl Write for PausedWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.entered.send(()).unwrap();
                self.release.recv().unwrap();
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let state = Arc::new(Mutex::new(true));
        let frame = Outbound {
            bytes: b"private result".to_vec(),
            disclosure: Some(Arc::new(MutexDisclosure(state.clone()))),
        };
        let (entered, wait_until_entered) = mpsc::sync_channel(0);
        let (release, wait_for_release) = mpsc::sync_channel(0);
        let writer = thread::spawn(move || {
            let mut input = PausedWriter {
                entered,
                release: wait_for_release,
            };
            write_guarded(&open_admission(), &frame, 0, &mut input)
        });
        wait_until_entered
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let (started, wait_for_start) = mpsc::sync_channel(0);
        let (revoked, wait_for_revocation) = mpsc::sync_channel(0);
        let transition = thread::spawn(move || {
            started.send(()).unwrap();
            *state.lock().unwrap() = false;
            revoked.send(()).unwrap();
        });
        wait_for_start.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            wait_for_revocation.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        release.send(()).unwrap();
        assert!(matches!(writer.join().unwrap(), Ok(Some(Ok(14)))));
        wait_for_revocation
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        transition.join().unwrap();
    }

    #[test]
    fn result_before_acknowledgement_is_implicitly_admitted_once() {
        let mut outstanding = BTreeMap::from([(1, (HostRequest::Observe {}, false))]);
        let mut implicitly_admitted = BTreeSet::new();
        let mut frames = VecDeque::new();
        stage_control(
            Control::Result(1, HostResult::Observation { targets: vec![] }, disclosure()),
            &mut outstanding,
            &mut implicitly_admitted,
            &mut frames,
        )
        .unwrap();
        stage_control(
            Control::Admitted(1),
            &mut outstanding,
            &mut implicitly_admitted,
            &mut frames,
        )
        .unwrap();
        assert_eq!(frames.len(), 1);
        assert!(stage_control(
            Control::Admitted(1),
            &mut outstanding,
            &mut implicitly_admitted,
            &mut frames,
        )
        .is_err());
    }

    #[test]
    fn acknowledgement_before_result_remains_ordered_and_nonrepeatable() {
        let mut outstanding = BTreeMap::from([(1, (HostRequest::Observe {}, false))]);
        let mut implicitly_admitted = BTreeSet::new();
        let mut frames = VecDeque::new();
        stage_control(
            Control::Admitted(1),
            &mut outstanding,
            &mut implicitly_admitted,
            &mut frames,
        )
        .unwrap();
        stage_control(
            Control::Result(1, HostResult::Observation { targets: vec![] }, disclosure()),
            &mut outstanding,
            &mut implicitly_admitted,
            &mut frames,
        )
        .unwrap();
        assert_eq!(frames.len(), 2);
        assert!(stage_control(
            Control::Admitted(1),
            &mut outstanding,
            &mut implicitly_admitted,
            &mut frames,
        )
        .is_err());
    }

    #[test]
    fn cancellation_serializes_broker_cut_with_inflight_admission() {
        let admission = Arc::new(Admission {
            state: AtomicU8::new(ADMISSION_OPEN),
            cancelled: AtomicBool::new(false),
            gate: Mutex::new(()),
            deadline: Instant::now() + Duration::from_secs(10),
        });
        let order = Arc::new(Mutex::new(Vec::new()));
        let (entered, wait_until_entered) = mpsc::sync_channel(0);
        let (release, wait_for_release) = mpsc::sync_channel(0);
        let worker_gate = admission.clone();
        let worker_order = order.clone();
        let worker = thread::spawn(move || {
            worker_gate
                .with_open(|permit| {
                    permit
                        .commit(|_| {
                            entered.send(()).unwrap();
                            wait_for_release.recv().unwrap();
                            worker_order.lock().unwrap().push("effect");
                        })
                        .unwrap();
                })
                .unwrap();
        });
        wait_until_entered
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let cut_gate = admission.clone();
        let cut_order = order.clone();
        let cancellation = thread::spawn(move || {
            cut_gate.close_with(|| cut_order.lock().unwrap().push("cut"));
        });
        release.send(()).unwrap();
        worker.join().unwrap();
        cancellation.join().unwrap();
        assert_eq!(*order.lock().unwrap(), ["effect", "cut"]);
        assert!(matches!(admission.with_open(|_| ()), Err(Error::Protocol)));
    }

    #[test]
    fn admission_deadline_closes_without_running_the_operation() {
        let admission = Admission {
            state: AtomicU8::new(ADMISSION_OPEN),
            cancelled: AtomicBool::new(false),
            gate: Mutex::new(()),
            deadline: Instant::now(),
        };
        assert!(matches!(
            admission.with_open(|_| panic!("expired admission ran")),
            Err(Error::Deadline)
        ));
        assert!(matches!(admission.with_open(|_| ()), Err(Error::Protocol)));
    }

    #[test]
    fn broker_commit_rechecks_the_deadline_after_preparation() {
        let admission = Admission {
            state: AtomicU8::new(ADMISSION_OPEN),
            cancelled: AtomicBool::new(false),
            gate: Mutex::new(()),
            deadline: Instant::now() + Duration::from_millis(300),
        };
        let result = admission
            .with_open(|permit| {
                thread::sleep(Duration::from_millis(350));
                permit.commit(|_| panic!("expired broker commit ran"))
            })
            .unwrap();
        assert!(matches!(result, Err(Error::Deadline)));
    }

    #[test]
    fn watchdog_closure_wins_before_the_exact_commit_claim() {
        let admission = Arc::new(Admission {
            state: AtomicU8::new(ADMISSION_OPEN),
            cancelled: AtomicBool::new(false),
            gate: Mutex::new(()),
            deadline: Instant::now() + Duration::from_secs(10),
        });
        let (entered, wait_until_entered) = mpsc::sync_channel(0);
        let (release, wait_for_release) = mpsc::sync_channel(0);
        let worker_gate = admission.clone();
        let worker = thread::spawn(move || {
            worker_gate
                .with_open(|permit| {
                    entered.send(()).unwrap();
                    wait_for_release.recv().unwrap();
                    permit.commit(|_| panic!("closed admission committed"))
                })
                .unwrap()
        });
        wait_until_entered
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        admission.stop();
        release.send(()).unwrap();
        assert!(matches!(worker.join().unwrap(), Err(Error::Protocol)));
    }

    #[cfg(unix)]
    #[test]
    fn watchdog_reaps_at_deadline_while_admission_gate_is_busy() {
        let child = Command::new("/bin/sleep")
            .arg("60")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let admission = Arc::new(Admission {
            state: AtomicU8::new(ADMISSION_OPEN),
            cancelled: AtomicBool::new(false),
            gate: Mutex::new(()),
            deadline: Instant::now() + Duration::from_millis(150),
        });
        let guard = admission.gate.lock().unwrap();
        assert!(matches!(admission.try_with_open(|_| ()), Ok(None)));
        let (controls, receiver) = mpsc::sync_channel(MAX_PENDING_CALLS);
        let (sender, events) = mpsc::sync_channel(MAX_PENDING_CALLS + 1);
        let (startup, _ready) = mpsc::sync_channel(1);
        let worker_gate = admission.clone();
        let watchdog = thread::spawn(move || {
            supervise(
                OwnedChild(child, false),
                VecDeque::new(),
                Limits::default(),
                receiver,
                sender,
                startup,
                worker_gate,
                None,
            )
        });
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(2)),
            Ok(Event::Failed(Error::Deadline))
        ));
        assert!(watchdog.join().unwrap().is_ok());
        drop(controls);
        drop(guard);
    }

    #[cfg(unix)]
    #[test]
    fn watchdog_drains_worker_output_while_admission_gate_is_busy() {
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg(
                r#"printf '\000\000\000\072{"type":"ready","profile":"linux-seccomp-stdio-rlimit-v1"}\000\000\000\071{"type":"hostCall","id":1,"request":{"method":"observe"}}'; exec /bin/sleep 60"#,
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let admission = Arc::new(Admission {
            state: AtomicU8::new(ADMISSION_OPEN),
            cancelled: AtomicBool::new(false),
            gate: Mutex::new(()),
            deadline: Instant::now() + Duration::from_secs(3),
        });
        let guard = admission.gate.lock().unwrap();
        let (controls, receiver) = mpsc::sync_channel(MAX_PENDING_CALLS);
        let (sender, events) = mpsc::sync_channel(MAX_PENDING_CALLS + 1);
        let (startup, ready) = mpsc::sync_channel(1);
        let worker_gate = admission.clone();
        let watchdog = thread::spawn(move || {
            supervise(
                OwnedChild(child, false),
                VecDeque::from([Outbound::unguarded(vec![0])]),
                Limits::default(),
                receiver,
                sender,
                startup,
                worker_gate,
                None,
            )
        });
        let qualified = ready.recv_timeout(Duration::from_secs(2));
        let event = events.recv_timeout(Duration::from_millis(500));
        admission.cancel();
        drop(guard);
        assert!(watchdog.join().unwrap().is_ok());
        drop(controls);
        assert!(matches!(qualified, Ok(Ok(()))));
        assert!(matches!(
            event,
            Ok(Event::HostCall {
                id: 1,
                request: HostRequest::Observe {}
            })
        ));
    }
}
