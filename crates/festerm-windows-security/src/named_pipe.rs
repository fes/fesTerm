//! Owned local byte pipes with cancellable connection and non-flushing teardown.
//!
//! Synchronous `PIPE_NOWAIT` operations keep borrowed buffers out of outstanding
//! kernel I/O. Waiting is explicit and bounded here, rather than delegated to a
//! dependency's unbounded connect loop or `FlushFileBuffers` destructor.

use std::{
    io::{self, Read, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{
        ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
        ERROR_PIPE_LISTENING, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE,
        INVALID_HANDLE_VALUE,
    },
    Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX,
    },
    System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, SetNamedPipeHandleState,
        PIPE_NOWAIT, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES,
    },
};

const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A local pipe endpoint. Dropping it closes its owned handle without flushing.
/// Do not force `DisconnectNamedPipe` here: it would discard buffered terminal
/// output and takeover/exit notices before a healthy client could read them.
#[derive(Debug)]
pub struct Pipe {
    handle: OwnedHandle,
    read_timeout: Duration,
    write_timeout: Duration,
}

impl Pipe {
    /// Opens only a local pipe, retrying busy instances until cancelled or timed
    /// out. Each `CreateFileW` attempt is single-shot: no internal wait loop.
    pub fn connect(name: &str, timeout: Duration, cancelled: &AtomicBool) -> io::Result<Self> {
        let name = local_pipe_name(name)?;
        let started = Instant::now();
        let mut attempted = false;
        loop {
            check_cancelled(cancelled)?;
            if attempted && started.elapsed() >= timeout {
                return Err(io::ErrorKind::TimedOut.into());
            }
            attempted = true;
            // SAFETY: the name is NUL-terminated and all optional pointers are
            // null. No overlapped operation or inheritable handle is created.
            let raw = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    ptr::null(),
                    OPEN_EXISTING,
                    0,
                    ptr::null_mut(),
                )
            };
            if raw != INVALID_HANDLE_VALUE {
                // SAFETY: CreateFileW transferred this fresh valid handle.
                let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
                let mode = PIPE_READMODE_BYTE | PIPE_NOWAIT;
                // SAFETY: the handle is live and mode points to a valid DWORD.
                if unsafe {
                    SetNamedPipeHandleState(handle.as_raw_handle(), &mode, ptr::null(), ptr::null())
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                check_cancelled(cancelled)?;
                return Ok(Self::new(handle));
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) {
                return Err(error);
            }
            wait_again(started, timeout)?;
        }
    }

    fn new(handle: OwnedHandle) -> Self {
        Self {
            handle,
            read_timeout: POLL_INTERVAL,
            write_timeout: Duration::from_secs(1),
        }
    }

    pub fn set_read_timeout(&mut self, timeout: Duration) {
        self.read_timeout = timeout;
    }

    pub fn set_write_timeout(&mut self, timeout: Duration) {
        self.write_timeout = timeout;
    }
}

impl Read for Pipe {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let started = Instant::now();
        loop {
            let mut read = 0;
            // SAFETY: synchronous PIPE_NOWAIT I/O completes before returning;
            // buffer and the byte count remain valid throughout the call.
            let result = unsafe {
                ReadFile(
                    self.handle.as_raw_handle(),
                    buffer.as_mut_ptr(),
                    buffer.len().min(u32::MAX as usize) as u32,
                    &mut read,
                    ptr::null_mut(),
                )
            };
            if result != 0 {
                return Ok(read as usize);
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error().map(|code| code as u32) {
                Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) => return Ok(0),
                Some(ERROR_NO_DATA) => wait_again(started, self.read_timeout)?,
                _ => return Err(error),
            }
        }
    }
}

impl Write for Pipe {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let started = Instant::now();
        loop {
            let mut written = 0;
            // SAFETY: this synchronous nonblocking write cannot retain buffer.
            let result = unsafe {
                WriteFile(
                    self.handle.as_raw_handle(),
                    buffer.as_ptr(),
                    buffer.len().min(u32::MAX as usize) as u32,
                    &mut written,
                    ptr::null_mut(),
                )
            };
            if result != 0 && written != 0 {
                return Ok(written as usize);
            }
            if result == 0 {
                return Err(io::Error::last_os_error());
            }
            // A full byte pipe in PIPE_NOWAIT mode succeeds with zero bytes.
            wait_again(started, self.write_timeout)?;
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // Writes already hand bytes to the pipe; FlushFileBuffers would wait
        // for peer consumption and defeat both deadlines and bounded teardown.
        Ok(())
    }
}

/// A single local named-pipe instance awaiting its first client.
#[derive(Debug)]
pub struct PipeListener {
    pipe: Pipe,
}

impl PipeListener {
    /// Uses the process token's default DACL. Callers needing an owner-only
    /// DACL must hold the crate's `DefaultDaclGuard` during this operation.
    pub fn bind(name: &str, first: bool) -> io::Result<Self> {
        let name = local_pipe_name(name)?;
        // SAFETY: the name is terminated and null security attributes select
        // the token DACL. Nonblocking synchronous I/O needs no OVERLAPPED.
        let raw = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX
                    | if first {
                        FILE_FLAG_FIRST_PIPE_INSTANCE
                    } else {
                        0
                    },
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                65536,
                65536,
                0,
                ptr::null(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            // SAFETY: CreateNamedPipeW transferred this fresh valid handle.
            pipe: Pipe::new(unsafe { OwnedHandle::from_raw_handle(raw) }),
        })
    }

    pub fn accept(self, cancelled: &AtomicBool) -> io::Result<Pipe> {
        loop {
            check_cancelled(cancelled)?;
            // SAFETY: this live PIPE_NOWAIT handle has no overlapped I/O.
            let result =
                unsafe { ConnectNamedPipe(self.pipe.handle.as_raw_handle(), ptr::null_mut()) };
            if result == 0 {
                let error = io::Error::last_os_error();
                match error.raw_os_error().map(|code| code as u32) {
                    Some(ERROR_PIPE_CONNECTED) => return Ok(self.pipe),
                    Some(ERROR_PIPE_LISTENING) => {}
                    Some(ERROR_NO_DATA) => {
                        // The client left before accept observed it. Reset the
                        // instance without flushing and continue listening.
                        // SAFETY: the listener owns this live server handle.
                        let _ = unsafe { DisconnectNamedPipe(self.pipe.handle.as_raw_handle()) };
                    }
                    _ => return Err(error),
                }
            }
            // Nonzero in NOWAIT mode only means "available for connection".
            thread::sleep(POLL_INTERVAL);
        }
    }
}

fn local_pipe_name(name: &str) -> io::Result<Vec<u16>> {
    const PREFIX: &str = r"\\.\pipe\";
    if !name.starts_with(PREFIX) || name.len() == PREFIX.len() || name.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a local named-pipe path",
        ));
    }
    Ok(name.encode_utf16().chain(Some(0)).collect())
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "named-pipe operation cancelled",
        ))
    } else {
        Ok(())
    }
}

fn wait_again(started: Instant, timeout: Duration) -> io::Result<()> {
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    thread::sleep(POLL_INTERVAL.min(remaining));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicUsize, mpsc, Arc};

    fn pipe_name() -> String {
        static NEXT_PIPE: AtomicUsize = AtomicUsize::new(0);
        format!(
            r"\\.\pipe\festerm-bounded-pipe-{}-{}",
            std::process::id(),
            NEXT_PIPE.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn connected_pair(name: &str) -> (Pipe, Pipe) {
        let listener = PipeListener::bind(name, true).unwrap();
        let client = Pipe::connect(name, Duration::from_secs(1), &AtomicBool::new(false)).unwrap();
        let server = listener.accept(&AtomicBool::new(false)).unwrap();
        (server, client)
    }

    #[test]
    fn busy_pipe_connect_has_a_deadline_without_releasing_the_existing_client() {
        let name = pipe_name();
        let (_server, _client) = connected_pair(&name);
        let started = Instant::now();
        let error =
            Pipe::connect(&name, Duration::from_millis(60), &AtomicBool::new(false)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn busy_pipe_connect_is_cancelled_while_the_existing_client_stays_connected() {
        let name = pipe_name();
        let (_server, _client) = connected_pair(&name);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (started, entered) = mpsc::sync_channel(1);
        let (done, finished) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            started.send(()).unwrap();
            let result = Pipe::connect(&name, Duration::from_secs(10), &worker_cancelled);
            done.send(result).unwrap();
        });
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        thread::sleep(2 * POLL_INTERVAL);
        assert!(finished.try_recv().is_err());
        cancelled.store(true, Ordering::Release);
        let error = finished
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        worker.join().unwrap();
    }

    #[test]
    fn dropping_a_server_does_not_wait_for_unread_output() {
        let name = pipe_name();
        let (mut server, _nonreading_client) = connected_pair(&name);
        server
            .write_all(b"the client deliberately never reads this")
            .unwrap();
        let (done, finished) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            drop(server);
            done.send(()).unwrap();
        });
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn closing_a_server_preserves_already_written_output_before_eof() {
        let (mut server, mut client) = connected_pair(&pipe_name());
        server
            .write_all(b"last output and session exit notice")
            .unwrap();
        drop(server);
        let mut received = Vec::new();
        client.read_to_end(&mut received).unwrap();
        assert_eq!(received, b"last output and session exit notice");
    }

    #[test]
    fn pipe_reads_and_writes_preserve_bytes_and_bound_backpressure() {
        let name = pipe_name();
        let (mut server, mut client) = connected_pair(&name);
        server.write_all(b"output").unwrap();
        let mut bytes = [0; 6];
        client.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"output");
        client.write_all(b"input!").unwrap();
        server.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"input!");
        assert_eq!(
            server.read(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );

        server.set_write_timeout(Duration::from_millis(20));
        let chunk = [b'x'; 4096];
        let mut total = 0;
        loop {
            match server.write(&chunk) {
                Ok(count) => {
                    assert!(count > 0);
                    total += count;
                    assert!(total <= 1024 * 1024, "the pipe buffer should be bounded");
                }
                Err(error) => {
                    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
                    break;
                }
            }
        }
        assert!(total > 0);
        let mut output = vec![0; total];
        client.read_exact(&mut output).unwrap();
        assert!(output.iter().all(|byte| *byte == b'x'));
        server.write_all(b"resumed").unwrap();
        let mut resumed = [0; 7];
        client.read_exact(&mut resumed).unwrap();
        assert_eq!(&resumed, b"resumed");
    }

    #[test]
    fn listener_wait_is_cancelled_without_a_wakeup_connection() {
        let listener = PipeListener::bind(&pipe_name(), true).unwrap();
        let error = listener.accept(&AtomicBool::new(true)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn only_local_pipe_paths_are_accepted() {
        for invalid in [r"\\remote\pipe\name", r"C:\file", "\\\\.\\pipe\\bad\0name"] {
            assert_eq!(
                local_pipe_name(invalid).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }
}
