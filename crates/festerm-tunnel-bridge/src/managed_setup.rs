use super::{cli, decode_token, refused, sign_in, Created};
use festerm_tunnel_bridge::{
    host_identity::HostIdentity,
    relay::{host_fingerprint, Destination},
    setup_storage,
};
use fs2::FileExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};

const CONFIG: &str = "installation.json";
const READY: &str = "ready.json";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Target {
    name: String,
    pid: u32,
    generation: u128,
    endpoint: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Installation {
    version: u16,
    id: String,
    executable: PathBuf,
    cli: PathBuf,
    runtime_root: PathBuf,
    account: String,
    tunnel: Option<String>,
    phase: String,
    target: Option<Target>,
    fingerprint: String,
}

#[derive(Deserialize)]
struct Account {
    status: String,
    provider: String,
    #[serde(rename = "objectId")]
    id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    installation: String,
    destination: Destination,
}

fn read_json<T: DeserializeOwned>(directory: &File, name: &str) -> io::Result<T> {
    let bytes = setup_storage::read_private(directory, Path::new(name), 64 * 1024)?;
    serde_json::from_slice(&bytes).map_err(|_| refused("setup.managed_state_decode"))
}

fn checkpoint(directory: &File, record: &Installation) -> io::Result<()> {
    let bytes =
        serde_json::to_vec_pretty(record).map_err(|_| refused("setup.managed_state_encode"))?;
    setup_storage::replace_private(directory, Path::new(CONFIG), &bytes)
}

fn lock(directory: &File, name: &str) -> io::Result<File> {
    let file = match setup_storage::open_private_file(directory, Path::new(name), true) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            setup_storage::open_private_file(directory, Path::new(name), false)?
        }
        Err(error) => return Err(error),
    };
    Ok(file)
}

fn state_path(args: &[OsString]) -> io::Result<PathBuf> {
    let [flag, path] = args else {
        return Err(refused(
            "usage: --serve --state-dir ABSOLUTE_PRIVATE_DIRECTORY",
        ));
    };
    let path = PathBuf::from(path);
    if flag != "--state-dir" || !path.is_absolute() {
        return Err(refused("setup.absolute_state_path_required"));
    }
    Ok(path)
}

fn validate_record(record: &Installation) -> io::Result<()> {
    let id =
        uuid::Uuid::parse_str(&record.id).map_err(|_| refused("setup.installation_identity"))?;
    if record.version != 1
        || id.to_string() != record.id
        || !record.cli.is_absolute()
        || !record.executable.is_absolute()
        || daemon_namespace(&record.runtime_root).is_err()
        || record.account.is_empty()
        || record.account.len() > 256
        || !record.account.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(refused("setup.installation_identity"));
    }
    if let Some(tunnel) = &record.tunnel {
        let expected = format!("festerm-{}", id.simple());
        super::split_created_id(tunnel, &expected)?;
    }
    if let Some(target) = &record.target {
        let generation = format!("{}-{}", target.pid, target.generation);
        if target.name != format!("festerm-{}", id.simple())
            || target.pid == 0
            || target.generation == 0
            || !(target.endpoint.ends_with(&generation)
                || target.endpoint.ends_with(&format!("{generation}.sock")))
        {
            return Err(refused("setup.target_identity"));
        }
    }
    Ok(())
}

fn service(record: &Installation, state: &Path) -> super::service_install::ServiceSpec {
    super::service_install::ServiceSpec {
        id: record.id.clone(),
        executable: record.executable.clone(),
        state: state.into(),
    }
}

fn ready_record(directory: &File, record: &Installation) -> io::Result<Ready> {
    let ready: Ready = read_json(directory, READY)?;
    let target = record
        .target
        .as_ref()
        .ok_or_else(|| refused("setup.target_missing"))?;
    let qualified = record
        .tunnel
        .as_ref()
        .ok_or_else(|| refused("setup.tunnel_missing"))?;
    let (tunnel, cluster) = qualified
        .rsplit_once('.')
        .ok_or_else(|| refused("setup.tunnel_identity"))?;
    let destination = &ready.destination;
    if ready.installation != record.id
        || destination.version != 1
        || destination.file_read
        || destination.port != 47461
        || destination.tunnel_id != tunnel
        || destination.cluster != cluster
        || destination.target.name != target.name
        || destination.target.pid != target.pid
        || destination.target.generation != target.generation.to_string()
        || destination.endpoint.host_public_keys.len() != 1
        || host_fingerprint(&destination.endpoint.host_public_keys[0])? != record.fingerprint
    {
        return Err(refused("setup.ready_identity"));
    }
    Ok(ready)
}

fn healthy_record(directory: &File, record: &Installation) -> io::Result<Option<Ready>> {
    let health = lock(directory, "health.lock")?;
    match FileExt::try_lock_shared(&health) {
        Ok(()) => {}
        Err(error) if festerm_sessiond::lock_is_contended(&error) => return Ok(None),
        Err(error) => return Err(error),
    }
    let worker = lock(directory, "worker.lock")?;
    match worker.try_lock_exclusive() {
        Ok(()) => {
            FileExt::unlock(&worker)?;
            Ok(None)
        }
        Err(error) if festerm_sessiond::lock_is_contended(&error) => {
            exact_target_alive(
                &record.runtime_root,
                record
                    .target
                    .as_ref()
                    .ok_or_else(|| refused("setup.target_missing"))?,
            )?;
            ready_record(directory, record).map(Some)
        }
        Err(error) => Err(error),
    }
}

fn print_profile(record: &Installation, ready: &Ready) -> io::Result<()> {
    println!("READY - existing exact private tunnel/session; dedicated per-user service.");
    println!(
        "Tunnel: {}",
        record
            .tunnel
            .as_deref()
            .ok_or_else(|| refused("setup.tunnel_missing"))?
    );
    println!("Verify host fingerprint: {}", record.fingerprint);
    println!("Paste the following PUBLIC JSON in fesTerm's Dev Tunnel profile import; authenticate separately:");
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "festerm_profile_version": 1,
            "name": format!("Dev Tunnel {}", &record.id[..8]),
            "destination": ready.destination
        }))
        .map_err(|_| refused("setup.profile_encode"))?
    );
    println!(
        "No credentials are included. Host key stays in the protected target state directory."
    );
    println!("Service starts at user sign-in (Linux boot only when user lingering is already enabled). A reboot does not preserve the daemon shell; stale generations refuse, never silently create a replacement.");
    Ok(())
}

async fn verify_account(path: &Path) -> io::Result<String> {
    let bytes = cli(path, &["user", "show", "--json"], "setup.verify_account").await?;
    let account: Account =
        serde_json::from_slice(&bytes).map_err(|_| refused("setup.verify_account"))?;
    if account.provider != "github"
        || account.status != "Logged in"
        || account.id.is_empty()
        || account.id.len() > 256
        || !account.id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(refused("setup.verify_account"));
    }
    Ok(account.id)
}

fn daemon_namespace(root: &Path) -> io::Result<(&'static str, &Path)> {
    #[cfg(unix)]
    let (variable, component) = ("XDG_STATE_HOME", "festerm");
    #[cfg(windows)]
    let (variable, component) = ("LOCALAPPDATA", "fesTerm");
    let application = root
        .parent()
        .ok_or_else(|| refused("setup.daemon_runtime"))?;
    let base = application
        .parent()
        .ok_or_else(|| refused("setup.daemon_runtime"))?;
    if !root.is_absolute()
        || root.file_name() != Some(std::ffi::OsStr::new("sessiond"))
        || application.file_name() != Some(std::ffi::OsStr::new(component))
    {
        return Err(refused("setup.daemon_runtime"));
    }
    Ok((variable, base))
}

fn exact_target_alive(root: &Path, target: &Target) -> io::Result<()> {
    if !festerm_sessiond::daemon_generation_is_live(
        root,
        target.pid,
        target.generation,
        &target.endpoint,
    )
    .map_err(|_| refused("setup.daemon_identity"))?
    {
        return Err(refused("setup.exact_daemon_gone_no_replacement_created"));
    }
    Ok(())
}

pub(super) async fn install(args: &[OsString]) -> io::Result<()> {
    super::service_install::ensure_unprivileged_user()?;
    let (cli_path, state) = {
        let [flag, cli_path, state_flag, state] = args else {
            return Err(refused(
                "usage: --managed --devtunnel PATH --state-dir PRIVATE_DIRECTORY",
            ));
        };
        if flag != "--devtunnel" || state_flag != "--state-dir" {
            return Err(refused("setup.managed_arguments"));
        }
        (PathBuf::from(cli_path), PathBuf::from(state))
    };
    if !cli_path.is_absolute() || !state.is_absolute() {
        return Err(refused("setup.absolute_paths_required"));
    }
    let create = match fs::symlink_metadata(&state) {
        Ok(_) => false,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => return Err(error),
    };
    let directory = setup_storage::private_directory(&state, create)?;
    let installer = lock(&directory, "installer.lock")?;
    installer
        .try_lock_exclusive()
        .map_err(|_| refused("setup.other_installer_running"))?;
    let record = match read_json::<Installation>(&directory, CONFIG) {
        Ok(record) => record,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            refuse_orphaned_state(&state)?;
            let runtime_root =
                festerm_sessiond::runtime_root().map_err(|_| refused("setup.daemon_runtime"))?;
            daemon_namespace(&runtime_root)?;
            sign_in(&cli_path).await?;
            let account = verify_account(&cli_path).await?;
            let identity = HostIdentity::load_or_create(&state.join("host-identity.key"))?;
            let record = Installation {
                version: 1,
                id: uuid::Uuid::new_v4().to_string(),
                executable: std::env::current_exe()?,
                cli: cli_path,
                runtime_root,
                account,
                tunnel: None,
                phase: "creating-tunnel".into(),
                target: None,
                fingerprint: identity.fingerprint()?,
            };
            validate_record(&record)?;
            setup_storage::write_new_private(
                &directory,
                Path::new(CONFIG),
                &serde_json::to_vec_pretty(&record)
                    .map_err(|_| refused("setup.managed_state_encode"))?,
            )?;
            let name = format!(
                "festerm-{}",
                uuid::Uuid::parse_str(&record.id)
                    .map_err(|_| refused("setup.installation_identity"))?
                    .simple()
            );
            println!("Provisioning only this new private tunnel/session: {name}. Interrupted cloud mutations are journaled and never blindly retried.");
            let response = cli(
                &record.cli,
                &[
                    "create",
                    &name,
                    "--expiration",
                    "30d",
                    "--description",
                    "fesTerm managed private shell",
                    "--json",
                ],
                "setup.create_tunnel",
            )
            .await?;
            let created: Created =
                serde_json::from_slice(&response).map_err(|_| refused("setup.create_response"))?;
            super::split_created_id(&created.tunnel.tunnel_id, &name)?;
            let mut record = record;
            record.tunnel = Some(created.tunnel.tunnel_id);
            record.phase = "creating-port".into();
            checkpoint(&directory, &record)?;
            cli(
                &record.cli,
                &[
                    "port",
                    "create",
                    record
                        .tunnel
                        .as_deref()
                        .ok_or_else(|| refused("setup.tunnel_missing"))?,
                    "--port-number",
                    "47461",
                    "--description",
                    "festerm-bridge-v1",
                    "--json",
                ],
                "setup.create_port",
            )
            .await?;
            record.phase = "starting-session".into();
            checkpoint(&directory, &record)?;
            let helpers = record
                .executable
                .parent()
                .ok_or_else(|| refused("setup.helpers"))?;
            let daemon = helpers.join(format!("festerm-sessiond{}", std::env::consts::EXE_SUFFIX));
            let mut start = Command::new(daemon);
            let (variable, base) = daemon_namespace(&record.runtime_root)?;
            start.env(variable, base);
            start.args(["start", "--name", &name]);
            #[cfg(unix)]
            start.args(["--shell", "/bin/sh"]);
            #[cfg(windows)]
            start.arg("--shell").arg(
                PathBuf::from(
                    std::env::var_os("SystemRoot")
                        .ok_or_else(|| refused("setup.windows_system_root"))?,
                )
                .join("System32/cmd.exe"),
            );
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
                .kill_on_drop(true)
                .status()
                .await
                .map_err(|_| refused("setup.start_daemon"))?;
            if !status.success() {
                return Err(refused("setup.start_daemon"));
            }
            let selected = festerm_sessiond::list_unattached_sessions_in(&record.runtime_root)
                .map_err(|_| refused("setup.daemon_discovery"))?
                .into_iter()
                .find(|session| session.name == name)
                .ok_or_else(|| refused("setup.daemon_discovery"))?;
            record.target = Some(Target {
                name,
                pid: selected.pid,
                generation: selected.created_at_unix_ms,
                endpoint: selected.endpoint,
            });
            record.phase = "configured".into();
            checkpoint(&directory, &record)?;
            record
        }
        Err(error) => return Err(error),
    };
    validate_record(&record)?;
    if record.phase != "configured" {
        return Err(refused(
            "setup.incomplete_previous_operation_reconcile_explicitly_no_retry",
        ));
    }
    if let Some(ready) = healthy_record(&directory, &record)? {
        print_profile(&record, &ready)?;
        return Ok(());
    }
    exact_target_alive(
        &record.runtime_root,
        record
            .target
            .as_ref()
            .ok_or_else(|| refused("setup.target_missing"))?,
    )?;
    if verify_account(&record.cli).await? != record.account {
        return Err(refused("setup.changed_cli_account"));
    }
    // Keep an existing installation's executable/CLI paths: reruns cannot
    // replace a live helper or silently upgrade another daemon generation.
    let specification = service(&record, &state);
    super::service_install::install_and_start(&specification)?;
    timeout(Duration::from_secs(50), async {
        loop {
            if let Some(ready) = healthy_record(&directory, &record)? {
                return print_profile(&record, &ready);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| refused("setup.service_not_ready_check_private_service_logs"))??;
    Ok(())
}

pub(super) async fn serve(args: &[OsString]) -> io::Result<()> {
    super::service_install::ensure_unprivileged_user()?;
    let state = state_path(args)?;
    let directory = setup_storage::private_directory(&state, false)?;
    let health = lock(&directory, "health.lock")?;
    health
        .try_lock_exclusive()
        .map_err(|_| refused("setup.health_publication_busy"))?;
    let worker = lock(&directory, "worker.lock")?;
    worker
        .try_lock_exclusive()
        .map_err(|_| refused("setup.worker_already_running"))?;
    let record: Installation = read_json(&directory, CONFIG)?;
    validate_record(&record)?;
    if record.phase != "configured" {
        return Err(refused("setup.incomplete_installation"));
    }
    let target = record
        .target
        .as_ref()
        .ok_or_else(|| refused("setup.target_missing"))?;
    exact_target_alive(&record.runtime_root, target)?;
    if verify_account(&record.cli).await? != record.account {
        return Err(refused("setup.changed_cli_account"));
    }
    let identity = HostIdentity::load(&state.join("host-identity.key"))?;
    if identity.fingerprint()? != record.fingerprint {
        return Err(refused("setup.changed_host_identity"));
    }
    let qualified = record
        .tunnel
        .as_deref()
        .ok_or_else(|| refused("setup.tunnel_missing"))?;
    let (tunnel, cluster) = qualified
        .rsplit_once('.')
        .ok_or_else(|| refused("setup.tunnel_identity"))?;
    let token_bytes = cli(
        &record.cli,
        &["token", qualified, "--scopes", "host", "--json"],
        "setup.host_token",
    )
    .await?;
    let token = decode_token(&token_bytes)?;
    drop(token_bytes);
    if verify_account(&record.cli).await? != record.account {
        return Err(refused("setup.changed_cli_account_after_token"));
    }
    let run = state.join(format!("run-{}", uuid::Uuid::new_v4()));
    let _run_directory = setup_storage::private_directory(&run, true)?;
    let destination = run.join("destination.json");
    let bridge = record
        .executable
        .parent()
        .ok_or_else(|| refused("setup.helpers"))?
        .join(format!(
            "festerm-tunnel-bridge{}",
            std::env::consts::EXE_SUFFIX
        ));
    let (variable, base) = daemon_namespace(&record.runtime_root)?;
    let mut child = Command::new(bridge)
        .env(variable, base)
        .args([
            "--experimental",
            "--tunnel-id",
            tunnel,
            "--cluster",
            cluster,
            "--port",
            "47461",
            "--session",
            &target.name,
            "--session-pid",
            &target.pid.to_string(),
            "--session-generation",
            &target.generation.to_string(),
        ])
        .arg("--destination")
        .arg(&destination)
        .arg("--host-identity")
        .arg(state.join("host-identity.key"))
        .arg("--existing-host-identity")
        .arg("--token-stdin")
        .stdin(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| refused("setup.start_bridge"))?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| refused("setup.bridge_input"))?;
    timeout(Duration::from_secs(5), async {
        input.write_all(token.as_bytes()).await?;
        input.write_all(b"\n").await
    })
    .await
    .map_err(|_| refused("setup.bridge_input_timeout"))??;
    drop(input);
    drop(token);
    let ready = timeout(Duration::from_secs(40), async {
        loop {
            if child.try_wait()?.is_some() {
                return Err(refused("setup.bridge_exited_before_ready"));
            }
            match fs::File::open(&destination) {
                Ok(file) => {
                    use std::io::Read;
                    let mut bytes = Vec::new();
                    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
                    if bytes.len() > 64 * 1024 {
                        return Err(refused("setup.destination_size"));
                    }
                    if let Ok(destination) = serde_json::from_slice::<Destination>(&bytes) {
                        return Ok(Ready {
                            installation: record.id.clone(),
                            destination,
                        });
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| refused("setup.bridge_ready_timeout"))??;
    setup_storage::replace_private(
        &directory,
        Path::new(READY),
        &serde_json::to_vec(&ready).map_err(|_| refused("setup.ready_encode"))?,
    )?;
    ready_record(&directory, &record)?;
    FileExt::unlock(&health)?;
    eprintln!("Managed experimental bridge ready; exact generation, no file capability. Reauthentication or a stale daemon requires explicit operator action.");
    let status = child
        .wait()
        .await
        .map_err(|_| refused("setup.bridge_wait"))?;
    health.try_lock_exclusive()?;
    if destination.exists() {
        fs::remove_file(&destination)?;
    }
    fs::remove_dir(&run)?;
    fs::remove_file(state.join(READY))?;
    if !status.success() {
        return Err(refused("setup.bridge_stopped"));
    }
    Ok(())
}

fn refuse_orphaned_state(state: &Path) -> io::Result<()> {
    for entry in fs::read_dir(state)? {
        if entry?.file_name() != "installer.lock" {
            return Err(refused(
                "setup.missing_journal_with_existing_state_no_replacement",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn managed_state_refuses_foreign_tunnel_account_and_generation() {
        let id = uuid::Uuid::new_v4();
        let mut record = Installation {
            version: 1,
            id: id.to_string(),
            executable: std::env::current_exe().unwrap(),
            cli: std::env::current_exe().unwrap(),
            runtime_root: festerm_sessiond::runtime_root().unwrap(),
            account: "123".into(),
            tunnel: Some(format!("festerm-{}.usw2", id.simple())),
            phase: "configured".into(),
            target: Some(Target {
                name: format!("festerm-{}", id.simple()),
                pid: 123,
                generation: 456,
                endpoint: "fixture-123-456".into(),
            }),
            fingerprint: "fixture".into(),
        };
        validate_record(&record).unwrap();
        record.tunnel = Some("foreign.usw2".into());
        assert!(validate_record(&record).is_err());
        record.tunnel = None;
        record.account = "other account".into();
        assert!(validate_record(&record).is_err());
        record.account = "123".into();
        record.target.as_mut().unwrap().pid = 0;
        assert!(validate_record(&record).is_err());
    }

    #[test]
    fn managed_state_refuses_unknown_credentials_and_relative_paths() {
        let id = uuid::Uuid::new_v4();
        let mut value = serde_json::json!({
            "version":1,"id":id.to_string(),"executable":std::env::current_exe().unwrap(),
            "cli":std::env::current_exe().unwrap(),"account":"123","tunnel":null,
            "runtime_root":festerm_sessiond::runtime_root().unwrap(),
            "phase":"creating-tunnel","target":null,"fingerprint":"fixture"
        });
        assert!(serde_json::from_value::<Installation>(value.clone()).is_ok());
        value["token"] = serde_json::json!("synthetic-not-a-credential");
        assert!(serde_json::from_value::<Installation>(value.clone()).is_err());
        value.as_object_mut().unwrap().remove("token");
        value["executable"] = serde_json::json!("relative-helper");
        let record = serde_json::from_value::<Installation>(value).unwrap();
        assert!(validate_record(&record).is_err());
    }

    #[test]
    fn managed_checkpoint_and_lock_are_private_and_reusable() {
        let root = crate::test_support::private_directory("festerm-managed-state");
        let directory = setup_storage::private_directory(&root, false).unwrap();
        let first = lock(&directory, "installer.lock").unwrap();
        refuse_orphaned_state(&root).unwrap();
        setup_storage::write_new_private(&directory, Path::new("host-identity.key"), b"fixture")
            .unwrap();
        assert!(refuse_orphaned_state(&root).is_err());
        fs::remove_file(root.join("host-identity.key")).unwrap();
        first.try_lock_exclusive().unwrap();
        let second = lock(&directory, "installer.lock").unwrap();
        assert!(festerm_sessiond::lock_is_contended(
            &second.try_lock_exclusive().unwrap_err()
        ));
        FileExt::unlock(&first).unwrap();
        second.try_lock_exclusive().unwrap();
        drop(first);
        drop(second);
        drop(directory);
        fs::remove_file(root.join("installer.lock")).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn managed_runtime_namespace_is_explicit_and_never_uses_worker_environment() {
        let runtime = festerm_sessiond::runtime_root().unwrap();
        let (variable, base) = daemon_namespace(&runtime).unwrap();
        #[cfg(unix)]
        assert_eq!(
            (variable, base.join("festerm/sessiond")),
            ("XDG_STATE_HOME", runtime.clone())
        );
        #[cfg(windows)]
        assert_eq!(
            (variable, base.join("fesTerm/sessiond")),
            ("LOCALAPPDATA", runtime.clone())
        );
        assert!(daemon_namespace(Path::new("relative/festerm/sessiond")).is_err());
        assert!(daemon_namespace(&base.join("foreign/sessiond")).is_err());
    }

    #[test]
    fn managed_healthy_reuse_preserves_metadata_and_refuses_a_dead_generation() {
        let root = crate::test_support::private_directory("festerm-managed-reuse");
        let directory = setup_storage::private_directory(&root, false).unwrap();
        #[cfg(unix)]
        let application_name = "festerm";
        #[cfg(windows)]
        let application_name = "fesTerm";
        let application_path = root.join(application_name);
        let application = setup_storage::private_directory(&application_path, true).unwrap();
        let runtime_root = application_path.join("sessiond");
        let runtime = setup_storage::private_directory(&runtime_root, true).unwrap();
        let id = uuid::Uuid::new_v4();
        let pid = std::process::id();
        let generation = 777;
        let lease_name = format!("lease-{pid}-{generation}");
        let lease =
            setup_storage::open_private_file(&runtime, Path::new(&lease_name), true).unwrap();
        lease.try_lock_exclusive().unwrap();
        let public_key = "AAAAC3NzaC1lZDI1NTE5AAAAINdamAGCsQq31Uv+08lkBzoO4XLz2qYjJa8CGmj3B1Ea";
        let name = format!("festerm-{}", id.simple());
        let record = Installation {
            version: 1,
            id: id.to_string(),
            executable: std::env::current_exe().unwrap(),
            cli: root.join("must-not-execute"),
            runtime_root: runtime_root.clone(),
            account: "123".into(),
            tunnel: Some(format!("{name}.usw2")),
            phase: "configured".into(),
            target: Some(Target {
                name: name.clone(),
                pid,
                generation,
                endpoint: format!("fixture-{pid}-{generation}.sock"),
            }),
            fingerprint: host_fingerprint(public_key).unwrap(),
        };
        validate_record(&record).unwrap();
        checkpoint(&directory, &record).unwrap();
        let destination = serde_json::from_value(serde_json::json!({
            "version": 1,
            "file_read": false,
            "path_style": festerm_tunnel_bridge::files::PathStyle::native(),
            "tunnel_id": name,
            "cluster": "usw2",
            "port": 47461,
            "target": {"name": name, "pid": pid, "generation": generation.to_string()},
            "endpoint": {
                "connectionMode": "TunnelRelay",
                "hostId": id.to_string(),
                "hostPublicKeys": [public_key],
                "clientRelayUri": "wss://test.rel.tunnels.api.visualstudio.com/client"
            }
        }))
        .unwrap();
        let ready = serde_json::to_vec(&Ready {
            installation: record.id.clone(),
            destination,
        })
        .unwrap();
        setup_storage::write_new_private(&directory, Path::new(READY), &ready).unwrap();
        let worker = lock(&directory, "worker.lock").unwrap();
        worker.try_lock_exclusive().unwrap();
        let health = lock(&directory, "health.lock").unwrap();
        health.try_lock_exclusive().unwrap();
        assert!(healthy_record(&directory, &record).unwrap().is_none());
        FileExt::unlock(&health).unwrap();
        drop(health);
        let before = setup_storage::read_private(&directory, Path::new(CONFIG), 65536).unwrap();
        for _ in 0..2 {
            let reused = healthy_record(&directory, &record).unwrap().unwrap();
            assert_eq!(serde_json::to_vec(&reused).unwrap(), ready);
        }
        assert_eq!(
            &*before,
            &*setup_storage::read_private(&directory, Path::new(CONFIG), 65536).unwrap()
        );
        FileExt::unlock(&lease).unwrap();
        assert!(healthy_record(&directory, &record).is_err());
        FileExt::unlock(&worker).unwrap();
        assert!(healthy_record(&directory, &record).unwrap().is_none());
        drop(worker);
        drop(lease);
        drop(runtime);
        drop(application);
        drop(directory);
        fs::remove_file(runtime_root.join(lease_name)).unwrap();
        fs::remove_dir(runtime_root).unwrap();
        fs::remove_dir(application_path).unwrap();
        for name in [CONFIG, READY, "worker.lock", "health.lock"] {
            fs::remove_file(root.join(name)).unwrap();
        }
        fs::remove_dir(root).unwrap();
    }
}
