//! Portable (iOS-capable) bridge client: it speaks the bridge wire protocol
//! directly and exposes the attachment as a [`festerm_session::Session`]. Unlike
//! the desktop adapter it has no daemon, PTY or file-service dependency.
//!
//! The connection is never re-established by this module. A displaced,
//! refused, exited or lost attachment ends the session; the application decides
//! whether to attach again (ADR 0057).
use std::{
    fmt, io,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use festerm_core::{ColorScheme, Terminal};
use festerm_session::{
    Session, SessionError, SessionErrorKind, SessionEvent, SessionEventNotifier, SessionExit,
    SessionId, SessionLifecycle, SessionMetrics, SessionOperation, SessionSendError,
    SessionTryReceiveError, ShutdownError, ShutdownResult, TerminalSize,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

use crate::{
    protocol::*,
    wire::{decode_size, encode_size, read_frame, write_frame, DEADLINE},
};

pub use crate::protocol::{Description, Displacer, Target};

/// Any ordered byte stream to a bridge: the Dev Tunnels relay channel in
/// production, an in-memory pipe in tests.
pub trait BridgeStream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> BridgeStream for T {}
pub type BoxStream = Box<dyn BridgeStream>;

const EVENT_QUEUE: usize = 64;
const COMMAND_QUEUE: usize = 64;
const INPUT_LIMIT: usize = CHUNK_LIMIT;

/// Why an attachment is over.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndReason {
    /// Another client took the session; never reattached automatically.
    Displaced(Displacer),
    /// The remote shell exited.
    Exited,
    /// The user detached; the remote shell keeps running.
    Detached,
    /// The relay or host connection was lost.
    ConnectionLost,
}

impl fmt::Display for EndReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Displaced(Displacer::Local) => {
                "Taken over by a local client on the host. Reconnect to take it back."
            }
            Self::Displaced(Displacer::Remote) => {
                "Taken over by another remote client. Reconnect to take it back."
            }
            Self::Displaced(Displacer::Unknown) => {
                "Taken over by another client. Reconnect to take it back."
            }
            Self::Exited => "The remote shell exited.",
            Self::Detached => "Detached. The remote shell is still running.",
            Self::ConnectionLost => "Connection lost.",
        })
    }
}

/// Failure to complete the attach handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachError {
    /// The host declined: the session is held and takeover is not allowed or
    /// not requested, or the pinned target no longer exists.
    Refused,
    /// The host spoke an unexpected protocol.
    Protocol,
    /// The stream failed or timed out.
    Connection,
    Cancelled,
}

impl fmt::Display for AttachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Refused => "The host refused the attachment.",
            Self::Protocol => "Unexpected host response.",
            Self::Connection => "Connection failed.",
            Self::Cancelled => "Cancelled.",
        })
    }
}
impl std::error::Error for AttachError {}

/// A completed handshake: the host's recovered terminal state, not yet adopted.
pub struct Handshake {
    stream: BoxStream,
    terminal: Terminal,
}

/// Asks the host to describe its session without attaching to it.
pub async fn describe(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
) -> io::Result<Description> {
    crate::wire::discover_stream(stream).await
}

/// Sends `Hello` and receives the recovery snapshot. The host holds the session
/// until [`TunnelSession::spawn`] acknowledges adoption.
pub async fn establish(
    mut stream: BoxStream,
    target: Target,
    takeover: bool,
    cancel: &CancellationToken,
) -> Result<Handshake, AttachError> {
    let work = async {
        let hello = serde_json::to_vec(&Hello {
            files: FileAccess::default(),
            takeover,
            version: VERSION,
            snapshot_schema: 3,
            target,
        })
        .map_err(|_| AttachError::Protocol)?;
        write_frame(&mut stream, HELLO, &hello)
            .await
            .map_err(|_| AttachError::Connection)?;
        let (kind, capabilities) = read(&mut stream).await?;
        if kind == REFUSED {
            return Err(AttachError::Refused);
        }
        if kind != CAPABILITIES || capabilities.len() != 2 || capabilities[0] != 0 {
            return Err(AttachError::Protocol);
        }
        let (kind, bytes) = read(&mut stream).await?;
        if kind == REFUSED {
            return Err(AttachError::Refused);
        }
        if kind != SNAPSHOT {
            return Err(AttachError::Protocol);
        }
        let terminal = decode_snapshot(&bytes).map_err(|_| AttachError::Protocol)?;
        Ok(Handshake { stream, terminal })
    };
    tokio::select! {
        _ = cancel.cancelled() => Err(AttachError::Cancelled),
        result = work => result,
    }
}

async fn read(stream: &mut BoxStream) -> Result<(u8, Vec<u8>), AttachError> {
    tokio::time::timeout(DEADLINE, read_frame(stream))
        .await
        .map_err(|_| AttachError::Connection)?
        .map_err(|error| {
            if error.kind() == io::ErrorKind::InvalidData {
                AttachError::Protocol
            } else {
                AttachError::Connection
            }
        })
}

enum Command {
    Adopt,
    Input(Vec<u8>),
    Resize(TerminalSize),
    Sync(Sync),
    Detach,
}

struct Shared {
    id: SessionId,
    lifecycle: Mutex<SessionLifecycle>,
    end: Mutex<Option<EndReason>>,
    recovered: Mutex<Option<Terminal>>,
    adopted: std::sync::atomic::AtomicBool,
    notifier: Arc<dyn SessionEventNotifier>,
    input_bytes: AtomicU64,
    output_bytes: AtomicU64,
    resizes: AtomicU64,
    errors: AtomicU64,
    high_water: AtomicUsize,
    cancel: CancellationToken,
}

impl Shared {
    fn set_lifecycle(&self, lifecycle: SessionLifecycle) {
        if let Ok(mut slot) = self.lifecycle.lock() {
            if !slot.is_terminal() {
                *slot = lifecycle;
            }
        }
    }
    fn finish(&self, reason: EndReason) -> SessionLifecycle {
        let mut end = self.end.lock().unwrap_or_else(|e| e.into_inner());
        let reason = *end.get_or_insert(reason);
        let lifecycle = match reason {
            EndReason::Exited => SessionLifecycle::Exited(SessionExit::with_exit_code(0)),
            EndReason::Detached => SessionLifecycle::Stopped,
            EndReason::Displaced(_) | EndReason::ConnectionLost => SessionLifecycle::Disconnected(
                SessionError::new(SessionErrorKind::Output, reason.to_string()),
            ),
        };
        drop(end);
        self.set_lifecycle(lifecycle.clone());
        lifecycle
    }
}

/// One live attachment to a bridge session. Cheap to poll from a UI thread:
/// every method is non-blocking except [`Session::shutdown`].
pub struct TunnelSession {
    shared: Arc<Shared>,
    commands: flume::Sender<Command>,
    events: flume::Receiver<SessionEvent>,
}

impl TunnelSession {
    /// Starts the I/O worker on `runtime`. The session is `Starting` until the
    /// application calls [`Self::take_recovered_terminal`], which adopts the
    /// snapshot, acknowledges it to the host and makes the session `Running`.
    pub fn spawn(
        runtime: &tokio::runtime::Handle,
        handshake: Handshake,
        notifier: Arc<dyn SessionEventNotifier>,
    ) -> Self {
        let (command_tx, command_rx) = flume::bounded(COMMAND_QUEUE);
        let (event_tx, event_rx) = flume::bounded(EVENT_QUEUE);
        let shared = Arc::new(Shared {
            id: SessionId::next(),
            lifecycle: Mutex::new(SessionLifecycle::Starting),
            end: Mutex::new(None),
            recovered: Mutex::new(Some(handshake.terminal)),
            adopted: false.into(),
            notifier,
            input_bytes: 0.into(),
            output_bytes: 0.into(),
            resizes: 0.into(),
            errors: 0.into(),
            high_water: 0.into(),
            cancel: CancellationToken::new(),
        });
        let worker = shared.clone();
        runtime.spawn(async move {
            let cancel = worker.cancel.clone();
            let reason = tokio::select! {
                _ = cancel.cancelled() => EndReason::Detached,
                reason = run(handshake.stream, &worker, command_rx, event_tx.clone()) => reason,
            };
            let lifecycle = worker.finish(reason);
            let _ = event_tx.try_send(SessionEvent::Lifecycle(lifecycle));
            worker.notifier.notify();
        });
        Self {
            shared,
            commands: command_tx,
            events: event_rx,
        }
    }

    /// The host's terminal state at attach time. Taking it adopts it: the host
    /// then releases live output. Call once, before sending input; returns
    /// `None` afterwards.
    pub fn take_recovered_terminal(&self) -> Option<Terminal> {
        let terminal = self.shared.recovered.lock().ok()?.take()?;
        if self.commands.try_send(Command::Adopt).is_err() {
            self.shared.errors.fetch_add(1, Ordering::Relaxed);
        }
        Some(terminal)
    }

    /// Why the session ended, once it has.
    pub fn end_reason(&self) -> Option<EndReason> {
        *self.shared.end.lock().ok()?
    }

    /// Mirrors a frontend scrollback change into the host's recovery mirror.
    pub fn sync_scrollback_limit(&self, limit_bytes: usize) -> Result<(), SessionSendError> {
        self.sync(Sync::Scrollback(limit_bytes))
    }

    /// Mirrors a frontend colour scheme change into the host's recovery mirror.
    pub fn sync_color_scheme(&self, colors: ColorScheme) -> Result<(), SessionSendError> {
        self.sync(Sync::Colors(colors))
    }

    fn sync(&self, sync: Sync) -> Result<(), SessionSendError> {
        self.send(Command::Sync(sync), SessionOperation::Input)
    }

    fn send(&self, command: Command, operation: SessionOperation) -> Result<(), SessionSendError> {
        if !self.shared.adopted.load(Ordering::Acquire) {
            return Err(SessionSendError::Full {
                operation,
                capacity: 0,
            });
        }
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                flume::TrySendError::Full(_) => SessionSendError::Full {
                    operation,
                    capacity: COMMAND_QUEUE,
                },
                flume::TrySendError::Disconnected(_) => SessionSendError::Closed { operation },
            })
    }
}

impl Drop for TunnelSession {
    fn drop(&mut self) {
        self.shared.cancel.cancel();
    }
}

impl Session for TunnelSession {
    fn id(&self) -> SessionId {
        self.shared.id
    }

    fn lifecycle(&self) -> SessionLifecycle {
        self.shared
            .lifecycle
            .lock()
            .map(|slot| slot.clone())
            .unwrap_or(SessionLifecycle::Stopped)
    }

    fn metrics(&self) -> SessionMetrics {
        SessionMetrics {
            input_bytes: self.shared.input_bytes.load(Ordering::Relaxed),
            output_bytes: self.shared.output_bytes.load(Ordering::Relaxed),
            resize_count: self.shared.resizes.load(Ordering::Relaxed),
            error_count: self.shared.errors.load(Ordering::Relaxed),
            backpressure_count: 0,
            event_queue_depth: self.events.len(),
            event_queue_high_watermark: self
                .shared
                .high_water
                .load(Ordering::Relaxed)
                .max(self.events.len()),
            event_queue_capacity: EVENT_QUEUE,
        }
    }

    fn try_send_input(&self, bytes: &[u8]) -> Result<(), SessionSendError> {
        if bytes.len() > INPUT_LIMIT {
            return Err(SessionSendError::TooLarge {
                operation: SessionOperation::Input,
                maximum: INPUT_LIMIT,
                actual: bytes.len(),
            });
        }
        if bytes.is_empty() {
            return Ok(());
        }
        self.send(Command::Input(bytes.to_vec()), SessionOperation::Input)?;
        self.shared
            .input_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    fn try_resize(&self, size: TerminalSize) -> Result<(), SessionSendError> {
        if size.columns() > 256 || size.rows() > 128 {
            return Err(SessionSendError::TooLarge {
                operation: SessionOperation::Resize,
                maximum: 256,
                actual: usize::from(size.columns().max(size.rows())),
            });
        }
        self.send(Command::Resize(size), SessionOperation::Resize)?;
        self.shared.resizes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn try_shutdown(&self) -> Result<(), SessionSendError> {
        // Detach never terminates the remote shell. Before adoption the
        // handshake is simply abandoned.
        if self.commands.try_send(Command::Detach).is_err() {
            self.shared.cancel.cancel();
        }
        Ok(())
    }

    fn try_recv_event(&self) -> Result<SessionEvent, SessionTryReceiveError> {
        match self.events.try_recv() {
            Ok(event) => Ok(event),
            Err(flume::TryRecvError::Empty) => Err(SessionTryReceiveError::Empty),
            Err(flume::TryRecvError::Disconnected) => Err(SessionTryReceiveError::Closed),
        }
    }

    /// The host's daemon mirror answers terminal queries for a detached shell,
    /// so the frontend must not also reply (as for the desktop bridge).
    fn terminal_replies_owned_by_backend(&self) -> bool {
        true
    }

    fn shutdown(&self, timeout: Duration) -> Result<ShutdownResult, ShutdownError> {
        if self.lifecycle().is_terminal() {
            return Ok(ShutdownResult::AlreadyStopped);
        }
        self.try_shutdown().map_err(ShutdownError::Request)?;
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.lifecycle().is_terminal() {
                return Ok(ShutdownResult::Stopped);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.shared.cancel.cancel();
        Err(ShutdownError::TimedOut { timeout })
    }
}

async fn run(
    mut stream: BoxStream,
    shared: &Arc<Shared>,
    commands: flume::Receiver<Command>,
    events: flume::Sender<SessionEvent>,
) -> EndReason {
    // The host waits for adoption (bounded by its own deadline). Nothing else
    // is sent before it.
    loop {
        match commands.recv_async().await {
            Ok(Command::Adopt) => break,
            Ok(Command::Detach) | Err(_) => return EndReason::Detached,
            Ok(_) => {}
        }
    }
    if write_frame(&mut stream, ADOPTED, &[]).await.is_err() {
        return EndReason::ConnectionLost;
    }
    shared.adopted.store(true, Ordering::Release);
    shared.set_lifecycle(SessionLifecycle::Running);
    if events
        .send_async(SessionEvent::Lifecycle(SessionLifecycle::Running))
        .await
        .is_err()
    {
        return EndReason::Detached;
    }
    shared.notifier.notify();
    let (mut reader, mut writer) = tokio::io::split(stream);
    let reading = async {
        loop {
            let (kind, bytes) = match read_frame(&mut reader).await {
                Ok(frame) => frame,
                Err(_) => return EndReason::ConnectionLost,
            };
            let event = match kind {
                OUTPUT => {
                    shared
                        .output_bytes
                        .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    SessionEvent::Output(bytes)
                }
                RESIZED => match decode_size(&bytes) {
                    Ok(size) => SessionEvent::ResizeApplied(size),
                    Err(_) => return EndReason::ConnectionLost,
                },
                SYNC => {
                    // Validate here so a malformed command ends the session
                    // instead of reaching the terminal.
                    if serde_json::from_slice::<Sync>(&bytes).is_err() {
                        return EndReason::ConnectionLost;
                    }
                    SessionEvent::RecoverySync(bytes)
                }
                EXITED if bytes.is_empty() => return EndReason::Exited,
                DISPLACED => {
                    return EndReason::Displaced(
                        bytes
                            .first()
                            .and_then(|code| Displacer::from_code(*code))
                            .unwrap_or(Displacer::Unknown),
                    )
                }
                _ => return EndReason::ConnectionLost,
            };
            if events.send_async(event).await.is_err() {
                return EndReason::Detached;
            }
            shared.high_water.fetch_max(events.len(), Ordering::Relaxed);
            shared.notifier.notify();
        }
    };
    let writing = async {
        loop {
            let Ok(command) = commands.recv_async().await else {
                return EndReason::Detached;
            };
            let sent = match command {
                Command::Adopt => continue,
                Command::Input(bytes) => write_frame(&mut writer, INPUT, &bytes).await,
                Command::Resize(size) => write_frame(&mut writer, RESIZE, &encode_size(size)).await,
                Command::Sync(sync) => match serde_json::to_vec(&sync) {
                    Ok(bytes) => write_frame(&mut writer, SYNC, &bytes).await,
                    Err(_) => return EndReason::ConnectionLost,
                },
                Command::Detach => {
                    let _ = write_frame(&mut writer, DETACH, &[]).await;
                    return EndReason::Detached;
                }
            };
            if sent.is_err() {
                return EndReason::ConnectionLost;
            }
        }
    };
    tokio::select! {
        reason = reading => reason,
        reason = writing => reason,
    }
}

/// Applies a host `RecoverySync` event payload to the frontend terminal, in
/// session order. Mirrors the desktop `apply_recovery_sync`.
pub fn apply_recovery_sync(terminal: &mut Terminal, payload: &[u8]) -> io::Result<()> {
    match serde_json::from_slice::<Sync>(payload).map_err(|_| invalid())? {
        Sync::Scrollback(limit) if limit <= SNAPSHOT_LIMIT => terminal.set_scrollback_limit(limit),
        Sync::Colors(colors) => terminal.set_color_scheme(colors),
        Sync::Bytes(bytes) if bytes.len() <= CHUNK_LIMIT => terminal.ingest(&bytes),
        Sync::Reset => terminal.reset_to_initial_state(),
        _ => return Err(invalid()),
    }
    let _ = terminal.drain_replies();
    let _ = terminal.take_reply_queue_overflowed();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_core::Dimensions;
    use festerm_session::noop_session_event_notifier;

    fn target() -> Target {
        Target {
            name: "owned".into(),
            pid: 7,
            generation: "9".into(),
        }
    }

    fn snapshot() -> Vec<u8> {
        let mut terminal = Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap();
        terminal.ingest(b"retained\r\n");
        encode_snapshot(&terminal.recovery_clone()).unwrap()
    }

    async fn attach(
        takeover: bool,
    ) -> (
        TunnelSession,
        tokio::io::DuplexStream,
        Terminal,
        tokio::runtime::Handle,
    ) {
        let (client, mut host) = tokio::io::duplex(1 << 16);
        let task = tokio::spawn(async move {
            let (kind, bytes) = read_frame(&mut host).await.unwrap();
            assert_eq!(kind, HELLO);
            let hello: Hello = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(hello.takeover, takeover);
            write_frame(&mut host, CAPABILITIES, &[0, 0]).await.unwrap();
            write_frame(&mut host, SNAPSHOT, &snapshot()).await.unwrap();
            host
        });
        let handshake = establish(
            Box::new(client),
            target(),
            takeover,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let host = task.await.unwrap();
        let handle = tokio::runtime::Handle::current();
        let session = TunnelSession::spawn(&handle, handshake, noop_session_event_notifier());
        assert!(session.terminal_replies_owned_by_backend());
        assert!(
            session.try_send_input(b"x").is_err(),
            "no input before adoption"
        );
        let terminal = session.take_recovered_terminal().unwrap();
        assert!(session.take_recovered_terminal().is_none());
        (session, host, terminal, handle)
    }

    async fn next_event(session: &TunnelSession) -> SessionEvent {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(event) = session.try_recv_event() {
                    return event;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn adopts_before_input_and_streams_output_input_and_resize() {
        let (session, mut host, terminal, _) = attach(true).await;
        assert!(terminal.row_text(0).unwrap().starts_with("retained"));
        assert_eq!(read_frame(&mut host).await.unwrap(), (ADOPTED, vec![]));
        assert!(matches!(
            next_event(&session).await,
            SessionEvent::Lifecycle(SessionLifecycle::Running)
        ));
        write_frame(&mut host, OUTPUT, b"live").await.unwrap();
        assert_eq!(
            next_event(&session).await,
            SessionEvent::Output(b"live".to_vec())
        );
        session.try_send_input(b"ls\r").unwrap();
        assert_eq!(
            read_frame(&mut host).await.unwrap(),
            (INPUT, b"ls\r".to_vec())
        );
        session
            .try_resize(TerminalSize::new(100, 30).unwrap())
            .unwrap();
        assert_eq!(read_frame(&mut host).await.unwrap().0, RESIZE);
        assert!(session.try_send_input(&vec![0; INPUT_LIMIT + 1]).is_err());
    }

    #[tokio::test]
    async fn displacement_ends_the_session_without_reattaching() {
        for (code, expected) in [
            (0, Displacer::Local),
            (1, Displacer::Remote),
            (9, Displacer::Unknown),
        ] {
            let (session, mut host, _, _) = attach(true).await;
            assert_eq!(read_frame(&mut host).await.unwrap().0, ADOPTED);
            write_frame(&mut host, DISPLACED, &[code]).await.unwrap();
            loop {
                if let SessionEvent::Lifecycle(lifecycle) = next_event(&session).await {
                    if lifecycle.is_terminal()
                        || matches!(lifecycle, SessionLifecycle::Disconnected(_))
                    {
                        assert!(matches!(lifecycle, SessionLifecycle::Disconnected(_)));
                        break;
                    }
                }
            }
            assert_eq!(session.end_reason(), Some(EndReason::Displaced(expected)));
            // Never a second connection: the host side was dropped, not re-dialled.
            assert!(read_frame(&mut host).await.is_err());
        }
    }

    #[tokio::test]
    async fn exit_detach_and_loss_are_distinct_and_detach_is_graceful() {
        let (session, mut host, _, _) = attach(true).await;
        read_frame(&mut host).await.unwrap();
        write_frame(&mut host, EXITED, &[]).await.unwrap();
        loop {
            if let SessionEvent::Lifecycle(l) = next_event(&session).await {
                if l.is_terminal() {
                    break;
                }
            }
        }
        assert_eq!(session.end_reason(), Some(EndReason::Exited));

        let (session, mut host, _, handle) = attach(false).await;
        read_frame(&mut host).await.unwrap();
        let blocking = std::thread::spawn(move || {
            let _keep = handle;
            session
                .shutdown(Duration::from_secs(3))
                .map(|r| (r, session.end_reason()))
        });
        assert_eq!(read_frame(&mut host).await.unwrap(), (DETACH, vec![]));
        assert_eq!(
            blocking.join().unwrap().unwrap(),
            (ShutdownResult::Stopped, Some(EndReason::Detached))
        );

        let (session, host, _, _) = attach(true).await;
        drop(host);
        loop {
            if let SessionEvent::Lifecycle(l) = next_event(&session).await {
                if matches!(l, SessionLifecycle::Disconnected(_)) {
                    break;
                }
            }
        }
        assert_eq!(session.end_reason(), Some(EndReason::ConnectionLost));
    }

    #[tokio::test]
    async fn refusal_and_malformed_hosts_fail_the_handshake_with_typed_errors() {
        let (client, mut host) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            read_frame(&mut host).await.unwrap();
            write_frame(&mut host, REFUSED, &[]).await.unwrap();
        });
        let result = establish(Box::new(client), target(), false, &CancellationToken::new()).await;
        assert_eq!(result.err(), Some(AttachError::Refused));

        let (client, mut host) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            read_frame(&mut host).await.unwrap();
            write_frame(&mut host, CAPABILITIES, &[1, 0]).await.unwrap();
        });
        let result = establish(Box::new(client), target(), true, &CancellationToken::new()).await;
        assert_eq!(result.err(), Some(AttachError::Protocol));

        let (client, _host) = tokio::io::duplex(4096);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = establish(Box::new(client), target(), true, &cancel).await;
        assert_eq!(result.err(), Some(AttachError::Cancelled));
    }

    #[test]
    fn recovery_sync_applies_in_order_and_refuses_malformed_payloads() {
        let mut terminal = Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap();
        let bytes = serde_json::to_vec(&Sync::Bytes(b"abc".to_vec())).unwrap();
        apply_recovery_sync(&mut terminal, &bytes).unwrap();
        assert!(terminal.row_text(0).unwrap().starts_with("abc"));
        apply_recovery_sync(&mut terminal, &serde_json::to_vec(&Sync::Reset).unwrap()).unwrap();
        assert!(apply_recovery_sync(&mut terminal, b"{}").is_err());
        let huge = serde_json::to_vec(&Sync::Scrollback(usize::MAX)).unwrap();
        assert!(apply_recovery_sync(&mut terminal, &huge).is_err());
    }
}
