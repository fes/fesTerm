use std::process::Command;

#[cfg(feature = "owner-routing")]
#[test]
fn headless_update_is_explicit_stopped_signed_transaction_and_status_is_not_readiness() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    let state = root.path().join("state");
    let binary = env!("CARGO_BIN_EXE_festerm-host");
    assert!(Command::new(binary)
        .args(["configure", "--name", "Update", "--state-dir"])
        .arg(&state)
        .status()
        .unwrap()
        .success());
    let output = Command::new(binary)
        .args(["update-status", "--state-dir"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::Value::Null
    );
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
    for operation in ["setup", "run", "update", "uninstall"] {
        let output = Command::new(env!("CARGO_BIN_EXE_festerm-host"))
            .args([operation, "--token", "fixture-do-not-echo"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        let expected = if cfg!(feature = "owner-routing")
            && matches!(operation, "setup" | "run" | "uninstall" | "update")
        {
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
            friendly_name: "Cleanup fixture".into(),
            allow_creation: false,
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
    festerm_host_setup::controller::disable(&store, &state).unwrap();
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
