//! Disposable native-manager fixture, never included in a release package.
use festerm_sessiond::private_storage as storage;
use std::{
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 || args[1] != "--state-dir" {
        return Err(io::Error::other("fixture.arguments"));
    }
    let state = PathBuf::from(&args[2]);
    let directory = storage::private_directory(&state, false)?;
    let mode = args[0].to_str().unwrap_or_default();
    if mode == "run" {
        let mut child = Command::new(std::env::current_exe()?)
            .arg("daemon")
            .arg("--state-dir")
            .arg(&state)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        storage::write_new_private(
            &directory,
            Path::new("fixture-host-started"),
            child.id().to_string().as_bytes(),
        )?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    } else if mode != "daemon" {
        return Err(io::Error::other("fixture.arguments"));
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut counter = 0u64;
    while Instant::now() < deadline {
        match storage::open_private_file(&directory, Path::new("fixture-stop"), false) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if mode == "daemon" {
            storage::replace_private(
                &directory,
                Path::new("fixture-daemon-heartbeat"),
                counter.to_string().as_bytes(),
            )?;
            counter += 1;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if mode == "daemon" {
        storage::write_new_private(&directory, Path::new("fixture-daemon-stopped"), b"stopped")?;
    }
    Ok(())
}
