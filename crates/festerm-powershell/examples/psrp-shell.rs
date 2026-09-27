use std::env;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use festerm_powershell::{
    value_to_display_text, PowerShellCommandMessage, PowerShellCredentials, PowerShellEndpoint,
    PowerShellOptions, PowerShellSession,
};
use festerm_secret_store::SecretBytes;
use psrp_rs::{PipelineEvent, PsValue};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputStream {
    Stdout,
    Stderr,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let host = args
        .next()
        .ok_or("usage: psrp-shell <host> <username> [domain] [ca-pem-file]")?;
    let username = args
        .next()
        .ok_or("usage: psrp-shell <host> <username> [domain] [ca-pem-file]")?;
    let domain = args.next();
    let ca_pem_file = args.next().map(PathBuf::from);

    let mut endpoint = PowerShellEndpoint::https(host)?;
    if let Some(path) = ca_pem_file {
        endpoint = endpoint.with_trusted_ca_pem(std::fs::read_to_string(path)?)?;
    }

    eprint!("Password: ");
    io::stderr().flush()?;
    let password = rpassword::read_password()?;
    let credentials = PowerShellCredentials::with_domain(
        username,
        domain,
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
