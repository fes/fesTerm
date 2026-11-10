use festerm_host_setup::{
    acquisition,
    lifecycle::{Origin, Phase, Settings, Startup, Store},
    services::{Services, TrustedStage},
    staging,
};
use std::{collections::BTreeMap, ffi::OsString, io, path::PathBuf, process::ExitCode};

const HELP: &str = "\
festerm-host — ADR 0084 headless lifecycle boundary

configure --state-dir PATH --name NAME [--creation allow|deny] [--startup manual|sign-in]
    Save disabled host-local settings only; no sign-in, service or shell is started.
status --state-dir PATH
    Read protected lifecycle JSON; pending cleanup shows unverified receipt context only, never removal authority or liveness.
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
    Revoke and remove exact stopped native registration; verified present cloud resources show explicit manual provider review, remain CleanupPending and exit unsuccessfully. Retry verifies absence only; never deletes.
update --state-dir PATH --from-version VERSION --manifest PATH --signature PATH --archive PATH --cache-root PATH --consent replace
    Authenticated stopped-only forward replacement; retain identity/grants/daemons, never automatic enable or rollback.
update-status --state-dir PATH
    Durable old/new reservation history and recovery provenance, not liveness or Ready.

Manual startup requires an explicit run. Sign-in is user sign-in, NOT pre-login.
No native startup registration is performed by configure. Paths must be absolute,
private, symlink-free and belong to the configured non-system OS user.
";

fn invalid() -> io::Error {
    io::Error::other("host.invalid_arguments (use --help)")
}

#[cfg(feature = "owner-routing")]
#[derive(Debug)]
struct ManualCleanupRequired(festerm_host_setup::cloud::CleanupContext);

#[cfg(feature = "owner-routing")]
impl std::fmt::Display for ManualCleanupRequired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("host.native_removed_cloud_cleanup_pending")
    }
}

#[cfg(feature = "owner-routing")]
impl std::error::Error for ManualCleanupRequired {}

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
        #[cfg(feature = "owner-routing")]
        "update" => &[
            "--state-dir",
            "--from-version",
            "--manifest",
            "--signature",
            "--archive",
            "--cache-root",
            "--consent",
        ],
        #[cfg(feature = "owner-routing")]
        "update-status" => &["--state-dir"],
        #[cfg(not(feature = "owner-routing"))]
        "setup" | "run" | "uninstall" | "update" | "update-status" => return Err(unavailable()),
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
        "update-status" => {
            let state = path("--state-dir")?;
            let store = Store::open(&state, Origin::Headless)?;
            let report = Services::current_user(&store, &state)?.replacement_status()?;
            serde_json::to_string_pretty(&report)
                .map(|report| format!("{report}\n"))
                .map_err(|_| io::Error::other("host.status_serialization"))
        }
        #[cfg(feature = "owner-routing")]
        "update" => {
            if options.get("--consent").and_then(|value| value.to_str()) != Some("replace") {
                return Err(invalid());
            }
            let from = options
                .get("--from-version")
                .and_then(|value| value.to_str())
                .ok_or_else(invalid)?;
            let state = path("--state-dir")?;
            let store = Store::open(&state, Origin::Headless)?;
            if store.snapshot()?.phase != Phase::Disabled {
                return Err(io::Error::other("host.update_confirmed_stop_required"));
            }
            let stage = TrustedStage::publish_release_files(
                &path("--manifest")?,
                &path("--signature")?,
                &path("--archive")?,
                &path("--cache-root")?,
            )?;
            let report = festerm_host_setup::controller::update(&store, &state, &stage, from)?;
            serde_json::to_string_pretty(&report)
                .map(|report| format!("{report}\n"))
                .map_err(|_| io::Error::other("host.status_serialization"))
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
                festerm_host_setup::controller::UninstallResult::NativeRemovedCloudCleanupPending { context } =>
                    Err(io::Error::other(ManualCleanupRequired(context))),
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
            let store = Store::open(&path("--state-dir")?, Origin::Headless)?;
            status_report(&store)
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

#[cfg(not(feature = "owner-routing"))]
fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "host.endpoint_adapter_unavailable: required qualified adapter unavailable (#681)",
    )
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
            eprintln!("{}", render_diagnostic(&error));
            ExitCode::FAILURE
        }
    }
}

fn status_report(store: &Store) -> io::Result<String> {
    let snapshot = store.snapshot()?;
    let mut report = serde_json::to_value(&snapshot)
        .map_err(|_| io::Error::other("host.status_serialization"))?;
    if snapshot.phase == Phase::CleanupPending {
        let context = festerm_host_setup::cloud::CloudHostAdapter::new(store)
            .pending_cleanup_context(&snapshot)
            .ok();
        report.as_object_mut().unwrap().remove("binding");
        report["binding_withheld"] = true.into();
        report["cloud_cleanup"] = serde_json::json!({
            "state": if context.is_some() { "stored-receipt-unverified" } else { "uncertain" },
            "receipt_context": context,
            "action": "Do not remove resources from this journal alone. Preserve the original protected receipt and account. Retry explicit uninstall --state-dir PATH --consent remove using a compatible owner-routing-enabled host build to authenticate and verify current ownership or absence; no sign-in or provider removal is started by status.",
        });
    }
    serde_json::to_string_pretty(&report)
        .map(|report| format!("{report}\n"))
        .map_err(|_| io::Error::other("host.status_serialization"))
}

fn render_diagnostic(error: &io::Error) -> String {
    #[cfg(feature = "owner-routing")]
    if let Some(pending) = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<ManualCleanupRequired>())
    {
        return format!(
            "host.native_removed_cloud_cleanup_pending; exact native registration removed; cloud CleanupPending, NOT Removed.\nReceipt-bound context verified during this foreground attempt: {}\nExplicit manual action: independently open the provider's resource management interface using the displayed numeric account; review cluster, tunnel ID, ownership label and creation incarnation against this protected installation. Only if ALL still match, deliberately remove that exact resource there. Stop if anything changed or ownership is uncertain; this observation is not deletion authority. No browser or portal was opened; no provider deletion was performed by fesTerm.\nThen retry uninstall --state-dir PATH --consent remove with the original protected state and account. Only authenticated verified absence commits Removed; denial/offline/unknown remains CleanupPending.",
            serde_json::to_string(&pending.0).expect("public string metadata serializes"),
        );
    }
    diagnostic(error).into()
}

fn diagnostic(error: &io::Error) -> &'static str {
    if error.to_string() == "host.runtime_stop_pending" {
        return "host.runtime_stop_pending; access revoked but foreground host exit is not confirmed; preserve the exact receipt and retry";
    }
    match error.kind() {
        io::ErrorKind::Unsupported => return "host.endpoint_adapter_unavailable (#681); no operation performed",
        io::ErrorKind::PermissionDenied => return "host.private_storage_required; use the configured non-system user and owner-only storage",
        io::ErrorKind::WouldBlock => return "host.installation_busy; retry after the current operation finishes",
        io::ErrorKind::NotFound => return "host.required_file_missing; verify the explicit state/source directory",
        _ => {}
    }
    match error.to_string().as_str() {
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
            "host.cloud_offline_or_unknown; preserve the exact receipt and retry read-only reconciliation; no verified manual removal target"
        }
        "host.cloud_denied" => {
            "host.cloud_denied; authenticated authority refused, retain protected cleanup evidence"
        }
        "host.cloud_account_changed" | "host.cloud_identity_changed" => {
            "host.cloud_account_or_identity_changed; original account and installation authority required"
        }
        "host.cloud_generation_changed" | "host.cloud_binding_changed" => {
            "host.cloud_generation_changed; observed resource does not match the retained exact cleanup receipt; do not remove it"
        }
        "host.cloud_cleanup_scope_changed"
        | "host.cloud_cleanup_context_unavailable"
        | "host.cloud_scope_changed"
        | "host.cloud_journal_invalid"
        | "host.cloud_host_identity_changed"
        | "host.cloud_receipt_missing"
        | "host.cloud_not_configured" => {
            "host.cloud_cleanup_uncertain; no verified owned removal target; preserve original protected state and account, reconcile receipt/identity/current lifecycle before retrying; do not remove resources by name"
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
        "host.update_recovery_required"
        | "host.update_conflicting_recovery_intent"
        | "host.update_superseded_reconcile_owned_resources" =>
            "host.update_recovery_required; inspect update-status, preserve both signed bundles/receipts, retry the same explicit stopped update or revoke and perform exact-owned cleanup; no automatic enable or rollback",
        "host.update_confirmed_stop_required" =>
            "host.update_confirmed_stop_required; explicitly disable and confirm exact host/runtime stop before replacement; persistent daemons are not stop targets",
        "host.update_strictly_newer_current_release_required"
        | "host.update_stable_version_required" =>
            "host.update_strictly_newer_current_release_required; use the independently trusted newer release updater and exact signed --from-version; downgrade and inferred version are forbidden",
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
    fn cleanup_fixture() -> (
        festerm_sessiond::private_storage::PrivateTestDirectory,
        Store,
    ) {
        use festerm_sessiond::private_storage as storage;
        use std::path::Path;

        let root = storage::private_test_directory();
        let state = root.path().join("host");
        let store = Store::create(
            &state,
            Origin::Headless,
            Settings {
                friendly_name: "Cleanup fixture".into(),
                allow_creation: false,
                startup: Startup::Manual,
            },
        )
        .unwrap();
        let ticket = store
            .request_enable(&store.snapshot().unwrap().settings)
            .unwrap();
        festerm_host_setup::cloud::CloudHostAdapter::new(&store)
            .foreground(&ticket, 42)
            .unwrap();
        let directory = storage::private_directory(&state, false).unwrap();
        let mut record: serde_json::Value = serde_json::from_slice(
            &storage::read_private(&directory, Path::new("host-cloud.json"), 16384).unwrap(),
        )
        .unwrap();
        let resource = serde_json::json!({
            "tunnel_id": "fixture-owned",
            "cluster": "usw2",
            "account_id": 42,
            "ownership_label": "festerm-owned-22222222222242228222222222222222",
            "port": 2222,
            "labels": ["festerm-host-v1", "festerm-owned-22222222222242228222222222222222"],
        });
        record["resource"] = resource.clone();
        record["receipt"] = serde_json::json!({
            "resource": resource,
            "created": "2026-11-09T00:00:00Z",
        });
        record["authenticated"] = true.into();
        record["submitted"] = true.into();
        storage::replace_private(
            &directory,
            Path::new("host-cloud.json"),
            &serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        store
            .record_provisioned(
                &ticket,
                festerm_host_setup::lifecycle::Binding {
                    host_id: record["host_id"].as_str().unwrap().into(),
                    account: "github:42".into(),
                    qualified_tunnel: "fixture-owned.usw2".into(),
                    cloud_incarnation: "2026-11-09T00:00:00Z".into(),
                    host_key_fingerprint: record["fingerprint"].as_str().unwrap().into(),
                },
            )
            .unwrap();
        festerm_host_setup::controller::disable(&store, &state).unwrap();
        store.request_cleanup().unwrap();
        (root, store)
    }

    #[cfg(feature = "owner-routing")]
    #[test]
    fn headless_cleanup_status_is_read_only_unverified_receipt_context() {
        let (root, store) = cleanup_fixture();
        let path = root.path().join("host");
        let record = std::fs::read(path.join("host-cloud.json")).unwrap();
        let before = store.snapshot().unwrap();
        let report = execute(arguments(&[
            "status",
            "--state-dir",
            path.to_str().unwrap(),
        ]))
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&report).unwrap();
        assert_eq!(value["phase"], "cleanup-pending");
        assert_eq!(value["cloud_cleanup"]["state"], "stored-receipt-unverified");
        let context = &value["cloud_cleanup"]["receipt_context"];
        assert_eq!(context["account_id"], 42);
        assert_eq!(context["cluster"], "usw2");
        assert_eq!(context["tunnel_id"], "fixture-owned");
        assert_eq!(context["installation"], before.installation);
        assert_eq!(context["epoch"], before.epoch);
        assert!(value["cloud_cleanup"]["action"]
            .as_str()
            .unwrap()
            .contains("Do not remove"));
        assert!(value.get("binding").is_none());
        assert_eq!(value["binding_withheld"], true);
        assert!(!report.contains("Explicit manual action:"));
        assert!(!report.contains("credential"));
        assert!(!report.contains("https://"));
        assert_eq!(store.snapshot().unwrap(), before);
        assert_eq!(std::fs::read(path.join("host-cloud.json")).unwrap(), record);
    }

    #[cfg(feature = "owner-routing")]
    #[test]
    fn headless_cleanup_uninstall_diagnostics_are_typed_and_uncertainty_safe() {
        use festerm_sessiond::private_storage as storage;
        use std::path::Path;

        let (root, store) = cleanup_fixture();
        let context = festerm_host_setup::cloud::CloudHostAdapter::new(&store)
            .pending_cleanup_context(&store.snapshot().unwrap())
            .unwrap();
        let rendered = render_diagnostic(&io::Error::other(ManualCleanupRequired(context)));
        for expected in [
            "CleanupPending, NOT Removed",
            "Explicit manual action:",
            "\"account_id\":42",
            "\"cluster\":\"usw2\"",
            "\"tunnel_id\":\"fixture-owned\"",
            "\"incarnation\":\"2026-11-09T00:00:00Z\"",
            "Only if ALL still match",
            "Only authenticated verified absence commits Removed",
        ] {
            assert!(rendered.contains(expected), "{expected}");
        }
        assert!(!rendered.contains("https://"));
        for error in [
            "host.cloud_offline_or_unknown",
            "host.cloud_denied",
            "host.cloud_binding_changed",
            "host.cloud_cleanup_scope_changed",
            "host.cloud_receipt_missing",
            "host.native_removed_cloud_cleanup_pending https://private.invalid/?token=DO_NOT_ECHO",
        ] {
            let rendered = render_diagnostic(&io::Error::other(error));
            assert!(!rendered.contains("Explicit manual action:"));
            assert!(!rendered.contains("DO_NOT_ECHO"));
            assert!(!rendered.contains("https://"));
        }
        let state = root.path().join("host");
        let directory = storage::private_directory(&state, false).unwrap();
        let mut record: serde_json::Value = serde_json::from_slice(
            &storage::read_private(&directory, Path::new("host-cloud.json"), 16384).unwrap(),
        )
        .unwrap();
        record["receipt"]["created"] = "https://private.invalid/?token=DO_NOT_ECHO".into();
        storage::replace_private(
            &directory,
            Path::new("host-cloud.json"),
            &serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let report = status_report(&store).unwrap();
        let value: serde_json::Value = serde_json::from_str(&report).unwrap();
        assert_eq!(value["cloud_cleanup"]["state"], "uncertain");
        assert!(value["cloud_cleanup"]["receipt_context"].is_null());
        assert!(!report.contains("DO_NOT_ECHO"));
        assert!(!report.contains("https://"));
        assert!(!report.contains("Explicit manual action:"));
        assert!(value.get("binding").is_none());
        assert_eq!(store.snapshot().unwrap().phase, Phase::CleanupPending);
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
                && matches!(command, "setup" | "run" | "uninstall" | "update")
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
        ] {
            assert!(diagnostic(&io::Error::other(error)).contains(expected));
        }
        assert!(!diagnostic(&io::Error::other("PRIVATE_FIXTURE_SECRET"))
            .contains("PRIVATE_FIXTURE_SECRET"));
    }
}
