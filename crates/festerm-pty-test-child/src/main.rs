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
//! | `read-line` | Read one line from stdin; strip trailing CR/LF. |
//! | `echo:PREFIX` | Write `PREFIX:{last-line}\n` to stdout. |
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
        } else if arg == "read-line" {
            last_line.clear();
            stdin
                .lock()
                .read_line(&mut last_line)
                .expect("read-line: stdin read succeeds");
            // Strip any trailing CR or LF so echo output is clean.
            let trimmed = last_line.trim_end_matches(['\r', '\n']);
            last_line = trimmed.to_owned();
        } else if let Some(prefix) = arg.strip_prefix("echo:") {
            let mut out = stdout.lock();
            writeln!(out, "{prefix}:{last_line}").expect("echo: stdout write succeeds");
            out.flush().expect("echo: stdout flush succeeds");
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
}
