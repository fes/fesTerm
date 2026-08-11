use std::{
    env, fs,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use festerm_session::{
    Session, SessionEvent, SessionLifecycle, SessionOperation, SessionSendError,
    SessionTryReceiveError, ShutdownResult, TerminalSize,
};
use festerm_ssh::{
    HostIdentity, HostTrustDecision, ReconnectPolicy, SshAuthentication, SshConnectionProfile,
    SshPrivateKey, SshSession, SshSessionOptions,
};

const MARKER: &[u8] = b"__FESTERM_OPENSSH_INTEROP_OK__";
const EVENT_TIMEOUT: Duration = Duration::from_secs(20);
const RECONNECT_EVENT_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const DOCKER_HEALTH_TIMEOUT: Duration = Duration::from_secs(20);

struct OpenSshConfiguration {
    host: String,
    port: u16,
    username: String,
    password: String,
}

impl OpenSshConfiguration {
    fn from_environment() -> Result<Self, String> {
        let host = required_environment("FESTERM_OPENSSH_HOST")?;
        let port = required_environment("FESTERM_OPENSSH_PORT")?
            .parse::<u16>()
            .map_err(|_| "FESTERM_OPENSSH_PORT must be a valid port number".to_owned())?;
        let username = required_environment("FESTERM_OPENSSH_USER")?;
        let password = required_environment("FESTERM_OPENSSH_PASSWORD")?;
        Ok(Self {
            host,
            port,
            username,
            password,
        })
    }
}

fn required_environment(name: &str) -> Result<String, String> {
    match env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        Ok(_) | Err(_) => Err(format!(
            "{name} must be set for the OpenSSH interoperability test"
        )),
    }
}

fn connection_profile(configuration: &OpenSshConfiguration) -> SshConnectionProfile {
    SshConnectionProfile::new(
        HostIdentity::new(configuration.host.clone(), configuration.port)
            .expect("OpenSSH fixture host identity is invalid"),
        configuration.username.clone(),
        SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
        TerminalSize::new(80, 24).expect("initial terminal size must be valid"),
    )
    .expect("OpenSSH fixture connection profile is invalid")
}

fn generated_private_key_from_environment() -> SshPrivateKey {
    let path = required_environment("FESTERM_OPENSSH_PRIVATE_KEY_PATH")
        .expect("OpenSSH fixture private-key path is invalid");
    let encoded = fs::read(path).expect("could not read generated OpenSSH private key");
    SshPrivateKey::from_openssh(encoded).expect("generated OpenSSH private key is invalid")
}

fn marker_seen(output_tail: &mut Vec<u8>, bytes: &[u8], marker: &[u8]) -> bool {
    output_tail.extend_from_slice(bytes);
    let seen = output_tail
        .windows(marker.len())
        .any(|window| window == marker);
    if output_tail.len() > marker.len() {
        let first_retained_byte = output_tail.len() - marker.len();
        output_tail.drain(..first_retained_byte);
    }
    seen
}

#[test]
#[ignore = "requires the repository-owned OpenSSH Docker fixture"]
fn controlled_openssh_interoperability() {
    let configuration =
        OpenSshConfiguration::from_environment().expect("OpenSSH fixture environment is invalid");
    let profile = connection_profile(&configuration);
    let session = SshSession::start(profile, SshAuthentication::password(configuration.password))
        .expect("could not start OpenSSH session");
    let resolver = session.host_key_decision_resolver();
    let deadline = Instant::now() + EVENT_TIMEOUT;
    let mut running = false;
    let mut resize_applied = false;
    let mut direct_marker_seen = false;
    let mut output_tail = Vec::new();

    while Instant::now() < deadline && !(running && resize_applied && direct_marker_seen) {
        match session.try_recv_event() {
            Ok(SessionEvent::HostKeyVerification(prompt)) => resolver
                .resolve(&prompt, HostTrustDecision::AcceptOnce)
                .expect("could not accept the test server host key"),
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) if !running => {
                running = true;
                session
                    .try_resize(TerminalSize::new(100, 40).expect("resize terminal size is valid"))
                    .expect("could not request SSH PTY resize");
                session
                    .try_send_input(b"printf '__FESTERM_OPENSSH_INTEROP_OK__\\n'\n")
                    .expect("could not send controlled SSH shell command");
            }
            Ok(SessionEvent::ResizeApplied(size))
                if size == TerminalSize::new(100, 40).expect("resize terminal size is valid") =>
            {
                resize_applied = true;
            }
            Ok(SessionEvent::Output(bytes)) => {
                direct_marker_seen |= marker_seen(&mut output_tail, &bytes, MARKER);
            }
            Ok(SessionEvent::Error(error)) => {
                panic!(
                    "SSH session emitted a {:?} error before controlled exchange completed",
                    error.kind()
                );
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!("SSH session event stream closed before controlled exchange completed");
            }
        }
    }

    assert!(
        running,
        "SSH session did not reach Running within the test timeout"
    );
    assert!(
        resize_applied,
        "SSH PTY resize was not acknowledged within the test timeout"
    );
    assert!(
        direct_marker_seen,
        "controlled SSH shell command did not produce its expected marker"
    );
    match session.shutdown(SHUTDOWN_TIMEOUT) {
        Ok(ShutdownResult::Stopped | ShutdownResult::AlreadyStopped) => {}
        Err(_) => panic!("SSH session did not shut down within the test timeout"),
    }
}

#[test]
#[ignore = "requires the repository-owned OpenSSH Docker fixture"]
fn controlled_openssh_public_key_interoperability() {
    const KEY_MARKER: &[u8] = b"__FESTERM_OPENSSH_PUBLIC_KEY_OK__";

    let configuration =
        OpenSshConfiguration::from_environment().expect("OpenSSH fixture environment is invalid");
    let session = SshSession::start(
        connection_profile(&configuration),
        SshAuthentication::public_key(generated_private_key_from_environment()),
    )
    .expect("could not start public-key OpenSSH session");
    let resolver = session.host_key_decision_resolver();
    let deadline = Instant::now() + EVENT_TIMEOUT;
    let mut host_key_prompt_seen = false;
    let mut running = false;
    let mut marker_seen_in_output = false;
    let mut output_tail = Vec::new();

    while Instant::now() < deadline && !(running && marker_seen_in_output) {
        match session.try_recv_event() {
            Ok(SessionEvent::HostKeyVerification(prompt)) if !host_key_prompt_seen => {
                resolver
                    .resolve(&prompt, HostTrustDecision::AcceptOnce)
                    .expect("could not accept the public-key test server host key");
                host_key_prompt_seen = true;
            }
            Ok(SessionEvent::HostKeyVerification(_)) => {
                panic!("public-key OpenSSH session emitted more than one host-key prompt")
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) if !running => {
                running = true;
                session
                    .try_send_input(b"printf '__FESTERM_OPENSSH_PUBLIC_KEY_OK__\\n'\n")
                    .expect("could not send public-key controlled SSH shell command");
            }
            Ok(SessionEvent::Output(bytes)) => {
                marker_seen_in_output |= marker_seen(&mut output_tail, &bytes, KEY_MARKER);
            }
            Ok(SessionEvent::Error(error)) => {
                panic!(
                    "public-key SSH session emitted a {:?} error before controlled exchange completed",
                    error.kind()
                );
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!("public-key SSH session event stream closed before controlled exchange completed");
            }
        }
    }

    assert!(
        host_key_prompt_seen,
        "public-key SSH session did not request host-key verification"
    );
    assert!(
        running,
        "public-key SSH session did not reach Running within the test timeout"
    );
    assert!(
        marker_seen_in_output,
        "public-key controlled SSH shell command did not produce its expected marker"
    );
    match session.shutdown(SHUTDOWN_TIMEOUT) {
        Ok(ShutdownResult::Stopped | ShutdownResult::AlreadyStopped) => {}
        Err(_) => panic!("public-key SSH session did not shut down within the test timeout"),
    }
}

struct DockerFixture {
    container_name: String,
}

impl DockerFixture {
    fn from_environment() -> Self {
        Self {
            container_name: required_environment("FESTERM_OPENSSH_CONTAINER_NAME")
                .expect("OpenSSH fixture container name is invalid"),
        }
    }

    fn kill(&self) {
        docker_status(&["kill", &self.container_name], "kill the OpenSSH fixture");
    }

    fn start_and_wait(&self, expected_port: u16) {
        docker_status(
            &["start", &self.container_name],
            "restart the OpenSSH fixture",
        );
        self.wait_until_healthy();
        self.assert_mapped_port(expected_port);
    }

    fn wait_until_healthy(&self) {
        let deadline = Instant::now() + DOCKER_HEALTH_TIMEOUT;
        loop {
            let health = docker_output(
                &[
                    "inspect",
                    "--format",
                    "{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}",
                    &self.container_name,
                ],
                "inspect the OpenSSH fixture health",
            );
            match health.trim() {
                "healthy" => return,
                "unhealthy" => panic!("OpenSSH fixture became unhealthy after restart"),
                _ if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
                _ => panic!("OpenSSH fixture did not become healthy after restart"),
            }
        }
    }

    fn assert_mapped_port(&self, expected_port: u16) {
        let mapping = docker_output(
            &["port", &self.container_name, "22/tcp"],
            "inspect the OpenSSH fixture port mapping",
        );
        let expected_port = expected_port.to_string();
        assert!(
            mapping
                .lines()
                .any(|line| line.rsplit(':').next() == Some(expected_port.as_str())),
            "OpenSSH fixture did not retain its mapped host port after restart"
        );
    }
}

fn docker_status(arguments: &[&str], operation: &str) {
    let output = Command::new("docker")
        .args(arguments)
        .output()
        .unwrap_or_else(|_| panic!("could not invoke Docker to {operation}"));
    assert!(output.status.success(), "Docker could not {operation}");
}

fn docker_output(arguments: &[&str], operation: &str) -> String {
    let output = Command::new("docker")
        .args(arguments)
        .output()
        .unwrap_or_else(|_| panic!("could not invoke Docker to {operation}"));
    assert!(output.status.success(), "Docker could not {operation}");
    String::from_utf8(output.stdout).expect("Docker returned non-UTF-8 fixture metadata")
}

#[test]
#[ignore = "requires the repository-owned OpenSSH Docker fixture"]
fn controlled_openssh_reconnect_interoperability() {
    const INITIAL_MARKER: &[u8] = b"__FESTERM_OPENSSH_RECONNECT_INITIAL_OK__";
    const RECONNECTED_MARKER: &[u8] = b"__FESTERM_OPENSSH_RECONNECT_OK__";

    let configuration =
        OpenSshConfiguration::from_environment().expect("OpenSSH fixture environment is invalid");
    let fixture = DockerFixture::from_environment();
    let reconnect_policy =
        ReconnectPolicy::new(6, Duration::from_millis(250), Duration::from_secs(1))
            .expect("reconnect policy must be valid");
    let session = SshSession::start_with_options(
        connection_profile(&configuration),
        SshAuthentication::password(configuration.password),
        SshSessionOptions::with_reconnect_policy(reconnect_policy),
    )
    .expect("could not start reconnect-enabled OpenSSH session");
    let resolver = session.host_key_decision_resolver();

    let deadline = Instant::now() + EVENT_TIMEOUT;
    let mut initial_prompt_seen = false;
    let mut initial_running = false;
    let mut initial_marker_seen = false;
    let mut output_tail = Vec::new();
    while Instant::now() < deadline && !(initial_running && initial_marker_seen) {
        match session.try_recv_event() {
            Ok(SessionEvent::HostKeyVerification(prompt)) if !initial_prompt_seen => {
                resolver
                    .resolve(&prompt, HostTrustDecision::AcceptOnce)
                    .expect("could not accept the initial test server host key");
                initial_prompt_seen = true;
            }
            Ok(SessionEvent::HostKeyVerification(_)) => {
                panic!("initial OpenSSH connection emitted more than one host-key prompt")
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) if !initial_running => {
                initial_running = true;
                session
                    .try_send_input(
                        b"printf '%s%s%s\\n' '__FESTERM_OPENSSH_' 'RECONNECT_INITIAL_' 'OK__'\n",
                    )
                    .expect("could not send initial controlled SSH shell command");
            }
            Ok(SessionEvent::Output(bytes)) => {
                initial_marker_seen |= marker_seen(&mut output_tail, &bytes, INITIAL_MARKER);
            }
            Ok(SessionEvent::Error(error)) => {
                panic!(
                    "SSH session emitted a {:?} error before reconnect testing began",
                    error.kind()
                );
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!("SSH session event stream closed before reconnect testing began");
            }
        }
    }
    assert!(
        initial_prompt_seen,
        "initial OpenSSH connection did not request host-key verification"
    );
    assert!(
        initial_running,
        "initial OpenSSH connection did not reach Running within the test timeout"
    );
    assert!(
        initial_marker_seen,
        "initial controlled SSH shell command did not produce its expected marker"
    );

    fixture.kill();
    let deadline = Instant::now() + RECONNECT_EVENT_TIMEOUT;
    let mut reconnecting = false;
    while Instant::now() < deadline && !reconnecting {
        match session.try_recv_event() {
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Starting)) => reconnecting = true,
            Ok(SessionEvent::Error(error)) => {
                panic!(
                    "SSH session emitted a {:?} error before it began reconnecting",
                    error.kind()
                );
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!("SSH session event stream closed before it began reconnecting");
            }
        }
    }
    assert!(
        reconnecting,
        "SSH session did not enter reconnecting state within the test timeout"
    );
    assert_eq!(
        session.try_send_input(b"must-not-be-queued-during-reconnect"),
        Err(SessionSendError::Closed {
            operation: SessionOperation::Input,
        }),
        "SSH input must be rejected while reconnecting"
    );
    assert_eq!(
        session.try_resize(TerminalSize::new(101, 41).expect("resize terminal size is valid")),
        Err(SessionSendError::Closed {
            operation: SessionOperation::Resize,
        }),
        "SSH resize must be rejected while reconnecting"
    );

    fixture.start_and_wait(configuration.port);

    let deadline = Instant::now() + RECONNECT_EVENT_TIMEOUT;
    let mut fresh_prompt_seen = false;
    let mut reconnected_running = false;
    let mut reconnected_marker_seen = false;
    let mut output_tail = Vec::new();
    while Instant::now() < deadline && !(reconnected_running && reconnected_marker_seen) {
        match session.try_recv_event() {
            Ok(SessionEvent::HostKeyVerification(prompt)) if !fresh_prompt_seen => {
                resolver
                    .resolve(&prompt, HostTrustDecision::AcceptOnce)
                    .expect("could not accept the fresh test server host key");
                fresh_prompt_seen = true;
            }
            Ok(SessionEvent::HostKeyVerification(_)) => {
                panic!("reconnected OpenSSH session emitted more than one host-key prompt")
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) if fresh_prompt_seen => {
                reconnected_running = true;
                session
                    .try_send_input(
                        b"printf '%s%s%s\\n' '__FESTERM_OPENSSH_' 'RECONNECT_' 'OK__'\n",
                    )
                    .expect("could not send reconnected controlled SSH shell command");
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) => {
                panic!("reconnected OpenSSH session reached Running without fresh host trust")
            }
            Ok(SessionEvent::Output(bytes)) => {
                if reconnected_running {
                    reconnected_marker_seen |=
                        marker_seen(&mut output_tail, &bytes, RECONNECTED_MARKER);
                }
            }
            Ok(SessionEvent::Error(error)) => {
                panic!(
                    "SSH session emitted a {:?} error while reconnecting",
                    error.kind()
                );
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!("SSH session event stream closed while reconnecting");
            }
        }
    }
    assert!(
        fresh_prompt_seen,
        "reconnected OpenSSH session did not request fresh host-key verification"
    );
    assert!(
        reconnected_running,
        "reconnected OpenSSH session did not reach Running within the test timeout"
    );
    assert!(
        reconnected_marker_seen,
        "reconnected controlled SSH shell command did not produce its expected marker"
    );
    match session.shutdown(SHUTDOWN_TIMEOUT) {
        Ok(ShutdownResult::Stopped | ShutdownResult::AlreadyStopped) => {}
        Err(_) => panic!("reconnected SSH session did not shut down within the test timeout"),
    }
}
