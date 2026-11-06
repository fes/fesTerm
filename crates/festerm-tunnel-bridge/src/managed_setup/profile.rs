use super::*;
use festerm_tunnel_bridge::{
    cli_pins,
    profile_hosting::{
        self, BindExistingRequest, CloudIdentity, CloudReservation, Consent, Failure,
        ManagedHostStatus, ManagementOperation, ManagementReply, ManagementRequest, Origin, Phase,
        Store,
    },
};

const INVENTORY_LIMIT: usize = 1000;

pub(crate) async fn request_file(args: &[OsString]) -> io::Result<()> {
    super::super::service_install::ensure_unprivileged_user()?;
    let [flag, path] = args else {
        return Err(refused("profile_host.request_arguments"));
    };
    let path = PathBuf::from(path);
    if flag != "--request-json"
        || !path.is_absolute()
        || path.parent() != Some(profile_hosting::namespace()?.join("requests").as_path())
    {
        return Err(refused("profile_host.request_path"));
    }
    let directory = setup_storage::private_directory(
        path.parent()
            .ok_or_else(|| refused("profile_host.request_path"))?,
        false,
    )?;
    let bytes = setup_storage::read_private(
        &directory,
        Path::new(
            path.file_name()
                .ok_or_else(|| refused("profile_host.request_path"))?,
        ),
        64 * 1024,
    )?;
    let request: ManagementRequest =
        serde_json::from_slice(&bytes).map_err(|_| refused("profile_host.request_decode"))?;
    if request.version != 1 {
        return Err(refused("profile_host.request_version"));
    }
    let installation = match &request.operation {
        ManagementOperation::BindExisting(request) => Some(request.installation.clone()),
        ManagementOperation::Status { installation }
        | ManagementOperation::Remove { installation, .. } => Some(installation.clone()),
        ManagementOperation::Inventory => None,
    };
    let result = match request.operation {
        ManagementOperation::BindExisting(request) => {
            bind_existing(*request).await.map(|row| vec![row])
        }
        ManagementOperation::Status { installation } => status(&installation).map(|row| vec![row]),
        ManagementOperation::Remove {
            installation,
            consent,
        } => match consent {
            Some(consent) => remove(&installation, &consent).await.map(|row| vec![row]),
            None => Err(refused("profile_host.consent_required")),
        },
        ManagementOperation::Inventory => inventory(),
    };
    let (hosts, error) = match result {
        Ok(hosts) => (hosts, None),
        Err(error) => {
            let rows = installation
                .as_deref()
                .and_then(|id| status(id).ok())
                .into_iter()
                .collect();
            (rows, Some(worker_log::code(&error.to_string()).to_owned()))
        }
    };
    let reply = serde_json::to_string(&ManagementReply {
        version: 1,
        hosts,
        error,
    })
    .map_err(|_| refused("profile_host.reply_encode"))?;
    if reply.len() > 1024 * 1024 {
        return Err(refused("profile_host.reply_size"));
    }
    println!("{reply}");
    Ok(())
}

fn validate_desktop_entry(record: &Installation, installation: &str) -> io::Result<()> {
    validate_record(record)?;
    let releases = profile_hosting::namespace()?.join("releases");
    let runtime =
        festerm_sessiond::runtime_root().map_err(|_| refused("profile_host.runtime_namespace"))?;
    let valid_helper = |path: &Path| {
        path.file_name()
            == Some(std::ffi::OsStr::new(&format!(
                "festerm-tunnel-setup{}",
                std::env::consts::EXE_SUFFIX
            )))
            && path.parent().and_then(Path::parent) == Some(releases.as_path())
    };
    if record.version != 2
        || record.origin.is_none()
        || record.id != installation
        || record.runtime_root != runtime
        || !valid_helper(&record.executable)
        || record.upgrading_from.iter().any(|path| !valid_helper(path))
        || record.cli
            != record
                .executable
                .with_file_name(format!("devtunnel{}", std::env::consts::EXE_SUFFIX))
    {
        return Err(refused("profile_host.installation_identity"));
    }
    Ok(())
}

fn current_helpers(cli: &Path) -> io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let expected = profile_hosting::namespace()?.join("releases").join(format!(
        "{}-{}-{}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    if executable.parent() != Some(expected.as_path())
        || cli != executable.with_file_name(format!("devtunnel{}", std::env::consts::EXE_SUFFIX))
    {
        return Err(refused("profile_host.helpers_not_staged"));
    }
    let _directory = setup_storage::private_directory(&expected, false)?;
    #[cfg(windows)]
    super::super::service_install::check_windows_worker(&executable)?;
    Ok(executable)
}

fn selected(target: &profile_hosting::Target) -> festerm_sessiond::NativeSessionIdentity {
    festerm_sessiond::NativeSessionIdentity {
        name: target.name.clone(),
        pid: target.pid,
        created_at_unix_ms: target.generation,
        endpoint: target.endpoint.clone(),
    }
}

fn verify_target(request: &BindExistingRequest) -> io::Result<()> {
    if request.target.runtime_root
        != festerm_sessiond::runtime_root()
            .map_err(|_| refused("profile_host.runtime_namespace"))?
    {
        return Err(refused("profile_host.runtime_namespace"));
    }
    festerm_sessiond::verify_spawn_fingerprint_in(
        &request.target.runtime_root,
        &selected(&request.target),
        &request.spawn_fingerprint,
    )
    .map_err(|_| refused("profile_host.command_mismatch"))?;
    exact_target_alive(
        &request.target.runtime_root,
        &Target {
            name: request.target.name.clone(),
            pid: request.target.pid,
            generation: request.target.generation,
            endpoint: request.target.endpoint.clone(),
        },
    )
}

fn require_request(store: &Store, request: &BindExistingRequest) -> io::Result<Consent> {
    let grant = store.read()?;
    if grant.consent != request.consent
        || grant.spawn_fingerprint != request.spawn_fingerprint
        || !grant.phase.allows_hosting()
    {
        return Err(refused("profile_host.consent_not_active"));
    }
    Ok(grant)
}

#[derive(Deserialize)]
pub(super) struct CloudReply {
    pub(super) tunnel: CloudSnapshot,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CloudSnapshot {
    pub(super) tunnel_id: String,
    #[serde(default)]
    pub(super) labels: Vec<String>,
    pub(super) created: Option<String>,
}

pub(super) enum Lookup {
    Found(CloudSnapshot),
    Missing,
    Uncertain,
}

fn unqualified_not_found(code: Option<i32>, stdout: &[u8], stderr: &[u8], name: &str) -> bool {
    if name.contains('.') || code != Some(2) {
        return false;
    }
    let response = if stdout.is_empty() {
        stderr
    } else if stderr.is_empty() {
        stdout
    } else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(response) else {
        return false;
    };
    let Some((cluster, actual)) = text
        .trim_end_matches(['\r', '\n'])
        .strip_prefix("Tunnel not found in ")
        .and_then(|text| text.split_once(": "))
    else {
        return false;
    };
    actual == name
        && !cluster.is_empty()
        && cluster.len() <= 32
        && cluster.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn exact_not_found(code: Option<i32>, stdout: &[u8], stderr: &[u8], qualified: &str) -> bool {
    let Some((name, cluster)) = qualified.rsplit_once('.') else {
        return false;
    };
    let expected = format!("Tunnel not found in {cluster}: {name}");
    let matches = |bytes: &[u8]| {
        std::str::from_utf8(bytes).is_ok_and(|text| text.trim_end_matches(['\r', '\n']) == expected)
    };
    code == Some(2)
        && ((stdout.is_empty() && matches(stderr)) || (stderr.is_empty() && matches(stdout)))
}

pub(super) async fn lookup(cli: &Path, id: &str) -> io::Result<Lookup> {
    let response =
        super::super::cli_captured(cli, &["show", id, "--json"], "profile_host.lookup").await?;
    if exact_not_found(response.code, &response.stdout, &response.stderr, id) {
        return Ok(Lookup::Missing);
    }
    if unqualified_not_found(response.code, &response.stdout, &response.stderr, id) {
        return Ok(Lookup::Uncertain);
    }
    if response.code != Some(0) {
        return Err(refused("profile_host.lookup_unknown"));
    }
    let reply: CloudReply = serde_json::from_slice(&response.stdout)
        .map_err(|_| refused("profile_host.lookup_unknown"))?;
    Ok(Lookup::Found(reply.tunnel))
}

fn cloud_matches(
    reservation: &CloudReservation,
    expected: Option<&CloudIdentity>,
    actual: &CloudSnapshot,
) -> io::Result<CloudIdentity> {
    super::super::split_created_id(&actual.tunnel_id, &reservation.name)?;
    if !actual
        .labels
        .iter()
        .any(|label| label == &reservation.ownership_label)
        || expected.is_some_and(|expected| {
            expected.qualified_id != actual.tunnel_id
                || expected
                    .incarnation
                    .as_ref()
                    .is_some_and(|created| actual.created.as_ref() != Some(created))
        })
    {
        return Err(refused("profile_host.cloud_identity"));
    }
    Ok(CloudIdentity {
        qualified_id: actual.tunnel_id.clone(),
        incarnation: actual.created.clone(),
    })
}

async fn require_account(cli: &Path, account: &str) -> io::Result<()> {
    if verify_account(cli).await? != account {
        return Err(refused("profile_host.sign_in_recorded_account"));
    }
    Ok(())
}

fn origin(grant: &Consent) -> io::Result<DesktopOrigin> {
    let reservation = grant
        .cloud_reservation
        .as_ref()
        .ok_or_else(|| refused("profile_host.cloud_identity"))?;
    Ok(DesktopOrigin {
        kind: "desktop-profile".into(),
        consent: grant.consent.clone(),
        spawn_fingerprint: grant.spawn_fingerprint.clone(),
        ownership_label: reservation.ownership_label.clone(),
        incarnation: grant
            .cloud
            .as_ref()
            .and_then(|cloud| cloud.incarnation.clone()),
    })
}

async fn ensure_port(cli: &Path, qualified: &str) -> io::Result<()> {
    #[derive(Deserialize)]
    struct Ports {
        ports: Vec<Port>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Port {
        port_number: u16,
        protocol: String,
    }
    let response = super::super::cli(
        cli,
        &["port", "list", qualified, "--json"],
        "profile_host.port_lookup",
    )
    .await?;
    let ports: Ports =
        serde_json::from_slice(&response).map_err(|_| refused("profile_host.port_lookup"))?;
    if let Some(port) = ports.ports.iter().find(|port| port.port_number == 47461) {
        return if port.protocol == "auto" {
            Ok(())
        } else {
            Err(refused("profile_host.port_identity"))
        };
    }
    super::super::cli(
        cli,
        &[
            "port",
            "create",
            qualified,
            "--port-number",
            "47461",
            "--description",
            "festerm-bridge-v1",
            "--json",
        ],
        "profile_host.create_port",
    )
    .await?;
    Ok(())
}

async fn wait_stopped(directory: &File) -> io::Result<()> {
    let worker = lock(directory, "worker.lock")?;
    for _ in 0..150 {
        match worker.try_lock_exclusive() {
            Ok(()) => return FileExt::unlock(&worker),
            Err(error) if festerm_sessiond::lock_is_contended(&error) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(refused("profile_host.worker_still_running"))
}

async fn bind_existing(request: BindExistingRequest) -> io::Result<ManagedHostStatus> {
    let state = profile_hosting::installation_path(&request.installation)?;
    let store = Store::open(&state, &request.installation, false)?;
    require_request(&store, &request)?;
    if request.description.is_empty()
        || request.description.chars().count() > 100
        || request.description.chars().any(char::is_control)
    {
        return Err(refused("profile_host.description"));
    }
    verify_target(&request)?;
    store.bind_target(&request.consent, request.target.clone())?;
    let executable = current_helpers(&request.cli)?;
    let verified_cli = cli_pins::verify(&request.cli)?;
    let directory = setup_storage::private_directory(&state, false)?;
    let installer = lock(&directory, "installer.lock")?;
    installer
        .try_lock_exclusive()
        .map_err(|_| refused("profile_host.operation_busy"))?;
    let mut existing = match read_json::<Installation>(&directory, CONFIG) {
        Ok(record) => {
            validate_desktop_entry(&record, &request.installation)?;
            Some(record)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let _account_lease = profile_hosting::account_lease(false)?;
    let account = match verify_account(verified_cli.path()).await {
        Ok(account) => account,
        Err(_) => {
            store.report_failure(&request.consent, Failure::SignInRequired)?;
            return status(&request.installation);
        }
    };
    let grant = require_request(&store, &request)?;
    if existing
        .as_ref()
        .is_some_and(|record| record.account != account)
        || grant
            .cloud_reservation
            .as_ref()
            .is_some_and(|reservation| reservation.account != account)
    {
        store.report_failure(&request.consent, Failure::SignInRequired)?;
        return status(&request.installation);
    }
    let grant = if grant.cloud_reservation.is_none() {
        store.reserve_creation(&request.consent, &account)?
    } else {
        grant
    };
    let reservation = grant
        .cloud_reservation
        .as_ref()
        .ok_or_else(|| refused("profile_host.cloud_identity"))?;
    let identity = HostIdentity::load_or_create(&state.join("host-identity.key"))?;
    let mut record = if let Some(mut record) = existing.take() {
        if record.fingerprint != identity.fingerprint()? {
            return Err(refused("profile_host.host_identity"));
        }
        let changed = record.executable != executable
            || record.origin.as_ref().is_none_or(|old| {
                old.consent != request.consent || old.ownership_label != reservation.ownership_label
            })
            || record.phase == UPGRADING;
        if changed {
            if record.executable != executable && !request.upgrade {
                return Err(refused("profile_host.helper_upgrade_required"));
            }
            if !record.upgrading_from.contains(&record.executable) {
                record.upgrading_from.push(record.executable.clone());
            }
            let previous: Vec<_> = record
                .upgrading_from
                .iter()
                .map(|path| super::super::service_install::ServiceSpec {
                    id: record.id.clone(),
                    executable: path.clone(),
                    state: state.clone(),
                })
                .collect();
            record.executable = executable.clone();
            record.cli = request.cli.clone();
            record.phase = UPGRADING.into();
            checkpoint(&directory, &record)?;
            super::super::service_install::stop_and_replace(&previous, &service(&record, &state))?;
            wait_stopped(&directory).await?;
            record.upgrading_from.clear();
        }
        record
    } else {
        Installation {
            version: 2,
            origin: Some(origin(&grant)?),
            id: request.installation.clone(),
            executable,
            cli: request.cli.clone(),
            runtime_root: request.target.runtime_root.clone(),
            account: account.clone(),
            tunnel: None,
            phase: "creating-tunnel".into(),
            target: None,
            fingerprint: identity.fingerprint()?,
            no_takeover: false,
            filesystem: None,
            observation: None,
            recovery: None,
            upgrading_from: Vec::new(),
        }
    };
    record.target = Some(Target {
        name: request.target.name.clone(),
        pid: request.target.pid,
        generation: request.target.generation,
        endpoint: request.target.endpoint.clone(),
    });
    record.origin = Some(origin(&grant)?);
    record.phase = "creating-tunnel".into();
    checkpoint(&directory, &record)?;
    let cloud = if let Some(cloud) = &grant.cloud {
        match lookup(verified_cli.path(), &cloud.qualified_id).await? {
            Lookup::Found(actual) => cloud_matches(reservation, Some(cloud), &actual)?,
            Lookup::Missing => {
                store.mark_cloud_missing(&request.consent)?;
                return status(&request.installation);
            }
            Lookup::Uncertain => return Err(refused("profile_host.lookup_unknown")),
        }
    } else {
        // A reservation survives crashes, so only its first creator submits the
        // mutation. Later invocations reconcile its ownership marker.
        require_request(&store, &request)?;
        verify_target(&request)?;
        let first = store.claim_creation_submission(&request.consent)?;
        let actual = if first {
            let response = super::super::cli(
                verified_cli.path(),
                &[
                    "create",
                    &reservation.name,
                    "--expiration",
                    "30d",
                    "--description",
                    &request.description,
                    "--labels",
                    &reservation.ownership_label,
                    "--json",
                ],
                "profile_host.create_tunnel",
            )
            .await?;
            let reply: CloudReply = serde_json::from_slice(&response)
                .map_err(|_| refused("profile_host.create_response"))?;
            reply.tunnel
        } else {
            match lookup(verified_cli.path(), &reservation.name).await? {
                Lookup::Found(actual) => actual,
                Lookup::Missing | Lookup::Uncertain => {
                    store.report_failure(&request.consent, Failure::CreationUncertain)?;
                    return status(&request.installation);
                }
            }
        };
        require_account(verified_cli.path(), &account).await?;
        let cloud = cloud_matches(reservation, None, &actual)?;
        store.record_created(&request.consent, cloud.clone())?;
        cloud
    };
    record.tunnel = Some(cloud.qualified_id.clone());
    record.origin = Some(origin(&store.read()?)?);
    record.phase = "creating-port".into();
    checkpoint(&directory, &record)?;
    require_request(&store, &request)?;
    require_account(verified_cli.path(), &account).await?;
    ensure_port(verified_cli.path(), &cloud.qualified_id).await?;
    verify_target(&request)?;
    require_request(&store, &request)?;
    record.phase = "configured".into();
    checkpoint(&directory, &record)?;
    super::super::service_install::install_and_start(&service(&record, &state))?;
    timeout(Duration::from_secs(50), async {
        loop {
            with_desktop_permit(&state, &record, || Ok(()))?;
            if healthy_record(&directory, &record)?.is_some() {
                return status(&request.installation);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| refused("profile_host.not_ready"))?
}

fn status(installation: &str) -> io::Result<ManagedHostStatus> {
    let state = profile_hosting::installation_path(installation)?;
    let store = Store::open(&state, installation, false)?;
    let grant = store.read()?;
    let directory = setup_storage::private_directory(&state, false)?;
    let mut destination = None;
    let mut phase = grant.phase;
    if grant.phase == Phase::Exposed {
        match read_json::<Installation>(&directory, CONFIG) {
            Ok(record) => {
                validate_desktop_entry(&record, installation)?;
                if with_desktop_permit(&state, &record, || Ok(())).is_ok() {
                    destination =
                        healthy_record(&directory, &record)?.map(|ready| ready.destination);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if destination.is_none() {
            phase = Phase::Provisioning;
        }
    }
    Ok(ManagedHostStatus {
        installation: installation.to_owned(),
        origin: Origin::DesktopProfile,
        consent: Some(grant.consent),
        phase,
        account: grant
            .cloud_reservation
            .as_ref()
            .map(|reservation| reservation.account.clone()),
        tunnel: grant.cloud.map(|cloud| cloud.qualified_id),
        failure: grant.failure,
        destination,
    })
}

fn inventory() -> io::Result<Vec<ManagedHostStatus>> {
    let root = profile_hosting::namespace()?.join("installations");
    let _directory = match setup_storage::private_directory(&root, false) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut hosts = Vec::new();
    for (count, entry) in fs::read_dir(root)?.take(INVENTORY_LIMIT + 1).enumerate() {
        if count == INVENTORY_LIMIT {
            return Err(refused("profile_host.inventory_limit"));
        }
        let entry = entry?;
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if profile_hosting::installation_path(&id).is_err() {
            continue;
        }
        hosts.push(status(&id).unwrap_or(ManagedHostStatus {
            installation: id,
            origin: Origin::Unknown,
            consent: None,
            phase: Phase::Prepared,
            account: None,
            tunnel: None,
            failure: Some(Failure::Unknown),
            destination: None,
        }));
    }
    hosts.sort_by(|left, right| left.installation.cmp(&right.installation));
    Ok(hosts)
}

async fn remove(installation: &str, consent: &str) -> io::Result<ManagedHostStatus> {
    let state = profile_hosting::installation_path(installation)?;
    let store = Store::open(&state, installation, false)?;
    store.revoke(consent)?;
    let directory = setup_storage::private_directory(&state, false)?;
    let installer = lock(&directory, "installer.lock")?;
    installer
        .try_lock_exclusive()
        .map_err(|_| refused("profile_host.operation_busy"))?;
    let record = match read_json::<Installation>(&directory, CONFIG) {
        Ok(record) => {
            validate_desktop_entry(&record, installation)?;
            let previous: Vec<_> = record
                .upgrading_from
                .iter()
                .map(|executable| super::super::service_install::ServiceSpec {
                    id: record.id.clone(),
                    executable: executable.clone(),
                    state: state.clone(),
                })
                .collect();
            super::super::service_install::uninstall(&previous, &service(&record, &state))?;
            Some(record)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let grant = store.read()?;
            if grant.cloud.is_some() || !grant.worker_stopped {
                return Err(refused("profile_host.missing_installation"));
            }
            None
        }
        Err(error) => return Err(error),
    };
    wait_stopped(&directory).await?;
    let grant = store.mark_stopped(consent)?;
    if grant.creation_pending && !grant.creation_submitted {
        store.cancel_unsubmitted_creation(consent)?;
        store.complete_deletion(consent)?;
        return status(installation);
    }
    if grant.phase == Phase::Revoked {
        return status(installation);
    }
    let cli = match &record {
        Some(record) => record.cli.clone(),
        None => {
            let cli = std::env::current_exe()?
                .with_file_name(format!("devtunnel{}", std::env::consts::EXE_SUFFIX));
            current_helpers(&cli)?;
            cli
        }
    };
    let verified = match cli_pins::verify(&cli) {
        Ok(verified) => verified,
        Err(_) => {
            store.report_failure(consent, Failure::HelperUnavailable)?;
            return status(installation);
        }
    };
    let _account_lease = profile_hosting::account_lease(false)?;
    let reservation = grant
        .cloud_reservation
        .as_ref()
        .ok_or_else(|| refused("profile_host.cloud_identity"))?;
    if require_account(verified.path(), &reservation.account)
        .await
        .is_err()
    {
        store.report_failure(consent, Failure::SignInRequired)?;
        return status(installation);
    }
    let id = grant
        .cloud
        .as_ref()
        .map_or(reservation.name.as_str(), |cloud| {
            cloud.qualified_id.as_str()
        });
    match lookup(verified.path(), id).await? {
        Lookup::Missing if !grant.creation_pending => {
            store.complete_deletion(consent)?;
        }
        Lookup::Missing | Lookup::Uncertain => {
            store.report_failure(consent, Failure::CreationUncertain)?;
        }
        Lookup::Found(actual) => {
            let cloud = cloud_matches(reservation, grant.cloud.as_ref(), &actual)?;
            if grant.creation_pending {
                store.record_created(consent, cloud.clone())?;
            }
            require_account(verified.path(), &reservation.account).await?;
            super::super::cli(
                verified.path(),
                &["delete", &cloud.qualified_id, "--force", "--json"],
                "profile_host.delete_tunnel",
            )
            .await?;
            store.complete_deletion(consent)?;
        }
    }
    status(installation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_host_not_found_requires_exact_pinned_cli_code_cluster_id_and_response() {
        let id = "festerm-123.usw3";
        assert!(exact_not_found(
            Some(2),
            b"",
            b"Tunnel not found in usw3: festerm-123\r\n",
            id
        ));
        for (code, message) in [
            (Some(1), "Tunnel not found in usw3: festerm-123"),
            (Some(2), "Tunnel not found in usw2: festerm-123"),
            (Some(2), "Tunnel not found in usw3: festerm-other"),
            (Some(2), "Authentication required"),
            (None, "Tunnel not found in usw3: festerm-123"),
        ] {
            assert!(!exact_not_found(code, b"", message.as_bytes(), id));
        }
        assert!(!exact_not_found(
            Some(2),
            b"unexpected",
            b"Tunnel not found in usw3: festerm-123",
            id
        ));
    }

    #[test]
    fn profile_host_cloud_identity_requires_full_id_label_and_available_incarnation() {
        let reservation = CloudReservation {
            account: "123".into(),
            name: "festerm-123".into(),
            ownership_label: "fixture-ownership".into(),
        };
        let expected = CloudIdentity {
            qualified_id: "festerm-123.usw3".into(),
            incarnation: Some("creation-one".into()),
        };
        let mut actual = CloudSnapshot {
            tunnel_id: expected.qualified_id.clone(),
            labels: vec![reservation.ownership_label.clone()],
            created: expected.incarnation.clone(),
        };
        assert!(cloud_matches(&reservation, Some(&expected), &actual).is_ok());
        actual.tunnel_id = "festerm-123.usw2".into();
        assert!(cloud_matches(&reservation, Some(&expected), &actual).is_err());
        actual.tunnel_id = expected.qualified_id.clone();
        actual.labels.clear();
        assert!(cloud_matches(&reservation, Some(&expected), &actual).is_err());
        actual.labels.push(reservation.ownership_label.clone());
        actual.created = Some("different-incarnation".into());
        assert!(cloud_matches(&reservation, Some(&expected), &actual).is_err());
        actual.created = None;
        assert!(cloud_matches(&reservation, Some(&expected), &actual).is_err());
    }
}
