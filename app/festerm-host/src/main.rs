use festerm_host_setup::{
    acquisition,
    lifecycle::{Origin, Phase, Settings, Startup, Store},
    owned_migration,
    services::{Services, TrustedStage},
    staging,
};
use std::{collections::BTreeMap, ffi::OsString, io, path::PathBuf, process::ExitCode};

const HELP: &str = "\
festerm-host — ADR 0084 headless lifecycle boundary

configure --state-dir PATH --name NAME [--creation allow|deny] [--startup manual|sign-in]
    Save disabled host-local settings only; no sign-in, service or shell is started.
status --state-dir PATH
    Read the protected headless lifecycle journal as JSON. This is not a liveness probe.
service-status --state-dir PATH
    Observe only the exact receipt-owned native process; Running is not Ready.
disable --state-dir PATH
    Durably revoke access before exact native stop; retain uncertain/cloud cleanup.
cleanup-local --state-dir PATH
    Remove exact stopped native resources only after lifecycle cleanup was requested.
recover-cleanup --state-dir PATH
    Read-only adoption of acknowledged uncertain cloud creation; never enables or creates.
stage-cli --source-dir PATH --cache-dir PATH
    Stage an already obtained official CLI against this release's reviewed pin.
acquire-cli --cache-dir PATH
    Download this release's pinned official CLI into private non-executable staging only.
stage-package --manifest PATH --signature PATH --archive PATH --cache-root PATH
    Verify the separate signed host package for this exact version/target and stage immutable bytes.
qualify-package --manifest PATH --signature PATH --archive PATH --cache-root PATH
    Verify native release publisher and publish private executable bytes; never starts a service.
setup --state-dir PATH --manifest PATH --signature PATH --archive PATH --cache-root PATH --account-id ID --consent enable
    Foreground signed-package/account consent; register zero shells and report public owner-approval context.
approve-owner --state-dir PATH --public-key HEX --fingerprint FINGERPRINT --consent approve [--creation allow|deny] [--end allow|deny] [--temporary allow|deny]
    Explicit host-local approval of the reviewed client key. Never imports trust from a recipe.
run --state-dir PATH
    Explicit authenticated host listener; existing native credentials and owner approval required.
run-status --state-dir PATH
    Durable last-run history, not a liveness or endpoint-ready claim.
uninstall --state-dir PATH --consent remove
    Revoke, observe exact host stop and remove owned native registration; unqualified cloud deletion remains CleanupPending and exits unsuccessfully.
update
    Unavailable until the authenticated stopped-version replacement transaction is integrated.
migration-inventory --selection PATH --account-id ID
    Inventory ONLY explicitly selected protected legacy installations; no account discovery.
migration-preview --state-dir PATH --selection PATH --account-id ID
    Review exact source/account/key/generation mapping into a fresh disabled destination.
migration-reserve --state-dir PATH --selection PATH --account-id ID --preview-id SHA256 --consent reserve
    Journal identity/claims only. Authenticated live daemon binding is unavailable; legacy use is unchanged.
migration-status --state-dir PATH
    Protected reservation/rollback history, not migration completion or source liveness.
migration-resume --state-dir PATH
    Revalidate/resume the SAME reservation, never create a new host or shell.
migration-defer --state-dir PATH
    Retain the reservation and leave legacy use unchanged.
migration-rollback --state-dir PATH --consent rollback
    Release only this operation's local claims. Never restores revoked grants or deletes resources.
migration-owner-init --state-dir PATH --operation UUID --account-id ID --owner-storage native-vault|headless-protected-file --consent initialize-owner
    Initialize a zero-grant owner for the exact reserved host; no service or cloud operation.
migration-owner-approve --state-dir PATH --operation UUID --public-key HEX --fingerprint FINGERPRINT --owner-storage native-vault|headless-protected-file --consent approve
    Separate explicit owner/key approval: inventory/attach/detach only, no create/end/temporary permission.
migration-bind --state-dir PATH --owner-storage native-vault|headless-protected-file --consent bind
    Authenticate selected current daemons and prepare private bindings without acquiring a frontend.
migration-switch --state-dir PATH --owner-storage native-vault|headless-protected-file --consent switch
    Publish only exact authenticated bindings; ordinary host enable remains separately consented.
migration-binding-status --state-dir PATH
    Protected per-generation binding/switch/release history, not endpoint readiness.

Manual startup requires an explicit run. Sign-in is user sign-in, NOT pre-login.
No native startup registration is performed by configure. Paths must be absolute,
private, symlink-free and belong to the configured non-system OS user.
";

fn invalid() -> io::Error {
    io::Error::other("host.invalid_arguments (use --help)")
}

fn execute(args: impl IntoIterator<Item = OsString>) -> io::Result<String> {
    let mut args = args.into_iter();
    let command = args.next().ok_or_else(invalid)?;
    if command == "--help" || command == "-h" {
        if args.next().is_some() {
            return Err(invalid());
        }
        return Ok(HELP.into());
    }
    let command = command.to_str().ok_or_else(invalid)?;
    let allowed: &[&str] = match command {
        "configure" => &["--state-dir", "--name", "--creation", "--startup"],
        "migration-inventory" => &["--selection", "--account-id"],
        "migration-preview" => &["--state-dir", "--selection", "--account-id"],
        "migration-reserve" => &[
            "--state-dir",
            "--selection",
            "--account-id",
            "--preview-id",
            "--consent",
        ],
        "migration-status" | "migration-defer" => &["--state-dir"],
        "migration-resume" => &["--state-dir", "--owner-storage"],
        "migration-rollback" => &["--state-dir", "--consent"],
        #[cfg(feature = "owner-routing")]
        "migration-owner-init" => &[
            "--state-dir",
            "--operation",
            "--account-id",
            "--owner-storage",
            "--consent",
        ],
        #[cfg(feature = "owner-routing")]
        "migration-owner-approve" => &[
            "--state-dir",
            "--operation",
            "--public-key",
            "--fingerprint",
            "--owner-storage",
            "--consent",
        ],
        #[cfg(feature = "owner-routing")]
        "migration-bind" | "migration-switch" => &["--state-dir", "--owner-storage", "--consent"],
        #[cfg(feature = "owner-routing")]
        "migration-binding-status" => &["--state-dir"],
        #[cfg(not(feature = "owner-routing"))]
        "migration-owner-init"
        | "migration-owner-approve"
        | "migration-bind"
        | "migration-switch"
        | "migration-binding-status" => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "host.migration_authenticated_binding_unavailable",
            ));
        }
        "status" | "service-status" | "disable" | "cleanup-local" | "recover-cleanup" => {
            &["--state-dir"]
        }
        "stage-cli" => &["--source-dir", "--cache-dir"],
        "acquire-cli" => &["--cache-dir"],
        "stage-package" | "qualify-package" => {
            &["--manifest", "--signature", "--archive", "--cache-root"]
        }
        #[cfg(feature = "owner-routing")]
        "setup" => &[
            "--state-dir",
            "--manifest",
            "--signature",
            "--archive",
            "--cache-root",
            "--account-id",
            "--consent",
        ],
        #[cfg(feature = "owner-routing")]
        "approve-owner" => &[
            "--state-dir",
            "--public-key",
            "--fingerprint",
            "--consent",
            "--creation",
            "--end",
            "--temporary",
        ],
        #[cfg(feature = "owner-routing")]
        "run" | "run-status" => &["--state-dir"],
        #[cfg(feature = "owner-routing")]
        "uninstall" => &["--state-dir", "--consent"],
        #[cfg(not(feature = "owner-routing"))]
        "setup" | "run" | "uninstall" => return Err(unavailable()),
        "update" => return Err(unavailable()),
        _ => return Err(invalid()),
    };
    let mut options = BTreeMap::new();
    while let Some(key) = args.next() {
        let key = key.into_string().map_err(|_| invalid())?;
        if !allowed.contains(&key.as_str()) || options.contains_key(&key) {
            return Err(invalid());
        }
        let value = args.next().ok_or_else(invalid)?;
        options.insert(key, value);
    }
    let path = |key: &str| -> io::Result<PathBuf> {
        let path = PathBuf::from(options.get(key).ok_or_else(invalid)?);
        if !path.is_absolute() {
            return Err(invalid());
        }
        Ok(path)
    };
    match command {
        #[cfg(feature = "owner-routing")]
        "migration-owner-init"
        | "migration-owner-approve"
        | "migration-bind"
        | "migration-switch"
        | "migration-binding-status" => {
            let state = path("--state-dir")?;
            let store = Store::open(&state, Origin::Headless)?;
            if command == "migration-binding-status" {
                return serde_json::to_string(&owned_migration::binding_status(&store)?)
                    .map_err(|_| invalid());
            }
            let storage = migration_storage(&options)?;
            let consent = options.get("--consent").and_then(|value| value.to_str());
            match command {
                "migration-owner-init" => {
                    if consent != Some("initialize-owner") {
                        return Err(invalid());
                    }
                    let operation = options
                        .get("--operation")
                        .and_then(|value| value.to_str())
                        .ok_or_else(invalid)?;
                    let account = options
                        .get("--account-id")
                        .and_then(|value| value.to_str())
                        .ok_or_else(invalid)?;
                    serde_json::to_string(
                        &store
                            .owner_enrollment_with_storage(storage)?
                            .initialize_for_migration(operation, account.into())?,
                    )
                    .map_err(|_| invalid())
                }
                "migration-owner-approve" => {
                    if consent != Some("approve") {
                        return Err(invalid());
                    }
                    let operation = options
                        .get("--operation")
                        .and_then(|value| value.to_str())
                        .ok_or_else(invalid)?;
                    let public = options
                        .get("--public-key")
                        .and_then(|value| value.to_str())
                        .ok_or_else(invalid)?;
                    if public.len() != 64 || !public.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        return Err(invalid());
                    }
                    let mut key = [0; 32];
                    for (index, byte) in key.iter_mut().enumerate() {
                        *byte = u8::from_str_radix(&public[index * 2..index * 2 + 2], 16)
                            .map_err(|_| invalid())?;
                    }
                    let fingerprint = options
                        .get("--fingerprint")
                        .and_then(|value| value.to_str())
                        .ok_or_else(invalid)?;
                    serde_json::to_string(&owned_migration::approve_migration_owner(
                        &store,
                        operation,
                        storage,
                        key,
                        fingerprint,
                    )?)
                    .map_err(|_| invalid())
                }
                "migration-bind" | "migration-switch" => {
                    let required = if command == "migration-bind" {
                        "bind"
                    } else {
                        "switch"
                    };
                    if consent != Some(required) {
                        return Err(invalid());
                    }
                    let inventory = owned_migration::Inventory::current_user()?;
                    let journal = if command == "migration-bind" {
                        inventory.bind_existing(&store, &state, storage)?
                    } else {
                        inventory.switch_existing(&store, &state, storage)?
                    };
                    serde_json::to_string(&journal).map_err(|_| invalid())
                }
                _ => Err(invalid()),
            }
        }
        "migration-inventory" | "migration-preview" | "migration-reserve" => {
            let account = options
                .get("--account-id")
                .and_then(|value| value.to_str())
                .ok_or_else(invalid)?;
            let selection_path = path("--selection")?;
            let values = owned_migration::read_selections(&selection_path)?;
            let inventory = owned_migration::Inventory::current_user()?;
            if command == "migration-inventory" {
                return serde_json::to_string(&inventory.inventory(&values, account)?)
                    .map_err(|_| invalid());
            }
            let state = path("--state-dir")?;
            let store = Store::open(&state, Origin::Headless)?;
            if command == "migration-preview" {
                return serde_json::to_string(&inventory.preview(&store, &values, account)?)
                    .map_err(|_| invalid());
            }
            if options.get("--consent").and_then(|value| value.to_str()) != Some("reserve") {
                return Err(invalid());
            }
            let approved = options
                .get("--preview-id")
                .and_then(|value| value.to_str())
                .ok_or_else(invalid)?;
            serde_json::to_string(&inventory.reserve(&store, &values, account, approved)?)
                .map_err(|_| invalid())
        }
        "migration-status" | "migration-resume" | "migration-defer" | "migration-rollback" => {
            let state = path("--state-dir")?;
            let store = Store::open(&state, Origin::Headless)?;
            let journal = if command == "migration-status" {
                owned_migration::status(&store)?
            } else {
                let inventory = owned_migration::Inventory::current_user()?;
                match command {
                    "migration-resume" => {
                        #[cfg(feature = "owner-routing")]
                        if owned_migration::has_bindings(&store)? {
                            if owned_migration::status(&store)?.phase
                                == owned_migration::Phase::RollbackPending
                            {
                                inventory.release_bindings(&store, &state)?;
                                return serde_json::to_string(&inventory.rollback(&store)?)
                                    .map_err(|_| invalid());
                            }
                            let storage = migration_storage(&options)?;
                            if owned_migration::binding_status(&store)?.switch_requested {
                                inventory.switch_existing(&store, &state, storage)?;
                            } else {
                                inventory.bind_existing(&store, &state, storage)?;
                            }
                            return serde_json::to_string(&owned_migration::status(&store)?)
                                .map_err(|_| invalid());
                        }
                        inventory.resume(&store)?
                    }
                    "migration-defer" => inventory.defer(&store)?,
                    "migration-rollback" => {
                        if options.get("--consent").and_then(|value| value.to_str())
                            != Some("rollback")
                        {
                            return Err(invalid());
                        }
                        #[cfg(feature = "owner-routing")]
                        if owned_migration::has_bindings(&store)? {
                            inventory.release_bindings(&store, &state)?;
                        }
                        inventory.rollback(&store)?
                    }
                    _ => return Err(invalid()),
                }
            };
            serde_json::to_string(&journal).map_err(|_| invalid())
        }
        #[cfg(feature = "owner-routing")]
        "uninstall" => {
            if options.get("--consent").and_then(|value| value.to_str()) != Some("remove") {
                return Err(invalid());
            }
            let state = path("--state-dir")?;
            let store = Store::open(&state, Origin::Headless)?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let cancel = tokio_util::sync::CancellationToken::new();
            match runtime.block_on(with_shutdown(
                festerm_host_setup::controller::uninstall(&store, &state, &cancel),
                cancel.clone(),
            ))? {
                festerm_host_setup::controller::UninstallResult::Removed =>
                    Ok("Exact owned native registration and cloud endpoint absence confirmed. Revocation evidence and persistent sessions retained.\n".into()),
                festerm_host_setup::controller::UninstallResult::AlreadyRemovedNotReobserved =>
                    Ok("Recorded removed tombstone retained; resources were not reobserved. Persistent sessions unchanged.\n".into()),
                festerm_host_setup::controller::UninstallResult::NativeRemovedCloudCleanupPending =>
                    Err(io::Error::other("host.native_removed_cloud_cleanup_pending")),
            }
        }
        #[cfg(feature = "owner-routing")]
        "setup" => {
            if options.get("--consent").and_then(|value| value.to_str()) != Some("enable") {
                return Err(invalid());
            }
            let account = options
                .get("--account-id")
                .and_then(|value| value.to_str())
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value != 0)
                .ok_or_else(invalid)?;
            let state = path("--state-dir")?;
            let store = Store::open(&state, Origin::Headless)?;
            let stage = TrustedStage::publish_release_files(
                &path("--manifest")?,
                &path("--signature")?,
                &path("--archive")?,
                &path("--cache-root")?,
            )?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let cancel = tokio_util::sync::CancellationToken::new();
            let report = runtime.block_on(with_shutdown(
                festerm_host_setup::controller::setup(
                    &store, &state, &stage, account, &cancel,
                    |prompt| eprintln!("Approve this foreground sign-in at https://github.com/login/device with code {}", prompt.user_code),
                ),
                cancel.clone(),
            ))?;
            serde_json::to_string_pretty(&report)
                .map(|report| format!("{report}\n"))
                .map_err(|_| io::Error::other("host.status_serialization"))
        }
        #[cfg(feature = "owner-routing")]
        "approve-owner" => {
            if options.get("--consent").and_then(|value| value.to_str()) != Some("approve") {
                return Err(invalid());
            }
            let public = options
                .get("--public-key")
                .and_then(|value| value.to_str())
                .ok_or_else(invalid)?;
            if public.len() != 64 || !public.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(invalid());
            }
            let mut key = [0; 32];
            for (index, byte) in key.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&public[index * 2..index * 2 + 2], 16)
                    .map_err(|_| invalid())?;
            }
            let store = Store::open(&path("--state-dir")?, Origin::Headless)?;
            let report = festerm_host_setup::controller::approve_owner(
                &store,
                festerm_host_setup::controller::OwnerConsent {
                    owner_public_key: key,
                    confirmed_owner_fingerprint: options
                        .get("--fingerprint")
                        .and_then(|value| value.to_str())
                        .ok_or_else(invalid)?
                        .into(),
                    permissions: owner_permissions(&options)?,
                },
            )?;
            serde_json::to_string_pretty(&report)
                .map(|report| format!("{report}\n"))
                .map_err(|_| io::Error::other("host.status_serialization"))
        }
        #[cfg(feature = "owner-routing")]
        "run" => {
            let state = path("--state-dir")?;
            let store = std::sync::Arc::new(Store::open(&state, Origin::Headless)?);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let cancel = tokio_util::sync::CancellationToken::new();
            runtime.block_on(with_shutdown(
                festerm_host_setup::controller::run(store, &state, cancel.clone()),
                cancel,
            ))?;
            Ok("Host listener stopped. Persistent sessions were not terminated.\n".into())
        }
        #[cfg(feature = "owner-routing")]
        "run-status" => {
            let store = Store::open(&path("--state-dir")?, Origin::Headless)?;
            serde_json::to_string_pretty(&serde_json::json!({
                "last_run": festerm_host_setup::controller::last_run(&store)?,
                "current_lifecycle": store.snapshot()?,
                "meaning": "history-only-not-liveness-or-ready",
            }))
            .map(|report| format!("{report}\n"))
            .map_err(|_| io::Error::other("host.status_serialization"))
        }
        "configure" => {
            let name = options
                .get("--name")
                .and_then(|v| v.to_str())
                .ok_or_else(invalid)?;
            let choice = |key: &str| -> io::Result<Option<&str>> {
                options
                    .get(key)
                    .map(|v| v.to_str().ok_or_else(invalid))
                    .transpose()
            };
            let settings = Settings {
                friendly_name: name.into(),
                allow_creation: match choice("--creation")? {
                    Some("allow") => true,
                    Some("deny") | None => false,
                    _ => return Err(invalid()),
                },
                startup: match choice("--startup")? {
                    Some("sign-in") => Startup::UserSignIn,
                    Some("manual") | None => Startup::Manual,
                    _ => return Err(invalid()),
                },
            };
            settings.validate()?;
            let state = path("--state-dir")?;
            match Store::open(&state, Origin::Headless) {
                Ok(store) => store.configure(settings)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    Store::create(&state, Origin::Headless, settings)?;
                }
                Err(error) => return Err(error),
            }
            Ok(
                "Saved disabled settings. Remote access is off; no service or shell created.\n"
                    .into(),
            )
        }
        "status" => {
            let snapshot = Store::open(&path("--state-dir")?, Origin::Headless)?.snapshot()?;
            serde_json::to_string_pretty(&snapshot)
                .map(|s| format!("{s}\n"))
                .map_err(|_| io::Error::other("host.status_serialization"))
        }
        "service-status" | "disable" | "cleanup-local" => {
            let state = path("--state-dir")?;
            let store = Store::open(&state, Origin::Headless)?;
            #[cfg(feature = "owner-routing")]
            if command == "disable" {
                return serde_json::to_string_pretty(&festerm_host_setup::controller::disable(
                    &store, &state,
                )?)
                .map(|report| format!("{report}\n"))
                .map_err(|_| io::Error::other("host.status_serialization"));
            }
            if command == "disable" {
                match store.snapshot()?.phase {
                    Phase::Removed => return Err(io::Error::other("host.installation_removed")),
                    Phase::CleanupPending => {
                        return Err(io::Error::other("host.cleanup_already_pending"))
                    }
                    Phase::StopRequested => {}
                    _ => store.revoke()?,
                }
                let snapshot = store.snapshot()?;
                if snapshot.phase == Phase::Reconciling {
                    return Err(io::Error::other("host.cloud_reconciliation_required"));
                }
                if snapshot.binding.is_none() {
                    return Ok("Access disabled. No bound endpoint; retained cloud uncertainty still requires reconciliation.\n".into());
                }
            }
            let services = Services::current_user(&store, &state)?;
            let observed = match command {
                "service-status" => services.status()?,
                "disable" => services.stop(store.snapshot()?.epoch)?,
                "cleanup-local" => services.cleanup(store.snapshot()?.epoch)?,
                _ => unreachable!(),
            };
            serde_json::to_string_pretty(&serde_json::json!({
                "lifecycle": store.snapshot()?,
                "native_process": observed,
                "endpoint_readiness": "unavailable",
                "cloud_cleanup": "not-confirmed",
            }))
            .map(|report| format!("{report}\n"))
            .map_err(|_| io::Error::other("host.status_serialization"))
        }
        "recover-cleanup" => {
            let store = Store::open(&path("--state-dir")?, Origin::Headless)?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(
                festerm_host_setup::cloud::CloudHostAdapter::new(&store)
                    .adopt_uncertain_for_cleanup(&tokio_util::sync::CancellationToken::new()),
            )?;
            Ok("Exact cloud resource adopted for cleanup only. Access remains revoked; exact native stop and conditional cloud cleanup are still required.\n".into())
        }
        "stage-cli" => {
            staging::stage_cli(&path("--source-dir")?, &path("--cache-dir")?)?;
            Ok("Official CLI pin verified and staged; nothing executed. No host configuration or service state changed.\n".into())
        }
        "acquire-cli" => {
            acquisition::acquire_cli(&path("--cache-dir")?)?;
            Ok("Pinned official CLI acquired into private staging only. Nothing executed, installed or registered; native publisher qualification is still required.\n".into())
        }
        "stage-package" => {
            let _staged = staging::stage_release_files(
                &path("--manifest")?,
                &path("--signature")?,
                &path("--archive")?,
                &path("--cache-root")?,
            )?;
            Ok("Authenticated helper artifact staged. Nothing executed or registered; no shell created.\n".into())
        }
        "qualify-package" => {
            let _stage = TrustedStage::publish_release_files(
                &path("--manifest")?,
                &path("--signature")?,
                &path("--archive")?,
                &path("--cache-root")?,
            )?;
            Ok("Native release publisher and exact bytes qualified. Private executable published; no service, endpoint or shell started.\n".into())
        }
        _ => unreachable!(),
    }
}

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "host.endpoint_adapter_unavailable: required qualified adapter unavailable (#681)",
    )
}

#[cfg(feature = "owner-routing")]
fn migration_storage(
    options: &BTreeMap<String, OsString>,
) -> io::Result<festerm_host_setup::enrollment::OwnerStorage> {
    match options
        .get("--owner-storage")
        .and_then(|value| value.to_str())
    {
        Some("native-vault") => Ok(festerm_host_setup::enrollment::OwnerStorage::NativeVault),
        Some("headless-protected-file") => {
            Ok(festerm_host_setup::enrollment::OwnerStorage::HeadlessProtectedFile)
        }
        _ => Err(invalid()),
    }
}

#[cfg(feature = "owner-routing")]
fn owner_permissions(
    options: &BTreeMap<String, OsString>,
) -> io::Result<festerm_host_protocol::Capabilities> {
    let choice = |name| match options.get(name) {
        None => Ok(false),
        Some(value) => match value.to_str() {
            Some("allow") => Ok(true),
            Some("deny") => Ok(false),
            _ => Err(invalid()),
        },
    };
    Ok(festerm_host_protocol::Capabilities {
        inventory: true,
        attach: true,
        detach: true,
        ensure: true,
        create: true,
        creation_allowed: choice("--creation")?,
        explicit_end: choice("--end")?,
        temporary: choice("--temporary")?,
        ..Default::default()
    })
}

#[cfg(feature = "owner-routing")]
async fn with_shutdown<T>(
    work: impl std::future::Future<Output = io::Result<T>>,
    cancel: tokio_util::sync::CancellationToken,
) -> io::Result<T> {
    tokio::pin!(work);
    tokio::select! {
        result = &mut work => result,
        signal = shutdown_signal() => {
            signal?;
            cancel.cancel();
            work.await
        }
    }
}

#[cfg(feature = "owner-routing")]
async fn shutdown_signal() -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = term.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}

fn main() -> ExitCode {
    match execute(std::env::args_os().skip(1)) {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            // OS errors may contain paths or native details; never relay them.
            eprintln!("{}", diagnostic(&error));
            ExitCode::FAILURE
        }
    }
}

fn diagnostic(error: &io::Error) -> &'static str {
    if error.to_string() == "host.runtime_stop_pending" {
        return "host.runtime_stop_pending; access revoked but foreground host exit is not confirmed; preserve the exact receipt and retry";
    }
    if error.to_string() == "host.migration_authenticated_binding_unavailable" {
        return "host.migration_authenticated_binding_unavailable; defer or roll back the protected reservation; no daemon restart, attachment or exposure switch was performed";
    }
    match error.kind() {
        io::ErrorKind::Unsupported => return "host.endpoint_adapter_unavailable (#681); no operation performed",
        io::ErrorKind::PermissionDenied => return "host.private_storage_required; use the configured non-system user and owner-only storage",
        io::ErrorKind::WouldBlock => return "host.installation_busy; retry after the current operation finishes",
        io::ErrorKind::NotFound => return "host.required_file_missing; verify the explicit state/source directory",
        _ => {}
    }
    match error.to_string().as_str() {
        "host.migration_binding_uncertain_resume_same_operation" =>
            "host.migration_binding_uncertain_resume_same_operation; preserve the exact journal and legacy session; resume the same operation or request exact rollback, never recreate",
        "host.migration_binding_busy" =>
            "host.migration_binding_busy; protected source/native authority is busy; no queued retry or takeover",
        "host.migration_binding_revoked" =>
            "host.migration_binding_revoked; revoked/released binding cannot be restored; preserve legacy use and request exact rollback",
        "host.migration_binding_refused_preserve_legacy" | "host.migration_binding_stale" =>
            "host.migration_binding_refused_preserve_legacy; require exact current owner/source/namespace/generation authority; never attach, restart or recreate",
        "native.binding_older_helper_defer" | "native.binding_transport_unqualified" | "native.binding_current_daemon_already_managed" =>
            "host.migration_authenticated_binding_unavailable; selected helper does not permit this in-place import; defer and preserve strict legacy use",
        "native.binding_untrusted_stale_or_unsupported" =>
            "host.migration_native_binding_untrusted; refuse changed/corrupt native capability or proof; preserve exact evidence and legacy use",
        "host.migration_authenticated_binding_unavailable"
        | "host.migration_native_binding_required_or_rollback" =>
            "host.migration_authenticated_binding_unavailable; defer or roll back the protected reservation; no daemon restart, attachment or exposure switch was performed",
        "host.migration_preview_changed_reconfirm" =>
            "host.migration_preview_changed_reconfirm; review the exact current mapping again; old preview is not consent",
        "host.migration_source_not_reservable"
        | "host.migration_source_changed_preserve_legacy"
        | "host.migration_native_generation_replaced"
        | "host.migration_native_generation_unavailable" =>
            "host.migration_source_unavailable_or_changed; preserve legacy use and protected evidence; never resume-or-create or stop",
        "host.migration_source_account_key_or_identity_mismatch"
        | "host.migration_source_receipt_mismatch"
        | "host.migration_foreign_source_origin"
        | "host.migration_foreign_runtime_namespace" =>
            "host.migration_owned_source_mismatch; original protected installation/account/key/namespace required; no adoption",
        "host.migration_stale_destination_epoch_or_settings"
        | "host.migration_requires_fresh_disabled_destination" =>
            "host.migration_destination_changed; stale reservation cannot enable or create; preserve evidence and explicitly reconcile or roll back",
        "host.migration_source_already_reserved_or_revoked" =>
            "host.migration_source_already_reserved_or_revoked; no second destination or grant restoration",
        "host.sign_in_required"
        | "host.secure_store_unavailable"
        | "owner.vault_locked_or_unavailable_unlock_native_store_and_retry"
        | "owner.vault_anchor_missing_local_reconciliation_required" => {
            "host.sign_in_required; native credentials unavailable, unlock or reconcile the original secure store; no fallback"
        }
        "host.cloud_cleanup_adoption_unavailable"
        | "host.cleanup_adoption_scope_changed" => {
            "host.cleanup_adoption_scope_changed; preserve the receipt and reconcile current lifecycle intent"
        }
        "host.cloud_creation_outcome_uncertain"
        | "host.cloud_reconciliation_required" => {
            "host.cloud_reconciliation_required; access revoked, uncertain creation retained; no new create or cleanup proof"
        }
        "host.cloud_offline_or_unknown" => {
            "host.cloud_offline_or_unknown; preserve the exact receipt and retry read-only reconciliation"
        }
        "host.cloud_denied" => {
            "host.cloud_denied; authenticated authority refused, retain protected cleanup evidence"
        }
        "host.cloud_account_changed" | "host.cloud_identity_changed" => {
            "host.cloud_account_or_identity_changed; original account and installation authority required"
        }
        "host.cloud_generation_changed" | "host.cloud_binding_changed" => {
            "host.cloud_generation_changed; observed resource does not match the retained exact cleanup receipt"
        }
        "host.cloud_rate_limited" => {
            "host.cloud_rate_limited; preserve receipt and retry later, outcome not confirmed"
        }
        "host.cloud_cancelled" => "host.cloud_cancelled; retained evidence was not discarded",
        "host.linux_sign_in_session_fence_required" =>
            "host.linux_sign_in_session_fence_required; choose manual startup until current-logon activation is qualified; no linger or permission changes performed",
        "host.runtime_current_provisioned_consent_required" =>
            "host.runtime_current_provisioned_consent_required; complete explicit current setup before run",
        "host.runtime_current_owner_consent_required" =>
            "host.runtime_current_owner_consent_required; complete reviewed host-local owner key and permission approval",
        "host.runtime_lock_missing_reconciliation_required" =>
            "host.runtime_lock_missing_reconciliation_required; preserve runtime history and reconcile exact ownership; no restart or stopped claim",
        "host.service_signed_provenance_required" =>
            "host.service_signed_provenance_required; original signed release provenance required for runtime; old receipt remains cleanup-only",
        "host.native_removed_cloud_cleanup_pending" =>
            "host.native_removed_cloud_cleanup_pending; access revoked and owned native registration removed; cloud CleanupPending retained because generation-atomic deletion is unqualified; no DELETE sent",
        "host.service_not_installed"
        | "host.service_not_installed_no_native_observation" => {
            "host.service_not_installed_no_native_observation; no receipt is not proof of process absence"
        }
        "host.service_native_timeout" => {
            "host.service_native_timeout; outcome unknown, preserve receipt and retry exact-owned observation"
        }
        "host.service_native_spawn_failed"
        | "host.service_native_operation_failed"
        | "host.service_observation_unavailable"
        | "host.service_launchd_observation_unavailable"
        | "host.service_task_unavailable" => {
            "host.service_manager_unavailable; access remains revoked, native outcome not confirmed"
        }
        "host.service_stop_not_observed"
        | "host.service_stop_before_cleanup"
        | "host.service_cleanup_not_observed_absent" => {
            "host.service_stop_or_cleanup_unconfirmed; retain exact receipt, never terminate persistent children"
        }
        "host.service_foreign_native_definition"
        | "host.service_foreign_receipt"
        | "host.service_foreign_state_directory"
        | "host.service_invalid_receipt"
        | "host.service_removed_resource_reappeared" => {
            "host.service_ownership_refused; foreign or changed evidence, no destructive recovery attempted"
        }
        "host.service_windows_release_publisher_policy_unconfigured" => {
            "host.service_windows_release_publisher_policy_unconfigured; approved publisher, rotation and revocation policy required"
        }
        "host.service_windows_authenticated_graceful_stop_unavailable" => {
            "host.service_windows_authenticated_graceful_stop_unavailable; no destructive task or child-process termination attempted"
        }
        "host.service_macos_release_publisher_required"
        | "host.service_macos_release_publisher_and_runtime_required" => {
            "host.service_macos_release_publisher_required; require the approved Developer ID team and non-ad-hoc hardened runtime"
        }
        "host.service_stale_or_unapproved_operation"
        | "host.service_operation_superseded_reconcile_owned_resource" => {
            "host.service_operation_superseded; current epoch refused the operation, reconcile retained exact-owned receipt"
        }
        "host.cleanup_already_pending" => {
            "host.cleanup_already_pending; access remains revoked, continue exact-owned cleanup"
        }
        "host.installation_removed" => "host.installation_removed; tombstone retained",
        "host.invalid_arguments (use --help)" => "host.invalid_arguments (use --help)",
        "host.foreign_origin_or_version" => {
            "host.foreign_origin_or_version; explicit verified migration required, not adoption"
        }
        "host.invalid_journal" | "host.installation_exists_or_incomplete" => {
            "host.journal_reconciliation_required; existing evidence was not overwritten"
        }
        "host.recordless_directory_requires_manual_review" => {
            "host.recordless_directory_requires_manual_review; inspect the exact --state-dir before retrying; no directory was adopted"
        }
        "host.journal_lock_missing_reconciliation_required" => {
            "host.journal_lock_missing_reconciliation_required; preserve the existing record and reconcile ownership"
        }
        "host.settings_changed_reconfirm" => {
            "host.settings_changed_reconfirm; review and explicitly approve the current settings"
        }
        "host.disable_before_configuration" => {
            "host.disable_before_configuration; existing consent and settings were preserved"
        }
        "host.cli_pin_mismatch_dependency_update_required" => {
            "host.cli_pin_mismatch_dependency_update_required; nothing executed"
        }
        "host.cached_cli_mismatch" => {
            "host.cached_cli_mismatch; existing cache was not overwritten"
        }
        "profile_host.cli_platform" | "host.cli_target_unavailable" => {
            "host.cli_target_unavailable; no qualified pin for this native target, no fallback"
        }
        "host.cli_download_policy" => "host.cli_download_policy; a reviewed dependency pin update is required",
        "host.cli_download_failed" => "host.cli_download_failed; bounded acquisition failed, nothing published",
        "host.cli_download_status" => "host.cli_download_status; require HTTP 200 without redirects, nothing published",
        "host.cli_download_limit" => "host.cli_download_limit; response is empty or oversized, nothing published",
        "host.cli_download_encoding" => "host.cli_download_encoding; require raw pinned bytes, nothing published",
        "host.artifact_authentication_failed" => "host.artifact_authentication_failed; require signed exact-version/target release helpers, no staging performed",
        "host.staged_artifact_mismatch" => "host.staged_artifact_mismatch; existing cache was not overwritten",
        _ => "host.operation_refused; check arguments, private storage, origin and journal state",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[cfg(feature = "owner-routing")]
    #[test]
    fn headless_temporary_permission_is_explicit_and_never_grants_creation() {
        let mut options = BTreeMap::new();
        let default = owner_permissions(&options).unwrap();
        assert!(!default.temporary);
        assert!(!default.creation_allowed);
        options.insert("--temporary".into(), "allow".into());
        let approved = owner_permissions(&options).unwrap();
        assert!(approved.temporary);
        assert!(!approved.creation_allowed);
        assert_eq!(approved.create, default.create);
        assert_eq!(approved.ensure, default.ensure);
        assert_eq!(approved.explicit_end, default.explicit_end);
        options.insert("--temporary".into(), "deny".into());
        assert!(!owner_permissions(&options).unwrap().temporary);
        options.insert("--temporary".into(), "yes".into());
        assert!(owner_permissions(&options).is_err());
    }

    #[test]
    fn headless_commands_configure_and_report_only_disabled_state() {
        let root = festerm_sessiond::private_storage::private_test_directory();
        let path = root.path().join("host");
        let path = path.to_str().unwrap();
        assert!(execute(arguments(&["status", "--state-dir", path])).is_err());
        assert!(!std::path::Path::new(path).exists());
        execute(arguments(&[
            "configure",
            "--state-dir",
            path,
            "--name",
            "Fixture",
        ]))
        .unwrap();
        let report = execute(arguments(&["status", "--state-dir", path])).unwrap();
        let report: serde_json::Value = serde_json::from_str(&report).unwrap();
        assert_eq!(report["phase"], "disabled");
        assert_eq!(report["settings"]["allow_creation"], false);
        assert_eq!(report["binding"], serde_json::Value::Null);
        assert_eq!(std::fs::read_dir(path).unwrap().count(), 2);
    }

    #[test]
    fn unavailable_host_operations_and_unsafe_options_fail_closed() {
        for command in ["setup", "run", "update", "uninstall"] {
            let expected = if cfg!(feature = "owner-routing")
                && matches!(command, "setup" | "run" | "uninstall")
            {
                io::ErrorKind::Other
            } else {
                io::ErrorKind::Unsupported
            };
            assert_eq!(execute(arguments(&[command])).unwrap_err().kind(), expected);
        }
        for args in [
            vec![
                "acquire-cli",
                "--cache-dir",
                "/unused",
                "--url",
                "fixture-do-not-echo",
            ],
            vec![
                "acquire-cli",
                "--cache-dir",
                "/unused",
                "--sha256",
                "fixture-do-not-echo",
            ],
            vec!["acquire-cli", "--cache-dir", "relative"],
            vec!["configure", "--state-dir", "relative", "--name", "Fixture"],
            vec!["status", "--state-dir", "/unused", "--state-dir", "/other"],
            vec!["status", "--token", "do-not-read-secrets"],
            vec![
                "configure",
                "--state-dir",
                "/unused",
                "--name",
                "Fixture",
                "--startup",
                "pre-login",
            ],
        ] {
            assert!(execute(arguments(&args)).is_err());
        }
    }

    #[test]
    fn headless_disable_fences_unbound_intent_without_touching_native_services() {
        let root = festerm_sessiond::private_storage::private_test_directory();
        let path = root.path().join("host");
        let path = path.to_str().unwrap();
        execute(arguments(&[
            "configure",
            "--state-dir",
            path,
            "--name",
            "Fixture",
        ]))
        .unwrap();
        execute(arguments(&["disable", "--state-dir", path])).unwrap();
        let store = Store::open(std::path::Path::new(path), Origin::Headless).unwrap();
        let initial = store.snapshot().unwrap();
        assert_eq!(initial.phase, Phase::Disabled);
        assert_eq!(std::fs::read_dir(path).unwrap().count(), 2);
        store.request_enable(&initial.settings).unwrap();
        let error = execute(arguments(&["disable", "--state-dir", path])).unwrap_err();
        assert!(diagnostic(&error).contains("cloud_reconciliation_required"));
        let cancelled = store.snapshot().unwrap();
        assert_eq!(cancelled.phase, Phase::Reconciling);
        assert!(cancelled.epoch > initial.epoch);
        assert!(cancelled.pending_ticket().is_some());
        assert_eq!(std::fs::read_dir(path).unwrap().count(), 2);
    }

    #[test]
    fn headless_native_and_vault_diagnostics_are_specific_and_sanitized() {
        for (error, expected) in [
            (
                "host.service_windows_release_publisher_policy_unconfigured",
                "publisher",
            ),
            ("host.service_native_timeout", "outcome unknown"),
            (
                "owner.vault_anchor_missing_local_reconciliation_required",
                "sign_in_required",
            ),
            (
                "host.service_not_installed_no_native_observation",
                "not proof",
            ),
            (
                "host.service_windows_authenticated_graceful_stop_unavailable",
                "no destructive",
            ),
            (
                "host.migration_binding_uncertain_resume_same_operation",
                "never recreate",
            ),
            ("host.migration_binding_busy", "no queued retry"),
            ("host.migration_binding_revoked", "cannot be restored"),
            ("native.binding_current_daemon_already_managed", "defer"),
        ] {
            assert!(diagnostic(&io::Error::other(error)).contains(expected));
        }
        assert!(!diagnostic(&io::Error::other("PRIVATE_FIXTURE_SECRET"))
            .contains("PRIVATE_FIXTURE_SECRET"));
    }
    #[test]
    fn headless_migration_rejects_stop_takeover_and_implicit_reservation_consent() {
        for command in [
            "migration-inventory",
            "migration-preview",
            "migration-reserve",
            "migration-status",
            "migration-resume",
            "migration-defer",
            "migration-rollback",
            "migration-owner-init",
            "migration-owner-approve",
            "migration-bind",
            "migration-switch",
            "migration-binding-status",
        ] {
            for flag in ["--stop", "--restart", "--takeover", "--yes", "--token"] {
                assert!(execute(arguments(&[command, flag, "fixture-do-not-echo"])).is_err());
            }
        }
        let error = execute(arguments(&["migration-switch"])).unwrap_err();
        #[cfg(not(feature = "owner-routing"))]
        {
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert!(diagnostic(&error).contains("authenticated_binding_unavailable"));
        }
        #[cfg(feature = "owner-routing")]
        assert!(diagnostic(&error).contains("invalid_arguments"));
        assert!(!HELP.contains("migration-stop"));
    }
}
