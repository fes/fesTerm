#![cfg_attr(windows, windows_subsystem = "windows")]

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
    let status = std::process::Command::new(setup)
        .arg("--serve")
        .arg("--state-dir")
        .arg(state)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .status()?;
    Ok(status.code().unwrap_or(1))
}

fn main() {
    #[cfg(windows)]
    std::process::exit(run().unwrap_or(1));
    #[cfg(not(windows))]
    std::process::exit(1);
}
