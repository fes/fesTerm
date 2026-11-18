use crate::{
    platform,
    program::{
        ApprovedProgram, ProgramCapability, ProgramHostMessage, ProgramOutcome, TerminalSize,
        MAX_INPUT_CHUNK_BYTES, MAX_OUTPUT_CHUNK_BYTES, MAX_OUTPUT_QUEUE_BYTES, MAX_PENDING_WRITES,
    },
    HEAP_BYTES, PROGRAM_TURN_CPU_LIMIT, PROGRAM_TURN_TIMEOUT, STACK_BYTES,
};
use rquickjs::{
    loader::{ImportAttributes, Loader, Resolver},
    module::Declared,
    promise::PromiseState,
    Context, Ctx, Function, Module, Object, Runtime, TypedArray,
};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::{sync_channel, Receiver, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const COMMAND_CAPACITY: usize = 16;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_millis(250);

const UNSTARTED: u8 = 0;
const RUNNING: u8 = 1;
const CLOSED: u8 = 2;
const FAILED: u8 = 3;

type Output = Vec<Vec<u8>>;
type Reply = SyncSender<Result<Output, QuickJsProgramRuntimeError>>;

enum Command {
    Start(TerminalSize, Reply),
    Input(Vec<u8>, Reply),
    Resize(TerminalSize, Reply),
    Close(Reply),
    Abort,
}

/// Content-free failures from the developer-only in-process program slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QuickJsProgramRuntimeError {
    #[error("JavaScript program runtime is unavailable on this platform")]
    Unavailable,
    #[error("JavaScript program runtime thread failed")]
    WorkerExited,
    #[error("JavaScript program runtime has already been started")]
    AlreadyStarted,
    #[error("JavaScript program runtime has not been started")]
    NotStarted,
    #[error("JavaScript program runtime is closed")]
    Closed,
    #[error("JavaScript program runtime is busy")]
    Busy,
    #[error("JavaScript program runtime operation timed out")]
    Deadline,
    #[error("JavaScript program input is invalid or too large")]
    Input,
    #[error("JavaScript program lacks a required capability")]
    Capability,
    #[error("JavaScript program source failed to compile or execute")]
    Script,
    #[error("JavaScript program terminal output is out of bounds")]
    Output,
    #[error("JavaScript program event handler is missing")]
    Handler,
}

struct Sources(BTreeMap<String, String>);

impl Loader for Sources {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js, Declared>> {
        if attributes.is_some() {
            return Err(rquickjs::Error::new_loading("denied"));
        }
        let source = self
            .0
            .get(name)
            .ok_or_else(|| rquickjs::Error::new_loading("denied"))?;
        Module::declare(ctx.clone(), name, source.as_bytes())
    }
}

struct Names(BTreeSet<String>);

impl Resolver for Names {
    fn resolve<'js>(
        &mut self,
        _: &Ctx<'js>,
        _: &str,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        if attributes.is_none() && self.0.contains(name) {
            Ok(name.to_owned())
        } else {
            Err(rquickjs::Error::new_resolving("denied", "denied"))
        }
    }
}

const BOOTSTRAP: &str = r#"
(() => {
    "use strict";
    const write = globalThis.__festerm_program_write;
    delete globalThis.__festerm_program_write;
    let size = Object.freeze({columns: 0, rows: 0});
    let handler;
    const events = callback => {
        if (typeof callback !== "function" || handler !== undefined) {
            throw new TypeError("invalid terminal event handler");
        }
        handler = callback;
    };
    const terminal = Object.freeze(Object.defineProperties({}, {
        write: {value: Object.freeze(write), enumerable: true},
        events: {value: Object.freeze(events), enumerable: true},
        size: {get: () => size, enumerable: true}
    }));
    for (const fn of [function(){}, async function(){}, function*(){}, async function*(){}]) {
        Object.defineProperty(Object.getPrototypeOf(fn), "constructor",
            {value: undefined, writable: false, configurable: false});
    }
    for (const name of ["eval", "Function"]) {
        Object.defineProperty(globalThis, name,
            {value: undefined, writable: false, configurable: false});
    }
    return {
        start: (main, initialSize) => {
            size = Object.freeze({columns: initialSize.columns, rows: initialSize.rows});
            const result = main({terminal});
            if (result && typeof result.then === "function") {
                throw new TypeError("async main is not supported by program API v1");
            }
        },
        setSize: next => {
            size = Object.freeze({columns: next.columns, rows: next.rows});
        },
        dispatch: event => {
            if (handler === undefined) throw new Error("missing terminal event handler");
            handler(Object.freeze(event));
        }
    };
})()
"#;

/// Developer-only, default-off, in-process QuickJS terminal program runtime.
///
/// The engine is confined to one dedicated worker thread and retains its state
/// between events. This is deliberately **not** production confinement or GUI
/// process enablement: callers must not expose it to untrusted sources outside a
/// reviewed, separately bounded integration.
pub struct QuickJsProgramRuntime {
    commands: SyncSender<Command>,
    cancel: Arc<AtomicBool>,
    state: Arc<AtomicU8>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl QuickJsProgramRuntime {
    pub fn cancellation_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub fn new(program: ApprovedProgram) -> Result<Self, QuickJsProgramRuntimeError> {
        if ![
            ProgramCapability::TerminalInput,
            ProgramCapability::TerminalOutput,
            ProgramCapability::TerminalResize,
        ]
        .iter()
        .all(|capability| program.manifest().capabilities.contains(capability))
        {
            return Err(QuickJsProgramRuntimeError::Capability);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let state = Arc::new(AtomicU8::new(UNSTARTED));
        let (commands, receive) = sync_channel(COMMAND_CAPACITY);
        let worker_cancel = cancel.clone();
        let worker_state = state.clone();
        let worker = thread::Builder::new()
            .name("festerm-quickjs-program".into())
            .spawn(move || worker_loop(program, receive, worker_cancel, worker_state))
            .map_err(|_| QuickJsProgramRuntimeError::Unavailable)?;
        Ok(Self {
            commands,
            cancel,
            state,
            worker: Mutex::new(Some(worker)),
        })
    }

    pub fn start(&self, size: TerminalSize) -> Result<Output, QuickJsProgramRuntimeError> {
        size.validate()
            .map_err(|_| QuickJsProgramRuntimeError::Input)?;
        if self
            .state
            .compare_exchange(UNSTARTED, RUNNING, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(QuickJsProgramRuntimeError::AlreadyStarted);
        }
        let result = self.request(|reply| Command::Start(size, reply), OPERATION_TIMEOUT);
        match result {
            Ok(output) => Ok(output),
            Err(error) => {
                self.state.store(FAILED, Ordering::Release);
                Err(error)
            }
        }
    }

    pub fn input(&self, bytes: &[u8]) -> Result<Output, QuickJsProgramRuntimeError> {
        self.ensure_running()?;
        if bytes.is_empty() || bytes.len() > MAX_INPUT_CHUNK_BYTES {
            return Err(QuickJsProgramRuntimeError::Input);
        }
        self.request(
            |reply| Command::Input(bytes.to_vec(), reply),
            OPERATION_TIMEOUT,
        )
    }

    pub fn resize(&self, size: TerminalSize) -> Result<Output, QuickJsProgramRuntimeError> {
        self.ensure_running()?;
        size.validate()
            .map_err(|_| QuickJsProgramRuntimeError::Input)?;
        self.request(|reply| Command::Resize(size, reply), OPERATION_TIMEOUT)
    }

    pub fn close(&self) -> Result<Output, QuickJsProgramRuntimeError> {
        match self.state.load(Ordering::Acquire) {
            UNSTARTED => {
                self.cancel.store(true, Ordering::Release);
                self.state.store(CLOSED, Ordering::Release);
                let _ = self.commands.try_send(Command::Abort);
                self.join();
                return Ok(Vec::new());
            }
            CLOSED => return Ok(Vec::new()),
            FAILED => {
                self.cancel.store(true, Ordering::Release);
                let _ = self.commands.try_send(Command::Abort);
                self.join();
                return Err(QuickJsProgramRuntimeError::Script);
            }
            RUNNING => {}
            _ => return Err(QuickJsProgramRuntimeError::WorkerExited),
        }
        let (send, receive) = sync_channel(1);
        match self.commands.try_send(Command::Close(send)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(QuickJsProgramRuntimeError::Busy),
            Err(TrySendError::Disconnected(_)) => {
                return Err(QuickJsProgramRuntimeError::WorkerExited);
            }
        }
        let result = match receive.recv_timeout(CLOSE_TIMEOUT) {
            Ok(result) => result,
            Err(_) => {
                self.cancel.store(true, Ordering::Release);
                receive
                    .recv_timeout(OPERATION_TIMEOUT)
                    .unwrap_or(Err(QuickJsProgramRuntimeError::Deadline))
            }
        };
        if result.is_ok() {
            self.state.store(CLOSED, Ordering::Release);
        } else {
            self.state.store(FAILED, Ordering::Release);
        }
        self.join();
        result
    }

    fn ensure_running(&self) -> Result<(), QuickJsProgramRuntimeError> {
        match self.state.load(Ordering::Acquire) {
            RUNNING => Ok(()),
            CLOSED => Err(QuickJsProgramRuntimeError::Closed),
            UNSTARTED => Err(QuickJsProgramRuntimeError::NotStarted),
            FAILED => Err(QuickJsProgramRuntimeError::Script),
            _ => Err(QuickJsProgramRuntimeError::WorkerExited),
        }
    }

    fn request(
        &self,
        command: impl FnOnce(Reply) -> Command,
        timeout: Duration,
    ) -> Result<Output, QuickJsProgramRuntimeError> {
        let (send, receive) = sync_channel(1);
        match self.commands.try_send(command(send)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(QuickJsProgramRuntimeError::Busy),
            Err(TrySendError::Disconnected(_)) => {
                return Err(QuickJsProgramRuntimeError::WorkerExited);
            }
        }
        match receive.recv_timeout(timeout) {
            Ok(result) => {
                if result.is_err() {
                    self.state.store(FAILED, Ordering::Release);
                }
                result
            }
            Err(_) => {
                self.cancel.store(true, Ordering::Release);
                self.state.store(FAILED, Ordering::Release);
                Err(QuickJsProgramRuntimeError::Deadline)
            }
        }
    }

    fn join(&self) {
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}

impl Drop for QuickJsProgramRuntime {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        let _ = self.commands.try_send(Command::Abort);
        self.join();
    }
}

fn worker_loop(
    program: ApprovedProgram,
    commands: Receiver<Command>,
    cancel: Arc<AtomicBool>,
    state: Arc<AtomicU8>,
) {
    let Ok(runtime) = Runtime::new() else {
        state.store(FAILED, Ordering::Release);
        return;
    };
    runtime.set_memory_limit(HEAP_BYTES);
    runtime.set_max_stack_size(STACK_BYTES);
    let interrupt = cancel.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || interrupt.load(Ordering::Acquire))));
    runtime.set_loader(
        Names(program.snapshot().modules.keys().cloned().collect()),
        Sources(program.snapshot().modules.clone()),
    );
    let Ok(context) = Context::full(&runtime) else {
        state.store(FAILED, Ordering::Release);
        return;
    };
    let result = context.with(|ctx| worker_context(&program, &commands, &ctx, &state));
    if result.is_err() && state.load(Ordering::Acquire) != CLOSED {
        state.store(FAILED, Ordering::Release);
    }
}

fn worker_context<'js>(
    program: &ApprovedProgram,
    commands: &Receiver<Command>,
    ctx: &Ctx<'js>,
    state: &AtomicU8,
) -> rquickjs::Result<()> {
    let output = Rc::new(RefCell::new(Output::new()));
    let output_bytes = Rc::new(RefCell::new(0usize));
    install_program_writer(ctx.clone(), output.clone(), output_bytes.clone())?;
    let bootstrap: Object = ctx.eval(BOOTSTRAP)?;
    let module = Module::declare(
        ctx.clone(),
        program.manifest().entry.as_str(),
        program.snapshot().modules[&program.manifest().entry].as_bytes(),
    )?;
    let (module, evaluated) = module.eval()?;
    for _ in 0..1_024 {
        if evaluated.state() != PromiseState::Pending {
            break;
        }
        if !ctx.execute_pending_job() {
            break;
        }
    }
    evaluated
        .result::<()>()
        .ok_or(rquickjs::Error::Exception)??;

    let start: Function = bootstrap.get("start")?;
    let dispatch: Function = bootstrap.get("dispatch")?;
    let set_size: Function = bootstrap.get("setSize")?;
    let main: Function = module.get("main")?;
    let mut started = false;
    while let Ok(command) = commands.recv() {
        clear_output(&output, &output_bytes);
        match command {
            Command::Start(size, reply) => {
                let size_value = size_object(ctx, size)?;
                let result = start
                    .call::<_, ()>((main.clone(), size_value))
                    .map_err(|_| QuickJsProgramRuntimeError::Script)
                    .and_then(|()| take_output(&output, &output_bytes));
                started = result.is_ok();
                send_reply(reply, result);
            }
            Command::Input(bytes, reply) if started => {
                let event = Object::new(ctx.clone())?;
                event.set("type", "input")?;
                event.set("bytes", TypedArray::<u8>::new(ctx.clone(), bytes)?)?;
                let result = dispatch
                    .call::<_, ()>((event,))
                    .map_err(|_| QuickJsProgramRuntimeError::Script)
                    .and_then(|()| take_output(&output, &output_bytes));
                send_reply(reply, result);
            }
            Command::Resize(size, reply) if started => {
                let size_value = size_object(ctx, size)?;
                set_size.call::<_, ()>((size_value,))?;
                let event = Object::new(ctx.clone())?;
                event.set("type", "resize")?;
                event.set("size", size_object(ctx, size)?)?;
                let result = dispatch
                    .call::<_, ()>((event,))
                    .map_err(|_| QuickJsProgramRuntimeError::Script)
                    .and_then(|()| take_output(&output, &output_bytes));
                send_reply(reply, result);
            }
            Command::Close(reply) if started => {
                let event = Object::new(ctx.clone())?;
                event.set("type", "close")?;
                let result = dispatch
                    .call::<_, ()>((event,))
                    .map_err(|_| QuickJsProgramRuntimeError::Script)
                    .and_then(|()| take_output(&output, &output_bytes));
                let successful = result.is_ok();
                send_reply(reply, result);
                if successful {
                    state.store(CLOSED, Ordering::Release);
                } else {
                    state.store(FAILED, Ordering::Release);
                }
                return Ok(());
            }
            Command::Close(reply) => {
                state.store(CLOSED, Ordering::Release);
                send_reply(reply, Ok(Vec::new()));
                return Ok(());
            }
            Command::Input(_, reply) | Command::Resize(_, reply) => {
                send_reply(reply, Err(QuickJsProgramRuntimeError::NotStarted));
            }
            Command::Abort => {
                state.store(CLOSED, Ordering::Release);
                return Ok(());
            }
        }
        if state.load(Ordering::Acquire) == FAILED {
            return Ok(());
        }
    }
    Ok(())
}

fn size_object<'js>(ctx: &Ctx<'js>, size: TerminalSize) -> rquickjs::Result<Object<'js>> {
    let value = Object::new(ctx.clone())?;
    value.set("columns", size.columns)?;
    value.set("rows", size.rows)?;
    Ok(value)
}

fn clear_output(output: &Rc<RefCell<Output>>, bytes: &Rc<RefCell<usize>>) {
    output.borrow_mut().clear();
    *bytes.borrow_mut() = 0;
}

fn take_output(
    output: &Rc<RefCell<Output>>,
    bytes: &Rc<RefCell<usize>>,
) -> Result<Output, QuickJsProgramRuntimeError> {
    if *bytes.borrow() > MAX_OUTPUT_QUEUE_BYTES {
        return Err(QuickJsProgramRuntimeError::Output);
    }
    let result = std::mem::take(&mut *output.borrow_mut());
    *bytes.borrow_mut() = 0;
    Ok(result)
}

fn send_reply(reply: Reply, result: Result<Output, QuickJsProgramRuntimeError>) {
    let _ = reply.try_send(result);
}

pub(crate) trait InlineProgramTransport {
    fn started(&mut self, generation: u64) -> rquickjs::Result<()>;
    fn receive(&mut self) -> rquickjs::Result<ProgramHostMessage>;
    fn output(&mut self, output: Output) -> rquickjs::Result<()>;
    fn completed(&mut self, generation: u64, sequence: u64) -> rquickjs::Result<()>;
    fn finished(&mut self, generation: u64, outcome: ProgramOutcome) -> rquickjs::Result<()>;
}

/// Executes the program on the already-confined worker's original thread.
///
/// A native program worker cannot create a nested thread after Linux
/// confinement. The desktop development adapter above keeps its own thread;
/// packaged worker entry uses this inline path so process confinement remains
/// the authority boundary.
pub(crate) fn run_inline_program(
    program: ApprovedProgram,
    generation: u64,
    initial_size: TerminalSize,
    transport: &mut impl InlineProgramTransport,
) -> Result<(), crate::Error> {
    let runtime = Runtime::new().map_err(|_| crate::Error::WorkerFailed("QuickJS runtime"))?;
    runtime.set_memory_limit(HEAP_BYTES);
    runtime.set_max_stack_size(STACK_BYTES);
    let active_turn = Rc::new(Cell::new(None::<(Duration, Instant)>));
    let interrupt_turn = active_turn.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        let Some((cpu_start, wall_start)) = interrupt_turn.get() else {
            return false;
        };
        platform::cpu_time().is_none_or(|cpu_now| {
            turn_budget_exhausted(cpu_start, cpu_now, wall_start, Instant::now())
        })
    })));
    runtime.set_loader(
        Names(program.snapshot().modules.keys().cloned().collect()),
        Sources(program.snapshot().modules.clone()),
    );
    let context =
        Context::full(&runtime).map_err(|_| crate::Error::WorkerFailed("QuickJS context"))?;
    context
        .with(|ctx| -> rquickjs::Result<()> {
            begin_inline_turn(&active_turn)?;
            let output = Rc::new(RefCell::new(Output::new()));
            let output_bytes = Rc::new(RefCell::new(0usize));
            install_program_writer(ctx.clone(), output.clone(), output_bytes.clone())?;
            let bootstrap: Object = ctx.eval(BOOTSTRAP)?;
            let module = Module::declare(
                ctx.clone(),
                program.manifest().entry.as_str(),
                program.snapshot().modules[&program.manifest().entry].as_bytes(),
            )?;
            let (module, evaluated) = module.eval()?;
            for _ in 0..1_024 {
                if evaluated.state() != PromiseState::Pending || !ctx.execute_pending_job() {
                    break;
                }
            }
            evaluated
                .result::<()>()
                .ok_or(rquickjs::Error::Exception)??;

            let start: Function = bootstrap.get("start")?;
            let dispatch: Function = bootstrap.get("dispatch")?;
            let set_size: Function = bootstrap.get("setSize")?;
            let main: Function = module.get("main")?;
            start.call::<_, ()>((main, size_object(&ctx, initial_size)?))?;
            transport.output(take_inline_output(&output, &output_bytes)?)?;
            active_turn.set(None);
            transport.started(generation)?;

            let mut sequence = 1_u64;
            loop {
                let message = transport.receive()?;
                if inline_message_generation(&message) != generation {
                    return Err(rquickjs::Error::Exception);
                }
                match message {
                    ProgramHostMessage::Input {
                        sequence: event_sequence,
                        bytes,
                        ..
                    } => {
                        if event_sequence != sequence {
                            return Err(rquickjs::Error::Exception);
                        }
                        begin_inline_turn(&active_turn)?;
                        sequence = sequence.checked_add(1).ok_or(rquickjs::Error::Exception)?;
                        let event = Object::new(ctx.clone())?;
                        event.set("type", "input")?;
                        event.set("bytes", TypedArray::<u8>::new(ctx.clone(), bytes)?)?;
                        dispatch.call::<_, ()>((event,))?;
                        transport.output(take_inline_output(&output, &output_bytes)?)?;
                        active_turn.set(None);
                        transport.completed(generation, event_sequence)?;
                    }
                    ProgramHostMessage::Resize {
                        sequence: event_sequence,
                        size,
                        ..
                    } => {
                        begin_inline_turn(&active_turn)?;
                        if event_sequence != sequence {
                            return Err(rquickjs::Error::Exception);
                        }
                        sequence = sequence.checked_add(1).ok_or(rquickjs::Error::Exception)?;
                        set_size.call::<_, ()>((size_object(&ctx, size)?,))?;
                        let event = Object::new(ctx.clone())?;
                        event.set("type", "resize")?;
                        event.set("size", size_object(&ctx, size)?)?;
                        dispatch.call::<_, ()>((event,))?;
                        transport.output(take_inline_output(&output, &output_bytes)?)?;
                        active_turn.set(None);
                        transport.completed(generation, event_sequence)?;
                    }
                    ProgramHostMessage::Close {
                        sequence: event_sequence,
                        ..
                    } if event_sequence == sequence => {
                        begin_inline_turn(&active_turn)?;
                        let event = Object::new(ctx.clone())?;
                        event.set("type", "close")?;
                        dispatch.call::<_, ()>((event,))?;
                        transport.output(take_inline_output(&output, &output_bytes)?)?;
                        active_turn.set(None);
                        return transport.finished(generation, ProgramOutcome::Completed {});
                    }
                    ProgramHostMessage::Cancel { .. } => {
                        return transport.finished(generation, ProgramOutcome::Cancelled {});
                    }
                    _ => return Err(rquickjs::Error::Exception),
                }
            }
        })
        .map_err(|_| crate::Error::WorkerFailed("JavaScript program failed"))
}

fn begin_inline_turn(turn: &Cell<Option<(Duration, Instant)>>) -> rquickjs::Result<()> {
    turn.set(Some((
        platform::cpu_time().ok_or(rquickjs::Error::Exception)?,
        Instant::now(),
    )));
    Ok(())
}

fn turn_budget_exhausted(
    cpu_start: Duration,
    cpu_now: Duration,
    wall_start: Instant,
    wall_now: Instant,
) -> bool {
    cpu_now.saturating_sub(cpu_start) >= PROGRAM_TURN_CPU_LIMIT
        || wall_now.saturating_duration_since(wall_start) >= PROGRAM_TURN_TIMEOUT
}

fn install_program_writer<'js>(
    ctx: Ctx<'js>,
    output: Rc<RefCell<Output>>,
    output_bytes: Rc<RefCell<usize>>,
) -> rquickjs::Result<()> {
    let text_output = output.clone();
    let text_bytes = output_bytes.clone();
    let write = Function::new(ctx.clone(), move |text: String| {
        queue_program_output(&text_output, &text_bytes, text.into_bytes())
    })?;
    ctx.globals().set("__festerm_program_write", write)?;
    Ok(())
}

fn queue_program_output(
    output: &Rc<RefCell<Output>>,
    output_bytes: &Rc<RefCell<usize>>,
    bytes: Vec<u8>,
) -> rquickjs::Result<()> {
    if output.borrow().len() >= MAX_PENDING_WRITES
        || bytes.is_empty()
        || bytes.len() > MAX_OUTPUT_CHUNK_BYTES
        || *output_bytes.borrow() + bytes.len() > MAX_OUTPUT_QUEUE_BYTES
    {
        return Err(rquickjs::Error::new_from_js("terminal", "output"));
    }
    *output_bytes.borrow_mut() += bytes.len();
    output.borrow_mut().push(bytes);
    Ok::<_, rquickjs::Error>(())
}

fn take_inline_output(
    output: &Rc<RefCell<Output>>,
    bytes: &Rc<RefCell<usize>>,
) -> rquickjs::Result<Output> {
    take_output(output, bytes).map_err(|_| rquickjs::Error::Exception)
}

fn inline_message_generation(message: &ProgramHostMessage) -> u64 {
    match message {
        ProgramHostMessage::Launch { generation, .. }
        | ProgramHostMessage::Input { generation, .. }
        | ProgramHostMessage::Resize { generation, .. }
        | ProgramHostMessage::Close { generation, .. }
        | ProgramHostMessage::WriteResult { generation, .. }
        | ProgramHostMessage::Cancel { generation } => *generation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::developer_echo_program;

    #[test]
    fn active_turn_interrupts_at_controlled_cpu_budget_boundary() {
        let wall = Instant::now();
        let cpu = Duration::from_secs(10);
        assert!(!turn_budget_exhausted(
            cpu,
            cpu + PROGRAM_TURN_CPU_LIMIT - Duration::from_millis(1),
            wall,
            wall + Duration::from_millis(100)
        ));
        assert!(turn_budget_exhausted(
            cpu,
            cpu + PROGRAM_TURN_CPU_LIMIT,
            wall,
            wall + Duration::from_millis(100)
        ));
        assert!(turn_budget_exhausted(
            cpu,
            cpu,
            wall,
            wall + PROGRAM_TURN_TIMEOUT
        ));
    }

    #[test]
    fn developer_echo_program_runs_and_preserves_event_order() {
        let runtime = QuickJsProgramRuntime::new(developer_echo_program().unwrap()).unwrap();
        assert_eq!(
            runtime
                .start(TerminalSize {
                    columns: 80,
                    rows: 24
                })
                .unwrap(),
            Vec::<Vec<u8>>::new()
        );
        assert_eq!(runtime.input(b"first").unwrap(), vec![b"first".to_vec()]);
        assert_eq!(
            runtime
                .resize(TerminalSize {
                    columns: 100,
                    rows: 30
                })
                .unwrap(),
            vec![b"resize:100x30".to_vec()]
        );
        assert_eq!(runtime.input(b"second").unwrap(), vec![b"second".to_vec()]);
        assert_eq!(runtime.close().unwrap(), vec![b"closed".to_vec()]);
        assert_eq!(
            runtime.input(b"late").unwrap_err(),
            QuickJsProgramRuntimeError::Closed
        );
    }

    #[test]
    fn split_utf8_input_is_decoded_across_events() {
        let runtime = QuickJsProgramRuntime::new(developer_echo_program().unwrap()).unwrap();
        runtime
            .start(TerminalSize {
                columns: 80,
                rows: 24,
            })
            .unwrap();
        assert_eq!(runtime.input(b"split:").unwrap(), vec![b"split:".to_vec()]);
        assert!(runtime.input(&[0xc3]).unwrap().is_empty());
        assert_eq!(
            runtime.input(&[0xa9]).unwrap(),
            vec!["é".as_bytes().to_vec()]
        );
        runtime.close().unwrap();
    }

    #[test]
    fn inline_worker_decodes_non_ascii_bytes() {
        use std::collections::VecDeque;

        #[derive(Default)]
        struct Transport {
            input: VecDeque<ProgramHostMessage>,
            output: Vec<Vec<u8>>,
        }
        impl InlineProgramTransport for Transport {
            fn started(&mut self, _: u64) -> rquickjs::Result<()> {
                Ok(())
            }
            fn receive(&mut self) -> rquickjs::Result<ProgramHostMessage> {
                self.input.pop_front().ok_or(rquickjs::Error::Exception)
            }
            fn output(&mut self, output: Output) -> rquickjs::Result<()> {
                self.output.extend(output);
                Ok(())
            }
            fn completed(&mut self, _: u64, _: u64) -> rquickjs::Result<()> {
                Ok(())
            }
            fn finished(&mut self, _: u64, _: ProgramOutcome) -> rquickjs::Result<()> {
                Ok(())
            }
        }
        let mut transport = Transport {
            input: VecDeque::from([
                ProgramHostMessage::Input {
                    generation: 7,
                    sequence: 1,
                    bytes: vec![0xc3],
                },
                ProgramHostMessage::Close {
                    generation: 7,
                    sequence: 2,
                },
            ]),
            output: Vec::new(),
        };
        run_inline_program(
            developer_echo_program().unwrap(),
            7,
            TerminalSize {
                columns: 80,
                rows: 24,
            },
            &mut transport,
        )
        .unwrap();
        assert_eq!(transport.output, vec![b"closed".to_vec()]);
    }

    #[test]
    fn worker_retains_javascript_state_without_ambient_host_apis() {
        let mut snapshot = developer_echo_program().unwrap().snapshot().clone();
        snapshot.modules.insert(
            "main.js".into(),
            r#"export function main({terminal}) {
    let count = 0;
    terminal.events(() => terminal.write(
        [++count, typeof process, typeof fetch, typeof require,
         typeof eval, typeof Function, typeof (() => {}).constructor].join(":")
    ));
}"#
            .into(),
        );
        let runtime = QuickJsProgramRuntime::new(ApprovedProgram::new(snapshot).unwrap()).unwrap();
        runtime
            .start(TerminalSize {
                columns: 80,
                rows: 24,
            })
            .unwrap();
        assert_eq!(
            runtime.input(b"x").unwrap(),
            vec![b"1:undefined:undefined:undefined:undefined:undefined:undefined".to_vec()]
        );
        assert_eq!(
            runtime.input(b"x").unwrap(),
            vec![b"2:undefined:undefined:undefined:undefined:undefined:undefined".to_vec()]
        );
        runtime.close().unwrap();
    }

    #[test]
    fn close_before_start_is_cancellable_and_idempotent() {
        let runtime = QuickJsProgramRuntime::new(developer_echo_program().unwrap()).unwrap();
        assert!(runtime.close().unwrap().is_empty());
        assert!(runtime.close().unwrap().is_empty());
    }
}
