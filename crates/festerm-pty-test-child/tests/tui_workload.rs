use festerm_test_support::tui_workload::Workload;
use std::process::Command;

#[test]
fn tui_producer_emits_exact_shared_frames_and_a_complete_report() {
    for workload in Workload::ALL {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("report.json");
        let output = Command::new(env!("CARGO_BIN_EXE_festerm-pty-test-child"))
            .arg(format!("tui:{}:3:1:{}", workload.name(), report.display()))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut expected = workload.setup();
        for frame in 1..=3 {
            expected.extend(workload.update(frame));
        }
        assert_eq!(output.stdout, expected);
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
        assert_eq!(report["workload"], workload.name());
        assert_eq!(report["frames"], 3);
        assert_eq!(report["bytes"], expected.len());
        let completed = report["completed_ms"].as_array().unwrap();
        assert_eq!(completed.len(), 3);
        for (index, time) in completed.iter().enumerate() {
            assert!(time.as_f64().unwrap() >= (index + 1) as f64);
        }
    }
}

#[test]
fn tui_producer_rejects_invalid_workloads_and_budgets() {
    for parameters in [
        "missing:1:1",
        "quiet:0:1",
        "quiet:6001:1",
        "quiet:1:0",
        "quiet:1:1001",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("report.json");
        let output = Command::new(env!("CARGO_BIN_EXE_festerm-pty-test-child"))
            .arg(format!("tui:{parameters}:{}", report.display()))
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!report.exists());
    }
}

#[test]
fn tui_producer_waits_for_the_start_marker() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("start");
    let report = directory.path().join("report.json");
    let mut child = Command::new(env!("CARGO_BIN_EXE_festerm-pty-test-child"))
        .arg(format!("wait-for-file:{}", marker.display()))
        .arg(format!("tui:quiet:1:1:{}", report.display()))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let blocked = child.try_wait().unwrap().is_none() && !report.exists();
    std::fs::write(marker, "").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(blocked);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, Workload::Quiet.setup());
    assert!(report.exists());
}
