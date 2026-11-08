use std::{
    future::Future,
    io,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Component, Path, PathBuf},
    sync::{mpsc, Arc},
    thread::{self, JoinHandle},
};

use futures_util::StreamExt;
use nix::{
    sys::socket::{
        connect, getsockopt, socket, sockopt::PeerCredentials, AddressFamily, SockFlag, SockType,
        UnixAddr,
    },
    unistd::geteuid,
};
use tokio::sync::watch;
use zbus::{
    message::Type as MessageType,
    zvariant::{OwnedObjectPath, OwnedValue},
    Connection, MatchRule, MessageStream,
};

use super::{
    bounded, lockable, other, Shared, WatchedSession, LOGIND, MANAGER, MANAGER_PATH, POLL_INTERVAL,
    PROPERTIES, READY_TIMEOUT,
};

const SHELL: &str = "org.gnome.Shell";
const SCREENSAVER_PATH: &str = "/org/gnome/ScreenSaver";
const SCREENSAVER: &str = "org.gnome.ScreenSaver";
const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const USER: &str = "org.freedesktop.login1.User";
const NO_SESSION_FOR_PID: &str = "org.freedesktop.login1.NoSessionForPID";
const MAX_SESSIONS: usize = 256;

pub(super) fn supported_desktop(desktop: Option<&str>) -> bool {
    desktop.is_some_and(|value| {
        value
            .split(':')
            .any(|part| part.eq_ignore_ascii_case("GNOME"))
    })
}

fn verify_shell_process(pid: u32, uid: u32, runtime: &str) -> io::Result<()> {
    let executable_path = format!("/proc/{pid}/exe");
    let executable = std::fs::read_link(&executable_path)?;
    let metadata = std::fs::metadata(executable_path)?;
    if executable.file_name() != Some(std::ffi::OsStr::new("gnome-shell"))
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "The GNOME Shell lock service is not owned by a trusted gnome-shell",
        ));
    }
    let path = wayland_socket_path(
        runtime,
        std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        std::env::var_os("WAYLAND_SOCKET").is_some(),
    )?;
    verify_wayland_peer(&path, pid, uid)
}

fn wayland_socket_path(
    runtime: &str,
    runtime_dir: Option<&std::ffi::OsStr>,
    display: Option<&std::ffi::OsStr>,
    inherited_socket: bool,
) -> io::Result<PathBuf> {
    if inherited_socket {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "An inherited Wayland socket cannot be verified for recording",
        ));
    }
    let runtime_dir = runtime_dir
        .ok_or_else(|| io::Error::other("the Wayland runtime directory is unavailable"))?;
    if Path::new(runtime_dir) != Path::new(runtime) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The Wayland runtime directory does not belong to this logind user",
        ));
    }
    let display = display.ok_or_else(|| io::Error::other("the Wayland display is unavailable"))?;
    let mut components = Path::new(display).components();
    let Some(Component::Normal(name)) = components.next() else {
        return Err(io::Error::other("the Wayland display name is invalid"));
    };
    if components.next().is_some() {
        return Err(io::Error::other("the Wayland display name is invalid"));
    }
    Ok(Path::new(runtime).join(name))
}

fn verify_wayland_peer(path: &Path, pid: u32, uid: u32) -> io::Result<()> {
    let fd = socket(
        AddressFamily::Unix,
        SockType::Stream,
        SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK,
        None,
    )
    .map_err(io::Error::from)?;
    let address = UnixAddr::new(path).map_err(io::Error::from)?;
    connect(fd.as_raw_fd(), &address).map_err(io::Error::from)?;
    let peer = getsockopt(&fd, PeerCredentials).map_err(io::Error::from)?;
    if u32::try_from(peer.pid()).ok() != Some(pid) || peer.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The Wayland compositor is not the monitored GNOME Shell",
        ));
    }
    Ok(())
}

async fn session_by_pid(connection: &Connection, pid: u32) -> io::Result<Option<OwnedObjectPath>> {
    match connection
        .call_method(
            Some(LOGIND),
            MANAGER_PATH,
            Some(MANAGER),
            "GetSessionByPID",
            &(pid,),
        )
        .await
    {
        Ok(reply) => reply.body().deserialize().map(Some).map_err(other),
        Err(zbus::Error::MethodError(name, _, _)) if name.as_str() == NO_SESSION_FOR_PID => {
            Ok(None)
        }
        Err(error) => Err(other(error)),
    }
}

async fn graphical_session(
    connection: &Connection,
    shell_pid: u32,
    uid: u32,
) -> io::Result<(OwnedObjectPath, String)> {
    let reply = connection
        .call_method(
            Some(LOGIND),
            MANAGER_PATH,
            Some(MANAGER),
            "GetUserByPID",
            &(shell_pid,),
        )
        .await
        .map_err(other)?;
    let shell_user: OwnedObjectPath = reply.body().deserialize().map_err(other)?;
    let reply = connection
        .call_method(
            Some(LOGIND),
            MANAGER_PATH,
            Some(MANAGER),
            "ListSessions",
            &(),
        )
        .await
        .map_err(other)?;
    let sessions: Vec<(String, u32, String, String, OwnedObjectPath)> =
        reply.body().deserialize().map_err(other)?;
    if sessions.len() > MAX_SESSIONS {
        return Err(io::Error::other("too many logind sessions to verify"));
    }
    let mut selected = None;
    for (_, session_uid, _, seat, path) in sessions {
        if session_uid != uid || seat.is_empty() {
            continue;
        }
        let session = WatchedSession {
            connection,
            session: &path,
        };
        let active = session.get::<bool>("Active").await?;
        let remote = session.get::<bool>("Remote").await?;
        let kind = session.get::<String>("Type").await?;
        let class = session.get::<String>("Class").await?;
        if !active || !lockable(remote, &kind, &seat) || class != "user" {
            continue;
        }
        let (owner_uid, owner): (u32, OwnedObjectPath) = session.get("User").await?;
        if owner_uid != uid || owner != shell_user {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The graphical session and GNOME Shell have different owners",
            ));
        }
        if selected.replace((path, kind)).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "More than one active graphical session belongs to this user",
            ));
        }
    }
    let (session, kind) = selected.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "No unique active local graphical session is available",
        )
    })?;
    if kind != "wayland" {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux recording requires a GNOME Wayland session",
        ));
    }
    for pid in [std::process::id(), shell_pid] {
        if session_by_pid(connection, pid)
            .await?
            .is_some_and(|process_session| process_session != session)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "GNOME Shell or the application belongs to a different logind session",
            ));
        }
    }
    let reply = connection
        .call_method(
            Some(LOGIND),
            shell_user.as_str(),
            Some(PROPERTIES),
            "Get",
            &(USER, "RuntimePath"),
        )
        .await
        .map_err(other)?;
    let runtime: OwnedValue = reply.body().deserialize().map_err(other)?;
    let runtime = String::try_from(runtime).map_err(|error| other(error.into()))?;
    Ok((session, runtime))
}

pub(super) struct GnomePrivacyMonitor {
    shared: Arc<Shared>,
    shutdown: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
    session: Option<OwnedObjectPath>,
}

impl GnomePrivacyMonitor {
    pub(super) fn install(callback: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        Self::install_with(
            callback,
            || async { Connection::session().await },
            || async { Connection::system().await },
            verify_shell_process,
        )
    }

    fn install_with<U, UF, S, SF, V>(
        callback: Arc<dyn Fn() + Send + Sync>,
        user_bus: U,
        system_bus: S,
        verify_process: V,
    ) -> io::Result<Self>
    where
        U: FnOnce() -> UF + Send + 'static,
        UF: Future<Output = zbus::Result<Connection>>,
        S: FnOnce() -> SF + Send + 'static,
        SF: Future<Output = zbus::Result<Connection>>,
        V: FnOnce(u32, u32, &str) -> io::Result<()> + Send + 'static,
    {
        let shared = Arc::new(Shared {
            lost: std::sync::atomic::AtomicBool::new(false),
            callback: std::sync::Mutex::new(Some(callback)),
        });
        let (shutdown, stop) = watch::channel(false);
        let (ready_send, ready) = mpsc::sync_channel(1);
        let thread_shared = shared.clone();
        let mut monitor = Self {
            shared: shared.clone(),
            shutdown,
            thread: None,
            session: None,
        };
        let started = thread::Builder::new()
            .name("festerm-gnome-lock-monitor".to_owned())
            .spawn(move || {
                run(
                    thread_shared,
                    user_bus,
                    system_bus,
                    verify_process,
                    stop,
                    ready_send,
                )
            });
        let result = match started {
            Ok(thread) => {
                monitor.thread = Some(thread);
                match ready.recv_timeout(READY_TIMEOUT) {
                    Ok(Ok(session)) if !shared.lost.load(std::sync::atomic::Ordering::Acquire) => {
                        Ok(session)
                    }
                    Ok(Ok(_)) => Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "GNOME screen lock is active",
                    )),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "GNOME screen-lock monitor did not become ready",
                    )),
                }
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(session) => {
                monitor.session = Some(session);
                Ok(monitor)
            }
            Err(error) => {
                shared.signal();
                Err(error)
            }
        }
    }

    pub(super) fn session(&self) -> &OwnedObjectPath {
        self.session
            .as_ref()
            .expect("installed monitor has a session")
    }
}

impl Drop for GnomePrivacyMonitor {
    fn drop(&mut self) {
        self.shared
            .lost
            .store(true, std::sync::atomic::Ordering::Release);
        self.shared
            .callback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let _ = self.shutdown.send(true);
        drop(self.thread.take());
    }
}

fn run<U, UF, S, SF, V>(
    shared: Arc<Shared>,
    user_bus: U,
    system_bus: S,
    verify_process: V,
    mut stop: watch::Receiver<bool>,
    ready: mpsc::SyncSender<io::Result<OwnedObjectPath>>,
) where
    U: FnOnce() -> UF,
    UF: Future<Output = zbus::Result<Connection>>,
    S: FnOnce() -> SF,
    SF: Future<Output = zbus::Result<Connection>>,
    V: FnOnce(u32, u32, &str) -> io::Result<()>,
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
        let mut watched = match bounded(Watched::start(user_bus, system_bus, verify_process)).await
        {
            Ok(watched) => watched,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        match watched.private().await {
            Ok(true) => {}
            Ok(false) => shared.signal(),
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        }
        if ready.send(Ok(watched.session.clone())).is_err()
            || shared.lost.load(std::sync::atomic::Ordering::Acquire)
        {
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
                // Every shield transition ends consent, even a rapid lock/unlock.
                _ = watched.shield.next() => false,
                // This stream is restricted to the shell's well-known name.
                _ = watched.owner_changes.next() => false,
                _ = poll.tick() => watched.private().await.unwrap_or(false),
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
                    "GNOME screen-lock monitor stopped",
                )));
            }
            () = monitor => {}
        }
    });
}

struct Watched {
    connection: Connection,
    system: Connection,
    owner: String,
    shell_pid: u32,
    uid: u32,
    session: OwnedObjectPath,
    shield: MessageStream,
    owner_changes: MessageStream,
}

impl Watched {
    async fn start<U, UF, S, SF, V>(
        user_bus: U,
        system_bus: S,
        verify_process: V,
    ) -> io::Result<Self>
    where
        U: FnOnce() -> UF,
        UF: Future<Output = zbus::Result<Connection>>,
        S: FnOnce() -> SF,
        SF: Future<Output = zbus::Result<Connection>>,
        V: FnOnce(u32, u32, &str) -> io::Result<()>,
    {
        let connection = user_bus().await.map_err(other)?;
        let system = system_bus().await.map_err(other)?;
        let owner_rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .interface(DBUS)
            .and_then(|builder| builder.member("NameOwnerChanged"))
            .and_then(|builder| builder.path(DBUS_PATH))
            .and_then(|builder| builder.arg(0, SHELL))
            .map(|builder| builder.build())
            .map_err(other)?;
        let owner_changes = MessageStream::for_match_rule(owner_rule, &connection, Some(8))
            .await
            .map_err(other)?;
        let owner = owner(&connection).await?;
        let pid: u32 = bounded(async {
            let reply = connection
                .call_method(
                    Some(DBUS),
                    DBUS_PATH,
                    Some(DBUS),
                    "GetConnectionUnixProcessID",
                    &(owner.as_str(),),
                )
                .await
                .map_err(other)?;
            reply.body().deserialize().map_err(other)
        })
        .await?;
        let uid = geteuid().as_raw();
        let (session, runtime) = bounded(graphical_session(&system, pid, uid)).await?;
        verify_process(pid, uid, &runtime)?;
        let shield_rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .interface(SCREENSAVER)
            .and_then(|builder| builder.member("ActiveChanged"))
            .and_then(|builder| builder.path(SCREENSAVER_PATH))
            .map(|builder| builder.build())
            .map_err(other)?;
        let shield = MessageStream::for_match_rule(shield_rule, &connection, Some(8))
            .await
            .map_err(other)?;
        Ok(Self {
            connection,
            system,
            owner,
            shell_pid: pid,
            uid,
            session,
            shield,
            owner_changes,
        })
    }

    async fn private(&self) -> io::Result<bool> {
        let owner = owner(&self.connection).await?;
        if owner != self.owner {
            return Ok(false);
        }
        let (session, _) =
            bounded(graphical_session(&self.system, self.shell_pid, self.uid)).await?;
        if session != self.session {
            return Ok(false);
        }
        bounded(async {
            let reply = self
                .connection
                .call_method(
                    Some(self.owner.as_str()),
                    SCREENSAVER_PATH,
                    Some(SCREENSAVER),
                    "GetActive",
                    &(),
                )
                .await
                .map_err(other)?;
            let active: bool = reply.body().deserialize().map_err(other)?;
            Ok(!active)
        })
        .await
    }
}

async fn owner(connection: &Connection) -> io::Result<String> {
    bounded(async {
        let reply = connection
            .call_method(Some(DBUS), DBUS_PATH, Some(DBUS), "GetNameOwner", &(SHELL,))
            .await
            .map_err(other)?;
        reply.body().deserialize().map_err(other)
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::net::UnixStream,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Mutex,
        },
        time::{Duration, Instant},
    };

    use zbus::{connection::Builder, object_server::SignalEmitter, zvariant::OwnedObjectPath};

    use super::*;

    const CURRENT_SESSION: &str = "/org/freedesktop/login1/session/_31";
    const OTHER_SESSION: &str = "/org/freedesktop/login1/session/_32";
    const CURRENT_USER: &str = "/org/freedesktop/login1/user/_501";
    const SHELL_PID: u32 = 123456;

    struct FakeBus {
        owner: Arc<Mutex<String>>,
    }

    #[zbus::interface(name = "org.freedesktop.DBus")]
    impl FakeBus {
        #[zbus(name = "GetNameOwner")]
        fn get_name_owner(&self, _name: &str) -> String {
            self.owner.lock().unwrap().clone()
        }

        #[zbus(name = "GetConnectionUnixProcessID")]
        fn get_connection_unix_process_id(&self, _name: &str) -> u32 {
            SHELL_PID
        }

        #[zbus(signal)]
        async fn name_owner_changed(
            emitter: &SignalEmitter<'_>,
            name: &str,
            old: &str,
            new: &str,
        ) -> zbus::Result<()>;
    }

    struct FakeShield {
        active: Arc<AtomicBool>,
    }

    #[zbus::interface(name = "org.gnome.ScreenSaver")]
    impl FakeShield {
        #[zbus(name = "GetActive")]
        fn get_active(&self) -> bool {
            self.active.load(Ordering::Acquire)
        }

        #[zbus(signal)]
        async fn active_changed(emitter: &SignalEmitter<'_>, active: bool) -> zbus::Result<()>;
    }

    struct FakeManager {
        foreign: bool,
        unscoped: bool,
        ambiguous: Arc<AtomicBool>,
    }

    #[derive(Debug, zbus::DBusError)]
    #[zbus(prefix = "org.freedesktop.login1")]
    enum FakeLogindError {
        NoSessionForPID(String),
    }

    #[zbus::interface(name = "org.freedesktop.login1.Manager")]
    impl FakeManager {
        #[zbus(name = "GetSessionByPID")]
        fn get_session_by_pid(&self, pid: u32) -> Result<OwnedObjectPath, FakeLogindError> {
            if self.unscoped {
                return Err(FakeLogindError::NoSessionForPID("no session".to_owned()));
            }
            let path = if self.foreign && pid == SHELL_PID {
                OTHER_SESSION
            } else {
                CURRENT_SESSION
            };
            Ok(OwnedObjectPath::try_from(path).unwrap())
        }

        #[zbus(name = "GetUserByPID")]
        fn get_user_by_pid(&self, _pid: u32) -> OwnedObjectPath {
            OwnedObjectPath::try_from(CURRENT_USER).unwrap()
        }

        #[zbus(name = "ListSessions")]
        fn list_sessions(&self) -> Vec<(String, u32, String, String, OwnedObjectPath)> {
            let session = |id: &str, path: &str| {
                (
                    id.to_owned(),
                    geteuid().as_raw(),
                    "user".to_owned(),
                    "seat0".to_owned(),
                    OwnedObjectPath::try_from(path).unwrap(),
                )
            };
            let mut sessions = vec![session("1", CURRENT_SESSION)];
            if self.ambiguous.load(Ordering::Acquire) {
                sessions.push(session("2", OTHER_SESSION));
            }
            sessions
        }
    }

    struct FakeSession;

    #[zbus::interface(name = "org.freedesktop.login1.Session")]
    impl FakeSession {
        #[zbus(property)]
        fn active(&self) -> bool {
            true
        }

        #[zbus(property)]
        fn remote(&self) -> bool {
            false
        }

        #[zbus(property, name = "Type")]
        fn kind(&self) -> &str {
            "wayland"
        }

        #[zbus(property)]
        fn class(&self) -> &str {
            "user"
        }

        #[zbus(property)]
        fn user(&self) -> (u32, OwnedObjectPath) {
            (
                geteuid().as_raw(),
                OwnedObjectPath::try_from(CURRENT_USER).unwrap(),
            )
        }
    }

    struct FakeUser;

    #[zbus::interface(name = "org.freedesktop.login1.User")]
    impl FakeUser {
        #[zbus(property)]
        fn runtime_path(&self) -> &str {
            "/run/user/test"
        }
    }

    struct Fixture {
        runtime: tokio::runtime::Runtime,
        user_server: Connection,
        _system_server: Connection,
        monitor: io::Result<GnomePrivacyMonitor>,
        calls: Arc<AtomicUsize>,
        active: Arc<AtomicBool>,
        ambiguous: Arc<AtomicBool>,
        owner: Arc<Mutex<String>>,
    }

    impl Fixture {
        fn start(active: bool, foreign: bool, process_ok: bool) -> Self {
            Self::start_scenario(active, foreign, process_ok, false, false)
        }

        fn start_scenario(
            active: bool,
            foreign: bool,
            process_ok: bool,
            unscoped: bool,
            ambiguous: bool,
        ) -> Self {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (user_server, user_client) = UnixStream::pair().unwrap();
            let (system_server, system_client) = UnixStream::pair().unwrap();
            let active = Arc::new(AtomicBool::new(active));
            let ambiguous = Arc::new(AtomicBool::new(ambiguous));
            let owner = Arc::new(Mutex::new(":1.42".to_owned()));
            let shield_active = active.clone();
            let manager_ambiguous = ambiguous.clone();
            let bus_owner = owner.clone();
            let user_server = thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap()
                    .block_on(async move {
                        Builder::unix_stream(user_server)
                            .server(zbus::Guid::generate())
                            .unwrap()
                            .p2p()
                            .serve_at(DBUS_PATH, FakeBus { owner: bus_owner })
                            .unwrap()
                            .serve_at(
                                SCREENSAVER_PATH,
                                FakeShield {
                                    active: shield_active,
                                },
                            )
                            .unwrap()
                            .build()
                            .await
                            .unwrap()
                    })
            });
            let system_server = thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap()
                    .block_on(async move {
                        Builder::unix_stream(system_server)
                            .server(zbus::Guid::generate())
                            .unwrap()
                            .p2p()
                            .serve_at(
                                MANAGER_PATH,
                                FakeManager {
                                    foreign,
                                    unscoped,
                                    ambiguous: manager_ambiguous,
                                },
                            )
                            .unwrap()
                            .serve_at(CURRENT_SESSION, FakeSession)
                            .unwrap()
                            .serve_at(OTHER_SESSION, FakeSession)
                            .unwrap()
                            .serve_at(CURRENT_USER, FakeUser)
                            .unwrap()
                            .build()
                            .await
                            .unwrap()
                    })
            });
            let calls = Arc::new(AtomicUsize::new(0));
            let counter = calls.clone();
            let monitor = GnomePrivacyMonitor::install_with(
                Arc::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                }),
                move || async move { Builder::unix_stream(user_client).p2p().build().await },
                move || async move { Builder::unix_stream(system_client).p2p().build().await },
                move |pid, uid, runtime| {
                    assert_eq!(pid, SHELL_PID);
                    assert_eq!(uid, geteuid().as_raw());
                    assert_eq!(runtime, "/run/user/test");
                    if process_ok {
                        Ok(())
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            "not a GNOME Shell process",
                        ))
                    }
                },
            );
            Self {
                runtime,
                user_server: user_server.join().unwrap(),
                _system_server: system_server.join().unwrap(),
                monitor,
                calls,
                active,
                ambiguous,
                owner,
            }
        }

        fn emit_shield(&self, active: bool) {
            self.active.store(active, Ordering::Release);
            self.runtime.block_on(async {
                let iface = self
                    .user_server
                    .object_server()
                    .interface::<_, FakeShield>(SCREENSAVER_PATH)
                    .await
                    .unwrap();
                FakeShield::active_changed(iface.signal_emitter(), active)
                    .await
                    .unwrap();
            });
        }

        fn replace_owner(&self, new: &str, emit: bool) {
            let old = std::mem::replace(&mut *self.owner.lock().unwrap(), new.to_owned());
            if emit {
                self.runtime.block_on(async {
                    let iface = self
                        .user_server
                        .object_server()
                        .interface::<_, FakeBus>(DBUS_PATH)
                        .await
                        .unwrap();
                    FakeBus::name_owner_changed(iface.signal_emitter(), SHELL, &old, new)
                        .await
                        .unwrap();
                });
            }
        }

        fn wait_for_loss(&self) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.calls.load(Ordering::SeqCst) == 0 {
                assert!(
                    Instant::now() < deadline,
                    "GNOME privacy loss was not observed"
                );
                thread::sleep(Duration::from_millis(10));
            }
            thread::sleep(POLL_INTERVAL * 2);
            assert_eq!(self.calls.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn only_explicit_gnome_desktops_are_supported() {
        assert!(supported_desktop(Some("ubuntu:GNOME")));
        assert!(supported_desktop(Some("gnome")));
        for desktop in [None, Some(""), Some("KDE"), Some("GNOMEish"), Some("XFCE")] {
            assert!(!supported_desktop(desktop));
        }
    }

    #[test]
    #[ignore = "requires a running GNOME Shell and an externally triggered screen shield"]
    fn real_gnome_screen_shield_revokes_consent() {
        let (send, receive) = mpsc::channel();
        let _monitor = super::super::RecordingPrivacyMonitor::install(Arc::new(move || {
            let _ = send.send(());
        }))
        .expect("install the GNOME screen-shield monitor");
        println!("GNOME_PRIVACY_MONITOR_READY");
        receive
            .recv_timeout(Duration::from_secs(30))
            .expect("GNOME screen-shield transition must revoke consent");
    }

    #[test]
    fn unavailable_locked_or_foreign_shell_fails_closed() {
        for (active, foreign, process_ok) in [
            (true, false, true),
            (false, true, true),
            (false, false, false),
        ] {
            let fixture = Fixture::start(active, foreign, process_ok);
            assert!(fixture.monitor.is_err());
            assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn unscoped_processes_require_one_unique_graphical_session() {
        let unique = Fixture::start_scenario(false, false, true, true, false);
        assert!(unique.monitor.is_ok());
        let ambiguous = Fixture::start_scenario(false, false, true, true, true);
        assert!(ambiguous.monitor.is_err());
        assert_eq!(ambiguous.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_new_ambiguous_graphical_session_revokes_existing_consent() {
        let fixture = Fixture::start_scenario(false, false, true, true, false);
        assert!(fixture.monitor.is_ok());
        fixture.ambiguous.store(true, Ordering::Release);
        fixture.wait_for_loss();
    }

    #[test]
    fn wayland_socket_must_be_owned_by_the_expected_shell_process_and_user() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wayland-test");
        let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let pid = std::process::id();
        let uid = geteuid().as_raw();
        assert!(verify_wayland_peer(&path, pid, uid).is_ok());
        assert!(verify_wayland_peer(&path, pid + 1, uid).is_err());
        assert!(verify_wayland_peer(&path, pid, uid + 1).is_err());
    }

    #[test]
    fn wayland_socket_path_rejects_unverified_or_inherited_displays() {
        use std::ffi::OsStr;

        let runtime = "/run/user/501";
        assert_eq!(
            wayland_socket_path(
                runtime,
                Some(OsStr::new(runtime)),
                Some(OsStr::new("wayland-0")),
                false,
            )
            .unwrap(),
            Path::new(runtime).join("wayland-0")
        );
        for (directory, display, inherited) in [
            (Some(runtime), Some("wayland-0"), true),
            (Some("/run/user/other"), Some("wayland-0"), false),
            (Some(runtime), Some("/tmp/wayland-0"), false),
            (Some(runtime), Some("../wayland-0"), false),
            (None, Some("wayland-0"), false),
            (Some(runtime), None, false),
        ] {
            assert!(wayland_socket_path(
                runtime,
                directory.map(OsStr::new),
                display.map(OsStr::new),
                inherited,
            )
            .is_err());
        }
    }

    #[test]
    fn a_brief_shield_transition_revokes_consent_once() {
        let fixture = Fixture::start(false, false, true);
        assert!(fixture.monitor.is_ok());
        fixture.emit_shield(true);
        fixture.emit_shield(false);
        fixture.wait_for_loss();
    }

    #[test]
    fn shell_owner_replacement_or_silent_loss_revokes_consent() {
        for emit in [true, false] {
            let fixture = Fixture::start(false, false, true);
            assert!(fixture.monitor.is_ok());
            fixture.replace_owner(":1.43", emit);
            fixture.wait_for_loss();
        }
    }
}
