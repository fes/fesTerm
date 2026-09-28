#![cfg(unix)]

use std::{
    io,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
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
}

impl Fixture {
    fn start() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from("/tmp").join(format!(
            "fs-rssh-{}-{stamp:x}-{sequence:x}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
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
        let server = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
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
                                let handler = Handler { root: server_root.clone(), channel: None };
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
            let result = forward_child(channel, &mut child).await;
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
) -> io::Result<()> {
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    let mut error = child.stderr.take().unwrap();
    let mut out = [0; 8192];
    let mut err = [0; 4096];
    let mut output_open = true;
    let mut error_open = true;
    let mut exec_seen = false;
    let deadline = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(deadline);
    while output_open || error_open {
        tokio::select! {
            _ = &mut deadline => return Err(io::Error::new(io::ErrorKind::TimedOut, "fixture bridge deadline")),
            message = channel.wait() => match message {
                Some(russh::ChannelMsg::Exec { .. }) if !exec_seen => exec_seen = true,
                Some(russh::ChannelMsg::Data { data }) => input.write_all(&data).await?,
                Some(russh::ChannelMsg::Eof | russh::ChannelMsg::Close) | None => return Ok(()),
                other => panic!("unexpected binary SSH client message: {other:?}"),
            },
            count = output.read(&mut out), if output_open => {
                let count = count?;
                output_open = count != 0;
                if output_open {
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
