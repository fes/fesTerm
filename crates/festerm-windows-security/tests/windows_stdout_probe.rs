//! Native feasibility probe only; this does not qualify an MCP output boundary.

#![cfg(windows)]

use std::{
    io::{self, Read},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    ptr,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use festerm_windows_job::WindowsJob;
use windows_sys::Win32::{
    Foundation::{
        DuplicateHandle, DUPLICATE_SAME_ACCESS, ERROR_BROKEN_PIPE, ERROR_NO_DATA,
        ERROR_PIPE_NOT_CONNECTED, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    },
    Storage::FileSystem::{GetFileType, WriteFile, FILE_TYPE_PIPE},
    System::{
        Console::{GetStdHandle, STD_OUTPUT_HANDLE},
        Pipes::{
            GetNamedPipeHandleStateW, GetNamedPipeInfo, PeekNamedPipe, SetNamedPipeHandleState,
            PIPE_NOWAIT, PIPE_TYPE_MESSAGE, PIPE_WAIT,
        },
        Threading::{
            CreateEventW, GetCurrentProcess, GetThreadIOPendingFlag, OpenProcess, SetEvent,
            WaitForSingleObject, PROCESS_DUP_HANDLE,
        },
    },
};

const ROLE: &str = "FESTERM_NATIVE_STDOUT_PROBE_ROLE";
const EVENT_OWNER: &str = "FESTERM_NATIVE_STDOUT_PROBE_EVENT_OWNER";
const START_EVENT: &str = "FESTERM_NATIVE_STDOUT_PROBE_START_EVENT";
const READY_EVENT: &str = "FESTERM_NATIVE_STDOUT_PROBE_READY_EVENT";
const HELPER: &str = "windows_stdout_probe_subprocess";
const MARKER: &str = "FESTERM_STDOUT_PROBE";
const STEP_TIMEOUT: Duration = Duration::from_secs(5);
const OUTER_TIMEOUT: Duration = Duration::from_secs(25);
const IO_LIMIT: Duration = Duration::from_secs(2);
const CHUNK: usize = 64 * 1024;
const MAX_WRITES: usize = 64;
const MAX_CONTROL: usize = 4096;
const MAX_PIPE_BYTES: usize = CHUNK * MAX_WRITES + MAX_CONTROL;
const FILL_BYTE: u8 = 0xa5;
const WAIT_BYTE: u8 = 0xb6;

#[derive(Clone, Copy, Debug)]
struct RawWrite {
    success: bool,
    count: u32,
    error: u32,
    elapsed: Duration,
}

fn raw_write(handle: HANDLE, bytes: &[u8]) -> RawWrite {
    let mut count = 0;
    let started = Instant::now();
    // SAFETY: the live synchronous handle, buffer and count remain valid until
    // WriteFile returns. Potentially blocking calls run only in job-owned children.
    let success = unsafe {
        WriteFile(
            handle,
            bytes.as_ptr(),
            u32::try_from(bytes.len()).unwrap(),
            &mut count,
            ptr::null_mut(),
        )
    } != 0;
    let error = if success {
        0
    } else {
        io::Error::last_os_error().raw_os_error().unwrap_or(-1) as u32
    };
    RawWrite {
        success,
        count,
        error,
        elapsed: started.elapsed(),
    }
}

fn set_pipe_mode(handle: HANDLE, mode: u32) -> io::Result<()> {
    // SAFETY: mode points to a live DWORD; no asynchronous operation is started.
    if unsafe { SetNamedPipeHandleState(handle, &mode, ptr::null(), ptr::null()) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// Write-only CreatePipe handles need not include FILE_READ_ATTRIBUTES. A
// failed query is reported as unknown, not mistaken for a mode or a set failure.
fn query_pipe_mode(handle: HANDLE) -> (u32, u32) {
    let mut mode = 0;
    // SAFETY: mode is a live output and all optional arguments are null.
    let result = unsafe {
        GetNamedPipeHandleStateW(
            handle,
            &mut mode,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
        )
    };
    if result == 0 {
        (
            0,
            io::Error::last_os_error().raw_os_error().unwrap_or(-1) as u32,
        )
    } else {
        (1, mode)
    }
}

fn check_pipe(handle: HANDLE) -> io::Result<()> {
    if handle.is_null()
        || handle == INVALID_HANDLE_VALUE
        // SAFETY: GetFileType accepts a borrowed handle and does not retain it.
        || unsafe { GetFileType(handle) } != FILE_TYPE_PIPE
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "stdout is not a supported pipe handle",
        ));
    }
    Ok(())
}

fn duplicate_from_process(process: HANDLE, source: HANDLE) -> io::Result<OwnedHandle> {
    let mut duplicate = ptr::null_mut();
    // SAFETY: process and source identify a live test-owned handle; the
    // duplicate belongs to this process and is not inheritable.
    let result = unsafe {
        DuplicateHandle(
            process,
            source,
            GetCurrentProcess(),
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: DuplicateHandle returned a new handle owned by this process.
    Ok(unsafe { OwnedHandle::from_raw_handle(duplicate) })
}

fn available(handle: HANDLE) -> io::Result<u32> {
    let mut count = 0;
    // SAFETY: the read handle is owned by the controller and count is live.
    // A synchronous PeekNamedPipe can still block in a multithreaded process;
    // all calls to this helper are inside a separately killable controller.
    if unsafe {
        PeekNamedPipe(
            handle,
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            &mut count,
            ptr::null_mut(),
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(count)
    }
}

fn command_byte(expected: u8) {
    let mut byte = [0];
    io::stdin().read_exact(&mut byte).unwrap();
    assert_eq!(byte[0], expected, "unexpected test control command");
}

fn report(message: &str) {
    eprintln!("{MARKER} {message}");
}

fn report_write(name: &str, write: RawWrite) {
    report(&format!(
        "{name} {} {} {} {}",
        u8::from(write.success),
        write.count,
        write.error,
        write.elapsed.as_micros()
    ));
}

fn assert_prompt_success(write: RawWrite) {
    assert!(write.success, "WriteFile failed with {}", write.error);
    assert!(write.elapsed < IO_LIMIT, "PIPE_NOWAIT write was not prompt");
}

fn new_event() -> OwnedHandle {
    // SAFETY: an unnamed event is owned exclusively by this test.
    let raw = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    assert!(!raw.is_null(), "cannot create the test event");
    // SAFETY: CreateEventW returned a fresh owned handle.
    unsafe { OwnedHandle::from_raw_handle(raw) }
}

struct ControllerEvents {
    start: OwnedHandle,
    ready: OwnedHandle,
}

impl ControllerEvents {
    fn new() -> Self {
        Self {
            start: new_event(),
            ready: new_event(),
        }
    }

    fn start(&self) {
        // SAFETY: this test owns a live, modifiable event.
        assert_ne!(unsafe { SetEvent(self.start.as_raw_handle()) }, 0);
    }

    fn wait_ready(&self) {
        // SAFETY: the live event has SYNCHRONIZE access and the wait is bounded.
        assert_eq!(
            unsafe { WaitForSingleObject(self.ready.as_raw_handle(), 5_000) },
            WAIT_OBJECT_0,
            "controller did not become ready"
        );
    }
}

fn controller_start() -> OwnedHandle {
    let owner = std::env::var(EVENT_OWNER).unwrap().parse::<u32>().unwrap();
    let start = std::env::var(START_EVENT)
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let ready = std::env::var(READY_EVENT)
        .unwrap()
        .parse::<usize>()
        .unwrap();
    // This is test-only control data from the spawning test process, not an
    // identity or provenance assertion about an MCP caller.
    // SAFETY: OpenProcess returns a new process handle, or null on failure.
    let owner = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, owner) };
    assert!(!owner.is_null(), "cannot open test event owner");
    // SAFETY: OpenProcess returned a fresh owned handle.
    let owner = unsafe { OwnedHandle::from_raw_handle(owner) };
    let start = duplicate_from_process(owner.as_raw_handle(), start as HANDLE).unwrap();
    let ready = duplicate_from_process(owner.as_raw_handle(), ready as HANDLE).unwrap();
    // SAFETY: this is our duplicated event and the wait is bounded.
    assert_eq!(
        unsafe { WaitForSingleObject(start.as_raw_handle(), OUTER_TIMEOUT.as_millis() as u32) },
        WAIT_OBJECT_0,
        "test controller start was not signalled"
    );
    ready
}

// The runner never reads or writes a synchronous pipe. Each controller has
// an explicit kill-on-close job, and starts its own job-owned probe only after
// the runner signals the event following assignment.
struct Supervised {
    child: Child,
    job: Option<WindowsJob>,
    stdin: Option<ChildStdin>,
}

impl Supervised {
    fn spawn(role: &str, capture: bool, events: Option<&ControllerEvents>) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", HELPER, "--ignored", "--nocapture"])
            .env(ROLE, role)
            .env("RUST_BACKTRACE", "0")
            .stdout(if capture {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(if capture {
                Stdio::piped()
            } else {
                Stdio::inherit()
            });
        if let Some(events) = events {
            command
                .stdin(Stdio::null())
                .env(EVENT_OWNER, std::process::id().to_string())
                .env(
                    START_EVENT,
                    (events.start.as_raw_handle() as usize).to_string(),
                )
                .env(
                    READY_EVENT,
                    (events.ready.as_raw_handle() as usize).to_string(),
                );
        } else {
            command
                .stdin(Stdio::piped())
                .env_remove(EVENT_OWNER)
                .env_remove(START_EVENT)
                .env_remove(READY_EVENT);
        }
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take();
        let mut supervised = Self {
            child,
            job: None,
            stdin,
        };
        supervised.job = Some(
            WindowsJob::assign_to_process(supervised.child.as_raw_handle())
                .expect("cannot contain the test subprocess in a job"),
        );
        if let Some(stdin) = &supervised.stdin {
            set_pipe_mode(stdin.as_raw_handle(), PIPE_NOWAIT)
                .expect("test control pipe does not support PIPE_NOWAIT");
        }
        supervised
    }

    fn send(&mut self, bytes: &[u8]) {
        let result = raw_write(self.stdin.as_ref().unwrap().as_raw_handle(), bytes);
        assert_prompt_success(result);
        assert_eq!(result.count as usize, bytes.len(), "control pipe full");
    }

    fn wait_until(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn terminate(&mut self) -> ExitStatus {
        let job = self.job.take().unwrap();
        job.terminate().expect("failed to terminate test job");
        drop(job);
        self.wait_until(STEP_TIMEOUT)
            .expect("test process termination was not confirmed")
    }

    fn require_success(&mut self, timeout: Duration) {
        let status = match self.wait_until(timeout) {
            Some(status) => status,
            None => {
                let status = self.terminate();
                panic!("test subprocess timed out; termination was confirmed: {status}");
            }
        };
        assert!(status.success(), "test subprocess failed: {status}");
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        // Close only our job and process; never wait from a destructor.
        drop(self.job.take());
        let _ = self.child.kill();
    }
}

struct Probe {
    process: Supervised,
    stdout: Option<ChildStdout>,
    stderr: ChildStderr,
    control: Vec<u8>,
}

impl Probe {
    fn spawn(role: &str) -> Self {
        let mut process = Supervised::spawn(role, true, None);
        let stdout = process.child.stdout.take().unwrap();
        let stderr = process.child.stderr.take().unwrap();
        process.send(b"G");
        Self {
            process,
            stdout: Some(stdout),
            stderr,
            control: Vec::new(),
        }
    }

    fn event(&mut self, expected: &str) -> Vec<u64> {
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            if let Some(end) = self.control.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = self.control.drain(..=end).collect();
                let line = std::str::from_utf8(&line).expect("invalid test control frame");
                let mut words = line.split_whitespace();
                assert_eq!(words.next(), Some(MARKER), "unexpected test control frame");
                assert_eq!(
                    words.next(),
                    Some(expected),
                    "unexpected test control event"
                );
                return words
                    .map(|word| word.parse().expect("invalid test control value"))
                    .collect();
            }
            let count = available(self.stderr.as_raw_handle()).expect("test control pipe closed");
            if count > 0 {
                let mut bytes = [0; 512];
                let take = bytes.len().min(count as usize);
                let read = self.stderr.read(&mut bytes[..take]).unwrap();
                assert_ne!(read, 0, "test control pipe closed");
                self.control.extend_from_slice(&bytes[..read]);
                assert!(self.control.len() <= MAX_CONTROL, "test control overflow");
            } else {
                assert!(
                    self.process.child.try_wait().unwrap().is_none(),
                    "probe exited before its control event"
                );
                assert!(Instant::now() < deadline, "probe control deadline expired");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn duplicate(&self, raw: u64) -> OwnedHandle {
        let raw = usize::try_from(raw).unwrap() as HANDLE;
        duplicate_from_process(self.process.child.as_raw_handle(), raw).unwrap()
    }
}

fn wait_for_pending_io(thread: &OwnedHandle, process: &mut Supervised) {
    let deadline = Instant::now() + STEP_TIMEOUT;
    loop {
        let mut pending = 0;
        // SAFETY: this is a duplicated handle to the test's worker thread.
        assert_ne!(
            unsafe { GetThreadIOPendingFlag(thread.as_raw_handle(), &mut pending) },
            0,
            "cannot query the test worker's I/O state"
        );
        if pending != 0 {
            return;
        }
        assert!(
            process.child.try_wait().unwrap().is_none(),
            "probe exited before the physical write became pending"
        );
        assert!(
            Instant::now() < deadline,
            "physical write did not become pending"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_mode_if_queryable(known: u64, value: u64, wanted: u32) {
    if known == 1 {
        assert_eq!((value as u32) & PIPE_NOWAIT, wanted);
    } else {
        assert_eq!(known, 0);
        eprintln!("Windows stdout pipe mode query unavailable (OS error {value})");
    }
}

fn probe_child() {
    command_byte(b'G');
    // SAFETY: the process owns its inherited standard output handle.
    let stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    check_pipe(stdout).unwrap();
    set_pipe_mode(stdout, PIPE_NOWAIT).unwrap();
    let (known, mode) = query_pipe_mode(stdout);
    report(&format!("READY {} {known} {mode}", stdout as usize));

    command_byte(b'F');
    let bytes = [FILL_BYTE; CHUNK];
    let mut total = 0_usize;
    let mut full = false;
    for _ in 0..MAX_WRITES {
        let write = raw_write(stdout, &bytes);
        assert_prompt_success(write);
        assert!((write.count as usize) <= bytes.len());
        if write.count == 0 {
            full = true;
            break;
        }
        total += write.count as usize;
    }
    assert!(
        full && total > 0,
        "bounded pipe backpressure was not observed"
    );
    report(&format!("FULL {total}"));

    command_byte(b'P');
    let mut length = [0; size_of::<u32>()];
    io::stdin().read_exact(&mut length).unwrap();
    let length = u32::from_le_bytes(length) as usize;
    assert!(length > 1 && length <= MAX_PIPE_BYTES + 1);
    let partial_bytes = vec![FILL_BYTE; length];
    let partial = raw_write(stdout, &partial_bytes);
    assert_prompt_success(partial);
    assert!(partial.count > 0 && (partial.count as usize) < partial_bytes.len());
    let zero = raw_write(stdout, &bytes);
    assert_prompt_success(zero);
    assert_eq!(zero.count, 0, "pipe did not return to full");
    report(&format!("PARTIAL {}", partial.count));

    command_byte(b'W');
    let (known, mode) = query_pipe_mode(stdout);
    let (start, receive) = mpsc::sync_channel::<()>(0);
    let raw_stdout = stdout as usize;
    let worker = thread::spawn(move || {
        receive.recv().unwrap();
        raw_write(raw_stdout as HANDLE, &[WAIT_BYTE])
    });
    report(&format!(
        "WAIT_THREAD {} {known} {mode}",
        worker.as_raw_handle() as usize
    ));
    start.send(()).unwrap();
    // This join is only in the killable probe process, never the test runner.
    let write = worker.join().unwrap();
    report_write("WAIT_DONE", write);

    command_byte(b'N');
    let (known, mode) = query_pipe_mode(stdout);
    report(&format!("RESTORED_MODE {known} {mode}"));
    let mut restored = 0_usize;
    let mut full = false;
    for _ in 0..MAX_WRITES {
        let write = raw_write(stdout, &bytes);
        assert_prompt_success(write);
        assert!((write.count as usize) <= bytes.len());
        if write.count == 0 {
            full = true;
            break;
        }
        restored += write.count as usize;
    }
    assert!(full && restored > 0, "restored pipe did not reach zero");
    report(&format!("RESTORED_FULL {restored}"));

    command_byte(b'C');
    report_write("CLOSED_WRITE", raw_write(stdout, &[WAIT_BYTE]));
    // libtest would otherwise write its own result to the deliberately closed
    // stdout pipe. Only this disposable helper exits without running it.
    std::process::exit(0);
}

fn assert_write_event(event: &[u64], success: bool, count: u32) {
    assert_eq!(event.len(), 4);
    assert_eq!(event[0], u64::from(success));
    assert_eq!(event[1], u64::from(count));
    if success {
        assert_eq!(event[2], 0);
    }
    assert!(
        event[3] < IO_LIMIT.as_micros() as u64,
        "physical write did not return promptly"
    );
}

fn check_byte_pipe(stdout: &ChildStdout) {
    let mut flags = 0;
    // SAFETY: the test owns this CreatePipe read handle and output word.
    assert_ne!(
        unsafe {
            GetNamedPipeInfo(
                stdout.as_raw_handle(),
                &mut flags,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        },
        0,
        "cannot inspect the inherited stdout pipe"
    );
    assert_eq!(flags & PIPE_TYPE_MESSAGE, 0, "stdout is not a byte pipe");
}

#[derive(Default)]
struct Observed {
    fill: usize,
    wait: usize,
    harness: usize,
}

impl Observed {
    fn add(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            match byte {
                FILL_BYTE => self.fill += 1,
                WAIT_BYTE => self.wait += 1,
                _ if byte.is_ascii() => self.harness += 1,
                _ => panic!("unexpected stdout byte"),
            }
        }
        assert!(self.wait <= 1 && self.harness <= MAX_CONTROL);
    }

    fn drain(&mut self, probe: &mut Probe) -> usize {
        let deadline = Instant::now() + STEP_TIMEOUT;
        let mut total = 0;
        loop {
            // Count using the physical read, not the child's reported write alone.
            let stdout = probe.stdout.as_mut().unwrap();
            let available = available(stdout.as_raw_handle()).unwrap() as usize;
            if available == 0 {
                break;
            }
            let mut bytes = [0; 4096];
            let count = stdout.read(&mut bytes[..available.min(4096)]).unwrap();
            assert_ne!(count, 0);
            self.add(&bytes[..count]);
            total += count;
            assert!(total <= MAX_PIPE_BYTES + 1);
            assert!(Instant::now() < deadline, "stdout drain deadline expired");
        }
        total
    }
}

fn probe_controller() {
    let _ready = controller_start();
    assert!(check_pipe(ptr::null_mut()).is_err());
    assert!(set_pipe_mode(ptr::null_mut(), PIPE_NOWAIT).is_err());
    // The controller's stdout is the real NUL device, not the probe pipe.
    // SAFETY: this borrowed standard handle remains live in the controller.
    let unsupported = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    assert!(check_pipe(unsupported).is_err());
    assert!(set_pipe_mode(unsupported, PIPE_NOWAIT).is_err());

    let mut probe = Probe::spawn("probe-child");
    check_byte_pipe(probe.stdout.as_ref().unwrap());
    let ready = probe.event("READY");
    assert_eq!(ready.len(), 3);
    assert_mode_if_queryable(ready[1], ready[2], PIPE_NOWAIT);
    let writer = probe.duplicate(ready[0]);

    probe.process.send(b"F");
    let filled = probe.event("FULL");
    assert_eq!(filled.len(), 1);
    let mut total = usize::try_from(filled[0]).unwrap();
    assert!(total > 0 && total <= CHUNK * MAX_WRITES);

    // The parent has deliberately not read stdout until actual backpressure.
    let mut observed = Observed::default();
    let capacity = observed.drain(&mut probe);
    assert_eq!(observed.fill, total);
    assert!(capacity > 0 && capacity <= MAX_PIPE_BYTES);
    probe.process.send(b"P");
    probe
        .process
        .send(&u32::try_from(capacity + 1).unwrap().to_le_bytes());
    let partial = probe.event("PARTIAL");
    assert_eq!(partial.len(), 1);
    let partial = usize::try_from(partial[0]).unwrap();
    total += partial;

    set_pipe_mode(writer.as_raw_handle(), PIPE_WAIT).unwrap();
    probe.process.send(b"W");
    let waiting = probe.event("WAIT_THREAD");
    assert_eq!(waiting.len(), 3);
    assert_mode_if_queryable(waiting[1], waiting[2], PIPE_WAIT);
    let thread = probe.duplicate(waiting[0]);
    // The worker does no other I/O: pending means its physical stdout write is
    // actually in flight, not merely delayed before entering WriteFile.
    wait_for_pending_io(&thread, &mut probe.process);
    observed.drain(&mut probe);
    let finished = probe.event("WAIT_DONE");
    assert_eq!(finished.len(), 4);
    assert_eq!(&finished[..3], &[1, 1, 0]);
    observed.drain(&mut probe);
    assert_eq!(
        observed.fill, total,
        "physical partial bytes differ from WriteFile counts"
    );
    assert_eq!(observed.wait, 1, "physical waiting write count differs");

    set_pipe_mode(writer.as_raw_handle(), PIPE_NOWAIT).unwrap();
    probe.process.send(b"N");
    let mode = probe.event("RESTORED_MODE");
    assert_eq!(mode.len(), 2);
    assert_mode_if_queryable(mode[0], mode[1], PIPE_NOWAIT);
    let restored = probe.event("RESTORED_FULL");
    assert_eq!(restored.len(), 1);
    total += usize::try_from(restored[0]).unwrap();
    observed.drain(&mut probe);
    assert_eq!(
        observed.fill, total,
        "physical fill bytes differ from WriteFile counts"
    );
    assert_eq!(observed.wait, 1, "physical waiting write count differs");

    drop(thread);
    drop(writer);
    drop(probe.stdout.take());
    probe.process.send(b"C");
    let closed = probe.event("CLOSED_WRITE");
    assert_write_event(&closed, false, 0);
    assert!(
        [ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED].contains(&(closed[2] as u32)),
        "unexpected error after the last reader closed"
    );
    probe.process.require_success(STEP_TIMEOUT);
    eprintln!(
        "Native stdout probe: total={total} partial={partial} full_write=0 \
         restored_full_write=0 duplicate_wait_write=1 closed_error={}",
        closed[2]
    );
}

fn hanging_writer_child() {
    command_byte(b'G');
    // SAFETY: this is the inherited stdout owned by the test subprocess.
    let stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    check_pipe(stdout).unwrap();
    set_pipe_mode(stdout, PIPE_WAIT).unwrap();
    let raw_stdout = stdout as usize;
    let worker = thread::spawn(move || {
        let bytes = [FILL_BYTE; CHUNK];
        for _ in 0..MAX_WRITES {
            let write = raw_write(raw_stdout as HANDLE, &bytes);
            assert!(write.success && write.count > 0);
        }
        panic!("pipe never blocked within the work bound");
    });
    report(&format!("HANG_THREAD {}", worker.as_raw_handle() as usize));
    let _ = worker.join();
    panic!("deliberately stalled writer unexpectedly returned");
}

fn stall_controller() {
    let _ready = controller_start();
    let mut probe = Probe::spawn("hanging-writer");
    let worker = probe.event("HANG_THREAD");
    assert_eq!(worker.len(), 1);
    let thread = probe.duplicate(worker[0]);
    wait_for_pending_io(&thread, &mut probe.process);
    assert!(!probe.process.terminate().success());
    drop(thread);
    drop(probe);

    let mut next = Probe::spawn("ping");
    next.process.require_success(STEP_TIMEOUT);
}

fn park_controller() {
    let event = controller_start();
    // SAFETY: event is a live event with modification access.
    assert_ne!(unsafe { SetEvent(event.as_raw_handle()) }, 0);
    thread::park_timeout(Duration::from_secs(60));
    panic!("deliberately stalled controller unexpectedly returned");
}

#[test]
#[ignore = "subprocess helper; invoked only by the deadline-supervised tests"]
fn windows_stdout_probe_subprocess() {
    match std::env::var(ROLE).as_deref() {
        Ok("probe-controller") => probe_controller(),
        Ok("probe-child") => probe_child(),
        Ok("stall-controller") => stall_controller(),
        Ok("hanging-writer") => hanging_writer_child(),
        Ok("park-controller") => park_controller(),
        Ok("controller-ping") => {
            let _ready = controller_start();
            std::process::exit(0);
        }
        Ok("ping") => {
            command_byte(b'G');
            std::process::exit(0);
        }
        _ => {}
    }
}

#[test]
fn windows_stdout_inherited_pipe_nowait_probe() {
    let events = ControllerEvents::new();
    let mut controller = Supervised::spawn("probe-controller", false, Some(&events));
    events.start();
    controller.require_success(OUTER_TIMEOUT);
}

#[test]
fn windows_stdout_probe_stalled_processes_are_supervised() {
    let events = ControllerEvents::new();
    let mut controller = Supervised::spawn("stall-controller", false, Some(&events));
    events.start();
    controller.require_success(OUTER_TIMEOUT);

    let parked_events = ControllerEvents::new();
    let mut parked = Supervised::spawn("park-controller", false, Some(&parked_events));
    parked_events.start();
    parked_events.wait_ready();
    assert!(parked.wait_until(Duration::from_millis(200)).is_none());
    assert!(!parked.terminate().success());

    let next_events = ControllerEvents::new();
    let mut next = Supervised::spawn("controller-ping", false, Some(&next_events));
    next_events.start();
    next.require_success(STEP_TIMEOUT);
}
