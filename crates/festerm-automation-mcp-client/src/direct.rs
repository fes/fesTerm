use std::{
    fmt,
    future::Future,
    io::{self, Read, Write},
    mem,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, RwLock, TryLockError},
    time::{Duration, Instant},
};

use festerm_automation_mcp::{
    secure_transport::{
        self, CleanupStatus, Progress, Role, SecureTransport, TrustedNativeVerification,
    },
    MAX_DEPTH, MAX_MESSAGE_BYTES,
};
#[cfg(windows)]
use festerm_windows_security::named_pipe::Pipe as DirectStream;
use rmcp::{
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
    transport::Transport,
    RoleClient,
};
use serde::Serialize;
use serde_json::ser::Formatter;
#[cfg(windows)]
use std::sync::Mutex;
#[cfg(unix)]
use tokio::net::UnixStream as DirectStream;
use tokio::sync::{mpsc, oneshot, watch, Notify};

const QUEUE_CAPACITY: usize = 4;
const FAIRNESS_BUDGET: usize = 32;
#[cfg(windows)]
const PIPE_RETRY: Duration = Duration::from_millis(20);

/// Wake the driver when a native decision or write-admission state changes.
#[derive(Clone, Default)]
pub struct BoundaryWake(Arc<Notify>);

impl BoundaryWake {
    pub fn wake(&self) {
        self.0.notify_one();
    }
}

/// A nonblocking, client-owned fence held through each physical write.
///
/// The implementation must validate the live local caller and privacy state
/// appropriate to the connection. Return `WouldBlock` only for temporary
/// contention and wake the driver when that state changes; any other error
/// closes the connection. Cancellation must report failure unless all
/// connection-owned resources are retired, even if admission was never used.
/// Callbacks, guards and destructors must not block or panic with sensitive
/// payloads: the process-wide panic hook runs before a panic can be caught.
/// This client-side fence does not replace the application's independent
/// authority, recipient, lock, and effect-admission checks.
pub trait ClientWriteAdmission: Send + 'static {
    type Guard<'a>
    where
        Self: 'a;

    fn try_admit(&self) -> io::Result<Self::Guard<'_>>;
    fn cancel(&mut self) -> Result<(), AdmissionCleanupFailed>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionCleanupFailed;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    RuntimeUnavailable,
    Closed,
    Io(io::ErrorKind),
    Channel(secure_transport::Error),
    InvalidMessage,
    FrameTooLarge,
    Backpressure,
    SendCancelled,
    StatePoisoned,
    AdmissionCleanupFailed,
    DriverStopped,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeUnavailable => formatter.write_str("Tokio runtime unavailable"),
            Self::Closed => formatter.write_str("direct MCP client closed"),
            Self::Io(kind) => write!(formatter, "direct MCP client I/O failed: {kind:?}"),
            Self::Channel(error) => write!(formatter, "{error}"),
            Self::InvalidMessage => formatter.write_str("direct MCP message refused"),
            Self::FrameTooLarge => formatter.write_str("direct MCP message exceeds its limit"),
            Self::Backpressure => formatter.write_str("direct MCP queue is full"),
            Self::SendCancelled => formatter.write_str("direct MCP send was cancelled"),
            Self::StatePoisoned => formatter.write_str("direct MCP cancellation state failed"),
            Self::AdmissionCleanupFailed => {
                formatter.write_str("direct MCP write-admission cleanup failed")
            }
            Self::DriverStopped => formatter.write_str("direct MCP driver stopped unexpectedly"),
        }
    }
}

impl std::error::Error for Error {}

/// Completion and cleanup remain observable after the SDK consumes its transport.
#[derive(Clone)]
pub struct ConnectorStatus {
    completion: watch::Receiver<Option<Result<(), Error>>>,
    cleanup: CleanupStatus,
    shared: Arc<Shared>,
}

impl ConnectorStatus {
    pub fn result(&self) -> Option<Result<(), Error>> {
        *self.completion.borrow()
    }

    /// `false` does not mean cleanup has completed; wait for the result first.
    pub fn cleanup_failed(&self) -> bool {
        self.cleanup.failed()
            || matches!(
                *self.completion.borrow(),
                Some(Err(Error::AdmissionCleanupFailed))
            )
    }

    /// Close on a local lifecycle or privacy cut, even while the channel is idle.
    /// This serializes with an in-progress physical write and may wait for it;
    /// call it off the UI thread. Any in-flight action may already have reached
    /// the peer.
    pub fn disconnect(&self) {
        self.shared.request(Stop::Requested);
    }

    pub async fn wait(&mut self) -> Result<(), Error> {
        loop {
            if let Some(result) = *self.completion.borrow_and_update() {
                return result;
            }
            if self.completion.changed().await.is_err() {
                return Err(Error::DriverStopped);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Stop {
    Requested,
    Failed(Error),
}

struct Shared {
    // Read-only checks must not stall a physical write. Revocation takes the
    // exclusive side and wakes the driver after releasing it.
    stop: RwLock<Option<Stop>>,
    wake: BoundaryWake,
}

impl Shared {
    fn request(&self, requested: Stop) {
        match self.stop.write() {
            Ok(mut stop) => {
                if stop.is_none() {
                    *stop = Some(requested);
                }
            }
            Err(poisoned) => {
                *poisoned.into_inner() = Some(Stop::Failed(Error::StatePoisoned));
            }
        }
        self.wake.wake();
    }

    fn stop_reason(&self) -> Option<Stop> {
        match self.stop.read() {
            Ok(stop) => *stop,
            Err(_) => Some(Stop::Failed(Error::StatePoisoned)),
        }
    }
}

struct CancelOnDrop {
    shared: Arc<Shared>,
    armed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.shared.request(Stop::Failed(Error::SendCancelled));
        }
    }
}

struct Pending {
    bytes: Vec<u8>,
    ack: Option<oneshot::Sender<()>>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

struct BoundedWriter {
    bytes: Vec<u8>,
    overflow: bool,
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_MESSAGE_BYTES.saturating_sub(self.bytes.len()) {
            self.overflow = true;
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for BoundedWriter {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

#[derive(Default)]
struct DepthFormatter {
    depth: usize,
}

impl DepthFormatter {
    fn begin<W: ?Sized + Write>(&mut self, writer: &mut W, byte: u8) -> io::Result<()> {
        if self.depth > MAX_DEPTH {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.depth += 1;
        writer.write_all(&[byte])
    }

    fn end<W: ?Sized + Write>(&mut self, writer: &mut W, byte: u8) -> io::Result<()> {
        self.depth -= 1;
        writer.write_all(&[byte])
    }
}

impl Formatter for DepthFormatter {
    fn begin_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.begin(writer, b'[')
    }

    fn end_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.end(writer, b']')
    }

    fn begin_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.begin(writer, b'{')
    }

    fn end_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.end(writer, b'}')
    }
}

fn encode(item: &ClientJsonRpcMessage) -> Result<Vec<u8>, Error> {
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        overflow: false,
    };
    let serialized = item.serialize(&mut serde_json::Serializer::with_formatter(
        &mut writer,
        DepthFormatter::default(),
    ));
    if serialized.is_err() || writer.write_all(b"\n").is_err() {
        return Err(if writer.overflow {
            Error::FrameTooLarge
        } else {
            Error::InvalidMessage
        });
    }
    Ok(mem::take(&mut writer.bytes))
}

#[cfg(unix)]
type SharedStream = DirectStream;
#[cfg(windows)]
type SharedStream = Mutex<DirectStream>;

struct RegisteredStream(Arc<SharedStream>);

impl Read for RegisteredStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            self.0.try_read(bytes)
        }
        #[cfg(windows)]
        {
            let mut pipe = self
                .0
                .try_lock()
                .map_err(|_| io::Error::other("named pipe ownership unavailable"))?;
            pipe.try_read(bytes)
        }
    }
}

impl Write for RegisteredStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            self.0.try_write(bytes)
        }
        #[cfg(windows)]
        {
            let mut pipe = self
                .0
                .try_lock()
                .map_err(|_| io::Error::other("named pipe ownership unavailable"))?;
            pipe.try_write(bytes)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn cleanup_step(operation: impl FnOnce() -> Result<(), AdmissionCleanupFailed>) -> bool {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(())) => true,
        Ok(Err(_)) => false,
        Err(payload) => {
            if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop(payload))) {
                mem::forget(payload);
            }
            false
        }
    }
}

fn cleanup_admission<A: ClientWriteAdmission>(mut admission: A) -> bool {
    let cancelled = cleanup_step(|| admission.cancel());
    let dropped = cleanup_step(|| {
        drop(admission);
        Ok(())
    });
    cancelled && dropped
}

/// An SDK transport over an exclusively owned, already-connected local stream.
pub struct DirectClientTransport {
    outgoing: mpsc::Sender<Pending>,
    incoming: mpsc::Receiver<ServerJsonRpcMessage>,
    shared: Arc<Shared>,
    status: ConnectorStatus,
}

impl DirectClientTransport {
    /// Construct inside a Tokio runtime with I/O and time drivers enabled.
    ///
    /// `boundaries` creates the independent native verifier and client write
    /// fence after the stream has been connected. They must use the supplied
    /// wake handle whenever a pending decision or temporary fence changes.
    /// The stream must not have duplicated handles or other readers/writers.
    /// Windows retries synchronous nonblocking pipe I/O every 20 ms while
    /// awaiting peer activity; its idle power cost is not qualified.
    pub fn new<N, A>(
        socket: DirectStream,
        bootstrap_duration: Duration,
        frame_duration: Duration,
        boundaries: impl FnOnce(BoundaryWake) -> (N, A),
    ) -> Result<(Self, ConnectorStatus), Error>
    where
        N: TrustedNativeVerification + Send + 'static,
        A: ClientWriteAdmission,
    {
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| Error::RuntimeUnavailable)?;
        #[cfg(windows)]
        let socket = { Mutex::new(socket) };
        let socket = Arc::new(socket);
        let wake = BoundaryWake::default();
        let (native, admission) = boundaries(wake.clone());
        let channel = match SecureTransport::new(
            RegisteredStream(socket.clone()),
            Role::Client,
            native,
            bootstrap_duration,
            frame_duration,
        ) {
            Ok(channel) => channel,
            Err(error) => {
                return Err(if cleanup_admission(admission) {
                    Error::Channel(error)
                } else {
                    Error::AdmissionCleanupFailed
                });
            }
        };
        let cleanup = channel.cleanup_status();
        let (outgoing, pending) = mpsc::channel(QUEUE_CAPACITY);
        let (received, incoming) = mpsc::channel(QUEUE_CAPACITY);
        let (completion, completion_rx) = watch::channel(None);
        let shared = Arc::new(Shared {
            stop: RwLock::new(None),
            wake,
        });
        let status = ConnectorStatus {
            completion: completion_rx,
            cleanup,
            shared: shared.clone(),
        };
        let driver = Driver {
            channel: Some(channel),
            socket: Some(socket),
            admission: Some(admission),
            frame_duration,
            pending,
            received,
            completion,
            shared: shared.clone(),
            in_flight: None,
            finished: false,
        };
        runtime.spawn(async move {
            driver.run().await;
        });
        Ok((
            Self {
                outgoing,
                incoming,
                shared,
                status: status.clone(),
            },
            status,
        ))
    }

    fn discard_incoming(&mut self) {
        self.incoming.close();
        while self.incoming.try_recv().is_ok() {}
    }
}

impl Transport<RoleClient> for DirectClientTransport {
    type Error = Error;

    fn send(
        &mut self,
        item: ClientJsonRpcMessage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let outgoing = self.outgoing.clone();
        let shared = self.shared.clone();
        let mut status = self.status.clone();
        async move {
            if shared.stop_reason().is_some() {
                return Err(Error::Closed);
            }
            let encoded = encode(&item);
            drop(item);
            let bytes = match encoded {
                Ok(bytes) => bytes,
                Err(error) => {
                    shared.request(Stop::Failed(error));
                    return Err(error);
                }
            };
            let (ack, delivered) = oneshot::channel();
            let pending = Pending {
                bytes,
                ack: Some(ack),
            };
            if let Err(error) = outgoing.try_send(pending) {
                let error = match error {
                    mpsc::error::TrySendError::Full(_) => Error::Backpressure,
                    mpsc::error::TrySendError::Closed(_) => Error::Closed,
                };
                shared.request(Stop::Failed(error));
                return Err(error);
            }
            let mut cancellation = CancelOnDrop {
                shared: shared.clone(),
                armed: true,
            };
            shared.wake.wake();
            let result = delivered.await;
            cancellation.armed = false;
            match result {
                Ok(()) => Ok(()),
                Err(_) => match status.wait().await {
                    Ok(()) => Err(Error::Closed),
                    Err(error) => Err(error),
                },
            }
        }
    }

    async fn receive(&mut self) -> Option<ServerJsonRpcMessage> {
        if self.shared.stop_reason().is_some() {
            self.discard_incoming();
            return None;
        }
        let message = self.incoming.recv().await?;
        if self.shared.stop_reason().is_some() {
            self.discard_incoming();
            None
        } else {
            Some(message)
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.shared.request(Stop::Requested);
        let mut status = self.status.clone();
        async move { status.wait().await }
    }
}

impl Drop for DirectClientTransport {
    fn drop(&mut self) {
        self.shared.request(Stop::Requested);
    }
}

struct Driver<N: TrustedNativeVerification, A: ClientWriteAdmission> {
    channel: Option<SecureTransport<RegisteredStream, N>>,
    socket: Option<Arc<SharedStream>>,
    admission: Option<A>,
    frame_duration: Duration,
    pending: mpsc::Receiver<Pending>,
    received: mpsc::Sender<ServerJsonRpcMessage>,
    completion: watch::Sender<Option<Result<(), Error>>>,
    shared: Arc<Shared>,
    in_flight: Option<oneshot::Sender<()>>,
    finished: bool,
}

impl<N: TrustedNativeVerification, A: ClientWriteAdmission> Driver<N, A> {
    async fn run(mut self) {
        let result = self.drive().await;
        self.finish(result);
    }

    async fn drive(&mut self) -> Result<(), Error> {
        let mut budget = 0;
        loop {
            if let Some(stop) = self.shared.stop_reason() {
                return match stop {
                    Stop::Requested => Ok(()),
                    Stop::Failed(error) => Err(error),
                };
            }
            let channel = self.channel.as_mut().ok_or(Error::DriverStopped)?;
            if channel.is_established() && self.in_flight.is_none() {
                if let Ok(mut pending) = self.pending.try_recv() {
                    let expires = Instant::now()
                        .checked_add(self.frame_duration)
                        .ok_or(Error::Channel(secure_transport::Error::Deadline))?;
                    channel
                        .prepare_send(&pending.bytes, expires)
                        .map_err(Error::Channel)?;
                    self.in_flight = pending.ack.take();
                }
            }

            let mut admission_blocked = false;
            let admission = self.admission.as_ref().ok_or(Error::DriverStopped)?;
            let shared = &self.shared;
            let progress = channel.poll(|| {
                let external = match admission.try_admit() {
                    Ok(guard) => guard,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        admission_blocked = true;
                        return Err(error);
                    }
                    Err(error) => return Err(error),
                };
                let local = match shared.stop.try_read() {
                    Ok(guard) => guard,
                    Err(TryLockError::WouldBlock) => {
                        admission_blocked = true;
                        return Err(io::ErrorKind::WouldBlock.into());
                    }
                    Err(TryLockError::Poisoned(_)) => {
                        return Err(io::ErrorKind::PermissionDenied.into());
                    }
                };
                if local.is_some() {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Ok((local, external))
            });
            let progress = match progress {
                Ok(progress) => progress,
                Err(secure_transport::Error::Denied) => {
                    return match self.shared.stop_reason() {
                        Some(Stop::Requested) => Ok(()),
                        Some(Stop::Failed(error)) => Err(error),
                        None => Err(Error::Channel(secure_transport::Error::Denied)),
                    };
                }
                Err(error) => return Err(Error::Channel(error)),
            };
            let blocked = matches!(&progress, Progress::Blocked);
            match progress {
                Progress::Blocked => {
                    budget = 0;
                    self.wait(admission_blocked).await?;
                }
                Progress::Frame(mut bytes) => {
                    let message = serde_json::from_slice(&bytes);
                    bytes.fill(0);
                    let message = message.map_err(|_| Error::InvalidMessage)?;
                    self.received
                        .try_send(message)
                        .map_err(|error| match error {
                            mpsc::error::TrySendError::Full(_) => Error::Backpressure,
                            mpsc::error::TrySendError::Closed(_) => Error::Closed,
                        })?;
                }
                Progress::Sent => {
                    let ack = self.in_flight.take().ok_or(Error::DriverStopped)?;
                    if ack.send(()).is_err() {
                        return Err(Error::SendCancelled);
                    }
                }
                Progress::Advanced | Progress::Established => {}
            }
            if !blocked {
                budget += 1;
                if budget >= FAIRNESS_BUDGET {
                    budget = 0;
                    tokio::task::yield_now().await;
                }
            }
        }
    }

    async fn wait(&mut self, admission_blocked: bool) -> Result<(), Error> {
        let channel = self.channel.as_ref().ok_or(Error::DriverStopped)?;
        let interest = channel.io_interest();
        let deadline = channel.next_deadline().ok_or(Error::Closed)?;
        #[cfg(unix)]
        let socket = self.socket.as_ref().ok_or(Error::DriverStopped)?;
        #[cfg(unix)]
        tokio::select! {
            biased;
            _ = self.shared.wake.0.notified() => Ok(()),
            result = socket.readable(), if interest.readable => {
                result.map_err(|error| Error::Io(error.kind()))
            }
            result = socket.writable(), if interest.writable && !admission_blocked => {
                result.map_err(|error| Error::Io(error.kind()))
            }
            _ = tokio::time::sleep_until(deadline.into()) => Ok(()),
        }
        #[cfg(windows)]
        tokio::select! {
            biased;
            _ = self.shared.wake.0.notified() => Ok(()),
            _ = tokio::time::sleep(PIPE_RETRY),
                if interest.readable || (interest.writable && !admission_blocked) => Ok(()),
            _ = tokio::time::sleep_until(deadline.into()) => Ok(()),
        }
    }

    fn finish(&mut self, mut result: Result<(), Error>) {
        if self.finished {
            return;
        }
        if let Err(error) = result {
            self.shared.request(Stop::Failed(error));
        }
        if let Some(mut channel) = self.channel.take() {
            if let Err(error) = channel.close() {
                result = Err(Error::Channel(error));
            }
        }
        self.socket.take();
        if let Some(admission) = self.admission.take() {
            if !cleanup_admission(admission)
                && result != Err(Error::Channel(secure_transport::Error::CleanupFailed))
            {
                result = Err(Error::AdmissionCleanupFailed);
            }
        }
        self.completion.send_replace(Some(result));
        self.finished = true;
    }
}

impl<N: TrustedNativeVerification, A: ClientWriteAdmission> Drop for Driver<N, A> {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(Err(Error::DriverStopped));
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use festerm_automation_mcp::secure_transport::{
        NativeCleanupFailed, NativeDecision, NativeUnavailable, MAX_FRAME_DURATION,
    };
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc as std_mpsc,
        },
        thread,
    };
    use tokio::net::UnixStream;

    struct Native;

    impl TrustedNativeVerification for Native {
        fn show_local_fingerprint(&mut self, _: &str) -> Result<(), NativeUnavailable> {
            Err(NativeUnavailable)
        }

        fn poll_decision(&mut self) -> NativeDecision {
            NativeDecision::Denied
        }

        fn cancel(&mut self) -> Result<(), NativeCleanupFailed> {
            Ok(())
        }
    }

    struct Admission;

    impl ClientWriteAdmission for Admission {
        type Guard<'a> = ();

        fn try_admit(&self) -> io::Result<Self::Guard<'_>> {
            Ok(())
        }

        fn cancel(&mut self) -> Result<(), AdmissionCleanupFailed> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_stop_state_reader_does_not_stall_a_write() {
        let (socket, peer) = UnixStream::pair().unwrap();
        let (client, mut status) =
            DirectClientTransport::new(socket, Duration::from_secs(8), MAX_FRAME_DURATION, |_| {
                (Native, Admission)
            })
            .unwrap();
        let shared = client.shared.clone();
        let (held_tx, held_rx) = std_mpsc::sync_channel(0);
        let (release_tx, release_rx) = std_mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            let guard = shared.stop.read().unwrap();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
            drop(guard);
        });
        held_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let readable = tokio::time::timeout(Duration::from_secs(2), peer.readable()).await;
        release_tx.send(()).unwrap();
        reader.join().unwrap();
        readable.unwrap().unwrap();
        assert!(peer.try_read(&mut [0; 128]).unwrap() > 0);

        status.disconnect();
        assert_eq!(status.wait().await, Ok(()));
    }

    #[test]
    fn revocation_waits_for_the_write_fence() {
        let shared = Arc::new(Shared {
            stop: RwLock::new(None),
            wake: BoundaryWake::default(),
        });
        let guard = shared.stop.read().unwrap();
        let (started_tx, started_rx) = std_mpsc::sync_channel(0);
        let (finished_tx, finished_rx) = std_mpsc::sync_channel(1);
        let requester = {
            let shared = shared.clone();
            thread::spawn(move || {
                started_tx.send(()).unwrap();
                shared.request(Stop::Requested);
                finished_tx.send(()).unwrap();
            })
        };
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            finished_rx.recv_timeout(Duration::from_millis(50)),
            Err(std_mpsc::RecvTimeoutError::Timeout)
        ));
        drop(guard);
        finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        requester.join().unwrap();
        assert!(matches!(shared.stop_reason(), Some(Stop::Requested)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disconnect_discards_buffered_sdk_messages() {
        let (socket, _peer) = UnixStream::pair().unwrap();
        let (mut client, mut status) =
            DirectClientTransport::new(socket, Duration::from_secs(5), MAX_FRAME_DURATION, |_| {
                (Native, Admission)
            })
            .unwrap();
        let (sender, incoming) = mpsc::channel(1);
        client.incoming = incoming;
        let message = serde_json::from_slice(
            br#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#,
        )
        .unwrap();
        sender.try_send(message).unwrap();
        status.disconnect();
        assert!(client.receive().await.is_none());
        assert!(sender.is_closed());
        assert_eq!(status.wait().await, Ok(()));
    }

    struct FailingAdmission(Arc<AtomicBool>);

    impl ClientWriteAdmission for FailingAdmission {
        type Guard<'a> = ();

        fn try_admit(&self) -> io::Result<Self::Guard<'_>> {
            Ok(())
        }

        fn cancel(&mut self) -> Result<(), AdmissionCleanupFailed> {
            self.0.store(true, Ordering::Release);
            Err(AdmissionCleanupFailed)
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admission_cleanup_failure_is_reported_before_completion() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let (socket, _peer) = UnixStream::pair().unwrap();
        let (_, mut status) =
            DirectClientTransport::new(socket, Duration::from_secs(5), MAX_FRAME_DURATION, |_| {
                (Native, FailingAdmission(cancelled.clone()))
            })
            .unwrap();
        status.disconnect();
        assert_eq!(status.wait().await, Err(Error::AdmissionCleanupFailed));
        assert!(cancelled.load(Ordering::Acquire));
        assert!(status.cleanup_failed());
    }
}
