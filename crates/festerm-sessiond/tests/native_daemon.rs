use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt, net::UnixStream};

const FRAME_MAGIC: &[u8; 4] = b"FSD1";
const FRAME_INPUT: u8 = 1;
const FRAME_RESIZE: u8 = 2;
const STOLEN_NOTICE: &[u8] =
    b"\n[festerm-sessiond] SESSION_STOLEN: reattached from another client\n";

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

    let mut first = connect(&endpoint, 80, 24);
    eprintln!("sessiond-native phase=first-connected");
    #[cfg(windows)]
    let initial_output = assert_windows_ready(&mut *first);
    #[cfg(unix)]
    let initial_output = Vec::new();
    eprintln!("sessiond-native phase=initial-output");
    assert_contains_or_received(
        &mut *first,
        &initial_output,
        b"ENV:FESTERM_SESSIOND_SMOKE=explicit-environment",
    );
    eprintln!("sessiond-native phase=environment");
    send_input(&mut *first, &test_input("first-marker")).unwrap();
    eprintln!("sessiond-native phase=first-input-sent");
    assert_contains(&mut *first, b"first-marker");
    eprintln!("sessiond-native phase=first-output");

    let mut second = connect(&endpoint, 100, 40);
    eprintln!("sessiond-native phase=second-connected");
    assert_contains(&mut *first, STOLEN_NOTICE);
    assert_eof(&mut *first);
    assert_contains(&mut *second, b"first-marker");
    eprintln!("sessiond-native phase=takeover");

    send_input(&mut *second, &test_input("second-marker")).unwrap();
    assert_contains(&mut *second, b"second-marker");
    eprintln!("sessiond-native phase=second-output");

    assert_contains(&mut *second, b"BURST-ONE-DONE");
    eprintln!("sessiond-native phase=burst-output");

    send_input(&mut *second, &test_input("report-size")).unwrap();
    assert_contains(&mut *second, b"40 100");
    eprintln!("sessiond-native phase=resize");

    send_input(&mut *second, &test_input("preempt-marker")).unwrap();
    assert_contains(&mut *second, b"INPUT:preempt-marker");
    std::thread::sleep(Duration::from_millis(100));
    let mut third = connect(&endpoint, 120, 50);
    assert_contains(&mut *second, STOLEN_NOTICE);
    assert_eof(&mut *second);
    assert_contains(&mut *third, b"BURST-TWO-DONE");
    send_input(&mut *third, &test_input("report-size-again")).unwrap();
    assert_contains(&mut *third, b"50 120");
    eprintln!("sessiond-native phase=reconnect-resize");

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

#[cfg(unix)]
fn launch_session(executable: &Path, runtime_root: &Path, name: &str) {
    let mut command = daemon_command(executable, runtime_root);
    command
        .args(["start", "--name", name, "--shell"])
        .arg(test_shell(executable));
    for argument in test_shell_arguments() {
        command.arg("--arg").arg(argument);
    }
    command.args([
        "--env-policy",
        "clear",
        "--env",
        "FESTERM_SESSIOND_SMOKE",
        "explicit-environment",
    ]);
    let output = command.output().unwrap();
    assert_success("start", &output);
}

#[cfg(windows)]
fn launch_session(executable: &Path, runtime_root: &Path, name: &str) -> std::process::Child {
    use std::{process::Stdio, thread};

    let mut command = daemon_command(executable, runtime_root);
    command
        .args(["daemon", "--name", name, "--shell"])
        .arg(test_shell(executable));
    for argument in test_shell_arguments() {
        command.arg("--arg").arg(argument);
    }
    command.args([
        "--env-policy",
        "clear",
        "--env",
        "FESTERM_SESSIOND_SMOKE",
        "explicit-environment",
    ]);
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
    PathBuf::from(format!("/tmp/fsd-native-{suffix}"))
}

#[cfg(windows)]
fn short_runtime_root(suffix: &str) -> PathBuf {
    std::env::temp_dir().join(format!("fsd-native-{suffix}"))
}

fn test_shell(daemon: &Path) -> PathBuf {
    daemon
        .parent()
        .expect("daemon executable has a parent directory")
        .join(format!(
            "festerm-pty-test-child{}",
            std::env::consts::EXE_SUFFIX
        ))
}

fn test_shell_arguments() -> Vec<&'static str> {
    vec![
        "report-env:FESTERM_SESSIOND_SMOKE",
        "emit:READY",
        "read-line",
        "echo:INPUT",
        "read-line",
        "echo:INPUT",
        "emit-bytes:524288:BURST-ONE-DONE",
        "read-line",
        "report-size",
        "read-line",
        "echo:INPUT",
        "emit-bytes:524288:BURST-TWO-DONE",
        "read-line",
        "report-size",
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
fn connect(endpoint: &str, cols: u16, rows: u16) -> Box<dyn ClientStream> {
    let mut stream = UnixStream::connect(endpoint).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    send_resize(&mut stream, cols, rows).unwrap();
    Box::new(stream)
}

#[cfg(windows)]
fn connect(endpoint: &str, cols: u16, rows: u16) -> Box<dyn ClientStream> {
    let mut stream = named_pipe::PipeClient::connect_ms(endpoint, 2_000).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(2)));
    stream.set_write_timeout(Some(Duration::from_secs(2)));
    send_resize(&mut stream, cols, rows).unwrap();
    Box::new(stream)
}

fn send_input(stream: &mut dyn ClientStream, bytes: &[u8]) -> io::Result<()> {
    send_frame(stream, FRAME_INPUT, bytes)
}

fn send_resize(stream: &mut dyn ClientStream, cols: u16, rows: u16) -> io::Result<()> {
    let mut payload = Vec::with_capacity(8);
    for value in [cols, rows, 0, 0] {
        payload.extend_from_slice(&value.to_be_bytes());
    }
    send_frame(stream, FRAME_RESIZE, &payload)
}

fn send_frame(stream: &mut dyn ClientStream, kind: u8, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(FRAME_MAGIC)?;
    stream.write_all(&[kind])?;
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(bytes)?;
    #[cfg(not(windows))]
    stream.flush()?;
    Ok(())
}

#[cfg(windows)]
fn assert_windows_ready(stream: &mut dyn ClientStream) -> Vec<u8> {
    let mut received = Vec::new();
    let mut replied_through = 0;
    let mut buffer = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(15);
    while !received.windows(5).any(|window| window == b"READY") {
        assert!(Instant::now() < deadline, "timed out waiting for READY");
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
    received
}

fn assert_contains_or_received(stream: &mut dyn ClientStream, received: &[u8], expected: &[u8]) {
    if !received
        .windows(expected.len())
        .any(|window| window == expected)
    {
        assert_contains(stream, expected);
    }
}

fn assert_contains(stream: &mut dyn ClientStream, expected: &[u8]) {
    let mut received = Vec::new();
    let mut buffer = [0u8; 4096];
    let mut received_bytes = 0usize;
    let deadline = Instant::now() + Duration::from_secs(15);
    while !received
        .windows(expected.len())
        .any(|window| window == expected)
    {
        assert!(
            Instant::now() < deadline,
            "timed out after {received_bytes} bytes waiting for {:?}",
            String::from_utf8_lossy(expected)
        );
        let count = stream.read(&mut buffer).unwrap();
        assert_ne!(count, 0, "stream closed before expected marker arrived");
        received_bytes = received_bytes.saturating_add(count);
        received.extend_from_slice(&buffer[..count]);
        if received.len() > 1_048_576 {
            let retained = expected.len().saturating_sub(1).max(4096);
            received.drain(..received.len().saturating_sub(retained));
        }
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
