#![cfg(unix)]

use std::{
    io,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use festerm_core::Terminal;
use festerm_session::{
    noop_session_event_notifier, Session, SessionEvent, SessionLifecycle, SessionTryReceiveError,
    TerminalSize,
};
use festerm_sessiond::{PersistentSession, RemoteSshEndpoint};
use festerm_ssh::{HostIdentity, SshAuthentication, SshConnectionProfile};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    server: Option<thread::JoinHandle<()>>,
    endpoint: RemoteSshEndpoint,
    replacement_fingerprint: String,
    rotated: Arc<AtomicBool>,
    authentications: Arc<AtomicUsize>,
    bridge_executions: Arc<AtomicUsize>,
    snapshot_corruption: Arc<AtomicUsize>,
    corrupt_adoptions: Arc<AtomicUsize>,
    corrupt_deliveries: Arc<AtomicUsize>,
    corrupt_disconnects: Arc<AtomicUsize>,
}

impl Fixture {
    fn start() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = if let Some(base) = std::env::var_os("FESTERM_SESSIOND_TEST_RUNTIME_ROOT") {
            PathBuf::from(base).join(format!("r{:x}-{sequence:x}", std::process::id()))
        } else {
            PathBuf::from("/tmp").join(format!(
                "fs-rssh-{}-{stamp:x}-{sequence:x}",
                std::process::id()
            ))
        };
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let output = helper_command(
            &root,
            &[
                "start", "--name", "existing", "--shell", "/bin/sh", "--arg", "-i",
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let (stop, mut stopping) = tokio::sync::oneshot::channel();
        let (ready, receiver) = mpsc::sync_channel(1);
        let server_root = root.clone();
        let rotated = Arc::new(AtomicBool::new(false));
        let authentications = Arc::new(AtomicUsize::new(0));
        let bridge_executions = Arc::new(AtomicUsize::new(0));
        let snapshot_corruption = Arc::new(AtomicUsize::new(0));
        let corrupt_adoptions = Arc::new(AtomicUsize::new(0));
        let corrupt_deliveries = Arc::new(AtomicUsize::new(0));
        let corrupt_disconnects = Arc::new(AtomicUsize::new(0));
        let server_rotated = Arc::clone(&rotated);
        let server_authentications = Arc::clone(&authentications);
        let server_bridges = Arc::clone(&bridge_executions);
        let server_corruption = Arc::clone(&snapshot_corruption);
        let server_adoptions = Arc::clone(&corrupt_adoptions);
        let server_deliveries = Arc::clone(&corrupt_deliveries);
        let server_disconnects = Arc::clone(&corrupt_disconnects);
        let server = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
                .block_on(async move {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let key = russh::keys::PrivateKey::random(
                        &mut russh::keys::key::safe_rng(), russh::keys::Algorithm::Ed25519,
                    ).unwrap();
                    let fingerprint = key.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string();
                    let replacement = russh::keys::PrivateKey::random(
                        &mut russh::keys::key::safe_rng(), russh::keys::Algorithm::Ed25519,
                    ).unwrap();
                    let replacement_fingerprint = replacement.public_key()
                        .fingerprint(russh::keys::HashAlg::Sha256).to_string();
                    ready.send((listener.local_addr().unwrap().port(), fingerprint, replacement_fingerprint)).unwrap();
                    let config = Arc::new(russh::server::Config {
                        keys: vec![key], auth_rejection_time: Duration::ZERO, ..Default::default()
                    });
                    let replacement_config = Arc::new(russh::server::Config {
                        keys: vec![replacement], auth_rejection_time: Duration::ZERO, ..Default::default()
                    });
                    loop {
                        tokio::select! {
                            _ = &mut stopping => break,
                            connection = listener.accept() => {
                                let (stream, _) = connection.unwrap();
                                let config = if server_rotated.load(Ordering::Acquire) {
                                    Arc::clone(&replacement_config)
                                } else {
                                    Arc::clone(&config)
                                };
                                let handler = Handler {
                                    root: server_root.clone(),
                                    channel: None,
                                    authentications: Arc::clone(&server_authentications),
                                    bridge_executions: Arc::clone(&server_bridges),
                                    snapshot_corruption: Arc::clone(&server_corruption),
                                    corrupt_adoptions: Arc::clone(&server_adoptions),
                                    corrupt_deliveries: Arc::clone(&server_deliveries),
                                    corrupt_disconnects: Arc::clone(&server_disconnects),
                                };
                                tokio::spawn(async move {
                                    let connection = russh::server::run_stream(config, stream, handler).await.unwrap();
                                    let _ = connection.await;
                                });
                            }
                        }
                    }
                });
        });
        let (port, fingerprint, replacement_fingerprint) =
            receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        let profile = SshConnectionProfile::new(
            HostIdentity::new("127.0.0.1", port).unwrap(),
            "fixture",
            "xterm-256color",
            TerminalSize::new(80, 24).unwrap(),
        )
        .unwrap()
        .with_local_bind_address(Some("127.0.0.1".parse().unwrap()))
        .unwrap();
        Self {
            root,
            stop: Some(stop),
            server: Some(server),
            endpoint: RemoteSshEndpoint::new(profile, fingerprint, "festerm-sessiond").unwrap(),
            replacement_fingerprint,
            rotated,
            authentications,
            bridge_executions,
            snapshot_corruption,
            corrupt_adoptions,
            corrupt_deliveries,
            corrupt_disconnects,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(server) = self.server.take() {
            server.join().unwrap();
        }
        let result = helper_command(&self.root, &["kill", "--name", "existing"]);
        if !result.status.success() {
            eprintln!(
                "fixture daemon cleanup failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            if !thread::panicking() {
                panic!("fixture daemon cleanup failed");
            }
            return;
        }
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn helper_command(root: &std::path::Path, args: &[&str]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_festerm-sessiond"))
        .env("XDG_STATE_HOME", root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("fixture helper deadline exceeded");
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

struct Handler {
    root: PathBuf,
    channel: Option<russh::Channel<russh::server::Msg>>,
    authentications: Arc<AtomicUsize>,
    bridge_executions: Arc<AtomicUsize>,
    snapshot_corruption: Arc<AtomicUsize>,
    corrupt_adoptions: Arc<AtomicUsize>,
    corrupt_deliveries: Arc<AtomicUsize>,
    corrupt_disconnects: Arc<AtomicUsize>,
}

impl russh::server::Handler for Handler {
    type Error = russh::Error;

    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        self.authentications.fetch_add(1, Ordering::AcqRel);
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
        assert!(self.channel.is_none());
        self.channel = Some(channel);
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        _channel: russh::ChannelId,
        _term: &str,
        _cols: u32,
        _rows: u32,
        _width: u32,
        _height: u32,
        _modes: &[(russh::Pty, u32)],
        _session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        panic!("remote binary attachment must not request a PTY");
    }

    async fn exec_request(
        &mut self,
        id: russh::ChannelId,
        command: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        let command = std::str::from_utf8(command).unwrap();
        let mut args = command.split_ascii_whitespace();
        assert_eq!(args.next(), Some("festerm-sessiond"));
        let args: Vec<_> = args.map(str::to_owned).collect();
        assert!(matches!(
            args.first().map(String::as_str),
            Some("discover" | "bridge")
        ));
        let corruption = if args.first().is_some_and(|arg| arg == "bridge") {
            self.bridge_executions.fetch_add(1, Ordering::AcqRel);
            self.snapshot_corruption.swap(0, Ordering::AcqRel)
        } else {
            0
        };
        let corrupt_adoptions = Arc::clone(&self.corrupt_adoptions);
        let corrupt_deliveries = Arc::clone(&self.corrupt_deliveries);
        let corrupt_disconnects = Arc::clone(&self.corrupt_disconnects);
        let channel = self.channel.take().unwrap();
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_festerm-sessiond"))
            .env("XDG_STATE_HOME", &self.root)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        session.channel_success(id)?;
        tokio::spawn(async move {
            let result = forward_child(
                channel,
                &mut child,
                corruption,
                corrupt_adoptions,
                corrupt_deliveries,
                corrupt_disconnects,
            )
            .await;
            if let Err(error) = result {
                // A dropped client intentionally interrupts its bridge.
                assert!(
                    matches!(
                        error.kind(),
                        io::ErrorKind::BrokenPipe
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::ConnectionAborted
                    ),
                    "{error}"
                );
            }
        });
        Ok(())
    }
}

async fn forward_child(
    mut channel: russh::Channel<russh::server::Msg>,
    child: &mut tokio::process::Child,
    corruption: usize,
    corrupt_adoptions: Arc<AtomicUsize>,
    corrupt_deliveries: Arc<AtomicUsize>,
    corrupt_disconnects: Arc<AtomicUsize>,
) -> io::Result<()> {
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    let mut error = child.stderr.take().unwrap();
    let mut out = [0; 8192];
    let mut err = [0; 4096];
    let mut output_open = true;
    let mut error_open = true;
    let mut exec_seen = false;
    let mut corrupted = false;
    let mut pending = Vec::new();
    let deadline = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(deadline);
    while output_open || error_open || (corruption != 0 && corrupted) {
        tokio::select! {
            _ = &mut deadline => return Err(io::Error::new(io::ErrorKind::TimedOut, "fixture bridge deadline")),
            message = channel.wait() => match message {
                Some(russh::ChannelMsg::Exec { .. }) if !exec_seen => exec_seen = true,
                Some(russh::ChannelMsg::Data { data }) => {
                    if corruption != 0 {
                        pending.extend_from_slice(&data);
                        while pending.len() >= 9 {
                            assert_eq!(&pending[..4], b"FSD1");
                            let length = u32::from_be_bytes(pending[5..9].try_into().unwrap()) as usize;
                            assert!(length <= 64 * 1024);
                            if pending.len() < 9 + length {
                                break;
                            }
                            if pending[4] == 4 {
                                corrupt_adoptions.fetch_add(1, Ordering::AcqRel);
                            }
                            pending.drain(..9 + length);
                        }
                    }
                    input.write_all(&data).await?
                },
                Some(russh::ChannelMsg::Eof | russh::ChannelMsg::Close) | None => {
                    if corruption != 0 && corrupted {
                        assert!(pending.is_empty(), "client closed with a partial protocol frame");
                        corrupt_disconnects.fetch_add(1, Ordering::AcqRel);
                    }
                    return Ok(())
                },
                other => panic!("unexpected binary SSH client message: {other:?}"),
            },
            count = output.read(&mut out), if output_open => {
                let count = count?;
                output_open = count != 0;
                if output_open {
                    if corruption != 0 && !corrupted {
                        corrupted = true;
                        let mut invalid = b"FSD2".to_vec();
                        let announced = match corruption {
                            1 => 2u64,
                            2 => 16u64,
                            3 => 1u64,
                            _ => unreachable!("fixture corruption mode"),
                        };
                        invalid.extend_from_slice(&announced.to_be_bytes());
                        if corruption == 3 {
                            invalid.push(2);
                        } else {
                            invalid.extend_from_slice(&[1, 0xff]);
                        }
                        channel.data(&invalid[..]).await.map_err(|_| io::ErrorKind::BrokenPipe)?;
                        corrupt_deliveries.fetch_add(1, Ordering::AcqRel);
                        output_open = false;
                        if corruption == 2 {
                            channel.exit_status(0).await.map_err(|_| io::ErrorKind::BrokenPipe)?;
                            channel.eof().await.map_err(|_| io::ErrorKind::BrokenPipe)?;
                            channel.close().await.map_err(|_| io::ErrorKind::BrokenPipe)?;
                        }
                        continue;
                    }
                    channel.data(&out[..count]).await.map_err(|_| io::ErrorKind::BrokenPipe)?;
                }
            },
            count = error.read(&mut err), if error_open => {
                let count = count?;
                error_open = count != 0;
                if error_open {
                    channel.extended_data(1, &err[..count]).await.map_err(|_| io::ErrorKind::BrokenPipe)?;
                }
            }
        }
    }
    let status = child.wait().await?;
    channel
        .exit_status(status.code().unwrap_or(1) as u32)
        .await
        .map_err(|_| io::ErrorKind::BrokenPipe)?;
    channel.eof().await.map_err(|_| io::ErrorKind::BrokenPipe)?;
    channel
        .close()
        .await
        .map_err(|_| io::ErrorKind::BrokenPipe)?;
    Ok(())
}

fn authentication() -> SshAuthentication {
    SshAuthentication::password("fixture-password")
}

fn adopt(session: &PersistentSession) -> Terminal {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(terminal) = session.take_recovered_terminal() {
            return terminal;
        }
        assert!(
            Instant::now() < deadline,
            "recovery timed out: {:?}",
            session.lifecycle()
        );
        assert!(
            !matches!(
                session.lifecycle(),
                SessionLifecycle::Failed(_) | SessionLifecycle::Disconnected(_)
            ),
            "{:?}",
            session.lifecycle()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_text(session: &PersistentSession, terminal: &mut Terminal, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match session.try_recv_event() {
            Ok(SessionEvent::Output(bytes)) => {
                terminal.ingest(&bytes);
            }
            Ok(SessionEvent::Lifecycle(
                SessionLifecycle::Failed(error) | SessionLifecycle::Disconnected(error),
            )) => panic!("{error}"),
            Ok(_) => {}
            Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(5)),
            Err(error) => panic!("{error:?}"),
        }
        if screen_text(terminal).contains(needle) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected remote text {needle:?}, screen={:?}",
            screen_text(terminal)
        );
    }
}

fn screen_text(terminal: &Terminal) -> String {
    let mut text = String::new();
    for row in 0..terminal.dimensions().rows() {
        for col in 0..terminal.dimensions().columns() {
            if let Some(cell) = terminal.cell_ref(col, row) {
                text.push_str(cell.text());
            }
        }
        text.push('\n');
    }
    text
}

#[test]
fn native_ssh_remote_sessiond_adopts_snapshot_and_reattaches_same_live_generation() {
    let fixture = Fixture::start();
    let inventory = fixture.endpoint.discover(authentication()).unwrap();
    let target = inventory.select("existing").unwrap();
    let session = target
        .attach_with_takeover(authentication(), noop_session_event_notifier())
        .unwrap();
    assert!(session.try_send_input(b"must-not-run\n").is_err());
    assert!(session
        .try_resize(TerminalSize::new(90, 30).unwrap())
        .is_err());
    let mut terminal = adopt(&session);
    assert!(session.recovery_protocol_is_authoritative());
    assert!(!session.reconnect_available());
    session.try_send_input(b"printf '\\162\\145\\155\\157\\164\\145\\055\\141\\164\\164\\141\\143\\150\\055\\157\\153\\n'\n").unwrap();
    wait_for_text(&session, &mut terminal, "remote-attach-ok");
    let resized = TerminalSize::new(90, 30).unwrap();
    session.try_resize(resized).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match session.try_recv_event() {
            Ok(SessionEvent::ResizeApplied(size)) => {
                assert_eq!(size, TerminalSize::with_pixels(90, 30, 0, 0).unwrap());
                break;
            }
            Ok(SessionEvent::Lifecycle(
                SessionLifecycle::Failed(error) | SessionLifecycle::Disconnected(error),
            )) => panic!("{error}"),
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(5)),
            Err(error) => panic!("{error:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "remote resize acknowledgement deadline"
        );
    }
    drop(session);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let inventory = fixture.endpoint.discover(authentication()).unwrap();
        let current = inventory
            .sessions()
            .iter()
            .find(|session| session.name == "existing")
            .unwrap();
        assert_eq!(current.pid, Some(target.pid()));
        assert_eq!(current.created_at_unix_ms, Some(target.generation()));
        if current.attached == Some(false) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "SSH drop must detach, not leave an attached daemon"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let second = target
        .attach_with_takeover(authentication(), noop_session_event_notifier())
        .unwrap();
    let mut restored = adopt(&second);
    assert_eq!(restored.dimensions().columns(), 90);
    assert_eq!(restored.dimensions().rows(), 30);
    assert!(screen_text(&restored).contains("remote-attach-ok"));
    second.try_send_input(b"printf '\\163\\145\\143\\157\\156\\144\\055\\141\\164\\164\\141\\143\\150\\055\\157\\153\\n'\n").unwrap();
    wait_for_text(&second, &mut restored, "second-attach-ok");
    drop(second);
}

#[test]
fn native_ssh_remote_sessiond_rejects_a_replaced_generation() {
    let fixture = Fixture::start();
    let target = fixture
        .endpoint
        .discover(authentication())
        .unwrap()
        .select("existing")
        .unwrap();
    assert!(
        helper_command(&fixture.root, &["kill", "--name", "existing"])
            .status
            .success()
    );
    assert!(helper_command(
        &fixture.root,
        &["start", "--name", "existing", "--shell", "/bin/sh", "--arg", "-i"]
    )
    .status
    .success());
    let current = fixture
        .endpoint
        .discover(authentication())
        .unwrap()
        .select("existing")
        .unwrap();
    assert_ne!(
        (target.pid(), target.generation()),
        (current.pid(), current.generation())
    );
    // Exec acceptance is not helper success; a rejected generation may arrive
    // asynchronously as the persistent worker's failure lifecycle.
    if let Ok(session) =
        target.attach_with_takeover(authentication(), noop_session_event_notifier())
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(session.take_recovered_terminal().is_none());
            if matches!(
                session.lifecycle(),
                SessionLifecycle::Failed(_) | SessionLifecycle::Disconnected(_)
            ) {
                break;
            }
            assert!(Instant::now() < deadline, "stale generation did not fail");
            thread::sleep(Duration::from_millis(10));
        }
    }
    let inventory = fixture.endpoint.discover(authentication()).unwrap();
    let entry = inventory
        .sessions()
        .iter()
        .find(|entry| entry.name == "existing")
        .unwrap();
    assert_eq!(entry.pid, Some(current.pid()));
    assert_eq!(entry.created_at_unix_ms, Some(current.generation()));
    assert_eq!(entry.attached, Some(false));
}

#[test]
fn native_ssh_remote_sessiond_rejects_an_untrusted_host_key() {
    let fixture = Fixture::start();
    let wrong_key = russh::keys::PrivateKey::random(
        &mut russh::keys::key::safe_rng(),
        russh::keys::Algorithm::Ed25519,
    )
    .unwrap();
    let endpoint = RemoteSshEndpoint::new(
        fixture.endpoint.profile().clone(),
        wrong_key
            .public_key()
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string(),
        "festerm-sessiond",
    )
    .unwrap();
    assert!(endpoint.discover(authentication()).is_err());
    let inventory = fixture.endpoint.discover(authentication()).unwrap();
    assert_eq!(inventory.sessions()[0].attached, Some(false));
}

#[test]
fn host_key_rotation_after_discovery_never_authenticates_or_displaces_the_live_session() {
    let fixture = Fixture::start();
    let target = fixture
        .endpoint
        .discover(authentication())
        .unwrap()
        .select("existing")
        .unwrap();
    let existing = target
        .attach_with_takeover(authentication(), noop_session_event_notifier())
        .unwrap();
    let mut terminal = adopt(&existing);
    existing
        .try_send_input(b"printf '\\157\\162\\151\\147\\151\\156\\141\\154\\055\\157\\153\\n'\n")
        .unwrap();
    wait_for_text(&existing, &mut terminal, "original-ok");
    let authentications = fixture.authentications.load(Ordering::Acquire);
    let bridges = fixture.bridge_executions.load(Ordering::Acquire);
    assert_eq!(bridges, 1);
    fixture.rotated.store(true, Ordering::Release);

    let candidate = target
        .attach_with_takeover(authentication(), noop_session_event_notifier())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(candidate.take_recovered_terminal().is_none());
        match candidate.lifecycle() {
            SessionLifecycle::Failed(error) | SessionLifecycle::Disconnected(error) => {
                assert!(
                    error.message().contains("SSH host key was rejected"),
                    "candidate failed for the wrong reason: {error}"
                );
                break;
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "changed host key did not fail");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        fixture.authentications.load(Ordering::Acquire),
        authentications
    );
    assert_eq!(fixture.bridge_executions.load(Ordering::Acquire), bridges);
    let rotated_endpoint = RemoteSshEndpoint::new(
        fixture.endpoint.profile().clone(),
        &fixture.replacement_fingerprint,
        "festerm-sessiond",
    )
    .unwrap();
    let inventory = rotated_endpoint.discover(authentication()).unwrap();
    assert_eq!(inventory.sessions()[0].attached, Some(true));
    existing
        .try_send_input(
            b"printf '\\163\\164\\151\\154\\154\\055\\141\\164\\164\\141\\143\\150\\145\\144\\n'\n",
        )
        .unwrap();
    wait_for_text(&existing, &mut terminal, "still-attached");
}

#[test]
fn invalid_remote_snapshots_never_adopt_or_displace_the_live_session() {
    let fixture = Fixture::start();
    let target = fixture
        .endpoint
        .discover(authentication())
        .unwrap()
        .select("existing")
        .unwrap();
    let existing = target
        .attach_with_takeover(authentication(), noop_session_event_notifier())
        .unwrap();
    let mut terminal = adopt(&existing);
    for (index, (corruption, command, expected)) in [
        (
            1,
            b"printf '\\141\\146\\164\\145\\162\\055\\142\\141\\144\\n'\n".as_slice(),
            "after-bad",
        ),
        (
            2,
            b"printf '\\141\\146\\164\\145\\162\\055\\163\\150\\157\\162\\164\\n'\n".as_slice(),
            "after-short",
        ),
        (
            3,
            b"printf '\\141\\146\\164\\145\\162\\055\\163\\145\\155\\141\\156\\164\\151\\143\\163\\n'\n".as_slice(),
            "after-semantics",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        fixture
            .snapshot_corruption
            .store(corruption, Ordering::Release);
        let candidate = target
            .attach_with_takeover(authentication(), noop_session_event_notifier())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(candidate.take_recovered_terminal().is_none());
            match candidate.lifecycle() {
                SessionLifecycle::Failed(error) | SessionLifecycle::Disconnected(error) => {
                    if corruption == 1 {
                        assert!(
                            error.message().contains("recovery snapshot is invalid"),
                            "malformed payload failed for the wrong reason: {error}"
                        );
                    } else if corruption == 3 {
                        assert!(
                            error.message().contains("unsupported terminal semantics"),
                            "unknown semantics failed for the wrong reason: {error}"
                        );
                    }
                    break;
                }
                _ => {}
            }
            assert!(Instant::now() < deadline, "invalid snapshot did not fail");
            thread::sleep(Duration::from_millis(5));
        }
        drop(candidate);
        while fixture.corrupt_disconnects.load(Ordering::Acquire) <= index {
            assert!(
                Instant::now() < deadline,
                "corrupt client did not disconnect"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            fixture.corrupt_deliveries.load(Ordering::Acquire),
            index + 1
        );
        assert_eq!(fixture.corrupt_adoptions.load(Ordering::Acquire), 0);
        existing.try_send_input(command).unwrap();
        wait_for_text(&existing, &mut terminal, expected);
    }
}
