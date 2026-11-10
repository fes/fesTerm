use festerm_host_setup::{
    lifecycle::{Origin, Settings, Startup, Store},
    staging,
};
use std::{collections::BTreeMap, ffi::OsString, io, path::PathBuf, process::ExitCode};

const HELP: &str = "\
festerm-host — ADR 0084 headless lifecycle boundary

configure --state-dir PATH --name NAME [--creation allow|deny] [--startup manual|sign-in]
    Save disabled host-local settings only; no sign-in, service or shell is started.
status --state-dir PATH
    Read the protected headless lifecycle journal as JSON. This is not a liveness probe.
stage-cli --source-dir PATH --cache-dir PATH
    Stage an already obtained official CLI against this release's reviewed pin.
stage-package --manifest PATH --signature PATH --archive PATH --cache-root PATH
    Verify signed helpers for this exact version/target and stage immutable bytes.
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
        "status" => &["--state-dir"],
        "stage-cli" => &["--source-dir", "--cache-dir"],
        "stage-package" => &["--manifest", "--signature", "--archive", "--cache-root"],
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
        "stage-cli" => {
            staging::stage_cli(&path("--source-dir")?, &path("--cache-dir")?)?;
            Ok("Official CLI pin verified and staged; nothing executed. No host configuration or service state changed.\n".into())
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
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
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
}
