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

trait ClientStream: Read + Write {}
impl<T: Read + Write> ClientStream for T {}

struct SessionCleanup {
    executable: PathBuf,
    runtime_root: PathBuf,
    name: String,
}

impl Drop for SessionCleanup {
    fn drop(&mut self) {
        let _ = daemon_command(&self.executable, &self.runtime_root)
            .args(["kill", "--name", &self.name])
            .output();
        let _ = fs::remove_dir_all(&self.runtime_root);
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
    let endpoint = registry_endpoint(&registry.join("registry.json"), &name);
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
fn native_start_command_with_piped_stderr_does_not_hang_when_the_daemon_stays_alive() {
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
    let _cleanup = SessionCleanup {
        executable: executable.clone(),
        runtime_root: runtime_root.clone(),
        name: name.clone(),
    };

    // Mirrors `connect_or_start`'s exact stdio configuration: stdin
    // discarded, stdout discarded, stderr piped and read to completion via
    // `output()` (which also waits for the child to exit).
    let mut command = daemon_command(&executable, &runtime_root);
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
    let output = command.output().unwrap();
    assert_success("start", &output);
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

#[cfg(unix)]
fn short_runtime_root(suffix: &str) -> PathBuf {
    static NEXT_ROOT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let index = NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    match std::env::var_os("FESTERM_SESSIOND_TEST_RUNTIME_ROOT") {
        Some(root) => PathBuf::from(root).join(format!("{suffix}-{index}")),
        None => PathBuf::from(format!("/tmp/fsd-native-{suffix}-{index}")),
    }
}

#[cfg(windows)]
fn short_runtime_root(suffix: &str) -> PathBuf {
    std::env::temp_dir().join(format!("fsd-native-{suffix}"))
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
        let query_count = received[replied_through..]
            .windows(4)
            .filter(|sequence| *sequence == b"\x1b[6n")
            .count();
        for _ in 0..query_count {
            send_input(stream, b"\x1b[1;1R").unwrap();
        }
        replied_through = received.len().saturating_sub(3);
    }
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
