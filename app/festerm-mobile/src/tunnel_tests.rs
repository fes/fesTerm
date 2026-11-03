//! Dev Tunnel flows against a scripted in-memory bridge host.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use festerm_core::{Dimensions, Terminal};
use festerm_mobile_profiles::ProfileId;
use festerm_mobile_tunnels::{
    testing::{seed_account, FakeGithub, FixedDirectory, StreamConnector, CLIENT_ID},
    TunnelClient,
};
use festerm_tunnel_bridge::{
    mobile::{BoxStream, Description, Displacer, Target},
    protocol::{
        encode_snapshot, FileAccess, Hello, Sync, ADOPTED, CAPABILITIES, DESCRIPTION, DETACH,
        DISCOVER, DISPLACED, HELLO, INPUT, OUTPUT, REFUSED, SNAPSHOT, SYNC, VERSION,
    },
    wire::{read_frame, write_frame},
};

use super::tunnel_link::ConnectStage;
use super::*;

struct Script {
    attached_by: Option<Displacer>,
    displace: tokio::sync::Notify,
    exit: tokio::sync::Notify,
    /// Frames the host sends right after the adoption handshake.
    after_adopt: Vec<(u8, Vec<u8>)>,
    input: Mutex<Vec<u8>>,
    connections: AtomicUsize,
    probes: AtomicUsize,
    probe_mode: AtomicUsize,
    probe_release: tokio::sync::Notify,
    clients: Mutex<Vec<Option<festerm_mobile_tunnels::ClientId>>>,
}

async fn host(mut stream: tokio::io::DuplexStream, script: Arc<Script>) {
    let Ok((kind, bytes)) = read_frame(&mut stream).await else {
        return;
    };
    if kind == DISCOVER {
        let description = Description {
            version: VERSION,
            target: Target {
                name: "main".into(),
                pid: 7,
                generation: "9".into(),
            },
            files: FileAccess::default(),
            attached_by: script.attached_by,
            takeover_allowed: true,
            reclaim: true,
        };
        let _ = write_frame(
            &mut stream,
            DESCRIPTION,
            &serde_json::to_vec(&description).unwrap(),
        )
        .await;
        return;
    }
    assert_eq!(kind, HELLO);
    let hello: Hello = serde_json::from_slice(&bytes).unwrap();
    script.clients.lock().unwrap().push(hello.client);
    // Like the real host: a held session needs takeover (no holder record
    // here, so nothing is reclaimable).
    if script.attached_by.is_some() && !hello.takeover {
        let _ = write_frame(&mut stream, REFUSED, &[]).await;
        return;
    }
    write_frame(&mut stream, CAPABILITIES, &[0, 0])
        .await
        .unwrap();
    let mut terminal = Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap();
    terminal.ingest(b"recovered line\r\n");
    write_frame(
        &mut stream,
        SNAPSHOT,
        &encode_snapshot(&terminal.recovery_clone()).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(read_frame(&mut stream).await.unwrap().0, ADOPTED);
    for (kind, bytes) in &script.after_adopt {
        write_frame(&mut stream, *kind, bytes).await.unwrap();
    }
    loop {
        tokio::select! {
            () = script.exit.notified() => {
                let _ = write_frame(&mut stream, festerm_tunnel_bridge::protocol::EXITED, &[]).await;
                return;
            }
            () = script.displace.notified() => {
                let _ = write_frame(&mut stream, DISPLACED, &[Displacer::Remote.code()]).await;
                return;
            }
            frame = read_frame(&mut stream) => match frame {
                Ok((DETACH, _)) | Err(_) => return,
                Ok((INPUT, bytes)) => script.input.lock().unwrap().extend(bytes),
                Ok(_) => {}
            },
        }
    }
}

impl festerm_mobile_tunnels::HostLiveness for Script {
    fn check(&self) -> festerm_mobile_tunnels::BoxFuture<'_, std::io::Result<()>> {
        Box::pin(async {
            self.probes.fetch_add(1, Ordering::SeqCst);
            match self.probe_mode.load(Ordering::SeqCst) {
                0 => Ok(()),
                1 => Err(std::io::Error::other("host unavailable")),
                _ => {
                    self.probe_release.notified().await;
                    Ok(())
                }
            }
        })
    }
}

struct ProbedConnector<F: Fn() -> BoxStream + Send + std::marker::Sync> {
    streams: StreamConnector<F>,
    script: Arc<Script>,
}
impl<F: Fn() -> BoxStream + Send + std::marker::Sync> festerm_mobile_tunnels::BridgeConnector
    for ProbedConnector<F>
{
    fn connect<'a>(
        &'a self,
        resolved: &'a festerm_mobile_tunnels::ResolvedTunnel,
    ) -> festerm_mobile_tunnels::BoxFuture<'a, std::io::Result<BoxStream>> {
        self.streams.connect(resolved)
    }
    fn connect_retained<'a>(
        &'a self,
        resolved: &'a festerm_mobile_tunnels::ResolvedTunnel,
    ) -> festerm_mobile_tunnels::BoxFuture<
        'a,
        std::io::Result<festerm_mobile_tunnels::RetainedConnection>,
    > {
        Box::pin(async {
            Ok(festerm_mobile_tunnels::RetainedConnection {
                stream: self.connect(resolved).await?,
                probe: Some(self.script.clone()),
            })
        })
    }
}

struct Rig {
    app: MobileApp,
    ctx: egui::Context,
    script: Arc<Script>,
    profile: String,
}

fn rig(attached_by: Option<Displacer>, take_over: bool, trusted: bool) -> Rig {
    rig_with(attached_by, take_over, trusted, &[])
}

fn rig_with(
    attached_by: Option<Displacer>,
    take_over: bool,
    trusted: bool,
    after_adopt: &[(u8, Vec<u8>)],
) -> Rig {
    let script = Arc::new(Script {
        attached_by,
        displace: tokio::sync::Notify::new(),
        exit: tokio::sync::Notify::new(),
        after_adopt: after_adopt.to_vec(),
        input: Mutex::new(Vec::new()),
        connections: AtomicUsize::new(0),
        probes: AtomicUsize::new(0),
        probe_mode: AtomicUsize::new(0),
        probe_release: tokio::sync::Notify::new(),
        clients: Mutex::new(Vec::new()),
    });
    let mut profiles = profiles::testing::profiles();
    let store = profiles.store_handle();
    let account = seed_account(&store, 42, "octo", false);
    let directory = Arc::new(FixedDirectory::new());
    let connector_script = script.clone();
    let connector = StreamConnector(move || -> BoxStream {
        connector_script.connections.fetch_add(1, Ordering::SeqCst);
        let (client, server) = tokio::io::duplex(1 << 16);
        tokio::spawn(host(server, connector_script.clone()));
        Box::new(client)
    });
    let client = TunnelClient::with_parts(
        store.clone(),
        Arc::new(FakeGithub::new(42, "octo")),
        directory.clone(),
        Arc::new(ProbedConnector {
            streams: connector,
            script: script.clone(),
        }),
        CLIENT_ID.into(),
    )
    .unwrap();
    let profile = profiles
        .save_tunnel(
            None,
            "devbox",
            Some(account),
            "devbox.euw",
            None,
            crate::profiles::TunnelBehaviour {
                take_over,
                auto_reconnect: true,
            },
        )
        .unwrap();
    if trusted {
        let id = ProfileId::parse(&profile).unwrap();
        client.approve_host(&id, directory.trusted_host()).unwrap();
    }
    let app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle::default())))
        .with_profiles(profiles)
        .with_tunnel_client(Arc::new(client));
    Rig {
        app,
        ctx: egui::Context::default(),
        script,
        profile,
    }
}

fn wait_until(rig: &mut Rig, mut done: impl FnMut(&mut MobileApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        rig.app.poll_tunnels(&rig.ctx);
        if done(&mut rig.app) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn pump(rig: &mut Rig) {
    let mut live = rig.app.sink.live.connection.borrow_mut();
    live.as_mut()
        .expect("attached")
        .poll(&mut rig.app.terminal, &rig.ctx);
}

#[test]
fn trusted_tunnel_attaches_into_a_tab_with_the_hosts_screen() {
    let mut rig = rig(None, true, true);
    let id = rig.profile.clone();
    let before = rig.app.tabs.len();
    rig.app.start_tunnel_connect(&rig.ctx, &id, None, false);
    wait_until(&mut rig, |app| app.active_tunnel().is_some());
    assert_eq!(rig.app.tabs.len(), before + 1);
    assert!(rig
        .app
        .terminal
        .row_text(0)
        .is_some_and(|row| row.contains("recovered line")));
    let surfaces = rig.app.tab_surfaces();
    let surface = surfaces.last().unwrap();
    assert_eq!(surface.kind, model::SurfaceKind::DevTunnel);
    assert_eq!(surface.state, model::SurfaceState::Connected);
    assert!(rig.app.chrome().subtitle.starts_with("Dev Tunnel"));
    assert!(
        rig.app.active_spec().is_none(),
        "tunnel tabs have no SSH spec, so Files stays hidden"
    );
}

#[test]
fn an_unapproved_host_waits_for_a_decision_before_attaching() {
    let mut rig = rig(None, true, false);
    let id = rig.profile.clone();
    rig.app.start_tunnel_connect(&rig.ctx, &id, None, false);
    wait_until(&mut rig, |app| {
        matches!(
            app.tunnel.connect.as_ref().map(|flow| &flow.stage),
            Some(ConnectStage::Verify(_))
        )
    });
    assert_eq!(rig.script.connections.load(Ordering::SeqCst), 1);
    assert!(rig.app.active_tunnel().is_none());
}

#[test]
fn a_held_session_is_refused_with_a_take_over_hint_unless_enabled() {
    let mut refused = rig(Some(Displacer::Remote), false, true);
    let id = refused.profile.clone();
    refused
        .app
        .start_tunnel_connect(&refused.ctx, &id, None, false);
    wait_until(&mut refused, |app| app.notice.is_some());
    let notice = refused.app.notice.clone().unwrap();
    assert!(notice.contains("another device") && notice.contains("Take over"));
    assert!(refused.app.active_tunnel().is_none());

    let mut allowed = rig(Some(Displacer::Remote), true, true);
    let id = allowed.profile.clone();
    allowed
        .app
        .start_tunnel_connect(&allowed.ctx, &id, None, false);
    wait_until(&mut allowed, |app| app.active_tunnel().is_some());
}

#[test]
fn being_taken_over_ends_the_session_without_reattaching() {
    let mut rig = rig(None, true, true);
    let id = rig.profile.clone();
    rig.app.start_tunnel_connect(&rig.ctx, &id, None, false);
    wait_until(&mut rig, |app| app.active_tunnel().is_some());
    let attached_connections = rig.script.connections.load(Ordering::SeqCst);
    rig.script.displace.notify_one();
    let deadline = Instant::now() + Duration::from_secs(5);
    while rig.app.live_view().is_some_and(|live| !live.can_reset) {
        pump(&mut rig);
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = rig.app.live_view().unwrap().status;
    assert!(
        status.starts_with("Taken over by another device"),
        "{status}"
    );
    let banner = rig.app.reconnect_banner();
    let custom = banner.custom.expect("tunnel wording");
    assert_eq!(custom.title, "Session taken over");
    assert_eq!(custom.action, "Attach again");
    rig.app.auto_reconnect_tunnel(&rig.ctx);
    assert!(rig.app.tunnel.connect.is_none());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        rig.script.connections.load(Ordering::SeqCst),
        attached_connections,
        "a displaced session is never reattached automatically"
    );
}

#[test]
fn a_failed_resume_probe_reconnects_once_as_the_same_client() {
    let mut rig = rig(None, true, true);
    let id = rig.profile.clone();
    rig.app.start_tunnel_connect(&rig.ctx, &id, None, false);
    wait_until(&mut rig, |app| app.active_tunnel().is_some());
    let tabs = rig.app.tabs.len();
    let tab = rig.app.active_tab;

    rig.script.probe_mode.store(1, Ordering::SeqCst);
    rig.app.suspend_sessions();
    assert!(rig
        .app
        .live_view()
        .is_some_and(|live| !live.can_reset && !live.input_enabled));
    wait_probe(&mut rig, |app| {
        app.live_view().is_some_and(|live| live.can_reset)
    });
    rig.app.auto_reconnect_tunnel(&rig.ctx);
    wait_until(&mut rig, |app| {
        app.tunnel.connect.is_none() && app.live_view().is_some_and(|live| !live.can_reset)
    });
    assert_eq!(rig.app.tabs.len(), tabs, "reconnects into the same tab");
    assert_eq!(rig.app.active_tab, tab);
    let clients = rig.script.clients.lock().unwrap().clone();
    assert_eq!(clients.len(), 2);
    assert!(
        clients[0].is_some(),
        "the tab identifies itself to the host"
    );
    assert_eq!(clients[0], clients[1], "the same tab reclaims as itself");

    // An explicit disconnect is a choice, not a loss.
    let dialled = rig.script.connections.load(Ordering::SeqCst);
    if let Some(live) = rig.app.sink.live.connection.borrow_mut().as_mut() {
        live.stop("Disconnected. Reconnect to continue.");
    }
    rig.app.auto_reconnect_tunnel(&rig.ctx);
    assert!(rig.app.tunnel.connect.is_none());
    assert_eq!(rig.script.connections.load(Ordering::SeqCst), dialled);
}

#[test]
fn recovery_sync_is_applied_and_the_host_alone_answers_terminal_queries() {
    let sync = serde_json::to_vec(&Sync::Bytes(b"synced text\r\n".to_vec())).unwrap();
    // A cursor-position query would normally be answered by the frontend.
    let frames = [(SYNC, sync), (OUTPUT, b"\x1b[6n".to_vec())];
    let mut rig = rig_with(None, true, true, &frames);
    let id = rig.profile.clone();
    rig.app.start_tunnel_connect(&rig.ctx, &id, None, false);
    wait_until(&mut rig, |app| app.active_tunnel().is_some());
    let deadline = Instant::now() + Duration::from_secs(5);
    let has_sync = |rig: &Rig| {
        (0..24).any(|row| {
            rig.app
                .terminal
                .row_text(row)
                .is_some_and(|text| text.contains("synced text"))
        })
    };
    while !has_sync(&rig) {
        pump(&mut rig);
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(100));
    pump(&mut rig);
    assert!(
        rig.script.input.lock().unwrap().is_empty(),
        "the app must not answer queries the bridge host owns"
    );
}

fn wait_probe(rig: &mut Rig, mut done: impl FnMut(&MobileApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        pump(rig);
        if done(&rig.app) {
            return;
        }
        assert!(Instant::now() < deadline, "probe timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn healthy_resume_retains_active_and_parked_tunnels_and_coalesces_notifications() {
    let mut rig = rig(None, true, true);
    let profile = rig.profile.clone();
    rig.app
        .start_tunnel_connect(&rig.ctx, &profile, None, false);
    wait_until(&mut rig, |app| app.active_tunnel().is_some());
    let first = rig.app.active_tab.unwrap();
    rig.app
        .start_tunnel_connect(&rig.ctx, &profile, None, false);
    wait_until(&mut rig, |app| app.tunnel.connect.is_none());
    let second = rig.app.active_tab.unwrap();
    assert_ne!(first, second);
    let connections = rig.script.connections.load(Ordering::SeqCst);
    let history = rig.app.terminal.row_text(0);
    let session = rig
        .app
        .sink
        .live
        .connection
        .borrow()
        .as_ref()
        .unwrap()
        .tunnel_target();
    rig.app.suspend_sessions();
    rig.app.suspend_sessions();
    assert!(!rig.app.live_view().unwrap().input_enabled);
    wait_probe(&mut rig, |app| app.live_view().unwrap().input_enabled);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        rig.app.poll_background(&rig.ctx);
        if rig
            .app
            .sink
            .live
            .background
            .borrow()
            .iter()
            .all(|(_, live)| live.input_enabled())
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    rig.app.auto_reconnect_tunnel(&rig.ctx);
    assert_eq!(rig.script.probes.load(Ordering::SeqCst), 2);
    assert_eq!(rig.script.connections.load(Ordering::SeqCst), connections);
    assert_eq!(rig.app.terminal.row_text(0), history);
    assert_eq!(rig.app.active_tab, Some(second));
    assert_eq!(
        rig.app
            .sink
            .live
            .connection
            .borrow()
            .as_ref()
            .unwrap()
            .tunnel_target(),
        session
    );
    rig.app.activate_tab(first);
    assert!(rig.app.live_view().unwrap().input_enabled);
    assert_eq!(rig.script.probes.load(Ordering::SeqCst), 2);
}

#[test]
fn cancelled_resume_probe_cannot_validate_or_retire_replacement() {
    let mut rig = rig(None, true, true);
    let profile = rig.profile.clone();
    rig.app
        .start_tunnel_connect(&rig.ctx, &profile, None, false);
    wait_until(&mut rig, |app| app.active_tunnel().is_some());
    rig.script.probe_mode.store(2, Ordering::SeqCst);
    rig.app.suspend_sessions();
    pump(&mut rig);
    let deadline = Instant::now() + Duration::from_secs(5);
    while rig.script.probes.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    // A new suspension invalidates the old result even if completion races cancellation.
    rig.app.suspend_sessions();
    rig.script.probe_release.notify_one();
    assert!(!rig.app.live_view().unwrap().input_enabled);
    rig.script.probe_mode.store(0, Ordering::SeqCst);
    wait_probe(&mut rig, |app| app.live_view().unwrap().input_enabled);
    rig.app.suspend_sessions();
    pump(&mut rig);
    rig.app
        .sink
        .live
        .connection
        .borrow_mut()
        .as_mut()
        .unwrap()
        .stop("Disconnected. Reconnect to continue.");
    std::thread::sleep(Duration::from_millis(20));
    pump(&mut rig);
    assert!(rig.app.live_view().unwrap().can_reset);
    rig.app.auto_reconnect_tunnel(&rig.ctx);
    assert!(rig.app.tunnel.connect.is_none());
    // Drop the old attachment/check while a new attachment is being installed.
    rig.app
        .start_tunnel_connect(&rig.ctx, &profile, None, false);
    wait_until(&mut rig, |app| app.tunnel.connect.is_none());
    assert!(rig.app.live_view().unwrap().input_enabled);
}

#[test]
fn blackholed_resume_probe_is_bounded_and_disabled_recovery_stays_disconnected() {
    let mut rig = rig(None, true, true);
    let profile = rig.profile.clone();
    rig.app
        .start_tunnel_connect(&rig.ctx, &profile, None, false);
    wait_until(&mut rig, |app| app.active_tunnel().is_some());
    // Turn recovery off in the persisted profile, as the real Settings path does.
    let profiles = rig.app.profiles.as_mut().unwrap();
    let spec = profiles.tunnel_spec(&profile).unwrap();
    profiles
        .save_tunnel(
            Some(&profile),
            &spec.name,
            spec.account,
            &spec.tunnel,
            spec.session,
            crate::profiles::TunnelBehaviour {
                take_over: true,
                auto_reconnect: false,
            },
        )
        .unwrap();
    rig.script.probe_mode.store(2, Ordering::SeqCst);
    rig.app.suspend_sessions();
    rig.app
        .sink
        .live
        .connection
        .borrow_mut()
        .as_mut()
        .unwrap()
        .send(b"never-replay-this");
    let started = Instant::now();
    wait_probe(&mut rig, |app| app.live_view().unwrap().can_reset);
    assert!(started.elapsed() < Duration::from_secs(11));
    rig.app.auto_reconnect_tunnel(&rig.ctx);
    assert!(rig.app.tunnel.connect.is_none());
    assert_eq!(rig.script.clients.lock().unwrap().len(), 1);
    assert!(rig.script.input.lock().unwrap().is_empty());
}

#[test]
fn closing_a_tab_cancels_its_probe_and_inflight_recovery_without_reopening() {
    let mut rig = rig(None, true, true);
    let profile = rig.profile.clone();
    rig.app
        .start_tunnel_connect(&rig.ctx, &profile, None, false);
    wait_until(&mut rig, |app| app.active_tunnel().is_some());
    let tab = rig.app.active_tab.unwrap();
    rig.script.probe_mode.store(2, Ordering::SeqCst);
    rig.app.sink.live.suspend(); // real native callback, without a UI frame
    assert!(!rig.app.live_view().unwrap().input_enabled);
    pump(&mut rig);
    rig.app.close_tab(tab);
    rig.script.probe_release.notify_one();
    std::thread::sleep(Duration::from_millis(20));
    rig.app.poll_tunnels(&rig.ctx);
    assert!(rig.app.tabs.iter().all(|t| t.id != tab));
    assert!(rig.app.live_view().is_none());

    rig.script.probe_mode.store(0, Ordering::SeqCst);
    rig.app
        .start_tunnel_connect(&rig.ctx, &profile, None, false);
    wait_until(&mut rig, |app| app.active_tunnel().is_some());
    let tab = rig.app.active_tab.unwrap();
    // Closing a recovering tab cancels its describing/attach job as well.
    rig.app
        .start_tunnel_connect(&rig.ctx, &profile, Some(tab), true);
    rig.app.close_tab(tab);
    assert!(rig.app.tunnel.connect.is_none());
    std::thread::sleep(Duration::from_millis(20));
    rig.app.poll_tunnels(&rig.ctx);
    assert!(rig.app.tabs.iter().all(|t| t.id != tab));
}

#[test]
fn displacement_during_a_resume_check_never_triggers_automatic_takeover() {
    let mut rig = rig(None, true, true);
    let profile = rig.profile.clone();
    rig.app
        .start_tunnel_connect(&rig.ctx, &profile, None, false);
    wait_until(&mut rig, |app| app.active_tunnel().is_some());
    let dials = rig.script.connections.load(Ordering::SeqCst);
    rig.script.probe_mode.store(2, Ordering::SeqCst);
    rig.app.suspend_sessions();
    pump(&mut rig);
    rig.script.displace.notify_one();
    wait_probe(&mut rig, |app| app.live_view().unwrap().can_reset);
    rig.script.probe_release.notify_one();
    rig.app.auto_reconnect_tunnel(&rig.ctx);
    assert!(rig
        .app
        .live_view()
        .unwrap()
        .status
        .starts_with("Taken over"));
    assert!(rig.app.tunnel.connect.is_none());
    assert_eq!(rig.script.connections.load(Ordering::SeqCst), dials);
}

#[test]
fn shell_exit_during_resume_is_never_recreated() {
    let mut rig = rig(None, true, true);
    let profile = rig.profile.clone();
    rig.app
        .start_tunnel_connect(&rig.ctx, &profile, None, false);
    wait_until(&mut rig, |app| app.active_tunnel().is_some());
    rig.script.probe_mode.store(2, Ordering::SeqCst);
    rig.app.suspend_sessions();
    pump(&mut rig);
    rig.script.exit.notify_one();
    wait_probe(&mut rig, |app| app.live_view().unwrap().can_reset);
    rig.script.probe_release.notify_one();
    rig.app.auto_reconnect_tunnel(&rig.ctx);
    assert_eq!(
        rig.app.live_view().unwrap().status,
        "The remote shell exited."
    );
    assert!(rig.app.tunnel.connect.is_none());
    assert_eq!(rig.script.clients.lock().unwrap().len(), 1);
}

#[test]
fn lifecycle_resume_checks_transport_while_lock_keeps_content_hidden() {
    use festerm_ios_lock::{
        AppLock, Availability, BiometryKind, FakeLocker, LockSettings, ManualClock,
    };
    let mut rig = rig(None, true, true);
    let profile = rig.profile.clone();
    rig.app
        .start_tunnel_connect(&rig.ctx, &profile, None, false);
    wait_until(&mut rig, |app| app.active_tunnel().is_some());
    let locker = FakeLocker::new(Availability::Available(BiometryKind::FaceId));
    let lock = AppLock::new(None, locker, Arc::new(ManualClock::new()), || {});
    lock.apply_settings(LockSettings {
        enabled: true,
        ..Default::default()
    })
    .unwrap();
    lock.lock_now();
    rig.app.lock = Some(app_lock::LockHost::new(lock.clone(), None));
    let dials = rig.script.connections.load(Ordering::SeqCst);
    rig.app.lifecycle.set(Lifecycle {
        active: true,
        ..Default::default()
    });
    rig.app.sink.live.suspend(); // MobileHost::suspended, without a frame
    let mut lifecycle = rig.app.lifecycle.get();
    lifecycle.suspend();
    lifecycle.resume();
    rig.app.lifecycle.set(lifecycle);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        rig.ctx
            .run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Text("must-stay-local".into())],
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(390.0, 844.0),
                    )),
                    ..Default::default()
                },
                |ui| rig.app.show(ui),
            )
            .drop_without_applying_deltas();
        assert!(
            !lock.should_draw_content(),
            "transport health must not unlock content"
        );
        if rig.app.live_view().unwrap().input_enabled {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(rig.script.probes.load(Ordering::SeqCst), 1);
    assert_eq!(rig.script.connections.load(Ordering::SeqCst), dials);
    assert!(rig.script.input.lock().unwrap().is_empty());
}
