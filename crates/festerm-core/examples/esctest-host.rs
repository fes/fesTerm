//! Hosts a child process on a pty and answers it with a `festerm-core`
//! `Terminal`, so the esctest2 conformance suite can be pointed at our
//! terminal model without a GUI.
//!
//! esctest2 does not read a stream we hand it. `escio.Init()` puts *its own
//! stdin* into raw mode and drives the terminal it is running inside: it
//! writes escape sequences to stdout and reads the terminal's replies back
//! from stdin. So conformance-testing `festerm-core` means being the
//! terminal on the other end of a pty, not feeding a parser from a file.
//! That is what this example is: read the child's output into `Terminal`,
//! write whatever the terminal wants to say back into the pty.
//!
//! Usage:
//!
//! ```text
//! cargo run -p festerm-core --example esctest-host -- \
//!     [--columns N] [--rows N] [--timeout-secs N] [--transcript PATH] \
//!     -- python3 esctest.py --include '...'
//! ```
//!
//! The process exits with the child's exit code, or `124` if the timeout
//! elapsed first, so a shell harness can branch on it.

use std::env;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use festerm_core::{Dimensions, Terminal};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

/// The geometry esctest2 assumes unless told otherwise: its per-test `reset()`
/// asks xterm to resize to 25x80 and then reads the size back, so starting
/// anywhere else just means every test disagrees with us about the screen.
const DEFAULT_COLUMNS: u16 = 80;
const DEFAULT_ROWS: u16 = 25;

/// A whole esctest2 run is minutes, not seconds, but it must not be able to
/// hang a CI job forever if the suite blocks on a reply we never send.
const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// The exit code `timeout(1)` uses, for the same meaning.
const TIMEOUT_EXIT_CODE: u8 = 124;

struct Options {
    columns: u16,
    rows: u16,
    timeout: Duration,
    transcript: Option<PathBuf>,
    command: Vec<String>,
}

fn main() -> ExitCode {
    let options = match parse_options(env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("esctest-host: {message}");
            eprintln!(
                "usage: esctest-host [--columns N] [--rows N] [--timeout-secs N] \
                 [--transcript PATH] -- <command> [args...]"
            );
            return ExitCode::from(2);
        }
    };

    match run(&options) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("esctest-host: {error}");
            ExitCode::from(1)
        }
    }
}

fn parse_options<I: Iterator<Item = String>>(mut arguments: I) -> Result<Options, String> {
    let mut options = Options {
        columns: DEFAULT_COLUMNS,
        rows: DEFAULT_ROWS,
        timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        transcript: None,
        command: Vec::new(),
    };

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--" => {
                options.command.extend(arguments);
                break;
            }
            "--columns" => options.columns = parse_value(&mut arguments, "--columns")?,
            "--rows" => options.rows = parse_value(&mut arguments, "--rows")?,
            "--timeout-secs" => {
                options.timeout =
                    Duration::from_secs(parse_value(&mut arguments, "--timeout-secs")?);
            }
            "--transcript" => {
                options.transcript = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| "--transcript needs a path".to_owned())?,
                ));
            }
            other => return Err(format!("unrecognised argument {other}")),
        }
    }

    if options.command.is_empty() {
        return Err("no command given; pass it after `--`".to_owned());
    }
    Ok(options)
}

fn parse_value<T: std::str::FromStr, I: Iterator<Item = String>>(
    arguments: &mut I,
    name: &str,
) -> Result<T, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{name} needs a value"))?
        .parse()
        .map_err(|_| format!("{name} needs a number"))
}

fn run(options: &Options) -> Result<u8, String> {
    let dimensions = Dimensions::new(usize::from(options.columns), usize::from(options.rows))
        .map_err(|error| format!("bad screen size: {error}"))?;
    let mut terminal = Terminal::new(dimensions).map_err(|error| format!("{error}"))?;

    let pty = native_pty_system()
        .openpty(PtySize {
            rows: options.rows,
            cols: options.columns,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("could not open a pty: {error}"))?;

    let mut command = CommandBuilder::new(&options.command[0]);
    command.args(&options.command[1..]);
    // esctest2 speaks raw escape sequences rather than going through
    // terminfo, but anything it shells out to should still see a sane TERM.
    command.env("TERM", "xterm-256color");
    if let Ok(directory) = env::current_dir() {
        command.cwd(directory);
    }

    let mut child = pty
        .slave
        .spawn_command(command)
        .map_err(|error| format!("could not start {}: {error}", options.command[0]))?;
    drop(pty.slave);

    let mut writer = pty
        .master
        .take_writer()
        .map_err(|error| format!("could not write to the pty: {error}"))?;
    let mut reader = pty
        .master
        .try_clone_reader()
        .map_err(|error| format!("could not read from the pty: {error}"))?;

    let (sender, receiver) = mpsc::channel::<Vec<u8>>();
    let pump = thread::spawn(move || {
        let mut buffer = [0_u8; 8_192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if sender.send(buffer[..count].to_vec()).is_err() {
                        break;
                    }
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });

    let mut transcript = match &options.transcript {
        Some(path) => Some(
            File::create(path).map_err(|error| format!("could not write a transcript: {error}"))?,
        ),
        None => None,
    };

    let deadline = Instant::now() + options.timeout;
    let mut timed_out = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            timed_out = true;
            break;
        }
        match receiver.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(bytes) => {
                if let Some(file) = transcript.as_mut() {
                    file.write_all(&bytes)
                        .map_err(|error| format!("could not write a transcript: {error}"))?;
                }
                terminal.ingest(&bytes);
                let replies = terminal.drain_replies();
                if !replies.is_empty() {
                    writer
                        .write_all(&replies)
                        .and_then(|()| writer.flush())
                        .map_err(|error| format!("could not answer the child: {error}"))?;
                }
                if terminal.take_reply_queue_overflowed() {
                    return Err("the reply queue overflowed; the child is not reading".to_owned());
                }
            }
            // The pty stayed quiet, which is normal between tests. Only the
            // channel hanging up means the child is gone.
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    if timed_out {
        let _ = child.kill();
        let _ = child.wait();
        drop(pty.master);
        let _ = pump.join();
        eprintln!(
            "esctest-host: timed out after {} seconds",
            options.timeout.as_secs()
        );
        return Ok(TIMEOUT_EXIT_CODE);
    }

    let status = child
        .wait()
        .map_err(|error| format!("could not wait for the child: {error}"))?;
    drop(pty.master);
    let _ = pump.join();

    if let Some(file) = transcript.as_mut() {
        file.flush()
            .map_err(|error| format!("could not write a transcript: {error}"))?;
    }

    Ok(u8::try_from(status.exit_code()).unwrap_or(1))
}
