#[cfg(windows)]
use crate::windows::Child;
use crate::{
    platform,
    program::{
        encode_program_frame, ApprovedProgram, ProgramBundleMessage, ProgramHostMessage,
        ProgramOutcome, ProgramWorkerMessage, Submission, WriteAcceptance, MAX_OUTPUT_QUEUE_BYTES,
        MAX_PENDING_WRITES, PROGRAM_API_VERSION, PROGRAM_PROTOCOL_VERSION,
    },
    protocol::{encode_frame, Decoder, CHUNK_BYTES, MAX_SAFE_INTEGER},
    Error, WorkerProgram,
};
#[cfg(not(windows))]
use std::process::Child;
use std::{
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
const OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

enum CommandRequest {
    Input {
        sequence: u64,
        bytes: Vec<u8>,
        reply: SyncSender<Result<Vec<Vec<u8>>, Error>>,
    },
    Resize {
        sequence: u64,
        size: crate::program::TerminalSize,
        reply: SyncSender<Result<Vec<Vec<u8>>, Error>>,
    },
    Close {
        sequence: u64,
        reply: SyncSender<Result<Vec<Vec<u8>>, Error>>,
    },
}

/// Separately confined, persistent process for one program generation.
///
/// It has no automation host-call authority. The only admitted operations are
/// the terminal events defined by the terminal-program protocol.
pub struct ProgramSupervisor {
    generation: u64,
    next_sequence: u64,
    startup_output: Vec<Vec<u8>>,
    commands: SyncSender<CommandRequest>,
    forced: Arc<AtomicBool>,
    join: Mutex<Option<JoinHandle<Result<(), Error>>>>,
}

impl ProgramSupervisor {
    pub fn spawn(
        worker: &WorkerProgram,
        program: ApprovedProgram,
        generation: u64,
        initial_size: crate::program::TerminalSize,
    ) -> Result<Self, Error> {
        Self::spawn_with_cancellation(
            worker,
            program,
            generation,
            initial_size,
            Arc::new(AtomicBool::new(false)),
        )
    }

    pub fn spawn_with_cancellation(
        worker: &WorkerProgram,
        program: ApprovedProgram,
        generation: u64,
        initial_size: crate::program::TerminalSize,
        forced: Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        platform::availability()?;
        if crate::cleanup_failed() {
            return Err(Error::Unavailable("an owned worker could not be reaped"));
        }
        initial_size.validate()?;
        if generation == 0 || generation > MAX_SAFE_INTEGER {
            return Err(Error::Protocol);
        }
        let source = program.snapshot_bytes()?;
        let path = worker.as_path().to_owned();
        let digest = program.digest().to_owned();
        let (commands, receive) = mpsc::sync_channel(4);
        let (startup, ready) = mpsc::sync_channel(1);
        let supervisor_forced = forced.clone();
        let join = thread::Builder::new()
            .name("festerm-javascript-program-supervisor".into())
            .spawn(move || {
                supervise_process(
                    path,
                    source,
                    digest,
                    generation,
                    initial_size,
                    receive,
                    startup,
                    supervisor_forced,
                )
            })
            .map_err(|_| Error::Unavailable("program supervisor thread unavailable"))?;
        match ready.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(startup_output)) => Ok(Self {
                generation,
                next_sequence: 1,
                startup_output,
                commands,
                forced,
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

    pub fn resize(&mut self, size: crate::program::TerminalSize) -> Result<Vec<Vec<u8>>, Error> {
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
        let output = result
            .recv_timeout(OPERATION_TIMEOUT + Duration::from_secs(1))
            .map_err(|_| {
                self.force();
                Error::Deadline
            })??;
        self.next_sequence = self.next_sequence.checked_add(1).ok_or(Error::Protocol)?;
        Ok((Submission::Accepted, output))
    }

    pub fn force(&self) {
        self.forced.store(true, Ordering::Release);
    }

    pub fn shutdown(mut self) -> Result<(), Error> {
        self.force();
        self.join()
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
        | ProgramHostMessage::Cancel { generation } => *generation,
    }
}

#[allow(clippy::too_many_arguments)]
fn supervise_process(
    path: std::path::PathBuf,
    source: Vec<u8>,
    digest: String,
    generation: u64,
    initial_size: crate::program::TerminalSize,
    commands: mpsc::Receiver<CommandRequest>,
    startup: SyncSender<Result<Vec<Vec<u8>>, Error>>,
    forced: Arc<AtomicBool>,
) -> Result<(), Error> {
    let mut child =
        crate::supervisor::spawn_worker_with_argument(&path, "--javascript-program-worker")?;
    let result = catch_unwind(AssertUnwindSafe(|| {
        supervise_child(
            &mut child,
            source,
            digest,
            generation,
            initial_size,
            commands,
            &startup,
            &forced,
        )
    }))
    .unwrap_or(Err(Error::Cleanup));
    if result.is_err() {
        let _ = startup.try_send(Err(Error::Unavailable(
            "program worker failed before startup qualification",
        )));
    }
    let cleanup = crate::supervisor::reap_worker(&mut child);
    cleanup?;
    result
}

#[allow(clippy::too_many_arguments)]
fn supervise_child(
    child: &mut Child,
    source: Vec<u8>,
    digest: String,
    generation: u64,
    initial_size: crate::program::TerminalSize,
    commands: mpsc::Receiver<CommandRequest>,
    startup: &SyncSender<Result<Vec<Vec<u8>>, Error>>,
    forced: &AtomicBool,
) -> Result<(), Error> {
    #[cfg(windows)]
    {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        while !child.poll_connected()? {
            if child.try_wait()?.is_some() || Instant::now() >= deadline {
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
    )?;
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
    )?;
    let launch = ProgramHostMessage::Launch {
        version: PROGRAM_PROTOCOL_VERSION,
        api_version: PROGRAM_API_VERSION,
        generation,
        bundle_digest: digest,
        initial_size,
    };
    write_message(&mut input, &launch, forced)?;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let startup_output = wait_started(
        child,
        &mut decoder,
        &mut output,
        &mut input,
        generation,
        deadline,
        forced,
    )?;
    startup
        .try_send(Ok(startup_output))
        .map_err(|_| Error::Protocol)?;

    while !forced.load(Ordering::Acquire) {
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
        write_message(&mut input, &message, forced)?;
        let deadline = Instant::now()
            + if close {
                CLOSE_TIMEOUT
            } else {
                OPERATION_TIMEOUT
            };
        let operation = wait_operation(
            child,
            &mut decoder,
            &mut output,
            &mut input,
            generation,
            message_sequence(&message),
            close,
            deadline,
            forced,
        );
        let finished = close;
        let _ = reply.try_send(operation);
        if finished {
            return Ok(());
        }
    }
    Ok(())
}

fn expect_ready(
    child: &mut Child,
    decoder: &mut Decoder,
    output: &mut impl Read,
    startup: &SyncSender<Result<Vec<Vec<u8>>, Error>>,
    forced: &AtomicBool,
    timeout: Duration,
) -> Result<(), Error> {
    let deadline = Instant::now() + timeout;
    loop {
        if forced.load(Ordering::Acquire) {
            return Err(Error::Deadline);
        }
        let message: Option<ProgramWorkerMessage> = decoder.read(output)?;
        if let Some(message) = message {
            match message {
                ProgramWorkerMessage::Ready { profile } if profile == platform::availability()? => {
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
) -> Result<(), Error> {
    begin.validate()?;
    write_frame(writer, begin, forced)?;
    for bytes in source.chunks(CHUNK_BYTES) {
        write_frame(
            writer,
            &ProgramBundleMessage::SourceChunk {
                generation,
                bytes: bytes.to_vec(),
            },
            forced,
        )?;
    }
    Ok(())
}

fn wait_started(
    child: &mut Child,
    decoder: &mut Decoder,
    output: &mut impl Read,
    input: &mut impl Write,
    generation: u64,
    deadline: Instant,
    forced: &AtomicBool,
) -> Result<Vec<Vec<u8>>, Error> {
    let mut writes = Vec::new();
    let mut output_bytes = 0;
    loop {
        check_deadline(deadline, forced)?;
        let message: Option<ProgramWorkerMessage> = decoder.read(output)?;
        if let Some(message) = message {
            message.validate()?;
            match message {
                ProgramWorkerMessage::Write {
                    generation: message_generation,
                    write_id,
                    bytes,
                } if message_generation == generation => {
                    write_message(
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
                } if started == generation => return Ok(writes),
                ProgramWorkerMessage::Failed { code } => return Err(worker_failure(code)),
                _ => return Err(Error::Protocol),
            }
        }
        check_wait(child, deadline, forced)?;
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
    deadline: Instant,
    forced: &AtomicBool,
) -> Result<Vec<Vec<u8>>, Error> {
    let mut writes = Vec::new();
    let mut output_bytes = 0;
    loop {
        check_deadline(deadline, forced)?;
        let decoded: Option<ProgramWorkerMessage> = decoder.read(output)?;
        if let Some(message) = decoded {
            message.validate()?;
            match message {
                ProgramWorkerMessage::Write {
                    generation: message_generation,
                    write_id,
                    bytes,
                } if message_generation == generation => {
                    write_message(
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
                } if completed_generation == generation && completed_sequence == sequence => {
                    return Ok(writes)
                }
                ProgramWorkerMessage::Finished {
                    generation: finished_generation,
                    outcome: ProgramOutcome::Completed {} | ProgramOutcome::Cancelled {},
                } if close && finished_generation == generation && sequence != 0 => {
                    return Ok(writes)
                }
                ProgramWorkerMessage::Failed { code } => return Err(worker_failure(code)),
                _ => return Err(Error::Protocol),
            }
        }
        check_wait(child, deadline, forced)?;
    }
}

fn check_wait(child: &mut Child, deadline: Instant, forced: &AtomicBool) -> Result<(), Error> {
    check_deadline(deadline, forced)?;
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

fn check_deadline(deadline: Instant, forced: &AtomicBool) -> Result<(), Error> {
    if forced.load(Ordering::Acquire) || Instant::now() >= deadline {
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
) -> Result<(), Error> {
    write_frame_bytes(writer, encode_program_frame(message)?, forced)
}

fn write_frame<T: serde::Serialize>(
    writer: &mut impl Write,
    message: &T,
    forced: &AtomicBool,
) -> Result<(), Error> {
    write_frame_bytes(writer, encode_frame(message)?, forced)
}

fn write_frame_bytes(
    writer: &mut impl Write,
    bytes: Vec<u8>,
    forced: &AtomicBool,
) -> Result<(), Error> {
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut sent = 0;
    while sent < bytes.len() {
        if forced.load(Ordering::Acquire) || Instant::now() >= deadline {
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
