use std::{
    env, fs,
    io::Write,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use festerm_session::{
    HostKeyPrompt, Session, SessionErrorKind, SessionEvent, SessionLifecycle, SessionOperation,
    SessionSendError, SessionTryReceiveError, ShutdownError, ShutdownResult, TerminalSize,
};
use festerm_ssh::{
    HostIdentity, HostTrustDecision, PersistenceProvider, PersistentSessionName, ReconnectPolicy,
    RecoveryPolicy, SessionStrategy, SshAuthentication, SshConnectionProfile, SshKeyPassphrase,
    SshLivenessCheckError, SshPrivateKey, SshSession, SshSessionOptions,
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

fn generated_encrypted_private_key_from_environment() -> SshPrivateKey {
    let path = required_environment("FESTERM_OPENSSH_ENCRYPTED_PRIVATE_KEY_PATH")
        .expect("OpenSSH fixture encrypted private-key path is invalid");
    let passphrase = required_environment("FESTERM_OPENSSH_ENCRYPTED_PRIVATE_KEY_PASSPHRASE")
        .expect("OpenSSH fixture encrypted private-key passphrase is invalid");
    let encoded = fs::read(path).expect("could not read generated encrypted OpenSSH private key");
    SshPrivateKey::from_encrypted_openssh(encoded, SshKeyPassphrase::new(passphrase))
        .expect("generated encrypted OpenSSH private key is invalid")
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
fn controlled_openssh_interactive_password_interoperability() {
    const INTERACTIVE_MARKER: &[u8] = b"__FESTERM_OPENSSH_INTERACTIVE_OK__";

    let configuration =
        OpenSshConfiguration::from_environment().expect("OpenSSH fixture environment is invalid");
    // Interactive auth never carries a credential upfront: the worker
    // connects and verifies the host key first, exactly like the fingerprint
    // -first UX this exercises, and only asks for a password afterward.
    let session = SshSession::start(
        connection_profile(&configuration),
        SshAuthentication::interactive(),
    )
    .expect("could not start interactive OpenSSH session");
    let host_key_resolver = session.host_key_decision_resolver();
    let password_resolver = session.password_decision_resolver();
    let deadline = Instant::now() + EVENT_TIMEOUT;
    let mut host_key_prompt_seen = false;
    let mut wrong_password_sent = false;
    let mut reprompt_after_rejection_seen = false;
    let mut running = false;
    let mut marker_seen_in_output = false;
    let mut output_tail = Vec::new();

    while Instant::now() < deadline && !(running && marker_seen_in_output) {
        match session.try_recv_event() {
            Ok(SessionEvent::HostKeyVerification(prompt)) if !host_key_prompt_seen => {
                host_key_resolver
                    .resolve(&prompt, HostTrustDecision::AcceptOnce)
                    .expect("could not accept the interactive test server host key");
                host_key_prompt_seen = true;
            }
            Ok(SessionEvent::HostKeyVerification(_)) => {
                panic!("interactive OpenSSH session emitted more than one host-key prompt")
            }
            Ok(SessionEvent::PasswordRequested(prompt)) if !wrong_password_sent => {
                assert!(
                    host_key_prompt_seen,
                    "the password prompt must arrive only after host-key verification, \
                     mirroring `ssh`'s own fingerprint-first ordering"
                );
                assert!(
                    !prompt.previous_attempt_failed(),
                    "the first password attempt must not claim a prior rejection"
                );
                password_resolver
                    .resolve(&prompt, "definitely-the-wrong-password".to_owned())
                    .expect("could not submit the deliberately wrong interactive password");
                wrong_password_sent = true;
            }
            Ok(SessionEvent::PasswordRequested(prompt)) if !reprompt_after_rejection_seen => {
                assert!(
                    prompt.previous_attempt_failed(),
                    "a reprompt following a rejected password must report the prior failure, \
                     mirroring `ssh`'s own \"Permission denied, please try again.\""
                );
                password_resolver
                    .resolve(&prompt, configuration.password.clone())
                    .expect("could not submit the correct interactive password after rejection");
                reprompt_after_rejection_seen = true;
            }
            Ok(SessionEvent::PasswordRequested(_)) => {
                panic!("interactive OpenSSH session requested a password more times than expected")
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) if !running => {
                running = true;
                session
                    .try_send_input(b"printf '__FESTERM_OPENSSH_INTERACTIVE_OK__\\n'\n")
                    .expect("could not send interactive controlled SSH shell command");
            }
            Ok(SessionEvent::Output(bytes)) => {
                marker_seen_in_output |= marker_seen(&mut output_tail, &bytes, INTERACTIVE_MARKER);
            }
            Ok(SessionEvent::Error(error)) => {
                panic!(
                    "interactive SSH session emitted a {:?} error before controlled exchange completed",
                    error.kind()
                );
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!(
                    "interactive SSH session event stream closed before controlled exchange completed"
                );
            }
        }
    }

    assert!(
        host_key_prompt_seen,
        "interactive SSH session did not request host-key verification"
    );
    assert!(
        wrong_password_sent && reprompt_after_rejection_seen,
        "interactive SSH session did not exercise a rejected-then-corrected password round"
    );
    assert!(
        running,
        "interactive SSH session did not reach Running within the test timeout"
    );
    assert!(
        marker_seen_in_output,
        "interactive controlled SSH shell command did not produce its expected marker"
    );
    match session.shutdown(SHUTDOWN_TIMEOUT) {
        Ok(ShutdownResult::Stopped | ShutdownResult::AlreadyStopped) => {}
        Err(_) => panic!("interactive SSH session did not shut down within the test timeout"),
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

#[test]
#[ignore = "requires the repository-owned OpenSSH Docker fixture"]
fn controlled_openssh_encrypted_public_key_interoperability() {
    const KEY_MARKER: &[u8] = b"__FESTERM_OPENSSH_ENCRYPTED_PUBLIC_KEY_OK__";

    let configuration =
        OpenSshConfiguration::from_environment().expect("OpenSSH fixture environment is invalid");
    let session = SshSession::start(
        connection_profile(&configuration),
        SshAuthentication::public_key(generated_encrypted_private_key_from_environment()),
    )
    .expect("could not start encrypted public-key OpenSSH session");
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
                    .expect("could not accept the encrypted public-key test server host key");
                host_key_prompt_seen = true;
            }
            Ok(SessionEvent::HostKeyVerification(_)) => {
                panic!("encrypted public-key OpenSSH session emitted more than one host-key prompt")
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) if !running => {
                running = true;
                session
                    .try_send_input(b"printf '__FESTERM_OPENSSH_ENCRYPTED_PUBLIC_KEY_OK__\\n'\n")
                    .expect("could not send encrypted public-key controlled SSH shell command");
            }
            Ok(SessionEvent::Output(bytes)) => {
                marker_seen_in_output |= marker_seen(&mut output_tail, &bytes, KEY_MARKER);
            }
            Ok(SessionEvent::Error(error)) => {
                panic!(
                    "encrypted public-key SSH session emitted a {:?} error before controlled exchange completed",
                    error.kind()
                );
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!("encrypted public-key SSH session event stream closed before controlled exchange completed");
            }
        }
    }

    assert!(
        host_key_prompt_seen,
        "encrypted public-key SSH session did not request host-key verification"
    );
    assert!(
        running,
        "encrypted public-key SSH session did not reach Running within the test timeout"
    );
    assert!(
        marker_seen_in_output,
        "encrypted public-key controlled SSH shell command did not produce its expected marker"
    );
    match session.shutdown(SHUTDOWN_TIMEOUT) {
        Ok(ShutdownResult::Stopped | ShutdownResult::AlreadyStopped) => {}
        Err(_) => {
            panic!("encrypted public-key SSH session did not shut down within the test timeout")
        }
    }
}

#[test]
#[ignore = "requires the repository-owned OpenSSH Docker fixture"]
fn controlled_openssh_ecdsa_p256_host_key_interoperability() {
    const ECDSA_MARKER: &[u8] = b"__FESTERM_OPENSSH_ECDSA_P256_HOST_KEY_OK__";

    let configuration =
        OpenSshConfiguration::from_environment().expect("OpenSSH fixture environment is invalid");
    let expected_fingerprint = required_environment("FESTERM_OPENSSH_EXPECTED_HOST_FINGERPRINT")
        .expect("OpenSSH fixture ECDSA host-key fingerprint is invalid");
    let session = SshSession::start(
        connection_profile(&configuration),
        SshAuthentication::password(configuration.password.clone()),
    )
    .expect("could not start ECDSA host-key OpenSSH session");
    let resolver = session.host_key_decision_resolver();
    let deadline = Instant::now() + EVENT_TIMEOUT;
    let mut host_key_prompt_seen = false;
    let mut running = false;
    let mut marker_seen_in_output = false;
    let mut output_tail = Vec::new();

    while Instant::now() < deadline && !(running && marker_seen_in_output) {
        match session.try_recv_event() {
            Ok(SessionEvent::HostKeyVerification(prompt)) if !host_key_prompt_seen => {
                assert_eq!(prompt.host(), configuration.host.as_str());
                assert_eq!(prompt.port(), configuration.port);
                assert_eq!(prompt.sha256_fingerprint(), expected_fingerprint.as_str());
                resolver
                    .resolve(&prompt, HostTrustDecision::AcceptOnce)
                    .expect("could not accept the ECDSA test server host key");
                host_key_prompt_seen = true;
            }
            Ok(SessionEvent::HostKeyVerification(_)) => {
                panic!("ECDSA host-key OpenSSH session emitted more than one host-key prompt")
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) if !running => {
                running = true;
                session
                    .try_send_input(b"printf '__FESTERM_OPENSSH_ECDSA_P256_HOST_KEY_OK__\\n'\n")
                    .expect("could not send ECDSA host-key controlled SSH shell command");
            }
            Ok(SessionEvent::Output(bytes)) => {
                marker_seen_in_output |= marker_seen(&mut output_tail, &bytes, ECDSA_MARKER);
            }
            Ok(SessionEvent::Error(error)) => {
                panic!(
                    "ECDSA host-key SSH session emitted a {:?} error before controlled exchange completed",
                    error.kind()
                );
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!("ECDSA host-key SSH session event stream closed before controlled exchange completed");
            }
        }
    }

    assert!(
        host_key_prompt_seen,
        "ECDSA host-key OpenSSH session did not request host-key verification"
    );
    assert!(
        running,
        "ECDSA host-key OpenSSH session did not reach Running within the test timeout"
    );
    assert!(
        marker_seen_in_output,
        "ECDSA host-key controlled SSH shell command did not produce its expected marker"
    );
    match session.shutdown(SHUTDOWN_TIMEOUT) {
        Ok(ShutdownResult::Stopped | ShutdownResult::AlreadyStopped) => {}
        Err(_) => panic!("ECDSA host-key SSH session did not shut down within the test timeout"),
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

    /// Severs only the client's active SSH connection by killing its
    /// per-connection `sshd` handler process inside the fixture container,
    /// leaving the container (and, crucially, any durable `tmux`/`screen`
    /// session it hosts) running. This is what actually distinguishes a
    /// durable-session interop test from [`Self::kill`]: killing the whole
    /// container would also kill the persistence provider's daemon, which
    /// would defeat the point of testing that a durable session survives an
    /// unintentional transport loss (ADR 0018).
    ///
    /// This container is reused across every test in this file, so a stale
    /// `sshd-session: festerm@...` process from an *earlier* test can
    /// briefly still be reaping in the background when this one starts.
    /// Selecting the numerically highest PID (i.e. the most recently forked
    /// matching process, since PIDs increase monotonically within a
    /// container) rather than the first line of `ps` output ensures this
    /// only ever targets the connection this test itself just established.
    fn sever_active_ssh_connection(&self) {
        let output = Command::new("docker")
            .args([
                "exec",
                &self.container_name,
                "sh",
                "-c",
                "pid=$(ps aux | grep 'sshd-session: festerm@' | grep -v grep \
                 | awk '{print $1}' | sort -rn | head -1); [ -n \"$pid\" ] && kill -9 \"$pid\"",
            ])
            .output()
            .unwrap_or_else(|_| {
                panic!("could not invoke Docker to sever the active SSH connection")
            });
        assert!(
            output.status.success(),
            "Docker could not sever the active SSH connection inside the OpenSSH fixture"
        );
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

    fn disable_screen(&self) -> DisabledFixtureProgram {
        let original_path = docker_output(
            &[
                "exec",
                &self.container_name,
                "sh",
                "-c",
                "command -v screen",
            ],
            "locate GNU Screen inside the OpenSSH fixture",
        )
        .trim()
        .to_owned();
        assert!(
            original_path.starts_with('/'),
            "OpenSSH fixture returned an invalid GNU Screen path"
        );
        let disabled_path = format!("{original_path}.festerm-disabled");
        docker_status(
            &[
                "exec",
                &self.container_name,
                "mv",
                &original_path,
                &disabled_path,
            ],
            "disable GNU Screen inside the OpenSSH fixture",
        );
        DisabledFixtureProgram {
            container_name: self.container_name.clone(),
            original_path,
            disabled_path,
        }
    }

    fn override_password(&self, replacement: &str, original: String) -> FixturePasswordOverride {
        assert!(
            set_fixture_password(&self.container_name, replacement),
            "Docker could not replace the OpenSSH fixture password"
        );
        FixturePasswordOverride {
            container_name: self.container_name.clone(),
            original,
        }
    }

    fn rotate_ecdsa_host_key(&self) -> RotatedFixtureHostKey {
        docker_status(
            &[
                "exec",
                &self.container_name,
                "sh",
                "-c",
                "rm -f /run/festerm-original-ecdsa-key \
                    /run/festerm-original-ecdsa-key.pub && \
                 cp /etc/ssh/ssh_host_ecdsa_key /run/festerm-original-ecdsa-key && \
                 cp /etc/ssh/ssh_host_ecdsa_key.pub /run/festerm-original-ecdsa-key.pub && \
                 rm -f /etc/ssh/ssh_host_ecdsa_key /etc/ssh/ssh_host_ecdsa_key.pub && \
                 ssh-keygen -q -t ecdsa -b 256 -N '' \
                    -f /etc/ssh/ssh_host_ecdsa_key",
            ],
            "rotate the OpenSSH fixture ECDSA host key",
        );
        docker_status(
            &["kill", "--signal", "HUP", &self.container_name],
            "reload the OpenSSH fixture after rotating its host key",
        );
        thread::sleep(Duration::from_secs(1));
        self.wait_until_healthy();
        RotatedFixtureHostKey {
            container_name: self.container_name.clone(),
            fingerprint: self.ecdsa_host_key_fingerprint(),
        }
    }

    fn ecdsa_host_key_fingerprint(&self) -> String {
        let details = docker_output(
            &[
                "exec",
                &self.container_name,
                "ssh-keygen",
                "-lf",
                "/etc/ssh/ssh_host_ecdsa_key.pub",
                "-E",
                "sha256",
            ],
            "read the OpenSSH fixture ECDSA host-key fingerprint",
        );
        let fingerprint = details
            .split_whitespace()
            .nth(1)
            .expect("OpenSSH fixture host-key details did not include a fingerprint")
            .to_owned();
        assert!(
            fingerprint.starts_with("SHA256:"),
            "OpenSSH fixture returned an invalid ECDSA host-key fingerprint"
        );
        fingerprint
    }
}

struct DisabledFixtureProgram {
    container_name: String,
    original_path: String,
    disabled_path: String,
}

impl Drop for DisabledFixtureProgram {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args([
                "exec",
                &self.container_name,
                "mv",
                &self.disabled_path,
                &self.original_path,
            ])
            .output();
    }
}

struct FixturePasswordOverride {
    container_name: String,
    original: String,
}

impl Drop for FixturePasswordOverride {
    fn drop(&mut self) {
        let _ = set_fixture_password(&self.container_name, &self.original);
    }
}

struct RotatedFixtureHostKey {
    container_name: String,
    fingerprint: String,
}

impl RotatedFixtureHostKey {
    fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

impl Drop for RotatedFixtureHostKey {
    fn drop(&mut self) {
        let restored = Command::new("docker")
            .args([
                "exec",
                &self.container_name,
                "sh",
                "-c",
                "mv /run/festerm-original-ecdsa-key \
                    /etc/ssh/ssh_host_ecdsa_key && \
                 mv /run/festerm-original-ecdsa-key.pub \
                    /etc/ssh/ssh_host_ecdsa_key.pub",
            ])
            .output()
            .is_ok_and(|output| output.status.success());
        if restored {
            let _ = Command::new("docker")
                .args(["kill", "--signal", "HUP", &self.container_name])
                .output();
            thread::sleep(Duration::from_secs(1));
        }
    }
}

fn set_fixture_password(container_name: &str, password: &str) -> bool {
    let Ok(mut child) = Command::new("docker")
        .args(["exec", "-i", container_name, "chpasswd"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let written = child
        .stdin
        .take()
        .is_some_and(|mut stdin| writeln!(stdin, "festerm:{password}").is_ok());
    written && child.wait().is_ok_and(|status| status.success())
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

fn start_known_host_automatic_screen_session(
    configuration: &OpenSshConfiguration,
    session_name: &str,
    marker: &[u8],
    command: &[u8],
) -> SshSession {
    let known_fingerprint = required_environment("FESTERM_OPENSSH_EXPECTED_HOST_FINGERPRINT")
        .expect("OpenSSH fixture host-key fingerprint is invalid");
    let strategy = SessionStrategy::Persistent {
        provider: PersistenceProvider::Screen,
        session_name: PersistentSessionName::new(session_name)
            .expect("durable session name is valid"),
    };
    let options = SshSessionOptions::with_recovery_policy(
        strategy,
        RecoveryPolicy::Automatic(ReconnectPolicy::default_automatic()),
    )
    .expect("automatic recovery is valid for a persistent strategy")
    .with_known_host_fingerprint(known_fingerprint);
    let session = SshSession::start_with_options(
        connection_profile(configuration),
        SshAuthentication::password(configuration.password.clone()),
        options,
    )
    .expect("could not start the automatic-recovery OpenSSH session");

    let deadline = Instant::now() + EVENT_TIMEOUT;
    let mut running = false;
    let mut marker_seen_in_output = false;
    let mut output_tail = Vec::new();
    while Instant::now() < deadline && !(running && marker_seen_in_output) {
        match session.try_recv_event() {
            Ok(SessionEvent::HostKeyVerification(_)) => {
                panic!("the unchanged known OpenSSH host key unexpectedly required confirmation")
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) if !running => {
                running = true;
                session
                    .try_send_input(command)
                    .expect("could not send the automatic-recovery readiness command");
            }
            Ok(SessionEvent::Output(bytes)) => {
                marker_seen_in_output |= marker_seen(&mut output_tail, &bytes, marker);
            }
            Ok(SessionEvent::Error(error)) => {
                panic!(
                    "SSH session emitted a {:?} error before automatic-recovery failure testing",
                    error.kind()
                );
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!("SSH session event stream closed before automatic-recovery failure testing")
            }
        }
    }
    assert!(
        running,
        "automatic-recovery OpenSSH session did not reach Running within the test timeout"
    );
    assert!(
        marker_seen_in_output,
        "automatic-recovery readiness command did not produce its expected marker"
    );
    session
}

fn assert_automatic_recovery_stops_with<F>(
    session: &SshSession,
    expected_kind: SessionErrorKind,
    expected_message: &str,
    mut handle_host_key_prompt: F,
) -> usize
where
    F: FnMut(&HostKeyPrompt),
{
    let deadline = Instant::now() + RECONNECT_EVENT_TIMEOUT;
    let mut starting_count = 0;
    let mut error_event = None;
    let mut failed_lifecycle = None;
    while Instant::now() < deadline && (error_event.is_none() || failed_lifecycle.is_none()) {
        match session.try_recv_event() {
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Starting)) => {
                starting_count += 1;
            }
            Ok(SessionEvent::HostKeyVerification(prompt)) => {
                handle_host_key_prompt(&prompt);
            }
            Ok(SessionEvent::Error(error)) => {
                assert!(
                    error_event.is_none(),
                    "automatic recovery emitted more than one terminal error"
                );
                error_event = Some(error);
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Failed(error))) => {
                assert!(
                    failed_lifecycle.is_none(),
                    "automatic recovery entered Failed more than once"
                );
                failed_lifecycle = Some(error);
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) => {
                panic!("automatic recovery unexpectedly returned to Running")
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Disconnected(_))) => {
                panic!("a permanent recovery failure must stop instead of becoming Disconnected")
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!("SSH session event stream closed before reporting recovery failure")
            }
        }
    }

    assert!(
        starting_count > 0,
        "the automatic-recovery session never entered Starting"
    );
    let error_event =
        error_event.expect("automatic recovery did not emit its terminal error in time");
    let failed_lifecycle =
        failed_lifecycle.expect("automatic recovery did not enter Failed in time");
    for error in [&error_event, &failed_lifecycle] {
        assert_eq!(error.kind(), expected_kind);
        assert_eq!(error.message(), expected_message);
    }
    assert_eq!(
        session.lifecycle(),
        SessionLifecycle::Failed(failed_lifecycle.clone()),
        "automatic recovery did not remain terminal after its permanent failure"
    );
    assert!(
        !session.reconnect_available(),
        "a terminal automatic-recovery failure must not remain reconnectable"
    );
    match session.shutdown(SHUTDOWN_TIMEOUT) {
        Err(ShutdownError::Failed(error)) => {
            assert_eq!(error.kind(), expected_kind);
            assert_eq!(error.message(), expected_message);
        }
        result => {
            panic!("expected failed SSH shutdown after recovery stopped, received {result:?}")
        }
    }
    starting_count
}

#[test]
#[ignore = "requires the repository-owned OpenSSH Docker fixture"]
fn controlled_openssh_manual_reconnect_interoperability() {
    const INITIAL_MARKER: &[u8] = b"__FESTERM_OPENSSH_RECONNECT_INITIAL_OK__";
    const RECONNECTED_MARKER: &[u8] = b"__FESTERM_OPENSSH_RECONNECT_OK__";

    let configuration =
        OpenSshConfiguration::from_environment().expect("OpenSSH fixture environment is invalid");
    let fixture = DockerFixture::from_environment();
    // Ordinary SSH sessions are plain shells with no durable-session
    // provider, so automatic recovery is not valid for them (ADR 0018);
    // `SshSessionOptions::new()` is the only constructible option today and
    // always means manual-only reconnect.
    let session = SshSession::start_with_options(
        connection_profile(&configuration),
        SshAuthentication::password(configuration.password),
        SshSessionOptions::new(),
    )
    .expect("could not start the OpenSSH session");
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

    // Ordinary SSH sessions have no durable-session provider (ADR 0018), so
    // there is no automatic recovery here: killing the fixture alone must
    // never move the session into a reconnecting state by itself. Recovery
    // only happens once the user explicitly requests it.
    fixture.kill();
    thread::sleep(Duration::from_millis(200));
    assert!(
        !matches!(
            session.try_recv_event(),
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Starting))
        ),
        "an unintentional transport loss must never auto-reconnect a plain SSH session"
    );
    session
        .try_reconnect()
        .expect("an explicit reconnect must always be available for a running plain SSH session");

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
        "the explicitly requested reconnect did not enter a reconnecting state within the test timeout"
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

#[test]
#[ignore = "requires the repository-owned OpenSSH Docker fixture"]
fn controlled_openssh_liveness_probe_succeeds_without_disrupting_a_healthy_session() {
    const MARKER_BEFORE: &[u8] = b"__FESTERM_OPENSSH_LIVENESS_BEFORE_OK__";
    const MARKER_AFTER: &[u8] = b"__FESTERM_OPENSSH_LIVENESS_AFTER_OK__";

    let configuration =
        OpenSshConfiguration::from_environment().expect("OpenSSH fixture environment is invalid");
    let session = SshSession::start(
        connection_profile(&configuration),
        SshAuthentication::password(configuration.password),
    )
    .expect("could not start OpenSSH session");
    let resolver = session.host_key_decision_resolver();

    let deadline = Instant::now() + EVENT_TIMEOUT;
    let mut running = false;
    let mut marker_before_seen = false;
    let mut output_tail = Vec::new();
    while Instant::now() < deadline && !(running && marker_before_seen) {
        match session.try_recv_event() {
            Ok(SessionEvent::HostKeyVerification(prompt)) => resolver
                .resolve(&prompt, HostTrustDecision::AcceptOnce)
                .expect("could not accept the test server host key"),
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) if !running => {
                running = true;
                session
                    .try_send_input(
                        b"printf '%s%s%s\\n' '__FESTERM_OPENSSH_' 'LIVENESS_BEFORE_' 'OK__'\n",
                    )
                    .expect("could not send controlled SSH shell command");
            }
            Ok(SessionEvent::Output(bytes)) => {
                marker_before_seen |= marker_seen(&mut output_tail, &bytes, MARKER_BEFORE);
            }
            Ok(SessionEvent::Error(error)) => {
                panic!(
                    "SSH session emitted a {:?} error before liveness testing began",
                    error.kind()
                );
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!("SSH session event stream closed before liveness testing began");
            }
        }
    }
    assert!(
        running,
        "SSH session did not reach Running within the test timeout"
    );
    assert!(
        marker_before_seen,
        "controlled SSH shell command did not produce its expected marker before the liveness probe"
    );

    // ADR 0018: an on-demand liveness probe against a healthy transport must
    // succeed silently and never disrupt the running session, and a second
    // request must coalesce with the still-pending first one.
    session
        .try_check_liveness()
        .expect("a liveness probe must be available for a running session");
    assert_eq!(
        session.try_check_liveness(),
        Err(SshLivenessCheckError::AlreadyRequested),
        "a second on-demand probe must coalesce with the first until the worker services it"
    );
    session
        .try_send_input(b"printf '%s%s%s\\n' '__FESTERM_OPENSSH_' 'LIVENESS_AFTER_' 'OK__'\n")
        .expect("could not send controlled SSH shell command after requesting a liveness probe");

    let deadline = Instant::now() + EVENT_TIMEOUT;
    let mut marker_after_seen = false;
    output_tail.clear();
    while Instant::now() < deadline && !marker_after_seen {
        match session.try_recv_event() {
            Ok(SessionEvent::Lifecycle(
                SessionLifecycle::Disconnected(_)
                | SessionLifecycle::Failed(_)
                | SessionLifecycle::Exited(_)
                | SessionLifecycle::Stopping
                | SessionLifecycle::Stopped,
            )) => {
                panic!("a liveness probe against a healthy transport must not disrupt the session");
            }
            Ok(SessionEvent::Output(bytes)) => {
                marker_after_seen |= marker_seen(&mut output_tail, &bytes, MARKER_AFTER);
            }
            Ok(SessionEvent::Error(error)) => {
                panic!(
                    "SSH session emitted a {:?} error while a liveness probe was pending",
                    error.kind()
                );
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!("SSH session event stream closed while a liveness probe was pending");
            }
        }
    }
    assert!(
        marker_after_seen,
        "controlled SSH shell command did not produce its expected marker after the liveness probe"
    );
    match session.shutdown(SHUTDOWN_TIMEOUT) {
        Ok(ShutdownResult::Stopped | ShutdownResult::AlreadyStopped) => {}
        Err(_) => panic!("SSH session did not shut down within the test timeout"),
    }
}

#[test]
#[ignore = "requires the repository-owned OpenSSH Docker fixture"]
fn controlled_openssh_tmux_persistent_session_reattaches_after_manual_reconnect() {
    const SET_MARKER: &[u8] = b"__FESTERM_TMUX_SET_OK__";
    const SIZE_MARKER: &[u8] = b"__FESTERM_TMUX_SIZE_37_111__";
    const STATUS_MARKER: &[u8] = b"__FESTERM_TMUX_STATUS_off__";
    const PROOF_MARKER: &[u8] = b"__FESTERM_TMUX_PROOF_persisted-value__";

    let configuration =
        OpenSshConfiguration::from_environment().expect("OpenSSH fixture environment is invalid");
    let fixture = DockerFixture::from_environment();
    let strategy = SessionStrategy::Persistent {
        provider: PersistenceProvider::Tmux,
        session_name: PersistentSessionName::new("festerm-interop-tmux")
            .expect("durable session name is valid"),
    };
    let session = SshSession::start_with_options(
        connection_profile(&configuration),
        SshAuthentication::password(configuration.password.clone()),
        SshSessionOptions::manual_recovery(strategy),
    )
    .expect("could not start the OpenSSH session");
    session
        .try_resize(TerminalSize::new(111, 37).expect("test size is valid"))
        .expect("could not queue the initial tmux PTY size");
    let resolver = session.host_key_decision_resolver();

    let deadline = Instant::now() + EVENT_TIMEOUT;
    let mut running = false;
    let mut proof_set = false;
    let mut correct_size_seen = false;
    let mut bare_status_seen = false;
    let mut output_tail = Vec::new();
    let mut size_output_tail = Vec::new();
    let mut status_output_tail = Vec::new();
    while Instant::now() < deadline
        && !(running && proof_set && correct_size_seen && bare_status_seen)
    {
        match session.try_recv_event() {
            Ok(SessionEvent::HostKeyVerification(prompt)) => resolver
                .resolve(&prompt, HostTrustDecision::AcceptOnce)
                .expect("could not accept the test server host key"),
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) if !running => {
                running = true;
                session
                    .try_send_input(
                        b"export FESTERM_PROOF=persisted-value; \
                          printf '%s\\n' '__FESTERM_TMUX_SET_OK__'; \
                          printf '__FESTERM_TMUX_SIZE_%s__\\n' \"$(stty size | tr ' ' '_')\"; \
                          printf '__FESTERM_TMUX_STATUS_%s__\\n' \
                          \"$(tmux show-option -t festerm-interop-tmux -v status)\"\n",
                    )
                    .expect("could not set the durable-session proof variable");
            }
            Ok(SessionEvent::Output(bytes)) => {
                proof_set |= marker_seen(&mut output_tail, &bytes, SET_MARKER);
                correct_size_seen |= marker_seen(&mut size_output_tail, &bytes, SIZE_MARKER);
                bare_status_seen |= marker_seen(&mut status_output_tail, &bytes, STATUS_MARKER);
            }
            Ok(SessionEvent::Error(error)) => {
                panic!(
                    "SSH session emitted a {:?} error before the durable-session proof was set",
                    error.kind()
                );
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!("SSH session event stream closed before the durable-session proof was set");
            }
        }
    }
    assert!(
        running,
        "SSH session did not reach Running within the test timeout"
    );
    assert!(
        proof_set,
        "could not set the durable-session proof variable inside the tmux session"
    );
    assert!(
        correct_size_seen,
        "the first tmux shell did not receive the pre-connect 111x37 terminal size"
    );
    assert!(
        bare_status_seen,
        "the tmux session did not disable its status-bar chrome"
    );

    // Sever only the transport, not the container: the durable tmux session
    // (and the proof variable set inside it) must survive, unlike
    // `controlled_openssh_manual_reconnect_interoperability`'s whole-container
    // kill, which would also kill the persistence provider's daemon.
    fixture.sever_active_ssh_connection();
    thread::sleep(Duration::from_millis(200));
    assert!(
        !matches!(
            session.try_recv_event(),
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Starting))
        ),
        "manual-only recovery must never auto-reconnect after an unintentional transport loss"
    );
    session
        .try_reconnect()
        .expect("an explicit reconnect must always be available for a running persistent session");

    let deadline = Instant::now() + RECONNECT_EVENT_TIMEOUT;
    let mut fresh_prompt_seen = false;
    let mut reconnected_running = false;
    let mut proof_confirmed = false;
    output_tail.clear();
    while Instant::now() < deadline && !(reconnected_running && proof_confirmed) {
        match session.try_recv_event() {
            Ok(SessionEvent::HostKeyVerification(prompt)) if !fresh_prompt_seen => {
                resolver
                    .resolve(&prompt, HostTrustDecision::AcceptOnce)
                    .expect("could not accept the fresh test server host key");
                fresh_prompt_seen = true;
            }
            Ok(SessionEvent::HostKeyVerification(_)) => {
                panic!("reattached OpenSSH session emitted more than one host-key prompt")
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) if fresh_prompt_seen => {
                reconnected_running = true;
                session
                    .try_send_input(b"printf '__FESTERM_TMUX_PROOF_%s__\\n' \"$FESTERM_PROOF\"\n")
                    .expect("could not read back the durable-session proof variable");
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) => {
                panic!("reattached OpenSSH session reached Running without fresh host trust")
            }
            Ok(SessionEvent::Output(bytes)) => {
                if reconnected_running {
                    proof_confirmed |= marker_seen(&mut output_tail, &bytes, PROOF_MARKER);
                }
            }
            Ok(SessionEvent::Error(error)) => {
                panic!(
                    "SSH session emitted a {:?} error while reattaching to the durable session",
                    error.kind()
                );
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!("SSH session event stream closed while reattaching to the durable session");
            }
        }
    }
    assert!(
        fresh_prompt_seen,
        "reattached OpenSSH session did not request fresh host-key verification"
    );
    assert!(
        reconnected_running,
        "reattached OpenSSH session did not reach Running within the test timeout"
    );
    assert!(
        proof_confirmed,
        "reattached tmux session did not retain the proof variable set before the transport was \
         severed; this means the client created a new session instead of reattaching to the \
         durable one"
    );
    match session.shutdown(SHUTDOWN_TIMEOUT) {
        Ok(ShutdownResult::Stopped | ShutdownResult::AlreadyStopped) => {}
        Err(_) => panic!("reattached SSH session did not shut down within the test timeout"),
    }
}

#[test]
#[ignore = "requires the repository-owned OpenSSH Docker fixture"]
fn controlled_openssh_screen_persistent_session_reattaches_after_automatic_recovery() {
    const SET_MARKER: &[u8] = b"__FESTERM_SCREEN_SET_OK__";
    const PROOF_MARKER: &[u8] = b"__FESTERM_SCREEN_PROOF_persisted-value__";

    let configuration =
        OpenSshConfiguration::from_environment().expect("OpenSSH fixture environment is invalid");
    let fixture = DockerFixture::from_environment();
    let strategy = SessionStrategy::Persistent {
        provider: PersistenceProvider::Screen,
        session_name: PersistentSessionName::new("festerm-interop-screen")
            .expect("durable session name is valid"),
    };
    let session = SshSession::start_with_options(
        connection_profile(&configuration),
        SshAuthentication::password(configuration.password.clone()),
        SshSessionOptions::with_recovery_policy(
            strategy,
            RecoveryPolicy::Automatic(ReconnectPolicy::default_automatic()),
        )
        .expect("automatic recovery is valid for a persistent strategy"),
    )
    .expect("could not start the OpenSSH session");
    let resolver = session.host_key_decision_resolver();

    let deadline = Instant::now() + EVENT_TIMEOUT;
    let mut running = false;
    let mut proof_set = false;
    let mut output_tail = Vec::new();
    while Instant::now() < deadline && !(running && proof_set) {
        match session.try_recv_event() {
            Ok(SessionEvent::HostKeyVerification(prompt)) => resolver
                .resolve(&prompt, HostTrustDecision::AcceptOnce)
                .expect("could not accept the test server host key"),
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) if !running => {
                running = true;
                session
                    .try_send_input(
                        b"export FESTERM_PROOF=persisted-value; printf '%s\\n' '__FESTERM_SCREEN_SET_OK__'\n",
                    )
                    .expect("could not set the durable-session proof variable");
            }
            Ok(SessionEvent::Output(bytes)) => {
                proof_set |= marker_seen(&mut output_tail, &bytes, SET_MARKER);
            }
            Ok(SessionEvent::Error(error)) => {
                panic!(
                    "SSH session emitted a {:?} error before the durable-session proof was set",
                    error.kind()
                );
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!("SSH session event stream closed before the durable-session proof was set");
            }
        }
    }
    assert!(
        running,
        "SSH session did not reach Running within the test timeout"
    );
    assert!(
        proof_set,
        "could not set the durable-session proof variable inside the screen session"
    );

    // Sever only the transport, not the container: an opted-in automatic
    // recovery policy must reconnect on its own, without any explicit
    // `try_reconnect()` call, and reattach to the same durable screen
    // session rather than creating a fresh one.
    fixture.sever_active_ssh_connection();

    let deadline = Instant::now() + RECONNECT_EVENT_TIMEOUT;
    let mut auto_reconnecting = false;
    let mut fresh_prompt_seen = false;
    let mut reconnected_running = false;
    let mut proof_confirmed = false;
    output_tail.clear();
    while Instant::now() < deadline && !(reconnected_running && proof_confirmed) {
        match session.try_recv_event() {
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Starting)) => {
                auto_reconnecting = true;
            }
            Ok(SessionEvent::HostKeyVerification(prompt)) if !fresh_prompt_seen => {
                resolver
                    .resolve(&prompt, HostTrustDecision::AcceptOnce)
                    .expect("could not accept the fresh test server host key");
                fresh_prompt_seen = true;
            }
            Ok(SessionEvent::HostKeyVerification(_)) => {
                panic!("reattached OpenSSH session emitted more than one host-key prompt")
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) if fresh_prompt_seen => {
                reconnected_running = true;
                session
                    .try_send_input(b"printf '__FESTERM_SCREEN_PROOF_%s__\\n' \"$FESTERM_PROOF\"\n")
                    .expect("could not read back the durable-session proof variable");
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) => {
                panic!("reattached OpenSSH session reached Running without fresh host trust")
            }
            Ok(SessionEvent::Output(bytes)) => {
                if reconnected_running {
                    proof_confirmed |= marker_seen(&mut output_tail, &bytes, PROOF_MARKER);
                }
            }
            Ok(SessionEvent::Error(error)) => {
                panic!(
                    "SSH session emitted a {:?} error while automatically reattaching to the \
                     durable session",
                    error.kind()
                );
            }
            Ok(_) | Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(SessionTryReceiveError::Closed) => {
                panic!(
                    "SSH session event stream closed while automatically reattaching to the \
                     durable session"
                );
            }
        }
    }
    assert!(
        auto_reconnecting,
        "an automatic-recovery-enabled durable session must reconnect on its own after an \
         unintentional transport loss, without any explicit try_reconnect() call"
    );
    assert!(
        fresh_prompt_seen,
        "automatically reattached OpenSSH session did not request fresh host-key verification"
    );
    assert!(
        reconnected_running,
        "automatically reattached OpenSSH session did not reach Running within the test timeout"
    );
    assert!(
        proof_confirmed,
        "automatically reattached screen session did not retain the proof variable set before \
         the transport was severed; this means the client created a new session instead of \
         reattaching to the durable one"
    );
    match session.shutdown(SHUTDOWN_TIMEOUT) {
        Ok(ShutdownResult::Stopped | ShutdownResult::AlreadyStopped) => {}
        Err(_) => {
            panic!("automatically reattached SSH session did not shut down within the test timeout")
        }
    }
}

#[test]
#[ignore = "requires the repository-owned OpenSSH Docker fixture"]
fn controlled_openssh_automatic_recovery_stops_when_provider_disappears() {
    const READY_MARKER: &[u8] = b"__FESTERM_RECOVERY_PROVIDER_READY_OK__";

    let configuration =
        OpenSshConfiguration::from_environment().expect("OpenSSH fixture environment is invalid");
    let fixture = DockerFixture::from_environment();
    let session = start_known_host_automatic_screen_session(
        &configuration,
        "festerm-recovery-provider-disappears",
        READY_MARKER,
        b"printf '%s%s%s\\n' '__FESTERM_' 'RECOVERY_PROVIDER_READY_' 'OK__'\n",
    );

    let _disabled_screen = fixture.disable_screen();
    fixture.sever_active_ssh_connection();

    let starting_count = assert_automatic_recovery_stops_with(
        &session,
        SessionErrorKind::Spawn,
        "the configured durable-session provider is not available on the remote host",
        |_| panic!("unchanged known host trust unexpectedly prompted during automatic recovery"),
    );
    assert_eq!(
        starting_count, 1,
        "provider disappearance must stop automatic recovery after its first fresh connection"
    );
}

#[test]
#[ignore = "requires the repository-owned OpenSSH Docker fixture"]
fn controlled_openssh_automatic_recovery_stops_when_host_key_change_is_rejected() {
    const READY_MARKER: &[u8] = b"__FESTERM_RECOVERY_HOST_KEY_READY_OK__";

    let configuration =
        OpenSshConfiguration::from_environment().expect("OpenSSH fixture environment is invalid");
    let expected_fingerprint = required_environment("FESTERM_OPENSSH_EXPECTED_HOST_FINGERPRINT")
        .expect("OpenSSH fixture host-key fingerprint is invalid");
    let fixture = DockerFixture::from_environment();
    let session = start_known_host_automatic_screen_session(
        &configuration,
        "festerm-recovery-host-key-changes",
        READY_MARKER,
        b"printf '%s%s%s\\n' '__FESTERM_' 'RECOVERY_HOST_KEY_READY_' 'OK__'\n",
    );
    let rotated_host_key = fixture.rotate_ecdsa_host_key();
    assert_ne!(
        rotated_host_key.fingerprint(),
        expected_fingerprint,
        "the OpenSSH fixture host-key rotation did not change its fingerprint"
    );
    fixture.sever_active_ssh_connection();

    let resolver = session.host_key_decision_resolver();
    let mut prompt_count = 0;
    let starting_count = assert_automatic_recovery_stops_with(
        &session,
        SessionErrorKind::Spawn,
        "SSH host key was rejected",
        |prompt| {
            prompt_count += 1;
            assert_eq!(prompt.host(), configuration.host);
            assert_eq!(prompt.port(), configuration.port);
            assert!(prompt.is_key_change());
            assert_eq!(
                prompt.previously_trusted_fingerprint(),
                Some(expected_fingerprint.as_str())
            );
            assert_eq!(prompt.sha256_fingerprint(), rotated_host_key.fingerprint());
            resolver
                .resolve(prompt, HostTrustDecision::Reject)
                .expect("could not reject the changed OpenSSH fixture host key");
        },
    );
    assert_eq!(
        prompt_count, 1,
        "changed host trust must produce exactly one recovery prompt"
    );
    assert_eq!(
        starting_count, 1,
        "rejected changed host trust must stop automatic recovery after its first attempt"
    );
}

#[test]
#[ignore = "requires the repository-owned OpenSSH Docker fixture"]
fn controlled_openssh_automatic_recovery_stops_when_password_is_rejected() {
    const READY_MARKER: &[u8] = b"__FESTERM_RECOVERY_PASSWORD_READY_OK__";

    let configuration =
        OpenSshConfiguration::from_environment().expect("OpenSSH fixture environment is invalid");
    let fixture = DockerFixture::from_environment();
    let session = start_known_host_automatic_screen_session(
        &configuration,
        "festerm-recovery-password-rejected",
        READY_MARKER,
        b"printf '%s%s%s\\n' '__FESTERM_' 'RECOVERY_PASSWORD_READY_' 'OK__'\n",
    );

    let _password_override = fixture.override_password(
        "festerm-deliberately-rejected-during-recovery",
        configuration.password,
    );
    fixture.sever_active_ssh_connection();

    let starting_count = assert_automatic_recovery_stops_with(
        &session,
        SessionErrorKind::Authentication,
        "SSH authentication failed",
        |_| panic!("unchanged known host trust unexpectedly prompted during automatic recovery"),
    );
    assert_eq!(
        starting_count, 1,
        "rejected credentials must stop automatic recovery after its first fresh connection"
    );
}
