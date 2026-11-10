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
        sharing: None,
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
        work.draft.manual_bundle = true;
        if signed_files_missing {
            work.draft.account = "42".into();
        }
        assert!(matches!(
            execute(work),
            Err(error) if error == if signed_files_missing {
                Failure::SignedBundle
            } else {
                Failure::SignIn
            }
        ));
        assert!(
            !state.exists(),
            "prerequisites must precede even local initialization"
        );
    }
}

#[test]
fn desktop_remote_access_automatic_owned_bytes_reach_only_existing_native_boundary() {
    let root = storage::private_test_directory();
    let state = root.path().join("desktop");
    let mut work = request(&state, Operation::Enable, Scope::Absent);
    work.draft.account = "42".into();
    work.draft.manifest = "ignored untrusted manual path".into();
    let bytes = vec![31, 41, 59];
    let report = Native
        .execute_with(
            work,
            &CancellationToken::new(),
            mpsc::sync_channel(1).0,
            |request, _| {
                assert!(!request.draft.manual_bundle);
                assert!(!state.exists());
                Ok(bytes)
            },
            |owned_bytes, cache| {
                assert_eq!(owned_bytes, [31, 41, 59]);
                assert_eq!(cache, state.join("signed-release-cache"));
                assert!(!state.join("native-service.json").exists());
                assert!(!state.join("owner-enrollment.json").exists());
                assert!(!state.join("host-cloud.json").exists());
                // Controlled publisher refusal: no native manager or real signing.
                Err(Failure::PlatformPrerequisite)
            },
        )
        .unwrap_or_else(|_| panic!("authenticated boundary failure retains actual local state"));
    assert_eq!(report.failure, Some(Failure::PlatformPrerequisite));
    assert!(matches!(report.scope, Scope::Present(snapshot) if snapshot.phase == Phase::Disabled));
    assert!(!report.owner_approved);
}

#[test]
fn desktop_remote_access_acquisition_404_and_failures_have_no_effect_or_fallback() {
    use festerm_host_setup::{acquisition::host_bundle::BundleAsset, artifacts::ArtifactError};
    for (error, expected) in [
        (
            HostBundleError::NotPublished(BundleAsset::Manifest),
            Failure::BundleNotPublished,
        ),
        (
            HostBundleError::NotPublished(BundleAsset::Signature),
            Failure::BundleNotPublished,
        ),
        (
            HostBundleError::NotPublished(BundleAsset::Archive),
            Failure::BundleNotPublished,
        ),
        (HostBundleError::Deadline, Failure::AcquisitionUnavailable),
        (
            HostBundleError::Redirect(BundleAsset::Archive),
            Failure::AcquisitionUnavailable,
        ),
        (
            HostBundleError::Status(BundleAsset::Archive, 401),
            Failure::AcquisitionUnavailable,
        ),
        (
            HostBundleError::Verification(ArtifactError::Signature),
            Failure::SignedBundle,
        ),
        (
            HostBundleError::UnsupportedTarget,
            Failure::PlatformPrerequisite,
        ),
        (HostBundleError::Cancelled, Failure::Cancelled),
    ] {
        let root = storage::private_test_directory();
        let state = root.path().join("desktop");
        let mut work = request(&state, Operation::Enable, Scope::Absent);
        work.draft.account = "42".into();
        let result = Native.execute_with(
            work,
            &CancellationToken::new(),
            mpsc::sync_channel(1).0,
            |_, _| Err::<(), _>(acquisition_failure(error)),
            |_, _| panic!("failed acquisition cannot publish or select another source"),
        );
        assert!(matches!(result, Err(error) if error == expected));
        assert!(!state.exists());
        assert!(!expected.message().contains("https://"));
    }
}

#[test]
fn desktop_remote_access_cancellation_during_sync_verification_never_accepts_owned_bytes() {
    let root = storage::private_test_directory();
    let state = root.path().join("desktop");
    let mut work = request(&state, Operation::Enable, Scope::Absent);
    work.draft.account = "42".into();
    let result = Native.execute_with(
        work,
        &CancellationToken::new(),
        mpsc::sync_channel(1).0,
        |_, cancel| {
            cancel.cancel();
            Ok(vec![1, 2, 3])
        },
        |_, _| panic!("late verification is not acceptance"),
    );
    assert!(matches!(result, Err(Failure::Cancelled)));
    assert!(!state.exists());
}

#[test]
fn desktop_remote_access_acquisition_rechecks_epoch_and_same_epoch_settings_before_effects() {
    for revoke in [false, true] {
        let root = storage::private_test_directory();
        let state = root.path().join("desktop");
        let store = Store::create(&state, Origin::Desktop, Draft::default().settings()).unwrap();
        let before = store.snapshot().unwrap();
        let mut work = request(
            &state,
            Operation::Enable,
            Scope::Present(before.clone().into()),
        );
        work.draft.account = "42".into();
        let result = Native.execute_with(
            work,
            &CancellationToken::new(),
            mpsc::sync_channel(1).0,
            |_, _| {
                if revoke {
                    store.revoke().unwrap();
                } else {
                    let mut settings = before.settings.clone();
                    settings.friendly_name = "Different consent".into();
                    store.configure(settings).unwrap();
                    assert_eq!(store.snapshot().unwrap().epoch, before.epoch);
                }
                Ok(())
            },
            |_, _| panic!("changed consent cannot publish"),
        );
        assert!(matches!(result, Err(Failure::ScopeChanged)));
        assert!(!state.join("signed-release-cache").exists());
        assert!(!state.join("native-service.json").exists());
        assert!(!state.join("owner-enrollment.json").exists());
        assert!(!state.join("host-cloud.json").exists());
    }
}

#[test]
fn desktop_remote_access_acquisition_cannot_adopt_new_foreign_or_replaced_installation() {
    for origin in [Origin::Desktop, Origin::Headless] {
        let root = storage::private_test_directory();
        let state = root.path().join("desktop");
        let mut work = request(&state, Operation::Enable, Scope::Absent);
        work.draft.account = "42".into();
        let result = Native.execute_with(
            work,
            &CancellationToken::new(),
            mpsc::sync_channel(1).0,
            |_, _| {
                Store::create(&state, origin, Draft::default().settings()).unwrap();
                Ok(())
            },
            |_, _| panic!("a new installation requires independent refreshed consent"),
        );
        assert!(matches!(
            result,
            Err(Failure::ScopeChanged | Failure::PrivateStorage)
        ));
        let store = Store::open(&state, origin).unwrap();
        assert_eq!(store.snapshot().unwrap().phase, Phase::Disabled);
        assert!(!state.join("signed-release-cache").exists());
    }
}

#[test]
fn desktop_remote_access_manual_route_is_explicit_and_never_opens_paths_by_default() {
    let root = storage::private_test_directory();
    let state = root.path().join("desktop");
    let mut work = request(&state, Operation::Enable, Scope::Absent);
    work.draft.account = "42".into();
    work.draft.manual_bundle = true;
    work.draft.manifest = "relative".into();
    assert!(matches!(execute(work), Err(Failure::SignedBundle)));
    assert!(!state.exists());
    assert!(!Draft::default().manual_bundle);
}

#[test]
fn desktop_remote_access_enable_requires_setup_phase_before_acquisition_or_publication() {
    let root = storage::private_test_directory();
    let state = root.path().join("desktop");
    let store = Store::create(&state, Origin::Desktop, Draft::default().settings()).unwrap();
    store.request_enable(&Draft::default().settings()).unwrap();
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.phase, Phase::EnableRequested);
    let mut work = request(
        &state,
        Operation::Enable,
        Scope::Present(snapshot.clone().into()),
    );
    work.draft.account = "42".into();
    let result = Native.execute_with(
        work,
        &CancellationToken::new(),
        mpsc::sync_channel(1).0,
        |_, _| -> Result<(), Failure> { panic!("reconcile the original operation first") },
        |_, _| panic!("no native publication"),
    );
    assert!(matches!(result, Err(Failure::Reconciliation)));
    assert_eq!(store.snapshot().unwrap(), snapshot);
    assert!(!state.join("signed-release-cache").exists());
}
