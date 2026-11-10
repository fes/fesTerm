use super::*;
use std::time::{Duration, Instant};

struct Fake<F>(F);

impl<F> Backend for Fake<F>
where
    F: Fn(Request, &CancellationToken) -> Result<Report, Failure> + Send + Sync,
{
    fn execute(
        &self,
        request: Request,
        cancel: &CancellationToken,
        _: SyncSender<Progress>,
    ) -> Result<Report, Failure> {
        (self.0)(request, cancel)
    }
}

fn fixture(backend: Arc<dyn Backend>) -> Controller {
    Controller::new(
        Some(PathBuf::from("unused-desktop-remote-access-fixture")),
        backend,
        Arc::new(AtomicU8::new(0)),
    )
}

fn drain(controller: &mut Controller, context: &egui::Context) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while controller.view.busy {
        controller.poll(context);
        assert!(
            Instant::now() < deadline,
            "bounded fixture worker did not drain"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn desktop_remote_access_metadata_and_unreviewed_commands_are_inert() {
    let backend = Arc::new(Fake(|_: Request, _: &CancellationToken| {
        panic!("metadata must never call a backend")
    }));
    let mut controller = fixture(backend);
    let context = egui::Context::default();
    assert!(!controller.view.draft.allow_creation);
    assert!(!controller.view.draft.approve_creation);
    assert!(!controller.view.draft.startup_at_sign_in);
    controller.action(
        Action::Edit(Draft {
            friendly_name: "Edited metadata".into(),
            allow_creation: true,
            ..Draft::default()
        }),
        &context,
    );
    controller.action(Action::Confirm, &context);
    controller.action(Action::Review(Operation::Enable), &context);
    assert_eq!(controller.view.failure, Some(Failure::RefreshFirst));
    assert!(!controller.view.busy);
    assert!(matches!(controller.view.scope, Scope::Unobserved));
    assert!(!controller.view.owner_approved);
}

#[test]
fn desktop_remote_access_review_is_invalidated_by_any_draft_edit() {
    let mut controller = fixture(Arc::new(Fake(|_: Request, _: &CancellationToken| {
        panic!("no confirmation")
    })));
    let context = egui::Context::default();
    controller.view.scope = Scope::Absent;
    controller.action(Action::Review(Operation::Enable), &context);
    assert_eq!(controller.view.review, Some(Operation::Enable));
    controller.action(Action::Edit(Draft::default()), &context);
    assert!(controller.view.review.is_none());
    controller.action(Action::Confirm, &context);
    assert!(!controller.view.busy);
}

#[test]
fn desktop_remote_access_refresh_cannot_retarget_an_existing_confirmation() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let mut controller = fixture(Arc::new(Fake(
        move |request: Request, _: &CancellationToken| {
            recorded.lock().unwrap().push(request.operation);
            Ok(Report {
                scope: Scope::Absent,
                ..Report::default()
            })
        },
    )));
    let context = egui::Context::default();
    controller.view.scope = Scope::Absent;
    controller.action(Action::Review(Operation::Enable), &context);
    controller.action(Action::Refresh, &context);
    drain(&mut controller, &context);
    controller.action(Action::Confirm, &context);
    assert_eq!(*calls.lock().unwrap(), [Operation::Refresh]);
    assert!(controller.view.review.is_none());
}

#[test]
fn desktop_remote_access_only_explicit_reviewed_action_reaches_bounded_worker() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let mut controller = fixture(Arc::new(Fake(
        move |request: Request, _: &CancellationToken| {
            recorded.lock().unwrap().push(request.operation);
            Ok(Report {
                scope: Scope::Absent,
                ..Report::default()
            })
        },
    )));
    let context = egui::Context::default();
    controller.action(Action::Refresh, &context);
    drain(&mut controller, &context);
    controller.action(Action::Review(Operation::Enable), &context);
    assert_eq!(*calls.lock().unwrap(), [Operation::Refresh]);
    controller.action(Action::Confirm, &context);
    drain(&mut controller, &context);
    assert_eq!(
        *calls.lock().unwrap(),
        [Operation::Refresh, Operation::Enable]
    );
    assert!(!controller.view.owner_approved);
    assert!(!controller.view.observation_label().contains("Ready"));
}

#[test]
fn desktop_remote_access_cancel_retains_global_admission_and_discards_late_grant() {
    let (release, blocked) = mpsc::sync_channel(1);
    let blocked = Mutex::new(blocked);
    let (entered_sender, entered) = mpsc::sync_channel(1);
    let backend = Arc::new(Fake(move |request: Request, cancel: &CancellationToken| {
        if request.operation == Operation::Enable {
            entered_sender.send(()).unwrap();
            blocked.lock().unwrap().recv().unwrap();
            assert!(cancel.is_cancelled());
            return Ok(Report {
                scope: Scope::Absent,
                owner_approved: true,
                owner_binding: Some("late binding must not be published".into()),
                ..Report::default()
            });
        }
        assert_eq!(request.operation, Operation::Disable);
        assert!(request.cancel_fence);
        Ok(Report {
            scope: Scope::Absent,
            ..Report::default()
        })
    }));
    let mut controller = fixture(backend.clone());
    let slots = controller.slots.clone();
    let context = egui::Context::default();
    controller.view.scope = Scope::Absent;
    controller.action(Action::Review(Operation::Enable), &context);
    controller.action(Action::Confirm, &context);
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    controller.action(Action::Cancel, &context);
    controller.action(Action::Cancel, &context);
    let mut other_window = Controller::new(controller.root.clone(), backend, slots.clone());
    other_window.action(Action::Refresh, &context);
    assert_eq!(other_window.view.failure, Some(Failure::Busy));
    assert!(!other_window.view.busy);
    assert_ne!(slots.load(Ordering::Acquire) & 1, 0);
    release.send(()).unwrap();
    drain(&mut controller, &context);
    assert!(!controller.view.owner_approved);
    assert!(controller.view.owner_binding.is_none());
    assert_eq!(controller.view.scope, Scope::Absent);
}

#[test]
fn desktop_remote_access_busy_never_edits_or_replaces_pending_consent() {
    let (release, blocked) = mpsc::sync_channel(1);
    let blocked = Mutex::new(blocked);
    let mut controller = fixture(Arc::new(Fake(move |_: Request, _: &CancellationToken| {
        blocked.lock().unwrap().recv().unwrap();
        Ok(Report {
            scope: Scope::Absent,
            ..Report::default()
        })
    })));
    let context = egui::Context::default();
    controller.action(Action::Refresh, &context);
    controller.action(
        Action::Edit(Draft {
            allow_creation: true,
            ..Draft::default()
        }),
        &context,
    );
    assert_eq!(controller.view.failure, Some(Failure::Busy));
    assert!(!controller.view.draft.allow_creation);
    release.send(()).unwrap();
    drain(&mut controller, &context);
}

#[test]
fn desktop_remote_access_share_and_unshare_never_substitute_pid_or_metadata_authority() {
    let mut controller = fixture(Arc::new(Fake(|_: Request, _: &CancellationToken| {
        panic!("no daemon binding API")
    })));
    let target = ShareTarget {
        tab: crate::tabs::TabId::next_for_test(),
        profile: Some("private-profile".into()),
        lifecycle_generation: 7,
        identity: festerm_sessiond::NativeSessionIdentity {
            name: "private".into(),
            pid: 123,
            created_at_unix_ms: 456,
            endpoint: "fixture-local-endpoint".into(),
        },
    };
    let context = egui::Context::default();
    controller.action(Action::ReviewShare(target.clone()), &context);
    controller.action(Action::Share(target.clone()), &context);
    assert_eq!(controller.view.failure, Some(Failure::ShareCapability));
    controller.action(Action::Unshare(target), &context);
    assert_eq!(controller.view.failure, Some(Failure::ShareCapability));
    assert!(!controller.view.busy);
    assert!(!controller.view.owner_approved);
}

#[test]
fn desktop_remote_access_composition_command_routes_without_metadata_grants() {
    let mut app = super::super::FesTermApp::for_test_with_configuration(
        festerm_config::Configuration::empty(),
    );
    app.local_remote_access = fixture(Arc::new(Fake(|_: Request, _: &CancellationToken| {
        panic!("editing must remain network free")
    })));
    let context = egui::Context::default();
    app.dispatch_for_test(
        crate::tabs::AppCommand::LocalRemoteAccess(Action::Edit(Draft {
            friendly_name: "Local explicit draft".into(),
            ..Draft::default()
        })),
        &context,
    );
    app.dispatch_for_test(
        crate::tabs::AppCommand::LocalRemoteAccess(Action::Confirm),
        &context,
    );
    assert_eq!(
        app.local_remote_access.view.draft.friendly_name,
        "Local explicit draft"
    );
    assert!(app.state.local_share_targets().is_empty());
    assert_eq!(app.state.tabs().len(), 1);
    assert!(!app.local_remote_access.view.busy);
}

#[test]
fn desktop_remote_access_settings_render_is_inert_and_device_debug_is_redacted() {
    let context = egui::Context::default();
    let view = View {
        device_code: Some(DeviceCode(zeroize::Zeroizing::new("PRIVATE-CODE".into()))),
        ..View::default()
    };
    assert!(!format!("{view:?}").contains("PRIVATE-CODE"));
    let mut output = context.run_ui(egui::RawInput::default(), |ui| {
        assert!(show(ui, &view, &[]).is_none());
    });
    output.textures_delta.clear();
}

#[test]
fn desktop_remote_access_running_process_and_history_never_become_ready() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    let store = festerm_host_setup::lifecycle::Store::create(
        &root.path().join("desktop"),
        festerm_host_setup::lifecycle::Origin::Desktop,
        Draft::default().settings(),
    )
    .unwrap();
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
    let view = View {
        scope: Scope::Present(store.snapshot().unwrap().into()),
        process: Some("Running".into()),
        owner_approved: true,
        ..View::default()
    };
    assert!(view.observation_label().contains("NOT verified"));
    assert!(!view.observation_label().contains("Ready"));
}

#[test]
fn desktop_remote_access_input_and_missing_platform_context_fail_without_workers() {
    let mut controller = fixture(Arc::new(Fake(|_: Request, _: &CancellationToken| {
        panic!("no admission")
    })));
    let context = egui::Context::default();
    controller.action(
        Action::Edit(Draft {
            manifest: "x".repeat(4097),
            ..Draft::default()
        }),
        &context,
    );
    assert_eq!(controller.view.failure, Some(Failure::InvalidDraft));
    assert!(controller.view.draft.manifest.is_empty());
    controller.root = None;
    controller.action(Action::Refresh, &context);
    assert_eq!(controller.view.failure, Some(Failure::PrivateStorage));
    assert_eq!(controller.view.scope, Scope::Unobserved);
    assert!(!controller.view.busy);
}

#[test]
fn desktop_remote_access_window_drop_cancels_without_another_ui_frame() {
    let (release, blocked) = mpsc::sync_channel(1);
    let blocked = Mutex::new(blocked);
    let (entered_sender, entered) = mpsc::sync_channel(1);
    let backend = Arc::new(Fake(move |request: Request, cancel: &CancellationToken| {
        if request.operation == Operation::Enable {
            entered_sender.send(()).unwrap();
            blocked.lock().unwrap().recv().unwrap();
            assert!(cancel.is_cancelled());
        } else {
            assert_eq!(request.operation, Operation::Disable);
            assert!(request.cancel_fence);
        }
        Ok(Report::default())
    }));
    let mut controller = fixture(backend);
    let slots = controller.slots.clone();
    let context = egui::Context::default();
    controller.view.scope = Scope::Absent;
    controller.action(Action::Review(Operation::Enable), &context);
    controller.action(Action::Confirm, &context);
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(controller);
    assert_ne!(slots.load(Ordering::Acquire) & 1, 0);
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while slots.load(Ordering::Acquire) != 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}
