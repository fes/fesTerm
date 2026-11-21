use super::*;

fn fixture() -> (
    storage::PrivateTestDirectory,
    SessionControl,
    File,
    LogicalSessionId,
    Epoch,
) {
    let directory = storage::private_test_directory();
    let private = storage::private_directory(directory.path(), false).unwrap();
    let lock = storage::open_private_file(&private, Path::new("policy.lock"), true).unwrap();
    drop(lock);
    let control = SessionControl {
        root: directory.path().into(),
        helper: directory.path().join("never-executed-helper"),
    };
    let shared = LogicalSessionId::new();
    let revision = control
        .set_profile(
            shared.clone(),
            &OperationId::new().to_string(),
            "Lab shared",
            Recipe::default(),
            true,
        )
        .unwrap();
    control
        .set_profile(
            LogicalSessionId::new(),
            &OperationId::new().to_string(),
            "Lab private",
            Recipe::default(),
            false,
        )
        .unwrap();
    (directory, control, private, shared, revision)
}

fn exclusive_policy(directory: &File) -> Result<storage::UnlockOnDrop, ControlError> {
    let lock = storage::open_private_file(directory, Path::new("policy.lock"), false).unwrap();
    FileExt::try_lock_exclusive(&lock).map_err(|_| ControlError::Busy)?;
    Ok(storage::UnlockOnDrop(lock))
}

#[test]
fn profile_catalog_releases_initial_exclusive_policy_before_fresh_publication() {
    let (_directory, control, private, shared, revision) = fixture();
    let before = std::fs::read(control.root.join(STATE_FILE)).unwrap();
    let mut checks = 0;
    let profiles = control
        .profiles(&PrincipalId::new(), |state| {
            checks += 1;
            assert_eq!(state.effect, Effect::Lookup);
            // The journal must remain serialized across both policy checks.
            assert!(matches!(control.transaction(), Err(ControlError::Busy)));
            exclusive_policy(&private)
        })
        .expect("publication must reacquire the real exclusive policy lease");
    assert_eq!(checks, 2, "publication still needs fresh authorization");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].logical, shared);
    assert_eq!(profiles[0].revision, revision);
    assert_eq!(profiles[0].name, "Lab shared");
    assert_eq!(
        std::fs::read(control.root.join(STATE_FILE)).unwrap(),
        before
    );
    exclusive_policy(&private).unwrap();
}

#[test]
fn profile_catalog_rejects_changed_policy_at_publication_without_leaking_catalog() {
    let (_directory, control, private, _, _) = fixture();
    let before = std::fs::read(control.root.join(STATE_FILE)).unwrap();
    let mut checks = 0;
    let result = control.profiles(&PrincipalId::new(), |_| {
        checks += 1;
        let guard = exclusive_policy(&private)?;
        if checks == 2 {
            return Err(ControlError::Denied);
        }
        Ok(guard)
    });
    assert_eq!(result, Err(ControlError::Denied));
    assert_eq!(checks, 2);
    assert_eq!(
        std::fs::read(control.root.join(STATE_FILE)).unwrap(),
        before
    );
    exclusive_policy(&private).unwrap();
}

#[test]
fn profile_status_does_not_create_missing_host_root_or_empty_catalog() {
    let directory = storage::private_test_directory();
    let logical = LogicalSessionId::new();
    let consent = OperationId::new().to_string();
    let root = directory.path().join("host").join("daemon");
    let control = SessionControl::retained_namespace(&root).unwrap();
    assert_eq!(control.profile_status(&logical, &consent), Ok(None));
    assert!(!directory.path().join("host").exists());
    drop(storage::private_directory(&directory.path().join("host"), true).unwrap());
    drop(storage::private_directory(&root, true).unwrap());
    assert_eq!(control.profile_status(&logical, &consent), Ok(None));
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    // Explicit catalog writes retain their ordinary initialization path.
    let revision = control
        .set_profile(
            logical.clone(),
            &consent,
            "Remote",
            Recipe::default(),
            false,
        )
        .unwrap();
    assert_eq!(
        control.profile_status(&logical, &consent),
        Ok(Some((revision, false)))
    );
}

#[test]
fn profile_status_refuses_half_journals_and_orphan_namespace_without_repair() {
    for name in [
        "host-sessions.lock",
        STATE_FILE,
        "host-binding-namespace.json",
    ] {
        let directory = storage::private_test_directory();
        let private = storage::private_directory(directory.path(), false).unwrap();
        let mut file = storage::open_private_file(&private, Path::new(name), true).unwrap();
        use std::io::Write;
        file.write_all(b"preserved-evidence").unwrap();
        drop(file);
        let control = SessionControl::retained_namespace(directory.path()).unwrap();
        assert_eq!(
            control.profile_status(&LogicalSessionId::new(), &OperationId::new().to_string()),
            Err(ControlError::Storage),
            "{name}"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        assert_eq!(
            std::fs::read(directory.path().join(name)).unwrap(),
            b"preserved-evidence"
        );
    }
}

#[test]
fn profile_status_preserves_consent_validation_contention_and_journal_bytes() {
    let (directory, control, _private, logical, revision) = fixture();
    let consent = control
        .transaction()
        .unwrap()
        .state
        .profiles
        .get(&logical)
        .unwrap()
        .consent
        .clone();
    let before = std::fs::read(directory.path().join(STATE_FILE)).unwrap();
    assert_eq!(
        control.profile_status(&logical, &consent),
        Ok(Some((revision, true)))
    );
    assert_eq!(
        control.profile_status(&logical, &OperationId::new().to_string()),
        Err(ControlError::Denied)
    );
    let lease = control.transaction().unwrap();
    assert_eq!(
        control.profile_status(&logical, &consent),
        Err(ControlError::Busy)
    );
    drop(lease);
    assert_eq!(
        std::fs::read(directory.path().join(STATE_FILE)).unwrap(),
        before
    );
    std::fs::write(directory.path().join(STATE_FILE), b"invalid").unwrap();
    assert_eq!(
        control.profile_status(&logical, &consent),
        Err(ControlError::Storage)
    );
    assert_eq!(
        std::fs::read(directory.path().join(STATE_FILE)).unwrap(),
        b"invalid"
    );
}

#[cfg(unix)]
#[test]
fn profile_status_refuses_symlink_catalog_without_touching_target() {
    let directory = storage::private_test_directory();
    let target = storage::private_test_directory();
    let root = directory.path().join("daemon");
    std::os::unix::fs::symlink(target.path(), &root).unwrap();
    let control = SessionControl::retained_namespace(&root).unwrap();
    assert_eq!(
        control.profile_status(&LogicalSessionId::new(), &OperationId::new().to_string()),
        Err(ControlError::Storage)
    );
    assert!(root.is_symlink());
    assert_eq!(std::fs::read_dir(target.path()).unwrap().count(), 0);
}
