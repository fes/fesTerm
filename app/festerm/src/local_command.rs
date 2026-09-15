//! Bounded subprocess output without blocking reader threads.

use std::{
    process::{Command, Output, Stdio},
    time::Duration,
};

const MAX_OUTPUT: u64 = 1024 * 1024;

async fn read_bounded(reader: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    reader
        .take(MAX_OUTPUT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_OUTPUT {
        return Err("provider inventory exceeds 1 MiB".into());
    }
    Ok(bytes)
}

pub fn output(command: Command, timeout: Duration) -> Result<Option<Output>, String> {
    let program = command.get_program().to_string_lossy().into_owned();
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
            .stdin(Stdio::null())
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
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let result = tokio::time::timeout(timeout, async {
            let (stdout, stderr) = tokio::try_join!(read_bounded(stdout), read_bounded(stderr))?;
            let status = child.wait().await.map_err(|error| error.to_string())?;
            Ok(Output {
                status,
                stdout,
                stderr,
            })
        })
        .await;
        match result {
            Ok(Ok(output)) => Ok(Some(output)),
            error => {
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
                        "{program} discovery timed out after {} ms; Refresh to retry",
                        timeout.as_millis()
                    )),
                }
            }
        }
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

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
