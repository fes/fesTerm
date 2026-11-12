#![cfg_attr(windows, windows_subsystem = "windows")]

/// Delay before restarting a setup worker that crashed or was killed. Its own
/// in-process retries handle transient failures; this covers the rest.
#[cfg(windows)]
const RESTART_DELAY: std::time::Duration = std::time::Duration::from_secs(15);

/// Restart only a worker that ended abnormally. A clean bridge shutdown (0) and
/// a permanent operator refusal (78) stop, as with the Linux unit.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn restarts_after(code: i32) -> bool {
    code != 0 && code != 78
}

#[cfg(windows)]
fn run() -> std::io::Result<i32> {
    use std::{os::windows::process::CommandExt, path::PathBuf, process::Stdio};

    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [flag, state] = args.as_slice() else {
        return Err(std::io::Error::other("worker.arguments"));
    };
    if flag != "--state-dir" || !PathBuf::from(state).is_absolute() {
        return Err(std::io::Error::other("worker.absolute_state_required"));
    }
    let setup = std::env::current_exe()?.with_file_name("festerm-tunnel-setup.exe");
    loop {
        let status = std::process::Command::new(&setup)
            .arg("--serve")
            .arg("--state-dir")
            .arg(state)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .status()?;
        let code = status.code().unwrap_or(1);
        if !restarts_after(code) {
            return Ok(code);
        }
        // The owned-task stop terminates this launcher as well as its worker,
        // and rescans until neither remains, so this wait cannot outlive it.
        std::thread::sleep(RESTART_DELAY);
    }
}

fn main() {
    #[cfg(windows)]
    std::process::exit(run().unwrap_or(1));
    #[cfg(not(windows))]
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::restarts_after;

    #[test]
    fn windows_launcher_restarts_only_abnormal_worker_exits() {
        assert!(!restarts_after(0), "clean shutdown stops");
        assert!(!restarts_after(78), "permanent refusal stops");
        for code in [1, 101, -1, 0xC000_0005_u32 as i32] {
            assert!(restarts_after(code), "{code}");
        }
    }
}
