use std::env;
use std::io::{self, BufRead, Read, Write};
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use festerm_powershell::{
    value_to_display_text, PipelineEvent, PowerShellCommandMessage, PowerShellCredentials,
    PowerShellEndpoint, PowerShellOptions, PowerShellSession, PsValue,
};
use festerm_secret_store::SecretBytes;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputStream {
    Stdout,
    Stderr,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let invocation = Invocation::parse(env::args().skip(1))?;

    let mut endpoint = PowerShellEndpoint::https(invocation.host)?;
    if let Some(name) = invocation.configuration_name {
        endpoint = endpoint.with_configuration_name(name)?;
    }
    if let Some(path) = invocation.ca_pem_file {
        endpoint = endpoint.with_trusted_ca_pem(std::fs::read_to_string(path)?)?;
    }
    let local_bind_address = match (invocation.local_address, invocation.ask_local_address) {
        (Some(address), _) => Some(address),
        (None, true) => prompt_local_address()?,
        (None, false) => None,
    };
    endpoint = endpoint.with_local_bind_address(local_bind_address)?;

    eprint!("Password: ");
    io::stderr().flush()?;
    let password = rpassword::read_password()?;
    let credentials = PowerShellCredentials::with_domain(
        invocation.username,
        invocation.domain,
        SecretBytes::from_secret_string(password),
    )?;
    let session = PowerShellSession::connect(endpoint, PowerShellOptions::default(), credentials)?;

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    eprintln!("Connected. Enter PowerShell script lines, or 'exit'.");
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().eq_ignore_ascii_case("exit") {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let mut command = session.start_script(line)?;
        while let Some(message) = command.recv_timeout(Duration::from_secs(60))? {
            match message {
                PowerShellCommandMessage::Event(event) => {
                    if let Some((stream, line)) = format_pipeline_event(&event) {
                        match stream {
                            OutputStream::Stdout => writeln!(stdout, "{line}")?,
                            OutputStream::Stderr => writeln!(stderr, "{line}")?,
                        }
                    }
                }
                PowerShellCommandMessage::Completed { state } => {
                    writeln!(stderr, "Pipeline finished with state {state:?}")?;
                }
                PowerShellCommandMessage::Failed(failure) => {
                    writeln!(stderr, "Pipeline failed: {:?}", failure.kind)?;
                    break;
                }
            }
        }
    }

    session.close()?;
    Ok(())
}

struct Invocation {
    host: String,
    username: String,
    domain: Option<String>,
    ca_pem_file: Option<PathBuf>,
    local_address: Option<IpAddr>,
    ask_local_address: bool,
    configuration_name: Option<String>,
}

impl Invocation {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, Box<dyn std::error::Error>> {
        let mut local_address = None;
        let mut ask_local_address = false;
        let mut configuration_name = None;
        let mut positional = Vec::new();
        let mut args = args;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--local-address" => {
                    if local_address.is_some() {
                        return Err("local source address was supplied more than once".into());
                    }
                    let value = args.next().ok_or(USAGE)?;
                    local_address = Some(value.parse()?);
                }
                "--ask-local-address" => {
                    if ask_local_address {
                        return Err("local source prompt was requested more than once".into());
                    }
                    ask_local_address = true;
                }
                "--configuration" => {
                    if configuration_name.is_some() {
                        return Err("configuration name was supplied more than once".into());
                    }
                    let value = args.next().ok_or(USAGE)?;
                    configuration_name = Some(validate_configuration(&value)?);
                }
                "--help" | "-h" => return Err(USAGE.into()),
                _ if arg.starts_with("--local-address=") => {
                    if local_address.is_some() {
                        return Err("local source address was supplied more than once".into());
                    }
                    let value = arg.strip_prefix("--local-address=").ok_or(USAGE)?;
                    local_address = Some(value.parse()?);
                }
                _ if arg.starts_with("--configuration=") => {
                    if configuration_name.is_some() {
                        return Err("configuration name was supplied more than once".into());
                    }
                    let value = arg.strip_prefix("--configuration=").ok_or(USAGE)?;
                    configuration_name = Some(validate_configuration(value)?);
                }
                _ if arg.starts_with('-') => return Err(USAGE.into()),
                _ => positional.push(arg),
            }
        }
        if local_address.is_some() && ask_local_address {
            return Err("choose either --local-address or --ask-local-address, not both".into());
        }
        let mut positional = positional.into_iter();
        let host = positional.next().ok_or(USAGE)?;
        let username = positional.next().ok_or(USAGE)?;
        let domain = positional.next();
        let ca_pem_file = positional.next().map(PathBuf::from);
        if positional.next().is_some() {
            return Err(USAGE.into());
        }
        Ok(Self {
            host,
            username,
            domain,
            ca_pem_file,
            local_address,
            ask_local_address,
            configuration_name,
        })
    }
}

const USAGE: &str = "usage: psrp-shell [--local-address IP | --ask-local-address] [--configuration NAME] <host> <username> [domain] [ca-pem-file]";

fn validate_configuration(value: &str) -> Result<String, Box<dyn std::error::Error>> {
    // Validate at parse time by reusing the public endpoint builder, so a
    // malformed name (empty, out-of-charset, a flag mistaken for a value, or a
    // full resource URI) is rejected before any credential prompt or network
    // use. The returned normalized name is stored for the eventual endpoint.
    let endpoint = PowerShellEndpoint::https("configuration-check.invalid")
        .and_then(|endpoint| endpoint.with_configuration_name(value))?;
    Ok(endpoint.configuration_name().to_string())
}

fn prompt_local_address() -> Result<Option<IpAddr>, Box<dyn std::error::Error>> {
    eprintln!("Source address only; this does not enforce an adapter, VPN path, or DNS policy.");
    eprint!("Local source IP (blank for Automatic): ");
    io::stderr().flush()?;
    read_local_address(&mut io::stdin().lock())
}

fn read_local_address(
    reader: &mut impl BufRead,
) -> Result<Option<IpAddr>, Box<dyn std::error::Error>> {
    let mut input = String::new();
    let length = reader.take(128).read_line(&mut input)?;
    if length == 0 {
        return Err("local source selection cancelled: input closed".into());
    }
    if length >= 128 {
        return Err("local source address input is too long".into());
    }
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.parse()?))
    }
}

fn format_pipeline_event(event: &PipelineEvent) -> Option<(OutputStream, String)> {
    match event {
        PipelineEvent::Output(value) => Some((OutputStream::Stdout, render_value(value))),
        PipelineEvent::Error(value) => Some((
            OutputStream::Stderr,
            format!("ERROR: {}", render_value(value)),
        )),
        PipelineEvent::Warning(value) => Some((
            OutputStream::Stderr,
            format!("WARNING: {}", render_value(value)),
        )),
        PipelineEvent::Verbose(value) => Some((
            OutputStream::Stderr,
            format!("VERBOSE: {}", render_value(value)),
        )),
        PipelineEvent::Debug(value) => Some((
            OutputStream::Stderr,
            format!("DEBUG: {}", render_value(value)),
        )),
        PipelineEvent::Information(value) => Some((
            OutputStream::Stderr,
            format!("INFO: {}", render_value(value)),
        )),
        PipelineEvent::Progress(value) => Some((
            OutputStream::Stderr,
            format!("PROGRESS: {}", render_value(value)),
        )),
        PipelineEvent::State(_) => None,
    }
}

fn render_value(value: &PsValue) -> String {
    escape_terminal_controls(&value_to_display_text(value))
}

fn escape_terminal_controls(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        let code = ch as u32;
        if code <= 0x1f || (0x7f..=0x9f).contains(&code) {
            if code <= 0xff {
                escaped.push_str(&format!("\\x{code:02X}"));
            } else {
                escaped.push_str(&format!("\\u{{{code:X}}}"));
            }
        } else {
            escaped.push(ch);
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psrp_local_bind_cli_preserves_positionals_and_parses_explicit_choice() {
        let invocation = Invocation::parse(
            ["server.test", "user", "domain", "ca.pem"]
                .map(str::to_owned)
                .into_iter(),
        )
        .unwrap();
        assert_eq!(invocation.host, "server.test");
        assert_eq!(invocation.username, "user");
        assert_eq!(invocation.domain.as_deref(), Some("domain"));
        assert_eq!(invocation.ca_pem_file, Some(PathBuf::from("ca.pem")));
        assert_eq!(invocation.local_address, None);
        assert!(!invocation.ask_local_address);
        for flag in ["--local-address=127.0.0.1", "--local-address=::1"] {
            let invocation =
                Invocation::parse([flag, "server.test", "user"].map(str::to_owned).into_iter())
                    .unwrap();
            assert!(invocation.local_address.unwrap().is_loopback());
        }
        let invocation = Invocation::parse(
            ["--ask-local-address", "server.test", "user"]
                .map(str::to_owned)
                .into_iter(),
        )
        .unwrap();
        assert!(invocation.ask_local_address);
        assert_eq!(invocation.local_address, None);
        assert_eq!(invocation.configuration_name, None);
    }

    #[test]
    fn psrp_configuration_cli_selects_endpoint_and_preserves_positionals() {
        for flag in ["--configuration", "--configuration=PowerShell.7"] {
            let args: Vec<String> = if flag == "--configuration" {
                vec!["--configuration", "PowerShell.7", "server.test", "user"]
            } else {
                vec![flag, "server.test", "user"]
            }
            .into_iter()
            .map(str::to_owned)
            .collect();
            let invocation = Invocation::parse(args.into_iter()).unwrap();
            assert_eq!(invocation.host, "server.test");
            assert_eq!(invocation.username, "user");
            assert_eq!(
                invocation.configuration_name.as_deref(),
                Some("PowerShell.7")
            );
        }

        let invocation = Invocation::parse(
            [
                "--configuration",
                "Microsoft.PowerShell",
                "--local-address=127.0.0.1",
                "server.test",
                "user",
                "domain",
                "ca.pem",
            ]
            .map(str::to_owned)
            .into_iter(),
        )
        .unwrap();
        assert_eq!(
            invocation.configuration_name.as_deref(),
            Some("Microsoft.PowerShell")
        );
        assert!(invocation.local_address.unwrap().is_loopback());
        assert_eq!(invocation.domain.as_deref(), Some("domain"));
        assert_eq!(invocation.ca_pem_file, Some(PathBuf::from("ca.pem")));
    }

    #[test]
    fn psrp_configuration_cli_rejects_missing_or_duplicate_flag() {
        for flags in [
            vec!["--configuration"],
            vec!["--configuration", "A", "--configuration", "B"],
            vec!["--configuration=A", "--configuration=B"],
        ] {
            assert!(Invocation::parse(
                ["server.test", "user"]
                    .into_iter()
                    .chain(flags)
                    .map(str::to_owned)
            )
            .is_err());
        }
    }

    #[test]
    fn psrp_configuration_cli_rejects_malformed_values_at_parse_time() {
        for flags in [
            vec!["--configuration", ""],
            vec!["--configuration", " "],
            vec!["--configuration", ".leading-dot"],
            vec!["--configuration", "bad name"],
            vec![
                "--configuration",
                "http://schemas.microsoft.com/powershell/X",
            ],
            vec!["--configuration", "--local-address"],
            vec!["--configuration=", ""],
            vec!["--configuration=--ask-local-address"],
        ] {
            assert!(
                Invocation::parse(
                    ["server.test", "user"]
                        .into_iter()
                        .chain(flags.iter().copied())
                        .map(str::to_owned)
                )
                .is_err(),
                "expected parse failure for {flags:?}"
            );
        }
    }

    #[test]
    fn psrp_local_bind_cli_rejects_conflicting_or_malformed_options() {
        for flags in [
            vec!["--local-address"],
            vec!["--local-address", "not-an-ip"],
            vec!["--local-address=127.0.0.1", "--ask-local-address"],
            vec!["--local-address=127.0.0.1", "--local-address", "::1"],
            vec!["--ask-local-address", "--ask-local-address"],
            vec!["--local-address=--local-address=127.0.0.1"],
        ] {
            assert!(Invocation::parse(
                ["server.test", "user"]
                    .into_iter()
                    .chain(flags)
                    .map(str::to_owned)
            )
            .is_err());
        }
    }

    #[test]
    fn psrp_local_bind_prompt_requires_explicit_input_and_bounds_it() {
        assert!(read_local_address(&mut io::Cursor::new(b"")).is_err());
        assert_eq!(
            read_local_address(&mut io::Cursor::new(b"\n")).unwrap(),
            None
        );
        assert_eq!(
            read_local_address(&mut io::Cursor::new(b"127.0.0.1\n")).unwrap(),
            Some(IpAddr::from([127, 0, 0, 1]))
        );
        assert!(read_local_address(&mut io::Cursor::new(b"not-an-ip\n")).is_err());
        assert!(read_local_address(&mut io::Cursor::new(vec![b' '; 256])).is_err());
    }

    #[test]
    fn every_stream_variant_escapes_terminal_controls() {
        let value = PsValue::String("ok\u{1b}\n\u{9b}\u{7f}".to_string());
        let variants = [
            PipelineEvent::Output(value.clone()),
            PipelineEvent::Error(value.clone()),
            PipelineEvent::Warning(value.clone()),
            PipelineEvent::Verbose(value.clone()),
            PipelineEvent::Debug(value.clone()),
            PipelineEvent::Information(value.clone()),
            PipelineEvent::Progress(value),
        ];

        for event in variants {
            let (_, rendered) = format_pipeline_event(&event).expect("rendered stream event");
            assert!(rendered.contains("ok\\x1B\\x0A\\x9B\\x7F"));
            assert!(!rendered.contains('\u{1b}'));
            assert!(!rendered.contains('\n'));
            assert!(!rendered.contains('\u{9b}'));
            assert!(!rendered.contains('\u{7f}'));
        }
    }
}
