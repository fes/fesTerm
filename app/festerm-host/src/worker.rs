#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(any(windows, test))]
use std::{ffi::OsString, io, path::PathBuf};

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
enum Route {
    Legacy(PathBuf),
    DesktopManaged(PathBuf),
}

#[cfg(any(windows, test))]
fn route(args: &[OsString]) -> io::Result<Route> {
    let (state, managed) = match args {
        [flag, state] if flag == "--state-dir" => (state, false),
        [verb, flag, state] if verb == "run-desktop-managed" && flag == "--state-dir" => {
            (state, true)
        }
        _ => return Err(io::Error::other("worker.arguments")),
    };
    let state = PathBuf::from(state);
    if !state.is_absolute() {
        return Err(io::Error::other("worker.absolute_state_required"));
    }
    Ok(if managed {
        Route::DesktopManaged(state)
    } else {
        Route::Legacy(state)
    })
}

#[cfg(all(windows, feature = "owner-routing"))]
fn desktop_managed(state: &std::path::Path) -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let cancel = tokio_util::sync::CancellationToken::new();
    // A GUI-subsystem process need not own a console. Native Disable uses
    // the shared protected revocation/watchdog; do not install Ctrl+C here.
    runtime.block_on(
        festerm_host_setup::controller::managed::run_windows_desktop_worker_from_path(
            state, cancel,
        ),
    )
}

#[cfg(windows)]
fn run() -> io::Result<i32> {
    use std::{os::windows::process::CommandExt, process::Stdio};
    match route(&std::env::args_os().skip(1).collect::<Vec<_>>())? {
        Route::DesktopManaged(state) => {
            #[cfg(feature = "owner-routing")]
            {
                Ok(match desktop_managed(&state) {
                    Ok(()) => 0,
                    Err(error) => i32::from(festerm_host_setup::run_exit::error_exit_code(&error)),
                })
            }
            #[cfg(not(feature = "owner-routing"))]
            {
                let _ = state;
                Err(io::Error::other("worker.managed_runtime_unavailable"))
            }
        }
        Route::Legacy(state) => {
            let setup = std::env::current_exe()?.with_file_name("festerm-tunnel-setup.exe");
            let status = std::process::Command::new(setup)
                .arg("--serve")
                .arg("--state-dir")
                .arg(state)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(0x0800_0000) // Existing legacy CREATE_NO_WINDOW route.
                .status()?;
            Ok(status.code().unwrap_or(1))
        }
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
    use super::*;
    #[test]
    fn fixed_worker_routes_reject_arbitrary_origin_flags_and_relative_paths() {
        let absolute = std::env::temp_dir();
        assert!(absolute.is_absolute());
        let path = absolute.into_os_string();
        assert_eq!(
            route(&["--state-dir".into(), path.clone()]).unwrap(),
            Route::Legacy(PathBuf::from(&path))
        );
        assert_eq!(
            route(&[
                "run-desktop-managed".into(),
                "--state-dir".into(),
                path.clone()
            ])
            .unwrap(),
            Route::DesktopManaged(PathBuf::from(&path))
        );
        for args in [
            vec![],
            vec!["run-managed".into(), "--state-dir".into(), path.clone()],
            vec!["run-desktop".into(), "--state-dir".into(), path.clone()],
            vec![
                "run-desktop-managed".into(),
                "--state-dir".into(),
                path,
                "--origin".into(),
                "headless".into(),
            ],
            vec!["--state-dir".into(), "relative".into()],
        ] {
            assert!(route(&args).is_err());
        }
    }
}
