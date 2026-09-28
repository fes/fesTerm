use std::env;
use std::io::{self, BufRead, Write};
use std::net::IpAddr;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use festerm_core::{Dimensions, Terminal};
use festerm_session::{
    noop_session_event_notifier, Session, SessionEvent, SessionLifecycle, SessionSendError,
    SessionTryReceiveError, TerminalSize, MAX_IO_CHUNK_BYTES,
};
use festerm_sessiond::{decode_recovery_sync_command, RecoverySyncCommand, RemoteSshEndpoint};
use festerm_ssh::{HostIdentity, SshAuthentication, SshConnectionProfile};

const USAGE: &str = "usage: remote-sessiond [--local-address IP] [--attach NAME --allow-takeover] <host> <port> <user> <fingerprint> <helper>";
const DEFAULT_COLUMNS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const INITIAL_RECOVERY_DEADLINE: Duration = Duration::from_secs(15);
const LOOP_IDLE_SLEEP: Duration = Duration::from_millis(25);
const MAX_EVENTS_PER_TICK: usize = 64;
const MAX_INPUT_EVENTS_PER_TICK: usize = 8;
const RETRY_SLEEP: Duration = Duration::from_millis(50);
const MAX_SEND_RETRIES: usize = 10;
const MAX_INPUT_LINE_BYTES: usize = MAX_IO_CHUNK_BYTES - 1;

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {}", sanitize_display_text(&error.to_string()));
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let invocation = Invocation::parse(env::args().skip(1))?;
    let endpoint = invocation.build_endpoint()?;

    let discovery_password = prompt_password(&format!(
        "SSH password for {}@{}:{} (discovery): ",
        sanitize_display_text(endpoint.profile().username()),
        sanitize_display_text(endpoint.profile().identity().host()),
        endpoint.profile().identity().port()
    ))?;
    let inventory = endpoint.discover(SshAuthentication::password(discovery_password))?;
    print_inventory(&endpoint, &inventory)?;

    let Some(name) = invocation.attach_name.as_deref() else {
        return Ok(());
    };

    let target = inventory.select(name)?;
    let attach_password = prompt_password(&format!(
        "SSH password for {}@{}:{} (attach {}): ",
        sanitize_display_text(target.endpoint().profile().username()),
        sanitize_display_text(target.endpoint().profile().identity().host()),
        target.endpoint().profile().identity().port(),
        sanitize_display_text(target.name())
    ))?;
    let session = target.attach_with_takeover(
        SshAuthentication::password(attach_password),
        noop_session_event_notifier(),
    )?;
    let mut terminal = wait_for_recovered_terminal(&session, INITIAL_RECOVERY_DEADLINE)?;
    let input = spawn_stdin_reader();

    eprintln!(
        "Attached to {} (pid {}, generation {}). Local commands: /detach, /resize <columns> <rows>.",
        sanitize_display_text(target.name()),
        target.pid(),
        target.generation()
    );

    let mut last_rendered = String::new();
    render_if_changed(&terminal, &mut last_rendered)?;

    loop {
        let (changed, terminal_lifecycle) = pump_session_events(&session, &mut terminal)?;
        let mut detach_requested = false;
        let input_budget = if matches!(session.lifecycle(), SessionLifecycle::Running) {
            MAX_INPUT_EVENTS_PER_TICK
        } else {
            0
        };
        for _ in 0..input_budget {
            match input.try_recv() {
                Ok(StdinEvent::Line(line)) => match parse_local_command(&line)? {
                    LocalCommand::Detach => {
                        eprintln!("Detaching without shutting down the remote daemon.");
                        detach_requested = true;
                        break;
                    }
                    LocalCommand::Resize(size) => {
                        match retry_send("resize", || session.try_resize(size))? {
                            ActionOutcome::Completed => {}
                            ActionOutcome::Dropped => {}
                        }
                    }
                    LocalCommand::Send(line) => {
                        let mut payload = line;
                        payload.push(b'\n');
                        match retry_send("input", || session.try_send_input(&payload))? {
                            ActionOutcome::Completed => {}
                            ActionOutcome::Dropped => {}
                        }
                    }
                },
                Ok(StdinEvent::TooLong { limit }) => {
                    eprintln!(
                        "Input line rejected before send: more than {limit} bytes would exceed the bounded session input limit."
                    );
                }
                Ok(StdinEvent::Eof) => {
                    eprintln!(
                        "Standard input closed; detaching without shutting down the remote daemon."
                    );
                    detach_requested = true;
                    break;
                }
                Ok(StdinEvent::ReadError(error)) => {
                    return Err(format!("standard input reader failed: {error}").into());
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    detach_requested = true;
                    break;
                }
            }
        }

        if changed {
            render_if_changed(&terminal, &mut last_rendered)?;
        }
        if detach_requested {
            break;
        }

        if let Some(lifecycle) = terminal_lifecycle {
            match lifecycle {
                SessionLifecycle::Exited(exit) => {
                    let signal = exit
                        .signal()
                        .map(|signal| format!(", signal {}", sanitize_display_text(signal)))
                        .unwrap_or_default();
                    eprintln!(
                        "Remote session exited with code {}{}.",
                        exit.exit_code(),
                        signal
                    );
                    break;
                }
                SessionLifecycle::Failed(error) => {
                    return Err(format!("remote session failed: {error}").into());
                }
                SessionLifecycle::Disconnected(error) => {
                    return Err(format!(
                        "remote session disconnected: {error}; reconnect is not automatic"
                    )
                    .into());
                }
                SessionLifecycle::Stopped => {
                    eprintln!("Remote session stopped.");
                    break;
                }
                SessionLifecycle::Starting
                | SessionLifecycle::Running
                | SessionLifecycle::Stopping => {}
            }
        }

        if !changed {
            thread::sleep(LOOP_IDLE_SLEEP);
        }
    }

    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Invocation {
    host: String,
    port: u16,
    user: String,
    fingerprint: String,
    helper: String,
    local_address: Option<IpAddr>,
    attach_name: Option<String>,
}

impl Invocation {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, Box<dyn std::error::Error>> {
        let mut local_address = None;
        let mut attach_name = None;
        let mut allow_takeover = false;
        let mut positional = Vec::new();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--local-address" => {
                    if local_address.is_some() {
                        return Err("local source address was supplied more than once".into());
                    }
                    local_address = Some(args.next().ok_or(USAGE)?.parse()?);
                }
                "--attach" => {
                    if attach_name.is_some() {
                        return Err("remote attach target was supplied more than once".into());
                    }
                    let value = args.next().ok_or(USAGE)?;
                    if value.is_empty() {
                        return Err("remote attach target must not be empty".into());
                    }
                    attach_name = Some(value);
                }
                "--allow-takeover" => {
                    allow_takeover = true;
                }
                "--help" | "-h" => return Err(USAGE.into()),
                _ if arg.starts_with("--local-address=") => {
                    if local_address.is_some() {
                        return Err("local source address was supplied more than once".into());
                    }
                    let value = arg.strip_prefix("--local-address=").ok_or(USAGE)?;
                    local_address = Some(value.parse()?);
                }
                _ if arg.starts_with("--attach=") => {
                    if attach_name.is_some() {
                        return Err("remote attach target was supplied more than once".into());
                    }
                    let value = arg.strip_prefix("--attach=").ok_or(USAGE)?;
                    if value.is_empty() {
                        return Err("remote attach target must not be empty".into());
                    }
                    attach_name = Some(value.to_owned());
                }
                _ if arg.starts_with('-') => return Err(USAGE.into()),
                _ => positional.push(arg),
            }
        }

        if allow_takeover && attach_name.is_none() {
            return Err("--allow-takeover requires --attach NAME".into());
        }
        if attach_name.is_some() && !allow_takeover {
            return Err(
                "remote attachment requires --allow-takeover and is rejected before any network connection"
                    .into(),
            );
        }
        if positional.len() != 5 {
            return Err(USAGE.into());
        }

        let host = positional.remove(0);
        let port = positional.remove(0).parse()?;
        let user = positional.remove(0);
        let fingerprint = positional.remove(0);
        let helper = positional.remove(0);

        Ok(Self {
            host,
            port,
            user,
            fingerprint,
            helper,
            local_address,
            attach_name,
        })
    }

    fn build_endpoint(&self) -> Result<RemoteSshEndpoint, Box<dyn std::error::Error>> {
        let profile = SshConnectionProfile::new(
            HostIdentity::new(&self.host, self.port)?,
            self.user.clone(),
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            TerminalSize::new(DEFAULT_COLUMNS, DEFAULT_ROWS)?,
        )?
        .with_local_bind_address(self.local_address)?;
        Ok(RemoteSshEndpoint::new(
            profile,
            self.fingerprint.clone(),
            self.helper.clone(),
        )?)
    }
}

fn prompt_password(prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
    eprint!("{prompt}");
    io::stderr().flush()?;
    Ok(rpassword::read_password()?)
}

fn print_inventory(
    endpoint: &RemoteSshEndpoint,
    inventory: &festerm_sessiond::RemoteSessionInventory,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stdout = io::stdout().lock();
    writeln!(
        stdout,
        "Discovered {} remote session(s) on {}@{}:{} via helper {}.",
        inventory.sessions().len(),
        sanitize_display_text(endpoint.profile().username()),
        sanitize_display_text(endpoint.profile().identity().host()),
        endpoint.profile().identity().port(),
        sanitize_display_text(endpoint.helper())
    )?;
    if inventory.sessions().is_empty() {
        writeln!(stdout, "(none)")?;
        stdout.flush()?;
        return Ok(());
    }
    for session in inventory.sessions() {
        writeln!(
            stdout,
            "- name={} status={} attached={} pid={} generation={} daemon={} snapshot={}",
            sanitize_display_text(&session.name),
            discovery_status_label(session.status),
            option_bool_label(session.attached),
            option_number_label(session.pid),
            option_number_label(session.created_at_unix_ms),
            discovered_version_label(&session.daemon_protocol),
            discovered_version_label(&session.recovery_snapshot_schema),
        )?;
    }
    stdout.flush()?;
    Ok(())
}

fn discovery_status_label(status: festerm_sessiond::DiscoveryStatus) -> &'static str {
    match status {
        festerm_sessiond::DiscoveryStatus::Available => "available",
        festerm_sessiond::DiscoveryStatus::Attached => "attached",
        festerm_sessiond::DiscoveryStatus::Stale => "stale",
        festerm_sessiond::DiscoveryStatus::IncompatibleProtocol => "incompatible_protocol",
        festerm_sessiond::DiscoveryStatus::IncompatibleRecoverySchema => {
            "incompatible_recovery_schema"
        }
        festerm_sessiond::DiscoveryStatus::InvalidIdentity => "invalid_identity",
        festerm_sessiond::DiscoveryStatus::Unreadable => "unreadable",
    }
}

fn discovered_version_label(version: &festerm_sessiond::DiscoveredVersion) -> String {
    let support = if version.supported {
        "supported"
    } else {
        "unsupported"
    };
    match version.version {
        Some(value) => format!("{value} ({support})"),
        None => format!("unknown ({support})"),
    }
}

fn option_bool_label(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

fn option_number_label<T: std::fmt::Display>(value: Option<T>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn wait_for_recovered_terminal(
    session: &festerm_sessiond::PersistentSession,
    deadline: Duration,
) -> Result<Terminal, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + deadline;
    loop {
        if let Some(terminal) = session.take_recovered_terminal() {
            return Ok(terminal);
        }
        match session.lifecycle() {
            SessionLifecycle::Failed(error) => {
                return Err(
                    format!("remote attachment failed before recovery completed: {error}").into(),
                );
            }
            SessionLifecycle::Disconnected(error) => {
                return Err(format!(
                    "remote attachment disconnected before recovery completed: {error}"
                )
                .into());
            }
            SessionLifecycle::Exited(exit) => {
                return Err(format!(
                    "remote session exited before recovery completed (exit code {})",
                    exit.exit_code()
                )
                .into());
            }
            SessionLifecycle::Stopped => {
                return Err("remote session stopped before recovery completed".into());
            }
            SessionLifecycle::Starting | SessionLifecycle::Running | SessionLifecycle::Stopping => {
            }
        }
        if Instant::now() >= deadline {
            return Err("timed out waiting for the initial recovered terminal snapshot".into());
        }
        thread::sleep(LOOP_IDLE_SLEEP);
    }
}

fn pump_session_events(
    session: &festerm_sessiond::PersistentSession,
    terminal: &mut Terminal,
) -> Result<(bool, Option<SessionLifecycle>), Box<dyn std::error::Error>> {
    let mut changed = false;
    for _ in 0..MAX_EVENTS_PER_TICK {
        match session.try_recv_event() {
            Ok(SessionEvent::Output(bytes)) => {
                terminal.ingest(&bytes);
                discard_generated_terminal_transport(terminal);
                changed = true;
            }
            Ok(SessionEvent::RecoverySync(payload)) => {
                let command = decode_recovery_sync_command(&payload)?;
                apply_recovery_sync(terminal, &command);
                changed = true;
            }
            Ok(SessionEvent::ResizeApplied(size)) => {
                if session.terminal_replies_owned_by_backend() {
                    let dimensions =
                        Dimensions::new(usize::from(size.columns()), usize::from(size.rows()))?;
                    terminal.resize(dimensions)?;
                    discard_generated_terminal_transport(terminal);
                    changed = true;
                }
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) => {}
            Ok(SessionEvent::Lifecycle(
                lifecycle @ (SessionLifecycle::Exited(_)
                | SessionLifecycle::Failed(_)
                | SessionLifecycle::Disconnected(_)
                | SessionLifecycle::Stopped),
            )) => {
                return Ok((changed, Some(lifecycle)));
            }
            Ok(SessionEvent::Lifecycle(lifecycle)) => {
                eprintln!(
                    "Session lifecycle changed: {}.",
                    sanitize_display_text(&format!("{lifecycle:?}"))
                );
            }
            Ok(SessionEvent::Backpressure {
                direction,
                queued,
                capacity,
            }) => {
                eprintln!(
                    "Session backpressure: {:?} queue at {queued}/{capacity}.",
                    direction
                );
            }
            Ok(SessionEvent::Error(error)) => {
                eprintln!(
                    "Session error: {}.",
                    sanitize_display_text(&error.to_string())
                );
            }
            Ok(SessionEvent::HostKeyVerification(prompt)) => {
                return Err(format!(
                    "unexpected host-key prompt for {}:{}",
                    prompt.host(),
                    prompt.port()
                )
                .into());
            }
            Ok(SessionEvent::PasswordRequested(prompt)) => {
                return Err(format!(
                    "unexpected password prompt for {}@{}",
                    prompt.username(),
                    prompt.host()
                )
                .into());
            }
            Ok(SessionEvent::PortForwardsUpdated(_)) => {}
            Err(SessionTryReceiveError::Empty) => break,
            Err(SessionTryReceiveError::Closed) => {
                return Err(
                    "remote session event stream closed without a terminal lifecycle".into(),
                );
            }
        }
    }
    Ok((changed, None))
}

fn apply_recovery_sync(terminal: &mut Terminal, command: &RecoverySyncCommand) {
    match command {
        RecoverySyncCommand::SetScrollbackLimit { limit_bytes } => {
            terminal.set_scrollback_limit(*limit_bytes);
        }
        RecoverySyncCommand::SetColorScheme(scheme) => {
            terminal.set_color_scheme(*scheme);
        }
        RecoverySyncCommand::MirrorBytes(bytes) => {
            terminal.ingest(bytes);
        }
        RecoverySyncCommand::ResetToInitialState => {
            terminal.reset_to_initial_state();
        }
    }
    discard_generated_terminal_transport(terminal);
}

fn discard_generated_terminal_transport(terminal: &mut Terminal) {
    let _ = terminal.drain_replies();
    let _ = terminal.take_reply_queue_overflowed();
    let _ = terminal.drain_input();
    let _ = terminal.take_input_queue_overflowed();
}

fn render_if_changed(
    terminal: &Terminal,
    last_rendered: &mut String,
) -> Result<(), Box<dyn std::error::Error>> {
    let rendered = render_visible_screen(terminal);
    if rendered == *last_rendered {
        return Ok(());
    }
    let mut stdout = io::stdout().lock();
    writeln!(
        stdout,
        "----- remote screen {}x{} -----",
        terminal.dimensions().columns(),
        terminal.dimensions().rows()
    )?;
    write!(stdout, "{rendered}")?;
    writeln!(stdout, "----- end screen -----")?;
    stdout.flush()?;
    *last_rendered = rendered;
    Ok(())
}

fn render_visible_screen(terminal: &Terminal) -> String {
    let mut rows = Vec::with_capacity(terminal.dimensions().rows());
    for row in 0..terminal.dimensions().rows() {
        let line = terminal.row_text(row).unwrap_or_default();
        rows.push(trim_trailing_spaces(&sanitize_display_text(&line)));
    }
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    if rows.is_empty() {
        "(blank screen)\n".to_owned()
    } else {
        let mut rendered = rows.join("\n");
        rendered.push('\n');
        rendered
    }
}

fn trim_trailing_spaces(text: &str) -> String {
    text.trim_end_matches(' ').to_owned()
}

fn sanitize_display_text(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => sanitized.push('\n'),
            '\t' => sanitized.push_str("\\t"),
            '\r' => sanitized.push_str("\\r"),
            ch if ch.is_control() => sanitized.push_str(&format!("\\u{{{:04X}}}", ch as u32)),
            ch => sanitized.push(ch),
        }
    }
    sanitized
}

#[derive(Debug)]
enum StdinEvent {
    Line(Vec<u8>),
    TooLong { limit: usize },
    Eof,
    ReadError(String),
}

fn spawn_stdin_reader() -> Receiver<StdinEvent> {
    let (sender, receiver) = mpsc::sync_channel(32);
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = io::BufReader::new(stdin.lock());
        let mut line = Vec::new();
        loop {
            match read_bounded_line(&mut reader, &mut line, MAX_INPUT_LINE_BYTES) {
                Ok(Some(BoundedLine::Line(bytes))) => {
                    if sender.send(StdinEvent::Line(bytes)).is_err() {
                        break;
                    }
                }
                Ok(Some(BoundedLine::TooLong)) => {
                    if sender
                        .send(StdinEvent::TooLong {
                            limit: MAX_INPUT_LINE_BYTES,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(None) => {
                    let _ = sender.send(StdinEvent::Eof);
                    break;
                }
                Err(error) => {
                    let _ = sender.send(StdinEvent::ReadError(error.to_string()));
                    break;
                }
            }
        }
    });
    receiver
}

enum BoundedLine {
    Line(Vec<u8>),
    TooLong,
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
    limit: usize,
) -> io::Result<Option<BoundedLine>> {
    buffer.clear();
    let mut overlong = false;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            if overlong {
                return Ok(Some(BoundedLine::TooLong));
            }
            if buffer.is_empty() {
                return Ok(None);
            }
            return Ok(Some(BoundedLine::Line(strip_trailing_newline(
                buffer.clone(),
            ))));
        }
        let newline = chunk.iter().position(|byte| *byte == b'\n');
        let consume = newline.map_or(chunk.len(), |index| index + 1);
        let segment = &chunk[..consume];
        if !overlong {
            let content_len = segment
                .iter()
                .rposition(|byte| *byte != b'\n')
                .map(|index| index + 1)
                .unwrap_or(0);
            if buffer.len().saturating_add(content_len) > limit {
                overlong = true;
                buffer.clear();
            } else {
                buffer.extend_from_slice(segment);
            }
        }
        reader.consume(consume);
        if newline.is_some() {
            if overlong {
                return Ok(Some(BoundedLine::TooLong));
            }
            return Ok(Some(BoundedLine::Line(strip_trailing_newline(
                buffer.clone(),
            ))));
        }
    }
}

fn strip_trailing_newline(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    bytes
}

enum LocalCommand {
    Detach,
    Resize(TerminalSize),
    Send(Vec<u8>),
}

fn parse_local_command(line: &[u8]) -> Result<LocalCommand, Box<dyn std::error::Error>> {
    let trimmed = strip_trailing_newline(line.to_vec());
    let Ok(text) = std::str::from_utf8(&trimmed) else {
        return Ok(LocalCommand::Send(trimmed));
    };
    let text = text.trim();
    if text == "/detach" {
        return Ok(LocalCommand::Detach);
    }
    if text == "/resize" || text.starts_with("/resize ") {
        let rest = text.strip_prefix("/resize").expect("prefix just checked");
        let mut parts = rest.split_whitespace();
        let columns = parts
            .next()
            .ok_or("usage: /resize <columns> <rows>")?
            .parse()?;
        let rows = parts
            .next()
            .ok_or("usage: /resize <columns> <rows>")?
            .parse()?;
        if parts.next().is_some() {
            return Err("usage: /resize <columns> <rows>".into());
        }
        return Ok(LocalCommand::Resize(TerminalSize::new(columns, rows)?));
    }
    Ok(LocalCommand::Send(trimmed))
}

enum ActionOutcome {
    Completed,
    Dropped,
}

fn retry_send(
    label: &str,
    mut send: impl FnMut() -> Result<(), SessionSendError>,
) -> Result<ActionOutcome, Box<dyn std::error::Error>> {
    let mut saw_full = false;
    for attempt in 0..=MAX_SEND_RETRIES {
        match send() {
            Ok(()) => {
                if saw_full {
                    eprintln!("{label} accepted after bounded retry.");
                }
                return Ok(ActionOutcome::Completed);
            }
            Err(SessionSendError::Full { capacity, .. }) if attempt < MAX_SEND_RETRIES => {
                if !saw_full {
                    eprintln!("{label} queue is full ({capacity}); retrying.");
                    saw_full = true;
                }
                thread::sleep(RETRY_SLEEP);
            }
            Err(SessionSendError::Full { capacity, .. }) => {
                eprintln!("{label} dropped after bounded retries because the queue remained full ({capacity}).");
                return Ok(ActionOutcome::Dropped);
            }
            Err(SessionSendError::TooLarge {
                maximum, actual, ..
            }) => {
                eprintln!(
                    "{label} rejected because {actual} bytes exceeds the {maximum}-byte limit."
                );
                return Ok(ActionOutcome::Dropped);
            }
            Err(SessionSendError::Closed { operation }) => {
                return Err(
                    format!("session is closed; {label} for {operation:?} was not sent").into(),
                );
            }
        }
    }
    unreachable!("every bounded send attempt either retries or returns")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invocation_parses_list_mode_with_optional_source_address() {
        let invocation = Invocation::parse(
            [
                "--local-address",
                "127.0.0.1",
                "host.example",
                "22",
                "alice",
                "SHA256:abc123",
                "festerm-sessiond",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(invocation.host, "host.example");
        assert_eq!(invocation.port, 22);
        assert_eq!(invocation.user, "alice");
        assert_eq!(invocation.local_address, Some("127.0.0.1".parse().unwrap()));
        assert_eq!(invocation.attach_name, None);
    }

    #[test]
    fn invocation_rejects_attach_without_explicit_takeover() {
        let error = Invocation::parse(
            [
                "--attach",
                "work",
                "host.example",
                "22",
                "alice",
                "SHA256:abc123",
                "festerm-sessiond",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap_err();
        assert!(error.to_string().contains("--allow-takeover"));
    }

    #[test]
    fn invocation_rejects_takeover_without_attach() {
        let error = Invocation::parse(
            [
                "--allow-takeover",
                "host.example",
                "22",
                "alice",
                "SHA256:abc123",
                "festerm-sessiond",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap_err();
        assert!(error.to_string().contains("--attach"));
    }

    #[test]
    fn local_command_parser_reserves_detach_and_resize() {
        assert!(matches!(
            parse_local_command(b"/detach").unwrap(),
            LocalCommand::Detach
        ));
        assert!(matches!(
            parse_local_command(b"/resize 90 30").unwrap(),
            LocalCommand::Resize(size) if size == TerminalSize::new(90, 30).unwrap()
        ));
        assert!(matches!(
            parse_local_command(b"/not-local").unwrap(),
            LocalCommand::Send(bytes) if bytes == b"/not-local"
        ));
    }

    #[test]
    fn closed_session_input_is_an_error_not_successful_detach() {
        let error = retry_send("input", || {
            Err(SessionSendError::Closed {
                operation: festerm_session::SessionOperation::Input,
            })
        })
        .err()
        .expect("closed sends must fail");
        assert!(error.to_string().contains("was not sent"));
    }

    #[test]
    fn bounded_line_reader_rejects_without_truncating_prefix() {
        let input = format!("{}\nshort\n", "x".repeat(MAX_INPUT_LINE_BYTES + 1));
        let mut reader = io::Cursor::new(input.into_bytes());
        let mut scratch = Vec::new();
        assert!(matches!(
            read_bounded_line(&mut reader, &mut scratch, MAX_INPUT_LINE_BYTES).unwrap(),
            Some(BoundedLine::TooLong)
        ));
        assert!(matches!(
            read_bounded_line(&mut reader, &mut scratch, MAX_INPUT_LINE_BYTES).unwrap(),
            Some(BoundedLine::Line(line)) if line == b"short"
        ));
    }

    #[test]
    fn renderer_projects_text_without_remote_escape_sequences() {
        let mut terminal = Terminal::new(Dimensions::new(10, 3).unwrap()).unwrap();
        terminal.ingest(b"hello\x1b[6n");
        terminal.ingest(b"\x1b]52;c;SGVsbG8=\x07");
        terminal.ingest(b"\nworld");
        let rendered = render_visible_screen(&terminal);
        assert!(rendered.contains("hello"));
        assert!(rendered.contains("world"));
        assert!(!rendered.contains('\u{001b}'));
        assert!(!rendered.contains("SGVsbG8="));
    }

    #[test]
    fn sanitize_display_text_escapes_controls() {
        assert_eq!(
            sanitize_display_text("bad\tline\r\x1b"),
            "bad\\tline\\r\\u{001B}"
        );
    }

    #[test]
    fn apply_recovery_sync_payloads_in_order() {
        let mut terminal = Terminal::new(Dimensions::new(8, 2).unwrap()).unwrap();
        apply_recovery_sync(
            &mut terminal,
            &decode_recovery_sync_command(
                &festerm_sessiond::encode_recovery_sync_command(&RecoverySyncCommand::MirrorBytes(
                    b"before".to_vec(),
                ))
                .unwrap(),
            )
            .unwrap(),
        );
        apply_recovery_sync(
            &mut terminal,
            &decode_recovery_sync_command(
                &festerm_sessiond::encode_recovery_sync_command(
                    &RecoverySyncCommand::ResetToInitialState,
                )
                .unwrap(),
            )
            .unwrap(),
        );
        apply_recovery_sync(
            &mut terminal,
            &decode_recovery_sync_command(
                &festerm_sessiond::encode_recovery_sync_command(&RecoverySyncCommand::MirrorBytes(
                    b"after".to_vec(),
                ))
                .unwrap(),
            )
            .unwrap(),
        );
        assert!(render_visible_screen(&terminal).contains("after"));
        assert!(!render_visible_screen(&terminal).contains("before"));
    }
}
