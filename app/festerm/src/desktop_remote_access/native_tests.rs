use super::*;
use festerm_sessiond::private_storage as storage;

fn request(root: &Path, operation: Operation, expected: Scope) -> Request {
    Request {
        operation,
        expected,
        root: root.into(),
        draft: Draft::default(),
        cancel_fence: false,
        owned: Arc::new(Mutex::new(None)),
        owner_revision: None,
    }
}

fn execute(request: Request) -> Result<Report, Failure> {
    Native.execute(request, &CancellationToken::new(), mpsc::sync_channel(1).0)
}

#[test]
fn desktop_remote_access_native_refuses_headless_corrupt_recordless_and_symlink_state() {
    let root = storage::private_test_directory();
    let headless = root.path().join("headless");
    let store = Store::create(&headless, Origin::Headless, Draft::default().settings()).unwrap();
    let original = store.snapshot().unwrap();
    assert!(matches!(
        execute(request(&headless, Operation::Refresh, Scope::Unobserved)),
        Err(Failure::PrivateStorage)
    ));
    assert_eq!(store.snapshot().unwrap(), original);
    let recordless = root.path().join("recordless");
    drop(storage::private_directory(&recordless, true).unwrap());
    assert!(matches!(
        execute(request(&recordless, Operation::Refresh, Scope::Unobserved)),
        Err(Failure::PrivateStorage)
    ));
    let desktop = root.path().join("desktop");
    let _store = Store::create(&desktop, Origin::Desktop, Draft::default().settings()).unwrap();
    std::fs::write(desktop.join("host-lifecycle.json"), b"{").unwrap();
    assert!(matches!(
        execute(request(&desktop, Operation::Refresh, Scope::Unobserved)),
        Err(Failure::PrivateStorage)
    ));
    assert_eq!(
        std::fs::read(desktop.join("host-lifecycle.json")).unwrap(),
        b"{"
    );
    #[cfg(unix)]
    {
        let healthy = root.path().join("healthy");
        let _healthy =
            Store::create(&healthy, Origin::Desktop, Draft::default().settings()).unwrap();
        let link = root.path().join("linked");
        std::os::unix::fs::symlink(&healthy, &link).unwrap();
        assert!(matches!(
            execute(request(&link, Operation::Refresh, Scope::Unobserved)),
            Err(Failure::PrivateStorage)
        ));
    }
}

#[test]
fn desktop_remote_access_native_stale_epoch_refuses_before_vault_manager_or_network() {
    let root = storage::private_test_directory();
    let path = root.path().join("desktop");
    let store = Store::create(&path, Origin::Desktop, Draft::default().settings()).unwrap();
    let mut stale = store.snapshot().unwrap();
    stale.epoch += 1;
    let original = store.snapshot().unwrap();
    for operation in [
        Operation::Enable,
        Operation::ApproveOwner,
        Operation::Start,
        Operation::Disable,
        Operation::Uninstall,
        Operation::InspectOwner,
        Operation::ObserveNative,
        Operation::Reconcile,
    ] {
        assert!(matches!(
            execute(request(
                &path,
                operation,
                Scope::Present(stale.clone().into())
            )),
            Err(Failure::ScopeChanged)
        ));
        assert_eq!(store.snapshot().unwrap(), original);
        assert!(!path.join("native-service.json").exists());
        assert!(!path.join("owner-enrollment.json").exists());
        assert!(!path.join("host-cloud.json").exists());
    }
}

#[test]
fn desktop_remote_access_native_cancel_scope_cannot_revoke_a_new_epoch() {
    let root = storage::private_test_directory();
    let path = root.path().join("desktop");
    let store = Store::create(&path, Origin::Desktop, Draft::default().settings()).unwrap();
    let original = store.snapshot().unwrap();
    let request = request(
        &path,
        Operation::Disable,
        Scope::Present(original.clone().into()),
    );
    *request.owned.lock().unwrap() = Some(original.clone());
    assert_eq!(cancellation_scope(&store, &request).unwrap(), original);
    store.revoke().unwrap();
    let current = store.snapshot().unwrap();
    assert_eq!(
        cancellation_scope(&store, &request),
        Err(Failure::ScopeChanged)
    );
    assert_eq!(store.snapshot().unwrap(), current);
}

#[test]
fn desktop_remote_access_native_error_ui_never_interpolates_credentials_or_urls() {
    for detail in [
        "owner.vault_locked_or_unavailable TOKEN=PRIVATE",
        "host.cloud_account_changed https://private.invalid/?token=PRIVATE",
        "host.cloud_cleanup_uncertain SECRET=PRIVATE",
    ] {
        let classified = failure(io::Error::other(detail));
        assert!(!classified.message().contains("PRIVATE"));
        assert!(!classified.message().contains("https://"));
    }
    assert_eq!(
        failure(io::Error::other("owner.vault_locked_or_unavailable")),
        Failure::NativeVault
    );
    assert_eq!(
        failure(io::Error::other(
            "host.service_windows_authenticated_graceful_stop_unavailable"
        )),
        Failure::PlatformPrerequisite
    );
}

#[test]
fn desktop_remote_access_native_prerequisite_failure_reports_actual_disabled_partial_state() {
    for signed_files_missing in [false, true] {
        let root = storage::private_test_directory();
        let state = root.path().join("desktop");
        let mut work = request(&state, Operation::Enable, Scope::Absent);
        if signed_files_missing {
            work.draft.account = "42".into();
        }
        let report = execute(work).unwrap_or_else(|_| panic!("local partial state is observable"));
        assert_eq!(
            report.failure,
            Some(if signed_files_missing {
                Failure::SignedBundle
            } else {
                Failure::SignIn
            })
        );
        assert!(
            matches!(report.scope, Scope::Present(snapshot) if snapshot.phase == Phase::Disabled)
        );
        assert!(!report.owner_approved);
        assert!(report.owner_binding.is_none());
        assert!(!state.join("native-service.json").exists());
        assert!(!state.join("owner-enrollment.json").exists());
        assert!(!state.join("host-cloud.json").exists());
    }
}
