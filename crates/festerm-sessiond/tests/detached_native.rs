use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use festerm_pty::LocalProfile;
use festerm_session::{
    noop_session_event_notifier, Session, SessionEvent, SessionLifecycle, TerminalSize,
};
use festerm_sessiond::{
    ensure_detached_in, list_running_sessions_in, list_unattached_sessions_in, private_storage,
    spawn_fingerprint, verify_spawn_fingerprint_in, DetachedSessionRequest, PersistentSession,
};

const CHILD_ROOT: &str = "FESTERM_DETACHED_NATIVE_FIXTURE";

fn helper_name() -> &'static str {
    if cfg!(windows) {
        "festerm-sessiond.exe"
    } else {
        "festerm-sessiond"
    }
}

fn bounded(mut command: Command) -> bool {
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.success();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

struct Cleanup {
    root: PathBuf,
    helper: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let mut command = Command::new(&self.helper);
        command
            .env("FESTERM_SESSIOND_RUNTIME_ROOT", self.root.join("state"))
            .args(["kill", "--name", "owned"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = bounded(command);
        fs::remove_dir_all(&self.root).unwrap();
    }
}

/// Run the public APIs from a fixture-owned packaged layout, rather than adding
/// a production helper-path override or touching the installed native runtime.
#[test]
fn detached_native_start_once_preserves_local_attachment() {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        exercise_native(&PathBuf::from(root));
        return;
    }
    let root = std::env::current_dir().unwrap().join(format!(
        "dn-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8],
    ));
    let directory = private_storage::private_directory(&root, true).unwrap();
    let bin = root.join("bin");
    let bin_directory = private_storage::private_directory(&bin, true).unwrap();
    let runner = bin.join(if cfg!(windows) {
        "fixture.exe"
    } else {
        "fixture"
    });
    let helper = bin.join(helper_name());
    fs::copy(std::env::current_exe().unwrap(), &runner).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_festerm-sessiond"), &helper).unwrap();
    let log_path = root.join("fixture.log");
    let log = fs::File::create(&log_path).unwrap();
    let mut command = Command::new(&runner);
    command
        .env(CHILD_ROOT, &root)
        .args([
            "--exact",
            "detached_native_start_once_preserves_local_attachment",
            "--nocapture",
        ])
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    let result = bounded(command);
    let output = fs::read_to_string(&log_path).unwrap();
    drop(bin_directory);
    drop(directory);
    let _cleanup = Cleanup { root, helper };
    assert!(result, "{output}");
}

fn exercise_native(fixture: &Path) {
    let root = fixture.join("state");
    #[cfg(windows)]
    let profile = LocalProfile::new(
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join(r"System32\cmd.exe"),
    )
    .with_arguments(["/Q"]);
    #[cfg(unix)]
    let profile = LocalProfile::new("/bin/sh");
    let profile = profile.with_working_directory(fixture);
    let id = uuid::Uuid::new_v4().to_string();
    let request = |id| DetachedSessionRequest {
        name: "owned",
        profile: &profile,
        size: TerminalSize::new(80, 24).unwrap(),
        scrollback_limit_bytes: 4096,
        launch_request: id,
    };
    let selected = ensure_detached_in(&root, request(&id)).unwrap();
    let fingerprint = spawn_fingerprint(&profile).unwrap();
    verify_spawn_fingerprint_in(&root, &selected, &fingerprint).unwrap();
    assert_eq!(list_unattached_sessions_in(&root).unwrap().len(), 1);
    let discovered = list_unattached_sessions_in(&root).unwrap().remove(0);
    let session =
        PersistentSession::resume_discovered_in(&discovered, &root, noop_session_event_notifier())
            .unwrap();
    session.acknowledge_recovery_adoption();
    let deadline = Instant::now() + Duration::from_secs(5);
    while list_running_sessions_in(&root).unwrap()[0]
        .attached_by
        .is_none()
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    // Both request replay and another explicit consent reuse a locally attached
    // daemon without an admission/takeover. The original client remains usable.
    assert_eq!(ensure_detached_in(&root, request(&id)).unwrap(), selected);
    let other_id = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        ensure_detached_in(&root, request(&other_id)).unwrap(),
        selected
    );
    let marker = fixture.join("unexpected-second-shell");
    let mut competing = Command::new(fixture.join("bin").join(helper_name()));
    competing
        .env("FESTERM_SESSIOND_RUNTIME_ROOT", &root)
        .env(
            "FESTERM_SESSIOND_GENERATION",
            (selected.created_at_unix_ms + 1).to_string(),
        )
        .args(["daemon", "--name", "owned", "--shell"])
        .arg(profile.executable())
        .arg("--cwd")
        .arg(fixture)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    for argument in [
        "/D".to_owned(),
        "/Q".into(),
        "/C".into(),
        format!("echo unexpected>\"{}\"", marker.display()),
    ] {
        competing.arg("--arg").arg(argument);
    }
    #[cfg(unix)]
    for argument in [
        "-c".to_owned(),
        "printf unexpected > \"$1\"".into(),
        "owned".into(),
        marker.to_str().unwrap().into(),
    ] {
        competing.arg("--arg").arg(argument);
    }
    assert!(
        !bounded(competing),
        "a direct daemon must not overwrite the live generation"
    );
    assert!(
        !marker.exists(),
        "a losing daemon must refuse before spawning its shell"
    );
    verify_spawn_fingerprint_in(&root, &selected, &fingerprint).unwrap();
    session
        .try_send_input(b"echo DETACHED_NATIVE_STILL_ATTACHED\r\n")
        .unwrap();
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !String::from_utf8_lossy(&output).contains("DETACHED_NATIVE_STILL_ATTACHED") {
        if let Ok(SessionEvent::Output(bytes)) = session.try_recv_event() {
            output.extend(bytes);
        }
        assert_eq!(session.lifecycle(), SessionLifecycle::Running);
        assert!(
            Instant::now() < deadline,
            "local attachment did not remain usable"
        );
        thread::sleep(Duration::from_millis(10));
    }
    session.shutdown(Duration::from_secs(2)).unwrap();
    let mut kill = Command::new(fixture.join("bin").join(helper_name()));
    kill.env("FESTERM_SESSIOND_RUNTIME_ROOT", &root)
        .args(["kill", "--name", "owned"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    assert!(bounded(kill));
    assert!(verify_spawn_fingerprint_in(&root, &selected, &fingerprint).is_err());
    assert!(ensure_detached_in(&root, request(&id)).is_err());
    assert!(ensure_detached_in(&root, request(&other_id)).is_err());
    assert!(list_running_sessions_in(&root).unwrap().is_empty());
}
