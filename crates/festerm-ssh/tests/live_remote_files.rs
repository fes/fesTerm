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

struct TestServer {
    channels: Vec<russh::Channel<russh::server::Msg>>,
    subsystem_started: mpsc::Sender<()>,
    reject_subsystem: bool,
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
        if self.reject_subsystem {
            session.channel_failure(channel)?;
        } else {
            // Accept the channel but deliberately never answer SFTP initialization.
            session.channel_success(channel)?;
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

fn start_server(reject_subsystem: bool) -> (OwnedServer, u16, mpsc::Receiver<()>) {
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
                    reject_subsystem,
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
        let (_server, port, subsystem_started) = start_server(reject_subsystem);
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
