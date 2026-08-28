//! Deterministic PTY test child for `festerm-pty` integration tests.
//!
//! Launched through a real PTY (Unix or Windows ConPTY) and driven by an
//! ordered sequence of protocol commands supplied as plain argv arguments.
//! No shell interpolation is involved; callers construct each argument
//! directly.
//!
//! # Protocol
//!
//! Each argument is one command, executed in order:
//!
//! | Command | Action |
//! |---------|--------|
//! | `emit:TEXT` | Write `TEXT\n` to stdout. |
//! | `emit-bytes:COUNT:MARKER` | Write `COUNT` `x` bytes followed by `\nMARKER\n`. |
//! | `emit-frames:COUNT:MILLIS` | Write `FRAME:00` through `FRAME:COUNT-1`, pausing `MILLIS` between lines. |
//! | `report-env:NAME` | Write `ENV:NAME=VALUE\n`, or `<unset>` when absent. |
//! | `read-line` | Read one line from stdin; strip trailing CR/LF. |
//! | `read-until-enter` | Read raw bytes through CR or LF and retain the preceding bytes. |
//! | `echo:PREFIX` | Write `PREFIX:{last-line}\n` to stdout. |
//! | `echo-hex:PREFIX` | Write `PREFIX:{last-line bytes as lowercase hex}\n` to stdout. |
//! | `set-raw-input` | On Windows, disable console line editing and echo. |
//! | `report-size` | Write `{rows} {cols}\n` (PTY dimensions) to stdout. |
//! | `spin` | Sleep until the process is killed. |
//! | `spawn` | Spawn self as a long-running descendant, write `CHILD:{pid}\n`, then wait for it. |
//! | `exit:N` | Exit with decimal code N. |

use std::{
    io::{BufRead, Write},
    process, thread,
    time::Duration,
};

use terminal_size::{terminal_size, Height, Width};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut last_line = String::new();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();

    for arg in &args {
        if let Some(text) = arg.strip_prefix("emit:") {
            let mut out = stdout.lock();
            out.write_all(text.as_bytes())
                .expect("emit: stdout write succeeds");
            out.write_all(b"\n").expect("emit: stdout newline succeeds");
            out.flush().expect("emit: stdout flush succeeds");
        } else if let Some(specification) = arg.strip_prefix("emit-bytes:") {
            let (count, marker) = specification.split_once(':').unwrap_or_else(|| {
                panic!("emit-bytes argument must be emit-bytes:COUNT:MARKER, got {arg:?}")
            });
            let count = count.parse::<usize>().unwrap_or_else(|_| {
                panic!("emit-bytes argument must contain a byte count, got {arg:?}")
            });
            let mut out = stdout.lock();
            let block = [b'x'; 4096];
            let mut remaining = count;
            while remaining > 0 {
                let chunk = remaining.min(block.len());
                out.write_all(&block[..chunk])
                    .expect("emit-bytes: stdout write succeeds");
                remaining -= chunk;
            }
            writeln!(out, "\n{marker}").expect("emit-bytes: marker write succeeds");
            out.flush().expect("emit-bytes: stdout flush succeeds");
        } else if let Some(specification) = arg.strip_prefix("emit-frames:") {
            let (count, interval_millis) = specification
                .split_once(':')
                .and_then(|(count, interval)| {
                    Some((count.parse::<usize>().ok()?, interval.parse::<u64>().ok()?))
                })
                .unwrap_or_else(|| {
                    panic!("emit-frames argument must be emit-frames:COUNT:MILLIS, got {arg:?}")
                });
            let mut out = stdout.lock();
            for index in 0..count {
                writeln!(out, "FRAME:{index:02}").expect("emit-frames: stdout write succeeds");
                out.flush().expect("emit-frames: stdout flush succeeds");
                thread::sleep(Duration::from_millis(interval_millis));
            }
        } else if let Some(name) = arg.strip_prefix("report-env:") {
            let value = std::env::var(name).unwrap_or_else(|_| "<unset>".to_owned());
            let mut out = stdout.lock();
            writeln!(out, "ENV:{name}={value}").expect("report-env: stdout write succeeds");
            out.flush().expect("report-env: stdout flush succeeds");
        } else if arg == "read-line" {
            last_line.clear();
            stdin
                .lock()
                .read_line(&mut last_line)
                .expect("read-line: stdin read succeeds");
            // Strip any trailing CR or LF so echo output is clean.
            let trimmed = last_line.trim_end_matches(['\r', '\n']);
            last_line = trimmed.to_owned();
        } else if arg == "read-until-enter" {
            last_line = read_until_enter(&stdin);
        } else if let Some(prefix) = arg.strip_prefix("echo:") {
            let mut out = stdout.lock();
            writeln!(out, "{prefix}:{last_line}").expect("echo: stdout write succeeds");
            out.flush().expect("echo: stdout flush succeeds");
        } else if let Some(prefix) = arg.strip_prefix("echo-hex:") {
            let hex = last_line
                .as_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let mut out = stdout.lock();
            writeln!(out, "{prefix}:{hex}").expect("echo-hex: stdout write succeeds");
            out.flush().expect("echo-hex: stdout flush succeeds");
        } else if arg == "set-raw-input" {
            set_raw_input();
        } else if arg == "report-size" {
            let (Width(cols), Height(rows)) =
                terminal_size().expect("report-size: PTY provides terminal dimensions");
            let mut out = stdout.lock();
            writeln!(out, "{rows} {cols}").expect("report-size: stdout write succeeds");
            out.flush().expect("report-size: stdout flush succeeds");
        } else if arg == "spin" {
            // Sleep until the process is killed by session shutdown.
            loop {
                thread::sleep(Duration::from_secs(3600));
            }
        } else if arg == "spawn" {
            // Spawn a long-running descendant of this process, announce its
            // PID, then wait for it.  The descendant inherits the process
            // group (Unix) or Job Object (Windows), so session shutdown
            // terminates the whole tree.
            let self_exe =
                std::env::current_exe().expect("spawn: current executable path is accessible");
            let mut child = process::Command::new(&self_exe)
                .arg("spin")
                .spawn()
                .expect("spawn: descendant process starts");
            let pid = child.id();
            {
                let mut out = stdout.lock();
                writeln!(out, "CHILD:{pid}").expect("spawn: stdout write succeeds");
                out.flush().expect("spawn: stdout flush succeeds");
            }
            // Wait for the child so we remain alive while it is running.
            let _ = child.wait();
        } else if let Some(code_str) = arg.strip_prefix("exit:") {
            let code: i32 = code_str.parse().unwrap_or_else(|_| {
                panic!("exit: argument must be a decimal integer, got {code_str:?}")
            });
            process::exit(code);
        } else {
            eprintln!("festerm-pty-test-child: unknown command: {arg:?}");
            process::exit(1);
        }
    }

    #[cfg(windows)]
    fn set_raw_input() {
        crossterm::terminal::enable_raw_mode().expect("set-raw-input: console mode is writable");
    }

    #[cfg(not(windows))]
    fn set_raw_input() {
        panic!("set-raw-input is supported only on Windows");
    }
}

#[cfg(not(windows))]
fn read_until_enter(stdin: &std::io::Stdin) -> String {
    use std::io::Read as _;

    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stdin
            .lock()
            .read_exact(&mut byte)
            .expect("read-until-enter: stdin read succeeds");
        if matches!(byte[0], b'\r' | b'\n') {
            break;
        }
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).expect("read-until-enter: input is valid UTF-8")
}

#[cfg(windows)]
fn read_until_enter(_stdin: &std::io::Stdin) -> String {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind};

    let mut value = String::new();
    loop {
        let Event::Key(key) = event::read().expect("read-until-enter: console event read succeeds")
        else {
            continue;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            continue;
        }
        match key.code {
            KeyCode::Enter => return value,
            KeyCode::Tab => value.push('\t'),
            KeyCode::Up => value.push_str("\x1b[A"),
            KeyCode::Char(character) => value.push(character),
            _ => {}
        }
    }
}
