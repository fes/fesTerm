use std::{
    future::Future,
    io,
    sync::{mpsc, Arc},
    thread::{self, JoinHandle},
};

use futures_util::StreamExt;
use tokio::sync::watch;
use zbus::{message::Type as MessageType, Connection, MatchRule, MessageStream};

use super::{
    bounded, other, resolve_session, Shared, LOGIND, MANAGER, MANAGER_PATH, POLL_INTERVAL,
    READY_TIMEOUT,
};

const SHELL: &str = "org.gnome.Shell";
const SCREENSAVER_PATH: &str = "/org/gnome/ScreenSaver";
const SCREENSAVER: &str = "org.gnome.ScreenSaver";
const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";

pub(super) fn supported_desktop(desktop: Option<&str>) -> bool {
    desktop.is_some_and(|value| {
        value
            .split(':')
            .any(|part| part.eq_ignore_ascii_case("GNOME"))
    })
}

fn verify_shell_process(pid: u32) -> io::Result<()> {
    let executable = std::fs::read_link(format!("/proc/{pid}/exe"))?;
    if executable.file_name() == Some(std::ffi::OsStr::new("gnome-shell")) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "The GNOME Shell lock service is not owned by gnome-shell",
        ))
    }
}

pub(super) struct GnomePrivacyMonitor {
    shared: Arc<Shared>,
    shutdown: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
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
        V: FnOnce(u32) -> io::Result<()> + Send + 'static,
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
                    Ok(Ok(())) if !shared.lost.load(std::sync::atomic::Ordering::Acquire) => Ok(()),
                    Ok(Ok(())) => Err(io::Error::new(
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
            Ok(()) => Ok(monitor),
            Err(error) => {
                shared.signal();
                Err(error)
            }
        }
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
    ready: mpsc::SyncSender<io::Result<()>>,
) where
    U: FnOnce() -> UF,
    UF: Future<Output = zbus::Result<Connection>>,
    S: FnOnce() -> SF,
    SF: Future<Output = zbus::Result<Connection>>,
    V: FnOnce(u32) -> io::Result<()>,
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
        if ready.send(Ok(())).is_err() || shared.lost.load(std::sync::atomic::Ordering::Acquire) {
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
    owner: String,
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
        V: FnOnce(u32) -> io::Result<()>,
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
        verify_process(pid)?;
        let shell_session: zbus::zvariant::OwnedObjectPath = bounded(async {
            let reply = system
                .call_method(
                    Some(LOGIND),
                    MANAGER_PATH,
                    Some(MANAGER),
                    "GetSessionByPID",
                    &(pid,),
                )
                .await
                .map_err(other)?;
            reply.body().deserialize().map_err(other)
        })
        .await?;
        let current_session = bounded(resolve_session(&system)).await?;
        if shell_session != current_session {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "GNOME Shell does not belong to this logind session",
            ));
        }
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
            owner,
            shield,
            owner_changes,
        })
    }

    async fn private(&self) -> io::Result<bool> {
        let owner = owner(&self.connection).await?;
        if owner != self.owner {
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
    }

    #[zbus::interface(name = "org.freedesktop.login1.Manager")]
    impl FakeManager {
        #[zbus(name = "GetSessionByPID")]
        fn get_session_by_pid(&self, pid: u32) -> OwnedObjectPath {
            let path = if self.foreign && pid == SHELL_PID {
                "/org/freedesktop/login1/session/_32"
            } else {
                CURRENT_SESSION
            };
            OwnedObjectPath::try_from(path).unwrap()
        }
    }

    struct Fixture {
        runtime: tokio::runtime::Runtime,
        user_server: Connection,
        _system_server: Connection,
        monitor: io::Result<GnomePrivacyMonitor>,
        calls: Arc<AtomicUsize>,
        active: Arc<AtomicBool>,
        owner: Arc<Mutex<String>>,
    }

    impl Fixture {
        fn start(active: bool, foreign: bool, process_ok: bool) -> Self {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (user_server, user_client) = UnixStream::pair().unwrap();
            let (system_server, system_client) = UnixStream::pair().unwrap();
            let active = Arc::new(AtomicBool::new(active));
            let owner = Arc::new(Mutex::new(":1.42".to_owned()));
            let shield_active = active.clone();
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
                            .serve_at(MANAGER_PATH, FakeManager { foreign })
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
                move |pid| {
                    assert_eq!(pid, SHELL_PID);
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
        let _monitor = GnomePrivacyMonitor::install(Arc::new(move || {
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
