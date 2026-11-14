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

fn provisioned_scope() -> Scope {
    use festerm_host_setup::lifecycle::{Binding, Origin, Store};
    let root = festerm_sessiond::private_storage::private_test_directory();
    let store = Store::create(
        &root.path().join("desktop"),
        Origin::Desktop,
        Draft::default().settings(),
    )
    .unwrap();
    let ticket = store.request_enable(&Draft::default().settings()).unwrap();
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
                recorded.lock().unwrap().push(request.draft.settings());
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
        controller.view.draft.startup_at_sign_in = initially_enabled;
        for label in [
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
