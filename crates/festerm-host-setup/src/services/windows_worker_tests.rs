//! Portable synthetic Windows inventory and fake Scheduler/publisher tests.
//! No Windows process, native publisher or signed no-console qualification.
use super::*;

#[test]
fn windows_desktop_worker_revision6_binds_both_roles_and_retains_raw_observation() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Desktop, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::windows_fixture(root.path(), crate::VERSION);
    let manager = services(root.path(), &store, Platform::Windows);
    manager
        .install_with_recovery(&stage, snapshot.epoch)
        .unwrap();
    let record = manager.registration().unwrap().1.unwrap();
    assert_eq!(record.version, 6);
    assert_eq!(record.executable, stage.path);
    assert_eq!(record.digest, stage.digest);
    let worker = record.windows_managed.as_ref().unwrap();
    assert_eq!(
        worker.path,
        stage.path.with_file_name("festerm-tunnel-worker.exe")
    );
    assert!(record
        .definition
        .contains(&format!("<Command>{}</Command>", worker.path.display())));
    assert!(record
        .definition
        .contains("run-desktop-managed --state-dir"));
    assert!(record
        .definition
        .contains("<RestartOnFailure><Interval>PT1M</Interval><Count>3</Count>"));
    stage.revalidate_registration(&record).unwrap();
    stage.require_running_worker(&record, &worker.path).unwrap();
    assert_eq!(
        stage
            .require_running_worker(&record, stage.path())
            .unwrap_err()
            .to_string(),
        "host.service_running_worker_mismatch"
    );
    assert!(
        manager.require_runtime_execution(&stage).is_err(),
        "console/test process is not the revision6 worker"
    );
    let bytes = std::fs::read(&manager.runner.receipt).unwrap();
    assert_eq!(manager.status().unwrap().process, ProcessState::Stopped);
    assert_eq!(std::fs::read(&manager.runner.receipt).unwrap(), bytes);
    assert_eq!(manager.registration().unwrap().1.unwrap(), record);
}

#[test]
fn windows_desktop_worker_role_refuses_cross_release_wrong_role_and_tamper() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Desktop, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::windows_fixture(root.path(), crate::VERSION);
    let foreign = trusted::windows_fixture(&root.path().join("other"), "0.15.0");
    let manager = services(root.path(), &store, Platform::Windows);
    manager
        .install_with_recovery(&stage, snapshot.epoch)
        .unwrap();
    let record = manager.registration().unwrap().1.unwrap();
    let calls = manager.runner.state.borrow().calls.len();
    for change in [
        "foreign-worker",
        "host-as-worker",
        "digest",
        "origin",
        "platform",
        "old-revision",
        "missing-role",
    ] {
        let mut changed = record.clone();
        match change {
            "foreign-worker" => {
                changed.windows_managed.as_mut().unwrap().path =
                    foreign.windows_managed_execution().unwrap().path
            }
            "host-as-worker" => changed.windows_managed.as_mut().unwrap().path = stage.path.clone(),
            "digest" => changed.windows_managed.as_mut().unwrap().digest = "0".repeat(64),
            "origin" => changed.origin = Origin::Headless,
            "platform" => changed.platform = Platform::Macos,
            "old-revision" => changed.version = 5,
            "missing-role" => changed.windows_managed = None,
            _ => unreachable!(),
        }
        let structural = manager.validate_registration(&changed, &snapshot);
        let authority = stage.revalidate_registration(&changed);
        assert!(structural.is_err() || authority.is_err(), "{change}");
    }
    let foreign_worker = foreign.windows_managed_execution().unwrap();
    assert!(stage
        .require_running_worker(&record, &foreign_worker.path)
        .is_err());
    std::fs::write(
        &record.windows_managed.as_ref().unwrap().path,
        b"tampered worker",
    )
    .unwrap();
    assert!(stage.revalidate_registration(&record).is_err());
    assert_eq!(manager.runner.state.borrow().calls.len(), calls);
}

#[test]
fn windows_worker_legacy_v2_to_v5_bytes_and_commands_do_not_upgrade_on_status() {
    for revision in 2..=5 {
        let root = storage::private_test_directory();
        let store = Store::create(&root.path().join("state"), Origin::Desktop, settings()).unwrap();
        let snapshot = provision(&store);
        let stage = trusted::windows_fixture(root.path(), crate::VERSION);
        let manager = services(root.path(), &store, Platform::Windows);
        manager.install_reserved(&stage, snapshot.epoch).unwrap();
        let mut record = manager.registration().unwrap().1.unwrap();
        record.version = revision;
        if matches!(revision, 3 | 5) {
            record.resource = definitions::replacement_resource(
                &snapshot,
                record.epoch,
                &record.reservation,
                Platform::Windows,
            )
            .unwrap();
        }
        record.definition = definitions::render(&record).unwrap();
        let raw = format!("normalized:{}", record.definition);
        record.native_expected = Some(raw.clone());
        record.native_snapshot = Some(raw.clone());
        manager.runner.state.borrow_mut().native_xml = Some(raw);
        manager
            .save(&record, snapshot.epoch, Phase::Provisioned)
            .unwrap();
        let before = std::fs::read(&manager.runner.receipt).unwrap();
        assert!(!String::from_utf8_lossy(&before).contains("windows_managed"));
        assert!(record
            .definition
            .contains(&format!("<Command>{}</Command>", stage.path.display())));
        assert!(!record.definition.contains("festerm-tunnel-worker.exe"));
        assert_eq!(
            definitions::record_runner_verb(&record),
            if revision >= 4 {
                "run-desktop-managed"
            } else {
                "run-desktop"
            }
        );
        assert!(
            manager.require_windows_worker_registration().is_err(),
            "strict worker may never execute legacy registration"
        );
        if revision >= 4 {
            assert!(
                manager.require_recovery_registration().unwrap().is_none(),
                "legacy console bootstrap remains accepted"
            );
        }
        assert_eq!(manager.status().unwrap().process, ProcessState::Stopped);
        assert_eq!(std::fs::read(&manager.runner.receipt).unwrap(), before);
    }
}

#[test]
fn windows_desktop_worker_pending_revision6_adopts_only_raw_snapshot_not_execution_role() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Desktop, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::windows_fixture(root.path(), crate::VERSION);
    let manager = services(root.path(), &store, Platform::Windows);
    manager
        .install_with_recovery(&stage, snapshot.epoch)
        .unwrap();
    let original = manager.registration().unwrap().1.unwrap();
    let mut pending = original.clone();
    pending.native_snapshot = None;
    manager
        .save(&pending, snapshot.epoch, Phase::Provisioned)
        .unwrap();
    assert_eq!(manager.status().unwrap().process, ProcessState::Stopped);
    assert_eq!(manager.registration().unwrap().1.unwrap(), original);
    assert!(manager
        .runner
        .state
        .borrow()
        .calls
        .iter()
        .all(|request| !matches!(
            request,
            NativeRequest::Task {
                action: TaskAction::Start | TaskAction::Stop,
                ..
            }
        )));
}

#[cfg(not(windows))]
#[test]
fn windows_worker_synthetic_foreign_tuple_is_not_public_native_launch_authority() {
    let root = storage::private_test_directory();
    let expected = crate::artifacts::ExpectedArtifact {
        version: crate::VERSION,
        platform: crate::artifacts::Platform::Windows,
        architecture: crate::artifacts::Architecture::Aarch64,
        package: crate::artifacts::Package::Host,
    };
    let artifact = crate::artifacts::host_acquisition_fixture(&expected);
    assert_eq!(
        TrustedStage::publish(&artifact, root.path())
            .err()
            .unwrap()
            .to_string(),
        "host.service_current_native_authenticated_bundle_required"
    );
}
