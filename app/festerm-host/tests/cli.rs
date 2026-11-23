use std::process::Command;

#[test]
fn host_file_capabilities_binary_is_static_and_has_no_state_directory_effects() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .arg("capabilities")
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        output.stdout,
        format!(
            "{{\"schema\":1,\"host_files_v2\":{}}}\n",
            cfg!(feature = "owner-routing")
        )
        .as_bytes()
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    let state = root.path().join("must-not-create");
    let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .args(["capabilities", "--state-dir"])
        .arg(&state)
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!state.exists());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn host_delete_capabilities_probe_is_separate_static_and_refuses_extra_arguments() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .args(["capabilities", "files-delete"])
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        output.stdout,
        format!(
            "{{\"schema\":1,\"host_files_delete_v3\":{}}}\n",
            cfg!(feature = "owner-routing")
        )
        .as_bytes()
    );
    let state = root.path().join("must-not-create");
    let refused = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .args(["capabilities", "files-delete", "--state-dir"])
        .arg(&state)
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert!(!state.exists());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[cfg(feature = "owner-routing")]
#[test]
fn host_process_recovery_managed_entries_fail_closed_without_registration_and_status_is_history() {
    use festerm_host_setup::lifecycle::{Origin, Settings, Startup, Store};
    let root = festerm_sessiond::private_storage::private_test_directory();
    for origin in [Origin::Headless, Origin::Desktop] {
        let state = root.path().join(format!("{origin:?}"));
        let store = Store::create(
            &state,
            origin,
            Settings {
                filesystem: None,
                ordinary_access: Default::default(),
                friendly_name: "managed refusal fixture".into(),
                allow_creation: false,
                allow_session_termination: false,
                startup: Startup::Manual,
            },
        )
        .unwrap();
        let before = store.snapshot().unwrap();
        let (run, status) = if origin == Origin::Desktop {
            ("run-desktop-managed", "run-desktop-status")
        } else {
            ("run-managed", "run-status")
        };
        let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
            .args([run, "--state-dir"])
            .arg(&state)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("invalid_arguments"));
        let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
            .args([status, "--state-dir"])
            .arg(&state)
            .output()
            .unwrap();
        assert!(output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(report["managed_result"].is_null());
        assert!(report["last_run"].is_null());
        assert_eq!(report["meaning"], "history-only-not-liveness-or-ready");
        assert_eq!(store.snapshot().unwrap(), before);
    }
}

#[cfg(feature = "owner-routing")]
#[test]
fn desktop_native_binary_entry_refuses_headless_origin_and_never_grants_from_metadata() {
    use festerm_host_setup::lifecycle::{Origin, Settings, Startup, Store};
    let root = festerm_sessiond::private_storage::private_test_directory();
    for origin in [Origin::Desktop, Origin::Headless] {
        let state = root.path().join(format!("{origin:?}"));
        let store = Store::create(
            &state,
            origin,
            Settings {
                filesystem: None,
                ordinary_access: Default::default(),
                friendly_name: "Synthetic desktop runner".into(),
                allow_creation: false,
                allow_session_termination: false,
                startup: Startup::Manual,
            },
        )
        .unwrap();
        let before = store.snapshot().unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
            .args(["run-desktop", "--state-dir"])
            .arg(&state)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(store.snapshot().unwrap(), before);
        assert_eq!(std::fs::read_dir(&state).unwrap().count(), 2);
        let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
            .args(["run-desktop", "--state-dir"])
            .arg(&state)
            .args(["--owner-storage", "headless-files"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr)
            .unwrap()
            .contains("invalid_arguments"));
        assert_eq!(store.snapshot().unwrap(), before);
    }
}

#[cfg(feature = "owner-routing")]
#[test]
fn headless_update_is_explicit_stopped_signed_transaction_and_status_is_not_readiness() {
    use festerm_host_setup::{
        lifecycle::{Origin, Store},
        services::Services,
    };

    let root = festerm_sessiond::private_storage::private_test_directory();
    let state = root.path().join("state");
    let binary = env!("CARGO_BIN_EXE_festerm-host");
    assert!(Command::new(binary)
        .args(["configure", "--name", "Update", "--state-dir"])
        .arg(&state)
        .status()
        .unwrap()
        .success());
    let store = Store::open(&state, Origin::Headless).unwrap();
    let before = store.snapshot().unwrap();
    // Construction checks the real account and protected state, without manager calls.
    let admission = Services::current_user(&store, &state)
        .map(|_| ())
        .map_err(|error| error.kind());
    let output = Command::new(binary)
        .args(["update-status", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    let private_refusal = output.stderr
        == b"host.private_storage_required; use the configured non-system user and owner-only storage\n";
    match admission {
        Ok(()) => {
            assert!(
                output.status.success(),
                "admitted update-status: exit={:?}, private_storage_required={private_refusal}",
                output.status.code(),
            );
            assert!(output.stderr.is_empty());
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
                serde_json::Value::Null
            );
        }
        Err(kind) => {
            assert_eq!(kind, std::io::ErrorKind::PermissionDenied);
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            assert!(
                private_refusal,
                "denied update-status must retain its exact sanitized diagnostic"
            );
        }
    }
    assert_eq!(store.snapshot().unwrap(), before);
    assert_eq!(std::fs::read_dir(&state).unwrap().count(), 2);
    let output = Command::new(binary)
        .args(["update", "--state-dir"])
        .arg(&state)
        .args(["--consent", "replace", "--from-version", "0.15.0"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(!diagnostic.contains("endpoint_adapter_unavailable"));
    assert!(!state.join("native-service-update.json").exists());
    assert!(Command::new(binary)
        .args(["status", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap()
        .status
        .success());
}

#[test]
fn headless_binary_exits_unsuccessfully_without_endpoint_or_echoing_arguments() {
    for operation in ["setup", "run", "run-desktop", "update", "uninstall"] {
        let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
            .args([operation, "--token", "fixture-do-not-echo"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        let expected = if cfg!(feature = "owner-routing")
            && matches!(
                operation,
                "setup" | "run" | "run-desktop" | "uninstall" | "update"
            ) {
            "invalid_arguments"
        } else {
            "endpoint_adapter_unavailable"
        };
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("fixture-do-not-echo"));
    }
}

#[cfg(feature = "owner-routing")]
#[test]
fn headless_run_requires_current_consent_and_history_never_claims_liveness() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    let state = root.path().join("host");
    let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .args(["configure", "--name", "Fixture", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .args(["run", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(std::fs::read_dir(&state).unwrap().count(), 2);
    let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .args(["run-status", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["last_run"].is_null());
    assert_eq!(report["current_lifecycle"]["phase"], "disabled");
    assert_eq!(report["meaning"], "history-only-not-liveness-or-ready");
}

#[cfg(feature = "owner-routing")]
#[test]
fn headless_setup_and_owner_approval_require_explicit_foreground_confirmation() {
    for (command, args) in [
        ("setup", vec!["--account-id", "42"]),
        ("approve-owner", vec!["--public-key", "fixture-do-not-echo"]),
        ("uninstall", vec!["--state-dir", "fixture-do-not-echo"]),
        ("update", vec!["--from-version", "fixture-do-not-echo"]),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
            .arg(command)
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let diagnostic = String::from_utf8(output.stderr).unwrap();
        assert!(diagnostic.contains("invalid_arguments"));
        assert!(!diagnostic.contains("fixture-do-not-echo"));
    }
}

#[test]
fn headless_binary_status_is_a_journal_not_a_ready_claim() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    let state = root.path().join("headless");
    let configured = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .args(["configure", "--name", "Fixture", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(configured.status.success(), "{configured:?}");
    let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .args(["status", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["phase"], "disabled");
    assert_eq!(report["settings"]["startup"], "manual");
    assert_eq!(report["binding"], serde_json::Value::Null);
    assert_eq!(std::fs::read_dir(state).unwrap().count(), 2);
}

#[cfg(feature = "owner-routing")]
#[test]
fn headless_binary_cleanup_without_receipt_is_uncertain_not_removal_guidance() {
    use festerm_host_setup::lifecycle::{Binding, Origin, Settings, Startup, Store};

    let root = festerm_sessiond::private_storage::private_test_directory();
    let state = root.path().join("headless");
    let store = Store::create(
        &state,
        Origin::Headless,
        Settings {
            filesystem: None,
            ordinary_access: Default::default(),
            friendly_name: "Cleanup fixture".into(),
            allow_creation: false,
            allow_session_termination: false,
            startup: Startup::Manual,
        },
    )
    .unwrap();
    let ticket = store
        .request_enable(&store.snapshot().unwrap().settings)
        .unwrap();
    store
        .record_provisioned(
            &ticket,
            Binding {
                host_id: "22222222-2222-4222-8222-222222222222".into(),
                account: "github:42".into(),
                qualified_tunnel: "unverified-target.usw2".into(),
                cloud_incarnation: "2026-11-09T00:00:00Z".into(),
                host_key_fingerprint: "sha256:fixture".into(),
            },
        )
        .unwrap();
    store.revoke().unwrap();
    let stopped = store.snapshot().unwrap();
    store
        .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
        .unwrap();
    store.request_cleanup().unwrap();
    let before = store.snapshot().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .args(["status", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["phase"], "cleanup-pending");
    assert_eq!(report["cloud_cleanup"]["state"], "uncertain");
    assert!(report["cloud_cleanup"]["receipt_context"].is_null());
    assert!(report.get("binding").is_none());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("unverified-target"));
    assert!(report["cloud_cleanup"]["action"]
        .as_str()
        .unwrap()
        .contains("Do not remove"));
    assert_eq!(store.snapshot().unwrap(), before);
}

#[test]
fn headless_package_staging_refuses_unauthenticated_input_before_writing() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    let source = root.path().join("source");
    let directory = festerm_sessiond::private_storage::private_directory(&source, true).unwrap();
    let input = source.join("untrusted");
    let cache = root.path().join("cache");
    festerm_sessiond::private_storage::write_new_private(
        &directory,
        std::path::Path::new("untrusted"),
        b"not a signed package",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .arg("stage-package")
        .arg("--manifest")
        .arg(&input)
        .arg("--signature")
        .arg(&input)
        .arg("--archive")
        .arg(&input)
        .arg("--cache-root")
        .arg(&cache)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("artifact_authentication_failed"));
    assert!(!cache.exists());
}

#[test]
fn headless_acquisition_rejects_provenance_overrides_without_network_or_state() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    let cache = root.path().join("cache");
    for option in ["--url", "--sha256", "--key", "--token"] {
        let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
            .arg("acquire-cli")
            .arg("--cache-dir")
            .arg(&cache)
            .args([option, "fixture-do-not-echo"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("invalid_arguments"));
        assert!(!error.contains("fixture-do-not-echo"));
        assert!(!cache.exists());
    }
}

#[test]
fn headless_native_management_refuses_missing_receipts_without_ready_or_cleanup_claims() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    let state = root.path().join("headless");
    let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .args(["configure", "--name", "Fixture", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(output.status.success());
    for operation in ["service-status", "cleanup-local"] {
        let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
            .args([operation, "--state-dir"])
            .arg(&state)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Ready"));
        assert!(!state.join("native-service.json").exists());
    }
}

#[test]
fn headless_native_publication_rejects_untrusted_inputs_without_service_effects() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    let source = root.path().join("source");
    let directory = festerm_sessiond::private_storage::private_directory(&source, true).unwrap();
    festerm_sessiond::private_storage::write_new_private(
        &directory,
        std::path::Path::new("unsigned"),
        b"not authenticated",
    )
    .unwrap();
    let input = source.join("unsigned");
    let cache = root.path().join("cache");
    let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .arg("qualify-package")
        .arg("--manifest")
        .arg(&input)
        .arg("--signature")
        .arg(&input)
        .arg("--archive")
        .arg(&input)
        .arg("--cache-root")
        .arg(&cache)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!cache.exists());
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("artifact_authentication_failed"));
}

#[cfg(feature = "owner-routing")]
#[test]
fn headless_configure_cannot_erase_an_unreviewable_file_grant() {
    use festerm_host_setup::lifecycle::{OrdinaryAccess, Origin, Settings, Startup, Store};
    let root = festerm_sessiond::private_storage::private_test_directory();
    let state = root.path().join("files-settings");
    let store = Store::create(
        &state,
        Origin::Headless,
        Settings {
            friendly_name: "Files fixture".into(),
            allow_creation: false,
            allow_session_termination: false,
            startup: Startup::Manual,
            ordinary_access: OrdinaryAccess::ProviderOwner,
            filesystem: Some(festerm_tunnel_bridge::file_protocol::Policy {
                delete: false,
                root: root.path().to_path_buf(),
                write: false,
            }),
        },
    )
    .unwrap();
    let before = store.snapshot().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .args(["configure", "--name", "Changed", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot review existing file access"));
    assert_eq!(store.snapshot().unwrap(), before);
}

#[test]
fn host_owner_readiness_cli_is_local_status_only_and_never_creates_a_root() {
    let help = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("owner-readiness --state-dir PATH --origin desktop|headless"));
    assert!(help.contains("LOCAL owner-admission status only, using noninteractive reads"));
    assert!(help.contains("No mutation"));
    assert!(help.contains("no HOST/listener readiness claim"));
    let root = festerm_sessiond::private_storage::private_test_directory();
    let missing = root.path().join("private-canary-missing-state");
    for origin in ["desktop", "headless"] {
        let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
            .args(["owner-readiness", "--state-dir"])
            .arg(&missing)
            .args(["--origin", origin])
            .output()
            .unwrap();
        if cfg!(feature = "owner-routing") {
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["schema"], 1);
            assert_eq!(report["scope"], "local_owner_admission");
            assert_eq!(report["status_only"], true);
            assert_eq!(report["events"][1]["stage"], "store_open");
            assert_eq!(report["events"][1]["outcome"], "error");
            assert_eq!(report["events"][1]["category"], "missing");
            assert!(output.stdout.len() < 4096);
            assert!(!String::from_utf8_lossy(&output.stdout).contains("private-canary"));
        } else {
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
        }
        assert!(!missing.exists());
    }
    for args in [
        vec!["--origin", "desktop"],
        vec!["--state-dir", "relative", "--origin", "desktop"],
        vec!["--state-dir", "/unused", "--origin", "unknown"],
        vec!["--state-dir", "/unused"],
        vec![
            "--state-dir",
            "/unused",
            "--origin",
            "desktop",
            "--origin",
            "headless",
        ],
        vec![
            "--state-dir",
            "/unused",
            "--origin",
            "desktop",
            "--allow-ui",
            "true",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
            .arg("owner-readiness")
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[cfg(feature = "owner-routing")]
#[test]
fn host_owner_readiness_cli_refuses_wrong_origin_or_malformed_policy_without_writes() {
    use festerm_host_setup::lifecycle::{Origin, Settings, Startup, Store};
    let root = festerm_sessiond::private_storage::private_test_directory();
    let state = root.path().join("state");
    Store::create(
        &state,
        Origin::Desktop,
        Settings {
            friendly_name: "public-fixture".into(),
            allow_creation: false,
            allow_session_termination: false,
            ordinary_access: Default::default(),
            filesystem: None,
            startup: Startup::Manual,
        },
    )
    .unwrap();
    let before = std::fs::read(state.join("host-lifecycle.json")).unwrap();
    let count = std::fs::read_dir(&state).unwrap().count();
    let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .args(["owner-readiness", "--state-dir"])
        .arg(&state)
        .args(["--origin", "headless"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["events"][1]["stage"], "store_open");
    assert_eq!(report["events"][1]["outcome"], "error");
    assert_eq!(
        std::fs::read(state.join("host-lifecycle.json")).unwrap(),
        before
    );
    assert_eq!(std::fs::read_dir(&state).unwrap().count(), count);
    assert!(!state.join("worker.log").exists());
    assert!(!state.join("owner-enrollment-vault-ref").exists());
    // A malformed protected policy is rejected before constructing the native
    // vault adapter, so this actual-binary fixture never opens a real credential.
    let directory = festerm_sessiond::private_storage::private_directory(&state, false).unwrap();
    use std::io::Write;
    let mut policy = festerm_sessiond::private_storage::open_private_file(
        &directory,
        std::path::Path::new("owner-enrollment.json"),
        true,
    )
    .unwrap();
    policy
        .write_all(b"private-canary-malformed-policy")
        .unwrap();
    drop(policy);
    let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
        .args(["owner-readiness", "--state-dir"])
        .arg(&state)
        .args(["--origin", "desktop"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["events"][1]["category"], "malformed");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-canary"));
    assert_eq!(
        std::fs::read(state.join("owner-enrollment.json")).unwrap(),
        b"private-canary-malformed-policy"
    );
    assert_eq!(std::fs::read_dir(&state).unwrap().count(), count + 1);
    assert!(!state.join("worker.log").exists());
}
