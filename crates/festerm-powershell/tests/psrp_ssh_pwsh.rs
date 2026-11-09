use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use festerm_powershell::{
    PipelineEvent, PowerShellCommandFailureKind, PowerShellCommandMessage, PowerShellOptions,
    PowerShellSessionError, PowerShellSshSession,
};
use festerm_session::TerminalSize;
use festerm_ssh::{
    HostIdentity, SshAuthentication, SshConnectionProfile, SshRawExecOptions, SshRawExecSession,
};
use psrp_rs::{
    BlockingIoPsrpTransport, ErrorRecord, FromPsObject, InformationRecord, Pipeline,
    ProgressRecord, PsValue, PsrpTransport, RunspacePool, TraceRecord, WarningRecord,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const OPT_IN_ENV: &str = "FESTERM_PSRP_SSH_PWSH_INTEROP";
const PWSH_PATH_ENV: &str = "FESTERM_PSRP_SSH_PWSH_PATH";

#[test]
fn opt_in_real_pwsh_ssh_subsystem_runs_structured_psrp() {
    if std::env::var(OPT_IN_ENV).as_deref() != Ok("1") {
        eprintln!("skipping real PowerShell SSH PSRP fixture; set {OPT_IN_ENV}=1 to opt in");
        return;
    }
    let pwsh = pwsh_path();
    assert!(
        std::process::Command::new(&pwsh)
            .arg("-NoLogo")
            .arg("-NoProfile")
            .arg("-Command")
            .arg("$PSVersionTable.PSVersion.ToString()")
            .output()
            .is_ok_and(|output| output.status.success()),
        "opted into real PSRP SSH fixture but PowerShell executable was unavailable: {}",
        pwsh.display()
    );

    let (server, port) = start_pwsh_subsystem_server(pwsh);
    let profile = SshConnectionProfile::new(
        HostIdentity::new("127.0.0.1", port).expect("loopback identity"),
        "alice",
        SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
        TerminalSize::new(80, 24).expect("valid fixture terminal size"),
    )
    .expect("valid fixture profile");
    let ssh_options = SshRawExecOptions::new()
        .with_known_host_fingerprint(server.fingerprint.clone())
        .expect("fixture host key fingerprint")
        .with_io_timeouts(Duration::from_millis(100), Duration::from_millis(100))
        .expect("nonzero timeouts");
    let session = PowerShellSshSession::connect_ssh_subsystem(
        profile.clone(),
        SshAuthentication::password("test-password"),
        ssh_options.clone(),
        PowerShellOptions {
            connect_timeout: Duration::from_secs(30),
            operation_timeout: Duration::from_secs(15),
            cancel_drain_timeout: Duration::from_secs(5),
            shutdown_timeout: Duration::from_secs(5),
            max_events_per_command: 128,
            ..PowerShellOptions::default()
        },
    )
    .expect("connect native SSH PowerShell subsystem");

    let marker = format!("ssh-psrp-ok-{}", std::process::id());
    let set = format!("$global:FestermSshInteropMarker = '{marker}'; 'set-ok'");
    assert_eq!(run_output_strings(&session, &set), ["set-ok"]);
    let outputs = output_strings(&run_events(
        &session,
        "$global:FestermSshInteropMarker; $PSVersionTable.PSEdition",
    ));
    assert!(
        outputs.contains(&marker),
        "expected marker output, got {outputs:?}"
    );
    assert!(
        outputs
            .iter()
            .any(|value| value == "Core" || value == "Desktop"),
        "expected real PowerShell edition output, got {outputs:?}"
    );
    let expected = patterned_input();
    let fragmented_script = format!("'{expected}'");
    assert!(
        run_output_strings(&session, &fragmented_script) == [expected],
        "fragmented script output did not match"
    );

    assert_real_streams(&session);
    assert_host_call_rejection_is_bounded_and_session_survives(&session, &marker);
    assert_cancellation_is_bounded_and_runspace_survives(&session, &marker);

    session.close().expect("close SSH PSRP session");
    assert!(matches!(
        session.start_script("'should-not-run'"),
        Err(PowerShellSessionError::Closed)
    ));
    server.assert_children_exited(1);
    assert_large_pipeline_input_roundtrips(profile, ssh_options);
    server.assert_children_exited(1);
    drop(server);
}

fn assert_large_pipeline_input_roundtrips(
    profile: SshConnectionProfile,
    ssh_options: SshRawExecOptions,
) {
    let input = patterned_input();
    let stream = SshRawExecSession::connect_subsystem(
        profile,
        SshAuthentication::password("test-password"),
        PowerShellSshSession::DEFAULT_SUBSYSTEM_NAME,
        ssh_options,
    )
    .expect("connect pipeline-input fixture");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("pipeline-input fixture runtime");
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), async {
            let transport =
                BlockingIoPsrpTransport::powershell_ssh_with_ack_timeout(stream, Duration::from_secs(15));
            let (rpid, creation) =
                RunspacePool::<BlockingIoPsrpTransport<SshRawExecSession>>::build_creation_fragments(1, 1)
                    .expect("build pipeline-input runspace");
            transport.send_fragment(&creation).await.expect("send runspace creation");
            let mut pool = RunspacePool::open_from_transport(transport, rpid, 1, 1)
                .await
                .expect("open pipeline-input runspace");
            let mut handle = Pipeline::new("$input | ForEach-Object { $_ }")
                .with_input(true)
                .start(&mut pool)
                .await
                .expect("start pipeline-input command");
            handle
                .write_input(PsValue::String(input.clone()))
                .await
                .expect("send fragmented pipeline input");
            let result = handle.collect().await.expect("collect pipeline-input result");
            assert!(
                result.output == vec![PsValue::String(input)],
                "fragmented pipeline input did not round-trip"
            );
            pool.close().await.expect("close pipeline-input runspace");
        })
        .await
        .expect("pipeline-input fixture exceeded deadline");
    });
}

fn patterned_input() -> String {
    (0..20_000).map(|value| format!("{value:05x}")).collect()
}

fn assert_real_streams(session: &PowerShellSshSession) {
    let events = run_events(
        session,
        r#"
$VerbosePreference = 'Continue'
$DebugPreference = 'Continue'
$InformationPreference = 'Continue'
Write-Output 'out-stream'
Write-Error 'error-stream' -ErrorAction Continue
Write-Warning 'warning-stream'
Write-Verbose 'verbose-stream'
Write-Debug 'debug-stream'
Write-Progress -Activity 'progress-stream' -Status 'half-way' -PercentComplete 50
Write-Information 'information-stream' -Tags 'festerm'
'done-stream'
"#,
    );

    let outputs = output_strings(&events);
    assert!(outputs.contains(&"out-stream".to_string()), "{outputs:?}");
    assert!(outputs.contains(&"done-stream".to_string()), "{outputs:?}");
    assert!(events.iter().any(|event| match event {
        PipelineEvent::Error(value) => ErrorRecord::from_ps_object(value)
            .and_then(|record| record.exception.and_then(|exception| exception.message))
            .is_some_and(|message| message.contains("error-stream")),
        _ => false,
    }));
    assert!(events.iter().any(|event| match event {
        PipelineEvent::Warning(value) => WarningRecord::from_ps_object(value)
            .is_some_and(|record| record.message.contains("warning-stream")),
        _ => false,
    }));
    assert!(events.iter().any(|event| match event {
        PipelineEvent::Verbose(value) => TraceRecord::from_ps_object(value)
            .is_some_and(|record| record.message.contains("verbose-stream")),
        _ => false,
    }));
    assert!(events.iter().any(|event| match event {
        PipelineEvent::Debug(value) => TraceRecord::from_ps_object(value)
            .is_some_and(|record| record.message.contains("debug-stream")),
        _ => false,
    }));
    assert!(events.iter().any(|event| match event {
        PipelineEvent::Information(value) => InformationRecord::from_ps_object(value)
            .and_then(|record| record.message_data)
            .and_then(|value| value.as_str().map(str::to_owned))
            .is_some_and(|message| message.contains("information-stream")),
        _ => false,
    }));
    assert!(events.iter().any(|event| match event {
        PipelineEvent::Progress(value) => ProgressRecord::from_ps_object(value)
            .and_then(|record| record.activity)
            .is_some_and(|activity| activity.contains("progress-stream")),
        _ => false,
    }));
}

fn assert_host_call_rejection_is_bounded_and_session_survives(
    session: &PowerShellSshSession,
    marker: &str,
) {
    let mut command = session
        .start_script("Read-Host -Prompt 'festerm unsupported prompt'; 'unexpected-success'")
        .expect("start unsupported host-call script");
    let failure = collect_failure(&mut command);
    assert_ne!(
        failure,
        PowerShellCommandFailureKind::TimedOut,
        "unsupported host calls must be rejected through PipelineHostResponse, not left waiting"
    );
    assert!(
        matches!(
            failure,
            PowerShellCommandFailureKind::PipelineFailed
                | PowerShellCommandFailureKind::ProtocolFailure
        ),
        "unexpected host-call rejection result: {failure:?}"
    );
    assert_eq!(
        output_strings(&run_events(
            session,
            "$global:FestermSshInteropMarker; 'after-host-call'",
        )),
        [marker.to_string(), "after-host-call".to_string()]
    );
}

fn assert_cancellation_is_bounded_and_runspace_survives(
    session: &PowerShellSshSession,
    marker: &str,
) {
    let mut command = session
        .start_script("'cancel-ready'; Start-Sleep -Seconds 30; 'unexpected-success'")
        .expect("start cancellation script");
    wait_for_output(&mut command, "cancel-ready");
    command.cancel();
    let failure = collect_failure(&mut command);
    assert!(
        matches!(
            failure,
            PowerShellCommandFailureKind::Cancelled | PowerShellCommandFailureKind::Stopped
        ),
        "unexpected cancellation result: {failure:?}"
    );
    assert_eq!(
        output_strings(&run_events(
            session,
            "$global:FestermSshInteropMarker; 'after-cancel'",
        )),
        [marker.to_string(), "after-cancel".to_string()]
    );
}

fn pwsh_path() -> PathBuf {
    std::env::var_os(PWSH_PATH_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("pwsh"))
}

#[track_caller]
fn run_events(session: &PowerShellSshSession, script: &str) -> Vec<PipelineEvent> {
    let mut command = session.start_script(script).expect("start PSRP script");
    collect_events(&mut command)
}

#[track_caller]
fn run_output_strings(session: &PowerShellSshSession, script: &str) -> Vec<String> {
    output_strings(&run_events(session, script))
}

fn output_strings(events: &[PipelineEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            PipelineEvent::Output(value) => value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_i32().map(|value| value.to_string())),
            _ => None,
        })
        .collect()
}

#[track_caller]
fn collect_events(command: &mut festerm_powershell::PowerShellCommand) -> Vec<PipelineEvent> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut output = Vec::new();
    loop {
        assert!(
            Instant::now() < deadline,
            "PSRP SSH command exceeded fixture deadline"
        );
        match command.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(PowerShellCommandMessage::Event(event))) => output.push(event),
            Ok(Some(PowerShellCommandMessage::Completed { .. })) => return output,
            Ok(Some(PowerShellCommandMessage::Failed(failure))) => {
                panic!(
                    "PSRP SSH command failed after {} event(s): {failure:?}",
                    output.len()
                )
            }
            Ok(None) => panic!("PSRP SSH command ended without completion"),
            Err(PowerShellSessionError::TimedOut) => continue,
            Err(error) => panic!("PSRP SSH receive failed: {error:?}"),
        }
    }
}

fn wait_for_output(command: &mut festerm_powershell::PowerShellCommand, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "wait for output timed out");
        match command.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(PowerShellCommandMessage::Event(PipelineEvent::Output(value))))
                if value.as_str() == Some(expected) =>
            {
                return;
            }
            Ok(Some(PowerShellCommandMessage::Event(_))) => {}
            Ok(Some(message)) => panic!("unexpected message while waiting for output: {message:?}"),
            Ok(None) => panic!("command ended before expected output"),
            Err(PowerShellSessionError::TimedOut) => continue,
            Err(error) => panic!("PSRP SSH receive failed: {error:?}"),
        }
    }
}

fn collect_failure(
    command: &mut festerm_powershell::PowerShellCommand,
) -> PowerShellCommandFailureKind {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "wait for failure timed out");
        match command.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(PowerShellCommandMessage::Event(_))) => {}
            Ok(Some(PowerShellCommandMessage::Failed(failure))) => return failure.kind,
            Ok(Some(message)) => {
                panic!("unexpected message while waiting for failure: {message:?}")
            }
            Ok(None) => panic!("command ended before expected failure"),
            Err(PowerShellSessionError::TimedOut) => continue,
            Err(error) => panic!("PSRP SSH receive failed: {error:?}"),
        }
    }
}

struct PwshSubsystemServer {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    join: Option<thread::JoinHandle<()>>,
    fingerprint: String,
    bridge_exits: mpsc::Receiver<std::process::ExitStatus>,
}

impl PwshSubsystemServer {
    fn assert_children_exited(&self, count: usize) {
        for _ in 0..count {
            let status = self
                .bridge_exits
                .recv_timeout(Duration::from_secs(5))
                .expect("PowerShell fixture child did not exit after session close");
            // pwsh -sshs reports closed stdin as 0x0fa0; Unix keeps the low byte.
            let eof_status = if cfg!(windows) { 0x0fa0 } else { 0xa0 };
            assert!(
                status.success() || status.code() == Some(eof_status),
                "PowerShell fixture child failed: {status}"
            );
        }
    }
}

impl Drop for PwshSubsystemServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(join) = self.join.take() {
            join.join().expect("fixture server thread joins");
        }
    }
}

fn start_pwsh_subsystem_server(pwsh: PathBuf) -> (PwshSubsystemServer, u16) {
    let (port_sender, port_receiver) = mpsc::channel();
    let (fingerprint_sender, fingerprint_receiver) = mpsc::channel();
    let (bridge_exits, bridge_exit_receiver) = mpsc::channel();
    let (stop, mut stop_receiver) = tokio::sync::oneshot::channel();
    let join = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("fixture runtime");
        runtime.block_on(async move {
            let listener =
                tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
                    .await
                    .expect("bind loopback fixture SSH server");
            port_sender
                .send(listener.local_addr().expect("fixture local addr").port())
                .expect("send fixture port");
            let key = russh::keys::PrivateKey::random(
                &mut russh::keys::key::safe_rng(),
                russh::keys::Algorithm::Ed25519,
            )
            .expect("generate fixture SSH host key");
            fingerprint_sender
                .send(
                    key.public_key()
                        .fingerprint(russh::keys::HashAlg::Sha256)
                        .to_string(),
                )
                .expect("send fixture fingerprint");
            let config = Arc::new(russh::server::Config {
                keys: vec![key],
                auth_rejection_time: Duration::ZERO,
                ..Default::default()
            });
            let mut sessions = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut stop_receiver => break,
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.expect("accept fixture SSH client");
                        let config = Arc::clone(&config);
                        let pwsh = pwsh.clone();
                        let bridge_exits = bridge_exits.clone();
                        sessions.spawn(async move {
                            let session = russh::server::run_stream(
                                config,
                                stream,
                                PwshSubsystemHandler { channel: None, pwsh, bridge_exits },
                            )
                            .await
                            .expect("run fixture SSH session");
                            let _ = session.await;
                        });
                    }
                }
            }
            sessions.abort_all();
        });
    });
    let port = port_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("fixture server reports port");
    let fingerprint = fingerprint_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("fixture server reports fingerprint");
    (
        PwshSubsystemServer {
            stop: Some(stop),
            join: Some(join),
            fingerprint,
            bridge_exits: bridge_exit_receiver,
        },
        port,
    )
}

struct PwshSubsystemHandler {
    channel: Option<russh::Channel<russh::server::Msg>>,
    pwsh: PathBuf,
    bridge_exits: mpsc::Sender<std::process::ExitStatus>,
}

impl russh::server::Handler for PwshSubsystemHandler {
    type Error = russh::Error;

    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        Ok(if user == "alice" && password == "test-password" {
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
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: russh::ChannelId,
        _command: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: russh::ChannelId,
        name: &str,
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        if name != PowerShellSshSession::DEFAULT_SUBSYSTEM_NAME {
            session.channel_failure(channel_id)?;
            return Ok(());
        }
        session.channel_success(channel_id)?;
        let channel = self.channel.take().expect("accepted session channel");
        let pwsh = self.pwsh.clone();
        let bridge_exits = self.bridge_exits.clone();
        tokio::spawn(async move {
            if let Err(error) = bridge_channel_to_pwsh(channel, pwsh, bridge_exits).await {
                eprintln!("PowerShell SSH fixture bridge failed: {error}");
            }
        });
        Ok(())
    }
}

async fn bridge_channel_to_pwsh(
    mut channel: russh::Channel<russh::server::Msg>,
    pwsh: PathBuf,
    bridge_exits: mpsc::Sender<std::process::ExitStatus>,
) -> std::io::Result<()> {
    let mut child = tokio::process::Command::new(pwsh)
        .arg("-sshs")
        .arg("-NoLogo")
        .arg("-NoProfile")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut child_stdin = child.stdin.take();
    assert!(child_stdin.is_some(), "piped pwsh stdin");
    let mut child_stdout = child.stdout.take().expect("piped pwsh stdout");
    let mut child_stderr = child.stderr.take().expect("piped pwsh stderr");
    let mut stdout = [0u8; 16 * 1024];
    let mut stderr = [0u8; 4 * 1024];
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut channel_open = true;

    loop {
        if !stdout_open && !stderr_open {
            break;
        }
        tokio::select! {
            read = child_stdout.read(&mut stdout), if stdout_open => {
                let read = read?;
                if read == 0 {
                    stdout_open = false;
                } else if channel_open {
                    channel.data(&stdout[..read]).await.map_err(io_other)?;
                }
            }
            read = child_stderr.read(&mut stderr), if stderr_open => {
                let read = read?;
                if read == 0 {
                    stderr_open = false;
                } else if channel_open {
                    channel.extended_data(1, &stderr[..read]).await.map_err(io_other)?;
                }
            }
            message = channel.wait(), if channel_open => {
                match message {
                    Some(russh::ChannelMsg::Data { data }) if child_stdin.is_some() => {
                        child_stdin.as_mut().expect("open pwsh stdin").write_all(&data).await?;
                    }
                    Some(russh::ChannelMsg::Eof) => {
                        drop(child_stdin.take());
                    }
                    Some(russh::ChannelMsg::Close) | None => {
                        drop(child_stdin.take());
                        channel_open = false;
                    }
                    _ => {}
                }
            }
        }
    }

    let status = child.wait().await?;
    let _ = bridge_exits.send(status);
    if channel_open {
        channel
            .exit_status(status.code().unwrap_or(1) as u32)
            .await
            .map_err(io_other)?;
        channel.eof().await.map_err(io_other)?;
        channel.close().await.map_err(io_other)?;
    }
    Ok(())
}

fn io_other(error: russh::Error) -> std::io::Error {
    std::io::Error::other(error)
}
