use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt, net::UnixStream};

const FRAME_MAGIC: &[u8; 4] = b"FSD1";
const FRAME_INPUT: u8 = 1;
const STOLEN_NOTICE: &[u8] =
    b"\n[festerm-sessiond] SESSION_STOLEN: reattached from another client\n";
const EXITED_NOTICE: &[u8] = b"\n[festerm-sessiond] SESSION_EXITED\n";

#[cfg(unix)]
#[test]
fn native_start_reports_socket_depth_without_starting_a_shell() {
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_festerm-sessiond"));
    let base = short_runtime_root("overlong");
    let _base_cleanup = BatchCleanup {
        executable: executable.clone(),
        runtime: base.clone(),
        names: Vec::new(),
    };
    let runtime = base.join("x".repeat(120));
    fs::create_dir(&runtime).unwrap();
    let _session_cleanup = SessionCleanup {
        executable: executable.clone(),
        runtime_root: runtime.clone(),
        name: "owned-overlong".into(),
    };
    let marker = base.join("unexpected-shell");
    let mut command = daemon_command(&executable, &runtime);
    command
        .args([
            "start",
            "--name",
            "owned-overlong",
            "--shell",
            "/usr/bin/touch",
            "--arg",
        ])
        .arg(&marker);
    let output = run_start_command(command);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("invalid Unix session socket"), "{error}");
    assert!(error.contains("path bytes"), "{error}");
    assert!(error.contains("shorter XDG_STATE_HOME"), "{error}");
    assert!(
        !marker.exists(),
        "an invalid endpoint must not start a shell"
    );
}

#[cfg(unix)]
#[test]
fn native_start_failure_removes_generation_artifacts_before_root_cleanup() {
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_festerm-sessiond"));
    let runtime = short_runtime_root("failed-shell");
    let _cleanup = SessionCleanup {
        executable: executable.clone(),
        runtime_root: runtime.clone(),
        name: "failed-shell".into(),
    };
    let mut command = daemon_command(&executable, &runtime);
    command.args([
        "start",
        "--name",
        "failed-shell",
        "--shell",
        "/festerm-owned-nonexistent-shell",
    ]);
    let output = run_start_command(command);
    assert!(!output.status.success());
    let registry = runtime.join("festerm/sessiond");
    assert!(festerm_sessiond::list_unattached_sessions_in(&registry)
        .unwrap()
        .is_empty());
    assert!(
        fs::read_dir(&registry).unwrap().all(|entry| {
            let path = entry.unwrap().path();
            !path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("lease-")
                && path.extension().is_none_or(|extension| extension != "sock")
        }),
        "failed startup left generation artifacts"
    );
}

fn assert_generation_artifacts_removed(
    root: &Path,
    selected: &festerm_sessiond::UnattachedSession,
) {
    let lease = root.join(format!(
        "lease-{}-{}",
        selected.pid, selected.created_at_unix_ms
    ));
    bounded_poll(
        || {
            !lease.try_exists().unwrap()
                && (!cfg!(unix) || !Path::new(&selected.endpoint).try_exists().unwrap())
        },
        "generation artifact cleanup",
    );
}

/// Uses the production inventory/resume APIs in an isolated registry. The
/// deterministic child reports its PID *after* a fresh post-reattach input,
/// so replay text alone cannot satisfy the continuity assertion.
#[test]
#[ignore = "isolated native churn; included in the optional-validation runner"]
fn native_discovery_churn_preserves_process_and_rejects_replaced_generations() {
    use festerm_session::{noop_session_event_notifier, Session, SessionLifecycle};
    use festerm_sessiond::{list_unattached_sessions_in, PersistentSession};
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_festerm-sessiond"));
    let batch = churn_setting("FESTERM_SESSION_CHURN_BATCH", 8, 128);
    let cycles = churn_setting("FESTERM_SESSION_CHURN_CYCLES", 3, 100);
    let runtime = short_runtime_root("churn");
    fs::create_dir_all(&runtime).unwrap();
    let registry = runtime
        .join(if cfg!(windows) { "fesTerm" } else { "festerm" })
        .join("sessiond");
    let mut cleanups = BatchCleanup {
        executable: executable.clone(),
        runtime: runtime.clone(),
        names: Vec::new(),
    };
    let mut windows_children = Vec::<std::process::Child>::new();
    let inventory = || list_unattached_sessions_in(&registry).unwrap();
    let read_until = read_session_until;
    for cycle in 0..cycles {
        for index in 0..batch {
            let name = format!("churn-{}-{index:04}", std::process::id());
            cleanups.names.push(name.clone());
            let shell = pty_test_child(&executable);
            let args = [
                "report-pid",
                "read-line",
                "echo:BEFORE",
                "read-line",
                "report-pid",
                "echo:AFTER",
                "read-line",
                "exit:0",
            ];
            #[cfg(unix)]
            launch_session_with(&executable, &runtime, &name, &shell, &args);
            #[cfg(windows)]
            windows_children.push(launch_session_with(
                &executable,
                &runtime,
                &name,
                &shell,
                &args,
            ));
        }
        bounded_poll(|| inventory().len() == batch, "batch discovery");
        let selected = inventory();
        assert!(selected.windows(2).all(|pair| pair[0].name < pair[1].name));
        for (index, selected) in selected.iter().enumerate() {
            assert_eq!(
                selected.shell,
                pty_test_child(&executable).to_string_lossy()
            );
            let first = PersistentSession::resume_discovered_in(
                selected,
                &registry,
                noop_session_event_notifier(),
            )
            .unwrap();
            let pid_output = read_until(&first, "PID:");
            let pid = pid_output
                .split("PID:")
                .nth(1)
                .unwrap()
                .split(":END")
                .next()
                .unwrap()
                .trim()
                .to_owned();
            first
                .try_send_input(&test_input(&format!("first-{cycle}-{index}")))
                .unwrap();
            read_until(&first, &format!("BEFORE:first-{cycle}-{index}"));
            bounded_poll(
                || !inventory().iter().any(|entry| entry.name == selected.name),
                "attached native exclusion",
            );
            drop(first);
            bounded_poll(
                || inventory().iter().any(|entry| entry.name == selected.name),
                "detach discovery",
            );
            let second = PersistentSession::resume_discovered_in(
                selected,
                &registry,
                noop_session_event_notifier(),
            )
            .unwrap();
            // Consume replay before the new challenge.
            read_until(&second, &format!("BEFORE:first-{cycle}-{index}"));
            second
                .try_send_input(&test_input(&format!("second-{cycle}-{index}")))
                .unwrap();
            let resumed = read_until(&second, &format!("AFTER:second-{cycle}-{index}"));
            assert!(
                resumed.contains(&format!("PID:{pid}:END")),
                "same child process must answer the fresh challenge"
            );
            if index % 2 == 0 {
                second.try_send_input(&test_input("exit")).unwrap();
                bounded_poll(
                    || {
                        matches!(
                            second.lifecycle(),
                            SessionLifecycle::Exited(_) | SessionLifecycle::Stopped
                        )
                    },
                    "child exit",
                );
            } else {
                kill_for_cleanup(&executable, &runtime, &selected.name).unwrap();
                bounded_poll(
                    || {
                        !festerm_sessiond::daemon_generation_is_live(
                            &registry,
                            selected.pid,
                            selected.created_at_unix_ms,
                            &selected.endpoint,
                        )
                        .unwrap()
                    },
                    "explicit daemon termination",
                );
            }
            drop(second);
            bounded_poll(
                || !inventory().iter().any(|entry| entry.name == selected.name),
                "natural exit removal",
            );
            assert!(PersistentSession::resume_discovered_in(
                selected,
                &registry,
                noop_session_event_notifier()
            )
            .is_err());
            assert_generation_artifacts_removed(&registry, selected);
        }
        assert!(inventory().is_empty());
        // Recreate the same names on the next pass; previous selections must
        // remain invalid even after their labels become visible again.
        if cycle + 1 < cycles {
            let old = &selected[0];
            let shell = pty_test_child(&executable);
            #[cfg(unix)]
            launch_session_with(
                &executable,
                &runtime,
                &old.name,
                &shell,
                &["report-pid", "read-line", "exit:0"],
            );
            #[cfg(windows)]
            windows_children.push(launch_session_with(
                &executable,
                &runtime,
                &old.name,
                &shell,
                &["report-pid", "read-line", "exit:0"],
            ));
            bounded_poll(|| inventory().len() == 1, "recreated generation");
            let replacement = inventory().remove(0);
            assert_ne!(old.endpoint, replacement.endpoint);
            assert!(PersistentSession::resume_discovered_in(
                old,
                &registry,
                noop_session_event_notifier()
            )
            .is_err());
            let client = PersistentSession::resume_discovered_in(
                &replacement,
                &registry,
                noop_session_event_notifier(),
            )
            .unwrap();
            read_until(&client, "PID:");
            client.try_send_input(&test_input("exit")).unwrap();
            bounded_poll(
                || {
                    matches!(
                        client.lifecycle(),
                        SessionLifecycle::Exited(_) | SessionLifecycle::Stopped
                    )
                },
                "replacement child exit",
            );
            drop(client);
            bounded_poll(|| inventory().is_empty(), "recreated exit");
            assert_generation_artifacts_removed(&registry, &replacement);
        }
        eprintln!(
            "sessiond-churn cycle={} batch={} status=pass",
            cycle + 1,
            batch
        );
    }
    for child in &mut windows_children {
        let _ = child.wait();
    }
    drop(cleanups);
}

fn churn_setting(name: &str, default: usize, max: usize) -> usize {
    let value = std::env::var(name)
        .map(|value| {
            value
                .parse::<usize>()
                .expect("churn setting must be an integer")
        })
        .unwrap_or(default);
    assert!((1..=max).contains(&value), "{name} must be 1..={max}");
    value
}

fn bounded_poll(mut ready: impl FnMut() -> bool, phase: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(std::time::Instant::now() < deadline, "timed out: {phase}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn read_session_until(session: &festerm_sessiond::PersistentSession, marker: &str) -> String {
    use festerm_session::{Session, SessionEvent, SessionTryReceiveError};
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut output = Vec::new();
    #[cfg(windows)]
    let mut replied_through = 0;
    loop {
        match session.try_recv_event() {
            Ok(SessionEvent::Output(bytes)) => output.extend(bytes),
            Ok(_) | Err(SessionTryReceiveError::Empty) => {}
            Err(error) => panic!("client closed before {marker}: {error:?}"),
        }
        assert!(output.len() <= 1024 * 1024);
        #[cfg(windows)]
        if marker == "PID:" {
            reply_to_cursor_queries(&output, &mut replied_through, |reply| {
                session.try_send_input(reply).unwrap()
            });
        }
        let text = String::from_utf8_lossy(&output);
        let complete = if marker == "PID:" {
            text.split_once("PID:")
                .and_then(|(_, tail)| tail.split_once(":END"))
                .is_some_and(|(pid, _)| pid.parse::<u32>().is_ok())
        } else {
            text.contains(marker)
        };
        if complete {
            return String::from_utf8(output).unwrap();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "missing {marker}: {}",
            String::from_utf8_lossy(&output)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "isolated native churn; included in the optional-validation runner"]
fn native_discovery_churn_reconnect_pins_generation_and_explicit_registry() {
    use festerm_session::{noop_session_event_notifier, Session, SessionLifecycle};
    use festerm_sessiond::{list_unattached_sessions_in, PersistentSession};
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_festerm-sessiond"));
    let runtime = short_runtime_root("reconnect");
    let name = format!("reconnect-{}", std::process::id());
    let _cleanup = SessionCleanup {
        executable: executable.clone(),
        runtime_root: runtime.clone(),
        name: name.clone(),
    };
    let registry = runtime
        .join(if cfg!(windows) { "fesTerm" } else { "festerm" })
        .join("sessiond");
    let shell = pty_test_child(&executable);
    let args = [
        "report-pid",
        "read-line",
        "echo:BEFORE",
        "read-line",
        "report-pid",
        "echo:AFTER",
        "spin",
    ];
    #[cfg(unix)]
    launch_session_with(&executable, &runtime, &name, &shell, &args);
    #[cfg(windows)]
    let mut first_child = launch_session_with(&executable, &runtime, &name, &shell, &args);
    let inventory = || list_unattached_sessions_in(&registry).unwrap();
    bounded_poll(|| inventory().len() == 1, "reconnect discovery");
    let selected = inventory().remove(0);
    let session = PersistentSession::resume_discovered_in(
        &selected,
        &registry,
        noop_session_event_notifier(),
    )
    .unwrap();
    let id = session.id();
    let output = read_session_until(&session, "PID:");
    let pid = output
        .split_once("PID:")
        .unwrap()
        .1
        .split_once(":END")
        .unwrap()
        .0;
    session.try_send_input(&test_input("original")).unwrap();
    read_session_until(&session, "BEFORE:original");

    let mut takeover = connect(&selected.endpoint);
    assert_contains(&mut *takeover, b"BEFORE:original");
    bounded_poll(|| session.reconnect_available(), "takeover disconnect");
    // A stale Launcher selection cannot steal, but this tab's explicit
    // reconnect must retain the existing same-generation takeover policy.
    assert!(PersistentSession::resume_discovered_in(
        &selected,
        &registry,
        noop_session_event_notifier()
    )
    .is_err());
    session.try_reconnect().unwrap();
    read_session_until(&session, "BEFORE:original");
    assert_contains(&mut *takeover, STOLEN_NOTICE);
    drop(takeover);
    assert_eq!(session.id(), id);
    session.try_send_input(&test_input("reconnected")).unwrap();
    let output = read_session_until(&session, "AFTER:reconnected");
    assert!(output.contains(&format!("PID:{pid}:END")), "{output}");

    // Leave A reconnectable rather than naturally exited, then replace its
    // name with B while keeping B's client attached throughout the attempt.
    let mut takeover = connect(&selected.endpoint);
    assert_contains(&mut *takeover, b"AFTER:reconnected");
    bounded_poll(
        || session.reconnect_available(),
        "second takeover disconnect",
    );
    kill_for_cleanup(&executable, &runtime, &name).unwrap();
    drop(takeover);
    bounded_poll(|| inventory().is_empty(), "generation A removal");
    assert_generation_artifacts_removed(&registry, &selected);
    #[cfg(windows)]
    first_child.wait().unwrap();
    let args = [
        "report-pid",
        "read-line",
        "report-pid",
        "echo:REPLACEMENT",
        "spin",
    ];
    #[cfg(unix)]
    launch_session_with(&executable, &runtime, &name, &shell, &args);
    #[cfg(windows)]
    let mut replacement_child = launch_session_with(&executable, &runtime, &name, &shell, &args);
    bounded_poll(|| inventory().len() == 1, "generation B discovery");
    let replacement = inventory().remove(0);
    assert_ne!(replacement.endpoint, selected.endpoint);
    let client = PersistentSession::resume_discovered_in(
        &replacement,
        &registry,
        noop_session_event_notifier(),
    )
    .unwrap();
    let output = read_session_until(&client, "PID:");
    let replacement_pid = output
        .split_once("PID:")
        .unwrap()
        .1
        .split_once(":END")
        .unwrap()
        .0;
    session.try_reconnect().unwrap();
    bounded_poll(
        || matches!(session.lifecycle(), SessionLifecycle::Disconnected(error) if error.message().contains("changed")),
        "replacement reconnect rejection",
    );
    assert!(format!("{:?}", session.lifecycle()).contains("changed"));
    assert_eq!(session.id(), id);
    client.try_send_input(&test_input("unaffected")).unwrap();
    let output = read_session_until(&client, "REPLACEMENT:unaffected");
    assert!(
        output.contains(&format!("PID:{replacement_pid}:END")),
        "{output}"
    );
    assert!(matches!(client.lifecycle(), SessionLifecycle::Running));
    assert!(
        inventory().is_empty(),
        "B remains attached; no replacement shell"
    );
    drop(client);
    drop(session);
    kill_for_cleanup(&executable, &runtime, &name).unwrap();
    assert_generation_artifacts_removed(&registry, &replacement);
    #[cfg(windows)]
    replacement_child.wait().unwrap();
}

#[test]
#[ignore = "isolated native churn; included in the optional-validation runner"]
fn native_discovery_churn_prunes_forcibly_terminated_generation_artifacts() {
    use festerm_session::{noop_session_event_notifier, Session};
    use festerm_sessiond::{list_unattached_sessions_in, PersistentSession};
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_festerm-sessiond"));
    let runtime = short_runtime_root("forced");
    let _cleanup = BatchCleanup {
        executable: executable.clone(),
        runtime: runtime.clone(),
        names: vec!["owned-force".into(), "unrelated-sentinel".into()],
    };
    let shell = pty_test_child(&executable);
    let args = ["report-pid", "read-line", "echo:ALIVE", "spin"];
    #[cfg(unix)]
    for name in ["owned-force", "unrelated-sentinel"] {
        launch_session_with(&executable, &runtime, name, &shell, &args);
    }
    #[cfg(windows)]
    let (mut forced_child, mut sentinel_child) = (
        launch_session_with(&executable, &runtime, "owned-force", &shell, &args),
        launch_session_with(&executable, &runtime, "unrelated-sentinel", &shell, &args),
    );
    let registry = runtime
        .join(if cfg!(windows) { "fesTerm" } else { "festerm" })
        .join("sessiond");
    let inventory = list_unattached_sessions_in(&registry).unwrap();
    assert_eq!(inventory.len(), 2);
    let selected = inventory
        .iter()
        .find(|session| session.name == "owned-force")
        .unwrap();
    let sentinel = inventory
        .iter()
        .find(|session| session.name == "unrelated-sentinel")
        .unwrap();
    let client =
        PersistentSession::resume_discovered_in(selected, &registry, noop_session_event_notifier())
            .unwrap();
    read_session_until(&client, "PID:");
    #[cfg(unix)]
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(selected.pid).unwrap()),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    #[cfg(windows)]
    {
        forced_child.kill().unwrap();
        forced_child.wait().unwrap();
    }
    bounded_poll(
        || {
            !festerm_sessiond::daemon_generation_is_live(
                &registry,
                selected.pid,
                selected.created_at_unix_ms,
                &selected.endpoint,
            )
            .unwrap()
        },
        "forced daemon exit",
    );
    let lease = registry.join(format!(
        "lease-{}-{}",
        selected.pid, selected.created_at_unix_ms
    ));
    assert!(
        lease.exists(),
        "forced termination should leave an artifact for prune to remove"
    );
    let mut command = daemon_command(&executable, &runtime);
    command.arg("list");
    #[cfg(unix)]
    let output = run_start_command(command);
    #[cfg(windows)]
    let output = command.output().unwrap();
    assert_success("prune", &output);
    assert_generation_artifacts_removed(&registry, selected);
    let remaining = list_unattached_sessions_in(&registry).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].endpoint, sentinel.endpoint);
    let live =
        PersistentSession::resume_discovered_in(sentinel, &registry, noop_session_event_notifier())
            .unwrap();
    read_session_until(&live, "PID:");
    live.try_send_input(&test_input("unaffected")).unwrap();
    read_session_until(&live, "ALIVE:unaffected");
    drop(live);
    drop(client);
    kill_for_cleanup(&executable, &runtime, &sentinel.name).unwrap();
    assert_generation_artifacts_removed(&registry, sentinel);
    #[cfg(windows)]
    sentinel_child.wait().unwrap();
}

trait ClientStream: Read + Write {}
impl<T: Read + Write> ClientStream for T {}

struct BatchCleanup {
    executable: PathBuf,
    runtime: PathBuf,
    names: Vec<String>,
}

impl Drop for BatchCleanup {
    fn drop(&mut self) {
        let mut errors = Vec::new();
        for name in &self.names {
            if let Err(error) = kill_for_cleanup(&self.executable, &self.runtime, name) {
                errors.push(error);
            }
        }
        finish_cleanup(&self.runtime, errors);
    }
}

fn finish_cleanup(root: &Path, mut errors: Vec<String>) {
    if errors.is_empty() {
        if let Err(error) = fs::remove_dir_all(root) {
            if error.kind() != io::ErrorKind::NotFound {
                errors.push(error.to_string());
            }
        }
    }
    if !errors.is_empty() {
        eprintln!(
            "cleanup incomplete; retained {}: {errors:?}",
            root.display()
        );
        assert!(
            std::thread::panicking(),
            "native cleanup failed: {errors:?}"
        );
    }
}

fn kill_for_cleanup(executable: &Path, runtime: &Path, name: &str) -> Result<(), String> {
    use std::process::Stdio;
    let mut child = daemon_command(executable, runtime)
        .args(["kill", "--name", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let output = child
                    .wait_with_output()
                    .map_err(|error| error.to_string())?;
                let message = String::from_utf8_lossy(&output.stderr);
                return if output.status.success() || message.contains("is not registered") {
                    Ok(())
                } else {
                    Err(format!("kill {name}: {message}"))
                };
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("kill {name} exceeded cleanup deadline"));
            }
        }
    }
}

struct SessionCleanup {
    executable: PathBuf,
    runtime_root: PathBuf,
    name: String,
}

impl Drop for SessionCleanup {
    fn drop(&mut self) {
        let errors = kill_for_cleanup(&self.executable, &self.runtime_root, &self.name)
            .err()
            .into_iter()
            .collect();
        finish_cleanup(&self.runtime_root, errors);
    }
}

#[test]
#[ignore = "native daemon smoke; run through native-smoke.yml or the VM optional-validation mode"]
fn native_daemon_survives_launcher_and_supports_input_replay_and_takeover() {
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_festerm-sessiond"));
    let suffix = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let name = format!("native-{suffix}");
    let runtime_root = short_runtime_root(&suffix);
    fs::create_dir_all(&runtime_root).unwrap();
    let _cleanup = SessionCleanup {
        executable: executable.clone(),
        runtime_root: runtime_root.clone(),
        name: name.clone(),
    };

    #[cfg(unix)]
    launch_session(&executable, &runtime_root, &name);
    #[cfg(windows)]
    let mut daemon = launch_session(&executable, &runtime_root, &name);
    eprintln!("sessiond-native phase=launched");

    let registry = runtime_root.join("festerm").join("sessiond");
    let registry_path = registry.join("registry.json");
    let endpoint = registry_endpoint(&registry_path, &name);
    let registry_document: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).unwrap()).unwrap();
    let record = &registry_document["sessions"][&name];
    assert_eq!(
        record["protocol_version"].as_u64(),
        Some(u64::from(festerm_sessiond::PROTOCOL_VERSION))
    );
    assert_native_permissions(&registry, &endpoint);

    let mut first = connect(&endpoint);
    eprintln!("sessiond-native phase=first-connected");
    #[cfg(windows)]
    assert_windows_ready(&mut *first);
    eprintln!("sessiond-native phase=initial-output");
    send_input(&mut *first, &test_input("first-marker")).unwrap();
    eprintln!("sessiond-native phase=first-input-sent");
    assert_contains(&mut *first, b"first-marker");
    eprintln!("sessiond-native phase=first-output");

    let mut second = connect(&endpoint);
    eprintln!("sessiond-native phase=second-connected");
    assert_contains(&mut *first, STOLEN_NOTICE);
    assert_eof(&mut *first);
    assert_contains(&mut *second, b"first-marker");
    eprintln!("sessiond-native phase=takeover");

    send_input(&mut *second, &test_input("second-marker")).unwrap();
    assert_contains(&mut *second, b"second-marker");
    eprintln!("sessiond-native phase=second-output");

    let output = daemon_command(&executable, &runtime_root)
        .args(["kill", "--name", &name])
        .output()
        .unwrap();
    assert_success("kill", &output);
    eprintln!("sessiond-native phase=killed");
    let output = daemon_command(&executable, &runtime_root)
        .arg("list")
        .output()
        .unwrap();
    assert_success("list", &output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "no live sessions"
    );
    #[cfg(windows)]
    let _terminated_status = daemon.wait().unwrap();
}

/// Regression test for the Windows handle-inheritance leak fixed alongside
/// this test: `connect_or_start` (crates/festerm-sessiond/src/lib.rs) spawns
/// `festerm-sessiond start` with a piped stderr and reads it via
/// `Command::output()`, which blocks until that pipe reaches EOF. Before the
/// fix, `run_start`'s own spawn of the detached "daemon" grandchild (which
/// redirects its own stdio to NUL) forced `bInheritHandles = TRUE`, which
/// duplicated the caller's inherited stderr-pipe write handle into that
/// long-lived, never-exiting daemon -- so the pipe never reached EOF and
/// `output()` hung forever.
///
/// The other test above deliberately manages the Windows daemon process
/// itself (spawning the "daemon" subcommand directly with all-NUL stdio) and
/// so never exercises this "start" + piped-stderr path at all. This test
/// exercises exactly that path, bounded by an explicit watchdog timeout so a
/// regression fails the test instead of hanging the test binary (and CI)
/// indefinitely the way it hung fesTerm's own UI thread.
#[cfg(windows)]
#[test]
#[ignore = "native daemon smoke; run through native-smoke.yml or the VM optional-validation mode"]
fn native_windows_packaged_helper_can_be_replaced_while_staged_daemon_remains_usable() {
    use std::{process::Stdio, thread};

    let executable = PathBuf::from(env!("CARGO_BIN_EXE_festerm-sessiond"));
    let suffix = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let name = format!("native-start-{suffix}");
    let runtime_root = short_runtime_root(&suffix);
    fs::create_dir_all(&runtime_root).unwrap();
    let package_directory = runtime_root.join("package");
    fs::create_dir(&package_directory).unwrap();
    let packaged_helper = package_directory.join("festerm-sessiond.exe");
    fs::copy(&executable, &packaged_helper).unwrap();
    let _cleanup = SessionCleanup {
        executable: executable.clone(),
        runtime_root: runtime_root.clone(),
        name: name.clone(),
    };

    // Mirrors `connect_or_start`'s exact stdio configuration: stdin
    // discarded, stdout discarded, stderr piped and read to completion via
    // `output()` (which also waits for the child to exit).
    let mut command = daemon_command(&packaged_helper, &runtime_root);
    command
        .args(["start", "--name", &name, "--shell"])
        .arg(test_shell(&executable));
    for argument in test_shell_arguments() {
        command.arg("--arg").arg(argument);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let handle = thread::spawn(move || {
        let result = command.output();
        let _ = result_tx.send(());
        result
    });

    const HANG_TIMEOUT: Duration = Duration::from_secs(15);
    if result_rx.recv_timeout(HANG_TIMEOUT).is_err() {
        panic!(
            "festerm-sessiond start with piped stderr did not complete within {HANG_TIMEOUT:?}; \
             this is the leaked-handle hang this test guards against"
        );
    }
    let output = handle
        .join()
        .expect("the start command's watcher thread must not panic")
        .expect("spawning festerm-sessiond start must succeed");
    assert_success("start", &output);

    let registry = runtime_root.join("fesTerm").join("sessiond");
    let registry_document: serde_json::Value =
        serde_json::from_slice(&fs::read(registry.join("registry.json")).unwrap()).unwrap();
    let record = &registry_document["sessions"][&name];
    let helper_identity = record["helper_identity"].as_str().unwrap();
    assert_eq!(
        helper_identity,
        format!(
            "festerm-sessiond-{}-{}.exe",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::ARCH
        )
    );
    let staged_helper = registry
        .join("helpers")
        .join(
            Path::new(helper_identity)
                .file_stem()
                .expect("helper identity has a generation name"),
        )
        .join(helper_identity);
    assert!(
        staged_helper.is_file(),
        "the daemon helper must be staged in its generation directory: {}",
        staged_helper.display()
    );

    fs::remove_file(&packaged_helper)
        .expect("the package-owned helper must not be locked by the staged daemon");
    fs::copy(&executable, &packaged_helper)
        .expect("an installer must be able to replace the package-owned helper");

    let endpoint = record["socket"].as_str().unwrap();
    let mut client = connect(endpoint);
    assert_windows_ready(&mut *client);
    send_input(&mut *client, &test_input("after-package-replacement")).unwrap();
    assert_contains(&mut *client, b"after-package-replacement");
}

#[cfg(windows)]
#[test]
#[ignore = "native daemon smoke; run through native-smoke.yml or the VM optional-validation mode"]
fn native_windows_versioned_helper_installs_beside_a_live_legacy_daemon() {
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_festerm-sessiond"));
    let suffix = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let name = format!("legacy-upgrade-{suffix}");
    let runtime_root = short_runtime_root(&suffix);
    fs::create_dir_all(&runtime_root).unwrap();
    let package_directory = runtime_root.join("package");
    fs::create_dir(&package_directory).unwrap();
    let legacy_helper = package_directory.join("festerm-sessiond.exe");
    fs::copy(&executable, &legacy_helper).unwrap();
    let _cleanup = SessionCleanup {
        executable: executable.clone(),
        runtime_root: runtime_root.clone(),
        name: name.clone(),
    };

    let mut legacy_daemon = launch_session_with(
        &legacy_helper,
        &runtime_root,
        &name,
        &test_shell(&executable),
        &test_shell_arguments(),
    );
    assert!(legacy_daemon.try_wait().unwrap().is_none());

    let versioned_helper = package_directory.join(format!(
        "festerm-sessiond-{}.exe",
        env!("CARGO_PKG_VERSION")
    ));
    fs::copy(&executable, &versioned_helper)
        .expect("a versioned helper must install without replacing the live legacy image");
    assert!(versioned_helper.is_file());

    let registry = runtime_root.join("fesTerm").join("sessiond");
    let endpoint = registry_endpoint(&registry.join("registry.json"), &name);
    let mut client = connect(&endpoint);
    assert_windows_ready(&mut *client);
    send_input(&mut *client, &test_input("after-side-by-side-install")).unwrap();
    assert_contains(&mut *client, b"after-side-by-side-install");
}

/// Regression test for the Windows zombie daemon reported in September 2026.
///
/// A ConPTY keeps its pseudoconsole (and the `conhost` process behind it) open
/// for as long as the daemon holds the master handle, so the pseudoterminal
/// reader never reports end of file when the shell exits. The daemon used to
/// rely on that end of file alone, so when the shell exited it kept running
/// forever: it stayed in the registry, `process_alive` still reported it, and
/// the Launcher went on offering a session that could be attached but would
/// never respond again. Killing the daemon by hand was the only way out.
///
/// This test drives a shell that exits by itself and asserts that the daemon
/// notices, tells the client, exits, and deregisters.
#[test]
#[ignore = "native daemon smoke; run through native-smoke.yml or the VM optional-validation mode"]
fn native_daemon_exits_and_deregisters_when_its_shell_exits() {
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_festerm-sessiond"));
    let suffix = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let name = format!("native-exit-{suffix}");
    let runtime_root = short_runtime_root(&suffix);
    fs::create_dir_all(&runtime_root).unwrap();
    let _cleanup = SessionCleanup {
        executable: executable.clone(),
        runtime_root: runtime_root.clone(),
        name: name.clone(),
    };

    let shell = exiting_test_shell(&executable);
    let arguments = exiting_test_shell_arguments();
    #[cfg(unix)]
    launch_session_with(&executable, &runtime_root, &name, &shell, &arguments);
    #[cfg(windows)]
    let mut daemon = launch_session_with(&executable, &runtime_root, &name, &shell, &arguments);

    let registry = runtime_root.join("festerm").join("sessiond");
    let endpoint = registry_endpoint(&registry.join("registry.json"), &name);
    let mut client = connect(&endpoint);
    #[cfg(windows)]
    assert_windows_ready(&mut *client);

    // The shell consumes this line and exits.
    send_input(&mut *client, &test_input("goodbye")).unwrap();
    assert_contains(&mut *client, EXITED_NOTICE);
    drop(client);

    #[cfg(windows)]
    wait_for(
        Duration::from_secs(15),
        "the daemon did not exit after its shell exited",
        || daemon.try_wait().unwrap().is_some(),
    );

    wait_for(
        Duration::from_secs(15),
        "the exited session stayed in the registry, so the Launcher would keep offering it",
        || {
            let output = daemon_command(&executable, &runtime_root)
                .arg("list")
                .output()
                .unwrap();
            assert_success("list", &output);
            String::from_utf8(output.stdout).unwrap().trim() == "no live sessions"
        },
    );
}

/// A client that stops reading for a moment must not be disconnected.
///
/// The GUI drains session output on its frame loop, so a busy frame (or any
/// burst larger than the daemon's queue) briefly stops it reading the
/// transport. That is ordinary backpressure and has to slow the shell down,
/// not tear the session down: users saw an actively streaming session drop to
/// `Disconnected` in the middle of their work because the daemon abandoned a
/// client that was merely slow.
#[test]
#[ignore = "native daemon smoke; run through native-smoke.yml or the VM optional-validation mode"]
fn native_daemon_keeps_a_slow_client_through_a_large_output_burst() {
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_festerm-sessiond"));
    let suffix = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let name = format!("native-slow-{suffix}");
    let runtime_root = short_runtime_root(&suffix);
    fs::create_dir_all(&runtime_root).unwrap();
    let _cleanup = SessionCleanup {
        executable: executable.clone(),
        runtime_root: runtime_root.clone(),
        name: name.clone(),
    };

    let shell = flooding_test_shell(&executable);
    let arguments = flooding_test_shell_arguments();
    #[cfg(unix)]
    launch_session_with(&executable, &runtime_root, &name, &shell, &arguments);
    #[cfg(windows)]
    let mut daemon = launch_session_with(&executable, &runtime_root, &name, &shell, &arguments);

    let registry = runtime_root.join("festerm").join("sessiond");
    let endpoint = registry_endpoint(&registry.join("registry.json"), &name);
    let mut client = connect(&endpoint);
    #[cfg(windows)]
    assert_windows_ready(&mut *client);

    // Release the burst, then stop reading for long enough to overrun every
    // buffer between the shell and this client.
    send_input(&mut *client, &test_input("go")).unwrap();
    std::thread::sleep(Duration::from_secs(5));

    // The session must still be attached and still delivering the burst.
    assert_contains(&mut *client, b"FLOOD-COMPLETE");

    // The test shell ends in `spin`, so the daemon only goes away when the
    // session is killed. Reap it here rather than leaving a zombie behind for
    // the rest of the suite.
    #[cfg(windows)]
    {
        drop(client);
        let _ = daemon_command(&executable, &runtime_root)
            .args(["kill", "--name", &name])
            .output();
        let _ = daemon.wait();
    }
}

fn wait_for(timeout: Duration, message: &str, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if condition() {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "{message}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
fn launch_session(executable: &Path, runtime_root: &Path, name: &str) {
    launch_session_with(
        executable,
        runtime_root,
        name,
        &test_shell(executable),
        &test_shell_arguments(),
    )
}

#[cfg(unix)]
fn launch_session_with(
    executable: &Path,
    runtime_root: &Path,
    name: &str,
    shell: &Path,
    arguments: &[&str],
) {
    let mut command = daemon_command(executable, runtime_root);
    command
        .args(["start", "--name", name, "--shell"])
        .arg(shell);
    for argument in arguments {
        command.arg("--arg").arg(argument);
    }
    let output = run_start_command(command);
    assert_success("start", &output);
}

#[cfg(unix)]
fn run_start_command(mut command: Command) -> Output {
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("native start deadline exceeded");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[cfg(windows)]
fn launch_session(executable: &Path, runtime_root: &Path, name: &str) -> std::process::Child {
    launch_session_with(
        executable,
        runtime_root,
        name,
        &test_shell(executable),
        &test_shell_arguments(),
    )
}

#[cfg(windows)]
fn launch_session_with(
    executable: &Path,
    runtime_root: &Path,
    name: &str,
    shell: &Path,
    arguments: &[&str],
) -> std::process::Child {
    use std::{process::Stdio, thread};

    let mut command = daemon_command(executable, runtime_root);
    command
        .args(["daemon", "--name", name, "--shell"])
        .arg(shell);
    for argument in arguments {
        command.arg("--arg").arg(argument);
    }
    let mut daemon = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let registry = runtime_root
        .join("fesTerm")
        .join("sessiond")
        .join("registry.json");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if fs::read(&registry)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|value| value["sessions"].get(name).is_some())
        {
            return daemon;
        }
        assert!(
            daemon.try_wait().unwrap().is_none(),
            "daemon exited before registering"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "daemon did not register within two seconds"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn daemon_command(executable: &Path, runtime_root: &Path) -> Command {
    let mut command = Command::new(executable);
    #[cfg(unix)]
    command.env("XDG_STATE_HOME", runtime_root);
    #[cfg(windows)]
    command.env("LOCALAPPDATA", runtime_root);
    command
}

fn short_runtime_root(_suffix: &str) -> PathBuf {
    static NEXT_ROOT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let index = NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (base, leaf) = match std::env::var_os("FESTERM_SESSIOND_TEST_RUNTIME_ROOT") {
        Some(root) => (
            PathBuf::from(root),
            format!("{}-{index}", std::process::id()),
        ),
        None => (
            if cfg!(unix) {
                PathBuf::from("/tmp")
            } else {
                std::env::temp_dir()
            },
            format!(
                "fsd-{}-{index}-{:x}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ),
        ),
    };
    let root = base.join(leaf);
    #[cfg(unix)]
    {
        let probe = root.join(format!(
            "festerm/sessiond/{}-{}.sock",
            u32::MAX,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::os::unix::net::SocketAddr::from_pathname(&probe).unwrap_or_else(|error| {
            panic!(
                "native test runtime {} cannot fit a generation socket ({} path bytes): {error}; set FESTERM_SESSIOND_TEST_RUNTIME_ROOT to a short private directory",
                root.display(), probe.as_os_str().as_encoded_bytes().len()
            )
        });
    }
    fs::create_dir_all(&base).expect("native test runtime base can be created");
    let mut builder = fs::DirBuilder::new();
    builder.recursive(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&root)
        .expect("native test runtime must be a new owned directory");
    eprintln!("sessiond-native runtime={}", root.display());
    root
}

#[cfg(unix)]
fn test_shell(_daemon: &Path) -> PathBuf {
    PathBuf::from("/bin/cat")
}

#[cfg(windows)]
fn test_shell(daemon: &Path) -> PathBuf {
    pty_test_child(daemon)
}

/// The workspace's deterministic PTY child, which understands the `emit:` /
/// `read-line` / `emit-frames:` / `spin` protocol used by the flooding test.
///
/// It is an ordinary cross-platform binary, not a Windows-only helper; both
/// the Linux and macOS smoke jobs `cargo build --workspace` before running
/// this suite, so it sits next to the daemon on every platform.
fn pty_test_child(daemon: &Path) -> PathBuf {
    daemon
        .parent()
        .expect("daemon executable has a parent directory")
        .join(format!(
            "festerm-pty-test-child{}",
            std::env::consts::EXE_SUFFIX
        ))
}

#[cfg(unix)]
fn test_shell_arguments() -> Vec<&'static str> {
    Vec::new()
}

/// A shell that exits on its own once it has consumed a single line of input,
/// used to exercise the daemon's shell-exit shutdown path.
#[cfg(unix)]
fn exiting_test_shell(_daemon: &Path) -> PathBuf {
    PathBuf::from("/bin/sh")
}

#[cfg(unix)]
fn exiting_test_shell_arguments() -> Vec<&'static str> {
    vec!["-c", "read line; exit 0"]
}

#[cfg(windows)]
fn exiting_test_shell(daemon: &Path) -> PathBuf {
    test_shell(daemon)
}

#[cfg(windows)]
fn exiting_test_shell_arguments() -> Vec<&'static str> {
    vec!["emit:READY", "read-line", "exit:0"]
}

/// Waits for a line, then emits far more output than the daemon's client queue
/// and the transport buffers can hold before announcing completion.
///
/// These arguments are the PTY test child's protocol, not a shell's, so the
/// flooding test has to launch `pty_test_child` on every platform. `/bin/cat`
/// -- what `test_shell` hands the other Unix tests -- treats them as filenames
/// to open, fails to find them and exits immediately, which the daemon
/// correctly reports as "exited during startup".
fn flooding_test_shell(daemon: &Path) -> PathBuf {
    pty_test_child(daemon)
}

fn flooding_test_shell_arguments() -> Vec<&'static str> {
    vec![
        "emit:READY",
        "read-line",
        "emit-frames:20000:0",
        "emit:FLOOD-COMPLETE",
        "spin",
    ]
}

#[cfg(windows)]
fn test_shell_arguments() -> Vec<&'static str> {
    vec![
        "emit:READY",
        "read-line",
        "echo:INPUT",
        "read-line",
        "echo:INPUT",
        "spin",
    ]
}

#[cfg(unix)]
fn test_input(marker: &str) -> Vec<u8> {
    format!("{marker}\n").into_bytes()
}

#[cfg(windows)]
fn test_input(marker: &str) -> Vec<u8> {
    format!("{marker}\r\n").into_bytes()
}

fn registry_endpoint(path: &Path, name: &str) -> String {
    let registry: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    registry["sessions"][name]["socket"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[cfg(unix)]
fn connect(endpoint: &str) -> Box<dyn ClientStream> {
    let stream = UnixStream::connect(endpoint).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    Box::new(stream)
}

#[cfg(windows)]
fn connect(endpoint: &str) -> Box<dyn ClientStream> {
    // Bundled ConPTY spawns a separate OpenConsole.exe host process for the
    // child shell. On a cold GitHub Actions Windows runner, that host's own
    // startup (plus first-write scheduling for the test child's `READY`
    // marker) can comfortably exceed 2s, even though the daemon's own
    // accept/replay/forwarding path (verified via FESTERM_SESSIOND_TRACE_FILE
    // tracing) completes well within that window. Use a longer timeout here
    // so the test tolerates legitimate ConPTY/process startup latency instead
    // of racing it.
    let mut stream = festerm_windows_security::named_pipe::Pipe::connect(
        endpoint,
        Duration::from_secs(10),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    stream.set_read_timeout(Duration::from_secs(10));
    stream.set_write_timeout(Duration::from_secs(10));
    Box::new(stream)
}

fn send_input(stream: &mut dyn ClientStream, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(FRAME_MAGIC)?;
    stream.write_all(&[FRAME_INPUT])?;
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(bytes)?;
    #[cfg(not(windows))]
    stream.flush()?;
    Ok(())
}

#[cfg(windows)]
fn assert_windows_ready(stream: &mut dyn ClientStream) {
    let mut received = Vec::new();
    let mut replied_through = 0;
    let mut buffer = [0u8; 4096];
    while !received.windows(5).any(|window| window == b"READY") {
        let count = stream.read(&mut buffer).unwrap();
        assert_ne!(count, 0, "stream closed before READY arrived");
        received.extend_from_slice(&buffer[..count]);
        reply_to_cursor_queries(&received, &mut replied_through, |reply| {
            send_input(stream, reply).unwrap()
        });
    }
}

#[cfg(any(windows, test))]
fn reply_to_cursor_queries(
    received: &[u8],
    replied_through: &mut usize,
    mut send: impl FnMut(&[u8]),
) {
    let query_count = received[*replied_through..]
        .windows(4)
        .filter(|sequence| *sequence == b"\x1b[6n")
        .count();
    for _ in 0..query_count {
        send(b"\x1b[1;1R");
    }
    *replied_through = received.len().saturating_sub(3);
}

#[test]
fn cursor_query_guard_replies_once_to_fragmented_queries() {
    let mut received = b"\x1b[6".to_vec();
    let mut replied = 0;
    let mut replies = Vec::new();
    reply_to_cursor_queries(&received, &mut replied, |reply| {
        replies.push(reply.to_vec())
    });
    assert!(replies.is_empty());
    received.extend_from_slice(b"nPID:42:END");
    reply_to_cursor_queries(&received, &mut replied, |reply| {
        replies.push(reply.to_vec())
    });
    reply_to_cursor_queries(&received, &mut replied, |reply| {
        replies.push(reply.to_vec())
    });
    assert_eq!(replies, vec![b"\x1b[1;1R".to_vec()]);
    received.extend_from_slice(b"\x1b[6n");
    reply_to_cursor_queries(&received, &mut replied, |reply| {
        replies.push(reply.to_vec())
    });
    assert_eq!(replies.len(), 2);
}

fn assert_contains(stream: &mut dyn ClientStream, expected: &[u8]) {
    let mut received = Vec::new();
    let mut buffer = [0u8; 4096];
    while !received
        .windows(expected.len())
        .any(|window| window == expected)
    {
        let count = stream.read(&mut buffer).unwrap_or_else(|error| {
            panic!(
                "reading until {:?} failed: {error}; received so far: {:?}",
                String::from_utf8_lossy(expected),
                String::from_utf8_lossy(&received)
            )
        });
        assert_ne!(count, 0, "stream closed before expected marker arrived");
        received.extend_from_slice(&buffer[..count]);
    }
}

fn assert_eof(stream: &mut dyn ClientStream) {
    let mut byte = [0u8; 1];
    match stream.read(&mut byte) {
        Ok(0) => {}
        Err(error) if is_eof_error(&error) => {}
        result => panic!("expected EOF after takeover, got {result:?}"),
    }
}

#[cfg(unix)]
fn is_eof_error(_error: &io::Error) -> bool {
    false
}

#[cfg(windows)]
fn is_eof_error(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(109 | 233))
}

#[cfg(unix)]
fn assert_native_permissions(registry: &Path, endpoint: &str) {
    assert_eq!(
        fs::metadata(registry).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for path in [
        registry.join("registry.json"),
        registry.join("registry.lock"),
        PathBuf::from(endpoint),
        registry.join(format!(
            "lease-{}",
            Path::new(endpoint).file_stem().unwrap().to_string_lossy()
        )),
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[cfg(windows)]
fn assert_native_permissions(_registry: &Path, _endpoint: &str) {}

fn assert_success(operation: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{operation} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
