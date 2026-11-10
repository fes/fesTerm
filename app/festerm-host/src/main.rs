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
setup | run | update | uninstall
    Unavailable until the new host endpoint/authenticated package adapter is integrated.

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
        "status" | "service-status" | "disable" | "cleanup-local" | "recover-cleanup" => {
            &["--state-dir"]
        }
        "stage-cli" => &["--source-dir", "--cache-dir"],
        "acquire-cli" => &["--cache-dir"],
        "stage-package" | "qualify-package" => {
            &["--manifest", "--signature", "--archive", "--cache-root"]
        }
        "setup" | "run" | "update" | "uninstall" => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "host.endpoint_adapter_unavailable: no setup/service/update/removal performed; \
                 new host endpoint, foreground secure sign-in, exact-owned service adapter \
                 and authenticated fesTerm artifacts required (#681)",
            ));
        }
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
            assert_eq!(
                execute(arguments(&[command])).unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
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
