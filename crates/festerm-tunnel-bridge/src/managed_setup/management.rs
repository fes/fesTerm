//! Explicit management of the installer's one protected script installation.
//! Desktop grants deliberately remain behind the profile consent authority.
use super::*;
use festerm_tunnel_bridge::cli_pins;

const OWNERSHIP: &str = "script-ownership.json";
const DELETION: &str = "deletion.json";
const LEGACY: &str = "legacy-deletion.json";
const USAGE: &str = "usage: --manage {list | delete INSTALLATION_ID --yes [--confirm-legacy-cloud-id QUALIFIED_ID] | delete-all --yes | archive INSTALLATION_ID --yes} [--json]";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    version: u16,
    installation: String,
    fingerprint: String,
    label: String,
    qualified: Option<String>,
    incarnation: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Deletion {
    version: u16,
    installation: String,
    fingerprint: String,
    stopped: bool,
    #[serde(default)]
    cloud_gone: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Legacy {
    version: u16,
    provenance: String,
    installation: String,
    fingerprint: String,
    account: String,
    qualified: String,
    incarnation: Option<String>,
    incarnation_available: bool,
    cloud_gone: bool,
    #[serde(skip)]
    journal_new: bool,
}

#[derive(Clone, Serialize)]
struct Row {
    installation: String,
    origin: &'static str,
    host: String,
    account: String,
    tunnel: Option<String>,
    service: String,
    phase: String,
    cleanup: &'static str,
    failure: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    legacy_incarnation_available: Option<bool>,
}

#[derive(Debug, PartialEq, Eq)]
enum Operation {
    List,
    Delete(String),
    DeleteAll,
    Archive(String),
}

fn arguments(args: &[OsString]) -> io::Result<(Operation, bool, Option<String>)> {
    let mut json = false;
    let mut yes = false;
    let mut positional = Vec::new();
    let mut legacy = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--json") if !json => json = true,
            Some("--yes") if !yes => yes = true,
            Some("--confirm-legacy-cloud-id") if legacy.is_none() => {
                let value = args
                    .next()
                    .and_then(|value| value.to_str())
                    .filter(|value| {
                        !value.is_empty()
                            && value.len() <= 256
                            && value
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || b"-.".contains(&byte))
                    })
                    .ok_or_else(|| refused(USAGE))?;
                legacy = Some(value.to_owned());
            }
            Some(value) if !value.starts_with('-') => positional.push(value),
            _ => return Err(refused(USAGE)),
        }
    }
    let operation = match positional.as_slice() {
        ["list"] if !yes => Operation::List,
        ["delete", id] if yes && valid_id(id) => Operation::Delete((*id).into()),
        ["delete-all"] if yes => Operation::DeleteAll,
        ["archive", id] if yes && valid_id(id) => Operation::Archive((*id).into()),
        _ => return Err(refused(USAGE)),
    };
    if legacy.is_some() && !matches!(operation, Operation::Delete(_)) {
        return Err(refused(USAGE));
    }
    Ok((operation, json, legacy))
}

fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id)
        .is_ok_and(|parsed| parsed.get_version_num() == 4 && parsed.to_string() == id)
}

fn namespace() -> io::Result<PathBuf> {
    setup_storage::script_namespace()
}

fn namespace_lock(root: &Path) -> io::Result<File> {
    let parent = setup_storage::private_directory(root, false)?;
    let installer = lock(&parent, "installer.lock")?;
    installer
        .try_lock_exclusive()
        .map_err(|_| refused("management.operation_busy"))?;
    Ok(installer)
}

pub(super) fn setup_namespace_lock(state: &Path) -> io::Result<Option<File>> {
    let Ok(root) = namespace() else {
        return Ok(None);
    };
    if state == root.join("state") {
        namespace_lock(&root).map(Some)
    } else {
        Ok(None)
    }
}

fn write<T: Serialize>(directory: &File, name: &str, value: &T) -> io::Result<()> {
    setup_storage::replace_private(
        directory,
        Path::new(name),
        &serde_json::to_vec_pretty(value).map_err(|_| refused("management.journal_encode"))?,
    )
}

pub(super) fn reserve_creation(directory: &File, record: &Installation) -> io::Result<String> {
    let ownership = Ownership {
        version: 1,
        installation: record.id.clone(),
        fingerprint: record.fingerprint.clone(),
        label: format!("festerm-owned-{}", uuid::Uuid::new_v4()),
        qualified: None,
        incarnation: None,
    };
    setup_storage::write_new_private(
        directory,
        Path::new(OWNERSHIP),
        &serde_json::to_vec_pretty(&ownership).map_err(|_| refused("management.journal_encode"))?,
    )?;
    Ok(ownership.label)
}

pub(super) fn record_creation(
    directory: &File,
    record: &Installation,
    response: &[u8],
) -> io::Result<()> {
    let mut ownership = ownership(directory, record)?;
    let actual: profile::CloudReply =
        serde_json::from_slice(response).map_err(|_| refused("setup.create_response"))?;
    super::super::split_created_id(&actual.tunnel.tunnel_id, &cloud_name(record)?)?;
    ownership.qualified = Some(actual.tunnel.tunnel_id);
    ownership.incarnation = actual.tunnel.created;
    write(directory, OWNERSHIP, &ownership)
}

fn cloud_name(record: &Installation) -> io::Result<String> {
    let id = uuid::Uuid::parse_str(&record.id)
        .map_err(|_| refused("management.installation_identity"))?;
    Ok(format!("festerm-{}", id.simple()))
}

fn ownership(directory: &File, record: &Installation) -> io::Result<Ownership> {
    let ownership: Ownership = read_json(directory, OWNERSHIP)?;
    if ownership.version != 1
        || ownership.installation != record.id
        || ownership.fingerprint != record.fingerprint
        || !ownership
            .label
            .strip_prefix("festerm-owned-")
            .is_some_and(valid_id)
        || ownership.qualified.as_ref().is_some_and(|qualified| {
            cloud_name(record)
                .and_then(|name| super::super::split_created_id(qualified, &name))
                .is_err()
                || record.tunnel.as_ref().is_some_and(|id| id != qualified)
        })
    {
        return Err(refused("management.cloud_ownership_unavailable"));
    }
    Ok(ownership)
}

fn deletion(directory: &File, record: &Installation) -> io::Result<Option<Deletion>> {
    match read_json::<Deletion>(directory, DELETION) {
        Ok(receipt)
            if receipt.version == 1
                && receipt.installation == record.id
                && receipt.fingerprint == record.fingerprint
                && matches!(record.phase.as_str(), "deleting" | "deleted") =>
        {
            Ok(Some(receipt))
        }
        Ok(_) => Err(refused("management.deletion_identity")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn load(root: &Path) -> io::Result<(File, Installation)> {
    let _root = setup_storage::private_directory(root, false)?;
    let state = root.join("state");
    let directory = setup_storage::private_directory(&state, false)?;
    let record: Installation = read_json(&directory, CONFIG)?;
    validate_record(&record)?;
    let release = record
        .executable
        .parent()
        .ok_or_else(|| refused("management.helper_identity"))?;
    if record.version != 1
        || record.origin.is_some()
        || !valid_id(&record.id)
        || release.parent() != Some(root.join("releases").as_path())
        || !release.file_name().and_then(|name| name.to_str()).is_some_and(|name| {
            let parts = name.split('-').collect::<Vec<_>>();
            matches!(parts.as_slice(), [version, "windows" | "linux" | "macos", "x86_64" | "aarch64"]
                if version.len() <= 32
                    && version.split('.').count() == 3
                    && version.split('.').all(|part| !part.is_empty()
                        && part.bytes().all(|byte| byte.is_ascii_digit())))
        })
        || record.executable.file_name()
            != Some(std::ffi::OsStr::new(&format!(
                "festerm-tunnel-setup{}",
                std::env::consts::EXE_SUFFIX
            )))
        || record.cli != release.join(format!("devtunnel{}", std::env::consts::EXE_SUFFIX))
        || !matches!(
            record.phase.as_str(),
            "creating-tunnel"
                | "creating-port"
                | "starting-session"
                | "configured"
                | UPGRADING
                | "deleting"
                | "deleted"
        )
    {
        return Err(refused("management.not_a_script_installation"));
    }
    let identity = HostIdentity::load(&state.join("host-identity.key"))?;
    if identity.fingerprint()? != record.fingerprint {
        return Err(refused("management.host_identity"));
    }
    let receipt = deletion(&directory, &record)?;
    if record.phase == "deleted"
        && !receipt.is_some_and(|receipt| receipt.stopped && receipt.cloud_gone)
    {
        return Err(refused("management.missing_completion_receipt"));
    }
    Ok((directory, record))
}

fn row(directory: &File, record: &Installation) -> io::Result<Row> {
    let stopped = deletion(directory, record)?.is_some_and(|receipt| receipt.stopped);
    let legacy_incarnation_available = match read_json::<Legacy>(directory, LEGACY) {
        Ok(_) => Some(
            legacy_authority(directory, record, record.tunnel.as_deref())?.incarnation_available,
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    Ok(Row {
        installation: record.id.clone(),
        origin: "script",
        host: cloud_name(record)?,
        account: record.account.clone(),
        tunnel: record.tunnel.clone(),
        service: if stopped {
            "stopped"
        } else {
            "recorded-unverified"
        }
        .into(),
        phase: record.phase.clone(),
        cleanup: match record.phase.as_str() {
            "deleted" if stopped => "complete",
            "deleting" if stopped => "pending",
            "deleting" => "stopping-failed-or-interrupted",
            _ => "not-requested",
        },
        failure: None,
        legacy_incarnation_available,
    })
}

// The script namespace has exactly one fixed state directory, not a directory
// scan, account listing or prefix match. Unknown neighboring entries are inert.
fn inventory(root: &Path) -> io::Result<Vec<Row>> {
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let _root = setup_storage::private_directory(root, false)?;
    match fs::symlink_metadata(root.join("state")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let (directory, record) = load(root)?;
    Ok(vec![row(&directory, &record)?])
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Complete,
    Pending(&'static str),
}

pub(super) fn check_restart_target(
    target: &festerm_tunnel_bridge::profile_hosting::Target,
) -> io::Result<()> {
    let root = namespace()?;
    check_restart_target_in(&root, target)
}

fn check_restart_target_in(
    root: &Path,
    target: &festerm_tunnel_bridge::profile_hosting::Target,
) -> io::Result<()> {
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let _root = setup_storage::private_directory(root, false)?;
    match fs::symlink_metadata(root.join("state")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let (directory, record) = load(root)?;
    if record.runtime_root != target.runtime_root {
        return Ok(());
    }
    if record.target.as_ref().is_none_or(|selected| {
        selected.name == target.name
            && selected.pid == target.pid
            && selected.generation == target.generation
            && selected.endpoint == target.endpoint
    }) && (record.phase != "deleted" || row(&directory, &record)?.cleanup != "complete")
    {
        return Err(refused("profile_host.restart_target_already_hosted"));
    }
    Ok(())
}

fn matches_cloud(
    record: &Installation,
    ownership: &Ownership,
    actual: &profile::CloudSnapshot,
) -> bool {
    cloud_name(record)
        .and_then(|name| super::super::split_created_id(&actual.tunnel_id, &name))
        .is_ok()
        && actual.labels.contains(&ownership.label)
        && ownership
            .qualified
            .as_ref()
            .or(record.tunnel.as_ref())
            .is_none_or(|expected| expected == &actual.tunnel_id)
        && ownership
            .incarnation
            .as_ref()
            .is_none_or(|expected| actual.created.as_ref() == Some(expected))
}

fn legacy_authority(
    directory: &File,
    record: &Installation,
    confirmation: Option<&str>,
) -> io::Result<Legacy> {
    let qualified = record
        .tunnel
        .as_deref()
        .ok_or_else(|| refused("management.legacy_qualified_identity_unavailable"))?;
    if confirmation != Some(qualified) {
        return Err(refused("management.legacy_full_id_confirmation_required"));
    }
    super::super::split_created_id(qualified, &cloud_name(record)?)?;
    match read_json::<Legacy>(directory, LEGACY) {
        Ok(receipt)
            if receipt.version == 1
                && receipt.provenance == "legacy-confirmed"
                && receipt.installation == record.id
                && receipt.fingerprint == record.fingerprint
                && receipt.account == record.account
                && receipt.qualified == qualified
                && receipt.incarnation_available == receipt.incarnation.is_some()
                && receipt.incarnation.as_ref().is_none_or(|value| {
                    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
                }) =>
        {
            Ok(receipt)
        }
        Ok(_) => Err(refused("management.legacy_receipt_identity")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Legacy {
            version: 1,
            provenance: "legacy-confirmed".into(),
            installation: record.id.clone(),
            fingerprint: record.fingerprint.clone(),
            account: record.account.clone(),
            qualified: qualified.into(),
            incarnation: None,
            incarnation_available: false,
            cloud_gone: false,
            journal_new: true,
        }),
        Err(error) => Err(error),
    }
}

fn legacy_matches(
    record: &Installation,
    receipt: &Legacy,
    actual: &profile::CloudSnapshot,
) -> bool {
    !receipt.cloud_gone
        && record.account == receipt.account
        && record.tunnel.as_ref() == Some(&receipt.qualified)
        && actual.tunnel_id == receipt.qualified
        && cloud_name(record)
            .and_then(|name| super::super::split_created_id(&actual.tunnel_id, &name))
            .is_ok()
        && actual.created.as_ref().is_none_or(|value| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        })
        && receipt
            .incarnation
            .as_ref()
            .is_none_or(|expected| actual.created.as_ref() == Some(expected))
}

async fn cleanup_cloud(
    directory: &File,
    record: &Installation,
    confirmation: Option<&str>,
) -> io::Result<Outcome> {
    if !deletion(directory, record)?.is_some_and(|receipt| receipt.stopped) {
        return Err(refused("management.local_stop_required"));
    }
    let (owned, legacy) = match ownership(directory, record) {
        Ok(ownership) if confirmation.is_none() => (Some(ownership), None),
        Ok(_) => return Ok(Outcome::Pending("legacy-confirmation-not-applicable")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let receipt = match legacy_authority(directory, record, confirmation) {
                Ok(receipt) => receipt,
                Err(_) => return Ok(Outcome::Pending("legacy-full-id-confirmation-required")),
            };
            (None, Some(receipt))
        }
        Err(_) => return Ok(Outcome::Pending("cloud-ownership-unavailable")),
    };
    let fallback = std::env::current_exe()?
        .with_file_name(format!("devtunnel{}", std::env::consts::EXE_SUFFIX));
    let verified = match cli_pins::verify(&record.cli).or_else(|_| cli_pins::verify(&fallback)) {
        Ok(verified) => verified,
        Err(_) => return Ok(Outcome::Pending("helper-unavailable")),
    };
    let script_lease = lock(directory, "account.lock")?;
    FileExt::try_lock_shared(&script_lease)?;
    let _lease = match festerm_tunnel_bridge::profile_hosting::account_lease(false) {
        Ok(lease) => Some(lease),
        // A standalone-only install has no desktop namespace. Never create it
        // just to perform cleanup or make desktop consent a script prerequisite.
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    cleanup_authorized(
        directory,
        record,
        owned,
        legacy,
        async || verify_account(verified.path()).await,
        async |id| profile::lookup(verified.path(), id).await,
        async |id| {
            super::super::cli(
                verified.path(),
                &["delete", id, "--force", "--json"],
                "management.delete_tunnel",
            )
            .await
            .map(|_| ())
        },
    )
    .await
}

async fn cleanup_authorized<A, L, D>(
    directory: &File,
    record: &Installation,
    mut owned: Option<Ownership>,
    mut legacy: Option<Legacy>,
    mut account: A,
    lookup: L,
    delete: D,
) -> io::Result<Outcome>
where
    A: AsyncFnMut() -> io::Result<String>,
    L: AsyncFnOnce(&str) -> io::Result<profile::Lookup>,
    D: AsyncFnOnce(&str) -> io::Result<()>,
{
    if account().await.ok().as_ref() != Some(&record.account) {
        return Ok(Outcome::Pending("recorded-account-sign-in-required"));
    }
    let name = cloud_name(record)?;
    let id = owned
        .as_ref()
        .and_then(|ownership| ownership.qualified.as_deref())
        .or(legacy.as_ref().map(|receipt| receipt.qualified.as_str()))
        .or(record.tunnel.as_deref())
        .unwrap_or(&name);
    let lookup = lookup(id).await;
    if account().await.ok().as_ref() != Some(&record.account) {
        return Ok(Outcome::Pending("recorded-account-sign-in-required"));
    }
    let actual = match lookup {
        Ok(profile::Lookup::Missing)
            if owned
                .as_ref()
                .is_some_and(|ownership| ownership.qualified.is_some())
                || legacy.is_some() =>
        {
            if let Some(receipt) = &mut legacy {
                receipt.cloud_gone = true;
                write(directory, LEGACY, receipt)?;
            }
            return Ok(Outcome::Complete);
        }
        Ok(profile::Lookup::Found(actual)) => actual,
        _ => return Ok(Outcome::Pending("cloud-lookup-or-creation-uncertain")),
    };
    // Reconcile submitted-but-uncertain creation before the destructive call.
    // An interruption after deletion can then prove exact not-found on retry.
    if let Some(ownership) = &mut owned {
        if !matches_cloud(record, ownership, &actual) {
            return Ok(Outcome::Pending("cloud-identity-mismatch"));
        }
        ownership.qualified = Some(actual.tunnel_id.clone());
        ownership.incarnation = actual.created;
        write(directory, OWNERSHIP, ownership)?;
    } else if let Some(receipt) = &mut legacy {
        if !legacy_matches(record, receipt, &actual) {
            return Ok(Outcome::Pending(
                "legacy-cloud-incarnation-or-identity-mismatch",
            ));
        }
        // A later optional timestamp cannot retroactively attest an earlier
        // no-timestamp attempt. Every retry still requires the typed full ID.
        if receipt.journal_new {
            receipt.incarnation = actual.created;
            receipt.incarnation_available = receipt.incarnation.is_some();
            receipt.journal_new = false;
        }
        write(directory, LEGACY, receipt)?;
    }
    if account().await.ok().as_ref() != Some(&record.account) {
        return Ok(Outcome::Pending("recorded-account-sign-in-required"));
    }
    match delete(&actual.tunnel_id).await {
        Ok(_) => Ok(Outcome::Complete),
        Err(_) => Ok(Outcome::Pending("cloud-delete-uncertain")),
    }
}

async fn remove_with<F, C>(root: &Path, installation: &str, stop: F, cloud: C) -> io::Result<Row>
where
    F: FnOnce(
        &[super::super::service_install::ServiceSpec],
        &super::super::service_install::ServiceSpec,
    ) -> io::Result<()>,
    C: AsyncFnOnce(&File, &Installation) -> io::Result<Outcome>,
{
    let (directory, mut record) = load(root)?;
    if record.id != installation {
        return Err(refused("management.exact_installation_required"));
    }
    let _namespace_installer = namespace_lock(root)?;
    let installer = lock(&directory, "installer.lock")?;
    installer
        .try_lock_exclusive()
        .map_err(|_| refused("management.operation_busy"))?;
    // Revalidate after taking the same lock used by setup, upgrade and recovery.
    let (_, current) = load(root)?;
    if current.id != installation || current.fingerprint != record.fingerprint {
        return Err(refused("management.installation_changed"));
    }
    record = current;
    if record.phase == "deleted" {
        return row(&directory, &record);
    }
    record.phase = "deleting".into();
    checkpoint(&directory, &record)?;
    let mut receipt = deletion(&directory, &record)?.unwrap_or(Deletion {
        version: 1,
        installation: record.id.clone(),
        fingerprint: record.fingerprint.clone(),
        stopped: false,
        cloud_gone: false,
    });
    write(&directory, DELETION, &receipt)?;
    let previous: Vec<_> = record
        .upgrading_from
        .iter()
        .map(|executable| super::super::service_install::ServiceSpec {
            id: record.id.clone(),
            executable: executable.clone(),
            state: root.join("state"),
        })
        .collect();
    // Even on retries, the native authority revalidates exact registrations,
    // worker command lines and journaled images. No daemon is a service child.
    if stop(&previous, &service(&record, &root.join("state"))).is_err()
        || wait_worker_stopped(&directory).await.is_err()
    {
        receipt.stopped = false;
        write(&directory, DELETION, &receipt)?;
        let mut result = row(&directory, &record)?;
        result.failure = Some("still-exposed-or-stopping-failed");
        return Ok(result);
    }
    receipt.stopped = true;
    write(&directory, DELETION, &receipt)?;
    let failure = match cloud(&directory, &record).await {
        Ok(Outcome::Complete) => {
            receipt.cloud_gone = true;
            write(&directory, DELETION, &receipt)?;
            record.phase = "deleted".into();
            checkpoint(&directory, &record)?;
            None
        }
        Ok(Outcome::Pending(reason)) => Some(reason),
        Err(_) => Some("cloud-cleanup-unavailable"),
    };
    let mut result = row(&directory, &record)?;
    result.failure = failure;
    Ok(result)
}

fn archive_completed(root: &Path, installation: &str) -> io::Result<PathBuf> {
    let parent = setup_storage::private_directory(root, false)?;
    let (directory, record) = load(root)?;
    if record.id != installation || record.phase != "deleted" {
        return Err(refused(
            "management.archive_requires_completed_exact_deletion",
        ));
    }
    let _namespace_installer = namespace_lock(root)?;
    let installer = lock(&directory, "installer.lock")?;
    installer
        .try_lock_exclusive()
        .map_err(|_| refused("management.operation_busy"))?;
    let (_, current) = load(root)?;
    if current.id != installation
        || current.phase != "deleted"
        || current.fingerprint != record.fingerprint
        || !deletion(&directory, &current)?
            .is_some_and(|receipt| receipt.stopped && receipt.cloud_gone)
    {
        return Err(refused(
            "management.archive_requires_completed_exact_deletion",
        ));
    }
    let name = format!("state-deleted-{installation}");
    #[cfg(unix)]
    {
        festerm_unix_security::rename_noreplace(
            &parent,
            Path::new("state"),
            &parent,
            Path::new(&name),
        )?;
        parent.sync_all()?;
    }
    #[cfg(windows)]
    {
        // Windows forbids renaming a directory containing an open file.
        // Completed state cannot recover/rehost; the namespace installer lock
        // still serializes archive, management and fresh explicit setup.
        drop(installer);
        let mover =
            festerm_windows_security::open_private_directory_for_move(&parent, Path::new("state"))
                .map_err(|error| {
                    io::Error::new(error.kind(), format!("management.archive_open: {error}"))
                })?;
        if !festerm_windows_security::same_file_identity(&directory, &mover)? {
            return Err(refused("management.archive_source_changed"));
        }
        festerm_windows_security::rename_private_directory_noreplace(
            &mover,
            &parent,
            Path::new(&name),
        )
        .map_err(|error| {
            io::Error::new(error.kind(), format!("management.archive_rename: {error}"))
        })?;
    }
    Ok(root.join(name))
}

pub(crate) async fn manage(args: &[OsString]) -> io::Result<()> {
    let (operation, json, confirmation) = arguments(args)?;
    manage_at(&namespace()?, operation, json, confirmation.as_deref()).await
}

async fn manage_at(
    root: &Path,
    operation: Operation,
    json: bool,
    confirmation: Option<&str>,
) -> io::Result<()> {
    if let Operation::Archive(installation) = &operation {
        super::super::service_install::ensure_unprivileged_user()?;
        let archive = archive_completed(root, installation)?;
        if json {
            println!(
                "{}",
                serde_json::json!({"version": 1, "archived": installation, "archive": archive})
            );
        } else {
            println!("Archived completed deletion {installation} to {}. Persistent shell preserved; run normal explicit setup to create a fresh installation.", archive.display());
        }
        return Ok(());
    }
    let mut rows = match inventory(root) {
        Ok(rows) => rows,
        Err(error) => {
            let reason = error.to_string();
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "version": 1,
                        "scope": "verified-local-script-installations",
                        "hosts": [],
                        "error": "inventory-unknown-or-unverified",
                        "reason": worker_log::code(&reason),
                        "io_kind": format!("{:?}", error.kind())
                    })
                );
            } else {
                println!("Local script inventory is unknown or unverified; no records were removed. Resolve protected-state access/ownership or corruption before retrying.");
            }
            return Err(error);
        }
    };
    if operation != Operation::List {
        super::super::service_install::ensure_unprivileged_user()?;
        let selected = match operation {
            Operation::Delete(id) => {
                if !rows.iter().any(|row| row.installation == id) {
                    return Err(refused(
                        "management.exact_verified_local_installation_required",
                    ));
                }
                rows.retain(|row| row.installation == id);
                rows
            }
            Operation::DeleteAll => rows,
            Operation::List => unreachable!(),
            Operation::Archive(_) => unreachable!(),
        };
        rows = Vec::new();
        for mut snapshot in selected {
            match remove_with(
                root,
                &snapshot.installation,
                super::super::service_install::uninstall,
                async |directory, record| cleanup_cloud(directory, record, confirmation).await,
            )
            .await
            {
                Ok(result) => rows.push(result),
                Err(_) => {
                    snapshot.service = "unknown".into();
                    snapshot.cleanup = "refused";
                    snapshot.failure = Some("ownership-or-operation-unavailable");
                    rows.push(snapshot);
                }
            }
        }
    }
    if json {
        println!(
            "{}",
            serde_json::json!({"version": 1, "scope": "verified-local-script-installations", "hosts": rows})
        );
    } else {
        if rows.is_empty() {
            println!(
                "No verified local script installations. Desktop hosts are managed in fesTerm."
            );
        }
        for row in &rows {
            println!(
                "{}  origin={} host={} account={} tunnel={} service={} phase={} cleanup={}",
                row.installation,
                row.origin,
                row.host,
                row.account,
                row.tunnel.as_deref().unwrap_or("(creation uncertain)"),
                row.service,
                row.phase,
                row.cleanup,
            );
            if row.legacy_incarnation_available == Some(false) {
                println!("Warning: the CLI lookup supplied no creation time; legacy cleanup relied on your exact full-ID confirmation, random installation name and recorded account, not a pinned incarnation. Every retry requires the full-ID flag again.");
            }
            if row.service == "stopped" {
                println!(
                    "{}; persistent shell preserved.",
                    if row.cleanup == "complete" {
                        "Exposure stopped; cloud cleanup complete"
                    } else {
                        "Exposure stopped; cloud cleanup pending"
                    }
                );
            } else if row.failure.is_some() {
                println!("Still exposed / stopping failed or unverified; metadata retained.");
            }
            if let Some(reason) = row.failure {
                println!("Reason: {reason}. Retry the same exact deletion after resolving it.");
                if reason == "legacy-full-id-confirmation-required" {
                    if let Some(tunnel) = &row.tunnel {
                        println!("Recorded GitHub account {}. After reviewing this exact legacy tunnel, run: festerm-tunnel-setup{} --manage delete {} --yes --confirm-legacy-cloud-id {}", row.account, std::env::consts::EXE_SUFFIX, row.installation, tunnel);
                    }
                }
            }
        }
    }
    if rows.iter().any(|row| row.failure.is_some()) {
        return Err(refused("management.cleanup_pending_or_refused"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = crate::test_support::private_directory("management-fixture");
            setup_storage::private_directory(&root.join("state"), true).unwrap();
            Self(root)
        }

        fn record(&self) -> Installation {
            let state = self.0.join("state");
            let identity = HostIdentity::load_or_create(&state.join("host-identity.key")).unwrap();
            let release = self
                .0
                .join("releases")
                .join(format!("1.2.3-{}-x86_64", std::env::consts::OS));
            Installation {
                version: 1,
                origin: None,
                id: uuid::Uuid::new_v4().to_string(),
                executable: release.join(format!(
                    "festerm-tunnel-setup{}",
                    std::env::consts::EXE_SUFFIX
                )),
                cli: release.join(format!("devtunnel{}", std::env::consts::EXE_SUFFIX)),
                runtime_root: self
                    .0
                    .join(if cfg!(windows) { "fesTerm" } else { "festerm" })
                    .join("sessiond"),
                account: "12345".into(),
                tunnel: None,
                phase: "configured".into(),
                target: None,
                fingerprint: identity.fingerprint().unwrap(),
                no_takeover: false,
                filesystem: None,
                observation: None,
                recovery: None,
                upgrading_from: Vec::new(),
            }
        }

        fn save(&self, record: &Installation) {
            let directory = setup_storage::private_directory(&self.0.join("state"), false).unwrap();
            checkpoint(&directory, record).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn restart_guard_preserves_script_target_and_allows_unrelated_generation() {
        let fixture = Fixture::new();
        let mut record = fixture.record();
        let target = festerm_tunnel_bridge::profile_hosting::Target {
            runtime_root: record.runtime_root.clone(),
            name: format!(
                "festerm-{}",
                uuid::Uuid::parse_str(&record.id).unwrap().simple()
            ),
            pid: 1234,
            generation: 5678,
            endpoint: "session-1234-5678".into(),
        };
        record.target = Some(Target {
            name: target.name.clone(),
            pid: target.pid,
            generation: target.generation,
            endpoint: target.endpoint.clone(),
        });
        fixture.save(&record);
        assert!(check_restart_target_in(&fixture.0, &target).is_err());
        let mut different = target.clone();
        different.generation += 1;
        different.endpoint = format!("session-{}-{}", different.pid, different.generation);
        check_restart_target_in(&fixture.0, &different).unwrap();
        record.target = None;
        fixture.save(&record);
        assert!(check_restart_target_in(&fixture.0, &target).is_err());
        different.runtime_root = fixture.0.join("other").join("sessiond");
        assert!(check_restart_target_in(&fixture.0, &different).is_ok());
        assert!(load(&fixture.0).unwrap().1.target.is_none());
    }

    #[test]
    fn management_arguments_require_exact_ids_confirmation_and_no_provisioning_flags() {
        let id = uuid::Uuid::new_v4().to_string();
        for args in [
            vec!["list", "--yes"],
            vec!["delete", &id],
            vec!["delete-all"],
            vec!["delete", "../state", "--yes"],
            vec!["delete", &id, "--yes", "--upgrade"],
            vec!["list", "--devtunnel", "anything"],
            vec!["list", "--json", "--json"],
            vec!["list", "--state-dir", "anything"],
            vec!["delete-all", "--yes", "--yes"],
            vec![
                "delete-all",
                "--yes",
                "--confirm-legacy-cloud-id",
                "anything.usw3",
            ],
            vec!["archive", &id],
            vec![
                "archive",
                &id,
                "--yes",
                "--confirm-legacy-cloud-id",
                "anything.usw3",
            ],
            vec!["delete", &id, "--yes", "--confirm-legacy-cloud-id"],
        ] {
            let args = args.into_iter().map(OsString::from).collect::<Vec<_>>();
            assert!(arguments(&args).is_err(), "{args:?}");
        }
        assert_eq!(
            arguments(&[
                "delete".into(),
                id.clone().into(),
                "--yes".into(),
                "--json".into()
            ])
            .unwrap(),
            (Operation::Delete(id.clone()), true, None)
        );
        assert_eq!(
            arguments(&["delete-all".into(), "--yes".into()]).unwrap(),
            (Operation::DeleteAll, false, None)
        );
        assert_eq!(
            arguments(&[
                "delete".into(),
                id.clone().into(),
                "--yes".into(),
                "--confirm-legacy-cloud-id".into(),
                "fixture.usw3".into()
            ])
            .unwrap(),
            (
                Operation::Delete(id.clone()),
                false,
                Some("fixture.usw3".into())
            )
        );
        assert_eq!(
            arguments(&["archive".into(), id.clone().into(), "--yes".into()]).unwrap(),
            (Operation::Archive(id), false, None)
        );
    }

    #[test]
    fn management_inventory_is_read_only_and_excludes_malformed_foreign_and_neighbors() {
        let fixture = Fixture::new();
        let mut record = fixture.record();
        fixture.save(&record);
        fs::write(fixture.0.join("foreign.json"), b"foreign").unwrap();
        let before = fs::read_dir(fixture.0.join("state"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        let rows = inventory(&fixture.0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].installation, record.id);
        let after = fs::read_dir(fixture.0.join("state"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(before, after);
        assert!(!fixture.0.join("state/installer.lock").exists());
        record.version = 2;
        fixture.save(&record);
        assert!(inventory(&fixture.0).is_err());
        record.version = 1;
        record.fingerprint = "foreign-key".into();
        fixture.save(&record);
        assert!(inventory(&fixture.0).is_err());
        fs::write(fixture.0.join("state").join(CONFIG), b"malformed").unwrap();
        assert!(inventory(&fixture.0).is_err());
        assert_eq!(
            fs::read(fixture.0.join("foreign.json")).unwrap(),
            b"foreign"
        );
    }

    #[tokio::test]
    async fn management_inventory_distinguishes_absence_from_unknown_and_bulk_delete_refuses() {
        let fixture = Fixture::new();
        assert!(inventory(&fixture.0.join("absent")).unwrap().is_empty());
        // An existing state directory with a missing journal is not proof that
        // no host exists. The same applies to a missing key inside valid state.
        assert!(inventory(&fixture.0).is_err());
        fs::remove_dir(fixture.0.join("state")).unwrap();
        assert!(inventory(&fixture.0).unwrap().is_empty());
        setup_storage::private_directory(&fixture.0.join("state"), true).unwrap();
        let record = fixture.record();
        fixture.save(&record);
        fs::remove_file(fixture.0.join("state/host-identity.key")).unwrap();
        assert!(inventory(&fixture.0).is_err());
        assert!(manage_at(&fixture.0, Operation::List, true, None)
            .await
            .is_err());
        assert!(manage_at(&fixture.0, Operation::DeleteAll, true, None)
            .await
            .is_err());
        assert!(!fixture.0.join("state/installer.lock").exists());
        let bytes = fs::read(fixture.0.join("state").join(CONFIG)).unwrap();
        assert_eq!(
            serde_json::from_slice::<Installation>(&bytes)
                .unwrap()
                .phase,
            "configured"
        );
        fs::write(fixture.0.join("state").join(CONFIG), b"malformed").unwrap();
        assert!(manage_at(&fixture.0, Operation::DeleteAll, false, None)
            .await
            .is_err());
        assert_eq!(
            fs::read(fixture.0.join("state").join(CONFIG)).unwrap(),
            b"malformed"
        );
    }

    #[test]
    fn management_creation_reservation_pins_cloud_label_identity_and_incarnation() {
        let fixture = Fixture::new();
        let mut record = fixture.record();
        record.phase = "creating-tunnel".into();
        fixture.save(&record);
        let directory = setup_storage::private_directory(&fixture.0.join("state"), false).unwrap();
        let label = reserve_creation(&directory, &record).unwrap();
        assert!(reserve_creation(&directory, &record).is_err());
        let mut actual = profile::CloudSnapshot {
            tunnel_id: format!("{}.usw3", cloud_name(&record).unwrap()),
            labels: vec![label],
            created: Some("fixture-created-time".into()),
        };
        let reserved = ownership(&directory, &record).unwrap();
        assert!(reserved.qualified.is_none());
        assert!(matches_cloud(&record, &reserved, &actual));
        actual.labels.clear();
        assert!(!matches_cloud(&record, &reserved, &actual));
        actual.created = None;
        assert!(!matches_cloud(&record, &reserved, &actual));
        actual.created = Some("fixture-created-time".into());
        actual.labels.push(reserved.label.clone());
        let response = serde_json::json!({"tunnel": {
            "tunnelId": actual.tunnel_id,
            "created": actual.created
        }});
        record_creation(&directory, &record, &serde_json::to_vec(&response).unwrap()).unwrap();
        let committed = ownership(&directory, &record).unwrap();
        assert!(matches_cloud(&record, &committed, &actual));
        actual.created = Some("recreated-foreign-time".into());
        assert!(!matches_cloud(&record, &committed, &actual));
        actual.created = committed.incarnation.clone();
        actual.tunnel_id = actual.tunnel_id.replace("usw3", "use2");
        assert!(!matches_cloud(&record, &committed, &actual));
        let mut foreign = record.clone();
        foreign.fingerprint = "different-key".into();
        assert!(ownership(&directory, &foreign).is_err());
    }

    #[test]
    fn management_v015_field_shape_and_explicit_legacy_authority_are_compatible() {
        let fixture = Fixture::new();
        let mut record = fixture.record();
        let release = fixture
            .0
            .join("releases")
            .join(format!("0.15.0-{}-x86_64", std::env::consts::OS));
        record.executable = release.join(format!(
            "festerm-tunnel-setup{}",
            std::env::consts::EXE_SUFFIX
        ));
        record.cli = release.join(format!("devtunnel{}", std::env::consts::EXE_SUFFIX));
        let name = cloud_name(&record).unwrap();
        let qualified = format!("{name}.usw3");
        // Field names/types come from the read-only 0.15 Windows metadata
        // inspection; all values and paths here are isolated synthetic data.
        let json = serde_json::json!({
            "version": 1, "id": record.id, "executable": record.executable,
            "cli": record.cli, "runtime_root": record.runtime_root, "account": "12345",
            "tunnel": qualified, "phase": "configured", "fingerprint": record.fingerprint,
            "target": {"name": name, "pid": 123, "generation": 456, "endpoint": "fixture-123-456"}
        });
        let directory = setup_storage::private_directory(&fixture.0.join("state"), false).unwrap();
        setup_storage::write_new_private(
            &directory,
            Path::new(CONFIG),
            &serde_json::to_vec(&json).unwrap(),
        )
        .unwrap();
        let (_, record) = load(&fixture.0).unwrap();
        assert!(record.origin.is_none());
        assert!(record.recovery.is_none());
        assert!(legacy_authority(&directory, &record, None).is_err());
        assert!(legacy_authority(&directory, &record, Some("foreign.usw3")).is_err());
        let mut receipt = legacy_authority(&directory, &record, Some(&qualified)).unwrap();
        let mut actual = profile::CloudSnapshot {
            tunnel_id: qualified.clone(),
            labels: vec![],
            created: Some("fixture-original-created".into()),
        };
        assert!(legacy_matches(&record, &receipt, &actual));
        receipt.incarnation = actual.created.clone();
        receipt.incarnation_available = true;
        write(&directory, LEGACY, &receipt).unwrap();
        assert!(legacy_authority(&directory, &record, Some(&qualified)).is_ok());
        actual.created = Some("fixture-recreated".into());
        assert!(!legacy_matches(&record, &receipt, &actual));
        actual.created = receipt.incarnation.clone();
        actual.tunnel_id = actual.tunnel_id.replace("usw3", "use2");
        assert!(!legacy_matches(&record, &receipt, &actual));
        actual.tunnel_id = qualified.clone();
        receipt.account = "99999".into();
        assert!(!legacy_matches(&record, &receipt, &actual));
        write(&directory, LEGACY, &receipt).unwrap();
        assert!(legacy_authority(&directory, &record, Some(&qualified)).is_err());
        let mut missing = record.clone();
        missing.tunnel = None;
        assert!(legacy_authority(&directory, &missing, Some(&qualified)).is_err());
        let mut changed = record.clone();
        changed.tunnel = Some(format!("{}.use2", cloud_name(&record).unwrap()));
        assert!(legacy_authority(
            &directory,
            &changed,
            Some(changed.tunnel.as_deref().unwrap())
        )
        .is_err());
        receipt.account = record.account.clone();
        receipt.cloud_gone = true;
        assert!(!legacy_matches(&record, &receipt, &actual));
    }

    #[tokio::test]
    async fn management_legacy_missing_or_mismatched_confirmation_never_reaches_cli() {
        let fixture = Fixture::new();
        let mut record = fixture.record();
        record.tunnel = Some(format!("{}.usw3", cloud_name(&record).unwrap()));
        fixture.save(&record);
        for confirmation in [None, Some("foreign.usw3")] {
            let result = remove_with(
                &fixture.0,
                &record.id,
                |_, _| Ok(()),
                async |directory, record| cleanup_cloud(directory, record, confirmation).await,
            )
            .await
            .unwrap();
            assert_eq!(result.failure, Some("legacy-full-id-confirmation-required"));
            assert_eq!(result.service, "stopped");
            assert!(!fixture.0.join("state/account.lock").exists());
            assert!(!fixture.0.join("state").join(LEGACY).exists());
        }
        let directory = setup_storage::private_directory(&fixture.0.join("state"), false).unwrap();
        reserve_creation(&directory, &record).unwrap();
        drop(directory);
        let qualified = record.tunnel.as_deref().unwrap();
        let result = remove_with(
            &fixture.0,
            &record.id,
            |_, _| Ok(()),
            async |directory, record| cleanup_cloud(directory, record, Some(qualified)).await,
        )
        .await
        .unwrap();
        assert_eq!(result.failure, Some("legacy-confirmation-not-applicable"));
        assert!(!fixture.0.join("state/account.lock").exists());
    }

    #[tokio::test]
    async fn management_legacy_cleanup_pins_incarnation_rechecks_account_and_recovers_exact_not_found(
    ) {
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        enum Step {
            UncertainDelete,
            WrongAccount,
            ChangedAccount,
            Recreated,
            MissingIncarnation,
            Missing,
        }
        let fixture = Fixture::new();
        let mut original = fixture.record();
        original.tunnel = Some(format!("{}.usw3", cloud_name(&original).unwrap()));
        fixture.save(&original);
        let deletes = Cell::new(0);
        for step in [
            Step::UncertainDelete,
            Step::WrongAccount,
            Step::ChangedAccount,
            Step::Recreated,
            Step::MissingIncarnation,
            Step::Missing,
        ] {
            let accounts = Cell::new(0);
            let lookups = Cell::new(0);
            let result = remove_with(
                &fixture.0,
                &original.id,
                |_, _| Ok(()),
                async |directory, record| {
                    let receipt = legacy_authority(directory, record, original.tunnel.as_deref())?;
                    cleanup_authorized(
                        directory,
                        record,
                        None,
                        Some(receipt),
                        async || {
                            let count = accounts.get();
                            accounts.set(count + 1);
                            Ok(
                                if step == Step::WrongAccount
                                    || (step == Step::ChangedAccount && count > 0)
                                {
                                    "99999".into()
                                } else {
                                    record.account.clone()
                                },
                            )
                        },
                        async |id| {
                            assert_eq!(Some(id), original.tunnel.as_deref());
                            lookups.set(lookups.get() + 1);
                            Ok(if step == Step::Missing {
                                profile::Lookup::Missing
                            } else {
                                profile::Lookup::Found(profile::CloudSnapshot {
                                    tunnel_id: id.into(),
                                    labels: vec![],
                                    created: if step == Step::MissingIncarnation {
                                        None
                                    } else {
                                        Some(
                                            if step == Step::Recreated {
                                                "foreign-recreation"
                                            } else {
                                                "original-incarnation"
                                            }
                                            .into(),
                                        )
                                    },
                                })
                            })
                        },
                        async |id| {
                            assert_eq!(step, Step::UncertainDelete);
                            assert_eq!(Some(id), original.tunnel.as_deref());
                            let before_delete: Legacy = read_json(directory, LEGACY)?;
                            assert_eq!(before_delete.provenance, "legacy-confirmed");
                            assert_eq!(
                                before_delete.incarnation.as_deref(),
                                Some("original-incarnation")
                            );
                            deletes.set(deletes.get() + 1);
                            Err(refused("fixture.delete_submitted_response_lost"))
                        },
                    )
                    .await
                },
            )
            .await
            .unwrap();
            let expected = match step {
                Step::UncertainDelete => Some("cloud-delete-uncertain"),
                Step::WrongAccount | Step::ChangedAccount => {
                    Some("recorded-account-sign-in-required")
                }
                Step::Recreated | Step::MissingIncarnation => {
                    Some("legacy-cloud-incarnation-or-identity-mismatch")
                }
                Step::Missing => None,
            };
            assert_eq!(result.failure, expected, "{step:?}");
            assert_eq!(lookups.get(), u32::from(step != Step::WrongAccount));
        }
        assert_eq!(deletes.get(), 1);
        let (directory, final_record) = load(&fixture.0).unwrap();
        assert_eq!(final_record.phase, "deleted");
        assert_eq!(final_record.executable, original.executable);
        assert_eq!(final_record.cli, original.cli);
        let receipt: Legacy = read_json(&directory, LEGACY).unwrap();
        assert!(receipt.cloud_gone);
        assert_eq!(receipt.incarnation.as_deref(), Some("original-incarnation"));
        assert!(
            deletion(&directory, &final_record)
                .unwrap()
                .unwrap()
                .cloud_gone
        );
    }

    #[tokio::test]
    async fn management_legacy_optional_incarnation_requires_reconfirmation_and_never_retroactively_pins(
    ) {
        for uncertain_first in [false, true] {
            let fixture = Fixture::new();
            let mut original = fixture.record();
            original.tunnel = Some(format!("{}.usw3", cloud_name(&original).unwrap()));
            fixture.save(&original);
            for attempt in 0..=u8::from(uncertain_first) {
                let accounts = Cell::new(0);
                let result = remove_with(
                    &fixture.0,
                    &original.id,
                    |_, _| Ok(()),
                    async |directory, record| {
                        assert!(legacy_authority(directory, record, None).is_err());
                        let receipt =
                            legacy_authority(directory, record, original.tunnel.as_deref())?;
                        cleanup_authorized(
                            directory,
                            record,
                            None,
                            Some(receipt),
                            async || {
                                accounts.set(accounts.get() + 1);
                                Ok(record.account.clone())
                            },
                            async |id| {
                                // The real pinned 0.15 CLI omits creation time.
                                let mut json = serde_json::json!({"tunnel": {
                                    "tunnelId": id, "labels": []
                                }});
                                if attempt > 0 {
                                    json["tunnel"]["created"] = "later-optional-time".into();
                                }
                                let reply: profile::CloudReply =
                                    serde_json::from_value(json).unwrap();
                                Ok(profile::Lookup::Found(reply.tunnel))
                            },
                            async |id| {
                                assert_eq!(Some(id), original.tunnel.as_deref());
                                let journal: serde_json::Value = read_json(directory, LEGACY)?;
                                assert!(journal["incarnation"].is_null());
                                assert_eq!(journal["incarnation_available"], false);
                                if uncertain_first && attempt == 0 {
                                    Err(refused("fixture.delete_submitted_response_lost"))
                                } else {
                                    Ok(())
                                }
                            },
                        )
                        .await
                    },
                )
                .await
                .unwrap();
                assert_eq!(accounts.get(), 3);
                assert_eq!(result.legacy_incarnation_available, Some(false));
                let pending = uncertain_first && attempt == 0;
                assert_eq!(result.failure, pending.then_some("cloud-delete-uncertain"));
                assert_eq!(result.cleanup, if pending { "pending" } else { "complete" });
                let (directory, record) = load(&fixture.0).unwrap();
                let journal =
                    legacy_authority(&directory, &record, original.tunnel.as_deref()).unwrap();
                assert!(journal.incarnation.is_none());
                assert!(!journal.incarnation_available);
                assert!(!journal.journal_new);
            }
        }
    }

    #[tokio::test]
    async fn management_archive_requires_complete_cleanup_preserves_receipts_and_allows_fresh_setup(
    ) {
        let fixture = Fixture::new();
        let record = fixture.record();
        fixture.save(&record);
        assert!(archive_completed(&fixture.0, &record.id).is_err());
        remove_with(
            &fixture.0,
            &record.id,
            |_, _| Ok(()),
            async |_, _| Ok(Outcome::Pending("offline")),
        )
        .await
        .unwrap();
        assert!(archive_completed(&fixture.0, &record.id).is_err());
        remove_with(
            &fixture.0,
            &record.id,
            |_, _| Ok(()),
            async |_, _| Ok(Outcome::Complete),
        )
        .await
        .unwrap();
        let (_, completed) = load(&fixture.0).unwrap();
        let guidance = rerun_plan(
            &completed,
            &completed.executable,
            &completed.cli,
            false,
            false,
        )
        .unwrap_err();
        assert!(guidance
            .to_string()
            .contains(&format!("--manage archive {} --yes", record.id)));
        let namespace_installer = namespace_lock(&fixture.0).unwrap();
        assert!(archive_completed(&fixture.0, &record.id).is_err());
        assert!(fixture.0.join("state").join(DELETION).exists());
        drop(namespace_installer);
        let target = fixture.0.join(format!("state-deleted-{}", record.id));
        setup_storage::private_directory(&target, true).unwrap();
        assert!(archive_completed(&fixture.0, &record.id).is_err());
        assert!(fixture.0.join("state").join(CONFIG).exists());
        fs::remove_dir(&target).unwrap();
        let archived = archive_completed(&fixture.0, &record.id).unwrap();
        assert_eq!(archived, target);
        assert!(archived.join(CONFIG).exists());
        assert!(archived.join(DELETION).exists());
        assert!(archived.join("host-identity.key").exists());
        assert!(inventory(&fixture.0).unwrap().is_empty());
        let state = fixture.0.join("state");
        setup_storage::private_directory(&state, true).unwrap();
        refuse_orphaned_state(&state).unwrap();
        let fresh = fixture.record();
        fixture.save(&fresh);
        assert_ne!(fresh.id, record.id);
        assert_ne!(fresh.fingerprint, record.fingerprint);
        assert_eq!(inventory(&fixture.0).unwrap()[0].installation, fresh.id);
        assert!(archived.join(CONFIG).exists());
    }

    #[cfg(unix)]
    #[test]
    fn management_inventory_never_follows_substituted_state_or_record() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let record = fixture.record();
        fixture.save(&record);
        let state = fixture.0.join("state");
        fs::rename(&state, fixture.0.join("foreign")).unwrap();
        symlink(fixture.0.join("foreign"), &state).unwrap();
        assert!(inventory(&fixture.0).is_err());
        fs::remove_file(&state).unwrap();
        fs::rename(fixture.0.join("foreign"), &state).unwrap();
        fs::rename(state.join(CONFIG), state.join("original.json")).unwrap();
        symlink("original.json", state.join(CONFIG)).unwrap();
        assert!(inventory(&fixture.0).is_err());
    }

    #[tokio::test]
    async fn management_exact_delete_preserves_shell_and_offline_evidence_then_retries() {
        let fixture = Fixture::new();
        let record = fixture.record();
        fixture.save(&record);
        let shell = fixture.0.join("independent-shell");
        fs::write(&shell, b"shell-retained").unwrap();
        let calls = Cell::new(0);
        let stop = |_: &[_], _: &super::super::super::service_install::ServiceSpec| {
            calls.set(calls.get() + 1);
            Ok(())
        };
        assert!(remove_with(
            &fixture.0,
            &uuid::Uuid::new_v4().to_string(),
            stop,
            async |_, _| panic!("wrong ID must never reach cloud")
        )
        .await
        .is_err());
        assert_eq!(calls.get(), 0);
        let pending = remove_with(&fixture.0, &record.id, stop, async |_, _| {
            Ok(Outcome::Pending("offline"))
        })
        .await
        .unwrap();
        assert_eq!(pending.service, "stopped");
        assert_eq!(pending.cleanup, "pending");
        let (directory, retained) = load(&fixture.0).unwrap();
        assert_eq!(retained.phase, "deleting");
        assert!(deletion(&directory, &retained).unwrap().unwrap().stopped);
        assert!(rerun_plan(&retained, &retained.executable, &retained.cli, false, true).is_err());
        drop(directory);
        let complete = remove_with(&fixture.0, &record.id, stop, async |_, _| {
            Ok(Outcome::Complete)
        })
        .await
        .unwrap();
        assert_eq!(complete.cleanup, "complete");
        remove_with(
            &fixture.0,
            &record.id,
            |_, _| panic!("completed deletion must be idempotent"),
            async |_, _| panic!("completed deletion must not repeat cloud calls"),
        )
        .await
        .unwrap();
        assert_eq!(fs::read(shell).unwrap(), b"shell-retained");
        assert!(fixture.0.join("state").join(CONFIG).exists());
        assert!(fixture.0.join("state/host-identity.key").exists());
    }

    #[tokio::test]
    async fn management_failed_stop_and_cancelled_cloud_never_lose_recovery_evidence() {
        let fixture = Fixture::new();
        let mut record = fixture.record();
        record.phase = UPGRADING.into();
        record
            .upgrading_from
            .push(record.executable.with_file_name("previous-helper"));
        fixture.save(&record);
        let result = remove_with(
            &fixture.0,
            &record.id,
            |previous, _| {
                assert_eq!(previous.len(), 1);
                Err(refused("fixture.stop_refused"))
            },
            async |_, _| panic!("failed stop must never delete cloud state"),
        )
        .await
        .unwrap();
        assert_ne!(result.service, "stopped");
        assert_eq!(result.failure, Some("still-exposed-or-stopping-failed"));
        let cancelled = timeout(
            Duration::from_millis(10),
            remove_with(
                &fixture.0,
                &record.id,
                |_, _| Ok(()),
                async |_, _| std::future::pending::<io::Result<Outcome>>().await,
            ),
        )
        .await;
        assert!(cancelled.is_err());
        let (directory, retained) = load(&fixture.0).unwrap();
        assert_eq!(retained.upgrading_from, record.upgrading_from);
        assert!(deletion(&directory, &retained).unwrap().unwrap().stopped);
    }

    #[tokio::test]
    async fn management_delete_all_snapshot_revalidates_ownership_and_keeps_uncertain_creation() {
        let fixture = Fixture::new();
        let record = fixture.record();
        fixture.save(&record);
        let snapshot = inventory(&fixture.0).unwrap();
        let mut replacement = record.clone();
        replacement.id = uuid::Uuid::new_v4().to_string();
        fixture.save(&replacement);
        assert!(remove_with(
            &fixture.0,
            &snapshot[0].installation,
            |_, _| panic!("snapshot replacement must refuse"),
            async |_, _| panic!("snapshot replacement must refuse"),
        )
        .await
        .is_err());
        replacement.phase = "creating-tunnel".into();
        fixture.save(&replacement);
        let result = remove_with(
            &fixture.0,
            &replacement.id,
            |_, _| Ok(()),
            async |directory, record| cleanup_cloud(directory, record, None).await,
        )
        .await
        .unwrap();
        assert_eq!(result.cleanup, "pending");
        assert_eq!(result.failure, Some("legacy-full-id-confirmation-required"));
        assert!(load(&fixture.0).unwrap().1.tunnel.is_none());
    }
}
