#![cfg(all(unix, feature = "relay"))]

use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("festerm-setup-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::copy(
            env!("CARGO_BIN_EXE_festerm-tunnel-setup"),
            root.join("festerm-tunnel-setup"),
        )
        .unwrap();
        Self { root }
    }

    fn script(&self, name: &str, body: &str) {
        let path = self.root.join(name);
        fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn command(&self) -> Command {
        let mut command = Command::new(self.root.join("festerm-tunnel-setup"));
        command
            .arg("--devtunnel")
            .arg(self.root.join("devtunnel"))
            .arg("--state-dir")
            .arg(self.root.join("run"))
            .env("MOCK_ROOT", &self.root)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    fn normal_cli(&self, token: &str) {
        self.script(
            "devtunnel",
            &format!(
                r#"
printf '%s\n' "$*" >> "$MOCK_ROOT/commands"
case "$1 $2" in
    'user login') exit 0 ;;
    'user show') printf '%s' '{{"status":"Logged in","provider":"github"}}' ;;
    create\ *) printf '{{"tunnel":{{"tunnelId":"%s.usw2"}}}}' "$2" ;;
    'port create') printf '{{}}' ;;
    token\ *) printf '%s' '{{"token":"{token}","scope":"host"}}' ;;
    *) exit 30 ;;
esac"#
            ),
        );
        self.script(
            "festerm-sessiond",
            r#"printf '%s\n' "$*" > "$MOCK_ROOT/daemon-args"; touch "$MOCK_ROOT/retained-shell""#,
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[tokio::test]
async fn setup_help_distinguishes_managed_service_from_foreground_mode() {
    let output = timeout(
        Duration::from_secs(5),
        Command::new(env!("CARGO_BIN_EXE_festerm-tunnel-setup"))
            .arg("--help")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for contract in [
        "--managed",
        "--serve",
        "foreground",
        "unelevated",
        "reboot",
        "never creates a replacement",
    ] {
        assert!(help.contains(contract), "missing help contract: {contract}");
    }
}

#[tokio::test]
async fn setup_keeps_tokens_off_argv_and_prints_verified_public_connection_details() {
    let fixture = Fixture::new();
    fixture.normal_cli("synthetic-host-token-not-a-credential");
    let key = russh::keys::PrivateKey::random(
        &mut russh::keys::key::safe_rng(),
        russh::keys::Algorithm::Ed25519,
    )
    .unwrap();
    let public = key.public_key().to_openssh().unwrap();
    let public = public.split_whitespace().nth(1).unwrap();
    let fingerprint = festerm_tunnel_bridge::relay::host_fingerprint(public).unwrap();
    fixture.script(
        "festerm-tunnel-bridge",
        &format!(
            r#"
printf '%s\n' "$*" > "$MOCK_ROOT/bridge-args"
IFS= read -r token
test "$token" = synthetic-host-token-not-a-credential
unset token
touch "$MOCK_ROOT/token-stdin-accepted"
while test "$#" -gt 0; do
    case "$1" in
        --session) name="$2" ;;
        --destination) destination="$2" ;;
    esac
    shift
done
printf '{{"version":1,"file_read":false,"path_style":"posix","tunnel_id":"%s","cluster":"usw2","port":47461,"target":{{"name":"%s","pid":123,"generation":"456"}},"endpoint":{{"hostId":"fixture","connectionMode":"TunnelRelay","hostPublicKeys":["{public}"]}}}}' "$name" "$name" > "$destination"
while :; do sleep 1; done"#
        ),
    );
    let mut child = fixture
        .command()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut output = Vec::new();
    timeout(Duration::from_secs(10), async {
        let mut bytes = [0; 1024];
        while !String::from_utf8_lossy(&output).contains("Keep this command running") {
            let count = stdout.read(&mut bytes).await.unwrap();
            assert_ne!(
                count,
                0,
                "setup stopped before READY: {}",
                String::from_utf8_lossy(&output)
            );
            output.extend_from_slice(&bytes[..count]);
        }
    })
    .await
    .unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("READY - experimental bridge"));
    assert!(output.contains(&fingerprint));
    assert!(!output.contains("synthetic-host-token"));
    assert!(fixture.root.join("token-stdin-accepted").is_file());
    let arguments = fs::read_to_string(fixture.root.join("bridge-args")).unwrap();
    assert!(arguments.contains("--token-stdin"));
    assert!(!arguments.contains("synthetic-host-token"));
    assert!(!arguments.contains("--allow-file-read"));
    let commands = fs::read_to_string(fixture.root.join("commands")).unwrap();
    assert!(commands.contains("--description festerm-bridge-v1"));
    assert!(commands.contains("--scopes host --json"));
    assert!(!commands.contains("--allow-anonymous"));
    assert!(!commands.contains("synthetic-host-token"));
    let status = Command::new("kill")
        .args(["-INT", &child.id().unwrap().to_string()])
        .status()
        .await
        .unwrap();
    assert!(status.success());
    assert!(timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    assert!(fixture.root.join("retained-shell").is_file());
}

#[tokio::test]
async fn setup_refuses_cli_failure_without_publishing_sensitive_stderr() {
    let fixture = Fixture::new();
    fixture.normal_cli("synthetic-host-token-not-a-credential");
    fixture.script("festerm-tunnel-bridge", "exit 0");
    fixture.script(
        "devtunnel",
        r#"
case "$1 $2" in
    'user login') exit 0 ;;
    'user show') printf '%s' '{"status":"Logged in","provider":"github"}' ;;
    *) printf 'SYNTHETIC-SENSITIVE-ERROR' >&2; exit 41 ;;
esac"#,
    );
    let output = timeout(Duration::from_secs(5), fixture.command().output())
        .await
        .unwrap()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("setup.create_tunnel"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("SYNTHETIC-SENSITIVE-ERROR"));
    assert!(!fixture.root.join("retained-shell").exists());
}

#[tokio::test]
async fn setup_refuses_invalid_token_without_starting_bridge_or_reporting_ready() {
    let fixture = Fixture::new();
    fixture.normal_cli(r"synthetic\ninvalid");
    fixture.script(
        "festerm-tunnel-bridge",
        r#"touch "$MOCK_ROOT/bridge-started""#,
    );
    let output = timeout(Duration::from_secs(5), fixture.command().output())
        .await
        .unwrap()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("setup.token_response"));
    assert!(!fixture.root.join("bridge-started").exists());
    assert!(fixture.root.join("retained-shell").exists());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("READY"));
}

#[tokio::test]
async fn setup_refuses_existing_state_directory_before_authentication() {
    let fixture = Fixture::new();
    fixture.normal_cli("synthetic-host-token-not-a-credential");
    fixture.script("festerm-tunnel-bridge", "exit 0");
    fs::create_dir(fixture.root.join("run")).unwrap();
    let output = timeout(Duration::from_secs(5), fixture.command().output())
        .await
        .unwrap()
        .unwrap();
    assert!(!output.status.success());
    assert!(!fixture.root.join("commands").exists());
}
