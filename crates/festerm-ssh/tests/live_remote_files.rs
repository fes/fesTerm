use std::{
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};

use festerm_session::{
    Session, SessionEvent, SessionLifecycle, SessionTryReceiveError, ShutdownResult, TerminalSize,
};
use festerm_ssh::{
    connect_gui_sftp_session, GuiSftpSessionConnectOutcome, HostIdentity, HostTrustDecision,
    RemoteFileReadError, SftpPath, SftpSessionError, SftpTerminalSession, SftpTransferEvent,
    SftpTransferManager, SftpTransferRequest, SshAuthentication, SshConnectionProfile, SshSession,
};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
};
use tokio::io::AsyncWriteExt;

#[derive(Clone, Copy)]
enum SubsystemBehavior {
    Stall,
    Reject,
    ServeFiles,
    StallFileReads,
    ControlledFileReads,
}

#[derive(Clone, Copy)]
enum ShellBehavior {
    Quiet,
    ContinuousOutput,
}

const FIXTURE_PATH: &str = "/notes-\u{03bb}.md";
const FIXTURE_PARENT_PATH: &str = "/link/../notes-\u{03bb}.md";
const FIXTURE_CONTENT: &[u8] = b"# remote document\n\nExact snapshot bytes.\n";
const CONTROLLED_CONTENT_BYTES: usize = 256 * 1024;

struct MemoryFiles {
    stall_reads: bool,
    read_started: mpsc::Sender<()>,
    content: Vec<u8>,
    read_resume: Option<Arc<tokio::sync::Notify>>,
}

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
            FIXTURE_PATH | FIXTURE_PARENT_PATH => 0o100644,
            "/" => 0o040755,
            _ => return Err(StatusCode::NoSuchFile),
        };
        Ok(Attrs {
            id,
            attrs: FileAttributes {
                size: Some(self.content.len() as u64),
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
        assert!(matches!(
            filename.as_str(),
            FIXTURE_PATH | FIXTURE_PARENT_PATH
        ));
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
        assert!(matches!(
            handle.as_str(),
            FIXTURE_PATH | FIXTURE_PARENT_PATH
        ));
        if self.stall_reads {
            self.read_started.send(()).unwrap();
            return std::future::pending().await;
        }
        let start = usize::try_from(offset).unwrap();
        if start >= self.content.len() {
            return Err(StatusCode::Eof);
        }
        if start > 0 {
            if let Some(resume) = &self.read_resume {
                resume.notified().await;
            }
        }
        let end = (start + len as usize).min(self.content.len());
        Ok(Data {
            id,
            data: self.content[start..end].to_vec(),
        })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        assert!(matches!(
            handle.as_str(),
            FIXTURE_PATH | FIXTURE_PARENT_PATH
        ));
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
    shell_behavior: ShellBehavior,
    read_resume: Arc<tokio::sync::Notify>,
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
        if matches!(self.shell_behavior, ShellBehavior::ContinuousOutput) {
            let index = self
                .channels
                .iter()
                .position(|item| item.id() == channel)
                .unwrap();
            let mut stream = self.channels.remove(index).into_stream();
            self.subsystems.spawn(async move {
                let chunk = vec![b'x'; 16 * 1024];
                while stream.write_all(&chunk).await.is_ok() {}
            });
        }
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
            SubsystemBehavior::ServeFiles
            | SubsystemBehavior::StallFileReads
            | SubsystemBehavior::ControlledFileReads => {
                session.channel_success(channel)?;
                let index = self
                    .channels
                    .iter()
                    .position(|item| item.id() == channel)
                    .unwrap();
                let stream = self.channels.remove(index).into_stream();
                self.subsystems.spawn(russh_sftp::server::run(
                    stream,
                    MemoryFiles {
                        stall_reads: matches!(self.behavior, SubsystemBehavior::StallFileReads),
                        read_started: self.subsystem_started.clone(),
                        content: if matches!(self.behavior, SubsystemBehavior::ControlledFileReads)
                        {
                            vec![b't'; CONTROLLED_CONTENT_BYTES]
                        } else {
                            FIXTURE_CONTENT.to_vec()
                        },
                        read_resume: matches!(
                            self.behavior,
                            SubsystemBehavior::ControlledFileReads
                        )
                        .then(|| Arc::clone(&self.read_resume)),
                    },
                ));
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
    connection_closed: mpsc::Receiver<()>,
    read_resume: Arc<tokio::sync::Notify>,
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

fn start_server(
    behavior: SubsystemBehavior,
    shell_behavior: ShellBehavior,
) -> (OwnedServer, u16, mpsc::Receiver<()>) {
    let (port_sender, port_receiver) = mpsc::channel();
    let (subsystem_started, subsystem_receiver) = mpsc::channel();
    let (connection_closed, closed_receiver) = mpsc::channel();
    let read_resume = Arc::new(tokio::sync::Notify::new());
    let server_read_resume = Arc::clone(&read_resume);
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
                    shell_behavior,
                    read_resume: server_read_resume,
                    subsystems: tokio::task::JoinSet::new(),
                }).await.unwrap();
                let _ = session.await;
                let _ = connection_closed.send(());
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
            connection_closed: closed_receiver,
            read_resume,
        },
        port,
        subsystem_receiver,
    )
}

fn fixture_profile(port: u16) -> SshConnectionProfile {
    SshConnectionProfile::new(
        HostIdentity::new("127.0.0.1", port).unwrap(),
        "fixture",
        SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
        TerminalSize::new(80, 24).unwrap(),
    )
    .unwrap()
}

#[test]
fn cancelling_gui_connect_wait_closes_the_dedicated_transport_thread() {
    let (server, port, subsystem_started) =
        start_server(SubsystemBehavior::Stall, ShellBehavior::Quiet);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let outcome = connect_gui_sftp_session(
            fixture_profile(port),
            SshAuthentication::password("fixture-password"),
            None,
            None,
        )
        .await
        .unwrap();
        let GuiSftpSessionConnectOutcome::NeedsHostKeyDecision {
            prompt,
            resolver,
            completion,
        } = outcome
        else {
            panic!("fixture must require explicit host trust");
        };
        resolver
            .resolve(&prompt, HostTrustDecision::AcceptOnce)
            .unwrap();
        let mut waiting = Box::pin(completion.wait());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut waiting)
                .await
                .is_err()
        );
        subsystem_started
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        drop(waiting);
        server
            .connection_closed
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
    });
}

#[test]
fn text_sftp_shutdown_interrupts_a_stalled_download_and_reports_its_partial_file() {
    let (server, port, activity) =
        start_server(SubsystemBehavior::StallFileReads, ShellBehavior::Quiet);
    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("test-artifacts")
        .join(format!("sftp-download-owner-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let session = SftpTerminalSession::start(
        fixture_profile(port),
        SshAuthentication::password("fixture-password"),
        Some(directory.clone()),
        None,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match session.try_recv_event() {
            Ok(SessionEvent::HostKeyVerification(prompt)) => session
                .host_key_decision_resolver()
                .resolve(&prompt, HostTrustDecision::AcceptOnce)
                .unwrap(),
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) => break,
            Ok(SessionEvent::Error(error)) => panic!("fixture startup failed: {error}"),
            Ok(_) | Err(SessionTryReceiveError::Empty) => {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(5));
            }
            Err(SessionTryReceiveError::Closed) => panic!("fixture closed during startup"),
        }
    }
    activity.recv_timeout(Duration::from_secs(3)).unwrap();
    session
        .try_send_input(format!("get \"{FIXTURE_PATH}\" cancelled.bin\r").as_bytes())
        .unwrap();
    activity.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(directory.join("cancelled.bin").exists());
    assert_eq!(
        session.shutdown(Duration::from_secs(5)).unwrap(),
        ShutdownResult::Stopped
    );
    assert!(directory.join("cancelled.bin").exists());
    let mut cleanup_reported = false;
    while let Ok(event) = session.try_recv_event() {
        if let SessionEvent::Error(error) = event {
            cleanup_reported |= error.message().contains("file was not removed")
                && error.message().contains("cancelled.bin");
        }
    }
    assert!(
        cleanup_reported,
        "preserved partial output must be reported"
    );
    server
        .connection_closed
        .recv_timeout(Duration::from_secs(3))
        .unwrap();
    std::fs::remove_file(directory.join("cancelled.bin")).unwrap();
    std::fs::remove_dir(&directory).unwrap();
}

#[test]
fn live_gui_copy_cancellation_reports_partial_output_without_losing_cancelled_state() {
    let (server, port, _activity) =
        start_server(SubsystemBehavior::ControlledFileReads, ShellBehavior::Quiet);
    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-artifacts")
        .join(format!("sftp-gui-copy-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let destination = directory.join("output.bin");
    let partial = directory.join("output.bin.festerm-part");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let outcome = connect_gui_sftp_session(
            fixture_profile(port),
            SshAuthentication::password("fixture-password"),
            Some(directory.clone()),
            None,
        )
        .await
        .unwrap();
        let GuiSftpSessionConnectOutcome::NeedsHostKeyDecision {
            prompt,
            resolver,
            completion,
        } = outcome
        else {
            panic!("fixture must require explicit host trust");
        };
        resolver
            .resolve(&prompt, HostTrustDecision::AcceptOnce)
            .unwrap();
        let session = completion.wait().await.unwrap();
        let mut transfers = SftpTransferManager::new(session);
        let batch = transfers
            .enqueue_batch(vec![SftpTransferRequest::new(
                SftpPath::remote(FIXTURE_PATH),
                SftpPath::local(destination.clone()),
            )
            .unwrap()])
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let event = transfers.recv_event().await.unwrap();
                if matches!(event, SftpTransferEvent::ItemProgress { .. }) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        transfers.cancel_transfer(batch.transfer_ids[0]).unwrap();
        server.read_resume.notify_one();
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut reported = false;
            let mut cancelled = false;
            loop {
                match transfers.recv_event().await.unwrap() {
                    SftpTransferEvent::CleanupIncomplete {
                        error: SftpSessionError::PartialFileOwnershipUnconfirmed { path },
                    } => {
                        assert!(path.contains("output.bin.festerm-part"));
                        reported = true;
                    }
                    SftpTransferEvent::ItemCancelled { .. } => {
                        assert!(reported);
                        cancelled = true;
                    }
                    SftpTransferEvent::ItemFailed { reason, .. } => {
                        panic!("cancelled live copy must not fail: {reason}");
                    }
                    SftpTransferEvent::ItemCompleted { .. } => {
                        panic!("controlled copy must not finish before cancellation");
                    }
                    SftpTransferEvent::BatchFinished { .. } => {
                        assert!(reported && cancelled);
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        transfers.shutdown().await.unwrap();
    });
    server
        .connection_closed
        .recv_timeout(Duration::from_secs(3))
        .unwrap();
    assert!(!destination.exists());
    let retained = std::fs::read(&partial).unwrap();
    assert!(!retained.is_empty() && retained.len() < CONTROLLED_CONTENT_BYTES);
    assert!(retained.iter().all(|byte| *byte == b't'));
    std::fs::remove_file(&partial).unwrap();
    std::fs::remove_dir(&directory).unwrap();
}

#[test]
fn live_remote_file_read_keeps_password_accept_once_shell_responsive() {
    for reject_subsystem in [false, true] {
        let (_server, port, subsystem_started) = start_server(
            if reject_subsystem {
                SubsystemBehavior::Reject
            } else {
                SubsystemBehavior::Stall
            },
            ShellBehavior::Quiet,
        );
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

#[test]
fn live_shell_control_and_shutdown_progress_with_busy_output_and_a_full_event_queue() {
    let (_server, port, _subsystems) =
        start_server(SubsystemBehavior::Reject, ShellBehavior::ContinuousOutput);
    let session = connect_session(port);
    let deadline = Instant::now() + Duration::from_secs(3);
    let before = loop {
        let metrics = session.metrics();
        if metrics.backpressure_count > 0
            && metrics.event_queue_depth == metrics.event_queue_capacity
        {
            break metrics;
        }
        assert!(
            Instant::now() < deadline,
            "fixture output never filled the frontend queue"
        );
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(before.event_queue_depth, before.event_queue_capacity);
    session.try_send_input(b"shell-remains-responsive").unwrap();
    session
        .try_resize(TerminalSize::new(100, 40).unwrap())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let metrics = session.metrics();
        if metrics.input_bytes > before.input_bytes && metrics.resize_count > before.resize_count {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "busy output starved queued input or resize"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        session.shutdown(Duration::from_secs(3)).unwrap(),
        ShutdownResult::Stopped
    );
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
    let (_server, port, _subsystems) =
        start_server(SubsystemBehavior::ServeFiles, ShellBehavior::Quiet);
    let session = connect_session(port);
    let requestor = session.remote_file_requestor();
    let snapshot = requestor
        .read_remote_file_snapshot(FIXTURE_PATH, FIXTURE_CONTENT.len())
        .unwrap();
    assert_eq!(snapshot.bytes(), FIXTURE_CONTENT);
    let literal = requestor
        .read_remote_file_snapshot(FIXTURE_PARENT_PATH, FIXTURE_CONTENT.len())
        .unwrap();
    assert_eq!(literal.metadata().path.display(), FIXTURE_PARENT_PATH);
    assert_eq!(literal.bytes(), FIXTURE_CONTENT);
    assert!(matches!(
        requestor.read_remote_file_snapshot("relative.txt", 1024),
        Err(RemoteFileReadError::InvalidRequest),
    ));
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
