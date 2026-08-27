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

    let registry = runtime_root.join("festerm").join("sessiond");
    let endpoint = registry_endpoint(&registry.join("registry.json"), &name);
    assert_native_permissions(&registry, &endpoint);

    let mut first = connect(&endpoint);
    send_input(&mut *first, b"first-marker\n").unwrap();
    assert_contains(&mut *first, b"first-marker");

    let mut second = connect(&endpoint);
    assert_contains(&mut *first, STOLEN_NOTICE);
    assert_eof(&mut *first);
    assert_contains(&mut *second, b"first-marker");

    send_input(&mut *second, b"second-marker\n").unwrap();
    assert_contains(&mut *second, b"second-marker");

    let output = daemon_command(&executable, &runtime_root)
        .args(["kill", "--name", &name])
        .output()
        .unwrap();
    assert_success("kill", &output);
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
    assert!(daemon.wait().unwrap().success());
}

#[cfg(unix)]
fn launch_session(executable: &Path, runtime_root: &Path, name: &str) {
    let output = daemon_command(executable, runtime_root)
        .args(["start", "--name", name, "--shell"])
        .arg(test_shell())
        .output()
        .unwrap();
    assert_success("start", &output);
}

#[cfg(windows)]
fn launch_session(executable: &Path, runtime_root: &Path, name: &str) -> std::process::Child {
    use std::{process::Stdio, thread};

    let mut daemon = daemon_command(executable, runtime_root)
        .args(["daemon", "--name", name, "--shell"])
        .arg(test_shell())
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

#[cfg(unix)]
fn test_shell() -> &'static str {
    "/bin/cat"
}

#[cfg(windows)]
fn test_shell() -> &'static str {
    "cmd.exe"
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
    let mut stream = named_pipe::PipeClient::connect_ms(endpoint, 2_000).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(2)));
    stream.set_write_timeout(Some(Duration::from_secs(2)));
    Box::new(stream)
}

fn send_input(stream: &mut dyn ClientStream, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(FRAME_MAGIC)?;
    stream.write_all(&[FRAME_INPUT])?;
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()
}

fn assert_contains(stream: &mut dyn ClientStream, expected: &[u8]) {
    let mut received = Vec::new();
    let mut buffer = [0u8; 4096];
    while !received
        .windows(expected.len())
        .any(|window| window == expected)
    {
        let count = stream.read(&mut buffer).unwrap();
        assert_ne!(count, 0, "stream closed before expected marker arrived");
        received.extend_from_slice(&buffer[..count]);
    }
}

fn assert_eof(stream: &mut dyn ClientStream) {
    let mut byte = [0u8; 1];
    assert_eq!(stream.read(&mut byte).unwrap(), 0);
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
