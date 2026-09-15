//! Bounded subprocess output without blocking reader threads.

use std::{
    cell::Cell,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

const MAX_OUTPUT: u64 = 1024 * 1024;

#[derive(Default)]
struct ReadProgress {
    bytes: Cell<usize>,
    eof: Cell<bool>,
}

async fn read_bounded(
    reader: impl tokio::io::AsyncRead + Unpin,
    progress: &ReadProgress,
) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    let mut reader = reader.take(MAX_OUTPUT + 1);
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader
            .read(&mut buffer)
            .await
            .map_err(|error| error.to_string())?;
        if count == 0 {
            progress.eof.set(true);
            break;
        }
        progress.bytes.set(bytes.len() + count);
        if progress.bytes.get() as u64 > MAX_OUTPUT {
            return Err("provider inventory exceeds 1 MiB".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(bytes)
}

pub fn output(command: Command, timeout: Duration) -> Result<Option<Output>, String> {
    output_with_stdin(command, timeout, Stdio::null())
}

pub fn output_with_stdin(
    command: Command,
    timeout: Duration,
    stdin: Stdio,
) -> Result<Option<Output>, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let trace = std::env::var_os("FESTERM_DISCOVERY_TIMING").is_some();
    let arguments = trace.then(|| {
        command
            .get_args()
            .map(|arg| arg.to_os_string())
            .collect::<Vec<_>>()
    });
    let started = Instant::now();
    #[cfg(unix)]
    let command = {
        use std::os::unix::process::CommandExt;
        let mut command = command;
        command.process_group(0);
        command
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let mut child = match tokio::process::Command::from(command)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("could not run {program}: {error}")),
        };
        #[cfg(unix)]
        let process_group = child.id();
        let pid = child.id();
        let spawn_ms = started.elapsed().as_millis();
        let stdout_progress = ReadProgress::default();
        let stderr_progress = ReadProgress::default();
        let waiting_for_exit = Cell::new(false);
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let result = tokio::time::timeout(timeout, async {
            let (stdout, stderr) = tokio::try_join!(
                read_bounded(stdout, &stdout_progress),
                read_bounded(stderr, &stderr_progress)
            )?;
            waiting_for_exit.set(true);
            let status = child.wait().await.map_err(|error| error.to_string())?;
            Ok(Output {
                status,
                stdout,
                stderr,
            })
        })
        .await;
        match result {
            Ok(Ok(output)) => {
                if let Some(arguments) = &arguments {
                    eprintln!(
                        "discovery-command program={program:?} args={arguments:?} pid={pid:?} spawn_ms={spawn_ms} elapsed_ms={} status={}",
                        started.elapsed().as_millis(), output.status
                    );
                }
                Ok(Some(output))
            }
            error => {
                let detail = format!("pid={pid:?} spawn_ms={spawn_ms} elapsed_ms={} stdout_bytes={} stdout_eof={} stderr_bytes={} stderr_eof={} waiting_for_exit={}",
                    started.elapsed().as_millis(), stdout_progress.bytes.get(), stdout_progress.eof.get(),
                    stderr_progress.bytes.get(), stderr_progress.eof.get(), waiting_for_exit.get());
                if let Some(arguments) = &arguments {
                    eprintln!("discovery-command program={program:?} args={arguments:?} {detail}");
                }
                #[cfg(unix)]
                if let Some(pid) = process_group {
                    use nix::{
                        sys::signal::{killpg, Signal},
                        unistd::Pid,
                    };
                    let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGKILL);
                }
                let _ = child.start_kill();
                let _ = child.wait().await;
                match error {
                    Ok(Err(error)) => Err(error),
                    _ => Err(format!(
                        "{program} discovery timed out after {} ms; Refresh to retry ({detail})",
                        timeout.as_millis(),
                    )),
                }
            }
        }
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn bounded_command_preserves_an_explicit_owned_terminal_stdin() {
        use festerm_session::Session;
        use std::os::unix::fs::OpenOptionsExt;
        let session = festerm_pty::LocalPtySession::start(
            festerm_pty::LocalProfile::new("/bin/sh").with_arguments(["-c", "read value"]),
            festerm_session::TerminalSize::new(80, 24).unwrap(),
        )
        .unwrap();
        let terminal = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOCTTY | nix::libc::O_NONBLOCK)
            .open(session.terminal_device().unwrap())
            .unwrap();
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "if test -t 0; then printf owned-tty; else printf no-tty; fi",
        ]);
        let result = output_with_stdin(command, Duration::from_secs(2), terminal.into())
            .unwrap()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"owned-tty");
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "if test -t 0; then printf owned-tty; else printf no-tty; fi",
        ]);
        let result = output(command, Duration::from_secs(2)).unwrap().unwrap();
        assert_eq!(result.stdout, b"no-tty");
        session.shutdown(Duration::from_secs(2)).unwrap();
    }

    struct Marker(std::path::PathBuf);
    impl Drop for Marker {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn timed_out_command_terminates_its_owned_descendants() {
        use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
        let path = std::env::current_dir()
            .unwrap()
            .join(format!(".command-child-{}", std::process::id()));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let marker = Marker(path);
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "/bin/sh -c 'while :; do :; done' & printf '%s' \"$!\" > \"$1\"; wait",
                "owned-command",
            ])
            .arg(&marker.0);
        assert!(output(command, Duration::from_millis(500))
            .unwrap_err()
            .contains("timed out"));
        let pid = std::fs::read_to_string(&marker.0)
            .unwrap()
            .parse::<i32>()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while kill(Pid::from_raw(pid), None) != Err(Errno::ESRCH) {
            assert!(
                std::time::Instant::now() < deadline,
                "owned descendant {pid} survived timeout cleanup"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
