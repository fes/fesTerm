pub(super) use festerm_automation::OutboundTargetGuard as TargetGuard;
use festerm_automation::{outbound_channel, OutboundFrame, OutboundReceiver, OutboundSender};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[cfg(any(unix, windows))]
#[allow(dead_code)]
mod direct;

const MAX_FRAME: usize = 65_536;
const MAX_CONNECTIONS: usize = 8;
const MAX_BUFFERED_INPUT: usize = 64;
const IO_SLICE: Duration = Duration::from_millis(100);
const IO_RETRY: Duration = Duration::from_millis(5);
const FRAME_DEADLINE: Duration = Duration::from_secs(2);
type Waker = Arc<dyn Fn() + Send + Sync>;
type Connections = BTreeMap<u64, (Connection, JoinHandle<()>)>;

#[must_use = "release admission and wake the direct owner"]
pub(super) struct DeferredCloseWake {
    #[cfg(any(unix, windows))]
    wake: Option<direct::Wake>,
}

/// Release admission before waking an executor, which may invoke a waker.
pub(super) fn wake_after_admission(
    guard: festerm_session::AutomationAdmissionGuard,
    wakes: impl IntoIterator<Item = DeferredCloseWake>,
) {
    drop(guard);
    for wake in wakes {
        #[cfg(any(unix, windows))]
        if let Some(wake) = wake.wake {
            wake.notify();
        }
        #[cfg(not(unix))]
        let _ = wake;
    }
}

#[cfg(test)]
struct EnqueueTestGate {
    entered: SyncSender<()>,
    resume: Receiver<()>,
}

#[cfg(test)]
thread_local! {
    static ENQUEUE_TEST_GATE: std::cell::RefCell<Option<EnqueueTestGate>> = const {
        std::cell::RefCell::new(None)
    };
}

fn next_connection_key(counter: &AtomicU64) -> Option<u64> {
    counter
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .ok()
}

#[derive(Clone)]
pub(super) struct Connection {
    pub key: u64,
    closed: Arc<AtomicBool>,
    pending_input: Arc<AtomicUsize>,
    output: OutboundSender,
    #[cfg(any(unix, windows))]
    output_wake: Option<direct::Wake>,
}

impl Connection {
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub fn close(&self) {
        super::set_admission_flag(&self.closed, true);
        self.wake_output();
    }

    /// Call only after releasing the application admission guard.
    pub(super) fn wake_output(&self) {
        #[cfg(any(unix, windows))]
        if let Some(wake) = &self.output_wake {
            wake.notify();
        }
    }

    pub fn close_under_admission(
        &self,
        _guard: &festerm_session::AutomationAdmissionGuard,
    ) -> DeferredCloseWake {
        self.closed.store(true, Ordering::Release);
        DeferredCloseWake {
            #[cfg(any(unix, windows))]
            wake: self.output_wake.clone(),
        }
    }

    pub fn closed_flag(&self) -> Arc<AtomicBool> {
        self.closed.clone()
    }

    pub fn has_pending_input(&self, _guard: &festerm_session::AutomationAdmissionGuard) -> bool {
        self.pending_input.load(Ordering::Acquire) != 0
    }

    pub(super) fn enqueue_message(
        &self,
        events: &SyncSender<Event>,
        bytes: Vec<u8>,
        stop: &AtomicBool,
        privacy_closed: &AtomicBool,
    ) -> io::Result<()> {
        let Some(guard) = festerm_session::automation_admission_lock() else {
            self.closed.store(true, Ordering::Release);
            self.wake_output();
            return Err(io::ErrorKind::BrokenPipe.into());
        };
        if self.is_closed()
            || stop.load(Ordering::Acquire)
            || !super::try_privacy_admission(privacy_closed)
        {
            let wake = self.close_under_admission(&guard);
            wake_after_admission(guard, [wake]);
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if self
            .pending_input
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .is_err()
        {
            let wake = self.close_under_admission(&guard);
            wake_after_admission(guard, [wake]);
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if events
            .try_send(Event::Message {
                key: self.key,
                bytes,
            })
            .is_err()
        {
            let wake = self.close_under_admission(&guard);
            wake_after_admission(guard, [wake]);
            return Err(io::ErrorKind::WouldBlock.into());
        }
        Ok(())
    }

    pub fn finish_message(&self) -> io::Result<()> {
        let updated = self
            .pending_input
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_sub(1)
            });
        #[cfg(test)]
        if updated == Err(0) {
            // Some coordinator fixtures inject events without an IPC reader.
            return Ok(());
        }
        if updated.is_err() {
            self.close();
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        Ok(())
    }

    pub fn send(
        &self,
        bytes: Vec<u8>,
        expires: Instant,
        targets: &[TargetGuard],
    ) -> io::Result<()> {
        self.send_guarded(bytes, expires, targets, &[])
    }

    pub fn send_guarded(
        &self,
        bytes: Vec<u8>,
        expires: Instant,
        targets: &[TargetGuard],
        flags: &[Arc<AtomicBool>],
    ) -> io::Result<()> {
        self.send_frame(OutboundFrame {
            bytes,
            expires,
            targets: targets.to_vec(),
            flags: flags.to_vec(),
            disclosure: None,
        })
    }

    pub fn send_disclosure(
        &self,
        bytes: Vec<u8>,
        expires: Instant,
        targets: &[TargetGuard],
        flags: &[Arc<AtomicBool>],
        disclosure: (
            festerm_automation::ConnectionId,
            festerm_automation::DisclosureLiveness,
        ),
    ) -> io::Result<()> {
        self.send_frame(OutboundFrame {
            bytes,
            expires,
            targets: targets.to_vec(),
            flags: flags.to_vec(),
            disclosure: Some(Box::new(disclosure)),
        })
    }

    fn send_frame(&self, frame: OutboundFrame) -> io::Result<()> {
        let Some(guard) = festerm_session::automation_admission_lock() else {
            self.close();
            return Err(io::ErrorKind::BrokenPipe.into());
        };
        if frame.bytes.len() > MAX_FRAME
            || self.is_closed()
            || Instant::now() >= frame.expires
            || !frame.is_valid()
        {
            let wake = self.close_under_admission(&guard);
            wake_after_admission(guard, [wake]);
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        #[cfg(test)]
        ENQUEUE_TEST_GATE.with(|slot| {
            if let Some(gate) = slot.borrow_mut().take() {
                gate.entered.send(()).expect("signal enqueue test gate");
                gate.resume
                    .recv_timeout(Duration::from_secs(5))
                    .expect("release enqueue test gate");
            }
        });
        if self.output.try_send(frame, &guard).is_err() {
            let wake = self.close_under_admission(&guard);
            wake_after_admission(guard, [wake]);
            return Err(io::ErrorKind::WouldBlock.into());
        }
        drop(guard);
        self.wake_output();
        Ok(())
    }
}

pub(super) enum Event {
    Connected(Connection),
    Message { key: u64, bytes: Vec<u8> },
}

pub(super) struct Server {
    events: Receiver<Event>,
    stop: Arc<AtomicBool>,
    privacy_closed: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn reap_finished(connections: &mut Connections) {
    let finished: Vec<_> = connections
        .iter()
        .filter_map(|(key, (_, thread))| thread.is_finished().then_some(*key))
        .collect();
    for key in finished {
        if let Some((_, thread)) = connections.remove(&key) {
            if thread.join().is_err() {
                tracing::warn!("automation IPC peer cleanup failed");
            }
        }
    }
}

impl Server {
    pub fn start(
        wake: Waker,
        stop: Arc<AtomicBool>,
        privacy_closed: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        require_qualified_bootstrap()?;
        let listener = Listener::bind()?;
        let (sender, events) = mpsc::sync_channel(32);
        let cancelled = stop.clone();
        let privacy = privacy_closed.clone();
        let thread = thread::Builder::new()
            .name("festerm-automation-ipc".into())
            .spawn(move || {
                let mut connections = Connections::new();
                while !cancelled.load(Ordering::Acquire) && !privacy.load(Ordering::Acquire) {
                    reap_finished(&mut connections);
                    match listener.accept(&cancelled) {
                        Ok(Some(stream)) => {
                            reap_finished(&mut connections);
                            if connections.len() >= MAX_CONNECTIONS {
                                continue;
                            }
                            static NEXT: AtomicU64 = AtomicU64::new(1);
                            let Some(key) = next_connection_key(&NEXT) else {
                                tracing::error!("automation IPC connection identities exhausted");
                                super::set_admission_flag(&cancelled, true);
                                wake();
                                break;
                            };
                            let closed = Arc::new(AtomicBool::new(false));
                            let (output, receiver) = outbound_channel();
                            let connection = Connection {
                                key,
                                closed,
                                pending_input: Arc::new(AtomicUsize::new(0)),
                                output,
                                #[cfg(any(unix, windows))]
                                output_wake: None,
                            };
                            let peer = connection.clone();
                            let events = sender.clone();
                            let notify = wake.clone();
                            let server_stop = cancelled.clone();
                            let server_privacy = privacy.clone();
                            let spawn = thread::Builder::new()
                                .name("festerm-automation-peer".into())
                                .spawn(move || {
                                    serve_peer(
                                        stream,
                                        peer,
                                        receiver,
                                        events,
                                        notify,
                                        server_stop,
                                        server_privacy,
                                    )
                                });
                            match spawn {
                                Ok(thread) => {
                                    connections.insert(key, (connection, thread));
                                }
                                Err(_) => {
                                    connection.close();
                                    tracing::warn!("automation IPC could not start a peer");
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => break,
                        Err(_) => {
                            tracing::warn!("automation IPC listener failed");
                            super::set_admission_flag(&cancelled, true);
                            wake();
                            break;
                        }
                    }
                }
                for (_, (connection, thread)) in connections {
                    connection.close();
                    if thread.join().is_err() {
                        tracing::warn!("automation IPC peer cleanup failed");
                    }
                }
            })?;
        Ok(Self {
            events,
            stop,
            privacy_closed,
            thread: Some(thread),
        })
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    pub fn failed(&self) -> bool {
        self.stop.load(Ordering::Acquire)
            || self.privacy_closed.load(Ordering::Acquire)
            || self.thread.as_ref().is_some_and(JoinHandle::is_finished)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        super::set_admission_flag(&self.stop, true);
        if self
            .thread
            .take()
            .is_some_and(|thread| thread.join().is_err())
        {
            tracing::warn!("automation IPC supervisor cleanup failed");
        }
    }
}

fn serve_peer(
    mut stream: Stream,
    connection: Connection,
    output: OutboundReceiver,
    events: SyncSender<Event>,
    wake: Waker,
    stop: Arc<AtomicBool>,
    privacy_closed: Arc<AtomicBool>,
) {
    let result = (|| -> io::Result<()> {
        let mut writer = stream.try_clone()?;
        prepare_writer(&mut writer)?;
        let writer_connection = connection.clone();
        let writer_stop = stop.clone();
        let writer_privacy = privacy_closed.clone();
        let writer_wake = wake.clone();
        let output_thread = thread::Builder::new()
            .name("festerm-automation-replies".into())
            .spawn(move || {
                while !writer_connection.is_closed()
                    && !writer_stop.load(Ordering::Acquire)
                    && !writer_privacy.load(Ordering::Acquire)
                {
                    match output.recv_timeout(IO_SLICE) {
                        Ok(frame) => {
                            let allowed = || {
                                peer_frame_is_live(
                                    &writer_connection,
                                    &writer_stop,
                                    &writer_privacy,
                                    &frame,
                                )
                            };
                            if write_frame_until(&mut writer, &frame.bytes, frame.expires, allowed)
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                writer_connection.close();
                writer_wake();
            })?;
        if stop.load(Ordering::Acquire)
            || privacy_closed.load(Ordering::Acquire)
            || events
                .try_send(Event::Connected(connection.clone()))
                .is_err()
        {
            connection.close();
        } else {
            wake();
            while !connection.is_closed()
                && !stop.load(Ordering::Acquire)
                && !privacy_closed.load(Ordering::Acquire)
            {
                match read_frame(&mut stream, &connection.closed, &stop) {
                    Ok(Some(bytes)) => {
                        if connection
                            .enqueue_message(&events, bytes, &stop, &privacy_closed)
                            .is_err()
                        {
                            break;
                        }
                        wake();
                    }
                    Ok(None) | Err(_) => break,
                }
            }
        }
        connection.close();
        if output_thread.join().is_err() {
            return Err(io::Error::other("automation reply worker failed"));
        }
        Ok(())
    })();
    if result.is_err() {
        tracing::debug!("automation IPC peer ended with a transport refusal");
    }
    connection.close();
    wake();
}

fn peer_frame_is_live(
    connection: &Connection,
    stop: &AtomicBool,
    privacy_closed: &AtomicBool,
    frame: &OutboundFrame,
) -> bool {
    !connection.is_closed()
        && !stop.load(Ordering::Acquire)
        && frame.is_valid()
        && super::try_privacy_admission(privacy_closed)
}

fn prepare_writer(writer: &mut Stream) -> io::Result<()> {
    #[cfg(unix)]
    {
        writer.set_nonblocking(true)
    }
    #[cfg(windows)]
    {
        writer.set_write_timeout(Duration::ZERO);
        Ok(())
    }
}

#[cfg(test)]
fn write_frame(writer: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    write_frame_until(writer, bytes, Instant::now() + FRAME_DEADLINE, || true)
}

#[cfg(test)]
pub(super) fn write_test_frame(writer: &mut impl Write, frame: &OutboundFrame) -> io::Result<()> {
    write_frame_until(writer, &frame.bytes, frame.expires, || frame.is_valid())
}

fn write_frame_until(
    writer: &mut impl Write,
    bytes: &[u8],
    expires: Instant,
    allowed: impl Fn() -> bool,
) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let deadline = expires.min(Instant::now() + FRAME_DEADLINE);
    write_until(
        writer,
        &(bytes.len() as u32).to_be_bytes(),
        deadline,
        &allowed,
    )?;
    write_until(writer, bytes, deadline, &allowed)?;
    writer.flush()
}

fn write_until(
    writer: &mut impl Write,
    mut bytes: &[u8],
    deadline: Instant,
    allowed: &impl Fn() -> bool,
) -> io::Result<()> {
    while !bytes.is_empty() {
        let result = {
            let _guard = festerm_session::automation_admission_lock()
                .ok_or_else(|| io::Error::other("automation admission lock poisoned"))?;
            if !allowed() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            writer.write(bytes)
        };
        match result {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if idle_error(&error) => thread::sleep(IO_RETRY),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_frame(
    reader: &mut impl Read,
    closed: &AtomicBool,
    stop: &AtomicBool,
) -> io::Result<Option<Vec<u8>>> {
    let mut prefix = [0; 4];
    loop {
        if closed.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
            return Ok(None);
        }
        match reader.read(&mut prefix[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(error) if idle_error(&error) => thread::sleep(IO_RETRY),
            Err(error) => return Err(error),
        }
    }
    let deadline = Instant::now() + FRAME_DEADLINE;
    read_until(reader, &mut prefix[1..], deadline, closed, stop)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = vec![0; length];
    read_until(reader, &mut bytes, deadline, closed, stop)?;
    Ok(Some(bytes))
}

fn read_until(
    reader: &mut impl Read,
    mut bytes: &mut [u8],
    deadline: Instant,
    closed: &AtomicBool,
    stop: &AtomicBool,
) -> io::Result<()> {
    while !bytes.is_empty() {
        if closed.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match reader.read(bytes) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if idle_error(&error) => thread::sleep(IO_RETRY),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn idle_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn send_until<T>(
    sender: &SyncSender<T>,
    mut value: T,
    deadline: Instant,
    closed: &AtomicBool,
) -> io::Result<()> {
    loop {
        if closed.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match sender.try_send(value) {
            Ok(()) => return Ok(()),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            Err(mpsc::TrySendError::Full(returned)) => {
                value = returned;
                thread::sleep(IO_RETRY);
            }
        }
    }
}

#[derive(Debug)]
enum StdioInput {
    Message(Vec<u8>),
    Stop,
}

fn read_stdio_input(
    mut input: impl io::BufRead,
    staged: SyncSender<Vec<u8>>,
    wake: SyncSender<StdioInput>,
    errors: SyncSender<io::Error>,
    closed: Arc<AtomicBool>,
) {
    loop {
        match festerm_automation_mcp::read_bounded_line(&mut input) {
            Ok(Some(bytes)) => {
                if staged.try_send(bytes).is_err() {
                    let _ = errors.try_send(io::Error::other("MCP input buffer is full or closed"));
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => {
                let _ = errors.try_send(io::Error::other("MCP input framing refused"));
                break;
            }
        }
    }
    super::set_admission_flag(&closed, true);
    let _ = wake.try_send(StdioInput::Stop);
}

fn forward_stdio_input(
    mut receive: impl FnMut(Duration) -> Result<Vec<u8>, mpsc::RecvTimeoutError>,
    output: SyncSender<StdioInput>,
    errors: SyncSender<io::Error>,
    closed: Arc<AtomicBool>,
) {
    while !closed.load(Ordering::Acquire) {
        match receive(IO_SLICE) {
            Ok(bytes) => {
                if let Err(error) = send_until(
                    &output,
                    StdioInput::Message(bytes),
                    Instant::now() + FRAME_DEADLINE,
                    &closed,
                ) {
                    if error.kind() != io::ErrorKind::Interrupted {
                        let _ = errors.try_send(error);
                    }
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    super::set_admission_flag(&closed, true);
    let _ = output.try_send(StdioInput::Stop);
}

/// Desktop platforms with a beta privacy monitor and private endpoint.
const BETA_PLATFORM: bool = cfg!(any(target_os = "macos", target_os = "linux", windows));

/// Beta bootstrap (ADR 0065). The peer is accepted only through the current
/// user's private socket or pipe, then must be matched by its public
/// verification code and approved in native Settings. No OS process identity
/// is attested. Unit tests keep it refused so they never bind the real
/// endpoint; they inject a bootstrap result instead.
pub(super) fn require_qualified_bootstrap() -> io::Result<()> {
    beta_bootstrap(BETA_PLATFORM && !cfg!(test))
}

fn beta_bootstrap(supported: bool) -> io::Result<()> {
    if supported {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "MCP beta is unavailable on this platform",
        ))
    }
}

#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Stream = festerm_windows_security::named_pipe::Pipe;

#[cfg(unix)]
struct Listener {
    listener: std::os::unix::net::UnixListener,
    _lock: std::fs::File,
    socket: std::path::PathBuf,
}

#[cfg(unix)]
impl Listener {
    fn bind() -> io::Result<Self> {
        use fs2::FileExt;
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        let (root, directory) = endpoint_directory(true)?;
        let lock = match festerm_unix_security::open_private_file(
            &directory,
            std::path::Path::new("broker.lock"),
            true,
        ) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                festerm_unix_security::open_private_file(
                    &directory,
                    std::path::Path::new("broker.lock"),
                    false,
                )?
            }
            Err(error) => return Err(error),
        };
        lock.try_lock_exclusive()?;
        let socket = root.join("broker.sock");
        match std::fs::symlink_metadata(&socket) {
            Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(&socket)?,
            Ok(_) => return Err(io::ErrorKind::PermissionDenied.into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = std::os::unix::net::UnixListener::bind(&socket)?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            _lock: lock,
            socket,
        })
    }

    fn accept(&self, stop: &AtomicBool) -> io::Result<Option<Stream>> {
        if stop.load(Ordering::Acquire) {
            return Ok(None);
        }
        match self.listener.accept() {
            Ok((stream, _)) => {
                stream.set_read_timeout(Some(IO_SLICE))?;
                stream.set_write_timeout(Some(Duration::from_millis(250)))?;
                Ok(Some(stream))
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(unix)]
impl Drop for Listener {
    fn drop(&mut self) {
        if std::fs::remove_file(&self.socket).is_err() {
            tracing::warn!("automation IPC endpoint cleanup failed");
        }
    }
}

#[cfg(unix)]
fn endpoint_directory(create: bool) -> io::Result<(std::path::PathBuf, std::fs::File)> {
    use std::{
        fs::File,
        path::{Component, Path},
    };
    let root = festerm_sessiond::runtime_root()
        .map_err(|_| io::Error::other("automation runtime root unavailable"))?
        .join("automation");
    if !root.is_absolute() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut parent = File::open("/")?;
    for component in root.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                festerm_unix_security::trusted_private_storage_ancestor(&parent)?;
                let name = Path::new(name);
                parent = match festerm_unix_security::open_directory_search_nofollow(&parent, name)
                {
                    Ok(directory) => directory,
                    Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                        festerm_unix_security::create_private_directory(&parent, name)?
                    }
                    Err(error) => return Err(error),
                };
            }
            _ => return Err(io::ErrorKind::PermissionDenied.into()),
        }
    }
    if !festerm_unix_security::is_current_user_only(&parent, true)? {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok((root, parent))
}

#[cfg(windows)]
struct Listener {
    pipe: std::sync::Mutex<Option<festerm_windows_security::named_pipe::PipeListener>>,
}

#[cfg(windows)]
fn pipe_name() -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let root = festerm_sessiond::runtime_root()
        .map_err(|_| io::Error::other("automation runtime root unavailable"))?;
    let digest: String = Sha256::digest(root.as_os_str().to_string_lossy().as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!(r"\\.\pipe\festerm-automation-{digest}"))
}

#[cfg(windows)]
fn bind_pipe(first: bool) -> io::Result<festerm_windows_security::named_pipe::PipeListener> {
    festerm_windows_security::named_pipe::PipeListener::bind_current_user(&pipe_name()?, first)
}

#[cfg(windows)]
impl Listener {
    fn bind() -> io::Result<Self> {
        Ok(Self {
            pipe: std::sync::Mutex::new(Some(bind_pipe(true)?)),
        })
    }

    fn accept(&self, stop: &AtomicBool) -> io::Result<Option<Stream>> {
        let listener = self
            .pipe
            .lock()
            .map_err(|_| io::Error::other("IPC lock failed"))?
            .take()
            .ok_or_else(|| io::Error::other("IPC listener unavailable"))?;
        let mut stream = listener.accept(stop)?;
        *self
            .pipe
            .lock()
            .map_err(|_| io::Error::other("IPC lock failed"))? = Some(bind_pipe(false)?);
        stream.set_read_timeout(IO_SLICE);
        stream.set_write_timeout(Duration::from_millis(250));
        Ok(Some(stream))
    }
}

pub(crate) fn run_stdio() -> io::Result<()> {
    require_qualified_bootstrap()?;
    #[cfg(unix)]
    let stream = {
        use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
        let (root, _directory) = endpoint_directory(false)?;
        let path = root.join("broker.sock");
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_socket()
            || metadata.uid() != nix::unistd::Uid::effective().as_raw()
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let stream = Stream::connect(path)?;
        stream.set_read_timeout(Some(IO_SLICE))?;
        stream.set_write_timeout(Some(FRAME_DEADLINE))?;
        stream
    };
    #[cfg(windows)]
    let stream = {
        let cancelled = AtomicBool::new(false);
        let mut stream = Stream::connect(&pipe_name()?, FRAME_DEADLINE, &cancelled)?;
        stream.verify_server_user()?;
        stream.set_read_timeout(IO_SLICE);
        stream.set_write_timeout(FRAME_DEADLINE);
        stream
    };
    proxy_stdio(stream, io::BufReader::new(io::stdin()), io::stdout())
}

fn proxy_stdio(
    mut stream: Stream,
    input: impl io::BufRead + Send + 'static,
    mut output: impl Write + Send + 'static,
) -> io::Result<()> {
    let mut reader = stream.try_clone()?;
    prepare_writer(&mut stream)?;
    #[cfg(windows)]
    reader.set_read_timeout(Duration::ZERO);
    let closed = Arc::new(AtomicBool::new(false));
    let (send_error, receive_error) = mpsc::sync_channel(3);
    let (send_input, receive_input) = mpsc::sync_channel::<StdioInput>(4);
    let (send_staged, receive_staged) = mpsc::sync_channel(MAX_BUFFERED_INPUT);
    let output_deadline = Arc::new(std::sync::Mutex::new(None::<Instant>));
    let input_error = send_error.clone();
    let input_closed = closed.clone();
    let input_wake = send_input.clone();
    let forward_error = send_error.clone();
    let forward_closed = closed.clone();
    let forward_output = send_input.clone();
    let output_wake = send_input.clone();
    let reader_wake = send_input.clone();
    thread::Builder::new()
        .name("festerm-mcp-input-forwarder".into())
        .spawn(move || {
            forward_stdio_input(
                |timeout| receive_staged.recv_timeout(timeout),
                forward_output,
                forward_error,
                forward_closed,
            )
        })?;
    thread::Builder::new()
        .name("festerm-mcp-stdin".into())
        .spawn(move || {
            read_stdio_input(input, send_staged, input_wake, input_error, input_closed)
        })?;

    let (send_output, receive_output) = mpsc::sync_channel::<Vec<u8>>(4);
    let output_error = send_error.clone();
    let output_closed = closed.clone();
    let writer_deadline = output_deadline.clone();
    thread::Builder::new()
        .name("festerm-mcp-stdout".into())
        .spawn(move || {
            while let Ok(bytes) = receive_output.recv() {
                if output_closed.load(Ordering::Acquire) {
                    break;
                }
                match writer_deadline.lock() {
                    Ok(mut deadline) => *deadline = Some(Instant::now() + FRAME_DEADLINE),
                    Err(_) => {
                        let _ =
                            output_error.try_send(io::Error::other("MCP stdout watchdog failed"));
                        super::set_admission_flag(&output_closed, true);
                        let _ = output_wake.try_send(StdioInput::Stop);
                        break;
                    }
                }
                let result = output.write_all(&bytes).and_then(|()| output.flush());
                match writer_deadline.lock() {
                    Ok(mut deadline) => *deadline = None,
                    Err(_) => {
                        let _ =
                            output_error.try_send(io::Error::other("MCP stdout watchdog failed"));
                        super::set_admission_flag(&output_closed, true);
                        let _ = output_wake.try_send(StdioInput::Stop);
                        break;
                    }
                }
                if let Err(error) = result {
                    let _ = output_error.try_send(error);
                    super::set_admission_flag(&output_closed, true);
                    let _ = output_wake.try_send(StdioInput::Stop);
                    break;
                }
            }
        })?;

    let reader_closed = closed.clone();
    thread::Builder::new()
        .name("festerm-mcp-replies".into())
        .spawn(move || {
            let stop = AtomicBool::new(false);
            loop {
                match read_frame(&mut reader, &reader_closed, &stop) {
                    Ok(Some(bytes)) => {
                        if send_output.try_send(bytes).is_err() {
                            let _ = send_error.try_send(io::Error::other(
                                "MCP stdout is backpressured or closed",
                            ));
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error)
                        if error.kind() == io::ErrorKind::Interrupted
                            && reader_closed.load(Ordering::Acquire) =>
                    {
                        break;
                    }
                    Err(error) => {
                        let _ = send_error.try_send(error);
                        break;
                    }
                }
            }
            super::set_admission_flag(&reader_closed, true);
            let _ = reader_wake.try_send(StdioInput::Stop);
        })?;

    let result = loop {
        if closed.load(Ordering::Acquire) {
            break receive_error.try_recv().map_or(Ok(()), Err);
        }
        match output_deadline.lock() {
            Ok(deadline) if deadline.is_some_and(|deadline| Instant::now() >= deadline) => {
                break Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "MCP stdout stalled",
                ));
            }
            Ok(_) => {}
            Err(_) => break Err(io::Error::other("MCP stdout watchdog failed")),
        }
        match receive_input.recv_timeout(IO_SLICE) {
            Ok(StdioInput::Message(bytes)) => {
                if let Err(error) =
                    write_frame_until(&mut stream, &bytes, Instant::now() + FRAME_DEADLINE, || {
                        !closed.load(Ordering::Acquire)
                    })
                {
                    if error.kind() == io::ErrorKind::Interrupted && closed.load(Ordering::Acquire)
                    {
                        continue;
                    }
                    break Err(error);
                }
            }
            Ok(StdioInput::Stop) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) if closed.load(Ordering::Acquire) => {
                break receive_error.try_recv().map_or(Ok(()), Err);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break receive_error
                    .try_recv()
                    .map_or_else(|_| Err(io::Error::other("MCP input worker stopped")), Err);
            }
        }
    };
    super::set_admission_flag(&closed, true);
    result
}

#[cfg(test)]
pub(super) fn test_server(stop: Arc<AtomicBool>) -> (Server, SyncSender<Event>) {
    let (sender, events) = mpsc::sync_channel(4);
    (
        Server {
            events,
            stop,
            privacy_closed: Arc::new(AtomicBool::new(false)),
            thread: None,
        },
        sender,
    )
}

#[cfg(test)]
pub(super) fn test_connection_with_frames(
    key: u64,
) -> (Connection, impl Fn() -> Option<OutboundFrame>) {
    let (output, receiver) = outbound_channel();
    (
        Connection {
            key,
            closed: Arc::new(AtomicBool::new(false)),
            pending_input: Arc::new(AtomicUsize::new(0)),
            output,
            #[cfg(any(unix, windows))]
            output_wake: None,
        },
        move || receiver.try_recv().ok(),
    )
}

#[cfg(test)]
pub(super) fn test_connection(key: u64) -> (Connection, impl Fn() -> Option<Vec<u8>>) {
    let (connection, receive) = test_connection_with_frames(key);
    (connection, move || receive().map(|frame| frame.bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn connection_refuses_incoming_events_after_stop() {
        let (connection, _) = test_connection(47);
        let (events, incoming) = mpsc::sync_channel(1);
        let stop = AtomicBool::new(true);
        let privacy_closed = AtomicBool::new(false);
        assert_eq!(
            connection
                .enqueue_message(&events, b"request".to_vec(), &stop, &privacy_closed)
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        assert!(connection.is_closed());
        assert!(incoming.try_recv().is_err());
        let guard = festerm_session::automation_admission_lock().unwrap();
        assert!(!connection.has_pending_input(&guard));
    }

    #[test]
    fn incoming_message_rechecks_observed_privacy_loss_under_admission() {
        let (connection, _) = test_connection(47);
        let (events, incoming) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let gate = super::super::PrivacyGate::new(false);
        let privacy_closed = gate.closed_flag();
        let guard = festerm_session::automation_admission_lock().unwrap();
        let worker_connection = connection.clone();
        let worker_stop = stop.clone();
        let (started, ready) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            started.send(()).unwrap();
            worker_connection.enqueue_message(
                &events,
                b"request".to_vec(),
                &worker_stop,
                &privacy_closed,
            )
        });
        ready.recv_timeout(FRAME_DEADLINE).unwrap();
        gate.signal_loss();
        assert!(!stop.load(Ordering::Acquire));
        drop(guard);

        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert!(connection.is_closed());
        assert!(incoming.try_recv().is_err());
    }

    #[test]
    fn queued_reply_rechecks_observed_privacy_loss_before_write() {
        let (connection, receive) = test_connection_with_frames(47);
        let deadline = Instant::now() + FRAME_DEADLINE;
        connection.send(b"private".to_vec(), deadline, &[]).unwrap();
        let frame = receive().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let gate = super::super::PrivacyGate::new(false);
        let privacy_closed = gate.closed_flag();
        let guard = festerm_session::automation_admission_lock().unwrap();
        let worker_stop = stop.clone();
        let (started, ready) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let mut written = Vec::new();
            started.send(()).unwrap();
            let result = write_frame_until(&mut written, &frame.bytes, frame.expires, || {
                peer_frame_is_live(&connection, &worker_stop, &privacy_closed, &frame)
            });
            (result, written)
        });
        ready.recv_timeout(FRAME_DEADLINE).unwrap();
        gate.signal_loss();
        assert!(!stop.load(Ordering::Acquire));
        drop(guard);

        let (result, written) = worker.join().unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert!(written.is_empty());
    }

    #[test]
    fn connection_closes_on_extracted_queue_full_or_disconnected() {
        let (output, receiver) = outbound_channel();
        let connection = Connection {
            key: 1,
            closed: Arc::new(AtomicBool::new(false)),
            pending_input: Arc::new(AtomicUsize::new(0)),
            output,
            #[cfg(any(unix, windows))]
            output_wake: None,
        };
        let expiry = Instant::now() + Duration::from_secs(30);
        for byte in 0..4 {
            connection.send(vec![byte], expiry, &[]).unwrap();
        }
        assert_eq!(
            connection.send(vec![4], expiry, &[]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(connection.is_closed());
        for byte in 0..4 {
            assert_eq!(receiver.try_recv().unwrap().bytes, [byte]);
        }
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(
            connection.send(vec![5], expiry, &[]).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );

        let (output, receiver) = outbound_channel();
        let connection = Connection {
            key: 2,
            closed: Arc::new(AtomicBool::new(false)),
            pending_input: Arc::new(AtomicUsize::new(0)),
            output,
            #[cfg(any(unix, windows))]
            output_wake: None,
        };
        drop(receiver);
        assert_eq!(
            connection.send(vec![1], expiry, &[]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(connection.is_closed());
    }

    #[test]
    fn connection_enqueue_serializes_validation_with_revocation() {
        if super::super::tests::isolate_poison_panic_test(
            "FESTERM_ENQUEUE_ADMISSION_CHILD",
            "automation::ipc::tests::connection_enqueue_serializes_validation_with_revocation",
        ) {
            return;
        }
        let (output, receiver) = outbound_channel();
        let connection = Connection {
            key: 3,
            closed: Arc::new(AtomicBool::new(false)),
            pending_input: Arc::new(AtomicUsize::new(0)),
            output,
            #[cfg(any(unix, windows))]
            output_wake: None,
        };
        let valid = Arc::new(AtomicBool::new(true));
        let (entered, entered_rx) = mpsc::sync_channel(1);
        let (resume, resume_rx) = mpsc::sync_channel(1);
        let sender_connection = connection.clone();
        let sender_valid = valid.clone();
        let sender = thread::spawn(move || {
            ENQUEUE_TEST_GATE.with(|slot| {
                *slot.borrow_mut() = Some(EnqueueTestGate {
                    entered,
                    resume: resume_rx,
                });
            });
            sender_connection.send_guarded(
                b"secret".to_vec(),
                Instant::now() + Duration::from_secs(30),
                &[],
                &[sender_valid],
            )
        });
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("enqueue did not reach the publication gate");
        let busy = match festerm_session::try_automation_admission_lock() {
            Err(festerm_session::AutomationAdmissionTryError::Busy) => true,
            Ok(guard) => {
                valid.store(false, Ordering::Release);
                drop(guard);
                false
            }
            Err(festerm_session::AutomationAdmissionTryError::Poisoned) => {
                panic!("admission was poisoned")
            }
        };
        resume.send(()).unwrap();
        let result = sender.join().unwrap();
        assert!(busy, "revocation crossed validation before insertion");
        assert!(result.is_ok());

        let guard = festerm_session::automation_admission_lock().unwrap();
        valid.store(false, Ordering::Release);
        let queued = receiver.try_recv().unwrap();
        assert!(!queued.is_valid());
        drop(guard);
        assert_eq!(
            connection
                .send_guarded(
                    b"late".to_vec(),
                    Instant::now() + Duration::from_secs(30),
                    &[],
                    &[valid],
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        assert!(connection.is_closed());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn beta_bootstrap_requires_a_supported_platform() {
        assert!(beta_bootstrap(true).is_ok());
        assert_eq!(
            beta_bootstrap(false).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            BETA_PLATFORM,
            cfg!(any(target_os = "macos", target_os = "linux", windows))
        );
    }

    #[test]
    fn unqualified_bootstrap_refuses_server_and_stdio_before_ipc() {
        let server = Server::start(
            Arc::new(|| {}),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(server.err().unwrap().kind(), io::ErrorKind::Unsupported);
        assert_eq!(run_stdio().unwrap_err().kind(), io::ErrorKind::Unsupported);
    }

    struct PausedOutput {
        entered: SyncSender<()>,
        release: Receiver<()>,
        finished: SyncSender<()>,
    }

    impl Write for PausedOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.entered.try_send(());
            self.release
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| io::ErrorKind::TimedOut)?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for PausedOutput {
        fn drop(&mut self) {
            let _ = self.finished.try_send(());
        }
    }

    #[cfg(unix)]
    struct StdioPair {
        broker: Stream,
        adapter: Stream,
        input: Stream,
        reader: Stream,
        output: PausedOutput,
        waiting: Receiver<()>,
        release: SyncSender<()>,
        output_finished: Receiver<()>,
    }

    #[cfg(unix)]
    fn stdio_pair() -> StdioPair {
        let (broker, adapter) = Stream::pair().unwrap();
        broker.set_read_timeout(Some(FRAME_DEADLINE)).unwrap();
        broker.set_write_timeout(Some(FRAME_DEADLINE)).unwrap();
        adapter.set_read_timeout(Some(IO_SLICE)).unwrap();
        adapter.set_write_timeout(Some(FRAME_DEADLINE)).unwrap();
        let (input, reader) = Stream::pair().unwrap();
        let (entered, waiting) = mpsc::sync_channel(1);
        let (release, resume) = mpsc::sync_channel(1);
        let (finished, output_finished) = mpsc::sync_channel(1);
        StdioPair {
            broker,
            adapter,
            input,
            reader,
            output: PausedOutput {
                entered,
                release: resume,
                finished,
            },
            waiting,
            release,
            output_finished,
        }
    }

    #[cfg(unix)]
    #[test]
    fn stdio_eof_closes_the_private_channel_even_when_stdout_is_blocked() {
        let StdioPair {
            mut broker,
            adapter,
            mut input,
            reader,
            output,
            waiting,
            release,
            output_finished,
        } = stdio_pair();
        let (finished, result) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let result = proxy_stdio(adapter, io::BufReader::new(reader), output);
            let _ = finished.send(result);
        });
        input.write_all(b"{}\n").unwrap();
        let no = AtomicBool::new(false);
        assert_eq!(
            read_frame(&mut broker, &no, &no).unwrap(),
            Some(b"{}".to_vec())
        );
        write_frame(&mut broker, b"reply\n").unwrap();
        waiting.recv_timeout(FRAME_DEADLINE).unwrap();

        drop(input);
        let completed = result.recv_timeout(Duration::from_secs(1));
        let mut byte = [0];
        let disconnected = broker.read(&mut byte);
        let _ = release.send(());
        assert!(completed.unwrap().is_ok());
        assert!(matches!(disconnected, Ok(0)));
        worker.join().unwrap();
        output_finished.recv_timeout(FRAME_DEADLINE).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stdio_forwards_a_healthy_input_burst() {
        let StdioPair {
            mut broker,
            adapter,
            mut input,
            reader,
            output,
            waiting: _waiting,
            release: _release,
            output_finished,
        } = stdio_pair();
        let (finished, result) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let result = proxy_stdio(adapter, io::BufReader::new(reader), output);
            let _ = finished.send(result);
        });
        input.write_all(&b"{}\n".repeat(12)).unwrap();
        let no = AtomicBool::new(false);
        for _ in 0..12 {
            assert_eq!(
                read_frame(&mut broker, &no, &no).unwrap(),
                Some(b"{}".to_vec())
            );
        }
        drop(input);
        assert!(result.recv_timeout(FRAME_DEADLINE).unwrap().is_ok());
        worker.join().unwrap();
        output_finished.recv_timeout(FRAME_DEADLINE).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stdio_stdout_backpressure_closes_the_private_channel() {
        let StdioPair {
            mut broker,
            adapter,
            input,
            reader,
            output,
            waiting,
            release,
            output_finished,
        } = stdio_pair();
        let (finished, result) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let result = proxy_stdio(adapter, io::BufReader::new(reader), output);
            let _ = finished.send(result);
        });
        write_frame(&mut broker, b"reply\n").unwrap();
        waiting.recv_timeout(FRAME_DEADLINE).unwrap();
        for _ in 0..5 {
            write_frame(&mut broker, b"queued\n").unwrap();
        }
        let completed = result.recv_timeout(Duration::from_secs(1));
        let mut byte = [0];
        let disconnected = broker.read(&mut byte);
        drop(input);
        let _ = release.send(());
        assert!(completed.unwrap().is_err());
        assert!(matches!(disconnected, Ok(0)));
        worker.join().unwrap();
        output_finished.recv_timeout(FRAME_DEADLINE).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stdio_single_stalled_stdout_write_times_out_and_revokes() {
        let StdioPair {
            mut broker,
            adapter,
            input,
            reader,
            output,
            waiting,
            release,
            output_finished,
        } = stdio_pair();
        let (finished, result) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let result = proxy_stdio(adapter, io::BufReader::new(reader), output);
            let _ = finished.send(result);
        });
        write_frame(&mut broker, b"reply\n").unwrap();
        waiting.recv_timeout(FRAME_DEADLINE).unwrap();
        let completed = result.recv_timeout(FRAME_DEADLINE + Duration::from_secs(1));
        let mut byte = [0];
        let disconnected = broker.read(&mut byte);
        drop(input);
        let _ = release.send(());
        assert_eq!(
            completed.unwrap().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(matches!(disconnected, Ok(0)));
        worker.join().unwrap();
        output_finished.recv_timeout(FRAME_DEADLINE).unwrap();
    }

    #[test]
    fn bounded_input_waits_for_a_briefly_full_queue() {
        let (sender, receiver) = mpsc::sync_channel(4);
        for value in 0..4 {
            sender.send(value).unwrap();
        }
        let closed = Arc::new(AtomicBool::new(false));
        let worker_closed = closed.clone();
        let (started, waiting) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            started.send(()).unwrap();
            send_until(&sender, 4, Instant::now() + FRAME_DEADLINE, &worker_closed)
        });
        waiting.recv_timeout(FRAME_DEADLINE).unwrap();
        thread::sleep(Duration::from_millis(50));
        for value in 0..4 {
            assert_eq!(receiver.recv().unwrap(), value);
        }
        worker.join().unwrap().unwrap();
        assert_eq!(receiver.recv().unwrap(), 4);
    }

    #[test]
    fn bounded_input_wait_is_cancelled_when_the_channel_closes() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        sender.send(1).unwrap();
        let closed = Arc::new(AtomicBool::new(false));
        let worker_closed = closed.clone();
        let (started, waiting) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            started.send(()).unwrap();
            send_until(&sender, 2, Instant::now() + FRAME_DEADLINE, &worker_closed)
        });
        waiting.recv_timeout(FRAME_DEADLINE).unwrap();
        super::super::set_admission_flag(&closed, true);
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn stdio_eof_revokes_even_when_forwarding_queue_is_full() {
        let closed = Arc::new(AtomicBool::new(false));
        let (output, _blocked) = mpsc::sync_channel(4);
        for _ in 0..4 {
            output
                .send(StdioInput::Message(b"queued".to_vec()))
                .unwrap();
        }
        let (staged, incoming) = mpsc::sync_channel(1);
        staged.send(b"pending".to_vec()).unwrap();
        let (errors, receive_errors) = mpsc::sync_channel(3);
        let (dequeued, dequeued_receiver) = mpsc::sync_channel(1);
        let forward_closed = closed.clone();
        let forward_output = output.clone();
        let forward_errors = errors.clone();
        let forwarder = thread::spawn(move || {
            forward_stdio_input(
                |timeout| {
                    let result = incoming.recv_timeout(timeout);
                    if result.is_ok() {
                        dequeued.send(()).unwrap();
                    }
                    result
                },
                forward_output,
                forward_errors,
                forward_closed,
            )
        });
        dequeued_receiver.recv_timeout(FRAME_DEADLINE).unwrap();
        let read_closed = closed.clone();
        let (finished, result) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            read_stdio_input(
                Cursor::new(b"{}\n".to_vec()),
                staged,
                output,
                errors,
                read_closed,
            );
            finished.send(()).unwrap();
        });
        result.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(closed.load(Ordering::Acquire));
        reader.join().unwrap();
        forwarder.join().unwrap();
        assert!(receive_errors.try_recv().is_err());
    }

    #[test]
    fn stdio_input_overflow_closes_instead_of_waiting_for_eof() {
        let closed = Arc::new(AtomicBool::new(false));
        let (staged, _incoming) = mpsc::sync_channel(MAX_BUFFERED_INPUT);
        let (output, _blocked) = mpsc::sync_channel(1);
        let (errors, received) = mpsc::sync_channel(1);
        read_stdio_input(
            Cursor::new(b"{}\n".repeat(MAX_BUFFERED_INPUT + 1)),
            staged,
            output,
            errors,
            closed.clone(),
        );
        assert!(closed.load(Ordering::Acquire));
        assert_eq!(received.try_recv().unwrap().kind(), io::ErrorKind::Other);
    }

    #[test]
    fn stdio_closure_serializes_with_private_write_admission() {
        struct PausedWrite {
            entered: SyncSender<()>,
            resume: Receiver<()>,
        }
        impl Write for PausedWrite {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.entered
                    .send(())
                    .map_err(|_| io::ErrorKind::BrokenPipe)?;
                self.resume
                    .recv_timeout(FRAME_DEADLINE)
                    .map_err(|_| io::ErrorKind::TimedOut)?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let closed = Arc::new(AtomicBool::new(false));
        let writer_closed = closed.clone();
        let (entered, waiting) = mpsc::sync_channel(1);
        let (resume, suspended) = mpsc::sync_channel(1);
        let writer = thread::spawn(move || {
            write_until(
                &mut PausedWrite {
                    entered,
                    resume: suspended,
                },
                b"admitted",
                Instant::now() + FRAME_DEADLINE,
                &|| !writer_closed.load(Ordering::Acquire),
            )
        });
        waiting.recv_timeout(FRAME_DEADLINE).unwrap();
        let closer_closed = closed.clone();
        let (stopped, observed) = mpsc::sync_channel(1);
        let closer = thread::spawn(move || {
            super::super::set_admission_flag(&closer_closed, true);
            stopped.send(()).unwrap();
        });
        let early_stop = observed.recv_timeout(Duration::from_millis(50));
        resume.send(()).unwrap();
        writer.join().unwrap().unwrap();
        closer.join().unwrap();
        assert!(matches!(early_stop, Err(mpsc::RecvTimeoutError::Timeout)));
        observed.recv_timeout(FRAME_DEADLINE).unwrap();
        let mut later = Vec::new();
        assert_eq!(
            write_until(
                &mut later,
                b"rejected",
                Instant::now() + FRAME_DEADLINE,
                &|| !closed.load(Ordering::Acquire),
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(later.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn stdio_reports_malformed_input_as_a_failure() {
        let StdioPair {
            broker: _broker,
            adapter,
            mut input,
            reader,
            output,
            waiting: _waiting,
            release: _release,
            output_finished,
        } = stdio_pair();
        let (finished, result) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let result = proxy_stdio(adapter, io::BufReader::new(reader), output);
            let _ = finished.send(result);
        });
        input.write_all(b"unterminated").unwrap();
        drop(input);
        assert!(result.recv_timeout(FRAME_DEADLINE).unwrap().is_err());
        worker.join().unwrap();
        output_finished.recv_timeout(FRAME_DEADLINE).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_stdio_eof_revokes_while_stdout_is_blocked() {
        struct ChannelInput {
            incoming: Receiver<Vec<u8>>,
            current: Cursor<Vec<u8>>,
        }

        impl Read for ChannelInput {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                loop {
                    let count = self.current.read(buffer)?;
                    if count > 0 || buffer.is_empty() {
                        return Ok(count);
                    }
                    match self.incoming.recv() {
                        Ok(bytes) => self.current = Cursor::new(bytes),
                        Err(_) => return Ok(0),
                    }
                }
            }
        }

        let name = format!(
            r"\\.\pipe\festerm-automation-stdio-test-{}",
            std::process::id()
        );
        let listener =
            festerm_windows_security::named_pipe::PipeListener::bind_current_user(&name, true)
                .unwrap();
        let mut adapter = Stream::connect(&name, FRAME_DEADLINE, &AtomicBool::new(false)).unwrap();
        let mut broker = listener.accept(&AtomicBool::new(false)).unwrap();
        adapter.set_read_timeout(IO_SLICE);
        adapter.set_write_timeout(FRAME_DEADLINE);
        broker.set_read_timeout(FRAME_DEADLINE);
        let (send_input, incoming) = mpsc::sync_channel(1);
        let input = io::BufReader::new(ChannelInput {
            incoming,
            current: Cursor::new(Vec::new()),
        });
        let (entered, waiting) = mpsc::sync_channel(1);
        let (release, resume) = mpsc::sync_channel(1);
        let (finished, output_finished) = mpsc::sync_channel(1);
        let output = PausedOutput {
            entered,
            release: resume,
            finished,
        };
        let (finished, result) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let result = proxy_stdio(adapter, input, output);
            let _ = finished.send(result);
        });
        send_input.send(b"{}\n".to_vec()).unwrap();
        let no = AtomicBool::new(false);
        assert_eq!(
            read_frame(&mut broker, &no, &no).unwrap(),
            Some(b"{}".to_vec())
        );
        write_frame(&mut broker, b"reply\n").unwrap();
        waiting.recv_timeout(FRAME_DEADLINE).unwrap();
        drop(send_input);
        let completed = result.recv_timeout(Duration::from_secs(1));
        let mut byte = [0];
        let disconnected = broker.read(&mut byte);
        let _ = release.send(());
        assert!(completed.unwrap().is_ok());
        assert!(matches!(disconnected, Ok(0)));
        worker.join().unwrap();
        output_finished.recv_timeout(FRAME_DEADLINE).unwrap();
    }

    #[test]
    fn private_frames_refuse_oversize_and_incomplete_payloads_before_dispatch() {
        let no = AtomicBool::new(false);
        let mut invalid = Cursor::new((MAX_FRAME as u32 + 1).to_be_bytes());
        assert_eq!(
            read_frame(&mut invalid, &no, &no).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let mut incomplete = Cursor::new([0, 0, 0, 3, b'{']);
        assert_eq!(
            read_frame(&mut incomplete, &no, &no).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        let mut encoded = Vec::new();
        write_frame(&mut encoded, b"{}").unwrap();
        assert_eq!(
            read_frame(&mut Cursor::new(encoded), &no, &no).unwrap(),
            Some(b"{}".to_vec())
        );
        assert!(write_frame(&mut Vec::new(), &vec![0; MAX_FRAME + 1]).is_err());
    }

    #[test]
    fn stopped_private_transport_admits_no_more_messages() {
        let yes = AtomicBool::new(true);
        let no = AtomicBool::new(false);
        let mut encoded = Vec::new();
        write_frame(&mut encoded, b"{}").unwrap();
        assert!(read_frame(&mut Cursor::new(encoded), &yes, &no)
            .unwrap()
            .is_none());
    }

    #[test]
    fn queued_reply_revocation_and_expiry_prevent_transport_admission() {
        let mut bytes = Vec::new();
        let future = Instant::now() + Duration::from_secs(2);
        assert_eq!(
            write_frame_until(&mut bytes, b"secret", future, || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted,
        );
        assert!(bytes.is_empty());
        assert!(write_frame_until(&mut bytes, b"secret", Instant::now(), || true).is_err());
        assert!(bytes.is_empty());
        let (connection, _receive) = test_connection(1);
        connection.close();
        assert!(connection.send(b"secret".to_vec(), future, &[]).is_err());
    }

    #[test]
    fn backpressured_reply_cannot_write_after_revocation() {
        struct Backpressured {
            attempted: SyncSender<()>,
            writable: Arc<AtomicBool>,
            written: Arc<std::sync::Mutex<Vec<u8>>>,
        }

        impl Write for Backpressured {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if !self.writable.load(Ordering::Acquire) {
                    let _ = self.attempted.try_send(());
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                self.written.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let (attempted, blocked) = mpsc::sync_channel(1);
        let writable = Arc::new(AtomicBool::new(false));
        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let live = Arc::new(AtomicBool::new(true));
        let worker_live = live.clone();
        let mut writer = Backpressured {
            attempted,
            writable: writable.clone(),
            written: written.clone(),
        };
        let worker = thread::spawn(move || {
            write_frame_until(
                &mut writer,
                b"secret",
                Instant::now() + FRAME_DEADLINE,
                || worker_live.load(Ordering::Acquire),
            )
        });
        blocked.recv_timeout(FRAME_DEADLINE).unwrap();
        super::super::set_admission_flag(&live, false);
        writable.store(true, Ordering::Release);
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert!(written.lock().unwrap().is_empty());
    }

    #[test]
    fn revocation_serializes_with_an_in_flight_write_admission() {
        struct PausedWrite {
            entered: SyncSender<()>,
            release: Receiver<()>,
        }

        impl Write for PausedWrite {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.entered.send(()).unwrap();
                self.release.recv_timeout(FRAME_DEADLINE).unwrap();
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let (entered, writing) = mpsc::sync_channel(1);
        let (release, resume) = mpsc::sync_channel(1);
        let live = Arc::new(AtomicBool::new(true));
        let writer_live = live.clone();
        let worker = thread::spawn(move || {
            let mut writer = PausedWrite {
                entered,
                release: resume,
            };
            write_until(
                &mut writer,
                b"admitted",
                Instant::now() + FRAME_DEADLINE,
                &|| writer_live.load(Ordering::Acquire),
            )
        });
        writing.recv_timeout(FRAME_DEADLINE).unwrap();
        let (revoking, started) = mpsc::sync_channel(1);
        let (finished, revoked) = mpsc::sync_channel(1);
        let revoker = thread::spawn(move || {
            revoking.send(()).unwrap();
            super::super::set_admission_flag(&live, false);
            finished.send(()).unwrap();
        });
        started.recv_timeout(FRAME_DEADLINE).unwrap();
        let early = revoked.recv_timeout(Duration::from_millis(50));
        release.send(()).unwrap();
        worker.join().unwrap().unwrap();
        if early.is_err() {
            revoked.recv_timeout(FRAME_DEADLINE).unwrap();
        }
        revoker.join().unwrap();
        assert!(matches!(early, Err(mpsc::RecvTimeoutError::Timeout)));
    }

    #[cfg(windows)]
    #[test]
    fn full_windows_pipe_retries_only_after_rechecking_revocation() {
        struct NotifyingWriter {
            pipe: Stream,
            attempted: SyncSender<()>,
        }

        impl Write for NotifyingWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let _ = self.attempted.try_send(());
                self.pipe.write(bytes)
            }

            fn flush(&mut self) -> io::Result<()> {
                self.pipe.flush()
            }
        }

        let name = format!(
            r"\\.\pipe\festerm-automation-revocation-test-{}",
            std::process::id()
        );
        let listener =
            festerm_windows_security::named_pipe::PipeListener::bind_current_user(&name, true)
                .unwrap();
        let mut client = Stream::connect(&name, FRAME_DEADLINE, &AtomicBool::new(false)).unwrap();
        let mut server = listener.accept(&AtomicBool::new(false)).unwrap();
        prepare_writer(&mut server).unwrap();
        client.set_read_timeout(FRAME_DEADLINE);
        let mut filled = 0;
        let mut full = false;
        for _ in 0..1024 {
            match server.write(&[b'x'; 4096]) {
                Ok(count) => filled += count,
                Err(error) if idle_error(&error) => {
                    full = true;
                    break;
                }
                Err(error) => panic!("filling private pipe failed: {error}"),
            }
        }
        assert!(full && filled > 0);

        let (attempted, blocked) = mpsc::sync_channel(1);
        let live = Arc::new(AtomicBool::new(true));
        let writer_live = live.clone();
        let worker = thread::spawn(move || {
            let mut writer = NotifyingWriter {
                pipe: server,
                attempted,
            };
            write_until(
                &mut writer,
                b"secret",
                Instant::now() + FRAME_DEADLINE,
                &|| writer_live.load(Ordering::Acquire),
            )
        });
        blocked.recv_timeout(FRAME_DEADLINE).unwrap();
        super::super::set_admission_flag(&live, false);
        let mut received = vec![0; filled];
        client.read_exact(&mut received).unwrap();
        assert!(received.iter().all(|byte| *byte == b'x'));
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[cfg(unix)]
    #[test]
    fn queued_reply_is_dropped_when_target_is_replaced_before_dispatch() {
        let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
        server.set_read_timeout(Some(IO_SLICE)).unwrap();
        server.set_write_timeout(Some(IO_SLICE)).unwrap();
        let (output, replies) = outbound_channel();
        let connection = Connection {
            key: 8,
            closed: Arc::new(AtomicBool::new(false)),
            pending_input: Arc::new(AtomicUsize::new(0)),
            output,
            output_wake: None,
        };
        let attachment = Arc::new(festerm_session::AutomationEpoch::new());
        attachment.transition(true);
        let target = TargetGuard {
            liveness: Arc::new(AtomicBool::new(true)),
            registration: Arc::new(AtomicBool::new(true)),
            generation: attachment.capture().unwrap(),
            attachment,
        };
        connection
            .send(
                b"secret".to_vec(),
                Instant::now() + FRAME_DEADLINE,
                std::slice::from_ref(&target),
            )
            .unwrap();
        target.attachment.transition(false);
        let (events, _incoming) = mpsc::sync_channel(4);
        let worker = thread::spawn(move || {
            serve_peer(
                server,
                connection,
                replies,
                events,
                Arc::new(|| {}),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            )
        });
        worker.join().unwrap();
        let mut bytes = [0; 16];
        assert_eq!(client.read(&mut bytes).unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn queued_controller_reply_closes_when_registration_is_invalidated() {
        let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
        server.set_read_timeout(Some(IO_SLICE)).unwrap();
        let (output, replies) = outbound_channel();
        let connection = Connection {
            key: 9,
            closed: Arc::new(AtomicBool::new(false)),
            pending_input: Arc::new(AtomicUsize::new(0)),
            output,
            output_wake: None,
        };
        let valid = Arc::new(AtomicBool::new(true));
        connection
            .send_guarded(
                b"non-authorizing correlation".to_vec(),
                Instant::now() + FRAME_DEADLINE,
                &[],
                std::slice::from_ref(&valid),
            )
            .unwrap();
        super::super::set_admission_flag(&valid, false);
        let (events, _incoming) = mpsc::sync_channel(4);
        let observed = connection.clone();
        let worker = thread::spawn(move || {
            serve_peer(
                server,
                connection,
                replies,
                events,
                Arc::new(|| {}),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            )
        });
        worker.join().unwrap();
        let mut bytes = [0; 16];
        assert_eq!(client.read(&mut bytes).unwrap(), 0);
        assert!(observed.is_closed());
    }

    #[test]
    fn connection_identity_exhaustion_never_reuses_a_channel() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(next_connection_key(&counter), Some(u64::MAX - 1));
        assert_eq!(next_connection_key(&counter), None);
        assert_eq!(next_connection_key(&counter), None);
    }

    #[cfg(unix)]
    #[test]
    fn private_peer_round_trips_frames_and_stops_after_revocation() {
        let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
        server.set_read_timeout(Some(IO_SLICE)).unwrap();
        server.set_write_timeout(Some(IO_SLICE)).unwrap();
        client.set_read_timeout(Some(IO_SLICE)).unwrap();
        client.set_write_timeout(Some(IO_SLICE)).unwrap();
        let (output, replies) = outbound_channel();
        let connection = Connection {
            key: 7,
            closed: Arc::new(AtomicBool::new(false)),
            pending_input: Arc::new(AtomicUsize::new(0)),
            output,
            output_wake: None,
        };
        let (events, incoming) = mpsc::sync_channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let peer = connection.clone();
        let worker_stop = stop.clone();
        let worker = thread::spawn(move || {
            serve_peer(
                server,
                peer,
                replies,
                events,
                Arc::new(|| {}),
                worker_stop,
                Arc::new(AtomicBool::new(false)),
            )
        });

        assert!(matches!(
            incoming.recv_timeout(FRAME_DEADLINE).unwrap(),
            Event::Connected(_)
        ));
        write_frame(&mut client, b"{\"request\":1}").unwrap();
        assert!(matches!(
            incoming.recv_timeout(FRAME_DEADLINE).unwrap(),
            Event::Message { key: 7, bytes } if bytes == b"{\"request\":1}"
        ));
        connection
            .send(
                b"{\"response\":1}\n".to_vec(),
                Instant::now() + FRAME_DEADLINE,
                &[],
            )
            .unwrap();
        let closed = AtomicBool::new(false);
        assert_eq!(
            read_frame(&mut client, &closed, &stop).unwrap(),
            Some(b"{\"response\":1}\n".to_vec())
        );
        connection.close();
        assert!(connection
            .send(b"late".to_vec(), Instant::now() + FRAME_DEADLINE, &[])
            .is_err());
        worker.join().unwrap();
    }
}
