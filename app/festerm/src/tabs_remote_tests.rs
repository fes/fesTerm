use super::*;
use std::{
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use festerm_sessiond::{encode_recovery_snapshot, encode_server_output_frame, RemoteSshEndpoint};

type CapturedFrames = Arc<Mutex<Vec<(u8, Vec<u8>)>>>;

struct RemoteFixture {
    endpoint: RemoteSshEndpoint,
    frames: CapturedFrames,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl RemoteFixture {
    fn start() -> Self {
        let (stop, mut stopping) = tokio::sync::oneshot::channel();
        let (ready, receiver) = mpsc::sync_channel(1);
        let frames = Arc::new(Mutex::new(Vec::new()));
        let server_frames = Arc::clone(&frames);
        let worker = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let key = russh::keys::PrivateKey::random(
                        &mut russh::keys::key::safe_rng(), russh::keys::Algorithm::Ed25519,
                    ).unwrap();
                    let fingerprint = key.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string();
                    ready.send((listener.local_addr().unwrap().port(), fingerprint)).unwrap();
                    let config = Arc::new(russh::server::Config {
                        keys: vec![key], auth_rejection_time: Duration::ZERO, ..Default::default()
                    });
                    loop {
                        tokio::select! {
                            _ = &mut stopping => break,
                            connection = listener.accept() => {
                                let (stream, _) = connection.unwrap();
                                let config = Arc::clone(&config);
                                let handler = Handler { channel: None, frames: Arc::clone(&server_frames) };
                                tokio::spawn(async move {
                                    let connection = russh::server::run_stream(config, stream, handler).await.unwrap();
                                    let _ = connection.await;
                                });
                            }
                        }
                    }
                });
        });
        let (port, fingerprint) = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        let profile = SshConnectionProfile::new(
            festerm_ssh::HostIdentity::new("127.0.0.1", port).unwrap(),
            "fixture",
            "xterm-256color",
            TerminalSize::new(80, 24).unwrap(),
        )
        .unwrap()
        .with_local_bind_address(Some("127.0.0.1".parse().unwrap()))
        .unwrap();
        Self {
            endpoint: RemoteSshEndpoint::new(profile, fingerprint, "festerm-sessiond").unwrap(),
            frames,
            stop: Some(stop),
            worker: Some(worker),
        }
    }
}

impl Drop for RemoteFixture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

struct Handler {
    channel: Option<russh::Channel<russh::server::Msg>>,
    frames: CapturedFrames,
}

impl russh::server::Handler for Handler {
    type Error = russh::Error;

    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        Ok(if user == "fixture" && password == "fixture-password" {
            russh::server::Auth::Accept
        } else {
            russh::server::Auth::reject()
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: russh::Channel<russh::server::Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.channel = Some(channel);
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        id: russh::ChannelId,
        command: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        let command = std::str::from_utf8(command).unwrap();
        session.channel_success(id)?;
        let mut channel = self.channel.take().unwrap();
        if command.contains(" discover") {
            let inventory = serde_json::json!({
                "schema_version": 1, "package_version": "fixture",
                "inventory": {"record_count": 1, "serialized_bytes": 0, "status_counts": {"available": 1}},
                "sessions": [{
                    "name": "existing", "validated_name": true, "pid": 123,
                    "created_at_unix_ms": 456, "attached": false, "status": "available",
                    "daemon_protocol": {"version": 2, "supported": true},
                    "recovery_snapshot_schema": {"version": 2, "supported": true}
                }]
            });
            channel
                .data(&serde_json::to_vec(&inventory).unwrap()[..])
                .await?;
            channel.exit_status(0).await?;
            channel.eof().await?;
            channel.close().await?;
        } else {
            assert!(command.contains(" bridge "));
            assert!(command.contains("--pid 123"));
            assert!(command.contains("--generation 456"));
            assert!(command.contains("--allow-takeover"));
            let terminal = Terminal::new(Dimensions::new(62, 17).unwrap()).unwrap();
            channel
                .data(&encode_recovery_snapshot(&terminal).unwrap()[..])
                .await?;
            let frames = Arc::clone(&self.frames);
            tokio::spawn(async move {
                let mut pending = Vec::new();
                while let Some(message) = channel.wait().await {
                    match message {
                        russh::ChannelMsg::Data { data } => pending.extend_from_slice(&data),
                        russh::ChannelMsg::Eof | russh::ChannelMsg::Close => break,
                        _ => continue,
                    }
                    while pending.len() >= 9 {
                        assert_eq!(&pending[..4], b"FSD1");
                        let kind = pending[4];
                        let length = u32::from_be_bytes(pending[5..9].try_into().unwrap()) as usize;
                        assert!(length <= 64 * 1024);
                        if pending.len() < 9 + length {
                            break;
                        }
                        let payload = pending[9..9 + length].to_vec();
                        pending.drain(..9 + length);
                        {
                            let mut seen = frames.lock().unwrap();
                            if seen.is_empty() {
                                assert_eq!(kind, 4, "snapshot adoption must precede all input");
                            }
                            seen.push((kind, payload));
                        }
                        if kind == 1 {
                            channel
                                .data(&encode_server_output_frame(b"\x1b]2;remote-output\x07")[..])
                                .await
                                .unwrap();
                        }
                    }
                }
            });
        }
        Ok(())
    }
}

#[test]
fn remote_picker_target_opens_normal_terminal_adopts_snapshot_and_stays_ephemeral() {
    let fixture = RemoteFixture::start();
    let inventory = fixture
        .endpoint
        .discover(SshAuthentication::password("fixture-password"))
        .unwrap();
    let target = inventory.select("existing").unwrap();
    let context = egui::Context::default();
    let mut state = AppState::for_test_with_configuration(Configuration::empty());
    state.dispatch(AppCommand::OpenRemoteSessions, &context);
    let picker = state.active();
    state.attach_remote_session(
        crate::remote_sessions::RemoteAttachRequest {
            target,
            authentication: SshAuthentication::password("fixture-password"),
        },
        &context,
    );
    assert_ne!(state.active(), picker);
    assert!(state
        .capture_window_workspace_tabs(&mut 0)
        .unwrap()
        .0
        .is_empty());
    let TabContent::Session(tab) = &mut state.active_tab_mut().content else {
        panic!("terminal tab");
    };
    assert!(tab.is_remote_persistent());
    assert!(!tab.is_ssh_session());
    assert!(tab.profile_identifier.is_none());
    assert_eq!(tab.label, "existing · fixture@127.0.0.1");
    assert!(tab
        .controller
        .session()
        .unwrap()
        .try_send_input(b"too-early")
        .is_err());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !tab.adopt_recovered_terminal() {
        assert!(Instant::now() < deadline, "snapshot adoption deadline");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(tab.terminal.dimensions(), Dimensions::new(62, 17).unwrap());
    tab.controller
        .session()
        .unwrap()
        .try_send_input(b"hello\n")
        .unwrap();
    while !fixture
        .frames
        .lock()
        .unwrap()
        .iter()
        .any(|(kind, data)| *kind == 1 && data == b"hello\n")
    {
        assert!(Instant::now() < deadline, "input forwarding deadline");
        tab.pump_session_events();
        thread::sleep(Duration::from_millis(5));
    }
    while tab.terminal.title() != "remote-output" {
        assert!(Instant::now() < deadline, "terminal output deadline");
        tab.pump_session_events();
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fixture.frames.lock().unwrap()[0].0, 4);
    assert!(tab.close_requires_confirmation());
    let attached = state.active();
    state.close(attached);
    assert_eq!(state.active(), picker);
}
