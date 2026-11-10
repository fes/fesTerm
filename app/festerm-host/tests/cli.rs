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
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
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
