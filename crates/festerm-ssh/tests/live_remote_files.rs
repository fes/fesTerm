use std::{
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};

use festerm_session::{
    Session, SessionEvent, SessionLifecycle, SessionTryReceiveError, TerminalSize,
};
use festerm_ssh::{
    HostIdentity, HostTrustDecision, RemoteFileReadError, SshAuthentication, SshConnectionProfile,
    SshSession,
};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
};

#[derive(Clone, Copy)]
enum SubsystemBehavior {
    Stall,
    Reject,
    ServeFiles,
}

const FIXTURE_PATH: &str = "/notes-\u{03bb}.md";
const FIXTURE_CONTENT: &[u8] = b"# remote document\n\nExact snapshot bytes.\n";

struct MemoryFiles;

impl russh_sftp::server::Handler for MemoryFiles {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        assert_eq!(path, ".");
        Ok(Name {
            id,
            files: vec![File::dummy("/")],
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let permissions = match path.as_str() {
            FIXTURE_PATH => 0o100644,
            "/" => 0o040755,
            _ => return Err(StatusCode::NoSuchFile),
        };
        Ok(Attrs {
            id,
            attrs: FileAttributes {
                size: Some(FIXTURE_CONTENT.len() as u64),
                permissions: Some(permissions),
                ..Default::default()
            },
        })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.stat(id, path).await
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        flags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        assert_eq!(filename, FIXTURE_PATH);
        assert_eq!(flags.bits(), OpenFlags::READ.bits());
        Ok(Handle {
            id,
            handle: filename,
        })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        assert_eq!(handle, FIXTURE_PATH);
        let start = usize::try_from(offset).unwrap();
        if start >= FIXTURE_CONTENT.len() {
            return Err(StatusCode::Eof);
        }
        let end = (start + len as usize).min(FIXTURE_CONTENT.len());
        Ok(Data {
            id,
            data: FIXTURE_CONTENT[start..end].to_vec(),
        })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        assert_eq!(handle, FIXTURE_PATH);
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }
}

struct TestServer {
    channels: Vec<russh::Channel<russh::server::Msg>>,
    subsystem_started: mpsc::Sender<()>,
    behavior: SubsystemBehavior,
    subsystems: tokio::task::JoinSet<()>,
}

impl russh::server::Handler for TestServer {
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
        self.channels.push(channel);
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: russh::ChannelId,
        _term: &str,
        _cols: u32,
        _rows: u32,
        _width: u32,
        _height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: russh::ChannelId,
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: russh::ChannelId,
        name: &str,
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        assert_eq!(name, "sftp");
        match self.behavior {
            SubsystemBehavior::Reject => session.channel_failure(channel)?,
            SubsystemBehavior::Stall => session.channel_success(channel)?,
            SubsystemBehavior::ServeFiles => {
                session.channel_success(channel)?;
                let index = self
                    .channels
                    .iter()
                    .position(|item| item.id() == channel)
                    .unwrap();
                let stream = self.channels.remove(index).into_stream();
                self.subsystems
                    .spawn(russh_sftp::server::run(stream, MemoryFiles));
            }
        }
        self.subsystem_started.send(()).unwrap();
        Ok(())
    }

    async fn data(
        &mut self,
        channel: russh::ChannelId,
        data: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        // Binary SFTP requests stay unanswered; only the shell challenge echoes.
        if data == b"shell-remains-responsive" {
            session.data(channel, data.to_vec())?;
        }
        Ok(())
    }
}

struct OwnedServer {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    join: Option<thread::JoinHandle<()>>,
}

impl Drop for OwnedServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}

fn start_server(behavior: SubsystemBehavior) -> (OwnedServer, u16, mpsc::Receiver<()>) {
    let (port_sender, port_receiver) = mpsc::channel();
    let (subsystem_started, subsystem_receiver) = mpsc::channel();
    let (stop, stop_receiver) = tokio::sync::oneshot::channel();
    let join = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            port_sender.send(listener.local_addr().unwrap().port()).unwrap();
            let config = russh::server::Config {
                keys: vec![russh::keys::PrivateKey::random(
                    &mut russh::keys::key::safe_rng(), russh::keys::Algorithm::Ed25519,
                ).unwrap()],
                auth_rejection_time: Duration::ZERO,
                ..Default::default()
            };
            let serve = async {
                let (stream, _) = listener.accept().await.unwrap();
                let session = russh::server::run_stream(Arc::new(config), stream, TestServer {
                    channels: Vec::new(),
                    subsystem_started,
                    behavior,
                    subsystems: tokio::task::JoinSet::new(),
                }).await.unwrap();
                let _ = session.await;
            };
            tokio::select! {
                _ = serve => {}
                _ = stop_receiver => {}
                _ = tokio::time::sleep(Duration::from_secs(15)) => panic!("fixture server deadline"),
            }
        });
    });
    let port = port_receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    (
        OwnedServer {
            stop: Some(stop),
            join: Some(join),
        },
        port,
        subsystem_receiver,
    )
}

#[test]
fn live_remote_file_read_keeps_password_accept_once_shell_responsive() {
    for reject_subsystem in [false, true] {
        let (_server, port, subsystem_started) = start_server(if reject_subsystem {
            SubsystemBehavior::Reject
        } else {
            SubsystemBehavior::Stall
        });
        let session = connect_session(port);
        let requestor = session.remote_file_requestor();
        assert!(requestor.verified_host_key_fingerprint().is_some());
        let read = thread::spawn(move || requestor.read_remote_file_snapshot("/fixture.txt", 4096));
        subsystem_started
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        let second_read = if !reject_subsystem {
            let second_requestor = session.remote_file_requestor();
            let second = thread::spawn(move || {
                second_requestor.read_remote_file_snapshot("/second.txt", 4096)
            });
            subsystem_started
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            assert_eq!(
                session
                    .remote_file_requestor()
                    .read_remote_file_snapshot("/third.txt", 4096),
                Err(RemoteFileReadError::QueueFull),
            );
            Some(second)
        } else {
            None
        };
        session.try_send_input(b"shell-remains-responsive").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut output = Vec::new();
        while !output
            .windows(b"shell-remains-responsive".len())
            .any(|w| w == b"shell-remains-responsive")
        {
            match session.try_recv_event() {
                Ok(SessionEvent::Output(bytes)) => output.extend(bytes),
                Ok(_) | Err(SessionTryReceiveError::Empty) => {
                    assert!(Instant::now() < deadline, "SFTP read blocked the shell");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(SessionTryReceiveError::Closed) => panic!("shell closed while reading file"),
            }
        }
        session.shutdown(Duration::from_secs(3)).unwrap();
        assert!(matches!(
            read.join().unwrap(),
            Err(RemoteFileReadError::Closed | RemoteFileReadError::Sftp(_))
        ));
        if let Some(second) = second_read {
            assert!(matches!(
                second.join().unwrap(),
                Err(RemoteFileReadError::Closed)
            ));
        }
    }
}

fn connect_session(port: u16) -> SshSession {
    let profile = SshConnectionProfile::new(
        HostIdentity::new("127.0.0.1", port).unwrap(),
        "fixture",
        SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
        TerminalSize::new(80, 24).unwrap(),
    )
    .unwrap();
    let session =
        SshSession::start(profile, SshAuthentication::password("fixture-password")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match session.try_recv_event() {
            Ok(SessionEvent::HostKeyVerification(prompt)) => {
                session
                    .host_key_decision_resolver()
                    .resolve(&prompt, HostTrustDecision::AcceptOnce)
                    .unwrap();
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) => break,
            Ok(SessionEvent::Error(error)) => panic!("fixture connection failed: {error}"),
            Ok(_) | Err(SessionTryReceiveError::Empty) => {
                assert!(Instant::now() < deadline, "session never started");
                thread::sleep(Duration::from_millis(5));
            }
            Err(SessionTryReceiveError::Closed) => panic!("fixture connection closed"),
        }
    }
    session
}

#[test]
fn live_remote_file_read_returns_exact_bytes_and_honest_bounds() {
    let (_server, port, _subsystems) = start_server(SubsystemBehavior::ServeFiles);
    let session = connect_session(port);
    let requestor = session.remote_file_requestor();
    let snapshot = requestor
        .read_remote_file_snapshot(FIXTURE_PATH, FIXTURE_CONTENT.len())
        .unwrap();
    assert_eq!(snapshot.bytes(), FIXTURE_CONTENT);
    assert!(matches!(
        requestor.read_remote_file_snapshot(FIXTURE_PATH, FIXTURE_CONTENT.len() - 1),
        Err(RemoteFileReadError::Sftp(
            festerm_ssh::SftpSessionError::RemoteFileTooLarge { .. }
        ))
    ));
    assert!(matches!(
        requestor.read_remote_file_snapshot("/", 1024),
        Err(RemoteFileReadError::NotFile { .. })
    ));
    assert!(matches!(
        requestor.read_remote_file_snapshot("/missing.txt", 1024),
        Err(RemoteFileReadError::Missing { .. })
    ));
    assert_eq!(session.lifecycle(), SessionLifecycle::Running);
    session.shutdown(Duration::from_secs(3)).unwrap();
}
