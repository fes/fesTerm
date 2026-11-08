use std::env;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use festerm_powershell::{
    value_to_display_text, PipelineEvent, PowerShellCommandMessage, PowerShellOptions,
    PowerShellSessionError, PowerShellSshSession,
};
use festerm_session::TerminalSize;
use festerm_ssh::{HostIdentity, SshAuthentication, SshConnectionProfile, SshRawExecOptions};

const USAGE: &str = "usage: psrp-ssh-shell --fingerprint SHA256:... --password-env ENV [--port PORT] [--subsystem NAME] <host> <username> <script>";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let invocation = Invocation::parse(env::args().skip(1))?;
    let password = env::var(&invocation.password_env).map_err(|_| {
        format!(
            "password environment variable {} is not set",
            invocation.password_env
        )
    })?;
    let profile = SshConnectionProfile::new(
        HostIdentity::new(invocation.host, invocation.port)?,
        invocation.username,
        SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
        TerminalSize::new(80, 24).expect("default terminal size is valid"),
    )?;
    let ssh_options = SshRawExecOptions::new()
        .with_known_host_fingerprint(invocation.fingerprint)?
        .with_io_timeouts(Duration::from_millis(100), Duration::from_millis(100))?;
    let session = PowerShellSshSession::connect_ssh_subsystem_named(
        profile,
        SshAuthentication::password(password),
        invocation.subsystem,
        ssh_options,
        PowerShellOptions {
            connect_timeout: Duration::from_secs(30),
            operation_timeout: Duration::from_secs(30),
            cancel_drain_timeout: Duration::from_secs(5),
            shutdown_timeout: Duration::from_secs(5),
            ..PowerShellOptions::default()
        },
    )?;

    let mut command = session.start_script(invocation.script)?;
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    loop {
        if Instant::now() >= deadline {
            command.cancel();
            let _ = session.close();
            return Err("PowerShell SSH command exceeded 120s deadline".into());
        }
        match command.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(PowerShellCommandMessage::Event(event))) => {
                write_event(&event, &mut stdout, &mut stderr)?;
            }
            Ok(Some(PowerShellCommandMessage::Completed { state })) => {
                writeln!(stderr, "Pipeline finished with state {state:?}")?;
                session.close()?;
                return Ok(());
            }
            Ok(Some(PowerShellCommandMessage::Failed(failure))) => {
                let _ = session.close();
                return Err(format!("PowerShell SSH pipeline failed: {:?}", failure.kind).into());
            }
            Ok(None) => {
                let _ = session.close();
                return Err("PowerShell SSH pipeline ended without completion".into());
            }
            Err(PowerShellSessionError::TimedOut) => {}
            Err(error) => {
                let _ = session.close();
                return Err(format!("PowerShell SSH receive failed: {error:?}").into());
            }
        }
    }
}

struct Invocation {
    host: String,
    port: u16,
    username: String,
    fingerprint: String,
    password_env: String,
    subsystem: String,
    script: String,
}

impl Invocation {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, Box<dyn std::error::Error>> {
        let mut port = 22;
        let mut fingerprint = None;
        let mut password_env = None;
        let mut subsystem = PowerShellSshSession::DEFAULT_SUBSYSTEM_NAME.to_string();
        let mut positional = Vec::new();
        let mut args = args;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--port" => port = args.next().ok_or(USAGE)?.parse()?,
                "--fingerprint" => fingerprint = Some(args.next().ok_or(USAGE)?),
                "--password-env" => password_env = Some(args.next().ok_or(USAGE)?),
                "--subsystem" => subsystem = args.next().ok_or(USAGE)?,
                "--help" | "-h" => return Err(USAGE.into()),
                _ if arg.starts_with('-') => return Err(USAGE.into()),
                _ => positional.push(arg),
            }
        }
        let mut positional = positional.into_iter();
        let host = positional.next().ok_or(USAGE)?;
        let username = positional.next().ok_or(USAGE)?;
        let script = positional.next().ok_or(USAGE)?;
        if positional.next().is_some() || fingerprint.is_none() || password_env.is_none() {
            return Err(USAGE.into());
        }
        Ok(Self {
            host,
            port,
            username,
            fingerprint: fingerprint.expect("checked"),
            password_env: password_env.expect("checked"),
            subsystem,
            script,
        })
    }
}

fn write_event(
    event: &PipelineEvent,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<()> {
    match event {
        PipelineEvent::Output(value) => writeln!(stdout, "{}", value_to_display_text(value)),
        PipelineEvent::Error(value) => writeln!(stderr, "ERROR: {}", value_to_display_text(value)),
        PipelineEvent::Warning(value) => {
            writeln!(stderr, "WARNING: {}", value_to_display_text(value))
        }
        PipelineEvent::Verbose(value) => {
            writeln!(stderr, "VERBOSE: {}", value_to_display_text(value))
        }
        PipelineEvent::Debug(value) => writeln!(stderr, "DEBUG: {}", value_to_display_text(value)),
        PipelineEvent::Information(value) => {
            writeln!(stderr, "INFO: {}", value_to_display_text(value))
        }
        PipelineEvent::Progress(value) => {
            writeln!(stderr, "PROGRESS: {}", value_to_display_text(value))
        }
        PipelineEvent::State(_) => Ok(()),
    }
}
