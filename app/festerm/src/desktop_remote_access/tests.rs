use super::*;
use std::time::{Duration, Instant};

#[test]
fn desktop_remote_access_enable_observation_warning_keeps_confirmed_phase_and_setup() {
    let confirmed = provisioned_scope();
    let mut previous = match &confirmed {
        Scope::Present(snapshot) => snapshot.as_ref().clone(),
        _ => unreachable!(),
    };
    previous.epoch -= 1;
    previous.phase = Phase::Disabled;
    let calls = Arc::new(AtomicU8::new(0));
    let counted = calls.clone();
    let projected = confirmed.clone();
    let warning = Failure::Observation {
        boundary: ObservationBoundary::ServiceStart,
        kind: std::io::ErrorKind::WouldBlock,
    };
    let mut controller = fixture(Arc::new(Fake(move |_: Request, _: &CancellationToken| {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(Report {
            scope: projected.clone(),
            setup_complete: true,
            failure: Some(warning),
            ..Report::default()
        })
    })));
    controller.view.scope = Scope::Present(previous.into());
    let context = egui::Context::default();
    controller.start(Operation::Enable, false, &context);
    drain(&mut controller, &context);
    assert_eq!(controller.view.scope, confirmed);
    assert_eq!(controller.view.bundle_status, BundleStatus::Complete);
    assert_eq!(controller.view.failure, Some(warning));
    assert!(controller.view.last_run.is_none() && controller.view.process.is_none());
    assert!(!controller.view.owner_approved);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn desktop_remote_access_setup_completion_does_not_mask_cancel_or_scope_failure() {
    for failure in [
        Failure::Cancelled,
        Failure::ScopeChanged,
        Failure::NativeVault,
    ] {
        let mut controller = fixture(Arc::new(Fake(move |_: Request, _: &CancellationToken| {
            Ok(Report {
                setup_complete: true,
                scope: Scope::Unobserved,
                failure: Some(failure),
                ..Report::default()
            })
        })));
        let context = egui::Context::default();
        controller.start(Operation::Enable, false, &context);
        drain(&mut controller, &context);
        assert_eq!(controller.view.failure, Some(failure));
        assert_eq!(
            controller.view.bundle_status,
            if failure == Failure::Cancelled {
                BundleStatus::Cancelled
            } else {
                BundleStatus::Blocked
            }
        );
        assert!(!controller.view.access_enabled());
    }
}

#[test]
fn desktop_remote_access_failed_mandatory_observation_clears_obsolete_scope() {
    for setup_complete in [false, true] {
        let mut controller = fixture(Arc::new(Fake(move |_: Request, _: &CancellationToken| {
            let warning = Failure::Observation {
                boundary: ObservationBoundary::Scope,
                kind: std::io::ErrorKind::WouldBlock,
            };
            if setup_complete {
                Ok(Report {
                    setup_complete,
                    failure: Some(warning),
                    ..Report::default()
                })
            } else {
                Err(warning)
            }
        })));
        controller.view.scope = provisioned_scope();
        controller.view.owner_approved = true;
        let context = egui::Context::default();
        controller.start(Operation::Enable, false, &context);
        drain(&mut controller, &context);
        assert_eq!(controller.view.scope, Scope::Unobserved);
        assert!(!controller.view.access_enabled());
        assert!(!controller.view.owner_approved);
        assert!(controller.view.last_run.is_none() && controller.view.process.is_none());
        assert_eq!(
            controller.view.bundle_status,
            if setup_complete {
                BundleStatus::Complete
            } else {
                BundleStatus::Blocked
            }
        );
    }
}

#[test]
fn desktop_remote_access_hostname_seed_fits_friendly_name_contract() {
    for hostname in [
        "Build-Mac.local".to_owned(),
        "界".repeat(40),
        "lab\nserver".into(),
    ] {
        let friendly_name = host_name_label(std::ffi::OsStr::new(&hostname));
        Draft {
            friendly_name,
            ..Draft::default()
        }
        .settings()
        .unwrap()
        .validate()
        .unwrap();
    }
    assert_eq!(
        host_name_label(std::ffi::OsStr::new("Build-Mac.local")),
        "Build-Mac.local"
    );
}

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
        if !controller.view.busy {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "bounded fixture worker did not drain: busy={} pending={} fence={} slots={} pending_cancelled={} fence_cancelled={}",
            controller.view.busy,
            controller.pending.is_some(),
            controller.fence.is_some(),
            controller.slots.load(Ordering::Acquire),
            controller.pending.as_ref().is_some_and(|pending| pending.cancel.is_cancelled()),
            controller.fence.as_ref().is_some_and(|pending| pending.cancel.is_cancelled()),
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn provisioned_scope() -> Scope {
    use festerm_host_setup::lifecycle::{Binding, Origin, Store};
    let root = festerm_sessiond::private_storage::private_test_directory();
    let store = Store::create(
        &root.path().join("desktop"),
        Origin::Desktop,
        Draft::default().settings().unwrap(),
    )
    .unwrap();
    let ticket = store
        .request_enable(&Draft::default().settings().unwrap())
        .unwrap();
    store
        .record_provisioned(
            &ticket,
            Binding {
                host_id: "11111111-1111-4111-8111-111111111111".into(),
                account: "github:42".into(),
                qualified_tunnel: "synthetic-cluster/synthetic-tunnel".into(),
                cloud_incarnation: "synthetic-incarnation".into(),
                host_key_fingerprint: "synthetic-public-pin".into(),
            },
        )
        .unwrap();
    Scope::Present(store.snapshot().unwrap().into())
}

#[test]
fn desktop_remote_access_master_checkbox_keeps_account_controls_conditional_and_reviewed() {
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 1200.0))
        .build_ui_state(
            |ui, state: &mut (View, Option<Action>)| {
                if let Some(action) = show(ui, &state.0, &[]) {
                    state.1 = Some(action);
                }
            },
            (
                View {
                    scope: Scope::Absent,
                    accounts: vec![HostAccount {
                        account_id: 42,
                        login: "verified-owner".into(),
                    }],
                    ..View::default()
                },
                None,
            ),
        );
    harness.run();
    assert!(harness.query_by_label("GitHub account").is_none());
    assert!(harness
        .query_by_label("Allow remote clients to create shell sessions")
        .is_none());
    assert!(harness
        .query_by_label("Owner public key · 64 hex characters")
        .is_none());
    assert_eq!(
        harness
            .get_by_role_and_label(
                egui::accesskit::Role::CheckBox,
                "Allow remote access to this computer",
            )
            .accesskit_node()
            .toggled(),
        Some(egui::accesskit::Toggled::False)
    );
    harness
        .get_by_role_and_label(
            egui::accesskit::Role::CheckBox,
            "Allow remote access to this computer",
        )
        .click();
    harness.run();
    assert!(matches!(
        harness.state().1,
        Some(Action::RequestAccess(true))
    ));
    let mut controller = fixture(Arc::new(Fake(|_: Request, _: &CancellationToken| {
        panic!("checking intent cannot perform setup");
    })));
    controller.view = harness.state().0.clone();
    controller.action(harness.state().1.clone().unwrap(), &harness.ctx);
    assert_eq!(controller.view.review, None);
    assert!(!controller.view.busy);
    harness.state_mut().0 = controller.view();
    harness.run();
    assert_eq!(
        harness
            .get_by_role_and_label(
                egui::accesskit::Role::CheckBox,
                "Allow remote access to this computer",
            )
            .accesskit_node()
            .toggled(),
        Some(egui::accesskit::Toggled::True)
    );
    harness.get_by_label("GitHub account");
    harness.get_by_role_and_label(
        egui::accesskit::Role::CheckBox,
        "Allow remote clients to create shell sessions",
    );
    assert_eq!(
        harness
            .get_by_role_and_label(
                egui::accesskit::Role::CheckBox,
                "Allow remote clients to create shell sessions",
            )
            .accesskit_node()
            .toggled(),
        Some(egui::accesskit::Toggled::False)
    );
    assert!(harness
        .query_by_label("GitHub numeric account ID (verified during sign-in)")
        .is_none());
    assert!(!harness.state().0.observation_label().contains("Ready"));
}

#[test]
fn desktop_remote_access_disable_review_preserves_draft_and_exact_local_shell_target() {
    let target = private_share_target();
    let expected = provisioned_scope();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let calls = recorded.clone();
    let before = expected.clone();
    let mut controller = fixture(Arc::new(Fake(
        move |request: Request, _: &CancellationToken| {
            assert_eq!(request.expected, before);
            assert!(
                request.sharing.is_none(),
                "Disable is not a session-end operation"
            );
            calls.lock().unwrap().push(request.operation);
            let Scope::Present(snapshot) = &request.expected else {
                unreachable!()
            };
            let mut disabled = snapshot.as_ref().clone();
            disabled.phase = Phase::Disabled;
            Ok(Report {
                scope: Scope::Present(disabled.into()),
                ..Report::default()
            })
        },
    )));
    controller.view.scope = expected;
    controller.view.draft.account = "42".into();
    controller.view.draft.allow_creation = true;
    controller.observe_share_targets(vec![target.clone()]);
    let context = egui::Context::default();
    controller.action(Action::RequestAccess(false), &context);
    assert!(recorded.lock().unwrap().is_empty());
    assert_eq!(controller.view.review, Some(Operation::Disable));
    controller.action(Action::Confirm, &context);
    drain(&mut controller, &context);
    assert_eq!(*recorded.lock().unwrap(), [Operation::Disable]);
    assert_eq!(*controller.share_targets.lock().unwrap(), [target]);
    assert_eq!(controller.view.draft.account, "42");
    assert!(controller.view.draft.allow_creation);
    assert!(!controller.view.access_enabled());
}

#[test]
fn desktop_remote_access_public_account_selection_never_rebinds_existing_host() {
    let mut controller = fixture(Arc::new(Fake(|_: Request, _: &CancellationToken| {
        panic!("account editing has no credential authority");
    })));
    controller.view.scope = provisioned_scope();
    controller.view.draft.account = "42".into();
    let mut changed = controller.view.draft.clone();
    changed.account = "43".into();
    controller.action(Action::Edit(changed), &egui::Context::default());
    assert_eq!(controller.view.failure, Some(Failure::ScopeChanged));
    assert_eq!(controller.view.draft.account, "42");
    assert!(!controller.view.owner_approved);
    let mut creation = controller.view.draft.clone();
    creation.allow_creation = true;
    controller.action(Action::Edit(creation), &egui::Context::default());
    assert!(matches!(
        sharing_button(&controller.view, "Review Disable to change host settings…"),
        Action::Review(Operation::Disable)
    ));
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
    assert!(!controller.view.draft.manual_bundle);
    controller.action(
        Action::Edit(Draft {
            friendly_name: "Edited metadata".into(),
            allow_creation: true,
            allow_session_termination: false,
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
fn desktop_remote_access_startup_observation_is_single_and_read_only() {
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
    controller.ensure_startup_observation(&context);
    controller.ensure_startup_observation(&context);
    drain(&mut controller, &context);
    assert_eq!(*calls.lock().unwrap(), [Operation::Refresh]);
    assert_eq!(controller.view.scope, Scope::Absent);
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
                diagnostic: Some(setup_diagnostic()),
                failure: Some(Failure::CloudUncertain),
                owner_binding: Some("late binding must not be published".into()),
                ..Report::default()
            });
        }
        assert_eq!(request.operation, Operation::Disable);
        assert!(request.cancel_fence);
        Ok(Report {
            scope: Scope::Absent,
            failure: Some(Failure::Reconciliation),
            diagnostic: Some(setup_diagnostic()),
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
    assert!(controller.view.diagnostic.is_none());
    assert_eq!(controller.view.scope, Scope::Absent);
}

#[test]
fn desktop_remote_access_releases_admission_before_publishing_completion() {
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (finish_tx, finish_rx) = mpsc::sync_channel(1);
    let finish_rx = Mutex::new(finish_rx);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_calls = calls.clone();
    let backend = Arc::new(Fake(move |_: Request, _: &CancellationToken| {
        if observed_calls.fetch_add(1, Ordering::AcqRel) == 0 {
            entered_tx.send(()).unwrap();
            finish_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
        Ok(Report {
            scope: Scope::Absent,
            ..Report::default()
        })
    }));
    let mut controller = fixture(backend.clone());
    let mut other = Controller::new(controller.root.clone(), backend, controller.slots.clone());
    let context = egui::Context::default();
    let (published_tx, published_rx) = mpsc::sync_channel(1);
    let (retire_tx, retire_rx) = mpsc::sync_channel(1);
    let retire_rx = Mutex::new(retire_rx);
    let first_repaint = std::sync::atomic::AtomicBool::new(true);
    // Pause in the real repaint callback after the result has been published,
    // without adding a production hook or relying on scheduler timing.
    context.set_request_repaint_callback(move |_| {
        if first_repaint.swap(false, Ordering::AcqRel) {
            published_tx.send(()).unwrap();
            retire_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    });
    controller.action(Action::Refresh, &context);
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    other.action(Action::Refresh, &context);
    assert_eq!(other.view.failure, Some(Failure::Busy));
    assert_eq!(calls.load(Ordering::Acquire), 1);
    finish_tx.send(()).unwrap();
    published_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    controller.poll(&context);
    assert!(!controller.view.busy);
    controller.action(Action::Refresh, &context);
    let admitted = controller.pending.is_some() && controller.view.failure.is_none();
    retire_tx.send(()).unwrap();
    drain(&mut controller, &context);
    assert!(
        admitted,
        "published completion retained the previous global permit"
    );
    assert_eq!(calls.load(Ordering::Acquire), 2);
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
            allow_session_termination: false,
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
        source: None,
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
    assert_eq!(controller.view.failure, Some(Failure::Reconciliation));
    assert!(!controller.view.busy);
    assert!(!controller.view.owner_approved);
}

fn private_share_target() -> ShareTarget {
    ShareTarget {
        tab: crate::tabs::TabId::next_for_test(),
        profile: Some("private-sharing-fixture".into()),
        lifecycle_generation: 9,
        identity: festerm_sessiond::NativeSessionIdentity {
            name: "private-sharing".into(),
            pid: 123,
            created_at_unix_ms: 456,
            endpoint: "fixture-local-endpoint".into(),
        },
        source: None,
    }
}

fn sharing_button(view: &View, label: &str) -> Action {
    use egui_kittest::{kittest::Queryable, Harness};

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 2000.0))
        .build_ui_state(
            |ui, state: &mut (View, Option<Action>)| {
                if let Some(action) = show(ui, &state.0, &[]) {
                    state.1 = Some(action);
                }
            },
            (view.clone(), None),
        );
    harness.run();
    harness.get_by_label("Advanced / Recovery").click();
    harness.run();
    if let Some(disclosure) = harness.query_by_label("Session sharing / Recovery") {
        disclosure.click();
        harness.run();
    }
    for label in [
        "Reviewed session / Recovery",
        "Sharing operation / Recovery",
    ] {
        if let Some(disclosure) = harness.query_by_label(label) {
            disclosure.click();
            harness.run();
        }
    }
    // Settings toggle rows label both their title and their switch; the
    // switch (a checkbox) is the control.
    match harness.query_by_role_and_label(egui::accesskit::Role::CheckBox, label) {
        Some(toggle) => toggle.click(),
        None => harness.get_by_label(label).click(),
    }
    harness.run();
    harness.state().1.clone().expect("explicit sharing button")
}

#[test]
fn desktop_remote_access_checkboxes_require_fresh_review_and_reach_host_settings() {
    for initially_enabled in [false, true] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let mut controller = fixture(Arc::new(Fake(
            move |request: Request, _: &CancellationToken| {
                assert_eq!(request.operation, Operation::Enable);
                assert!(request.sharing.is_none());
                recorded
                    .lock()
                    .unwrap()
                    .push(request.draft.settings().unwrap());
                Ok(Report {
                    scope: Scope::Absent,
                    ..Report::default()
                })
            },
        )));
        let context = egui::Context::default();
        controller.view.scope = Scope::Absent;
        controller.view.access_requested = Some(true);
        controller.view.draft.allow_creation = initially_enabled;
        controller.view.draft.ordinary_access = if initially_enabled {
            OrdinaryAccess::ProviderOwner
        } else {
            OrdinaryAccess::OwnerOnly
        };
        controller.view.draft.startup_at_sign_in = initially_enabled;
        for label in [
            "Allow connections through my GitHub account",
            "Allow remote clients to create shell sessions",
            "Start at my sign-in (only where the native policy is qualified)",
        ] {
            controller.action(Action::Review(Operation::Enable), &context);
            assert_eq!(controller.view.review, Some(Operation::Enable));
            let action = sharing_button(&controller.view, label);
            assert!(matches!(action, Action::Edit(_)));
            controller.action(action, &context);
            assert!(controller.view.review.is_none());
            controller.action(Action::Confirm, &context);
            assert!(!controller.view.busy);
            assert!(calls.lock().unwrap().is_empty());
        }
        controller.action(Action::Review(Operation::Enable), &context);
        controller.action(Action::Confirm, &context);
        drain(&mut controller, &context);
        let settings = calls.lock().unwrap();
        assert_eq!(settings.len(), 1);
        assert_eq!(settings[0].allow_creation, !initially_enabled);
        assert_eq!(
            settings[0].ordinary_access,
            if initially_enabled {
                OrdinaryAccess::OwnerOnly
            } else {
                OrdinaryAccess::ProviderOwner
            }
        );
        assert_eq!(
            settings[0].startup,
            if initially_enabled {
                Startup::Manual
            } else {
                Startup::UserSignIn
            }
        );
        assert!(controller.view.sharing.is_none());
        assert!(!controller.view.owner_approved);
    }
}

#[test]
fn desktop_remote_access_refresh_preserves_existing_ordinary_policy_without_enabling() {
    assert_eq!(
        Draft::default().ordinary_access,
        OrdinaryAccess::ProviderOwner
    );
    for policy in [OrdinaryAccess::OwnerOnly, OrdinaryAccess::ProviderOwner] {
        for edited in [false, true] {
            let Scope::Present(mut snapshot) = provisioned_scope() else {
                unreachable!();
            };
            Arc::make_mut(&mut snapshot).settings.ordinary_access = policy;
            let expected = Scope::Present(snapshot);
            let report_scope = expected.clone();
            let mut controller = fixture(Arc::new(Fake(
                move |request: Request, _: &CancellationToken| {
                    assert_eq!(request.operation, Operation::Refresh);
                    Ok(Report {
                        scope: report_scope.clone(),
                        ..Report::default()
                    })
                },
            )));
            let context = egui::Context::default();
            if edited {
                let mut draft = controller.view.draft.clone();
                draft.ordinary_access = OrdinaryAccess::OwnerOnly;
                controller.action(Action::Edit(draft), &context);
            }
            controller.action(Action::Refresh, &context);
            drain(&mut controller, &context);
            assert_eq!(controller.view.scope, expected);
            assert_eq!(
                controller.view.draft.ordinary_access,
                if edited {
                    OrdinaryAccess::OwnerOnly
                } else {
                    policy
                }
            );
            assert!(controller.view.review.is_none());
            assert!(!controller.view.owner_approved);
        }
    }
}

#[test]
fn desktop_remote_access_normal_enable_consent_explains_shell_privileges_and_no_pairing() {
    use egui_kittest::{kittest::Queryable, Harness};
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 2000.0))
        .build_ui_state(
            |ui, view: &mut View| {
                assert!(show(ui, view, &[]).is_none());
            },
            View {
                scope: Scope::Absent,
                access_requested: Some(true),
                review: Some(Operation::Enable),
                ..View::default()
            },
        );
    harness.run();
    harness.get_by_label("Allow clients signed in to this host's GitHub account to list and open shared sessions without device pairing.");
    harness.get_by_label("Enable and start listening now. Starting automatically at OS sign-in remains a separate preference.");
    harness.get_by_label("A shared shell runs as your user on this computer and has that user's file and command permissions. Anyone holding a valid tunnel connection credential can obtain this access. New shells require the separate creation setting.");
    assert!(harness
        .query_by_label("Owner public key · 64 hex characters")
        .is_none());
}

#[test]
fn desktop_remote_access_sharing_reviewed_recovery_uses_selected_target_and_typed_original() {
    for phase in [SharePhase::Released, SharePhase::Uncertain] {
        for same_target in [false, true] {
            for (step, label) in [
                (
                    ShareStep::Resume,
                    "Resume original protected status · no switch",
                ),
                (
                    ShareStep::Release,
                    "Release reviewed original operation · keep shell",
                ),
                (
                    ShareStep::Rollback,
                    "Rollback reviewed original operation · irreversible unshare",
                ),
            ] {
                let calls = Arc::new(Mutex::new(Vec::new()));
                let recorded = calls.clone();
                let mut controller = fixture(Arc::new(Fake(
                    move |request: Request, _: &CancellationToken| {
                        let task = request.sharing.unwrap();
                        recorded.lock().unwrap().push((
                            request.operation,
                            task.target,
                            task.operation,
                        ));
                        Err(Failure::Reconciliation)
                    },
                )));
                let context = egui::Context::default();
                let previous = private_share_target();
                let reviewed = if same_target {
                    previous.clone()
                } else {
                    let mut other = private_share_target();
                    other.identity.name = "other-private-session".into();
                    other.identity.created_at_unix_ms += 1;
                    other.lifecycle_generation += 1;
                    other
                };
                let original = uuid::Uuid::new_v4().to_string();
                controller.view.sharing = Some(ShareStatus {
                    target: previous.clone(),
                    operation: uuid::Uuid::new_v4().to_string(),
                    phase,
                });
                controller.observe_share_epoch(Arc::new(AtomicU64::new(0)));
                controller.observe_share_targets(vec![reviewed.clone()]);
                controller.action(Action::ReviewShare(reviewed.clone()), &context);
                controller.action(Action::RecoverShare(original.clone()), &context);
                let action = sharing_button(&controller.view, label);
                controller.action(action, &context);
                drain(&mut controller, &context);
                assert_eq!(
                    *calls.lock().unwrap(),
                    [(Operation::Share(step), reviewed.clone(), original.clone())]
                );
                assert_eq!(controller.view.failure, Some(Failure::Reconciliation));
                assert_eq!(
                    controller.view.sharing,
                    Some(ShareStatus {
                        target: reviewed,
                        operation: original,
                        phase: SharePhase::Uncertain,
                    })
                );
            }
        }
    }
}

#[test]
fn desktop_remote_access_sharing_status_controls_keep_displayed_target_and_original() {
    for phase in [SharePhase::Released, SharePhase::Uncertain] {
        for (step, label) in [
            (ShareStep::Resume, "Resume original status · no switch"),
            (ShareStep::Release, "Confirm Release · keep shell/frontend"),
            (
                ShareStep::Rollback,
                "Confirm Rollback · irreversible unshare only",
            ),
        ] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let recorded = calls.clone();
            let mut controller = fixture(Arc::new(Fake(
                move |request: Request, _: &CancellationToken| {
                    let task = request.sharing.unwrap();
                    recorded
                        .lock()
                        .unwrap()
                        .push((request.operation, task.target, task.operation));
                    Err(Failure::Reconciliation)
                },
            )));
            let retained = ShareStatus {
                target: private_share_target(),
                operation: uuid::Uuid::new_v4().to_string(),
                phase,
            };
            let mut reviewed = private_share_target();
            reviewed.identity.name = "other-reviewed-session".into();
            reviewed.identity.created_at_unix_ms += 1;
            reviewed.lifecycle_generation += 1;
            let context = egui::Context::default();
            controller.view.sharing = Some(retained.clone());
            controller.observe_share_epoch(Arc::new(AtomicU64::new(0)));
            controller.observe_share_targets(vec![reviewed.clone()]);
            controller.action(Action::ReviewShare(reviewed), &context);
            controller.action(
                Action::RecoverShare(uuid::Uuid::new_v4().to_string()),
                &context,
            );
            let action = sharing_button(&controller.view, label);
            controller.action(action, &context);
            drain(&mut controller, &context);
            assert_eq!(
                *calls.lock().unwrap(),
                [(Operation::Share(step), retained.target, retained.operation)]
            );
        }
    }
}

#[test]
fn desktop_remote_access_sharing_reviewed_recovery_refuses_missing_stale_or_grant_intent() {
    for step in [ShareStep::Resume, ShareStep::Release, ShareStep::Rollback] {
        for refusal in 0..4 {
            let mut controller = fixture(Arc::new(Fake(|_: Request, _: &CancellationToken| {
                panic!("invalid reviewed recovery must not reach a backend")
            })));
            let target = private_share_target();
            let epoch = Arc::new(AtomicU64::new(0));
            let context = egui::Context::default();
            controller.view.sharing = Some(ShareStatus {
                target: private_share_target(),
                operation: uuid::Uuid::new_v4().to_string(),
                phase: SharePhase::Uncertain,
            });
            controller.observe_share_epoch(epoch.clone());
            controller.observe_share_targets(vec![target.clone()]);
            controller.action(Action::ReviewShare(target), &context);
            controller.action(
                Action::RecoverShare(uuid::Uuid::new_v4().to_string()),
                &context,
            );
            let expected = match refusal {
                0 => {
                    controller.action(Action::RecoverShare(String::new()), &context);
                    Failure::Reconciliation
                }
                1 => {
                    epoch.fetch_add(1, Ordering::AcqRel);
                    Failure::SessionChanged
                }
                2 => {
                    controller.observe_share_targets(Vec::new());
                    Failure::SessionChanged
                }
                _ => {
                    controller.view.share_review = None;
                    Failure::SessionChanged
                }
            };
            controller.action(Action::RecoverReviewedShare(step), &context);
            assert_eq!(controller.view.failure, Some(expected));
            assert!(!controller.view.busy);
        }
    }
    let mut controller = fixture(Arc::new(Fake(|_: Request, _: &CancellationToken| {
        panic!("recovery cannot bind or switch")
    })));
    let context = egui::Context::default();
    for step in [ShareStep::Bind, ShareStep::Switch] {
        controller.action(Action::RecoverReviewedShare(step), &context);
        assert_eq!(controller.view.failure, Some(Failure::InvalidDraft));
        assert!(!controller.view.busy);
    }
}

#[test]
fn desktop_remote_access_sharing_reviewed_malformed_original_reaches_only_native_parse_refusal() {
    for step in [ShareStep::Resume, ShareStep::Release, ShareStep::Rollback] {
        let mut controller = fixture(Arc::new(native::Native));
        let target = private_share_target();
        let context = egui::Context::default();
        controller.view.sharing = Some(ShareStatus {
            target: private_share_target(),
            operation: uuid::Uuid::new_v4().to_string(),
            phase: SharePhase::Released,
        });
        controller.observe_share_epoch(Arc::new(AtomicU64::new(0)));
        controller.observe_share_targets(vec![target.clone()]);
        controller.action(Action::ReviewShare(target.clone()), &context);
        controller.action(Action::RecoverShare("malformed-original".into()), &context);
        controller.action(Action::RecoverReviewedShare(step), &context);
        drain(&mut controller, &context);
        assert_eq!(controller.view.failure, Some(Failure::InvalidDraft));
        assert_eq!(
            controller.view.sharing,
            Some(ShareStatus {
                target,
                operation: "malformed-original".into(),
                phase: SharePhase::Uncertain,
            })
        );
    }
}

#[test]
fn desktop_remote_access_sharing_review_edit_and_missing_provenance_never_dispatch() {
    let mut controller = fixture(Arc::new(Fake(|_: Request, _: &CancellationToken| {
        panic!("no native capability or consent")
    })));
    let context = egui::Context::default();
    let target = private_share_target();
    controller.action(Action::ReviewShare(target.clone()), &context);
    controller.action(Action::Edit(Draft::default()), &context);
    assert!(controller.view.share_review.is_none());
    controller.action(Action::Share(target), &context);
    assert_eq!(controller.view.failure, Some(Failure::ShareCapability));
    controller.action(
        Action::Review(Operation::Share(ShareStep::Switch)),
        &context,
    );
    assert_eq!(controller.view.failure, Some(Failure::InvalidDraft));
    controller.action(Action::Confirm, &context);
    assert!(!controller.view.busy);
    assert!(controller.view.sharing.is_none());
}

#[test]
fn desktop_remote_access_sharing_selection_witness_is_only_a_negative_fence() {
    let mut app = super::super::FesTermApp::for_test_with_configuration(
        festerm_config::Configuration::empty(),
    );
    let target = private_share_target();
    let epoch = app.state.local_share_epoch();
    let current = Arc::new(Mutex::new(vec![target.clone()]));
    let task = ShareTask {
        target,
        operation: uuid::Uuid::new_v4().to_string(),
        current: current.clone(),
        epoch: Some((epoch.clone(), epoch.load(Ordering::Acquire))),
    };
    let cancel = CancellationToken::new();
    assert!(task.is_current(&cancel));
    assert!(task.target.source.is_none());
    app.state.dispatch(
        crate::tabs::AppCommand::OpenSettings,
        &egui::Context::default(),
    );
    assert!(!task.is_current(&cancel));
    current.lock().unwrap().clear();
    assert!(!task.is_current(&cancel));
    assert!(app.state.local_share_targets().is_empty());
}

#[test]
fn desktop_remote_access_sharing_review_cannot_retarget_after_command_or_refresh() {
    let mut controller = fixture(Arc::new(Fake(|request: Request, _: &CancellationToken| {
        assert_eq!(request.operation, Operation::Refresh);
        Ok(Report::default())
    })));
    let epoch = Arc::new(AtomicU64::new(0));
    let context = egui::Context::default();
    controller.observe_share_epoch(epoch.clone());
    controller.action(Action::ReviewShare(private_share_target()), &context);
    assert!(controller.view.share_review.is_some());
    epoch.fetch_add(1, Ordering::AcqRel);
    controller.observe_share_epoch(epoch);
    assert!(controller.view.share_review.is_none());
    controller.action(Action::ReviewShare(private_share_target()), &context);
    controller.action(Action::Refresh, &context);
    drain(&mut controller, &context);
    assert!(controller.view.share_review.is_none());
    assert!(controller.share_review_epoch.is_none());
}

#[test]
fn desktop_remote_access_sharing_reduction_retains_original_operation_without_frontend() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let mut controller = fixture(Arc::new(Fake(
        move |request: Request, _: &CancellationToken| {
            let task = request.sharing.unwrap();
            assert!(matches!(
                request.operation,
                Operation::Share(ShareStep::Release | ShareStep::Rollback)
            ));
            recorded.lock().unwrap().push(task.operation.clone());
            Ok(Report {
                scope: Scope::Absent,
                sharing: Some(ShareStatus {
                    target: task.target,
                    operation: task.operation,
                    phase: SharePhase::Released,
                }),
                ..Report::default()
            })
        },
    )));
    let original = uuid::Uuid::new_v4().to_string();
    controller.view.sharing = Some(ShareStatus {
        target: private_share_target(),
        operation: original.clone(),
        phase: SharePhase::Active,
    });
    let context = egui::Context::default();
    controller.action(Action::ReleaseShare, &context);
    drain(&mut controller, &context);
    controller.action(Action::RollbackShare, &context);
    drain(&mut controller, &context);
    assert_eq!(*calls.lock().unwrap(), [original.clone(), original]);
    assert_eq!(
        controller.view.sharing.as_ref().unwrap().phase,
        SharePhase::Released
    );
    assert!(!controller.view.owner_approved);
}

#[test]
fn desktop_remote_access_sharing_cancel_queued_result_uses_exact_release_not_disable() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let mut controller = fixture(Arc::new(Fake(
        move |request: Request, _: &CancellationToken| {
            let task = request.sharing.unwrap();
            recorded
                .lock()
                .unwrap()
                .push((request.operation, task.operation.clone()));
            let phase = match request.operation {
                Operation::Share(ShareStep::Switch) => SharePhase::Active,
                Operation::Share(ShareStep::Release) => {
                    assert!(request.cancel_fence);
                    SharePhase::Released
                }
                _ => panic!("sharing cancellation must not disable the host"),
            };
            Ok(Report {
                scope: Scope::Absent,
                sharing: Some(ShareStatus {
                    target: task.target,
                    operation: task.operation,
                    phase,
                }),
                ..Report::default()
            })
        },
    )));
    let target = private_share_target();
    let original = uuid::Uuid::new_v4().to_string();
    let epoch = Arc::new(AtomicU64::new(0));
    controller.observe_share_targets(vec![target.clone()]);
    controller.view.sharing = Some(ShareStatus {
        target: target.clone(),
        operation: original.clone(),
        phase: SharePhase::Uncertain,
    });
    // This is a worker-plumbing fixture, never a constructed native capability.
    controller.share_request = Some(ShareTask {
        target,
        operation: original.clone(),
        current: controller.share_targets.clone(),
        epoch: Some((epoch, 0)),
    });
    let context = egui::Context::default();
    controller.start(Operation::Share(ShareStep::Switch), false, &context);
    let deadline = Instant::now() + Duration::from_secs(5);
    while controller.slots.load(Ordering::Acquire) & 1 != 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    controller.action(Action::Cancel, &context);
    drain(&mut controller, &context);
    assert_eq!(
        *calls.lock().unwrap(),
        [
            (Operation::Share(ShareStep::Switch), original.clone()),
            (Operation::Share(ShareStep::Release), original),
        ]
    );
    assert_eq!(
        controller.view.sharing.as_ref().unwrap().phase,
        SharePhase::Released
    );
    assert_eq!(controller.slots.load(Ordering::Acquire), 0);
}

#[test]
fn desktop_remote_access_sharing_cancel_blocked_switch_retains_admission_and_original() {
    let (release, blocked) = mpsc::sync_channel(1);
    let blocked = Mutex::new(blocked);
    let (entered_sender, entered) = mpsc::sync_channel(1);
    let original = uuid::Uuid::new_v4().to_string();
    let expected = original.clone();
    let backend = Arc::new(Fake(move |request: Request, cancel: &CancellationToken| {
        let task = request.sharing.unwrap();
        assert_eq!(task.operation, expected);
        match request.operation {
            Operation::Share(ShareStep::Switch) => {
                entered_sender.send(()).unwrap();
                blocked.lock().unwrap().recv().unwrap();
                assert!(cancel.is_cancelled());
                Ok(Report {
                    owner_approved: true,
                    sharing: Some(ShareStatus {
                        target: task.target,
                        operation: task.operation,
                        phase: SharePhase::Active,
                    }),
                    ..Report::default()
                })
            }
            Operation::Share(ShareStep::Release) => {
                assert!(request.cancel_fence);
                Ok(Report {
                    sharing: Some(ShareStatus {
                        target: task.target,
                        operation: task.operation,
                        phase: SharePhase::Released,
                    }),
                    ..Report::default()
                })
            }
            _ => panic!("no Disable, replacement or frontend acquisition"),
        }
    }));
    let mut controller = fixture(backend.clone());
    let target = private_share_target();
    let epoch = Arc::new(AtomicU64::new(0));
    controller.observe_share_targets(vec![target.clone()]);
    controller.view.sharing = Some(ShareStatus {
        target: target.clone(),
        operation: original.clone(),
        phase: SharePhase::Prepared,
    });
    controller.share_request = Some(ShareTask {
        target,
        operation: original,
        current: controller.share_targets.clone(),
        epoch: Some((epoch.clone(), 0)),
    });
    let context = egui::Context::default();
    controller.start(Operation::Share(ShareStep::Switch), false, &context);
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    epoch.fetch_add(1, Ordering::AcqRel);
    controller.poll(&context);
    assert!(controller.view.cancelling);
    let mut other = Controller::new(controller.root.clone(), backend, controller.slots.clone());
    other.action(Action::Refresh, &context);
    assert_eq!(other.view.failure, Some(Failure::Busy));
    assert_ne!(controller.slots.load(Ordering::Acquire) & 1, 0);
    release.send(()).unwrap();
    drain(&mut controller, &context);
    assert!(!controller.view.owner_approved);
    assert_eq!(
        controller.view.sharing.as_ref().unwrap().phase,
        SharePhase::Released
    );
}

#[test]
fn desktop_remote_access_sharing_recovery_input_is_bounded_and_never_switches() {
    let mut controller = fixture(Arc::new(Fake(|_: Request, _: &CancellationToken| {
        panic!("unselected recovery never reaches native authority")
    })));
    let context = egui::Context::default();
    controller.action(Action::RecoverShare("x".repeat(129)), &context);
    assert_eq!(controller.view.failure, Some(Failure::InvalidDraft));
    controller.action(Action::RecoverShare("private\ncontrol".into()), &context);
    assert_eq!(controller.view.failure, Some(Failure::InvalidDraft));
    controller.action(Action::RecoverShare("original-operation".into()), &context);
    controller.action(Action::ResumeShare, &context);
    assert_eq!(controller.view.failure, Some(Failure::SessionChanged));
    assert!(!controller.view.busy);
    assert!(!ShareStep::Resume.grants());
}

#[test]
fn desktop_remote_access_sharing_phase_render_is_metadata_only() {
    let context = egui::Context::default();
    let view = View {
        sharing: Some(ShareStatus {
            target: private_share_target(),
            operation: "original-operation".into(),
            phase: SharePhase::Prepared,
        }),
        ..View::default()
    };
    let mut output = context.run_ui(egui::RawInput::default(), |ui| {
        assert!(show(ui, &view, &[]).is_none());
    });
    output.textures_delta.clear();
    assert!(!view.owner_approved);
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
        Draft::default().settings().unwrap(),
    )
    .unwrap();
    let ticket = store
        .request_enable(&Draft::default().settings().unwrap())
        .unwrap();
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

#[test]
fn desktop_remote_access_not_published_blocks_without_retry_or_manual_fallback() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let mut controller = fixture(Arc::new(Fake(
        move |request: Request, _: &CancellationToken| {
            recorded.lock().unwrap().push(request.draft.manual_bundle);
            Err(Failure::BundleNotPublished)
        },
    )));
    let context = egui::Context::default();
    controller.view.scope = Scope::Absent;
    controller.action(Action::Review(Operation::Enable), &context);
    assert!(calls.lock().unwrap().is_empty());
    controller.action(Action::Confirm, &context);
    drain(&mut controller, &context);
    assert_eq!(controller.view.bundle_status, BundleStatus::Blocked);
    assert_eq!(controller.view.failure, Some(Failure::BundleNotPublished));
    assert_eq!(*calls.lock().unwrap(), [false]);
    assert!(!controller.view.draft.manual_bundle);
    controller.action(Action::Confirm, &context);
    controller.poll(&context);
    assert_eq!(*calls.lock().unwrap(), [false]);
    controller.action(Action::Review(Operation::Enable), &context);
    controller.action(
        Action::Edit(Draft {
            manual_bundle: true,
            ..Draft::default()
        }),
        &context,
    );
    assert!(controller.view.review.is_none());
    controller.action(Action::Confirm, &context);
    assert_eq!(*calls.lock().unwrap(), [false]);
    controller.action(Action::Review(Operation::Enable), &context);
    controller.action(Action::Confirm, &context);
    drain(&mut controller, &context);
    assert_eq!(*calls.lock().unwrap(), [false, true]);
}

#[test]
fn desktop_remote_access_bundle_progress_cannot_displace_sign_in_or_publish_after_cancel() {
    struct ProgressFixture {
        entered: SyncSender<()>,
        release: Mutex<Receiver<()>>,
    }
    impl Backend for ProgressFixture {
        fn execute(
            &self,
            request: Request,
            _: &CancellationToken,
            progress: SyncSender<Progress>,
        ) -> Result<Report, Failure> {
            if request.operation == Operation::Disable {
                return Ok(Report {
                    scope: Scope::Absent,
                    ..Report::default()
                });
            }
            assert_eq!(request.operation, Operation::Enable);
            assert!(progress
                .try_send(Progress::Bundle(BundleStatus::Authenticated))
                .is_ok());
            assert!(progress
                .try_send(Progress::Bundle(BundleStatus::Qualifying))
                .is_ok());
            assert!(progress
                .try_send(Progress::DeviceCode(DeviceCode(zeroize::Zeroizing::new(
                    "TEST-123".into()
                ))))
                .is_ok());
            self.entered.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
            Ok(Report {
                scope: Scope::Absent,
                ..Report::default()
            })
        }
    }
    let (release, blocked) = mpsc::sync_channel(1);
    let (entered_sender, entered) = mpsc::sync_channel(1);
    let mut controller = fixture(Arc::new(ProgressFixture {
        entered: entered_sender,
        release: Mutex::new(blocked),
    }));
    let context = egui::Context::default();
    controller.view.scope = Scope::Absent;
    controller.action(Action::Review(Operation::Enable), &context);
    controller.action(Action::Confirm, &context);
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    controller.poll(&context);
    assert_eq!(controller.view.bundle_status, BundleStatus::Qualifying);
    assert_eq!(
        controller.view.device_code.as_ref().unwrap().0.as_str(),
        "TEST-123"
    );
    controller.action(Action::Cancel, &context);
    release.send(()).unwrap();
    drain(&mut controller, &context);
    assert_eq!(controller.view.bundle_status, BundleStatus::Cancelled);
    assert!(controller.view.device_code.is_none());
    assert!(!controller.view.owner_approved);
}

#[test]
fn desktop_remote_access_sign_in_delivery_controls_do_not_replace_pending_work() {
    let context = egui::Context::default();
    let mut controller = fixture(Arc::new(Fake(|_: Request, _: &CancellationToken| {
        panic!("delivery controls cannot start host work")
    })));
    controller.view.busy = true;
    controller.view.device_code = Some(gallery_device_code());
    controller.view.failure = Some(Failure::SignIn);
    let generation = controller.generation;
    for action in [Action::CopySignInCode, Action::OpenSignInBrowser] {
        controller.action(action, &context);
        assert_eq!(controller.generation, generation);
        assert!(controller.pending.is_none());
        assert!(controller.fence.is_none());
        assert_eq!(controller.view.failure, Some(Failure::SignIn));
        assert!(controller.view.device_code.is_some());
    }
    controller.view.device_code = None;
    controller.action(Action::CopySignInCode, &context);
    assert!(controller.view.device_code.is_none());
    assert!(controller.pending.is_none());
}

#[test]
fn desktop_remote_access_unobserved_setup_renders_unknown_instead_of_disabled() {
    use egui_kittest::{kittest::Queryable, Harness};
    for requested in [false, true] {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 1200.0))
            .build_ui_state(
                |ui, view: &mut View| {
                    show(ui, view, &[]);
                },
                View {
                    scope: Scope::Unobserved,
                    access_requested: Some(requested),
                    bundle_status: BundleStatus::Complete,
                    failure: Some(Failure::Observation {
                        boundary: ObservationBoundary::Scope,
                        kind: std::io::ErrorKind::WouldBlock,
                    }),
                    ..View::default()
                },
            );
        harness.run();
        assert!(harness
            .query_by_label("Remote access status is unavailable. Refresh local status.")
            .is_some());
        assert!(harness
            .query_by_label("Setup pending. Remote access is not enabled yet.")
            .is_none());
        assert!(harness
            .query_by_label("Remote access is enabled. No current host report is available.")
            .is_none());
    }
}

#[test]
fn desktop_remote_access_summary_does_not_claim_pending_or_unobserved_access_is_on() {
    let mut view = View::default();
    assert_eq!(view.access_summary(), "Not observed");
    view.scope = Scope::Absent;
    assert_eq!(view.access_summary(), "Off");
    let Scope::Present(mut snapshot) = provisioned_scope() else {
        panic!("fixture")
    };
    for (phase, label) in [
        (Phase::EnableRequested, "Setup pending"),
        (Phase::Provisioned, "Configured"),
        (Phase::Reconciling, "Recovery pending"),
        (Phase::StopRequested, "Disabling"),
        (Phase::CleanupPending, "Cleanup pending"),
        (Phase::Disabled, "Off"),
        (Phase::Removed, "Off"),
    ] {
        Arc::make_mut(&mut snapshot).phase = phase;
        view.scope = Scope::Present(snapshot.clone());
        assert_eq!(view.access_summary(), label);
    }
}

#[test]
fn desktop_remote_access_recovery_buttons_remain_readable_at_native_small_window_widths() {
    use egui_kittest::{kittest::Queryable, Harness};
    for width in [500.0, 900.0] {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(width, 1800.0))
            .build_ui_state(
                |ui, view: &mut View| {
                    show(ui, view, &[]);
                },
                View {
                    scope: Scope::Absent,
                    ..View::default()
                },
            );
        harness.run();
        harness.get_by_label("Advanced / Recovery").click();
        harness.run();
        for label in [
            "Refresh local status",
            "Observe native process…",
            "Review native owner context…",
            "Review interrupted setup recovery…",
            "Review Start…",
            "Review Disable…",
            "Review Uninstall / retry cleanup…",
        ] {
            let rect = harness.get_by_label(label).rect();
            assert!(
                rect.width() > 90.0 && rect.height() < 60.0,
                "{label} collapsed at width {width}: {rect:?}"
            );
        }
    }
}

fn setup_diagnostic() -> HostSetupDiagnostic {
    use festerm_host_setup::diagnostics::{Category, Recovery, Stage};
    HostSetupDiagnostic {
        stage: Stage::Creation,
        http_status: Some(400),
        category: Category::Response,
        recovery: Recovery::Reconcile,
    }
}

#[test]
fn desktop_remote_access_diagnostic_result_clears_on_changed_draft_and_successful_refresh() {
    let expected = setup_diagnostic();
    let mut controller = fixture(Arc::new(Fake(
        move |request: Request, _: &CancellationToken| {
            Ok(Report {
                scope: Scope::Absent,
                failure: (request.operation == Operation::Enable)
                    .then_some(Failure::CloudUncertain),
                // A success must clear even a malformed backend's old diagnostic.
                diagnostic: Some(expected),
                ..Report::default()
            })
        },
    )));
    let context = egui::Context::default();
    controller.view.scope = Scope::Absent;
    controller.start(Operation::Enable, false, &context);
    drain(&mut controller, &context);
    assert_eq!(controller.view.diagnostic, Some(expected));
    controller.action(Action::Review(Operation::Reconcile), &context);
    assert_eq!(controller.view.diagnostic, Some(expected));
    let mut draft = controller.view.draft.clone();
    draft.friendly_name = "Changed consent".into();
    controller.action(Action::Edit(draft), &context);
    assert!(controller.view.diagnostic.is_none());
    controller.view.diagnostic = Some(expected);
    controller.action(Action::Refresh, &context);
    assert!(controller.view.diagnostic.is_none());
    drain(&mut controller, &context);
    assert!(controller.view.diagnostic.is_none());
    assert!(controller.view.failure.is_none());
}

#[test]
fn desktop_remote_access_diagnostics_render_static_details_without_readiness_claim() {
    use egui_kittest::{kittest::Queryable, Harness};
    let expected = setup_diagnostic();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(340.0, 600.0))
        .build_ui_state(
            |ui, state: &mut Option<HostSetupDiagnostic>| diagnostics::show(ui, *state, &[]),
            Some(expected),
        );
    harness.run();
    harness.get_by_label("Last setup operation");
    harness.get_by_label("HTTP status: 400");
    harness.get_by_label(&format!("Stage: {}", expected.stage.label()));
    harness.get_by_label(&format!("Category: {}", expected.category.label()));
    harness.get_by_label(expected.recovery.guidance());
    harness.get_by_label("Operation result only. Current endpoint readiness is not verified.");
    *harness.state_mut() = Some(HostSetupDiagnostic {
        http_status: None,
        ..expected
    });
    harness.run();
    harness.get_by_label("Last setup operation");
    assert!(harness.query_by_label("HTTP status: 400").is_none());
    *harness.state_mut() = None;
    harness.run();
    assert!(harness.query_by_label("Last setup operation").is_none());
    assert!(harness.query_by_label("HTTP status: 400").is_none());
}

#[test]
fn desktop_remote_access_untyped_cloud_failures_never_invent_transport_metadata() {
    for report_failure in [false, true] {
        let mut controller = fixture(Arc::new(Fake(move |_: Request, _: &CancellationToken| {
            if report_failure {
                Ok(Report {
                    scope: Scope::Absent,
                    failure: Some(Failure::CloudUncertain),
                    ..Report::default()
                })
            } else {
                Err(Failure::CloudUncertain)
            }
        })));
        let context = egui::Context::default();
        controller.start(Operation::Refresh, false, &context);
        drain(&mut controller, &context);
        assert_eq!(controller.view.failure, Some(Failure::CloudUncertain));
        assert!(controller.view.diagnostic.is_none());
    }
}

#[test]
fn desktop_remote_access_diagnostics_refuse_invalidated_scope_reports() {
    for failure in [
        Failure::Cancelled,
        Failure::ScopeChanged,
        Failure::SessionChanged,
    ] {
        let mut controller = fixture(Arc::new(Fake(move |_: Request, _: &CancellationToken| {
            Ok(Report {
                scope: Scope::Absent,
                failure: Some(failure),
                diagnostic: Some(setup_diagnostic()),
                ..Report::default()
            })
        })));
        let context = egui::Context::default();
        controller.start(Operation::Refresh, false, &context);
        drain(&mut controller, &context);
        assert_eq!(controller.view.failure, Some(failure));
        assert!(controller.view.diagnostic.is_none());
    }
}

#[test]
fn desktop_remote_access_normal_status_shows_recorded_state_and_checked_process_action() {
    use egui_kittest::{kittest::Queryable, Harness};
    use festerm_host_setup::controller::RunState;
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 1200.0))
        .build_ui_state(
            |ui, state: &mut (View, Option<Action>)| {
                if let Some(action) = show(ui, &state.0, &[]) {
                    state.1 = Some(action);
                }
            },
            (
                View {
                    scope: provisioned_scope(),
                    last_run: Some(RunState::Listening),
                    ..View::default()
                },
                None,
            ),
        );
    harness.get_by_label("Remote access is enabled. Last reported: listening.");
    harness.get_by_label("Recorded status, not a current connection test.");
    assert!(harness
        .query_by_label("Host configured. Connection status not checked.")
        .is_none());
    harness.get_by_label("Check host process…").click();
    harness.run();
    assert!(matches!(
        harness.state().1,
        Some(Action::Review(Operation::ObserveNative))
    ));
    for (state, label) in [
        (
            Some(RunState::Starting),
            "Remote access is enabled. Last reported: starting.",
        ),
        (
            Some(RunState::Stopped),
            "Remote access is enabled. Last reported: stopped.",
        ),
        (
            Some(RunState::Failed),
            "Remote access is enabled. Last reported: failed.",
        ),
        (
            None,
            "Remote access is enabled. No current host report is available.",
        ),
    ] {
        harness.state_mut().0.last_run = state;
        harness.run();
        harness.get_by_label(label);
    }
    let Scope::Present(snapshot) = &mut harness.state_mut().0.scope else {
        panic!("fixture installation missing")
    };
    Arc::make_mut(snapshot).phase = Phase::Disabled;
    harness.run();
    assert!(harness.query_by_label("Check host process…").is_none());
    assert!(harness
        .query_by_label("Remote access is enabled. No current host report is available.")
        .is_none());
}

fn runtime_history_fixture() -> festerm_host_setup::controller::runtime_diagnostics::Record {
    use festerm_host_setup::controller::runtime_diagnostics::{
        Correlation, Outcome, Phase, Record,
    };
    Record {
        timestamp: "2026-11-19T00:00:00Z".into(),
        pid: 42,
        epoch: 16,
        run_id: uuid::Uuid::new_v4(),
        phase: Phase::RelayCleanup,
        outcome: Outcome::Error,
        outer: Some("host.outer_timeout"),
        disposition: Some(festerm_tunnel_bridge::relay::HostFailureDisposition::Transient),
        category: None,
        http_status: None,
        os_code: Some(5),
        correlation: Correlation::ManagedHistory,
    }
}

#[test]
fn desktop_remote_access_runtime_history_is_separate_and_clears_on_refresh_failure() {
    let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_failed = failed.clone();
    let history = runtime_history_fixture();
    let supplied = history.clone();
    let mut controller = fixture(Arc::new(Fake(move |_: Request, _: &CancellationToken| {
        if worker_failed.load(Ordering::Acquire) {
            return Err(Failure::ScopeChanged);
        }
        Ok(Report {
            scope: provisioned_scope(),
            runtime_history: vec![supplied.clone()],
            ..Report::default()
        })
    })));
    controller.view.diagnostic = Some(setup_diagnostic());
    let context = egui::Context::default();
    controller.action(Action::Refresh, &context);
    assert!(controller.view.diagnostic.is_none());
    drain(&mut controller, &context);
    assert_eq!(controller.view.runtime_history, vec![history]);
    assert!(controller.view.diagnostic.is_none());
    assert!(controller.view.process.is_none());
    assert!(controller.view.last_run.is_none());
    failed.store(true, Ordering::Release);
    controller.action(Action::Refresh, &context);
    assert!(controller.view.runtime_history.is_empty());
    drain(&mut controller, &context);
    assert!(controller.view.runtime_history.is_empty());
    assert_eq!(controller.view.failure, Some(Failure::ScopeChanged));
}

#[test]
fn desktop_remote_access_runtime_history_renders_without_operation_or_readiness_claim() {
    use egui_kittest::{kittest::Queryable, Harness};
    let mut harness = Harness::builder()
        .with_size(egui::vec2(600.0, 650.0))
        .build_ui_state(
            |ui, history: &mut Vec<festerm_host_setup::controller::runtime_diagnostics::Record>| {
                diagnostics::show(ui, None, history)
            },
            vec![runtime_history_fixture()],
        );
    harness.run();
    harness
        .get_by_label("Remote access runtime history")
        .click();
    harness.run();
    harness.get_by_label("Recorded events; use Refresh local status for an updated report.");
    harness.get_by_label("2026-11-19T00:00:00Z · relay_cleanup · error");
    harness.get_by_label("Numeric OS code: 5");
    assert!(harness.query_by_label("Last setup operation").is_none());
    harness.state_mut().clear();
    harness.run();
    assert!(harness
        .query_by_label("Remote access runtime history")
        .is_none());
}

#[test]
fn desktop_remote_access_refresh_replaces_recorded_state_and_clears_previous_process_observation() {
    use festerm_host_setup::controller::RunState;
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_fail = fail.clone();
    let mut controller = fixture(Arc::new(Fake(move |_: Request, _: &CancellationToken| {
        if worker_fail.load(Ordering::Acquire) {
            return Err(Failure::ScopeChanged);
        }
        Ok(Report {
            scope: provisioned_scope(),
            last_run: Some(RunState::Stopped),
            ..Report::default()
        })
    })));
    controller.view.last_run = Some(RunState::Listening);
    controller.view.process = Some("Running".into());
    let context = egui::Context::default();
    controller.action(Action::Refresh, &context);
    drain(&mut controller, &context);
    assert_eq!(controller.view.last_run, Some(RunState::Stopped));
    assert!(controller.view.process.is_none());
    fail.store(true, Ordering::Release);
    controller.view.last_run = Some(RunState::Listening);
    controller.view.process = Some("Running".into());
    controller.action(Action::Refresh, &context);
    drain(&mut controller, &context);
    assert!(controller.view.last_run.is_none());
    assert!(controller.view.process.is_none());
    assert_eq!(controller.view.failure, Some(Failure::ScopeChanged));
}

#[test]
fn desktop_remote_access_enable_confirmation_uses_task_copy_and_retains_disclosure() {
    use egui_kittest::{kittest::Queryable, Harness};
    for ordinary_access in [OrdinaryAccess::ProviderOwner, OrdinaryAccess::OwnerOnly] {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 1600.0))
            .build_ui_state(
                |ui, state: &mut (View, Option<Action>)| {
                    if let Some(action) = show(ui, &state.0, &[]) {
                        state.1 = Some(action);
                    }
                },
                (
                    View {
                        scope: Scope::Absent,
                        access_requested: Some(true),
                        review: Some(Operation::Enable),
                        accounts: vec![HostAccount {
                            account_id: 42,
                            login: "lab-owner".into(),
                        }],
                        draft: Draft {
                            friendly_name: "Study computer".into(),
                            account: "42".into(),
                            ordinary_access,
                            allow_creation: false,
                            allow_session_termination: false,
                            startup_at_sign_in: false,
                            ..Draft::default()
                        },
                        ..View::default()
                    },
                    None,
                ),
            );
        harness.get_by_label("Enable remote access?");
        harness.get_by_label("Host: Study computer");
        harness.get_by_label("GitHub account: @lab-owner");
        harness.get_by_label("Create additional shells: Off");
        harness.get_by_label("Start automatically at sign-in: Off");
        harness.get_by_label("You may be asked to sign in. Setup can create a Dev Tunnel. No shell sessions are created or shared by enabling access.");
        assert!(harness
            .query_by_label("Confirm Enable for the exact observed installation and epoch?")
            .is_none());
        assert!(harness.query_by_label("Acquire and authenticate this exact current-version native host bundle from the official release. Missing assets block setup; no older/latest retry.").is_none());
        let label = if ordinary_access == OrdinaryAccess::ProviderOwner {
            harness.get_by_label("Allow clients signed in to this host's GitHub account to list and open shared sessions without device pairing.");
            harness.get_by_label("A shared shell runs as your user on this computer and has that user's file and command permissions. Anyone holding a valid tunnel connection credential can obtain this access. New shells require the separate creation setting.");
            "Enable and start"
        } else {
            harness.get_by_label("Account-based connections are disabled. Only separately paired owner devices can connect.");
            "Enable"
        };
        harness.get_by_label(label).click();
        harness.run();
        assert!(matches!(harness.state().1, Some(Action::Confirm)));
        assert_eq!(harness.state().0.review, Some(Operation::Enable));
        assert_eq!(harness.state().0.draft.account, "42");
        assert_eq!(harness.state().0.draft.ordinary_access, ordinary_access);
        assert!(!harness.state().0.draft.allow_creation);
    }
}

#[test]
fn desktop_remote_access_process_confirmation_is_plain_and_keeps_observation_intent() {
    use egui_kittest::{kittest::Queryable, Harness};
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 1600.0))
        .build_ui_state(
            |ui, state: &mut (View, Option<Action>)| {
                if let Some(action) = show(ui, &state.0, &[]) {
                    state.1 = Some(action);
                }
            },
            (
                View {
                    scope: provisioned_scope(),
                    review: Some(Operation::ObserveNative),
                    ..View::default()
                },
                None,
            ),
        );
    let original_scope = harness.state().0.scope.clone();
    harness.get_by_label("Check host process?");
    harness
        .get_by_label("Check whether the host process is running. This does not start or stop it.");
    harness
        .get_by_label("A running process does not guarantee that a remote connection will work.");
    assert!(harness
        .query_by_label("Confirm ObserveNative for the exact observed installation and epoch?")
        .is_none());
    harness.get_by_label("Check process").click();
    harness.run();
    assert!(matches!(harness.state().1, Some(Action::Confirm)));
    assert_eq!(harness.state().0.review, Some(Operation::ObserveNative));
    assert_eq!(harness.state().0.scope, original_scope);
}

#[test]
fn desktop_remote_access_confirmation_keeps_bundle_mechanics_in_advanced() {
    use egui_kittest::{kittest::Queryable, Harness};
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 1800.0))
        .build_ui_state(
            |ui, state: &mut (View, Option<Action>)| {
                if let Some(action) = show(ui, &state.0, &[]) {
                    state.1 = Some(action);
                }
            },
            (
                View {
                    scope: provisioned_scope(),
                    review: Some(Operation::Enable),
                    ..View::default()
                },
                None,
            ),
        );
    let scope = harness.state().0.scope.clone();
    harness.get_by_label("Advanced / Recovery").click();
    harness.run();
    harness.get_by_label("Acquire and authenticate this exact current-version native host bundle from the official release. Missing assets block setup; no older/latest retry.");
    assert!(
        harness.state().1.is_none(),
        "opening details never confirms work"
    );
    harness.get_by_label("Not now").click();
    harness.run();
    assert!(matches!(harness.state().1, Some(Action::Dismiss)));
    assert_eq!(harness.state().0.scope, scope);
}

fn update_fixture_scope() -> Scope {
    let mut scope = provisioned_scope();
    let Scope::Present(snapshot) = &mut scope else {
        unreachable!()
    };
    Arc::make_mut(snapshot).phase = Phase::Disabled;
    scope
}

fn update_fixture_release() -> festerm_host_setup::services::ReleaseMetadata {
    festerm_host_setup::services::ReleaseMetadata {
        recorded_version: "0.0.1".into(),
        replacement: None,
    }
}

#[test]
fn desktop_remote_access_update_requires_explicit_review_and_keeps_host_disabled() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let captured = calls.clone();
    let expected = update_fixture_scope();
    let original = expected.clone();
    let mut controller = fixture(Arc::new(Fake(
        move |request: Request, _: &CancellationToken| {
            assert_eq!(request.operation, Operation::Update);
            assert_eq!(request.expected, original);
            assert_eq!(request.release.as_ref(), Some(&update_fixture_release()));
            captured.lock().unwrap().push(request.operation);
            Ok(Report {
                scope: request.expected,
                release: Some(festerm_host_setup::services::ReleaseMetadata {
                    recorded_version: festerm_host_setup::VERSION.into(),
                    replacement: None,
                }),
                ..Report::default()
            })
        },
    )));
    controller.view.scope = expected.clone();
    controller.view.release = Some(update_fixture_release());
    controller.view.access_requested = Some(true); // unconfirmed Enable intent is not retained.
    controller.view.draft.allow_creation = true; // update must not apply pending preferences.
    let context = egui::Context::default();
    controller.action(Action::Review(Operation::Update), &context);
    assert_eq!(controller.view.review, Some(Operation::Update));
    assert_eq!(controller.view.access_requested, Some(false));
    assert!(calls.lock().unwrap().is_empty());
    controller.action(Action::Confirm, &context);
    drain(&mut controller, &context);
    assert_eq!(*calls.lock().unwrap(), [Operation::Update]);
    assert_eq!(controller.view.scope, expected);
    assert!(!controller.view.access_enabled());
    assert!(!controller.view.update_eligible());
    assert!(controller.view.draft.allow_creation);
}

#[test]
fn desktop_remote_access_update_blocks_live_unknown_equal_and_older_target_without_worker() {
    let context = egui::Context::default();
    for (scope, release) in [
        (provisioned_scope(), Some(update_fixture_release())),
        (Scope::Absent, Some(update_fixture_release())),
        (update_fixture_scope(), None),
        (
            update_fixture_scope(),
            Some(festerm_host_setup::services::ReleaseMetadata {
                recorded_version: festerm_host_setup::VERSION.into(),
                replacement: None,
            }),
        ),
        (
            update_fixture_scope(),
            Some(festerm_host_setup::services::ReleaseMetadata {
                recorded_version: "99.0.0".into(),
                replacement: None,
            }),
        ),
    ] {
        let mut controller = fixture(Arc::new(Fake(
            |_: Request, _: &CancellationToken| -> Result<Report, Failure> {
                panic!("blocked update cannot admit a worker")
            },
        )));
        controller.view.scope = scope;
        controller.view.release = release;
        controller.view.review = Some(Operation::Enable);
        controller.action(Action::Review(Operation::Update), &context);
        assert_eq!(controller.view.failure, Some(Failure::UpdateBlocked));
        assert!(controller.view.review.is_none());
        assert!(!controller.view.busy);
    }
}

#[test]
fn desktop_remote_access_update_cancel_retains_admission_without_start_or_automatic_rollback() {
    let (send, receive) = mpsc::sync_channel(1);
    let receive = Arc::new(Mutex::new(receive));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let mut controller = fixture(Arc::new(Fake(
        move |request: Request, cancel: &CancellationToken| {
            recorded.lock().unwrap().push(request.operation);
            receive
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            assert!(cancel.is_cancelled());
            Ok(Report {
                scope: request.expected,
                ..Report::default()
            })
        },
    )));
    controller.view.scope = update_fixture_scope();
    controller.view.release = Some(update_fixture_release());
    let context = egui::Context::default();
    controller.action(Action::Review(Operation::Update), &context);
    controller.action(Action::Confirm, &context);
    controller.action(Action::Cancel, &context);
    assert!(controller.view.busy);
    assert!(controller.fence.is_none());
    controller.action(Action::Review(Operation::Update), &context);
    assert_eq!(controller.view.failure, Some(Failure::Busy));
    send.send(()).unwrap();
    drain(&mut controller, &context);
    assert_eq!(*calls.lock().unwrap(), [Operation::Update]);
    assert!(!controller.view.access_enabled());
}

#[test]
fn desktop_remote_access_update_view_shows_versions_blocked_state_and_confirmation() {
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 1200.0))
        .build_ui_state(
            |ui, state: &mut (View, Option<Action>)| {
                if let Some(action) = show(ui, &state.0, &[]) {
                    state.1 = Some(action);
                }
            },
            (
                View {
                    scope: update_fixture_scope(),
                    release: Some(update_fixture_release()),
                    ..View::default()
                },
                None,
            ),
        );
    harness.get_by_label(&format!(
        "Host components: 0.0.1 → {}",
        festerm_host_setup::VERSION
    ));
    let metadata_note =
        "Recorded host version is metadata; signed components are verified again when updating.";
    assert!(harness.query_by_label(metadata_note).is_none());
    harness.get_by_label("Update host…").click();
    harness.run();
    assert!(matches!(
        harness.state().1,
        Some(Action::Review(Operation::Update))
    ));
    harness.state_mut().0.review = Some(Operation::Update);
    harness.run();
    harness.get_by_label("Update host components?");
    harness.get_by_label("Remote access must be off. The host stays disabled after updating; enable access separately when you are ready.");
    harness.get_by_label("Update host").click();
    harness.run();
    assert!(matches!(harness.state().1, Some(Action::Confirm)));
    harness.state_mut().0.review = None;
    harness.state_mut().0.scope = provisioned_scope();
    harness.run();
    harness.get_by_label("Disable remote access before updating the host.");
    harness.get_by_label("Advanced / Recovery").click();
    harness.run();
    harness.get_by_label(metadata_note);
    harness
        .state_mut()
        .0
        .release
        .as_mut()
        .unwrap()
        .recorded_version = festerm_host_setup::VERSION.into();
    harness.run();
    assert!(harness
        .query_by_label("Disable remote access before updating the host.")
        .is_none());
    assert!(harness
        .get_by_label("Update host…")
        .accesskit_node()
        .is_disabled());

    // A retained committed update is history, including after a later Disable.
    // It must not be presented as the result of the current operation.
    harness.state_mut().0.scope = update_fixture_scope();
    harness.state_mut().0.release.as_mut().unwrap().replacement =
        Some(festerm_host_setup::services::ReplacementReport {
            operation: "historical-update".into(),
            from_version: "0.0.1".into(),
            to_version: festerm_host_setup::VERSION.into(),
            state: festerm_host_setup::services::ReplacementState::Committed,
            previous_provenance: None,
            candidate_provenance: None,
            candidate_executable: PathBuf::from("fixture-host"),
            candidate_bundle_sha256: "a".repeat(64),
            meaning: "Recorded update history only.",
        });
    harness.state_mut().1 = None;
    harness.run();
    let disabled_notice = "Remote access is disabled. Enable it separately when ready.";
    harness.get_by_label(disabled_notice);
    assert!(harness
        .query_by_label("Host updated. Enable remote access separately when ready.")
        .is_none());
    assert!(harness.state().1.is_none());
    harness.state_mut().0.scope = provisioned_scope();
    harness.run();
    assert!(harness.query_by_label(disabled_notice).is_none());
    assert!(harness.state().1.is_none());
}

#[test]
fn desktop_remote_access_update_failure_keeps_disabled_state_and_never_claims_rollback() {
    let mut controller = fixture(Arc::new(Fake(|request: Request, _: &CancellationToken| {
        assert_eq!(request.operation, Operation::Update);
        Ok(Report {
            scope: request.expected,
            release: request.release,
            failure: Some(Failure::UpdateIncomplete),
            ..Report::default()
        })
    })));
    controller.view.scope = update_fixture_scope();
    controller.view.release = Some(update_fixture_release());
    let context = egui::Context::default();
    controller.action(Action::Review(Operation::Update), &context);
    controller.action(Action::Confirm, &context);
    drain(&mut controller, &context);
    assert_eq!(controller.view.failure, Some(Failure::UpdateIncomplete));
    assert!(!controller.view.access_enabled());
    assert_eq!(controller.view.bundle_status, BundleStatus::Blocked);
    assert!(!Failure::UpdateIncomplete
        .notice()
        .contains("Nothing was installed"));
    assert!(!controller.view.busy);
}

#[test]
fn desktop_remote_access_old_draft_cannot_erase_protected_file_consent() {
    use festerm_host_setup::lifecycle::{Origin, Store};
    let root = festerm_sessiond::private_storage::private_test_directory();
    let old_draft = Draft::default().settings().unwrap();
    let mut configured = old_draft.clone();
    configured.filesystem = Some(festerm_tunnel_bridge::file_protocol::Policy {
        start_in: None,
        locations: None,
        delete: false,
        root: root.path().to_path_buf(),
        write: false,
    });
    let store = Store::create(
        &root.path().join("desktop-files"),
        Origin::Desktop,
        configured,
    )
    .unwrap();
    let before = store.snapshot().unwrap();
    assert!(store
        .request_enable_if_current(&before, &old_draft)
        .is_err());
    assert_eq!(store.snapshot().unwrap(), before);
}

#[test]
fn desktop_remote_access_files_require_explicit_folder_and_account_consent() {
    let mut draft = Draft::default();
    assert_eq!(draft.files, FileAccessDraft::default());
    assert!(draft.files.root.as_os_str().is_empty());
    assert!(draft.settings().unwrap().filesystem.is_none());
    draft.files.enabled = true;
    assert!(!draft.valid_files());
    draft.files.root = PathBuf::from("relative-folder");
    assert!(!draft.valid_files());
    let directory = festerm_sessiond::private_storage::private_test_directory();
    draft.files.root = directory.path().join("private-file-consent-canary");
    assert!(draft.valid_files());
    assert!(!draft.settings().unwrap().filesystem.as_ref().unwrap().write);
    draft.ordinary_access = OrdinaryAccess::OwnerOnly;
    assert!(!draft.valid_files());
    assert_eq!(draft.ordinary_access, OrdinaryAccess::OwnerOnly);
    assert!(!format!("{draft:?}").contains("private-file-consent-canary"));
    draft.files.enabled = false;
    assert!(draft.valid_files());
    assert!(draft.settings().unwrap().filesystem.is_none());
}

#[test]
fn desktop_remote_access_file_refresh_preserves_exact_consent_and_edited_draft() {
    let directory = festerm_sessiond::private_storage::private_test_directory();
    for write in [false, true] {
        for edited in [false, true] {
            let Scope::Present(mut snapshot) = provisioned_scope() else {
                unreachable!()
            };
            let policy = festerm_tunnel_bridge::file_protocol::Policy {
                start_in: None,
                locations: None,
                delete: false,
                root: directory.path().join("Exact Folder"),
                write,
            };
            Arc::make_mut(&mut snapshot).settings.filesystem = Some(policy.clone());
            let expected = Scope::Present(snapshot);
            let observed = expected.clone();
            let mut controller = fixture(Arc::new(Fake(
                move |request: Request, _: &CancellationToken| {
                    assert_eq!(request.operation, Operation::Refresh);
                    Ok(Report {
                        scope: observed.clone(),
                        ..Report::default()
                    })
                },
            )));
            let context = egui::Context::default();
            if edited {
                let mut draft = controller.view.draft.clone();
                draft.friendly_name = "Edited before refresh".into();
                controller.action(Action::Edit(draft), &context);
            }
            controller.action(Action::Refresh, &context);
            drain(&mut controller, &context);
            assert_eq!(controller.view.scope, expected);
            assert_eq!(
                controller.view.draft.settings().unwrap().filesystem,
                Some(policy)
            );
            if edited {
                assert_eq!(controller.view.draft.friendly_name, "Edited before refresh");
            }
            assert!(controller.view.review.is_none());
        }
    }
}

#[test]
fn desktop_remote_access_file_confirmation_captures_exact_scope_and_consent() {
    let directory = festerm_sessiond::private_storage::private_test_directory();
    let Scope::Present(mut snapshot) = provisioned_scope() else {
        unreachable!()
    };
    Arc::make_mut(&mut snapshot).phase = Phase::Disabled;
    let expected = Scope::Present(snapshot);
    let captured = Arc::new(Mutex::new(Vec::new()));
    let recorded = captured.clone();
    let mut controller = fixture(Arc::new(Fake(
        move |request: Request, _: &CancellationToken| {
            recorded.lock().unwrap().push((
                request.operation,
                request.expected.clone(),
                request.draft.settings().unwrap(),
            ));
            Ok(Report {
                scope: request.expected,
                ..Report::default()
            })
        },
    )));
    controller.view.scope = expected.clone();
    let context = egui::Context::default();
    let mut draft = controller.view.draft.clone();
    draft.files = FileAccessDraft {
        locations: None,
        start_root: None,
        start_in: String::new(),
        delete: false,
        enabled: true,
        root: directory.path().join("Exact Folder"),
        write: true,
    };
    // The bound account is preserved while the reviewed file policy changes.
    draft.account = "42".into();
    controller.action(Action::Edit(draft.clone()), &context);
    controller.action(Action::Review(Operation::Enable), &context);
    assert!(captured.lock().unwrap().is_empty());
    controller.action(Action::Confirm, &context);
    drain(&mut controller, &context);
    assert_eq!(
        *captured.lock().unwrap(),
        [(Operation::Enable, expected, draft.settings().unwrap())]
    );
}

#[test]
fn desktop_remote_access_file_edits_need_disabled_scope_and_fresh_review() {
    let directory = festerm_sessiond::private_storage::private_test_directory();
    let context = egui::Context::default();
    for scope in [Scope::Unobserved, provisioned_scope()] {
        let mut controller = fixture(Arc::new(Fake(|_: Request, _: &CancellationToken| {
            panic!("no dispatch")
        })));
        controller.view.scope = scope;
        let before = controller.view.draft.clone();
        let mut draft = before.clone();
        draft.files = FileAccessDraft {
            locations: None,
            start_root: None,
            start_in: String::new(),
            delete: false,
            enabled: true,
            root: directory.path().to_path_buf(),
            write: false,
        };
        controller.action(Action::Edit(draft), &context);
        assert_eq!(controller.view.draft, before);
        assert_eq!(controller.view.failure, Some(Failure::FileAccessReview));
        assert!(!controller.view.busy);
    }
    let mut controller = fixture(Arc::new(Fake(|_: Request, _: &CancellationToken| {
        panic!("no confirmation")
    })));
    controller.view.scope = Scope::Absent;
    controller.view.draft.files = FileAccessDraft {
        locations: None,
        start_root: None,
        start_in: String::new(),
        delete: false,
        enabled: true,
        root: directory.path().to_path_buf(),
        write: false,
    };
    for change in 0..3 {
        controller.action(Action::Review(Operation::Enable), &context);
        assert_eq!(controller.view.review, Some(Operation::Enable));
        let mut draft = controller.view.draft.clone();
        match change {
            0 => draft.files.root = directory.path().join("Different Folder"),
            1 => draft.files.write = true,
            _ => draft.files.enabled = false,
        }
        controller.action(Action::Edit(draft), &context);
        assert!(controller.view.review.is_none());
        controller.action(Action::Confirm, &context);
        assert!(!controller.view.busy);
    }
    controller.action(Action::Review(Operation::Enable), &context);
    controller.action(Action::Dismiss, &context);
    controller.action(Action::Confirm, &context);
    assert!(!controller.view.busy);
}

#[test]
fn desktop_remote_access_file_review_names_exact_folder_and_permissions() {
    use egui_kittest::{kittest::Queryable, Harness};
    let directory = festerm_sessiond::private_storage::private_test_directory();
    for (write, delete) in [(false, false), (true, false), (true, true)] {
        let root = directory.path().join("Reviewed Folder");
        let mut view = View {
            scope: Scope::Absent,
            access_requested: Some(true),
            review: Some(Operation::Enable),
            ..View::default()
        };
        view.draft.files = FileAccessDraft {
            locations: None,
            start_root: None,
            start_in: if write { "~".into() } else { String::new() },
            delete,
            enabled: true,
            root: root.clone(),
            write,
        };
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1400.0, 2400.0))
            .build_ui_state(
                |ui, view: &mut View| {
                    assert!(show(ui, view, &[]).is_none());
                },
                view,
            );
        harness.run();
        harness.get_by_label(&format!("Allowed location: {}", root.display()));
        if write {
            harness.get_by_label("Start in: ~");
        } else {
            harness.get_by_label(&format!("Start in: {}", root.display()));
        }
        harness.get_by_label(&format!("Up stops at: {}", root.display()));
        harness.get_by_label(if write {
            "File permissions: Read and write — upload and create folders"
        } else {
            "File permissions: Read only — browse and download"
        });
        harness.get_by_label(if delete {
            "Delete files and empty folders: On — permanent, reviewed individually"
        } else {
            "Delete files and empty folders: Off"
        });
        harness.get_by_label("Clients authorized through this host's GitHub account can access this folder. The folder restriction applies to file transfers, not shared shells.");
    }
}

#[test]
fn desktop_remote_access_file_review_distinguishes_start_location_and_up_boundary() {
    use egui_kittest::{kittest::Queryable, Harness};
    let directory = std::path::Path::new(if cfg!(windows) {
        "C:\\Locations"
    } else {
        "/locations"
    });
    let first = directory.join("First");
    let second = directory.join("Second");
    let child = second.join("Child").display().to_string();
    let nested = first.join("Nested");
    let nested_child = nested.join("Child").display().to_string();
    for (other, selected, start, initial, boundary) in [
        (
            &second,
            Some(1),
            "",
            format!("Start in: {}", second.display()),
            format!("Up stops at: {}", second.display()),
        ),
        (
            &second,
            None,
            "",
            "Start in: Choose a location when opening".into(),
            "Up stops at the location you choose.".into(),
        ),
        (
            &second,
            None,
            child.as_str(),
            format!("Start in: {child}"),
            format!("Up stops at: {}", second.display()),
        ),
        (
            &nested,
            Some(1),
            nested_child.as_str(),
            format!("Start in: {nested_child}"),
            format!("Up stops at: {}", nested.display()),
        ),
        (
            &second,
            None,
            "~",
            "Start in: ~".into(),
            "Up stops at the allowed location containing the resolved home path.".into(),
        ),
    ] {
        let mut view = View {
            scope: Scope::Absent,
            access_requested: Some(true),
            review: Some(Operation::Enable),
            ..View::default()
        };
        view.draft.files = FileAccessDraft {
            enabled: true,
            root: first.clone(),
            locations: Some(vec![first.clone(), other.to_path_buf()]),
            start_root: selected,
            start_in: start.into(),
            ..FileAccessDraft::default()
        };
        assert!(view.draft.valid_files());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1400.0, 2400.0))
            .build_ui_state(
                |ui, view: &mut View| {
                    assert!(show(ui, view, &[]).is_none());
                },
                view,
            );
        harness.run();
        harness.get_by_label(&initial);
        harness.get_by_label(&boundary);
        harness.get_by_label("Clients authorized through this host’s GitHub account can browse and download supported files this computer’s account can read inside the allowed locations. Write and Delete have stricter folder checks. Shared shells retain this computer’s account permissions.");
    }
}

#[test]
fn desktop_remote_access_editing_only_legacy_start_retains_singleton_read_contract() {
    use egui_kittest::{kittest::Queryable, Harness};
    let root = PathBuf::from(if cfg!(windows) {
        "C:\\Locations"
    } else {
        "/locations"
    });
    let mut view = View {
        scope: Scope::Absent,
        access_requested: Some(true),
        ..View::default()
    };
    let policy = festerm_tunnel_bridge::file_protocol::Policy {
        root: root.clone(),
        start_in: None,
        locations: None,
        write: false,
        delete: false,
    };
    view.draft.files = FileAccessDraft::from_policy(Some(&policy));
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 2400.0))
        .build_ui_state(
            |ui, view: &mut View| {
                if let Some(Action::Edit(draft)) = show(ui, view, &[]) {
                    view.draft = draft;
                }
            },
            view,
        );
    harness.run();
    harness
        .get_by_role_and_label(egui::accesskit::Role::TextInput, "Start in (optional)")
        .click();
    harness.run();
    harness
        .get_by_role_and_label(egui::accesskit::Role::TextInput, "Start in (optional)")
        .type_text(&root.join("Child").display().to_string());
    harness.run();
    let edited = harness
        .state()
        .draft
        .settings()
        .unwrap()
        .filesystem
        .unwrap();
    assert_eq!(
        edited,
        festerm_tunnel_bridge::file_protocol::Policy {
            start_in: Some("/Child".into()),
            ..policy
        }
    );
}

#[test]
fn desktop_remote_access_delete_permission_is_separate_requires_write_and_survives_draft_roundtrip()
{
    let root = festerm_sessiond::private_storage::private_test_directory();
    let mut draft = Draft::default();
    draft.files.enabled = true;
    draft.files.root = root.path().into();
    draft.files.write = true;
    assert!(draft.valid_files());
    assert!(
        !draft.settings().unwrap().filesystem.unwrap().delete,
        "old upload consent grants no deletion"
    );
    draft.files.delete = true;
    assert!(draft.valid_files());
    let settings = draft.settings().unwrap();
    assert!(settings.filesystem.as_ref().unwrap().delete);
    let restored = FileAccessDraft::from_policy(settings.filesystem.as_ref());
    assert_eq!(restored, draft.files);
    draft.files.write = false;
    assert!(!draft.valid_files());
    assert!(
        draft.files.delete,
        "an invalid edited combination must not silently rewrite consent"
    );
    draft.files.delete = false;
    assert!(draft.valid_files());
}

#[test]
fn desktop_remote_access_enable_timeout_clears_stale_scope_without_replay_and_renders_recovery() {
    use egui_kittest::{kittest::Queryable, Harness};
    let calls = Arc::new(AtomicU8::new(0));
    let counted = calls.clone();
    let mut controller = fixture(Arc::new(Fake(
        move |request: Request, _: &CancellationToken| {
            assert_eq!(request.operation, Operation::Enable);
            counted.fetch_add(1, Ordering::SeqCst);
            Err(Failure::Timeout)
        },
    )));
    controller.view.scope = update_fixture_scope();
    controller.view.access_requested = Some(true);
    let context = egui::Context::default();
    controller.start(Operation::Enable, false, &context);
    drain(&mut controller, &context);
    assert_eq!(controller.view.scope, Scope::Unobserved);
    assert_eq!(controller.view.failure, Some(Failure::Timeout));
    assert_eq!(controller.view.bundle_status, BundleStatus::Blocked);
    controller.poll(&context);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "no automatic retry after unknown setup result"
    );
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 1600.0))
        .build_ui_state(
            |ui, view: &mut View| {
                assert!(show(ui, view, &[]).is_none());
            },
            controller.view.clone(),
        );
    harness.run();
    harness.get_by_label(Failure::Timeout.notice());
    assert!(
        harness.query_by_label(Failure::Timeout.message()).is_none(),
        "main view keeps detailed deadline guidance in Recovery"
    );
    harness.get_by_label("Remote access status is unavailable. Refresh local status.");
    harness.get_by_label("Advanced / Recovery").click();
    harness.run();
    harness.get_by_label(Failure::Timeout.message());
    assert!(harness
        .query_by_label(Failure::Cancelled.notice())
        .is_none());
    assert!(!harness.state().access_enabled());
}

#[test]
fn desktop_remote_access_native_timeout_clears_stale_scope_without_replay_and_renders_recovery() {
    use egui_kittest::{kittest::Queryable, Harness};
    let calls = Arc::new(AtomicU8::new(0));
    let counted = calls.clone();
    let mut controller = fixture(Arc::new(Fake(
        move |request: Request, _: &CancellationToken| {
            assert_eq!(request.operation, Operation::Enable);
            counted.fetch_add(1, Ordering::SeqCst);
            Err(Failure::NativeTimeout)
        },
    )));
    controller.view.scope = update_fixture_scope();
    controller.view.access_requested = Some(true);
    let context = egui::Context::default();
    controller.start(Operation::Enable, false, &context);
    drain(&mut controller, &context);
    assert_eq!(controller.view.scope, Scope::Unobserved);
    assert_eq!(controller.view.failure, Some(Failure::NativeTimeout));
    assert_eq!(controller.view.bundle_status, BundleStatus::Blocked);
    controller.poll(&context);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "no automatic retry after unknown setup result"
    );
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 1600.0))
        .build_ui_state(
            |ui, view: &mut View| {
                assert!(show(ui, view, &[]).is_none());
            },
            controller.view.clone(),
        );
    harness.run();
    harness.get_by_label(Failure::NativeTimeout.notice());
    assert!(
        harness
            .query_by_label(Failure::NativeTimeout.message())
            .is_none(),
        "main view keeps detailed deadline guidance in Recovery"
    );
    harness.get_by_label("Remote access status is unavailable. Refresh local status.");
    harness.get_by_label("Advanced / Recovery").click();
    harness.run();
    harness.get_by_label(Failure::NativeTimeout.message());
    assert!(harness
        .query_by_label(Failure::Cancelled.notice())
        .is_none());
    assert!(!harness.state().access_enabled());
    assert!(harness.query_by_label(Failure::Timeout.message()).is_none());
    assert!(harness
        .query_by_label(Failure::PrivateStorage.notice())
        .is_none());
}

fn update_review_reveals_once_in_small_scroll_area(advanced_open: bool) {
    use egui_kittest::{kittest::Queryable, Harness};
    let calls = Arc::new(AtomicU8::new(0));
    let recorded = calls.clone();
    let mut controller = fixture(Arc::new(Fake(move |_: Request, _: &CancellationToken| {
        recorded.fetch_add(1, Ordering::AcqRel);
        Err(Failure::InvalidDraft)
    })));
    controller.view.scope = update_fixture_scope();
    controller.view.release = Some(update_fixture_release());
    controller.view.draft.manual_bundle = true;
    controller.view.draft.manifest = "C:/fixture/host.manifest.json".into();
    controller.view.draft.signature = "C:/fixture/host.manifest.json.sig".into();
    controller.view.draft.archive = "C:/fixture/host.zip".into();
    let original_scope = controller.view.scope.clone();
    let original_draft = controller.view.draft.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(752.0, 516.0))
        .build_ui_state(
            |ui, state: &mut (Controller, egui::Rect)| {
                let view = state.0.view();
                let shown = egui::ScrollArea::vertical()
                    .id_salt("remote_access_scroll_review_test")
                    .show(ui, |ui| show(ui, &view, &[]));
                state.1 = shown.inner_rect;
                if let Some(action) = shown.inner {
                    state.0.action(action, ui.ctx());
                }
            },
            (controller, egui::Rect::NOTHING),
        );
    harness.run_steps(3);
    if advanced_open {
        harness.get_by_label("Advanced / Recovery").click();
        harness.run_steps(3);
        harness
            .get_by_label("Advanced · explicit signed local bundle")
            .scroll_to_me();
        harness.run_steps(3);
        harness
            .get_by_label("Advanced · explicit signed local bundle")
            .click();
        harness.run_steps(3);
        harness.get_by_label("Update host…").scroll_to_me();
        harness.run_steps(3);
    }
    harness.get_by_label("Update host…").click();
    harness.run_steps(10);
    assert_eq!(harness.state().0.view.review, Some(Operation::Update));
    for label in ["Update host components?", "Update host", "Not now"] {
        let rect = harness.get_by_label(label).rect();
        assert!(
            harness.state().1.contains_rect(rect),
            "{label}: {rect:?} outside {:?}",
            harness.state().1
        );
    }
    assert_eq!(calls.load(Ordering::Acquire), 0);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    assert!(
        harness.state().0.view.review.is_none(),
        "Enter must cancel, never update"
    );
    assert_eq!(calls.load(Ordering::Acquire), 0);
    assert_eq!(harness.state().0.view.scope, original_scope);
    assert_eq!(harness.state().0.view.draft, original_draft);

    harness.get_by_label("Update host…").scroll_to_me();
    harness.run_steps(3);
    harness.get_by_label("Update host…").click();
    harness.run_steps(10);
    assert!(
        harness
            .state()
            .1
            .contains_rect(harness.get_by_label("Not now").rect()),
        "a reopened review is revealed again"
    );
    harness.get_by_label("Update host…").scroll_to_me();
    harness.run_steps(10);
    assert!(harness
        .state()
        .1
        .contains_rect(harness.get_by_label("Update host…").rect()));
    harness.run_steps(10);
    if advanced_open {
        assert!(
            !harness
                .state()
                .1
                .contains_rect(harness.get_by_label("Not now").rect()),
            "pending review must not force scrolling every frame"
        );
    }
    assert_eq!(harness.state().0.view.review, Some(Operation::Update));
    harness.get_by_label("Not now").scroll_to_me();
    harness.run_steps(3);
    harness.get_by_label("Not now").click();
    harness.run_steps(3);
    assert!(harness.state().0.view.review.is_none());
    assert_eq!(calls.load(Ordering::Acquire), 0);
    assert!(harness.state().0.pending.is_none() && !harness.state().0.view.busy);
}

#[test]
fn host_update_review_reveals_from_collapsed_details_and_cancel_is_safe_default() {
    update_review_reveals_once_in_small_scroll_area(false);
}

#[test]
fn host_update_review_reveals_from_expanded_manual_inputs_without_forced_scroll() {
    update_review_reveals_once_in_small_scroll_area(true);
}

#[test]
fn host_enable_long_files_review_reveals_footer_once_and_cancel_is_safe_default() {
    use egui_kittest::{kittest::Queryable, Harness};
    for present in [false, true] {
        let calls = Arc::new(AtomicU8::new(0));
        let recorded = calls.clone();
        let mut controller = fixture(Arc::new(Fake(move |_: Request, _: &CancellationToken| {
            recorded.fetch_add(1, Ordering::AcqRel);
            Err(Failure::InvalidDraft)
        })));
        controller.view.scope = if present {
            update_fixture_scope()
        } else {
            Scope::Absent
        };
        controller.view.access_requested = Some(true);
        controller.view.draft.account = "42".into();
        controller.view.draft.friendly_name = "Reviewed host".into();
        controller.view.draft.ordinary_access = OrdinaryAccess::ProviderOwner;
        controller.view.draft.files.enabled = true;
        controller.view.draft.files.root =
            std::env::temp_dir().join("festerm-enable-review-fixture");
        assert!(controller.view.draft.valid_files());
        let scope = controller.view.scope.clone();
        let draft = controller.view.draft.clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(752.0, 300.0))
            .build_ui_state(
                |ui, state: &mut (Controller, egui::Rect)| {
                    let view = state.0.view();
                    let shown = egui::ScrollArea::vertical()
                        .id_salt("enable_review_viewport")
                        .show(ui, |ui| show(ui, &view, &[]));
                    state.1 = shown.inner_rect;
                    if let Some(action) = shown.inner {
                        state.0.action(action, ui.ctx());
                    }
                },
                (controller, egui::Rect::NOTHING),
            );
        harness.run_steps(3);
        harness
            .get_by_label("Review Enable and start…")
            .scroll_to_me();
        harness.run_steps(3);
        harness.get_by_label("Review Enable and start…").click();
        harness.run_steps(10);
        assert_eq!(harness.state().0.view.review, Some(Operation::Enable));
        for label in ["Enable and start", "Not now"] {
            let rect = harness.get_by_label(label).rect();
            assert!(
                harness.state().1.contains_rect(rect),
                "present={present}: {label} footer {rect:?} outside {:?}",
                harness.state().1
            );
        }
        assert_eq!(calls.load(Ordering::Acquire), 0);
        harness.key_press(egui::Key::Enter);
        harness.run_steps(3);
        assert!(
            harness.state().0.view.review.is_none(),
            "Enter cancels Enable"
        );
        assert_eq!(harness.state().0.view.scope, scope);
        assert_eq!(harness.state().0.view.draft, draft);
        // Dismiss intentionally clears the requested-access draft. Reopen
        // through the ordinary toggle rather than bypassing that safe reset.
        harness
            .get_by_role_and_label(
                egui::accesskit::Role::CheckBox,
                "Allow remote access to this computer",
            )
            .scroll_to_me();
        harness.run_steps(3);
        harness
            .get_by_role_and_label(
                egui::accesskit::Role::CheckBox,
                "Allow remote access to this computer",
            )
            .click();
        harness.run_steps(3);
        harness
            .get_by_label("Review Enable and start…")
            .scroll_to_me();
        harness.run_steps(3);
        harness.get_by_label("Review Enable and start…").click();
        harness.run_steps(10);
        assert!(harness
            .state()
            .1
            .contains_rect(harness.get_by_label("Not now").rect()));
        harness
            .get_by_role_and_label(
                egui::accesskit::Role::CheckBox,
                "Allow remote access to this computer",
            )
            .scroll_to_me();
        harness.run_steps(10);
        assert!(
            !harness
                .state()
                .1
                .contains_rect(harness.get_by_label("Not now").rect()),
            "open review must not force repeated scrolling"
        );
        assert_eq!(calls.load(Ordering::Acquire), 0);
        assert!(harness.state().0.pending.is_none());
    }
}

#[test]
fn desktop_remote_access_bound_enable_discard_requires_local_proof_before_review() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let recorded = calls.clone();
    let mut controller = fixture(Arc::new(Fake(move |_: Request, _: &CancellationToken| {
        recorded.fetch_add(1, Ordering::SeqCst);
        panic!("missing proof must not admit a worker")
    })));
    controller.view.scope = provisioned_scope();
    let context = egui::Context::default();
    controller.action(Action::Review(Operation::DiscardBoundEnable), &context);
    assert!(controller.view.review.is_none());
    assert_eq!(controller.view.failure, Some(Failure::Reconciliation));
    controller.action(Action::Confirm, &context);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn desktop_remote_access_bound_enable_discard_review_preserves_off_and_separate_actions() {
    use egui_kittest::{kittest::Queryable, Harness};
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 1600.0))
        .build_ui_state(
            |ui, state: &mut (View, Option<Action>)| {
                if let Some(action) = show(ui, &state.0, &[]) {
                    state.1 = Some(action);
                }
            },
            (
                View {
                    scope: provisioned_scope(),
                    access_requested: Some(false),
                    review: Some(Operation::DiscardBoundEnable),
                    ..View::default()
                },
                None,
            ),
        );
    harness.run();
    harness.get_by_label("Discard interrupted setup?");
    harness.get_by_label("Keep remote access off and preserve this host and its sessions. This clears the interrupted setup; it does not stop the host or check its cloud status. Review Disable next, then Enable to sign in again.");
    harness.get_by_label("Discard interrupted setup").click();
    harness.run();
    assert!(matches!(harness.state().1, Some(Action::Confirm)));
    assert_eq!(
        harness.state().0.review,
        Some(Operation::DiscardBoundEnable)
    );
    assert_eq!(harness.state().0.access_requested, Some(false));
    assert!(!Operation::DiscardBoundEnable.needs_cancel_fence());
}

#[test]
fn desktop_files_start_directory_owner_input_is_relative_to_allowed_location() {
    let root = festerm_sessiond::private_storage::private_test_directory();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let mut files = FileAccessDraft {
        locations: None,
        start_root: None,
        enabled: true,
        root: root.path().into(),
        ..Default::default()
    };
    assert!(files.policy().unwrap().unwrap().start_in.is_none());
    files.start_in = "~".into();
    assert!(
        files.policy().is_err(),
        "unresolved shorthand cannot enter protected Settings"
    );
    files
        .resolve_start_in(|| Ok(root.path().join("home")))
        .unwrap();
    let policy = files.policy().unwrap().unwrap();
    assert_eq!(policy.start_in.as_deref(), Some("/home"));
    assert_eq!(FileAccessDraft::from_policy(Some(&policy)), files);
    assert!(!format!("{files:?}").contains("home"));
    for bad in [
        "relative",
        "/outside",
        "~/../escape",
        "~//escape",
        "~/C:\\outside",
    ] {
        files.start_in = bad.into();
        assert!(files
            .resolve_start_in(|| Ok(root.path().join("home")))
            .is_err());
    }
    files.start_in = root.path().join("\u{202e}hidden").to_str().unwrap().into();
    assert!(
        files.policy().is_err(),
        "unsupported name controls are not normalized into another path"
    );
    files.start_in = root.path().to_str().unwrap().into();
    assert!(files.policy().unwrap().unwrap().start_in.is_none());
    files.start_in = "~".into();
    assert!(files
        .resolve_start_in(|| Err(std::io::ErrorKind::PermissionDenied.into()))
        .is_err());
}

#[test]
fn desktop_files_start_directory_drive_suffix_refuses_before_home_callback() {
    for input in [
        "~/C:/folder",
        "~/C:folder",
        "~//server/share",
        "~/../folder",
    ] {
        let mut files = FileAccessDraft {
            locations: None,
            start_root: None,
            enabled: true,
            root: PathBuf::from(if cfg!(windows) { "C:/" } else { "/" }),
            start_in: input.into(),
            ..Default::default()
        };
        assert!(files
            .resolve_start_in(|| panic!(
                "invalid suffix must not look up home or open any directory"
            ))
            .is_err());
        assert_eq!(files.start_in, input);
    }
}

#[cfg(windows)]
#[test]
fn desktop_files_start_directory_windows_home_join_never_receives_drive_prefix() {
    let home = PathBuf::from("C:/Users/fixture");
    assert_eq!(
        home.join("C:/folder"),
        PathBuf::from("C:/folder"),
        "native join would replace the account home"
    );
    let mut files = FileAccessDraft {
        locations: None,
        start_root: None,
        enabled: true,
        root: PathBuf::from("C:/"),
        start_in: "~/C:/folder".into(),
        ..Default::default()
    };
    assert!(files
        .resolve_start_in(|| panic!("prefix must refuse before callback"))
        .is_err());
}

#[test]
fn desktop_files_allowed_locations_start_is_separate_and_legacy_stays_legacy() {
    let first = if cfg!(windows) {
        PathBuf::from("C:/first")
    } else {
        PathBuf::from("/first")
    };
    let second = if cfg!(windows) {
        PathBuf::from("C:/second")
    } else {
        PathBuf::from("/second")
    };
    let mut files = FileAccessDraft {
        enabled: true,
        root: first.clone(),
        locations: Some(vec![first.clone(), second.clone()]),
        ..Default::default()
    };
    let chooser = files.policy().unwrap().unwrap();
    assert!(chooser.locations.as_ref().unwrap().start.is_none());
    assert!(chooser.start_in.is_none());
    files.start_root = Some(1);
    let root_start = files.policy().unwrap().unwrap();
    let start = root_start
        .locations
        .as_ref()
        .unwrap()
        .start
        .as_ref()
        .unwrap();
    assert_eq!(start.root, 1);
    assert_eq!(start.path.as_str(), "/");
    assert_eq!(FileAccessDraft::from_policy(Some(&root_start)), files);
    files.start_root = None;
    files.start_in = second.join("home").to_str().unwrap().into();
    let selected = files.policy().unwrap().unwrap();
    let start = selected.locations.as_ref().unwrap().start.as_ref().unwrap();
    assert_eq!(start.root, 1);
    assert_eq!(start.path.as_str(), "/home");
    assert_eq!(
        FileAccessDraft::from_policy(Some(&selected))
            .policy()
            .unwrap(),
        Some(selected)
    );
    files.start_root = Some(0);
    assert!(
        files.policy().is_err(),
        "explicit chosen root cannot be replaced by path matching"
    );
    files.locations = None;
    files.start_root = None;
    files.start_in = first.join("home").to_str().unwrap().into();
    assert!(files.policy().unwrap().unwrap().locations.is_none());
}
