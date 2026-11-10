use std::process::Command;

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
        assert!(error.contains("endpoint_adapter_unavailable"));
        assert!(!error.contains("fixture-do-not-echo"));
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
        let directory =
            festerm_sessiond::private_storage::private_directory(&source, true).unwrap();
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
        assert!(diagnostic.contains(if cfg!(windows) {
            "publisher_policy_unconfigured"
        } else {
            "artifact_authentication_failed"
        }));
    }
}
