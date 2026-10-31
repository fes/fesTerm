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
        DISCOVER, DISPLACED, HELLO, INPUT, OUTPUT, SNAPSHOT, SYNC, VERSION,
    },
    wire::{read_frame, write_frame},
};

use super::tunnel_link::ConnectStage;
use super::*;

struct Script {
    attached_by: Option<Displacer>,
    displace: tokio::sync::Notify,
    /// Frames the host sends right after the adoption handshake.
    after_adopt: Vec<(u8, Vec<u8>)>,
    input: Mutex<Vec<u8>>,
    connections: AtomicUsize,
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
    let _hello: Hello = serde_json::from_slice(&bytes).unwrap();
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
        after_adopt: after_adopt.to_vec(),
        input: Mutex::new(Vec::new()),
        connections: AtomicUsize::new(0),
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
        Arc::new(connector),
        CLIENT_ID.into(),
    )
    .unwrap();
    let profile = profiles
        .save_tunnel(None, "devbox", Some(account), "devbox.euw", None, take_over)
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
    rig.app.start_tunnel_connect(&rig.ctx, &id, None);
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
    rig.app.start_tunnel_connect(&rig.ctx, &id, None);
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
    refused.app.start_tunnel_connect(&refused.ctx, &id, None);
    wait_until(&mut refused, |app| app.notice.is_some());
    let notice = refused.app.notice.clone().unwrap();
    assert!(notice.contains("another device") && notice.contains("Take over"));
    assert!(refused.app.active_tunnel().is_none());

    let mut allowed = rig(Some(Displacer::Remote), true, true);
    let id = allowed.profile.clone();
    allowed.app.start_tunnel_connect(&allowed.ctx, &id, None);
    wait_until(&mut allowed, |app| app.active_tunnel().is_some());
}

#[test]
fn being_taken_over_ends_the_session_without_reattaching() {
    let mut rig = rig(None, true, true);
    let id = rig.profile.clone();
    rig.app.start_tunnel_connect(&rig.ctx, &id, None);
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
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        rig.script.connections.load(Ordering::SeqCst),
        attached_connections,
        "a displaced session is never reattached automatically"
    );
}

#[test]
fn recovery_sync_is_applied_and_the_host_alone_answers_terminal_queries() {
    let sync = serde_json::to_vec(&Sync::Bytes(b"synced text\r\n".to_vec())).unwrap();
    // A cursor-position query would normally be answered by the frontend.
    let frames = [(SYNC, sync), (OUTPUT, b"\x1b[6n".to_vec())];
    let mut rig = rig_with(None, true, true, &frames);
    let id = rig.profile.clone();
    rig.app.start_tunnel_connect(&rig.ctx, &id, None);
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
