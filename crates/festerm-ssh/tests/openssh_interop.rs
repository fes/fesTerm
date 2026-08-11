use std::{
    env, thread,
    time::{Duration, Instant},
};

use festerm_session::{
    Session, SessionEvent, SessionLifecycle, SessionTryReceiveError, ShutdownResult, TerminalSize,
};
use festerm_ssh::{
    HostIdentity, HostTrustDecision, SshAuthentication, SshConnectionProfile, SshSession,
};

const MARKER: &[u8] = b"__FESTERM_OPENSSH_INTEROP_OK__";
const EVENT_TIMEOUT: Duration = Duration::from_secs(20);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

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

#[test]
#[ignore = "requires the repository-owned OpenSSH Docker fixture"]
fn controlled_openssh_interoperability() {
    let configuration =
        OpenSshConfiguration::from_environment().expect("OpenSSH fixture environment is invalid");
    let profile = SshConnectionProfile::new(
        HostIdentity::new(configuration.host, configuration.port)
            .expect("OpenSSH fixture host identity is invalid"),
        configuration.username,
        SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
        TerminalSize::new(80, 24).expect("initial terminal size must be valid"),
    )
    .expect("OpenSSH fixture connection profile is invalid");
    let session = SshSession::start(profile, SshAuthentication::password(configuration.password))
        .expect("could not start OpenSSH session");
    let resolver = session.host_key_decision_resolver();
    let deadline = Instant::now() + EVENT_TIMEOUT;
    let mut running = false;
    let mut resize_applied = false;
    let mut marker_seen = false;
    let mut output_tail = Vec::new();

    while Instant::now() < deadline && !(running && resize_applied && marker_seen) {
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
                output_tail.extend_from_slice(&bytes);
                if output_tail
                    .windows(MARKER.len())
                    .any(|window| window == MARKER)
                {
                    marker_seen = true;
                }
                if output_tail.len() > MARKER.len() {
                    let first_retained_byte = output_tail.len() - MARKER.len();
                    output_tail.drain(..first_retained_byte);
                }
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
        marker_seen,
        "controlled SSH shell command did not produce its expected marker"
    );
    match session.shutdown(SHUTDOWN_TIMEOUT) {
        Ok(ShutdownResult::Stopped | ShutdownResult::AlreadyStopped) => {}
        Err(_) => panic!("SSH session did not shut down within the test timeout"),
    }
}
