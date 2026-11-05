//! Beta Linux automation privacy monitor (ADR 0065).
//!
//! Uses systemd-logind on the system bus. The caller's session is resolved by
//! `GetSessionByPID`, falling back to `XDG_SESSION_ID`. Loss is signalled
//! once, and permanently, when any of these is observed:
//!
//! - the session's `Lock` signal;
//! - its `LockedHint` property being true or its `Active` property false,
//!   read after any `PropertiesChanged` signal and polled every
//!   [`POLL_INTERVAL`];
//! - `PrepareForSleep(true)`;
//! - any bus, signal-stream or property-read failure.
//!
//! `LockedHint` is set by the desktop's lock screen; a desktop that does not
//! report it is unsupported and can lock without an observed loss. Signals
//! have no documented delivery bound, and other bus clients can emit them;
//! they only cut admission and never restore it. This is an observed-event
//! cut, not an atomic physical cutoff. Installation fails closed when the
//! session cannot be resolved, is remote or not graphical (SSH, headless or
//! a text console have no lock screen), is already locked or inactive, or
//! monitoring cannot start. Every bus call is bounded by [`CALL_TIMEOUT`];
//! a stalled bus is loss. Dropping the monitor never waits for the bus.

use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::watch;
use zbus::message::Type as MessageType;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{Connection, MatchRule, MessageStream};

/// Upper bound between property reads when no signal is delivered.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);
const READY_TIMEOUT: Duration = Duration::from_secs(5);
/// Bound on one logind call; exceeding it is treated as loss.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(2);
/// Session types with a desktop lock screen.
const GRAPHICAL_TYPES: [&str; 3] = ["x11", "wayland", "mir"];

const LOGIND: &str = "org.freedesktop.login1";
const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";
const SESSION: &str = "org.freedesktop.login1.Session";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// Classifies the two session properties that bound privacy.
pub(crate) const fn private(locked_hint: bool, active: bool) -> bool {
    !locked_hint && active
}

/// Whether a session is local and graphical, so a lock screen can exist.
pub(crate) fn lockable(remote: bool, kind: &str) -> bool {
    !remote && GRAPHICAL_TYPES.contains(&kind)
}

async fn bounded<T>(call: impl Future<Output = io::Result<T>>) -> io::Result<T> {
    tokio::time::timeout(CALL_TIMEOUT, call)
        .await
        .unwrap_or_else(|_| {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "logind call timed out",
            ))
        })
}

struct Shared {
    lost: AtomicBool,
    callback: Arc<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn signal(&self) {
        if !self.lost.swap(true, Ordering::AcqRel) {
            (self.callback)();
        }
    }
}

/// Signals automation privacy loss once; dropping it stops monitoring.
pub struct PrivacyMonitor {
    shared: Arc<Shared>,
    shutdown: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}

impl PrivacyMonitor {
    /// Starts monitoring the caller's logind session. `callback` runs at most
    /// once, on loss or monitor failure, from the monitor thread or the
    /// installing thread. Returns an error (after invoking `callback`) if
    /// monitoring cannot start or privacy is already lost.
    pub fn install(callback: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        Self::install_with(callback, || async { Connection::system().await })
    }

    pub(crate) fn install_with<C, F>(
        callback: Arc<dyn Fn() + Send + Sync>,
        connect: C,
    ) -> io::Result<Self>
    where
        C: FnOnce() -> F + Send + 'static,
        F: Future<Output = zbus::Result<Connection>>,
    {
        let shared = Arc::new(Shared {
            lost: AtomicBool::new(false),
            callback,
        });
        let (shutdown, stop) = watch::channel(false);
        let (ready_send, ready) = mpsc::sync_channel(1);
        let thread_shared = shared.clone();
        let mut monitor = Self {
            shared: shared.clone(),
            shutdown,
            thread: None,
        };
        let started = thread::Builder::new()
            .name("festerm-privacy-monitor".into())
            .spawn(move || run(thread_shared, connect, stop, ready_send));
        let result = match started {
            Ok(thread) => {
                monitor.thread = Some(thread);
                match ready.recv_timeout(READY_TIMEOUT) {
                    Ok(Ok(())) if !monitor.is_lost() => Ok(()),
                    Ok(Ok(())) => Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "logind session is locked or inactive",
                    )),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "logind privacy monitor did not become ready",
                    )),
                }
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => Ok(monitor),
            Err(error) => {
                shared.signal();
                Err(error)
            }
        }
    }

    /// Whether loss has been observed.
    pub fn is_lost(&self) -> bool {
        self.shared.lost.load(Ordering::Acquire)
    }
}

impl Drop for PrivacyMonitor {
    fn drop(&mut self) {
        // Latch first so a worker still finishing a bus call cannot invoke
        // the callback after drop. The worker cancels any in-flight call on
        // shutdown and exits on its own; it is detached rather than joined
        // so a wedged bus cannot block the caller.
        self.shared.lost.store(true, Ordering::Release);
        let _ = self.shutdown.send(true);
        drop(self.thread.take());
    }
}

fn other(error: zbus::Error) -> io::Error {
    io::Error::other(error)
}

fn run<C, F>(
    shared: Arc<Shared>,
    connect: C,
    mut stop: watch::Receiver<bool>,
    ready: mpsc::SyncSender<io::Result<()>>,
) where
    C: FnOnce() -> F,
    F: Future<Output = zbus::Result<Connection>>,
{
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let mut cancelled = stop.clone();
    let shutdown = async move {
        while !*cancelled.borrow_and_update() {
            if cancelled.changed().await.is_err() {
                return;
            }
        }
    };
    let ready_on_cancel = ready.clone();
    let monitor = async move {
        let mut watched = match bounded(Watched::start(connect)).await {
            Ok(watched) => watched,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        // Signal streams are subscribed before the first read so a lock
        // between them is still observed.
        match watched.read().await {
            Ok(true) => {}
            Ok(false) => shared.signal(),
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        }
        if ready.send(Ok(())).is_err() || shared.lost.load(Ordering::Acquire) {
            return;
        }
        let mut poll = tokio::time::interval(POLL_INTERVAL);
        loop {
            let still_private = tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return;
                    }
                    continue;
                }
                // Any Lock signal, or the end of its stream, is loss.
                _ = watched.lock.next() => false,
                message = watched.sleep.next() => match message {
                    Some(Ok(message)) => message.body().deserialize::<bool>() == Ok(false),
                    _ => false,
                },
                message = watched.properties.next() => match message {
                    Some(Ok(_)) => watched.read().await.unwrap_or(false),
                    _ => false,
                },
                _ = poll.tick() => watched.read().await.unwrap_or(false),
            };
            if !still_private {
                shared.signal();
                return;
            }
        }
    };
    runtime.block_on(async move {
        tokio::select! {
            () = shutdown => {
                let _ = ready_on_cancel.try_send(Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "logind privacy monitor stopped",
                )));
            }
            () = monitor => {}
        }
    });
}

struct Watched {
    connection: Connection,
    session: OwnedObjectPath,
    lock: MessageStream,
    sleep: MessageStream,
    properties: MessageStream,
}

impl Watched {
    async fn start<C, F>(connect: C) -> io::Result<Self>
    where
        C: FnOnce() -> F,
        F: Future<Output = zbus::Result<Connection>>,
    {
        let connection = connect().await.map_err(other)?;
        let session = resolve_session(&connection).await?;
        let watched_session = WatchedSession {
            connection: &connection,
            session: &session,
        };
        if !lockable(
            watched_session.get::<bool>("Remote").await?,
            &watched_session.get::<String>("Type").await?,
        ) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "logind session is remote or has no graphical lock screen",
            ));
        }
        let signal = |path: &str, interface: &'static str, member: &'static str| {
            MatchRule::builder()
                .msg_type(MessageType::Signal)
                .interface(interface)
                .and_then(|builder| builder.member(member))
                .and_then(|builder| builder.path(path.to_owned()))
                .map(|builder| builder.build())
                .map_err(other)
        };
        let lock = signal(session.as_str(), SESSION, "Lock")?;
        let sleep = signal(MANAGER_PATH, MANAGER, "PrepareForSleep")?;
        let properties = signal(session.as_str(), PROPERTIES, "PropertiesChanged")?;
        Ok(Self {
            lock: MessageStream::for_match_rule(lock, &connection, Some(8))
                .await
                .map_err(other)?,
            sleep: MessageStream::for_match_rule(sleep, &connection, Some(8))
                .await
                .map_err(other)?,
            properties: MessageStream::for_match_rule(properties, &connection, Some(8))
                .await
                .map_err(other)?,
            connection,
            session,
        })
    }

    /// Returns whether the session is currently private.
    async fn read(&self) -> io::Result<bool> {
        let session = WatchedSession {
            connection: &self.connection,
            session: &self.session,
        };
        Ok(private(
            session.get::<bool>("LockedHint").await?,
            session.get::<bool>("Active").await?,
        ))
    }
}

struct WatchedSession<'a> {
    connection: &'a Connection,
    session: &'a OwnedObjectPath,
}

impl WatchedSession<'_> {
    async fn get<T>(&self, name: &str) -> io::Result<T>
    where
        T: TryFrom<OwnedValue>,
        T::Error: Into<zbus::Error>,
    {
        bounded(async {
            let reply = self
                .connection
                .call_method(
                    Some(LOGIND),
                    self.session.as_str(),
                    Some(PROPERTIES),
                    "Get",
                    &(SESSION, name),
                )
                .await
                .map_err(other)?;
            let value: OwnedValue = reply.body().deserialize().map_err(other)?;
            T::try_from(value).map_err(|error| other(error.into()))
        })
        .await
    }
}

async fn resolve_session(connection: &Connection) -> io::Result<OwnedObjectPath> {
    let by_pid = connection
        .call_method(
            Some(LOGIND),
            MANAGER_PATH,
            Some(MANAGER),
            "GetSessionByPID",
            &(std::process::id(),),
        )
        .await;
    let reply = match (by_pid, std::env::var("XDG_SESSION_ID")) {
        (Ok(reply), _) => reply,
        (Err(_), Ok(id)) if !id.is_empty() => connection
            .call_method(
                Some(LOGIND),
                MANAGER_PATH,
                Some(MANAGER),
                "GetSession",
                &(id,),
            )
            .await
            .map_err(other)?,
        (Err(error), _) => return Err(other(error)),
    };
    reply.body().deserialize().map_err(other)
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::AtomicUsize;

    use zbus::connection::Builder;
    use zbus::object_server::SignalEmitter;

    use super::*;

    const SESSION_PATH: &str = "/org/freedesktop/login1/session/_31";

    struct FakeManager {
        known: bool,
    }

    #[zbus::interface(name = "org.freedesktop.login1.Manager")]
    impl FakeManager {
        #[zbus(name = "GetSessionByPID")]
        fn get_session_by_pid(&self, _pid: u32) -> zbus::fdo::Result<OwnedObjectPath> {
            if self.known {
                Ok(OwnedObjectPath::try_from(SESSION_PATH).unwrap())
            } else {
                Err(zbus::fdo::Error::Failed("no session".into()))
            }
        }

        #[zbus(signal)]
        async fn prepare_for_sleep(emitter: &SignalEmitter<'_>, start: bool) -> zbus::Result<()>;
    }

    struct FakeSession {
        locked: bool,
        active: bool,
        remote: bool,
        kind: &'static str,
    }

    #[zbus::interface(name = "org.freedesktop.login1.Session")]
    impl FakeSession {
        #[zbus(property)]
        fn locked_hint(&self) -> bool {
            self.locked
        }

        #[zbus(property)]
        fn active(&self) -> bool {
            self.active
        }

        #[zbus(property)]
        fn remote(&self) -> bool {
            self.remote
        }

        #[zbus(property, name = "Type")]
        fn kind(&self) -> String {
            self.kind.to_owned()
        }

        #[zbus(signal)]
        async fn lock(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    }

    struct Fixture {
        runtime: tokio::runtime::Runtime,
        server: Option<Connection>,
        monitor: io::Result<PrivacyMonitor>,
        calls: Arc<AtomicUsize>,
    }

    impl Fixture {
        fn start(known: bool, locked: bool, active: bool) -> Self {
            Self::start_session(
                known,
                FakeSession {
                    locked,
                    active,
                    remote: false,
                    kind: "wayland",
                },
            )
        }

        fn start_session(known: bool, session: FakeSession) -> Self {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (server, client) = UnixStream::pair().unwrap();
            let guid = zbus::Guid::generate();
            let server = std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap()
                    .block_on(async move {
                        Builder::unix_stream(server)
                            .server(guid)
                            .unwrap()
                            .p2p()
                            .serve_at(MANAGER_PATH, FakeManager { known })
                            .unwrap()
                            .serve_at(SESSION_PATH, session)
                            .unwrap()
                            .build()
                            .await
                            .unwrap()
                    })
            });
            let calls = Arc::new(AtomicUsize::new(0));
            let counter = calls.clone();
            let monitor = PrivacyMonitor::install_with(
                Arc::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                }),
                move || async move { Builder::unix_stream(client).p2p().build().await },
            );
            Self {
                runtime,
                server: Some(server.join().unwrap()),
                monitor,
                calls,
            }
        }

        fn server(&self) -> &Connection {
            self.server.as_ref().unwrap()
        }

        fn wait_for_loss(&self) {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while self.calls.load(Ordering::SeqCst) == 0 {
                assert!(std::time::Instant::now() < deadline, "loss not observed");
                thread::sleep(Duration::from_millis(10));
            }
            thread::sleep(POLL_INTERVAL * 2);
            assert_eq!(self.calls.load(Ordering::SeqCst), 1);
            assert!(self.monitor.as_ref().unwrap().is_lost());
        }

        fn set_locked(&self, locked: bool) {
            self.runtime.block_on(async {
                let iface = self
                    .server()
                    .object_server()
                    .interface::<_, FakeSession>(SESSION_PATH)
                    .await
                    .unwrap();
                iface.get_mut().await.locked = locked;
                iface
                    .get()
                    .await
                    .locked_hint_changed(iface.signal_emitter())
                    .await
                    .unwrap();
            });
        }
    }

    #[test]
    fn classification_requires_unlocked_active_session() {
        assert!(private(false, true));
        assert!(!private(true, true));
        assert!(!private(false, false));
    }

    #[test]
    fn remote_or_non_graphical_sessions_fail_closed_once() {
        assert!(lockable(false, "x11"));
        for (remote, kind) in [(true, "wayland"), (false, "tty"), (false, "unspecified")] {
            let fixture = Fixture::start_session(
                true,
                FakeSession {
                    locked: false,
                    active: true,
                    remote,
                    kind,
                },
            );
            assert!(fixture.monitor.is_err());
            assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn drop_does_not_wait_for_the_bus_or_signal_loss() {
        let fixture = Fixture::start(true, false, true);
        let Fixture { monitor, calls, .. } = fixture;
        let started = std::time::Instant::now();
        drop(monitor.unwrap());
        assert!(started.elapsed() < CALL_TIMEOUT);
        thread::sleep(POLL_INTERVAL * 2);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn unlocked_active_session_installs_without_loss() {
        let fixture = Fixture::start(true, false, true);
        assert!(fixture.monitor.is_ok());
        thread::sleep(POLL_INTERVAL * 3);
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn locked_or_inactive_or_unknown_session_fails_closed_once() {
        for (known, locked, active) in [
            (true, true, true),
            (true, false, false),
            (false, false, true),
        ] {
            let fixture = Fixture::start(known, locked, active);
            assert!(fixture.monitor.is_err());
            assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn lock_signal_signals_loss_once() {
        let fixture = Fixture::start(true, false, true);
        fixture.runtime.block_on(async {
            let emitter = SignalEmitter::new(fixture.server(), SESSION_PATH).unwrap();
            FakeSession::lock(&emitter).await.unwrap();
            FakeSession::lock(&emitter).await.unwrap();
        });
        fixture.wait_for_loss();
    }

    #[test]
    fn locked_hint_change_signals_loss() {
        let fixture = Fixture::start(true, false, true);
        fixture.set_locked(true);
        fixture.wait_for_loss();
    }

    #[test]
    fn only_suspend_not_resume_signals_loss() {
        let fixture = Fixture::start(true, false, true);
        let emitter = SignalEmitter::new(fixture.server(), MANAGER_PATH).unwrap();
        fixture
            .runtime
            .block_on(FakeManager::prepare_for_sleep(&emitter, false))
            .unwrap();
        thread::sleep(POLL_INTERVAL * 2);
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
        fixture
            .runtime
            .block_on(FakeManager::prepare_for_sleep(&emitter, true))
            .unwrap();
        fixture.wait_for_loss();
    }

    #[test]
    fn bus_disconnect_signals_loss() {
        let mut fixture = Fixture::start(true, false, true);
        let server = fixture.server.take().unwrap();
        fixture.runtime.block_on(server.close()).unwrap();
        fixture.wait_for_loss();
    }
}
