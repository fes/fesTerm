use std::{
    ffi::OsString,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use festerm_tunnel_bridge::relay::{host_fingerprint, Destination};
use serde::Deserialize;
use tokio::{io::AsyncReadExt, io::AsyncWriteExt, process::Command, time::timeout};
use zeroize::{Zeroize, Zeroizing};

const PORT: &str = "47461";
const MAX_RESPONSE: u64 = 64 * 1024;
const CLI_TIMEOUT: Duration = Duration::from_secs(60);

mod managed_setup;
mod service_install;

fn refused(stage: &'static str) -> io::Error {
    io::Error::other(stage)
}

#[derive(Deserialize)]
struct Account {
    status: String,
    provider: String,
}

#[derive(Deserialize)]
struct Created {
    tunnel: CreatedTunnel,
}

#[derive(Deserialize)]
struct CreatedTunnel {
    #[serde(rename = "tunnelId")]
    tunnel_id: String,
}

#[derive(Deserialize)]
struct ScopedToken {
    token: String,
}

impl Drop for ScopedToken {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

fn decode_token(bytes: &[u8]) -> io::Result<Zeroizing<String>> {
    let mut response: ScopedToken =
        serde_json::from_slice(bytes).map_err(|_| refused("setup.token_response"))?;
    if response.token.is_empty()
        || response.token.len() > 16 * 1024
        || !response
            .token
            .bytes()
            .all(|byte| (33..=126).contains(&byte))
    {
        return Err(refused("setup.token_response"));
    }
    Ok(Zeroizing::new(std::mem::take(&mut response.token)))
}

fn split_created_id(value: &str, expected: &str) -> io::Result<(String, String)> {
    let (id, cluster) = value
        .rsplit_once('.')
        .ok_or_else(|| refused("setup.created_tunnel_identity"))?;
    if id != expected
        || cluster.is_empty()
        || cluster.len() > 32
        || !cluster.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(refused("setup.created_tunnel_identity"));
    }
    Ok((id.into(), cluster.into()))
}

async fn cli(path: &Path, args: &[&str], stage: &'static str) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut child = Command::new(path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| refused(stage))?;
    let mut stdout = child.stdout.take().ok_or_else(|| refused(stage))?;
    let result = timeout(CLI_TIMEOUT, async {
        let mut bytes = Zeroizing::new(Vec::new());
        (&mut stdout)
            .take(MAX_RESPONSE + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| refused(stage))?;
        if bytes.len() as u64 > MAX_RESPONSE {
            return Err(refused(stage));
        }
        let status = child.wait().await.map_err(|_| refused(stage))?;
        if !status.success() {
            return Err(refused(stage));
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| refused(stage))??;
    Ok(result)
}

async fn sign_in(path: &Path) -> io::Result<()> {
    // Login is deliberately interactive; never capture a device code or tokens in a log.
    let status = Command::new(path)
        .args(["user", "login", "--github", "--use-device-code-auth"])
        .kill_on_drop(true)
        .status()
        .await
        .map_err(|_| refused("setup.github_login"))?;
    if !status.success() {
        return Err(refused("setup.github_login"));
    }
    let response = cli(path, &["user", "show", "--json"], "setup.verify_account").await?;
    let account: Account =
        serde_json::from_slice(&response).map_err(|_| refused("setup.verify_account"))?;
    if account.provider != "github" || account.status != "Logged in" {
        return Err(refused("setup.verify_account"));
    }
    Ok(())
}

fn private_directory(path: &Path) -> io::Result<fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(path)?;
        let directory = fs::File::open(path)?;
        festerm_unix_security::secure_staging_parent(&directory)?;
        Ok(directory)
    }
    #[cfg(windows)]
    {
        let parent = path
            .parent()
            .ok_or_else(|| refused("setup.state_directory"))?;
        let name = path
            .file_name()
            .ok_or_else(|| refused("setup.state_directory"))?;
        let directory =
            festerm_windows_security::lock_directory_path_without_delete_sharing(parent)?;
        festerm_windows_security::create_current_user_only_directory(&directory, Path::new(name))
    }
}

fn arguments() -> io::Result<(PathBuf, PathBuf)> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.len() != 4 || args[0] != "--devtunnel" || args[2] != "--state-dir" {
        return Err(refused(
            "usage: festerm-tunnel-setup --devtunnel PATH --state-dir NEW_DIRECTORY",
        ));
    }
    let cli = PathBuf::from(&args[1]);
    let state = PathBuf::from(&args[3]);
    if !cli.is_absolute() || !state.is_absolute() {
        return Err(refused("setup.absolute_paths_required"));
    }
    Ok((cli, state))
}

async fn run() -> io::Result<()> {
    let (devtunnel, state) = arguments()?;
    let executable = std::env::current_exe()?;
    let helpers = executable
        .parent()
        .ok_or_else(|| refused("setup.helpers"))?;
    let suffix = std::env::consts::EXE_SUFFIX;
    let daemon = helpers.join(format!("festerm-sessiond{suffix}"));
    let bridge = helpers.join(format!("festerm-tunnel-bridge{suffix}"));
    if !daemon.is_file() || !bridge.is_file() || !devtunnel.is_file() {
        return Err(refused("setup.helpers_missing"));
    }
    let _directory = private_directory(&state)?;
    println!("Explicit experimental setup: a NEW private tunnel and shell; no service, firewall or existing tunnel changes.");
    println!("Sign in to GitHub using the official Dev Tunnels CLI. Its own credential storage policy applies.");
    sign_in(&devtunnel).await?;
    let name = format!("festerm-{}", uuid::Uuid::new_v4().simple());
    let response = cli(
        &devtunnel,
        &[
            "create",
            &name,
            "--expiration",
            "30d",
            "--description",
            "fesTerm experimental private shell",
            "--json",
        ],
        "setup.create_tunnel",
    )
    .await?;
    let created: Created =
        serde_json::from_slice(&response).map_err(|_| refused("setup.create_response"))?;
    let (id, cluster) = split_created_id(&created.tunnel.tunnel_id, &name)?;
    let qualified = format!("{id}.{cluster}");
    println!("Created private tunnel: {qualified} (30-day expiry). Remove only this tunnel with: devtunnel delete {qualified}");
    cli(
        &devtunnel,
        &[
            "port",
            "create",
            &qualified,
            "--port-number",
            PORT,
            "--description",
            "festerm-bridge-v1",
            "--json",
        ],
        "setup.create_port",
    )
    .await?;
    let mut start = Command::new(&daemon);
    start.args(["start", "--name", &name]);
    #[cfg(windows)]
    start.arg("--shell").arg(
        PathBuf::from(
            std::env::var_os("SystemRoot").ok_or_else(|| refused("setup.windows_system_root"))?,
        )
        .join("System32")
        .join("cmd.exe"),
    );
    #[cfg(unix)]
    start.args(["--shell", "/bin/sh"]);
    let status = start
        .current_dir(&state)
        .args([
            "--cols",
            "80",
            "--rows",
            "24",
            "--scrollback-limit-bytes",
            "1048576",
        ])
        .status()
        .await
        .map_err(|_| refused("setup.start_daemon"))?;
    if !status.success() {
        return Err(refused("setup.start_daemon"));
    }
    println!(
        "Retained shell session: {name}. Stopping setup/bridge does not terminate this shell."
    );
    let token_bytes = cli(
        &devtunnel,
        &["token", &qualified, "--scopes", "host", "--json"],
        "setup.host_token",
    )
    .await?;
    let token = decode_token(&token_bytes)?;
    drop(token_bytes);
    let destination = state.join("destination.json");
    let mut child = Command::new(&bridge)
        .args([
            "--experimental",
            "--tunnel-id",
            &id,
            "--cluster",
            &cluster,
            "--port",
            PORT,
            "--session",
            &name,
        ])
        .arg("--destination")
        .arg(&destination)
        .arg("--token-stdin")
        .stdin(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| refused("setup.start_bridge"))?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| refused("setup.bridge_input"))?;
    input.write_all(token.as_bytes()).await?;
    input.write_all(b"\n").await?;
    drop(input);
    drop(token);
    let metadata = timeout(Duration::from_secs(40), async {
        loop {
            if child.try_wait()?.is_some() {
                return Err(refused("setup.bridge_exited_before_ready"));
            }
            match fs::File::open(&destination) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
                    if bytes.len() > 64 * 1024 {
                        return Err(refused("setup.destination_size"));
                    }
                    // The bridge creates the file before writing; wait for its complete JSON.
                    if let Ok(metadata) = serde_json::from_slice::<Destination>(&bytes) {
                        break Ok(metadata);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => return Err(refused("setup.destination_read")),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| refused("setup.bridge_ready_timeout"))??;
    if metadata.version != 1
        || metadata.tunnel_id != id
        || metadata.cluster != cluster
        || metadata.port != 47461
        || metadata.target.name != name
        || metadata.file_read
    {
        return Err(refused("setup.destination_identity"));
    }
    let key = metadata
        .endpoint
        .host_public_keys
        .first()
        .ok_or_else(|| refused("setup.host_key"))?;
    println!("READY - experimental bridge (foreground; no reboot or automatic-refresh guarantee)");
    println!("fesTerm Tunnel: {qualified}");
    println!("Verify host fingerprint: {}", host_fingerprint(key)?);
    println!(
        "Trusted destination file (public metadata, NO credentials): {}",
        destination.display()
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&metadata).map_err(|_| refused("setup.destination_encode"))?
    );
    println!("Native GitHub discovery remains unqualified (management HTTP 401 in the live fixture); use Advanced destination-file mode with a scoped connect token supplied privately on stdin. Never paste tokens into chat, command arguments or ordinary files.");
    println!("Keep this command running to host. Ctrl+C detaches; the shell remains.");
    tokio::select! {
        status = child.wait() => {
            let status = status.map_err(|_| refused("setup.bridge_wait"))?;
            if !status.success() { return Err(refused("setup.bridge_stopped")); }
        }
        result = tokio::signal::ctrl_c() => {
            result?;
            if child.try_wait()?.is_none() {
                child.start_kill()?;
                child.wait().await?;
            }
        }
    }
    Ok(())
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args == ["--version"] {
        println!(
            "festerm-tunnel-setup {} experimental",
            env!("CARGO_PKG_VERSION")
        );
        return;
    }
    if args == ["--help"] {
        println!("festerm-tunnel-setup [--managed] --devtunnel ABSOLUTE_CLI_PATH --state-dir ABSOLUTE_PRIVATE_DIRECTORY\nDefault: new foreground-only setup in a new directory.\n--managed: explicit per-user service setup; healthy reruns reuse the original private tunnel, host identity and exact shell generation, printing public profile JSON without credentials. Stale/partial/foreign state refuses, never creates a replacement.\n--serve --state-dir ABSOLUTE_PRIVATE_DIRECTORY: existing installation worker, no provisioning or key regeneration.\nFirst setup uses interactive official GitHub login and creates one private 30-day tunnel, marked virtual port and retained /bin/sh or cmd.exe session. Managed startup requires an unelevated user; sign-in auto-start does not preserve a shell through reboot. No remote file access, ACL widening, takeover or automatic reconnect.");
        return;
    }
    let result = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .and_then(|runtime| {
            runtime.block_on(async {
                match args.first().and_then(|arg| arg.to_str()) {
                    Some("--managed") => managed_setup::install(&args[1..]).await,
                    Some("--serve") => managed_setup::serve(&args[1..]).await,
                    _ => run().await,
                }
            })
        });
    if let Err(error) = result {
        eprintln!("Experimental tunnel setup refused/stopped: {error}. Any explicitly reported new tunnel/session remains; existing resources were not removed.");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_host_token_decoding_rejects_empty_multiline_nonascii_and_oversized() {
        for token in ["", "a\nb", "abc def", "é", &"a".repeat(16 * 1024 + 1)] {
            let json = serde_json::json!({"token": token}).to_string();
            assert!(decode_token(json.as_bytes()).is_err());
        }
        assert_eq!(
            &**decode_token(br#"{"token":"abc._-123","scope":"host"}"#).unwrap(),
            "abc._-123"
        );
    }

    #[test]
    fn created_tunnel_must_match_only_the_new_owned_id() {
        assert_eq!(
            split_created_id("festerm-owned.usw2", "festerm-owned").unwrap(),
            ("festerm-owned".into(), "usw2".into())
        );
        for value in [
            "other.usw2",
            "festerm-owned",
            "festerm-owned.",
            "festerm-owned.us/w2",
            "festerm-owned.usw2.extra",
        ] {
            assert!(split_created_id(value, "festerm-owned").is_err());
        }
    }

    #[test]
    fn setup_cli_schema_is_typed_without_capturing_unneeded_token_fields() {
        let created: Created = serde_json::from_str(
            r#"{"tunnel":{"tunnelId":"festerm-owned.usw2","accessControl":[]}}"#,
        )
        .unwrap();
        assert_eq!(created.tunnel.tunnel_id, "festerm-owned.usw2");
        assert!(serde_json::from_str::<Created>(r#"{"tunnelId":"wrong-shape"}"#).is_err());
    }
}
