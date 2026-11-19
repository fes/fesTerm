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
        release: None,
    }
}

fn execute(request: Request) -> Result<Report, Failure> {
    Native.execute(request, &CancellationToken::new(), mpsc::sync_channel(1).0)
}

#[test]
fn desktop_remote_access_native_refresh_never_promotes_legacy_or_enrolls_a_device() {
    for ordinary_access in [OrdinaryAccess::OwnerOnly, OrdinaryAccess::ProviderOwner] {
        let root = storage::private_test_directory();
        let path = root.path().join("desktop");
        let settings = Settings {
            ordinary_access,
            ..Draft::default().settings()
        };
        let store = Store::create(&path, Origin::Desktop, settings).unwrap();
        let before = store.snapshot().unwrap();
        let record = path.join("host-lifecycle.json");
        let original = std::fs::read(&record).unwrap();
        if ordinary_access == OrdinaryAccess::OwnerOnly {
            assert!(!std::str::from_utf8(&original)
                .unwrap()
                .contains("ordinary_access"));
        }
        let report = execute(request(&path, Operation::Refresh, Scope::Unobserved)).unwrap();
        assert_eq!(report.scope, Scope::Present(before.clone().into()));
        assert_eq!(store.snapshot().unwrap(), before);
        assert_eq!(std::fs::read(record).unwrap(), original);
        assert!(!report.owner_approved);
        assert!(report.owner_binding.is_none());
        assert!(!path.join("owner-enrollment.json").exists());
        assert!(!path.join("native-service.json").exists());
    }
}

fn sharing_request(root: &Path, step: ShareStep, expected: Scope) -> Request {
    let mut work = request(root, Operation::Share(step), expected);
    let target = ShareTarget {
        tab: crate::tabs::TabId::next_for_test(),
        profile: None,
        lifecycle_generation: 1,
        identity: festerm_sessiond::NativeSessionIdentity {
            name: "untrusted-metadata-only".into(),
            pid: 1,
            created_at_unix_ms: 1,
            endpoint: "never-opened".into(),
        },
        source: None,
    };
    work.sharing = Some(ShareTask {
        current: Arc::new(Mutex::new(vec![target.clone()])),
        target,
        operation: uuid::Uuid::new_v4().to_string(),
        epoch: Some((Arc::new(AtomicU64::new(0)), 0)),
    });
    work
}

#[test]
fn desktop_remote_access_sharing_native_original_parse_and_origin_refuse_without_namespace() {
    let root = storage::private_test_directory();
    let path = root.path().join("desktop");
    let store = Store::create(&path, Origin::Desktop, Draft::default().settings()).unwrap();
    let before = store.snapshot().unwrap();
    for step in [ShareStep::Resume, ShareStep::Release, ShareStep::Rollback] {
        let mut work = sharing_request(&path, step, Scope::Present(before.clone().into()));
        work.sharing.as_mut().unwrap().operation = "not-an-original-operation".into();
        assert!(matches!(execute(work), Err(Failure::InvalidDraft)));
        assert_eq!(store.snapshot().unwrap(), before);
        assert!(!path.join("daemon").exists());
        assert!(!path.join("owner-enrollment.json").exists());
    }
    let foreign = root.path().join("headless");
    let store = Store::create(&foreign, Origin::Headless, Draft::default().settings()).unwrap();
    let before = store.snapshot().unwrap();
    for step in [
        ShareStep::Bind,
        ShareStep::Switch,
        ShareStep::Resume,
        ShareStep::Release,
        ShareStep::Rollback,
    ] {
        assert!(matches!(
            execute(sharing_request(
                &foreign,
                step,
                Scope::Present(before.clone().into())
            )),
            Err(Failure::PrivateStorage)
        ));
        assert_eq!(store.snapshot().unwrap(), before);
        assert!(!foreign.join("daemon").exists());
    }
}

#[test]
fn desktop_remote_access_sharing_native_stale_cancel_and_account_refuse_before_authority() {
    let root = storage::private_test_directory();
    let path = root.path().join("desktop");
    let store = Store::create(&path, Origin::Desktop, Draft::default().settings()).unwrap();
    let ticket = store.request_enable(&Draft::default().settings()).unwrap();
    store
        .record_provisioned(
            &ticket,
            festerm_host_setup::lifecycle::Binding {
                host_id: "11111111-1111-4111-8111-111111111111".into(),
                account: "github:42".into(),
                qualified_tunnel: "synthetic-cluster/synthetic-tunnel".into(),
                cloud_incarnation: "synthetic-incarnation".into(),
                host_key_fingerprint: "synthetic-public-pin".into(),
            },
        )
        .unwrap();
    let before = store.snapshot().unwrap();
    let mut stale = before.clone();
    stale.epoch += 1;
    for step in [ShareStep::Bind, ShareStep::Switch] {
        assert!(matches!(
            execute(sharing_request(
                &path,
                step,
                Scope::Present(stale.clone().into())
            )),
            Err(Failure::ScopeChanged)
        ));
        let mut work = sharing_request(&path, step, Scope::Present(before.clone().into()));
        work.draft.account = "43".into();
        assert!(matches!(execute(work), Err(Failure::ScopeChanged)));
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            Native.execute(
                sharing_request(&path, step, Scope::Present(before.clone().into())),
                &cancel,
                mpsc::sync_channel(1).0,
            ),
            Err(Failure::Cancelled)
        ));
        assert_eq!(store.snapshot().unwrap(), before);
        for file in [
            "daemon",
            "owner-enrollment.json",
            "native-service.json",
            "host-cloud.json",
        ] {
            assert!(!path.join(file).exists());
        }
    }
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

#[test]
fn desktop_remote_access_native_preserves_only_typed_diagnostic_before_classification() {
    use festerm_host_setup::diagnostics::{attach, Category, Recovery, Stage};
    let expected = HostSetupDiagnostic {
        stage: Stage::Creation,
        http_status: Some(400),
        category: Category::Response,
        recovery: Recovery::Reconcile,
    };
    let error = attach(
        io::Error::other("host.cloud_response_refused private-token-path"),
        expected,
    );
    let runtime = runtime().unwrap();
    let mut diagnostic = None;
    let result: Result<(), Failure> = run_async(
        &runtime,
        &CancellationToken::new(),
        &mut diagnostic,
        async { Err(error) },
    );
    assert_eq!(result, Err(Failure::CloudUncertain));
    assert_eq!(diagnostic, Some(expected));
    assert!(!format!("{diagnostic:?}").contains("private-token-path"));
}

#[test]
fn desktop_remote_access_native_diagnostic_matches_new_owned_enable_epoch_not_replacement() {
    use festerm_host_setup::diagnostics::{Category, Recovery, Stage};
    let root = storage::private_test_directory();
    let path = root.path().join("desktop");
    let store = Store::create(&path, Origin::Desktop, Draft::default().settings()).unwrap();
    store.request_enable(&Draft::default().settings()).unwrap();
    let owned = store.snapshot().unwrap();
    let work = request(&path, Operation::Enable, Scope::Absent);
    let diagnostic = Some(HostSetupDiagnostic {
        stage: Stage::Creation,
        http_status: Some(400),
        category: Category::ProviderRejected,
        recovery: Recovery::Reconcile,
    });
    let scope = Scope::Present(owned.clone().into());
    assert!(scoped_diagnostic(diagnostic, &work, &scope).is_none());
    *work.owned.lock().unwrap() = Some(owned.clone());
    assert_eq!(scoped_diagnostic(diagnostic, &work, &scope), diagnostic);
    let mut changed = owned.clone();
    changed.epoch += 1;
    assert!(scoped_diagnostic(diagnostic, &work, &Scope::Present(changed.into())).is_none());
    let mut changed = owned.clone();
    changed.installation = uuid::Uuid::new_v4().to_string();
    assert!(scoped_diagnostic(diagnostic, &work, &Scope::Present(changed.into())).is_none());
    let mut changed = owned;
    changed.origin = Origin::Headless;
    assert!(scoped_diagnostic(diagnostic, &work, &Scope::Present(changed.into())).is_none());
    assert!(scoped_diagnostic(diagnostic, &work, &Scope::Absent).is_none());
}

#[test]
fn desktop_remote_access_runtime_history_requires_current_provisioned_epoch() {
    let root = storage::private_test_directory();
    let store = Store::create(
        &root.path().join("desktop"),
        Origin::Desktop,
        Draft::default().settings(),
    )
    .unwrap();
    // Disabled projection must not be blocked or relabelled by obsolete run history.
    std::fs::write(root.path().join("desktop/host-runtime.json"), b"{").unwrap();
    let disabled = report(Some(&store), &root.path().join("desktop")).unwrap();
    assert!(disabled.last_run.is_none());
    assert!(matches!(disabled.scope, Scope::Present(ref state) if state.phase == Phase::Disabled));
    let mut snapshot = store.snapshot().unwrap();
    snapshot.phase = Phase::Provisioned;
    let record = |installation: &str, epoch| {
        serde_json::from_value::<controller::RunRecord>(serde_json::json!({
            "version": 1, "installation": installation, "epoch": epoch,
            "run_id": "11111111-1111-4111-8111-111111111111", "state": "listening", "failure": null
        }))
        .unwrap()
    };
    assert_eq!(
        current_run_state(
            &snapshot,
            Some(record(&snapshot.installation, snapshot.epoch))
        ),
        Some(controller::RunState::Listening)
    );
    assert_eq!(current_run_state(&snapshot, None), None);
    assert_eq!(
        current_run_state(
            &snapshot,
            Some(record("another-installation", snapshot.epoch))
        ),
        None
    );
    assert_eq!(
        current_run_state(
            &snapshot,
            Some(record(&snapshot.installation, snapshot.epoch + 1))
        ),
        None
    );
    snapshot.epoch += 1;
    assert_eq!(
        current_run_state(
            &snapshot,
            Some(record(&snapshot.installation, snapshot.epoch - 1))
        ),
        None
    );
    snapshot.phase = Phase::Disabled;
    assert_eq!(
        current_run_state(
            &snapshot,
            Some(record(&snapshot.installation, snapshot.epoch))
        ),
        None
    );
}

#[test]
fn desktop_remote_access_update_refuses_active_unbound_foreign_and_cancelled_before_acquisition() {
    for origin in [Origin::Desktop, Origin::Headless] {
        for bound in [false, true] {
            let root = storage::private_test_directory();
            let state = root.path().join("host");
            let store = Store::create(&state, origin, Draft::default().settings()).unwrap();
            if bound {
                let ticket = store
                    .request_enable(&store.snapshot().unwrap().settings)
                    .unwrap();
                store
                    .record_provisioned(
                        &ticket,
                        festerm_host_setup::lifecycle::Binding {
                            host_id: "11111111-1111-4111-8111-111111111111".into(),
                            account: "github:42".into(),
                            qualified_tunnel: "cluster/tunnel".into(),
                            cloud_incarnation: "incarnation".into(),
                            host_key_fingerprint: "public-pin".into(),
                        },
                    )
                    .unwrap();
            }
            let scope = store.snapshot().unwrap();
            let mut work = request(
                &state,
                Operation::Update,
                Scope::Present(scope.clone().into()),
            );
            work.release = Some(festerm_host_setup::services::ReleaseMetadata {
                recorded_version: "0.0.1".into(),
                replacement: None,
            });
            let result = Native.execute_with(
                work,
                &CancellationToken::new(),
                mpsc::sync_channel(1).0,
                |_, _| -> Result<(), Failure> { panic!("blocked update cannot acquire") },
                |_, _| panic!("blocked update cannot publish"),
            );
            assert!(matches!(
                result,
                Err(Failure::UpdateBlocked | Failure::PrivateStorage)
            ));
            let cancel = CancellationToken::new();
            cancel.cancel();
            let cancelled = Native.execute_with(
                request(
                    &state,
                    Operation::Update,
                    Scope::Present(scope.clone().into()),
                ),
                &cancel,
                mpsc::sync_channel(1).0,
                |_, _| -> Result<(), Failure> { panic!("cancelled update cannot acquire") },
                |_, _| panic!("cancelled update cannot publish"),
            );
            assert!(matches!(cancelled, Err(Failure::Cancelled)));
            assert_eq!(store.snapshot().unwrap(), scope);
            assert!(!state.join("signed-release-cache").exists());
            assert!(!state.join("native-service.json").exists());
        }
    }
}

#[test]
fn desktop_remote_access_update_metadata_change_and_invalid_version_refuse_original_retarget() {
    use festerm_host_setup::services::ReleaseMetadata;
    let original = ReleaseMetadata {
        recorded_version: "0.0.1".into(),
        replacement: None,
    };
    assert_eq!(
        check_update_release(Some(&original), Some(&original)),
        Ok(())
    );
    assert_eq!(
        check_update_release(Some(&original), None),
        Err(Failure::UpdateBlocked)
    );
    for version in ["0.0.2", "99.0.0", "invalid"] {
        let changed = ReleaseMetadata {
            recorded_version: version.into(),
            replacement: None,
        };
        assert_eq!(
            check_update_release(Some(&original), Some(&changed)),
            Err(Failure::ScopeChanged)
        );
    }
    let current = ReleaseMetadata {
        recorded_version: festerm_host_setup::VERSION.into(),
        replacement: None,
    };
    assert_eq!(
        check_update_release(Some(&current), Some(&current)),
        Err(Failure::UpdateBlocked)
    );
}

#[test]
fn desktop_remote_access_native_bad_runtime_logs_never_fail_refresh_or_change_scope() {
    let root = storage::private_test_directory();
    let path = root.path().join("desktop");
    let store = Store::create(&path, Origin::Desktop, Draft::default().settings()).unwrap();
    let before = store.snapshot().unwrap();
    let directory = storage::private_directory(&path, false).unwrap();
    for bytes in [
        b"legacy worker.exit reason=old\n".to_vec(),
        vec![0xff; 128],
        vec![b'x'; 265_000],
    ] {
        storage::replace_private(&directory, Path::new("worker.log"), &bytes).unwrap();
        let report = execute(request(&path, Operation::Refresh, Scope::Unobserved)).unwrap();
        assert_eq!(report.scope, Scope::Present(before.clone().into()));
        assert!(report.runtime_history.is_empty());
        assert!(report.failure.is_none());
        assert!(report.process.is_none());
        assert_eq!(store.snapshot().unwrap(), before);
    }
}
