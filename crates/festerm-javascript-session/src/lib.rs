//! Bounded session adapter for an independently owned JavaScript runtime.
//!
//! The runtime is injected behind [`JavaScriptRuntime`]. This crate owns
//! transport ordering and session lifecycle only; it does not own automation
//! authority or terminal state.

use festerm_session::{
    noop_session_event_notifier, Session, SessionError, SessionErrorKind, SessionEvent,
    SessionEventNotifier, SessionId, SessionLifecycle, SessionMetrics, SessionOperation,
    SessionSendError, SessionTryReceiveError, ShutdownError, ShutdownResult, TerminalSize,
    DEFAULT_COMMAND_QUEUE_CAPACITY, DEFAULT_EVENT_QUEUE_CAPACITY, MAX_IO_CHUNK_BYTES,
};
use std::{
    collections::VecDeque,
    fmt,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
        Arc, Condvar, Mutex,
    },
    thread,
    time::Duration,
};

const MAX_RUNTIME_OUTPUT_BYTES: usize = DEFAULT_EVENT_QUEUE_CAPACITY * MAX_IO_CHUNK_BYTES;

/// A content-free failure reported by an injected JavaScript runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JavaScriptRuntimeError(&'static str);

impl JavaScriptRuntimeError {
    pub const fn new(message: &'static str) -> Self {
        Self(message)
    }
}

impl fmt::Display for JavaScriptRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for JavaScriptRuntimeError {}

/// Production boundary between session transport and a JavaScript worker.
///
/// Implementations are owned and called only by the session worker thread.
/// Input, resize and close methods may block, but must be cooperatively
/// bounded by their implementation. Returned chunks are bounded below by the
/// session before delivery to the application.
pub trait JavaScriptRuntime: Send + 'static {
    fn cancellation_flag(&self) -> Option<Arc<AtomicBool>> {
        None
    }

    fn start(&mut self, size: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError>;
    fn input(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError>;
    fn resize(&mut self, size: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError>;
    fn close(&mut self) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError>;
}

/// Deterministic runtime useful for transport tests and simple smoke callers.
#[derive(Clone, Copy, Debug, Default)]
pub struct EchoJavaScriptRuntime;

impl JavaScriptRuntime for EchoJavaScriptRuntime {
    fn start(&mut self, _size: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
        Ok(Vec::new())
    }

    fn input(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
        Ok(vec![bytes.to_vec()])
    }

    fn resize(&mut self, _size: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
        Ok(Vec::new())
    }
    fn close(&mut self) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
        Ok(Vec::new())
    }
}

#[cfg(not(target_os = "ios"))]
pub struct QuickJsJavaScriptRuntime {
    runtime: festerm_automation_js::QuickJsProgramRuntime,
}

#[cfg(not(target_os = "ios"))]
impl QuickJsJavaScriptRuntime {
    pub fn developer_echo() -> Result<Self, JavaScriptSessionError> {
        let program = festerm_automation_js::program::developer_echo_program().map_err(|_| {
            JavaScriptSessionError::Runtime(JavaScriptRuntimeError::new(
                "JavaScript program source is invalid",
            ))
        })?;
        let runtime = festerm_automation_js::QuickJsProgramRuntime::new(program).map_err(|_| {
            JavaScriptSessionError::Runtime(JavaScriptRuntimeError::new(
                "JavaScript runtime could not start",
            ))
        })?;
        Ok(Self { runtime })
    }
}

#[cfg(not(target_os = "ios"))]
impl JavaScriptRuntime for QuickJsJavaScriptRuntime {
    fn cancellation_flag(&self) -> Option<Arc<AtomicBool>> {
        Some(self.runtime.cancellation_flag())
    }

    fn start(&mut self, size: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
        self.runtime
            .start(program_size(size))
            .map_err(|_| JavaScriptRuntimeError::new("JavaScript program start failed"))
    }

    fn input(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
        self.runtime
            .input(bytes)
            .map_err(|_| JavaScriptRuntimeError::new("JavaScript program input failed"))
    }

    fn resize(&mut self, size: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
        self.runtime
            .resize(program_size(size))
            .map_err(|_| JavaScriptRuntimeError::new("JavaScript program resize failed"))
    }

    fn close(&mut self) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
        self.runtime
            .close()
            .map_err(|_| JavaScriptRuntimeError::new("JavaScript program close failed"))
    }
}

#[cfg(not(target_os = "ios"))]
pub struct ProcessJavaScriptRuntime {
    worker: festerm_automation_js::WorkerProgram,
    program: Option<festerm_automation_js::program::ApprovedProgram>,
    supervisor: Option<festerm_automation_js::ProgramSupervisor>,
    generation: u64,
    cancellation: Arc<AtomicBool>,
}

#[cfg(not(target_os = "ios"))]
impl ProcessJavaScriptRuntime {
    pub fn current_executable_developer_echo() -> Result<Self, JavaScriptSessionError> {
        let executable = std::env::current_exe().map_err(|_| {
            JavaScriptSessionError::Runtime(JavaScriptRuntimeError::new(
                "JavaScript worker executable could not be resolved",
            ))
        })?;
        let worker = festerm_automation_js::WorkerProgram::from_install_owned_absolute(&executable)
            .map_err(|_| {
                JavaScriptSessionError::Runtime(JavaScriptRuntimeError::new(
                    "JavaScript worker executable is not trusted",
                ))
            })?;
        Self::developer_echo(worker)
    }

    pub fn developer_echo(
        worker: festerm_automation_js::WorkerProgram,
    ) -> Result<Self, JavaScriptSessionError> {
        let program = festerm_automation_js::program::developer_echo_program().map_err(|_| {
            JavaScriptSessionError::Runtime(JavaScriptRuntimeError::new(
                "JavaScript program source is invalid",
            ))
        })?;
        Ok(Self {
            worker,
            program: Some(program),
            supervisor: None,
            generation: 1,
            cancellation: Arc::new(AtomicBool::new(false)),
        })
    }
}

#[cfg(not(target_os = "ios"))]
impl JavaScriptRuntime for ProcessJavaScriptRuntime {
    fn cancellation_flag(&self) -> Option<Arc<AtomicBool>> {
        Some(self.cancellation.clone())
    }

    fn start(&mut self, size: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
        let program = self.program.take().ok_or_else(|| {
            JavaScriptRuntimeError::new("JavaScript program has already been started")
        })?;
        let mut supervisor = festerm_automation_js::ProgramSupervisor::spawn_with_cancellation(
            &self.worker,
            program,
            self.generation,
            program_size(size),
            self.cancellation.clone(),
        )
        .map_err(|_| JavaScriptRuntimeError::new("JavaScript program worker could not start"))?;
        let output = supervisor.take_start_output();
        self.supervisor = Some(supervisor);
        Ok(output)
    }

    fn input(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
        self.supervisor
            .as_mut()
            .ok_or_else(|| JavaScriptRuntimeError::new("JavaScript program is not started"))?
            .input(bytes)
            .map_err(|_| JavaScriptRuntimeError::new("JavaScript program input failed"))
    }

    fn resize(&mut self, size: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
        self.supervisor
            .as_mut()
            .ok_or_else(|| JavaScriptRuntimeError::new("JavaScript program is not started"))?
            .resize(program_size(size))
            .map_err(|_| JavaScriptRuntimeError::new("JavaScript program resize failed"))
    }

    fn close(&mut self) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
        let Some(mut supervisor) = self.supervisor.take() else {
            return Ok(Vec::new());
        };
        let output = supervisor.close().map_err(|_| {
            JavaScriptRuntimeError::new("JavaScript program cooperative close failed")
        })?;
        supervisor
            .shutdown()
            .map_err(|_| JavaScriptRuntimeError::new("JavaScript program cleanup failed"))?;
        Ok(output)
    }
}

#[cfg(not(target_os = "ios"))]
fn program_size(size: TerminalSize) -> festerm_automation_js::program::TerminalSize {
    festerm_automation_js::program::TerminalSize {
        columns: size.columns(),
        rows: size.rows(),
    }
}

/// Failure to create the session worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JavaScriptSessionError {
    InvalidQueueCapacity,
    WorkerSpawn,
    Runtime(JavaScriptRuntimeError),
}

impl fmt::Display for JavaScriptSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidQueueCapacity => {
                formatter.write_str("JavaScript session queue capacities must be nonzero")
            }
            Self::WorkerSpawn => formatter.write_str("could not start JavaScript session worker"),
            Self::Runtime(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for JavaScriptSessionError {}

enum SessionCommand {
    Input { generation: u64, bytes: Vec<u8> },
    Resize { generation: u64, size: TerminalSize },
}

struct Shared {
    id: SessionId,
    lifecycle: Mutex<SessionLifecycle>,
    metrics: Mutex<SessionMetrics>,
    command_capacity: usize,
    commands: Mutex<VecDeque<SessionCommand>>,
    command_ready: Condvar,
    admission: Mutex<()>,
    output_space: Mutex<()>,
    output_ready: Condvar,
    generation: AtomicU64,
    stopping: AtomicBool,
    event_sender: SyncSender<SessionEvent>,
    control_sender: SyncSender<SessionEvent>,
    event_notifier: Arc<dyn SessionEventNotifier>,
    cancellation: Option<Arc<AtomicBool>>,
}

impl Shared {
    fn lifecycle(&self) -> SessionLifecycle {
        self.lifecycle
            .lock()
            .expect("JavaScript session lifecycle lock is not poisoned")
            .clone()
    }

    fn metrics(&self) -> SessionMetrics {
        *self
            .metrics
            .lock()
            .expect("JavaScript session metrics lock is not poisoned")
    }

    fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn is_current(&self, generation: u64) -> bool {
        !self.stopping.load(Ordering::Acquire) && generation == self.current_generation()
    }

    fn record_event(&self) {
        let mut metrics = self
            .metrics
            .lock()
            .expect("JavaScript session metrics lock is not poisoned");
        metrics.event_queue_depth = metrics.event_queue_depth.saturating_add(1);
        metrics.event_queue_high_watermark = metrics
            .event_queue_high_watermark
            .max(metrics.event_queue_depth);
    }

    fn event_received(&self) {
        let mut metrics = self
            .metrics
            .lock()
            .expect("JavaScript session metrics lock is not poisoned");
        metrics.event_queue_depth = metrics.event_queue_depth.saturating_sub(1);
    }

    fn note_backpressure(&self) {
        let mut metrics = self
            .metrics
            .lock()
            .expect("JavaScript session metrics lock is not poisoned");
        metrics.backpressure_count = metrics.backpressure_count.saturating_add(1);
    }

    fn emit_control(&self, event: SessionEvent) -> bool {
        match self.control_sender.try_send(event) {
            Ok(()) => {
                self.event_notifier.notify();
                true
            }
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    }

    fn request_stop(&self) -> bool {
        let _admission = self
            .admission
            .lock()
            .expect("JavaScript session admission lock is not poisoned");
        let should_stop = {
            let mut lifecycle = self
                .lifecycle
                .lock()
                .expect("JavaScript session lifecycle lock is not poisoned");
            if lifecycle.is_terminal() || matches!(*lifecycle, SessionLifecycle::Stopping) {
                false
            } else {
                *lifecycle = SessionLifecycle::Stopping;
                true
            }
        };
        if should_stop {
            self.stopping.store(true, Ordering::Release);
            self.generation.fetch_add(1, Ordering::AcqRel);
            self.commands
                .lock()
                .expect("JavaScript session command lock is not poisoned")
                .clear();
            self.command_ready.notify_all();
            {
                let _space = self
                    .output_space
                    .lock()
                    .expect("JavaScript session output space lock is not poisoned");
                self.output_ready.notify_all();
            }
            let _ = self.emit_control(SessionEvent::Lifecycle(SessionLifecycle::Stopping));
            true
        } else {
            false
        }
    }

    fn set_terminal(&self, lifecycle: SessionLifecycle) {
        {
            let mut current = self
                .lifecycle
                .lock()
                .expect("JavaScript session lifecycle lock is not poisoned");
            *current = lifecycle.clone();
        }
        self.stopping.store(true, Ordering::Release);
        let _ = self.emit_control(SessionEvent::Lifecycle(lifecycle));
    }

    fn force_cancellation(&self) {
        if let Some(cancellation) = &self.cancellation {
            cancellation.store(true, Ordering::Release);
        }
        self.command_ready.notify_all();
    }

    fn fail(&self, message: &'static str) {
        let error = SessionError::new(SessionErrorKind::Internal, message);
        self.stopping.store(true, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        let _ = self.emit_control(SessionEvent::Error(error.clone()));
        self.set_terminal(SessionLifecycle::Failed(error));
    }
}

/// A JavaScript session that implements the ordinary bounded session contract.
///
/// It deliberately exposes no automation epoch and never owns terminal
/// protocol replies. Automation admission remains outside this slice.
pub struct JavaScriptSession {
    shared: Arc<Shared>,
    event_receiver: Mutex<Receiver<SessionEvent>>,
    control_receiver: Mutex<Receiver<SessionEvent>>,
    completion_receiver: Mutex<Receiver<Result<ShutdownResult, SessionError>>>,
    completion: Mutex<Option<Result<ShutdownResult, SessionError>>>,
}

impl JavaScriptSession {
    pub fn start(
        runtime: impl JavaScriptRuntime,
        size: TerminalSize,
    ) -> Result<Self, JavaScriptSessionError> {
        Self::start_with_notifier(runtime, size, noop_session_event_notifier())
    }

    pub fn start_with_notifier(
        runtime: impl JavaScriptRuntime,
        size: TerminalSize,
        event_notifier: Arc<dyn SessionEventNotifier>,
    ) -> Result<Self, JavaScriptSessionError> {
        Self::start_with_capacities(
            runtime,
            size,
            event_notifier,
            DEFAULT_EVENT_QUEUE_CAPACITY,
            DEFAULT_COMMAND_QUEUE_CAPACITY,
        )
    }

    fn start_with_capacities(
        mut runtime: impl JavaScriptRuntime,
        size: TerminalSize,
        event_notifier: Arc<dyn SessionEventNotifier>,
        event_capacity: usize,
        command_capacity: usize,
    ) -> Result<Self, JavaScriptSessionError> {
        if event_capacity == 0 || command_capacity == 0 {
            return Err(JavaScriptSessionError::InvalidQueueCapacity);
        }
        let (event_sender, event_receiver) = mpsc::sync_channel(event_capacity);
        let (control_sender, control_receiver) = mpsc::sync_channel(DEFAULT_EVENT_QUEUE_CAPACITY);
        let (completion_sender, completion_receiver) = mpsc::sync_channel(1);
        let cancellation = runtime.cancellation_flag();
        let shared = Arc::new(Shared {
            id: SessionId::next(),
            lifecycle: Mutex::new(SessionLifecycle::Starting),
            metrics: Mutex::new(SessionMetrics {
                event_queue_capacity: event_capacity,
                ..SessionMetrics::default()
            }),
            command_capacity,
            commands: Mutex::new(VecDeque::with_capacity(command_capacity)),
            command_ready: Condvar::new(),
            admission: Mutex::new(()),
            output_space: Mutex::new(()),
            output_ready: Condvar::new(),
            generation: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
            event_sender,
            control_sender,
            event_notifier,
            cancellation,
        });
        let _ = shared.emit_control(SessionEvent::Lifecycle(SessionLifecycle::Starting));
        let worker_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name(format!("festerm-javascript-{}", shared.id))
            .spawn(move || {
                let panic_shared = Arc::clone(&worker_shared);
                let worker_completion = completion_sender.clone();
                if catch_unwind(AssertUnwindSafe(|| {
                    runtime_worker(worker_shared, &mut runtime, size, worker_completion);
                }))
                .is_err()
                {
                    panic_shared.fail("JavaScript session worker panicked");
                    let error = SessionError::new(
                        SessionErrorKind::Internal,
                        "JavaScript session worker panicked",
                    );
                    let _ = completion_sender.send(Err(error));
                }
            })
            .map_err(|_| JavaScriptSessionError::WorkerSpawn)?;

        Ok(Self {
            shared,
            event_receiver: Mutex::new(event_receiver),
            control_receiver: Mutex::new(control_receiver),
            completion_receiver: Mutex::new(completion_receiver),
            completion: Mutex::new(None),
        })
    }
}

impl Session for JavaScriptSession {
    fn id(&self) -> SessionId {
        self.shared.id
    }

    fn lifecycle(&self) -> SessionLifecycle {
        self.shared.lifecycle()
    }

    fn metrics(&self) -> SessionMetrics {
        self.shared.metrics()
    }

    fn try_send_input(&self, bytes: &[u8]) -> Result<(), SessionSendError> {
        if bytes.len() > MAX_IO_CHUNK_BYTES {
            return Err(SessionSendError::TooLarge {
                operation: SessionOperation::Input,
                maximum: MAX_IO_CHUNK_BYTES,
                actual: bytes.len(),
            });
        }
        let command = SessionCommand::Input {
            generation: self.shared.current_generation(),
            bytes: bytes.to_vec(),
        };
        self.shared.enqueue(command, SessionOperation::Input)
    }

    fn try_resize(&self, size: TerminalSize) -> Result<(), SessionSendError> {
        let command = SessionCommand::Resize {
            generation: self.shared.current_generation(),
            size,
        };
        self.shared.enqueue(command, SessionOperation::Resize)
    }

    fn try_shutdown(&self) -> Result<(), SessionSendError> {
        if self.shared.lifecycle().is_terminal() {
            return Err(SessionSendError::Closed {
                operation: SessionOperation::Shutdown,
            });
        }
        self.shared.request_stop();
        Ok(())
    }

    fn try_recv_event(&self) -> Result<SessionEvent, SessionTryReceiveError> {
        let _space = self
            .shared
            .output_space
            .lock()
            .expect("JavaScript session output space lock is not poisoned");
        match self
            .event_receiver
            .lock()
            .expect("JavaScript session event receiver lock is not poisoned")
            .try_recv()
        {
            Ok(event) => {
                self.shared.event_received();
                self.shared.output_ready.notify_all();
                return Ok(event);
            }
            Err(TryRecvError::Disconnected) => return Err(SessionTryReceiveError::Closed),
            Err(TryRecvError::Empty) => {}
        }
        match self
            .control_receiver
            .lock()
            .expect("JavaScript session control receiver lock is not poisoned")
            .try_recv()
        {
            Ok(event) => Ok(event),
            Err(TryRecvError::Empty) => Err(SessionTryReceiveError::Empty),
            Err(TryRecvError::Disconnected) => Err(SessionTryReceiveError::Closed),
        }
    }

    fn reserve_automation_input(
        &self,
        _controller_instance: u64,
        _bytes: &[u8],
    ) -> Result<
        festerm_session::ReservedAutomationInput,
        festerm_session::ReserveAutomationInputError,
    > {
        Err(festerm_session::ReserveAutomationInputError::Unsupported)
    }

    fn shutdown(&self, timeout: Duration) -> Result<ShutdownResult, ShutdownError> {
        let _ = self.try_shutdown();
        let mut completion = self
            .completion
            .lock()
            .expect("JavaScript session completion lock is not poisoned");
        if let Some(result) = completion.clone() {
            return result.map_err(ShutdownError::Failed);
        }
        match self
            .completion_receiver
            .lock()
            .expect("JavaScript session completion receiver lock is not poisoned")
            .recv_timeout(timeout)
        {
            Ok(result) => {
                *completion = Some(result.clone());
                result.map_err(ShutdownError::Failed)
            }
            Err(RecvTimeoutError::Timeout) => {
                self.shared.force_cancellation();
                Err(ShutdownError::TimedOut { timeout })
            }
            Err(RecvTimeoutError::Disconnected) => Err(ShutdownError::Failed(SessionError::new(
                SessionErrorKind::Shutdown,
                "JavaScript session worker ended without reporting shutdown completion",
            ))),
        }
    }
}

impl Drop for JavaScriptSession {
    fn drop(&mut self) {
        let _ = self.try_shutdown();
        self.shared.force_cancellation();
    }
}

impl Shared {
    fn enqueue(
        &self,
        command: SessionCommand,
        operation: SessionOperation,
    ) -> Result<(), SessionSendError> {
        let _admission = self
            .admission
            .lock()
            .expect("JavaScript session admission lock is not poisoned");
        if self.stopping.load(Ordering::Acquire) {
            return Err(SessionSendError::Closed { operation });
        }
        let accepted_input_bytes = match &command {
            SessionCommand::Input { bytes, .. } => Some(bytes.len() as u64),
            SessionCommand::Resize { .. } => None,
        };
        let mut commands = self
            .commands
            .lock()
            .expect("JavaScript session command lock is not poisoned");
        if commands.len() >= self.command_capacity {
            return Err(SessionSendError::Full {
                operation,
                capacity: self.command_capacity,
            });
        }
        commands.push_back(command);
        if let Some(bytes) = accepted_input_bytes {
            let mut metrics = self
                .metrics
                .lock()
                .expect("JavaScript session metrics lock is not poisoned");
            metrics.input_bytes = metrics.input_bytes.saturating_add(bytes);
        }
        self.command_ready.notify_one();
        Ok(())
    }

    fn next_command(&self) -> Option<SessionCommand> {
        let mut commands = self
            .commands
            .lock()
            .expect("JavaScript session command lock is not poisoned");
        loop {
            if self.stopping.load(Ordering::Acquire) {
                return None;
            }
            if let Some(command) = commands.pop_front() {
                return Some(command);
            }
            commands = self
                .command_ready
                .wait(commands)
                .expect("JavaScript session command lock is not poisoned");
        }
    }
}

fn runtime_worker(
    shared: Arc<Shared>,
    runtime: &mut impl JavaScriptRuntime,
    size: TerminalSize,
    completion: SyncSender<Result<ShutdownResult, SessionError>>,
) {
    let startup_generation = shared.current_generation();
    if !shared.stopping.load(Ordering::Acquire) {
        match runtime.start(size) {
            Ok(chunks) if shared.is_current(startup_generation) => {
                let mut lifecycle = shared
                    .lifecycle
                    .lock()
                    .expect("JavaScript session lifecycle lock is not poisoned");
                *lifecycle = SessionLifecycle::Running;
                drop(lifecycle);
                let _ = shared.emit_control(SessionEvent::Lifecycle(SessionLifecycle::Running));
                if !validate_output(&chunks) {
                    shared.fail("JavaScript runtime exceeded the output bound");
                } else {
                    for chunk in chunks {
                        if !deliver_output(&shared, startup_generation, chunk) {
                            break;
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(_) if shared.stopping.load(Ordering::Acquire) => {}
            Err(_) => {
                shared.fail("JavaScript runtime could not start");
            }
        }
    }

    while let Some(command) = shared.next_command() {
        match command {
            SessionCommand::Input { generation, bytes } => {
                if !shared.is_current(generation) {
                    continue;
                }
                for bytes in bytes.chunks(festerm_automation_js::program::MAX_INPUT_CHUNK_BYTES) {
                    if !shared.is_current(generation) {
                        break;
                    }
                    match runtime.input(bytes) {
                        Ok(chunks) => {
                            if !validate_output(&chunks) {
                                shared.fail("JavaScript runtime exceeded the output bound");
                                break;
                            }
                            for chunk in chunks {
                                if !deliver_output(&shared, generation, chunk) {
                                    break;
                                }
                            }
                        }
                        Err(_) => {
                            shared.fail("JavaScript runtime input failed");
                            break;
                        }
                    }
                }
            }
            SessionCommand::Resize { generation, size } => {
                if !shared.is_current(generation) {
                    continue;
                }
                match runtime.resize(size) {
                    Ok(chunks) if shared.is_current(generation) => {
                        if !validate_output(&chunks) {
                            shared.fail("JavaScript runtime exceeded the output bound");
                            break;
                        }
                        for chunk in chunks {
                            if !deliver_output(&shared, generation, chunk) {
                                break;
                            }
                        }
                        let mut metrics = shared
                            .metrics
                            .lock()
                            .expect("JavaScript session metrics lock is not poisoned");
                        metrics.resize_count = metrics.resize_count.saturating_add(1);
                        drop(metrics);
                        if !shared.emit_control(SessionEvent::ResizeApplied(size)) {
                            shared.fail("JavaScript session control queue is full");
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {
                        shared.fail("JavaScript runtime resize failed");
                        break;
                    }
                }
            }
        }
    }

    let close_result = runtime.close();
    let already_failed = matches!(shared.lifecycle(), SessionLifecycle::Failed(_));
    let result = match close_result {
        Ok(chunks) if !already_failed && validate_output(&chunks) => {
            if chunks
                .into_iter()
                .all(|chunk| deliver_closing_output(&shared, chunk))
            {
                shared.set_terminal(SessionLifecycle::Stopped);
                Ok(ShutdownResult::Stopped)
            } else {
                shared.fail("JavaScript runtime close output could not be delivered");
                Err(SessionError::new(
                    SessionErrorKind::Shutdown,
                    "JavaScript runtime close output could not be delivered",
                ))
            }
        }
        Ok(_) if !already_failed => {
            shared.fail("JavaScript runtime exceeded the close output bound");
            Err(SessionError::new(
                SessionErrorKind::Shutdown,
                "JavaScript runtime exceeded the close output bound",
            ))
        }
        Ok(_) => match shared.lifecycle() {
            SessionLifecycle::Failed(error) => Err(error),
            _ => unreachable!("failed lifecycle changed during shutdown"),
        },
        Err(_) if already_failed => match shared.lifecycle() {
            SessionLifecycle::Failed(error) => Err(error),
            _ => unreachable!("failed lifecycle changed during shutdown"),
        },
        Err(_) => {
            shared.fail("JavaScript runtime close failed");
            Err(SessionError::new(
                SessionErrorKind::Shutdown,
                "JavaScript runtime close failed",
            ))
        }
    };
    let _ = completion.send(result);
}

fn deliver_closing_output(shared: &Shared, bytes: Vec<u8>) -> bool {
    loop {
        let admission = shared
            .admission
            .lock()
            .expect("JavaScript session admission lock is not poisoned");
        let mut space = shared
            .output_space
            .lock()
            .expect("JavaScript session output space lock is not poisoned");
        if !matches!(shared.lifecycle(), SessionLifecycle::Stopping) {
            return false;
        }
        match shared
            .event_sender
            .try_send(SessionEvent::Output(bytes.clone()))
        {
            Ok(()) => {
                shared.record_event();
                let mut metrics = shared
                    .metrics
                    .lock()
                    .expect("JavaScript session metrics lock is not poisoned");
                metrics.output_bytes = metrics.output_bytes.saturating_add(bytes.len() as u64);
                shared.event_notifier.notify();
                return true;
            }
            Err(TrySendError::Full(_)) => {
                shared.note_backpressure();
                drop(admission);
                space = shared
                    .output_ready
                    .wait(space)
                    .expect("JavaScript session output space lock is not poisoned");
                drop(space);
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

fn validate_output(chunks: &[Vec<u8>]) -> bool {
    chunks
        .iter()
        .try_fold(0usize, |total, chunk| {
            (!chunk.is_empty() && chunk.len() <= MAX_IO_CHUNK_BYTES)
                .then(|| total.checked_add(chunk.len()))
                .flatten()
        })
        .is_some_and(|total| total <= MAX_RUNTIME_OUTPUT_BYTES)
}

fn deliver_output(shared: &Shared, generation: u64, bytes: Vec<u8>) -> bool {
    loop {
        let admission = shared
            .admission
            .lock()
            .expect("JavaScript session admission lock is not poisoned");
        let mut space = shared
            .output_space
            .lock()
            .expect("JavaScript session output space lock is not poisoned");
        if !shared.is_current(generation) {
            return false;
        }
        match shared
            .event_sender
            .try_send(SessionEvent::Output(bytes.clone()))
        {
            Ok(()) => {
                shared.record_event();
                {
                    let mut metrics = shared
                        .metrics
                        .lock()
                        .expect("JavaScript session metrics lock is not poisoned");
                    metrics.output_bytes = metrics.output_bytes.saturating_add(bytes.len() as u64);
                }
                shared.event_notifier.notify();
                return true;
            }
            Err(TrySendError::Full(_)) => {
                shared.note_backpressure();
                drop(admission);
                space = shared
                    .output_ready
                    .wait(space)
                    .expect("JavaScript session output space lock is not poisoned");
                drop(space);
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_session::ReserveAutomationInputError;
    use std::{
        sync::{atomic::AtomicUsize, mpsc, Condvar},
        time::Instant,
    };

    fn size() -> TerminalSize {
        TerminalSize::new(80, 24).unwrap()
    }

    fn wait_for(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !predicate() {
            assert!(Instant::now() < deadline, "condition was not reached");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[derive(Default)]
    struct CountingNotifier(AtomicUsize);

    impl SessionEventNotifier for CountingNotifier {
        fn notify(&self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn input_and_resize_are_delivered_in_command_order() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        struct RecordingRuntime {
            calls: Arc<Mutex<Vec<&'static str>>>,
        }
        impl JavaScriptRuntime for RecordingRuntime {
            fn start(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                self.calls.lock().unwrap().push("start");
                Ok(Vec::new())
            }
            fn input(&mut self, _: &[u8]) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                self.calls.lock().unwrap().push("input");
                Ok(Vec::new())
            }
            fn resize(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                self.calls.lock().unwrap().push("resize");
                Ok(Vec::new())
            }
            fn close(&mut self) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
        }

        let session = JavaScriptSession::start(
            RecordingRuntime {
                calls: Arc::clone(&calls),
            },
            size(),
        )
        .unwrap();
        wait_for(|| session.lifecycle() == SessionLifecycle::Running);
        session.try_send_input(b"one").unwrap();
        session
            .try_resize(TerminalSize::new(100, 30).unwrap())
            .unwrap();
        wait_for(|| session.metrics().resize_count == 1);
        assert_eq!(*calls.lock().unwrap(), ["start", "input", "resize"]);
        assert_eq!(
            session.shutdown(Duration::from_secs(1)),
            Ok(ShutdownResult::Stopped)
        );
    }

    #[test]
    fn command_queue_is_bounded_while_runtime_is_busy() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (entered_sender, entered_receiver) = mpsc::channel();
        struct BlockingRuntime {
            entered: mpsc::Sender<()>,
            gate: Arc<(Mutex<bool>, Condvar)>,
        }
        impl JavaScriptRuntime for BlockingRuntime {
            fn start(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
            fn input(&mut self, _: &[u8]) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                self.entered.send(()).unwrap();
                let (lock, ready) = &*self.gate;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = ready.wait(released).unwrap();
                }
                Ok(Vec::new())
            }
            fn resize(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
            fn close(&mut self) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                let (_, ready) = &*self.gate;
                ready.notify_all();
                Ok(Vec::new())
            }
        }

        let session = JavaScriptSession::start_with_capacities(
            BlockingRuntime {
                entered: entered_sender,
                gate: Arc::clone(&gate),
            },
            size(),
            noop_session_event_notifier(),
            4,
            2,
        )
        .unwrap();
        wait_for(|| session.lifecycle() == SessionLifecycle::Running);
        session.try_send_input(b"busy").unwrap();
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        session.try_send_input(b"queued-one").unwrap();
        session.try_send_input(b"queued-two").unwrap();
        assert!(matches!(
            session.try_send_input(b"full"),
            Err(SessionSendError::Full {
                operation: SessionOperation::Input,
                ..
            })
        ));
        {
            let (lock, ready) = &*gate;
            *lock.lock().unwrap() = true;
            ready.notify_all();
        }
        assert_eq!(
            session.shutdown(Duration::from_secs(1)),
            Ok(ShutdownResult::Stopped)
        );
    }

    #[test]
    fn stale_runtime_output_is_suppressed_during_close() {
        let (entered_sender, entered_receiver) = mpsc::channel();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        struct DelayedEcho {
            entered: mpsc::Sender<()>,
            gate: Arc<(Mutex<bool>, Condvar)>,
        }
        impl JavaScriptRuntime for DelayedEcho {
            fn start(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
            fn input(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                self.entered.send(()).unwrap();
                let (lock, ready) = &*self.gate;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = ready.wait(released).unwrap();
                }
                Ok(vec![bytes.to_vec()])
            }
            fn resize(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
            fn close(&mut self) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
        }

        let session = JavaScriptSession::start(
            DelayedEcho {
                entered: entered_sender,
                gate: Arc::clone(&gate),
            },
            size(),
        )
        .unwrap();
        wait_for(|| session.lifecycle() == SessionLifecycle::Running);
        session.try_send_input(b"stale").unwrap();
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        session.try_shutdown().unwrap();
        {
            let (lock, ready) = &*gate;
            *lock.lock().unwrap() = true;
            ready.notify_all();
        }
        assert_eq!(
            session.shutdown(Duration::from_secs(1)),
            Ok(ShutdownResult::Stopped)
        );
        loop {
            match session.try_recv_event() {
                Ok(SessionEvent::Output(output)) => panic!("stale output escaped: {output:?}"),
                Ok(_) => {}
                Err(SessionTryReceiveError::Empty | SessionTryReceiveError::Closed) => break,
            }
        }
    }

    #[test]
    fn a_full_output_queue_applies_cooperative_backpressure() {
        struct ThreeChunks;
        impl JavaScriptRuntime for ThreeChunks {
            fn start(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
            fn input(&mut self, _: &[u8]) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()])
            }
            fn resize(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
            fn close(&mut self) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
        }

        let session = JavaScriptSession::start_with_capacities(
            ThreeChunks,
            size(),
            noop_session_event_notifier(),
            2,
            2,
        )
        .unwrap();
        wait_for(|| session.lifecycle() == SessionLifecycle::Running);
        while session.try_recv_event().is_ok() {}
        session.try_send_input(b"go").unwrap();
        wait_for(|| session.metrics().backpressure_count > 0);
        assert!(session.metrics().event_queue_depth <= 2);
        assert_eq!(
            session.shutdown(Duration::from_secs(1)),
            Ok(ShutdownResult::Stopped)
        );
    }

    #[test]
    fn session_has_no_automation_or_terminal_reply_authority() {
        let session = JavaScriptSession::start(EchoJavaScriptRuntime, size()).unwrap();
        wait_for(|| session.lifecycle() == SessionLifecycle::Running);
        assert!(session.automation_epoch().is_none());
        assert!(!session.terminal_replies_owned_by_backend());
        assert!(matches!(
            session.reserve_automation_input(7, b"forbidden"),
            Err(ReserveAutomationInputError::Unsupported)
        ));
        assert_eq!(
            session.shutdown(Duration::from_secs(1)),
            Ok(ShutdownResult::Stopped)
        );
    }

    #[test]
    fn notifier_fires_only_after_events_are_enqueued() {
        let notifier = Arc::new(CountingNotifier::default());
        let session = JavaScriptSession::start_with_notifier(
            EchoJavaScriptRuntime,
            size(),
            Arc::clone(&notifier) as Arc<dyn SessionEventNotifier>,
        )
        .unwrap();
        wait_for(|| session.lifecycle() == SessionLifecycle::Running);
        session.try_send_input(b"echo").unwrap();
        wait_for(|| session.metrics().event_queue_depth >= 1);
        assert!(notifier.0.load(Ordering::Relaxed) >= 3);
        assert_eq!(
            session.shutdown(Duration::from_secs(1)),
            Ok(ShutdownResult::Stopped)
        );
    }

    #[test]
    fn oversized_input_is_rejected_without_entering_the_runtime() {
        let session = JavaScriptSession::start(EchoJavaScriptRuntime, size()).unwrap();
        wait_for(|| session.lifecycle() == SessionLifecycle::Running);
        assert!(matches!(
            session.try_send_input(&vec![0; MAX_IO_CHUNK_BYTES + 1]),
            Err(SessionSendError::TooLarge {
                operation: SessionOperation::Input,
                maximum: MAX_IO_CHUNK_BYTES,
                ..
            })
        ));
        assert_eq!(session.metrics().input_bytes, 0);
        assert_eq!(
            session.shutdown(Duration::from_secs(1)),
            Ok(ShutdownResult::Stopped)
        );
    }

    #[test]
    fn accepted_large_input_is_split_at_program_protocol_boundaries() {
        let (chunks_sender, chunks_receiver) = mpsc::channel();
        struct ChunkRecordingRuntime {
            chunks: mpsc::Sender<usize>,
        }
        impl JavaScriptRuntime for ChunkRecordingRuntime {
            fn start(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
            fn input(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                self.chunks.send(bytes.len()).unwrap();
                Ok(Vec::new())
            }
            fn resize(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
            fn close(&mut self) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
        }

        let session = JavaScriptSession::start(
            ChunkRecordingRuntime {
                chunks: chunks_sender,
            },
            size(),
        )
        .unwrap();
        wait_for(|| session.lifecycle() == SessionLifecycle::Running);
        let chunk_limit = festerm_automation_js::program::MAX_INPUT_CHUNK_BYTES;
        session
            .try_send_input(&vec![b'x'; chunk_limit * 2 + 1])
            .unwrap();
        let observed: Vec<_> = (0..3)
            .map(|_| {
                chunks_receiver
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
            })
            .collect();
        assert_eq!(observed, [chunk_limit, chunk_limit, 1]);
        assert_eq!(
            session.shutdown(Duration::from_secs(1)),
            Ok(ShutdownResult::Stopped)
        );
    }

    #[test]
    fn shutdown_between_input_fragments_cuts_the_remainder() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let (entered_sender, entered_receiver) = mpsc::channel();
        struct FragmentRuntime {
            gate: Arc<(Mutex<bool>, Condvar)>,
            calls: Arc<AtomicUsize>,
            entered: mpsc::Sender<()>,
        }
        impl JavaScriptRuntime for FragmentRuntime {
            fn start(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
            fn input(&mut self, _: &[u8]) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
                    self.entered.send(()).unwrap();
                    let (lock, ready) = &*self.gate;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = ready.wait(released).unwrap();
                    }
                }
                Ok(Vec::new())
            }
            fn resize(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
            fn close(&mut self) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
        }

        let session = JavaScriptSession::start(
            FragmentRuntime {
                gate: Arc::clone(&gate),
                calls: Arc::clone(&calls),
                entered: entered_sender,
            },
            size(),
        )
        .unwrap();
        wait_for(|| session.lifecycle() == SessionLifecycle::Running);
        let fragment = festerm_automation_js::program::MAX_INPUT_CHUNK_BYTES;
        session.try_send_input(&vec![b'x'; fragment * 2]).unwrap();
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        session.try_shutdown().unwrap();
        {
            let (lock, ready) = &*gate;
            *lock.lock().unwrap() = true;
            ready.notify_all();
        }
        assert_eq!(
            session.shutdown(Duration::from_secs(1)),
            Ok(ShutdownResult::Stopped)
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn close_output_drains_before_terminal_stop() {
        struct ClosingRuntime;
        impl JavaScriptRuntime for ClosingRuntime {
            fn start(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
            fn input(&mut self, _: &[u8]) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
            fn resize(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
            fn close(&mut self) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(vec![b"closed".to_vec()])
            }
        }

        let session = JavaScriptSession::start(ClosingRuntime, size()).unwrap();
        wait_for(|| session.lifecycle() == SessionLifecycle::Running);
        assert_eq!(
            session.shutdown(Duration::from_secs(1)),
            Ok(ShutdownResult::Stopped)
        );
        let mut saw_close_output = false;
        loop {
            match session.try_recv_event() {
                Ok(SessionEvent::Output(bytes)) if bytes == b"closed" => saw_close_output = true,
                Ok(SessionEvent::Lifecycle(SessionLifecycle::Stopped)) => {
                    assert!(saw_close_output, "close output was overtaken by Stopped");
                    break;
                }
                Ok(_) => {}
                Err(SessionTryReceiveError::Empty | SessionTryReceiveError::Closed) => {
                    panic!("Stopped lifecycle was not delivered")
                }
            }
        }
    }

    #[test]
    fn runtime_failure_closes_the_session_as_failed() {
        struct FailingRuntime;
        impl JavaScriptRuntime for FailingRuntime {
            fn start(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Err(JavaScriptRuntimeError::new("runtime start failed"))
            }
            fn input(&mut self, _: &[u8]) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                unreachable!()
            }
            fn resize(&mut self, _: TerminalSize) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                unreachable!()
            }
            fn close(&mut self) -> Result<Vec<Vec<u8>>, JavaScriptRuntimeError> {
                Ok(Vec::new())
            }
        }

        let session = JavaScriptSession::start(FailingRuntime, size()).unwrap();
        wait_for(|| matches!(session.lifecycle(), SessionLifecycle::Failed(_)));
        assert!(matches!(
            session.shutdown(Duration::from_secs(1)),
            Err(ShutdownError::Failed(_))
        ));
    }
}
