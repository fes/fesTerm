use std::env;
use std::net::IpAddr;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use festerm_powershell::{
    PowerShellCommandFailureKind, PowerShellCommandMessage, PowerShellCredentials,
    PowerShellEndpoint, PowerShellOptions, PowerShellSession, PowerShellSessionError,
    PowerShellSessionStatus,
};
use festerm_secret_store::SecretBytes;
use psrp_rs::{
    ErrorRecord, FromPsObject, InformationRecord, PipelineEvent, PipelineState, ProgressRecord,
    PsValue, TraceRecord, WarningRecord,
};
use zeroize::Zeroizing;

const ENV_HOST: &str = "FESTERM_PSRP_INTEROP_HOST";
const ENV_PORT: &str = "FESTERM_PSRP_INTEROP_PORT";
const ENV_USER: &str = "FESTERM_PSRP_INTEROP_USER";
const ENV_DOMAIN: &str = "FESTERM_PSRP_INTEROP_DOMAIN";
const ENV_PASSWORD: &str = "FESTERM_PSRP_INTEROP_PASSWORD";
const ENV_CA_PEM: &str = "FESTERM_PSRP_INTEROP_CA_PEM";
const ENV_CA_PEM_PATH: &str = "FESTERM_PSRP_INTEROP_CA_PEM_PATH";
const ENV_PS7_CONFIG: &str = "FESTERM_PSRP_INTEROP_PS7_CONFIG";
const ENV_PS7_REQUIRED: &str = "FESTERM_PSRP_INTEROP_PS7_REQUIRED";

struct InteropConfig {
    host: String,
    port: u16,
    username: String,
    domain: Option<String>,
    password: Zeroizing<String>,
    ca_pem: String,
}

impl InteropConfig {
    fn from_env() -> Self {
        let host = required_env(ENV_HOST);
        let port = env::var(ENV_PORT)
            .unwrap_or_else(|_| "5986".to_string())
            .parse()
            .expect("FESTERM_PSRP_INTEROP_PORT must be a valid TCP port");
        assert_eq!(
            host, "127.0.0.1",
            "PSRP interop tests only accept the authorized loopback fixture endpoint"
        );
        let username = required_env(ENV_USER);
        let domain = env::var(ENV_DOMAIN)
            .ok()
            .filter(|value| !value.trim().is_empty());
        let password = Zeroizing::new(required_env(ENV_PASSWORD));
        let ca_pem = match env::var(ENV_CA_PEM) {
            Ok(value) if !value.trim().is_empty() => value,
            _ => {
                let path = required_env(ENV_CA_PEM_PATH);
                std::fs::read_to_string(&path).unwrap_or_else(|error| {
                    panic!("failed to read {ENV_CA_PEM_PATH}={path}: {error}")
                })
            }
        };
        assert!(
            ca_pem.contains("BEGIN CERTIFICATE"),
            "trusted CA PEM must contain a certificate"
        );
        Self {
            host,
            port,
            username,
            domain,
            password,
            ca_pem,
        }
    }

    fn connect(&self) -> Arc<PowerShellSession> {
        self.connect_configuration(None)
    }

    fn endpoint(&self, configuration_name: Option<&str>) -> PowerShellEndpoint {
        let mut endpoint = PowerShellEndpoint::https(self.host.clone())
            .expect("loopback host must be accepted")
            .with_port(self.port)
            .with_trusted_ca_pem(self.ca_pem.clone())
            .expect("trusted CA PEM must be accepted")
            .with_local_bind_address(Some(IpAddr::from([127, 0, 0, 1])))
            .expect("loopback local bind must be accepted");
        if let Some(name) = configuration_name {
            endpoint = endpoint
                .with_configuration_name(name)
                .expect("configuration name must be accepted");
        }
        endpoint
    }

    fn options(&self) -> PowerShellOptions {
        PowerShellOptions {
            connect_timeout: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(10),
            cancel_drain_timeout: Duration::from_secs(3),
            shutdown_timeout: Duration::from_secs(5),
            max_events_per_command: 128,
            ..PowerShellOptions::default()
        }
    }

    fn credentials(&self) -> PowerShellCredentials {
        PowerShellCredentials::with_domain(
            self.username.clone(),
            self.domain.clone(),
            SecretBytes::from_secret_string(self.password.as_str().to_owned()),
        )
        .expect("interop username and password must be valid")
    }

    fn try_connect(
        &self,
        configuration_name: Option<&str>,
    ) -> Result<PowerShellSession, PowerShellSessionError> {
        let endpoint = self.endpoint(configuration_name);
        let options = self.options();
        let credentials = self.credentials();
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = tx.send(PowerShellSession::connect(endpoint, options, credentials));
        });
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("connect to native PSRP endpoint exceeded 30s deadline")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("connect to native PSRP endpoint worker exited without a result")
            }
        }
    }

    fn connect_configuration(&self, configuration_name: Option<&str>) -> Arc<PowerShellSession> {
        match self.try_connect(configuration_name) {
            Ok(session) => Arc::new(session),
            Err(error) => panic!("connect to native PSRP endpoint failed: {error:?}"),
        }
    }
}

fn required_env(name: &str) -> String {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => panic!("{name} must be set for ignored PSRP interoperability tests"),
    }
}

fn start_script_bounded(
    session: &Arc<PowerShellSession>,
    script: &str,
    deadline: Duration,
) -> festerm_powershell::PowerShellCommand {
    let session = Arc::clone(session);
    let script = script.to_string();
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(session.start_script(script));
    });
    match rx.recv_timeout(deadline) {
        Ok(Ok(command)) => command,
        Ok(Err(error)) => panic!("start PSRP script failed: {error:?}"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("start PSRP script exceeded {deadline:?} deadline")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("start PSRP script worker exited without a result")
        }
    }
}

fn run_script(
    session: &Arc<PowerShellSession>,
    script: &str,
    deadline: Duration,
) -> Vec<PipelineEvent> {
    let mut command = start_script_bounded(session, script, Duration::from_secs(15));
    collect_command(&mut command, deadline)
}

fn collect_command(
    command: &mut festerm_powershell::PowerShellCommand,
    deadline: Duration,
) -> Vec<PipelineEvent> {
    let start = Instant::now();
    let mut events = Vec::new();
    loop {
        let elapsed = start.elapsed();
        assert!(elapsed < deadline, "PSRP command exceeded {deadline:?}");
        let wait = (deadline - elapsed).min(Duration::from_millis(500));
        match command.recv_timeout(wait) {
            Ok(Some(PowerShellCommandMessage::Event(event))) => events.push(event),
            Ok(Some(PowerShellCommandMessage::Completed { state })) => {
                assert_eq!(state, PipelineState::Completed);
                return events;
            }
            Ok(Some(PowerShellCommandMessage::Failed(failure))) => {
                panic!("PSRP command failed: {failure:?}")
            }
            Ok(None) => panic!("PSRP command ended without a terminal message"),
            Err(PowerShellSessionError::TimedOut) => continue,
            Err(error) => panic!("PSRP command receive failed: {error:?}"),
        }
    }
}

fn collect_failure(
    command: &mut festerm_powershell::PowerShellCommand,
    deadline: Duration,
) -> PowerShellCommandFailureKind {
    let start = Instant::now();
    loop {
        let elapsed = start.elapsed();
        assert!(
            elapsed < deadline,
            "PSRP failure wait exceeded {deadline:?}"
        );
        let wait = (deadline - elapsed).min(Duration::from_millis(500));
        match command.recv_timeout(wait) {
            Ok(Some(PowerShellCommandMessage::Event(_))) => {}
            Ok(Some(PowerShellCommandMessage::Completed { state })) => {
                panic!("expected failure, got completion state {state:?}")
            }
            Ok(Some(PowerShellCommandMessage::Failed(failure))) => return failure.kind,
            Ok(None) => panic!("PSRP command ended without a failure message"),
            Err(PowerShellSessionError::TimedOut) => continue,
            Err(error) => panic!("PSRP failure receive failed: {error:?}"),
        }
    }
}

fn wait_for_output(
    command: &mut festerm_powershell::PowerShellCommand,
    expected: &str,
    deadline: Duration,
) {
    let start = Instant::now();
    loop {
        let elapsed = start.elapsed();
        assert!(elapsed < deadline, "PSRP output wait exceeded {deadline:?}");
        let wait = (deadline - elapsed).min(Duration::from_millis(500));
        match command.recv_timeout(wait) {
            Ok(Some(PowerShellCommandMessage::Event(PipelineEvent::Output(value))))
                if value.as_str() == Some(expected) =>
            {
                return;
            }
            Ok(Some(PowerShellCommandMessage::Event(_))) => {}
            Ok(Some(PowerShellCommandMessage::Completed { state })) => {
                panic!("expected output {expected:?}, got completion state {state:?}")
            }
            Ok(Some(PowerShellCommandMessage::Failed(failure))) => {
                panic!("expected output {expected:?}, got failure: {failure:?}")
            }
            Ok(None) => panic!("PSRP command ended before output {expected:?}"),
            Err(PowerShellSessionError::TimedOut) => continue,
            Err(error) => panic!("PSRP output receive failed: {error:?}"),
        }
    }
}

fn assert_close_after_failure(session: &PowerShellSession) {
    match session.close() {
        Ok(()) => assert_eq!(session.status(), PowerShellSessionStatus::Closed),
        Err(PowerShellSessionError::Invalidated) => {
            assert_eq!(session.status(), PowerShellSessionStatus::Invalidated);
        }
        Err(error) => panic!("unexpected close result after failure: {error:?}"),
    }
}

fn output_strings(events: &[PipelineEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            PipelineEvent::Output(PsValue::String(value)) => Some(value.clone()),
            _ => None,
        })
        .collect()
}

fn output_i32(events: &[PipelineEvent]) -> Vec<i32> {
    events
        .iter()
        .filter_map(|event| match event {
            PipelineEvent::Output(value) => value.as_i32(),
            _ => None,
        })
        .collect()
}

#[test]
#[ignore = "requires GitHub-hosted Windows loopback WinRM HTTPS endpoint"]
fn native_psrp_persists_state_across_commands() {
    let cfg = InteropConfig::from_env();
    let session = cfg.connect();

    let marker = format!("festerm-state-{}", std::process::id());
    let set = format!(
        "$script:FestermInteropState = '{marker}'; $script:FestermInteropCounter = 41; 'set-ok'"
    );
    assert_eq!(
        output_strings(&run_script(&session, &set, Duration::from_secs(15))),
        ["set-ok"]
    );

    let read = run_script(
        &session,
        "$script:FestermInteropCounter += 1; $script:FestermInteropState; $script:FestermInteropCounter",
        Duration::from_secs(15),
    );
    assert!(output_strings(&read).contains(&marker));
    assert_eq!(output_i32(&read), [42]);

    session.close().expect("close PSRP session");
    assert_eq!(session.status(), PowerShellSessionStatus::Closed);
}

#[test]
#[ignore = "requires GitHub-hosted Windows loopback WinRM HTTPS endpoint"]
fn native_psrp_reports_typed_success_streams() {
    let cfg = InteropConfig::from_env();
    let session = cfg.connect();

    let script = r#"
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
"#;
    let events = run_script(&session, script, Duration::from_secs(20));

    let outputs = output_strings(&events);
    assert!(
        outputs.contains(&"out-stream".to_string()),
        "outputs: {outputs:?}"
    );
    assert!(
        outputs.contains(&"done-stream".to_string()),
        "outputs: {outputs:?}"
    );

    let errors: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            PipelineEvent::Error(value) => ErrorRecord::from_ps_object(value),
            _ => None,
        })
        .collect();
    assert!(
        errors.iter().any(|record| record
            .exception
            .as_ref()
            .and_then(|exception| exception.message.as_deref())
            .is_some_and(|message| message.contains("error-stream"))),
        "errors: {errors:?}"
    );

    let warnings: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            PipelineEvent::Warning(value) => WarningRecord::from_ps_object(value),
            _ => None,
        })
        .collect();
    assert!(
        warnings
            .iter()
            .any(|record| record.message.contains("warning-stream")),
        "warnings: {warnings:?}"
    );

    let verbose: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            PipelineEvent::Verbose(value) => TraceRecord::from_ps_object(value),
            _ => None,
        })
        .collect();
    assert!(
        verbose
            .iter()
            .any(|record| record.message.contains("verbose-stream")),
        "verbose: {verbose:?}"
    );

    let debug: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            PipelineEvent::Debug(value) => TraceRecord::from_ps_object(value),
            _ => None,
        })
        .collect();
    assert!(
        debug
            .iter()
            .any(|record| record.message.contains("debug-stream")),
        "debug: {debug:?}"
    );

    let progress: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            PipelineEvent::Progress(value) => ProgressRecord::from_ps_object(value),
            _ => None,
        })
        .collect();
    assert!(
        progress.iter().any(|record| record
            .activity
            .as_deref()
            .is_some_and(|activity| activity.contains("progress-stream"))),
        "progress: {progress:?}"
    );

    let information: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            PipelineEvent::Information(value) => InformationRecord::from_ps_object(value),
            _ => None,
        })
        .collect();
    assert!(
        information.iter().any(|record| record
            .message_data
            .as_ref()
            .and_then(PsValue::as_str)
            .is_some_and(|message| message.contains("information-stream"))),
        "information: {information:?}"
    );

    session.close().expect("close PSRP session");
}

#[test]
#[ignore = "requires GitHub-hosted Windows loopback WinRM HTTPS endpoint"]
fn native_psrp_rejects_unsupported_prompt_without_hanging() {
    let cfg = InteropConfig::from_env();
    let session = cfg.connect();

    let mut command = start_script_bounded(
        &session,
        "Read-Host -Prompt 'festerm unsupported prompt'; 'after-prompt'",
        Duration::from_secs(15),
    );
    let kind = collect_failure(&mut command, Duration::from_secs(20));
    assert!(
        matches!(
            kind,
            PowerShellCommandFailureKind::PipelineFailed
                | PowerShellCommandFailureKind::ProtocolFailure
                | PowerShellCommandFailureKind::Invalidated
        ),
        "unexpected prompt failure kind: {kind:?}"
    );
    if kind == PowerShellCommandFailureKind::Invalidated {
        assert_eq!(session.status(), PowerShellSessionStatus::Invalidated);
        assert!(matches!(
            session.start_script("'after-invalidated-prompt'"),
            Err(PowerShellSessionError::Invalidated)
        ));
        assert_close_after_failure(&session);
    } else {
        assert_eq!(
            output_strings(&run_script(
                &session,
                "'after-prompt-failure'",
                Duration::from_secs(15)
            )),
            ["after-prompt-failure"]
        );
        session
            .close()
            .expect("close PSRP session after prompt failure");
    }
}

#[test]
#[ignore = "requires GitHub-hosted Windows loopback WinRM HTTPS endpoint"]
fn native_psrp_cancellation_is_bounded_and_does_not_claim_uncertain_success() {
    let cfg = InteropConfig::from_env();
    let session = cfg.connect();

    let mut command = start_script_bounded(
        &session,
        "$global:FestermCancelMarker = 'cancel-session'; 'cancel-ready'; Start-Sleep -Seconds 30; 'unexpected-success'",
        Duration::from_secs(15),
    );
    wait_for_output(&mut command, "cancel-ready", Duration::from_secs(10));
    command.cancel();
    let kind = collect_failure(&mut command, Duration::from_secs(10));
    assert_ne!(
        kind,
        PowerShellCommandFailureKind::TimedOut,
        "cancellation must resolve before the bounded test deadline"
    );
    assert!(
        matches!(
            kind,
            PowerShellCommandFailureKind::Cancelled | PowerShellCommandFailureKind::Stopped
        ),
        "cancellation on the isolated endpoint must confirm a remote stop, got {kind:?}"
    );
    assert_eq!(
        output_strings(&run_script(
            &session,
            "$global:FestermCancelMarker; 'after-cancel'",
            Duration::from_secs(15)
        )),
        ["cancel-session", "after-cancel"]
    );
    session
        .close()
        .expect("close PSRP session after cancellation");
}

#[test]
#[ignore = "requires GitHub-hosted Windows loopback WinRM HTTPS endpoint"]
fn native_psrp_close_is_idempotent_and_prevents_new_commands() {
    let cfg = InteropConfig::from_env();
    let session = cfg.connect();

    assert_eq!(
        output_i32(&run_script(&session, "21 * 2", Duration::from_secs(15))),
        [42]
    );
    session.close().expect("first close succeeds");
    session.close().expect("second close is idempotent");
    assert_eq!(session.status(), PowerShellSessionStatus::Closed);
    assert!(matches!(
        session.start_script("'should-not-run'"),
        Err(PowerShellSessionError::Closed)
    ));
}

fn close_bounded(session: &Arc<PowerShellSession>, context: &str) {
    let session = Arc::clone(session);
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(session.close());
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => panic!("close {context} PSRP session failed: {error:?}"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("close {context} PSRP session exceeded 30s deadline")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("close {context} PSRP session worker exited without a result")
        }
    }
}

fn assert_edition_and_major(
    session: &Arc<PowerShellSession>,
    expected_edition: &str,
    expected_major: i32,
) {
    let events = run_script(
        session,
        "$PSVersionTable.PSEdition; [int]$PSVersionTable.PSVersion.Major",
        Duration::from_secs(15),
    );
    assert!(
        output_strings(&events).contains(&expected_edition.to_string()),
        "expected PSEdition {expected_edition:?}, got outputs {:?}",
        output_strings(&events)
    );
    assert!(
        output_i32(&events).contains(&expected_major),
        "expected PSVersion major {expected_major}, got outputs {:?}",
        output_i32(&events)
    );
}

fn assert_same_runspace_and_cancellation(session: &Arc<PowerShellSession>, marker: &str) {
    let set = format!("$global:FestermConfigMarker = '{marker}'; 'marker-set'");
    assert_eq!(
        output_strings(&run_script(session, &set, Duration::from_secs(15))),
        ["marker-set"]
    );

    let mut command = start_script_bounded(
        session,
        "'cancel-ready'; Start-Sleep -Seconds 30; 'unexpected-success'",
        Duration::from_secs(15),
    );
    wait_for_output(&mut command, "cancel-ready", Duration::from_secs(10));
    command.cancel();
    let kind = collect_failure(&mut command, Duration::from_secs(10));
    assert_ne!(
        kind,
        PowerShellCommandFailureKind::TimedOut,
        "cancellation must resolve before the bounded deadline"
    );
    assert!(
        matches!(
            kind,
            PowerShellCommandFailureKind::Cancelled | PowerShellCommandFailureKind::Stopped
        ),
        "cancellation must confirm a remote stop, got {kind:?}"
    );

    assert!(
        output_strings(&run_script(
            session,
            "$global:FestermConfigMarker; 'after-cancel'",
            Duration::from_secs(15)
        ))
        .contains(&marker.to_string()),
        "the same runspace must retain state set before cancellation"
    );
}

#[test]
#[ignore = "requires GitHub-hosted Windows loopback WinRM HTTPS endpoint"]
fn native_psrp_default_configuration_targets_windows_powershell() {
    let cfg = InteropConfig::from_env();
    let session = cfg.connect();
    // The default resource URI must actually reach Windows PowerShell 5.1,
    // proven by the reported edition/major rather than a bare success.
    assert_edition_and_major(&session, "Desktop", 5);
    close_bounded(&session, "default");
}

#[test]
#[ignore = "requires GitHub-hosted Windows loopback WinRM HTTPS endpoint"]
fn native_psrp_unknown_configuration_fails_without_fallback() {
    let cfg = InteropConfig::from_env();
    // A syntactically valid but unregistered configuration must fail to
    // connect; it must never silently fall back to the default endpoint.
    let error = cfg
        .try_connect(Some("Festerm.Unregistered.Config"))
        .expect_err("unregistered configuration must not connect");
    assert!(
        !matches!(error, PowerShellSessionError::UnsupportedAuth(_)),
        "unexpected pre-network rejection for unknown configuration: {error:?}"
    );
}

#[test]
#[ignore = "requires GitHub-hosted Windows loopback WinRM HTTPS endpoint"]
fn native_psrp_powershell7_configuration_when_registered() {
    let cfg = InteropConfig::from_env();
    // The isolated hosted harness sets FESTERM_PSRP_INTEROP_PS7_REQUIRED when it
    // has provisioned a PowerShell 7 session configuration (pwsh is expected on
    // GitHub-hosted Windows runners). In that required mode a missing/blank
    // configuration is a hard failure, so a green run can never imply the
    // PowerShell 7 endpoint was qualified when it was actually skipped.
    let required = env::var(ENV_PS7_REQUIRED)
        .ok()
        .is_some_and(|value| !value.trim().is_empty());
    let config_name = env::var(ENV_PS7_CONFIG)
        .ok()
        .filter(|value| !value.trim().is_empty());
    let config_name = match config_name {
        Some(name) => name,
        None if required => panic!(
            "{ENV_PS7_REQUIRED} is set but {ENV_PS7_CONFIG} is missing: the PowerShell 7 session \
             configuration was not provisioned; refusing to report success without qualifying the \
             Core 7 endpoint"
        ),
        None => {
            eprintln!(
                "skipping PowerShell 7 endpoint qualification: {ENV_PS7_CONFIG} not set (PowerShell \
                 7 session configuration was not provisioned on this runner)"
            );
            return;
        }
    };

    let session = cfg.connect_configuration(Some(&config_name));
    // Prove the PowerShell 7 resource URI actually reached a Core 7 host.
    assert_edition_and_major(&session, "Core", 7);
    let marker = format!("festerm-ps7-{}", std::process::id());
    assert_same_runspace_and_cancellation(&session, &marker);
    close_bounded(&session, "PowerShell 7");
}
