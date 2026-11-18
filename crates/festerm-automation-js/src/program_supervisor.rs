#[cfg(windows)]
use crate::windows::Child;
use crate::{
    platform,
    program::{
        encode_program_frame, ApprovedProgram, ProgramBundleMessage, ProgramDeliveryFence,
        ProgramHostError, ProgramHostMessage, ProgramHostRequest, ProgramHostResult,
        ProgramOutcome, ProgramWorkerMessage, Submission, WriteAcceptance, MAX_OUTPUT_QUEUE_BYTES,
        MAX_PENDING_WRITES, MAX_SLEEP_MS, PROGRAM_API_VERSION, PROGRAM_PROTOCOL_VERSION,
    },
    protocol::{encode_frame, Decoder, CHUNK_BYTES, MAX_SAFE_INTEGER},
    Error, ProgramStorageCommit, ProgramStorageCommitRequest, ProgramStorageHost,
    ProgramStorageLoad, UnavailableProgramStorage, WorkerProgram, PROGRAM_CPU_WINDOW,
    PROGRAM_CPU_WINDOW_LIMIT, PROGRAM_HARD_CPU_SECONDS, PROGRAM_TURN_TIMEOUT,
};
#[cfg(not(windows))]
use std::process::Child;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{Read, Write},
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(3);
const OPERATION_TIMEOUT: Duration = PROGRAM_TURN_TIMEOUT;
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);
const HOST_WAIT_TIMEOUT: Duration = Duration::from_millis(MAX_SLEEP_MS as u64);
const CPU_SAMPLE_INTERVAL: Duration = Duration::from_millis(10);
type StartupReply = Result<(Vec<Vec<u8>>, bool), Error>;

struct ProgramCpuBudget {
    previous_cpu: Option<Duration>,
    window_cpu: Duration,
    samples: VecDeque<(Instant, Duration)>,
    next_sample: Instant,
}

impl ProgramCpuBudget {
    fn new(now: Instant) -> Self {
        Self {
            previous_cpu: None,
            window_cpu: Duration::ZERO,
            samples: VecDeque::new(),
            next_sample: now + CPU_SAMPLE_INTERVAL,
        }
    }

    fn sample_due(&self, now: Instant) -> bool {
        now >= self.next_sample
    }

    fn sample(
        &mut self,
        now: Instant,
        query: impl FnOnce() -> Result<Duration, Error>,
    ) -> Result<(), Error> {
        if !self.sample_due(now) {
            return Ok(());
        }
        self.observe(now, query()?)
    }

    fn observe(&mut self, now: Instant, cpu: Duration) -> Result<(), Error> {
        if self.previous_cpu.is_some_and(|previous| cpu < previous) {
            return Err(Error::Unavailable("worker CPU evidence regressed"));
        }
        if cpu >= Duration::from_secs(PROGRAM_HARD_CPU_SECONDS) {
            return Err(Error::WorkerFailed("program hard CPU fallback exhausted"));
        }
        while self.samples.front().is_some_and(|(sampled, _)| {
            now.saturating_duration_since(*sampled) > PROGRAM_CPU_WINDOW
        }) {
            if let Some((_, used)) = self.samples.pop_front() {
                self.window_cpu = self.window_cpu.saturating_sub(used);
            }
        }
        if now < self.next_sample {
            return Ok(());
        }
        let used = cpu.saturating_sub(self.previous_cpu.unwrap_or_default());
        self.previous_cpu = Some(cpu);
        self.next_sample = now + CPU_SAMPLE_INTERVAL;
        if !used.is_zero() {
            self.samples.push_back((now, used));
            self.window_cpu = self.window_cpu.saturating_add(used);
        }
        if self.window_cpu >= PROGRAM_CPU_WINDOW_LIMIT {
            return Err(Error::WorkerFailed("program rolling CPU budget exhausted"));
        }
        Ok(())
    }
}

enum CommandRequest {
    Input {
        sequence: u64,
        bytes: Vec<u8>,
        reply: SyncSender<OperationReply>,
    },
    Resize {
        sequence: u64,
        size: crate::program::TerminalSize,
        reply: SyncSender<OperationReply>,
    },
    Close {
        sequence: u64,
        reply: SyncSender<OperationReply>,
    },
}

type OperationReply = Result<OperationResponse, Error>;

struct OperationResponse {
    output: Vec<Vec<u8>>,
    completed: bool,
}

/// Separately confined, persistent process for one program generation.
///
/// It has no automation host-call authority. The only admitted operations are
/// the terminal events defined by the terminal-program protocol.
pub struct ProgramSupervisor {
    generation: u64,
    next_sequence: u64,
    startup_output: Vec<Vec<u8>>,
    completed: bool,
    commands: SyncSender<CommandRequest>,
    forced: Arc<AtomicBool>,
    fence: Arc<dyn ProgramDeliveryFence>,
    join: Mutex<Option<JoinHandle<Result<(), Error>>>>,
}

impl ProgramSupervisor {
    pub fn spawn(
        worker: &WorkerProgram,
        program: ApprovedProgram,
        generation: u64,
        initial_size: crate::program::TerminalSize,
    ) -> Result<Self, Error> {
        struct TestFence {
            cancelled: Arc<AtomicBool>,
        }
        impl ProgramDeliveryFence for TestFence {
            fn is_live(&self, _worker_generation: u64) -> bool {
                !self.cancelled.load(Ordering::Acquire)
            }

            fn try_enter(
                &self,
                _worker_generation: u64,
            ) -> Result<Option<Box<dyn crate::program::ProgramDeliveryPermit + '_>>, Error>
            {
                if self.cancelled.load(Ordering::Acquire) {
                    Err(Error::DisclosureRevoked)
                } else {
                    Ok(Some(Box::new(())))
                }
            }
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        Self::spawn_with_authority(
            worker,
            program,
            generation,
            initial_size,
            Arc::new(AtomicBool::new(false)),
            Arc::new(TestFence { cancelled }),
        )
    }

    pub fn spawn_with_authority(
        worker: &WorkerProgram,
        program: ApprovedProgram,
        generation: u64,
        initial_size: crate::program::TerminalSize,
        forced: Arc<AtomicBool>,
        fence: Arc<dyn ProgramDeliveryFence>,
    ) -> Result<Self, Error> {
        Self::spawn_with_authority_and_storage(
            worker,
            program,
            generation,
            initial_size,
            forced,
            fence,
            Arc::new(UnavailableProgramStorage),
        )
    }

    pub fn spawn_with_authority_and_storage(
        worker: &WorkerProgram,
        program: ApprovedProgram,
        generation: u64,
        initial_size: crate::program::TerminalSize,
        forced: Arc<AtomicBool>,
        fence: Arc<dyn ProgramDeliveryFence>,
        storage: Arc<dyn ProgramStorageHost>,
    ) -> Result<Self, Error> {
        Self::spawn_with_launch_and_storage(
            worker,
            program,
            generation,
            initial_size,
            forced,
            fence,
            storage,
            crate::program::ProgramLaunchParams::new(crate::program::ProgramLaunchMode::New),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_launch_and_storage(
        worker: &WorkerProgram,
        program: ApprovedProgram,
        generation: u64,
        initial_size: crate::program::TerminalSize,
        forced: Arc<AtomicBool>,
        fence: Arc<dyn ProgramDeliveryFence>,
        storage: Arc<dyn ProgramStorageHost>,
        params: crate::program::ProgramLaunchParams,
    ) -> Result<Self, Error> {
        platform::program_availability()?;
        if crate::cleanup_failed() {
            return Err(Error::Unavailable("an owned worker could not be reaped"));
        }
        initial_size.validate()?;
        if generation == 0 || generation > MAX_SAFE_INTEGER {
            return Err(Error::Protocol);
        }
        {
            let _prepare_permit = fence
                .try_enter(generation)?
                .ok_or(Error::DisclosureRevoked)?;
            storage
                .prepare()
                .map_err(|error| Error::Storage(error.message()))?;
        }
        let source = program.snapshot_bytes()?;
        let capabilities = program.manifest().capabilities.clone();
        let assets = program.assets().clone();
        let path = worker.as_path().to_owned();
        let digest = program.digest().to_owned();
        let (commands, receive) = mpsc::sync_channel(4);
        let (startup, ready) = mpsc::sync_channel(1);
        let supervisor_forced = forced.clone();
        let supervisor_fence = fence.clone();
        let join = thread::Builder::new()
            .name("festerm-javascript-program-supervisor".into())
            .spawn(move || {
                supervise_process(
                    path,
                    source,
                    capabilities,
                    assets,
                    digest,
                    params,
                    generation,
                    initial_size,
                    receive,
                    startup,
                    supervisor_forced,
                    supervisor_fence,
                    storage,
                )
            })
            .map_err(|_| Error::Unavailable("program supervisor thread unavailable"))?;
        match ready.recv_timeout(STARTUP_TIMEOUT + HOST_WAIT_TIMEOUT) {
            Ok(Ok((startup_output, completed))) => Ok(Self {
                generation,
                next_sequence: 1,
                startup_output,
                completed,
                commands,
                forced,
                fence,
                join: Mutex::new(Some(join)),
            }),
            Ok(Err(error)) => {
                forced.store(true, Ordering::Release);
                let _ = join.join();
                Err(error)
            }
            Err(_) => {
                forced.store(true, Ordering::Release);
                let _ = join.join();
                Err(Error::Deadline)
            }
        }
    }

    pub fn input(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
        if self.completed {
            return Err(Error::Protocol);
        }
        self.submit(&ProgramHostMessage::Input {
            generation: self.generation,
            sequence: self.next_sequence,
            bytes: bytes.to_vec(),
        })
        .and_then(|(submission, output)| {
            if submission == Submission::Accepted {
                Ok(output)
            } else {
                Err(Error::Protocol)
            }
        })
    }

    pub fn take_start_output(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.startup_output)
    }

    pub fn is_completed(&self) -> bool {
        self.completed
    }

    pub fn resize(&mut self, size: crate::program::TerminalSize) -> Result<Vec<Vec<u8>>, Error> {
        if self.completed {
            return Err(Error::Protocol);
        }
        self.submit(&ProgramHostMessage::Resize {
            generation: self.generation,
            sequence: self.next_sequence,
            size,
        })
        .and_then(|(submission, output)| {
            if submission == Submission::Accepted {
                Ok(output)
            } else {
                Err(Error::Protocol)
            }
        })
    }

    pub fn close(&mut self) -> Result<Vec<Vec<u8>>, Error> {
        if self.completed {
            return Ok(Vec::new());
        }
        self.submit(&ProgramHostMessage::Close {
            generation: self.generation,
            sequence: self.next_sequence,
        })
        .and_then(|(submission, output)| {
            if submission == Submission::Accepted {
                Ok(output)
            } else {
                Err(Error::Protocol)
            }
        })
    }

    pub fn submit(
        &mut self,
        message: &ProgramHostMessage,
    ) -> Result<(Submission, Vec<Vec<u8>>), Error> {
        message.validate()?;
        if message_generation(message) != self.generation {
            return Ok((Submission::StaleGeneration, Vec::new()));
        }
        if !self.fence.is_live(self.generation) {
            return Err(Error::DisclosureRevoked);
        }
        let (reply, result) = mpsc::sync_channel(1);
        let request = match message {
            ProgramHostMessage::Input {
                sequence, bytes, ..
            } => {
                expect_sequence(*sequence, self.next_sequence)?;
                CommandRequest::Input {
                    sequence: *sequence,
                    bytes: bytes.clone(),
                    reply,
                }
            }
            ProgramHostMessage::Resize { sequence, size, .. } => {
                expect_sequence(*sequence, self.next_sequence)?;
                CommandRequest::Resize {
                    sequence: *sequence,
                    size: *size,
                    reply,
                }
            }
            ProgramHostMessage::Close { sequence, .. } => {
                expect_sequence(*sequence, self.next_sequence)?;
                CommandRequest::Close {
                    sequence: *sequence,
                    reply,
                }
            }
            _ => return Err(Error::Protocol),
        };
        self.commands
            .try_send(request)
            .map_err(|_| Error::Protocol)?;
        let response = result
            .recv_timeout(OPERATION_TIMEOUT + HOST_WAIT_TIMEOUT + Duration::from_secs(1))
            .map_err(|_| {
                self.force();
                Error::Deadline
            })??;
        self.completed = response.completed;
        self.next_sequence = self.next_sequence.checked_add(1).ok_or(Error::Protocol)?;
        Ok((Submission::Accepted, response.output))
    }

    pub fn force(&self) {
        self.forced.store(true, Ordering::Release);
    }

    pub fn shutdown(mut self) -> Result<(), Error> {
        self.force();
        let revoked = !self.fence.is_live(self.generation);
        let result = self.join();
        if revoked {
            match result {
                Err(error @ Error::Cleanup) => Err(error),
                _ => Err(Error::DisclosureRevoked),
            }
        } else {
            result
        }
    }

    fn join(&mut self) -> Result<(), Error> {
        self.join
            .lock()
            .map_err(|_| Error::Cleanup)?
            .take()
            .ok_or(Error::Cleanup)?
            .join()
            .map_err(|_| Error::Cleanup)?
    }
}

impl Drop for ProgramSupervisor {
    fn drop(&mut self) {
        self.force();
        if let Ok(mut join) = self.join.lock() {
            if let Some(join) = join.take() {
                let _ = join.join();
            }
        }
    }
}

fn expect_sequence(sequence: u64, expected: u64) -> Result<(), Error> {
    if sequence == expected {
        Ok(())
    } else {
        Err(Error::Protocol)
    }
}

fn message_generation(message: &ProgramHostMessage) -> u64 {
    match message {
        ProgramHostMessage::Launch { generation, .. }
        | ProgramHostMessage::Input { generation, .. }
        | ProgramHostMessage::Resize { generation, .. }
        | ProgramHostMessage::Close { generation, .. }
        | ProgramHostMessage::WriteResult { generation, .. }
        | ProgramHostMessage::HostCallResult { generation, .. }
        | ProgramHostMessage::Cancel { generation } => *generation,
    }
}

#[allow(clippy::too_many_arguments)]
fn supervise_process(
    path: std::path::PathBuf,
    source: Vec<u8>,
    capabilities: BTreeSet<crate::program::ProgramCapability>,
    assets: BTreeMap<String, Vec<u8>>,
    digest: String,
    params: crate::program::ProgramLaunchParams,
    generation: u64,
    initial_size: crate::program::TerminalSize,
    commands: mpsc::Receiver<CommandRequest>,
    startup: SyncSender<StartupReply>,
    forced: Arc<AtomicBool>,
    fence: Arc<dyn ProgramDeliveryFence>,
    storage: Arc<dyn ProgramStorageHost>,
) -> Result<(), Error> {
    if forced.load(Ordering::Acquire) {
        return Err(Error::DisclosureRevoked);
    }
    let mut child = {
        let _launch_permit = fence
            .try_enter(generation)?
            .ok_or(Error::DisclosureRevoked)?;
        crate::supervisor::spawn_worker_with_argument(&path, "--javascript-program-worker")?
    };
    let result = catch_unwind(AssertUnwindSafe(|| {
        supervise_child(
            &mut child,
            source,
            capabilities,
            assets,
            digest,
            params,
            generation,
            initial_size,
            commands,
            &startup,
            &forced,
            fence.as_ref(),
            storage.as_ref(),
        )
    }))
    .unwrap_or(Err(Error::Cleanup));
    if result.is_err() {
        let _ = startup.try_send(Err(Error::Unavailable(
            "program worker failed before startup qualification",
        )));
    }
    crate::supervisor::reap_worker(&mut child).map_err(|_| Error::Cleanup)?;
    result
}

#[allow(clippy::too_many_arguments)]
fn supervise_child(
    child: &mut Child,
    source: Vec<u8>,
    capabilities: BTreeSet<crate::program::ProgramCapability>,
    assets: BTreeMap<String, Vec<u8>>,
    digest: String,
    params: crate::program::ProgramLaunchParams,
    generation: u64,
    initial_size: crate::program::TerminalSize,
    commands: mpsc::Receiver<CommandRequest>,
    startup: &SyncSender<StartupReply>,
    forced: &AtomicBool,
    fence: &dyn ProgramDeliveryFence,
    storage: &dyn ProgramStorageHost,
) -> Result<(), Error> {
    #[cfg(windows)]
    {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        while !child.poll_connected()? {
            if forced.load(Ordering::Acquire)
                || child.try_wait()?.is_some()
                || Instant::now() >= deadline
            {
                return Err(Error::Deadline);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
    let mut input = child.stdin.take().ok_or(Error::Protocol)?;
    let mut output = child.stdout.take().ok_or(Error::Protocol)?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        platform::nonblocking(input.as_raw_fd())?;
        platform::nonblocking(output.as_raw_fd())?;
    }
    let mut decoder = Decoder::default();
    expect_ready(
        child,
        &mut decoder,
        &mut output,
        startup,
        forced,
        STARTUP_TIMEOUT,
        fence,
        generation,
    )?;
    let mut cpu = ProgramCpuBudget::new(Instant::now());
    write_program_bundle(
        &mut input,
        &source,
        &digest,
        generation,
        &ProgramBundleMessage::Begin {
            generation,
            bundle_digest: digest.clone(),
            snapshot_bytes: source.len().try_into().map_err(|_| Error::Protocol)?,
        },
        forced,
        fence,
    )?;
    let launch = ProgramHostMessage::Launch {
        version: PROGRAM_PROTOCOL_VERSION,
        api_version: PROGRAM_API_VERSION,
        generation,
        bundle_digest: digest,
        initial_size,
        params,
    };
    write_message(&mut input, &launch, forced, fence, generation)?;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut next_host_call_id = 1_u64;
    let (startup_output, completed) = wait_started(
        child,
        &mut decoder,
        &mut output,
        &mut input,
        generation,
        deadline,
        forced,
        fence,
        &mut cpu,
        &capabilities,
        &assets,
        &mut next_host_call_id,
        storage,
    )?;
    startup
        .try_send(Ok((startup_output, completed)))
        .map_err(|_| Error::Protocol)?;
    if completed {
        return Ok(());
    }

    while !forced.load(Ordering::Acquire) {
        if !fence.is_live(generation) {
            return Err(Error::DisclosureRevoked);
        }
        check_program_limits(child, &mut cpu)?;
        #[cfg(target_os = "macos")]
        if platform::footprint_exceeded(child.id())? {
            return Err(Error::WorkerFailed("native memory ceiling exceeded"));
        }
        let request = match commands.recv_timeout(Duration::from_millis(2)) {
            Ok(request) => request,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if child.try_wait()?.is_some() {
                    return Err(Error::Protocol);
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        };
        let (message, reply, close) = match request {
            CommandRequest::Input {
                sequence,
                bytes,
                reply,
            } => (
                ProgramHostMessage::Input {
                    generation,
                    sequence,
                    bytes,
                },
                reply,
                false,
            ),
            CommandRequest::Resize {
                sequence,
                size,
                reply,
            } => (
                ProgramHostMessage::Resize {
                    generation,
                    sequence,
                    size,
                },
                reply,
                false,
            ),
            CommandRequest::Close { sequence, reply } => (
                ProgramHostMessage::Close {
                    generation,
                    sequence,
                },
                reply,
                true,
            ),
        };
        let deadline = Instant::now()
            + if close {
                CLOSE_TIMEOUT
            } else {
                OPERATION_TIMEOUT
            };
        let operation =
            write_message(&mut input, &message, forced, fence, generation).and_then(|_| {
                wait_operation(
                    child,
                    &mut decoder,
                    &mut output,
                    &mut input,
                    generation,
                    message_sequence(&message),
                    close,
                    deadline,
                    forced,
                    fence,
                    &mut cpu,
                    &capabilities,
                    &assets,
                    &mut next_host_call_id,
                    storage,
                )
            });
        let completed = operation.as_ref().is_ok_and(|response| response.completed);
        let failed = operation.is_err();
        let _ = reply.try_send(operation);
        if failed {
            return Err(Error::Protocol);
        }
        if completed || close {
            return Ok(());
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn expect_ready(
    child: &mut Child,
    decoder: &mut Decoder,
    output: &mut impl Read,
    startup: &SyncSender<StartupReply>,
    forced: &AtomicBool,
    timeout: Duration,
    fence: &dyn ProgramDeliveryFence,
    generation: u64,
) -> Result<(), Error> {
    let deadline = Instant::now() + timeout;
    loop {
        if forced.load(Ordering::Acquire) {
            return Err(Error::Deadline);
        }
        if !fence.is_live(generation) {
            return Err(Error::DisclosureRevoked);
        }
        let message: Option<ProgramWorkerMessage> = decoder.read(output)?;
        if let Some(message) = message {
            match message {
                ProgramWorkerMessage::Ready { profile }
                    if profile == platform::program_availability()? =>
                {
                    return Ok(())
                }
                ProgramWorkerMessage::Failed { code } => return Err(worker_failure(code)),
                _ => return Err(Error::Protocol),
            }
        }
        if Instant::now() >= deadline || child.try_wait()?.is_some() {
            let _ = startup.try_send(Err(Error::Deadline));
            return Err(Error::Deadline);
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn write_program_bundle(
    writer: &mut impl Write,
    source: &[u8],
    _digest: &str,
    generation: u64,
    begin: &ProgramBundleMessage,
    forced: &AtomicBool,
    fence: &dyn ProgramDeliveryFence,
) -> Result<(), Error> {
    begin.validate()?;
    write_frame(writer, begin, forced, fence, generation)?;
    for bytes in source.chunks(CHUNK_BYTES) {
        write_frame(
            writer,
            &ProgramBundleMessage::SourceChunk {
                generation,
                bytes: bytes.to_vec(),
            },
            forced,
            fence,
            generation,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn wait_started(
    child: &mut Child,
    decoder: &mut Decoder,
    output: &mut impl Read,
    input: &mut impl Write,
    generation: u64,
    mut deadline: Instant,
    forced: &AtomicBool,
    fence: &dyn ProgramDeliveryFence,
    cpu: &mut ProgramCpuBudget,
    capabilities: &BTreeSet<crate::program::ProgramCapability>,
    assets: &BTreeMap<String, Vec<u8>>,
    next_host_call_id: &mut u64,
    storage: &dyn ProgramStorageHost,
) -> Result<(Vec<Vec<u8>>, bool), Error> {
    let mut writes = Vec::new();
    let mut output_bytes = 0;
    let mut host_wait = Duration::ZERO;
    loop {
        check_deadline(deadline, forced, fence, generation)?;
        let message: Option<ProgramWorkerMessage> = decoder.read(output)?;
        if let Some(message) = message {
            message.validate()?;
            match message {
                ProgramWorkerMessage::Write {
                    generation: message_generation,
                    write_id,
                    bytes,
                } if message_generation == generation => {
                    let _permit = fence
                        .try_enter(generation)?
                        .ok_or(Error::DisclosureRevoked)?;
                    write_message_under_permit(
                        input,
                        &ProgramHostMessage::WriteResult {
                            generation,
                            write_id,
                            acceptance: WriteAcceptance::Accepted,
                        },
                        forced,
                    )?;
                    append_output(&mut writes, &mut output_bytes, bytes.clone())?;
                }
                ProgramWorkerMessage::Started {
                    generation: started,
                } if started == generation => return Ok((writes, false)),
                ProgramWorkerMessage::Finished {
                    generation: finished,
                    outcome: ProgramOutcome::Completed {},
                } if finished == generation => return Ok((writes, true)),
                ProgramWorkerMessage::HostCall {
                    generation: call_generation,
                    call_id,
                    request,
                } if call_generation == generation => {
                    expect_host_call_id(call_id, next_host_call_id)?;
                    let admitted =
                        admit_host_call_wait(&mut deadline, &request, false, &mut host_wait);
                    serve_program_host_call(
                        child,
                        input,
                        generation,
                        call_id,
                        request,
                        capabilities,
                        assets,
                        forced,
                        fence,
                        cpu,
                        admitted,
                        storage,
                    )?
                }
                ProgramWorkerMessage::Failed { code } => return Err(worker_failure(code)),
                _ => return Err(Error::Protocol),
            }
        }
        check_wait(child, deadline, forced, fence, generation, cpu)?;
    }
}

#[allow(clippy::too_many_arguments)]
fn wait_operation(
    child: &mut Child,
    decoder: &mut Decoder,
    output: &mut impl Read,
    input: &mut impl Write,
    generation: u64,
    sequence: u64,
    close: bool,
    mut deadline: Instant,
    forced: &AtomicBool,
    fence: &dyn ProgramDeliveryFence,
    cpu: &mut ProgramCpuBudget,
    capabilities: &BTreeSet<crate::program::ProgramCapability>,
    assets: &BTreeMap<String, Vec<u8>>,
    next_host_call_id: &mut u64,
    storage: &dyn ProgramStorageHost,
) -> Result<OperationResponse, Error> {
    let mut writes = Vec::new();
    let mut output_bytes = 0;
    let mut host_wait = Duration::ZERO;
    loop {
        check_deadline(deadline, forced, fence, generation)?;
        let decoded: Option<ProgramWorkerMessage> = decoder.read(output)?;
        if let Some(message) = decoded {
            message.validate()?;
            match message {
                ProgramWorkerMessage::Write {
                    generation: message_generation,
                    write_id,
                    bytes,
                } if message_generation == generation => {
                    let _permit = fence
                        .try_enter(generation)?
                        .ok_or(Error::DisclosureRevoked)?;
                    write_message_under_permit(
                        input,
                        &ProgramHostMessage::WriteResult {
                            generation,
                            write_id,
                            acceptance: WriteAcceptance::Accepted,
                        },
                        forced,
                    )?;
                    append_output(&mut writes, &mut output_bytes, bytes)?;
                }
                ProgramWorkerMessage::EventCompleted {
                    generation: completed_generation,
                    sequence: completed_sequence,
                } if !close
                    && completed_generation == generation
                    && completed_sequence == sequence =>
                {
                    return Ok(OperationResponse {
                        output: writes,
                        completed: false,
                    })
                }
                ProgramWorkerMessage::HostCall {
                    generation: call_generation,
                    call_id,
                    request,
                } if call_generation == generation => {
                    expect_host_call_id(call_id, next_host_call_id)?;
                    let admitted =
                        admit_host_call_wait(&mut deadline, &request, close, &mut host_wait);
                    serve_program_host_call(
                        child,
                        input,
                        generation,
                        call_id,
                        request,
                        capabilities,
                        assets,
                        forced,
                        fence,
                        cpu,
                        admitted,
                        storage,
                    )?
                }
                ProgramWorkerMessage::Finished {
                    generation: finished_generation,
                    outcome,
                } if finished_generation == generation && sequence != 0 => {
                    return match outcome {
                        ProgramOutcome::Completed {} | ProgramOutcome::Cancelled {} => {
                            Ok(OperationResponse {
                                output: writes,
                                completed: true,
                            })
                        }
                        _ => Err(Error::WorkerFailed("program settled unsuccessfully")),
                    };
                }
                ProgramWorkerMessage::Failed { code } => return Err(worker_failure(code)),
                _ => return Err(Error::Protocol),
            }
        }

        check_wait(child, deadline, forced, fence, generation, cpu)?;
    }
}

fn expect_host_call_id(call_id: u64, next_host_call_id: &mut u64) -> Result<(), Error> {
    if call_id != *next_host_call_id {
        return Err(Error::Protocol);
    }
    *next_host_call_id = (*next_host_call_id).checked_add(1).ok_or(Error::Protocol)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn serve_program_host_call(
    child: &mut Child,
    input: &mut impl Write,
    generation: u64,
    call_id: u64,
    request: ProgramHostRequest,
    capabilities: &BTreeSet<crate::program::ProgramCapability>,
    assets: &BTreeMap<String, Vec<u8>>,
    forced: &AtomicBool,
    fence: &dyn ProgramDeliveryFence,
    cpu: &mut ProgramCpuBudget,
    admitted: bool,
    storage: &dyn ProgramStorageHost,
) -> Result<(), Error> {
    request.validate()?;
    if !capabilities.contains(&request.capability()) {
        return Err(Error::Protocol);
    }
    let _storage_permit = match &request {
        ProgramHostRequest::StorageLoad { .. } | ProgramHostRequest::StorageCommit { .. } => Some(
            fence
                .try_enter(generation)?
                .ok_or(Error::DisclosureRevoked)?,
        ),
        _ => None,
    };
    let result = if !admitted {
        ProgramHostResult::Error {
            code: ProgramHostError::ResourceLimit,
        }
    } else {
        match &request {
            ProgramHostRequest::AssetsRead { id } => match assets.get(id) {
                Some(bytes) => ProgramHostResult::Asset {
                    bytes: bytes.clone(),
                },
                None => ProgramHostResult::Error {
                    code: ProgramHostError::NotFound,
                },
            },
            ProgramHostRequest::ClockNow {} => ProgramHostResult::Clock {
                unix_milliseconds: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            },
            ProgramHostRequest::ClockSleep { milliseconds } => {
                let deadline = Instant::now() + Duration::from_millis((*milliseconds).into());
                while Instant::now() < deadline {
                    check_wait(child, deadline, forced, fence, generation, cpu)?;
                }
                ProgramHostResult::Slept {}
            }
            ProgramHostRequest::StorageLoad { slot } => storage_load_result(storage.load(slot)),
            ProgramHostRequest::StorageCommit {
                slot,
                expected_revision,
                schema_version,
                value,
                operation_id,
            } => storage_commit_result(storage.commit(
                slot,
                ProgramStorageCommitRequest {
                    expected_revision: *expected_revision,
                    schema_version: *schema_version,
                    value: value.clone(),
                    operation_id: operation_id.clone(),
                },
            )),
        }
    };
    result.validate_for(&request)?;
    let _delivery_permit = if _storage_permit.is_none() {
        Some(
            fence
                .try_enter(generation)?
                .ok_or(Error::DisclosureRevoked)?,
        )
    } else {
        None
    };
    write_message_under_permit(
        input,
        &ProgramHostMessage::HostCallResult {
            generation,
            call_id,
            result,
        },
        forced,
    )
}

fn storage_load_result(result: ProgramStorageLoad) -> ProgramHostResult {
    match result {
        ProgramStorageLoad::Missing => ProgramHostResult::StorageMissing {},
        ProgramStorageLoad::Present {
            schema_version,
            revision,
            value,
        } => ProgramHostResult::StoragePresent {
            schema_version,
            revision,
            value,
        },
        ProgramStorageLoad::Corrupt => ProgramHostResult::Error {
            code: ProgramHostError::Corrupt,
        },
        ProgramStorageLoad::Future { .. } => ProgramHostResult::Error {
            code: ProgramHostError::Future,
        },
        ProgramStorageLoad::Unavailable => ProgramHostResult::Error {
            code: ProgramHostError::Unavailable,
        },
        ProgramStorageLoad::BusyWriter => ProgramHostResult::Error {
            code: ProgramHostError::BusyWriter,
        },
        ProgramStorageLoad::ResourceLimit => ProgramHostResult::Error {
            code: ProgramHostError::ResourceLimit,
        },
    }
}

fn storage_commit_result(result: ProgramStorageCommit) -> ProgramHostResult {
    match result {
        ProgramStorageCommit::Committed { revision } => {
            ProgramHostResult::StorageCommitted { revision }
        }
        ProgramStorageCommit::Conflict { actual_revision } => {
            ProgramHostResult::StorageConflict { actual_revision }
        }
        ProgramStorageCommit::Corrupt => ProgramHostResult::Error {
            code: ProgramHostError::Corrupt,
        },
        ProgramStorageCommit::Future { .. } => ProgramHostResult::Error {
            code: ProgramHostError::Future,
        },
        ProgramStorageCommit::Unavailable => ProgramHostResult::Error {
            code: ProgramHostError::Unavailable,
        },
        ProgramStorageCommit::BusyWriter => ProgramHostResult::Error {
            code: ProgramHostError::BusyWriter,
        },
        ProgramStorageCommit::Uncertain => ProgramHostResult::Error {
            code: ProgramHostError::Uncertain,
        },
        ProgramStorageCommit::ResourceLimit => ProgramHostResult::Error {
            code: ProgramHostError::ResourceLimit,
        },
    }
}

fn admit_host_call_wait(
    deadline: &mut Instant,
    request: &ProgramHostRequest,
    close: bool,
    host_wait: &mut Duration,
) -> bool {
    if close
        && matches!(
            request,
            ProgramHostRequest::StorageLoad { .. } | ProgramHostRequest::StorageCommit { .. }
        )
    {
        return false;
    }
    let ProgramHostRequest::ClockSleep { milliseconds } = request else {
        return true;
    };
    if close {
        return false;
    }
    let Some(total) = host_wait.checked_add(Duration::from_millis(u64::from(*milliseconds))) else {
        return false;
    };
    if total > HOST_WAIT_TIMEOUT {
        return false;
    }
    *host_wait = total;
    *deadline = (*deadline).max(Instant::now() + total + PROGRAM_TURN_TIMEOUT);
    true
}

fn check_wait(
    child: &mut Child,
    deadline: Instant,
    forced: &AtomicBool,
    fence: &dyn ProgramDeliveryFence,
    generation: u64,
    cpu: &mut ProgramCpuBudget,
) -> Result<(), Error> {
    check_deadline(deadline, forced, fence, generation)?;
    check_program_limits(child, cpu)?;
    if child.try_wait()?.is_some() {
        return Err(Error::Protocol);
    }
    #[cfg(target_os = "macos")]
    if platform::footprint_exceeded(child.id())? {
        return Err(Error::WorkerFailed("native memory ceiling exceeded"));
    }
    thread::sleep(Duration::from_millis(1));
    Ok(())
}

fn worker_cpu_time(child: &Child) -> Result<Duration, Error> {
    #[cfg(windows)]
    return child.cpu_time();
    #[cfg(not(windows))]
    return platform::child_cpu_time(child.id());
}

fn check_program_limits(child: &Child, cpu: &mut ProgramCpuBudget) -> Result<(), Error> {
    cpu.sample(Instant::now(), || worker_cpu_time(child))
}

fn worker_failure(code: crate::program::ProgramWorkerFailure) -> Error {
    match code {
        crate::program::ProgramWorkerFailure::Unavailable => {
            Error::Unavailable("program worker unavailable")
        }
        crate::program::ProgramWorkerFailure::InvalidSnapshot => Error::Snapshot,
        crate::program::ProgramWorkerFailure::Protocol => Error::Protocol,
        crate::program::ProgramWorkerFailure::Deadline => Error::Deadline,
        crate::program::ProgramWorkerFailure::Io => Error::WorkerFailed("program worker I/O"),
        crate::program::ProgramWorkerFailure::Runtime => {
            Error::WorkerFailed("program worker runtime")
        }
    }
}

fn check_deadline(
    deadline: Instant,
    forced: &AtomicBool,
    fence: &dyn ProgramDeliveryFence,
    generation: u64,
) -> Result<(), Error> {
    if forced.load(Ordering::Acquire) {
        return Err(Error::DisclosureRevoked);
    }
    if !fence.is_live(generation) {
        return Err(Error::DisclosureRevoked);
    }
    if Instant::now() >= deadline {
        return Err(Error::Deadline);
    }
    Ok(())
}

fn append_output(
    output: &mut Vec<Vec<u8>>,
    output_bytes: &mut usize,
    bytes: Vec<u8>,
) -> Result<(), Error> {
    let next_bytes = output_bytes
        .checked_add(bytes.len())
        .ok_or(Error::Protocol)?;
    if output.len() >= MAX_PENDING_WRITES || next_bytes > MAX_OUTPUT_QUEUE_BYTES {
        return Err(Error::Protocol);
    }
    *output_bytes = next_bytes;
    output.push(bytes);
    Ok(())
}

fn message_sequence(message: &ProgramHostMessage) -> u64 {
    match message {
        ProgramHostMessage::Input { sequence, .. }
        | ProgramHostMessage::Resize { sequence, .. }
        | ProgramHostMessage::Close { sequence, .. } => *sequence,
        _ => 0,
    }
}

fn write_message(
    writer: &mut impl Write,
    message: &ProgramHostMessage,
    forced: &AtomicBool,
    fence: &dyn ProgramDeliveryFence,
    generation: u64,
) -> Result<(), Error> {
    write_frame_bytes(
        writer,
        encode_program_frame(message)?,
        forced,
        fence,
        generation,
    )
}

fn write_message_under_permit(
    writer: &mut impl Write,
    message: &ProgramHostMessage,
    forced: &AtomicBool,
) -> Result<(), Error> {
    write_frame_bytes_physical(writer, encode_program_frame(message)?, forced)
}

fn write_frame<T: serde::Serialize>(
    writer: &mut impl Write,
    message: &T,
    forced: &AtomicBool,
    fence: &dyn ProgramDeliveryFence,
    generation: u64,
) -> Result<(), Error> {
    write_frame_bytes(writer, encode_frame(message)?, forced, fence, generation)
}

fn write_frame_bytes(
    writer: &mut impl Write,
    bytes: Vec<u8>,
    forced: &AtomicBool,
    fence: &dyn ProgramDeliveryFence,
    generation: u64,
) -> Result<(), Error> {
    let deadline = Instant::now() + Duration::from_millis(250);
    let _permit = fence
        .try_enter(generation)?
        .ok_or(Error::DisclosureRevoked)?;
    write_frame_bytes_physical_with_deadline(writer, bytes, forced, deadline)
}

fn write_frame_bytes_physical(
    writer: &mut impl Write,
    bytes: Vec<u8>,
    forced: &AtomicBool,
) -> Result<(), Error> {
    write_frame_bytes_physical_with_deadline(
        writer,
        bytes,
        forced,
        Instant::now() + Duration::from_millis(250),
    )
}

fn write_frame_bytes_physical_with_deadline(
    writer: &mut impl Write,
    bytes: Vec<u8>,
    forced: &AtomicBool,
    deadline: Instant,
) -> Result<(), Error> {
    let mut sent = 0;
    while sent < bytes.len() {
        if forced.load(Ordering::Acquire) {
            return Err(Error::DisclosureRevoked);
        }
        if Instant::now() >= deadline {
            return Err(Error::Deadline);
        }
        match writer.write(&bytes[sent..]) {
            Ok(0) => return Err(Error::Protocol),
            Ok(count) => sent += count,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
        thread::yield_now();
    }
    writer.flush().map_err(Error::from)
}

#[cfg(test)]
mod delivery_race_tests {
    use super::*;
    use std::io;
    use std::sync::mpsc;

    struct GateFence {
        generation: u64,
        gate: Arc<Mutex<()>>,
        revoked: Arc<AtomicBool>,
    }

    impl ProgramDeliveryFence for GateFence {
        fn is_live(&self, generation: u64) -> bool {
            generation == self.generation && !self.revoked.load(Ordering::Acquire)
        }

        fn try_enter(
            &self,
            generation: u64,
        ) -> Result<Option<Box<dyn crate::program::ProgramDeliveryPermit + '_>>, Error> {
            let permit = self.gate.lock().map_err(|_| Error::DisclosureRevoked)?;
            if generation != self.generation || self.revoked.load(Ordering::Acquire) {
                Err(Error::DisclosureRevoked)
            } else {
                Ok(Some(Box::new(permit)))
            }
        }
    }

    struct BlockingWriter {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        calls: usize,
    }

    impl Write for BlockingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.calls == 1 {
                self.entered.send(()).unwrap();
                self.release.recv().unwrap();
                Ok(1)
            } else {
                Ok(bytes.len())
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn revocation_waits_for_the_actual_ipc_write_permit() {
        let fence = Arc::new(GateFence {
            generation: 7,
            gate: Arc::new(Mutex::new(())),
            revoked: Arc::new(AtomicBool::new(false)),
        });
        let forced = Arc::new(AtomicBool::new(false));
        let (entered_send, entered_receive) = mpsc::channel();
        let (release_send, release_receive) = mpsc::channel();
        let mut writer = BlockingWriter {
            entered: entered_send,
            release: release_receive,
            calls: 0,
        };
        let (write_done_send, write_done_receive) = mpsc::channel();
        let (revoke_done_send, revoke_done_receive) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let result =
                    write_frame_bytes(&mut writer, vec![b'x'; 8], &forced, fence.as_ref(), 7);
                write_done_send.send(result).unwrap();
            });
            entered_receive.recv().unwrap();
            let revoke_fence = fence.clone();
            scope.spawn(move || {
                let _permit = revoke_fence.gate.lock().unwrap();
                revoke_fence.revoked.store(true, Ordering::Release);
                revoke_done_send.send(()).unwrap();
            });
            assert!(matches!(
                revoke_done_receive.recv_timeout(Duration::from_millis(20)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            release_send.send(()).unwrap();
            assert!(write_done_receive
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_ok());
            revoke_done_receive
                .recv_timeout(Duration::from_secs(1))
                .unwrap();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_sleep_budget_is_aggregate_and_close_waits_are_refused() {
        let mut deadline = Instant::now() + OPERATION_TIMEOUT;
        let mut host_wait = Duration::ZERO;
        assert!(admit_host_call_wait(
            &mut deadline,
            &ProgramHostRequest::ClockSleep {
                milliseconds: MAX_SLEEP_MS,
            },
            false,
            &mut host_wait,
        ));
        assert!(!admit_host_call_wait(
            &mut deadline,
            &ProgramHostRequest::ClockSleep { milliseconds: 1 },
            false,
            &mut host_wait,
        ));
        assert!(!admit_host_call_wait(
            &mut deadline,
            &ProgramHostRequest::ClockSleep { milliseconds: 1 },
            true,
            &mut host_wait,
        ));
        assert!(!admit_host_call_wait(
            &mut deadline,
            &ProgramHostRequest::StorageLoad {
                slot: "primary".into(),
            },
            true,
            &mut host_wait,
        ));
    }

    #[test]
    fn rolling_cpu_budget_expires_idle_work_and_limits_new_work() {
        let start = Instant::now();
        let mut budget = ProgramCpuBudget::new(start);
        budget
            .observe(
                start + CPU_SAMPLE_INTERVAL,
                PROGRAM_CPU_WINDOW_LIMIT - Duration::from_millis(10),
            )
            .unwrap();
        budget
            .observe(
                start + PROGRAM_CPU_WINDOW + Duration::from_millis(20),
                PROGRAM_CPU_WINDOW_LIMIT - Duration::from_millis(10),
            )
            .unwrap();
        assert!(budget
            .observe(
                start + PROGRAM_CPU_WINDOW + Duration::from_millis(30),
                PROGRAM_CPU_WINDOW_LIMIT + Duration::from_millis(350),
            )
            .is_err());
    }

    #[test]
    fn rolling_cpu_budget_rejects_regression_and_has_hard_fallback() {
        let start = Instant::now();
        let mut budget = ProgramCpuBudget::new(start);
        budget
            .observe(start + CPU_SAMPLE_INTERVAL, Duration::from_millis(10))
            .unwrap();
        assert!(budget
            .observe(start + CPU_SAMPLE_INTERVAL * 2, Duration::ZERO)
            .is_err());
        let mut budget = ProgramCpuBudget::new(start);
        assert!(budget
            .observe(
                start + CPU_SAMPLE_INTERVAL,
                Duration::from_secs(PROGRAM_HARD_CPU_SECONDS),
            )
            .is_err());
    }

    #[test]
    fn rolling_cpu_budget_allows_cumulative_work_after_windows_expire() {
        let start = Instant::now();
        let mut budget = ProgramCpuBudget::new(start);
        let mut cpu = Duration::ZERO;
        for window in 1..=30 {
            let base = start + Duration::from_secs(window * 2);
            for sample in 1..=20 {
                cpu += CPU_SAMPLE_INTERVAL;
                budget
                    .observe(base + CPU_SAMPLE_INTERVAL * sample, cpu)
                    .unwrap();
            }
        }
        assert!(cpu > Duration::from_secs(5));
    }

    #[test]
    fn process_cpu_query_waits_for_sampling_deadline() {
        let start = Instant::now();
        let mut budget = ProgramCpuBudget::new(start);
        let queries = std::cell::Cell::new(0);
        budget
            .sample(
                start + CPU_SAMPLE_INTERVAL - Duration::from_millis(1),
                || {
                    queries.set(queries.get() + 1);
                    Ok(Duration::ZERO)
                },
            )
            .unwrap();
        assert_eq!(queries.get(), 0);
        budget
            .sample(start + CPU_SAMPLE_INTERVAL, || {
                queries.set(queries.get() + 1);
                Ok(Duration::from_millis(1))
            })
            .unwrap();
        assert_eq!(queries.get(), 1);
    }
}
