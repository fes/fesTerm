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
