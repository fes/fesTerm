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
//! | `emit-bytes-hex:HEX` | Write the exact bytes encoded by even-length hexadecimal `HEX`. |
//! | `emit-repeat:COUNT:TEXT` | Write `TEXT` to stdout `COUNT` times, with no extra newline. |
//! | `emit-frames:COUNT:MILLIS` | Write `FRAME:00` through `FRAME:COUNT-1`, pausing `MILLIS` between lines. |
//! | `wait-for-file:PATH` | Wait up to 120 seconds for a start marker; publish PTY size in `PATH.geometry.json`. |
//! | `tui:KIND:COUNT:MILLIS:PATH` | Run a fixed 120x40 TUI workload and write producer statistics to `PATH`. |
//! | `read-line` | Read one line from stdin; strip trailing CR/LF. |
//! | `expect-line-suffix:TEXT` | Fail unless the last complete input line ends with `TEXT`. |
//! | `echo:PREFIX` | Write `PREFIX:{last-line}\n` to stdout. |
//! | `report-size` | Write `{rows} {cols}\n` (PTY dimensions) to stdout. |
//! | `report-pid` | Write `PID:{pid}:END\n` for process-continuity assertions. |
//! | `spin` | Sleep until the process is killed. |
//! | `spawn` | Spawn self as a long-running descendant, write `CHILD:{pid}\n`, then wait for it. |
//! | `exit:N` | Exit with decimal code N. |

use std::{
    io::{BufRead, Write},
    process, thread,
    time::Duration,
};

use terminal_size::{terminal_size, Height, Width};

fn publish_report(path: &str, contents: &str) {
    let temporary = format!("{path}.tmp");
    std::fs::write(&temporary, contents).expect("write probe report");
    std::fs::rename(temporary, path).expect("publish probe report");
}

fn decode_hex_bytes(specification: &str) -> Vec<u8> {
    assert!(
        specification.len().is_multiple_of(2),
        "emit-bytes-hex argument must contain an even number of hex digits, got {specification:?}"
    );
    specification
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let pair = std::str::from_utf8(pair).expect("hex digits are valid UTF-8");
            u8::from_str_radix(pair, 16)
                .unwrap_or_else(|_| panic!("emit-bytes-hex contains non-hex digits: {pair:?}"))
        })
        .collect()
}

fn emit_tui(specification: &str, out: &mut impl Write) {
    use festerm_test_support::tui_workload::Workload;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    let mut parts = specification.splitn(4, ':');
    let workload =
        Workload::parse(parts.next().expect("tui workload")).expect("valid TUI workload");
    let count: usize = parts
        .next()
        .expect("tui count")
        .parse()
        .expect("numeric TUI count");
    let millis: u64 = parts
        .next()
        .expect("tui interval")
        .parse()
        .expect("numeric TUI interval");
    let report = parts.next().expect("tui report path");
    assert!((1..=6000).contains(&count), "TUI count must be 1..=6000");
    assert!(
        (1..=1000).contains(&millis),
        "TUI interval must be 1..=1000 milliseconds"
    );
    let dimensions = terminal_size();
    let setup = workload.setup();
    out.write_all(&setup).expect("TUI setup write");
    out.flush().expect("TUI setup flush");
    let started_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let clock = Instant::now();
    let mut bytes = setup.len();
    let mut completed = Vec::with_capacity(count);
    for frame in 1..=count {
        let due = Duration::from_millis(millis * frame as u64);
        thread::sleep(due.saturating_sub(clock.elapsed()));
        let update = workload.update(frame);
        out.write_all(&update).expect("TUI update write");
        out.flush().expect("TUI update flush");
        bytes += update.len();
        completed.push(clock.elapsed().as_secs_f64() * 1000.0);
    }
    let geometry = dimensions.map_or_else(
        || "null".to_owned(),
        |(Width(columns), Height(rows))| format!("{{\"columns\":{columns},\"rows\":{rows}}}"),
    );
    let report_json = format!(
        "{{\"workload\":\"{}\",\"pid\":{},\"frames\":{count},\"bytes\":{bytes},\
         \"interval_ms\":{millis},\"started_unix_ms\":{started_unix_ms},\
         \"geometry\":{geometry},\"completed_ms\":{completed:?}}}\n",
        workload.name(),
        process::id(),
    );
    publish_report(report, &report_json);
    // Leave the final TUI visible for capture. A following `spin` command keeps
    // the child alive without sending output or restoring the alternate screen.
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut last_line = String::new();
    let mut last_line_complete = false;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();

    for arg in &args {
        if let Some(text) = arg.strip_prefix("emit:") {
            let mut out = stdout.lock();
            out.write_all(text.as_bytes())
                .expect("emit: stdout write succeeds");
            out.write_all(b"\n").expect("emit: stdout newline succeeds");
            out.flush().expect("emit: stdout flush succeeds");
        } else if let Some(specification) = arg.strip_prefix("emit-bytes-hex:") {
            let bytes = decode_hex_bytes(specification);
            let mut out = stdout.lock();
            out.write_all(&bytes)
                .expect("emit-bytes-hex: stdout write succeeds");
            out.flush().expect("emit-bytes-hex: stdout flush succeeds");
        } else if let Some(specification) = arg.strip_prefix("emit-repeat:") {
            let (count, text) = specification
                .split_once(':')
                .and_then(|(count, text)| Some((count.parse::<usize>().ok()?, text)))
                .unwrap_or_else(|| {
                    panic!("emit-repeat argument must be emit-repeat:COUNT:TEXT, got {arg:?}")
                });
            let mut out = stdout.lock();
            for _ in 0..count {
                out.write_all(text.as_bytes())
                    .expect("emit-repeat: stdout write succeeds");
            }
            out.flush().expect("emit-repeat: stdout flush succeeds");
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
        } else if let Some(path) = arg.strip_prefix("wait-for-file:") {
            let started = std::time::Instant::now();
            let mut previous = None;
            while !std::path::Path::new(path)
                .try_exists()
                .expect("read start marker status")
            {
                let geometry =
                    terminal_size().map(|(Width(columns), Height(rows))| (columns, rows));
                if geometry != previous {
                    if let Some((columns, rows)) = geometry {
                        let report = format!("{path}.geometry.json");
                        publish_report(
                            &report,
                            &format!(
                                "{{\"pid\":{},\"columns\":{columns},\"rows\":{rows}}}\n",
                                process::id()
                            ),
                        );
                    }
                    previous = geometry;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(120),
                    "start marker timed out"
                );
                thread::sleep(Duration::from_millis(20));
            }
        } else if let Some(specification) = arg.strip_prefix("tui:") {
            emit_tui(specification, &mut stdout.lock());
        } else if arg == "read-line" {
            last_line.clear();
            stdin
                .lock()
                .read_line(&mut last_line)
                .expect("read-line: stdin read succeeds");
            last_line_complete = last_line.ends_with(['\r', '\n']);
            // Strip any trailing CR or LF so echo output is clean.
            let trimmed = last_line.trim_end_matches(['\r', '\n']);
            last_line = trimmed.to_owned();
        } else if let Some(suffix) = arg.strip_prefix("expect-line-suffix:") {
            assert!(
                last_line_complete && last_line.ends_with(suffix),
                "expect-line-suffix: controlled input did not match"
            );
        } else if let Some(prefix) = arg.strip_prefix("echo:") {
            let mut out = stdout.lock();
            writeln!(out, "{prefix}:{last_line}").expect("echo: stdout write succeeds");
            out.flush().expect("echo: stdout flush succeeds");
        } else if arg == "report-pid" {
            let mut out = stdout.lock();
            writeln!(out, "PID:{}:END", process::id()).expect("report-pid: stdout write succeeds");
            out.flush().expect("report-pid: stdout flush succeeds");
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
