use super::{cli, decode_token, refused, sign_in, Created};
use festerm_tunnel_bridge::{
    host_identity::HostIdentity,
    relay::{host_fingerprint, Destination},
    setup_storage,
    worker_log::{self, WorkerLog},
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
const PROFILE_JSON: &str = "festerm-profile.json";

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
    /// Remote takeover is on unless this is set; absent from older records.
    #[serde(default, skip_serializing_if = "is_false")]
    no_takeover: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recovery: Option<DaemonRecovery>,
    /// Helper executables an interrupted explicit upgrade may still find in
    /// the service definition; empty (and absent) outside phase `upgrading`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    upgrading_from: Vec<PathBuf>,
}

const UPGRADING: &str = "upgrading";
const MAX_UPGRADE_SOURCES: usize = 8;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DaemonRecovery {
    attempts: Vec<u128>,
    pending_generation: Option<u128>,
}

impl DaemonRecovery {
    fn reserve(&mut self, now: u128, generation: u128) -> io::Result<()> {
        if self.attempts.iter().any(|time| *time > now) {
            return Err(refused("setup.daemon_restart_clock_changed"));
        }
        self.attempts.retain(|time| now - *time < 600_000);
        if self.attempts.len() >= 3 {
            return Err(refused("setup.daemon_restart_limit"));
        }
        self.attempts.push(now);
        self.pending_generation = Some(generation);
        Ok(())
    }
}

fn native_target(target: &Target) -> festerm_sessiond::NativeSessionIdentity {
    festerm_sessiond::NativeSessionIdentity {
        name: target.name.clone(),
        pid: target.pid,
        created_at_unix_ms: target.generation,
        endpoint: target.endpoint.clone(),
    }
}

fn is_false(value: &bool) -> bool {
    !*value
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
    if (record.phase == UPGRADING) == record.upgrading_from.is_empty()
        || record.upgrading_from.len() > MAX_UPGRADE_SOURCES
        || !record.upgrading_from.iter().all(|path| path.is_absolute())
    {
        return Err(refused("setup.installation_identity"));
    }
    if record
        .recovery
        .as_ref()
        .is_some_and(|policy| policy.attempts.len() > 3 || policy.pending_generation == Some(0))
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
            let alive = exact_target_alive(
                &record.runtime_root,
                record
                    .target
                    .as_ref()
                    .ok_or_else(|| refused("setup.target_missing"))?,
            );
            if let Err(error) = alive {
                if error.to_string() == "setup.exact_daemon_gone_no_replacement_created" {
                    return Ok(None);
                }
                return Err(error);
            }
            ready_record(directory, record).map(Some)
        }
        Err(error) => Err(error),
    }
}

fn print_profile(state: &Path, record: &Installation, ready: &Ready) -> io::Result<()> {
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
    let profile = serde_json::to_string_pretty(&serde_json::json!({
        "festerm_profile_version": 1,
        "name": format!("Dev Tunnel {}", &record.id[..8]),
        "destination": ready.destination
    }))
    .map_err(|_| refused("setup.profile_encode"))?;
    println!("{profile}");
    // Public data only; the installers read it back to copy it for the user.
    let path = state.join(PROFILE_JSON);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::io::Write::write_all(
        &mut fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?,
        format!("{profile}\n").as_bytes(),
    )?;
    println!(
        "No credentials are included. Host key stays in the protected target state directory."
    );
    println!("Service starts at user sign-in (Linux boot only when user lingering is already enabled). A reboot does not preserve the daemon shell.");
    if record.recovery.is_some() {
        println!("Unexpected daemon failures may start a NEW shell (at most three in ten minutes). Intentional exit/stop stays stopped. Reconnect explicitly to the new generation; running programs and terminal state are not restored.");
    } else {
        println!("Stale generations refuse; no replacement shell is created automatically.");
    }
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
    let ManagedArguments {
        cli: cli_path,
        state,
        no_takeover,
        upgrade,
        friendly_name,
        restart_unexpected,
    } = managed_arguments(args)?;
    if !cli_path.is_absolute() || !state.is_absolute() {
        return Err(refused("setup.absolute_paths_required"));
    }
    let create = match fs::symlink_metadata(&state) {
        Ok(_) => false,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => return Err(error),
    };
    if !create && friendly_name.is_some() {
        return Err(refused(
            "setup.friendly_name_only_applies_to_new_hosts_rename_existing_tunnels_in_festerm",
        ));
    }
    if upgrade && create {
        return Err(refused("setup.upgrade_requires_an_existing_installation"));
    }
    super::service_install::ensure_unprivileged_user()?;
    #[cfg(windows)]
    super::service_install::check_windows_worker(&std::env::current_exe()?)?;
    let directory = setup_storage::private_directory(&state, create)?;
    let installer = lock(&directory, "installer.lock")?;
    installer
        .try_lock_exclusive()
        .map_err(|_| refused("setup.other_installer_running"))?;
    let record = match read_json::<Installation>(&directory, CONFIG) {
        Ok(record) => record,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if upgrade {
                return Err(refused("setup.upgrade_requires_an_existing_installation"));
            }
            refuse_orphaned_state(&state)?;
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            super::service_install::preflight_registration_directory()?;
            super::service_install::preflight_service_manager()?;
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
                cli: cli_path.clone(),
                runtime_root,
                account,
                tunnel: None,
                phase: "creating-tunnel".into(),
                target: None,
                fingerprint: identity.fingerprint()?,
                no_takeover,
                recovery: restart_unexpected
                    .unwrap_or(false)
                    .then(DaemonRecovery::default),
                upgrading_from: Vec::new(),
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
            let description = friendly_name.unwrap_or_else(default_friendly_name);
            println!("Provisioning only this new private tunnel/session: {name}. Interrupted cloud mutations are journaled and never blindly retried.");
            let response = cli(
                &record.cli,
                &[
                    "create",
                    &name,
                    "--expiration",
                    "30d",
                    "--description",
                    &description,
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
            super::shell_command(&mut start);
            if let Some(home) = super::shell_home() {
                start.arg("--cwd").arg(home);
            }
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
    let executable = std::env::current_exe()?;
    let changing_recovery =
        restart_unexpected.is_some_and(|enabled| enabled != record.recovery.is_some());
    if changing_recovery && record.executable != executable && !upgrade {
        return Err(refused("setup.daemon_recovery_requires_helper_upgrade"));
    }
    let plan = rerun_plan(&record, &executable, &cli_path, no_takeover, upgrade)?;
    let plan = if changing_recovery {
        Rerun::Upgrade
    } else {
        plan
    };
    let mut record = match plan {
        Rerun::Keep(hint) => {
            if let Some(hint) = hint {
                println!("{hint}");
            }
            record
        }
        Rerun::AlreadyCurrent => {
            println!("Already running this release's helpers; --upgrade has nothing to switch.");
            record
        }
        Rerun::Upgrade => {
            upgrade_installation(
                &directory,
                &state,
                &record,
                executable.clone(),
                cli_path,
                no_takeover,
                restart_unexpected == Some(false),
            )
            .await?
        }
    };
    if changing_recovery {
        record.recovery = restart_unexpected
            .unwrap_or(false)
            .then(DaemonRecovery::default);
        checkpoint(&directory, &record)?;
    }
    if record.executable == executable
        && super::service_install::migrate_legacy_definition(&service(&record, &state))?
    {
        println!("Enabled automatic worker restart for this host.");
    }
    if let Some(ready) = healthy_record(&directory, &record)? {
        print_profile(&state, &record, &ready)?;
        return Ok(());
    }
    if record.recovery.is_none() {
        let alive = exact_target_alive(
            &record.runtime_root,
            record
                .target
                .as_ref()
                .ok_or_else(|| refused("setup.target_missing"))?,
        );
        if changing_recovery
            && restart_unexpected == Some(false)
            && alive.as_ref().is_err_and(|error| {
                error.to_string() == "setup.exact_daemon_gone_no_replacement_created"
            })
        {
            println!("Automatic daemon replacement disabled. The exported shell has ended; hosting remains stopped.");
            return Ok(());
        }
        alive?;
    }
    if verify_account(&record.cli).await? != record.account {
        return Err(refused("setup.changed_cli_account"));
    }
    // Keep an existing installation's executable/CLI paths: only an explicit
    // --upgrade switches helpers, and never replaces the daemon generation.
    let specification = service(&record, &state);
    super::service_install::install_and_start(&specification)?;
    timeout(Duration::from_secs(50), async {
        loop {
            let record: Installation = read_json(&directory, CONFIG)?;
            if let Some(ready) = healthy_record(&directory, &record)? {
                return print_profile(&state, &record, &ready);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| refused("setup.service_not_ready_check_private_service_logs"))??;
    Ok(())
}

struct ManagedArguments {
    cli: PathBuf,
    state: PathBuf,
    no_takeover: bool,
    upgrade: bool,
    friendly_name: Option<String>,
    restart_unexpected: Option<bool>,
}

fn managed_arguments(args: &[OsString]) -> io::Result<ManagedArguments> {
    let usage = || {
        refused(
            "usage: --managed --devtunnel PATH --state-dir PRIVATE_DIRECTORY [--no-takeover] [--upgrade] [--friendly-name NAME] [--restart-unexpected|--no-restart-unexpected]",
        )
    };
    let [flag, cli, state_flag, state, options @ ..] = args else {
        return Err(usage());
    };
    if flag != "--devtunnel" || state_flag != "--state-dir" {
        return Err(refused("setup.managed_arguments"));
    }
    let (mut no_takeover, mut upgrade, mut friendly_name) = (false, false, None);
    let mut restart_unexpected = None;
    let mut index = 0;
    while index < options.len() {
        let option = &options[index];
        let seen = if option == "--restart-unexpected" || option == "--no-restart-unexpected" {
            if restart_unexpected
                .replace(option == "--restart-unexpected")
                .is_some()
            {
                return Err(usage());
            }
            index += 1;
            continue;
        } else if option == "--no-takeover" {
            &mut no_takeover
        } else if option == "--upgrade" {
            &mut upgrade
        } else if option == "--friendly-name" {
            if friendly_name.is_some() || index + 1 >= options.len() {
                return Err(usage());
            }
            let name = options[index + 1].to_str().ok_or_else(usage)?;
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
                return Err(refused("setup.invalid_friendly_name"));
            }
            friendly_name = Some(name.to_owned());
            index += 2;
            continue;
        } else {
            return Err(usage());
        };
        if std::mem::replace(seen, true) {
            return Err(usage());
        }
        index += 1;
    }
    Ok(ManagedArguments {
        cli: PathBuf::from(cli),
        state: PathBuf::from(state),
        no_takeover,
        upgrade,
        friendly_name,
        restart_unexpected,
    })
}

fn default_friendly_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty() && name.chars().count() <= 100)
        .unwrap_or_else(|| "fesTerm host".to_owned())
}

/// The release a helper directory was installed from, as the installer
/// recorded it next to the binaries; the directory itself as a fallback.
fn release_label(executable: &Path) -> String {
    let Some(directory) = executable.parent() else {
        return executable.display().to_string();
    };
    fs::read(directory.join("VERSION"))
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|text| text.trim().to_owned())
        .filter(|version| {
            !version.is_empty()
                && version.len() <= 32
                && version.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        })
        .unwrap_or_else(|| directory.display().to_string())
}

fn upgrade_hint(record: &Installation, executable: &Path) -> Option<String> {
    (record.executable != executable).then(|| {
        format!(
            "NOTE: this host is running fesTerm {} helpers; this installer is {}. Re-run the installer with --upgrade (-Upgrade on Windows) to switch to this version, keeping the tunnel, host key and session.",
            release_label(&record.executable),
            release_label(executable),
        )
    })
}

#[derive(Debug, PartialEq)]
enum Rerun {
    /// Plain rerun: keep the recorded helpers, optionally hinting at --upgrade.
    Keep(Option<String>),
    /// --upgrade, but the recorded helpers already are this release's.
    AlreadyCurrent,
    Upgrade,
}

fn rerun_plan(
    record: &Installation,
    executable: &Path,
    cli: &Path,
    no_takeover: bool,
    upgrade: bool,
) -> io::Result<Rerun> {
    if record.phase == UPGRADING && !upgrade {
        return Err(io::Error::other(
            "setup.upgrade_interrupted: re-run the installer with --upgrade (-Upgrade on Windows) to finish switching this host's helpers",
        ));
    }
    if record.phase != "configured" && record.phase != UPGRADING {
        return Err(refused(
            "setup.incomplete_previous_operation_reconcile_explicitly_no_retry",
        ));
    }
    Ok(if !upgrade {
        Rerun::Keep(upgrade_hint(record, executable))
    } else if record.phase == UPGRADING
        || record.executable != executable
        || record.cli != cli
        || no_takeover && !record.no_takeover
    {
        Rerun::Upgrade
    } else {
        Rerun::AlreadyCurrent
    })
}

/// Journals the switch to new helpers. Only `executable`/`cli` (and an
/// explicit `--no-takeover`) change; tunnel, target generation, host identity
/// and account stay. Resuming accumulates every definition still possible.
fn upgrade_record(
    record: &Installation,
    executable: PathBuf,
    cli: PathBuf,
    no_takeover: bool,
) -> io::Result<Installation> {
    let mut next = record.clone();
    if !next.upgrading_from.contains(&record.executable) {
        next.upgrading_from.push(record.executable.clone());
    }
    if next.upgrading_from.len() > MAX_UPGRADE_SOURCES {
        return Err(refused(
            "setup.upgrade_interrupted_too_often_reconcile_explicitly",
        ));
    }
    next.executable = executable;
    next.cli = cli;
    next.no_takeover = record.no_takeover || no_takeover;
    next.phase = UPGRADING.into();
    validate_record(&next)?;
    Ok(next)
}

fn upgrade_completed(record: &Installation) -> Installation {
    let mut next = record.clone();
    next.upgrading_from.clear();
    next.phase = "configured".into();
    next
}

async fn wait_worker_stopped(directory: &File) -> io::Result<()> {
    let worker = lock(directory, "worker.lock")?;
    timeout(Duration::from_secs(30), async {
        loop {
            match worker.try_lock_exclusive() {
                Ok(()) => return FileExt::unlock(&worker),
                Err(error) if festerm_sessiond::lock_is_contended(&error) => {}
                Err(error) => return Err(error),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| refused("setup.previous_worker_still_running"))?
}

async fn upgrade_installation(
    directory: &File,
    state: &Path,
    record: &Installation,
    executable: PathBuf,
    cli: PathBuf,
    no_takeover: bool,
    revoking_recovery: bool,
) -> io::Result<Installation> {
    let target = record
        .target
        .as_ref()
        .ok_or_else(|| refused("setup.target_missing"))?;
    if !revoking_recovery {
        exact_target_alive(&record.runtime_root, target)?;
        if verify_account(&cli).await? != record.account {
            return Err(refused("setup.changed_cli_account"));
        }
    }
    let next = upgrade_record(record, executable, cli, no_takeover)?;
    checkpoint(directory, &next)?;
    println!(
        "Upgrading host helpers from {} to {}; keeping tunnel {}, host key {} and recorded daemon session {} (pid {}). Only owned hosting workers are deliberately restarted.",
        release_label(next.upgrading_from.last().unwrap_or(&record.executable)),
        release_label(&next.executable),
        next.tunnel.as_deref().unwrap_or("-"),
        next.fingerprint,
        target.name,
        target.pid,
    );
    let previous: Vec<_> = next
        .upgrading_from
        .iter()
        .map(|executable| super::service_install::ServiceSpec {
            id: next.id.clone(),
            executable: executable.clone(),
            state: state.into(),
        })
        .collect();
    super::service_install::stop_and_replace(&previous, &service(&next, state))?;
    wait_worker_stopped(directory).await?;
    let next = upgrade_completed(&next);
    validate_record(&next)?;
    checkpoint(directory, &next)?;
    Ok(next)
}

/// Refusals a restart cannot fix; the operator must rerun setup. Everything
/// else (relay drops, bridge crashes, token/network failures) is retried.
const PERMANENT_REFUSALS: &[&str] = &[
    "service.user_context",
    "setup.incomplete_installation",
    "setup.target_missing",
    "setup.target_identity",
    "setup.daemon_identity",
    "setup.exact_daemon_gone_no_replacement_created",
    "setup.changed_cli_account",
    "setup.changed_cli_account_after_token",
    "setup.changed_host_identity",
    "setup.tunnel_missing",
    "setup.tunnel_identity",
    "setup.worker_state",
    "setup.worker_cleanup",
    "setup.bridge_cleanup",
    "setup.daemon_exit_unknown",
    "setup.daemon_stopped_intentionally",
    "setup.daemon_restart_clock_changed",
    "setup.daemon_restart_limit",
    "setup.daemon_restart_interrupted",
    "setup.daemon_restart_identity",
    "setup.daemon_restart_state",
];

async fn recover_daemon<F, R>(
    directory: &File,
    state: &Path,
    record: &mut Installation,
    log: &WorkerLog,
    launch: F,
) -> io::Result<()>
where
    F: FnOnce(Command) -> R,
    R: std::future::Future<Output = io::Result<()>>,
{
    let old = record
        .target
        .clone()
        .ok_or_else(|| refused("setup.target_missing"))?;
    let pending = record
        .recovery
        .as_ref()
        .ok_or_else(|| refused("setup.exact_daemon_gone_no_replacement_created"))?
        .pending_generation;
    let generation = if let Some(generation) = pending {
        generation
    } else {
        let history =
            festerm_sessiond::lifecycle::history(&record.runtime_root, &native_target(&old))
                .map_err(|_| refused("setup.daemon_exit_unknown"))?;
        if history.is_empty() {
            return Err(refused("setup.daemon_exit_unknown"));
        }
        if !festerm_sessiond::lifecycle::unexpected_exit(&history) {
            return Err(refused("setup.daemon_stopped_intentionally"));
        }
        let now = festerm_sessiond::lifecycle::now_ms();
        let generation = now.max(
            old.generation
                .checked_add(1)
                .ok_or_else(|| refused("setup.daemon_restart_identity"))?,
        );
        record
            .recovery
            .as_mut()
            .expect("checked policy")
            .reserve(now, generation)?;
        checkpoint(directory, record).map_err(|_| refused("setup.daemon_restart_state"))?;
        log.event(
            "worker.daemon_restart_start",
            &[("generation", &generation.to_string())],
        );
        let daemon = record
            .executable
            .parent()
            .ok_or_else(|| refused("setup.helpers"))?
            .join(format!("festerm-sessiond{}", std::env::consts::EXE_SUFFIX));
        let (variable, base) = daemon_namespace(&record.runtime_root)?;
        let mut start = super::background_command(&daemon);
        start
            .env(variable, base)
            .env("FESTERM_SESSIOND_GENERATION", generation.to_string())
            .args([
                "start",
                "--name",
                &old.name,
                "--cols",
                "80",
                "--rows",
                "24",
                "--scrollback-limit-bytes",
                "1048576",
            ])
            .current_dir(state)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        super::shell_command(&mut start);
        if let Some(home) = super::shell_home() {
            start.arg("--cwd").arg(home);
        }
        launch(start).await?;
        generation
    };
    // A worker killed after launching but before publication may adopt only its
    // journaled generation. Never create again or adopt an unrelated live shell.
    let selected = festerm_sessiond::list_unattached_sessions_in(&record.runtime_root)
        .map_err(|_| refused("setup.daemon_restart_identity"))?
        .into_iter()
        .find(|session| session.name == old.name && session.created_at_unix_ms == generation)
        .ok_or_else(|| refused("setup.daemon_restart_interrupted"))?;
    record.target = Some(Target {
        name: selected.name,
        pid: selected.pid,
        generation,
        endpoint: selected.endpoint,
    });
    record
        .recovery
        .as_mut()
        .expect("checked policy")
        .pending_generation = None;
    validate_record(record)?;
    checkpoint(directory, record).map_err(|_| refused("setup.daemon_restart_state"))?;
    log.event(
        "worker.daemon_restarted",
        &[
            ("old_generation", &old.generation.to_string()),
            ("generation", &generation.to_string()),
            ("pid", &selected.pid.to_string()),
        ],
    );
    Ok(())
}

async fn launch_replacement(mut start: Command, log: &WorkerLog) -> io::Result<()> {
    let status = timeout(Duration::from_secs(10), start.status())
        .await
        .map_err(|_| refused("setup.daemon_restart_interrupted"))?
        .map_err(|_| refused("setup.daemon_restart_interrupted"))?;
    log.event(
        "worker.daemon_start_exit",
        &[("status", &exit_status(&status))],
    );
    if !status.success() {
        return Err(refused("setup.daemon_restart_interrupted"));
    }
    Ok(())
}

async fn target_loss(root: &Path, target: &Target) -> io::Error {
    loop {
        if let Err(error) = exact_target_alive(root, target) {
            return error;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub(super) fn is_permanent_refusal(error: &io::Error) -> bool {
    PERMANENT_REFUSALS.contains(&error.to_string().as_str())
}

async fn retry_worker<F, R>(log: &WorkerLog, delay: Duration, mut attempt: F) -> io::Result<()>
where
    F: FnMut() -> R,
    R: std::future::Future<Output = io::Result<()>>,
{
    loop {
        match attempt().await {
            Err(error) if !is_permanent_refusal(&error) => {
                log.event(
                    "worker.retry",
                    &[("reason", worker_log::code(&error.to_string()))],
                );
                tokio::time::sleep(delay).await;
            }
            result => return result,
        }
    }
}

pub(super) async fn serve(args: &[OsString]) -> io::Result<()> {
    super::service_install::ensure_unprivileged_user()?;
    let state = state_path(args)?;
    let log = WorkerLog::new(&state);
    {
        let log = log.clone();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            log.event("worker.panic", &[]);
            previous(info);
        }));
    }
    log.event("worker.start", &[("version", env!("CARGO_PKG_VERSION"))]);
    let supervise = cfg!(windows)
        || setup_storage::private_directory(&state, false)
            .and_then(|directory| read_json::<Installation>(&directory, CONFIG))
            .is_ok_and(|record| record.recovery.is_some());
    let result = if supervise {
        retry_worker(&log, Duration::from_secs(15), || run_worker(&state, &log)).await
    } else {
        run_worker(&state, &log).await
    };
    match &result {
        Ok(()) => log.event("worker.exit", &[("reason", "bridge_stopped_cleanly")]),
        Err(error) => log.event(
            "worker.exit",
            &[
                ("reason", worker_log::code(&error.to_string())),
                (
                    "permanent",
                    if is_permanent_refusal(error) {
                        "yes"
                    } else {
                        "no"
                    },
                ),
            ],
        ),
    }
    result
}

async fn run_worker(state: &Path, log: &WorkerLog) -> io::Result<()> {
    let state = state.to_path_buf();
    let directory = setup_storage::private_directory(&state, false)?;
    let health = lock(&directory, "health.lock")?;
    health
        .try_lock_exclusive()
        .map_err(|_| refused("setup.health_publication_busy"))?;
    let worker = lock(&directory, "worker.lock")?;
    worker
        .try_lock_exclusive()
        .map_err(|_| refused("setup.worker_already_running"))?;
    let mut record: Installation = read_json(&directory, CONFIG)?;
    validate_record(&record)?;
    if record.phase != "configured" {
        return Err(refused("setup.incomplete_installation"));
    }
    if verify_account(&record.cli).await? != record.account {
        return Err(refused("setup.changed_cli_account"));
    }
    let identity = HostIdentity::load(&state.join("host-identity.key"))?;
    if identity.fingerprint()? != record.fingerprint {
        return Err(refused("setup.changed_host_identity"));
    }
    let qualified = record
        .tunnel
        .clone()
        .ok_or_else(|| refused("setup.tunnel_missing"))?;
    let (tunnel, cluster) = qualified
        .rsplit_once('.')
        .ok_or_else(|| refused("setup.tunnel_identity"))?;
    let token_bytes = cli(
        &record.cli,
        &["token", qualified.as_str(), "--scopes", "host", "--json"],
        "setup.host_token",
    )
    .await?;
    let token = decode_token(&token_bytes)?;
    drop(token_bytes);
    if verify_account(&record.cli).await? != record.account {
        return Err(refused("setup.changed_cli_account_after_token"));
    }
    let target = record
        .target
        .as_ref()
        .ok_or_else(|| refused("setup.target_missing"))?;
    match exact_target_alive(&record.runtime_root, target) {
        Ok(())
            if record
                .recovery
                .as_ref()
                .is_some_and(|policy| policy.pending_generation.is_some()) =>
        {
            return Err(refused("setup.daemon_restart_identity"));
        }
        Ok(()) => {}
        Err(error) if error.to_string() == "setup.exact_daemon_gone_no_replacement_created" => {
            // Withdraw stale readiness before replacement, while holding health.
            match fs::remove_file(state.join(READY)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => return Err(refused("setup.worker_cleanup")),
            }
            recover_daemon(&directory, &state, &mut record, log, |start| {
                launch_replacement(start, log)
            })
            .await?;
        }
        Err(error) => return Err(error),
    }
    let target = record
        .target
        .as_ref()
        .ok_or_else(|| refused("setup.target_missing"))?;
    let bridge = record
        .executable
        .parent()
        .ok_or_else(|| refused("setup.helpers"))?
        .join(format!(
            "festerm-tunnel-bridge{}",
            std::env::consts::EXE_SUFFIX
        ));
    let (variable, base) = daemon_namespace(&record.runtime_root)?;
    let run = state.join(format!("run-{}", uuid::Uuid::new_v4()));
    let _run_directory =
        setup_storage::private_directory(&run, true).map_err(|_| refused("setup.worker_state"))?;
    let destination = run.join("destination.json");
    let spawned = super::background_command(&bridge)
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
        .args(if record.no_takeover {
            None
        } else {
            Some("--takeover")
        })
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(_) => {
            cleanup_worker_run(&state, &run)?;
            return Err(refused("setup.start_bridge"));
        }
    };
    let mut published = false;
    let result = async {
        let events = child.stderr.take().map(|stderr| {
            let log = log.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                // Drain to EOF regardless of content: a stopped reader would make
                // the bridge's next stderr write fail.
                let mut reader = tokio::io::BufReader::new(stderr);
                let mut line = Vec::new();
                while matches!(reader.read_until(b'\n', &mut line).await, Ok(read) if read > 0) {
                    if let Some(text) = std::str::from_utf8(&line)
                        .ok()
                        .and_then(|line| worker_log::bridge_event(line.trim_end()))
                    {
                        log.raw_event(&text);
                    }
                    line.clear();
                }
            })
        });
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
        published = true;
        log.event("worker.bridge_ready", &[]);
        log.event(
            "worker.daemon_alive",
            &[
                ("pid", &target.pid.to_string()),
                ("generation", &target.generation.to_string()),
            ],
        );
        eprintln!(
            "Managed experimental bridge ready; exact generation, no file capability. \
         Reauthentication or a stale daemon requires explicit operator action."
        );
        let status = tokio::select! {
                status = child.wait() => {
                    status.map_err(|_| refused("setup.bridge_wait"))?
                }
                error = target_loss(&record.runtime_root, target) => {
                        log.event("worker.daemon_lost", &[
                            ("pid", &target.pid.to_string()),
                            ("generation", &target.generation.to_string()),
                            ("reason", worker_log::code(&error.to_string())),
                        ]);
                        return Err(error);
                }
        };
        if let Some(events) = events {
            let _ = timeout(Duration::from_secs(2), events).await;
        }
        log.event("worker.bridge_exit", &[("status", &exit_status(&status))]);
        if !status.success() {
            return Err(refused("setup.bridge_stopped"));
        }
        Ok(())
    }
    .await;
    if child
        .try_wait()
        .map_err(|_| refused("setup.bridge_cleanup"))?
        .is_none()
    {
        child
            .kill()
            .await
            .map_err(|_| refused("setup.bridge_cleanup"))?;
    }
    if published && !reacquire_health(&health).await? {
        // A setup rerun is still reading the published record; leave ready.json
        // (it is ignored without a live worker) and let the next attempt retry.
        cleanup_worker_destination(&run)?;
        return Err(refused("setup.health_publication_busy"));
    }
    cleanup_worker_run(&state, &run)?;
    result.map_err(|error| {
        if error.to_string() == "setup.exact_daemon_gone_no_replacement_created"
            && record.recovery.is_some()
        {
            refused("setup.daemon_lost")
        } else {
            error
        }
    })
}

/// Readers (`healthy_record`) hold the shared health lock only briefly, so
/// contention after the bridge exits is transient rather than a refusal.
async fn reacquire_health(health: &File) -> io::Result<bool> {
    for _ in 0..50 {
        match health.try_lock_exclusive() {
            Ok(()) => return Ok(true),
            Err(error) if festerm_sessiond::lock_is_contended(&error) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(_) => return Err(refused("setup.worker_cleanup")),
        }
    }
    Ok(false)
}

fn cleanup_worker_destination(run: &Path) -> io::Result<()> {
    match fs::remove_file(run.join("destination.json")) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(refused("setup.worker_cleanup")),
    }
    fs::remove_dir(run).map_err(|_| refused("setup.worker_cleanup"))
}

fn cleanup_worker_run(state: &Path, run: &Path) -> io::Result<()> {
    cleanup_worker_destination(run)?;
    match fs::remove_file(state.join(READY)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(refused("setup.worker_cleanup")),
    }
}

fn exit_status(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(status) {
        return format!("signal-{signal}");
    }
    status
        .code()
        .map_or_else(|| "unknown".into(), |code| code.to_string())
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

    #[tokio::test]
    async fn managed_worker_waits_out_a_setup_rerun_reading_health_instead_of_refusing() {
        let state = crate::test_support::private_directory("festerm-worker-health-contention");
        let path = state.join("health.lock");
        let worker = File::create(&path).unwrap();
        let reader = File::open(&path).unwrap();
        FileExt::lock_shared(&reader).unwrap();
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            FileExt::unlock(&reader).unwrap();
        });
        assert!(reacquire_health(&worker).await.unwrap());
        release.await.unwrap();
        FileExt::unlock(&worker).unwrap();

        let reader = File::open(&path).unwrap();
        FileExt::lock_shared(&reader).unwrap();
        assert!(!reacquire_health(&worker).await.unwrap());
        assert!(!is_permanent_refusal(&refused(
            "setup.health_publication_busy"
        )));
        let run = state.join("run-test");
        fs::create_dir(&run).unwrap();
        fs::write(run.join("destination.json"), b"{}").unwrap();
        cleanup_worker_destination(&run).unwrap();
        assert!(!run.exists());
        drop(reader);
        drop(worker);
        fs::remove_dir_all(state).unwrap();
    }

    #[tokio::test]
    async fn managed_windows_worker_retries_until_recovery_and_stops_on_refusal() {
        let state = crate::test_support::private_directory("festerm-worker-retry");
        let log = WorkerLog::new(&state);
        let mut outcomes = [
            Err(refused("setup.bridge_stopped")),
            Err(refused("setup.host_token")),
            Ok(()),
        ]
        .into_iter();
        retry_worker(&log, Duration::ZERO, || {
            std::future::ready(outcomes.next().expect("clean shutdown must stop retries"))
        })
        .await
        .unwrap();
        assert!(outcomes.next().is_none());
        for stage in PERMANENT_REFUSALS {
            let mut attempts = 0;
            let error = retry_worker(&log, Duration::ZERO, || {
                attempts += 1;
                assert_eq!(attempts, 1, "operator refusals must not retry");
                std::future::ready(Err(refused(stage)))
            })
            .await
            .unwrap_err();
            assert_eq!(error.to_string(), *stage);
        }
        let mut outcomes = [
            Err(refused("setup.bridge_exited_before_ready")),
            Err(refused("setup.changed_host_identity")),
        ]
        .into_iter();
        let error = retry_worker(&log, Duration::ZERO, || {
            std::future::ready(outcomes.next().expect("a new refusal must stop retries"))
        })
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "setup.changed_host_identity");
        assert!(outcomes.next().is_none());
        let text = fs::read_to_string(state.join("worker.log")).unwrap();
        assert_eq!(text.matches("worker.retry").count(), 3);
        fs::remove_dir_all(state).unwrap();
    }

    #[tokio::test]
    async fn managed_worker_retry_waits_and_can_be_cancelled_before_another_attempt() {
        let state = crate::test_support::private_directory("festerm-worker-retry-wait");
        let log = WorkerLog::new(&state);
        let mut attempts = 0;
        let result = timeout(
            Duration::from_millis(20),
            retry_worker(&log, Duration::from_secs(60), || {
                attempts += 1;
                std::future::ready(Err(refused("setup.bridge_stopped")))
            }),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            attempts, 1,
            "do not retry immediately or after cancellation"
        );
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn managed_worker_cleans_ready_and_pre_ready_runs_without_touching_installation() {
        let state = crate::test_support::private_directory("festerm-worker-cleanup");
        fs::write(state.join(CONFIG), b"unchanged installation").unwrap();
        for published in [false, true] {
            let run = state.join("run-fixture");
            fs::create_dir(&run).unwrap();
            if published {
                fs::write(run.join("destination.json"), b"destination").unwrap();
                fs::write(state.join(READY), b"ready").unwrap();
            }
            cleanup_worker_run(&state, &run).unwrap();
            assert!(!run.exists());
            assert!(!state.join(READY).exists());
            assert_eq!(
                fs::read(state.join(CONFIG)).unwrap(),
                b"unchanged installation"
            );
        }
        let run = state.join("run-refuse-unknown");
        fs::create_dir(&run).unwrap();
        fs::write(run.join("foreign"), b"do not recursively delete").unwrap();
        let error = cleanup_worker_run(&state, &run).unwrap_err();
        assert!(is_permanent_refusal(&error));
        assert!(run.join("foreign").exists());
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn managed_worker_retries_transient_failures_but_not_operator_refusals() {
        for stage in [
            "setup.exact_daemon_gone_no_replacement_created",
            "setup.changed_cli_account",
            "setup.changed_host_identity",
        ] {
            assert!(is_permanent_refusal(&refused(stage)), "{stage}");
        }
        for stage in [
            "host.outer_task_closed",
            "setup.bridge_stopped",
            "setup.bridge_exited_before_ready",
            "setup.host_token",
            "setup.verify_account",
        ] {
            assert!(!is_permanent_refusal(&refused(stage)), "{stage}");
        }
    }
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
            no_takeover: false,
            recovery: None,
            upgrading_from: Vec::new(),
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
            no_takeover: false,
            recovery: None,
            upgrading_from: Vec::new(),
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
        assert!(healthy_record(&directory, &record).unwrap().is_none());
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

    fn configured_fixture(executable: PathBuf) -> Installation {
        let id = uuid::Uuid::new_v4();
        Installation {
            version: 1,
            id: id.to_string(),
            executable,
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
            fingerprint: "fixture-fingerprint".into(),
            no_takeover: true,
            recovery: None,
            upgrading_from: Vec::new(),
        }
    }

    #[test]
    fn managed_daemon_restart_budget_survives_reload_and_refuses_clock_rollback() {
        let base: Vec<OsString> = ["--devtunnel", "cli", "--state-dir", "state"]
            .into_iter()
            .map(OsString::from)
            .collect();
        for enabled in [true, false] {
            let mut args = base.clone();
            args.push(
                if enabled {
                    "--restart-unexpected"
                } else {
                    "--no-restart-unexpected"
                }
                .into(),
            );
            assert_eq!(
                managed_arguments(&args).unwrap().restart_unexpected,
                Some(enabled)
            );
            args.push("--restart-unexpected".into());
            assert!(managed_arguments(&args).is_err());
        }
        let mut policy = DaemonRecovery::default();
        for generation in 1..=3 {
            policy.reserve(1_000 + generation, generation).unwrap();
            policy = serde_json::from_slice(&serde_json::to_vec(&policy).unwrap()).unwrap();
        }
        assert_eq!(
            policy.reserve(1_004, 4).unwrap_err().to_string(),
            "setup.daemon_restart_limit"
        );
        assert_eq!(
            policy.reserve(999, 4).unwrap_err().to_string(),
            "setup.daemon_restart_clock_changed"
        );
        policy.reserve(601_004, 4).unwrap();
        assert_eq!(policy.attempts, [601_004]);
    }

    #[tokio::test]
    async fn managed_daemon_recovery_replaces_only_the_authorized_generation() {
        use festerm_sessiond::lifecycle::{self, Event};
        let state = crate::test_support::private_directory("festerm-daemon-restart");
        let directory = setup_storage::private_directory(&state, false).unwrap();
        let application_path = state.join(if cfg!(windows) { "fesTerm" } else { "festerm" });
        let application = setup_storage::private_directory(&application_path, true).unwrap();
        let runtime_path = application_path.join("sessiond");
        let runtime = setup_storage::private_directory(&runtime_path, true).unwrap();
        let mut record = configured_fixture(state.join(format!(
            "festerm-tunnel-setup{}",
            std::env::consts::EXE_SUFFIX
        )));
        record.runtime_root = runtime_path.clone();
        record.recovery = Some(DaemonRecovery::default());
        let old = record.target.clone().unwrap();
        lifecycle::record(&runtime_path, &native_target(&old), Event::Ready).unwrap();
        let before = serde_json::to_value(&record).unwrap();
        let log = WorkerLog::new(&state);
        let mut leases = Vec::new();
        let mut generation = 0;
        recover_daemon(&directory, &state, &mut record, &log, |start| {
            let command = start.as_std();
            generation = command
                .get_envs()
                .find(|(key, _)| *key == "FESTERM_SESSIOND_GENERATION")
                .unwrap()
                .1
                .unwrap()
                .to_str()
                .unwrap()
                .parse::<u128>()
                .unwrap();
            assert_eq!(command.get_args().next().unwrap(), "start");
            let saved: Installation = read_json(&directory, CONFIG).unwrap();
            assert_eq!(saved.recovery.unwrap().pending_generation, Some(generation));
            let pid = std::process::id();
            let lease = setup_storage::open_private_file(
                &runtime,
                Path::new(&format!("lease-{pid}-{generation}")),
                true,
            )
            .unwrap();
            lease.try_lock_exclusive().unwrap();
            leases.push(lease);
            fs::write(
                runtime_path.join("registry.json"),
                serde_json::to_vec(&serde_json::json!({
                    "sessions": { (old.name.clone()): {
                        "name": old.name, "pid": pid, "created_at_unix_ms": generation,
                        "socket": format!("fixture-{pid}-{generation}.sock"),
                        "protocol_version": 3, "snapshot_schema_version": 3
                    }}
                }))
                .unwrap(),
            )
            .unwrap();
            std::future::ready(Ok(()))
        })
        .await
        .unwrap();
        assert_eq!(record.target.as_ref().unwrap().generation, generation);
        let saved: Installation = read_json(&directory, CONFIG).unwrap();
        assert_eq!(saved.target.unwrap().generation, generation);
        let after = serde_json::to_value(&record).unwrap();
        for key in [
            "id",
            "account",
            "tunnel",
            "fingerprint",
            "cli",
            "executable",
            "no_takeover",
        ] {
            assert_eq!(before[key], after[key], "{key} must not change");
        }
        assert!(record
            .recovery
            .as_ref()
            .unwrap()
            .pending_generation
            .is_none());

        // Simulate interruption after spawning, before checkpointing its target.
        record.target = Some(old.clone());
        record.recovery.as_mut().unwrap().pending_generation = Some(generation);
        recover_daemon(&directory, &state, &mut record, &log, |_| async {
            panic!("must reconcile, not launch again")
        })
        .await
        .unwrap();
        assert_eq!(record.target.as_ref().unwrap().generation, generation);
        record.target = Some(old.clone());
        record.recovery.as_mut().unwrap().pending_generation = Some(generation + 1);
        assert!(
            recover_daemon(&directory, &state, &mut record, &log, |_| async {
                panic!("must not adopt another generation")
            })
            .await
            .is_err()
        );
        assert_eq!(record.target.as_ref().unwrap().generation, old.generation);

        for (index, event) in [
            None,
            Some(Event::ShellExited { code: 0 }),
            Some(Event::ShellExited { code: 1 }),
            Some(Event::StopRequested),
            Some(Event::Shutdown { success: true }),
        ]
        .into_iter()
        .enumerate()
        {
            let target = record.target.as_mut().unwrap();
            target.generation = 100 + index as u128;
            record.recovery = Some(DaemonRecovery::default());
            if let Some(event) = event {
                lifecycle::record(
                    &runtime_path,
                    &native_target(record.target.as_ref().unwrap()),
                    event,
                )
                .unwrap();
            }
            let error = recover_daemon(&directory, &state, &mut record, &log, |_| async {
                panic!("unknown or deliberate exits never launch a replacement")
            })
            .await
            .unwrap_err();
            assert!(is_permanent_refusal(&error));
        }
        // The real watchdog watches the lease, not merely a still-live reused PID.
        let live = Target {
            name: old.name,
            pid: std::process::id(),
            generation,
            endpoint: format!("fixture-{}-{generation}.sock", std::process::id()),
        };
        assert!(
            timeout(Duration::from_millis(10), target_loss(&runtime_path, &live))
                .await
                .is_err()
        );
        drop(leases);
        let error = timeout(Duration::from_secs(2), target_loss(&runtime_path, &live))
            .await
            .unwrap();
        assert_eq!(
            error.to_string(),
            "setup.exact_daemon_gone_no_replacement_created"
        );
        drop(runtime);
        drop(application);
        drop(directory);
        fs::remove_dir_all(state).unwrap();
    }

    fn old_executable() -> PathBuf {
        std::env::current_dir()
            .unwrap()
            .join("releases/0.0.1-fixture/festerm-tunnel-setup")
    }

    #[test]
    fn managed_upgrade_switches_helpers_and_keeps_tunnel_target_identity_and_takeover() {
        let current = std::env::current_exe().unwrap();
        let new_cli = current.with_file_name("devtunnel-new");
        let record = configured_fixture(old_executable());
        let upgrading = upgrade_record(&record, current.clone(), new_cli.clone(), false).unwrap();
        assert_eq!(upgrading.phase, UPGRADING);
        assert_eq!(upgrading.upgrading_from, [old_executable()]);
        let done = upgrade_completed(&upgrading);
        validate_record(&done).unwrap();
        assert_eq!(
            (done.executable.clone(), done.cli.clone()),
            (current, new_cli)
        );
        assert_eq!(done.phase, "configured");
        assert!(done.upgrading_from.is_empty());
        assert_eq!(
            (&done.id, &done.tunnel, &done.account, &done.runtime_root),
            (
                &record.id,
                &record.tunnel,
                &record.account,
                &record.runtime_root
            )
        );
        assert_eq!(done.fingerprint, record.fingerprint);
        let (old, new) = (record.target.unwrap(), done.target.clone().unwrap());
        assert_eq!(
            (old.name, old.pid, old.generation, old.endpoint),
            (new.name, new.pid, new.generation, new.endpoint)
        );
        // A recorded no-takeover stays on; an explicit flag turns it on.
        assert!(done.no_takeover);
        let mut takeover = configured_fixture(old_executable());
        takeover.no_takeover = false;
        let exe = std::env::current_exe().unwrap();
        assert!(
            !upgrade_record(&takeover, exe.clone(), exe.clone(), false)
                .unwrap()
                .no_takeover
        );
        assert!(
            upgrade_record(&takeover, exe.clone(), exe, true)
                .unwrap()
                .no_takeover
        );
        // A completed record serializes exactly like one older helpers wrote.
        let json = serde_json::to_value(&done).unwrap();
        assert!(json.get("upgrading_from").is_none());
    }

    #[test]
    fn managed_rerun_with_other_helpers_hints_upgrade_and_keeps_the_record() {
        let current = std::env::current_exe().unwrap();
        let record = configured_fixture(old_executable());
        let Rerun::Keep(Some(hint)) =
            rerun_plan(&record, &current, &record.cli, false, false).unwrap()
        else {
            panic!("plain rerun must keep the record and hint");
        };
        assert!(
            hint.contains("--upgrade") && hint.contains("-Upgrade"),
            "{hint}"
        );
        assert!(
            hint.contains("keeping the tunnel, host key and session"),
            "{hint}"
        );
        let same = configured_fixture(current.clone());
        assert_eq!(
            rerun_plan(&same, &current, &same.cli, false, false).unwrap(),
            Rerun::Keep(None)
        );
        assert_eq!(
            rerun_plan(&same, &current, &same.cli, false, true).unwrap(),
            Rerun::AlreadyCurrent
        );
        assert_eq!(
            rerun_plan(&record, &current, &record.cli, false, true).unwrap(),
            Rerun::Upgrade
        );
        let mut partial = configured_fixture(current.clone());
        partial.phase = "starting-session".into();
        assert!(rerun_plan(&partial, &current, &partial.cli, false, true).is_err());
    }

    #[test]
    fn managed_release_label_reads_the_installed_version() {
        let root = crate::test_support::private_directory("festerm-managed-label");
        let executable = root.join("festerm-tunnel-setup");
        assert_eq!(release_label(&executable), root.display().to_string());
        fs::write(root.join("VERSION"), "0.14.0\n").unwrap();
        assert_eq!(release_label(&executable), "0.14.0");
        fs::write(root.join("VERSION"), "not a version").unwrap();
        assert_eq!(release_label(&executable), root.display().to_string());
        fs::remove_file(root.join("VERSION")).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn managed_interrupted_upgrade_refuses_plain_rerun_and_resumes_with_upgrade() {
        let current = std::env::current_exe().unwrap();
        let interrupted_target = current.with_file_name("other-release-setup");
        let record = configured_fixture(old_executable());
        let upgrading = upgrade_record(
            &record,
            interrupted_target.clone(),
            record.cli.clone(),
            false,
        )
        .unwrap();
        let encoded = serde_json::to_vec(&upgrading).unwrap();
        let decoded: Installation = serde_json::from_slice(&encoded).unwrap();
        validate_record(&decoded).unwrap();
        let refusal = rerun_plan(&decoded, &current, &decoded.cli, false, false).unwrap_err();
        assert!(refusal.to_string().contains("--upgrade"), "{refusal}");
        assert_eq!(
            rerun_plan(&decoded, &interrupted_target, &decoded.cli, false, true).unwrap(),
            Rerun::Upgrade
        );
        // Resuming keeps every definition the service might still hold.
        let resumed =
            upgrade_record(&decoded, current.clone(), decoded.cli.clone(), false).unwrap();
        assert_eq!(
            resumed.upgrading_from,
            [old_executable(), interrupted_target]
        );
        assert_eq!(upgrade_completed(&resumed).executable, current);
        // The journal is consistent: sources exist exactly while upgrading.
        let mut inconsistent = decoded.clone();
        inconsistent.phase = "configured".into();
        assert!(validate_record(&inconsistent).is_err());
        let mut inconsistent = decoded;
        inconsistent.upgrading_from.clear();
        assert!(validate_record(&inconsistent).is_err());
    }

    #[test]
    fn managed_upgrade_arguments_and_missing_installation_refuse() {
        let parse = |extra: &[&str]| {
            let mut args: Vec<OsString> = ["--devtunnel", "/cli", "--state-dir", "/state"]
                .into_iter()
                .map(OsString::from)
                .collect();
            args.extend(extra.iter().map(OsString::from));
            managed_arguments(&args)
                .map(|parsed| (parsed.no_takeover, parsed.upgrade, parsed.friendly_name))
        };
        assert_eq!(parse(&[]).unwrap(), (false, false, None));
        assert_eq!(parse(&["--upgrade"]).unwrap(), (false, true, None));
        assert_eq!(
            parse(&["--friendly-name", "Raspberry Pi 5"]).unwrap(),
            (false, false, Some("Raspberry Pi 5".into()))
        );
        assert_eq!(
            parse(&["--upgrade", "--no-takeover"]).unwrap(),
            (true, true, None)
        );
        assert_eq!(
            parse(&["--no-takeover", "--upgrade"]).unwrap(),
            (true, true, None)
        );
        assert!(parse(&["--friendly-name", " "]).is_err());
        assert!(parse(&["--friendly-name", "first", "--friendly-name", "second"]).is_err());
        assert!(parse(&["--friendly-name", "line\nbreak"]).is_err());
        assert!(parse(&["--upgrade", "--upgrade"]).is_err());
        assert!(parse(&["--force"]).is_err());
        let missing = std::env::current_dir()
            .unwrap()
            .join(format!("missing-managed-state-{}", uuid::Uuid::new_v4()));
        let args: Vec<OsString> = vec![
            "--devtunnel".into(),
            std::env::current_exe().unwrap().into(),
            "--state-dir".into(),
            missing.clone().into(),
            "--upgrade".into(),
        ];
        let error = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(install(&args))
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "setup.upgrade_requires_an_existing_installation"
        );
        assert!(!missing.exists());
    }
}
