//! Real loopback: a live `festerm-sessiond` daemon served through the bridge
//! host over an in-memory relay, driven only through the mobile tunnels API.
//! No Microsoft service is involved; the account and tunnel directory are fakes.
#![cfg(unix)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use festerm_mobile_profiles::{
    DevTunnelProfile, ProfileId, ProfileKind, ProfileStore, TunnelSource,
};
use festerm_mobile_tunnels::{
    testing::{seed_account, FakeGithub, FixedDirectory, StreamConnector, CLIENT_ID},
    AttachMode, CancelToken, ClientId, SessionState, TunnelClient, TunnelError,
};
use festerm_secret_store::MemorySecretStore;
use festerm_session::{noop_session_event_notifier, Session, SessionEvent, SessionLifecycle};
use festerm_sessiond::{
    list_running_sessions_in, list_unattached_sessions_in, AttachKind, PersistentSession,
};
use festerm_tunnel_bridge::{
    files::FileService,
    mobile::{BoxStream, Displacer},
    transport::{serve_with_reclaim, Holder},
};

struct Cleanup {
    executable: PathBuf,
    root: PathBuf,
    name: String,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = Command::new(&self.executable)
            .env("XDG_STATE_HOME", &self.root)
            .args(["kill", "--name", &self.name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn poll(mut ready: impl FnMut() -> bool, phase: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out: {phase}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn short_root() -> PathBuf {
    let base = std::env::var_os("FESTERM_SESSIOND_TEST_RUNTIME_ROOT").map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(".f")
        },
        PathBuf::from,
    );
    fs::create_dir_all(&base).unwrap();
    let root = base.join(format!("mt-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();
    fs::canonicalize(root).unwrap()
}

fn profile(
    store: &Mutex<ProfileStore>,
    account: &str,
    trusted: festerm_mobile_profiles::TrustedTunnelHost,
    takeover: bool,
) -> ProfileId {
    store
        .lock()
        .unwrap()
        .create(
            &format!("loopback-{takeover}"),
            ProfileKind::DevTunnel(DevTunnelProfile {
                account: Some(account.to_owned()),
                source: TunnelSource::Named {
                    tunnel_name: "devbox.euw".into(),
                    trusted_host: Some(trusted),
                    connect_credential: None,
                },
                session: None,
                take_over_if_attached: takeover,
                auto_reconnect: true,
            }),
        )
        .unwrap()
}

fn read_until(session: &Arc<dyn Session>, marker: &str) {
    let mut seen = String::new();
    poll(
        || {
            while let Ok(event) = session.try_recv_event() {
                if let SessionEvent::Output(bytes) = event {
                    seen.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
            seen.contains(marker)
        },
        marker,
    );
}

#[test]
fn mobile_api_attaches_takes_over_and_is_displaced_on_a_real_daemon() {
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_festerm-sessiond"));
    let root = short_root();
    // The bridge host reads the holder from the default registry location.
    std::env::set_var("XDG_STATE_HOME", &root);
    let name = format!("mobile-{}", std::process::id());
    let _cleanup = Cleanup {
        executable: executable.clone(),
        root: root.clone(),
        name: name.clone(),
    };
    let registry = root.join("festerm").join("sessiond");
    let started = Command::new(&executable)
        .env("XDG_STATE_HOME", &root)
        .args(["start", "--name", &name, "--shell", "/bin/cat"])
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");
    poll(
        || list_unattached_sessions_in(&registry).is_ok_and(|s| s.len() == 1),
        "daemon discovery",
    );
    let selected = list_unattached_sessions_in(&registry).unwrap().remove(0);
    let holder = || {
        list_running_sessions_in(&registry)
            .unwrap()
            .into_iter()
            .find(|running| running.session.name == name)
            .map(|running| running.attached_by)
    };

    // A local terminal holds the session first.
    let local = PersistentSession::resume_discovered_in(
        &selected,
        &registry,
        noop_session_event_notifier(),
    )
    .unwrap();
    let identity = local.native_identity().expect("native identity");
    poll(
        || matches!(local.lifecycle(), SessionLifecycle::Running),
        "local running",
    );
    local.acknowledge_recovery_adoption();
    poll(|| holder() == Some(Some(AttachKind::Local)), "local holder");

    let store_dir = root.join("profiles");
    fs::create_dir(&store_dir).unwrap();
    let store = Arc::new(Mutex::new(
        ProfileStore::open(&store_dir, Arc::new(MemorySecretStore::new())).unwrap(),
    ));
    let account = seed_account(&store, 42, "octo", false);
    let directory = Arc::new(FixedDirectory::new());
    let connections = Arc::new(AtomicUsize::new(0));
    let bridge_holder = Holder::default();
    let connector = {
        let connections = connections.clone();
        StreamConnector(move || -> BoxStream {
            connections.fetch_add(1, Ordering::SeqCst);
            let (client, server) = tokio::io::duplex(1 << 16);
            let identity = identity.clone();
            let bridge_holder = bridge_holder.clone();
            tokio::spawn(async move {
                let _ = serve_with_reclaim(
                    server,
                    identity,
                    FileService::new(false),
                    true,
                    &bridge_holder,
                )
                .await;
            });
            Box::new(client)
        })
    };
    let client = TunnelClient::with_parts(
        store.clone(),
        Arc::new(FakeGithub::new(42, "octo")),
        directory.clone(),
        Arc::new(connector),
        CLIENT_ID.into(),
    )
    .unwrap();
    let cancel = CancelToken::new();
    let refusing = profile(&store, &account, directory.trusted_host(), false);
    let taking = profile(&store, &account, directory.trusted_host(), true);

    let sessions = client.list_sessions(&refusing, &cancel).unwrap();
    assert_eq!(sessions[0].name, name);
    assert_eq!(
        sessions[0].state,
        SessionState::Attached {
            by: Some(Displacer::Local)
        }
    );
    assert_eq!(
        client
            .attach(&refusing, noop_session_event_notifier(), &cancel)
            .err(),
        Some(TunnelError::SessionInUse {
            by: Some(Displacer::Local)
        }),
        "a profile without take_over_if_attached never displaces the local client"
    );
    assert!(matches!(local.lifecycle(), SessionLifecycle::Running));

    // Takeover: the mobile client displaces the local holder and works the shell.
    let attachment = client
        .attach(&taking, noop_session_event_notifier(), &cancel)
        .unwrap();
    assert_eq!(attachment.took_over, Some(Some(Displacer::Local)));
    attachment
        .take_recovered_terminal()
        .expect("recovered terminal");
    let session = attachment.session();
    poll(
        || !matches!(local.lifecycle(), SessionLifecycle::Running),
        "local displaced",
    );
    poll(
        || holder() == Some(Some(AttachKind::Remote)),
        "remote holder",
    );
    poll(
        || matches!(session.lifecycle(), SessionLifecycle::Running),
        "tunnel running",
    );
    session.try_send_input(b"ping-from-mobile\n").unwrap();
    read_until(&session, "ping-from-mobile");
    drop(local);

    let dialled = connections.load(Ordering::SeqCst);
    // The local terminal takes it back: the mobile session ends displaced and stays ended.
    let running = list_running_sessions_in(&registry).unwrap();
    let back = PersistentSession::take_over_discovered_in(
        &running[0].session,
        &registry,
        noop_session_event_notifier(),
    )
    .unwrap();
    poll(
        || matches!(back.lifecycle(), SessionLifecycle::Running),
        "local back running",
    );
    back.acknowledge_recovery_adoption();
    poll(
        || attachment.end_error().is_some(),
        "mobile session displaced",
    );
    assert_eq!(
        attachment.end_error(),
        Some(TunnelError::Displaced(Displacer::Local))
    );
    assert!(matches!(
        session.lifecycle(),
        SessionLifecycle::Disconnected(_)
    ));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        connections.load(Ordering::SeqCst),
        dialled,
        "no silent reattach"
    );
    drop(back);
    poll(|| holder() == Some(None), "local released");

    // An explicit attach and detach releases the daemon session again.
    let again = client
        .attach(&refusing, noop_session_event_notifier(), &cancel)
        .unwrap();
    assert_eq!(again.took_over, None);
    again.take_recovered_terminal().unwrap();
    poll(
        || holder() == Some(Some(AttachKind::Remote)),
        "second holder",
    );
    again.detach(Duration::from_secs(3)).unwrap();
    poll(|| holder() == Some(None), "released after detach");

    // Automatic reconnects only ever reclaim the same tab's own attachment.
    let tab = ClientId::new();
    let other_tab = ClientId::new();
    let automatic = |id: &ClientId| {
        client.attach_as(
            &refusing,
            id,
            AttachMode::Automatic,
            noop_session_event_notifier(),
            &cancel,
        )
    };
    let stale = automatic(&tab).unwrap();
    stale.take_recovered_terminal().unwrap();
    poll(|| holder() == Some(Some(AttachKind::Remote)), "tab holder");
    assert_eq!(
        automatic(&other_tab).err(),
        Some(TunnelError::SessionInUse {
            by: Some(Displacer::Remote)
        }),
        "another tab never steals automatically"
    );
    let reclaimed = automatic(&tab).unwrap();
    assert_eq!(reclaimed.took_over, None);
    reclaimed.take_recovered_terminal().unwrap();
    poll(|| stale.end_error().is_some(), "stale attachment replaced");
    let session = reclaimed.session();
    poll(
        || matches!(session.lifecycle(), SessionLifecycle::Running),
        "reclaimed running",
    );
    session.try_send_input(b"ping-after-reclaim\n").unwrap();
    read_until(&session, "ping-after-reclaim");

    // Once another client takes the session over, the displaced tab cannot reclaim it.
    let thief = client
        .attach_as(
            &taking,
            &other_tab,
            AttachMode::Manual,
            noop_session_event_notifier(),
            &cancel,
        )
        .unwrap();
    thief.take_recovered_terminal().unwrap();
    poll(|| reclaimed.end_error().is_some(), "reclaimed displaced");
    assert_eq!(
        automatic(&tab).err(),
        Some(TunnelError::SessionInUse {
            by: Some(Displacer::Remote)
        }),
        "a displaced tab does not take the session back"
    );
    thief.detach(Duration::from_secs(3)).unwrap();
    poll(|| holder() == Some(None), "released after thief detach");
}
